import type { Tone } from "../../components/ui";
import type { OrganizationInfo, OrgGoalVerdict } from "../../types/protocol";

export type CheckStatus = "met" | "partial" | "open" | "unchecked";

export interface ChecklistRow {
  criterion: string;
  status: CheckStatus;
  evidence: string;
}

export interface GoalChecklist {
  rows: ChecklistRow[];
  /** The last verdict, or null before the first goal check. */
  verdict: OrgGoalVerdict | null;
  summary: string | null;
  atMs: number | null;
  by: string | null;
  offGoal: string[];
  /** Supervision turns in a row that accepted no work. */
  stalledTurns: number;
}

/**
 * The objective's progress checklist (ADR 0046): the last goal check's
 * criteria with their evidence, or, before the first check, the user's
 * done-when criteria as not checked yet. Only the referee's verdict marks a
 * criterion met; agents' reports never do.
 */
export function goalChecklist(o: OrganizationInfo): GoalChecklist {
  const root = o.tasks.find((t) => t.parent === null);
  const g = o.goal ?? null;
  const rows: ChecklistRow[] = g?.criteria.length
    ? g.criteria.map((c) => ({ criterion: c.criterion, status: c.status, evidence: c.evidence }))
    : (root?.acceptance ?? []).map((c) => ({ criterion: c, status: "unchecked" as const, evidence: "" }));
  return {
    rows,
    verdict: g?.verdict ?? null,
    summary: g?.summary ?? null,
    atMs: g?.at_ms ?? null,
    by: g?.by || null,
    offGoal: g?.off_goal ?? [],
    stalledTurns: o.turns_since_progress ?? 0,
  };
}

export const VERDICT_TEXT: Record<OrgGoalVerdict, [string, Tone]> = {
  done: ["Objective met", "ok"],
  on_track: ["On track", "ok"],
  drifting: ["Drifting", "error"],
  looping: ["Going in circles", "error"],
};

export const STATUS_TEXT: Record<CheckStatus, string> = {
  met: "Met",
  partial: "Partly",
  open: "Open",
  unchecked: "Not checked yet",
};
