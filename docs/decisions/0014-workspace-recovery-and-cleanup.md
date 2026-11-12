# ADR 0014: Workspace recovery and cleanup safety

- Status: accepted (2026-11-11)

## Decision: nothing is deleted automatically

A workspace is never deleted because:
- its session completed, stopped or failed;
- Switchyard restarted, or Core crashed;
- it is dirty;
- it is missing from Git or from the database.

Branches are never deleted. There is no `git worktree prune`, no `--force`, and nothing that
discards changes.

## Cleanup is explicit, two-step and bound to what the operator saw

1. **Archive.** Only `ready`, `dirty`, `completed` or `failed` workspaces can be archived. An
   `active` one can't. Archiving changes only the status; files stay.
2. **Cleanup check** (`workspaces.cleanup_check`). Core gathers facts:
   - the record and the derived path;
   - repository availability;
   - `git worktree list` (registered, locked, branch);
   - the directory's identity;
   - `git status` counts;
   - ignored paths;
   - the branch tip;
   - whether the branch is contained in its base ref or the project's `HEAD`, and otherwise how
     many commits are unmerged.

   `switchyard_workspaces::evaluate_cleanup` turns the facts into a report:
   - **Blockers**, which can't be overridden:
     - `not_archived`, `legacy_checkout`, `path_outside_workspace_root`, `repository_unavailable`;
     - `not_a_registered_worktree` (a directory Git doesn't know is never deleted);
     - `identity_mismatch`, `unexpected_branch`, `worktree_locked`, `status_unknown`;
     - `uncommitted_changes`, `untracked_files`.
   - **Warnings:**
     - `ignored_files_deleted`, with a sample and the total;
     - `unmerged_commits`, `branch_merged`, `merge_status_unknown`;
     - `directory_missing`, `nothing_to_remove`, `branch_missing`.
   - A `confirmation` token derived from all the facts.
3. **Cleanup** (`workspaces.cleanup {confirmation}`):
   - Under the repository lock, Core re-gathers and re-evaluates.
   - Any blocker refuses the cleanup: `dirty_workspace` for user work, `unsafe_cleanup` otherwise.
     The refusal is journaled.
   - A confirmation that differs from what the operator reviewed refuses it with `unsafe_cleanup`.
   - Otherwise:
     1. Persist `cleanup_pending` together with `workspace.cleanup_requested`.
     2. Run `git worktree remove <path>`. It is never forced, so Git itself refuses modified,
       untracked or locked trees.
     3. Verify the directory is gone, and remove the per-project directory only if it is empty.
     4. Persist `removed` together with `workspace.removed {branch_kept: true}`.
   - On failure the workspace returns to `archived`, with `cleanup_failed: …` and
     `workspace.cleanup_failed`.

Unmerged commits are a warning, not a blocker: the branch is kept, so they stay reachable.

## Amendment (hardening, 2026-11-11): the confirmation covers the whole tree

**Gap.** Git reports an ignored directory as one entry (`build/`), whatever it contains. The first
version's token was derived from Git's view only. A file added, changed or removed inside an
already-listed ignored directory after review didn't change the token, and `git worktree remove`
would delete it. Nothing re-checked the filesystem before removal.

**Mechanism** (`switchyard-core/src/workspaces/fingerprint.rs`):

1. **Full scan.** Every review scans everything under the worktree with `lstat` and never follows
   symlinks; a link's target string is hashed instead.
   - Hashed, in sorted order: relative path, file type, size, mode, owner, inode, modification time
     and change time (`ctime`, maintained by the kernel). A content edit changes `ctime` even if
     `mtime` is restored.
   - File contents are not hashed. That would cost time proportional to the size of build outputs
     and dependency trees; one `lstat` per entry costs about a second per 100k entries.
   - The fingerprint, entry count and byte count are in the report and the
     `workspace.cleanup_requested` event. The confirmation token includes the fingerprint.
2. **New blockers from the scan:**
   - `unreadable`: a directory that can't be listed, a failed `lstat`, or a file without owner read
     permission. Content we can't see is never assumed safe to delete.
   - `foreign_filesystem`: a mount point inside the tree, which removal would cross.
   - `foreign_owner` / `not_removable`: entries owned by another user, or directories without owner
     write/execute permission. Git's removal could fail **halfway** and leave a partly deleted
     worktree, so it isn't attempted.
   - `nested_repository`: a `.git` anywhere below the root. It may hold the only copy of some
     commits.
   - `too_large`: more than 2,000,000 entries, so the tree can't be fully verified. Remove the
     content yourself, or keep the workspace.
   - `tree_not_scanned`: the scan didn't run.
3. **Final check.** Right before `git worktree remove`, after `cleanup_pending` is persisted, Core
   scans again and compares with the reviewed fingerprint. On any difference or problem, the
   workspace returns to `archived` (`cleanup_failed`) with event `workspace.cleanup_failed
   {reason: "changed_before_removal"}`, nothing is deleted, and a new review is required.

**Residual risk.** Between the final scan and Git's deletion (milliseconds), a process that keeps
writing into the directory could still change it. Switchyard can't freeze other processes' writes;
no agent session can be attached, because the workspace is archived. The hash is SipHash-1-3 (64
bits) from the standard library, so an accidental collision is negligible; it isn't designed to
resist a deliberate attacker who controls the files.

**Tests** (`tests/workspaces.rs`): a new file in a listed ignored directory (the original failure),
same-size modifications, nested new directories, deletions, symlink retargeting with the outside
target left untouched, unreadable directories, nested repositories, and a change injected between
confirmation and removal through a test-only pause.

## Startup reconciliation

This runs after the sessions of the previous run are marked `failed`. It only changes statuses and
reasons, never files, and never starts agents.

| Persisted status | Finding | Result |
|---|---|---|
| `provisioning` | worktree complete and valid | `ready`/`dirty` (finished) |
| `provisioning` | anything else | `failed` (`provisioning_interrupted: …`) |
| `active`, `ready`, `dirty`, `completed` | valid | Git status refreshed; `active` becomes `ready`/`dirty` |
| same | missing dir / not a worktree / wrong repo / wrong branch / repo unavailable | `failed` with that reason |
| `cleanup_pending` | directory gone and not registered | `removed` |
| `cleanup_pending` | anything else | `archived` (`cleanup_interrupted`) |
| `archived`, `failed`, `removed` | — | unchanged (operator decides) |

A locked worktree isn't a failure; it only blocks cleanup. A `failed` workspace is recovered only
by an explicit **Re-check** (`workspaces.verify`) once the cause is fixed.

## Consequences

- **Disk use grows** until the operator cleans up.
- **Crash during provisioning:** a crash between `git worktree add` and verification can leave a
  worktree with no `ready` record. The next start either finishes it (if it is valid) or leaves it
  `failed` for inspection; it is never deleted.
- **Crash while an agent runs:** the agent exits when its stdin closes (ADR 0010). Its workspace
  and every change in it are preserved, it becomes `ready`/`dirty`, and the session can be resumed
  explicitly in the same workspace.
