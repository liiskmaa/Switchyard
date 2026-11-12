# ADR 0012: Projects, workspaces and sessions are separate entities

- Status: accepted (2026-11-11)
- Supersedes the "primary workspace" part of [ADR 0011](0011-agent-workspace-boundary.md).

## Context

Milestone 2 ran every Claude Code session in the registered repository's own checkout, so a project
could have only one live session (`ensure_project_free`). Two agents sharing one writable working
tree would overwrite each other's changes and the user's.

## Decision

- **Project:** the registered source repository. Agents never get it as a writable working
  directory. Registration and Git inspection are unchanged.
- **Workspace:** a Core-owned, persisted, isolated working copy. Today that is always a Git
  worktree on its own branch. It has a stable UUIDv7 id that doesn't depend on its branch or path,
  and its own lifecycle (see [workspaces.md](../architecture/workspaces.md)).
- **Session:** one agent execution attached to exactly one workspace. Its working directory is the
  workspace path.
- **Relationship:** `Project 1 → N Workspaces → N Sessions`.
  - **At most one live session per workspace,** enforced three ways: the workspace status
    (`active`), a check inside the transaction that attaches the session, and a partial unique
    index `sessions_live_workspace`.
  - **Any number of live sessions per project.**
- **`sessions.start` requires `workspace_id`.** There is no fallback to the project directory: if
  validation fails, the start is refused with a typed error.
- **Workspace logic lives in Core** (`switchyard-core/src/workspaces`). The pure rules live in
  `switchyard-workspaces`: lifecycle, naming, layout and cleanup evaluation. Git mechanics live in
  `switchyard-git::worktree`. The Claude adapter only receives a `cwd`.

## Migration of milestone-2 data

- **Legacy checkout records.** M2 recorded one `primary` workspace per project: the checkout itself.
  Migration `0003` turns each into a workspace of kind `legacy_checkout` with status `archived`. It
  isn't presented as isolated.
- **Sessions are kept.** Sessions keep their `workspace_id`, so history and events stay attached.
  The mapping is saved and restored around the table rebuild.
- **Legacy sessions are never resumed** (`legacy_workspace`). Resuming would run an agent in the
  project checkout again, and Claude also keys sessions by working directory. The UI shows them as
  history.
- **New sessions must name a workspace.** A trigger refuses a new session row without a
  `workspace_id`. `sessions` isn't rebuilt, because rebuilding it would cascade into `approvals`.

## Protocol compatibility

> **Superseded by [ADR 0015](0015-protocol-version-2.md):** the change is now protocol version 2,
> and both sides refuse version 1 during the handshake. The text below is the original reasoning.


- **Additive changes:** new methods, new event types and new error codes.
- **One incompatible change within v1:** `sessions.start` now requires `workspace_id`. An old
  client that sends only `project_id` gets `invalid_params` naming the missing field. Keeping the
  old behaviour would contradict this decision.
- **No version bump.** The only client is the bundled desktop, which ships with Core, so
  `PROTOCOL_VERSION` stays 1. A new desktop talking to an older running Core gets `method_not_found`
  for `workspaces.*`.

## Consequences

- **Projects can't be removed while worktrees remain.** A project can't be unregistered while any
  of its worktree workspaces is not `removed`, because that would orphan worktrees and their
  uncommitted work.
- **`files.changed` is per workspace.** Watchers are per workspace and paths are relative to the
  workspace root.
