import { useCallback } from "react";

import { Badge, EmptyState, ErrorNotice, PageHeader, Panel } from "../../components/ui";
import { coreRequest } from "../../lib/core";
import { formatDuration } from "../../lib/format";
import { useNow } from "../../lib/useNow";
import { useConnection, useEventFeed } from "../../stores/core";
import { useCoreQuery } from "../../stores/useCoreQuery";
import type { EventEnvelope, HealthReport } from "../../types/protocol";
import { EventList } from "../activity/EventList";
import { STATE_LABEL, isAgentEvent, listSessions, stateTone } from "../agents/api";
import { isProjectEvent, listProjects } from "../projects/api";
import { navigate } from "../../stores/navigation";

const fetchHealth = () => coreRequest("system.health", {});

function Stat({ label, value, sub, tone }: { label: string; value: string; sub?: string | undefined; tone?: "ok" | "error" | "warn" | undefined }) {
  return (
    <div className="stat">
      <span className="stat-label">{label}</span>
      <span className={`stat-value${tone ? ` text-${tone}` : ""}`}>{value}</span>
      {sub ? <span className="stat-sub">{sub}</span> : null}
    </div>
  );
}

const countOrDash = (n: number | null | undefined) => (n === null || n === undefined ? "—" : String(n));

export function DashboardView({ onOpenProjects }: { onOpenProjects: () => void }) {
  const connection = useConnection();
  const refreshOn = useCallback((e: EventEnvelope) => isProjectEvent(e) || e.event_type === "agent.state_changed", []);
  const liveSessions = useCoreQuery("dashboard-live-sessions", () => listSessions({ live_only: true }), {
    refreshOn: (e) => isAgentEvent(e) && e.event_type !== "agent.message",
  });
  const health = useCoreQuery<HealthReport>("health", fetchHealth, { intervalMs: 5_000, refreshOn });
  const projects = useCoreQuery("projects", listProjects, { refreshOn: isProjectEvent });
  const feed = useEventFeed();
  const now = useNow();
  const h = health.data;
  const connected = connection.state === "connected";

  return (
    <div className="page">
      <PageHeader title="Dashboard" subtitle="State of the local Switchyard Core." />
      {health.error ? <ErrorNotice title="Health check failed" message={health.error.message} code={health.error.code} /> : null}
      <div className="stats">
        <Stat
          label="Core"
          value={connected ? "Connected" : connection.state === "connecting" ? "Connecting" : "Offline"}
          tone={connected ? "ok" : connection.state === "connecting" ? "warn" : "error"}
          sub={h ? `v${h.core.version} · up ${formatDuration(h.uptime_ms)}` : undefined}
        />
        <Stat
          label="Database"
          value={!h ? "—" : h.database.initialized ? "Ready" : "Degraded"}
          tone={!h ? undefined : h.database.initialized ? "ok" : "error"}
          sub={h ? (h.database.error ?? `${(h.database.journal_mode ?? "?").toUpperCase()} · schema v${h.database.schema_version ?? "?"}`) : undefined}
        />
        <Stat label="Projects" value={countOrDash(h?.projects_count)} sub="registered repositories" />
        <Stat label="Active sessions" value={countOrDash(h?.active_sessions_count)} sub="agent processes" />
      </div>

      <div className="grid-2">
        <Panel
          title="Projects"
          actions={
            <button type="button" className="link" onClick={onOpenProjects}>
              Manage
            </button>
          }
        >
          {!projects.data ? (
            <p className="muted">{connected ? "Loading…" : "Unavailable while Core is offline."}</p>
          ) : projects.data.length === 0 ? (
            <EmptyState icon="projects" title="No repositories registered">
              <button type="button" className="link" onClick={onOpenProjects}>
                Add a local Git repository
              </button>
            </EmptyState>
          ) : (
            <ul className="mini-list">
              {projects.data.slice(0, 8).map((p) => (
                <li key={p.id}>
                  <span className="mini-title">{p.name}</span>
                  <span className="mini-sub mono">{p.root_path}</span>
                  {!p.accessible ? <Badge tone="error">missing</Badge> : null}
                </li>
              ))}
            </ul>
          )}
        </Panel>
        <Panel title="Recent activity">
          {feed.events.length === 0 ? (
            <p className="muted">{feed.loaded ? "No events recorded yet." : "Waiting for Core…"}</p>
          ) : (
            <EventList events={feed.events.slice(-8)} now={now} compact />
          )}
        </Panel>
      </div>

      <Panel title="Active agent sessions">
        {(liveSessions.data ?? []).length === 0 ? (
          <EmptyState icon="agents" title="No agent sessions running">
            Start Claude Code or Codex from a project in Projects.
          </EmptyState>
        ) : (
          <ul className="mini-list">
            {(liveSessions.data ?? []).map((s) => (
              <li key={s.id}>
                <Badge tone={stateTone(s.state)}>{STATE_LABEL[s.state]}</Badge>
                <button type="button" className="link" onClick={() => navigate("agents", { sessionId: s.id })}>
                  {projects.data?.find((p) => p.id === s.project_id)?.name ?? "project"} · pid {s.pid}
                </button>
                {s.pending_approvals > 0 ? <Badge tone="warn">{s.pending_approvals} pending approval</Badge> : null}
              </li>
            ))}
          </ul>
        )}
      </Panel>
    </div>
  );
}
