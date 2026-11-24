//! Workflow runs and project role bindings (migration 0008, ADR 0033).

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::Value;
use uuid::Uuid;

use crate::{StorageError, parse_uuid};

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowRecord {
    pub task_id: Uuid,
    pub project_id: Uuid,
    pub definition: String,
    pub version: u32,
    /// Snapshot of the effective bindings and limits.
    pub config: Value,
    pub workspace_id: Option<Uuid>,
    pub status: String,
    pub stage: Option<String>,
    pub step_key: Option<String>,
    pub reason: Option<String>,
    /// The pure workflow state.
    pub state: Value,
    pub last_event_seq: i64,
    pub revision: i64,
    pub started_by: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    /// The M7 integration the workflow delivers through (M8.2).
    pub integration_id: Option<Uuid>,
}

const COLUMNS: &str = "task_id, project_id, definition, version, config, workspace_id, status, stage, step_key, \
     reason, state, last_event_seq, revision, started_by, created_at, updated_at, ended_at, integration_id";

fn json(s: &str, what: &'static str) -> Result<Value, StorageError> {
    serde_json::from_str(s).map_err(|e| StorageError::Corrupt(format!("{what}: {e}")))
}

fn from_row(r: &Row<'_>) -> rusqlite::Result<Result<WorkflowRecord, StorageError>> {
    let (task, project, definition, version): (String, String, String, i64) =
        (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?);
    let (config, workspace): (String, Option<String>) = (r.get(4)?, r.get(5)?);
    let (status, stage, step_key, reason, state): (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
    ) = (r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?);
    let (seq, revision, started_by): (i64, i64, String) = (r.get(11)?, r.get(12)?, r.get(13)?);
    let times: (i64, i64, Option<i64>) = (r.get(14)?, r.get(15)?, r.get(16)?);
    let integration: Option<String> = r.get(17)?;
    Ok((|| {
        Ok(WorkflowRecord {
            task_id: parse_uuid(&task, "task_workflows.task_id")?,
            project_id: parse_uuid(&project, "task_workflows.project_id")?,
            definition,
            version: u32::try_from(version).unwrap_or(0),
            config: json(&config, "task_workflows.config")?,
            workspace_id: workspace.map(|w| parse_uuid(&w, "task_workflows.workspace_id")).transpose()?,
            status,
            stage,
            step_key,
            reason,
            state: json(&state, "task_workflows.state")?,
            last_event_seq: seq,
            revision,
            started_by,
            created_at_ms: times.0,
            updated_at_ms: times.1,
            ended_at_ms: times.2,
            integration_id: integration
                .map(|i| parse_uuid(&i, "task_workflows.integration_id"))
                .transpose()?,
        })
    })())
}

pub fn insert_workflow(conn: &Connection, w: &WorkflowRecord) -> Result<(), StorageError> {
    conn.execute(
        &format!("INSERT INTO task_workflows ({COLUMNS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)"),
        params![
            w.task_id.to_string(),
            w.project_id.to_string(),
            w.definition,
            w.version,
            w.config.to_string(),
            w.workspace_id.map(|u| u.to_string()),
            w.status,
            w.stage,
            w.step_key,
            w.reason,
            w.state.to_string(),
            w.last_event_seq,
            w.revision,
            w.started_by,
            w.created_at_ms,
            w.updated_at_ms,
            w.ended_at_ms,
            w.integration_id.map(|i| i.to_string()),
        ],
    )
    .map_err(|e| match e {
        rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation => {
            StorageError::Conflict("the task already has a workflow".into())
        }
        other => other.into(),
    })?;
    Ok(())
}

pub fn find_workflow(conn: &Connection, task: Uuid) -> Result<Option<WorkflowRecord>, StorageError> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM task_workflows WHERE task_id = ?1"),
        [task.to_string()],
        from_row,
    )
    .optional()?
    .transpose()
}

pub fn get_workflow(conn: &Connection, task: Uuid) -> Result<WorkflowRecord, StorageError> {
    find_workflow(conn, task)?.ok_or_else(|| StorageError::NotFound(format!("workflow of task {task}")))
}

/// Every workflow that worked in `workspace`, oldest first (a task started
/// again continues in its predecessor's workspace).
pub fn workflows_in_workspace(
    conn: &Connection,
    workspace: Uuid,
) -> Result<Vec<WorkflowRecord>, StorageError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM task_workflows WHERE workspace_id = ?1 ORDER BY created_at, task_id"
    ))?;
    let rows = stmt.query_map([workspace.to_string()], from_row)?;
    rows.map(|r| r?).collect()
}

/// Workflows that are not finished.
pub fn active_workflows(conn: &Connection) -> Result<Vec<WorkflowRecord>, StorageError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM task_workflows WHERE status IN ('pending','running','waiting','paused') \
         ORDER BY created_at"
    ))?;
    let rows = stmt.query_map([], from_row)?;
    rows.map(|r| r?).collect()
}

/// The new state of a workflow after a decision.
#[derive(Debug, Clone)]
pub struct WorkflowUpdate<'a> {
    pub status: &'a str,
    pub stage: Option<&'a str>,
    pub step_key: Option<&'a str>,
    pub reason: Option<&'a str>,
    pub state: &'a Value,
    pub workspace_id: Option<Uuid>,
    pub ended: bool,
}

/// Writes `u` if the workflow is still at `revision`; returns the new record.
pub fn update_workflow(
    conn: &Connection,
    task: Uuid,
    revision: i64,
    u: &WorkflowUpdate<'_>,
    now: i64,
) -> Result<WorkflowRecord, StorageError> {
    let changed = conn.execute(
        "UPDATE task_workflows SET status = ?3, stage = ?4, step_key = ?5, reason = ?6, state = ?7, \
         workspace_id = COALESCE(?8, workspace_id), revision = revision + 1, updated_at = ?9, \
         ended_at = CASE WHEN ?10 THEN COALESCE(ended_at, ?9) ELSE NULL END \
         WHERE task_id = ?1 AND revision = ?2",
        params![
            task.to_string(),
            revision,
            u.status,
            u.stage,
            u.step_key,
            u.reason,
            u.state.to_string(),
            u.workspace_id.map(|w| w.to_string()),
            now,
            u.ended,
        ],
    )?;
    if changed == 0 {
        return Err(StorageError::Conflict(format!("the workflow of task {task} changed meanwhile")));
    }
    get_workflow(conn, task)
}

/// Records the integration a workflow delivers through (M8.2). Only sets it
/// when unset or when the previous one is closed (the caller checks).
pub fn set_integration(conn: &Connection, task: Uuid, integration: Uuid) -> Result<(), StorageError> {
    conn.execute(
        "UPDATE task_workflows SET integration_id = ?2 WHERE task_id = ?1",
        params![task.to_string(), integration.to_string()],
    )?;
    Ok(())
}

/// Records that the workflow processed journal event `seq`. `false` if it
/// already had (a replay), so the caller applies nothing.
pub fn advance_workflow_seq(conn: &Connection, task: Uuid, seq: i64) -> Result<bool, StorageError> {
    Ok(conn.execute(
        "UPDATE task_workflows SET last_event_seq = ?2 WHERE task_id = ?1 AND last_event_seq < ?2",
        params![task.to_string(), seq],
    )? > 0)
}

// --- project role bindings -------------------------------------------------------

/// The project's saved bindings and their revision, if any were saved.
pub fn settings(conn: &Connection, project: Uuid) -> Result<Option<(Value, i64)>, StorageError> {
    conn.query_row(
        "SELECT bindings, revision FROM workflow_settings WHERE project_id = ?1",
        [project.to_string()],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
    )
    .optional()?
    .map(|(b, rev)| Ok((json(&b, "workflow_settings.bindings")?, rev)))
    .transpose()
}

pub fn set_settings(
    conn: &Connection,
    project: Uuid,
    bindings: &Value,
    now: i64,
) -> Result<i64, StorageError> {
    conn.execute(
        "INSERT INTO workflow_settings (project_id, bindings, revision, updated_at) VALUES (?1, ?2, 1, ?3) \
         ON CONFLICT (project_id) DO UPDATE SET bindings = excluded.bindings, revision = revision + 1, \
         updated_at = excluded.updated_at",
        params![project.to_string(), bindings.to_string(), now],
    )?;
    Ok(conn.query_row(
        "SELECT revision FROM workflow_settings WHERE project_id = ?1",
        [project.to_string()],
        |r| r.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use serde_json::json;

    use super::*;
    use crate::Storage;

    #[test]
    fn workflow_rows_are_guarded_by_revision_and_seq() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("t.db")).unwrap();
        storage
            .with_conn(|c| {
                let now = 1;
                let project = Uuid::now_v7();
                let task = Uuid::now_v7();
                c.execute(
                    "INSERT INTO projects (id, name, root_path, created_at, updated_at) VALUES (?1, 'p', '/p', 1, 1)",
                    [project.to_string()],
                )?;
                c.execute(
                    "INSERT INTO tasks (id, project_id, title, kind, priority, status, origin, revision, created_at, \
                     updated_at) VALUES (?1, ?2, 't', 'general', 'normal', 'ready', 'operator', 1, 1, 1)",
                    params![task.to_string(), project.to_string()],
                )?;
                let w = WorkflowRecord {
                    task_id: task,
                    project_id: project,
                    definition: "engineering".into(),
                    version: 1,
                    config: json!({}),
                    workspace_id: None,
                    status: "pending".into(),
                    stage: None,
                    step_key: None,
                    reason: None,
                    state: json!({}),
                    last_event_seq: 0,
                    revision: 1,
                    started_by: "test".into(),
                    created_at_ms: now,
                    updated_at_ms: now,
                    ended_at_ms: None,
                    integration_id: None,
                };
                insert_workflow(c, &w)?;
                assert!(matches!(insert_workflow(c, &w), Err(StorageError::Conflict(_))));
                let state = json!({"status": "running"});
                let u = WorkflowUpdate {
                    status: "running",
                    stage: Some("investigation"),
                    step_key: Some("investigate#1"),
                    reason: None,
                    state: &state,
                    workspace_id: None,
                    ended: false,
                };
                let after = update_workflow(c, task, 1, &u, 2)?;
                assert_eq!((after.revision, after.status.as_str()), (2, "running"));
                assert!(matches!(update_workflow(c, task, 1, &u, 3), Err(StorageError::Conflict(_))));
                assert!(advance_workflow_seq(c, task, 5)?);
                assert!(!advance_workflow_seq(c, task, 5)?, "a replayed event is applied once");
                assert_eq!(active_workflows(c)?.len(), 1);
                assert!(settings(c, project)?.is_none());
                assert_eq!(set_settings(c, project, &json!({"a": 1}), 1)?, 1);
                assert_eq!(set_settings(c, project, &json!({"a": 2}), 2)?, 2);
                assert_eq!(settings(c, project)?.unwrap(), (json!({"a": 2}), 2));
                Ok(())
            })
            .unwrap();
    }
}

/// One test run of the workflow's test gate (ADR 0039).
#[derive(Debug, Clone, PartialEq)]
pub struct TestRunRecord {
    pub id: Uuid,
    pub task_id: Uuid,
    pub step_key: String,
    /// `passed`, `failed` or `needs_authorization`.
    pub status: String,
    pub summary: String,
    pub results: Value,
    pub ui_change: bool,
    pub authorized_by: Option<String>,
    pub started_at_ms: i64,
    pub ended_at_ms: i64,
}

pub fn insert_test_run(conn: &Connection, r: &TestRunRecord) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO workflow_test_runs (id, task_id, step_key, status, summary, results, ui_change, authorized_by, \
         started_at, ended_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            r.id.to_string(),
            r.task_id.to_string(),
            r.step_key,
            r.status,
            r.summary,
            r.results.to_string(),
            i64::from(r.ui_change),
            r.authorized_by,
            r.started_at_ms,
            r.ended_at_ms,
        ],
    )?;
    Ok(())
}

type TestRunRow = (String, String, String, String, String, String, i64, Option<String>, i64, i64);

fn test_run_of(row: &Row<'_>) -> rusqlite::Result<TestRunRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
    ))
}

/// The task's test runs, oldest first.
pub fn test_runs_for_task(conn: &Connection, task: Uuid) -> Result<Vec<TestRunRecord>, StorageError> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, task_id, step_key, status, summary, results, ui_change, authorized_by, started_at, ended_at \
         FROM workflow_test_runs WHERE task_id = ?1 ORDER BY ended_at, rowid",
    )?;
    let rows = stmt.query_map([task.to_string()], test_run_of)?;
    let mut out = Vec::new();
    for row in rows {
        let (id, task_id, step_key, status, summary, results, ui, authorized_by, started, ended) = row?;
        out.push(TestRunRecord {
            id: parse_uuid(&id, "workflow_test_runs.id")?,
            task_id: parse_uuid(&task_id, "workflow_test_runs.task_id")?,
            step_key,
            status,
            summary,
            results: serde_json::from_str(&results)
                .map_err(|e| StorageError::Corrupt(format!("test run results: {e}")))?,
            ui_change: ui != 0,
            authorized_by,
            started_at_ms: started,
            ended_at_ms: ended,
        });
    }
    Ok(out)
}
