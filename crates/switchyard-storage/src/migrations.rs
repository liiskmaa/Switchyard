//! Forward-only migrations tracked with `PRAGMA user_version`.
//!
//! Migration files live in the repository's top-level `migrations/` directory
//! and are embedded at compile time. Each migration runs in its own
//! transaction together with the version bump.

use rusqlite::Connection;

use crate::error::StorageError;

/// `(version, sql)` in ascending order. Versions are contiguous from 1.
pub const MIGRATIONS: &[(u32, &str)] = &[
    (1, include_str!("../../../migrations/0001_initial.sql")),
    (2, include_str!("../../../migrations/0002_agent_sessions.sql")),
];

pub const LATEST_SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

pub(crate) fn current_version(conn: &Connection) -> Result<u32, StorageError> {
    Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

pub(crate) fn apply(conn: &mut Connection) -> Result<(), StorageError> {
    let current = current_version(conn)?;
    if current > LATEST_SCHEMA_VERSION {
        return Err(StorageError::UnsupportedSchema { found: current, supported: LATEST_SCHEMA_VERSION });
    }
    for &(version, sql) in MIGRATIONS.iter().filter(|(v, _)| *v > current) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql).map_err(|source| StorageError::Migration { version, source })?;
        tx.pragma_update(None, "user_version", version)
            .map_err(|source| StorageError::Migration { version, source })?;
        tx.commit().map_err(|source| StorageError::Migration { version, source })?;
        tracing::info!(version, "applied database migration");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Storage;

    #[test]
    fn versions_are_contiguous() {
        for (i, (v, _)) in MIGRATIONS.iter().enumerate() {
            assert_eq!(*v as usize, i + 1);
        }
    }

    #[test]
    fn reopening_is_idempotent_and_creates_all_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db");
        drop(Storage::open(&path).unwrap());
        let storage = Storage::open(&path).unwrap();
        let tables: Vec<String> = storage
            .with_conn(|c| {
                let mut stmt = c.prepare(
                    "SELECT name FROM sqlite_master WHERE type='table' \
                     AND name NOT LIKE 'sqlite_%' ORDER BY name",
                )?;
                let rows = stmt.query_map([], |r| r.get(0))?;
                Ok(rows.collect::<Result<_, _>>()?)
            })
            .unwrap();
        assert_eq!(
            tables,
            [
                "agents",
                "approvals",
                "decisions",
                "events",
                "findings",
                "messages",
                "permissions",
                "projects",
                "sessions",
                "tasks",
                "workspaces"
            ]
        );
        let version = storage.with_conn(|c| current_version(c)).unwrap();
        assert_eq!(version, LATEST_SCHEMA_VERSION);
    }

    #[test]
    fn newer_schema_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db");
        {
            let c = Connection::open(&path).unwrap();
            c.pragma_update(None, "user_version", LATEST_SCHEMA_VERSION + 1).unwrap();
        }
        assert!(matches!(Storage::open(&path), Err(StorageError::UnsupportedSchema { .. })));
    }
}
