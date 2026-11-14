-- Workflow orchestration and role/model bindings (milestone 8.1, ADR 0033).
-- Additive: existing tasks and assignments keep working unchanged.

-- One workflow run per task. `state` is the pure workflow state
-- (switchyard_tasks::workflow::WorkflowState); `status`, `stage`, `step_key`
-- and `reason` mirror it for queries and the UI.
CREATE TABLE task_workflows (
    task_id         TEXT    PRIMARY KEY NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    project_id      TEXT    NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    definition      TEXT    NOT NULL CHECK (definition IN ('engineering')),
    version         INTEGER NOT NULL CHECK (version >= 1),
    -- The effective role bindings and limits, snapshotted when the workflow
    -- started: later project setting changes do not alter a running workflow.
    config          TEXT    NOT NULL CHECK (json_valid(config)),
    workspace_id    TEXT    REFERENCES workspaces (id),
    status          TEXT    NOT NULL CHECK (status IN
                        ('pending', 'running', 'waiting', 'paused', 'completed', 'cancelled', 'failed')),
    stage           TEXT,
    step_key        TEXT,
    reason          TEXT,
    state           TEXT    NOT NULL CHECK (json_valid(state)),
    -- Highest journal seq already processed (idempotent replay, like assignments).
    last_event_seq  INTEGER NOT NULL DEFAULT 0,
    revision        INTEGER NOT NULL DEFAULT 1,
    started_by      TEXT    NOT NULL,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    ended_at        INTEGER
) STRICT;
CREATE INDEX task_workflows_status ON task_workflows (status);

-- Project-level role bindings: provider and model per workflow role.
CREATE TABLE workflow_settings (
    project_id  TEXT    PRIMARY KEY NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    bindings    TEXT    NOT NULL CHECK (json_valid(bindings)),
    revision    INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
) STRICT;

-- Who created an assignment, which workflow step it is, and the model it ran
-- with (NULL = the CLI's default, chosen explicitly).
ALTER TABLE task_assignments ADD COLUMN origin TEXT NOT NULL DEFAULT 'operator'
    CHECK (origin IN ('operator', 'workflow'));
ALTER TABLE task_assignments ADD COLUMN step_key TEXT;
ALTER TABLE task_assignments ADD COLUMN model TEXT;
-- A workflow step is created at most once, however often its trigger is replayed.
CREATE UNIQUE INDEX task_assignments_step ON task_assignments (task_id, step_key)
    WHERE step_key IS NOT NULL;
