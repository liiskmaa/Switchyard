import type { EventEnvelope, OrganizationInfo } from "../../types/protocol";
import type { NodeTime } from "./flow";

/**
 * An agent organization as a live picture (ADR 0035): you, the lead and the
 * specialists as a reporting tree, with the hand-offs between them taken only
 * from the organization's journal events and its current state. Nothing is
 * simulated: an arrow exists because Core recorded a delegation, a report,
 * a verdict or a question.
 */
export const YOU = "you";

export type TeamNodeState = "working" | "checking" | "needs_you" | "done" | "idle";

export interface TeamNode {
  key: string;
  title: string;
  /** `null` for you. */
  provider: string | null;
  model: string | null;
  parent: string | null;
  depth: number;
  x: number;
  y: number;
  state: TeamNodeState;
}

export interface Handoff {
  from: string;
  to: string;
  label: string;
}

export interface Team {
  nodes: TeamNode[];
  handoffs: Handoff[];
  /** The hand-off in progress, if any. */
  active: Handoff | null;
  times: Record<string, NodeTime>;
  /** Tasks per owner: [open, accepted]. */
  load: Record<string, [number, number]>;
  width: number;
  height: number;
}

const VERDICT: Record<string, string> = {
  accept: "Accepted",
  correct: "Fix this",
  reassign: "Reassigned",
  cancel: "Cancelled",
};

const str = (p: Record<string, unknown>, k: string): string | null => (typeof p[k] === "string" ? (p[k] as string) : null);

/** Lays out the reporting tree: leaves side by side, parents above the middle of their reports. */
function layout(o: OrganizationInfo): { nodes: TeamNode[]; width: number; height: number } {
  const kids = new Map<string, string[]>();
  for (const a of o.agents) {
    const p = a.reports_to ?? YOU;
    kids.set(p, [...(kids.get(p) ?? []), a.key]);
  }
  const byKey = new Map(o.agents.map((a) => [a.key, a]));
  const order: { key: string; depth: number; parent: string | null }[] = [];
  const walk = (key: string, depth: number, parent: string | null) => {
    order.push({ key, depth, parent });
    for (const k of kids.get(key) ?? []) walk(k, depth + 1, key);
  };
  walk(YOU, 0, null);
  const STEP_X = 170;
  const STEP_Y = 170;
  const xs = new Map<string, number>();
  let leaf = 0;
  const place = (key: string): number => {
    const children = kids.get(key) ?? [];
    if (children.length === 0) {
      const x = leaf * STEP_X;
      leaf += 1;
      xs.set(key, x);
      return x;
    }
    const childXs = children.map(place);
    const x = (Math.min(...childXs) + Math.max(...childXs)) / 2;
    xs.set(key, x);
    return x;
  };
  place(YOU);
  const maxDepth = Math.max(...order.map((n) => n.depth));
  const width = Math.max(leaf * STEP_X, 360);
  const offset = (width - (leaf - 1) * STEP_X) / 2;
  const nodes = order.map(({ key, depth, parent }) => {
    const a = byKey.get(key);
    return {
      key,
      title: a?.title ?? "You",
      provider: a?.provider ?? null,
      model: a?.model ?? null,
      parent,
      depth,
      x: (xs.get(key) ?? 0) + offset,
      y: 60 + depth * STEP_Y,
      state: (a?.status === "done" ? "done" : "idle") as TeamNodeState,
    };
  });
  // Below the lowest circle: two lines of name and the provider line.
  return { nodes, width, height: 60 + maxDepth * STEP_Y + 110 };
}

/** The team picture from the organization and its `organization.*` events (oldest first). */
export function buildTeam(o: OrganizationInfo, events: EventEnvelope[], nowMs: number = Date.now()): Team {
  const { nodes, width, height } = layout(o);
  const node = new Map(nodes.map((n) => [n.key, n]));
  const ownerOf = new Map(o.tasks.map((t) => [t.key, t.owner]));
  const bossOf = (agent: string) => node.get(agent)?.parent ?? YOU;
  const handoffs: Handoff[] = [];
  const push = (h: Handoff) => {
    if (h.from !== h.to && node.has(h.from) && node.has(h.to)) handoffs.push(h);
  };
  const times: Record<string, NodeTime> = {};
  const add = (k: string, ms: number) => {
    const t = (times[k] ??= { totalMs: 0 });
    t.totalMs += Math.max(0, ms);
  };
  // One session at a time per organization: a turn lasts until the next one
  // starts or the organization stops.
  let turn: { agent: string; since: number } | null = null;
  const close = (at: number) => {
    if (turn) add(turn.agent, at - turn.since);
    turn = null;
  };

  for (const e of [...events].sort((a, b) => a.seq - b.seq)) {
    const p = e.payload;
    const at = e.occurred_at_ms;
    switch (e.event_type) {
      case "organization.task_delegated": {
        const by = str(p, "delegated_by");
        const owner = str(p, "owner");
        if (owner) push({ from: by && node.has(by) ? by : bossOf(owner), to: owner, label: "Delegated" });
        break;
      }
      case "organization.supervision_started": {
        const sup = str(p, "supervisor");
        close(at);
        if (!sup) break;
        turn = { agent: sup, since: at };
        const items = Array.isArray(p["items"]) ? (p["items"] as unknown[]).map(String) : [];
        for (const item of items) {
          if (item.startsWith("review ")) {
            const owner = ownerOf.get(item.slice(7));
            if (owner) push({ from: owner, to: sup, label: "Report" });
          } else if (item.startsWith("escalation from ")) {
            push({ from: item.slice(16), to: sup, label: "Question" });
          } else if (item === "your answer") {
            push({ from: YOU, to: sup, label: "Your answer" });
          }
        }
        break;
      }
      case "organization.work_started": {
        const agent = str(p, "agent");
        close(at);
        if (agent) turn = { agent, since: at };
        break;
      }
      case "organization.assessed": {
        const by = str(p, "by");
        const owner = ownerOf.get(str(p, "key") ?? "");
        const verdict = str(p, "verdict") ?? "";
        if (by && owner) push({ from: by, to: owner, label: VERDICT[verdict] ?? verdict });
        break;
      }
      case "organization.escalated": {
        const by = str(p, "by");
        const to = str(p, "to");
        if (by) push({ from: by, to: to === "user" ? YOU : bossOf(by), label: "Question" });
        break;
      }
      case "organization.status_changed": {
        const s = str(p, "status");
        if (s && s !== "running") close(at);
        break;
      }
      default:
        break;
    }
  }

  // Now: who works, and the hand-off in progress.
  let active: Handoff | null = null;
  const a = o.activity;
  const running = o.status === "running";
  const current = turn as { agent: string; since: number } | null;
  if (running && a.agent && node.has(a.agent)) {
    const n = node.get(a.agent);
    if (n) n.state = a.kind === "supervising" ? "checking" : "working";
    if (current && current.agent === a.agent) {
      add(a.agent, nowMs - current.since);
      (times[a.agent] ??= { totalMs: 0 }).runningSinceMs = current.since;
    }
    if (a.kind === "working") {
      active = { from: bossOf(a.agent), to: a.agent, label: a.task ? (o.tasks.find((t) => t.key === a.task)?.title ?? "Work") : "Work" };
    } else {
      active = [...handoffs].reverse().find((h) => h.to === a.agent) ?? null;
    }
  } else if (current) {
    close(nowMs);
  }
  if (o.status === "waiting" || o.question) {
    const you = node.get(YOU);
    if (you) you.state = "needs_you";
    const asker = o.question_from && node.has(o.question_from) ? o.question_from : (nodes[1]?.key ?? null);
    if (asker) {
      active = { from: asker, to: YOU, label: o.question ? "Question" : "Needs you" };
    }
  }

  const load: Record<string, [number, number]> = {};
  for (const t of o.tasks) {
    const l = (load[t.owner] ??= [0, 0]);
    if (t.state === "accepted") l[1] += 1;
    else if (t.state !== "cancelled") l[0] += 1;
  }
  return { nodes, handoffs, active, times, load, width, height };
}
