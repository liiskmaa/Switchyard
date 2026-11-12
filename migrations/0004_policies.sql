-- Switchyard schema v4: approval policies (ADRs 0016-0018).
--
-- * `policy_rules`: the current state of each configured rule.
-- * `policy_rule_revisions`: append-only snapshots; one global, gapless
--   policy revision per change.
-- * `policy_evaluations`: append-only record of every evaluation.
-- * `approvals` gains `evaluation_id` and the decision source `policy`.
--
-- No rule is created here: a fresh or upgraded database starts with the
-- built-in defaults only. Existing approvals are carried over unchanged
-- (pending ones stay pending under their existing expiry and settle as
-- before); none is turned into a rule.

-- The reserved `permissions` table (migration 0001) was never written by any
-- Core version. Refuse to drop it if it holds anything rather than lose data.
CREATE TEMP TABLE m0004_guard (
    rows INTEGER NOT NULL CONSTRAINT permissions_table_must_be_empty CHECK (rows = 0)
);
INSERT INTO m0004_guard SELECT COUNT(*) FROM permissions;
DROP TABLE m0004_guard;
DROP TABLE permissions;

-- Rules ---------------------------------------------------------------------

CREATE TABLE policy_rules (
    id            TEXT    PRIMARY KEY NOT NULL,
    project_id    TEXT    NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    -- NULL: project rule. Otherwise a workspace of `project_id` (trigger below).
    workspace_id  TEXT    REFERENCES workspaces (id) ON DELETE CASCADE,
    category      TEXT    NOT NULL CHECK (category IN (
                      'file_read', 'file_write', 'file_edit', 'file_delete', 'directory_list',
                      'search', 'shell', 'network', 'external_tool')),
    decision      TEXT    NOT NULL CHECK (decision IN ('allow', 'ask', 'deny')),
    -- {"path_prefix"?: "...", "command"?: "..."}; validated by Core.
    conditions    TEXT    NOT NULL CHECK (json_valid(conditions)),
    description   TEXT    NOT NULL CHECK (length(trim(description)) > 0),
    enabled       INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    status        TEXT    NOT NULL CHECK (status IN ('active', 'archived')),
    revision      INTEGER NOT NULL CHECK (revision >= 1),
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    archived_at   INTEGER,
    CHECK ((status = 'archived') = (archived_at IS NOT NULL)),
    -- A shell rule may only allow one exact command.
    CHECK (NOT (category = 'shell' AND decision = 'allow' AND json_extract(conditions, '$.command') IS NULL)),
    CHECK (NOT (category IN ('network', 'external_tool') AND decision = 'allow'))
) STRICT;
CREATE INDEX policy_rules_scope ON policy_rules (project_id, workspace_id, status);

CREATE TRIGGER policy_rules_workspace_scope_insert BEFORE INSERT ON policy_rules
WHEN NEW.workspace_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM workspaces WHERE id = NEW.workspace_id AND project_id = NEW.project_id)
BEGIN
    SELECT RAISE(ABORT, 'policy rule workspace does not belong to the rule project');
END;

CREATE TRIGGER policy_rules_scope_immutable BEFORE UPDATE OF project_id, workspace_id, category ON policy_rules
WHEN NEW.project_id IS NOT OLD.project_id OR NEW.workspace_id IS NOT OLD.workspace_id
     OR NEW.category IS NOT OLD.category
BEGIN
    SELECT RAISE(ABORT, 'a policy rule cannot change scope or category');
END;

-- Every rule change is one row here. `policy_revision` is the global,
-- strictly increasing revision of the whole policy state.
CREATE TABLE policy_rule_revisions (
    rule_id          TEXT    NOT NULL,
    revision         INTEGER NOT NULL,
    policy_revision  INTEGER NOT NULL UNIQUE CHECK (policy_revision >= 1),
    project_id       TEXT    NOT NULL,
    workspace_id     TEXT,
    change           TEXT    NOT NULL CHECK (change IN
                         ('created', 'updated', 'enabled', 'disabled', 'archived')),
    -- Full snapshot of the rule after the change.
    rule             TEXT    NOT NULL CHECK (json_valid(rule)),
    changed_at       INTEGER NOT NULL,
    changed_by       TEXT,
    PRIMARY KEY (rule_id, revision)
) STRICT;
CREATE INDEX policy_rule_revisions_project ON policy_rule_revisions (project_id, policy_revision);

CREATE TRIGGER policy_rule_revisions_no_update BEFORE UPDATE ON policy_rule_revisions
BEGIN
    SELECT RAISE(ABORT, 'policy rule revisions are append-only');
END;
CREATE TRIGGER policy_rule_revisions_no_delete BEFORE DELETE ON policy_rule_revisions
BEGIN
    SELECT RAISE(ABORT, 'policy rule revisions are append-only');
END;

-- Evaluations ---------------------------------------------------------------
-- Like `events`, no foreign keys: the audit trail outlives what it mentions.

CREATE TABLE policy_evaluations (
    id                      TEXT    PRIMARY KEY NOT NULL,
    evaluated_at            INTEGER NOT NULL,
    -- request: a new permission request; reevaluation: a pending approval
    -- re-checked after a rule change; decision_check: re-checked when an
    -- operator approved.
    trigger                 TEXT    NOT NULL CHECK (trigger IN ('request', 'reevaluation', 'decision_check')),
    project_id              TEXT    NOT NULL,
    workspace_id            TEXT,
    session_id              TEXT    NOT NULL,
    agent_id                TEXT,
    adapter                 TEXT    NOT NULL,
    tool_name               TEXT    NOT NULL,
    native_request_id       TEXT    NOT NULL,
    category                TEXT    NOT NULL,
    -- {"operation": {...}, "paths": [...]}: what was evaluated (no file contents).
    input                   TEXT    NOT NULL CHECK (json_valid(input)),
    decision                TEXT    NOT NULL CHECK (decision IN ('allow', 'ask', 'deny')),
    reason                  TEXT    NOT NULL CHECK (reason IN ('system_restriction', 'rule', 'default', 'capped')),
    explanation             TEXT    NOT NULL,
    deciding_rule_id        TEXT,
    deciding_rule_revision  INTEGER,
    matched_rules           TEXT    NOT NULL CHECK (json_valid(matched_rules)),
    restrictions            TEXT    NOT NULL CHECK (json_valid(restrictions)),
    policy_revision         INTEGER NOT NULL,
    system_version          INTEGER NOT NULL,
    -- The approval this evaluation created (ask) or settled (policy denial).
    approval_id             TEXT
) STRICT;
CREATE INDEX policy_evaluations_project ON policy_evaluations (project_id, evaluated_at);
CREATE INDEX policy_evaluations_session ON policy_evaluations (session_id, evaluated_at);
CREATE INDEX policy_evaluations_approval ON policy_evaluations (approval_id) WHERE approval_id IS NOT NULL;
CREATE INDEX policy_evaluations_rule ON policy_evaluations (deciding_rule_id) WHERE deciding_rule_id IS NOT NULL;

CREATE TRIGGER policy_evaluations_no_update BEFORE UPDATE ON policy_evaluations
BEGIN
    SELECT RAISE(ABORT, 'policy evaluations are append-only');
END;
CREATE TRIGGER policy_evaluations_no_delete BEFORE DELETE ON policy_evaluations
BEGIN
    SELECT RAISE(ABORT, 'policy evaluations are append-only');
END;

-- Approvals: link to the evaluation; new decision source `policy` ------------

CREATE TABLE approvals_v4 (
    id                 TEXT    PRIMARY KEY NOT NULL,
    project_id         TEXT    NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    session_id         TEXT    NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    agent_id           TEXT    REFERENCES agents (id) ON DELETE SET NULL,
    task_id            TEXT    REFERENCES tasks (id) ON DELETE SET NULL,
    kind               TEXT    NOT NULL CHECK (kind IN ('tool_use')),
    tool_name          TEXT    NOT NULL,
    tool_use_id        TEXT,
    native_request_id  TEXT    NOT NULL,
    request            TEXT    NOT NULL CHECK (json_valid(request)),
    status             TEXT    NOT NULL CHECK (status IN
                           ('pending', 'approved', 'denied', 'expired', 'cancelled')),
    requested_at       INTEGER NOT NULL,
    expires_at         INTEGER NOT NULL,
    decided_at         INTEGER,
    decision_source    TEXT    CHECK (decision_source IN (
                           'operator', 'timeout', 'agent_cancelled', 'agent_exited',
                           'core_shutdown', 'core_restart', 'policy')),
    decided_by         TEXT,
    decision_note      TEXT,
    -- The evaluation that created this approval; NULL for approvals recorded
    -- before policies existed.
    evaluation_id      TEXT,
    CHECK ((status = 'pending') = (decided_at IS NULL)),
    -- A policy can only ever deny.
    CHECK (decision_source IS NOT 'policy' OR status = 'denied'),
    UNIQUE (session_id, native_request_id)
) STRICT;

INSERT INTO approvals_v4 (id, project_id, session_id, agent_id, task_id, kind, tool_name, tool_use_id,
                          native_request_id, request, status, requested_at, expires_at, decided_at,
                          decision_source, decided_by, decision_note, evaluation_id)
SELECT id, project_id, session_id, agent_id, task_id, kind, tool_name, tool_use_id,
       native_request_id, request, status, requested_at, expires_at, decided_at,
       decision_source, decided_by, decision_note, NULL
FROM approvals;

DROP TABLE approvals;
ALTER TABLE approvals_v4 RENAME TO approvals;
CREATE INDEX approvals_status ON approvals (status, requested_at);
CREATE INDEX approvals_session ON approvals (session_id);
