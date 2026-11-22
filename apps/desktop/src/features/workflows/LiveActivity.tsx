import { useEffect, useRef, useState } from "react";

import { providerName } from "../../lib/agents";
import { coreRequest } from "../../lib/core";
import { formatClock } from "../../lib/format";
import { useNow } from "../../lib/useNow";
import { navigate } from "../../stores/navigation";
import { useCoreEvents } from "../../stores/core";
import { useCoreQuery } from "../../stores/useCoreQuery";
import type { EventEnvelope, Session } from "../../types/protocol";
import { type Activity, activityOf } from "./activity";
import { formatDuration } from "./flow";

const PAGE = 1000;

/** Every event of a session, read once and then only what is new. */
function useSessionEvents(sessionId: string): EventEnvelope[] {
  const [events, setEvents] = useState<EventEnvelope[]>([]);
  const last = useRef(0);
  const busy = useRef(false);
  const again = useRef(false);
  const pull = () => {
    if (busy.current) {
      again.current = true;
      return;
    }
    busy.current = true;
    void (async () => {
      try {
        for (let page = 0; page < 10; page++) {
          const got = (await coreRequest("events.list", { session_id: sessionId, after_seq: last.current, limit: PAGE })).events;
          if (got.length === 0) break;
          last.current = got[got.length - 1]?.seq ?? last.current;
          setEvents((prev) => [...prev, ...got].slice(-600));
          if (got.length < PAGE) break;
        }
      } catch {
        // Shown again on the next event or tick.
      } finally {
        busy.current = false;
        if (again.current) {
          again.current = false;
          pull();
        }
      }
    })();
  };
  useEffect(() => {
    last.current = 0;
    setEvents([]);
    pull();
  }, [sessionId]);
  useCoreEvents((e) => {
    if (e.session_id === sessionId && e.event_type.startsWith("agent.")) pull();
  });
  return events;
}

const STATE_CLASS: Record<Activity["state"], string> = {
  running_tool: "working",
  thinking: "working",
  waiting_approval: "needs_you",
  idle: "idle",
};

function SessionActivity({ session, now }: { session: Session; now: number }) {
  const events = useSessionEvents(session.id);
  const a = activityOf(events);
  const who = `${providerName(session.provider)}${session.access === "review" ? " (read-only)" : ""}`;
  return (
    <div className={`live-activity ${STATE_CLASS[a.state]}`}>
      <div className="live-now">
        <span className="live-dot" aria-hidden="true" />
        <strong>{who}</strong>
        <span className="live-what">{a.now}</span>
        {a.sinceMs ? <span className="live-time">{formatDuration(now - a.sinceMs)}</span> : null}
        <button type="button" className="link small" onClick={() => navigate("agents", { sessionId: session.id })}>
          Open session
        </button>
      </div>
      {a.recent.length ? (
        <ol className="live-recent small">
          {a.recent.map((r, i) => (
            <li key={`${r.at}-${i}`} className={`live-${r.kind}`}>
              <span className="tl-time">{formatClock(r.at)}</span>
              <span className="live-text">{r.text}</span>
            </li>
          ))}
        </ol>
      ) : null}
    </div>
  );
}

/**
 * What the agents of a task are doing right now: for each live session in
 * the task's workspace, its current action (a command and how long it has
 * run, thinking, or waiting for you) and its last few actions, from Core's
 * journal. Nothing is shown when no agent runs.
 */
export function LiveActivity({ workspaceId }: { workspaceId: string | null | undefined }) {
  const now = useNow(1000);
  const sessions = useCoreQuery(
    `live-sessions:${workspaceId ?? "none"}`,
    async () => (workspaceId ? (await coreRequest("sessions.list", { workspace_id: workspaceId, live_only: true })).sessions : []),
    { refreshOn: (e) => e.workspace_id === workspaceId && e.event_type.startsWith("agent."), intervalMs: 10_000 },
  );
  const live = sessions.data ?? [];
  if (!workspaceId || live.length === 0) return null;
  return (
    <section className="live" aria-label="What the agents are doing now">
      {live.map((s) => (
        <SessionActivity key={s.id} session={s} now={now} />
      ))}
    </section>
  );
}
