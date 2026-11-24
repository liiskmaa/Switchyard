import { Badge } from "../../components/ui";
import { formatRelative } from "../../lib/format";
import { useNow } from "../../lib/useNow";
import type { OrganizationInfo } from "../../types/protocol";
import { STATUS_TEXT, VERDICT_TEXT, goalChecklist } from "./goal";

const ROW_TONE = { met: "ok", partial: "warn", open: "neutral", unchecked: "neutral" } as const;

/**
 * The objective and how far the team is (ADR 0046): each criterion met,
 * partly met or open, as judged by the last independent goal check from
 * the code changes, plus the loop guard's count.
 */
export function GoalPanel({ o }: { o: OrganizationInfo }) {
  const now = useNow(30_000);
  const c = goalChecklist(o);
  const title = (key: string) => o.tasks.find((t) => t.key === key)?.title ?? key;
  const met = c.rows.filter((r) => r.status === "met").length;
  return (
    <section className="goal-panel" aria-labelledby="goal-title">
      <div className="goal-head">
        <h3 id="goal-title" className="policy-section">
          Progress toward the goal
        </h3>
        {c.verdict ? <Badge tone={VERDICT_TEXT[c.verdict][1]}>{VERDICT_TEXT[c.verdict][0]}</Badge> : null}
        {c.rows.length ? (
          <span className="muted small">
            {met} of {c.rows.length} met
          </span>
        ) : null}
      </div>
      {c.rows.length ? (
        <ul className="goal-list">
          {c.rows.map((r, i) => (
            <li key={i} className={`goal-row goal-${r.status}`}>
              <Badge tone={ROW_TONE[r.status]}>{STATUS_TEXT[r.status]}</Badge>
              <span className="goal-criterion">{r.criterion}</span>
              {r.evidence ? <span className="muted small goal-evidence">{r.evidence}</span> : null}
            </li>
          ))}
        </ul>
      ) : (
        <p className="muted small">The objective has no done-when criteria; the first goal check derives them.</p>
      )}
      {c.summary ? <p className="small">{c.summary}</p> : null}
      {c.offGoal.length ? (
        <p className="small goal-off">Off the objective: {c.offGoal.map(title).join(", ")}</p>
      ) : null}
      <p className="muted small">
        {c.atMs
          ? `Checked ${formatRelative(c.atMs, now)}${c.by ? ` by ${c.by}` : ""}, from the code changes, not the agents' claims.`
          : "Not checked yet: an independent agent checks after accepted work and every 30 minutes."}
        {c.stalledTurns > 2 ? ` No work accepted in the last ${c.stalledTurns} supervision turns.` : ""}
      </p>
    </section>
  );
}
