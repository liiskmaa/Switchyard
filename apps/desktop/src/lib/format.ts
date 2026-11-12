import type { EventEnvelope } from "../types/protocol";

export function formatRelative(thenMs: number, nowMs: number): string {
  const seconds = Math.round((nowMs - thenMs) / 1000);
  if (seconds < 5) return "just now";
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}

export function formatDuration(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const d = Math.floor(total / 86400);
  const h = Math.floor((total % 86400) / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  if (d > 0) return `${d}d ${h}h`;
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m ${s}s`;
  return `${s}s`;
}

export function formatTimestamp(ms: number): string {
  return new Date(ms).toLocaleString(undefined, {
    year: "numeric",
    month: "short",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

export const shortId = (id: string): string => id.slice(-8);
export const shortOid = (oid: string): string => oid.slice(0, 8);

function str(payload: Record<string, unknown>, key: string): string | undefined {
  const value = payload[key];
  return typeof value === "string" ? value : undefined;
}

/** One-line human description of a journal event. */
export function describeEvent(event: EventEnvelope): string {
  const p = event.payload;
  switch (event.event_type) {
    case "core.started":
      return `Core ${str(p, "core_version") ?? "?"} started`;
    case "core.stopping":
      return `Core stopping${str(p, "reason") ? ` (${str(p, "reason")})` : ""}`;
    case "project.registered":
      return `Registered ${str(p, "name") ?? "project"} at ${str(p, "root_path") ?? "?"}`;
    case "project.removed":
      return `Removed registration for ${str(p, "root_path") ?? "project"}`;
    case "agent.started":
      return `${p["resumed"] === true ? "Resumed" : "Started"} Claude Code ${str(p, "cli_version") ?? ""} (pid ${String(p["pid"] ?? "?")})`;
    case "agent.ready":
      return `Agent ready${str(p, "model") ? ` · ${str(p, "model")}` : ""}`;
    case "agent.state_changed":
      return `${str(p, "from") ?? "?"} → ${str(p, "to") ?? "?"}`;
    case "agent.turn_started":
      return `${p["queued"] === true ? "Queued" : "Task"}: ${str(p, "prompt") ?? ""}`;
    case "agent.message":
      return str(p, "text") ?? "";
    case "agent.tool_started":
      return `${str(p, "tool_name") ?? "Tool"} ${toolSummary(str(p, "tool_name"), p["input"])}`;
    case "agent.tool_completed":
      return `${str(p, "tool_name") ?? "Tool"} completed`;
    case "agent.tool_failed":
      return `${str(p, "tool_name") ?? "Tool"} ${p["rejected"] === true ? "not run" : "failed"}: ${str(p, "output") ?? ""}`;
    case "agent.turn_completed":
      return p["interrupted"] === true
        ? "Turn interrupted"
        : p["success"] === true
          ? "Turn completed"
          : `Turn ended with an error (${str(p, "reason") ?? "unknown"})`;
    case "agent.permission_requested":
      return `Permission requested: ${str(p, "tool_name") ?? "tool"} ${toolSummary(str(p, "tool_name"), p["input"])}`;
    case "agent.permission_resolved":
      return `${str(p, "tool_name") ?? "Tool"} ${str(p, "status") ?? "resolved"} (${str(p, "source") ?? ""})`;
    case "agent.interrupted":
      return "Interrupt acknowledged";
    case "agent.session_resumed":
      return `Session resumed (#${String(p["resume_count"] ?? "?")})`;
    case "agent.warning":
      return `Warning: ${str(p, "message") ?? ""}`;
    case "agent.exited":
      return `Process exited (${p["exit_code"] !== null && p["exit_code"] !== undefined ? `code ${String(p["exit_code"])}` : `signal ${String(p["exit_signal"] ?? "?")}`}) → ${str(p, "final_state") ?? ""}`;
    case "agent.failed":
      return `Agent failed: ${str(p, "detail") ?? str(p, "reason") ?? ""}`;
    case "workspace.created":
      return `Workspace “${str(p, "name") ?? "?"}” created on ${str(p, "branch") ?? "?"}`;
    case "workspace.state_changed":
      return `Workspace ${str(p, "from") ?? "?"} → ${str(p, "to") ?? "?"}${str(p, "reason") ? ` (${str(p, "reason")})` : ""}`;
    case "workspace.cleanup_requested":
      return p["allowed"] === true
        ? "Workspace cleanup confirmed"
        : `Workspace cleanup refused: ${Array.isArray(p["blockers"]) ? (p["blockers"] as string[]).join(", ") : ""}`;
    case "workspace.removed":
      return `Workspace removed from disk; branch ${str(p, "branch") ?? "?"} kept`;
    case "workspace.cleanup_failed":
      return `Workspace cleanup failed (${str(p, "reason") ?? "?"}); files preserved`;
    case "files.changed": {
      const changes = Array.isArray(p["changes"]) ? (p["changes"] as { path?: string; kind?: string }[]) : [];
      const head = changes
        .slice(0, 3)
        .map((c) => `${c.kind ?? "?"} ${c.path ?? "?"}`)
        .join(", ");
      const more = changes.length > 3 ? ` and ${changes.length - 3} more` : "";
      return `Files: ${head}${more}`;
    }
    default:
      return event.event_type;
  }
}

/** Merges events by sequence number, keeping the newest `limit`, ascending. */
export function mergeEvents(current: EventEnvelope[], incoming: EventEnvelope[], limit: number): EventEnvelope[] {
  const bySeq = new Map<number, EventEnvelope>();
  for (const e of current) bySeq.set(e.seq, e);
  for (const e of incoming) bySeq.set(e.seq, e);
  const merged = [...bySeq.values()].sort((a, b) => a.seq - b.seq);
  return merged.length > limit ? merged.slice(merged.length - limit) : merged;
}

/** Short human summary of a tool input (command, path, pattern). */
export function toolSummary(tool: string | undefined, input: unknown): string {
  if (typeof input !== "object" || input === null) return "";
  const i = input as Record<string, unknown>;
  const pick = (k: string) => (typeof i[k] === "string" ? (i[k] as string) : undefined);
  switch (tool) {
    case "Bash":
      return pick("command") ?? "";
    case "Write":
    case "Edit":
    case "Read":
    case "NotebookEdit":
      return pick("file_path") ?? pick("notebook_path") ?? "";
    case "Glob":
    case "Grep":
      return pick("pattern") ?? "";
    default:
      return "";
  }
}
