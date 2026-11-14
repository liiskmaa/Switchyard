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
  the target at `T`. Core refuses any merge, fast-forward or abort that would replace or remove an
  untracked or ignored file (for example `.env`), including a file where the merge needs a directory,
  files under a directory the merge replaces, and files a directory rename would move onto (ADR 0029).
  It never commits files that gained conflict blocks, staged or not. It never pushes, rebases, resets,
  force-updates or deletes.
- **No authorization from agents.** Agent reports and review verdicts never create, approve or
  execute an integration. Only the operator's IPC methods do.
- **Core owns authorized work.** Approving and every Git step in the workspace run as Core-owned
  operations (ADR 0032): closing the app or losing the connection does not stop them, a reconnecting
  client sees them on the integration, a retry cannot run them twice, and they always release the
  repository lock and the workspace.
- **Validation is evidence.** A run records exactly the commands, command lines, timeouts and policy
  decisions it executed (read in one transaction), and is `passed` only if every step's success was
  observed and recorded. Failures to record fail closed.
- **Recoverable.** Every step and every settings change is journaled. After a crash, the target
  branch decides the outcome (ADR 0031); Core never guesses when Git cannot be read.

## Core-owned operations

| Event | Effect |
|---|---|
| Client disconnects (app closed, socket lost) | The operation continues; reconnect to see it. |
| *Cancel validation* | Stops the validation's commands; the run is `cancelled`. Git steps cannot be cancelled. |
| Core shuts down | Validation runs are stopped. Other operations get up to `operation_shutdown_grace` (60 s), then are stopped and settled at the next start. |
| Core crashes | At the next start the operation is `interrupted` and the integration is settled from Git. |
| Git times out | The step fails. Core reports what Git shows now; for a checkout it says "nothing was changed" only after checking that. |
| A checkout may be partially updated | The failure says so and to inspect it with `git status`. Switchyard does not undo or repair it. |
| Recovery cannot decide (Git error, `index.lock` present) | The integration stays `integrating` with the reason; *Check again* settles it later. |

## Known limitations

- **Only the ref update is atomic.** A fast-forward of a checked-out target updates files and then
  the branch; it is as safe as `git merge --ff-only`, nothing more. A client disconnect no longer
  interrupts it, but a Git timeout, a Core crash, or a shutdown whose grace runs out can still leave
  the checkout partially updated with the branch not moved. Core then reports the integration as
  failed, says the checkout may be partially updated, and changes nothing more. Inspect the checkout
  with `git status` (and remove a leftover `.git/index.lock` only once no Git process is running).
- **Checkout races are detected, not prevented.** If someone switches the target checkout to another
  branch in the instant between Core's check and Git's fast-forward, Git fast-forwards that branch
  instead; Core detects and reports it. If the target branch is checked out in the instant between the
  check and the ref update, the integration records that the checkout's files still show `T`.
- **Interrupted steps.** After a crash, an interrupted validation run's unfinished steps are marked
  `cancelled` with one `integration.validation_finished` event for the run, not one per step.
- **Panics** inside an operation are contained (the workspace is released and the state re-read
  from Git), but they are bugs; the operation is recorded `failed`.
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
- **Interrupted runs.** After a Core crash a running validation is `interrupted`; processes that
  survived the crash are not supervised any more. A graceful shutdown stops them first.
- **Conflict-block detection is a rule, not a parser.** A file that gains a complete block
  (`<<<<<<<` … `=======` … `>>>>>>>` at line starts) is refused even outside a merge; commit such content
  in the workspace with Git directly if it is intended. Binary files are not scanned.
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
- **Storage:** `crates/switchyard-storage/src/integrations.rs`; `migrations/0006_integrations.sql`,
  `migrations/0007_integration_operations.sql`.
- **Core:** `crates/switchyard-core/src/integrations/`:
  - `facts.rs`: preparation;
  - `workspace_ops.rs`;
  - `validate.rs`;
  - `execute.rs`: approval and recovery;
  - `operations.rs`: Core-owned operations, shutdown, settling stranded integrations;
  - `inspect.rs`.
- **UI:** `apps/desktop/src/features/integrations/`; the Delivery panel in the task view.
- **Tests:**
  - `crates/switchyard-git/tests/integrate.rs`;
  - `crates/switchyard-core/tests/integrations.rs`;
  - `tests/real_integration.rs` (opt-in, paid).
