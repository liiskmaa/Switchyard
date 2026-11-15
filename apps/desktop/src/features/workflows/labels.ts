import type { DeliveryPhase, RoleBinding, RoleBindings, RoleBindingsPatch, WorkflowInfo, WorkflowStatus } from "../../types/protocol";

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


/** Delivery phases in plain words. */
export const DELIVERY_LABEL: Record<DeliveryPhase, string> = {
  preparing: "Preparing the integration",
  needs_commit: "Commit the reviewed changes",
  validation_starting: "Starting validation",
  awaiting_validation_authorization: "Authorize validation",
  validating: "Validating",
  validation_failed: "Validation failed",
  blocked: "Delivery blocked",
  awaiting_approval: "Approve the integration",
  integrating: "Integrating",
  delivered: "Delivered",
  delivery_failed: "Integration failed",
  cancelled: "Integration cancelled",
};

/** Where a workflow is, in a few plain words (for lists and Home). */
export function workflowStageText(w: WorkflowInfo): string {
  if (w.status === "paused") return "Paused";
  if (w.status === "delivered") return "Delivered";
  if (w.stage === "delivery") return w.delivery ? DELIVERY_LABEL[w.delivery] : "Preparing delivery";
  const round = w.step_key ? Number(/#(\d+)/.exec(w.step_key)?.[1] ?? "1") : 1;
  switch (w.stage) {
    case "investigation":
      return "Investigating";
    case "implementation":
      return "Implementing";
    case "review":
      return round > 1 ? "Reviewing the correction" : "Reviewing";
    case "correction":
      return `Correcting (round ${round - 1})`;
    default:
      return "Starting";
  }
}

const STEP_KEY = /\b(investigate|implement|review)#(\d+)(?:@(\d+))?/g;

/** A workflow step in plain words: `implement#2` → "Correction 1", `review#2@2` → "Re-review (attempt 2)". */
export function stepName(key: string): string {
  return key.replace(STEP_KEY, (_m, role: string, roundText: string, attemptText?: string) => {
    const round = Number(roundText);
    const n = round > 2 ? ` ${round - 1}` : "";
    const base =
      role === "investigate"
        ? round > 1
          ? `Investigation ${round}`
          : "Investigation"
        : role === "implement"
          ? round > 1
            ? `Correction ${round - 1}`
            : "Implementation"
          : round > 1
            ? `Re-review${n}`
            : "Review";
    return attemptText ? `${base} (attempt ${attemptText})` : base;
  });
}

/** Core's workflow reasons name steps by key; show them in plain words. */
export const humanizeSteps = (text: string): string => stepName(text);
