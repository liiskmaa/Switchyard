// TypeScript mirror of the Switchyard Core protocol (crates/switchyard-protocol).
// Keep in sync with docs/protocols/core-ipc.md.

export type Uuid = string;

export interface CoreInfo {
  version: string;
  instance_id: Uuid;
  started_at_ms: number;
  pid: number;
}

export type ConnectionState =
  | { state: "connecting"; attempt: number }
  | { state: "connected"; core: CoreInfo; protocol_version: number }
  | { state: "disconnected"; error: string; retry_in_ms: number; launched_core: boolean; incompatible?: boolean }
  /** Stopped from the app: nothing starts it until "Start Core". */
  | { state: "stopped" }
  /** The page is not running inside the Switchyard desktop runtime. */
  | { state: "unavailable"; error: string };

export interface DatabaseHealth {
  initialized: boolean;
  path: string;
  journal_mode: string | null;
  schema_version: number | null;
  error: string | null;
}

export interface HealthReport {
  status: "ok" | "degraded";
  core: CoreInfo;
  protocol_version: number;
  uptime_ms: number;
  database: DatabaseHealth;
  projects_count: number | null;
  active_sessions_count: number | null;
  last_event_seq: number | null;
  connected_clients: number;
}

export interface Project {
  id: Uuid;
  name: string;
  root_path: string;
  created_at_ms: number;
  updated_at_ms: number;
  accessible: boolean;
}

export interface GitStatusReport {
  project_id: Uuid;
  branch: string | null;
  head_oid: string | null;
  detached: boolean;
  upstream: string | null;
  ahead: number | null;
  behind: number | null;
  staged: number;
  unstaged: number;
  untracked: number;
  conflicted: number;
  clean: boolean;
  checked_at_ms: number;
}

export interface EventEnvelope {
  seq: number;
  id: Uuid;
  schema_version: number;
  event_type: string;
  occurred_at_ms: number;
  project_id: Uuid | null;
  workspace_id: Uuid | null;
  session_id: Uuid | null;
  task_id: Uuid | null;
  agent_id: Uuid | null;
  causation_id: Uuid | null;
  payload: Record<string, unknown>;
}

export interface CoreEnvironment {
  data_dir: string;
  socket_path: string;
  database_path: string;
  core_binary: string | null;
  /** When the Core executable on disk was built (Unix ms). */
  core_binary_modified_ms: number | null;
  core_log: string;
  auto_launch: boolean;
  desktop_version: string;
}

/** `supported` = implemented and verified against the real CLI (ADR 0022). */
export type Support = "supported" | "unsupported" | "unknown";

export interface Capabilities {
  multi_turn: Support;
  streaming_text: Support;
  tool_activity: Support;
  interrupt_turn: Support;
  queue_message_during_turn: Support;
  steer_running_turn: Support;
  resume_session: Support;
  permission_requests: Support;
  denial_reason_to_agent: Support;
  caller_assigned_session_id: Support;
  usage_tokens: Support;
  cost: Support;
  file_read_tool: Support;
}

export type AgentProvider = "claude_code" | "codex";

export interface AgentAvailability {
  provider: string;
  display_name: string;
  installed: boolean;
  executable: string | null;
  version: string | null;
  tested_version: string;
  error: string | null;
  auth: { logged_in: boolean; method: string | null; provider: string | null } | null;
  capabilities: Capabilities;
  checked_at_ms: number;
}

export type SessionState =
  | "starting"
  | "initializing"
  | "idle"
  | "running"
  | "waiting_for_approval"
  | "interrupted"
  | "completed"
  | "failed"
  | "stopped";

export const TERMINAL_STATES: readonly SessionState[] = ["completed", "failed", "stopped"];

export interface Session {
  id: Uuid;
  project_id: Uuid;
  workspace_id: Uuid | null;
  workspace_name?: string | null;
  agent_id: Uuid;
  provider: string;
  native_session_id: string | null;
  cwd: string;
  state: SessionState;
  pid: number | null;
  cli_version: string | null;
  model: string | null;
  started_at_ms: number;
  ended_at_ms: number | null;
  last_activity_at_ms: number;
  exit_code: number | null;
  exit_signal: number | null;
  failure: string | null;
  resume_count: number;
  /** `review` sessions cannot change files (M6, ADR 0027). */
  access: SessionAccess;
  attached: boolean;
  pending_approvals: number;
  capabilities: Capabilities;
}

export type SessionAccess = "write" | "review";

export type ApprovalStatus = "pending" | "approved" | "denied" | "expired" | "cancelled";

export interface Approval {
  id: Uuid;
  project_id: Uuid;
  session_id: Uuid;
  workspace_id?: Uuid | null;
  workspace_name?: string | null;
  agent_id: Uuid | null;
  /** The session's agent. */
  provider: string;
  tool_name: string;
  tool_use_id: string | null;
  input: unknown;
  description: string | null;
  details: unknown;
  status: ApprovalStatus;
  requested_at_ms: number;
  expires_at_ms: number;
  decided_at_ms: number | null;
  decision_source: string | null;
  decided_by: string | null;
  decision_note: string | null;
  /** The policy evaluation that asked for this decision (null for approvals from before policies). */
  policy?: ApprovalPolicy | null;
}

// --- policies (ADR 0016) -----------------------------------------------------

export type OperationCategory =
  | "file_read"
  | "file_write"
  | "file_edit"
  | "file_delete"
  | "directory_list"
  | "search"
  | "shell"
  | "network"
  | "external_tool"
  | "unknown";

export type PolicyDecision = "allow" | "ask" | "deny";
export type PolicyReason = "system_restriction" | "rule" | "default" | "capped" | "auto_mode";
export type RuleScope = "project" | "workspace";

export interface PolicyConditions {
  path_prefix?: string;
  command?: string;
}

export interface PolicyRuleRef {
  id: Uuid;
  revision: number;
  scope: RuleScope;
  workspace_id: Uuid | null;
  decision: PolicyDecision;
  description: string;
}

export interface PolicyRestriction {
  code: string;
  message: string;
}

export interface ApprovalPolicy {
  evaluation_id: Uuid;
  category: OperationCategory;
  reason: PolicyReason;
  explanation: string;
  deciding_rule: PolicyRuleRef | null;
  policy_revision: number;
  /** What approving would reach; absent for approvals recorded before it existed. */
  scope?: ApprovalScope;
}

export interface ApprovalScope {
  kind: "workspace" | "outside_workspace" | "unknown" | "unrestricted";
  outside_paths: string[];
  unresolved_paths: string[];
  note: string;
}

export interface PolicyRule {
  id: Uuid;
  project_id: Uuid;
  workspace_id: Uuid | null;
  workspace_name: string | null;
  scope: RuleScope;
  category: OperationCategory;
  decision: PolicyDecision;
  conditions: PolicyConditions;
  description: string;
  enabled: boolean;
  archived: boolean;
  /** Pass back as `expected_revision` when changing the rule. */
  revision: number;
  created_at_ms: number;
  updated_at_ms: number;
  archived_at_ms: number | null;
}

export interface PolicyRuleRevision {
  revision: number;
  policy_revision: number;
  change: "created" | "updated" | "enabled" | "disabled" | "archived";
  rule: PolicyRule;
  changed_at_ms: number;
  changed_by: string | null;
}

export interface PolicyCategoryInfo {
  category: OperationCategory;
  default_decision: PolicyDecision;
  configurable: boolean;
  allow_supported: boolean;
  path_condition: boolean;
  command_condition: boolean;
}

/** A project's policy settings (ADR 0037). */
export interface PolicySettings {
  project_id: Uuid;
  auto_mode: boolean;
  /** 0 until first changed. */
  revision: number;
  updated_at_ms?: number;
  updated_by?: string;
}

export interface EffectivePolicy {
  project_id: Uuid;
  workspace_id: Uuid | null;
  policy_revision: number;
  system_version: number;
  system_restrictions: PolicyRestriction[];
  categories: PolicyCategoryInfo[];
  project_rules: PolicyRule[];
  workspace_rules: PolicyRule[];
}

export interface PolicyPathFact {
  location: "inside" | "outside" | "unresolved";
  lexical?: string;
  resolved?: string;
  reason?: string;
}

export interface PolicyEvaluation {
  id: Uuid;
  evaluated_at_ms: number;
  trigger: "request" | "reevaluation" | "decision_check";
  project_id: Uuid;
  workspace_id: Uuid | null;
  workspace_name: string | null;
  session_id: Uuid;
  adapter: string;
  tool_name: string;
  category: OperationCategory;
  input: {
    operation?: { tool?: string; paths?: string[]; command?: string; background?: boolean; problem?: string };
    paths?: PolicyPathFact[];
  };
  decision: PolicyDecision;
  reason: PolicyReason;
  explanation: string;
  deciding_rule: PolicyRuleRef | null;
  matched_rules: PolicyRuleRef[];
  restrictions: PolicyRestriction[];
  policy_revision: number;
  system_version: number;
  approval_id: Uuid | null;
  approval_status: ApprovalStatus | null;
}

export interface PolicyRuleResult {
  rule: PolicyRule;
  policy_revision: number;
}

export type LiveKind = "text" | "thinking" | "message_start" | "message_stop" | "gap";

export interface LiveOutput {
  session_id: Uuid;
  seq: number;
  kind: LiveKind;
  text: string;
  missed?: number;
}

export type Delivery = "started_turn" | "queued_for_next_turn";

export type WorkspaceKind = "legacy_checkout" | "worktree";

export type WorkspaceStatus =
  | "provisioning"
  | "ready"
  | "active"
  | "dirty"
  | "integrating"
  | "completed"
  | "failed"
  | "cleanup_pending"
  | "archived"
  | "removed";

export interface Workspace {
  id: Uuid;
  project_id: Uuid;
  kind: WorkspaceKind;
  name: string;
  /** Absolute working directory; the agent's cwd. */
  path: string;
  branch: string | null;
  base_ref: string | null;
  base_commit: string | null;
  status: WorkspaceStatus;
  status_reason: string | null;
  dirty: boolean | null;
  git_checked_at_ms: number | null;
  created_at_ms: number;
  updated_at_ms: number;
  last_activity_at_ms: number;
  archived_at_ms: number | null;
  active_session_id: Uuid | null;
  path_exists: boolean;
}

export interface WorkspaceGitStatus {
  workspace_id: Uuid;
  branch: string | null;
  expected_branch: string | null;
  on_expected_branch: boolean;
  head_oid: string | null;
  detached: boolean;
  base_commit: string | null;
  commits_since_base: number | null;
  staged: number;
  unstaged: number;
  untracked: number;
  conflicted: number;
  clean: boolean;
  checked_at_ms: number;
}

export interface CleanupFinding {
  code: string;
  message: string;
}

export interface CleanupReport {
  allowed: boolean;
  blockers: CleanupFinding[];
  warnings: CleanupFinding[];
  will_remove_directory: boolean;
  branch: string;
  branch_kept: boolean;
  ignored_files: string[];
  ignored_total: number;
  unmerged_commits: number | null;
  merged_into: string | null;
  /** Everything under the worktree that removal would delete (full scan). */
  entries_total: number | null;
  bytes_total: number | null;
  tree_fingerprint: string | null;
  confirmation: string;
}

type NoParams = Record<string, never>;

/** Methods the desktop bridge allows the UI to call. */
export interface CoreMethods extends TaskMethods, IntegrationMethods {
  "system.health": { params: NoParams; result: HealthReport };
  "projects.list": { params: NoParams; result: { projects: Project[] } };
  "projects.get": { params: { project_id: Uuid }; result: { project: Project } };
  "projects.add": { params: { path: string }; result: { project: Project } };
  "projects.remove": { params: { project_id: Uuid }; result: { project_id: Uuid } };
  "projects.git_status": { params: { project_id: Uuid }; result: GitStatusReport };
  "events.list": {
    params: {
      after_seq?: number;
      project_id?: Uuid;
      session_id?: Uuid;
      task_id?: Uuid;
      event_types?: string[];
      limit?: number;
    };
    result: { events: EventEnvelope[] };
  };
  "agents.list": { params: { refresh?: boolean }; result: { agents: AgentAvailability[] } };
  "workspaces.list": {
    params: { project_id: Uuid; include_archived?: boolean };
    result: { workspaces: Workspace[] };
  };
  "workspaces.get": { params: { workspace_id: Uuid }; result: { workspace: Workspace; sessions: Session[] } };
  "workspaces.create": {
    params: { project_id: Uuid; name?: string; base_ref?: string };
    result: { workspace: Workspace };
  };
  "workspaces.git_status": { params: { workspace_id: Uuid }; result: WorkspaceGitStatus };
  "workspaces.verify": { params: { workspace_id: Uuid }; result: { workspace: Workspace } };
  "workspaces.archive": { params: { workspace_id: Uuid }; result: { workspace: Workspace } };
  "workspaces.restore": { params: { workspace_id: Uuid }; result: { workspace: Workspace } };
  "workspaces.complete": { params: { workspace_id: Uuid }; result: { workspace: Workspace } };
  "workspaces.cleanup_check": {
    params: { workspace_id: Uuid };
    result: { workspace: Workspace; report: CleanupReport };
  };
  "workspaces.cleanup": {
    params: { workspace_id: Uuid; confirmation: string };
    result: { workspace: Workspace; report: CleanupReport };
  };
  "sessions.start": {
    params: { workspace_id: Uuid; project_id?: Uuid; provider?: AgentProvider; prompt?: string; model?: string };
    result: { session: Session };
  };
  "sessions.list": {
    params: { project_id?: Uuid; workspace_id?: Uuid; live_only?: boolean; limit?: number };
    result: { sessions: Session[] };
  };
  "sessions.get": { params: { session_id: Uuid }; result: { session: Session; live_text: string; live_seq: number } };
  "sessions.send": {
    params: { session_id: Uuid; text: string; queue_if_busy?: boolean };
    result: { delivery: Delivery };
  };
  "sessions.interrupt": { params: { session_id: Uuid }; result: { session: Session } };
  "sessions.stop": { params: { session_id: Uuid }; result: { session: Session } };
  "sessions.resume": { params: { session_id: Uuid; prompt?: string }; result: { session: Session } };
  "approvals.list": {
    params: { status?: ApprovalStatus; session_id?: Uuid; project_id?: Uuid; limit?: number };
    result: { approvals: Approval[] };
  };
  "approvals.decide": {
    params: { approval_id: Uuid; decision: "approve" | "deny"; note?: string };
    result: { approval: Approval };
  };
  "policies.list": {
    params: { project_id: Uuid; include_archived?: boolean };
    result: { rules: PolicyRule[]; policy_revision: number };
  };
  "policies.get": { params: { rule_id: Uuid }; result: { rule: PolicyRule; history: PolicyRuleRevision[] } };
  "policies.create": {
    params: {
      project_id: Uuid;
      workspace_id?: Uuid;
      category: OperationCategory;
      decision: PolicyDecision;
      conditions?: PolicyConditions;
      description: string;
      enabled?: boolean;
    };
    result: PolicyRuleResult;
  };
  "policies.update": {
    params: {
      rule_id: Uuid;
      expected_revision: number;
      decision?: PolicyDecision;
      conditions?: PolicyConditions;
      description?: string;
    };
    result: PolicyRuleResult;
  };
  "policies.set_enabled": {
    params: { rule_id: Uuid; expected_revision: number; enabled: boolean };
    result: PolicyRuleResult;
  };
  "policies.archive": { params: { rule_id: Uuid; expected_revision: number }; result: PolicyRuleResult };
  "policies.effective": { params: { project_id: Uuid; workspace_id?: Uuid }; result: EffectivePolicy };
  "policies.settings": { params: { project_id: Uuid }; result: PolicySettings };
  "policies.set_auto_mode": {
    params: { project_id: Uuid; enabled: boolean; expected_revision: number };
    result: { settings: PolicySettings; policy_revision: number };
  };
  "policies.evaluations": {
    params: {
      project_id?: Uuid;
      workspace_id?: Uuid;
      session_id?: Uuid;
      rule_id?: Uuid;
      decision?: PolicyDecision;
      limit?: number;
    };
    result: { evaluations: PolicyEvaluation[] };
  };
}

export type CoreMethod = keyof CoreMethods;

// --- tasks (M6, ADRs 0023-0027) -------------------------------------------------

export type TaskStatus =
  | "draft"
  | "ready"
  | "blocked"
  | "assigned"
  | "in_progress"
  | "awaiting_review"
  | "in_review"
  | "completed"
  | "failed"
  | "cancelled";
export type TaskKind = "investigation" | "implementation" | "general";
export type TaskPriority = "low" | "normal" | "high" | "urgent";
export type TaskRole = "investigate" | "implement" | "review";
export type AssignmentStatus = "pending" | "active" | "reported" | "failed" | "cancelled";
export type MessageKind =
  | "task_assignment"
  | "status_update"
  | "info_request"
  | "findings"
  | "proposal"
  | "review_result"
  | "blocker"
  | "delegation_request"
  | "completion_report"
  | "operator_note"
  | "malformed_report";
export type Severity = "info" | "low" | "medium" | "high" | "critical";
export type ProposalStatus = "pending" | "approved" | "rejected";
export type TaskReadiness = "satisfied" | "waiting" | "broken";

export interface TaskAssignment {
  id: Uuid;
  task_id: Uuid;
  role: TaskRole;
  provider: AgentProvider;
  workspace_id: Uuid;
  session_id: Uuid | null;
  status: AssignmentStatus;
  context: Record<string, unknown> | null;
  context_version: number | null;
  outcome: string | null;
  created_at_ms: number;
  started_at_ms: number | null;
  ended_at_ms: number | null;
  /** `operator` or `workflow` (M8.1). */
  origin: "operator" | "workflow";
  /** The workflow step, e.g. `implement#2`. */
  step_key: string | null;
  /** The model asked for; null = the CLI's default. */
  model: string | null;
  /** The model the CLI reported it runs. */
  actual_model: string | null;
}

// --- workflows (M8.1, ADR 0033) ---

export interface RoleBinding {
  provider: AgentProvider;
  /** Passed to the CLI exactly; null = the CLI's own default. */
  model: string | null;
}

export interface RoleBindings {
  investigate: RoleBinding;
  implement: RoleBinding;
  review: RoleBinding;
  /** null = same as implement. */
  correct: RoleBinding | null;
}

export type RoleBindingsPatch = Partial<Record<keyof RoleBindings, RoleBinding>>;

export interface WorkflowSettingsInfo {
  project_id: Uuid;
  bindings: RoleBindings;
  /** 0 while the project uses Switchyard's defaults. */
  revision: number;
  warnings: string[];
}

export type WorkflowStatus = "pending" | "running" | "waiting" | "paused" | "completed" | "delivered" | "cancelled" | "failed";

/** Where delivery stands (M8.2), derived from the task's integration. */
export type DeliveryPhase =
  | "preparing"
  | "needs_commit"
  | "validation_starting"
  | "awaiting_validation_authorization"
  | "validating"
  | "validation_failed"
  | "blocked"
  | "awaiting_approval"
  | "integrating"
  | "delivered"
  | "delivery_failed"
  | "cancelled";

export interface WorkflowInfo {
  task_id: Uuid;
  definition: string;
  version: number;
  status: WorkflowStatus;
  /** `organization`: an agent organization works on the objective (M8.3). */
  /** `triage`: reasoning decides whether review findings are fixed or disputed (ADR 0038). */
  stage:
    | "investigation"
    | "implementation"
    | "review"
    | "correction"
    | "triage"
    | "testing"
    | "delivery"
    | "organization"
    | null;
  step_key: string | null;
  reason: string | null;
  resume_action: string | null;
  bindings: RoleBindings;
  workspace_id: Uuid | null;
  corrections: number;
  max_corrections: number;
  warnings: string[];
  started_by: string;
  created_at_ms: number;
  updated_at_ms: number;
  ended_at_ms: number | null;
  integration_id: Uuid | null;
  delivery: DeliveryPhase | null;
  /** Reasoning's plan when it split the task, in order (ADR 0040). */
  plan?: { title: string; description?: string; done: boolean }[];
  /** The subtask being worked on, 1-based. */
  subtask?: number | null;
  /** The step's agent asked this and waits for the answer (sent with `tasks.note`); Resume is refused until then. */
  question?: string | null;
}

// --- agent organizations (M8.3) ---------------------------------------------------

export type OrgStatus = "running" | "waiting" | "paused" | "done" | "cancelled";
export type OrgTaskState =
  | "planning"
  | "delegated"
  | "waiting"
  | "ready"
  | "working"
  | "reported"
  | "accepted"
  | "cancelled";

export interface OrgAgentInfo {
  id: Uuid;
  key: string;
  title: string;
  kind: "supervisor" | "specialist";
  /** `null`: the lead, who reports to you. */
  reports_to: string | null;
  responsibility: string;
  scope: string[];
  capabilities: string[];
  provider: AgentProvider;
  model: string | null;
  /** Why Switchyard chose this provider and model. */
  selection: string;
  status: "idle" | "active" | "done";
  created_by: string;
  depth: number;
}

export interface OrgTaskInfo {
  key: string;
  task_id: Uuid | null;
  title: string;
  description: string | null;
  work: "coordination" | "implementation" | "investigation" | "documentation" | "testing" | "review";
  parent: string | null;
  owner: string;
  scope: string[];
  acceptance: string[];
  depends_on: string[];
  state: OrgTaskState;
  corrections: number;
  summary: string | null;
  created_by: string;
}

export interface OrgContextEntry {
  seq: number;
  kind: string;
  status: "verified" | "claim" | "proposal" | "decided" | "superseded";
  text: string;
  task: string | null;
  source: string;
  supersedes: number | null;
  created_at_ms: number;
}

export interface OrganizationInfo {
  task_id: Uuid;
  workspace_id: Uuid;
  status: OrgStatus;
  reason: string | null;
  agents: OrgAgentInfo[];
  tasks: OrgTaskInfo[];
  activity: { kind: "idle" | "supervising" | "working"; agent: string | null; task: string | null; items: string[] };
  queued_checkpoints: number;
  limits: Record<string, unknown>;
  usage: {
    supervision_turns: number;
    max_supervision_turns: number;
    agents: number;
    max_agents: number;
    tasks: number;
    max_tasks: number;
    cost_usd: number;
    tokens: number;
  };
  question: string | null;
  question_from: string | null;
  context: OrgContextEntry[];
  final_summary: string | null;
  created_at_ms: number;
  updated_at_ms: number;
}

export interface OrgLimits {
  max_depth: number;
  max_agents: number;
  max_tasks: number;
  max_concurrent_sessions: number;
  max_corrections: number;
  max_retries: number;
  max_supervision_turns: number;
  max_minutes: number;
  max_cost_usd: number | null;
  max_tokens: number | null;
}

export type SkillLevel = "unknown" | "unsupported" | "basic" | "good" | "strong";

export interface OrgEstimate {
  provider: string;
  model?: string | null;
  skills: Record<string, SkillLevel>;
  relative_cost?: number | null;
  relative_speed?: number | null;
}

export interface OrgProfileInfo {
  provider: AgentProvider;
  model: string | null;
  display: string;
  summary: string;
  runtime: Record<string, { level: SkillLevel; basis: "verified" | "reported" | "configured" | "none" }>;
  skills: Record<string, { level: SkillLevel; basis: "verified" | "reported" | "configured" | "none" }>;
}

export interface OrgSettingsInfo {
  project_id: Uuid;
  limits: OrgLimits;
  estimates: OrgEstimate[];
  overrides: { skill: string; provider: string; model?: string | null }[];
  revision: number;
  profiles: OrgProfileInfo[];
  skills: string[];
}

export interface ModelOption {
  id: string;
  display_name: string;
  is_default: boolean;
}

export interface ModelCatalog {
  provider: AgentProvider;
  models: ModelOption[];
  accepts_full_names: boolean;
  source: string;
  error: string | null;
}

export interface Task {
  id: Uuid;
  project_id: Uuid;
  parent_task_id: Uuid | null;
  title: string;
  description: string | null;
  kind: TaskKind;
  priority: TaskPriority;
  acceptance_criteria: string[];
  status: TaskStatus;
  status_reason: string | null;
  result: Record<string, unknown> | null;
  failure: string | null;
  origin: "operator" | "proposal";
  origin_proposal_id: Uuid | null;
  revision: number;
  created_at_ms: number;
  updated_at_ms: number;
  completed_at_ms: number | null;
  open_assignment: TaskAssignment | null;
  dependency_ids: Uuid[];
  pending_proposals: number;
  open_findings: number;
}

/** A structured message. A message from an agent is a claim or a request; it never authorizes anything. */
export interface TaskMessage {
  id: Uuid;
  task_id: Uuid;
  assignment_id: Uuid | null;
  kind: MessageKind;
  sender: "agent" | "operator" | "core";
  sender_session_id: Uuid | null;
  recipient: "operator" | "agent" | "task";
  payload: Record<string, unknown>;
  causation_id: string | null;
  delivery: "recorded" | "pending" | "delivered";
  delivered_at_ms: number | null;
  created_at_ms: number;
}

export interface TaskFinding {
  id: Uuid;
  task_id: Uuid;
  message_id: Uuid;
  assignment_id: Uuid | null;
  severity: Severity;
  title: string;
  explanation: string | null;
  suggestion: string | null;
  path: string | null;
  line: number | null;
  status: "open" | "resolved" | "dismissed";
  created_at_ms: number;
  resolved_at_ms: number | null;
}

export interface ProposedSubtask {
  title: string;
  description?: string | null;
  kind?: TaskKind;
  acceptance_criteria?: string[];
  depends_on?: (number | Uuid)[];
}

export interface TaskProposal {
  id: Uuid;
  task_id: Uuid;
  message_id: Uuid;
  position: number;
  proposal: ProposedSubtask;
  status: ProposalStatus;
  decided_by: string | null;
  decision_note: string | null;
  created_task_id: Uuid | null;
  created_at_ms: number;
  decided_at_ms: number | null;
}

export interface TaskRef {
  id: Uuid;
  title: string;
  status: TaskStatus;
}

export interface TaskDetail {
  task: Task;
  dependencies: TaskRef[];
  dependents: TaskRef[];
  readiness: TaskReadiness;
  assignments: TaskAssignment[];
  messages: TaskMessage[];
  findings: TaskFinding[];
  proposals: TaskProposal[];
  workflow: WorkflowInfo | null;
  /** The workflow's test gate runs, oldest first (ADR 0039). */
  test_runs?: TestRunInfo[];
}

export type TestKind = "unit" | "integration" | "regression" | "browser";

/** One command of a test gate run. */
export interface TestResult {
  kind: TestKind;
  name?: string | null;
  command: string;
  /** The policy's decision: allow, ask or deny. */
  policy: string;
  /** passed, failed, timed_out, error, refused, skipped, cancelled. */
  status: string;
  exit_code?: number | null;
  duration_ms?: number;
  stdout_tail?: string;
  stderr_tail?: string;
  detail?: string | null;
}

/** One run of the workflow's test gate (ADR 0039). */
export interface TestRunInfo {
  id: Uuid;
  step_key: string;
  status: "passed" | "failed" | "needs_authorization";
  summary: string;
  results: TestResult[];
  ui_change: boolean;
  authorized_by?: string;
  started_at_ms: number;
  ended_at_ms: number;
}

type TaskResult = { task: Task };
type TaskAssignmentResult = { task: Task; assignment: TaskAssignment };
type TaskDecision = { task_id: Uuid; note?: string };

export interface TaskMethods {
  "tasks.list": { params: { project_id?: Uuid; include_closed?: boolean; limit?: number }; result: { tasks: Task[] } };
  "tasks.get": { params: { task_id: Uuid }; result: TaskDetail };
  "tasks.create": {
    params: {
      project_id: Uuid;
      title: string;
      description?: string;
      kind?: TaskKind;
      priority?: TaskPriority;
      acceptance_criteria?: string[];
      depends_on?: Uuid[];
      parent_task_id?: Uuid;
      publish?: boolean;
    };
    result: TaskResult;
  };
  "tasks.update": {
    params: {
      task_id: Uuid;
      expected_revision: number;
      title?: string;
      description?: string;
      kind?: TaskKind;
      priority?: TaskPriority;
      acceptance_criteria?: string[];
    };
    result: TaskResult;
  };
  "tasks.publish": { params: { task_id: Uuid }; result: TaskResult };
  "tasks.add_dependency": { params: { task_id: Uuid; depends_on: Uuid }; result: TaskResult };
  "tasks.remove_dependency": { params: { task_id: Uuid; depends_on: Uuid }; result: TaskResult };
  "tasks.assign": {
    params: { task_id: Uuid; role: TaskRole; provider: AgentProvider; workspace_id: Uuid; start?: boolean; model?: string };
    result: TaskAssignmentResult;
  };
  "tasks.start_assignment": { params: { assignment_id: Uuid }; result: TaskAssignmentResult };
  "tasks.cancel_assignment": { params: { assignment_id: Uuid; reason?: string }; result: TaskAssignmentResult };
  "tasks.note": { params: { task_id: Uuid; text: string }; result: { message: TaskMessage; delivered: boolean } };
  "tasks.complete": { params: TaskDecision; result: TaskResult };
  "tasks.fail": { params: TaskDecision; result: TaskResult };
  "tasks.cancel": { params: TaskDecision; result: TaskResult };
  "tasks.retry": { params: TaskDecision; result: TaskResult };
  "tasks.decide_proposal": {
    params: { proposal_id: Uuid; approve: boolean; note?: string };
    result: { proposal: TaskProposal; created_task: Task | null };
  };
}

// --- integrations (M7, ADRs 0028-0031) -------------------------------------------

export type IntegrationStatus =
  | "open"
  | "conflicted"
  | "resolving"
  | "validating"
  | "ready"
  | "integrating"
  | "integrated"
  | "failed"
  | "cancelled";
export type RunStatus = "running" | "passed" | "failed" | "cancelled" | "interrupted";
export type StepStatus =
  | "pending"
  | "running"
  | "passed"
  | "failed"
  | "timed_out"
  | "cancelled"
  | "refused"
  | "skipped"
  | "error";

export interface Blocker {
  code: string;
  message: string;
}

export interface IntegrationFacts {
  workspace_valid: { Ok: null } | { Err: string };
  live_session: boolean;
  uncommitted_changes: boolean;
  workspace_busy: string[];
  source_ahead: number;
  has_changes: boolean;
  merge: { kind: "up_to_date" } | { kind: "clean" } | { kind: "conflicts"; files: string[] };
  target_checkout: { path: string; has_local_changes: boolean; busy: string[] } | null;
  identity: { Ok: string } | { Err: string };
  task: { Ok: null } | { Err: string } | null;
  review: { required: boolean; satisfied: boolean; detail: string };
  validations_configured: number;
  validation_passed: boolean;
  validation_failed: boolean;
}

export interface Integration {
  id: Uuid;
  project_id: Uuid;
  task_id: Uuid | null;
  workspace_id: Uuid;
  workspace_name: string | null;
  source_branch: string;
  source_commit: string;
  target_branch: string;
  target_commit: string;
  base_commit: string | null;
  status: IntegrationStatus;
  status_reason: string | null;
  blockers: Blocker[];
  preflight: {
    facts?: IntegrationFacts;
    blockers?: Blocker[];
    summary?: { commits_ahead: number; files_changed: number; additions: number; deletions: number; source_tree: string };
    require_review?: boolean;
  };
  prepared_at_ms: number;
  approved_by: string | null;
  approved_at_ms: number | null;
  merge_commit: string | null;
  method: "ref_update" | "checkout_fast_forward" | null;
  failure: string | null;
  revision: number;
  created_at_ms: number;
  updated_at_ms: number;
  completed_at_ms: number | null;
  /** The latest Core-owned operation (ADR 0032): running, or how the last one ended. */
  operation: IntegrationOperation | null;
}

export type IntegrationOperationKind = "approve" | "commit_workspace" | "update_from_target" | "abort_resolution" | "validate";

/** A Core-owned operation. It continues when the client disconnects. */
export interface IntegrationOperation {
  id: Uuid;
  kind: IntegrationOperationKind;
  status: "running" | "completed" | "failed" | "interrupted";
  requested_by: string;
  error: string | null;
  started_at_ms: number;
  ended_at_ms: number | null;
}

export interface CommitSummary {
  oid: string;
  parents: string[];
  author_name: string;
  author_email: string;
  authored_at_ms: number;
  subject: string;
}

export interface ChangedFileInfo {
  path: string;
  old_path: string | null;
  kind: "added" | "modified" | "deleted" | "renamed" | "copied" | "type_changed" | "unmerged" | "unknown";
  additions: number | null;
  deletions: number | null;
  binary: boolean;
}

export interface IntegrationChanges {
  source_commit: string;
  target_commit: string;
  base_commit: string | null;
  diverged: boolean;
  commits: CommitSummary[];
  commits_total: number;
  files: ChangedFileInfo[];
  files_total: number;
  additions: number;
  deletions: number;
  truncated: boolean;
  uncommitted: ChangedFileInfo[];
}

export interface ValidationCommandConfig {
  id: Uuid;
  name: string;
  argv: string[];
  command_line: string;
  timeout_secs: number;
  enabled: boolean;
  position: number;
}

export interface ValidationPlanEntry {
  command_id: Uuid;
  name: string;
  command_line: string;
  policy_decision: "allow" | "ask" | "deny";
  policy_explanation: string;
}

export interface ValidationStepInfo {
  id: Uuid;
  position: number;
  name: string;
  command_line: string;
  cwd: string;
  timeout_secs: number;
  policy_decision: string;
  policy_explanation: string;
  authorized_by: string | null;
  status: StepStatus;
  exit_code: number | null;
  signal: number | null;
  stdout: string | null;
  stderr: string | null;
  stdout_truncated: boolean;
  stderr_truncated: boolean;
  detail: string | null;
  started_at_ms: number | null;
  ended_at_ms: number | null;
}

export interface ValidationRunInfo {
  id: Uuid;
  source_commit: string;
  status: RunStatus;
  requested_by: string | null;
  started_at_ms: number;
  ended_at_ms: number | null;
  steps: ValidationStepInfo[];
}

export interface IntegrationSettingsInfo {
  project_id: Uuid;
  require_review: boolean;
  revision: number;
  commands: ValidationCommandConfig[];
}

export interface IntegrationDetail {
  integration: Integration;
  runs: ValidationRunInfo[];
  plan: ValidationPlanEntry[];
}

type IntegrationResult = { integration: Integration };
type IntegrationId = { integration_id: Uuid };

export interface IntegrationMethods {
  "integrations.list": {
    params: { project_id?: Uuid; task_id?: Uuid; include_closed?: boolean; limit?: number };
    result: { integrations: Integration[] };
  };
  "integrations.get": { params: IntegrationId; result: IntegrationDetail };
  "integrations.create": {
    params: { task_id?: Uuid; workspace_id?: Uuid; target_branch?: string };
    result: IntegrationResult;
  };
  "integrations.refresh": { params: IntegrationId; result: IntegrationResult };
  "integrations.changes": { params: IntegrationId; result: IntegrationChanges };
  "integrations.patch": {
    params: { integration_id: Uuid; path?: string; uncommitted?: boolean };
    result: { patch: string; truncated: boolean };
  };
  "integrations.commit_workspace": { params: { integration_id: Uuid; message: string }; result: IntegrationResult };
  "integrations.update_from_target": { params: IntegrationId; result: IntegrationResult };
  "integrations.abort_resolution": { params: IntegrationId; result: IntegrationResult };
  "integrations.validate": {
    /** Each authorization names the exact command line the operator saw. */
    params: { integration_id: Uuid; authorize?: { command_id: Uuid; command_line: string }[] };
    result: { integration: Integration; run: ValidationRunInfo };
  };
  "integrations.cancel_validation": { params: IntegrationId; result: IntegrationResult };
  "integrations.approve": {
    params: { integration_id: Uuid; source_commit: string; target_commit: string };
    result: IntegrationResult;
  };
  "integrations.cancel": { params: { integration_id: Uuid; reason?: string }; result: IntegrationResult };
  "integrations.settings": { params: { project_id: Uuid }; result: IntegrationSettingsInfo };
  "integrations.set_settings": { params: { project_id: Uuid; require_review: boolean }; result: IntegrationSettingsInfo };
  "integrations.set_command": {
    params: {
      project_id: Uuid;
      command_id?: Uuid;
      name: string;
      argv: string[];
      timeout_secs: number;
      enabled?: boolean;
      position?: number;
    };
    result: IntegrationSettingsInfo;
  };
  "integrations.delete_command": { params: { command_id: Uuid }; result: IntegrationSettingsInfo };
  "agents.models": { params: { provider: AgentProvider; refresh?: boolean }; result: { catalog: ModelCatalog } };
  "workflows.settings": { params: { project_id: Uuid }; result: WorkflowSettingsInfo };
  "workflows.set_settings": { params: { project_id: Uuid; bindings: RoleBindings }; result: WorkflowSettingsInfo };
  "workflows.start": {
    params: {
      task_id: Uuid;
      overrides?: RoleBindingsPatch;
      workspace_id?: Uuid;
      definition?: "engineering" | "organization";
      lead?: RoleBinding;
    };
    result: { workflow: WorkflowInfo };
  };
  "workflows.get": { params: { task_id: Uuid }; result: { workflow: WorkflowInfo | null } };
  "workflows.pause": { params: { task_id: Uuid }; result: { workflow: WorkflowInfo } };
  "workflows.resume": { params: { task_id: Uuid }; result: { workflow: WorkflowInfo } };
  "workflows.cancel": { params: { task_id: Uuid; reason?: string }; result: { workflow: WorkflowInfo } };
  "workflows.request_correction": { params: { task_id: Uuid; note?: string }; result: { workflow: WorkflowInfo } };
  "organizations.get": { params: { task_id: Uuid }; result: { organization: OrganizationInfo | null } };
  "organizations.answer": { params: { task_id: Uuid; text: string }; result: { organization: OrganizationInfo | null } };
  "organizations.settings": { params: { project_id: Uuid }; result: OrgSettingsInfo };
  "organizations.set_settings": {
    params: { project_id: Uuid; limits?: Partial<OrgLimits>; estimates?: OrgEstimate[]; overrides?: OrgSettingsInfo["overrides"] };
    result: OrgSettingsInfo;
  };
}
