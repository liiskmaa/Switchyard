# ADR 0029: Merge commits, up-to-date sources, and approvals bound to exact commits

- Status: accepted (2026-11-14)

## Context

The operator must be able to approve *exactly* what lands, and Switchyard must never overwrite
uncommitted work, run destructive Git commands, or silently use a newer commit than the one
approved. Agents leave their work uncommitted (M6 tells them not to commit).

## Decision

- **Strategy: always a merge commit (no fast-forward)** whose parents are the approved target commit
  `T` and source commit `S`. Squash merges would hide the source commits, and plain fast-forwards
  would leave no record of the integration. The merge commit carries trailers
  (`Switchyard-Integration`, `-Task`, `-Source`, `-Target-Base`, `-Approved-By`), so Git history alone
  traces it.
- **The source must contain the target** (`T` is an ancestor of `S`). Then the merge result's tree is
  exactly `tree(S)`. Validation and review of `S` therefore cover precisely what lands, and integration
  cannot conflict. If the target moved, the operator **updates the workspace from the target**
  (`git merge --no-ff T` inside the Core-owned worktree). Conflicts stay in the workspace (`resolving`)
  for the operator, or an agent assigned to that workspace, to resolve; the operator then commits
  the resolution or aborts it (`git merge --abort`). Core refuses to commit a resolution while a file
  still contains conflict markers, or while a conflicted path that has no markers (one side deleted or
  renamed it, or a binary file) has not been explicitly staged (`git add`) by whoever resolved it. The target branch is never used to resolve anything.
- **Execution** (under the per-project repository lock):
  1. re-read every fact;
  2. require `S` and `T` unchanged and no blockers;
  3. build the merge in memory with `git merge-tree --write-tree` and create it with `git commit-tree`
     (no ref moves);
  4. record `integrating` with the merge commit;
  5. move the target. If the target is not checked out: `git update-ref refs/heads/<target> <merge>
     <T>`, a compare-and-swap that Git applies atomically. If it is checked out (usually the project
     checkout): `git merge --ff-only <merge>` in that checkout. That requires no local changes to
     tracked files, and Git refuses rather than overwrite local or untracked files. Core also checks,
     before both merges it runs in a working tree (update from the target, fast-forward), that no
     ignored file would be overwritten (`--no-overwrite-ignore` alone does not stop Git's three-way
     merge from replacing an ignored file), and that the target checkout is still on the target
     branch at `T`.
- **Commit binding:** an approval names `S` and `T`. A request with other commits, or any change
  detected under the lock, refuses the approval and re-prepares the integration. Preparing again
  clears any recorded approver.
- **Review binding:** when a review starts, Core records the Git tree of the workspace's whole
  content (tracked files as on disk plus untracked, non-ignored files), computed with a private index
  file so nothing changes. The review requirement holds only if the latest review approved and its
  recorded tree equals `tree(S)`.
- **Committing agent work** is an explicit operator action (`git add -A` + `git commit` in the
  workspace). Ignored files are never included.
- All Git writes run with `core.hooksPath=/dev/null`: an integration never runs repository hook
  scripts. Validation is the explicit, authorized way to run project code (ADR 0030). Nothing pushes,
  rebases, resets, force-updates or deletes branches.

## Consequences

- An approval is only ever for the commits the operator saw. Movement on either side is caught, even
  between the check and the ref update (compare-and-swap).
- The operator usually integrates into the branch checked out in the project checkout. That checkout
  must be clean (untracked files are allowed, and protected by Git), and it moves forward to the
  merge commit.
- The checkout fast-forward is a Git operation that updates files and then the ref. If it fails,
  Git leaves the checkout unchanged; it is not claimed to be atomic beyond what Git provides.
- Repository hooks (for example `pre-commit`) do not run for Switchyard's merge commits. Configure
  equivalent validation commands instead.
