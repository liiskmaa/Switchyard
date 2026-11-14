# ADR 0023: Tasks are independent of sessions; assignments connect them

- Status: accepted (2026-11-13)

## Context

Milestone 6 coordinates several native agents on one piece of work: one agent investigates, another
implements, a third reviews. Sessions are processes with their own lifecycle (they crash, stop,
resume, end with Core). Work must outlive any of them, and the operator must be able to see who
did what, when, and with which instructions.

## Decision

- A **task** is a durable row (`tasks`) with its own state machine, owned by Core and changed only
  through `TaskService` (rows and `task.*` events in one transaction):
  `draft → ready|blocked → assigned → in_progress → awaiting_review ⇄ in_review → completed`, plus
  `failed` (retryable) and `cancelled` (terminal). Transitions are validated in
  `switchyard_tasks::TaskStatus::transition` and guarded in SQL on the current status.
- An **assignment** (`task_assignments`) gives one role (`investigate`, `implement`, `review`) on one
  task to one provider in one workspace. Core starts (or resumes) a session for it, stores the
  context it sent, and records the outcome (`reported`, `failed`, `cancelled`). A task has at most
  one open assignment (unique partial index); a workspace has open assignments of at most one task.
- Capability checks: a provider can be assigned only if it is available and its
  `permission_requests` capability is `supported` (ADR 0022). Both current providers qualify.
- Core follows sessions through the journal (`agent.turn_completed`, `agent.exited`,
  `agent.failed`) with a bus subscription that starts at the lowest `last_event_seq` of the active
  assignments. Each reaction advances that mark in the same transaction as its writes, so replay
  after a restart is idempotent. A session that ends before reporting fails its assignment.
- Completing, failing, cancelling and retrying are operator decisions only. No agent output changes
  a task to `completed`.
- Protocol: `tasks.*` methods, `task.*` events, `Session.access` and `events.list {task_id}` are
  additive, so the protocol stays at version 4 (ADR 0015).

## Consequences

- The task history survives crashes and restarts; an interrupted assignment is visible as failed,
  and the operator retries it (resuming the earlier session when the provider supports it).
- One workspace serves one task at a time. Parallel work needs separate workspaces; integration of
  their branches stays manual (unchanged from Milestone 3).
