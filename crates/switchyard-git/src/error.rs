use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("path must be absolute: {0}")]
    NotAbsolute(PathBuf),
    #[error("path does not exist: {0}")]
    NotFound(PathBuf),
    #[error("path is not a directory: {0}")]
    NotADirectory(PathBuf),
    #[error("permission denied: {0}")]
    PermissionDenied(PathBuf),
    #[error("not a Git working tree: {path} ({reason})")]
    NotARepository { path: PathBuf, reason: String },
    #[error("{path} is inside the Git repository rooted at {root}; register the repository root")]
    NotRepositoryRoot { path: PathBuf, root: PathBuf },
    #[error("cannot run git executable {binary:?}: {source}")]
    Unavailable {
        binary: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("git {args} timed out after {timeout_ms} ms")]
    Timeout { args: String, timeout_ms: u128 },
    #[error("git {args} failed ({status}): {stderr}")]
    Failed { args: String, status: String, stderr: String },
    #[error("unexpected git output: {0}")]
    UnexpectedOutput(String),
    #[error("i/o error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}
