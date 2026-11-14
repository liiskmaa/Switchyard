-- Milestone 6: task orchestration (ADRs 0023-0027).
--
-- The milestone-1 placeholders `tasks`, `messages` and `findings` were never
-- written by any code; they are replaced by the structures below. Other
-- tables that reference `tasks` (`approvals.task_id`, `decisions.task_id`,
-- ON DELETE SET NULL) keep referencing the new table by name.

DROP TABLE findings;
DROP TABLE messages;
DROP TABLE tasks;

-- A unit of engineering work, independent of any agent session.
CREATE TABLE tasks (
    id                  TEXT    PRIMARY KEY NOT NULL,
    project_id          TEXT    NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    parent_task_id      TEXT    REFERENCES tasks (id) ON DELETE SET NULL,
    title               TEXT    NOT NULL CHECK (length(title) BETWEEN 1 AND 200),
    description         TEXT,
    kind                TEXT    NOT NULL CHECK (kind IN ('investigation', 'implementation', 'general')),
    priority            TEXT    NOT NULL CHECK (priority IN ('low', 'normal', 'high', 'urgent')),
    acceptance_criteria TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(acceptance_criteria)),
    status              TEXT    NOT NULL CHECK (status IN
                            ('draft', 'ready', 'blocked', 'assigned', 'in_progress', 'awaiting_review',
                             'in_review', 'completed', 'failed', 'cancelled')),
    status_reason       TEXT,
    -- The result the operator accepted (summary and report), set on completion.
    result              TEXT    CHECK (result IS NULL OR json_valid(result)),
    failure             TEXT,
    -- 'operator' or 'proposal' (created from an approved agent proposal).
    origin              TEXT    NOT NULL CHECK (origin IN ('operator', 'proposal')),
    origin_proposal_id  TEXT,
    -- Optimistic concurrency for edits and transitions.
    revision            INTEGER NOT NULL DEFAULT 1,
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL,
    completed_at        INTEGER
) STRICT;
CREATE INDEX tasks_project ON tasks (project_id, status);
CREATE INDEX tasks_parent ON tasks (parent_task_id);

-- `task_id` may only start once `depends_on` is completed (ADR 0024).
CREATE TABLE task_dependencies (
    task_id    TEXT    NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    depends_on TEXT    NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (task_id, depends_on),
    CHECK (task_id <> depends_on)
) STRICT;
CREATE INDEX task_dependencies_reverse ON task_dependencies (depends_on);

CREATE TRIGGER task_dependencies_same_project BEFORE INSERT ON task_dependencies
WHEN (SELECT project_id FROM tasks WHERE id = NEW.task_id)
     IS NOT (SELECT project_id FROM tasks WHERE id = NEW.depends_on)
BEGIN
    SELECT RAISE(ABORT, 'task dependencies must stay within one project');
END;

-- One agent's piece of work on a task: an investigation, an implementation
-- turn or a review, run by one session. A task has many over time (ADR 0023).
CREATE TABLE task_assignments (
    id                 TEXT    PRIMARY KEY NOT NULL,
    task_id            TEXT    NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    project_id         TEXT    NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    role               TEXT    NOT NULL CHECK (role IN ('investigate', 'implement', 'review')),
    provider           TEXT    NOT NULL CHECK (provider IN ('claude_code', 'codex')),
    workspace_id       TEXT    NOT NULL REFERENCES workspaces (id),
    session_id         TEXT    REFERENCES sessions (id) ON DELETE SET NULL,
    status             TEXT    NOT NULL CHECK (status IN ('pending', 'active', 'reported', 'failed', 'cancelled')),
    -- The task context exactly as sent to the agent (ADR 0025).
    context            TEXT    CHECK (context IS NULL OR json_valid(context)),
    context_version    INTEGER,
    -- Review assignments: workspace fingerprint when the review started (ADR 0027).
    review_fingerprint TEXT,
    -- Highest journal seq of the session's events already processed (idempotency).
    last_event_seq     INTEGER NOT NULL DEFAULT 0,
    outcome            TEXT,
    created_at         INTEGER NOT NULL,
    started_at         INTEGER,
    ended_at           INTEGER
) STRICT;
CREATE INDEX task_assignments_task ON task_assignments (task_id, created_at);
CREATE INDEX task_assignments_session ON task_assignments (session_id);
-- At most one open assignment per task.
CREATE UNIQUE INDEX task_assignments_one_open ON task_assignments (task_id)
    WHERE status IN ('pending', 'active');

-- Structured, durable messages about a task (ADR 0025). A message from an
-- agent is a claim or a request; it never authorizes anything.
CREATE TABLE messages (
    id                      TEXT    PRIMARY KEY NOT NULL,
    project_id              TEXT    NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    task_id                 TEXT    NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    assignment_id           TEXT    REFERENCES task_assignments (id) ON DELETE SET NULL,
    kind                    TEXT    NOT NULL CHECK (kind IN
                                ('task_assignment', 'status_update', 'info_request', 'findings', 'proposal',
                                 'review_result', 'blocker', 'delegation_request', 'completion_report',
                                 'operator_note', 'malformed_report')),
    sender                  TEXT    NOT NULL CHECK (sender IN ('agent', 'operator', 'core')),
    sender_session_id       TEXT    REFERENCES sessions (id) ON DELETE SET NULL,
    recipient               TEXT    NOT NULL CHECK (recipient IN ('operator', 'agent', 'task')),
    recipient_assignment_id TEXT    REFERENCES task_assignments (id) ON DELETE SET NULL,
    payload                 TEXT    NOT NULL CHECK (json_valid(payload)),
    -- What caused it: a journal event id (agent turn) or another message id.
    causation_id            TEXT,
    -- Groups the conversation: the task id.
    correlation_id          TEXT    NOT NULL,
    -- recorded: kept for the operator; pending: waits for the next turn of an
    -- agent on this task; delivered: included in a turn's context.
    delivery                TEXT    NOT NULL CHECK (delivery IN ('recorded', 'pending', 'delivered')),
    delivered_at            INTEGER,
    created_at              INTEGER NOT NULL
) STRICT;
CREATE INDEX messages_task ON messages (task_id, created_at);
-- An agent turn is recorded once (replay-safe).
CREATE UNIQUE INDEX messages_one_per_turn ON messages (assignment_id, causation_id)
    WHERE sender = 'agent' AND causation_id IS NOT NULL;

-- Findings from a review (or an investigation) about a task.
CREATE TABLE findings (
    id            TEXT    PRIMARY KEY NOT NULL,
    project_id    TEXT    NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    task_id       TEXT    NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    message_id    TEXT    NOT NULL REFERENCES messages (id) ON DELETE CASCADE,
    assignment_id TEXT    REFERENCES task_assignments (id) ON DELETE SET NULL,
    severity      TEXT    NOT NULL CHECK (severity IN ('info', 'low', 'medium', 'high', 'critical')),
    title         TEXT    NOT NULL,
    explanation   TEXT,
    suggestion    TEXT,
    path          TEXT,
    line          INTEGER,
    status        TEXT    NOT NULL CHECK (status IN ('open', 'resolved', 'dismissed')),
    created_at    INTEGER NOT NULL,
    resolved_at   INTEGER
) STRICT;
CREATE INDEX findings_task ON findings (task_id, status);

-- Subtasks proposed by an agent, awaiting the operator (ADR 0026).
CREATE TABLE task_proposals (
    id              TEXT    PRIMARY KEY NOT NULL,
    project_id      TEXT    NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    task_id         TEXT    NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    message_id      TEXT    NOT NULL REFERENCES messages (id) ON DELETE CASCADE,
    position        INTEGER NOT NULL,
    proposal        TEXT    NOT NULL CHECK (json_valid(proposal)),
    status          TEXT    NOT NULL CHECK (status IN ('pending', 'approved', 'rejected')),
    decided_by      TEXT,
    decision_note   TEXT,
    created_task_id TEXT    REFERENCES tasks (id) ON DELETE SET NULL,
    created_at      INTEGER NOT NULL,
    decided_at      INTEGER,
    UNIQUE (message_id, position)
) STRICT;
CREATE INDEX task_proposals_task ON task_proposals (task_id, status);

-- A task's history in the journal.
CREATE INDEX events_task ON events (task_id, seq) WHERE task_id IS NOT NULL;

-- Review sessions cannot change files (ADR 0027).
ALTER TABLE sessions ADD COLUMN access TEXT NOT NULL DEFAULT 'write' CHECK (access IN ('write', 'review'));
