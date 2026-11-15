//! Agent organizations, their shared context and project settings
//! (migration 0010, ADRs 0035–0036).

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::Value;
use uuid::Uuid;

use crate::{StorageError, parse_uuid};

#[derive(Debug, Clone, PartialEq)]
pub struct OrgRecord {
    /// The objective's (root) task.
    pub task_id: Uuid,
    pub project_id: Uuid,
    pub workspace_id: Uuid,
    pub status: String,
    pub reason: Option<String>,
    /// The pure organization state.
    pub state: Value,
    pub base_commit: Option<String>,
    pub last_event_seq: i64,
    pub revision: i64,
    pub started_by: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub ended_at_ms: Option<i64>,
}

const COLUMNS: &str = "task_id, project_id, workspace_id, status, reason, state, base_commit, last_event_seq, \
     revision, started_by, created_at, updated_at, ended_at";

fn json(s: &str, what: &'static str) -> Result<Value, StorageError> {
    serde_json::from_str(s).map_err(|e| StorageError::Corrupt(format!("{what}: {e}")))
}

fn from_row(r: &Row<'_>) -> rusqlite::Result<Result<OrgRecord, StorageError>> {
    let (task, project, workspace): (String, String, String) = (r.get(0)?, r.get(1)?, r.get(2)?);
    let (status, reason, state, base): (String, Option<String>, String, Option<String>) =
        (r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?);
    let (seq, revision, started_by): (i64, i64, String) = (r.get(7)?, r.get(8)?, r.get(9)?);
    let times: (i64, i64, Option<i64>) = (r.get(10)?, r.get(11)?, r.get(12)?);
    Ok((|| {
        Ok(OrgRecord {
            task_id: parse_uuid(&task, "organizations.task_id")?,
            project_id: parse_uuid(&project, "organizations.project_id")?,
            workspace_id: parse_uuid(&workspace, "organizations.workspace_id")?,
            status,
            reason,
            state: json(&state, "organizations.state")?,
            base_commit: base,
            last_event_seq: seq,
            revision,
            started_by,
            created_at_ms: times.0,
            updated_at_ms: times.1,
            ended_at_ms: times.2,
        })
    })())
}

pub fn insert(conn: &Connection, o: &OrgRecord) -> Result<(), StorageError> {
    conn.execute(
        &format!("INSERT INTO organizations ({COLUMNS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)"),
        params![
            o.task_id.to_string(),
            o.project_id.to_string(),
            o.workspace_id.to_string(),
            o.status,
            o.reason,
            o.state.to_string(),
            o.base_commit,
            o.last_event_seq,
            o.revision,
            o.started_by,
            o.created_at_ms,
            o.updated_at_ms,
            o.ended_at_ms,
        ],
    )
    .map_err(|e| match e {
        rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation => {
            StorageError::Conflict("the task already has an organization".into())
        }
        other => other.into(),
    })?;
    Ok(())
}

pub fn find(conn: &Connection, task: Uuid) -> Result<Option<OrgRecord>, StorageError> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM organizations WHERE task_id = ?1"),
        [task.to_string()],
        from_row,
    )
    .optional()?
    .transpose()
}

pub fn get(conn: &Connection, task: Uuid) -> Result<OrgRecord, StorageError> {
    find(conn, task)?.ok_or_else(|| StorageError::NotFound(format!("organization of task {task}")))
}

/// Organizations that are not finished.
pub fn active(conn: &Connection) -> Result<Vec<OrgRecord>, StorageError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM organizations WHERE status IN ('running','waiting','paused') ORDER BY created_at"
    ))?;
    let rows = stmt.query_map([], from_row)?;
    rows.map(|r| r?).collect()
}

/// The organization whose state names this M6 task (root or subtask), if any.
/// Subtasks are found through their parent chain.
pub fn for_task(conn: &Connection, task: Uuid) -> Result<Option<OrgRecord>, StorageError> {
    let mut current = Some(task);
    let mut hops = 0;
    while let Some(t) = current {
        if let Some(o) = find(conn, t)? {
            return Ok(Some(o));
        }
        hops += 1;
        if hops > 64 {
            return Ok(None);
        }
        current = conn
            .query_row("SELECT parent_task_id FROM tasks WHERE id = ?1", [t.to_string()], |r| {
                r.get::<_, Option<String>>(0)
            })
            .optional()?
            .flatten()
            .map(|p| parse_uuid(&p, "tasks.parent_task_id"))
            .transpose()?;
    }
    Ok(None)
}

/// Writes a decision's new state, guarded by `revision`.
#[allow(clippy::too_many_arguments)]
pub fn update(
    conn: &Connection,
    task: Uuid,
    revision: i64,
    status: &str,
    reason: Option<&str>,
    state: &Value,
    ended: bool,
    now: i64,
) -> Result<(), StorageError> {
    let n = conn.execute(
        "UPDATE organizations SET status = ?3, reason = ?4, state = ?5, revision = revision + 1, updated_at = ?6, \
         ended_at = CASE WHEN ?7 THEN COALESCE(ended_at, ?6) ELSE NULL END \
         WHERE task_id = ?1 AND revision = ?2",
        params![task.to_string(), revision, status, reason, state.to_string(), now, ended],
    )?;
    if n == 0 {
        return Err(StorageError::Conflict("the organization changed concurrently".into()));
    }
    Ok(())
}

/// Marks `seq` processed. `false` if it already was (a replay).
pub fn advance_seq(conn: &Connection, task: Uuid, seq: i64) -> Result<bool, StorageError> {
    let n = conn.execute(
        "UPDATE organizations SET last_event_seq = ?2 WHERE task_id = ?1 AND last_event_seq < ?2",
        params![task.to_string(), seq],
    )?;
    Ok(n == 1)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextRecord {
    pub seq: i64,
    pub kind: String,
    pub status: String,
    pub text: String,
    pub task_key: Option<String>,
    pub source: String,
    pub supersedes: Option<i64>,
    pub created_at_ms: i64,
}

/// Appends an entry; returns its sequence number (the context version).
#[allow(clippy::too_many_arguments)]
pub fn append_context(
    conn: &Connection,
    org: Uuid,
    kind: &str,
    status: &str,
    text: &str,
    task_key: Option<&str>,
    source: &str,
    supersedes: Option<i64>,
    now: i64,
) -> Result<i64, StorageError> {
    let seq: i64 = conn.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM org_context WHERE org_task_id = ?1",
        [org.to_string()],
        |r| r.get(0),
    )?;
    // A supersession must name an earlier entry of the same organization.
    let supersedes = match supersedes {
        Some(s) if s >= 1 && s < seq => Some(s),
        _ => None,
    };
    let text: String = text.chars().take(8000).collect();
    conn.execute(
        "INSERT INTO org_context (org_task_id, seq, kind, status, text, task_key, source, supersedes, created_at) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![org.to_string(), seq, kind, status, text, task_key, source, supersedes, now],
    )?;
    Ok(seq)
}

pub fn context(conn: &Connection, org: Uuid) -> Result<Vec<ContextRecord>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT seq, kind, status, text, task_key, source, supersedes, created_at FROM org_context \
         WHERE org_task_id = ?1 ORDER BY seq",
    )?;
    let rows = stmt.query_map([org.to_string()], |r| {
        Ok(ContextRecord {
            seq: r.get(0)?,
            kind: r.get(1)?,
            status: r.get(2)?,
            text: r.get(3)?,
            task_key: r.get(4)?,
            source: r.get(5)?,
            supersedes: r.get(6)?,
            created_at_ms: r.get(7)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The project's organization settings (`None`: defaults).
pub fn settings(conn: &Connection, project: Uuid) -> Result<Option<(Value, i64)>, StorageError> {
    conn.query_row(
        "SELECT settings, revision FROM org_settings WHERE project_id = ?1",
        [project.to_string()],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
    )
    .optional()?
    .map(|(s, rev)| Ok((json(&s, "org_settings.settings")?, rev)))
    .transpose()
}

pub fn put_settings(
    conn: &Connection,
    project: Uuid,
    settings: &Value,
    now: i64,
) -> Result<i64, StorageError> {
    conn.execute(
        "INSERT INTO org_settings (project_id, settings, revision, updated_at) VALUES (?1, ?2, 1, ?3) \
         ON CONFLICT (project_id) DO UPDATE SET settings = excluded.settings, revision = revision + 1, \
         updated_at = excluded.updated_at",
        params![project.to_string(), settings.to_string(), now],
    )?;
    Ok(conn.query_row(
        "SELECT revision FROM org_settings WHERE project_id = ?1",
        [project.to_string()],
        |r| r.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::Storage;

    #[test]
    fn organizations_context_and_settings() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("o.db")).unwrap();
        storage
            .with_conn(|c| {
                let project = Uuid::now_v7();
                let task = Uuid::now_v7();
                let ws = Uuid::now_v7();
                c.execute(
                    "INSERT INTO projects (id, name, root_path, created_at, updated_at) VALUES (?1, 'p', '/p', 0, 0)",
                    [project.to_string()],
                )?;
                c.execute(
                    "INSERT INTO workspaces (id, project_id, kind, name, path, branch, base_ref, base_commit, status, \
                     created_at, updated_at, last_activity_at) VALUES (?1, ?2, 'worktree', 'w', '/w', 'b', 'HEAD', 'abc', 'ready', 0, 0, 0)",
                    [ws.to_string(), project.to_string()],
                )?;
                c.execute(
                    "INSERT INTO tasks (id, project_id, title, kind, priority, status, origin, created_at, updated_at) \
                     VALUES (?1, ?2, 't', 'general', 'normal', 'ready', 'operator', 0, 0)",
                    [task.to_string(), project.to_string()],
                )?;
                let sub = Uuid::now_v7();
                c.execute(
                    "INSERT INTO tasks (id, project_id, parent_task_id, title, kind, priority, status, origin, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, 's', 'general', 'normal', 'draft', 'proposal', 0, 0)",
                    [sub.to_string(), project.to_string(), task.to_string()],
                )?;
                let rec = OrgRecord {
                    task_id: task,
                    project_id: project,
                    workspace_id: ws,
                    status: "running".into(),
                    reason: None,
                    state: json!({"x": 1}),
                    base_commit: Some("abc".into()),
                    last_event_seq: 5,
                    revision: 1,
                    started_by: "user".into(),
                    created_at_ms: 1,
                    updated_at_ms: 1,
                    ended_at_ms: None,
                };
                insert(c, &rec)?;
                assert!(matches!(insert(c, &rec), Err(StorageError::Conflict(_))));
                assert_eq!(for_task(c, sub)?.map(|o| o.task_id), Some(task), "found through the parent chain");
                assert!(!advance_seq(c, task, 5)? && advance_seq(c, task, 6)?);
                update(c, task, 1, "done", Some("ok"), &json!({"x": 2}), true, 9)?;
                assert!(matches!(update(c, task, 1, "done", None, &json!({}), true, 9), Err(StorageError::Conflict(_))));
                let got = get(c, task)?;
                assert_eq!((got.status.as_str(), got.revision, got.ended_at_ms), ("done", 2, Some(9)));
                assert!(active(c)?.is_empty());
                assert_eq!(append_context(c, task, "finding", "claim", "a", None, "agent:x", None, 1)?, 1);
                assert_eq!(append_context(c, task, "finding", "claim", "b", Some("k"), "agent:x", Some(1), 1)?, 2);
                assert_eq!(append_context(c, task, "finding", "claim", "c", None, "core", Some(7), 1)?, 3);
                let ctx = context(c, task)?;
                assert_eq!((ctx[1].supersedes, ctx[2].supersedes), (Some(1), None), "only earlier entries");
                assert!(c.execute("UPDATE org_context SET text = 'x'", []).is_err(), "append-only");
                assert_eq!(settings(c, project)?, None);
                assert_eq!(put_settings(c, project, &json!({"a": 1}), 1)?, 1);
                assert_eq!(put_settings(c, project, &json!({"a": 2}), 2)?, 2);
                assert_eq!(settings(c, project)?, Some((json!({"a": 2}), 2)));
                Ok(())
            })
            .unwrap();
    }
}
