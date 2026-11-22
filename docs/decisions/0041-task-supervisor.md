# ADR 0041: A supervisor on each task answers the operator and routes requested work

- Status: accepted (2026-11-22)

## Context

The operator wanted one place on a task to ask questions ("why did the tests fail?") or ask for
work ("have review check the error handling again"), with a supervisor deciding which role or
model should handle it. Before this, the only levers were Resume, a note to the working agent, and
manual assignments. The operator chose: the routed work starts right away; the box lives on each
task's page; work asked for on a delivered task opens a follow-up task.

## Decision

1. **The supervisor is a read-only agent session** (`SessionAccess::Review`) using the task's
   *Investigation & reasoning* binding (the workflow's snapshot, else the project's roles). It runs
   in **its own workspace** (`supervisor-<task>`, branched from the task's branch), because a
   workspace has at most one live session and the task's agent may be working in its own. The
   task's uncommitted changes reach it as a bounded diff in the prompt. The next question resumes
   its last session when the provider supports it; the session is stopped after each answer.
2. **Facts come from Core.** The prompt (`switchyard_tasks::supervisor::render_prompt`) carries
   the task, the workflow state and any open question, the steps, recent reports (as claims), open
   findings, the latest test run, the diff, the last six exchanges, and which routes the workflow
   refuses right now and why.
3. **The answer may end with one `switchyard-supervisor` block**: `{route, instruction, title?}`
   with `route` one of `none`, `reasoning`, `coding`, `review`, `tests`. A missing block is only an
   answer; a malformed one routes nothing (and says why). The block is a request, never an
   authorization.
4. **Core routes only what the workflow allows**:
   - an agent is working (or waiting on its open step) and the route names *its* role: the
     instruction is sent to it as an operator note (`tasks.note`), which also answers an open
     question (`Input::Answered`);
   - an agent is working and the route names another role: refused ("ask again when it reports");
   - nothing is working: the pure `Input::Directed { target }` starts a new round of that role
     (`investigate#n` is a triage step, `implement#n`, `review#n`) with the instruction as a pending
     note, or reruns the test gate for the current implementation (`workflow.directed`). Refused
     while the tests run or the delivery validates or integrates;
   - the task is delivered (or completed): a new task linking back to it is created and its own
     engineering workflow starts; the delivered task never changes.
5. **Never**: push, deploy, merge, approve an integration, answer a permission request, or route
   work on an organization (its lead decides). One open question per task; a Core restart records
   the unanswered one as interrupted.
6. Storage: `task_supervisors` (workspace, last session) and append-only `supervisor_messages`
   (migration 0013). Protocol (additive, v4): `supervisor.ask`, `supervisor.get`; events
   `supervisor.asked`, `supervisor.answered`, `workflow.directed`.

## Consequences

- One extra worktree per task the operator asks about; like every workspace, it is never deleted
  automatically (clean it up from Workspaces).
- A redirect uses a new round, so it counts toward the two-round limit after which the workflow
  waits for the operator.
- The supervisor sees committed work in its workspace and the rest only through the diff in its
  prompt.
