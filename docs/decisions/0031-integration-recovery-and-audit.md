# ADR 0031: Integration crash recovery and audit trail

- Status: accepted (2026-11-14)

## Context

Integration spans Git and SQLite, which cannot share a transaction. Core can stop at any point. The
operator must be able to tell, afterwards, exactly what happened.

## Decision

- The merge commit is created and recorded (status `integrating`, `merge_commit`, `method`, approver)
  *before* the target branch moves. Its outcome is recorded after the move, from what Git reports
  (the branch tip), not from the command's exit status alone.
- On startup, after session and workspace reconciliation, Core settles every `integrating`
  integration from the target branch:
  - **at the recorded merge commit:** `integrated` (`recovered: true`);
  - **still at the approved target commit:** `failed`, "nothing changed";
  - **anywhere else:** `failed`, the target moved, for the operator to inspect.

  Running validation runs become `interrupted` and their integrations are prepared again. A
  workspace left `integrating` settles to `ready`/`dirty`.
- Audit: `integration.created`, `.prepared` (commits and blocker codes), `.status_changed`,
  `.workspace_changed` (commits, updates, conflicts, aborts, by whom), `.validation_started`,
  `.validation_step_finished` (decision, authorizer, exit status), `.validation_finished`,
  `.approved` (exact commits, merge commit, method, approver), `.integrated` and `.failed`. All are
  journaled in the transaction that changes the record, and the merge commit's trailers repeat the
  integration, task, commits and approver in Git itself.

## Consequences

- No step is reported as atomic unless Git makes it so. The only atomic step is the ref
  compare-and-swap; the checkout fast-forward is as safe as Git's own `merge --ff-only`.
- The operator can verify any integration from the journal, the record and `git log` alone.
