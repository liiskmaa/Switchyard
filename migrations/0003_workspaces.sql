-- Switchyard schema v3: isolated workspaces (Git worktrees).
--
-- A project is the registered repository; a workspace is a Core-owned
-- worktree on its own branch; every session runs in exactly one workspace.
--
-- `workspaces` is rebuilt because its CHECK constraints cannot express the
-- lifecycle. Milestone 2 recorded one 'primary' workspace per project: the
-- project's own checkout. Those rows become `legacy_checkout` workspaces in
-- status `archived`. They are not isolated, never accept new sessions and are
-- never cleaned up; they exist so milestone 2 sessions keep their history.
--
-- Dropping the old table fires `ON DELETE SET NULL` on
-- `sessions.workspace_id` (foreign keys are enforced and cannot be switched
-- off inside the migration transaction), so the session → workspace mapping
-- is saved first and restored afterwards. Workspace ids are preserved.

CREATE TEMP TABLE m3_session_workspace AS
    SELECT id, workspace_id FROM sessions WHERE workspace_id IS NOT NULL;

CREATE TABLE workspaces_v3 (
    id                TEXT    PRIMARY KEY NOT NULL,
    project_id        TEXT    NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    kind              TEXT    NOT NULL CHECK (kind IN ('legacy_checkout', 'worktree')),
    name              TEXT    NOT NULL CHECK (length(name) BETWEEN 1 AND 80),
    -- Canonical absolute path of the working directory. For a worktree this
    -- is <data dir>/workspaces/<project_id>/<id>, derived from the ids; it
    -- is both the worktree root and the agent's cwd.
    path              TEXT    NOT NULL UNIQUE,
    -- Short branch name (switchyard/<id>); NULL for legacy checkouts.
    branch            TEXT,
    -- Revision the operator asked to branch from (e.g. HEAD, main).
    base_ref          TEXT,
    -- Commit the branch was created at.
    base_commit       TEXT,
    status            TEXT    NOT NULL CHECK (status IN (
                          'provisioning', 'ready', 'active', 'dirty', 'integrating', 'completed',
                          'failed', 'cleanup_pending', 'archived', 'removed')),
    -- Machine-readable reason plus detail for failed / unusual states.
    status_reason     TEXT,
    -- Last observed Git status (1 = uncommitted or untracked changes).
    dirty             INTEGER CHECK (dirty IN (0, 1)),
    git_checked_at    INTEGER,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL,
    last_activity_at  INTEGER NOT NULL,
    archived_at       INTEGER,
    CHECK (kind = 'legacy_checkout' OR (branch IS NOT NULL AND base_commit IS NOT NULL)),
    CHECK (status NOT IN ('archived', 'cleanup_pending', 'removed') OR archived_at IS NOT NULL)
) STRICT;

INSERT INTO workspaces_v3 (id, project_id, kind, name, path, branch, base_ref, base_commit, status,
                           status_reason, created_at, updated_at, last_activity_at, archived_at)
SELECT id, project_id, 'legacy_checkout', 'Project checkout (legacy)', path, NULL, NULL, NULL, 'archived',
       'legacy_checkout: sessions from before workspace isolation ran directly in the project checkout',
       created_at, updated_at, updated_at, updated_at
FROM workspaces;

DROP TABLE workspaces;
ALTER TABLE workspaces_v3 RENAME TO workspaces;
CREATE INDEX workspaces_project ON workspaces (project_id, created_at);
CREATE UNIQUE INDEX workspaces_branch ON workspaces (project_id, branch) WHERE branch IS NOT NULL;

UPDATE sessions
SET workspace_id = (SELECT m.workspace_id FROM m3_session_workspace m WHERE m.id = sessions.id)
WHERE id IN (SELECT id FROM m3_session_workspace);
DROP TABLE m3_session_workspace;

-- Every new session names its workspace. (Existing rows are not rebuilt:
-- rebuilding `sessions` would cascade into `approvals`.)
CREATE TRIGGER sessions_require_workspace BEFORE INSERT ON sessions
WHEN NEW.workspace_id IS NULL
BEGIN
    SELECT RAISE(ABORT, 'sessions.workspace_id is required');
END;

-- At most one live session per workspace. Matches ACTIVE_SESSION_STATUSES.
CREATE UNIQUE INDEX sessions_live_workspace ON sessions (workspace_id)
    WHERE state IN ('starting', 'initializing', 'idle', 'running', 'waiting_for_approval', 'interrupted');
CREATE INDEX sessions_workspace ON sessions (workspace_id, started_at);
