# Architecture overview

Switchyard is a local-first orchestration platform for native AI coding agents (Claude Code, OpenAI
Codex). It is not an agent itself. It supervises agent processes, coordinates their work, persists
what happens, and gives a human operator control.

This document describes what exists **today** (milestones 1–6: the foundation, native Claude Code
sessions, workspace isolation, approval policies, native Codex sessions, and task orchestration) and the boundaries that later milestones build on.

## Processes

```text
┌──────────────────────────────┐        ┌────────────────────────────────────────────┐
│ Switchyard Desktop (Tauri 2) │        │ Switchyard Core  (switchyard-core binary)  │
│                              │        │                                            │
│  React UI (webview)          │        │  server / connections ── handlers          │
│    │ invoke / events         │  UDS   │        │                    │              │
│  Rust bridge ────────────────┼────────┼─►  JSON-lines protocol      ├─ git (CLI)   │
│   - one Core connection      │ 0600   │                             ├─ storage ──► SQLite (WAL)
│   - reconnect + replay       │        │  event bus ◄── publish ─────┘              │
│   - auto-launch Core         │        │   (persist → broadcast)                    │
└──────────────────────────────┘        │  agent supervisors ─► claude, codex        │
                                        └────────────────────────────────────────────┘
```

- **Core** (`crates/switchyard-core`) is the long-running owner of all state. It holds the instance
  lock, the SQLite database, the event bus and (in later milestones) every agent process. Its
  lifetime is independent of any client.
- **Desktop** (`apps/desktop`) is a client. Its Rust side keeps exactly one IPC connection to Core,
  and starts Core if it is not running. The webview never touches the socket, SQLite or agent
  processes; it calls an allow-list of Core methods through Tauri commands.
- Closing the desktop app closes one connection, and Core keeps running. Reopening the app
  reconnects to the same Core instance and replays missed events from the journal. This was checked
  manually: [README › Verification](../../README.md#verification).

## Crates

| Crate | Responsibility | Depends on |
|-------|----------------|------------|
| `switchyard-events` | Event envelope and typed `DomainEvent` catalogue, the `EventJournal` persistence trait, and the in-process `EventBus` (bounded broadcast, persist-then-publish, lossless catch-up). | — |
| `switchyard-protocol` | Wire frames, typed method definitions, error codes, bounded line codec, and shared path resolution (data dir and socket). | events |
| `switchyard-storage` | SQLite connection (WAL, foreign keys, `synchronous=FULL`), embedded migrations, project repository, event journal implementation, health stats. | events |
| `switchyard-git` | Git through the `git` CLI: repository-root validation, `status --porcelain=v2` parsing, and worktree add/list/identity/remove for Core-owned workspaces (never forced, never pruning). | — |
| `switchyard-client` | Async protocol client: handshake, request correlation, notifications, disconnect detection. Used by the desktop and the tests. | protocol |
| `switchyard-agents` | Vendor-neutral agent model; process supervision (no shell, controlled environment, bounded lines, SIGTERM/SIGKILL escalation); the provider-neutral `AgentSession` handle; Claude Code and Codex adapters. | — |
| `switchyard-workspaces` | Workspace domain rules without I/O: lifecycle and transitions, branch and path naming, cleanup safety evaluation. | — |
| `switchyard-policy` | Approval policy without I/O: normalized operations, rules and their validation, deterministic evaluation with explanations. | — |
| `switchyard-tasks` | Task domain without I/O: lifecycle and transitions, the dependency graph (cycles, readiness), the `switchyard-report` parser and the versioned task context. | — |
| `switchyard-core` | The executable: config, lifecycle (lock, socket, signals), connection server, request handlers, workspaces (`src/workspaces`), agent sessions (`src/agents`), and the `switchyard-fake-claude` test binary. | all of the above |
| `apps/desktop/src-tauri` | Desktop shell: Core bridge (connect, reconnect, replay, forward events), launcher, Tauri commands. | client, protocol |

Dependency rule: **transport, persistence and domain stay separate.** `events` does not know about
SQLite, `storage` does not know about sockets, and `protocol` does not know about handlers. Only
`core` wires them together.

`switchyard-permissions` from the initial proposal became `switchyard-policy` in milestone 4 (see
[ADR 0006](../decisions/0006-crate-layout.md) and [ADR 0016](../decisions/0016-approval-policy-model.md)).

## Request path (example: `projects.add`)

1. The UI calls `invoke("core_request", {method: "projects.add", params: {path}})`.
2. The desktop bridge checks the method against its allow-list and forwards the request over its
   Core connection.
3. Core's connection task decodes the frame, checks that `initialize` was done, takes an in-flight
   permit (at most 32 per connection), and spawns the handler.
4. The handler validates the path with `switchyard-git`: absolute, exists, is a directory, readable,
   the root of a non-bare working tree. Symlinks are canonicalised.
5. Inside `EventBus::publish_with`, the handler inserts the project row **and** appends
   `project.registered` in one SQLite transaction. After the commit, the event is broadcast.
6. The response goes back to the bridge and then the UI. Every subscriber, including the desktop's
   own subscription, receives the event. The UI uses it to refresh views.

## Event engine

- `EventDraft`: an event that has not been stored yet. `EventEnvelope`: a stored event with a
  journal-assigned `seq`.
- `publish(event)`: append to the journal, then broadcast. `publish_with(work)`: for domain changes;
  the domain write and the event append share one transaction (transactional outbox).
- Both paths hold an ordering lock while appending and broadcasting, so subscribers observe events
  in strictly increasing `seq` order.
- The live broadcast channel is bounded (capacity 1024). A subscriber that falls behind gets
  `Lagged`, then **re-reads the gap from the journal** and continues. It never loses events and
  never blocks publishers.
- Each client connection has a bounded outbound queue (256 frames). A slow client stalls only its
  own subscription tasks, which then fall behind and recover from the journal as described above.

## Persistence

- One SQLite file, `<data dir>/switchyard.db`, in WAL mode with `synchronous=FULL` and foreign keys
  enforced.
- Migrations live in `/migrations`, are embedded at compile time, and are tracked with
  `PRAGMA user_version`. Core refuses a database written by a newer schema.
- The schema covers projects, workspaces, agents, sessions, approvals, policy rules with their
  revisions and evaluations, events, and the reserved tasks, messages, findings and decisions. See [database.md](database.md) for what is written today.
- The `events` table is append-only (`UPDATE` and `DELETE` are blocked by triggers). Its context
  columns have no foreign keys: the journal outlives the entities it mentions.
- Core holds a single connection behind a mutex, and async callers run on Tokio's blocking pool.
  This is enough for current load and keeps write ordering trivial.

## Core lifecycle

Startup order: create the data dir (`0700`) → take an exclusive `flock` on `core.lock` (exit code 3
if another Core holds it) → open and migrate the database → record `core.started` → remove a stale
socket file (safe because the lock is held) → bind the socket (`0600`) → accept connections.

Shutdown is triggered by SIGTERM, SIGINT, SIGHUP or `system.shutdown`:

1. Stop accepting connections.
2. Journal and broadcast `core.stopping`.
3. Tell connections to close. Subscription tasks first flush what they have already received (so
   clients see `core.stopping`), then each connection sends a `shutdown` frame and closes.
4. Wait up to 3 s for connections to drain, then abort the rest.
5. Remove the socket and release the lock.

A crash (SIGKILL) leaves a stale socket file, which the next Core removes. The `flock` is released by
the OS. Both cases are covered by tests.

## Desktop bridge

- `supervise()` runs for the lifetime of the app: connect, `initialize`, `events.subscribe` (with
  `after_seq` = last seen seq), forward events to the webview as `core://event`, and publish
  connection state as `core://connection`.
- On a connection failure where the socket is missing or refused, the bridge launches
  `switchyard-core` (at most every 10 s) in **its own process group**, with output appended to
  `<data dir>/logs/core.log`. Core is never waited on or killed by the desktop.
- Reconnect back-off grows from 250 ms to 5 s. The UI's "Retry now" skips the wait.
- Core binary resolution: `$SWITCHYARD_CORE_BIN`, otherwise `switchyard-core` next to the desktop
  executable. That location is `target/<profile>/` during development and `Contents/MacOS/` in the
  bundle, via Tauri `externalBin`.
- Auto-launch can be disabled with `SWITCHYARD_AUTO_LAUNCH_CORE=0`.

## Frontend

React 19 with strict TypeScript and no router or state library. A small external store
(`useSyncExternalStore`) holds connection state and the live event feed. `useCoreQuery` fetches data
when Core (re)connects and refreshes on relevant events. Sections: Dashboard (live health, projects,
recent events, active sessions), Projects (register, project checkout Git status, workspaces:
create, start a session, Git status, archive, restore, confirmed cleanup), Agents (session view with
workspace, branch and path, live output, composer, automatic policy decisions), Approvals (with
the policy result and reason), Policies (rules per project and workspace, defaults, enforced
restrictions, decision history), Activity (journal) and Settings (real
runtime paths and versions). No simulated data anywhere.

## Agent sessions (milestone 2)

```text
UI ──invoke──► bridge ──IPC──► handlers/agents.rs ──► AgentService (core/src/agents)
                                                        │  commit(): state + events + approvals, one SQLite tx
                                                        ├─ supervisor task per session ◄── SessionUpdate (bounded 256)
                                                        │      └─ LiveHub (lossy broadcast) ──► live frames
                                                        ├─ approvals: request → pending → operator/timeout/cancel
                                                        └─ WatcherHub (notify/FSEvents, debounced) ──► files.changed
switchyard-agents::claude::spawn_session ── driver task owns child stdin/stdout ── `claude -p` (stream-json)
switchyard-agents::codex::spawn_session  ── driver task + codex::Protocol      ── `codex app-server` (JSON-RPC)
```

- `switchyard-agents` is vendor-neutral (states, capabilities, adapter output, the `AgentSession`
  handle) plus two adapters, each with detection, argv and environment construction, a protocol
  normalizer and a session driver: Claude Code (`claude/`) and Codex (`codex/`, see
  [codex-integration.md](codex-integration.md)). Core picks the adapter by the session's recorded
  provider and never sees either wire format. The fake CLIs used by tests are
  `switchyard-core/src/bin/fake_claude.rs` and `fake_codex.rs`.
- **Durable vs. ephemeral.** Complete messages, tool calls and results, approvals, state changes and
  file changes go to the journal. Token deltas go only to the live channel, with a per-session
  in-progress snapshot ([ADR 0009](../decisions/0009-durable-vs-live-output.md)).
- **Approvals** are rows in `approvals`, settled exactly once by a SQL guard. A decision is committed
  before it is delivered; expiry and every non-operator path deny.
- **Process ownership and recovery** follow [ADR 0010](../decisions/0010-agent-process-ownership.md):
  agents survive UI disconnects, are stopped on Core shutdown, and are not re-adopted after a Core
  crash (they are marked `failed` and resumed explicitly).
- **Workspace boundary:** [ADR 0011](../decisions/0011-agent-workspace-boundary.md).
- **Files.** Actual changes come from the filesystem watcher (a fact), tool calls from the agent
  stream (intent), and repository state from `projects.git_status`. The UI shows all three
  separately.

## Workspaces (milestone 3)

```text
Project (registered repo, never an agent cwd)
  └── Workspace  <data>/workspaces/<project>/<workspace>   branch switchyard/<workspace>
        └── Session  (at most one live per workspace; cwd = workspace path)
```

- `WorkspaceService` (`core/src/workspaces`) creates worktrees (`provisioning → ready`), validates
  them before every session start or resume, settles them after sessions (`active → ready/dirty`),
  and handles archive, restore, verify, and explicit, confirmed cleanup. Startup reconciliation
  runs after session reconciliation and never deletes anything.
- Git mutations are serialized per repository. Status changes and their events commit together.
- File watchers run per workspace; `files.changed` carries project, workspace and session.
- Details: [workspaces.md](workspaces.md), ADRs 0012–0014. Worktrees isolate source trees, **not
  processes**.

## Approval policies (milestone 4)

```text
request ─► classify(provider) ─► Operation ─► path facts (Core, I/O) ─┐
                                                                          ▼
     one SQLite transaction: load rules + revision ─► evaluate (pure) ─► record evaluation + events
                                                                          │
                     allow ─► answer allow    ask ─► approval (person)    deny ─► answer deny
```

- Every Claude tool reaches Switchyard: the launch settings carry an `ask` rule for every enabled
  tool. Codex runs with `approvalPolicy: "untrusted"`, so every command and patch is an approval
  request; tools that would act without one are disabled (ADR 0021).
- `switchyard-policy` evaluates. The precedence is:
  1. system restrictions;
  2. the most restrictive matching rule (deny > ask > allow, whatever its scope);
  3. built-in defaults;
  4. caps that turn `allow` into `ask`.

  Failures deny.
- Human decisions apply to one request and never create rules.
- A pending approval is never upgraded by a rule change, but a change can deny it.
- Details: [policies.md](policies.md), ADRs 0016–0019. **Policies are an authorization and
  workflow mechanism, not an OS-level sandbox.**

## Tasks (milestone 6)

- Tasks are durable work, independent of sessions. An assignment gives one role (investigate,
  implement, review) to one provider in one workspace.
- Core sends a versioned task context. The agent answers with a `switchyard-report` block, which
  becomes structured messages, findings and proposals. None of these authorizes anything.
- Review sessions cannot change files, and the workspace is fingerprinted around each review.
- The operator publishes, assigns, completes, fails and approves proposals.
- Details: [tasks.md](tasks.md), ADRs 0023–0027.

## What is intentionally absent

There is no support for Codex sub-agents (their requests are denied), no integration of workspace branches (merge, rebase, cherry-pick), no
agent-initiated task creation or assignment (agents only propose), no automatic branch deletion or workspace pruning, no global
(cross-project) policy rules, and no OS-level sandbox yet.
