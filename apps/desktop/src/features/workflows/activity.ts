import type { EventEnvelope } from "../../types/protocol";

/** What a live agent session is doing, from its journal events only. */
export type ActivityState = "running_tool" | "thinking" | "waiting_approval" | "idle";

export interface ActivityLine {
  at: number;
  kind: "tool" | "message" | "approval" | "turn";
  text: string;
}

export interface Activity {
  state: ActivityState;
  /** What it is doing now, in a few words. */
  now: string;
  /** Since when it has been doing that. */
  sinceMs: number;
  /** The latest actions, newest first. */
  recent: ActivityLine[];
}

const str = (v: unknown): string | null => (typeof v === "string" ? v : null);

/** One line for a tool call: its command, file or name. */
export function toolText(payload: Record<string, unknown>): string {
  const input = (payload["input"] ?? {}) as Record<string, unknown>;
  const name = str(payload["tool_name"]) ?? "a tool";
  const command = str(input["command"]) ?? str(input["title"]);
  if (command) return command.replace(/\s+/g, " ").trim();
  const path = str(input["file_path"]) ?? str(input["path"]) ?? str(input["pattern"]);
  if (path) return `${name} ${path.split("/").slice(-2).join("/")}`;
  return name;
}

const clip = (s: string, n: number) => (s.length > n ? `${s.slice(0, n - 1)}…` : s);

/** The session's current activity from its events (ascending by seq). */
export function activityOf(events: EventEnvelope[]): Activity {
  let state: ActivityState = "idle";
  let now = "Idle";
  let sinceMs = events[events.length - 1]?.occurred_at_ms ?? 0;
  const open = new Map<string, { at: number; text: string }>();
  const recent: ActivityLine[] = [];
  let pendingApproval: number | null = null;
  for (const e of events) {
    const p = e.payload;
    const at = e.occurred_at_ms;
    switch (e.event_type) {
      case "agent.turn_started":
        state = "thinking";
        now = "Thinking";
        sinceMs = at;
        recent.push({ at, kind: "turn", text: "Started a turn" });
        break;
      case "agent.tool_started": {
        pendingApproval = null;
        const text = toolText(p);
        open.set(str(p["tool_use_id"]) ?? String(e.seq), { at, text });
        recent.push({ at, kind: "tool", text });
        break;
      }
      case "agent.tool_completed":
      case "agent.tool_failed":
        open.delete(str(p["tool_use_id"]) ?? "");
        if (open.size === 0 && state !== "idle") {
          state = "thinking";
          now = "Thinking";
          sinceMs = at;
        }
        break;
      case "agent.message": {
        const text = str(p["text"]);
        if (text) recent.push({ at, kind: "message", text: clip(text.replace(/\s+/g, " "), 240) });
        if (state !== "idle") {
          state = "thinking";
          now = "Thinking";
          sinceMs = at;
        }
        break;
      }
      // Every tool call is evaluated; only an `ask` waits for a person.
      case "agent.permission_evaluated":
        if (p["decision"] === "ask") {
          pendingApproval = at;
          recent.push({ at, kind: "approval", text: "Asked for your approval" });
        }
        break;
      case "agent.permission_resolved":
        pendingApproval = null;
        break;
      case "agent.turn_completed":
        state = "idle";
        now = "Finished its turn";
        sinceMs = at;
        open.clear();
        recent.push({ at, kind: "turn", text: "Finished the turn" });
        break;
      default:
        break;
    }
  }
  if (state !== "idle" && open.size > 0) {
    const [first] = open.values();
    if (first) {
      state = "running_tool";
      now = `Running ${clip(first.text, 140)}`;
      sinceMs = first.at;
    }
  }
  if (state !== "idle" && pendingApproval !== null) {
    state = "waiting_approval";
    now = "Waiting for your approval";
    sinceMs = pendingApproval;
  }
  return { state, now, sinceMs, recent: recent.reverse().slice(0, 6) };
}
