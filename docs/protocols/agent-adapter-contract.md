# Agent adapter contract (v2, implemented for Claude Code and Codex)

Code: `crates/switchyard-agents` (model, process supervision, the `AgentSession` handle, the Claude
and Codex adapters) and `crates/switchyard-core/src/agents` (sessions, approvals, live output, file
watching). Vendor wire types are private to `switchyard_agents::claude` and
`switchyard_agents::codex`, so the rest of Switchyard sees only the vendor-neutral types below.
Core dispatches by the session's recorded provider through `switchyard_agents::{classify,
capabilities}`. Background: [agent-integration.md](../architecture/agent-integration.md).

## Principles (unchanged from the draft)

1. **Native agents.** Adapters supervise the vendor CLI as a child of Core. They never call model
   APIs or reimplement the agent loop.
2. **Core owns processes.** The UI observes and sends commands through the Core protocol.
3. **Explicit capabilities.** Operations an agent lacks fail with `unsupported` or `invalid_state`.
   They are never emulated, for example by silently restarting a process.
4. **Important activity is durable; token streams are not.**
5. **Switchyard owns every permission decision.** Every request is evaluated by Core's policy engine
   (ADRs 0016–0018). Only an explicit rule or a documented built-in default allows without a
   person. Every failure path, and every non-operator settlement of an approval, denies.
6. **Adapters classify, Core decides.** An adapter maps its vendor requests to
   `switchyard_policy::Operation` (category, targets, command, unknown fields). Anything it cannot
   classify carries a `problem` and is denied. Vendor JSON never reaches the policy engine.

## Vendor-neutral model (`switchyard_agents::model`)

| Type | Values |
|------|--------|
| `AgentProvider` | `claude_code`, `codex` |
| `SessionState` | `starting` → `initializing` → `idle` ⇄ `running` ⇄ `waiting_for_approval`; `running`/`waiting_for_approval` → `interrupted` → `running`; any live state → `completed` \| `failed` \| `stopped`; terminal → `starting` (explicit resume only). `SessionState::transition` enforces this graph and storage rejects illegal transitions. |
| `Capabilities` | Each is `supported`, `unsupported` or `unknown` (ADR 0022): `multi_turn`, `streaming_text`, `tool_activity`, `interrupt_turn`, `queue_message_during_turn`, `steer_running_turn`, `resume_session`, `permission_requests`, `denial_reason_to_agent`, `caller_assigned_session_id`, `usage_tokens`, `cost`, `file_read_tool`. Core uses a capability only when it is `supported`. |
| `ApprovalStatus` | `pending`, `approved`, `denied`, `expired`, `cancelled` |
| `DecisionSource` | `operator`, `timeout`, `agent_cancelled`, `agent_exited`, `core_shutdown`, `core_restart` |
| `PermissionDecision` | `Allow` (original input) \| `Deny { message }` |

Claude Code: everything is supported except `steer_running_turn` (unsupported, verified) and
`usage_tokens` (unknown: reported by the CLI, not surfaced). Codex: `denial_reason_to_agent`,
`caller_assigned_session_id`, `cost` and `file_read_tool` are unsupported, `steer_running_turn` is
unknown (Codex has `turn/steer`; Switchyard does not use it), the rest are supported. See
[codex-integration.md](../architecture/codex-integration.md).

## Adapter → Core (`AdapterOutput`)

| Output | Meaning | Durable? |
|--------|---------|----------|
| `Event(Ready { native_session_id, model, cwd, tools })` | Handshake. Emitted once per process. | yes, as `agent.ready` |
| `Event(Message { text, truncated })` | A complete assistant message (at most 8 KB). | yes, as `agent.message` |
| `Event(ToolStarted { tool_use_id, tool_name, input })` | The agent issued a tool call. This records intent, not effect. | yes |
| `Event(ToolFinished { outcome: Succeeded \| Failed \| Rejected, output })` | The tool result. `Rejected` means the tool never ran (denied, cancelled or interrupted). | yes |
| `Event(TurnFinished { success, interrupted, reason, duration_ms, cost_usd, turn_cost_usd, usage, … })` | End of a turn. `cost_usd` is the session's running total; `turn_cost_usd` is this turn's cost, `None` when unreported; `usage` is this turn's token counts, when reported. | yes |
| `Event(InterruptAcknowledged)` | The agent confirmed an interrupt request. | yes |
| `Event(Warning { message })` | A malformed line, an API retry and similar. | yes (at most 25 per session) |
| `Live(LiveChunk { kind: Text \| Thinking \| MessageStart \| MessageStop })` | A streaming fragment. | **no** |
| `PermissionRequested(PermissionRequest)` | The agent waits for a decision. | yes (evaluation row; approval row only for `ask`) |
| `PermissionCancelled { native_request_id }` | The agent withdrew the request. | yes |

Each session driver (`claude::spawn_session`, `codex::spawn_session`) delivers these on a bounded
channel (256 entries). The last update is always
`Exited(ExitReport { exit, stop_requested, stderr_tail, malformed_lines, failure })`; `failure` is set
when the adapter ended the session because it could not continue (for example a Codex thread that
could not be resumed), and Core then records the session as failed.

## Core → adapter (`AgentSession` handle)

| Operation | Contract |
|-----------|----------|
| `send_message(text)` | Writes a user message. Core decides whether that is a new turn (state `idle` or `interrupted`) or a queued next turn (state `running`, and only with `queue_if_busy`). |
| `interrupt()` | Requests the end of the running turn. The resulting state change arrives as `TurnFinished { interrupted: true }`. |
| `decide(native_request_id, decision)` | Answers one pending request. Fails with `UnknownPermission` if the request was already cancelled. |
| `stop()` | Closes stdin, waits `stop_grace`, sends SIGTERM, waits `term_grace`, then sends SIGKILL. Resolves with `ExitInfo` once the process is reaped. |
| dropping every handle | Treated as `stop()`; `kill_on_drop` guarantees no orphan. |

## Normalized journal events

Every agent event carries `project_id`, `workspace_id`, `session_id` and `agent_id`, plus a
`causation_id` where it applies (for example, `tool_completed` and `tool_failed` point to their
`tool_started` event).

`agent.started`, `agent.ready`, `agent.state_changed`, `agent.turn_started` (`queued`),
`agent.message`, `agent.tool_started`, `agent.tool_completed`, `agent.tool_failed` (`rejected`),
`agent.turn_completed`, `agent.permission_requested`, `agent.permission_resolved`,
`agent.interrupted`, `agent.session_resumed`, `agent.warning`, `agent.exited`, `agent.failed`, and
`files.changed`.

The draft names map onto these:

- `AgentTurnStarted` is `agent.turn_started`. It is written **before** the message goes to the agent,
  so a turn can never complete before it is recorded as started.
- `AgentExited` and `AgentFailed` are separate events. A failure always has both.

## Approvals

- Every request is classified (`claude::classify`) and evaluated. The evaluation is recorded in the
  same transaction as its outcome:
  - **`allow` and `deny`:** a `policy_evaluations` row and `agent.permission_evaluated`. The answer
    is delivered after the commit.
  - **`ask`:** additionally an `approvals` row (status `pending`, with `expires_at`,
    `evaluation_id`), `agent.permission_requested`, and the transition to `waiting_for_approval`.
- A decision is committed with a SQL guard (`… WHERE status = 'pending'`), so at most one decision
  ever wins. Only after the commit is the decision delivered to the agent. A failed delivery is
  journaled as `agent.warning` and never reported as success.
- The session returns to `running` only when **no** request of that session is still pending; the
  agent may send several in parallel.

## Differences from the milestone-1 draft

- **Permission transport.** Claude's permission requests use `--permission-prompt-tool stdio`
  control messages, not an MCP server (see the integration notes).
- **Steering.** `steer` is not part of the Claude handle. The capability is false, and the protocol
  exposes queued messages (`queue_if_busy`) instead.
- **Approve-with-modifications.** `Approve { scope: Session }` and approving with modified input are
  not offered yet. Every approval covers one tool call, with the agent's original input.
- **Sessions per project.** One attached session per project until workspace isolation (worktrees)
  existed; since milestone 3, one per workspace.
- **Codex.** Its approval requests (`item/commandExecution/requestApproval`,
  `item/fileChange/requestApproval`) map to `shell` and `file_write`/`file_edit`/`file_delete`
  operations; other server requests are `unknown` and denied. Allow is `accept`, deny is `decline`;
  `acceptForSession` and policy amendments are never sent, because human decisions do not become
  rules (ADR 0021). Codex assigns its own thread id, which `Ready` reports; Core marks a Codex
  session idle only after that identity is recorded.

## Working directory

The adapter receives a `cwd` in `LaunchSpec` and runs the agent there. It never chooses, creates or
validates a directory. Core passes the path of a validated workspace (a Core-owned Git worktree; see
`docs/architecture/workspaces.md`) and refuses to launch instead of falling back to the project
checkout.

## Conformance harness

`switchyard-fake-claude` (`crates/switchyard-core/src/bin/fake_claude.rs`) implements the same
process contract: arguments, stream-json on both pipes, `can_use_tool`, interrupt,
`control_cancel_request`, resume by ID, and exit codes. Its shapes follow the recorded real
transcripts. Scenarios are selected with a `[fake:…]` marker in the prompt: text, write, multi,
slow, delay, crash, malformed, whoami and exit. A `.fake-claude-fail` file simulates an
unauthenticated start.

`switchyard-fake-codex` (`crates/switchyard-core/src/bin/fake_codex.rs`) is the Codex equivalent: it
answers `--version`, `login status` and `app-server --help`, and `app-server` speaks the JSON-RPC
protocol with shapes taken from the recordings in
`crates/switchyard-agents/tests/fixtures/codex-0.166.0/`. It refuses any thread that is not
`untrusted` + `workspace-write`. Scenarios: text, write, edit, mixed, move, cmd, slow, pending,
perm, subagent (a child turn, which the adapter must interrupt, then a child request), newreq (unknown
request method), garbage (malformed and unknown output), crashpending, dupid (reused request id),
unmediated, whoami, remember, crash and exit. Every answer and child interrupt it receives is logged
per thread, so tests can prove that a denied operation never ran. Threads persist under
`$TMPDIR/switchyard-fake-codex/` so a new process can resume them.
