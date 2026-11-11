//! Data types carried inside method results.

use serde::{Deserialize, Serialize};
use serde_json::Value;
pub use switchyard_agents::{ApprovalStatus, Capabilities, DecisionSource, SessionState};
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

/// Installation status of one agent provider on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAvailability {
    pub provider: String,
    pub display_name: String,
    pub installed: bool,
    pub executable: Option<String>,
    pub version: Option<String>,
    /// Version the adapter was verified against.
    pub tested_version: String,
    /// Why the agent cannot be used, if it cannot.
    pub error: Option<String>,
    /// Login state; never includes credentials or account identity.
    pub auth: Option<AgentAuth>,
    pub capabilities: Capabilities,
    pub checked_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAuth {
    pub logged_in: bool,
    pub method: Option<String>,
    pub provider: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: Uuid,
    pub project_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub agent_id: Uuid,
    pub provider: String,
    /// The agent's own session id (Claude `session_id`), used for resume.
    pub native_session_id: Option<String>,
    pub cwd: String,
    pub state: SessionState,
    pub pid: Option<u32>,
    pub cli_version: Option<String>,
    pub model: Option<String>,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    pub last_activity_at_ms: i64,
    pub exit_code: Option<i32>,
    pub exit_signal: Option<i32>,
    pub failure: Option<String>,
    pub resume_count: u32,
    /// A process is attached and supervised by this Core instance.
    pub attached: bool,
    pub pending_approvals: u32,
    pub capabilities: Capabilities,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Approval {
    pub id: Uuid,
    pub project_id: Uuid,
    pub session_id: Uuid,
    pub agent_id: Option<Uuid>,
    pub tool_name: String,
    pub tool_use_id: Option<String>,
    /// Tool input exactly as the agent requested it.
    pub input: Value,
    pub description: Option<String>,
    pub details: Value,
    pub status: ApprovalStatus,
    pub requested_at_ms: i64,
    pub expires_at_ms: i64,
    pub decided_at_ms: Option<i64>,
    pub decision_source: Option<DecisionSource>,
    pub decided_by: Option<String>,
    pub decision_note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveKind {
    Text,
    Thinking,
    MessageStart,
    MessageStop,
    /// `missed` chunks were dropped because the client fell behind.
    Gap,
}

/// One ephemeral output chunk. `seq` increases per session within one Core
/// run; it resets when Core restarts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveOutput {
    pub session_id: Uuid,
    pub seq: u64,
    pub kind: LiveKind,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missed: Option<u64>,
}
