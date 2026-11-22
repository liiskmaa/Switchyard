-- Switchyard schema v12: the workflow's test gate (ADR 0039).
--
-- One row per test run Core performed for a workflow step: the commands the
-- implementer named, the policy's decision for each, and every result.
-- Append-only: a run is evidence, never edited.

CREATE TABLE workflow_test_runs (
    id             TEXT    PRIMARY KEY NOT NULL,
    task_id        TEXT    NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    -- The implementation step tested, e.g. `implement#2`.
    step_key       TEXT    NOT NULL,
    status         TEXT    NOT NULL CHECK (status IN ('passed', 'failed', 'needs_authorization')),
    summary        TEXT    NOT NULL,
    -- [{kind, name?, command, policy, status, exit_code?, duration_ms, stdout_tail, stderr_tail, detail?}]
    results        TEXT    NOT NULL CHECK (json_valid(results)),
    -- The change touched UI files, so browser tests were required.
    ui_change      INTEGER NOT NULL CHECK (ui_change IN (0, 1)),
    -- Who authorized commands the policy asked about (the operator's resume), if any.
    authorized_by  TEXT,
    started_at     INTEGER NOT NULL,
    ended_at       INTEGER NOT NULL
) STRICT;
CREATE INDEX workflow_test_runs_task ON workflow_test_runs (task_id, ended_at);

CREATE TRIGGER workflow_test_runs_no_update BEFORE UPDATE ON workflow_test_runs
BEGIN
    SELECT RAISE(ABORT, 'workflow test runs are append-only');
END;
