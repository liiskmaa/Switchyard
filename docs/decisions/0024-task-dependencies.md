# ADR 0024: Task dependencies form a validated acyclic graph

- Status: accepted (2026-11-13)

## Context

Work often has an order: implement after the investigation, document after the implementation. The
order must be explicit, cannot contain cycles, and must not be bypassed by an agent.

## Decision

- An edge `(task, depends_on)` in `task_dependencies` means `task` may start only when `depends_on`
  is `completed`. Edges stay within one project (trigger) and never point at the task itself
  (`CHECK`).
- Adding an edge runs a depth-first reachability check over the project's edges
  (`switchyard_tasks::would_create_cycle`) inside the same transaction; a cycle is refused with
  `invalid_params` naming both tasks.
- Readiness is derived, never stored separately: `satisfied` (all completed), `waiting`, or
  `broken` (a dependency failed or was cancelled). Publishing a draft moves it to `ready` or
  `blocked` accordingly. When a task's status changes, its `ready`/`blocked` dependents are moved in
  the same transaction. Core marks dependency blocking with a `dependencies:` reason, and only lifts
  that kind of block automatically; a blocker reported by an agent stays until the operator retries.
- Dependencies can change only before work starts (`draft`, `ready`, `blocked`, `failed`).
- The task context lists each dependency with its status and accepted result summary.

## Consequences

- No task starts before its prerequisites are accepted by the operator.
- A failed prerequisite blocks its dependents visibly (`broken`) instead of silently.
