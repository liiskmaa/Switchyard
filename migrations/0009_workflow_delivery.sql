-- Workflow delivery (milestone 8.2, ADR 0034): a workflow follows its task's
-- M7 integration to the target branch. Adds the `delivered` status and the
-- integration the workflow delivers through. SQLite cannot change a CHECK
-- constraint, so the table is rebuilt; every existing row is kept as is.
CREATE TABLE task_workflows_v2 (
    task_id         TEXT    PRIMARY KEY NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    project_id      TEXT    NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    definition      TEXT    NOT NULL CHECK (definition IN ('engineering')),
    version         INTEGER NOT NULL CHECK (version >= 1),
    config          TEXT    NOT NULL CHECK (json_valid(config)),
    workspace_id    TEXT    REFERENCES workspaces (id),
    status          TEXT    NOT NULL CHECK (status IN
                        ('pending', 'running', 'waiting', 'paused', 'completed', 'delivered', 'cancelled', 'failed')),
    stage           TEXT,
    step_key        TEXT,
    reason          TEXT,
    state           TEXT    NOT NULL CHECK (json_valid(state)),
    last_event_seq  INTEGER NOT NULL DEFAULT 0,
    revision        INTEGER NOT NULL DEFAULT 1,
    started_by      TEXT    NOT NULL,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    ended_at        INTEGER,
    -- The M7 integration this workflow delivers through (set once created).
    integration_id  TEXT    REFERENCES integrations (id)
) STRICT;
INSERT INTO task_workflows_v2 (task_id, project_id, definition, version, config, workspace_id, status, stage,
    step_key, reason, state, last_event_seq, revision, started_by, created_at, updated_at, ended_at)
SELECT task_id, project_id, definition, version, config, workspace_id, status, stage, step_key, reason, state,
    last_event_seq, revision, started_by, created_at, updated_at, ended_at
FROM task_workflows;
DROP TABLE task_workflows;
ALTER TABLE task_workflows_v2 RENAME TO task_workflows;
CREATE INDEX task_workflows_status ON task_workflows (status);
