import { type ReactNode, useState } from "react";

import { Badge } from "../../components/ui";
import { providerName } from "../../lib/agents";
import { formatRelative } from "../../lib/format";
import { useNow } from "../../lib/useNow";
import { navigate } from "../../stores/navigation";
import type { NodeTime } from "./flow";
import { formatDuration } from "./flow";

type Tone = "ok" | "warn" | "error" | "accent" | "neutral";

/** One thing a member works on. */
export interface BoardItem {
  id: string;
  title: string;
  status: string;
  tone: Tone;
  /** Being worked on right now. */
  current: boolean;
  /** Finished (accepted, reported, passed…): listed after the open ones. */
  done: boolean;
  meta?: string;
  /** Shown when the item is opened. */
  details: [string, ReactNode][];
  /** Links shown when the item is opened. */
  links: { label: string; open: () => void }[];
}

export interface BoardLane {
  key: string;
  title: string;
  subtitle: string;
  state: "working" | "checking" | "needs_you" | "done" | "idle";
  time?: NodeTime | undefined;
  items: BoardItem[];
}

const STATE_TEXT: Record<BoardLane["state"], string> = {
  working: "working now",
  checking: "checking work now",
  needs_you: "needs you",
  done: "done",
  idle: "",
};

export const laneId = (key: string) => `board-lane-${key.replace(/[^a-z0-9-]/gi, "_")}`;

/** Scrolls a lane into view and flashes it (from a click on the graph). */
export function showLane(key: string) {
  const el = document.getElementById(laneId(key));
  if (!el) return;
  el.scrollIntoView({ behavior: "smooth", block: "nearest" });
  el.classList.remove("flash");
  // Restart the animation.
  void el.offsetWidth;
  el.classList.add("flash");
}

function Item({ item }: { item: BoardItem }) {
  // Closed by default: the current item is highlighted, its details open on a click.
  const [open, setOpen] = useState(false);
  return (
    <li className={`board-item${item.current ? " current" : ""}${item.done ? " done" : ""}`}>
      <button type="button" className="board-item-head" aria-expanded={open} onClick={() => setOpen(!open)}>
        <span className="board-caret" aria-hidden="true">
          {open ? "▾" : "▸"}
        </span>
        <span className="board-item-title">{item.title}</span>
        <Badge tone={item.tone}>{item.status}</Badge>
      </button>
      {item.meta ? <span className="board-item-meta small muted">{item.meta}</span> : null}
      {open ? (
        <div className="board-item-body small">
          {item.details.length ? (
            <dl className="board-details">
              {item.details.map(([k, v]) => (
                <div key={k}>
                  <dt>{k}</dt>
                  <dd>{v}</dd>
                </div>
              ))}
            </dl>
          ) : (
            <p className="muted">No details recorded yet.</p>
          )}
          {item.links.length ? (
            <div className="board-links">
              {item.links.map((l) => (
                <button key={l.label} type="button" className="link" onClick={l.open}>
                  {l.label}
                </button>
              ))}
            </div>
          ) : null}
        </div>
      ) : null}
    </li>
  );
}

function Lane({ lane, now }: { lane: BoardLane; now: number }) {
  const [showDone, setShowDone] = useState(false);
  const open = lane.items.filter((i) => !i.done);
  const done = lane.items.filter((i) => i.done);
  const running = lane.time?.runningSinceMs;
  return (
    <section id={laneId(lane.key)} className={`board-lane ${lane.state}`} aria-label={lane.title}>
      <header className="board-lane-head">
        <span className={`org-dot ${lane.state === "working" || lane.state === "checking" ? "busy" : lane.state}`} aria-hidden="true" />
        <span className="board-lane-title">
          <strong>{lane.title}</strong>
          <span className="muted small">{lane.subtitle}</span>
        </span>
        <span className="board-lane-state small">
          {STATE_TEXT[lane.state]}
          {running !== undefined ? <span className="flow-time-inline running"> {formatDuration(now - running)}</span> : null}
          {lane.time && running === undefined ? <span className="muted"> {formatDuration(lane.time.totalMs)} in total</span> : null}
        </span>
      </header>
      {lane.items.length === 0 ? <p className="muted small board-empty">Nothing assigned yet.</p> : null}
      {open.length ? (
        <ul className="board-items">
          {open.map((i) => (
            <Item key={i.id} item={i} />
          ))}
        </ul>
      ) : lane.items.length ? (
        <p className="muted small board-empty">Nothing open.</p>
      ) : null}
      {done.length ? (
        <>
          <button type="button" className="link small" onClick={() => setShowDone(!showDone)}>
            {showDone ? "Hide finished" : `Finished (${done.length})`}
          </button>
          {showDone ? (
            <ul className="board-items">
              {done.map((i) => (
                <Item key={i.id} item={i} />
              ))}
            </ul>
          ) : null}
        </>
      ) : null}
    </section>
  );
}

/** What each member or role works on, as lanes of items you can open. */
export function WorkBoard({ lanes, title = "What each one works on" }: { lanes: BoardLane[]; title?: string }) {
  // A one-second clock keeps the working lane's timer live.
  const now = useNow(1000);
  return (
    <section className="board" aria-label={title}>
      <h3 className="policy-section">{title}</h3>
      <div className="board-lanes">
        {lanes.map((l) => (
          <Lane key={l.key} lane={l} now={now} />
        ))}
      </div>
    </section>
  );
}

// --- helpers for building lanes ---------------------------------------------------

export const providerModel = (provider: string, model: string | null | undefined) =>
  `${providerName(provider)} · ${model ?? "CLI default model"}`;

export const when = (ms: number | null | undefined, now: number) => (ms ? formatRelative(ms, now) : null);

export const openTask = (projectId: string, taskId: string) => () => navigate("tasks", { taskProjectId: projectId, taskId });
export const openSession = (sessionId: string) => () => navigate("agents", { sessionId });
