import { useEffect, useState } from "react";

import { Badge, Button, EmptyState, ErrorNotice, PageHeader } from "../../components/ui";
import { providerName } from "../../lib/agents";
import { formatRelative } from "../../lib/format";
import { useNow } from "../../lib/useNow";
import { navigate, useNavigation } from "../../stores/navigation";
import { useCoreQuery } from "../../stores/useCoreQuery";
import { listProjects } from "../projects/api";
import { isWorkspaceEvent, listWorkspaces } from "../workspaces/api";
import { isTaskEvent, listTasks } from "./api";
import { ROLE_LABEL, STATUS_LABEL, statusTone } from "./labels";
import { TaskDetailView } from "./TaskDetailView";
import { TaskForm } from "./TaskForm";

/**
 * Tasks of one project: durable units of work that agents investigate,
 * implement and review. Core coordinates them; every decision is yours.
 */
export function TasksView() {
  const { taskId, taskProjectId } = useNavigation();
  const projects = useCoreQuery("projects", listProjects);
  // Kept in the navigation store, so the project and its selected task stay
  // together when the operator visits another section and comes back.
  const projectId = taskProjectId ?? "";
  const [includeClosed, setIncludeClosed] = useState(false);
  const [adding, setAdding] = useState(false);
  const now = useNow();

  useEffect(() => {
    const first = projects.data?.[0];
    if (!projectId && first) navigate("tasks", { taskProjectId: first.id, taskId: null });
  }, [projects.data, projectId]);

  const enabled = projectId !== "";
  const tasks = useCoreQuery(`tasks:${projectId}:${includeClosed}`, () => listTasks(projectId, includeClosed), {
    enabled,
    refreshOn: isTaskEvent,
  });
  const workspaces = useCoreQuery(`task-ws:${projectId}`, () => listWorkspaces(projectId, false), {
    enabled,
    refreshOn: isWorkspaceEvent,
  });
  const list = tasks.data ?? [];

  if (projects.data && projects.data.length === 0) {
    return (
      <div className="page">
        <PageHeader title="Tasks" />
        <EmptyState icon="projects" title="No projects yet">
          Register a repository under Projects; its tasks are managed here.
        </EmptyState>
      </div>
    );
  }

  // The selected task stays open after it is completed or cancelled, even though the list hides it.
  const selected = taskId;
  return (
    <div className="page">
      <PageHeader
        title="Tasks"
        subtitle="Durable work that agents investigate, implement and review. Agents report back; you decide what happens next."
        actions={
          adding ? null : (
            <Button icon="plus" variant="primary" disabled={!enabled} onClick={() => setAdding(true)}>
              New task
            </Button>
          )
        }
      />
      <div className="policy-scope">
        <label htmlFor="tasks-project">Project</label>
        <select
          id="tasks-project"
          className="input select"
          value={projectId}
          onChange={(e) => {
            setAdding(false);
            navigate("tasks", { taskProjectId: e.target.value, taskId: null });
          }}
        >
          {(projects.data ?? []).map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </select>
        <label className="check">
          <input type="checkbox" checked={includeClosed} onChange={(e) => setIncludeClosed(e.target.checked)} /> Show completed and
          cancelled
        </label>
      </div>
      {tasks.error ? <ErrorNotice title="Cannot load tasks" message={tasks.error.message} code={tasks.error.code} /> : null}
      {adding ? (
        <TaskForm
          projectId={projectId}
          tasks={list}
          onCancel={() => setAdding(false)}
          onDone={(t) => {
            setAdding(false);
            tasks.refresh();
            navigate("tasks", { taskId: t.id });
          }}
        />
      ) : null}
      <div className="split">
        <div className="list" role="listbox" aria-label="Tasks">
          {list.length === 0 && tasks.data ? (
            <EmptyState icon="approvals" title="No tasks">
              Create a task, then assign an agent to investigate or implement it.
            </EmptyState>
          ) : null}
          {list.map((t) => (
            <button
              key={t.id}
              type="button"
              role="option"
              aria-selected={t.id === selected}
              className={`list-item${t.id === selected ? " active" : ""}`}
              onClick={() => navigate("tasks", { taskId: t.id })}
            >
              <span className="list-item-title">
                {t.title}
                <Badge tone={statusTone(t.status)}>{STATUS_LABEL[t.status]}</Badge>
                {t.pending_proposals > 0 ? <Badge tone="warn">{t.pending_proposals} proposed</Badge> : null}
              </span>
              <span className="list-item-sub">
                {t.open_assignment
                  ? `${ROLE_LABEL[t.open_assignment.role]} · ${providerName(t.open_assignment.provider)} · `
                  : ""}
                {t.priority} · {formatRelative(t.updated_at_ms, now)}
              </span>
            </button>
          ))}
        </div>
        {selected ? (
          <TaskDetailView taskId={selected} tasks={list} workspaces={workspaces.data ?? []} onChanged={tasks.refresh} />
        ) : (
          <EmptyState icon="approvals" title="Select a task">
            Its assignments, messages, findings and history appear here.
          </EmptyState>
        )}
      </div>
    </div>
  );
}
