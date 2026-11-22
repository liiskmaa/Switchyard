-- Switchyard schema v11: per-project auto mode (ADR 0037).
--
-- * `policy_project_settings`: the current setting; no row means off.
-- * `policy_settings_revisions`: append-only history. Each change takes the
--   next global policy revision, shared with `policy_rule_revisions`, so an
--   evaluation's `policy_revision` still names one exact policy state.
--
-- * `policy_evaluations` is rebuilt so `reason` accepts `auto_mode`; every
--   recorded evaluation is copied unchanged (nothing references the table).
--
-- Nothing is turned on here: every existing project keeps auto mode off.

CREATE TABLE policy_project_settings (
    project_id  TEXT    PRIMARY KEY NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    auto_mode   INTEGER NOT NULL CHECK (auto_mode IN (0, 1)),
    revision    INTEGER NOT NULL CHECK (revision >= 1),
    updated_at  INTEGER NOT NULL,
    updated_by  TEXT
) STRICT;

-- Like `policy_rule_revisions`, no foreign key: the history outlives the project.
CREATE TABLE policy_settings_revisions (
    project_id       TEXT    NOT NULL,
    revision         INTEGER NOT NULL CHECK (revision >= 1),
    policy_revision  INTEGER NOT NULL UNIQUE CHECK (policy_revision >= 1),
    auto_mode        INTEGER NOT NULL CHECK (auto_mode IN (0, 1)),
    changed_at       INTEGER NOT NULL,
    changed_by       TEXT,
    PRIMARY KEY (project_id, revision)
) STRICT;

CREATE TRIGGER policy_settings_revisions_no_update BEFORE UPDATE ON policy_settings_revisions
BEGIN
    SELECT RAISE(ABORT, 'policy settings revisions are append-only');
END;
CREATE TRIGGER policy_settings_revisions_no_delete BEFORE DELETE ON policy_settings_revisions
BEGIN
    SELECT RAISE(ABORT, 'policy settings revisions are append-only');
END;

-- A rule change and a settings change never share a global revision.
CREATE TRIGGER policy_settings_revisions_unique_global BEFORE INSERT ON policy_settings_revisions
WHEN EXISTS (SELECT 1 FROM policy_rule_revisions WHERE policy_revision = NEW.policy_revision)
BEGIN
    SELECT RAISE(ABORT, 'policy revision already used by a rule change');
END;
CREATE TRIGGER policy_rule_revisions_unique_global BEFORE INSERT ON policy_rule_revisions
WHEN EXISTS (SELECT 1 FROM policy_settings_revisions WHERE policy_revision = NEW.policy_revision)
BEGIN
    SELECT RAISE(ABORT, 'policy revision already used by a settings change');
END;

-- Evaluations: accept the reason `auto_mode` ---------------------------------

CREATE TABLE policy_evaluations_v11 (
    id                      TEXT    PRIMARY KEY NOT NULL,
    evaluated_at            INTEGER NOT NULL,
    trigger                 TEXT    NOT NULL CHECK (trigger IN ('request', 'reevaluation', 'decision_check')),
    project_id              TEXT    NOT NULL,
    workspace_id            TEXT,
    session_id              TEXT    NOT NULL,
    agent_id                TEXT,
    adapter                 TEXT    NOT NULL,
    tool_name               TEXT    NOT NULL,
    native_request_id       TEXT    NOT NULL,
    category                TEXT    NOT NULL,
    input                   TEXT    NOT NULL CHECK (json_valid(input)),
    decision                TEXT    NOT NULL CHECK (decision IN ('allow', 'ask', 'deny')),
    reason                  TEXT    NOT NULL CHECK (reason IN ('system_restriction', 'rule', 'default', 'capped', 'auto_mode')),
    explanation             TEXT    NOT NULL,
    deciding_rule_id        TEXT,
    deciding_rule_revision  INTEGER,
    matched_rules           TEXT    NOT NULL CHECK (json_valid(matched_rules)),
    restrictions            TEXT    NOT NULL CHECK (json_valid(restrictions)),
    policy_revision         INTEGER NOT NULL,
    system_version          INTEGER NOT NULL,
    approval_id             TEXT
) STRICT;

INSERT INTO policy_evaluations_v11 (id, evaluated_at, trigger, project_id, workspace_id, session_id, agent_id,
    adapter, tool_name, native_request_id, category, input, decision, reason, explanation, deciding_rule_id,
    deciding_rule_revision, matched_rules, restrictions, policy_revision, system_version, approval_id)
SELECT id, evaluated_at, trigger, project_id, workspace_id, session_id, agent_id,
    adapter, tool_name, native_request_id, category, input, decision, reason, explanation, deciding_rule_id,
    deciding_rule_revision, matched_rules, restrictions, policy_revision, system_version, approval_id
FROM policy_evaluations;

-- Dropping a table removes its triggers without firing them.
DROP TABLE policy_evaluations;
ALTER TABLE policy_evaluations_v11 RENAME TO policy_evaluations;

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
