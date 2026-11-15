import { useState } from "react";

import { Button, EmptyState, ErrorNotice, PageHeader } from "../../components/ui";
import { coreRequest } from "../../lib/core";
import { formatDuration, formatRelative } from "../../lib/format";
import { useNow } from "../../lib/useNow";
import { useConnection } from "../../stores/core";
import { navigate } from "../../stores/navigation";
import { useCoreQuery } from "../../stores/useCoreQuery";
import type { Approval, EventEnvelope, HealthReport, Project, Task, WorkflowInfo } from "../../types/protocol";
import { ApprovalCard } from "../approvals/ApprovalCard";
import { listTasks } from "../tasks/api";
import { humanizeSteps, workflowStageText } from "../workflows/labels";

const fetchHealth = () => coreRequest("system.health", {});

interface Item {
  task: Task;
  project: Project;
  workflow: WorkflowInfo | null;
}

interface HomeData {
  projects: Project[];
  items: Item[];
  approvals: Approval[];
}

/** Everything Home needs: open tasks with their workflows, and pending decisions. */
async function loadHome(): Promise<HomeData> {
  const projects = (await coreRequest("projects.list", {})).projects;
  const perProject = await Promise.all(projects.map(async (p) => (await listTasks(p.id, false)).map((task) => ({ task, project: p }))));
  const items = await Promise.all(
    perProject.flat().map(async (x) => ({ ...x, workflow: (await coreRequest("workflows.get", { task_id: x.task.id })).workflow })),
  );
  const approvals = (await coreRequest("approvals.list", { status: "pending", limit: 50 })).approvals;
  return { projects, items, approvals };
}

const homeEvent = (e: EventEnvelope) =>
  ["task.", "workflow.", "integration.", "approval."].some((p) => e.event_type.startsWith(p)) ||
  e.event_type === "agent.permission_requested" ||
  e.event_type === "agent.permission_decided" ||
  e.event_type === "project.added" ||
  e.event_type === "project.removed";

const openTask = (i: Item) => navigate("tasks", { taskProjectId: i.project.id, taskId: i.task.id });

/** Home: what needs you first, then what is running. System detail is one line, expandable. */
export function DashboardView({ onOpenProjects }: { onOpenProjects: () => void }) {
  const connection = useConnection();
  const now = useNow();
  const home = useCoreQuery("home", loadHome, { refreshOn: homeEvent, intervalMs: 15_000 });
  const health = useCoreQuery<HealthReport>("health", fetchHealth, { intervalMs: 15_000 });
  const [showSystem, setShowSystem] = useState(false);
  const d = home.data;
  const connected = connection.state === "connected";
  const taskOfSession = (sessionId: string) => d?.items.find((i) => i.task.open_assignment?.session_id === sessionId);
  const waiting = (d?.items ?? []).filter((i) => i.workflow?.status === "waiting");
  const running = (d?.items ?? []).filter(
    (i) => i.workflow?.status === "running" || i.workflow?.status === "paused" || (!i.workflow && i.task.open_assignment),
  );
  const needs = (d?.approvals.length ?? 0) + waiting.length;
  const h = health.data;

  return (
    <div className="page home">
      <PageHeader title="Home" />
      {home.error ? <ErrorNotice title="Cannot load your tasks" message={home.error.message} code={home.error.code} /> : null}

      {d && d.projects.length === 0 ? (
        <EmptyState icon="projects" title="Start by adding a project">
          <p>Switchyard works on a Git repository on this Mac.</p>
          <Button variant="primary" icon="plus" onClick={onOpenProjects}>
            Add a repository
          </Button>
        </EmptyState>
      ) : null}

      {d && d.projects.length > 0 ? (
        <section className="home-section" aria-labelledby="needs-you">
          <h2 id="needs-you" className="home-heading">
            {needs > 0 ? `Needs you (${needs})` : "Nothing needs you right now"}
          </h2>
          {needs === 0 ? (
            <p className="muted">Agents ask here when they need a permission, and tasks appear here when a decision is yours.</p>
          ) : null}
          {d.approvals.map((a) => {
            const item = taskOfSession(a.session_id);
            return (
              <ApprovalCard
                key={a.id}
                approval={a}
                context={item?.task.title}
                projectName={d.projects.find((p) => p.id === a.project_id)?.name}
                onDecided={home.refresh}
              />
            );
          })}
          {waiting.map((i) => (
            <button key={i.task.id} type="button" className="home-row home-row-needs" onClick={() => openTask(i)}>
              <span className="home-row-main">
                <span className="home-row-title">{i.task.title}</span>
                <span className="home-row-reason">{humanizeSteps(i.workflow?.reason ?? "")}</span>
              </span>
              <span className="home-row-side">Open →</span>
            </button>
          ))}
        </section>
      ) : null}

      {running.length > 0 ? (
        <section className="home-section" aria-labelledby="in-progress">
          <h2 id="in-progress" className="home-heading">
            In progress
          </h2>
          {running.map((i) => (
            <button key={i.task.id} type="button" className="home-row" onClick={() => openTask(i)}>
              <span className="home-row-main">
                <span className="home-row-title">{i.task.title}</span>
                <span className="home-row-reason">
                  {i.workflow ? workflowStageText(i.workflow) : "An agent is working on it"}
                  {d && d.projects.length > 1 ? ` · ${i.project.name}` : ""}
                </span>
              </span>
              <span className="home-row-side muted small">{formatRelative(i.task.updated_at_ms, now)}</span>
            </button>
          ))}
        </section>
      ) : null}

      <footer className="home-system">
        <button type="button" className="link small" onClick={() => setShowSystem(!showSystem)} aria-expanded={showSystem}>
          {connected ? "Switchyard is running" : connection.state === "connecting" ? "Connecting to Switchyard…" : "Switchyard is offline"}
          {d ? ` · ${d.projects.length} project${d.projects.length === 1 ? "" : "s"}` : ""}
          {h ? ` · ${h.active_sessions_count} agent${h.active_sessions_count === 1 ? "" : "s"} working` : ""}
          {showSystem ? " · hide details" : " · details"}
        </button>
        {showSystem && h ? (
          <dl className="kv home-system-details">
            <div className="kv-row">
              <dt>Core</dt>
              <dd>
                v{h.core.version} · up {formatDuration(h.uptime_ms)}
              </dd>
            </div>
            <div className="kv-row">
              <dt>Database</dt>
              <dd>
                {h.database.initialized ? "Ready" : "Degraded"}
                {h.database.error ? ` · ${h.database.error}` : ` · schema v${h.database.schema_version ?? "?"}`}
              </dd>
            </div>
            <div className="kv-row">
              <dt>Activity</dt>
              <dd>
                <button type="button" className="link" onClick={() => navigate("activity")}>
                  Open the activity log
                </button>
              </dd>
            </div>
          </dl>
        ) : null}
      </footer>
    </div>
  );
}
