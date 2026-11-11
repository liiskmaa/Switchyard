#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("migration {version} failed: {source}")]
    Migration {
        version: u32,
        #[source]
        source: rusqlite::Error,
    },
    #[error(
        "database schema version {found} is newer than this build supports ({supported}); \
         upgrade Switchyard"
    )]
    UnsupportedSchema { found: u32, supported: u32 },
    #[error("SQLite refused WAL mode (journal_mode = {0})")]
    WalUnavailable(String),
    #[error("database configuration failed: {0}")]
    Configuration(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("stored data is invalid: {0}")]
    Corrupt(String),
    #[error("database connection mutex poisoned by an earlier panic")]
    Poisoned,
    #[error("storage task failed: {0}")]
    Task(String),
}

impl StorageError {
    /// Maps SQLite UNIQUE violations to [`StorageError::Conflict`].
    pub(crate) fn from_unique(err: rusqlite::Error, what: impl FnOnce() -> String) -> Self {
        match &err {
            rusqlite::Error::SqliteFailure(e, _)
                if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
                    || e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY =>
            {
                Self::Conflict(what())
            }
            _ => Self::Sqlite(err),
        }
    }
}
