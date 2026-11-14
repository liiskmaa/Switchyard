import { describe, expect, it } from "vitest";

import type { Task, TaskStatus } from "../../types/protocol";
import { STATUS_LABEL, taskActions } from "./labels";

const task = (status: TaskStatus): Task => ({
  id: "t",
  project_id: "p",
  parent_task_id: null,
  title: "T",
  description: null,
  kind: "general",
  priority: "normal",
  acceptance_criteria: [],
  status,
  status_reason: null,
  result: null,
  failure: null,
  origin: "operator",
  origin_proposal_id: null,
  revision: 1,
  created_at_ms: 0,
  updated_at_ms: 0,
  completed_at_ms: null,
  open_assignment: null,
  dependency_ids: [],
  pending_proposals: 0,
  open_findings: 0,
});

describe("taskActions", () => {
  it("only offers completion after reported work, never while an agent works", () => {
    expect(taskActions(task("awaiting_review")).complete).toBe(true);
    for (const s of ["draft", "ready", "assigned", "in_progress", "in_review", "failed"] as TaskStatus[]) {
      expect(taskActions(task(s)).complete).toBe(false);
    }
  });

  it("offers review only for reported work", () => {
    expect(taskActions(task("ready")).assign).toEqual(["investigate", "implement"]);
    expect(taskActions(task("awaiting_review")).assign).toContain("review");
    expect(taskActions(task("in_progress")).assign).toEqual([]);
  });

  it("closed tasks offer nothing", () => {
    for (const s of ["completed", "cancelled"] as TaskStatus[]) {
      const a = taskActions(task(s));
      expect([a.publish, a.complete, a.fail, a.cancel, a.retry, a.edit, a.assign.length > 0]).toEqual([
        false, false, false, false, false, false, false,
      ]);
    }
  });

  it("labels every status", () => {
    expect(Object.keys(STATUS_LABEL)).toHaveLength(10);
  });
});
