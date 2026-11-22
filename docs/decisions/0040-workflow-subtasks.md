# ADR 0040: Reasoning splits large tasks; the workflow runs the subtasks

- Status: accepted (2026-11-22)

## Context

Large tasks should be divided into smaller subtasks, each going around the
investigate → implement → test → review loop until its review is green. ADR 0026 lets agents only
propose subtasks for the operator to decide. The operator chose that reasoning's split runs
automatically, within a limit, and that the task is delivered once.

## Decision

1. When the first investigation (`investigate#1`) of an `engineering@2` workflow reports a
   `proposal` with 2 to 8 `proposed_subtasks`, Core reads them from its record of the report and
   the workflow takes them as its **plan** (`workflow.planned`). They are not recorded as proposals
   to decide, and they are not separate M6 tasks: they are the parent task's plan.
2. Each subtask runs in order in the task's workspace: implementation → test gate → review →
   reasoning on findings, with its own rounds. Step keys carry the subtask (`s2/implement#1`); the
   step's context carries `subtask` (number, total, title, description, criteria, done ones).
3. A subtask is done when its review approves (`workflow.subtask_completed`); the next starts with
   an implementation. After the last one, the task goes to delivery (M7) once, approved by the
   operator as always.
4. More than 8 subtasks: the workflow waits; resuming implements the task as one piece.

## Consequences

- Supersedes ADR 0026 for an automatic workflow's own first plan only. Proposals from any other
  assignment still wait for the operator.
- Sequential: one subtask at a time in one workspace; no parallelism.
- The plan is visible on the task page as a checklist; the subtasks do not appear in the task list.
