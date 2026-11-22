//! The task supervisor's conversation (migration 0013, ADR 0041).

use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::{StorageError, parse_uuid};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorRecord {
    pub task_id: Uuid,
    pub project_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorMessageRecord {
    pub id: Uuid,
    pub task_id: Uuid,
    /// `operator`, `supervisor` or `core`.
    pub sender: String,
    pub text: String,
    pub route: Option<String>,
    pub instruction: Option<String>,
    pub outcome: Option<String>,
    pub follow_up_task_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub by: Option<String>,
    pub created_at_ms: i64,
}

fn opt_uuid(s: Option<String>, what: &'static str) -> Result<Option<Uuid>, StorageError> {
    s.map(|s| parse_uuid(&s, what)).transpose()
}

pub fn get(conn: &Connection, task: Uuid) -> Result<Option<SupervisorRecord>, StorageError> {
    let row = conn
        .query_row(
            "SELECT task_id, project_id, workspace_id, session_id, updated_at FROM task_supervisors WHERE task_id = ?1",
            [task.to_string()],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()?;
    row.map(|(t, p, w, s, u)| {
        Ok(SupervisorRecord {
            task_id: parse_uuid(&t, "task_supervisors.task_id")?,
            project_id: parse_uuid(&p, "task_supervisors.project_id")?,
            workspace_id: opt_uuid(w, "task_supervisors.workspace_id")?,
            session_id: opt_uuid(s, "task_supervisors.session_id")?,
            updated_at_ms: u,
        })
    })
    .transpose()
}

pub fn upsert(conn: &Connection, r: &SupervisorRecord) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO task_supervisors (task_id, project_id, workspace_id, session_id, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT (task_id) DO UPDATE SET workspace_id = excluded.workspace_id, \
         session_id = excluded.session_id, updated_at = excluded.updated_at",
        params![
            r.task_id.to_string(),
            r.project_id.to_string(),
            r.workspace_id.map(|u| u.to_string()),
            r.session_id.map(|u| u.to_string()),
            r.updated_at_ms,
        ],
    )?;
    Ok(())
}

pub fn insert_message(conn: &Connection, m: &SupervisorMessageRecord) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO supervisor_messages (id, task_id, sender, text, route, instruction, outcome, follow_up_task_id, \
         session_id, by, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            m.id.to_string(),
            m.task_id.to_string(),
            m.sender,
            m.text,
            m.route,
            m.instruction,
            m.outcome,
            m.follow_up_task_id.map(|u| u.to_string()),
            m.session_id.map(|u| u.to_string()),
            m.by,
            m.created_at_ms,
        ],
    )?;
    Ok(())
}

/// The task's conversation with its supervisor, oldest first.
pub fn messages(conn: &Connection, task: Uuid) -> Result<Vec<SupervisorMessageRecord>, StorageError> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, task_id, sender, text, route, instruction, outcome, follow_up_task_id, session_id, by, created_at \
         FROM supervisor_messages WHERE task_id = ?1 ORDER BY created_at, rowid",
    )?;
    let rows = stmt.query_map([task.to_string()], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, Option<String>>(5)?,
            r.get::<_, Option<String>>(6)?,
            r.get::<_, Option<String>>(7)?,
            r.get::<_, Option<String>>(8)?,
            r.get::<_, Option<String>>(9)?,
            r.get::<_, i64>(10)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, task_id, sender, text, route, instruction, outcome, follow_up, session, by, created) = row?;
        out.push(SupervisorMessageRecord {
            id: parse_uuid(&id, "supervisor_messages.id")?,
            task_id: parse_uuid(&task_id, "supervisor_messages.task_id")?,
            sender,
            text,
            route,
            instruction,
            outcome,
            follow_up_task_id: opt_uuid(follow_up, "supervisor_messages.follow_up_task_id")?,
            session_id: opt_uuid(session, "supervisor_messages.session_id")?,
            by,
            created_at_ms: created,
        });
    }
    Ok(out)
}

/// Whether the supervisor owes an answer: the last message is the operator's.
pub fn thinking(conn: &Connection, task: Uuid) -> Result<bool, StorageError> {
    let last: Option<String> = conn
        .query_row(
            "SELECT sender FROM supervisor_messages WHERE task_id = ?1 ORDER BY created_at DESC, rowid DESC LIMIT 1",
            [task.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    Ok(last.as_deref() == Some("operator"))
}

/// Tasks whose supervisor owes an answer (after a restart: interrupted).
pub fn tasks_thinking(conn: &Connection) -> Result<Vec<Uuid>, StorageError> {
    let mut stmt = conn.prepare_cached(
        "SELECT m.task_id FROM supervisor_messages m WHERE m.rowid = (SELECT m2.rowid FROM supervisor_messages m2 \
         WHERE m2.task_id = m.task_id ORDER BY m2.created_at DESC, m2.rowid DESC LIMIT 1) AND m.sender = 'operator'",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(parse_uuid(&r?, "supervisor_messages.task_id")?);
    }
    Ok(out)
}
