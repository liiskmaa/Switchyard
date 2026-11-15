# ADR 0035: Agent organizations

- Status: accepted (2026-11-15)
- Supersedes ADR 0026 for work inside an organization; ADR 0026 still governs ordinary tasks.

## Context

M8.1 runs one fixed sequence (investigate → implement → review → correct). Real objectives need different
teams: a CSS fix needs one implementer and one reviewer; a feature with documentation needs a lead,
specialists and perhaps a documentation supervisor with its own writers. The user should state the
objective once and see a team organize itself. That must not become unlimited autonomy, a hard-coded
"Codex supervises, Claude executes" pairing, or a weakening of M4/M5/M7 safety.

## Decision

1. **Three separate models.**
   - **Organization**: agents (identity, title, kind `supervisor`/`specialist`, responsibility, scope,
     capabilities, provider/model with the selection rationale, reporting line, status).
   - **Task graph**: organization tasks (the objective, `coordination` tasks owned by supervisors, and
     work: `implementation`, `investigation`, `documentation`, `testing`, `review`), each backed by an
     M6 task with dependencies and acceptance criteria.
   - **Runtime**: M6 assignments and sessions. Nothing in the organization refers to a process.

   The pure state and decision function live in `switchyard_tasks::organization`; Core stores the
   state (`organizations.state`) and commits each decision with its events, subtasks and shared
   context in one transaction before it acts.
2. **Hierarchy without a fixed depth.** Agents form a tree through `reports_to`; the model has no
   depth limit. Depth, agents, tasks, corrections, retries, supervision turns, wall-clock minutes, and
   cost/tokens where reported are **operational limits** in the project's settings, enforced by Core.
3. **Supervisors decide within their authority.** A supervisor answers each supervision turn with a
   `switchyard-org` directive: agents to add, tasks to delegate, assessments of reported work (accept,
   correct, reassign, cancel), context notes, an escalation, or that its scope is done. Core validates
   the whole directive against the organization and its limits and applies it atomically, or rejects it
   with the reason and asks the same supervisor again (twice), then waits for the user. A supervisor can
   only act inside its subtree: add agents reporting to itself or its subordinates, delegate under
   coordination tasks it owns, assess work under them.
4. **Delegation without operator approval, inside the organization.** A directive's tasks become M6
   subtasks through the existing proposal path: a `delegation_request` message on the coordination
   task, a `task_proposals` row decided by `organization:<supervisor>`, a task with
   `origin: proposal`. This replaces ADR 0026's "only the operator decides" **for organizations only**:
   the decision is Core's, bounded by the limits and recorded with its author.
5. **Reports are still claims.** A specialist's report becomes a checkpoint for its supervisor together
   with **evidence Core verified itself**: the files changed in the workspace since the organization
   started (Git, untracked included) and those outside the task's scope. The shared context labels
   every entry `verified` (Core, the user), `claim`, `proposal`, `decided` (a supervisor's decision) or
   superseded. A supervisor's acceptance completes a **subtask** (`accepted_by organization:<key>`);
   the **objective** is completed only by M7 delivery, which the user approves (ADR 0034).
6. **Disagreement is recorded, not escalated by default.** A specialist that disagrees ends its turn
   with a `proposal` report (alternative + evidence); it reaches its supervisor as a disagreement in the
   shared context, and the supervisor decides. Only an explicit escalation reaches the user.
7. **Provider-neutral selection.** A profile per installed provider/model: runtime capabilities
   *verified* for the adapter (permission routing, resume, interrupt, steering, streaming), models
   *reported* by the CLI, skills *configured* by the user (no built-in estimates: without them every
   model is "unknown"). Selection order: the user's override for a required skill, the supervisor's
   request if available and eligible, else the best score on the user's estimates with a preference for
   a provider different from the implementer when reviewing; the rationale is stored on the agent.
   Supervisors need verified resumption; every agent needs verified permission routing.
8. **Delivery is M7.** When the lead reports its scope done, Core first checks the project's
   review requirement: an approved review in the organization must have seen exactly the current
   content (ADR 0029); otherwise the directive is rejected with that reason. Then the workflow enters
   the M8.2 delivery stage unchanged. M7's review fact now looks for the latest review in the task's
   subtree, still bound to the exact content.
9. **Workflow definition `organization@1`.** Started through `workflows.start` (`definition:
   organization`, optional `lead`); pause, resume and cancel go through `workflows.*`; migration 0010
   rebuilds `task_workflows` (nothing references it) to allow the definition.

## Consequences

- One task, one start: the lead plans, delegates, supervises and adapts; the user answers escalations
  and approves delivery.
- No new authority for agents: every tool request still goes through M4 policies; organization review
  work runs in read-only review sessions; nothing commits, approves or pushes on its own.
- M6 gained two narrow rules: `ready → in_progress` for tasks an organization coordinates, and review
  assignments that review the shared workspace from `ready` (`workspace_review`).
- Agents can still ignore the directive format; a rejected or missing directive is retried twice, then a
  person decides.
