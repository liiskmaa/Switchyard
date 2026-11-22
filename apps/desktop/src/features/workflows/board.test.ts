import { describe, expect, it } from "vitest";

import type { TaskAssignment, TaskDetail, WorkflowInfo } from "../../types/protocol";
import { roleLanes } from "./board";
import { buildFlow } from "./flow";

function step(step_key: string, status: TaskAssignment["status"], at: number, outcome: string | null = null): TaskAssignment {
  return {
    id: step_key,
    task_id: "t",
    role: step_key.startsWith("investigate") ? "investigate" : step_key.startsWith("implement") ? "implement" : "review",
    provider: "claude_code",
    workspace_id: "w",
    session_id: `s-${step_key}`,
    status,
    context: null,
    context_version: 3,
    outcome,
    created_at_ms: at,
    started_at_ms: at,
    ended_at_ms: status === "active" ? null : at + 1000,
    origin: "workflow",
    step_key,
    model: null,
    actual_model: null,
  };
}

describe("role lanes", () => {
  it("lists each role's steps newest first, the working one current and opened with its report", () => {
    const binding = { provider: "codex", model: null };
    const d = {
      assignments: [
        step("investigate#1", "reported", 1, "reported: findings"),
        step("implement#1", "reported", 2, "reported: implementation"),
        step("review#1", "reported", 3, "review: changes_requested"),
        step("investigate#2", "reported", 4, "triage: fix"),
        step("implement#2", "active", 5),
      ],
      messages: [
        { id: "m1", task_id: "t", assignment_id: "review#1", kind: "review_result", sender: "agent", sender_session_id: null, recipient: "operator", payload: { report: { summary: "Handle the empty inbox." } }, causation_id: null, delivery: "recorded", delivered_at_ms: null, created_at_ms: 3 },
      ],
      findings: [
        { id: "f1", task_id: "t", message_id: "m1", assignment_id: "review#1", severity: "medium", title: "Empty inbox crashes", explanation: null, suggestion: null, path: "src/inbox.ts", line: null, status: "open", created_at_ms: 3 },
      ],
      test_runs: [],
      workflow: {
        definition: "engineering",
        status: "running",
        stage: "implementation",
        delivery: null,
        bindings: { investigate: binding, implement: binding, review: binding, correct: null },
      } as unknown as WorkflowInfo,
    } as unknown as TaskDetail;
    const flow = buildFlow(d, 10)!;
    const lanes = roleLanes(d, flow, 10);
    expect(lanes.map((l) => [l.key, l.items.map((i) => i.id)])).toEqual([
      ["reasoning", ["investigate#2", "investigate#1"]],
      ["coding", ["implement#2", "implement#1"]],
      ["tests", []],
      ["review", ["review#1"]],
    ]);
    const coding = lanes[1]!;
    expect(coding.state).toBe("working");
    expect(coding.items[0]).toMatchObject({ current: true, done: false, status: "Working" });
    expect(coding.items[0]?.links[0]?.label).toBe("Open the agent's session");
    const review = lanes[3]!.items[0]!;
    expect(review.details.map(([k]) => k)).toEqual(["Reported", "Outcome", "Findings", "Agent", "Time"]);
  });
});
