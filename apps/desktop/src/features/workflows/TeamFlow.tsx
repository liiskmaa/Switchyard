import { providerName } from "../../lib/agents";
import { useNow } from "../../lib/useNow";
import type { EventEnvelope, OrganizationInfo } from "../../types/protocol";
import { formatDuration } from "./flow";
import { type Handoff, type TeamNode, YOU, buildTeam } from "./team";
import { showLane } from "./WorkBoard";

const R = 32;

function curve(a: TeamNode, b: TeamNode): { d: string; mid: { x: number; y: number }; side: number } {
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  const len = Math.hypot(dx, dy) || 1;
  const ux = dx / len;
  const uy = dy / len;
  // Bend to the right of the travel direction, so down and up separate.
  const nx = -uy;
  const ny = ux;
  const sx = a.x + ux * (R + 4) + nx * 6;
  const sy = a.y + uy * (R + 4) + ny * 6;
  const ex = b.x - ux * (R + 8) + nx * 6;
  const ey = b.y - uy * (R + 8) + ny * 6;
  const cx = (a.x + b.x) / 2 + nx * 18;
  const cy = (a.y + b.y) / 2 + ny * 18;
  const mid = { x: 0.25 * sx + 0.5 * cx + 0.25 * ex, y: 0.25 * sy + 0.5 * cy + 0.25 * ey };
  // Labels sit beside the curve, on its outer side: a hand-off down and one up never overlap.
  const side = nx >= 0 ? 1 : -1;
  return { d: `M ${sx.toFixed(1)} ${sy.toFixed(1)} Q ${cx.toFixed(1)} ${cy.toFixed(1)} ${ex.toFixed(1)} ${ey.toFixed(1)}`, mid, side };
}

const short = (s: string, n: number) => (s.length > n ? `${s.slice(0, n - 1)}…` : s);
const same = (a: Handoff | null, from: string, to: string) => a !== null && a.from === from && a.to === to;

/**
 * The agent team as a live picture: who reports to whom, who is working or
 * checking now (with a timer), and every hand-off Core recorded between them.
 */
export function TeamFlow({ org, events, now: caption }: { org: OrganizationInfo; events: EventEnvelope[]; now: string }) {
  const nowMs = useNow(1000);
  const team = buildTeam(org, events, nowMs);
  const node = new Map(team.nodes.map((n) => [n.key, n]));
  // Routes: the reporting lines (faint, both directions possible) plus every used hand-off.
  const counts = new Map<string, number>();
  for (const h of team.handoffs) counts.set(`${h.from}>${h.to}`, (counts.get(`${h.from}>${h.to}`) ?? 0) + 1);
  const routes = new Map<string, [string, string]>();
  for (const n of team.nodes) if (n.parent && !counts.has(`${n.key}>${n.parent}`)) routes.set(`${n.parent}>${n.key}`, [n.parent, n.key]);
  for (const k of counts.keys()) routes.set(k, k.split(">") as [string, string]);
  if (team.active) routes.set(`${team.active.from}>${team.active.to}`, [team.active.from, team.active.to]);
  const lastLabel = (from: string, to: string) => [...team.handoffs].reverse().find((h) => h.from === from && h.to === to)?.label;
  // Room beside the outer nodes for their labels.
  const pad = 150;
  const vw = team.width + pad * 2;

  return (
    <figure className="task-flow team-flow">
      <svg
        viewBox={`${-pad} 0 ${vw} ${team.height}`}
        width={vw}
        role="img"
        aria-label={`Agent team. ${caption}`}
      >
        <defs>
          <marker id="team-arrow" viewBox="0 0 10 10" refX="7" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
            <path d="M 0 0 L 10 5 L 0 10 z" className="flow-arrowhead" />
          </marker>
          <marker id="team-arrow-active" viewBox="0 0 10 10" refX="7" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
            <path d="M 0 0 L 10 5 L 0 10 z" className="flow-arrowhead active" />
          </marker>
        </defs>
        {[...routes.values()].map(([from, to]) => {
          const a = node.get(from);
          const b = node.get(to);
          if (!a || !b) return null;
          const { d, mid, side } = curve(a, b);
          const n = counts.get(`${from}>${to}`) ?? 0;
          const isActive = same(team.active, from, to);
          const cls = isActive ? "flow-route active" : n > 0 ? "flow-route used" : "flow-route";
          const label = isActive ? team.active?.label : n > 0 ? `${lastLabel(from, to) ?? ""}${n > 1 ? ` ×${n}` : ""}` : null;
          return (
            <g key={`${from}>${to}`} className={cls}>
              <path d={d} markerEnd={n > 0 || isActive ? `url(#${isActive ? "team-arrow-active" : "team-arrow"})` : undefined} />
              {label ? (
                <text
                  x={mid.x + side * 10}
                  y={mid.y + 4}
                  className={`flow-label${isActive ? " active" : ""}`}
                  textAnchor={side > 0 ? "start" : "end"}
                >
                  {short(label, 26)}
                </text>
              ) : null}
            </g>
          );
        })}
        {team.nodes.map((n) => {
          const time = team.times[n.key];
          const running = time?.runningSinceMs !== undefined;
          const load = team.load[n.key];
          const sub =
            n.key === YOU
              ? null
              : `${providerName(n.provider ?? "")}${n.model ? ` · ${short(n.model, 14)}` : ""}${load ? ` · ${load[1]}/${load[0] + load[1]} done` : ""}`;
          return (
            <g key={n.key} className={`flow-node ${n.state}${n.key === YOU ? "" : " clickable"}`} onClick={() => showLane(n.key)}>
              <title>{n.key === YOU ? "You" : `${n.title} (${n.key})`}</title>
              {n.state === "working" || n.state === "checking" || n.state === "needs_you" ? (
                <circle cx={n.x} cy={n.y} r={R + 6} className="flow-halo" />
              ) : null}
              <circle cx={n.x} cy={n.y} r={R} className="flow-disc" />
              <text x={n.x} y={time ? n.y - 2 : n.y + 4} textAnchor="middle" className="flow-name">
                {short(n.title, 13)}
              </text>
              {time ? (
                <text x={n.x} y={n.y + 13} textAnchor="middle" className={`flow-time${running ? " running" : ""}`}>
                  <title>{`${formatDuration(time.totalMs)} in total`}</title>
                  {running ? formatDuration(nowMs - (time.runningSinceMs ?? nowMs)) : formatDuration(time.totalMs)}
                </text>
              ) : null}
              {sub ? (
                <text x={n.x} y={n.y + R + 16} textAnchor="middle" className="flow-sub">
                  {sub}
                </text>
              ) : null}
            </g>
          );
        })}
      </svg>
      <figcaption className="small">{caption}</figcaption>
    </figure>
  );
}
