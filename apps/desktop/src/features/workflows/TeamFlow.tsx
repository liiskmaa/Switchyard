import { providerName } from "../../lib/agents";
import { useNow } from "../../lib/useNow";
import type { EventEnvelope, OrganizationInfo } from "../../types/protocol";
import { formatDuration } from "./flow";
import { type Handoff, type Keeper, type TeamNode, YOU, buildTeam } from "./team";
import { showLane } from "./WorkBoard";

const R = 32;

function curve(a: TeamNode, b: TeamNode): { d: string; at: (t: number) => { x: number; y: number } } {
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
  // A point on the quadratic curve.
  const at = (t: number) => ({
    x: (1 - t) ** 2 * sx + 2 * (1 - t) * t * cx + t ** 2 * ex,
    y: (1 - t) ** 2 * sy + 2 * (1 - t) * t * cy + t ** 2 * ey,
  });
  return { d: `M ${sx.toFixed(1)} ${sy.toFixed(1)} Q ${cx.toFixed(1)} ${cy.toFixed(1)} ${ex.toFixed(1)} ${ey.toFixed(1)}`, at };
}

/**
 * Where a route's label goes: outward from the parent (left for a child on
 * the left, right for one on the right), a hand-off down nearer the parent
 * and one up nearer the child, so the two labels of a pair never meet.
 */
function labelPlace(a: TeamNode, b: TeamNode, at: (t: number) => { x: number; y: number }) {
  const down = b.depth > a.depth;
  const [parent, child] = down ? [a, b] : [b, a];
  const out = Math.sign(child.x - parent.x) || (down ? -1 : 1);
  const p = at(down ? 0.35 : 0.65);
  return { x: p.x + out * 12, y: p.y + 4, anchor: out > 0 ? "start" : "end" } as const;
}

/** A used route's label: "Delegated ×2" when every hand-off was the same, else the latest and the count. */
export function routeLabel(labels: string[]): string {
  const last = labels[labels.length - 1] ?? "";
  if (labels.length <= 1) return last;
  return labels.every((l) => l === last) ? `${last} ×${labels.length}` : `last: ${last} (${labels.length})`;
}

const short = (s: string, n: number) => (s.length > n ? `${s.slice(0, n - 1)}…` : s);

/** "Mailbox implementation specialist" → "MI" (first letters of the first two words). */
export function initials(title: string): string {
  const words = title.split(/[\s_-]+/).filter(Boolean);
  return words
    .slice(0, 2)
    .map((w) => w[0]?.toUpperCase() ?? "")
    .join("");
}

/** The name on up to two lines of about `width` characters; only a third line is cut. */
export function wrapName(title: string, width = 22): string[] {
  const lines: string[] = [];
  let line = "";
  for (const word of title.split(/\s+/).filter(Boolean)) {
    if (line && (line + " " + word).length > width) {
      lines.push(line);
      line = word;
    } else {
      line = line ? `${line} ${word}` : word;
    }
  }
  if (line) lines.push(line);
  if (lines.length > 2) return [lines[0] ?? "", short(lines.slice(1).join(" "), width)];
  return lines;
}
/**
 * The goal keeper beside you: a dashed ring (it is not an agent of the team),
 * who judges, and its status; a dashed line to the lead it watches, labelled
 * with the last verdict. A click opens the goal checklist below.
 */
function KeeperNode({ k, lead }: { k: Keeper; lead: TeamNode | undefined }) {
  const label = `Goal keeper: ${k.referee ? `checked by ${k.referee}` : "no agent available"}. ${k.line}.`;
  const show = () => document.getElementById("goal-title")?.scrollIntoView({ behavior: "smooth", block: "start" });
  return (
    <g>
      {lead ? (
        <g className={`flow-route keeper-route ${k.state}`}>
          <path d={`M ${k.x} ${k.y + R + 4} Q ${k.x} ${(k.y + lead.y) / 2} ${lead.x + R + 4} ${lead.y - 6}`} />
          {k.verdict ? (
            <text x={k.x + 8} y={(k.y + lead.y) / 2 + 10} className="flow-label">
              {short(k.verdict, 18)}
            </text>
          ) : null}
        </g>
      ) : null}
      <g className={`flow-node keeper ${k.state} clickable`} onClick={show} role="button" aria-label={label}>
        <title>{`${label} It checks the code changes against the objective after accepted work and every 30 minutes. It never approves or completes anything.`}</title>
        {k.state === "checking" || k.state === "needs_you" ? <circle cx={k.x} cy={k.y} r={R + 6} className="flow-halo" /> : null}
        <circle cx={k.x} cy={k.y} r={R} className="flow-disc" />
        <text x={k.x} y={k.y + 4} textAnchor="middle" className="flow-name">
          GK
        </text>
        <text x={k.x} y={k.y + R + 16} textAnchor="middle" className="flow-node-title">
          Goal keeper
        </text>
        <text x={k.x} y={k.y + R + 30} textAnchor="middle" className="flow-sub">
          {k.referee ? short(k.referee, 24) : "no agent"}
        </text>
        <text x={k.x} y={k.y + R + 44} textAnchor="middle" className="flow-sub keeper-line">
          {short(k.line, 26)}
        </text>
      </g>
    </g>
  );
}

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
  const labelsOf = (from: string, to: string) => team.handoffs.filter((h) => h.from === from && h.to === to).map((h) => h.label);
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
          const { d, at } = curve(a, b);
          const place = labelPlace(a, b, at);
          const n = counts.get(`${from}>${to}`) ?? 0;
          const isActive = same(team.active, from, to);
          const cls = isActive ? "flow-route active" : n > 0 ? "flow-route used" : "flow-route";
          const label = isActive ? team.active?.label : n > 0 ? routeLabel(labelsOf(from, to)) : null;
          return (
            <g key={`${from}>${to}`} className={cls}>
              <path d={d} markerEnd={n > 0 || isActive ? `url(#${isActive ? "team-arrow-active" : "team-arrow"})` : undefined} />
              {label ? (
                <text x={place.x} y={place.y} className={`flow-label${isActive ? " active" : ""}`} textAnchor={place.anchor}>
                  <title>{label}</title>
                  {short(label, 20)}
                </text>
              ) : null}
            </g>
          );
        })}
        {team.keeper.watches ? <KeeperNode k={team.keeper} lead={node.get(team.keeper.watches)} /> : null}
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
                {n.key === YOU ? "You" : initials(n.title)}
              </text>
              {time ? (
                <text x={n.x} y={n.y + 13} textAnchor="middle" className={`flow-time${running ? " running" : ""}`}>
                  <title>{`${formatDuration(time.totalMs)} in total`}</title>
                  {running ? formatDuration(nowMs - (time.runningSinceMs ?? nowMs)) : formatDuration(time.totalMs)}
                </text>
              ) : null}
              {n.key === YOU
                ? null
                : wrapName(n.title).map((line, i) => (
                    <text key={i} x={n.x} y={n.y + R + 16 + i * 14} textAnchor="middle" className="flow-node-title">
                      {line}
                    </text>
                  ))}
              {sub ? (
                <text
                  x={n.x}
                  y={n.y + R + 16 + (n.key === YOU ? 0 : wrapName(n.title).length) * 14}
                  textAnchor="middle"
                  className="flow-sub"
                >
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
