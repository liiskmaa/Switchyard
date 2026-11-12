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
  | { state: "disconnected"; error: string; retry_in_ms: number; launched_core: boolean }
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
  core_log: string;
  auto_launch: boolean;
  desktop_version: string;
}

export interface Capabilities {
  multi_turn: boolean;
  streaming_text: boolean;
  interrupt_turn: boolean;
  queue_message_during_turn: boolean;
  steer_running_turn: boolean;
  resume_session: boolean;
  permission_requests: boolean;
  caller_assigned_session_id: boolean;
}

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
  attached: boolean;
  pending_approvals: number;
  capabilities: Capabilities;
}

export type ApprovalStatus = "pending" | "approved" | "denied" | "expired" | "cancelled";

export interface Approval {
  id: Uuid;
  project_id: Uuid;
  session_id: Uuid;
  workspace_id?: Uuid | null;
  workspace_name?: string | null;
  agent_id: Uuid | null;
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
export interface CoreMethods {
  "system.health": { params: NoParams; result: HealthReport };
  "projects.list": { params: NoParams; result: { projects: Project[] } };
  "projects.get": { params: { project_id: Uuid }; result: { project: Project } };
  "projects.add": { params: { path: string }; result: { project: Project } };
  "projects.remove": { params: { project_id: Uuid }; result: { project_id: Uuid } };
  "projects.git_status": { params: { project_id: Uuid }; result: GitStatusReport };
  "events.list": {
    params: { after_seq?: number; project_id?: Uuid; session_id?: Uuid; event_types?: string[]; limit?: number };
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
    params: { workspace_id: Uuid; project_id?: Uuid; prompt?: string; model?: string };
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
}

export type CoreMethod = keyof CoreMethods;
