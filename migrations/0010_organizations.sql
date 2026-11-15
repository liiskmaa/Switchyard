-- Agent organizations (milestone 8.3, ADRs 0035–0036).
--
-- * `task_workflows` accepts the `organization` definition. SQLite cannot
--   change a CHECK constraint, so the table is rebuilt (nothing references it;
--   every row is kept as is).
-- * `organizations`: one per objective (its root task). `state` is the pure
--   organization state (agents, organization tasks, checkpoints, counters);
--   each organization task is backed by an M6 task.
-- * `org_context`: the shared engineering context, append-only and versioned
--   (`seq`); a later entry supersedes an earlier one, nothing is edited.
-- * `org_settings`: per project, the organization limits and the user's
--   capability estimates and overrides.
CREATE TABLE task_workflows_v3 (
    task_id         TEXT    PRIMARY KEY NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    project_id      TEXT    NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    definition      TEXT    NOT NULL CHECK (definition IN ('engineering', 'organization')),
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
    integration_id  TEXT    REFERENCES integrations (id)
) STRICT;
INSERT INTO task_workflows_v3 SELECT * FROM task_workflows;
DROP TABLE task_workflows;
ALTER TABLE task_workflows_v3 RENAME TO task_workflows;
CREATE INDEX task_workflows_status ON task_workflows (status);

CREATE TABLE organizations (
    task_id         TEXT    PRIMARY KEY NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    project_id      TEXT    NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    workspace_id    TEXT    NOT NULL REFERENCES workspaces (id),
    status          TEXT    NOT NULL CHECK (status IN ('running', 'waiting', 'paused', 'done', 'cancelled')),
    reason          TEXT,
    state           TEXT    NOT NULL CHECK (json_valid(state)),
    -- The workspace commit when the organization started: changed files are
    -- verified against it.
    base_commit     TEXT,
    last_event_seq  INTEGER NOT NULL DEFAULT 0,
    revision        INTEGER NOT NULL DEFAULT 1,
    started_by      TEXT    NOT NULL,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    ended_at        INTEGER
) STRICT;
CREATE INDEX organizations_status ON organizations (status);

CREATE TABLE org_context (
    org_task_id TEXT    NOT NULL REFERENCES organizations (task_id) ON DELETE CASCADE,
    seq         INTEGER NOT NULL CHECK (seq >= 1),
    kind        TEXT    NOT NULL CHECK (kind IN ('objective', 'criterion', 'decision', 'finding', 'risk',
                    'instruction', 'deliverable', 'progress', 'disagreement', 'observation', 'escalation')),
    status      TEXT    NOT NULL CHECK (status IN ('verified', 'claim', 'proposal', 'decided')),
    text        TEXT    NOT NULL CHECK (length(text) BETWEEN 1 AND 8000),
    task_key    TEXT,
    -- `core`, `user` or `agent:<key>`.
    source      TEXT    NOT NULL,
    supersedes  INTEGER,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (org_task_id, seq)
) STRICT;
CREATE TRIGGER org_context_no_update BEFORE UPDATE ON org_context
BEGIN
    SELECT RAISE(ABORT, 'the shared context is append-only');
END;

CREATE TABLE org_settings (
    project_id  TEXT    PRIMARY KEY NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    settings    TEXT    NOT NULL CHECK (json_valid(settings)),
    revision    INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
) STRICT;
