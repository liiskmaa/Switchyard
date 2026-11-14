# ADR 0028: Integrations are separate from tasks; delivery is not completion

- Status: accepted (2026-11-14)

## Context

M6 takes work from investigation to an approved review, but the result still lives on a workspace
branch. Delivering it into a real branch is a different decision, with different failure modes:
the target moves, merges conflict, validation fails, Git operations fail half-way. Folding that into
task states would mix "the engineering is done" with "the change is in `main`".

## Decision

- An **integration** is its own record. It proposes delivering one workspace's committed changes
  into one target branch, and is always bound to an exact source commit and an exact target commit
  (ADR 0029). States: `open` (blockers listed) ⇄ `conflicted` → `resolving` (a merge from the target
  in progress in the workspace) → … → `ready` → `integrating` → `integrated`, plus `validating`,
  `failed` (re-preparable) and `cancelled`. Transitions are validated in
  `switchyard_integrations::IntegrationStatus::transition`, guarded in SQL by status and revision,
  and journaled (`integration.*`) in the same transaction.
- Integration-specific conditions (`out_of_date`, `validation_failed`, `uncommitted_changes`, …) are
  explicit **blockers** computed from Git facts, rather than more states.
- Task states are unchanged. A task shows its *delivery* (the integrations that reference it)
  separately; integrating does not complete a task and completing does not integrate.
- A task is eligible when its work was reported (`awaiting_review`) or completed, and no agent is
  working on it. By default a project requires an approved review of exactly the integrated content
  (ADR 0029); the operator can switch that off per project.
- One unfinished integration per workspace (unique partial index). While Core runs a step in the
  workspace (commit, update, validation) the workspace is `integrating`, which accepts no session.
- Only operator IPC methods change an integration. Agent reports and review verdicts never create,
  approve or execute one.

## Consequences

- A failed integration never marks a task as delivered; the task history and the integration
  history stay separate and both are journaled.
- A completed task whose content changed after its review cannot satisfy the review requirement
  (completed tasks cannot be re-reviewed). The operator either reviews before completing or turns
  the requirement off for the project, which is recorded.
