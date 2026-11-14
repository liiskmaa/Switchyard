import type { RoleBinding, RoleBindings, RoleBindingsPatch, WorkflowInfo, WorkflowStatus } from "../../types/protocol";

export type BindingKey = keyof RoleBindings;

export const BINDING_KEYS: BindingKey[] = ["investigate", "implement", "review", "correct"];

export const BINDING_LABEL: Record<BindingKey, string> = {
  investigate: "Investigation",
  implement: "Implementation",
  review: "Review",
  correct: "Correction",
};

export const WORKFLOW_STATUS_LABEL: Record<WorkflowStatus, string> = {
  pending: "Starting",
  running: "Running",
  waiting: "Needs you",
  paused: "Paused",
  completed: "Ready for delivery",
  delivered: "Delivered",
  cancelled: "Cancelled",
  failed: "Failed",
};

export function workflowTone(status: WorkflowStatus): "ok" | "warn" | "error" | "accent" | "neutral" {
  switch (status) {
    case "running":
    case "pending":
      return "accent";
    case "waiting":
    case "paused":
      return "warn";
    case "completed":
      return "ok";
    case "failed":
      return "error";
    default:
      return "neutral";
  }
}

/** The binding a role runs with; corrections default to the implementation's. */
export function effectiveBinding(b: RoleBindings, key: BindingKey): RoleBinding {
  return key === "correct" ? (b.correct ?? b.implement) : b[key];
}

export function modelLabel(model: string | null): string {
  return model ?? "CLI default";
}

/** The stages of `engineering@1` shown in order, with where the workflow is. */
export function stageOrder(w: WorkflowInfo): { key: string; label: string; state: "done" | "current" | "todo" }[] {
  const stages = ["investigation", "implementation", "review", "delivery"] as const;
  const labels: Record<(typeof stages)[number], string> = {
    investigation: "Investigate",
    implementation: "Implement",
    review: "Review & correct",
    delivery: "Deliver",
  };
  const current = w.stage === "correction" ? "review" : w.stage;
  const at = current ? stages.indexOf(current) : -1;
  return stages.map((s, i) => ({
    key: s,
    label: labels[s],
    state: w.status === "completed" || w.status === "delivered" || i < at ? "done" : i === at ? "current" : "todo",
  }));
}

/** Only the roles that differ from the project's bindings. */
export function overridesOf(project: RoleBindings, chosen: RoleBindings): RoleBindingsPatch {
  const patch: RoleBindingsPatch = {};
  for (const key of BINDING_KEYS) {
    const a = effectiveBinding(project, key);
    const b = effectiveBinding(chosen, key);
    if (a.provider !== b.provider || a.model !== b.model) patch[key] = b;
  }
  return patch;
}

