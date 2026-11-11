//! Project registrations.

use rusqlite::{Connection, OptionalExtension, Row, params};
use switchyard_events::{EventDraft, EventEnvelope};
use uuid::Uuid;

use crate::{StorageError, events, parse_uuid};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRecord {
    pub id: Uuid,
    pub name: String,
    pub root_path: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

const COLUMNS: &str = "id, name, root_path, created_at, updated_at";

fn from_row(row: &Row<'_>) -> rusqlite::Result<(String, String, String, i64, i64)> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
}

fn into_record(raw: (String, String, String, i64, i64)) -> Result<ProjectRecord, StorageError> {
    let (id, name, root_path, created_at_ms, updated_at_ms) = raw;
    Ok(ProjectRecord { id: parse_uuid(&id, "projects.id")?, name, root_path, created_at_ms, updated_at_ms })
}

pub fn list(conn: &Connection) -> Result<Vec<ProjectRecord>, StorageError> {
    let mut stmt =
        conn.prepare_cached(&format!("SELECT {COLUMNS} FROM projects ORDER BY name COLLATE NOCASE, id"))?;
    let rows = stmt.query_map([], from_row)?;
    rows.map(|r| r.map_err(StorageError::from).and_then(into_record)).collect()
}

pub fn get(conn: &Connection, id: Uuid) -> Result<ProjectRecord, StorageError> {
    conn.query_row(&format!("SELECT {COLUMNS} FROM projects WHERE id = ?1"), [id.to_string()], from_row)
        .optional()?
        .ok_or_else(|| StorageError::NotFound(format!("project {id}")))
        .and_then(into_record)
}

pub fn find_by_path(conn: &Connection, root_path: &str) -> Result<Option<ProjectRecord>, StorageError> {
    conn.query_row(&format!("SELECT {COLUMNS} FROM projects WHERE root_path = ?1"), [root_path], from_row)
        .optional()?
        .map(into_record)
        .transpose()
}

pub fn count(conn: &Connection) -> Result<u64, StorageError> {
    count_query(conn, "SELECT COUNT(*) FROM projects")
}

/// Runs a `SELECT COUNT(*)` query. SQLite counts are non-negative `i64`.
pub(crate) fn count_query(conn: &Connection, sql: &str) -> Result<u64, StorageError> {
    let n: i64 = conn.query_row(sql, [], |r| r.get(0))?;
    u64::try_from(n).map_err(|_| StorageError::Corrupt(format!("negative count from {sql:?}")))
}

/// Inserts the project and appends `event` in a single transaction.
pub fn insert_with_event(
    conn: &mut Connection,
    record: &ProjectRecord,
    event: EventDraft,
) -> Result<EventEnvelope, StorageError> {
    let tx = conn.transaction()?;
    tx.execute(
        &format!("INSERT INTO projects ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5)"),
        params![
            record.id.to_string(),
            record.name,
            record.root_path,
            record.created_at_ms,
            record.updated_at_ms
        ],
    )
    .map_err(|e| {
        StorageError::from_unique(e, || format!("repository {} is already registered", record.root_path))
    })?;
    let stored = events::append(&tx, event)?;
    tx.commit()?;
    Ok(stored)
}

/// Deletes the registration (cascading to dependent rows, never to files) and
/// appends the event built by `event` in a single transaction.
pub fn delete_with_event(
    conn: &mut Connection,
    id: Uuid,
    event: impl FnOnce(&ProjectRecord) -> Result<EventDraft, StorageError>,
) -> Result<(ProjectRecord, EventEnvelope), StorageError> {
    let tx = conn.transaction()?;
    let record = get(&tx, id)?;
    tx.execute("DELETE FROM projects WHERE id = ?1", [id.to_string()])?;
    let stored = events::append(&tx, event(&record)?)?;
    tx.commit()?;
    Ok((record, stored))
}

#[cfg(test)]
mod tests {
    use switchyard_events::{DomainEvent, EventContext, new_id, now_ms};

    use super::*;
    use crate::test_support::temp_storage;

    fn record(path: &str) -> ProjectRecord {
        let now = now_ms();
        ProjectRecord {
            id: new_id(),
            name: "demo".into(),
            root_path: path.into(),
            created_at_ms: now,
            updated_at_ms: now,
        }
    }

    fn registered(r: &ProjectRecord) -> EventDraft {
        EventDraft::new(
            DomainEvent::ProjectRegistered {
                project_id: r.id,
                name: r.name.clone(),
                root_path: r.root_path.clone(),
            },
            EventContext { project_id: Some(r.id), ..Default::default() },
        )
        .unwrap()
    }

    #[test]
    fn insert_list_get_delete() {
        let (_dir, storage) = temp_storage();
        let r = record("/repo/a");
        storage.with_conn(|c| insert_with_event(c, &r, registered(&r))).unwrap();
        assert_eq!(storage.with_conn(|c| list(c)).unwrap(), vec![r.clone()]);
        assert_eq!(storage.with_conn(|c| get(c, r.id)).unwrap(), r);
        assert_eq!(storage.with_conn(|c| find_by_path(c, "/repo/a")).unwrap(), Some(r.clone()));
        assert_eq!(storage.with_conn(|c| count(c)).unwrap(), 1);

        let (removed, ev) = storage
            .with_conn(|c| {
                delete_with_event(c, r.id, |p| {
                    EventDraft::new(
                        DomainEvent::ProjectRemoved { project_id: p.id, root_path: p.root_path.clone() },
                        EventContext::default(),
                    )
                    .map_err(|e| StorageError::Corrupt(e.to_string()))
                })
            })
            .unwrap();
        assert_eq!(removed, r);
        assert_eq!(ev.event_type, "project.removed");
        assert!(matches!(storage.with_conn(|c| get(c, r.id)), Err(StorageError::NotFound(_))));
    }

    #[test]
    fn duplicate_path_conflicts_and_writes_no_event() {
        let (_dir, storage) = temp_storage();
        let a = record("/repo/a");
        storage.with_conn(|c| insert_with_event(c, &a, registered(&a))).unwrap();
        let b = record("/repo/a");
        let result = storage.with_conn(|c| insert_with_event(c, &b, registered(&b)));
        assert!(matches!(result, Err(StorageError::Conflict(_))));
        assert_eq!(storage.with_conn(|c| crate::events::last_seq(c)).unwrap(), Some(1));
    }

    #[test]
    fn deleting_unknown_project_is_not_found() {
        let (_dir, storage) = temp_storage();
        let result = storage.with_conn(|c| {
            delete_with_event(c, new_id(), |_| unreachable!("no record to build an event for"))
        });
        assert!(matches!(result, Err(StorageError::NotFound(_))));
    }
}
