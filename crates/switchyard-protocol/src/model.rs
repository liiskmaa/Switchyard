//! Data types carried inside method results.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreInfo {
    pub version: String,
    /// Changes every time Core starts. Lets clients detect a Core restart
    /// across reconnections.
    pub instance_id: Uuid,
    pub started_at_ms: i64,
    pub pid: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    Ok,
    Degraded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseHealth {
    /// Migrations applied and the database answered a live query.
    pub initialized: bool,
    pub path: String,
    pub journal_mode: Option<String>,
    pub schema_version: Option<u32>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthReport {
    pub status: HealthStatus,
    pub core: CoreInfo,
    pub protocol_version: u32,
    pub uptime_ms: i64,
    pub database: DatabaseHealth,
    /// `None` when the database query failed (see `database.error`).
    pub projects_count: Option<u64>,
    pub active_sessions_count: Option<u64>,
    pub last_event_seq: Option<i64>,
    pub connected_clients: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    /// Canonical absolute path of the repository's working tree root.
    pub root_path: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    /// Whether the path existed and was a readable directory when this
    /// record was produced. Registration is kept either way.
    pub accessible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStatusReport {
    pub project_id: Uuid,
    /// Current branch, `None` when HEAD is detached.
    pub branch: Option<String>,
    /// `None` on an unborn branch (no commits yet).
    pub head_oid: Option<String>,
    pub detached: bool,
    pub upstream: Option<String>,
    pub ahead: Option<u32>,
    pub behind: Option<u32>,
    /// Paths with staged changes.
    pub staged: u32,
    /// Tracked paths with unstaged changes.
    pub unstaged: u32,
    pub untracked: u32,
    pub conflicted: u32,
    pub clean: bool,
    pub checked_at_ms: i64,
}
