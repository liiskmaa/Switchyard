use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::ids::{EventId, new_id, now_ms};

/// Version of the envelope + payload schema written by this build.
///
/// Bump when an existing event type's payload changes incompatibly. Readers
/// must keep accepting older versions found in the journal.
pub const EVENT_SCHEMA_VERSION: u32 = 1;

/// Typed catalogue of events emitted by Switchyard Core.
///
/// Serialized adjacently tagged (`{"type": "...", "payload": {...}}`) so the
/// type string and payload can be stored in separate journal columns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum DomainEvent {
    #[serde(rename = "core.started")]
    CoreStarted { core_version: String, instance_id: Uuid },
    #[serde(rename = "core.stopping")]
    CoreStopping { instance_id: Uuid, reason: String },
    #[serde(rename = "project.registered")]
    ProjectRegistered { project_id: Uuid, name: String, root_path: String },
    #[serde(rename = "project.removed")]
    ProjectRemoved { project_id: Uuid, root_path: String },

    // --- agent sessions (context carries project/workspace/session/agent) --
    /// A native agent process was launched for a session.
    #[serde(rename = "agent.started")]
    AgentStarted {
        provider: String,
        cli_version: String,
        pid: u32,
        cwd: String,
        native_session_id: Option<String>,
        resumed: bool,
    },
    /// The agent completed its handshake.
    #[serde(rename = "agent.ready")]
    AgentReady { native_session_id: String, model: Option<String>, tools: Vec<String> },
    #[serde(rename = "agent.state_changed")]
    AgentStateChanged { from: String, to: String },
    /// Switchyard delivered a user message. `queued` means a turn was still
    /// running, so the agent will process it as its next turn.
    #[serde(rename = "agent.turn_started")]
    AgentTurnStarted { prompt: String, prompt_truncated: bool, queued: bool },
    /// A complete assistant message (never individual tokens).
    #[serde(rename = "agent.message")]
    AgentMessage { message_id: Option<String>, text: String, truncated: bool },
    #[serde(rename = "agent.tool_started")]
    AgentToolStarted {
        tool_use_id: String,
        tool_name: String,
        input: serde_json::Value,
        input_truncated: bool,
    },
    #[serde(rename = "agent.tool_completed")]
    AgentToolCompleted {
        tool_use_id: String,
        tool_name: Option<String>,
        output: String,
        output_truncated: bool,
    },
    /// The tool failed, or did not run (`rejected`: permission denied or the
    /// turn was interrupted first).
    #[serde(rename = "agent.tool_failed")]
    AgentToolFailed {
        tool_use_id: String,
        tool_name: Option<String>,
        output: String,
        output_truncated: bool,
        rejected: bool,
    },
    #[serde(rename = "agent.turn_completed")]
    AgentTurnCompleted {
        success: bool,
        interrupted: bool,
        reason: Option<String>,
        stop_reason: Option<String>,
        duration_ms: Option<u64>,
        /// The session's running total as reported by the agent, not this turn's cost.
        cost_usd: Option<f64>,
        /// This turn's cost, when reported. Absent from events journaled before it existed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_cost_usd: Option<f64>,
        /// Token counts for this turn (`input_tokens`, `cached_input_tokens`,
        /// `output_tokens`, `reasoning_output_tokens`, `total_tokens`), when reported.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<serde_json::Value>,
        result: Option<String>,
    },
    #[serde(rename = "agent.permission_requested")]
    AgentPermissionRequested {
        approval_id: Uuid,
        tool_name: String,
        tool_use_id: Option<String>,
        input: serde_json::Value,
        description: Option<String>,
        expires_at_ms: i64,
        /// The `ask` evaluation that created the approval (since v3).
        #[serde(default)]
        evaluation_id: Option<Uuid>,
        #[serde(default)]
        explanation: Option<String>,
    },
    /// A permission request was evaluated against the policy. For `allow`
    /// and `deny` this is the final, automatic decision; for `ask` an
    /// approval follows (`agent.permission_requested`).
    #[serde(rename = "agent.permission_evaluated")]
    AgentPermissionEvaluated {
        evaluation_id: Uuid,
        /// `request`, `reevaluation` or `decision_check`.
        trigger: String,
        tool_name: String,
        category: String,
        decision: String,
        reason: String,
        explanation: String,
        rule_id: Option<Uuid>,
        rule_revision: Option<u32>,
        policy_revision: i64,
        approval_id: Option<Uuid>,
        /// What the request targeted (path or command), bounded.
        target: Option<String>,
    },
    #[serde(rename = "agent.permission_resolved")]
    AgentPermissionResolved {
        approval_id: Uuid,
        tool_name: String,
        status: String,
        source: String,
        decided_by: Option<String>,
        note: Option<String>,
    },
    /// The agent acknowledged an interrupt request.
    #[serde(rename = "agent.interrupted")]
    AgentInterrupted {},
    #[serde(rename = "agent.session_resumed")]
    AgentSessionResumed { native_session_id: String, resume_count: u32 },
    #[serde(rename = "agent.warning")]
    AgentWarning { message: String },
    /// The agent process ended.
    #[serde(rename = "agent.exited")]
    AgentExited {
        exit_code: Option<i32>,
        exit_signal: Option<i32>,
        stop_requested: bool,
        final_state: String,
    },
    /// The session failed (start failure, crash, lost supervision).
    #[serde(rename = "agent.failed")]
    AgentFailed { reason: String, detail: Option<String> },

    // --- policies (context carries project_id, and workspace_id for
    // workspace rules) -----------------------------------------------------
    /// A policy rule was created, changed, enabled, disabled or archived.
    #[serde(rename = "policy.rule_changed")]
    PolicyRuleChanged {
        rule_id: Uuid,
        /// `created`, `updated`, `enabled`, `disabled` or `archived`.
        change: String,
        revision: u32,
        policy_revision: i64,
        scope: String,
        category: String,
        decision: String,
        enabled: bool,
        archived: bool,
        description: String,
        conditions: serde_json::Value,
    },
    /// A project's policy settings changed (ADR 0037); context carries the
    /// project.
    #[serde(rename = "policy.settings_changed")]
    PolicySettingsChanged { auto_mode: bool, revision: u32, policy_revision: i64 },

    // --- workspaces (context carries project_id and workspace_id) ----------
    /// A workspace record was created; provisioning of its worktree begins.
    #[serde(rename = "workspace.created")]
    WorkspaceCreated { name: String, path: String, branch: String, base_ref: String, base_commit: String },
    #[serde(rename = "workspace.state_changed")]
    WorkspaceStateChanged { from: String, to: String, reason: Option<String> },
    /// The operator asked for cleanup. `allowed` is the safety verdict;
    /// `blockers`/`warnings` are finding codes.
    #[serde(rename = "workspace.cleanup_requested")]
    WorkspaceCleanupRequested {
        allowed: bool,
        blockers: Vec<String>,
        warnings: Vec<String>,
        will_remove_directory: bool,
        /// Scanned tree state the verdict was based on (audit).
        #[serde(default)]
        tree_fingerprint: Option<String>,
        #[serde(default)]
        entries: Option<u64>,
    },
    /// Cleanup finished: the worktree is gone; the branch is kept.
    #[serde(rename = "workspace.removed")]
    WorkspaceRemoved { path: String, branch: String, directory_removed: bool, branch_kept: bool },
    /// Cleanup was attempted and failed; the workspace was preserved.
    #[serde(rename = "workspace.cleanup_failed")]
    WorkspaceCleanupFailed { reason: String, detail: Option<String> },

    // --- filesystem --------------------------------------------------------
    /// Actual changes observed on disk in one workspace (debounced batch).
    /// This records what the filesystem reported, independent of agent
    /// intent. Context: project, workspace and (when one is attached)
    /// session; `occurred_at_ms` is the batch time.
    #[serde(rename = "files.changed")]
    FilesChanged { changes: Vec<FileChange>, dropped: u32 },

    // --- tasks (context carries project_id and task_id; ADRs 0023-0027) ----
    /// A task was created by the operator or from an approved proposal.
    #[serde(rename = "task.created")]
    TaskCreated {
        title: String,
        kind: String,
        priority: String,
        status: String,
        /// `operator` or `proposal`.
        origin: String,
        parent_task_id: Option<Uuid>,
        origin_proposal_id: Option<Uuid>,
    },
    /// Editable fields changed (`fields` names them).
    #[serde(rename = "task.updated")]
    TaskUpdated { revision: i64, fields: Vec<String> },
    #[serde(rename = "task.status_changed")]
    TaskStatusChanged { from: String, to: String, reason: Option<String> },
    #[serde(rename = "task.dependency_added")]
    TaskDependencyAdded { depends_on: Uuid },
    #[serde(rename = "task.dependency_removed")]
    TaskDependencyRemoved { depends_on: Uuid },
    /// The operator assigned a role on the task to an agent provider.
    #[serde(rename = "task.assigned")]
    TaskAssigned {
        assignment_id: Uuid,
        role: String,
        provider: String,
        workspace_id: Uuid,
        /// The model asked for (`None` = the CLI's default). M8.1.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// The workflow step, for assignments the workflow created. M8.1.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step_key: Option<String>,
    },
    /// The assignment's session was started (or resumed) and given the context.
    #[serde(rename = "task.assignment_started")]
    TaskAssignmentStarted {
        assignment_id: Uuid,
        session_id: Uuid,
        access: String,
        context_version: u32,
        resumed: bool,
    },
    /// `status` is `reported`, `failed` or `cancelled`.
    #[serde(rename = "task.assignment_ended")]
    TaskAssignmentEnded { assignment_id: Uuid, status: String, outcome: Option<String> },
    /// A structured message was recorded. A message from an agent is a claim
    /// or a request; it never authorizes anything.
    #[serde(rename = "task.message_recorded")]
    TaskMessageRecorded {
        message_id: Uuid,
        kind: String,
        sender: String,
        assignment_id: Option<Uuid>,
        summary: Option<String>,
    },
    /// A review verdict. `result` differs from `reported_result` when Core
    /// overrode it (the workspace changed during the review).
    #[serde(rename = "task.review_recorded")]
    TaskReviewRecorded {
        assignment_id: Uuid,
        reported_result: String,
        result: String,
        workspace_changed: bool,
        findings: u32,
    },
    /// The operator approved or rejected a proposed subtask.
    #[serde(rename = "task.proposal_decided")]
    TaskProposalDecided { proposal_id: Uuid, decision: String, created_task_id: Option<Uuid> },

    // --- integrations (context carries project_id, workspace_id and, when
    // linked, task_id; ADRs 0028-0031) ---------------------------------------
    /// The operator proposed delivering a workspace into a target branch.
    #[serde(rename = "integration.created")]
    IntegrationCreated {
        integration_id: Uuid,
        source_branch: String,
        target_branch: String,
        created_by: String,
    },
    /// Core (re)bound the integration to the current commits and checked it.
    #[serde(rename = "integration.prepared")]
    IntegrationPrepared {
        integration_id: Uuid,
        source_commit: String,
        target_commit: String,
        base_commit: Option<String>,
        /// Blocker codes; empty means ready for approval.
        blockers: Vec<String>,
        files_changed: u64,
    },
    #[serde(rename = "integration.status_changed")]
    IntegrationStatusChanged { integration_id: Uuid, from: String, to: String, reason: Option<String> },
    /// Core changed the source workspace on the operator's request:
    /// `committed`, `updated` (merged the target), `conflicts`, `aborted`.
    #[serde(rename = "integration.workspace_changed")]
    IntegrationWorkspaceChanged {
        integration_id: Uuid,
        action: String,
        commit: Option<String>,
        files: Vec<String>,
        by: String,
    },
    #[serde(rename = "integration.validation_started")]
    IntegrationValidationStarted {
        integration_id: Uuid,
        run_id: Uuid,
        source_commit: String,
        commands: Vec<String>,
    },
    #[serde(rename = "integration.validation_step_finished")]
    IntegrationValidationStepFinished {
        integration_id: Uuid,
        run_id: Uuid,
        name: String,
        command: String,
        status: String,
        exit_code: Option<i32>,
        authorized_by: Option<String>,
        policy_decision: String,
    },
    #[serde(rename = "integration.validation_finished")]
    IntegrationValidationFinished {
        integration_id: Uuid,
        run_id: Uuid,
        source_commit: String,
        status: String,
    },
    /// The operator approved these exact commits; Core starts integrating.
    #[serde(rename = "integration.approved")]
    IntegrationApproved {
        integration_id: Uuid,
        source_commit: String,
        target_commit: String,
        merge_commit: String,
        method: String,
        approved_by: String,
    },
    /// The target branch now points to the merge commit.
    #[serde(rename = "integration.integrated")]
    IntegrationIntegrated {
        integration_id: Uuid,
        target_branch: String,
        merge_commit: String,
        source_commit: String,
        target_commit: String,
        /// Settled by crash recovery after a Core restart.
        recovered: bool,
    },
    /// The integration did not happen; `target_moved` says whether the
    /// target branch is somewhere unexpected.
    #[serde(rename = "integration.failed")]
    IntegrationFailed { integration_id: Uuid, reason: String, target_moved: bool },
    /// Core started an operation it owns (ADR 0032): `approve`,
    /// `commit_workspace`, `update_from_target`, `abort_resolution`,
    /// `validate`. It runs to completion even if the requesting client
    /// disconnects.
    #[serde(rename = "integration.operation_started")]
    IntegrationOperationStarted {
        integration_id: Uuid,
        operation_id: Uuid,
        kind: String,
        requested_by: String,
    },
    /// The operation ended: `completed`, `failed` (with `error`) or
    /// `interrupted` (Core stopped during it; settled from Git at startup).
    #[serde(rename = "integration.operation_finished")]
    IntegrationOperationFinished {
        integration_id: Uuid,
        operation_id: Uuid,
        kind: String,
        status: String,
        error: Option<String>,
    },
    /// The operator changed the project's integration settings.
    #[serde(rename = "integration.settings_changed")]
    IntegrationSettingsChanged {
        project_id: Uuid,
        require_review_before: bool,
        require_review_after: bool,
        revision: i64,
        changed_by: String,
    },
    /// The operator created, changed or deleted a validation command.
    /// `before`/`after`: `{name, command_line, timeout_secs, enabled, position}`.
    #[serde(rename = "integration.validation_command_changed")]
    IntegrationValidationCommandChanged {
        project_id: Uuid,
        command_id: Uuid,
        /// `created`, `updated` or `deleted`.
        action: String,
        before: Option<Value>,
        after: Option<Value>,
        changed_by: String,
    },
    /// A task's workflow started (M8.1, ADR 0033). `bindings`: the effective
    /// provider and model per role, snapshotted now.
    #[serde(rename = "workflow.started")]
    WorkflowStarted {
        definition: String,
        version: u32,
        workspace_id: Option<Uuid>,
        bindings: Value,
        started_by: String,
    },
    /// The workflow created the assignment of its next step.
    #[serde(rename = "workflow.stage_changed")]
    WorkflowStageChanged {
        stage: String,
        step_key: String,
        assignment_id: Uuid,
        role: String,
        provider: String,
        model: Option<String>,
    },
    /// The workflow stopped for a human; `reason` says why.
    #[serde(rename = "workflow.waiting")]
    WorkflowWaiting { step_key: Option<String>, reason: String },
    #[serde(rename = "workflow.paused")]
    WorkflowPaused { by: String },
    #[serde(rename = "workflow.resumed")]
    WorkflowResumed { by: String },
    /// The review approved: ready for human-approved delivery.
    #[serde(rename = "workflow.completed")]
    WorkflowCompleted { reason: String },
    /// Delivery progressed (M8.2): `phase` as in `WorkflowInfo.delivery`.
    #[serde(rename = "workflow.delivery_changed")]
    WorkflowDeliveryChanged { phase: String, reason: String, integration_id: Option<Uuid> },
    /// The test gate progressed (ADR 0039): `phase` is `running`, `passed`,
    /// `failed` or `needs_authorization`.
    #[serde(rename = "workflow.tests_changed")]
    WorkflowTestsChanged { phase: String, reason: String, step_key: Option<String> },
    /// Reasoning split the task; the workflow runs these subtasks in order (ADR 0040).
    #[serde(rename = "workflow.planned")]
    WorkflowPlanned { subtasks: Vec<String> },
    /// A subtask's review approved (ADR 0040).
    #[serde(rename = "workflow.subtask_completed")]
    WorkflowSubtaskCompleted { number: u32, title: String },
    /// The approved integration landed on the target branch (M8.2).
    #[serde(rename = "workflow.delivered")]
    WorkflowDelivered { reason: String, integration_id: Option<Uuid> },
    #[serde(rename = "workflow.failed")]
    WorkflowFailed { reason: String },
    /// The operator, through the task's supervisor, sent the work to `to`
    /// (`reasoning`, `coding`, `review` or `tests`; ADR 0041).
    #[serde(rename = "workflow.directed")]
    WorkflowDirected { to: String, by: String },
    /// The operator asked the task's supervisor something (ADR 0041).
    #[serde(rename = "supervisor.asked")]
    SupervisorAsked { message_id: Uuid, by: String },
    /// The supervisor answered (or Core recorded why it could not);
    /// `route` and `outcome` say where the work went.
    #[serde(rename = "supervisor.answered")]
    SupervisorAnswered {
        message_id: Uuid,
        route: Option<String>,
        outcome: Option<String>,
        follow_up_task_id: Option<Uuid>,
    },
    #[serde(rename = "workflow.cancelled")]
    WorkflowCancelled { reason: String },
    /// The operator changed the project's role bindings.
    #[serde(rename = "workflow.settings_changed")]
    WorkflowSettingsChanged {
        project_id: Uuid,
        before: Option<Value>,
        after: Value,
        revision: i64,
        changed_by: String,
    },

    // --- agent organizations (M8.3, ADRs 0035–0036; context carries the
    // objective's task_id) ---------------------------------------------------
    /// An organization was created for the objective, with its lead supervisor.
    #[serde(rename = "organization.started")]
    OrganizationStarted { lead: Value, limits: Value, workspace_id: Uuid, started_by: String },
    /// A supervisor added an agent (or Core added the lead).
    #[serde(rename = "organization.agent_added")]
    OrganizationAgentAdded {
        agent_id: Uuid,
        key: String,
        title: String,
        kind: String,
        reports_to: Option<String>,
        provider: String,
        model: Option<String>,
        /// Why this provider/model (Core's capability-aware selection).
        selection: String,
        added_by: String,
    },
    /// A supervisor delegated work: an organization task backed by an M6 task.
    #[serde(rename = "organization.task_delegated")]
    OrganizationTaskDelegated {
        key: String,
        subtask_id: Uuid,
        title: String,
        work: String,
        owner: String,
        parent: Option<String>,
        delegated_by: String,
    },
    /// A supervision turn began: `items` is what the supervisor was asked to look at.
    #[serde(rename = "organization.supervision_started")]
    OrganizationSupervisionStarted { supervisor: String, key: String, items: Vec<String> },
    /// A work turn began. `instructions` were put into the turn's task context
    /// (delivered when the turn starts; never injected into a running turn).
    #[serde(rename = "organization.work_started")]
    OrganizationWorkStarted { key: String, agent: String, instructions: Vec<String> },
    /// A supervisor assessed reported work: accept, correct, reassign or cancel.
    #[serde(rename = "organization.assessed")]
    OrganizationAssessed { key: String, verdict: String, by: String, instructions: Option<String> },
    /// Core observed something during a work turn (and interrupted it), or in
    /// the verified changes after it.
    #[serde(rename = "organization.observed")]
    OrganizationObserved { key: String, text: String, interrupted: bool },
    #[serde(rename = "organization.escalated")]
    OrganizationEscalated { by: String, to: String, question: String },
    /// A supervisor's directive did not fit the organization or its limits; nothing changed.
    #[serde(rename = "organization.directive_rejected")]
    OrganizationDirectiveRejected { by: String, error: String },
    /// Entries `from_seq..=to_seq` were added to the shared context.
    #[serde(rename = "organization.context_added")]
    OrganizationContextAdded { from_seq: i64, to_seq: i64 },
    /// `running`, `waiting`, `paused`, `done` or `cancelled`, and why.
    #[serde(rename = "organization.status_changed")]
    OrganizationStatusChanged { status: String, reason: Option<String> },
    /// The project's organization settings (limits, estimates, overrides) changed.
    #[serde(rename = "organization.settings_changed")]
    OrganizationSettingsChanged { project_id: Uuid, before: Option<Value>, after: Value, revision: i64 },
}

/// One observed filesystem change, relative to the workspace root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    /// `created`, `modified`, `deleted` or `renamed`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

impl DomainEvent {
    /// The stable wire/journal name of this event type.
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::CoreStarted { .. } => "core.started",
            Self::CoreStopping { .. } => "core.stopping",
            Self::ProjectRegistered { .. } => "project.registered",
            Self::ProjectRemoved { .. } => "project.removed",
            Self::AgentStarted { .. } => "agent.started",
            Self::AgentReady { .. } => "agent.ready",
            Self::AgentStateChanged { .. } => "agent.state_changed",
            Self::AgentTurnStarted { .. } => "agent.turn_started",
            Self::AgentMessage { .. } => "agent.message",
            Self::AgentToolStarted { .. } => "agent.tool_started",
            Self::AgentToolCompleted { .. } => "agent.tool_completed",
            Self::AgentToolFailed { .. } => "agent.tool_failed",
            Self::AgentTurnCompleted { .. } => "agent.turn_completed",
            Self::AgentPermissionRequested { .. } => "agent.permission_requested",
            Self::AgentPermissionResolved { .. } => "agent.permission_resolved",
            Self::AgentPermissionEvaluated { .. } => "agent.permission_evaluated",
            Self::PolicyRuleChanged { .. } => "policy.rule_changed",
            Self::PolicySettingsChanged { .. } => "policy.settings_changed",
            Self::AgentInterrupted {} => "agent.interrupted",
            Self::AgentSessionResumed { .. } => "agent.session_resumed",
            Self::AgentWarning { .. } => "agent.warning",
            Self::AgentExited { .. } => "agent.exited",
            Self::AgentFailed { .. } => "agent.failed",
            Self::WorkspaceCreated { .. } => "workspace.created",
            Self::WorkspaceStateChanged { .. } => "workspace.state_changed",
            Self::WorkspaceCleanupRequested { .. } => "workspace.cleanup_requested",
            Self::WorkspaceRemoved { .. } => "workspace.removed",
            Self::WorkspaceCleanupFailed { .. } => "workspace.cleanup_failed",
            Self::FilesChanged { .. } => "files.changed",
            Self::TaskCreated { .. } => "task.created",
            Self::TaskUpdated { .. } => "task.updated",
            Self::TaskStatusChanged { .. } => "task.status_changed",
            Self::TaskDependencyAdded { .. } => "task.dependency_added",
            Self::TaskDependencyRemoved { .. } => "task.dependency_removed",
            Self::TaskAssigned { .. } => "task.assigned",
            Self::TaskAssignmentStarted { .. } => "task.assignment_started",
            Self::TaskAssignmentEnded { .. } => "task.assignment_ended",
            Self::TaskMessageRecorded { .. } => "task.message_recorded",
            Self::TaskReviewRecorded { .. } => "task.review_recorded",
            Self::TaskProposalDecided { .. } => "task.proposal_decided",
            Self::IntegrationCreated { .. } => "integration.created",
            Self::IntegrationPrepared { .. } => "integration.prepared",
            Self::IntegrationStatusChanged { .. } => "integration.status_changed",
            Self::IntegrationWorkspaceChanged { .. } => "integration.workspace_changed",
            Self::IntegrationValidationStarted { .. } => "integration.validation_started",
            Self::IntegrationValidationStepFinished { .. } => "integration.validation_step_finished",
            Self::IntegrationValidationFinished { .. } => "integration.validation_finished",
            Self::IntegrationApproved { .. } => "integration.approved",
            Self::IntegrationIntegrated { .. } => "integration.integrated",
            Self::IntegrationFailed { .. } => "integration.failed",
            Self::IntegrationOperationStarted { .. } => "integration.operation_started",
            Self::IntegrationOperationFinished { .. } => "integration.operation_finished",
            Self::IntegrationSettingsChanged { .. } => "integration.settings_changed",
            Self::IntegrationValidationCommandChanged { .. } => "integration.validation_command_changed",
            Self::WorkflowStarted { .. } => "workflow.started",
            Self::WorkflowStageChanged { .. } => "workflow.stage_changed",
            Self::WorkflowWaiting { .. } => "workflow.waiting",
            Self::WorkflowPaused { .. } => "workflow.paused",
            Self::WorkflowResumed { .. } => "workflow.resumed",
            Self::WorkflowCompleted { .. } => "workflow.completed",
            Self::WorkflowDeliveryChanged { .. } => "workflow.delivery_changed",
            Self::WorkflowTestsChanged { .. } => "workflow.tests_changed",
            Self::WorkflowPlanned { .. } => "workflow.planned",
            Self::WorkflowSubtaskCompleted { .. } => "workflow.subtask_completed",
            Self::WorkflowDelivered { .. } => "workflow.delivered",
            Self::WorkflowFailed { .. } => "workflow.failed",
            Self::WorkflowDirected { .. } => "workflow.directed",
            Self::SupervisorAsked { .. } => "supervisor.asked",
            Self::SupervisorAnswered { .. } => "supervisor.answered",
            Self::WorkflowCancelled { .. } => "workflow.cancelled",
            Self::WorkflowSettingsChanged { .. } => "workflow.settings_changed",
            Self::OrganizationStarted { .. } => "organization.started",
            Self::OrganizationAgentAdded { .. } => "organization.agent_added",
            Self::OrganizationTaskDelegated { .. } => "organization.task_delegated",
            Self::OrganizationSupervisionStarted { .. } => "organization.supervision_started",
            Self::OrganizationWorkStarted { .. } => "organization.work_started",
            Self::OrganizationAssessed { .. } => "organization.assessed",
            Self::OrganizationObserved { .. } => "organization.observed",
            Self::OrganizationEscalated { .. } => "organization.escalated",
            Self::OrganizationDirectiveRejected { .. } => "organization.directive_rejected",
            Self::OrganizationContextAdded { .. } => "organization.context_added",
            Self::OrganizationStatusChanged { .. } => "organization.status_changed",
            Self::OrganizationSettingsChanged { .. } => "organization.settings_changed",
        }
    }

    fn into_payload(self) -> Result<Value, serde_json::Error> {
        let mut tagged = serde_json::to_value(self)?;
        Ok(tagged.as_object_mut().and_then(|o| o.remove("payload")).unwrap_or(Value::Null))
    }
}

/// Correlation fields attached to an event. All are optional because many
/// events (e.g. core lifecycle) are not scoped to a project or session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventContext {
    pub project_id: Option<Uuid>,
    pub workspace_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub agent_id: Option<Uuid>,
    /// The event that directly caused this one.
    pub causation_id: Option<EventId>,
}

/// An event that has been constructed but not yet stored. It has no sequence
/// number; the journal assigns one on append.
#[derive(Debug, Clone, PartialEq)]
pub struct EventDraft {
    pub id: EventId,
    pub schema_version: u32,
    pub event_type: String,
    pub occurred_at_ms: i64,
    pub context: EventContext,
    pub payload: Value,
}

impl EventDraft {
    pub fn new(event: DomainEvent, context: EventContext) -> Result<Self, serde_json::Error> {
        let event_type = event.event_type().to_owned();
        Ok(Self {
            id: new_id(),
            schema_version: EVENT_SCHEMA_VERSION,
            event_type,
            occurred_at_ms: now_ms(),
            context,
            payload: event.into_payload()?,
        })
    }

    /// Attaches the journal-assigned sequence number.
    pub fn into_envelope(self, seq: i64) -> EventEnvelope {
        EventEnvelope {
            seq,
            id: self.id,
            schema_version: self.schema_version,
            event_type: self.event_type,
            occurred_at_ms: self.occurred_at_ms,
            context: self.context,
            payload: self.payload,
        }
    }
}

/// A durably stored event, as read from the journal and delivered to
/// subscribers.
///
/// `event_type` and `payload` are kept untyped so that events written by a
/// newer schema version can still be read, listed and forwarded. Use
/// [`EventEnvelope::decode`] to obtain a typed [`DomainEvent`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// Monotonic journal position. Strictly increasing, may contain gaps.
    pub seq: i64,
    pub id: EventId,
    pub schema_version: u32,
    pub event_type: String,
    pub occurred_at_ms: i64,
    #[serde(flatten)]
    pub context: EventContext,
    pub payload: Value,
}

#[derive(Debug, thiserror::Error)]
#[error("cannot decode event {event_type} (schema v{schema_version}): {source}")]
pub struct EventDecodeError {
    pub event_type: String,
    pub schema_version: u32,
    #[source]
    pub source: serde_json::Error,
}

impl EventEnvelope {
    pub fn decode(&self) -> Result<DomainEvent, EventDecodeError> {
        let tagged = serde_json::json!({ "type": self.event_type, "payload": self.payload });
        serde_json::from_value(tagged).map_err(|source| EventDecodeError {
            event_type: self.event_type.clone(),
            schema_version: self.schema_version,
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_splits_type_and_payload_and_decodes_back() -> Result<(), Box<dyn std::error::Error>> {
        let project_id = new_id();
        let event =
            DomainEvent::ProjectRegistered { project_id, name: "demo".into(), root_path: "/tmp/demo".into() };
        let draft = EventDraft::new(
            event.clone(),
            EventContext { project_id: Some(project_id), ..Default::default() },
        )?;
        assert_eq!(draft.event_type, "project.registered");
        assert_eq!(draft.payload["name"], "demo");
        assert_eq!(draft.schema_version, EVENT_SCHEMA_VERSION);

        let envelope = draft.into_envelope(7);
        assert_eq!(envelope.seq, 7);
        assert_eq!(envelope.decode()?, event);
        Ok(())
    }

    #[test]
    fn every_variant_tag_matches_event_type() -> Result<(), Box<dyn std::error::Error>> {
        let samples = vec![
            DomainEvent::AgentInterrupted {},
            DomainEvent::AgentWarning { message: "m".into() },
            DomainEvent::AgentStateChanged { from: "idle".into(), to: "running".into() },
            DomainEvent::WorkspaceStateChanged { from: "ready".into(), to: "active".into(), reason: None },
            DomainEvent::WorkspaceRemoved {
                path: "/w".into(),
                branch: "switchyard/x".into(),
                directory_removed: true,
                branch_kept: true,
            },
            DomainEvent::FilesChanged {
                changes: vec![FileChange { path: "a".into(), kind: "created".into(), from: None }],
                dropped: 0,
            },
            DomainEvent::TaskStatusChanged { from: "ready".into(), to: "assigned".into(), reason: None },
            DomainEvent::TaskDependencyAdded { depends_on: new_id() },
            DomainEvent::TaskReviewRecorded {
                assignment_id: new_id(),
                reported_result: "approved".into(),
                result: "blocked".into(),
                workspace_changed: true,
                findings: 1,
            },
            DomainEvent::TaskProposalDecided {
                proposal_id: new_id(),
                decision: "rejected".into(),
                created_task_id: None,
            },
            DomainEvent::IntegrationFailed {
                integration_id: new_id(),
                reason: "x".into(),
                target_moved: false,
            },
            DomainEvent::IntegrationStatusChanged {
                integration_id: new_id(),
                from: "ready".into(),
                to: "integrating".into(),
                reason: None,
            },
            DomainEvent::IntegrationOperationStarted {
                integration_id: new_id(),
                operation_id: new_id(),
                kind: "approve".into(),
                requested_by: "desktop".into(),
            },
            DomainEvent::IntegrationOperationFinished {
                integration_id: new_id(),
                operation_id: new_id(),
                kind: "approve".into(),
                status: "failed".into(),
                error: Some("x".into()),
            },
            DomainEvent::IntegrationSettingsChanged {
                project_id: new_id(),
                require_review_before: true,
                require_review_after: false,
                revision: 2,
                changed_by: "desktop".into(),
            },
            DomainEvent::IntegrationValidationCommandChanged {
                project_id: new_id(),
                command_id: new_id(),
                action: "updated".into(),
                before: Some(serde_json::json!({"name": "a"})),
                after: None,
                changed_by: "desktop".into(),
            },
            DomainEvent::WorkflowStarted {
                definition: "engineering".into(),
                version: 1,
                workspace_id: Some(new_id()),
                bindings: serde_json::json!({"review": {"provider": "claude_code", "model": "opus"}}),
                started_by: "desktop".into(),
            },
            DomainEvent::WorkflowStageChanged {
                stage: "correction".into(),
                step_key: "implement#2".into(),
                assignment_id: new_id(),
                role: "implement".into(),
                provider: "codex".into(),
                model: None,
            },
            DomainEvent::WorkflowWaiting { step_key: Some("review#3".into()), reason: "x".into() },
            DomainEvent::WorkflowPaused { by: "desktop".into() },
            DomainEvent::WorkflowResumed { by: "desktop".into() },
            DomainEvent::WorkflowCompleted { reason: "approved".into() },
            DomainEvent::WorkflowFailed { reason: "x".into() },
            DomainEvent::WorkflowDirected { to: "coding".into(), by: "desktop".into() },
            DomainEvent::SupervisorAsked { message_id: new_id(), by: "desktop".into() },
            DomainEvent::SupervisorAnswered {
                message_id: new_id(),
                route: Some("coding".into()),
                outcome: Some("started implement#2".into()),
                follow_up_task_id: None,
            },
            DomainEvent::WorkflowPlanned { subtasks: vec!["a".into(), "b".into()] },
            DomainEvent::WorkflowSubtaskCompleted { number: 1, title: "a".into() },
            DomainEvent::WorkflowTestsChanged {
                phase: "failed".into(),
                reason: "unit failed".into(),
                step_key: Some("implement#1".into()),
            },
            DomainEvent::WorkflowDeliveryChanged {
                phase: "validating".into(),
                reason: "x".into(),
                integration_id: Some(new_id()),
            },
            DomainEvent::WorkflowDelivered { reason: "x".into(), integration_id: None },
            DomainEvent::WorkflowCancelled { reason: "x".into() },
            DomainEvent::WorkflowSettingsChanged {
                project_id: new_id(),
                before: None,
                after: serde_json::json!({}),
                revision: 1,
                changed_by: "desktop".into(),
            },
            DomainEvent::OrganizationStarted {
                lead: serde_json::json!({"key": "lead"}),
                limits: serde_json::json!({}),
                workspace_id: new_id(),
                started_by: "desktop".into(),
            },
            DomainEvent::OrganizationAgentAdded {
                agent_id: new_id(),
                key: "docs".into(),
                title: "Docs".into(),
                kind: "specialist".into(),
                reports_to: Some("lead".into()),
                provider: "codex".into(),
                model: None,
                selection: "x".into(),
                added_by: "lead".into(),
            },
            DomainEvent::OrganizationTaskDelegated {
                key: "t".into(),
                subtask_id: new_id(),
                title: "T".into(),
                work: "documentation".into(),
                owner: "docs".into(),
                parent: Some("objective".into()),
                delegated_by: "lead".into(),
            },
            DomainEvent::OrganizationSupervisionStarted {
                supervisor: "lead".into(),
                key: "objective".into(),
                items: vec!["plan".into()],
            },
            DomainEvent::OrganizationWorkStarted {
                key: "t".into(),
                agent: "docs".into(),
                instructions: vec![],
            },
            DomainEvent::OrganizationAssessed {
                key: "t".into(),
                verdict: "accept".into(),
                by: "lead".into(),
                instructions: None,
            },
            DomainEvent::OrganizationObserved { key: "t".into(), text: "x".into(), interrupted: true },
            DomainEvent::OrganizationEscalated { by: "lead".into(), to: "user".into(), question: "?".into() },
            DomainEvent::OrganizationDirectiveRejected { by: "lead".into(), error: "x".into() },
            DomainEvent::OrganizationContextAdded { from_seq: 1, to_seq: 3 },
            DomainEvent::OrganizationStatusChanged { status: "waiting".into(), reason: Some("x".into()) },
            DomainEvent::OrganizationSettingsChanged {
                project_id: new_id(),
                before: None,
                after: serde_json::json!({}),
                revision: 1,
            },
            DomainEvent::TaskAssigned {
                assignment_id: new_id(),
                role: "review".into(),
                provider: "claude_code".into(),
                workspace_id: new_id(),
                model: Some("opus".into()),
                step_key: Some("review#1".into()),
            },
        ];
        for event in samples {
            let tagged = serde_json::to_value(&event)?;
            assert_eq!(tagged["type"], event.event_type());
            let envelope = EventDraft::new(event.clone(), EventContext::default())?.into_envelope(1);
            assert_eq!(envelope.decode()?, event);
        }
        Ok(())
    }

    #[test]
    fn unknown_event_types_are_preserved_but_not_decodable() -> Result<(), Box<dyn std::error::Error>> {
        let json = serde_json::json!({
            "seq": 1, "id": new_id(), "schema_version": 9, "event_type": "future.thing",
            "occurred_at_ms": 1, "project_id": null, "workspace_id": null, "session_id": null,
            "task_id": null, "agent_id": null, "causation_id": null, "payload": {"x": 1}
        });
        let envelope: EventEnvelope = serde_json::from_value(json)?;
        assert_eq!(envelope.event_type, "future.thing");
        assert!(envelope.decode().is_err());
        Ok(())
    }
}
