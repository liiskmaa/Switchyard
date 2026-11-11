//! Live database health information.

use rusqlite::Connection;

use crate::{StorageError, events, migrations, projects};

/// Session statuses that count as "active" for health reporting.
pub const ACTIVE_SESSION_STATUSES: &str = "'starting', 'active', 'idle'";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseStats {
    pub journal_mode: String,
    pub schema_version: u32,
    pub projects: u64,
    pub active_sessions: u64,
    pub last_event_seq: Option<i64>,
}

/// Queries the live database. Any failure means the database is unhealthy.
pub fn collect(conn: &Connection) -> Result<DatabaseStats, StorageError> {
    Ok(DatabaseStats {
        journal_mode: conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?,
        schema_version: migrations::current_version(conn)?,
        projects: projects::count(conn)?,
        active_sessions: projects::count_query(
            conn,
            &format!("SELECT COUNT(*) FROM sessions WHERE status IN ({ACTIVE_SESSION_STATUSES})"),
        )?,
        last_event_seq: events::last_seq(conn)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_storage;

    #[test]
    fn fresh_database_reports_wal_and_zero_counts() {
        let (_dir, storage) = temp_storage();
        let stats = storage.with_conn(|c| collect(c)).unwrap();
        assert_eq!(stats.journal_mode, "wal");
        assert_eq!(stats.schema_version, crate::LATEST_SCHEMA_VERSION);
        assert_eq!((stats.projects, stats.active_sessions, stats.last_event_seq), (0, 0, None));
    }
}
