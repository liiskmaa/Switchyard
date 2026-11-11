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
    params: { after_seq?: number; project_id?: Uuid; event_types?: string[]; limit?: number };
    result: { events: EventEnvelope[] };
  };
}

export type CoreMethod = keyof CoreMethods;
