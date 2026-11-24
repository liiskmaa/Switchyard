# ADR 0047: Stopping work cancels the task; starting again keeps the work

- Status: accepted (2026-11-23)

## Context

Stopping a team (or a workflow) ended only the workflow record. The objective stayed "in progress"
and the team's subtasks stayed open with nothing working on them. A task cannot get a second
workflow, so it could not continue either. Its code stayed in its workspace, owned by no live task.

## Decision

1. `workflows.cancel` cancels the task too (through the same close path as `tasks.cancel`: open
   assignment cancelled, session stopped). Subtasks belong to their main task: every open task below
   it (child tasks and a team's tasks, at any depth) is cancelled too, by `tasks.cancel` as well.
   Nothing is deleted (their history stays). The task list drops finished subtasks; the main task's
   page lists them. The workspace and branch are kept, as always.
2. `tasks.start_again {task_id, keep_work = true, definition?, lead?}` creates a new task from a
   cancelled or failed one (same title, description, kind, priority, criteria and parent). It also accepts a
   task whose workflow ended while the task stayed open (that task is closed first). The new task
   starts the same kind of workflow (a team stays a team). With `keep_work`, it continues in the old
   task's workspace. That is refused while another live workflow uses the workspace or an
   integration is open on it. The new description says where the work is and lists what the
   earlier team accepted, marked as its agents' claims, plus the last goal check and where it stopped.
3. The link is derived, not stored: workflows that share a workspace, in creation order, give
   `WorkflowInfo.continues_task_id` and `continued_by_task_id`. The task page shows both. A
   cancelled task whose work nobody continues offers **Start again with this work** or
   **from scratch**. Only a team's own subtasks are refused (start the objective again).

## Consequences

- The old "cancel the workflow, then take the task over by hand" path is gone. A manual takeover
  starts again instead.
- Work is never left without an owner without the user seeing it on the cancelled task's page.
