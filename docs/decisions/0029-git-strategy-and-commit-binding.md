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
  the resolution or aborts it (`git merge --abort`). Core refuses to commit while a conflicted path that
  has no markers (one side deleted or renamed it, or a binary file) has not been explicitly staged
  (`git add`) by whoever resolved it, and while **any** file about to be committed, staged or not, has
  new conflict blocks. A conflict block is a line starting with at least seven `<` followed by a space
  or the line end, later a line of at least seven `=`, later a line starting with at least seven `>`
  followed by a space or the line end. A file has new blocks if it has more than the same path in HEAD
  and, during a merge, in `MERGE_HEAD`. Isolated marker-like lines (a heading underline, a quoted
  marker) are not blocks, and content either side already had is allowed. A file that gains a complete
  block outside a merge is refused too; to commit such content deliberately, commit it in the workspace
  with Git directly. Binary files are not scanned. The target branch is never used to resolve anything.
- **Execution** (under the per-project repository lock):
  1. re-read every fact;
  2. require `S` and `T` unchanged and no blockers;
  3. build the merge in memory with `git merge-tree --write-tree` and create it with `git commit-tree`
     (no ref moves);
  4. record `integrating` with the merge commit;
  5. move the target. If the target is not checked out: `git update-ref refs/heads/<target> <merge>
     <T>`, a compare-and-swap that Git applies atomically. If it is checked out (usually the project
     checkout): `git merge --ff-only <merge>` in that checkout. That requires no local changes to
     tracked files, and Git refuses rather than overwrite local or untracked files. The fast-forward is
     bound to the target branch: Core checks the checkout is on it at `T` immediately before, and
     afterwards that it still is and that the branch is at the merge commit (if the checkout was
     switched meanwhile, Git moved whatever was checked out; that is reported).
- **Untracked and ignored files are never replaced or removed** by a merge Core runs in a working tree
  (update from the target, the fast-forward, aborting a resolution). `--no-overwrite-ignore` does not
  stop Git's three-way merge from replacing an ignored file (directory/file clashes, directory-rename
  detection), so Core computes the paths the operation writes: for a merge, the paths the merge result
  (`merge-tree --write-tree`, the same machinery as `git merge`) changes relative to HEAD, plus
  everything the incoming side changed since the merge base; for a fast-forward, the diff; for an
  abort, everything that differs from HEAD. It refuses if any untracked file (ignored or not) is at
  one of those paths, under one of them, or where one of them needs a directory (an ancestor that is a
  file or symlink). Matching ignores case. Any error refuses.
- **Target race:** the ref compare-and-swap is used only when the target is not checked out. Core
  re-checks that right before updating it and refuses if it has been checked out since preparation; if
  it was checked out in the instant between that check and the update, the integration is recorded
  with a note that the checkout's files still show the previous commit.
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
