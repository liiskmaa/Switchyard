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
    /// `operator` or `workflow` (M8.1).
    #[serde(default = "operator_origin")]
    pub origin: String,
    /// The workflow step, e.g. `implement#2`.
    #[serde(default)]
    pub step_key: Option<String>,
    /// The model asked for; `None` = the CLI's default.
    #[serde(default)]
    pub model: Option<String>,
    /// The model the CLI reported it runs (from its session).
    #[serde(default)]
    pub actual_model: Option<String>,
}

fn operator_origin() -> String {
    "operator".into()
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

// --- integrations (M7, ADRs 0028-0031) --------------------------------------

pub use switchyard_integrations::{Blocker, IntegrationStatus, RunStatus, StepStatus};

/// A Core-owned operation on an integration (ADR 0032). It belongs to Core,
/// not to the request that started it: a client that disconnects or
/// reconnects sees it here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationOperation {
    pub id: Uuid,
    /// `approve`, `commit_workspace`, `update_from_target`, `abort_resolution`, `validate`.
    pub kind: String,
    /// `running`, `completed`, `failed` or `interrupted`.
    pub status: String,
    pub requested_by: String,
    pub error: Option<String>,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
}

/// The proposed delivery of one workspace's changes into a target branch,
/// bound to an exact source and target commit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Integration {
    pub id: Uuid,
    pub project_id: Uuid,
    pub task_id: Option<Uuid>,
    pub workspace_id: Uuid,
    pub workspace_name: Option<String>,
    pub source_branch: String,
    /// The exact source commit everything was checked against.
    pub source_commit: String,
    pub target_branch: String,
    /// The exact target commit everything was checked against.
    pub target_commit: String,
    pub base_commit: Option<String>,
    pub status: IntegrationStatus,
    pub status_reason: Option<String>,
    /// What prevents approval now; empty when ready.
    pub blockers: Vec<Blocker>,
    /// The facts of the latest preparation, as Core read them from Git.
    pub preflight: serde_json::Value,
    pub prepared_at_ms: i64,
    pub approved_by: Option<String>,
    pub approved_at_ms: Option<i64>,
    /// The merge commit Core created (set when integrating starts).
    pub merge_commit: Option<String>,
    /// `ref_update` or `checkout_fast_forward`.
    pub method: Option<String>,
    pub failure: Option<String>,
    pub revision: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub completed_at_ms: Option<i64>,
    /// The latest Core-owned operation on this integration (ADR 0032):
    /// running, or how the last one ended. `None` if there was none.
    #[serde(default)]
    pub operation: Option<IntegrationOperation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitSummary {
    pub oid: String,
    pub parents: Vec<String>,
    pub author_name: String,
    pub author_email: String,
    pub authored_at_ms: i64,
    pub subject: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFileInfo {
    pub path: String,
    pub old_path: Option<String>,
    /// `added`, `modified`, `deleted`, `renamed`, `copied`, `type_changed`, `unmerged`, `unknown`.
    pub kind: String,
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
    pub binary: bool,
}

/// What an integration delivers, read from Git.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationChanges {
    pub source_commit: String,
    pub target_commit: String,
    /// The common ancestor the changes are measured from.
    pub base_commit: Option<String>,
    /// The target contains commits the source lacks.
    pub diverged: bool,
    pub commits: Vec<CommitSummary>,
    pub commits_total: u32,
    pub files: Vec<ChangedFileInfo>,
    pub files_total: u64,
    pub additions: u64,
    pub deletions: u64,
    pub truncated: bool,
    /// Uncommitted changes in the workspace (not part of the source commit).
    pub uncommitted: Vec<ChangedFileInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationCommandConfig {
    pub id: Uuid,
    pub name: String,
    pub argv: Vec<String>,
    /// For display and policy matching; never passed to a shell.
    pub command_line: String,
    pub timeout_secs: u32,
    pub enabled: bool,
    pub position: i64,
}

/// What the policy says about one configured command, before running it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationPlanEntry {
    pub command_id: Uuid,
    pub name: String,
    pub command_line: String,
    /// `allow`: runs; `ask`: runs only if you authorize it; `deny`: never runs.
    pub policy_decision: String,
    pub policy_explanation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationStepInfo {
    pub id: Uuid,
    pub position: i64,
    pub name: String,
    pub command_line: String,
    pub cwd: String,
    pub timeout_secs: u32,
    pub policy_decision: String,
    pub policy_explanation: String,
    pub authorized_by: Option<String>,
    pub status: StepStatus,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub detail: Option<String>,
    pub started_at_ms: Option<i64>,
    pub ended_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationRunInfo {
    pub id: Uuid,
    /// The exact commit validated; results for another commit are stale.
    pub source_commit: String,
    pub status: RunStatus,
    pub requested_by: Option<String>,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    pub steps: Vec<ValidationStepInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationSettingsInfo {
    pub project_id: Uuid,
    pub require_review: bool,
    pub revision: i64,
    pub commands: Vec<ValidationCommandConfig>,
}

// --- workflows (M8.1, ADR 0033) ---------------------------------------------------

pub use switchyard_agents::{ModelCatalog, ModelOption};

/// The provider and model one workflow role runs with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleBinding {
    /// `claude_code` or `codex`.
    pub provider: String,
    /// Passed to the CLI exactly; `None` = the CLI's own default.
    #[serde(default)]
    pub model: Option<String>,
}

/// Provider and model per workflow role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleBindings {
    pub investigate: RoleBinding,
    pub implement: RoleBinding,
    pub review: RoleBinding,
    /// Corrections; `None` = the same as `implement`.
    #[serde(default)]
    pub correct: Option<RoleBinding>,
}

/// Task-level overrides of the project's bindings (only the roles given).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleBindingsPatch {
    #[serde(default)]
    pub investigate: Option<RoleBinding>,
    #[serde(default)]
    pub implement: Option<RoleBinding>,
    #[serde(default)]
    pub review: Option<RoleBinding>,
    #[serde(default)]
    pub correct: Option<RoleBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowSettingsInfo {
    pub project_id: Uuid,
    pub bindings: RoleBindings,
    /// 0 while the project uses Switchyard's defaults.
    pub revision: i64,
    /// For example: implementation and review use the same provider.
    pub warnings: Vec<String>,
}

/// A task's workflow run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowInfo {
    pub task_id: Uuid,
    /// `engineering`.
    pub definition: String,
    pub version: u32,
    /// `pending`, `running`, `waiting`, `paused`, `completed`, `cancelled`, `failed`.
    pub status: String,
    /// `investigation`, `implementation`, `review`, `correction`, `delivery`.
    pub stage: Option<String>,
    pub step_key: Option<String>,
    /// Why it waits, stopped or finished.
    pub reason: Option<String>,
    /// What resuming does, when it waits or is paused.
    pub resume_action: Option<String>,
    /// The bindings snapshotted when it started.
    pub bindings: RoleBindings,
    pub workspace_id: Option<Uuid>,
    /// Correction rounds so far, and the automatic limit.
    pub corrections: u32,
    pub max_corrections: u32,
    pub warnings: Vec<String>,
    pub started_by: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub ended_at_ms: Option<i64>,
}
