# Change integration (milestone 7)

Integration takes work from an isolated workspace into a real branch: you inspect it, validate it,
and approve exact commits, and Core performs a recorded, recoverable Git operation. Nothing is
pushed or deployed. Decisions: ADRs 0028–0031.

## The workflow

1. **Prepare.** From a task (Tasks → Delivery → *Prepare integration*) or a workspace. Core binds
   the integration to the workspace branch's current commit `S` and the target branch's current
   commit `T` (by default the branch checked out in the project checkout). It reads every fact from
   Git and lists **blockers**.
2. **Commit the workspace.** Agents leave their work uncommitted. *Commit workspace* commits
   everything (tracked changes plus untracked, non-ignored files) on the workspace branch. The
   *Changes* panel lists the uncommitted files first, so you see what will be committed.
3. **Inspect.** Commits `base..S`, files (added, modified, deleted, renamed, binary) with line
   counts, and the patch per file. All of it comes from Git: `git log`, `git diff -M`.
4. **Update from the target**, if `T` has commits `S` lacks. Core merges `T` into the workspace.
   A clean merge creates a merge commit on the workspace branch. Conflicts leave the merge in progress
   in the workspace (`resolving`): resolve them yourself or assign an agent to that workspace, then
   *Complete resolution* (refused while conflict markers remain), or *Abort*.
5. **Validate.** The project's commands run in the workspace for exactly `S`. Each is evaluated by
   the policy: *ask* commands run only if you authorize them for this run, *deny* never runs.
6. **Approve.** The approval view shows the project, task, workspace and branch, the target, the
   **full** `S` and `T`, the changes, review and validation status, conflicts, and what approving
   does. Approving sends exactly those two commits.
7. **Integrate.** Under the repository lock, Core:
   - re-checks everything;
   - creates the merge commit (parents `T`, `S`), recording it before anything moves;
   - moves the target by compare-and-swap, or fast-forwards its clean checkout;
   - records the result as Git reports it.

## Blockers

| Code | Meaning |
|---|---|
| `workspace_invalid` | The worktree is missing, on the wrong branch, or the workspace is archived/failed |
| `agent_working` | A session is attached to the workspace |
| `workspace_busy` | A rebase, cherry-pick or `index.lock` in the workspace |
| `uncommitted_changes` | The worktree differs from its last commit (including a merge in progress) |
| `no_changes` | The source adds nothing to the target |
| `out_of_date` | The target has commits the source lacks (merge would be clean) |
| `conflicts` | Merging the target into the source conflicts (files listed) |
| `target_checkout_dirty` / `_busy` | The checkout of the target branch has local changes, or an operation in progress |
| `identity_missing` | Git has no committer identity for the merge commit |
| `task_not_eligible` | The task is not awaiting review or completed, or an agent works on it |
| `review_required` | No approved review of exactly `tree(S)` (and the project requires one) |
| `validation_required` / `validation_failed` | Commands configured and no passing run for exactly `S` |

## Guarantees

- **Exact commits.** An integration lands only the `S` and `T` you approved. Any movement refuses
  the approval and re-prepares; the ref update itself is a compare-and-swap.
- **What was validated and reviewed is what lands.** `S` contains `T`, so the merge tree is
  `tree(S)`. Validation passed for `S`, and the approved review recorded `tree(S)`.
- **No overwrites.** Core never touches the target branch to resolve conflicts. It integrates into
  a checked-out branch only when that checkout has no local changes to tracked files and is still on
  the target at `T`. Git refuses rather than overwrite untracked files, and Core refuses any merge that
  would replace an ignored file (for example `.env`). It never pushes, rebases, resets, force-updates or deletes.
- **No authorization from agents.** Agent reports and review verdicts never create, approve or
  execute an integration. Only the operator's IPC methods do.
- **Recoverable.** Every step is journaled. After a crash, the target branch decides the outcome
  (ADR 0031).

## Known limitations

- **Only the ref update is atomic.** A fast-forward of a checked-out target updates files and then
  the branch; it is as safe as `git merge --ff-only`, nothing more. If Core is killed in the middle,
  the checkout can be left partially updated with the branch not yet moved; recovery then reports
  the integration as failed, says the checkout may be partly updated, and changes nothing more.
  Inspect the checkout with `git status`.
- **Recovery after a crash** settles from the target branch: if it contains the recorded merge
  commit, the integration is `integrated`. A workspace the operator had marked `completed` and that
  was mid-step when Core stopped comes back as `ready` or `dirty`. A validation run that was
  interrupted is marked `interrupted` without a journal event for each step.
- **Content checks scan the workspace.** Preparation computes the tree of the whole workspace
  content (with a private index), which reads every non-ignored file. Very large workspaces make
  preparation slower.
- **Repository hooks are not run** for Switchyard's commits and merges. Use validation commands
  instead.
- **Approved validation commands run with your user's permissions.** They usually execute code from
  the workspace that an agent wrote (tests, build scripts): authorizing a command authorizes that code.
  They inherit Core's environment; a desktop-started Core may have a minimal `PATH`. A process that
  leaves the command's process group is not tracked (a reused process-group id between the end of a
  command and its final kill is theoretically possible but not guarded against).
- **Interrupted runs.** After a Core restart a running validation is `interrupted`; its processes
  are not supervised any more.
- **Completed tasks cannot be re-reviewed.** If a completed task's content changes after its review
  (for example after an update from the target), the review requirement cannot be met again unless it
  is turned off for the project.
- **Local only.** No pushing, pull requests, remote hosting or deployment.
- **Targets.** Integrating into another Switchyard workspace branch (`switchyard/*`) is refused.
- **Same-user clients.** Core's socket accepts any client of the same user. An approved shell command
  of an agent runs as that user too. Policies and approvals are not a sandbox (ADR 0019).

## Code

- **Git primitives:** `crates/switchyard-git/src/integrate.rs`.
- **Domain (no I/O):** `crates/switchyard-integrations` (state machine, blockers, validation rules,
  merge message).
- **Storage:** `crates/switchyard-storage/src/integrations.rs`; `migrations/0006_integrations.sql`.
- **Core:** `crates/switchyard-core/src/integrations/`:
  - `facts.rs`: preparation;
  - `workspace_ops.rs`;
  - `validate.rs`;
  - `execute.rs`: approval and recovery;
  - `inspect.rs`.
- **UI:** `apps/desktop/src/features/integrations/`; the Delivery panel in the task view.
- **Tests:**
  - `crates/switchyard-git/tests/integrate.rs`;
  - `crates/switchyard-core/tests/integrations.rs`;
  - `tests/real_integration.rs` (opt-in, paid).
