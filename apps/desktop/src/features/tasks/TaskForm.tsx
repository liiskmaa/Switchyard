import { useState } from "react";

import { Button, ErrorNotice } from "../../components/ui";
import { toCoreError, type CoreRequestError } from "../../lib/core";
import type { Task, TaskKind, TaskPriority } from "../../types/protocol";
import { createTask } from "./api";

const KINDS: TaskKind[] = ["implementation", "investigation", "general"];
const PRIORITIES: TaskPriority[] = ["low", "normal", "high", "urgent"];

/** Creates a task. Core validates it and checks dependencies for cycles. */
export function TaskForm({
  projectId,
  tasks,
  onDone,
  onCancel,
}: {
  projectId: string;
  tasks: Task[];
  onDone: (task: Task) => void;
  onCancel: () => void;
}) {
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [kind, setKind] = useState<TaskKind>("implementation");
  const [priority, setPriority] = useState<TaskPriority>("normal");
  const [criteria, setCriteria] = useState("");
  const [dependsOn, setDependsOn] = useState<string[]>([]);
  const [publish, setPublish] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<CoreRequestError | null>(null);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const { task } = await createTask({
        project_id: projectId,
        title,
        ...(description.trim() ? { description } : {}),
        kind,
        priority,
        acceptance_criteria: criteria.split("\n").map((c) => c.trim()).filter((c) => c !== ""),
        depends_on: dependsOn,
        publish,
      });
      onDone(task);
    } catch (err) {
      setError(toCoreError(err));
    } finally {
      setBusy(false);
    }
  }

  const candidates = tasks.filter((t) => t.status !== "cancelled");
  return (
    <form className="add-project rule-form" onSubmit={(e) => void submit(e)} aria-label="New task">
      <label htmlFor="task-title">Title</label>
      <input id="task-title" className="input" value={title} maxLength={200} onChange={(e) => setTitle(e.target.value)} required />
      <label htmlFor="task-description">Description</label>
      <textarea
        id="task-description"
        className="input textarea"
        rows={4}
        value={description}
        onChange={(e) => setDescription(e.target.value)}
        placeholder="What needs to be done, and any context the agent needs."
      />
      <div className="rule-form-grid">
        <div>
          <label htmlFor="task-kind">Kind</label>
          <select id="task-kind" className="input select" value={kind} onChange={(e) => setKind(e.target.value as TaskKind)}>
            {KINDS.map((k) => (
              <option key={k} value={k}>
                {k}
              </option>
            ))}
          </select>
        </div>
        <div>
          <label htmlFor="task-priority">Priority</label>
          <select
            id="task-priority"
            className="input select"
            value={priority}
            onChange={(e) => setPriority(e.target.value as TaskPriority)}
          >
            {PRIORITIES.map((p) => (
              <option key={p} value={p}>
                {p}
              </option>
            ))}
          </select>
        </div>
      </div>
      <label htmlFor="task-criteria">Acceptance criteria (one per line)</label>
      <textarea id="task-criteria" className="input textarea" rows={3} value={criteria} onChange={(e) => setCriteria(e.target.value)} />
      {candidates.length > 0 ? (
        <>
          <label htmlFor="task-deps">Depends on (the task waits until these are completed)</label>
          <select
            id="task-deps"
            className="input select"
            multiple
            size={Math.min(5, candidates.length)}
            value={dependsOn}
            onChange={(e) => setDependsOn([...e.target.selectedOptions].map((o) => o.value))}
          >
            {candidates.map((t) => (
              <option key={t.id} value={t.id}>
                {t.title}
              </option>
            ))}
          </select>
        </>
      ) : null}
      <label className="check">
        <input type="checkbox" checked={publish} onChange={(e) => setPublish(e.target.checked)} /> Publish now (otherwise it stays a
        draft)
      </label>
      {error ? <ErrorNotice title="Cannot create the task" message={error.message} code={error.code} /> : null}
      <div className="form-actions">
        <Button onClick={onCancel}>Cancel</Button>
        <Button type="submit" variant="primary" disabled={busy || title.trim() === ""}>
          Create task
        </Button>
      </div>
    </form>
  );
}
