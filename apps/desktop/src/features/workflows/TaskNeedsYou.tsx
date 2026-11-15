import { useState } from "react";

import { Button, ErrorNotice } from "../../components/ui";
import { toCoreError, type CoreRequestError } from "../../lib/core";
import { useCoreQuery } from "../../stores/useCoreQuery";
import type { TaskDetail } from "../../types/protocol";
import { isApprovalEvent } from "../agents/api";
import { ApprovalCard } from "../approvals/ApprovalCard";
import { cancelWorkflow, pendingApprovals, resumeWorkflow } from "./api";
import { DeliveryPanel } from "./DeliveryPanel";
import { humanizeSteps } from "./labels";

/** Whether the task's delivery step is waiting for the user (shown in "Needs you"). */
export const deliveryNeedsYou = (d: TaskDetail): boolean =>
  d.workflow?.stage === "delivery" && d.workflow.status === "waiting";

/**
 * The one thing on this task that waits for the user, shown first: an
 * agent's permission request, a workflow that stopped and says why, or the
 * delivery step that is the user's to take.
 */
export function TaskNeedsYou({ detail, onChanged }: { detail: TaskDetail; onChanged: () => void }) {
  const w = detail.workflow;
  const session = detail.task.open_assignment?.session_id ?? null;
  const approvals = useCoreQuery(`task-approvals:${session ?? "none"}`, () => (session ? pendingApprovals(session) : Promise.resolve([])), {
    refreshOn: isApprovalEvent,
    intervalMs: 5_000,
  });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<CoreRequestError | null>(null);
  const pending = approvals.data ?? [];
  const delivery = deliveryNeedsYou(detail);
  const stopped = w?.status === "waiting" && !delivery;
  if (pending.length === 0 && !stopped && !delivery) return null;
  const act = (f: () => Promise<unknown>) =>
    void (async () => {
      setBusy(true);
      setError(null);
      try {
        await f();
        onChanged();
      } catch (e) {
        setError(toCoreError(e));
      } finally {
        setBusy(false);
      }
    })();
  return (
    <section className="needs-you" aria-labelledby="task-needs-you">
      <h2 id="task-needs-you" className="home-heading">
        Needs you
      </h2>
      {pending.map((a) => (
        <ApprovalCard
          key={a.id}
          approval={a}
          onDecided={() => {
            approvals.refresh();
            onChanged();
          }}
        />
      ))}
      {stopped && w ? (
        <div className="needs-card">
          <p>{humanizeSteps(w.reason ?? "")}</p>
          {error ? <ErrorNotice title="Not done" message={error.message} code={error.code} /> : null}
          <div className="form-actions start">
            <Button variant="primary" disabled={busy} onClick={() => act(() => resumeWorkflow(w.task_id))}>
              {w.resume_action ? `Resume: ${w.resume_action}` : "Resume"}
            </Button>
            <Button variant="ghost" disabled={busy} onClick={() => act(() => cancelWorkflow(w.task_id))}>
              Stop the workflow
            </Button>
          </div>
        </div>
      ) : null}
      {delivery ? <DeliveryPanel detail={detail} onChanged={onChanged} /> : null}
    </section>
  );
}
