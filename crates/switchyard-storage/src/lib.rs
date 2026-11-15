//! SQLite persistence for Switchyard Core.
//!
//! A single [`Storage`] owns one write connection guarded by a mutex. All
//! operations are synchronous functions over `&Connection`/`&mut Connection`;
//! async callers go through [`Storage::call`], which runs them on Tokio's
//! blocking pool. One connection is sufficient for the current load and keeps
//! write ordering trivial; WAL mode keeps external readers (e.g. `sqlite3`
//! during debugging) from blocking Core.

pub mod agents;
pub mod approvals;
mod error;
pub mod events;
pub mod integrations;
mod migrations;
pub mod organizations;
pub mod policies;
pub mod projects;
pub mod sessions;
pub mod stats;
pub mod tasks;
pub mod workflows;
pub mod workspaces;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::OpenFlags;

pub use approvals::ApprovalRecord;
pub use error::StorageError;
pub use migrations::{LATEST_SCHEMA_VERSION, MIGRATIONS};
pub use projects::ProjectRecord;
/// Re-exported so callers composing transactions need no direct `rusqlite` dependency.
pub use rusqlite::{Connection, Transaction};
pub use sessions::{SessionChanges, SessionRecord};
pub use stats::DatabaseStats;
pub use workspaces::{WorkspaceChanges, WorkspaceRecord};

#[derive(Clone)]
pub struct Storage {
    conn: Arc<Mutex<Connection>>,
    path: PathBuf,
}

impl std::fmt::Debug for Storage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Storage").field("path", &self.path).finish_non_exhaustive()
    }
}

impl Storage {
    /// Opens (creating if needed) the database at `path`, enables WAL and
    /// foreign keys, and applies pending migrations.
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        let mut conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        configure(&conn)?;
        migrations::apply(&mut conn)?;
        tracing::info!(path = %path.display(), schema_version = LATEST_SCHEMA_VERSION, "database ready");
        Ok(Self { conn: Arc::new(Mutex::new(conn)), path: path.to_path_buf() })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Runs `f` with exclusive access to the connection on the blocking pool.
    pub async fn call<R, F>(&self, f: F) -> Result<R, StorageError>
    where
        F: FnOnce(&mut Connection) -> Result<R, StorageError> + Send + 'static,
        R: Send + 'static,
    {
        let storage = self.clone();
        tokio::task::spawn_blocking(move || storage.with_conn(f))
            .await
            .map_err(|e| StorageError::Task(e.to_string()))?
    }

    /// Runs `f` with exclusive access to the connection on the current thread.
    /// Use only from blocking contexts.
    pub fn with_conn<R>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<R, StorageError>,
    ) -> Result<R, StorageError> {
        let mut guard = self.lock()?;
        f(&mut guard)
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, StorageError> {
        self.conn.lock().map_err(|_| StorageError::Poisoned)
    }
}

fn configure(conn: &Connection) -> Result<(), StorageError> {
    conn.busy_timeout(Duration::from_secs(5))?;
    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(StorageError::WalUnavailable(mode));
    }
    // FULL: in WAL mode every commit is fsynced, so an acknowledged event
    // survives power loss. Write volume is low; durability wins.
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.pragma_update(None, "foreign_keys", true)?;
    let fk: bool = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    if !fk {
        return Err(StorageError::Configuration("foreign keys could not be enabled".into()));
    }
    Ok(())
}

pub(crate) fn parse_uuid(value: &str, column: &'static str) -> Result<uuid::Uuid, StorageError> {
    uuid::Uuid::parse_str(value)
        .map_err(|e| StorageError::Corrupt(format!("invalid UUID in {column}: {value:?} ({e})")))
}

pub(crate) fn parse_enum<T: std::str::FromStr>(value: &str, column: &'static str) -> Result<T, StorageError> {
    value.parse().map_err(|_| StorageError::Corrupt(format!("invalid value in {column}: {value:?}")))
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub fn temp_storage() -> (tempfile::TempDir, Storage) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("test.db")).unwrap();
        (dir, storage)
    }

    pub fn insert_project(storage: &Storage, path: &str) -> uuid::Uuid {
        let id = switchyard_events::new_id();
        storage
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO projects (id, name, root_path, created_at, updated_at) VALUES (?1, 'p', ?2, 1, 1)",
                    [id.to_string(), path.to_owned()],
                )?;
                Ok(())
            })
            .unwrap();
        id
    }
}
