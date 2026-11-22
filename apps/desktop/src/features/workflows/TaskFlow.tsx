import { providerName } from "../../lib/agents";
import type { TaskDetail } from "../../types/protocol";
import { useNow } from "../../lib/useNow";
import { type FlowNode, NODE_LABEL, type Transfer, buildFlow, formatDuration, routeCounts } from "./flow";
import { effectiveBinding, modelLabel } from "./labels";
import { showLane } from "./WorkBoard";

/**
 * Where each role sits: the work loop (reasoning, coding, review) around you
 * at the hub, so every "needs you" is a short spoke; delivery on the right.
 */
const POS: Record<FlowNode, { x: number; y: number }> = {
  reasoning: { x: 140, y: 70 },
  coding: { x: 460, y: 70 },
  tests: { x: 540, y: 205 },
  review: { x: 300, y: 300 },
  you: { x: 300, y: 170 },
  validation: { x: 560, y: 330 },
  branch: { x: 700, y: 250 },
};
const R = 36;

/** The routes always drawn faintly, so the shape of the workflow is visible before anything moves. */
const ROUTES: [FlowNode, FlowNode][] = [
  ["you", "reasoning"],
  ["reasoning", "coding"],
  ["coding", "tests"],
  ["tests", "review"],
  ["review", "reasoning"],
  ["review", "validation"],
  ["validation", "branch"],
];

/** A curved path from one node to another; the two directions bend to opposite sides. */
function route(from: FlowNode, to: FlowNode): { d: string; mid: { x: number; y: number } } {
  const a = POS[from];
  const b = POS[to];
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  const len = Math.hypot(dx, dy) || 1;
  const ux = dx / len;
  const uy = dy / len;
  // Normal: always to the right of the travel direction, so A→B and B→A separate.
  const nx = -uy;
  const ny = ux;
  const bend = 22;
  const sx = a.x + ux * (R + 4) + nx * 6;
  const sy = a.y + uy * (R + 4) + ny * 6;
  const ex = b.x - ux * (R + 8) + nx * 6;
  const ey = b.y - uy * (R + 8) + ny * 6;
  const cx = (a.x + b.x) / 2 + nx * bend;
  const cy = (a.y + b.y) / 2 + ny * bend;
  // Point on the quadratic curve at t = 0.5.
  const mid = { x: 0.25 * sx + 0.5 * cx + 0.25 * ex, y: 0.25 * sy + 0.5 * cy + 0.25 * ey };
  return { d: `M ${sx.toFixed(1)} ${sy.toFixed(1)} Q ${cx.toFixed(1)} ${cy.toFixed(1)} ${ex.toFixed(1)} ${ey.toFixed(1)}`, mid };
}

const same = (a: Transfer | null, from: FlowNode, to: FlowNode) => a !== null && a.from === from && a.to === to;

/**
 * Live hand-offs between the roles of a task's workflow, like an energy-flow
 * display: the route in use animates in its direction, routes used before
 * stay lit with how often they were used. Driven by Core's records only.
 */
export function TaskFlow({ detail }: { detail: TaskDetail }) {
  // A one-second clock keeps the working role's timer live.
  const nowMs = useNow(1000);
  const flow = buildFlow(detail, nowMs);
  const w = detail.workflow;
  if (!flow || !w) return null;
  const counts = routeCounts(flow.transfers);
  const used = [...counts.keys()].map((k) => k.split(">") as [FlowNode, FlowNode]);
  const routes = new Map<string, [FlowNode, FlowNode]>();
  for (const [a, b] of ROUTES) if (!counts.has(`${b}>${a}`)) routes.set(`${a}>${b}`, [a, b]);
  for (const [a, b] of used) routes.set(`${a}>${b}`, [a, b]);

  const agent = (node: FlowNode): string | null => {
    if (node === "tests") return "run by Switchyard";
    const key = node === "reasoning" ? "investigate" : node === "coding" ? "implement" : node === "review" ? "review" : null;
    if (!key) return null;
    const b = effectiveBinding(w.bindings, key);
    return `${providerName(b.provider)} · ${modelLabel(b.model)}`;
  };
  const active = flow.active;
  const summary = active ? `${NODE_LABEL[active.from]} to ${NODE_LABEL[active.to]}: ${active.label}. ${flow.now}` : flow.now;

  return (
    <figure className="task-flow">
      <svg viewBox="0 0 760 390" role="img" aria-label={`Work flow. ${summary}`}>
        <defs>
          <marker id="flow-arrow" viewBox="0 0 10 10" refX="7" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
            <path d="M 0 0 L 10 5 L 0 10 z" className="flow-arrowhead" />
          </marker>
          <marker id="flow-arrow-active" viewBox="0 0 10 10" refX="7" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
            <path d="M 0 0 L 10 5 L 0 10 z" className="flow-arrowhead active" />
          </marker>
        </defs>
        {[...routes.values()].map(([from, to]) => {
          const { d, mid } = route(from, to);
          const n = counts.get(`${from}>${to}`) ?? 0;
          const isActive = same(active, from, to);
          const cls = isActive ? "flow-route active" : n > 0 ? "flow-route used" : "flow-route";
          return (
            <g key={`${from}>${to}`} className={cls}>
              <path d={d} markerEnd={n > 0 || isActive ? `url(#${isActive ? "flow-arrow-active" : "flow-arrow"})` : undefined} />
              {isActive && active ? (
                <text x={mid.x} y={mid.y - 8} className="flow-label active" textAnchor="middle">
                  {active.label}
                </text>
              ) : n > 1 ? (
                <text x={mid.x} y={mid.y - 6} className="flow-label" textAnchor="middle">
                  ×{n}
                </text>
              ) : null}
            </g>
          );
        })}
        {(Object.keys(POS) as FlowNode[]).map((node) => {
          const p = POS[node];
          const state = flow.nodes[node];
          const sub = node === "branch" ? null : agent(node);
          const time = flow.times[node];
          const running = time?.runningSinceMs !== undefined;
          return (
            <g
              key={node}
              className={`flow-node ${state}${node === "you" || node === "validation" || node === "branch" ? "" : " clickable"}`}
              onClick={() => showLane(node)}
            >
              {state === "working" || state === "needs_you" ? <circle cx={p.x} cy={p.y} r={R + 6} className="flow-halo" /> : null}
              <circle cx={p.x} cy={p.y} r={R} className="flow-disc" />
              <text x={p.x} y={time ? p.y - 2 : p.y + 4} textAnchor="middle" className="flow-name">
                {NODE_LABEL[node]}
              </text>
              {time ? (
                <text x={p.x} y={p.y + 13} textAnchor="middle" className={`flow-time${running ? " running" : ""}`}>
                  <title>{`${formatDuration(time.totalMs)} in total`}</title>
                  {running ? formatDuration(nowMs - (time.runningSinceMs ?? nowMs)) : formatDuration(time.totalMs)}
                </text>
              ) : null}
              {sub ? (
                <text x={p.x} y={p.y + R + 16} textAnchor="middle" className="flow-sub">
                  {sub}
                </text>
              ) : null}
            </g>
          );
        })}
      </svg>
      <figcaption className="small">{flow.now}</figcaption>
    </figure>
  );
}
