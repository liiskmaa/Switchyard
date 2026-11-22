-- Switchyard schema v13: the task supervisor (ADR 0041).
--
-- The operator's conversation with a task's supervisor: a read-only agent
-- that answers questions about the task and may route requested work to a
-- workflow role. Messages are append-only; the supervisor row only remembers
-- its workspace and last session so the next question continues it.

CREATE TABLE task_supervisors (
    task_id      TEXT    PRIMARY KEY NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    project_id   TEXT    NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    -- The supervisor's own read-only workspace (never the task's: one live
    -- session per workspace, and the task's agent may be working there).
    workspace_id TEXT    REFERENCES workspaces (id) ON DELETE SET NULL,
    session_id   TEXT    REFERENCES sessions (id) ON DELETE SET NULL,
    updated_at   INTEGER NOT NULL
) STRICT;

CREATE TABLE supervisor_messages (
    id                TEXT    PRIMARY KEY NOT NULL,
    task_id           TEXT    NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    -- operator: the question or request; supervisor: its answer; core: what
    -- Core did or why the supervisor could not answer.
    sender            TEXT    NOT NULL CHECK (sender IN ('operator', 'supervisor', 'core')),
    text              TEXT    NOT NULL,
    -- The supervisor's routing request (supervisor messages only).
    route             TEXT    CHECK (route IN ('none', 'reasoning', 'coding', 'review', 'tests')),
    instruction       TEXT,
    -- What Core did with the route, in a sentence.
    outcome           TEXT,
    follow_up_task_id TEXT    REFERENCES tasks (id) ON DELETE SET NULL,
    session_id        TEXT    REFERENCES sessions (id) ON DELETE SET NULL,
    by                TEXT,
    created_at        INTEGER NOT NULL
) STRICT;
CREATE INDEX supervisor_messages_task ON supervisor_messages (task_id, created_at);

CREATE TRIGGER supervisor_messages_no_update BEFORE UPDATE ON supervisor_messages
BEGIN
    SELECT RAISE(ABORT, 'supervisor messages are append-only');
END;
