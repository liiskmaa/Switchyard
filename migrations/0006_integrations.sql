-- Milestone 7: change integration (ADRs 0028-0031).

-- Per-project integration settings.
CREATE TABLE integration_settings (
    project_id     TEXT    PRIMARY KEY NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    -- An approved review of exactly the integrated content is required.
    require_review INTEGER NOT NULL DEFAULT 1 CHECK (require_review IN (0, 1)),
    revision       INTEGER NOT NULL DEFAULT 1,
    updated_at     INTEGER NOT NULL
) STRICT;

-- Validation commands the operator configured for a project (ADR 0030).
-- `argv` is run directly, never through a shell.
CREATE TABLE validation_commands (
    id           TEXT    PRIMARY KEY NOT NULL,
    project_id   TEXT    NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    position     INTEGER NOT NULL,
    name         TEXT    NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    argv         TEXT    NOT NULL CHECK (json_valid(argv) AND json_array_length(argv) >= 1),
    timeout_secs INTEGER NOT NULL CHECK (timeout_secs BETWEEN 1 AND 14400),
    enabled      INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
) STRICT;
CREATE INDEX validation_commands_project ON validation_commands (project_id, position);

-- The proposed delivery of one workspace's changes into a target branch
-- (ADR 0028). Bound to exact source and target commits.
CREATE TABLE integrations (
    id                 TEXT    PRIMARY KEY NOT NULL,
    project_id         TEXT    NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    task_id            TEXT    REFERENCES tasks (id) ON DELETE SET NULL,
    workspace_id       TEXT    NOT NULL REFERENCES workspaces (id),
    source_branch      TEXT    NOT NULL,
    source_commit      TEXT    NOT NULL,
    target_branch      TEXT    NOT NULL,
    target_commit      TEXT    NOT NULL,
    base_commit        TEXT,
    status             TEXT    NOT NULL CHECK (status IN
                           ('open', 'conflicted', 'resolving', 'validating', 'ready', 'integrating',
                            'integrated', 'failed', 'cancelled')),
    status_reason      TEXT,
    -- Facts and blockers of the latest preparation.
    preflight          TEXT    NOT NULL CHECK (json_valid(preflight)),
    prepared_at        INTEGER NOT NULL,
    approved_by        TEXT,
    approved_at        INTEGER,
    -- The merge commit Core created: recorded before the branch update
    -- (integrating), confirmed after it (integrated). Crash recovery
    -- compares the target branch with it (ADR 0031).
    merge_commit       TEXT,
    -- `ref_update` (target not checked out) or `checkout_fast_forward`.
    method             TEXT,
    failure            TEXT,
    revision           INTEGER NOT NULL DEFAULT 1,
    created_at         INTEGER NOT NULL,
    updated_at         INTEGER NOT NULL,
    completed_at       INTEGER
) STRICT;
CREATE INDEX integrations_project ON integrations (project_id, status);
CREATE INDEX integrations_task ON integrations (task_id);
-- One unfinished integration per workspace.
CREATE UNIQUE INDEX integrations_one_open_per_workspace ON integrations (workspace_id)
    WHERE status NOT IN ('integrated', 'cancelled');

-- A validation run of the project's commands for one exact source commit.
CREATE TABLE validation_runs (
    id             TEXT    PRIMARY KEY NOT NULL,
    integration_id TEXT    NOT NULL REFERENCES integrations (id) ON DELETE CASCADE,
    project_id     TEXT    NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    source_commit  TEXT    NOT NULL,
    status         TEXT    NOT NULL CHECK (status IN ('running', 'passed', 'failed', 'cancelled', 'interrupted')),
    requested_by   TEXT,
    -- The commands, timeouts and policy decisions the run was valid for: a
    -- passed run counts only while they are unchanged.
    config         TEXT    NOT NULL,
    started_at     INTEGER NOT NULL,
    ended_at       INTEGER
) STRICT;
CREATE INDEX validation_runs_integration ON validation_runs (integration_id, started_at);

-- One command of a run, with its authorization and outcome.
CREATE TABLE validation_steps (
    id                 TEXT    PRIMARY KEY NOT NULL,
    run_id             TEXT    NOT NULL REFERENCES validation_runs (id) ON DELETE CASCADE,
    position           INTEGER NOT NULL,
    command_id         TEXT,
    name               TEXT    NOT NULL,
    argv               TEXT    NOT NULL CHECK (json_valid(argv)),
    command_line       TEXT    NOT NULL,
    cwd                TEXT    NOT NULL,
    timeout_secs       INTEGER NOT NULL,
    -- The policy evaluation (allow / ask / deny), as for an agent's command.
    policy_decision    TEXT    NOT NULL CHECK (policy_decision IN ('allow', 'ask', 'deny')),
    policy_reason      TEXT    NOT NULL,
    policy_explanation TEXT    NOT NULL,
    policy_rule_id     TEXT,
    policy_revision    INTEGER NOT NULL,
    -- `policy` (allowed by a rule) or the operator who authorized an `ask`.
    authorized_by      TEXT,
    status             TEXT    NOT NULL CHECK (status IN
                           ('pending', 'running', 'passed', 'failed', 'timed_out', 'cancelled', 'refused',
                            'skipped', 'error')),
    exit_code          INTEGER,
    signal             INTEGER,
    stdout             TEXT,
    stderr             TEXT,
    stdout_truncated   INTEGER NOT NULL DEFAULT 0,
    stderr_truncated   INTEGER NOT NULL DEFAULT 0,
    detail             TEXT,
    started_at         INTEGER,
    ended_at           INTEGER,
    UNIQUE (run_id, position)
) STRICT;

-- The exact content a review saw, as a Git tree of the whole worktree
-- (tracked and untracked, non-ignored files), recorded when it started
-- (ADR 0029). An integration needs the source tree to equal it.
ALTER TABLE task_assignments ADD COLUMN review_tree TEXT;
