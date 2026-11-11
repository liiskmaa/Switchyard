-- Switchyard schema v2: native agent sessions and centralized approvals.
--
-- `sessions` and `approvals` were reserved in v1 but never written, and
-- their CHECK constraints cannot express the agent lifecycle. SQLite cannot
-- alter a CHECK constraint, so both tables are rebuilt. Existing rows (none
-- are written by v1) are carried over where the columns map.

-- Sessions ------------------------------------------------------------------

CREATE TABLE sessions_v2 (
    id                 TEXT    PRIMARY KEY NOT NULL,
    project_id         TEXT    NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    workspace_id       TEXT    REFERENCES workspaces (id) ON DELETE SET NULL,
    agent_id           TEXT    NOT NULL REFERENCES agents (id),
    provider           TEXT    NOT NULL CHECK (provider IN ('claude_code', 'codex')),
    -- The vendor's own session identifier (Claude `session_id`), used for
    -- `--resume`. Known before launch for Claude (`--session-id`).
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
    resume_count       INTEGER NOT NULL DEFAULT 0
) STRICT;

INSERT INTO sessions_v2 (id, project_id, workspace_id, agent_id, provider, native_session_id, cwd,
                         state, pid, started_at, ended_at, last_activity_at, exit_code)
SELECT s.id, s.project_id, s.workspace_id, s.agent_id, a.vendor, s.vendor_session_id, p.root_path,
       CASE s.status WHEN 'active' THEN 'running' WHEN 'terminated' THEN 'stopped' ELSE s.status END,
       s.pid, s.started_at, s.ended_at, s.started_at, s.exit_code
FROM sessions s JOIN agents a ON a.id = s.agent_id JOIN projects p ON p.id = s.project_id;

DROP TABLE sessions;
ALTER TABLE sessions_v2 RENAME TO sessions;
CREATE INDEX sessions_project ON sessions (project_id, started_at);
CREATE INDEX sessions_state ON sessions (state);
CREATE UNIQUE INDEX sessions_native ON sessions (provider, native_session_id)
    WHERE native_session_id IS NOT NULL;

-- Approvals -----------------------------------------------------------------

CREATE TABLE approvals_v2 (
    id                 TEXT    PRIMARY KEY NOT NULL,
    project_id         TEXT    NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    session_id         TEXT    NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    agent_id           TEXT    REFERENCES agents (id) ON DELETE SET NULL,
    task_id            TEXT    REFERENCES tasks (id) ON DELETE SET NULL,
    kind               TEXT    NOT NULL CHECK (kind IN ('tool_use')),
    tool_name          TEXT    NOT NULL,
    -- The agent's tool invocation this request belongs to.
    tool_use_id        TEXT,
    -- The vendor's id for the permission request (Claude control request_id).
    native_request_id  TEXT    NOT NULL,
    -- {input, description, permission_suggestions, ...} as sent by the agent.
    request            TEXT    NOT NULL CHECK (json_valid(request)),
    status             TEXT    NOT NULL CHECK (status IN
                           ('pending', 'approved', 'denied', 'expired', 'cancelled')),
    requested_at       INTEGER NOT NULL,
    expires_at         INTEGER NOT NULL,
    decided_at         INTEGER,
    decision_source    TEXT    CHECK (decision_source IN (
                           'operator', 'timeout', 'agent_cancelled', 'agent_exited',
                           'core_shutdown', 'core_restart')),
    decided_by         TEXT,
    decision_note      TEXT,
    CHECK ((status = 'pending') = (decided_at IS NULL)),
    UNIQUE (session_id, native_request_id)
) STRICT;

DROP TABLE approvals;
ALTER TABLE approvals_v2 RENAME TO approvals;
CREATE INDEX approvals_status ON approvals (status, requested_at);
CREATE INDEX approvals_session ON approvals (session_id);

-- Agents: one row per detected executable ----------------------------------

CREATE UNIQUE INDEX agents_vendor_executable ON agents (vendor, executable);

-- Workspaces: at most one primary workspace per project ---------------------

CREATE UNIQUE INDEX workspaces_primary ON workspaces (project_id) WHERE kind = 'primary';

-- Event journal: per-session timelines ---------------------------------------

CREATE INDEX events_session ON events (session_id, seq) WHERE session_id IS NOT NULL;
