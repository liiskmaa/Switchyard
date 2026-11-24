//! Tasks, dependencies, assignments, messages, findings and proposals
//! (milestone 6). Transitions are validated against
//! `switchyard_tasks::TaskStatus::transition` and guarded by SQL.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::Value;
use switchyard_tasks::{
    AssignmentStatus, MessageKind, Priority, ProposalStatus, Role, Severity, TaskKind, TaskStatus,
};
use uuid::Uuid;

use crate::{StorageError, parse_enum, parse_uuid};

fn opt_uuid(s: Option<String>, what: &'static str) -> Result<Option<Uuid>, StorageError> {
    s.map(|v| parse_uuid(&v, what)).transpose()
}

fn json(s: &str, what: &'static str) -> Result<Value, StorageError> {
    serde_json::from_str(s).map_err(|e| StorageError::Corrupt(format!("{what}: {e}")))
}

// --- tasks ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct TaskRecord {
    pub id: Uuid,
    pub project_id: Uuid,
    pub parent_task_id: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub kind: TaskKind,
    pub priority: Priority,
    pub acceptance_criteria: Vec<String>,
    pub status: TaskStatus,
    pub status_reason: Option<String>,
    pub result: Option<Value>,
    pub failure: Option<String>,
    /// `operator` or `proposal`.
    pub origin: String,
    pub origin_proposal_id: Option<Uuid>,
    pub revision: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub completed_at_ms: Option<i64>,
}

const TASK_COLUMNS: &str = "id, project_id, parent_task_id, title, description, kind, priority, acceptance_criteria, \
     status, status_reason, result, failure, origin, origin_proposal_id, revision, created_at, updated_at, completed_at";

#[allow(clippy::type_complexity)]
fn task_from_row(r: &Row<'_>) -> rusqlite::Result<Result<TaskRecord, StorageError>> {
    let raw: (
        String,
        String,
        Option<String>,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
    ) = (r.get(0)?, r.get(1)?, r.get(2)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(10)?, r.get(13)?);
    let title: String = r.get(3)?;
    let description: Option<String> = r.get(4)?;
    let status_reason: Option<String> = r.get(9)?;
    let failure: Option<String> = r.get(11)?;
    let origin: String = r.get(12)?;
    let revision: i64 = r.get(14)?;
    let created: i64 = r.get(15)?;
    let updated: i64 = r.get(16)?;
    let completed: Option<i64> = r.get(17)?;
    Ok((|| {
        let (id, project, parent, kind, priority, criteria, status, result, proposal) = raw;
        Ok(TaskRecord {
            id: parse_uuid(&id, "tasks.id")?,
            project_id: parse_uuid(&project, "tasks.project_id")?,
            parent_task_id: opt_uuid(parent, "tasks.parent_task_id")?,
            title,
            description,
            kind: parse_enum(&kind, "tasks.kind")?,
            priority: parse_enum(&priority, "tasks.priority")?,
            acceptance_criteria: serde_json::from_str(&criteria)
                .map_err(|e| StorageError::Corrupt(format!("tasks.acceptance_criteria: {e}")))?,
            status: parse_enum(&status, "tasks.status")?,
            status_reason,
            result: result.map(|r| json(&r, "tasks.result")).transpose()?,
            failure,
            origin,
            origin_proposal_id: opt_uuid(proposal, "tasks.origin_proposal_id")?,
            revision,
            created_at_ms: created,
            updated_at_ms: updated,
            completed_at_ms: completed,
        })
    })())
}

pub fn insert_task(conn: &Connection, t: &TaskRecord) -> Result<(), StorageError> {
    conn.execute(
        &format!("INSERT INTO tasks ({TASK_COLUMNS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)"),
        params![
            t.id.to_string(),
            t.project_id.to_string(),
            t.parent_task_id.map(|u| u.to_string()),
            t.title,
            t.description,
            t.kind.as_str(),
            t.priority.as_str(),
            serde_json::to_string(&t.acceptance_criteria).unwrap_or_else(|_| "[]".into()),
            t.status.as_str(),
            t.status_reason,
            t.result.as_ref().map(Value::to_string),
            t.failure,
            t.origin,
            t.origin_proposal_id.map(|u| u.to_string()),
            t.revision,
            t.created_at_ms,
            t.updated_at_ms,
            t.completed_at_ms,
        ],
    )?;
    Ok(())
}

pub fn find_task(conn: &Connection, id: Uuid) -> Result<Option<TaskRecord>, StorageError> {
    conn.query_row(
        &format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1"),
        [id.to_string()],
        task_from_row,
    )
    .optional()?
    .transpose()
}

/// The ids of the tasks whose parent is `parent`.
pub fn child_task_ids(conn: &Connection, parent: Uuid) -> Result<Vec<Uuid>, StorageError> {
    let mut stmt = conn.prepare("SELECT id FROM tasks WHERE parent_task_id = ?1 ORDER BY created_at")?;
    let rows = stmt.query_map([parent.to_string()], |r| r.get::<_, String>(0))?;
    rows.map(|r| Uuid::parse_str(&r?).map_err(|e| StorageError::Corrupt(e.to_string()))).collect()
}

pub fn get_task(conn: &Connection, id: Uuid) -> Result<TaskRecord, StorageError> {
    find_task(conn, id)?.ok_or_else(|| StorageError::NotFound(format!("task {id}")))
}

pub fn list_tasks(
    conn: &Connection,
    project_id: Option<Uuid>,
    include_closed: bool,
    limit: u32,
) -> Result<Vec<TaskRecord>, StorageError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {TASK_COLUMNS} FROM tasks WHERE (?1 IS NULL OR project_id = ?1) \
         AND (?2 OR status NOT IN ('completed', 'cancelled')) ORDER BY created_at DESC LIMIT ?3"
    ))?;
    let rows =
        stmt.query_map(params![project_id.map(|u| u.to_string()), include_closed, limit], task_from_row)?;
    rows.map(|r| r?).collect()
}

/// Editable fields; `None` keeps the current value.
#[derive(Debug, Clone, Default)]
pub struct TaskEdit {
    pub title: Option<String>,
    pub description: Option<Option<String>>,
    pub kind: Option<TaskKind>,
    pub priority: Option<Priority>,
    pub acceptance_criteria: Option<Vec<String>>,
}

/// Applies `edit` if the task is still at `expected_revision`.
pub fn edit_task(
    conn: &Connection,
    id: Uuid,
    expected_revision: i64,
    edit: &TaskEdit,
    now_ms: i64,
) -> Result<TaskRecord, StorageError> {
    let current = get_task(conn, id)?;
    if current.revision != expected_revision {
        return Err(StorageError::Conflict(format!(
            "task {id} is at revision {}, not {expected_revision}",
            current.revision
        )));
    }
    let criteria = edit.acceptance_criteria.as_ref().unwrap_or(&current.acceptance_criteria);
    let n = conn.execute(
        "UPDATE tasks SET title = ?1, description = ?2, kind = ?3, priority = ?4, acceptance_criteria = ?5, \
         revision = revision + 1, updated_at = ?6 WHERE id = ?7 AND revision = ?8",
        params![
            edit.title.as_ref().unwrap_or(&current.title),
            edit.description.clone().unwrap_or(current.description.clone()),
            edit.kind.unwrap_or(current.kind).as_str(),
            edit.priority.unwrap_or(current.priority).as_str(),
            serde_json::to_string(criteria).unwrap_or_else(|_| "[]".into()),
            now_ms,
            id.to_string(),
            expected_revision,
        ],
    )?;
    if n == 0 {
        return Err(StorageError::Conflict(format!("task {id} changed concurrently")));
    }
    get_task(conn, id)
}

/// Validated status change (state machine + SQL guard on the current status).
pub struct StatusChange<'a> {
    pub reason: Option<&'a str>,
    pub result: Option<&'a Value>,
    pub failure: Option<&'a str>,
}

pub fn set_task_status(
    conn: &Connection,
    id: Uuid,
    to: TaskStatus,
    change: StatusChange<'_>,
    now_ms: i64,
) -> Result<(TaskRecord, TaskRecord), StorageError> {
    let before = get_task(conn, id)?;
    before.status.transition(to).map_err(|e| StorageError::Conflict(format!("task {id}: {e}")))?;
    let completed = (to == TaskStatus::Completed).then_some(now_ms);
    let n = conn.execute(
        "UPDATE tasks SET status = ?1, status_reason = ?2, result = COALESCE(?3, result), \
         failure = CASE WHEN ?1 = 'failed' THEN ?4 WHEN ?1 IN ('ready', 'assigned') THEN NULL ELSE failure END, \
         completed_at = COALESCE(?5, completed_at), revision = revision + 1, updated_at = ?6 \
         WHERE id = ?7 AND status = ?8",
        params![
            to.as_str(),
            change.reason,
            change.result.map(Value::to_string),
            change.failure,
            completed,
            now_ms,
            id.to_string(),
            before.status.as_str(),
        ],
    )?;
    if n == 0 {
        return Err(StorageError::Conflict(format!("task {id} changed concurrently")));
    }
    Ok((before, get_task(conn, id)?))
}

// --- dependencies ----------------------------------------------------------------

pub fn add_dependency(
    conn: &Connection,
    task: Uuid,
    depends_on: Uuid,
    now_ms: i64,
) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO task_dependencies (task_id, depends_on, created_at) VALUES (?1, ?2, ?3)",
        params![task.to_string(), depends_on.to_string(), now_ms],
    )?;
    Ok(())
}

pub fn remove_dependency(conn: &Connection, task: Uuid, depends_on: Uuid) -> Result<bool, StorageError> {
    Ok(conn.execute(
        "DELETE FROM task_dependencies WHERE task_id = ?1 AND depends_on = ?2",
        params![task.to_string(), depends_on.to_string()],
    )? > 0)
}

/// `(depends_on, its status)` for `task`.
pub fn dependencies_of(conn: &Connection, task: Uuid) -> Result<Vec<(Uuid, TaskStatus)>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT d.depends_on, t.status FROM task_dependencies d JOIN tasks t ON t.id = d.depends_on \
         WHERE d.task_id = ?1 ORDER BY d.created_at",
    )?;
    let rows =
        stmt.query_map([task.to_string()], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    rows.map(|r| {
        let (id, status) = r?;
        Ok((parse_uuid(&id, "task_dependencies.depends_on")?, parse_enum(&status, "tasks.status")?))
    })
    .collect()
}

/// Tasks that depend on `task`.
pub fn dependents_of(conn: &Connection, task: Uuid) -> Result<Vec<Uuid>, StorageError> {
    let mut stmt = conn.prepare("SELECT task_id FROM task_dependencies WHERE depends_on = ?1")?;
    let rows = stmt.query_map([task.to_string()], |r| r.get::<_, String>(0))?;
    rows.map(|r| parse_uuid(&r?, "task_dependencies.task_id")).collect()
}

/// Every dependency edge in a project (for cycle detection).
pub fn project_edges(conn: &Connection, project_id: Uuid) -> Result<Vec<(Uuid, Uuid)>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT d.task_id, d.depends_on FROM task_dependencies d JOIN tasks t ON t.id = d.task_id \
         WHERE t.project_id = ?1",
    )?;
    let rows =
        stmt.query_map([project_id.to_string()], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    rows.map(|r| {
        let (a, b) = r?;
        Ok((parse_uuid(&a, "task_dependencies.task_id")?, parse_uuid(&b, "task_dependencies.depends_on")?))
    })
    .collect()
}

// --- assignments -------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct AssignmentRecord {
    pub id: Uuid,
    pub task_id: Uuid,
    pub project_id: Uuid,
    pub role: Role,
    /// `claude_code` or `codex`.
    pub provider: String,
    pub workspace_id: Uuid,
    pub session_id: Option<Uuid>,
    pub status: AssignmentStatus,
    pub context: Option<Value>,
    pub context_version: Option<u32>,
    pub review_fingerprint: Option<String>,
    pub last_event_seq: i64,
    pub outcome: Option<String>,
    pub created_at_ms: i64,
    pub started_at_ms: Option<i64>,
    pub ended_at_ms: Option<i64>,
    /// `operator` or `workflow`.
    pub origin: String,
    /// The workflow step (`implement#2`), for workflow assignments.
    pub step_key: Option<String>,
    /// The model asked for; `None` = the CLI's default.
    pub model: Option<String>,
}

const ASSIGNMENT_COLUMNS: &str = "id, task_id, project_id, role, provider, workspace_id, session_id, status, context, \
     context_version, review_fingerprint, last_event_seq, outcome, created_at, started_at, ended_at, origin, step_key, model";

fn assignment_from_row(r: &Row<'_>) -> rusqlite::Result<Result<AssignmentRecord, StorageError>> {
    let ids: (String, String, String, String, Option<String>) =
        (r.get(0)?, r.get(1)?, r.get(2)?, r.get(5)?, r.get(6)?);
    let role: String = r.get(3)?;
    let provider: String = r.get(4)?;
    let status: String = r.get(7)?;
    let context: Option<String> = r.get(8)?;
    let version: Option<i64> = r.get(9)?;
    let fingerprint: Option<String> = r.get(10)?;
    let seq: i64 = r.get(11)?;
    let outcome: Option<String> = r.get(12)?;
    let times: (i64, Option<i64>, Option<i64>) = (r.get(13)?, r.get(14)?, r.get(15)?);
    let meta: (String, Option<String>, Option<String>) = (r.get(16)?, r.get(17)?, r.get(18)?);
    Ok((|| {
        Ok(AssignmentRecord {
            id: parse_uuid(&ids.0, "task_assignments.id")?,
            task_id: parse_uuid(&ids.1, "task_assignments.task_id")?,
            project_id: parse_uuid(&ids.2, "task_assignments.project_id")?,
            role: parse_enum(&role, "task_assignments.role")?,
            provider,
            workspace_id: parse_uuid(&ids.3, "task_assignments.workspace_id")?,
            session_id: opt_uuid(ids.4, "task_assignments.session_id")?,
            status: parse_enum(&status, "task_assignments.status")?,
            context: context.map(|c| json(&c, "task_assignments.context")).transpose()?,
            context_version: version.and_then(|v| u32::try_from(v).ok()),
            review_fingerprint: fingerprint,
            last_event_seq: seq,
            outcome,
            created_at_ms: times.0,
            started_at_ms: times.1,
            ended_at_ms: times.2,
            origin: meta.0,
            step_key: meta.1,
            model: meta.2,
        })
    })())
}

pub fn insert_assignment(conn: &Connection, a: &AssignmentRecord) -> Result<(), StorageError> {
    conn.execute(
        &format!("INSERT INTO task_assignments ({ASSIGNMENT_COLUMNS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)"),
        params![
            a.id.to_string(),
            a.task_id.to_string(),
            a.project_id.to_string(),
            a.role.as_str(),
            a.provider,
            a.workspace_id.to_string(),
            a.session_id.map(|u| u.to_string()),
            a.status.as_str(),
            a.context.as_ref().map(Value::to_string),
            a.context_version,
            a.review_fingerprint,
            a.last_event_seq,
            a.outcome,
            a.created_at_ms,
            a.started_at_ms,
            a.ended_at_ms,
            a.origin,
            a.step_key,
            a.model,
        ],
    )
    .map_err(|e| match e {
        rusqlite::Error::SqliteFailure(f, Some(m)) if f.code == rusqlite::ErrorCode::ConstraintViolation && m.contains("step_key") => {
            StorageError::Conflict("this workflow step already has an assignment".into())
        }
        rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation => {
            StorageError::Conflict("the task already has an open assignment".into())
        }
        other => other.into(),
    })?;
    Ok(())
}

pub fn get_assignment(conn: &Connection, id: Uuid) -> Result<AssignmentRecord, StorageError> {
    conn.query_row(
        &format!("SELECT {ASSIGNMENT_COLUMNS} FROM task_assignments WHERE id = ?1"),
        [id.to_string()],
        assignment_from_row,
    )
    .optional()?
    .ok_or_else(|| StorageError::NotFound(format!("assignment {id}")))?
}

pub fn assignments_for_task(conn: &Connection, task: Uuid) -> Result<Vec<AssignmentRecord>, StorageError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ASSIGNMENT_COLUMNS} FROM task_assignments WHERE task_id = ?1 ORDER BY created_at"
    ))?;
    let rows = stmt.query_map([task.to_string()], assignment_from_row)?;
    rows.map(|r| r?).collect()
}

pub fn open_assignment(conn: &Connection, task: Uuid) -> Result<Option<AssignmentRecord>, StorageError> {
    conn.query_row(
        &format!("SELECT {ASSIGNMENT_COLUMNS} FROM task_assignments WHERE task_id = ?1 AND status IN ('pending','active')"),
        [task.to_string()],
        assignment_from_row,
    )
    .optional()?
    .transpose()
}

/// Active assignments (for recovery and event processing).
pub fn active_assignments(conn: &Connection) -> Result<Vec<AssignmentRecord>, StorageError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ASSIGNMENT_COLUMNS} FROM task_assignments WHERE status = 'active' ORDER BY created_at"
    ))?;
    let rows = stmt.query_map([], assignment_from_row)?;
    rows.map(|r| r?).collect()
}

/// The active assignment whose session is `session_id`, if any.
pub fn active_assignment_for_session(
    conn: &Connection,
    session_id: Uuid,
) -> Result<Option<AssignmentRecord>, StorageError> {
    conn.query_row(
        &format!(
            "SELECT {ASSIGNMENT_COLUMNS} FROM task_assignments WHERE session_id = ?1 AND status = 'active'"
        ),
        [session_id.to_string()],
        assignment_from_row,
    )
    .optional()?
    .transpose()
}

/// Open assignments with write access on a workspace, other than `except_task`.
pub fn open_writers_on_workspace(
    conn: &Connection,
    workspace_id: Uuid,
    except_task: Uuid,
) -> Result<Vec<Uuid>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT task_id FROM task_assignments WHERE workspace_id = ?1 AND task_id <> ?2 \
         AND status IN ('pending','active') AND role <> 'review'",
    )?;
    let rows = stmt
        .query_map(params![workspace_id.to_string(), except_task.to_string()], |r| r.get::<_, String>(0))?;
    rows.map(|r| parse_uuid(&r?, "task_assignments.task_id")).collect()
}

/// Open assignments (any role) on a workspace for tasks other than `except_task`.
pub fn open_assignments_on_workspace(
    conn: &Connection,
    workspace_id: Uuid,
    except_task: Uuid,
) -> Result<Vec<(Uuid, Role)>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT task_id, role FROM task_assignments WHERE workspace_id = ?1 AND task_id <> ?2 \
         AND status IN ('pending','active') ORDER BY created_at",
    )?;
    let rows = stmt.query_map(params![workspace_id.to_string(), except_task.to_string()], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    rows.map(|r| {
        let (t, role) = r?;
        Ok((parse_uuid(&t, "task_assignments.task_id")?, parse_enum(&role, "task_assignments.role")?))
    })
    .collect()
}

/// Moves an assignment from `from` to `to` (SQL-guarded).
#[allow(clippy::too_many_arguments)]
pub fn set_assignment_status(
    conn: &Connection,
    id: Uuid,
    from: &[AssignmentStatus],
    to: AssignmentStatus,
    outcome: Option<&str>,
    now_ms: i64,
) -> Result<AssignmentRecord, StorageError> {
    let current = get_assignment(conn, id)?;
    if !from.contains(&current.status) {
        return Err(StorageError::Conflict(format!("assignment {id} is {}, not {:?}", current.status, from)));
    }
    let ended = (!to.is_open()).then_some(now_ms);
    let n = conn.execute(
        "UPDATE task_assignments SET status = ?1, outcome = COALESCE(?2, outcome), ended_at = COALESCE(?3, ended_at) \
         WHERE id = ?4 AND status = ?5",
        params![to.as_str(), outcome, ended, id.to_string(), current.status.as_str()],
    )?;
    if n == 0 {
        return Err(StorageError::Conflict(format!("assignment {id} changed concurrently")));
    }
    get_assignment(conn, id)
}

/// Records the session, context and start of a pending assignment.
#[allow(clippy::too_many_arguments)]
pub fn start_assignment(
    conn: &Connection,
    id: Uuid,
    session_id: Uuid,
    context: &Value,
    context_version: u32,
    review_fingerprint: Option<&str>,
    after_seq: i64,
    now_ms: i64,
) -> Result<AssignmentRecord, StorageError> {
    let n = conn.execute(
        "UPDATE task_assignments SET status = 'active', session_id = ?1, context = ?2, context_version = ?3, \
         review_fingerprint = ?4, last_event_seq = ?5, started_at = ?6 WHERE id = ?7 AND status = 'pending'",
        params![
            session_id.to_string(),
            context.to_string(),
            context_version,
            review_fingerprint,
            after_seq,
            now_ms,
            id.to_string()
        ],
    )?;
    if n == 0 {
        return Err(StorageError::Conflict(format!("assignment {id} is not pending")));
    }
    get_assignment(conn, id)
}

/// Advances the processed-event mark. Returns `false` if `seq` was already
/// processed (replay), so the caller does nothing twice.
pub fn advance_assignment_seq(conn: &Connection, id: Uuid, seq: i64) -> Result<bool, StorageError> {
    Ok(conn.execute(
        "UPDATE task_assignments SET last_event_seq = ?1 WHERE id = ?2 AND last_event_seq < ?1",
        params![seq, id.to_string()],
    )? > 0)
}

// --- messages ----------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct MessageRecord {
    pub id: Uuid,
    pub project_id: Uuid,
    pub task_id: Uuid,
    pub assignment_id: Option<Uuid>,
    pub kind: MessageKind,
    /// `agent`, `operator` or `core`.
    pub sender: String,
    pub sender_session_id: Option<Uuid>,
    /// `operator`, `agent` or `task`.
    pub recipient: String,
    pub recipient_assignment_id: Option<Uuid>,
    pub payload: Value,
    pub causation_id: Option<String>,
    pub correlation_id: String,
    /// `recorded`, `pending` or `delivered`.
    pub delivery: String,
    pub delivered_at_ms: Option<i64>,
    pub created_at_ms: i64,
}

const MESSAGE_COLUMNS: &str = "id, project_id, task_id, assignment_id, kind, sender, sender_session_id, recipient, \
     recipient_assignment_id, payload, causation_id, correlation_id, delivery, delivered_at, created_at";

fn message_from_row(r: &Row<'_>) -> rusqlite::Result<Result<MessageRecord, StorageError>> {
    let ids: (String, String, String, Option<String>, Option<String>, Option<String>) =
        (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(6)?, r.get(8)?);
    let kind: String = r.get(4)?;
    let sender: String = r.get(5)?;
    let recipient: String = r.get(7)?;
    let payload: String = r.get(9)?;
    let causation: Option<String> = r.get(10)?;
    let correlation: String = r.get(11)?;
    let delivery: String = r.get(12)?;
    let delivered: Option<i64> = r.get(13)?;
    let created: i64 = r.get(14)?;
    Ok((|| {
        Ok(MessageRecord {
            id: parse_uuid(&ids.0, "messages.id")?,
            project_id: parse_uuid(&ids.1, "messages.project_id")?,
            task_id: parse_uuid(&ids.2, "messages.task_id")?,
            assignment_id: opt_uuid(ids.3, "messages.assignment_id")?,
            kind: parse_enum(&kind, "messages.kind")?,
            sender,
            sender_session_id: opt_uuid(ids.4, "messages.sender_session_id")?,
            recipient,
            recipient_assignment_id: opt_uuid(ids.5, "messages.recipient_assignment_id")?,
            payload: json(&payload, "messages.payload")?,
            causation_id: causation,
            correlation_id: correlation,
            delivery,
            delivered_at_ms: delivered,
            created_at_ms: created,
        })
    })())
}

/// Inserts a message. Returns `false` (and inserts nothing) when the same
/// agent turn was already recorded (replay).
pub fn insert_message(conn: &Connection, m: &MessageRecord) -> Result<bool, StorageError> {
    let n = conn.execute(
        &format!("INSERT OR IGNORE INTO messages ({MESSAGE_COLUMNS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)"),
        params![
            m.id.to_string(),
            m.project_id.to_string(),
            m.task_id.to_string(),
            m.assignment_id.map(|u| u.to_string()),
            m.kind.as_str(),
            m.sender,
            m.sender_session_id.map(|u| u.to_string()),
            m.recipient,
            m.recipient_assignment_id.map(|u| u.to_string()),
            m.payload.to_string(),
            m.causation_id,
            m.correlation_id,
            m.delivery,
            m.delivered_at_ms,
            m.created_at_ms,
        ],
    )?;
    Ok(n > 0)
}

pub fn messages_for_task(conn: &Connection, task: Uuid) -> Result<Vec<MessageRecord>, StorageError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {MESSAGE_COLUMNS} FROM messages WHERE task_id = ?1 ORDER BY created_at, id"
    ))?;
    let rows = stmt.query_map([task.to_string()], message_from_row)?;
    rows.map(|r| r?).collect()
}

/// Operator notes waiting for the next agent turn on `task`.
pub fn pending_notes(conn: &Connection, task: Uuid) -> Result<Vec<MessageRecord>, StorageError> {
    Ok(messages_for_task(conn, task)?.into_iter().filter(|m| m.delivery == "pending").collect())
}

/// Marks pending messages as delivered with an assignment (exactly once).
pub fn mark_delivered(
    conn: &Connection,
    ids: &[Uuid],
    assignment: Uuid,
    now_ms: i64,
) -> Result<usize, StorageError> {
    let mut n = 0;
    for id in ids {
        n += conn.execute(
            "UPDATE messages SET delivery = 'delivered', delivered_at = ?1, recipient_assignment_id = ?2 \
             WHERE id = ?3 AND delivery = 'pending'",
            params![now_ms, assignment.to_string(), id.to_string()],
        )?;
    }
    Ok(n)
}

// --- findings ----------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct FindingRecord {
    pub id: Uuid,
    pub project_id: Uuid,
    pub task_id: Uuid,
    pub message_id: Uuid,
    pub assignment_id: Option<Uuid>,
    pub severity: Severity,
    pub title: String,
    pub explanation: Option<String>,
    pub suggestion: Option<String>,
    pub path: Option<String>,
    pub line: Option<u32>,
    /// `open`, `resolved` or `dismissed`.
    pub status: String,
    pub created_at_ms: i64,
    pub resolved_at_ms: Option<i64>,
}

const FINDING_COLUMNS: &str = "id, project_id, task_id, message_id, assignment_id, severity, title, explanation, suggestion, path, line, status, created_at, resolved_at";

pub fn insert_finding(conn: &Connection, f: &FindingRecord) -> Result<(), StorageError> {
    conn.execute(
        &format!(
            "INSERT INTO findings ({FINDING_COLUMNS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)"
        ),
        params![
            f.id.to_string(),
            f.project_id.to_string(),
            f.task_id.to_string(),
            f.message_id.to_string(),
            f.assignment_id.map(|u| u.to_string()),
            f.severity.as_str(),
            f.title,
            f.explanation,
            f.suggestion,
            f.path,
            f.line,
            f.status,
            f.created_at_ms,
            f.resolved_at_ms,
        ],
    )?;
    Ok(())
}

pub fn findings_for_task(conn: &Connection, task: Uuid) -> Result<Vec<FindingRecord>, StorageError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {FINDING_COLUMNS} FROM findings WHERE task_id = ?1 ORDER BY created_at, id"
    ))?;
    let rows = stmt.query_map([task.to_string()], |r| {
        let ids: (String, String, String, String, Option<String>) =
            (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?);
        let severity: String = r.get(5)?;
        let line: Option<i64> = r.get(10)?;
        Ok((
            ids,
            severity,
            r.get::<_, String>(6)?,
            r.get(7)?,
            r.get(8)?,
            r.get(9)?,
            line,
            r.get::<_, String>(11)?,
            r.get(12)?,
            r.get(13)?,
        ))
    })?;
    rows.map(|r| {
        let (ids, severity, title, explanation, suggestion, path, line, status, created, resolved) = r?;
        Ok(FindingRecord {
            id: parse_uuid(&ids.0, "findings.id")?,
            project_id: parse_uuid(&ids.1, "findings.project_id")?,
            task_id: parse_uuid(&ids.2, "findings.task_id")?,
            message_id: parse_uuid(&ids.3, "findings.message_id")?,
            assignment_id: opt_uuid(ids.4, "findings.assignment_id")?,
            severity: parse_enum(&severity, "findings.severity")?,
            title,
            explanation,
            suggestion,
            path,
            line: line.and_then(|l| u32::try_from(l).ok()),
            status,
            created_at_ms: created,
            resolved_at_ms: resolved,
        })
    })
    .collect()
}

/// Marks a task's open findings resolved (when the operator accepts the work).
pub fn resolve_findings(conn: &Connection, task: Uuid, now_ms: i64) -> Result<usize, StorageError> {
    Ok(conn.execute(
        "UPDATE findings SET status = 'resolved', resolved_at = ?1 WHERE task_id = ?2 AND status = 'open'",
        params![now_ms, task.to_string()],
    )?)
}

// --- proposals ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct ProposalRecord {
    pub id: Uuid,
    pub project_id: Uuid,
    pub task_id: Uuid,
    pub message_id: Uuid,
    pub position: u32,
    pub proposal: Value,
    pub status: ProposalStatus,
    pub decided_by: Option<String>,
    pub decision_note: Option<String>,
    pub created_task_id: Option<Uuid>,
    pub created_at_ms: i64,
    pub decided_at_ms: Option<i64>,
}

const PROPOSAL_COLUMNS: &str = "id, project_id, task_id, message_id, position, proposal, status, decided_by, \
     decision_note, created_task_id, created_at, decided_at";

fn proposal_from_row(r: &Row<'_>) -> rusqlite::Result<Result<ProposalRecord, StorageError>> {
    let ids: (String, String, String, String, Option<String>) =
        (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(9)?);
    let position: i64 = r.get(4)?;
    let proposal: String = r.get(5)?;
    let status: String = r.get(6)?;
    let decided_by: Option<String> = r.get(7)?;
    let note: Option<String> = r.get(8)?;
    let created: i64 = r.get(10)?;
    let decided: Option<i64> = r.get(11)?;
    Ok((|| {
        Ok(ProposalRecord {
            id: parse_uuid(&ids.0, "task_proposals.id")?,
            project_id: parse_uuid(&ids.1, "task_proposals.project_id")?,
            task_id: parse_uuid(&ids.2, "task_proposals.task_id")?,
            message_id: parse_uuid(&ids.3, "task_proposals.message_id")?,
            position: u32::try_from(position).unwrap_or(0),
            proposal: json(&proposal, "task_proposals.proposal")?,
            status: parse_enum(&status, "task_proposals.status")?,
            decided_by,
            decision_note: note,
            created_task_id: opt_uuid(ids.4, "task_proposals.created_task_id")?,
            created_at_ms: created,
            decided_at_ms: decided,
        })
    })())
}

pub fn insert_proposal(conn: &Connection, p: &ProposalRecord) -> Result<(), StorageError> {
    conn.execute(
        &format!(
            "INSERT INTO task_proposals ({PROPOSAL_COLUMNS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)"
        ),
        params![
            p.id.to_string(),
            p.project_id.to_string(),
            p.task_id.to_string(),
            p.message_id.to_string(),
            p.position,
            p.proposal.to_string(),
            p.status.as_str(),
            p.decided_by,
            p.decision_note,
            p.created_task_id.map(|u| u.to_string()),
            p.created_at_ms,
            p.decided_at_ms,
        ],
    )?;
    Ok(())
}

pub fn get_proposal(conn: &Connection, id: Uuid) -> Result<ProposalRecord, StorageError> {
    conn.query_row(
        &format!("SELECT {PROPOSAL_COLUMNS} FROM task_proposals WHERE id = ?1"),
        [id.to_string()],
        proposal_from_row,
    )
    .optional()?
    .ok_or_else(|| StorageError::NotFound(format!("proposal {id}")))?
}

pub fn proposals_for_task(conn: &Connection, task: Uuid) -> Result<Vec<ProposalRecord>, StorageError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {PROPOSAL_COLUMNS} FROM task_proposals WHERE task_id = ?1 ORDER BY created_at, position"
    ))?;
    let rows = stmt.query_map([task.to_string()], proposal_from_row)?;
    rows.map(|r| r?).collect()
}

pub fn pending_proposals(
    conn: &Connection,
    project_id: Option<Uuid>,
) -> Result<Vec<ProposalRecord>, StorageError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {PROPOSAL_COLUMNS} FROM task_proposals WHERE status = 'pending' AND (?1 IS NULL OR project_id = ?1) \
         ORDER BY created_at, position"
    ))?;
    let rows = stmt.query_map([project_id.map(|u| u.to_string())], proposal_from_row)?;
    rows.map(|r| r?).collect()
}

/// Settles a pending proposal exactly once.
pub fn decide_proposal(
    conn: &Connection,
    id: Uuid,
    to: ProposalStatus,
    decided_by: &str,
    note: Option<&str>,
    created_task: Option<Uuid>,
    now_ms: i64,
) -> Result<ProposalRecord, StorageError> {
    let n = conn.execute(
        "UPDATE task_proposals SET status = ?1, decided_by = ?2, decision_note = ?3, created_task_id = ?4, \
         decided_at = ?5 WHERE id = ?6 AND status = 'pending'",
        params![to.as_str(), decided_by, note, created_task.map(|u| u.to_string()), now_ms, id.to_string()],
    )?;
    if n == 0 {
        return Err(StorageError::Conflict(format!("proposal {id} is not pending")));
    }
    get_proposal(conn, id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Storage;

    fn setup() -> (tempfile::TempDir, Storage, Uuid) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("t.db")).unwrap();
        let project = Uuid::now_v7();
        storage
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO projects (id, name, root_path, created_at, updated_at) VALUES (?1, 'p', ?2, 1, 1)",
                    params![project.to_string(), format!("/tmp/{project}")],
                )?;
                Ok(())
            })
            .unwrap();
        (dir, storage, project)
    }

    fn task(project: Uuid) -> TaskRecord {
        TaskRecord {
            id: Uuid::now_v7(),
            project_id: project,
            parent_task_id: None,
            title: "t".into(),
            description: None,
            kind: TaskKind::General,
            priority: Priority::Normal,
            acceptance_criteria: vec!["a".into()],
            status: TaskStatus::Draft,
            status_reason: None,
            result: None,
            failure: None,
            origin: "operator".into(),
            origin_proposal_id: None,
            revision: 1,
            created_at_ms: 1,
            updated_at_ms: 1,
            completed_at_ms: None,
        }
    }

    #[test]
    fn transitions_are_validated_and_revisions_guard_edits() {
        let (_d, s, p) = setup();
        let t = task(p);
        s.with_conn(|c| {
            insert_task(c, &t)?;
            let change = || StatusChange { reason: None, result: None, failure: None };
            assert!(
                set_task_status(c, t.id, TaskStatus::Completed, change(), 2).is_err(),
                "draft → completed"
            );
            let (_, after) = set_task_status(c, t.id, TaskStatus::Ready, change(), 2)?;
            assert_eq!((after.status, after.revision), (TaskStatus::Ready, 2));
            let stale = edit_task(c, t.id, 1, &TaskEdit { title: Some("x".into()), ..Default::default() }, 3);
            assert!(matches!(stale, Err(StorageError::Conflict(_))), "a stale revision is refused");
            let ok = edit_task(c, t.id, 2, &TaskEdit { title: Some("x".into()), ..Default::default() }, 3)?;
            assert_eq!((ok.title.as_str(), ok.revision), ("x", 3));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn dependencies_stay_in_one_project_and_never_point_at_themselves() {
        let (_d, s, p) = setup();
        let (a, b) = (task(p), task(p));
        s.with_conn(|c| {
            insert_task(c, &a)?;
            insert_task(c, &b)?;
            add_dependency(c, b.id, a.id, 1)?;
            assert!(add_dependency(c, a.id, a.id, 1).is_err(), "self dependency (CHECK)");
            let other = Uuid::now_v7();
            c.execute(
                "INSERT INTO projects (id, name, root_path, created_at, updated_at) VALUES (?1, 'q', ?2, 1, 1)",
                params![other.to_string(), format!("/tmp/{other}")],
            )?;
            let foreign = task(other);
            insert_task(c, &foreign)?;
            assert!(add_dependency(c, b.id, foreign.id, 1).is_err(), "cross-project (trigger)");
            assert_eq!(dependencies_of(c, b.id)?, vec![(a.id, TaskStatus::Draft)]);
            assert_eq!(dependents_of(c, a.id)?, vec![b.id]);
            Ok(())
        })
        .unwrap();
    }
}
