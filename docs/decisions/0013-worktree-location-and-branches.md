# ADR 0013: Worktree location and branch naming

- Status: accepted (2026-11-11)

## Decision

- **Location:** `<data dir>/workspaces/<project-id>/<workspace-id>`. On macOS the default data dir
  is `~/Library/Application Support/Switchyard`, so paths contain a space.
  - Directories are created `0700`.
  - The workspaces root is canonicalized at startup, and every comparison uses canonical paths.
  - Creation refuses a layout where the workspaces directory and the repository overlap.
- **Names come from ids, never from user input.** The display name is a label only; it is checked
  for length and control characters and is never used in a path or ref.
- **Branch:** `switchyard/<workspace-id>`, created with `git worktree add --no-track -b <branch>
  <path> <base-commit>`.
  - The base revision (default `HEAD`) is resolved to a commit first. User input goes after
    `--end-of-options` and is rejected if it starts with `-`. The commit is persisted as
    `base_commit`.
  - An existing branch is **never reused or reset.** A collision fails with `branch_collision` and
    the workspace is recorded `failed`. An existing path is never overwritten.
  - Git's own admin directory for the worktree, `<repo>/.git/worktrees/<workspace-id>`, is named
    after the unique directory name.
- **Persisted identity:** the record stores `path`, `branch`, `base_ref` and `base_commit`.
  - Before every session start or resume, and on verify and reconciliation, Core checks that:
    - the recorded path equals the path derived from the ids;
    - it is a real directory (not a symlink);
    - Git reports it as the top level of a worktree;
    - that worktree shares the project's common Git directory;
    - `HEAD` is `refs/heads/switchyard/<id>`.
  - Any mismatch refuses the operation, with a reason such as `path_mismatch`, `missing_worktree`,
    `not_a_worktree`, `wrong_repository` or `branch_mismatch`.
- **No shell, no new Git implementation:** everything goes through `switchyard-git`. Arguments are
  passed as an array, paths are absolute, and there are timeouts (5 minutes for `worktree add`,
  whose checkout scales with repository size).
- **Concurrency:** Git mutations are serialized per repository with an async mutex. The database
  enforces unique paths and `(project_id, branch)`.

## If the user moves the original repository

- Git stores absolute paths on both sides: in the worktree's `.git` file and in
  `<repo>/.git/worktrees/<id>/gitdir`.
- After a move, the project is reported inaccessible, as in milestone 1, and validation fails with
  `repository_unavailable`.
- At startup the workspace becomes `failed` and nothing is deleted.
- To recover, move the repository back and click **Re-check**, or run `git worktree repair` and
  re-register. Switchyard doesn't run `repair` itself.
- Moving the Switchyard data directory breaks the same links, with the same conservative outcome.

## Alternatives considered

- **Worktrees inside the repository** (for example `.switchyard/worktrees`). Rejected: they pollute
  the user's tree and risk being committed or watched recursively.
- **Full clones.** Rejected for now: slower, they duplicate objects, and they need remote and fetch
  handling. The `kind` column leaves room for them.
- **Branch names from the workspace name.** Rejected: user input in refs, plus collisions and
  renames.
