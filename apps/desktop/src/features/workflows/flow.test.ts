import { describe, expect, it } from "vitest";

import type { TaskAssignment, TaskDetail, TestRunInfo, WorkflowInfo } from "../../types/protocol";
import { buildFlow, formatDuration, routeCounts } from "./flow";

let clock = 0;
function step(step_key: string, status: TaskAssignment["status"], outcome: string | null = null): TaskAssignment {
  clock += 1;
  return {
    id: `a${clock}`,
    task_id: "t",
    role: step_key.startsWith("investigate") ? "investigate" : step_key.startsWith("implement") ? "implement" : "review",
    provider: "claude_code",
    workspace_id: "w",
    session_id: null,
    status,
    context: null,
    context_version: 3,
    outcome,
    created_at_ms: clock,
    started_at_ms: clock,
    ended_at_ms: null,
    origin: "workflow",
    step_key,
    model: null,
    actual_model: null,
  };
}

function run(step_key: string, status: TestRunInfo["status"]): TestRunInfo {
  clock += 1;
  return { id: `r${clock}`, step_key, status, summary: status, results: [], ui_change: false, started_at_ms: clock, ended_at_ms: clock };
}

function detail(assignments: TaskAssignment[], w: Partial<WorkflowInfo>, test_runs: TestRunInfo[] = []): TaskDetail {
  return {
    assignments,
    test_runs,
    workflow: { definition: "engineering", status: "running", stage: "review", delivery: null, ...w } as WorkflowInfo,
  } as TaskDetail;
}

const route = (d: TaskDetail) => buildFlow(d)?.transfers.map((t) => `${t.from}>${t.to}:${t.label}`);

describe("task flow", () => {
  it("follows review findings to reasoning, a dispute back to review, without coding", () => {
    const d = detail(
      [
        step("investigate#1", "reported", "reported: findings"),
        step("implement#1", "reported", "reported: implementation"),
        step("review#1", "reported", "review: changes_requested"),
        step("investigate#2", "reported", "triage: dispute"),
        step("review#2", "active"),
      ],
      { stage: "review" },
    );
    expect(route(d)).toEqual([
      "you>reasoning:Task",
      "reasoning>coding:Plan",
      "coding>review:Changes",
      "review>reasoning:Findings",
      "reasoning>review:Disputed",
    ]);
    const f = buildFlow(d);
    expect(f?.active).toEqual({ from: "reasoning", to: "review", label: "Disputed" });
    expect(f?.nodes.review).toBe("working");
    expect(f?.now).toContain("checking again");
  });

  it("a fix goes from reasoning to coding; repeated routes are counted", () => {
    const d = detail(
      [
        step("investigate#1", "reported", "reported: findings"),
        step("implement#1", "reported", "reported: implementation"),
        step("review#1", "reported", "review: changes_requested"),
        step("investigate#2", "reported", "triage: fix"),
        step("implement#2", "reported", "reported: implementation"),
        step("review#2", "active"),
      ],
      {},
    );
    const f = buildFlow(d);
    expect(f?.transfers.map((t) => t.label)).toContain("Fix these");
    expect(routeCounts(f?.transfers ?? []).get("coding>review")).toBe(2);
  });

  it("a waiting workflow hands over to you; a retry is not a hand-off", () => {
    const d = detail(
      [step("investigate#1", "failed"), step("investigate#1@2", "cancelled")],
      { status: "waiting", stage: "investigation" },
    );
    const f = buildFlow(d);
    expect(route(d)).toEqual(["you>reasoning:Task", "reasoning>you:Needs you"]);
    expect(f?.nodes.you).toBe("needs_you");
  });

  it("delivery runs through validation to the branch", () => {
    const steps = [
      step("investigate#1", "reported", "reported: findings"),
      step("implement#1", "reported", "reported: implementation"),
      step("review#1", "reported", "review: approved"),
    ];
    const approving = buildFlow(detail(steps, { stage: "delivery", delivery: "awaiting_approval", status: "waiting" }));
    expect(approving?.active).toEqual({ from: "validation", to: "you", label: "Approve the integration" });
    const delivered = buildFlow(detail(steps, { stage: "delivery", delivery: "delivered", status: "delivered" }));
    expect(delivered?.active).toBeNull();
    expect(delivered?.nodes.branch).toBe("done");
    expect(delivered?.transfers.at(-1)).toEqual({ from: "validation", to: "branch", label: "Merged" });
  });

  it("work sent back from delivery comes from validation", () => {
    const d = detail(
      [
        step("investigate#1", "reported", "reported: findings"),
        step("implement#1", "reported", "reported: implementation"),
        step("review#1", "reported", "review: approved"),
        step("implement#2", "active"),
      ],
      { stage: "correction" },
    );
    expect(route(d)?.slice(-2)).toEqual(["review>validation:Approved", "validation>coding:Sent back"]);
  });

  it("organizations and tasks without a workflow have no flow", () => {
    expect(buildFlow({ assignments: [], workflow: null } as unknown as TaskDetail)).toBeNull();
    expect(buildFlow(detail([], { definition: "organization" }))).toBeNull();
  });

  it("work goes through the tests: failing tests go to reasoning, passing ones to review", () => {
    const d = detail(
      [
        step("investigate#1", "reported", "reported: findings"),
        step("implement#1", "reported", "reported: implementation"),
        step("investigate#2", "reported", "triage: fix"),
        step("implement#2", "reported", "reported: implementation"),
        step("review#2", "active"),
      ],
      { stage: "review" },
      [run("implement#1", "failed"), run("implement#2", "passed")],
    );
    expect(route(d)).toEqual([
      "you>reasoning:Task",
      "reasoning>coding:Plan",
      "coding>tests:Changes",
      "tests>reasoning:Failed",
      "reasoning>coding:Fix these",
      "coding>tests:Changes",
      "tests>review:Passed",
    ]);
    expect(buildFlow(d)?.nodes.tests).toBe("done");
  });

  it("tests that need authorization hand over to you; running tests light the Tests node", () => {
    const steps = [step("investigate#1", "reported", "reported: findings"), step("implement#1", "reported", "reported: implementation")];
    const waiting = buildFlow(detail(steps, { stage: "testing", status: "waiting" }, [run("implement#1", "needs_authorization")]));
    expect(waiting?.active).toEqual({ from: "tests", to: "you", label: "Authorize" });
    expect(waiting?.nodes.you).toBe("needs_you");
    const running = buildFlow(detail(steps, { stage: "testing", status: "running" }, []));
    expect(running?.active).toEqual({ from: "coding", to: "tests", label: "Changes" });
    expect(running?.nodes.tests).toBe("working");
    const authorized = buildFlow(detail(steps, { stage: "testing", status: "running" }, [run("implement#1", "needs_authorization")]));
    expect(authorized?.active).toEqual({ from: "you", to: "tests", label: "Authorized" });
  });

  it("subtasks loop on their own and hand over to the next one", () => {
    const d = detail(
      [
        step("investigate#1", "reported", "reported: proposal"),
        step("s1/implement#1", "reported", "reported: implementation"),
        step("s1/review#1", "reported", "review: approved"),
        step("s2/implement#1", "active"),
      ],
      { stage: "implementation", subtask: 2, plan: [{ title: "Parser", done: true }, { title: "Printer", done: false }] },
      [run("s1/implement#1", "passed")],
    );
    expect(route(d)).toEqual([
      "you>reasoning:Task",
      "reasoning>coding:Plan",
      "coding>tests:Changes",
      "tests>review:Passed",
      "review>coding:Next subtask",
    ]);
    expect(buildFlow(d)?.now).toMatch(/^Subtask 2 of 2: Printer\./);
  });

  it("times each role: total spent, and live since its current step began", () => {
    const a = step("investigate#1", "reported", "reported: findings");
    a.started_at_ms = 1_000;
    a.ended_at_ms = 61_000;
    const b = step("implement#1", "active");
    b.started_at_ms = 70_000;
    b.ended_at_ms = null;
    const f = buildFlow(detail([a, b], { stage: "implementation" }), 100_000);
    expect(f?.times.reasoning).toEqual({ totalMs: 60_000 });
    expect(f?.times.coding).toEqual({ totalMs: 30_000, runningSinceMs: 70_000 });
    expect(formatDuration(45_000)).toBe("45s");
    expect(formatDuration(725_000)).toBe("12m 05s");
    expect(formatDuration(3_840_000)).toBe("1h 04m");
  });
});
