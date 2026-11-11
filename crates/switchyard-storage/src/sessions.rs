//! Agent sessions.

use rusqlite::{Connection, OptionalExtension, Row, params};
use switchyard_agents::{AgentProvider, SessionState};
use uuid::Uuid;

use crate::{StorageError, parse_enum, parse_uuid};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub id: Uuid,
    pub project_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub agent_id: Uuid,
    pub provider: AgentProvider,
    pub native_session_id: Option<String>,
    pub cwd: String,
    pub state: SessionState,
    pub pid: Option<u32>,
    pub cli_version: Option<String>,
    pub model: Option<String>,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    pub last_activity_at_ms: i64,
    pub exit_code: Option<i32>,
    pub exit_signal: Option<i32>,
    pub failure: Option<String>,
    pub resume_count: u32,
}

const COLUMNS: &str = "id, project_id, workspace_id, agent_id, provider, native_session_id, cwd, state, pid, \
                       cli_version, model, started_at, ended_at, last_activity_at, exit_code, exit_signal, \
                       failure, resume_count";

fn from_row(r: &Row<'_>) -> rusqlite::Result<Result<SessionRecord, StorageError>> {
    let id: String = r.get(0)?;
    let project: String = r.get(1)?;
    let workspace: Option<String> = r.get(2)?;
    let agent: String = r.get(3)?;
    let provider: String = r.get(4)?;
    let state: String = r.get(7)?;
    let pid: Option<i64> = r.get(8)?;
    let resume_count: i64 = r.get(17)?;
    let partial = SessionRecord {
        id: Uuid::nil(),
        project_id: Uuid::nil(),
        workspace_id: None,
        agent_id: Uuid::nil(),
        provider: AgentProvider::ClaudeCode,
        native_session_id: r.get(5)?,
        cwd: r.get(6)?,
        state: SessionState::Starting,
        pid: pid.and_then(|p| u32::try_from(p).ok()),
        cli_version: r.get(9)?,
        model: r.get(10)?,
        started_at_ms: r.get(11)?,
        ended_at_ms: r.get(12)?,
        last_activity_at_ms: r.get(13)?,
        exit_code: r.get(14)?,
        exit_signal: r.get(15)?,
        failure: r.get(16)?,
        resume_count: u32::try_from(resume_count).unwrap_or(0),
    };
    Ok((|| {
        Ok(SessionRecord {
            id: parse_uuid(&id, "sessions.id")?,
            project_id: parse_uuid(&project, "sessions.project_id")?,
            workspace_id: workspace.as_deref().map(|w| parse_uuid(w, "sessions.workspace_id")).transpose()?,
            agent_id: parse_uuid(&agent, "sessions.agent_id")?,
            provider: parse_enum(&provider, "sessions.provider")?,
            state: parse_enum(&state, "sessions.state")?,
            ..partial
        })
    })())
}

fn collect(
    stmt: &mut rusqlite::Statement<'_>,
    args: impl rusqlite::Params,
) -> Result<Vec<SessionRecord>, StorageError> {
    let rows = stmt.query_map(args, from_row)?;
    rows.map(|r| r.map_err(StorageError::from).and_then(|inner| inner)).collect()
}

pub fn insert(conn: &Connection, s: &SessionRecord) -> Result<(), StorageError> {
    conn.execute(
        &format!("INSERT INTO sessions ({COLUMNS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)"),
        params![
            s.id.to_string(),
            s.project_id.to_string(),
            s.workspace_id.map(|w| w.to_string()),
            s.agent_id.to_string(),
            s.provider.as_str(),
            s.native_session_id,
            s.cwd,
            s.state.as_str(),
            s.pid,
            s.cli_version,
            s.model,
            s.started_at_ms,
            s.ended_at_ms,
            s.last_activity_at_ms,
            s.exit_code,
            s.exit_signal,
            s.failure,
            s.resume_count,
        ],
    )
    .map_err(|e| StorageError::from_unique(e, || format!("session {} already exists", s.id)))?;
    Ok(())
}

pub fn get(conn: &Connection, id: Uuid) -> Result<SessionRecord, StorageError> {
    let mut stmt = conn.prepare_cached(&format!("SELECT {COLUMNS} FROM sessions WHERE id = ?1"))?;
    collect(&mut stmt, [id.to_string()])?.pop().ok_or_else(|| StorageError::NotFound(format!("session {id}")))
}

/// Sessions newest first, optionally for one project and/or only live ones.
pub fn list(
    conn: &Connection,
    project_id: Option<Uuid>,
    live_only: bool,
    limit: u32,
) -> Result<Vec<SessionRecord>, StorageError> {
    let live = crate::stats::ACTIVE_SESSION_STATUSES;
    let sql = format!(
        "SELECT {COLUMNS} FROM sessions WHERE (?1 IS NULL OR project_id = ?1) {} \
         ORDER BY started_at DESC, id DESC LIMIT ?2",
        if live_only { format!("AND state IN ({live})") } else { String::new() }
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    collect(&mut stmt, params![project_id.map(|p| p.to_string()), limit])
}

/// Field updates applied together with a state change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionChanges {
    pub pid: Option<Option<u32>>,
    pub native_session_id: Option<String>,
    pub model: Option<String>,
    pub ended_at_ms: Option<Option<i64>>,
    pub exit_code: Option<Option<i32>>,
    pub exit_signal: Option<Option<i32>>,
    pub failure: Option<Option<String>>,
    pub increment_resume_count: bool,
}

/// Validates and applies a state transition plus `changes`. Returns the
/// updated record. Fails with `Conflict` on an illegal transition.
pub fn transition(
    conn: &Connection,
    id: Uuid,
    to: SessionState,
    changes: &SessionChanges,
    now_ms: i64,
) -> Result<SessionRecord, StorageError> {
    let mut current = get(conn, id)?;
    current.state =
        current.state.transition(to).map_err(|e| StorageError::Conflict(format!("session {id}: {e}")))?;
    if let Some(pid) = changes.pid {
        current.pid = pid;
    }
    if let Some(native) = &changes.native_session_id {
        current.native_session_id = Some(native.clone());
    }
    if let Some(model) = &changes.model {
        current.model = Some(model.clone());
    }
    if let Some(ended) = changes.ended_at_ms {
        current.ended_at_ms = ended;
    }
    if let Some(code) = changes.exit_code {
        current.exit_code = code;
    }
    if let Some(signal) = changes.exit_signal {
        current.exit_signal = signal;
    }
    if let Some(failure) = &changes.failure {
        current.failure = failure.clone();
    }
    if changes.increment_resume_count {
        current.resume_count += 1;
    }
    current.last_activity_at_ms = now_ms;
    conn.execute(
        "UPDATE sessions SET state = ?2, pid = ?3, native_session_id = ?4, model = ?5, ended_at = ?6, \
         exit_code = ?7, exit_signal = ?8, failure = ?9, resume_count = ?10, last_activity_at = ?11 \
         WHERE id = ?1",
        params![
            id.to_string(),
            current.state.as_str(),
            current.pid,
            current.native_session_id,
            current.model,
            current.ended_at_ms,
            current.exit_code,
            current.exit_signal,
            current.failure,
            current.resume_count,
            now_ms,
        ],
    )
    .map_err(|e| {
        StorageError::from_unique(e, || format!("native session id already mapped (session {id})"))
    })?;
    Ok(current)
}

pub fn touch(conn: &Connection, id: Uuid, now_ms: i64) -> Result<(), StorageError> {
    conn.execute("UPDATE sessions SET last_activity_at = ?2 WHERE id = ?1", params![id.to_string(), now_ms])?;
    Ok(())
}

pub fn find_by_native(
    conn: &Connection,
    provider: AgentProvider,
    native: &str,
) -> Result<Option<SessionRecord>, StorageError> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM sessions WHERE provider = ?1 AND native_session_id = ?2"
    ))?;
    Ok(collect(&mut stmt, params![provider.as_str(), native])?.pop())
}

pub fn exists_live_for_project(conn: &Connection, project_id: Uuid) -> Result<bool, StorageError> {
    let live = crate::stats::ACTIVE_SESSION_STATUSES;
    Ok(conn
        .query_row(
            &format!("SELECT 1 FROM sessions WHERE project_id = ?1 AND state IN ({live}) LIMIT 1"),
            [project_id.to_string()],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

#[cfg(test)]
pub(crate) mod tests {
    use switchyard_events::new_id;

    use super::*;
    use crate::test_support::{insert_project, temp_storage};

    pub(crate) fn sample(conn: &Connection, project_id: Uuid) -> SessionRecord {
        let agent = crate::agents::upsert_agent(
            conn,
            AgentProvider::ClaudeCode,
            "Claude Code",
            "/bin/claude",
            "2.1.322",
            1,
        )
        .unwrap();
        SessionRecord {
            id: new_id(),
            project_id,
            workspace_id: None,
            agent_id: agent,
            provider: AgentProvider::ClaudeCode,
            native_session_id: Some(new_id().to_string()),
            cwd: "/repo".into(),
            state: SessionState::Starting,
            pid: Some(42),
            cli_version: Some("2.1.322".into()),
            model: None,
            started_at_ms: 1,
            ended_at_ms: None,
            last_activity_at_ms: 1,
            exit_code: None,
            exit_signal: None,
            failure: None,
            resume_count: 0,
        }
    }

    #[test]
    fn insert_get_transition_and_list() {
        let (_dir, storage) = temp_storage();
        let project = insert_project(&storage, "/repo");
        storage
            .with_conn(|c| {
                let s = sample(c, project);
                insert(c, &s)?;
                assert_eq!(get(c, s.id)?, s);
                assert!(exists_live_for_project(c, project)?);
                let s2 = transition(
                    c,
                    s.id,
                    SessionState::Idle,
                    &SessionChanges { model: Some("haiku".into()), ..Default::default() },
                    5,
                )?;
                assert_eq!(
                    (s2.state, s2.model.as_deref(), s2.last_activity_at_ms),
                    (SessionState::Idle, Some("haiku"), 5)
                );
                assert!(matches!(
                    transition(c, s.id, SessionState::WaitingForApproval, &SessionChanges::default(), 6),
                    Err(StorageError::Conflict(_))
                ));
                let done = transition(
                    c,
                    s.id,
                    SessionState::Stopped,
                    &SessionChanges {
                        pid: Some(None),
                        ended_at_ms: Some(Some(9)),
                        exit_code: Some(Some(0)),
                        ..Default::default()
                    },
                    9,
                )?;
                assert_eq!((done.pid, done.exit_code), (None, Some(0)));
                assert!(!exists_live_for_project(c, project)?);
                assert_eq!(list(c, Some(project), false, 10)?.len(), 1);
                assert!(list(c, Some(project), true, 10)?.is_empty());
                assert_eq!(
                    find_by_native(
                        c,
                        AgentProvider::ClaudeCode,
                        s.native_session_id.as_deref().unwrap_or("")
                    )?
                    .map(|r| r.id),
                    Some(s.id)
                );
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn native_session_ids_are_unique_per_provider() {
        let (_dir, storage) = temp_storage();
        let project = insert_project(&storage, "/repo");
        storage
            .with_conn(|c| {
                let a = sample(c, project);
                insert(c, &a)?;
                let mut b = sample(c, project);
                b.native_session_id = a.native_session_id.clone();
                assert!(matches!(insert(c, &b), Err(StorageError::Conflict(_))));
                Ok(())
            })
            .unwrap();
    }
}
