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
