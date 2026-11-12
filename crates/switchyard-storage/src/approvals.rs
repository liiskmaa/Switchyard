//! Centralized approvals. Status changes are guarded in SQL so that a request
//! can be decided at most once, regardless of which path (operator, timeout,
//! agent cancellation, shutdown) gets there first.

use rusqlite::{Connection, Row, params};
use serde_json::Value;
use switchyard_agents::{ApprovalStatus, DecisionSource};
use uuid::Uuid;

use crate::{StorageError, parse_enum, parse_uuid};

#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalRecord {
    pub id: Uuid,
    pub project_id: Uuid,
    pub session_id: Uuid,
    pub agent_id: Option<Uuid>,
    pub tool_name: String,
    pub tool_use_id: Option<String>,
    pub native_request_id: String,
    /// `{input, description, details}` as reported by the agent.
    pub request: Value,
    pub status: ApprovalStatus,
    pub requested_at_ms: i64,
    pub expires_at_ms: i64,
    pub decided_at_ms: Option<i64>,
    pub decision_source: Option<DecisionSource>,
    pub decided_by: Option<String>,
    pub decision_note: Option<String>,
    /// The policy evaluation that created this approval (`None` for
    /// approvals recorded before policies existed).
    pub evaluation_id: Option<Uuid>,
}

const COLUMNS: &str = "id, project_id, session_id, agent_id, tool_name, tool_use_id, native_request_id, request, \
                       status, requested_at, expires_at, decided_at, decision_source, decided_by, decision_note, evaluation_id";

fn from_row(r: &Row<'_>) -> rusqlite::Result<Result<ApprovalRecord, StorageError>> {
    let id: String = r.get(0)?;
    let project: String = r.get(1)?;
    let session: String = r.get(2)?;
    let agent: Option<String> = r.get(3)?;
    let request: String = r.get(7)?;
    let status: String = r.get(8)?;
    let source: Option<String> = r.get(12)?;
    let evaluation: Option<String> = r.get(15)?;
    let partial = ApprovalRecord {
        id: uuid::Uuid::nil(),
        project_id: uuid::Uuid::nil(),
        session_id: uuid::Uuid::nil(),
        agent_id: None,
        tool_name: r.get(4)?,
        tool_use_id: r.get(5)?,
        native_request_id: r.get(6)?,
        request: Value::Null,
        status: ApprovalStatus::Pending,
        requested_at_ms: r.get(9)?,
        expires_at_ms: r.get(10)?,
        decided_at_ms: r.get(11)?,
        decision_source: None,
        decided_by: r.get(13)?,
        decision_note: r.get(14)?,
        evaluation_id: None,
    };
    Ok((|| {
        Ok(ApprovalRecord {
            id: parse_uuid(&id, "approvals.id")?,
            project_id: parse_uuid(&project, "approvals.project_id")?,
            session_id: parse_uuid(&session, "approvals.session_id")?,
            agent_id: agent.as_deref().map(|a| parse_uuid(a, "approvals.agent_id")).transpose()?,
            request: serde_json::from_str(&request)
                .map_err(|e| StorageError::Corrupt(format!("approvals.request: {e}")))?,
            status: parse_enum(&status, "approvals.status")?,
            decision_source: source
                .as_deref()
                .map(|s| parse_enum(s, "approvals.decision_source"))
                .transpose()?,
            evaluation_id: evaluation
                .as_deref()
                .map(|e| parse_uuid(e, "approvals.evaluation_id"))
                .transpose()?,
            ..partial
        })
    })())
}

fn collect(
    stmt: &mut rusqlite::Statement<'_>,
    args: impl rusqlite::Params,
) -> Result<Vec<ApprovalRecord>, StorageError> {
    let rows = stmt.query_map(args, from_row)?;
    rows.map(|r| r.map_err(StorageError::from).and_then(|inner| inner)).collect()
}

pub fn insert(conn: &Connection, a: &ApprovalRecord) -> Result<(), StorageError> {
    conn.execute(
        &format!("INSERT INTO approvals (kind, {COLUMNS}) VALUES ('tool_use', ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)"),
        params![
            a.id.to_string(),
            a.project_id.to_string(),
            a.session_id.to_string(),
            a.agent_id.map(|x| x.to_string()),
            a.tool_name,
            a.tool_use_id,
            a.native_request_id,
            a.request.to_string(),
            a.status.as_str(),
            a.requested_at_ms,
            a.expires_at_ms,
            a.decided_at_ms,
            a.decision_source.map(DecisionSource::as_str),
            a.decided_by,
            a.decision_note,
            a.evaluation_id.map(|e| e.to_string()),
        ],
    )
    .map_err(|e| StorageError::from_unique(e, || format!("permission request {} already recorded", a.native_request_id)))?;
    Ok(())
}

pub fn get(conn: &Connection, id: Uuid) -> Result<ApprovalRecord, StorageError> {
    let mut stmt = conn.prepare_cached(&format!("SELECT {COLUMNS} FROM approvals WHERE id = ?1"))?;
    collect(&mut stmt, [id.to_string()])?
        .pop()
        .ok_or_else(|| StorageError::NotFound(format!("approval {id}")))
}

pub fn find_by_native(
    conn: &Connection,
    session_id: Uuid,
    native_request_id: &str,
) -> Result<Option<ApprovalRecord>, StorageError> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM approvals WHERE session_id = ?1 AND native_request_id = ?2"
    ))?;
    Ok(collect(&mut stmt, params![session_id.to_string(), native_request_id])?.pop())
}

/// Newest first, filtered by optional status/session/project.
pub fn list(
    conn: &Connection,
    status: Option<ApprovalStatus>,
    session_id: Option<Uuid>,
    project_id: Option<Uuid>,
    limit: u32,
) -> Result<Vec<ApprovalRecord>, StorageError> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM approvals WHERE (?1 IS NULL OR status = ?1) AND (?2 IS NULL OR session_id = ?2) \
         AND (?3 IS NULL OR project_id = ?3) ORDER BY requested_at DESC, id DESC LIMIT ?4"
    ))?;
    collect(
        &mut stmt,
        params![
            status.map(ApprovalStatus::as_str),
            session_id.map(|s| s.to_string()),
            project_id.map(|p| p.to_string()),
            limit
        ],
    )
}

/// Moves a *pending* approval to a final status. Fails with `Conflict` if it
/// was already decided, expired or cancelled.
pub fn decide(
    conn: &Connection,
    id: Uuid,
    status: ApprovalStatus,
    source: DecisionSource,
    decided_by: Option<&str>,
    note: Option<&str>,
    now_ms: i64,
) -> Result<ApprovalRecord, StorageError> {
    if !status.is_final() {
        return Err(StorageError::Conflict("an approval can only move to a final status".into()));
    }
    let changed = conn.execute(
        "UPDATE approvals SET status = ?2, decision_source = ?3, decided_by = ?4, decision_note = ?5, decided_at = ?6 \
         WHERE id = ?1 AND status = 'pending'",
        params![id.to_string(), status.as_str(), source.as_str(), decided_by, note, now_ms],
    )?;
    let record = get(conn, id)?;
    if changed == 0 {
        return Err(StorageError::Conflict(format!("approval {id} is already {}", record.status)));
    }
    Ok(record)
}

/// Settles every pending approval of a session (agent exit, shutdown).
pub fn settle_pending_for_session(
    conn: &Connection,
    session_id: Uuid,
    status: ApprovalStatus,
    source: DecisionSource,
    now_ms: i64,
) -> Result<Vec<ApprovalRecord>, StorageError> {
    let pending = list(conn, Some(ApprovalStatus::Pending), Some(session_id), None, u32::MAX)?;
    pending.into_iter().map(|a| decide(conn, a.id, status, source, None, None, now_ms)).collect()
}

pub fn count_pending(conn: &Connection, session_id: Uuid) -> Result<u32, StorageError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM approvals WHERE session_id = ?1 AND status = 'pending'",
        [session_id.to_string()],
        |r| r.get(0),
    )?;
    Ok(u32::try_from(n).unwrap_or(u32::MAX))
}

/// Pending approvals with `expires_at <= now`.
pub fn due(conn: &Connection, now_ms: i64) -> Result<Vec<ApprovalRecord>, StorageError> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM approvals WHERE status = 'pending' AND expires_at <= ?1 ORDER BY expires_at"
    ))?;
    collect(&mut stmt, [now_ms])
}

#[cfg(test)]
mod tests {
    use switchyard_events::new_id;

    use super::*;
    use crate::test_support::{insert_project, temp_storage};

    fn pending(project: Uuid, session: Uuid, expires: i64) -> ApprovalRecord {
        ApprovalRecord {
            id: new_id(),
            project_id: project,
            session_id: session,
            agent_id: None,
            tool_name: "Bash".into(),
            tool_use_id: Some("toolu_1".into()),
            native_request_id: new_id().to_string(),
            request: serde_json::json!({"input": {"command": "ls"}}),
            status: ApprovalStatus::Pending,
            requested_at_ms: 1,
            expires_at_ms: expires,
            decided_at_ms: None,
            decision_source: None,
            decided_by: None,
            decision_note: None,
            evaluation_id: None,
        }
    }

    #[test]
    fn decisions_happen_exactly_once() {
        let (_dir, storage) = temp_storage();
        let project = insert_project(&storage, "/repo");
        storage
            .with_conn(|c| {
                let s = crate::sessions::tests::sample(c, project);
                crate::sessions::insert(c, &s)?;
                let a = pending(project, s.id, 100);
                insert(c, &a)?;
                assert!(matches!(insert(c, &a), Err(StorageError::Conflict(_))));
                assert_eq!(find_by_native(c, s.id, &a.native_request_id)?.map(|r| r.id), Some(a.id));
                let approved = decide(
                    c,
                    a.id,
                    ApprovalStatus::Approved,
                    DecisionSource::Operator,
                    Some("desktop"),
                    None,
                    5,
                )?;
                assert_eq!((approved.status, approved.decided_at_ms), (ApprovalStatus::Approved, Some(5)));
                // A second decision (or a late timeout) is rejected.
                assert!(matches!(
                    decide(c, a.id, ApprovalStatus::Denied, DecisionSource::Operator, None, None, 6),
                    Err(StorageError::Conflict(_))
                ));
                assert!(matches!(
                    decide(c, a.id, ApprovalStatus::Pending, DecisionSource::Operator, None, None, 6),
                    Err(StorageError::Conflict(_))
                ));
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn expiry_and_session_settlement() {
        let (_dir, storage) = temp_storage();
        let project = insert_project(&storage, "/repo");
        storage
            .with_conn(|c| {
                let s = crate::sessions::tests::sample(c, project);
                crate::sessions::insert(c, &s)?;
                let early = pending(project, s.id, 10);
                let late = pending(project, s.id, 1000);
                insert(c, &early)?;
                insert(c, &late)?;
                assert_eq!(due(c, 50)?.iter().map(|a| a.id).collect::<Vec<_>>(), vec![early.id]);
                decide(c, early.id, ApprovalStatus::Expired, DecisionSource::Timeout, None, None, 50)?;
                let settled = settle_pending_for_session(
                    c,
                    s.id,
                    ApprovalStatus::Cancelled,
                    DecisionSource::AgentExited,
                    60,
                )?;
                assert_eq!(settled.iter().map(|a| a.id).collect::<Vec<_>>(), vec![late.id]);
                assert!(list(c, Some(ApprovalStatus::Pending), None, None, 10)?.is_empty());
                assert_eq!(list(c, None, Some(s.id), None, 10)?.len(), 2);
                Ok(())
            })
            .unwrap();
    }
}
