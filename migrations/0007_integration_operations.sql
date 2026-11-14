-- Core-owned integration operations (ADR 0032). Every step that changes Git
-- for an integration (approve, workspace commit/update/abort, a validation
-- run) is one row, created before the work starts and ended when it ends.
-- A row still `running` after a restart was interrupted by a crash or by a
-- shutdown that could not wait for it; startup recovery settles it from Git.
CREATE TABLE integration_operations (
    id               TEXT    PRIMARY KEY NOT NULL,
    integration_id   TEXT    NOT NULL REFERENCES integrations (id) ON DELETE CASCADE,
    project_id       TEXT    NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    kind             TEXT    NOT NULL CHECK (kind IN
                         ('approve', 'commit_workspace', 'update_from_target', 'abort_resolution', 'validate')),
    status           TEXT    NOT NULL CHECK (status IN ('running', 'completed', 'failed', 'interrupted')),
    requested_by     TEXT    NOT NULL,
    -- The workspace status before Core marked it `integrating` for this
    -- operation, so recovery can restore an operator's `completed` mark.
    workspace_before TEXT,
    error            TEXT,
    started_at       INTEGER NOT NULL,
    ended_at         INTEGER,
    CHECK ((status = 'running') = (ended_at IS NULL))
) STRICT;
CREATE INDEX integration_operations_integration ON integration_operations (integration_id, started_at);
-- At most one running operation per integration: a retried request cannot
-- start the same work twice.
CREATE UNIQUE INDEX integration_operations_one_running ON integration_operations (integration_id)
    WHERE status = 'running';
