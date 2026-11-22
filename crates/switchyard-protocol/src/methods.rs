//! Request methods. Each method is a zero-sized type implementing [`Method`],
//! binding its wire name to its params and result types.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use switchyard_events::EventEnvelope;
use uuid::Uuid;

use crate::model::{
    AgentAvailability, Approval, ApprovalStatus, CleanupReport, ClientInfo, CoreInfo, EffectivePolicy,
    GitStatusReport, HealthReport, OperationCategory, PolicyConditions, PolicyDecision, PolicyEvaluation,
    PolicyRule, PolicyRuleRevision, PolicySettings, Priority, Project, Role, Session, Task, TaskAssignment,
    TaskFinding, TaskKind, TaskMessage, TaskProposal, TaskReadiness, TaskRef, Workspace, WorkspaceGitStatus,
};

use crate::model::{
    Integration, IntegrationChanges, IntegrationSettingsInfo, ValidationPlanEntry, ValidationRunInfo,
};
use crate::model::{
    ModelCatalog, OrgSettingsInfo, OrganizationInfo, RoleBinding, RoleBindings, RoleBindingsPatch,
    WorkflowInfo, WorkflowSettingsInfo,
};

pub trait Method {
    const NAME: &'static str;
    type Params: Serialize + DeserializeOwned + Send + 'static;
    type Result: Serialize + DeserializeOwned + Send + 'static;
}

/// Params/result for methods that carry no data.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Empty {}

macro_rules! method {
    ($ty:ident, $name:literal, $params:ty, $result:ty) => {
        #[derive(Debug, Clone, Copy)]
        pub struct $ty;
        impl Method for $ty {
            const NAME: &'static str = $name;
            type Params = $params;
            type Result = $result;
        }
    };
}

// --- initialize -------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitializeParams {
    /// Highest protocol version the client speaks.
    pub protocol_version: u32,
    pub client: ClientInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitializeResult {
    /// Version selected for this connection.
    pub protocol_version: u32,
    pub core: CoreInfo,
}

method!(Initialize, "initialize", InitializeParams, InitializeResult);

// --- system -----------------------------------------------------------------

method!(SystemHealth, "system.health", Empty, HealthReport);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShutdownParams {
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShutdownResult {
    pub accepted: bool,
}

method!(SystemShutdown, "system.shutdown", ShutdownParams, ShutdownResult);

// --- projects ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectsListResult {
    pub projects: Vec<Project>,
}

method!(ProjectsList, "projects.list", Empty, ProjectsListResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectsAddParams {
    /// Absolute path to the root of an existing Git working tree.
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectResult {
    pub project: Project,
}

method!(ProjectsAdd, "projects.add", ProjectsAddParams, ProjectResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectIdParams {
    pub project_id: Uuid,
}

method!(ProjectsGet, "projects.get", ProjectIdParams, ProjectResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectsRemoveResult {
    pub project_id: Uuid,
}

method!(ProjectsRemove, "projects.remove", ProjectIdParams, ProjectsRemoveResult);
method!(ProjectsGitStatus, "projects.git_status", ProjectIdParams, GitStatusReport);

// --- events -----------------------------------------------------------------

/// Upper bound for `events.list` page size.
pub const MAX_EVENTS_PAGE: u32 = 1000;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventsListParams {
    #[serde(default)]
    pub after_seq: Option<i64>,
    #[serde(default)]
    pub project_id: Option<Uuid>,
    #[serde(default)]
    pub session_id: Option<Uuid>,
    /// Events about one task (since protocol 4, M6).
    #[serde(default)]
    pub task_id: Option<Uuid>,
    #[serde(default)]
    pub event_types: Vec<String>,
    /// Defaults to 100, capped at [`MAX_EVENTS_PAGE`].
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventsListResult {
    pub events: Vec<EventEnvelope>,
}

method!(EventsList, "events.list", EventsListParams, EventsListResult);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventsSubscribeParams {
    /// Replay stored events with `seq > after_seq` before going live.
    /// Omit to receive only events published after subscribing.
    #[serde(default)]
    pub after_seq: Option<i64>,
    #[serde(default)]
    pub project_id: Option<Uuid>,
    #[serde(default)]
    pub event_types: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventsSubscribeResult {
    pub subscription_id: Uuid,
}

method!(EventsSubscribe, "events.subscribe", EventsSubscribeParams, EventsSubscribeResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventsUnsubscribeParams {
    pub subscription_id: Uuid,
}

method!(EventsUnsubscribe, "events.unsubscribe", EventsUnsubscribeParams, Empty);

// --- agents -----------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentsListParams {
    /// Re-run detection instead of returning the cached result.
    #[serde(default)]
    pub refresh: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsListResult {
    pub agents: Vec<AgentAvailability>,
}

method!(AgentsList, "agents.list", AgentsListParams, AgentsListResult);

// --- workspaces -------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspacesListParams {
    pub project_id: Uuid,
    /// Also return archived, cleaned-up and legacy records.
    #[serde(default)]
    pub include_archived: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspacesListResult {
    pub workspaces: Vec<Workspace>,
}

method!(WorkspacesList, "workspaces.list", WorkspacesListParams, WorkspacesListResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceIdParams {
    pub workspace_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceDetail {
    pub workspace: Workspace,
    /// Sessions that ran in this workspace, newest first.
    pub sessions: Vec<Session>,
}

method!(WorkspacesGet, "workspaces.get", WorkspaceIdParams, WorkspaceDetail);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspacesCreateParams {
    pub project_id: Uuid,
    /// Display label; defaults to "Workspace N".
    #[serde(default)]
    pub name: Option<String>,
    /// Revision to branch from; defaults to the project's `HEAD`.
    #[serde(default)]
    pub base_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceResult {
    pub workspace: Workspace,
}

method!(WorkspacesCreate, "workspaces.create", WorkspacesCreateParams, WorkspaceResult);
method!(WorkspacesGitStatus, "workspaces.git_status", WorkspaceIdParams, WorkspaceGitStatus);
method!(WorkspacesVerify, "workspaces.verify", WorkspaceIdParams, WorkspaceResult);
method!(WorkspacesArchive, "workspaces.archive", WorkspaceIdParams, WorkspaceResult);
method!(WorkspacesRestore, "workspaces.restore", WorkspaceIdParams, WorkspaceResult);
method!(WorkspacesComplete, "workspaces.complete", WorkspaceIdParams, WorkspaceResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceCleanupResult {
    pub workspace: Workspace,
    pub report: CleanupReport,
}

method!(WorkspacesCleanupCheck, "workspaces.cleanup_check", WorkspaceIdParams, WorkspaceCleanupResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspacesCleanupParams {
    pub workspace_id: Uuid,
    /// `report.confirmation` from the `workspaces.cleanup_check` the
    /// operator reviewed. Cleanup is refused if the state changed since.
    pub confirmation: String,
}

method!(WorkspacesCleanup, "workspaces.cleanup", WorkspacesCleanupParams, WorkspaceCleanupResult);

// --- sessions ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionsStartParams {
    /// The workspace the agent runs in (its cwd). Required: sessions never
    /// run in the project checkout.
    pub workspace_id: Uuid,
    /// Optional cross-check: fails with `workspace_project_mismatch` when the
    /// workspace belongs to another project.
    #[serde(default)]
    pub project_id: Option<Uuid>,
    /// `claude_code` (default) or `codex`.
    #[serde(default)]
    pub provider: Option<String>,
    /// First task; sent once the agent is ready.
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionResult {
    pub session: Session,
}

method!(SessionsStart, "sessions.start", SessionsStartParams, SessionResult);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionsListParams {
    #[serde(default)]
    pub project_id: Option<Uuid>,
    #[serde(default)]
    pub workspace_id: Option<Uuid>,
    #[serde(default)]
    pub live_only: bool,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionsListResult {
    pub sessions: Vec<Session>,
}

method!(SessionsList, "sessions.list", SessionsListParams, SessionsListResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionIdParams {
    pub session_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionDetail {
    pub session: Session,
    /// Assistant text of the message currently being streamed (ephemeral;
    /// empty when nothing is streaming). Lets a reconnecting client catch up
    /// on in-progress output without the journal storing tokens.
    pub live_text: String,
    /// Live sequence number the snapshot corresponds to.
    pub live_seq: u64,
}

method!(SessionsGet, "sessions.get", SessionIdParams, SessionDetail);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionsSendParams {
    pub session_id: Uuid,
    pub text: String,
    /// When a turn is running: `true` queues the message as the next turn
    /// (if the agent supports it); `false` fails with `invalid_state`.
    #[serde(default)]
    pub queue_if_busy: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// The message started a new turn.
    StartedTurn,
    /// A turn was running; the agent will process the message next.
    QueuedForNextTurn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionsSendResult {
    pub delivery: Delivery,
}

method!(SessionsSend, "sessions.send", SessionsSendParams, SessionsSendResult);
method!(SessionsInterrupt, "sessions.interrupt", SessionIdParams, SessionResult);
method!(SessionsStop, "sessions.stop", SessionIdParams, SessionResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionsResumeParams {
    pub session_id: Uuid,
    #[serde(default)]
    pub prompt: Option<String>,
}

method!(SessionsResume, "sessions.resume", SessionsResumeParams, SessionResult);

// --- approvals --------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalsListParams {
    #[serde(default)]
    pub status: Option<ApprovalStatus>,
    #[serde(default)]
    pub session_id: Option<Uuid>,
    #[serde(default)]
    pub project_id: Option<Uuid>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalsListResult {
    pub approvals: Vec<Approval>,
}

method!(ApprovalsList, "approvals.list", ApprovalsListParams, ApprovalsListResult);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approve,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalsDecideParams {
    pub approval_id: Uuid,
    pub decision: ApprovalDecision,
    /// Shown to the agent on deny.
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalResult {
    pub approval: Approval,
}

method!(ApprovalsDecide, "approvals.decide", ApprovalsDecideParams, ApprovalResult);

// --- policies ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoliciesListParams {
    pub project_id: Uuid,
    #[serde(default)]
    pub include_archived: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoliciesListResult {
    /// Project and workspace rules of the project, oldest first.
    pub rules: Vec<PolicyRule>,
    pub policy_revision: i64,
}

method!(PoliciesList, "policies.list", PoliciesListParams, PoliciesListResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyRuleIdParams {
    pub rule_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoliciesGetResult {
    pub rule: PolicyRule,
    /// Every revision, oldest first.
    pub history: Vec<PolicyRuleRevision>,
}

method!(PoliciesGet, "policies.get", PolicyRuleIdParams, PoliciesGetResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoliciesCreateParams {
    pub project_id: Uuid,
    /// Omit for a project rule.
    #[serde(default)]
    pub workspace_id: Option<Uuid>,
    pub category: OperationCategory,
    pub decision: PolicyDecision,
    #[serde(default)]
    pub conditions: PolicyConditions,
    pub description: String,
    /// Defaults to `true`.
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyRuleResult {
    pub rule: PolicyRule,
    /// Global policy revision created by this change.
    pub policy_revision: i64,
}

method!(PoliciesCreate, "policies.create", PoliciesCreateParams, PolicyRuleResult);

/// Changes a rule's effect. Scope and category are fixed for a rule's life;
/// create a new rule (and archive this one) to change them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoliciesUpdateParams {
    pub rule_id: Uuid,
    pub expected_revision: u32,
    #[serde(default)]
    pub decision: Option<PolicyDecision>,
    #[serde(default)]
    pub conditions: Option<PolicyConditions>,
    #[serde(default)]
    pub description: Option<String>,
}

method!(PoliciesUpdate, "policies.update", PoliciesUpdateParams, PolicyRuleResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoliciesSetEnabledParams {
    pub rule_id: Uuid,
    pub expected_revision: u32,
    pub enabled: bool,
}

method!(PoliciesSetEnabled, "policies.set_enabled", PoliciesSetEnabledParams, PolicyRuleResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoliciesArchiveParams {
    pub rule_id: Uuid,
    pub expected_revision: u32,
}

method!(PoliciesArchive, "policies.archive", PoliciesArchiveParams, PolicyRuleResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoliciesEffectiveParams {
    pub project_id: Uuid,
    #[serde(default)]
    pub workspace_id: Option<Uuid>,
}

method!(PoliciesEffective, "policies.effective", PoliciesEffectiveParams, EffectivePolicy);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoliciesSettingsParams {
    pub project_id: Uuid,
}

method!(PoliciesSettings, "policies.settings", PoliciesSettingsParams, PolicySettings);

/// Turns the project's auto mode on or off (ADR 0037).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoliciesSetAutoModeParams {
    pub project_id: Uuid,
    pub enabled: bool,
    /// The settings revision the change is based on (0 when never set).
    pub expected_revision: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicySettingsResult {
    pub settings: PolicySettings,
    /// Global policy revision created by this change.
    pub policy_revision: i64,
}

method!(PoliciesSetAutoMode, "policies.set_auto_mode", PoliciesSetAutoModeParams, PolicySettingsResult);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoliciesEvaluationsParams {
    #[serde(default)]
    pub project_id: Option<Uuid>,
    #[serde(default)]
    pub workspace_id: Option<Uuid>,
    #[serde(default)]
    pub session_id: Option<Uuid>,
    /// Evaluations this rule decided or matched.
    #[serde(default)]
    pub rule_id: Option<Uuid>,
    #[serde(default)]
    pub decision: Option<PolicyDecision>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PoliciesEvaluationsResult {
    /// Newest first.
    pub evaluations: Vec<PolicyEvaluation>,
}

method!(PoliciesEvaluations, "policies.evaluations", PoliciesEvaluationsParams, PoliciesEvaluationsResult);

// --- live output ------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveSubscribeParams {
    /// Only this session; omit for all sessions.
    #[serde(default)]
    pub session_id: Option<Uuid>,
}

method!(LiveSubscribe, "live.subscribe", LiveSubscribeParams, EventsSubscribeResult);
method!(LiveUnsubscribe, "live.unsubscribe", EventsUnsubscribeParams, Empty);

// --- tasks (M6, ADRs 0023-0027) ---------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TasksListParams {
    #[serde(default)]
    pub project_id: Option<Uuid>,
    /// Include completed and cancelled tasks.
    #[serde(default)]
    pub include_closed: bool,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TasksListResult {
    pub tasks: Vec<Task>,
}

method!(TasksList, "tasks.list", TasksListParams, TasksListResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskIdParams {
    pub task_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskDetail {
    pub task: Task,
    pub dependencies: Vec<TaskRef>,
    pub dependents: Vec<TaskRef>,
    pub readiness: TaskReadiness,
    pub assignments: Vec<TaskAssignment>,
    pub messages: Vec<TaskMessage>,
    pub findings: Vec<TaskFinding>,
    pub proposals: Vec<TaskProposal>,
    /// The task's workflow run, if it has one (M8.1).
    #[serde(default)]
    pub workflow: Option<WorkflowInfo>,
    /// The workflow's test gate runs, oldest first (ADR 0039).
    #[serde(default)]
    pub test_runs: Vec<TestRunInfo>,
}

/// One run of the workflow's test gate (ADR 0039): what Core ran, what the
/// policy decided for each command, and every result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TestRunInfo {
    pub id: Uuid,
    /// The implementation step tested, e.g. `implement#2`.
    pub step_key: String,
    /// `passed`, `failed` or `needs_authorization`.
    pub status: String,
    pub summary: String,
    /// `[{kind, name?, command, policy, status, exit_code?, duration_ms?, stdout_tail?, stderr_tail?, detail?}]`.
    pub results: serde_json::Value,
    /// The change touched UI files, so browser tests were required.
    pub ui_change: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorized_by: Option<String>,
    pub started_at_ms: i64,
    pub ended_at_ms: i64,
}

method!(TasksGet, "tasks.get", TaskIdParams, TaskDetail);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TasksCreateParams {
    pub project_id: Uuid,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub kind: Option<TaskKind>,
    #[serde(default)]
    pub priority: Option<Priority>,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub depends_on: Vec<Uuid>,
    #[serde(default)]
    pub parent_task_id: Option<Uuid>,
    /// Publish right away (`ready` or `blocked`) instead of keeping a draft.
    #[serde(default)]
    pub publish: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskResult {
    pub task: Task,
}

method!(TasksCreate, "tasks.create", TasksCreateParams, TaskResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TasksUpdateParams {
    pub task_id: Uuid,
    /// The revision the edit is based on; a stale revision is refused.
    pub expected_revision: i64,
    #[serde(default)]
    pub title: Option<String>,
    /// An empty string clears the description.
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub kind: Option<TaskKind>,
    #[serde(default)]
    pub priority: Option<Priority>,
    #[serde(default)]
    pub acceptance_criteria: Option<Vec<String>>,
}

method!(TasksUpdate, "tasks.update", TasksUpdateParams, TaskResult);
method!(TasksPublish, "tasks.publish", TaskIdParams, TaskResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDependencyParams {
    pub task_id: Uuid,
    pub depends_on: Uuid,
}

method!(TasksAddDependency, "tasks.add_dependency", TaskDependencyParams, TaskResult);
method!(TasksRemoveDependency, "tasks.remove_dependency", TaskDependencyParams, TaskResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TasksAssignParams {
    pub task_id: Uuid,
    pub role: Role,
    /// `claude_code`, `codex` or `gemini`.
    pub provider: String,
    pub workspace_id: Uuid,
    /// Start the session right away (default true).
    #[serde(default = "default_true")]
    pub start: bool,
    /// Model passed to the CLI; `None` = its default (M8.1).
    #[serde(default)]
    pub model: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskAssignmentResult {
    pub task: Task,
    pub assignment: TaskAssignment,
}

method!(TasksAssign, "tasks.assign", TasksAssignParams, TaskAssignmentResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskAssignmentIdParams {
    pub assignment_id: Uuid,
    #[serde(default)]
    pub reason: Option<String>,
}

method!(TasksStartAssignment, "tasks.start_assignment", TaskAssignmentIdParams, TaskAssignmentResult);
method!(TasksCancelAssignment, "tasks.cancel_assignment", TaskAssignmentIdParams, TaskAssignmentResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TasksNoteParams {
    pub task_id: Uuid,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TasksNoteResult {
    pub message: TaskMessage,
    /// Sent to the running agent now; otherwise it goes with the next assignment.
    pub delivered: bool,
}

method!(TasksNote, "tasks.note", TasksNoteParams, TasksNoteResult);

/// The operator's question or request to the task's supervisor (ADR 0041).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorAskParams {
    pub task_id: Uuid,
    pub text: String,
}

/// The task's conversation with its supervisor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorInfo {
    pub task_id: Uuid,
    /// The supervisor is answering the operator's last message.
    pub thinking: bool,
    /// Oldest first.
    pub messages: Vec<crate::model::SupervisorMessage>,
    /// The supervisor's own read-only workspace, once it has one.
    #[serde(default)]
    pub workspace_id: Option<Uuid>,
}

method!(SupervisorAsk, "supervisor.ask", SupervisorAskParams, SupervisorInfo);
method!(SupervisorGet, "supervisor.get", TaskIdParams, SupervisorInfo);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDecisionParams {
    pub task_id: Uuid,
    #[serde(default)]
    pub note: Option<String>,
}

method!(TasksComplete, "tasks.complete", TaskDecisionParams, TaskResult);
method!(TasksFail, "tasks.fail", TaskDecisionParams, TaskResult);
method!(TasksCancel, "tasks.cancel", TaskDecisionParams, TaskResult);
method!(TasksRetry, "tasks.retry", TaskDecisionParams, TaskResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TasksDecideProposalParams {
    pub proposal_id: Uuid,
    pub approve: bool,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TasksDecideProposalResult {
    pub proposal: TaskProposal,
    /// The draft task created from an approved proposal.
    pub created_task: Option<Task>,
}

method!(TasksDecideProposal, "tasks.decide_proposal", TasksDecideProposalParams, TasksDecideProposalResult);

// --- integrations (M7, ADRs 0028-0031) --------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationsListParams {
    #[serde(default)]
    pub project_id: Option<Uuid>,
    #[serde(default)]
    pub task_id: Option<Uuid>,
    /// Include integrated and cancelled integrations.
    #[serde(default)]
    pub include_closed: bool,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntegrationsListResult {
    pub integrations: Vec<Integration>,
}

method!(IntegrationsList, "integrations.list", IntegrationsListParams, IntegrationsListResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationIdParams {
    pub integration_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntegrationDetail {
    pub integration: Integration,
    /// Newest first.
    pub runs: Vec<ValidationRunInfo>,
    /// The policy decision for each enabled validation command.
    pub plan: Vec<ValidationPlanEntry>,
}

method!(IntegrationsGet, "integrations.get", IntegrationIdParams, IntegrationDetail);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationsCreateParams {
    /// The task whose work is delivered; its implementation workspace is
    /// used unless `workspace_id` is given.
    #[serde(default)]
    pub task_id: Option<Uuid>,
    #[serde(default)]
    pub workspace_id: Option<Uuid>,
    /// Defaults to the branch checked out in the project checkout.
    #[serde(default)]
    pub target_branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntegrationResult {
    pub integration: Integration,
}

method!(IntegrationsCreate, "integrations.create", IntegrationsCreateParams, IntegrationResult);
method!(IntegrationsRefresh, "integrations.refresh", IntegrationIdParams, IntegrationResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationsChangesParams {
    pub integration_id: Uuid,
}

method!(IntegrationsChanges, "integrations.changes", IntegrationsChangesParams, IntegrationChanges);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationsPatchParams {
    pub integration_id: Uuid,
    #[serde(default)]
    pub path: Option<String>,
    /// The workspace's uncommitted changes instead of the source commits.
    #[serde(default)]
    pub uncommitted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationsPatchResult {
    pub patch: String,
    pub truncated: bool,
}

method!(IntegrationsPatch, "integrations.patch", IntegrationsPatchParams, IntegrationsPatchResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationsCommitWorkspaceParams {
    pub integration_id: Uuid,
    pub message: String,
}

method!(
    IntegrationsCommitWorkspace,
    "integrations.commit_workspace",
    IntegrationsCommitWorkspaceParams,
    IntegrationResult
);
method!(
    IntegrationsUpdateFromTarget,
    "integrations.update_from_target",
    IntegrationIdParams,
    IntegrationResult
);
method!(IntegrationsAbortResolution, "integrations.abort_resolution", IntegrationIdParams, IntegrationResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationsValidateParams {
    pub integration_id: Uuid,
    /// Commands the policy asks about that the operator authorizes for this
    /// run, each with the exact command line the operator saw.
    #[serde(default)]
    pub authorize: Vec<ValidationAuthorization>,
}

/// The operator's authorization of one command, bound to its exact command
/// line: if the command was edited since, the authorization does not apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationAuthorization {
    pub command_id: Uuid,
    pub command_line: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntegrationsValidateResult {
    pub integration: Integration,
    pub run: ValidationRunInfo,
}

method!(
    IntegrationsValidate,
    "integrations.validate",
    IntegrationsValidateParams,
    IntegrationsValidateResult
);
method!(
    IntegrationsCancelValidation,
    "integrations.cancel_validation",
    IntegrationIdParams,
    IntegrationResult
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationsApproveParams {
    pub integration_id: Uuid,
    /// The source commit the operator inspected and approves.
    pub source_commit: String,
    /// The target commit the operator inspected and approves.
    pub target_commit: String,
}

method!(IntegrationsApprove, "integrations.approve", IntegrationsApproveParams, IntegrationResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationsCancelParams {
    pub integration_id: Uuid,
    #[serde(default)]
    pub reason: Option<String>,
}

method!(IntegrationsCancel, "integrations.cancel", IntegrationsCancelParams, IntegrationResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationsSettingsParams {
    pub project_id: Uuid,
}

method!(IntegrationsSettings, "integrations.settings", IntegrationsSettingsParams, IntegrationSettingsInfo);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationsSetSettingsParams {
    pub project_id: Uuid,
    pub require_review: bool,
}

method!(
    IntegrationsSetSettings,
    "integrations.set_settings",
    IntegrationsSetSettingsParams,
    IntegrationSettingsInfo
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationsSetCommandParams {
    pub project_id: Uuid,
    /// Omitted to add a command.
    #[serde(default)]
    pub command_id: Option<Uuid>,
    pub name: String,
    /// Program and arguments; run directly, never through a shell.
    pub argv: Vec<String>,
    pub timeout_secs: u32,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub position: Option<i64>,
}

method!(
    IntegrationsSetCommand,
    "integrations.set_command",
    IntegrationsSetCommandParams,
    IntegrationSettingsInfo
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationsDeleteCommandParams {
    pub command_id: Uuid,
}

method!(
    IntegrationsDeleteCommand,
    "integrations.delete_command",
    IntegrationsDeleteCommandParams,
    IntegrationSettingsInfo
);

// --- workflows (M8.1, ADR 0033) ---------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsModelsParams {
    /// `claude_code`, `codex` or `gemini`.
    pub provider: String,
    /// Ask the CLI again instead of using the cached answer.
    #[serde(default)]
    pub refresh: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsModelsResult {
    pub catalog: ModelCatalog,
}

method!(AgentsModels, "agents.models", AgentsModelsParams, AgentsModelsResult);

method!(WorkflowsSettings, "workflows.settings", ProjectIdParams, WorkflowSettingsInfo);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowsSetSettingsParams {
    pub project_id: Uuid,
    pub bindings: RoleBindings,
}

method!(WorkflowsSetSettings, "workflows.set_settings", WorkflowsSetSettingsParams, WorkflowSettingsInfo);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowsStartParams {
    pub task_id: Uuid,
    /// Task-level overrides of the project's role bindings.
    #[serde(default)]
    pub overrides: RoleBindingsPatch,
    /// Work in this workspace; by default Core creates one for the task.
    #[serde(default)]
    pub workspace_id: Option<Uuid>,
    /// `engineering` (default: the fixed M8.1 sequence) or `organization`
    /// (M8.3: a supervisor builds the team for the objective).
    #[serde(default)]
    pub definition: Option<String>,
    /// `organization` only: the lead supervisor's provider and model, instead
    /// of Core's capability-aware choice.
    #[serde(default)]
    pub lead: Option<RoleBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowResult {
    pub workflow: WorkflowInfo,
}

method!(WorkflowsStart, "workflows.start", WorkflowsStartParams, WorkflowResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowsGetResult {
    pub workflow: Option<WorkflowInfo>,
}

method!(WorkflowsGet, "workflows.get", TaskIdParams, WorkflowsGetResult);
method!(WorkflowsPause, "workflows.pause", TaskIdParams, WorkflowResult);
method!(WorkflowsResume, "workflows.resume", TaskIdParams, WorkflowResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowsCancelParams {
    pub task_id: Uuid,
    #[serde(default)]
    pub reason: Option<String>,
}

method!(WorkflowsCancel, "workflows.cancel", WorkflowsCancelParams, WorkflowResult);

/// Step-through mode (ADR 0043): wait before each hand-off to the next role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowsSetStepThroughParams {
    pub task_id: Uuid,
    pub on: bool,
}

method!(WorkflowsSetStepThrough, "workflows.set_step_through", WorkflowsSetStepThroughParams, WorkflowResult);

/// What the step the workflow waits to start would receive, built by the
/// same code that starts it (pending notes included).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowsPreviewNextResult {
    pub step_key: String,
    pub role: String,
    pub provider: String,
    #[serde(default)]
    pub model: Option<String>,
    /// The rendered prompt, exactly as it would be sent now.
    pub prompt: String,
    /// The structured context inside it.
    pub context: serde_json::Value,
}

method!(WorkflowsPreviewNext, "workflows.preview_next", TaskIdParams, WorkflowsPreviewNextResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowsRequestCorrectionParams {
    pub task_id: Uuid,
    /// Given to the correcting agent with the task context (for example what failed).
    #[serde(default)]
    pub note: Option<String>,
}

method!(
    WorkflowsRequestCorrection,
    "workflows.request_correction",
    WorkflowsRequestCorrectionParams,
    WorkflowResult
);

// --- agent organizations (M8.3) -------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrganizationsGetResult {
    pub organization: Option<OrganizationInfo>,
}

method!(OrganizationsGet, "organizations.get", TaskIdParams, OrganizationsGetResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrganizationsAnswerParams {
    pub task_id: Uuid,
    /// The user's answer to a supervisor's escalation.
    pub text: String,
}

method!(OrganizationsAnswer, "organizations.answer", OrganizationsAnswerParams, OrganizationsGetResult);

method!(OrganizationsSettings, "organizations.settings", ProjectIdParams, OrgSettingsInfo);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrganizationsSetSettingsParams {
    pub project_id: Uuid,
    /// Each omitted part keeps its current value.
    #[serde(default)]
    pub limits: Option<serde_json::Value>,
    #[serde(default)]
    pub estimates: Option<serde_json::Value>,
    #[serde(default)]
    pub overrides: Option<serde_json::Value>,
}

method!(
    OrganizationsSetSettings,
    "organizations.set_settings",
    OrganizationsSetSettingsParams,
    OrgSettingsInfo
);
