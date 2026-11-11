use std::path::PathBuf;

/// Fatal errors that prevent Core from starting or cause it to stop.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error(transparent)]
    Paths(#[from] switchyard_protocol::paths::PathError),
    #[error("cannot prepare data directory {path}: {source}")]
    DataDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("another Switchyard Core is already running (lock held on {0})")]
    AlreadyRunning(PathBuf),
    #[error("cannot acquire instance lock {path}: {source}")]
    Lock {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{0} exists and is not a socket; refusing to replace it")]
    SocketPathOccupied(PathBuf),
    #[error("cannot listen on {path}: {source}")]
    Listen {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("database initialization failed: {0}")]
    Storage(#[from] switchyard_storage::StorageError),
    #[error("cannot record lifecycle event: {0}")]
    Event(#[from] switchyard_events::PublishError),
    #[error("Core task failed: {0}")]
    Task(String),
}

impl CoreError {
    /// Process exit code for this error.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::AlreadyRunning(_) => 3,
            Self::Paths(_) => 2,
            _ => 1,
        }
    }
}
