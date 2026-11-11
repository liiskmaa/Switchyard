//! Mapping of internal errors to protocol errors.

use serde_json::json;
use switchyard_events::PublishError;
use switchyard_git::GitError;
use switchyard_protocol::{ErrorCode, ProtocolError};
use switchyard_storage::StorageError;

pub(crate) fn internal(context: &str, err: impl std::fmt::Display) -> ProtocolError {
    tracing::error!(%err, "{context}");
    ProtocolError::new(ErrorCode::Internal, format!("{context}: {err}"))
}

pub(crate) fn storage(err: StorageError) -> ProtocolError {
    match err {
        StorageError::NotFound(what) => ProtocolError::new(ErrorCode::NotFound, format!("{what} not found")),
        StorageError::Conflict(what) => ProtocolError::new(ErrorCode::Conflict, what),
        other => internal("database error", other),
    }
}

pub(crate) fn publish(err: PublishError) -> ProtocolError {
    internal("event journal error", err)
}

/// Maps errors raised while validating a path supplied by the client.
pub(crate) fn git_registration(err: GitError) -> ProtocolError {
    match err {
        e @ (GitError::NotAbsolute(_) | GitError::NotFound(_) | GitError::NotADirectory(_)) => {
            ProtocolError::new(ErrorCode::InvalidPath, e.to_string())
        }
        e @ GitError::PermissionDenied(_) => ProtocolError::new(ErrorCode::PermissionDenied, e.to_string()),
        e @ GitError::NotARepository { .. } => {
            ProtocolError::new(ErrorCode::NotAGitRepository, e.to_string())
        }
        GitError::NotRepositoryRoot { path, root } => ProtocolError::new(
            ErrorCode::NotAGitRepository,
            format!(
                "{} is inside the repository rooted at {}; add the repository root instead",
                path.display(),
                root.display()
            ),
        )
        .with_data(
            json!({ "path": path.display().to_string(), "repository_root": root.display().to_string() }),
        ),
        other => git_operational(other),
    }
}

/// Maps errors raised while inspecting an already registered repository.
pub(crate) fn git_registered(err: GitError) -> ProtocolError {
    match err {
        e @ (GitError::NotFound(_)
        | GitError::NotADirectory(_)
        | GitError::PermissionDenied(_)
        | GitError::NotARepository { .. }) => {
            ProtocolError::new(ErrorCode::RepositoryUnavailable, e.to_string())
        }
        other => git_operational(other),
    }
}

fn git_operational(err: GitError) -> ProtocolError {
    match err {
        e @ GitError::Unavailable { .. } => ProtocolError::new(ErrorCode::GitUnavailable, e.to_string()),
        e @ (GitError::Timeout { .. } | GitError::Failed { .. } | GitError::UnexpectedOutput(_)) => {
            ProtocolError::new(ErrorCode::GitFailed, e.to_string())
        }
        other => internal("repository inspection failed", other),
    }
}
