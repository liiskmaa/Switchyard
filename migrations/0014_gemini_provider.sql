-- switchyard: foreign_keys=off
-- Switchyard schema v14: Gemini CLI as a provider (ADR 0044).
--
-- `agents.vendor`, `sessions.provider` and `task_assignments.provider` accept
-- 'gemini'. SQLite cannot change a CHECK constraint, so the three tables are
-- rebuilt (copy, drop, rename) with every row, index and trigger unchanged.
-- The runner switches foreign keys off for this migration (dropping
-- `sessions` would otherwise cascade into `approvals`) and requires
-- `PRAGMA foreign_key_check` to be empty before it commits.

CREATE TABLE agents_v14 (
    id            TEXT    PRIMARY KEY NOT NULL,
    vendor        TEXT    NOT NULL CHECK (vendor IN ('claude_code', 'codex', 'gemini')),
    display_name  TEXT    NOT NULL,
    executable    TEXT    NOT NULL,
    cli_version   TEXT,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
) STRICT;
INSERT INTO agents_v14 (id, vendor, display_name, executable, cli_version, created_at, updated_at)
SELECT id, vendor, display_name, executable, cli_version, created_at, updated_at FROM agents;
DROP TABLE agents;
ALTER TABLE agents_v14 RENAME TO agents;
CREATE UNIQUE INDEX agents_vendor_executable ON agents (vendor, executable);

CREATE TABLE sessions_v14 (
    id                 TEXT    PRIMARY KEY NOT NULL,
    project_id         TEXT    NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    workspace_id       TEXT    REFERENCES workspaces (id) ON DELETE SET NULL,
    agent_id           TEXT    NOT NULL REFERENCES agents (id),
    provider           TEXT    NOT NULL CHECK (provider IN ('claude_code', 'codex', 'gemini')),
    -- The vendor's own session identifier (Claude `session_id`, Codex thread
    -- id, ACP session id), used to resume.
    native_session_id  TEXT,
    cwd                TEXT    NOT NULL,
    state              TEXT    NOT NULL CHECK (state IN (
                           'starting', 'initializing', 'idle', 'running', 'waiting_for_approval',
                           'interrupted', 'completed', 'failed', 'stopped')),
    pid                INTEGER,
    cli_version        TEXT,
    model              TEXT,
    started_at         INTEGER NOT NULL,
    ended_at           INTEGER,
    last_activity_at   INTEGER NOT NULL,
    exit_code          INTEGER,
    exit_signal        INTEGER,
    -- Machine-readable reason for a failed/stopped session plus detail.
    failure            TEXT,
    resume_count       INTEGER NOT NULL DEFAULT 0,
    access             TEXT    NOT NULL DEFAULT 'write' CHECK (access IN ('write', 'review'))
) STRICT;
INSERT INTO sessions_v14 (id, project_id, workspace_id, agent_id, provider, native_session_id, cwd, state, pid,
    cli_version, model, started_at, ended_at, last_activity_at, exit_code, exit_signal, failure, resume_count, access)
SELECT id, project_id, workspace_id, agent_id, provider, native_session_id, cwd, state, pid,
    cli_version, model, started_at, ended_at, last_activity_at, exit_code, exit_signal, failure, resume_count, access
FROM sessions;
DROP TABLE sessions;
ALTER TABLE sessions_v14 RENAME TO sessions;
CREATE INDEX sessions_project ON sessions (project_id, started_at);
CREATE INDEX sessions_state ON sessions (state);
CREATE UNIQUE INDEX sessions_native ON sessions (provider, native_session_id)
    WHERE native_session_id IS NOT NULL;
CREATE TRIGGER sessions_require_workspace BEFORE INSERT ON sessions
WHEN NEW.workspace_id IS NULL
BEGIN
    SELECT RAISE(ABORT, 'sessions.workspace_id is required');
END;
CREATE UNIQUE INDEX sessions_live_workspace ON sessions (workspace_id)
    WHERE state IN ('starting', 'initializing', 'idle', 'running', 'waiting_for_approval', 'interrupted');
CREATE INDEX sessions_workspace ON sessions (workspace_id, started_at);

CREATE TABLE task_assignments_v14 (
    id                 TEXT    PRIMARY KEY NOT NULL,
    task_id            TEXT    NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    project_id         TEXT    NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    role               TEXT    NOT NULL CHECK (role IN ('investigate', 'implement', 'review')),
    provider           TEXT    NOT NULL CHECK (provider IN ('claude_code', 'codex', 'gemini')),
    workspace_id       TEXT    NOT NULL REFERENCES workspaces (id),
    session_id         TEXT    REFERENCES sessions (id) ON DELETE SET NULL,
    status             TEXT    NOT NULL CHECK (status IN ('pending', 'active', 'reported', 'failed', 'cancelled')),
    -- The task context exactly as sent to the agent (ADR 0025).
    context            TEXT    CHECK (context IS NULL OR json_valid(context)),
    context_version    INTEGER,
    -- Review assignments: workspace fingerprint when the review started (ADR 0027).
    review_fingerprint TEXT,
    -- Highest journal seq of the session's events already processed (idempotency).
    last_event_seq     INTEGER NOT NULL DEFAULT 0,
    outcome            TEXT,
    created_at         INTEGER NOT NULL,
    started_at         INTEGER,
    ended_at           INTEGER,
    review_tree        TEXT,
    origin             TEXT    NOT NULL DEFAULT 'operator' CHECK (origin IN ('operator', 'workflow')),
    step_key           TEXT,
    model              TEXT
) STRICT;
INSERT INTO task_assignments_v14 (id, task_id, project_id, role, provider, workspace_id, session_id, status, context,
    context_version, review_fingerprint, last_event_seq, outcome, created_at, started_at, ended_at, review_tree, origin,
    step_key, model)
SELECT id, task_id, project_id, role, provider, workspace_id, session_id, status, context,
    context_version, review_fingerprint, last_event_seq, outcome, created_at, started_at, ended_at, review_tree, origin,
    step_key, model
FROM task_assignments;
DROP TABLE task_assignments;
ALTER TABLE task_assignments_v14 RENAME TO task_assignments;
CREATE INDEX task_assignments_task ON task_assignments (task_id, created_at);
CREATE INDEX task_assignments_session ON task_assignments (session_id);
CREATE UNIQUE INDEX task_assignments_one_open ON task_assignments (task_id)
    WHERE status IN ('pending', 'active');
CREATE UNIQUE INDEX task_assignments_step ON task_assignments (task_id, step_key)
    WHERE step_key IS NOT NULL;
