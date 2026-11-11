//! Event journal persistence.

use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter, types::Value as SqlValue};
use switchyard_events::{EventContext, EventDraft, EventEnvelope, EventFilter, EventJournal, JournalError};
use uuid::Uuid;

use crate::{Storage, StorageError, parse_uuid};

const COLUMNS: &str = "seq, id, schema_version, event_type, occurred_at, project_id, workspace_id, \
                       session_id, task_id, agent_id, causation_id, payload";

/// Appends `draft` within the caller's connection/transaction.
pub fn append(conn: &Connection, draft: EventDraft) -> Result<EventEnvelope, StorageError> {
    let payload = serde_json::to_string(&draft.payload)
        .map_err(|e| StorageError::Corrupt(format!("unserializable payload: {e}")))?;
    let ctx = &draft.context;
    let id = draft.id;
    conn.execute(
        "INSERT INTO events (id, schema_version, event_type, occurred_at, project_id, workspace_id, \
         session_id, task_id, agent_id, causation_id, payload) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            id.to_string(),
            draft.schema_version,
            draft.event_type,
            draft.occurred_at_ms,
            ctx.project_id.map(|u| u.to_string()),
            ctx.workspace_id.map(|u| u.to_string()),
            ctx.session_id.map(|u| u.to_string()),
            ctx.task_id.map(|u| u.to_string()),
            ctx.agent_id.map(|u| u.to_string()),
            ctx.causation_id.map(|u| u.to_string()),
            payload,
        ],
    )
    .map_err(|e| StorageError::from_unique(e, || format!("event {id} already exists")))?;
    Ok(draft.into_envelope(conn.last_insert_rowid()))
}

/// Reads events in ascending `seq` order.
pub fn read(conn: &Connection, filter: &EventFilter) -> Result<Vec<EventEnvelope>, StorageError> {
    let mut sql = format!("SELECT {COLUMNS} FROM events WHERE seq > ?");
    let mut args: Vec<SqlValue> = vec![SqlValue::Integer(filter.after_seq.unwrap_or(0))];
    if let Some(project_id) = filter.project_id {
        sql.push_str(" AND project_id = ?");
        args.push(SqlValue::Text(project_id.to_string()));
    }
    if let Some(session_id) = filter.session_id {
        sql.push_str(" AND session_id = ?");
        args.push(SqlValue::Text(session_id.to_string()));
    }
    if !filter.event_types.is_empty() {
        sql.push_str(" AND event_type IN (");
        sql.push_str(&vec!["?"; filter.event_types.len()].join(", "));
        sql.push(')');
        args.extend(filter.event_types.iter().cloned().map(SqlValue::Text));
    }
    sql.push_str(" ORDER BY seq ASC LIMIT ?");
    args.push(SqlValue::Integer(i64::from(filter.limit)));

    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(params_from_iter(args), RawEvent::from_row)?;
    rows.map(|r| r.map_err(StorageError::from).and_then(RawEvent::into_envelope)).collect()
}

/// Reads the most recent `limit` events, returned in ascending `seq` order.
pub fn read_latest(conn: &Connection, limit: u32) -> Result<Vec<EventEnvelope>, StorageError> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM (SELECT * FROM events ORDER BY seq DESC LIMIT ?1) ORDER BY seq ASC"
    ))?;
    let rows = stmt.query_map([limit], RawEvent::from_row)?;
    rows.map(|r| r.map_err(StorageError::from).and_then(RawEvent::into_envelope)).collect()
}

pub fn last_seq(conn: &Connection) -> Result<Option<i64>, StorageError> {
    Ok(conn.query_row("SELECT MAX(seq) FROM events", [], |r| r.get(0)).optional()?.flatten())
}

struct RawEvent {
    seq: i64,
    id: String,
    schema_version: u32,
    event_type: String,
    occurred_at_ms: i64,
    ids: [Option<String>; 6],
    payload: String,
}

impl RawEvent {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            seq: row.get(0)?,
            id: row.get(1)?,
            schema_version: row.get(2)?,
            event_type: row.get(3)?,
            occurred_at_ms: row.get(4)?,
            ids: [row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?, row.get(10)?],
            payload: row.get(11)?,
        })
    }

    fn into_envelope(self) -> Result<EventEnvelope, StorageError> {
        let opt = |v: &Option<String>, col: &'static str| -> Result<Option<Uuid>, StorageError> {
            v.as_deref().map(|s| parse_uuid(s, col)).transpose()
        };
        let [project, workspace, session, task, agent, causation] = &self.ids;
        Ok(EventEnvelope {
            seq: self.seq,
            id: parse_uuid(&self.id, "events.id")?,
            schema_version: self.schema_version,
            event_type: self.event_type,
            occurred_at_ms: self.occurred_at_ms,
            context: EventContext {
                project_id: opt(project, "events.project_id")?,
                workspace_id: opt(workspace, "events.workspace_id")?,
                session_id: opt(session, "events.session_id")?,
                task_id: opt(task, "events.task_id")?,
                agent_id: opt(agent, "events.agent_id")?,
                causation_id: opt(causation, "events.causation_id")?,
            },
            payload: serde_json::from_str(&self.payload)
                .map_err(|e| StorageError::Corrupt(format!("event {} payload: {e}", self.seq)))?,
        })
    }
}

impl From<StorageError> for JournalError {
    fn from(err: StorageError) -> Self {
        match err {
            StorageError::Conflict(msg) => JournalError::Storage(msg),
            other => JournalError::Storage(other.to_string()),
        }
    }
}

impl EventJournal for Storage {
    fn append(&self, draft: EventDraft) -> Result<EventEnvelope, JournalError> {
        let id = draft.id;
        self.with_conn(|c| append(c, draft)).map_err(|e| match e {
            StorageError::Conflict(_) => JournalError::Duplicate(id),
            other => other.into(),
        })
    }

    fn read(&self, filter: &EventFilter) -> Result<Vec<EventEnvelope>, JournalError> {
        Ok(self.with_conn(|c| read(c, filter))?)
    }
}

#[cfg(test)]
mod tests {
    use switchyard_events::{DomainEvent, new_id};

    use super::*;
    use crate::test_support::temp_storage;

    fn draft(project_id: Uuid) -> EventDraft {
        EventDraft::new(
            DomainEvent::ProjectRemoved { project_id, root_path: "/x".into() },
            EventContext { project_id: Some(project_id), causation_id: Some(new_id()), ..Default::default() },
        )
        .unwrap()
    }

    #[test]
    fn append_assigns_increasing_seq_and_roundtrips_all_fields() {
        let (_dir, storage) = temp_storage();
        let d = draft(new_id());
        let expected = d.clone();
        let a = EventJournal::append(&storage, d).unwrap();
        let b = EventJournal::append(&storage, draft(new_id())).unwrap();
        assert!(b.seq > a.seq);
        let all = EventJournal::read(&storage, &EventFilter { limit: 10, ..Default::default() }).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0], expected.into_envelope(a.seq));
        assert_eq!(storage.with_conn(|c| last_seq(c)).unwrap(), Some(b.seq));
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let (_dir, storage) = temp_storage();
        let d = draft(new_id());
        EventJournal::append(&storage, d.clone()).unwrap();
        assert!(matches!(EventJournal::append(&storage, d), Err(JournalError::Duplicate(_))));
    }

    #[test]
    fn read_filters_by_after_seq_project_type_and_limit() {
        let (_dir, storage) = temp_storage();
        let p = new_id();
        for i in 0..6 {
            EventJournal::append(&storage, draft(if i % 2 == 0 { p } else { new_id() })).unwrap();
        }
        let f = EventFilter { after_seq: Some(1), project_id: Some(p), limit: 10, ..Default::default() };
        let seqs: Vec<i64> = EventJournal::read(&storage, &f).unwrap().iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![3, 5]);
        let f = EventFilter { event_types: vec!["nope".into()], limit: 10, ..Default::default() };
        assert!(EventJournal::read(&storage, &f).unwrap().is_empty());
        let f = EventFilter { limit: 2, ..Default::default() };
        assert_eq!(EventJournal::read(&storage, &f).unwrap().len(), 2);
        let latest: Vec<i64> =
            storage.with_conn(|c| read_latest(c, 2)).unwrap().iter().map(|e| e.seq).collect();
        assert_eq!(latest, vec![5, 6]);
    }

    #[test]
    fn journal_is_append_only() {
        let (_dir, storage) = temp_storage();
        EventJournal::append(&storage, draft(new_id())).unwrap();
        let update = storage.with_conn(|c| Ok(c.execute("UPDATE events SET event_type = 'x'", [])?));
        assert!(update.is_err());
        let delete = storage.with_conn(|c| Ok(c.execute("DELETE FROM events", [])?));
        assert!(delete.is_err());
    }
}
