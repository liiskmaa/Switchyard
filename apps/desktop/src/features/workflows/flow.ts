import type { TaskAssignment, TaskDetail, TestRunInfo, WorkflowInfo } from "../../types/protocol";
import { DELIVERY_LABEL } from "./labels";

/**
 * The task's work as hand-offs between roles, derived only from Core's
 * records (the workflow and its step assignments). Nothing is simulated:
 * a hand-off exists because a step reported and the next one started.
 */
export type FlowNode = "you" | "reasoning" | "coding" | "tests" | "review" | "validation" | "branch";

export type NodeState = "working" | "needs_you" | "done" | "idle";

export interface Transfer {
  from: FlowNode;
  to: FlowNode;
  label: string;
}

/** Time a role spent on the task: in total, and since its current step began if it is working. */
export interface NodeTime {
  totalMs: number;
  runningSinceMs?: number;
}

export interface Flow {
  /** Every hand-off so far, oldest first. */
  transfers: Transfer[];
  /** The hand-off in progress (its target is working or waiting), if any. */
  active: Transfer | null;
  nodes: Record<FlowNode, NodeState>;
  /** One sentence: what is happening now. */
  now: string;
  /** Time per role, from the steps' recorded start and end (and the test runs). */
  times: Partial<Record<FlowNode, NodeTime>>;
}

export const NODE_LABEL: Record<FlowNode, string> = {
  you: "You",
  reasoning: "Reasoning",
  coding: "Coding",
  tests: "Tests",
  review: "Review",
  validation: "Validation",
  branch: "Branch",
};

const triageRound = (key: string) => /^investigate#(\d+)/.exec(key)?.[1];

/** The key without its subtask prefix (`s2/implement#1` → `implement#1`, ADR 0040). */
export const baseKey = (key: string): string => key.replace(/^s\d+\//, "");

export function nodeOfStep(fullKey: string): FlowNode | null {
  const key = baseKey(fullKey);
  if (key.startsWith("investigate#")) return "reasoning";
  if (key.startsWith("implement#")) return "coding";
  if (key.startsWith("review#")) return "review";
  return null;
}

const isTriage = (key: string) => Number(triageRound(baseKey(key)) ?? "1") > 1 || /^s\d+\/investigate#/.test(key);

/** What a finished step handed to the next one. */
function handoffLabel(prev: TaskAssignment, to: FlowNode): string {
  const key = baseKey(prev.step_key ?? "");
  const outcome = prev.outcome ?? "";
  if (key.startsWith("investigate#")) {
    if (outcome === "triage: dispute") return "Disputed";
    if (outcome === "triage: fix") return "Fix these";
    return isTriage(key) ? "Fix these" : "Plan";
  }
  if (key.startsWith("implement#")) return "Changes";
  if (outcome === "review: changes_requested") return to === "reasoning" ? "Findings" : "Changes requested";
  if (outcome === "review: rejected") return "Rejected";
  if (outcome === "review: approved") return "Approved";
  return "Hand-off";
}

const OPERATOR_PHASES = new Set([
  "needs_commit",
  "awaiting_validation_authorization",
  "validation_failed",
  "blocked",
  "awaiting_approval",
  "delivery_failed",
  "cancelled",
]);

function emptyNodes(): Record<FlowNode, NodeState> {
  return {
    you: "idle",
    reasoning: "idle",
    coding: "idle",
    tests: "idle",
    review: "idle",
    validation: "idle",
    branch: "idle",
  };
}

/** The flow of an `engineering` workflow; `null` for none or an organization. */
export function buildFlow(detail: TaskDetail, nowMs: number = Date.now()): Flow | null {
  const w: WorkflowInfo | null = detail.workflow;
  if (!w || w.definition !== "engineering") return null;
  const steps = detail.assignments
    .filter((a) => a.step_key !== null && nodeOfStep(a.step_key) !== null)
    .sort((a, b) => a.created_at_ms - b.created_at_ms);
  const transfers: Transfer[] = [];
  const nodes = emptyNodes();
  const push = (t: Transfer) => {
    if (t.from !== t.to) transfers.push(t);
  };
  const runs = detail.test_runs ?? [];
  const runsFor = (key: string): TestRunInfo[] => runs.filter((r) => r.step_key === key);
  /** Coding → Tests, and the gate's hand-offs; returns the last run. */
  const throughTests = (key: string): TestRunInfo | undefined => {
    const mine = runsFor(key);
    if (mine.length === 0) return undefined;
    push({ from: "coding", to: "tests", label: "Changes" });
    mine.forEach((r, i) => {
      if (r.status === "needs_authorization") {
        push({ from: "tests", to: "you", label: "Authorize" });
        if (i < mine.length - 1) push({ from: "you", to: "tests", label: "Authorized" });
      }
    });
    if (mine.some((r) => r.status === "passed")) nodes.tests = "done";
    return mine[mine.length - 1];
  };

  if (steps.length > 0) push({ from: "you", to: "reasoning", label: "Task" });
  for (let i = 1; i < steps.length; i++) {
    const prev = steps[i - 1];
    const cur = steps[i];
    if (!prev?.step_key || !cur?.step_key) continue;
    const to = nodeOfStep(cur.step_key);
    const from = nodeOfStep(prev.step_key);
    if (!to || !from) continue;
    // The test gate between an implementation and what follows it (ADR 0039).
    if (from === "coding" && baseKey(prev.step_key).startsWith("implement#")) {
      const run = throughTests(prev.step_key);
      if (run) {
        push({ from: "tests", to, label: run.status === "passed" ? "Passed" : "Failed" });
        continue;
      }
    }
    // A subtask's review approved: the next subtask starts (ADR 0040).
    const sub = (k: string) => /^s(\d+)\//.exec(k)?.[1];
    if (prev.outcome === "review: approved" && sub(cur.step_key) && sub(cur.step_key) !== sub(prev.step_key)) {
      push({ from: "review", to, label: "Next subtask" });
      continue;
    }
    // Sent back from delivery after an approved review.
    if (prev.outcome === "review: approved" && to === "coding") {
      push({ from: "review", to: "validation", label: "Approved" });
      push({ from: "validation", to: "coding", label: "Sent back" });
      continue;
    }
    push({ from, to, label: handoffLabel(prev, to) });
  }
  for (const s of steps) {
    const n = s.step_key ? nodeOfStep(s.step_key) : null;
    if (n && s.status === "reported") nodes[n] = "done";
  }

  const last = steps[steps.length - 1];
  const lastNode = last?.step_key ? nodeOfStep(last.step_key) : null;
  let now = "Starting.";
  const final = w.status === "delivered" || w.status === "cancelled" || w.status === "failed" || w.status === "completed";

  if (w.stage === "delivery" || w.status === "delivered" || w.status === "completed") {
    push({ from: "review", to: "validation", label: "Approved" });
    const phase = w.delivery;
    if (w.status === "delivered" || phase === "delivered") {
      push({ from: "validation", to: "branch", label: "Merged" });
      nodes.validation = "done";
      nodes.branch = "done";
      now = "Delivered: the approved work is merged into the target branch.";
    } else if (phase === "integrating") {
      push({ from: "validation", to: "branch", label: "Merging" });
      nodes.validation = "done";
      nodes.branch = "working";
      now = "Merging the approved commits into the target branch.";
    } else if (phase && OPERATOR_PHASES.has(phase)) {
      push({ from: "validation", to: "you", label: DELIVERY_LABEL[phase] });
      nodes.validation = phase === "validation_failed" ? "needs_you" : "done";
      nodes.you = "needs_you";
      now = `Needs you: ${DELIVERY_LABEL[phase].toLowerCase()}.`;
    } else {
      nodes.validation = "working";
      now = phase ? `${DELIVERY_LABEL[phase]}.` : "Preparing the delivery.";
    }
  } else if (w.stage === "testing" && last?.step_key) {
    const run = throughTests(last.step_key);
    if (!run) push({ from: "coding", to: "tests", label: "Changes" });
    if (w.status === "waiting") {
      if (run?.status !== "needs_authorization") push({ from: "tests", to: "you", label: "Needs you" });
      nodes.you = "needs_you";
      now = "The tests need you: authorize the commands your policies ask about, or turn on auto mode.";
    } else {
      if (run?.status === "needs_authorization") push({ from: "you", to: "tests", label: "Authorized" });
      nodes.tests = "working";
      now = "Switchyard is running the tests before the review.";
    }
  } else if (lastNode && last) {
    const working = last.status === "active" || last.status === "pending";
    if (w.status === "waiting") {
      push({ from: lastNode, to: "you", label: "Needs you" });
      nodes.you = "needs_you";
      now = `${NODE_LABEL[lastNode]} stopped and needs you.`;
    } else if (working) {
      nodes[lastNode] = "working";
      now = describeWorking(last.step_key ?? "", lastNode, transfers[transfers.length - 1]?.label);
    }
  }
  const plan = w.plan ?? [];
  if (plan.length > 0 && w.subtask && !final && w.stage !== "delivery") {
    now = `Subtask ${w.subtask} of ${plan.length}: ${plan[w.subtask - 1]?.title ?? ""}. ${now}`;
  }
  if (w.status === "paused") now = `Paused. ${now}`;

  const active = final || w.status === "paused" ? null : (transfers[transfers.length - 1] ?? null);

  const times: Partial<Record<FlowNode, NodeTime>> = {};
  const add = (n: FlowNode, ms: number) => {
    const t = (times[n] ??= { totalMs: 0 });
    t.totalMs += Math.max(0, ms);
  };
  for (const s of steps) {
    const n = s.step_key ? nodeOfStep(s.step_key) : null;
    if (!n || s.started_at_ms === null) continue;
    const running = s.status === "active";
    add(n, (s.ended_at_ms ?? (running ? nowMs : s.started_at_ms)) - s.started_at_ms);
    if (running && nodes[n] === "working") (times[n] ??= { totalMs: 0 }).runningSinceMs = s.started_at_ms;
  }
  for (const r of runs) add("tests", r.ended_at_ms - r.started_at_ms);
  if (nodes.tests === "working") {
    const since = last?.ended_at_ms ?? null;
    if (since !== null) {
      add("tests", nowMs - since);
      (times.tests ??= { totalMs: 0 }).runningSinceMs = since;
    }
  }
  return { transfers, active, nodes, now, times };
}

function describeWorking(key: string, node: FlowNode, arrived?: string): string {
  if (node === "reasoning") {
    if (!isTriage(key)) return "Reasoning is investigating the task.";
    return arrived === "Failed"
      ? "Reasoning is working out why the tests failed and what to fix."
      : "Reasoning is deciding whether the review's findings should be fixed or disputed.";
  }
  const k = baseKey(key);
  if (node === "coding") return k === "implement#1" ? "Coding is implementing." : "Coding is fixing what was found.";
  return k === "review#1" ? "Review is checking the work, read-only." : "Review is checking again, read-only.";
}

/** How many times each route was used, keyed `from>to`. */
export function routeCounts(transfers: Transfer[]): Map<string, number> {
  const m = new Map<string, number>();
  for (const t of transfers) m.set(`${t.from}>${t.to}`, (m.get(`${t.from}>${t.to}`) ?? 0) + 1);
  return m;
}

/** A short clock: `45s`, `12m 05s`, `1h 04m`. */
export function formatDuration(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${String(s % 60).padStart(2, "0")}s`;
  return `${Math.floor(m / 60)}h ${String(m % 60).padStart(2, "0")}m`;
}
