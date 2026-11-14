//! Integrations, integration settings, validation commands and validation
//! runs (milestone 7). Status changes are validated against
//! `switchyard_integrations::IntegrationStatus::transition` and guarded by SQL.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::Value;
use switchyard_integrations::{IntegrationStatus, RunStatus, StepStatus};
use uuid::Uuid;

use crate::{StorageError, parse_enum, parse_uuid};

fn opt_uuid(s: Option<String>, what: &'static str) -> Result<Option<Uuid>, StorageError> {
    s.map(|v| parse_uuid(&v, what)).transpose()
}

fn json(s: &str, what: &'static str) -> Result<Value, StorageError> {
    serde_json::from_str(s).map_err(|e| StorageError::Corrupt(format!("{what}: {e}")))
}

fn constraint(err: rusqlite::Error, message: &str) -> StorageError {
    match err {
        rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation => {
            StorageError::Conflict(message.to_owned())
        }
        other => other.into(),
    }
}

// --- integrations --------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct IntegrationRecord {
    pub id: Uuid,
    pub project_id: Uuid,
    pub task_id: Option<Uuid>,
    pub workspace_id: Uuid,
    pub source_branch: String,
    pub source_commit: String,
    pub target_branch: String,
    pub target_commit: String,
    pub base_commit: Option<String>,
    pub status: IntegrationStatus,
    pub status_reason: Option<String>,
    pub preflight: Value,
    pub prepared_at_ms: i64,
    pub approved_by: Option<String>,
    pub approved_at_ms: Option<i64>,
    pub merge_commit: Option<String>,
    pub method: Option<String>,
    pub failure: Option<String>,
    pub revision: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub completed_at_ms: Option<i64>,
}

const COLUMNS: &str = "id, project_id, task_id, workspace_id, source_branch, source_commit, target_branch, \
     target_commit, base_commit, status, status_reason, preflight, prepared_at, approved_by, approved_at, merge_commit, \
     method, failure, revision, created_at, updated_at, completed_at";

fn from_row(r: &Row<'_>) -> rusqlite::Result<Result<IntegrationRecord, StorageError>> {
    let id: String = r.get(0)?;
    let project: String = r.get(1)?;
    let task: Option<String> = r.get(2)?;
    let workspace: String = r.get(3)?;
    let status: String = r.get(9)?;
    let preflight: String = r.get(11)?;
    let rec = (|| -> Result<IntegrationRecord, StorageError> {
        Ok(IntegrationRecord {
            id: parse_uuid(&id, "integrations.id")?,
            project_id: parse_uuid(&project, "integrations.project_id")?,
            task_id: opt_uuid(task, "integrations.task_id")?,
            workspace_id: parse_uuid(&workspace, "integrations.workspace_id")?,
            source_branch: r.get(4)?,
            source_commit: r.get(5)?,
            target_branch: r.get(6)?,
            target_commit: r.get(7)?,
            base_commit: r.get(8)?,
            status: parse_enum(&status, "integrations.status")?,
            status_reason: r.get(10)?,
            preflight: json(&preflight, "integrations.preflight")?,
            prepared_at_ms: r.get(12)?,
            approved_by: r.get(13)?,
            approved_at_ms: r.get(14)?,
            merge_commit: r.get(15)?,
            method: r.get(16)?,
            failure: r.get(17)?,
            revision: r.get(18)?,
            created_at_ms: r.get(19)?,
            updated_at_ms: r.get(20)?,
            completed_at_ms: r.get(21)?,
        })
    })();
    Ok(rec)
}

pub fn insert_integration(conn: &Connection, i: &IntegrationRecord) -> Result<(), StorageError> {
    conn.execute(
        &format!("INSERT INTO integrations ({COLUMNS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)"),
        params![
            i.id.to_string(),
            i.project_id.to_string(),
            i.task_id.map(|u| u.to_string()),
            i.workspace_id.to_string(),
            i.source_branch,
            i.source_commit,
            i.target_branch,
            i.target_commit,
            i.base_commit,
            i.status.as_str(),
            i.status_reason,
            i.preflight.to_string(),
            i.prepared_at_ms,
            i.approved_by,
            i.approved_at_ms,
            i.merge_commit,
            i.method,
            i.failure,
            i.revision,
            i.created_at_ms,
            i.updated_at_ms,
            i.completed_at_ms,
        ],
    )
    .map_err(|e| constraint(e, "the workspace already has an unfinished integration"))?;
    Ok(())
}

pub fn find_integration(conn: &Connection, id: Uuid) -> Result<Option<IntegrationRecord>, StorageError> {
    conn.query_row(&format!("SELECT {COLUMNS} FROM integrations WHERE id = ?1"), [id.to_string()], from_row)
        .optional()?
        .transpose()
}

pub fn get_integration(conn: &Connection, id: Uuid) -> Result<IntegrationRecord, StorageError> {
    find_integration(conn, id)?.ok_or_else(|| StorageError::NotFound(format!("integration {id}")))
}

pub fn list_integrations(
    conn: &Connection,
    project_id: Option<Uuid>,
    task_id: Option<Uuid>,
    include_closed: bool,
    limit: u32,
) -> Result<Vec<IntegrationRecord>, StorageError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM integrations WHERE (?1 IS NULL OR project_id = ?1) AND (?2 IS NULL OR task_id = ?2) \
         AND (?3 OR status NOT IN ('integrated', 'cancelled')) ORDER BY created_at DESC LIMIT ?4"
    ))?;
    let rows = stmt.query_map(
        params![project_id.map(|u| u.to_string()), task_id.map(|u| u.to_string()), include_closed, limit],
        from_row,
    )?;
    rows.map(|r| r?).collect()
}

/// Integrations Core was in the middle of (crash recovery).
pub fn busy_integrations(conn: &Connection) -> Result<Vec<IntegrationRecord>, StorageError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM integrations WHERE status IN ('validating', 'integrating')"
    ))?;
    let rows = stmt.query_map([], from_row)?;
    rows.map(|r| r?).collect()
}

/// Fields an update may change; `None` keeps the value.
#[derive(Debug, Clone, Default)]
pub struct IntegrationUpdate {
    pub status: Option<IntegrationStatus>,
    pub status_reason: Option<Option<String>>,
    pub source_commit: Option<String>,
    pub target_commit: Option<String>,
    pub base_commit: Option<Option<String>>,
    pub preflight: Option<Value>,
    pub prepared: bool,
    pub approved_by: Option<Option<String>>,
    pub approved_at_ms: Option<Option<i64>>,
    pub merge_commit: Option<Option<String>>,
    pub method: Option<Option<String>>,
    pub failure: Option<Option<String>>,
    pub completed: bool,
}

/// Applies `u` if the integration is still in `expected` status. A status
/// change must be legal. Returns the record before and after.
pub fn update_integration(
    conn: &Connection,
    id: Uuid,
    expected: IntegrationStatus,
    u: &IntegrationUpdate,
    now: i64,
) -> Result<(IntegrationRecord, IntegrationRecord), StorageError> {
    let before = get_integration(conn, id)?;
    if before.status != expected {
        return Err(StorageError::Conflict(format!("integration {id} is {}, not {expected}", before.status)));
    }
    let to = u.status.unwrap_or(before.status);
    if to != before.status {
        before.status.transition(to).map_err(|e| StorageError::Conflict(format!("integration {id}: {e}")))?;
    }
    let mut next = before.clone();
    next.status = to;
    if let Some(v) = &u.status_reason {
        next.status_reason.clone_from(v);
    }
    if let Some(v) = &u.source_commit {
        next.source_commit.clone_from(v);
    }
    if let Some(v) = &u.target_commit {
        next.target_commit.clone_from(v);
    }
    if let Some(v) = &u.base_commit {
        next.base_commit.clone_from(v);
    }
    if let Some(v) = &u.preflight {
        next.preflight.clone_from(v);
    }
    if u.prepared {
        next.prepared_at_ms = now;
    }
    if let Some(v) = &u.approved_by {
        next.approved_by.clone_from(v);
    }
    if let Some(v) = u.approved_at_ms {
        next.approved_at_ms = v;
    }
    if let Some(v) = &u.merge_commit {
        next.merge_commit.clone_from(v);
    }
    if let Some(v) = &u.method {
        next.method.clone_from(v);
    }
    if let Some(v) = &u.failure {
        next.failure.clone_from(v);
    }
    if u.completed {
        next.completed_at_ms = Some(now);
    }
    let n = conn.execute(
        "UPDATE integrations SET status = ?1, status_reason = ?2, source_commit = ?3, target_commit = ?4, \
         base_commit = ?5, preflight = ?6, prepared_at = ?7, approved_by = ?8, approved_at = ?9, merge_commit = ?10, \
         method = ?11, failure = ?12, completed_at = ?13, revision = revision + 1, updated_at = ?14 \
         WHERE id = ?15 AND status = ?16 AND revision = ?17",
        params![
            next.status.as_str(),
            next.status_reason,
            next.source_commit,
            next.target_commit,
            next.base_commit,
            next.preflight.to_string(),
            next.prepared_at_ms,
            next.approved_by,
            next.approved_at_ms,
            next.merge_commit,
            next.method,
            next.failure,
            next.completed_at_ms,
            now,
            id.to_string(),
            before.status.as_str(),
            before.revision,
        ],
    )?;
    if n == 0 {
        return Err(StorageError::Conflict(format!("integration {id} changed concurrently")));
    }
    Ok((before, get_integration(conn, id)?))
}

// --- settings ------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntegrationSettings {
    pub require_review: bool,
    pub revision: i64,
}

/// Defaults (review required) when nothing is stored.
pub fn settings(conn: &Connection, project_id: Uuid) -> Result<IntegrationSettings, StorageError> {
    Ok(conn
        .query_row(
            "SELECT require_review, revision FROM integration_settings WHERE project_id = ?1",
            [project_id.to_string()],
            |r| Ok(IntegrationSettings { require_review: r.get::<_, i64>(0)? != 0, revision: r.get(1)? }),
        )
        .optional()?
        .unwrap_or(IntegrationSettings { require_review: true, revision: 0 }))
}

pub fn set_settings(
    conn: &Connection,
    project_id: Uuid,
    require_review: bool,
    now: i64,
) -> Result<IntegrationSettings, StorageError> {
    conn.execute(
        "INSERT INTO integration_settings (project_id, require_review, revision, updated_at) VALUES (?1, ?2, 1, ?3) \
         ON CONFLICT (project_id) DO UPDATE SET require_review = excluded.require_review, \
         revision = revision + 1, updated_at = excluded.updated_at",
        params![project_id.to_string(), require_review, now],
    )?;
    settings(conn, project_id)
}

// --- validation commands -------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRecord {
    pub id: Uuid,
    pub project_id: Uuid,
    pub position: i64,
    pub name: String,
    pub argv: Vec<String>,
    pub timeout_secs: u32,
    pub enabled: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

const COMMAND_COLUMNS: &str =
    "id, project_id, position, name, argv, timeout_secs, enabled, created_at, updated_at";

fn command_from_row(r: &Row<'_>) -> rusqlite::Result<Result<CommandRecord, StorageError>> {
    let id: String = r.get(0)?;
    let project: String = r.get(1)?;
    let argv: String = r.get(4)?;
    let rec = (|| -> Result<CommandRecord, StorageError> {
        Ok(CommandRecord {
            id: parse_uuid(&id, "validation_commands.id")?,
            project_id: parse_uuid(&project, "validation_commands.project_id")?,
            position: r.get(2)?,
            name: r.get(3)?,
            argv: serde_json::from_str(&argv)
                .map_err(|e| StorageError::Corrupt(format!("validation_commands.argv: {e}")))?,
            timeout_secs: r.get(5)?,
            enabled: r.get::<_, i64>(6)? != 0,
            created_at_ms: r.get(7)?,
            updated_at_ms: r.get(8)?,
        })
    })();
    Ok(rec)
}

pub fn commands(conn: &Connection, project_id: Uuid) -> Result<Vec<CommandRecord>, StorageError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COMMAND_COLUMNS} FROM validation_commands WHERE project_id = ?1 ORDER BY position, created_at"
    ))?;
    let rows = stmt.query_map([project_id.to_string()], command_from_row)?;
    rows.map(|r| r?).collect()
}

pub fn get_command(conn: &Connection, id: Uuid) -> Result<CommandRecord, StorageError> {
    conn.query_row(
        &format!("SELECT {COMMAND_COLUMNS} FROM validation_commands WHERE id = ?1"),
        [id.to_string()],
        command_from_row,
    )
    .optional()?
    .transpose()?
    .ok_or_else(|| StorageError::NotFound(format!("validation command {id}")))
}

pub fn upsert_command(conn: &Connection, c: &CommandRecord) -> Result<(), StorageError> {
    let argv = serde_json::to_string(&c.argv).map_err(|e| StorageError::Corrupt(e.to_string()))?;
    conn.execute(
        &format!(
            "INSERT INTO validation_commands ({COMMAND_COLUMNS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9) \
             ON CONFLICT (id) DO UPDATE SET position = excluded.position, name = excluded.name, argv = excluded.argv, \
             timeout_secs = excluded.timeout_secs, enabled = excluded.enabled, updated_at = excluded.updated_at"
        ),
        params![
            c.id.to_string(),
            c.project_id.to_string(),
            c.position,
            c.name,
            argv,
            c.timeout_secs,
            c.enabled,
            c.created_at_ms,
            c.updated_at_ms
        ],
    )?;
    Ok(())
}

pub fn delete_command(conn: &Connection, id: Uuid) -> Result<bool, StorageError> {
    Ok(conn.execute("DELETE FROM validation_commands WHERE id = ?1", [id.to_string()])? > 0)
}

// --- validation runs -----------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRecord {
    pub id: Uuid,
    pub integration_id: Uuid,
    pub project_id: Uuid,
    /// The command configuration and policy decisions the run was valid for.
    pub config: String,
    pub source_commit: String,
    pub status: RunStatus,
    pub requested_by: Option<String>,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
}

const RUN_COLUMNS: &str =
    "id, integration_id, project_id, source_commit, status, requested_by, started_at, ended_at, config";

fn run_from_row(r: &Row<'_>) -> rusqlite::Result<Result<RunRecord, StorageError>> {
    let id: String = r.get(0)?;
    let integration: String = r.get(1)?;
    let project: String = r.get(2)?;
    let status: String = r.get(4)?;
    let rec = (|| -> Result<RunRecord, StorageError> {
        Ok(RunRecord {
            id: parse_uuid(&id, "validation_runs.id")?,
            integration_id: parse_uuid(&integration, "validation_runs.integration_id")?,
            project_id: parse_uuid(&project, "validation_runs.project_id")?,
            source_commit: r.get(3)?,
            status: parse_enum(&status, "validation_runs.status")?,
            requested_by: r.get(5)?,
            started_at_ms: r.get(6)?,
            ended_at_ms: r.get(7)?,
            config: r.get(8)?,
        })
    })();
    Ok(rec)
}

pub fn insert_run(conn: &Connection, run: &RunRecord) -> Result<(), StorageError> {
    conn.execute(
        &format!("INSERT INTO validation_runs ({RUN_COLUMNS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)"),
        params![
            run.id.to_string(),
            run.integration_id.to_string(),
            run.project_id.to_string(),
            run.source_commit,
            run.status.as_str(),
            run.requested_by,
            run.started_at_ms,
            run.ended_at_ms,
            run.config
        ],
    )?;
    Ok(())
}

pub fn get_run(conn: &Connection, id: Uuid) -> Result<RunRecord, StorageError> {
    conn.query_row(
        &format!("SELECT {RUN_COLUMNS} FROM validation_runs WHERE id = ?1"),
        [id.to_string()],
        run_from_row,
    )
    .optional()?
    .transpose()?
    .ok_or_else(|| StorageError::NotFound(format!("validation run {id}")))
}

/// Newest first.
pub fn runs_for_integration(conn: &Connection, integration: Uuid) -> Result<Vec<RunRecord>, StorageError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {RUN_COLUMNS} FROM validation_runs WHERE integration_id = ?1 ORDER BY started_at DESC, id DESC"
    ))?;
    let rows = stmt.query_map([integration.to_string()], run_from_row)?;
    rows.map(|r| r?).collect()
}

pub fn running_runs(conn: &Connection) -> Result<Vec<RunRecord>, StorageError> {
    let mut stmt =
        conn.prepare(&format!("SELECT {RUN_COLUMNS} FROM validation_runs WHERE status = 'running'"))?;
    let rows = stmt.query_map([], run_from_row)?;
    rows.map(|r| r?).collect()
}

/// Ends a running run exactly once.
pub fn finish_run(conn: &Connection, id: Uuid, status: RunStatus, now: i64) -> Result<bool, StorageError> {
    Ok(conn.execute(
        "UPDATE validation_runs SET status = ?1, ended_at = ?2 WHERE id = ?3 AND status = 'running'",
        params![status.as_str(), now, id.to_string()],
    )? > 0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepRecord {
    pub id: Uuid,
    pub run_id: Uuid,
    pub position: i64,
    pub command_id: Option<Uuid>,
    pub name: String,
    pub argv: Vec<String>,
    pub command_line: String,
    pub cwd: String,
    pub timeout_secs: u32,
    pub policy_decision: String,
    pub policy_reason: String,
    pub policy_explanation: String,
    pub policy_rule_id: Option<Uuid>,
    pub policy_revision: i64,
    pub authorized_by: Option<String>,
    pub status: StepStatus,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub detail: Option<String>,
    pub started_at_ms: Option<i64>,
    pub ended_at_ms: Option<i64>,
}

const STEP_COLUMNS: &str = "id, run_id, position, command_id, name, argv, command_line, cwd, timeout_secs, \
     policy_decision, policy_reason, policy_explanation, policy_rule_id, policy_revision, authorized_by, status, \
     exit_code, signal, stdout, stderr, stdout_truncated, stderr_truncated, detail, started_at, ended_at";

fn step_from_row(r: &Row<'_>) -> rusqlite::Result<Result<StepRecord, StorageError>> {
    let id: String = r.get(0)?;
    let run: String = r.get(1)?;
    let command: Option<String> = r.get(3)?;
    let argv: String = r.get(5)?;
    let rule: Option<String> = r.get(12)?;
    let status: String = r.get(15)?;
    let rec = (|| -> Result<StepRecord, StorageError> {
        Ok(StepRecord {
            id: parse_uuid(&id, "validation_steps.id")?,
            run_id: parse_uuid(&run, "validation_steps.run_id")?,
            position: r.get(2)?,
            command_id: opt_uuid(command, "validation_steps.command_id")?,
            name: r.get(4)?,
            argv: serde_json::from_str(&argv)
                .map_err(|e| StorageError::Corrupt(format!("validation_steps.argv: {e}")))?,
            command_line: r.get(6)?,
            cwd: r.get(7)?,
            timeout_secs: r.get(8)?,
            policy_decision: r.get(9)?,
            policy_reason: r.get(10)?,
            policy_explanation: r.get(11)?,
            policy_rule_id: opt_uuid(rule, "validation_steps.policy_rule_id")?,
            policy_revision: r.get(13)?,
            authorized_by: r.get(14)?,
            status: parse_enum(&status, "validation_steps.status")?,
            exit_code: r.get(16)?,
            signal: r.get(17)?,
            stdout: r.get(18)?,
            stderr: r.get(19)?,
            stdout_truncated: r.get::<_, i64>(20)? != 0,
            stderr_truncated: r.get::<_, i64>(21)? != 0,
            detail: r.get(22)?,
            started_at_ms: r.get(23)?,
            ended_at_ms: r.get(24)?,
        })
    })();
    Ok(rec)
}

pub fn insert_step(conn: &Connection, s: &StepRecord) -> Result<(), StorageError> {
    let argv = serde_json::to_string(&s.argv).map_err(|e| StorageError::Corrupt(e.to_string()))?;
    conn.execute(
        &format!(
            "INSERT INTO validation_steps ({STEP_COLUMNS}) VALUES \
             (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25)"
        ),
        params![
            s.id.to_string(),
            s.run_id.to_string(),
            s.position,
            s.command_id.map(|u| u.to_string()),
            s.name,
            argv,
            s.command_line,
            s.cwd,
            s.timeout_secs,
            s.policy_decision,
            s.policy_reason,
            s.policy_explanation,
            s.policy_rule_id.map(|u| u.to_string()),
            s.policy_revision,
            s.authorized_by,
            s.status.as_str(),
            s.exit_code,
            s.signal,
            s.stdout,
            s.stderr,
            s.stdout_truncated,
            s.stderr_truncated,
            s.detail,
            s.started_at_ms,
            s.ended_at_ms
        ],
    )?;
    Ok(())
}

pub fn steps_for_run(conn: &Connection, run: Uuid) -> Result<Vec<StepRecord>, StorageError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {STEP_COLUMNS} FROM validation_steps WHERE run_id = ?1 ORDER BY position"
    ))?;
    let rows = stmt.query_map([run.to_string()], step_from_row)?;
    rows.map(|r| r?).collect()
}

pub fn mark_step_running(conn: &Connection, id: Uuid, now: i64) -> Result<(), StorageError> {
    conn.execute(
        "UPDATE validation_steps SET status = 'running', started_at = ?1 WHERE id = ?2 AND status = 'pending'",
        params![now, id.to_string()],
    )?;
    Ok(())
}

/// The outcome of one step.
#[derive(Debug, Clone, Default)]
pub struct StepOutcome {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub detail: Option<String>,
}

/// Ends a pending or running step exactly once.
pub fn finish_step(
    conn: &Connection,
    id: Uuid,
    status: StepStatus,
    o: &StepOutcome,
    now: i64,
) -> Result<bool, StorageError> {
    Ok(conn.execute(
        "UPDATE validation_steps SET status = ?1, exit_code = ?2, signal = ?3, stdout = ?4, stderr = ?5, \
         stdout_truncated = ?6, stderr_truncated = ?7, detail = ?8, ended_at = ?9 \
         WHERE id = ?10 AND status IN ('pending', 'running')",
        params![
            status.as_str(),
            o.exit_code,
            o.signal,
            o.stdout,
            o.stderr,
            o.stdout_truncated,
            o.stderr_truncated,
            o.detail,
            now,
            id.to_string()
        ],
    )? > 0)
}

// --- review content (M6 assignments) ---------------------------------------------

pub fn set_review_tree(conn: &Connection, assignment: Uuid, tree: &str) -> Result<(), StorageError> {
    conn.execute(
        "UPDATE task_assignments SET review_tree = ?1 WHERE id = ?2",
        params![tree, assignment.to_string()],
    )?;
    Ok(())
}

pub fn review_tree(conn: &Connection, assignment: Uuid) -> Result<Option<String>, StorageError> {
    Ok(conn
        .query_row("SELECT review_tree FROM task_assignments WHERE id = ?1", [assignment.to_string()], |r| {
            r.get::<_, Option<String>>(0)
        })
        .optional()?
        .flatten())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Storage;

    fn setup() -> (tempfile::TempDir, Storage, Uuid, Uuid) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("t.db")).unwrap();
        let (project, ws) = (Uuid::now_v7(), Uuid::now_v7());
        storage
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO projects (id, name, root_path, created_at, updated_at) VALUES (?1, 'p', '/r', 1, 1)",
                    [project.to_string()],
                )?;
                c.execute(
                    "INSERT INTO workspaces (id, project_id, kind, name, path, branch, base_commit, status, created_at, \
                     updated_at, last_activity_at) VALUES (?1, ?2, 'worktree', 'w', '/w', 'switchyard/w', 'abc', 'ready', 1, 1, 1)",
                    params![ws.to_string(), project.to_string()],
                )?;
                Ok(())
            })
            .unwrap();
        (dir, storage, project, ws)
    }

    fn integration(project: Uuid, ws: Uuid) -> IntegrationRecord {
        IntegrationRecord {
            id: Uuid::now_v7(),
            project_id: project,
            task_id: None,
            workspace_id: ws,
            source_branch: "switchyard/w".into(),
            source_commit: "a".repeat(40),
            target_branch: "main".into(),
            target_commit: "b".repeat(40),
            base_commit: None,
            status: IntegrationStatus::Open,
            status_reason: None,
            preflight: serde_json::json!({}),
            prepared_at_ms: 1,
            approved_by: None,
            approved_at_ms: None,
            merge_commit: None,
            method: None,
            failure: None,
            revision: 1,
            created_at_ms: 1,
            updated_at_ms: 1,
            completed_at_ms: None,
        }
    }

    #[test]
    fn one_unfinished_integration_per_workspace_and_guarded_transitions() {
        let (_d, s, p, w) = setup();
        s.with_conn(|c| {
            let i = integration(p, w);
            insert_integration(c, &i)?;
            assert!(matches!(insert_integration(c, &integration(p, w)), Err(StorageError::Conflict(_))));
            let illegal =
                IntegrationUpdate { status: Some(IntegrationStatus::Integrating), ..Default::default() };
            assert!(
                update_integration(c, i.id, IntegrationStatus::Open, &illegal, 2).is_err(),
                "open → integrating"
            );
            let stale = IntegrationUpdate { status: Some(IntegrationStatus::Ready), ..Default::default() };
            assert!(
                update_integration(c, i.id, IntegrationStatus::Ready, &stale, 2).is_err(),
                "wrong expected status"
            );
            let (_, after) = update_integration(c, i.id, IntegrationStatus::Open, &stale, 2)?;
            assert_eq!((after.status, after.revision), (IntegrationStatus::Ready, 2));
            let cancel = IntegrationUpdate {
                status: Some(IntegrationStatus::Cancelled),
                completed: true,
                ..Default::default()
            };
            update_integration(c, i.id, IntegrationStatus::Ready, &cancel, 3)?;
            // A cancelled integration frees the workspace.
            insert_integration(c, &integration(p, w))?;
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn settings_default_to_requiring_review() {
        let (_d, s, p, _) = setup();
        s.with_conn(|c| {
            assert!(settings(c, p)?.require_review);
            assert!(!set_settings(c, p, false, 5)?.require_review);
            assert_eq!(set_settings(c, p, true, 6)?.revision, 2);
            Ok(())
        })
        .unwrap();
    }
}
