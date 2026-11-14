import { describe, expect, it } from "vitest";

import type { RoleBindings, WorkflowInfo } from "../../types/protocol";
import { effectiveBinding, overridesOf, stageOrder } from "./labels";

const project: RoleBindings = {
  investigate: { provider: "claude_code", model: "sonnet" },
  implement: { provider: "codex", model: null },
  review: { provider: "claude_code", model: "opus" },
  correct: null,
};

describe("workflow role bindings", () => {
  it("corrections use the implementation binding unless set", () => {
    expect(effectiveBinding(project, "correct")).toEqual(project.implement);
    expect(effectiveBinding({ ...project, correct: { provider: "claude_code", model: "fable" } }, "correct").model).toBe("fable");
  });

  it("task overrides contain only the roles that differ", () => {
    expect(overridesOf(project, project)).toEqual({});
    const chosen = { ...project, review: { provider: "codex" as const, model: "gpt-6-luna" } };
    expect(overridesOf(project, chosen)).toEqual({ review: { provider: "codex", model: "gpt-6-luna" } });
  });
});

describe("workflow stages", () => {
  const w = (stage: WorkflowInfo["stage"], status: WorkflowInfo["status"]) => ({ stage, status }) as WorkflowInfo;
  it("a correction counts as the review stage", () => {
    expect(stageOrder(w("correction", "running")).map((s) => s.state)).toEqual(["done", "done", "current", "todo"]);
  });
  it("a completed workflow shows every stage done", () => {
    expect(stageOrder(w("delivery", "completed")).every((s) => s.state === "done")).toBe(true);
  });
});
