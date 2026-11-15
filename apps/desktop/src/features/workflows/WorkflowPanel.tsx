import { useState } from "react";

import { Badge, Button, ErrorNotice, Panel } from "../../components/ui";
import { providerName } from "../../lib/agents";
import { toCoreError, type CoreRequestError } from "../../lib/core";
import { formatRelative } from "../../lib/format";
import { useNow } from "../../lib/useNow";
import { useCoreQuery } from "../../stores/useCoreQuery";
import type { RoleBindings, TaskAssignment, TaskDetail } from "../../types/protocol";
import { isApprovalEvent } from "../agents/api";
import { ApprovalCard } from "../approvals/ApprovalCard";
import { ROLE_LABEL } from "../tasks/labels";
import { cancelWorkflow, pauseWorkflow, pendingApprovals, resumeWorkflow, startWorkflow, workflowSettings } from "./api";
import { BINDING_KEYS, BINDING_LABEL, WORKFLOW_STATUS_LABEL, effectiveBinding, modelLabel, overridesOf, stageOrder, workflowTone } from "./labels";
import { RoleBindingsEditor } from "./RoleBindingsEditor";

/** Permission requests of the running step, answered right here. */
function StepApprovals({ sessionId, onDecided }: { sessionId: string; onDecided: () => void }) {
  const approvals = useCoreQuery(`approvals:${sessionId}`, () => pendingApprovals(sessionId), {
    refreshOn: isApprovalEvent,
    intervalMs: 5_000,
  });
  const list = approvals.data ?? [];
  if (list.length === 0) return null;
  return (
    <div className="workflow-approvals">
      <h3 className="policy-section">Waiting for your approval</h3>
      {list.map((a) => (
        <ApprovalCard key={a.id} approval={a} onDecided={onDecided} />
      ))}
    </div>
  );
}

function StepRow({ a, now }: { a: TaskAssignment; now: number }) {
  const correction = a.role === "implement" && a.step_key !== null && !a.step_key.startsWith("implement#1");
  const running = a.status === "active" || a.status === "pending";
  const model = a.actual_model && a.actual_model !== a.model ? `${modelLabel(a.model)} → ${a.actual_model}` : modelLabel(a.model);
  return (
    <div className={`step-row${running ? " current" : ""}`} role="row">
      <code className="mono">{a.step_key}</code>
      <span>{correction ? "Correction" : ROLE_LABEL[a.role]}</span>
      <span className="step-agent">
        <strong>{providerName(a.provider)}</strong> <span className="muted">{model}</span>
      </span>
      <span className="step-outcome muted">{a.outcome ?? (running ? "working…" : "")}</span>
      <span className="step-status">
        <Badge tone={a.status === "reported" ? "ok" : a.status === "failed" ? "error" : running ? "accent" : "neutral"}>{a.status}</Badge>
        <span className="muted small">{formatRelative(a.created_at_ms, now)}</span>
      </span>
    </div>
  );
}

function StartWorkflow({ detail, onDone }: { detail: TaskDetail; onDone: () => void }) {
  const t = detail.task;
  const settings = useCoreQuery(`workflow-settings:${t.project_id}`, () => workflowSettings(t.project_id));
  const [chosen, setChosen] = useState<RoleBindings | null>(null);
  const [overriding, setOverriding] = useState(false);
  const [error, setError] = useState<CoreRequestError | null>(null);
  const [busy, setBusy] = useState(false);
  const project = settings.data?.bindings ?? null;
  const bindings = chosen ?? project;
  if (t.status !== "ready") {
    return <p className="muted small">The engineering workflow starts on a ready task (this one is {t.status}).</p>;
  }
  return (
    <div>
      <p className="small">
        Switchyard runs <strong>engineering@1</strong>: investigation, implementation, an independent review and up to two
        correction rounds, then stops for your delivery decision. It creates every assignment itself.
      </p>
      {bindings ? (
        <ul className="mini-list">
          {BINDING_KEYS.map((k) => {
            const b = effectiveBinding(bindings, k);
            return (
              <li key={k} className="small">
                {BINDING_LABEL[k]}: <strong>{providerName(b.provider)}</strong> · {modelLabel(b.model)}
              </li>
            );
          })}
        </ul>
      ) : null}
      {overriding && bindings ? <RoleBindingsEditor idPrefix="task-override" value={bindings} onChange={setChosen} /> : null}
      {error ? <ErrorNotice title="Cannot start the workflow" message={error.message} code={error.code} /> : null}
      <div className="form-actions start">
        <Button
          variant="primary"
          icon="agents"
          disabled={busy || !project}
          onClick={() =>
            void (async () => {
              setBusy(true);
              setError(null);
              try {
                const patch = project && chosen ? overridesOf(project, chosen) : undefined;
                await startWorkflow(t.id, patch && Object.keys(patch).length > 0 ? patch : undefined);
                onDone();
              } catch (e) {
                setError(toCoreError(e));
              } finally {
                setBusy(false);
              }
            })()
          }
        >
          Start workflow
        </Button>
        {!overriding ? (
          <Button variant="ghost" onClick={() => setOverriding(true)}>
            Override roles for this task
          </Button>
        ) : null}
      </div>
    </div>
  );
}

/** The workflow is the primary view of a task: stages, who runs each with which model, and what needs you. */
export function WorkflowPanel({ detail, onChanged }: { detail: TaskDetail; onChanged: () => void }) {
  const now = useNow();
  const w = detail.workflow;
  const [error, setError] = useState<CoreRequestError | null>(null);
  const [busy, setBusy] = useState(false);
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
  if (!w) {
    return (
      <Panel title="Workflow">
        <StartWorkflow detail={detail} onDone={onChanged} />
      </Panel>
    );
  }
  const steps = detail.assignments.filter((a) => a.step_key !== null);
  const running = steps.find((a) => a.status === "active" && a.session_id !== null);
  const final = w.status === "completed" || w.status === "delivered" || w.status === "cancelled" || w.status === "failed";
  return (
    <Panel
      title={`Workflow ${w.definition}@${w.version}`}
      actions={<Badge tone={workflowTone(w.status)}>{WORKFLOW_STATUS_LABEL[w.status]}</Badge>}
    >
      <ol className="workflow-stages" aria-label="Workflow stages">
        {stageOrder(w).map((s) => (
          <li key={s.key} className={`workflow-stage ${s.state}`}>
            {s.label}
          </li>
        ))}
      </ol>
      <p className="small">
        {w.status === "running" ? (
          <>
            Running <code className="code-tag">{w.step_key}</code> automatically — nothing needed from you unless an approval
            appears below.
          </>
        ) : null}
        {w.status !== "running" && w.reason ? <span className={w.status === "waiting" ? "workflow-reason" : ""}>{w.reason}</span> : null}
      </p>
      <p className="muted small">
        Correction rounds: {w.corrections} of {w.max_corrections} automatic
        {detail.task.status === "awaiting_review" && w.status === "running" ? " · the task status “awaiting review” here is an internal hand-off, not a request to you" : ""}
      </p>
      {w.warnings.map((m) => (
        <p key={m} className="muted small">
          ⚠ {m}
        </p>
      ))}
      {running?.session_id ? <StepApprovals sessionId={running.session_id} onDecided={onChanged} /> : null}
      {error ? <ErrorNotice title="Workflow action failed" message={error.message} code={error.code} /> : null}
      {!final ? (
        <div className="form-actions start">
          {w.status === "waiting" || w.status === "paused" ? (
            <Button variant="primary" disabled={busy} onClick={() => act(() => resumeWorkflow(w.task_id))}>
              Resume{w.resume_action ? `: ${w.resume_action}` : ""}
            </Button>
          ) : null}
          {w.status === "running" ? (
            <Button disabled={busy} onClick={() => act(() => pauseWorkflow(w.task_id))}>
              Pause
            </Button>
          ) : null}
          <Button variant="ghost" disabled={busy} onClick={() => act(() => cancelWorkflow(w.task_id))}>
            Cancel workflow
          </Button>
        </div>
      ) : null}
      {w.status === "completed" ? (
        <p className="small">
          <strong>Ready for delivery.</strong> Inspect, validate and approve the integration under Delivery; completing the task
          is your decision.
        </p>
      ) : null}
      <h3 className="policy-section">Steps</h3>
      <div className="step-table" role="table" aria-label="Workflow steps">
        <div className="step-row step-head" role="row">
          <span>Step</span>
          <span>Role</span>
          <span>Agent and model</span>
          <span>Result</span>
          <span>Status</span>
        </div>
        {steps.map((a) => (
          <StepRow key={a.id} a={a} now={now} />
        ))}
      </div>
      <p className="muted small">
        Bindings snapshotted when it started:{" "}
        {BINDING_KEYS.map((k) => {
          const b = effectiveBinding(w.bindings, k);
          return `${BINDING_LABEL[k]} ${providerName(b.provider)} (${modelLabel(b.model)})`;
        }).join(" · ")}
      </p>
    </Panel>
  );
}
