# ADR 0032: Core-owned integration operations

- Status: accepted (2026-11-14)

## Context

Core runs each IPC request as a task of the client's connection. When the client disconnects (the
desktop app quits, the socket drops) or Core closes connections at shutdown, those tasks are aborted,
and Git child processes are killed with them (`kill_on_drop`). For an approval that means Git could be
stopped in the middle of fast-forwarding a checkout, with nothing recorded and the integration left
`integrating` until the next start. Workspace steps had the same problem and could leave the workspace
locked (`integrating`).

## Decision

Every integration step that changes Git is an **operation** owned by Core, not by the request:

- **Kinds:** `approve`, `commit_workspace`, `update_from_target`, `abort_resolution`, `validate`
  (a whole validation run).
- **Durable identity:** before any work starts, Core inserts a row in `integration_operations`
  (`running`, who asked, when) and journals `integration.operation_started` in the same transaction.
  When the work ends, whatever the outcome, it records `completed`, `failed` (with the error) or
  `interrupted`, with `integration.operation_finished`. The integration carries its latest operation
  (`Integration.operation`), so a client that reconnects sees what is running and how it ended.
- **The end is atomic with the result.** The transaction that commits an operation's final change to the
  integration also ends the operation:
  - `approve`: recording `integrated` or `failed`, or the refusal's re-preparation;
  - `validate`: leaving `validating` (as prepared again, or as `failed` if that is impossible);
  - workspace steps: the preparation after the Git step, `failed` with the step's error if it failed.

  So no client can observe that final state with its operation still `running`. In particular, an
  integration is `integrated` only together with a finished `approve` operation, and leaves `validating`
  only together with a finished `validate` operation. The supervisor ends an operation itself only when
  its work stopped without such a commit (an error before any change, a panic, a failed final commit).
  In those cases the integration may still be `integrating`/`validating` with its operation `failed`;
  that state is settled from Git by `integrations.refresh` or at the next start, never left running.
- **Supervision:** the work runs in a task Core spawns and tracks. The request only waits for its
  result. If the request is aborted (client disconnect), the work continues. If the work panics, Core
  releases the workspace, settles the integration from Git, and records the operation `failed`.
- **No duplicates:** at most one operation per integration, enforced by an in-memory claim and by a
  unique partial index on running rows. A retried request gets `conflict` and never starts the same work
  twice.
- **Locks:** each operation holds the project's repository lock while it touches Git, and marks the
  workspace `integrating` through `begin_workspace_step`, which also records the workspace's previous
  status in the operation row. Every path releases both, including errors and Git timeouts.
- **Explicit cancellation** exists only for validation runs (`integrations.cancel_validation`), which
  stops their process groups. An authorized Git step is never cancelled half-way on purpose.
- **Shutdown:** Core stops accepting operations, cancels running validation runs (process groups get
  SIGTERM, then SIGKILL), and waits up to `operation_shutdown_grace` (60 s by default) for the rest.
  Operations still running after that are stopped with Core and stay recorded `running`.
- **Restart:** operations left `running` were interrupted (by a crash or by the shutdown grace). Core
  records them `interrupted`, restores an operator's `completed` mark on the workspace if the operation
  had replaced it, and settles each `integrating` or `validating` integration from Git (ADR 0031).
- **Settling on request:** an integration left `integrating` or `validating` with no operation of this
  Core working on it can be settled from Git again with `integrations.refresh`. That is the supported
  retry path when recovery could not decide at startup.

## The cases this distinguishes

| Event | What happens to a running operation |
|---|---|
| Client disconnect, request aborted | Nothing: it continues; the client sees it on reconnect. |
| Explicit cancellation | Validation runs only: their commands are stopped and the run is `cancelled`. |
| Core shutdown | Validation runs are cancelled; other operations get the shutdown grace, then are stopped and settled at the next start. |
| Core crash | Git children may be killed or orphaned; at the next start the operation is `interrupted` and the integration is settled from Git. |
| Git timeout | Git is stopped; the step fails with what Git shows now. For a checkout, Core says it is unchanged only if it checked that. |
| Partial checkout update | Reported as possibly partially updated, with `git status` as the instruction. Core does not repair or undo it. |
| Successful recovery | The integration is `integrated` if the target branch contains the recorded merge commit. |

## Consequences

- Closing the app or losing the connection cannot cut an authorized integration short.
- A shutdown can take up to the grace period while Git finishes. A Core killed (or a grace that runs
  out) still leaves a checkout possibly partially updated. That is reported, not repaired.
- Panics inside an operation are contained, but a panic is still a bug: the state is re-read from Git,
  not continued.
