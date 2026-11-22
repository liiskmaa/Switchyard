import { createElement, type ReactNode } from "react";

import type { TaskAssignment, TaskDetail, TestRunInfo } from "../../types/protocol";
import { type Flow, type FlowNode, formatDuration, nodeOfStep } from "./flow";
import { effectiveBinding, stepName } from "./labels";
import { type BoardItem, type BoardLane, openSession, providerModel, when } from "./WorkBoard";

type Tone = BoardItem["tone"];

const ASSIGNMENT: Record<TaskAssignment["status"], [string, Tone]> = {
  pending: ["Starting", "accent"],
  active: ["Working", "accent"],
  reported: ["Reported", "ok"],
  failed: ["Failed", "error"],
  cancelled: ["Stopped", "neutral"],
};

const RUN: Record<TestRunInfo["status"], [string, Tone]> = {
  passed: ["Passed", "ok"],
  failed: ["Failed", "error"],
  needs_authorization: ["Needs you", "warn"],
};

const text = (s: string) => createElement("span", { className: "board-text" }, s);

/** The latest report text of an assignment, from Core's record of its messages. */
function reportOf(d: TaskDetail, assignment: string): { summary?: string; question?: string; details?: string } {
  for (const m of [...d.messages].reverse()) {
    if (m.assignment_id !== assignment || m.sender !== "agent") continue;
    const r = m.payload["report"] as Record<string, unknown> | undefined;
    if (!r) continue;
    const s = (k: string) => (typeof r[k] === "string" ? (r[k] as string) : undefined);
    const out: { summary?: string; question?: string; details?: string } = {};
    const summary = s("summary");
    const question = s("question");
    const details = s("details");
    if (summary) out.summary = summary;
    if (question) out.question = question;
    if (details) out.details = details;
    return out;
  }
  return {};
}

function stepItem(d: TaskDetail, a: TaskAssignment, now: number): BoardItem {
  const key = a.step_key ?? "";
  const [status, tone] = ASSIGNMENT[a.status];
  const r = reportOf(d, a.id);
  const details: [string, ReactNode][] = [];
  if (r.summary) details.push(["Reported", text(r.summary)]);
  if (r.question) details.push(["Asked you", text(r.question)]);
  if (r.details) details.push(["Details", text(r.details)]);
  if (a.outcome) details.push(["Outcome", a.outcome]);
  const findings = d.findings.filter((f) => f.assignment_id === a.id);
  if (findings.length) {
    details.push([
      "Findings",
      createElement(
        "ul",
        { className: "board-sublist" },
        findings.map((f) => createElement("li", { key: f.id }, `[${f.severity}] ${f.title}${f.path ? ` (${f.path})` : ""}`)),
      ),
    ]);
  }
  details.push(["Agent", providerModel(a.provider, a.actual_model ?? a.model)]);
  if (a.started_at_ms) {
    const end = a.ended_at_ms ?? (a.status === "active" ? now : null);
    details.push(["Time", `started ${when(a.started_at_ms, now) ?? ""}${end ? ` · ${formatDuration(end - a.started_at_ms)}` : ""}`]);
  }
  const meta = [a.started_at_ms ? when(a.started_at_ms, now) : null, r.summary ? r.summary.slice(0, 80) : null]
    .filter(Boolean)
    .join(" · ");
  return {
    id: a.id,
    title: stepName(key),
    status,
    tone,
    current: a.status === "active" || a.status === "pending",
    done: a.status !== "active" && a.status !== "pending",
    ...(meta ? { meta } : {}),
    details,
    links: a.session_id ? [{ label: "Open the agent's session", open: openSession(a.session_id) }] : [],
  };
}

function runItem(r: TestRunInfo, now: number, current: boolean): BoardItem {
  const [status, tone] = RUN[r.status];
  return {
    id: r.id,
    title: `Tests of ${stepName(r.step_key).toLowerCase()}`,
    status,
    tone,
    current,
    done: !current && r.status !== "needs_authorization",
    meta: `${when(r.ended_at_ms, now) ?? ""} · ${r.summary}`,
    details: r.results.map((x): [string, ReactNode] => [
      `${x.kind}${x.name ? ` (${x.name})` : ""}`,
      text(`${x.command} → ${x.status}${x.exit_code != null ? ` (exit ${x.exit_code})` : ""}`),
    ]),
    links: [],
  };
}

const ROLE_LANES: [FlowNode, string][] = [
  ["reasoning", "investigate"],
  ["coding", "implement"],
  ["tests", ""],
  ["review", "review"],
];

const TITLE: Record<string, string> = { reasoning: "Reasoning", coding: "Coding", tests: "Tests", review: "Review" };

/** Lanes of an engineering workflow: each role's steps (and the test runs), newest first. */
export function roleLanes(d: TaskDetail, flow: Flow, now: number): BoardLane[] {
  const w = d.workflow;
  if (!w) return [];
  const steps = d.assignments.filter((a) => a.step_key && nodeOfStep(a.step_key)).sort((a, b) => b.created_at_ms - a.created_at_ms);
  return ROLE_LANES.map(([node, role]): BoardLane => {
    let items: BoardItem[];
    let subtitle: string;
    if (node === "tests") {
      const runs = [...(d.test_runs ?? [])].reverse();
      items = runs.map((r, i) => runItem(r, now, i === 0 && flow.nodes.tests === "working"));
      if (flow.nodes.tests === "working" && runs.length === 0) {
        items.unshift({ id: "running", title: "Running the tests", status: "Running", tone: "accent", current: true, done: false, details: [], links: [] });
      }
      subtitle = "run by Switchyard";
    } else {
      items = steps.filter((a) => nodeOfStep(a.step_key ?? "") === node).map((a) => stepItem(d, a, now));
      const b = effectiveBinding(w.bindings, role as "investigate" | "implement" | "review");
      subtitle = providerModel(b.provider, b.model);
    }
    const state = flow.nodes[node];
    return {
      key: node,
      title: TITLE[node] ?? node,
      subtitle,
      state: state === "working" ? "working" : state === "needs_you" ? "needs_you" : state === "done" ? "done" : "idle",
      time: flow.times[node],
      items,
    };
  });
}

