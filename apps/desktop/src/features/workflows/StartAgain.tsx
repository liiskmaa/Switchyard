import { useState } from "react";

import { Button, ErrorNotice, Panel } from "../../components/ui";
import { toCoreError, type CoreRequestError } from "../../lib/core";
import { navigate } from "../../stores/navigation";
import type { TaskDetail } from "../../types/protocol";
import { startAgain } from "./api";

function TaskLink({ id, children }: { id: string; children: string }) {
  return (
    <button type="button" className="link" onClick={() => navigate("tasks", { taskId: id })}>
      {children}
    </button>
  );
}

/**
 * Who owns a task's work: the earlier task this one continues, the newer
 * task that took its work over, or, for a cancelled or failed task whose
 * work nobody continues yet, the choice to start it again with that work
 * (the same workspace and branch) or from scratch.
 */
export function StartAgain({ detail }: { detail: TaskDetail }) {
  const w = detail.workflow;
  const t = detail.task;
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<CoreRequestError | null>(null);
  if (!w) return null;
  // A task whose workflow ended is over even if an older Core left its status open.
  const ended = t.status === "cancelled" || t.status === "failed" || w.status === "cancelled" || w.status === "failed";
  const run = (keep: boolean) =>
    void (async () => {
      setBusy(true);
      setError(null);
      try {
        const r = await startAgain(t.id, keep);
        navigate("tasks", { taskId: r.task.id });
      } catch (e) {
        setError(toCoreError(e));
      } finally {
        setBusy(false);
      }
    })();
  return (
    <>
      {w.continues_task_id ? (
        <p className="small muted lineage">
          Continues the work of an <TaskLink id={w.continues_task_id}>earlier task</TaskLink> in the same workspace.
        </p>
      ) : null}
      {ended && w.continued_by_task_id ? (
        <p className="small lineage">
          This task's work continues in a <TaskLink id={w.continued_by_task_id}>newer task</TaskLink>.
        </p>
      ) : null}
      {ended && !w.continued_by_task_id ? (
        <Panel title="This task's work">
          <p className="small">
            The task is {t.status}, but the code its agents changed is still in its workspace, on its own branch. No task owns
            that work until you start it again.
          </p>
          <div className="form-actions start">
            <Button variant="primary" disabled={busy} onClick={() => run(true)}>
              Start again with this work
            </Button>
            <Button variant="ghost" disabled={busy} onClick={() => run(false)}>
              Start again from scratch
            </Button>
          </div>
          <p className="muted small">
            Either way a new task with the same title, description and criteria starts with the same kind of
            {w.definition === "organization" ? " team" : " workflow"}. With the work, it continues in this workspace and is
            told what was already accepted; from scratch, this work stays where it is.
          </p>
          {error ? <ErrorNotice title="Not started" message={error.message} code={error.code} /> : null}
        </Panel>
      ) : null}
    </>
  );
}
