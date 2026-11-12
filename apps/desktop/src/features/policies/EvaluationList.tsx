import { Badge, EmptyState } from "../../components/ui";
import { formatRelative, formatTimestamp } from "../../lib/format";
import type { PolicyEvaluation } from "../../types/protocol";
import { CATEGORY_LABEL, REASON_LABEL, evaluationOutcome, evaluationTarget } from "./labels";

/** Recorded policy decisions, newest first, with their explanation. */
export function EvaluationList({ evaluations, empty }: { evaluations: PolicyEvaluation[]; empty: string }) {
  if (evaluations.length === 0) {
    return (
      <EmptyState icon="policy" title="No decisions yet">
        {empty}
      </EmptyState>
    );
  }
  const now = Date.now();
  return (
    <ol className="evaluations">
      {evaluations.map((e) => {
        const outcome = evaluationOutcome(e);
        return (
          <li key={e.id} className="evaluation">
            <div className="evaluation-head">
              <Badge tone={outcome.tone}>{outcome.label}</Badge>
              <span className="evaluation-what">
                {CATEGORY_LABEL[e.category]} <span className="muted">· {e.tool_name}</span>
              </span>
              <span className="muted small evaluation-where">
                {e.workspace_name ? `${e.workspace_name} · ` : ""}session {e.session_id.slice(-6)} ·{" "}
                <time title={formatTimestamp(e.evaluated_at_ms)}>{formatRelative(e.evaluated_at_ms, now)}</time>
              </span>
            </div>
            {evaluationTarget(e) ? <code className="evaluation-target">{evaluationTarget(e)}</code> : null}
            <p className="small evaluation-why">
              <span className="muted">{REASON_LABEL[e.reason]}:</span> {e.explanation}
            </p>
            <p className="muted small">
              Policy revision {e.policy_revision}
              {e.matched_rules.length > 1 ? ` · ${e.matched_rules.length} rules matched; the most restrictive decided` : ""}
            </p>
          </li>
        );
      })}
    </ol>
  );
}
