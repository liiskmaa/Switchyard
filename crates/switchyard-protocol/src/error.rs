use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Machine-readable error categories. Clients branch on these, never on the
/// human-readable message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The frame was not valid JSON or not a recognised frame shape.
    ParseError,
    /// A frame exceeded the maximum size.
    FrameTooLarge,
    /// A request other than `initialize` arrived before initialization.
    NotInitialized,
    /// `initialize` was sent twice on one connection.
    AlreadyInitialized,
    /// No protocol version in common.
    UnsupportedProtocolVersion,
    MethodNotFound,
    InvalidParams,
    NotFound,
    /// The resource already exists (e.g. repository already registered).
    Conflict,
    /// A filesystem path was rejected (not absolute, missing, not a directory).
    InvalidPath,
    /// The path exists but cannot be read by Core.
    PermissionDenied,
    /// The path is not the root of a Git working tree.
    NotAGitRepository,
    /// A registered repository is no longer present or readable.
    RepositoryUnavailable,
    /// The `git` executable could not be run.
    GitUnavailable,
    /// A `git` invocation failed or timed out.
    GitFailed,
    /// The agent CLI is missing, unusable or unsupported.
    AgentUnavailable,
    /// The operation is not valid in the session's current state (e.g.
    /// sending to a running session without `queue_if_busy`).
    InvalidState,
    /// The agent does not support the operation (see its capabilities).
    Unsupported,
    /// The agent process failed to start or to accept the command.
    AgentFailed,
    /// Core is shutting down and will not accept new work.
    ShuttingDown,
    /// Core is temporarily unable to accept work.
    Overloaded,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct ProtocolError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl ProtocolError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), data: None }
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}
