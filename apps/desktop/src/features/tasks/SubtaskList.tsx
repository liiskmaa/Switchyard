import { Badge, Panel } from "../../components/ui";
import { coreRequest } from "../../lib/core";
import { navigate } from "../../stores/navigation";
import { useCoreQuery } from "../../stores/useCoreQuery";
import type { Task } from "../../types/protocol";
import { STATUS_LABEL, statusTone } from "./labels";

/**
 * A task's subtasks, finished ones included: the task list drops finished
 * subtasks, so this is where they stay reachable. A team's tasks are shown
 * in its plan instead (`hideTeam`).
 */
export function SubtaskList({ task, hideTeam }: { task: Task; hideTeam: boolean }) {
  const all = useCoreQuery(
    `subtasks:${task.id}`,
    async () => (await coreRequest("tasks.list", { project_id: task.project_id, include_closed: true, limit: 500 })).tasks,
    { refreshOn: (e) => e.event_type.startsWith("task.") },
  );
  const children = (all.data ?? []).filter((t) => t.parent_task_id === task.id);
  if (hideTeam || children.length === 0) return null;
  const open = children.filter((t) => !["completed", "cancelled"].includes(t.status)).length;
  return (
    <Panel title={`Subtasks (${open} open of ${children.length})`}>
      <ul className="mini-list">
        {children.map((t) => (
          <li key={t.id}>
            <button type="button" className="link" onClick={() => navigate("tasks", { taskId: t.id })}>
              {t.title}
            </button>{" "}
            <Badge tone={statusTone(t.status)}>{STATUS_LABEL[t.status]}</Badge>
          </li>
        ))}
      </ul>
    </Panel>
  );
}
