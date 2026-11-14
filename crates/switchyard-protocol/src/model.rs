//! Data types carried inside method results.

use serde::{Deserialize, Serialize};
use serde_json::Value;
pub use switchyard_agents::{ApprovalStatus, Capabilities, DecisionSource, SessionAccess, SessionState};

fn default_access() -> SessionAccess {
    SessionAccess::Write
}
pub use switchyard_policy::{
    Conditions as PolicyConditions, Decision as PolicyDecision, OperationCategory,
    ReasonCode as PolicyReason, Restriction as PolicyRestriction, RuleRef as PolicyRuleRef, RuleScope,
};
pub use switchyard_workspaces::{CleanupFinding, CleanupReport, WorkspaceKind, WorkspaceStatus};
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
    /// `None` only for records that predate workspaces entirely.
    pub workspace_id: Option<Uuid>,
    /// Display name of the workspace (for labels; additive in v1).
    #[serde(default)]
    pub workspace_name: Option<String>,
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
    /// `review` sessions cannot change files (ADR 0027).
    #[serde(default = "default_access")]
    pub access: SessionAccess,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Approval {
    pub id: Uuid,
    pub project_id: Uuid,
    pub session_id: Uuid,
    /// Workspace of the requesting session (additive in v1).
    #[serde(default)]
    pub workspace_id: Option<Uuid>,
    #[serde(default)]
    pub workspace_name: Option<String>,
    pub agent_id: Option<Uuid>,
    /// The session's agent: `claude_code` or `codex`.
    pub provider: String,
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
    /// The policy evaluation that asked for this decision. `None` for
    /// approvals recorded before policies existed.
    #[serde(default)]
    pub policy: Option<ApprovalPolicy>,
}

/// Why an approval was created (an `ask` evaluation).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalPolicy {
    pub evaluation_id: Uuid,
    pub category: OperationCategory,
    pub reason: PolicyReason,
    pub explanation: String,
    pub deciding_rule: Option<PolicyRuleRef>,
    pub policy_revision: i64,
    /// What approving would reach, as Core established it before asking.
    /// Absent for approvals recorded before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<ApprovalScope>,
}

/// The reach of an approval, so a person never mistakes it for a
/// workspace-limited action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalScope {
    /// `workspace` (every target resolved inside the workspace),
    /// `outside_workspace`, `unknown` (a target cannot be resolved, or the
    /// operation has no file targets) or `unrestricted` (a shell command).
    pub kind: String,
    /// Targets outside the workspace.
    #[serde(default)]
    pub outside_paths: Vec<String>,
    /// Targets whose location could not be established.
    #[serde(default)]
    pub unresolved_paths: Vec<String>,
    /// One or two sentences for the person deciding.
    pub note: String,
}

/// A configured policy rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyRule {
    pub id: Uuid,
    pub project_id: Uuid,
    /// `None`: applies to every workspace of the project.
    pub workspace_id: Option<Uuid>,
    pub workspace_name: Option<String>,
    pub scope: RuleScope,
    pub category: OperationCategory,
    pub decision: PolicyDecision,
    pub conditions: PolicyConditions,
    pub description: String,
    pub enabled: bool,
    pub archived: bool,
    /// Increments on every change; pass it back as `expected_revision`.
    pub revision: u32,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub archived_at_ms: Option<i64>,
}

/// One entry of a rule's append-only history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyRuleRevision {
    pub revision: u32,
    /// Global policy revision this change created.
    pub policy_revision: i64,
    /// `created`, `updated`, `enabled`, `disabled` or `archived`.
    pub change: String,
    /// The rule as it was after this change.
    pub rule: PolicyRule,
    pub changed_at_ms: i64,
    pub changed_by: Option<String>,
}

/// What policies can do with one operation category.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyCategoryInfo {
    pub category: OperationCategory,
    /// Built-in default when no rule matches (before system caps).
    pub default_decision: PolicyDecision,
    pub configurable: bool,
    pub allow_supported: bool,
    pub path_condition: bool,
    pub command_condition: bool,
}

/// Everything that applies to requests in a project or one workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectivePolicy {
    pub project_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub policy_revision: i64,
    pub system_version: u32,
    /// Always enforced; not configurable.
    pub system_restrictions: Vec<PolicyRestriction>,
    pub categories: Vec<PolicyCategoryInfo>,
    /// Active project rules (inherited by every workspace).
    pub project_rules: Vec<PolicyRule>,
    /// Active rules of `workspace_id` only (empty without a workspace).
    pub workspace_rules: Vec<PolicyRule>,
}

/// One recorded policy evaluation (audit trail).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolicyEvaluation {
    pub id: Uuid,
    pub evaluated_at_ms: i64,
    /// `request`, `reevaluation` or `decision_check`.
    pub trigger: String,
    pub project_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub workspace_name: Option<String>,
    pub session_id: Uuid,
    pub adapter: String,
    pub tool_name: String,
    pub category: OperationCategory,
    /// What was evaluated: `{operation: {...}, paths: [...]}` (never file
    /// contents).
    pub input: Value,
    pub decision: PolicyDecision,
    pub reason: PolicyReason,
    pub explanation: String,
    pub deciding_rule: Option<PolicyRuleRef>,
    pub matched_rules: Vec<PolicyRuleRef>,
    pub restrictions: Vec<PolicyRestriction>,
    pub policy_revision: i64,
    pub system_version: u32,
    /// The approval created (ask) or settled (policy denial).
    pub approval_id: Option<Uuid>,
    /// Current status of that approval.
    pub approval_status: Option<ApprovalStatus>,
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

/// An isolated working copy of a project (a Core-owned Git worktree), or a
/// legacy record of the project checkout from before isolation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: Uuid,
    pub project_id: Uuid,
    pub kind: WorkspaceKind,
    pub name: String,
    /// Absolute working directory; the agent's cwd.
    pub path: String,
    /// `switchyard/<id>`; `None` for legacy records.
    pub branch: Option<String>,
    pub base_ref: Option<String>,
    pub base_commit: Option<String>,
    pub status: WorkspaceStatus,
    /// Why the workspace is in its status (failure reason, legacy note).
    pub status_reason: Option<String>,
    /// Last observed Git status; `None` until first checked.
    pub dirty: Option<bool>,
    pub git_checked_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub last_activity_at_ms: i64,
    pub archived_at_ms: Option<i64>,
    /// The live session attached to this workspace, if any.
    pub active_session_id: Option<Uuid>,
    /// Whether the directory existed when this record was produced.
    pub path_exists: bool,
}

/// Git status of one workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceGitStatus {
    pub workspace_id: Uuid,
    /// Branch checked out, `None` when detached.
    pub branch: Option<String>,
    /// The branch the workspace record expects.
    pub expected_branch: Option<String>,
    pub on_expected_branch: bool,
    pub head_oid: Option<String>,
    pub detached: bool,
    pub base_commit: Option<String>,
    /// Commits on HEAD since the base commit.
    pub commits_since_base: Option<u32>,
    pub staged: u32,
    pub unstaged: u32,
    pub untracked: u32,
    pub conflicted: u32,
    pub clean: bool,
    pub checked_at_ms: i64,
}

// --- tasks (M6, ADRs 0023-0027) ---------------------------------------------

pub use switchyard_tasks::{
    AssignmentStatus, MessageKind, Priority, ProposalStatus, ReviewResult, Role, Severity, TaskKind,
    TaskStatus,
};

/// A unit of engineering work, independent of any agent session (ADR 0023).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: Uuid,
    pub project_id: Uuid,
    pub parent_task_id: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub kind: TaskKind,
    pub priority: Priority,
    pub acceptance_criteria: Vec<String>,
    pub status: TaskStatus,
    pub status_reason: Option<String>,
    /// What the operator accepted on completion (`summary`, `report`, `note`).
    pub result: Option<serde_json::Value>,
    pub failure: Option<String>,
    /// `operator` or `proposal`.
    pub origin: String,
    pub origin_proposal_id: Option<Uuid>,
    /// Increases on every change; edits must name the revision they saw.
    pub revision: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub completed_at_ms: Option<i64>,
    /// The pending or active assignment, if any.
    pub open_assignment: Option<TaskAssignment>,
    pub dependency_ids: Vec<Uuid>,
    pub pending_proposals: u32,
    pub open_findings: u32,
}

/// One agent's piece of work on a task, run by one session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskAssignment {
    pub id: Uuid,
    pub task_id: Uuid,
    pub role: Role,
    /// `claude_code` or `codex`.
    pub provider: String,
    pub workspace_id: Uuid,
    pub session_id: Option<Uuid>,
    pub status: AssignmentStatus,
    /// The task context exactly as sent to the agent.
    pub context: Option<serde_json::Value>,
    pub context_version: Option<u32>,
    pub outcome: Option<String>,
    pub created_at_ms: i64,
    pub started_at_ms: Option<i64>,
    pub ended_at_ms: Option<i64>,
}

/// A structured, durable message about a task (ADR 0025). Messages from an
/// agent are claims or requests; none of them authorizes anything.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskMessage {
    pub id: Uuid,
    pub task_id: Uuid,
    pub assignment_id: Option<Uuid>,
    pub kind: MessageKind,
    /// `agent`, `operator` or `core`.
    pub sender: String,
    pub sender_session_id: Option<Uuid>,
    /// `operator`, `agent` or `task`.
    pub recipient: String,
    pub payload: serde_json::Value,
    pub causation_id: Option<String>,
    /// `recorded`, `pending` (waits for the next agent turn) or `delivered`.
    pub delivery: String,
    pub delivered_at_ms: Option<i64>,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskFinding {
    pub id: Uuid,
    pub task_id: Uuid,
    pub message_id: Uuid,
    pub assignment_id: Option<Uuid>,
    pub severity: Severity,
    pub title: String,
    pub explanation: Option<String>,
    pub suggestion: Option<String>,
    pub path: Option<String>,
    pub line: Option<u32>,
    /// `open`, `resolved` or `dismissed`.
    pub status: String,
    pub created_at_ms: i64,
    pub resolved_at_ms: Option<i64>,
}

/// A subtask an agent proposed; only the operator can turn it into a task (ADR 0026).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskProposal {
    pub id: Uuid,
    pub task_id: Uuid,
    pub message_id: Uuid,
    pub position: u32,
    pub proposal: serde_json::Value,
    pub status: ProposalStatus,
    pub decided_by: Option<String>,
    pub decision_note: Option<String>,
    pub created_task_id: Option<Uuid>,
    pub created_at_ms: i64,
    pub decided_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRef {
    pub id: Uuid,
    pub title: String,
    pub status: TaskStatus,
}

/// `satisfied`, `waiting` or `broken` (a dependency failed or was cancelled).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskReadiness {
    Satisfied,
    Waiting,
    Broken,
}
