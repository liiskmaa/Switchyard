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
  - **at the recorded merge commit, or containing it** (commits on top): `integrated` (`recovered: true`);
  - **still at the approved target commit:** `failed`. For a checked-out target, Core checks the
    checkout: it says "nothing was changed" only if the checkout is still on the target at that commit
    with no tracked changes and no new files where the update writes; otherwise it says the checkout
    may be partially updated and to inspect it with `git status`;
  - **anywhere else:** `failed`, the target moved, for the operator to inspect.

  It does not decide while the checkout has an `index.lock` (a Git process may still be writing): the
  integration stays `integrating` with that reason. If Git cannot be read (an error, or a killed Git),
  it stays `integrating` too; nothing is guessed. `integrations.refresh` settles it later (ADR 0032).
  A merge commit that no longer exists (never landed, then pruned) counts as not landed.

  Running validation runs become `interrupted` (with `integration.validation_finished`) and their
  integrations are prepared again, or `failed` if that is impossible (for example the target branch was
  deleted), never left `validating`. A workspace left `integrating` settles to `ready`/`dirty`, and
  recovery restores an operator's `completed` mark recorded by the interrupted operation.
- Audit: `integration.created`, `.prepared` (commits and blocker codes), `.status_changed`,
  `.workspace_changed` (commits, updates, conflicts, aborts, by whom), `.validation_started`,
  `.validation_step_finished` (decision, authorizer, exit status), `.validation_finished`,
  `.approved` (exact commits, merge commit, method, approver), `.integrated` and `.failed`. All are
  journaled in the transaction that changes the record, and the merge commit's trailers repeat the
  integration, task, commits and approver in Git itself.

## Consequences

- Git's answers are read by exact exit code: "not an ancestor", "no such branch" or "no such object"
  only from the exit status Git documents for them, never from a failure without output (a killed
  Git).
- No step is reported as atomic unless Git makes it so. The only atomic step is the ref
  compare-and-swap; the checkout fast-forward is as safe as Git's own `merge --ff-only`.
- The operator can verify any integration from the journal, the record and `git log` alone.
