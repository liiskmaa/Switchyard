# ADR 0033: Workflow orchestration and role/model bindings

- Status: accepted (2026-11-15)

## Context

Through M7 the operator was the orchestration engine: every investigation, implementation, review
and correction was a manual `tasks.assign`. Assignment creation only differs in *who decides what
runs next*; the M6 task engine (state machine, reports, task context with review findings, session
reuse, read-only reviews) and the M4/M7 safety rules already supported every step. The assignment UI
also offered a provider but never a model, although both adapters accept one.

## Decision

1. **A built-in, versioned workflow `engineering@1`**, no workflow designer. Its decisions are a pure
   function (`switchyard_tasks::workflow::decide`): state + input → new state + one action. Sequence:
   investigation → implementation → independent review → correction rounds (implement again, with
   the open review findings in the task context) → re-review → **stop at the human delivery gate**.
2. **Limits**: at most 2 automatic correction rounds; 2 automatic repair notes for a turn without a
   usable report; 1 automatic retry of a failed session with the *same* provider and model (no
   fallback). A rejected or blocked review, a blocker, an exhausted limit, a cancelled assignment,
   an agent's question and a Core restart all put the workflow in `waiting` with the reason and what
   *Resume* will do. Nothing is ever read as success without a usable report.
3. **Steps are assignments.** The workflow creates them through the same code as the operator
   (`create_assignment`, `start_assignment`), with `origin = workflow` and a unique step key
   (`investigate#1`, `implement#2`, retries `review#1@2`). Policies, approvals, read-only reviews and
   workspace rules apply unchanged.
4. **Durability and idempotency.** One `task_workflows` row per task (migration 0008). The driver
   follows the journal like the task reactor; a decision, its `workflow.*` events, the processed
   journal position and the next step's assignment are **one transaction**, and the session starts
   only after the commit. A unique index on `(task_id, step_key)`, a per-task lock and a revision
   guard make replays and concurrent inputs harmless. After a restart, a step whose session was
   running is *interrupted*: the workflow waits; agents are never restarted automatically. A step
   created but never started is started.
5. **Role bindings.** A project stores provider + model per role (investigate, implement, review,
   correct; correct defaults to implement). A task can override roles when the workflow starts; the
   effective bindings are snapshotted on the workflow, so later setting changes do not alter it. The
   model is persisted on every assignment and passed to the native adapter exactly (`--model` for
   Claude Code, `model` on Codex's `thread/start`); the model the CLI reports is shown next to it. A
   resumed correction session is reused only for the same model.
6. **Model choices come from the installed CLI**, never a Switchyard catalogue: Claude Code's
   `--model` aliases as its `--help` names them (2.1.325: `fable`, `opus`, `sonnet`) plus a full
   `claude-…` name; Codex's app-server `model/list` (0.166.0). An unlisted model is refused before
   anything starts; Switchyard never passes `--fallback-model`, so an unknown full name fails the
   session instead of being replaced. "No model" is the explicit choice "CLI default".
7. **Role capabilities** are checked against the existing provider capability declarations (ADR
   0022): every role needs `permission_requests` (policies and read-only reviews depend on it).
   Implementation and review on the same provider is allowed with a warning.
8. **The operator keeps control**: pause, resume, cancel; manual assignment is a takeover, refused
   while the workflow runs. The workflow never completes a task, prepares or approves an integration,
   commits, pushes or deploys (ADR 0028/0029 unchanged).

## Consequences

- One task with *Start* runs to the delivery gate without manual assignments; the operator only
  answers permission requests and exceptional states.
- The task status still moves through `awaiting_review` between automatic steps (the M6 state
  machine is unchanged); the UI shows the workflow stage as primary and labels the task status as
  internal.
- Sequential only: one open assignment per task. Parallel subtasks (M8.3) build on the same driver.
- No provider fallback, no stage timeouts yet (M8.4). A hung agent turn waits until the operator acts.
- Adding a provider means a capability declaration and a model source in its adapter; the role
  binding format does not change. The `provider` CHECK constraint on `task_assignments` still needs a
  migration for a third provider.
