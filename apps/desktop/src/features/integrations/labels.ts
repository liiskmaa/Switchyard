import type { Tone } from "../../components/ui";
import type { Integration, IntegrationOperationKind, IntegrationStatus, RunStatus, StepStatus } from "../../types/protocol";

export const STATUS_LABEL: Record<IntegrationStatus, string> = {
  open: "Not ready",
  conflicted: "Conflicts",
  resolving: "Resolving conflicts",
  validating: "Validating",
  ready: "Ready for your approval",
  integrating: "Integrating",
  integrated: "Integrated",
  failed: "Failed",
  cancelled: "Cancelled",
};

export function statusTone(s: IntegrationStatus): Tone {
  switch (s) {
    case "integrated":
      return "ok";
    case "ready":
      return "accent";
    case "failed":
    case "conflicted":
      return "error";
    case "open":
    case "resolving":
      return "warn";
    default:
      return "neutral";
  }
}

export const RUN_TONE: Record<RunStatus, Tone> = {
  running: "accent",
  passed: "ok",
  failed: "error",
  cancelled: "warn",
  interrupted: "warn",
};

export const STEP_TONE: Record<StepStatus, Tone> = {
  pending: "neutral",
  running: "accent",
  passed: "ok",
  failed: "error",
  timed_out: "error",
  cancelled: "warn",
  refused: "warn",
  skipped: "neutral",
  error: "error",
};

export const shortOid = (oid: string | null | undefined): string => (oid ? oid.slice(0, 10) : "—");

export interface IntegrationActions {
  commit: boolean;
  update: boolean;
  abort: boolean;
  validate: boolean;
  approve: boolean;
  cancel: boolean;
  refresh: boolean;
}

const has = (i: Integration, code: string) => i.blockers.some((b) => b.code === code);

/** What the operator can do now. Core re-checks every request. */
export function integrationActions(i: Integration): IntegrationActions {
  const s = i.status;
  const idle = s === "open" || s === "conflicted" || s === "ready" || s === "failed";
  const sourceFinal = !["uncommitted_changes", "agent_working", "workspace_busy", "workspace_invalid", "out_of_date", "conflicts", "no_changes"].some(
    (c) => has(i, c),
  );
  return {
    commit: (idle || s === "resolving") && (has(i, "uncommitted_changes") || s === "resolving"),
    update: idle && (has(i, "out_of_date") || has(i, "conflicts")) && !has(i, "uncommitted_changes"),
    abort: s === "resolving",
    validate: idle && sourceFinal && (i.preflight.facts?.validations_configured ?? 0) > 0,
    approve: s === "ready" && i.blockers.length === 0,
    cancel: s !== "integrated" && s !== "cancelled" && s !== "integrating" && s !== "validating",
    // While no Core operation works on it, an integrating/validating
    // integration can be settled from Git again (after a Core restart).
    refresh: idle || s === "resolving" || ((s === "integrating" || s === "validating") && !operationRunning(i)),
  };
}

export function operationRunning(i: Integration): boolean {
  return i.operation?.status === "running";
}

const OPERATION_LABEL: Record<IntegrationOperationKind, string> = {
  approve: "Integrating the approved commits",
  commit_workspace: "Committing the workspace",
  update_from_target: "Updating the workspace from the target",
  abort_resolution: "Aborting the conflict resolution",
  validate: "Validating",
};

/** One line about the latest Core-owned operation, or null if there is nothing to say. */
export function operationLine(i: Integration): { tone: "info" | "warn" | "error"; text: string } | null {
  const op = i.operation;
  if (!op) return null;
  const what = OPERATION_LABEL[op.kind];
  switch (op.status) {
    case "running":
      return { tone: "info", text: `${what} (requested by ${op.requested_by}). Core continues even if you close the app or lose the connection.` };
    case "failed":
      return { tone: "error", text: `${what} did not complete: ${op.error ?? "no reason recorded"}` };
    case "interrupted":
      return { tone: "warn", text: `${what} was interrupted because Core stopped. ${op.error ?? ""}`.trim() };
    default:
      return null;
  }
}

/** The consequences of approving, in plain words. */
export function consequences(i: Integration): string[] {
  const checkout = i.preflight.facts?.target_checkout;
  const lines = [
    `Switchyard creates a merge commit whose parents are ${i.target_branch} at ${shortOid(i.target_commit)} and ${i.source_branch} at ${shortOid(i.source_commit)}.`,
    checkout
      ? `${i.target_branch} is checked out at ${checkout.path}: that checkout is fast-forwarded to the merge commit. Git refuses rather than overwrite local changes, and Switchyard refuses if an untracked or ignored file is in the way.`
      : `${i.target_branch} is moved to the merge commit only if it is still at ${shortOid(i.target_commit)} (compare-and-swap).`,
    "Nothing is pushed or deployed. The workspace and its branch are left as they are.",
    "If either commit has changed by the time you approve, nothing happens and the integration is checked again.",
  ];
  return lines;
}
