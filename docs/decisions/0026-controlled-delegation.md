# ADR 0026: Delegation by proposal only

- Status: accepted (2026-11-13)

## Context

An investigating agent often finds that work should be split. Letting agents create or assign
tasks would allow recursive spawning, cost blow-ups and work nobody asked for.

## Decision

- An agent can only *propose* subtasks, in a `proposal` report (`proposed_subtasks`: title,
  description, kind, acceptance criteria, `depends_on` by index of an earlier proposed subtask or by
  existing task id). Each one becomes a `task_proposals` row in `pending`.
- Only the operator decides (`tasks.decide_proposal`), exactly once (SQL guard). Approving creates a
  **draft** task (`origin: proposal`, parent = the proposing task, the parent's priority) with the
  dependencies validated like any other (same project, no cycles). A subtask that depends on an
  earlier proposal requires that proposal to be approved first.
- Drafts must still be published and assigned by the operator. Nothing runs automatically.
- Agents are told, in the context, not to start sub-agents or create tasks. Codex sub-agent turns
  are still interrupted and their requests denied (ADR 0021).

## Consequences

- Delegation is bounded: at most 10 proposals per report, and each needs a human decision to exist
  and another to run.
- There are no swarms, recursive spawning or agent-to-agent assignment.
