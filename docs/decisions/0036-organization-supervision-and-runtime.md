# ADR 0036: Supervision and execution in an organization

- Status: accepted (2026-11-15)

## Context

Supervisors must not disappear after delegating, but calling a model on every event is wasteful and
the existing runtime has firm limits: one live session per workspace (ADR 0012/0013), one task's open
assignments per workspace, resumable sessions, interruptible turns, messages queued for the next turn.
Neither CLI's mid-turn steering is verified (Claude: unsupported; Codex: `turn/steer` unverified).

## Decision

1. **One session at a time per organization.** An organization works in one workspace (a Core-owned
   worktree). Core alternates: a work turn (an M6 assignment of the specialist), then a supervision turn
   (the supervisor's own read-only session, resumed so it keeps its conversation), then the next step.
   The limit `max_concurrent_sessions` exists and must be 1. Hierarchy does not imply parallelism;
   parallel work needs one workspace per branch of the tree and merging, a later milestone.
2. **Event-driven checkpoints.** A supervisor is called when something in its scope needs it: a plan,
   reported work (with verified evidence), an observation, an escalation, an answer, a rejected
   directive, or "every subtask finished". Checkpoints for the same supervisor and scope are merged.
3. **Continuous observation without a model.** During a work turn Core watches the journal: a file
   change (Claude `Write`/`Edit`/`NotebookEdit`, Codex `apply_patch`, from `agent.tool_started` or
   `agent.permission_requested`) outside the task's scope, or three failed tool operations, is an
   observation. Core records it as verified context and **interrupts the turn at once** (the
   assignment is cancelled and its session stopped; a pending approval is denied by the stop). The
   supervisor decides at its checkpoint. A permission request is never answered by this path: the
   M4 evaluation is unchanged.
4. **Instructions reach the next turn.** A correction or reassignment is delivered as
   `organization.instructions` in the next work turn's task context (context v2), which continues the
   specialist's conversation when the CLI can resume it. `organization.work_started` records exactly
   which instructions were sent. Switchyard never claims to steer a running turn.
5. **Questions and unusable turns end the turn.** A specialist's question or a turn without a usable
   report becomes a checkpoint (one automatic retry first for unusable turns) and its assignment is
   ended, so the workspace is free for the supervisor.
6. **Restarts.** A turn running when Core stops did not survive: at startup the organization waits for
   the user, before the journal replay, so a replayed end of that turn changes nothing. Agents are never
   restarted automatically (as in M8.1).
7. **Bounded.** Directive repairs (2), corrections per task, retries, supervision turns, minutes,
   cost/tokens where reported; reaching one makes the organization wait with the reason. Duplicate task
   titles under one coordination task, unknown or out-of-scope owners, circular or missing
   dependencies and agents beyond the depth are rejected.

## Consequences

- The user sees what runs now, who decided what and why, and every intervention, in the journal and the
  shared context.
- Throughput is sequential; supervision adds a model turn per checkpoint. Both are stated in the UI.
- Fakes cannot show real model collaboration; tests prove the machinery with scripted directives.
