import { describe, expect, it } from "vitest";

import type { EventEnvelope, OrgAgentInfo, OrganizationInfo, OrgTaskInfo } from "../../types/protocol";
import { YOU, buildTeam } from "./team";

function agent(key: string, reports_to: string | null, status: OrgAgentInfo["status"] = "idle"): OrgAgentInfo {
  return {
    id: key,
    key,
    title: key.toUpperCase(),
    kind: reports_to === null ? "supervisor" : "specialist",
    reports_to,
    responsibility: "",
    scope: [],
    capabilities: [],
    provider: "claude_code",
    model: null,
    selection: "",
    status,
    created_by: "core",
    depth: reports_to === null ? 0 : 1,
  };
}

function task(key: string, owner: string, state: OrgTaskInfo["state"]): OrgTaskInfo {
  return {
    key,
    task_id: null,
    title: `Task ${key}`,
    description: null,
    work: "implementation",
    parent: null,
    owner,
    scope: [],
    acceptance: [],
    depends_on: [],
    state,
    corrections: 0,
    summary: null,
    created_by: "lead",
  };
}

let seq = 0;
function ev(event_type: string, at: number, payload: Record<string, unknown>): EventEnvelope {
  seq += 1;
  return {
    seq,
    id: `e${seq}`,
    schema_version: 1,
    event_type,
    occurred_at_ms: at,
    project_id: null,
    workspace_id: null,
    session_id: null,
    task_id: "root",
    agent_id: null,
    causation_id: null,
    payload,
  };
}

function org(over: Partial<OrganizationInfo>): OrganizationInfo {
  return {
    task_id: "root",
    workspace_id: "w",
    status: "running",
    reason: null,
    agents: [agent("lead", null), agent("api", "lead"), agent("ui", "lead")],
    tasks: [task("t1", "api", "accepted"), task("t2", "ui", "working")],
    activity: { kind: "idle", agent: null, task: null, items: [] },
    queued_checkpoints: 0,
    limits: {},
    usage: { supervision_turns: 0, max_supervision_turns: 0, agents: 3, max_agents: 8, tasks: 2, max_tasks: 20, cost_usd: 0, tokens: 0 },
    question: null,
    question_from: null,
    context: [],
    final_summary: null,
    created_at_ms: 0,
    updated_at_ms: 0,
    ...over,
  };
}

const EVENTS = [
  ev("organization.supervision_started", 0, { supervisor: "lead", key: "c1", items: ["plan"] }),
  ev("organization.task_delegated", 10_000, { key: "t1", owner: "api", delegated_by: "lead", title: "Task t1" }),
  ev("organization.task_delegated", 10_000, { key: "t2", owner: "ui", delegated_by: "lead", title: "Task t2" }),
  ev("organization.work_started", 20_000, { key: "t1", agent: "api", instructions: [] }),
  ev("organization.supervision_started", 80_000, { supervisor: "lead", key: "c2", items: ["review t1"] }),
  ev("organization.assessed", 90_000, { key: "t1", verdict: "accept", by: "lead" }),
  ev("organization.work_started", 100_000, { key: "t2", agent: "ui", instructions: [] }),
];

describe("team picture", () => {
  it("lays out you, the lead and the specialists as a tree", () => {
    const t = buildTeam(org({}), [], 0);
    expect(t.nodes.map((n) => [n.key, n.parent, n.depth])).toEqual([
      [YOU, null, 0],
      ["lead", YOU, 1],
      ["api", "lead", 2],
      ["ui", "lead", 2],
    ]);
    const [you, lead, api, ui] = t.nodes;
    expect(lead?.x).toBe(you?.x);
    expect(((api?.x ?? 0) + (ui?.x ?? 0)) / 2).toBe(lead?.x);
  });

  it("follows delegations, reports and verdicts from the journal", () => {
    const t = buildTeam(org({ activity: { kind: "working", agent: "ui", task: "t2", items: [] } }), EVENTS, 130_000);
    expect(t.handoffs.map((h) => `${h.from}>${h.to}:${h.label}`)).toEqual([
      "lead>api:Delegated",
      "lead>ui:Delegated",
      "api>lead:Report",
      "lead>api:Accepted",
    ]);
    expect(t.active).toEqual({ from: "lead", to: "ui", label: "Task t2" });
    expect(t.nodes.find((n) => n.key === "ui")?.state).toBe("working");
    expect(t.load["api"]).toEqual([0, 1]);
  });

  it("times each agent's turns; the current one runs live", () => {
    const t = buildTeam(org({ activity: { kind: "working", agent: "ui", task: "t2", items: [] } }), EVENTS, 130_000);
    expect(t.times["lead"]).toEqual({ totalMs: 20_000 + 20_000 });
    expect(t.times["api"]).toEqual({ totalMs: 60_000 });
    expect(t.times["ui"]).toEqual({ totalMs: 30_000, runningSinceMs: 100_000 });
  });

  it("a supervisor checking a report is lit, and a question goes to you", () => {
    const checking = buildTeam(
      org({ activity: { kind: "supervising", agent: "lead", task: null, items: ["review t1"] } }),
      EVENTS.slice(0, 5),
      85_000,
    );
    expect(checking.nodes.find((n) => n.key === "lead")?.state).toBe("checking");
    expect(checking.active).toEqual({ from: "api", to: "lead", label: "Report" });

    const asking = buildTeam(org({ status: "waiting", question: "Which database?", question_from: "lead" }), EVENTS, 130_000);
    expect(asking.nodes[0]?.state).toBe("needs_you");
    expect(asking.active).toEqual({ from: "lead", to: YOU, label: "Question" });
    expect(asking.times["ui"]?.runningSinceMs).toBeUndefined();
  });
});

import { initials, wrapName } from "./TeamFlow";

describe("team node labels", () => {
  it("shows full names on up to two lines and initials in the circle", () => {
    expect(wrapName("Lead supervisor")).toEqual(["Lead supervisor"]);
    expect(wrapName("Mailbox implementation specialist")).toEqual(["Mailbox implementation", "specialist"]);
    expect(wrapName("A very long specialist title for database migrations")).toEqual(["A very long specialist", "title for database mi…"]);
    expect(initials("Mailbox implementation specialist")).toBe("MI");
    expect(initials("lead")).toBe("L");
  });
});

import { routeLabel } from "./TeamFlow";

describe("route labels", () => {
  it("count only what repeats; mixed hand-offs name the latest", () => {
    expect(routeLabel(["Delegated"])).toBe("Delegated");
    expect(routeLabel(["Report", "Report"])).toBe("Report ×2");
    expect(routeLabel(["Delegated", "Fix this", "Cancelled"])).toBe("last: Cancelled (3)");
  });
});
