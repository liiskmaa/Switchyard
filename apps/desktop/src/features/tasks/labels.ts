import type { Tone } from "../../components/ui";
import type { MessageKind, Severity, Task, TaskRole, TaskStatus } from "../../types/protocol";

export const STATUS_LABEL: Record<TaskStatus, string> = {
  draft: "Draft",
  ready: "Ready",
  blocked: "Blocked",
  assigned: "Assigned",
  in_progress: "In progress",
  awaiting_review: "Awaiting your review",
  in_review: "In review",
  completed: "Completed",
  failed: "Failed",
  cancelled: "Cancelled",
};

export function statusTone(s: TaskStatus): Tone {
  switch (s) {
    case "completed":
      return "ok";
    case "failed":
      return "error";
    case "blocked":
    case "awaiting_review":
      return "warn";
    case "assigned":
    case "in_progress":
    case "in_review":
      return "accent";
    default:
      return "neutral";
  }
}

export const ROLE_LABEL: Record<TaskRole, string> = {
  investigate: "Investigate",
  implement: "Implement",
  review: "Review",
};

export const SEVERITY_TONE: Record<Severity, Tone> = {
  info: "neutral",
  low: "neutral",
  medium: "warn",
  high: "error",
  critical: "error",
};

export const AGENT_KINDS: ReadonlySet<MessageKind> = new Set([
  "info_request",
  "findings",
  "proposal",
  "review_result",
  "blocker",
  "delegation_request",
  "completion_report",
]);

export interface TaskActions {
  publish: boolean;
  assign: TaskRole[];
  complete: boolean;
  fail: boolean;
  cancel: boolean;
  retry: boolean;
  edit: boolean;
  dependencies: boolean;
}

/** What the operator can do with a task in its current state (Core re-checks everything). */
export function taskActions(t: Task): TaskActions {
  const s = t.status;
  const terminal = s === "completed" || s === "cancelled";
  const working = s === "assigned" || s === "in_progress" || s === "in_review";
  return {
    publish: s === "draft",
    assign: s === "ready" ? ["investigate", "implement"] : s === "awaiting_review" ? ["review", "implement", "investigate"] : [],
    complete: s === "awaiting_review",
    fail: s === "blocked" || s === "in_progress" || s === "awaiting_review" || s === "in_review",
    cancel: !terminal,
    retry: s === "failed" || s === "blocked",
    edit: !terminal && !working,
    dependencies: s === "draft" || s === "ready" || s === "blocked" || s === "failed",
  };
}
