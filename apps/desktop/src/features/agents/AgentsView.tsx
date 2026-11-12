import { useEffect } from "react";

import { Badge, EmptyState, ErrorNotice, PageHeader } from "../../components/ui";
import { providerName } from "../../lib/agents";
import { formatRelative } from "../../lib/format";
import { useNow } from "../../lib/useNow";
import { navigate, useNavigation } from "../../stores/navigation";
import { useCoreQuery } from "../../stores/useCoreQuery";
import { listProjects } from "../projects/api";
import { STATE_LABEL, isAgentEvent, listSessions, stateTone } from "./api";
import { AvailabilityPanel } from "./AvailabilityPanel";
import { SessionView } from "./SessionView";

export function AgentsView() {
  const { sessionId } = useNavigation();
  const sessions = useCoreQuery("sessions", () => listSessions({ limit: 50 }), {
    refreshOn: (e) => isAgentEvent(e) && e.event_type !== "agent.message",
  });
  const projects = useCoreQuery("projects", listProjects);
  const now = useNow();
  const name = (id: string) => projects.data?.find((p) => p.id === id)?.name;
  const list = sessions.data ?? [];

  useEffect(() => {
    if (!sessionId && list[0]) navigate("agents", { sessionId: list[0].id });
  }, [sessionId, list]);

  const selected = list.find((s) => s.id === sessionId);
  return (
    <div className="page">
      <PageHeader title="Agents" subtitle="Native Claude Code and Codex sessions run and supervised by Switchyard Core." />
      <AvailabilityPanel />
      {sessions.error ? <ErrorNotice title="Cannot load sessions" message={sessions.error.message} code={sessions.error.code} /> : null}
      <div className="split">
        <div className="list" role="listbox" aria-label="Sessions">
          {list.length === 0 && sessions.data ? (
            <EmptyState icon="agents" title="No sessions yet">
              Start one from a project in Projects.
            </EmptyState>
          ) : null}
          {list.map((s) => (
            <button
              key={s.id}
              type="button"
              role="option"
              aria-selected={s.id === sessionId}
              className={`list-item${s.id === sessionId ? " active" : ""}`}
              onClick={() => navigate("agents", { sessionId: s.id })}
            >
              <span className="list-item-title">
                {s.workspace_name ?? name(s.project_id) ?? "project"}
                <Badge tone={stateTone(s.state)}>{STATE_LABEL[s.state]}</Badge>
                {s.pending_approvals > 0 ? <Badge tone="warn">{s.pending_approvals} approval</Badge> : null}
              </span>
              <span className="list-item-sub">
                {providerName(s.provider)} · {name(s.project_id) ?? "project"} · {formatRelative(s.last_activity_at_ms, now)}
              </span>
            </button>
          ))}
        </div>
        <div className="detail">
          {selected ? <SessionView key={selected.id} sessionId={selected.id} projectName={name(selected.project_id)} /> : null}
        </div>
      </div>
    </div>
  );
}
