# Architectural decisions

Each significant decision is recorded as an ADR in [`docs/decisions/`](../decisions/). Summary:

| ADR | Decision | Key trade-off |
|-----|----------|---------------|
| [0001](../decisions/0001-separate-core-process.md) | Orchestration lives in a separate, long-running `switchyard-core` process; the desktop is a client that can launch it but never owns it. | Two binaries and a protocol to version, in exchange for agent sessions that survive the UI. |
| [0002](../decisions/0002-unix-socket-json-lines.md) | IPC over a `0600` Unix domain socket with peer-UID check; newline-delimited JSON frames; versioned `initialize` handshake. | Simple and inspectable; macOS/Linux only until a named-pipe transport exists. |
| [0003](../decisions/0003-sqlite-persistence.md) | SQLite (bundled) in WAL mode, `synchronous=FULL`, a single writer connection, migrations embedded and tracked with `user_version`. | One fsync per commit; trivial ordering and no pool. |
| [0004](../decisions/0004-event-engine.md) | Persist-then-publish; transactional outbox (`publish_with`); total order by `seq`; bounded broadcast with lossless catch-up from the journal. | Durable events cost an fsync; high-rate agent output may need a separate channel later. |
| [0005](../decisions/0005-git-cli.md) | Inspect repositories through the `git` CLI (`--porcelain=v2`, no optional locks, no prompts, timeouts). | Requires `git` installed; matches exactly what agents see. |
| [0006](../decisions/0006-crate-layout.md) | No empty `agents`/`permissions` crates; `switchyard-git` instead of `switchyard-workspaces` for now; a separate `switchyard-client`; per-crate integration tests. | Deviates from the initial proposal for concrete reasons. |
| [0008](../decisions/0008-claude-code-stream-json.md) | Claude Code runs as one long-lived `claude -p` process per session, with stream-json on stdin and stdout; permission prompts arrive as `can_use_tool` control requests over stdio. | Relies on control messages that are verified but undocumented for the CLI; pinned by fixtures and a fake CLI. |
| [0009](../decisions/0009-durable-vs-live-output.md) | Durable journal for agent activity; a lossy, bounded live channel for token streams, with an in-progress snapshot for reconnecting clients. | Fragments streamed while disconnected are not replayed. |
| [0010](../decisions/0010-agent-process-ownership.md) | Agents are children of Core only. Graceful shutdown stops them. After a crash the process is not re-adopted: the session is marked `failed` and must be resumed explicitly. | No hot recovery after a Core crash. |
| [0011](../decisions/0011-agent-workspace-boundary.md) | `--restricted`, `--strict-mcp-config`, an explicit tool list, and an `ask` rule for every enabled tool (Bash only until [0017](../decisions/0017-normalized-operations.md)), so every request reaches Switchyard's policy evaluation. | No OS-level sandbox is claimed. |
| [0012](../decisions/0012-project-workspace-session.md) | Project (registered repo) → workspaces (Core-owned worktrees) → sessions; one live session per workspace, many per project; `sessions.start` requires a workspace; M2 checkout records become `legacy_checkout`. | One documented incompatible change inside protocol v1. |
| [0013](../decisions/0013-worktree-location-and-branches.md) | Worktrees in `<data dir>/workspaces/<project>/<workspace>`, branch `switchyard/<workspace-id>`, identity verified before every use, branches never reused. | Moving the repository breaks the links until it is moved back or repaired. |
| [0014](../decisions/0014-workspace-recovery-and-cleanup.md) | Nothing is deleted automatically; cleanup is archive → checked report → confirmation bound to the inspected state → `git worktree remove` without force; branches kept; reconciliation only changes statuses. | Disk use grows until the operator cleans up. |
| [0015](../decisions/0015-protocol-version-2.md) | Protocol 2 with minimum 2 on both sides; old clients and old Cores are refused during `initialize` with actionable, structured errors. | A running old Core must be stopped during upgrades. |
| [0016](../decisions/0016-approval-policy-model.md) | Approval policies: system restrictions, then the most restrictive matching rule (deny > ask > allow), then built-in defaults, then caps that turn `allow` into `ask`; project and workspace scopes; global revisions; protocol 3. | A broad `ask` makes narrower `allow` rules ineffective; no session scope or global rules. |
| [0017](../decisions/0017-normalized-operations.md) | Every Claude tool goes through Switchyard (`ask` rules for all enabled tools); requests are classified into adapter-neutral operations; Bash is always `shell`; unknown, disabled and malformed requests are denied. | One evaluation and event per read; a new CLI field turns allows into asks until it is classified. |
| [0018](../decisions/0018-policies-and-human-approvals.md) | One evaluation path for every request, recorded before answering; human decisions never become rules; pending approvals stay bound to their evaluation and can only be denied by later changes. | An operator who just allowed something still has to decide requests that were already pending. |
| [0019](../decisions/0019-policy-threat-model.md) | Policies are authorization and workflow, not a sandbox; the protections and the non-protections are listed. | Allowed commands keep the user's full privileges. |
| [0007](../decisions/0007-toolchain-and-versions.md) | Tauri 2.12 (not 3-alpha), Rust 1.99.1 pinned, React 19.3, Vite 8, TS 5.9. | Conservative TypeScript choice. |

## Decisions deliberately deferred

- **How Core is started at login** (LaunchAgent vs. on-demand from the desktop). It is currently on
  demand only.
- **Integration of workspace branches** back into the project (merge, rebase, review). Workspaces
  have a reserved `integrating` status; nothing merges automatically.
- **OS-level sandboxing** of agent commands. Worktrees isolate source trees only.
- **Global (cross-project) policy rules, session-scoped rules and "always allow" shortcuts.**
  Milestone 4 has project and workspace rules only (ADR 0016).
