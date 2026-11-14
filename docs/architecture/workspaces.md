# Workspaces

A **workspace** is an isolated working copy of a registered project: a Git worktree that Core owns,
on its own branch, in its own directory. Every agent session runs in exactly one workspace, which
lets several sessions work on the same project at the same time without sharing a writable tree.

> **Git worktrees isolate source trees, not processes.** An approved Bash command still runs with
> the user's privileges. It can read and write files outside its workspace, including the original
> checkout and other workspaces. Workspace isolation doesn't make untrusted agent commands safe;
> the approval gate does what it can (see ADR 0011), and there is no OS-level sandbox.

Decisions: [ADR 0012](../decisions/0012-project-workspace-session.md) (the entities),
[ADR 0013](../decisions/0013-worktree-location-and-branches.md) (location and branches),
[ADR 0014](../decisions/0014-workspace-recovery-and-cleanup.md) (recovery and cleanup).

## Lifecycle

```text
                 ┌──────────── failed ◄───────────────┐ (validation fails: missing dir,
                 │   ▲  ▲          │ verify ok          │  wrong branch, not a worktree…)
                 │   │  │          ▼                    │
 create ──► provisioning ──► ready ⇄ dirty ──► active ──┘
                               │  ▲  │   ▲       │  session ends → ready/dirty
                               ▼  │  ▼   │       │  (never deleted)
                            completed ───┘ ◄─────┘
                               │
              archive ─────────┴──► archived ──► cleanup_pending ──► removed
                                   ▲  │ restore            │ failure
                                   │  └─► ready/dirty      ▼
                                   └──────────────── archived (reason recorded)
```

| Status | Meaning | Sessions? |
|---|---|---|
| `provisioning` | Record written; worktree being created and verified | no |
| `ready` | Verified, clean, nothing attached | yes |
| `dirty` | Verified, uncommitted or untracked changes, nothing attached | yes |
| `active` | One session attached | no (occupied) |
| `completed` | Operator marked the work done; reopens on a new session | yes |
| `integrating` | Core runs an integration step in the worktree (commit, update from the target, validation); entered only from `ready`/`dirty`/`completed` without a session, settles back to `ready`/`dirty` (also at startup) | no |
| `failed` | Provisioning failed or validation found a problem (`status_reason`) | no; **Re-check** to recover |
| `archived` | Kept on disk, hidden, no sessions | no; **Restore** or **Clean up** |
| `cleanup_pending` | Confirmed cleanup in progress | no |
| `removed` | Worktree removed by cleanup; record and branch kept | final |

The legal transitions are `WorkspaceStatus::can_transition` (`crates/switchyard-workspaces`).
Core checks each transition inside the transaction that writes it, together with a
`workspace.state_changed` event. `status_reason` is `code: detail`, and the code is stable:
`missing_worktree`, `not_a_worktree`, `wrong_repository`, `branch_mismatch`, `path_mismatch`,
`repository_unavailable`, `branch_collision`, `path_exists`, `worktree_add_failed`,
`provisioning_interrupted`, `cleanup_interrupted`, `cleanup_failed`, `legacy_checkout`.

Kinds:
- `worktree` is everything created from milestone 3 on.
- `legacy_checkout` is the milestone-2 record of a project checkout. It is archived, accepts no
  sessions, is never cleaned up, and its sessions can't be resumed.

## Creating a workspace

1. Validate the name and base revision. Resolve the project and check it is still a repository
   root. Resolve the base to a commit; an unborn repository is refused.
2. Under the per-repository lock, compute the id, the path
   (`<data>/workspaces/<project>/<id>`) and the branch (`switchyard/<id>`).
3. Insert the record as `provisioning` and journal `workspace.created`. This is persisted before
   any filesystem change, so a crash leaves a trace.
4. Run `git worktree add --no-track -b switchyard/<id> <path> <base_commit>`. It refuses an
   existing branch or path.
5. Verify the identity (see ADR 0013) and that `HEAD` is the base commit.
6. Read `git status` and move to `ready` (or `dirty`, if checkout hooks or filters changed files).

Any failure records `failed` with a reason, returns a typed error and leaves files in place.

## Sessions in a workspace

`sessions.start {workspace_id, project_id?}`:

1. Check:
   - the workspace exists (`workspace_not_found`);
   - it belongs to the project, when one is given (`workspace_project_mismatch`);
   - it isn't legacy (`legacy_workspace`);
   - it accepts a session (`workspace_occupied` / `invalid_workspace_state`);
   - it validates on disk and in Git (`worktree_missing` / `worktree_identity_mismatch` /
     `repository_unavailable`).

   A validation failure also marks the workspace `failed`, so the UI shows why.
2. In one transaction: move the workspace to `active` (re-checked there) and insert the session.
3. Launch `claude` with `cwd` set to the workspace path. Resume does the same in the session's
   workspace and refuses if the recorded `cwd` differs from the workspace path.
4. When the process ends for any reason, the workspace returns to `ready`/`dirty` (or `failed`, if
   its worktree vanished). The final transition is guarded so that it never clobbers a session that
   attached in the meantime.

## Filesystem observation

- There is one watcher (FSEvents through `notify`) per workspace with a live session, rooted at
  the workspace directory and never higher.
- Events are debounced (500 ms, at most a 3 s window, 200 paths per batch), and `.git` and ignored
  paths are dropped.
- Each `files.changed` event carries the project, workspace and session in its context, the batch
  time in `occurred_at_ms`, and paths relative to the workspace root.
- A deleted workspace directory stops producing events; the next validation marks it `failed`.

## Git status

- `workspaces.git_status` reports:
  - the branch and the expected branch;
  - `HEAD` and the commits since the base;
  - staged, unstaged, untracked and conflicted counts.
- It also records the observation (`dirty`, `git_checked_at`).
- A settled workspace follows it (`ready` ⇄ `dirty`).
- The UI refreshes it on `files.changed` events for that workspace.

## Archive, restore, cleanup, recovery

See ADR 0014 for the full rules and the reconciliation table. In short:
- archiving never deletes;
- cleanup needs an archived workspace and no blockers, and the operator must send back the
  confirmation from the report they reviewed;
- that confirmation covers a full scan of every entry in the worktree, including inside ignored
  directories, and the scan is repeated right before removal;
- cleanup keeps the branch, and Git's own refusal (no `--force`) is a second safety net;
- startup reconciliation only changes statuses.

## Known issues

- **Intermittent: workspace buttons after archiving (desktop UI, unresolved).** In the browser
  end-to-end suite (phase p09), a click on **Restore** right after archiving a workspace, or on
  **Clean up…** after archiving another, sometimes times out. Playwright finds the button, but it is
  repeatedly replaced in the DOM for 30 s, while Core has already archived the workspace and the UI
  makes no unusual requests. Observed on 2026-11-14 in 3 of 9 full runs at normal machine load (on
  the M6 and M7 code alike), and four more times under heavy load (twice during the M7 review
  remediation, at load averages of 15 to 48). Archiving switches the panel to "Show
  archived", which reloads the list and remounts the detail panel once. The failure did not
  reproduce in isolation (10 of 10 archive/restore cycles restored in 0.4–0.7 s). It is unclear
  whether it is a test timing assumption or a UI race; it affects only the buttons, not Core's
  workspace state.
