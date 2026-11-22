import { useState } from "react";

import { Badge, Button, ErrorNotice, Panel } from "../../components/ui";
import { providerName } from "../../lib/agents";
import { toCoreError, type CoreRequestError } from "../../lib/core";
import { formatRelative } from "../../lib/format";
import { useNow } from "../../lib/useNow";
import { useCoreQuery } from "../../stores/useCoreQuery";
import type { RoleBindings, TaskAssignment, TaskDetail } from "../../types/protocol";
import { ROLE_LABEL } from "../tasks/labels";
import { cancelWorkflow, pauseWorkflow, resumeWorkflow, startOrganization, startWorkflow, workflowSettings } from "./api";
import {
  BINDING_KEYS,
  BINDING_LABEL,
  WORKFLOW_STATUS_LABEL,
  effectiveBinding,
  humanizeSteps,
  modelLabel,
  overridesOf,
  stepName,
  workflowTone,
} from "./labels";
import { TaskFlow } from "./TaskFlow";
import { WorkBoard } from "./WorkBoard";
import { roleLanes } from "./board";
import { buildFlow } from "./flow";
import { TestResults } from "./TestResults";
import { OrganizationPanel } from "./OrganizationPanel";
import { RoleBindingsEditor } from "./RoleBindingsEditor";

const STEP_STATUS: Record<TaskAssignment["status"], string> = {
  pending: "Starting",
  active: "Working",
  reported: "Done",
  failed: "Failed",
  cancelled: "Cancelled",
};

function StepRow({ a, now }: { a: TaskAssignment; now: number }) {
  const running = a.status === "active" || a.status === "pending";
  const model = a.actual_model ?? modelLabel(a.model);
  const detail = `${a.step_key ?? ""} · ${ROLE_LABEL[a.role]} · ${providerName(a.provider)} · chosen ${modelLabel(a.model)}${a.actual_model ? ` · ran ${a.actual_model}` : ""}`;
  return (
    <div className={`step-row${running ? " current" : ""}`} role="listitem" title={detail}>
      <strong>{stepName(a.step_key ?? "")}</strong>
      <span className="step-agent">
        {providerName(a.provider)} <span className="muted">{model}</span>
      </span>
      <span className="step-outcome muted">{a.outcome ?? (running ? "Working…" : "")}</span>
      <span className="step-status">
        <Badge tone={a.status === "reported" ? "ok" : a.status === "failed" ? "error" : running ? "accent" : "neutral"}>{STEP_STATUS[a.status]}</Badge>
        <span className="muted small">{formatRelative(a.created_at_ms, now)}</span>
      </span>
    </div>
  );
}

/** Start: a team the lead supervisor builds for the objective, or the fixed sequence. */
function StartWorkflow({ detail, onDone }: { detail: TaskDetail; onDone: () => void }) {
  const [mode, setMode] = useState<"organization" | "engineering">("organization");
  const t = detail.task;
  if (t.status !== "ready") {
    return <p className="muted small">Work starts on a ready task (this one is {t.status}).</p>;
  }
  return (
    <div className="start-choice">
      <fieldset className="choice-group">
        <legend className="sr-only">How should the work be organized?</legend>
        <label className={`choice${mode === "organization" ? " chosen" : ""}`}>
          <input type="radio" name="start-mode" checked={mode === "organization"} onChange={() => setMode("organization")} />
          <span>
            <strong>Let a supervisor build the team</strong>
            <span className="muted small">
              A lead supervisor plans the work, brings in the specialists it needs, checks every result against the
              acceptance criteria and corrects what drifts. You answer questions and approve delivery.
            </span>
          </span>
        </label>
        <label className={`choice${mode === "engineering" ? " chosen" : ""}`}>
          <input type="radio" name="start-mode" checked={mode === "engineering"} onChange={() => setMode("engineering")} />
          <span>
            <strong>Fixed sequence</strong>
            <span className="muted small">Investigate, implement, review; findings go back to reasoning and round again, with the project&apos;s roles and models.</span>
          </span>
        </label>
      </fieldset>
      {mode === "organization" ? <StartOrganization detail={detail} onDone={onDone} /> : <StartSequence detail={detail} onDone={onDone} />}
    </div>
  );
}

function StartOrganization({ detail, onDone }: { detail: TaskDetail; onDone: () => void }) {
  const [lead, setLead] = useState<"" | "claude_code" | "codex">("");
  const [error, setError] = useState<CoreRequestError | null>(null);
  const [busy, setBusy] = useState(false);
  return (
    <div className="start-org">
      <label className="field-inline small">
        Lead supervisor
        <select className="input" value={lead} onChange={(e) => setLead(e.target.value as typeof lead)}>
          <option value="">Let Switchyard choose</option>
          <option value="claude_code">Claude Code</option>
          <option value="codex">Codex</option>
        </select>
      </label>
      {error ? <ErrorNotice title="Cannot start the team" message={error.message} code={error.code} /> : null}
      <div className="form-actions start">
        <Button
          variant="primary"
          icon="agents"
          disabled={busy}
          onClick={() =>
            void (async () => {
              setBusy(true);
              setError(null);
              try {
                await startOrganization(detail.task.id, lead ? { provider: lead, model: null } : undefined);
                onDone();
              } catch (e) {
                setError(toCoreError(e));
              } finally {
                setBusy(false);
              }
            })()
          }
        >
          Start
        </Button>
      </div>
    </div>
  );
}

function StartSequence({ detail, onDone }: { detail: TaskDetail; onDone: () => void }) {
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
        Switchyard runs <strong>engineering@2</strong>: investigation, implementation and an independent review. When the
        review finds something, reasoning decides whether it gets fixed or disputed, for up to two rounds; then it stops for your
        delivery decision. It creates every assignment itself.
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
  const flow = detail.workflow ? buildFlow(detail, now) : null;
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
      <Panel title="Get it done">
        <StartWorkflow detail={detail} onDone={onChanged} />
      </Panel>
    );
  }
  if (w.definition === "organization") {
    return <OrganizationPanel detail={detail} onChanged={onChanged} />;
  }
  const steps = detail.assignments.filter((a) => a.step_key !== null);
  const final = w.status === "completed" || w.status === "delivered" || w.status === "cancelled" || w.status === "failed";
  return (
    <Panel
      title="Progress"
      actions={<Badge tone={workflowTone(w.status)}>{WORKFLOW_STATUS_LABEL[w.status]}</Badge>}
    >
      <TaskFlow detail={detail} />
      {flow ? <WorkBoard lanes={roleLanes(detail, flow, now)} /> : null}
      {w.plan && w.plan.length > 0 ? (
        <section aria-label="Work plan">
          <h3 className="policy-section">Work plan</h3>
          <ol className="plan-list small">
            {w.plan.map((p, i) => (
              <li key={`${i}-${p.title}`} className={p.done ? "done" : w.subtask === i + 1 ? "current" : ""}>
                <span className="plan-mark" aria-hidden="true">
                  {p.done ? "✓" : w.subtask === i + 1 ? "▸" : "·"}
                </span>
                <span>
                  {p.title}
                  <span className="sr-only">{p.done ? " (done)" : w.subtask === i + 1 ? " (in progress)" : " (to do)"}</span>
                </span>
              </li>
            ))}
          </ol>
        </section>
      ) : null}
      <TestResults detail={detail} now={now} />
      <p className="small">
        {w.status === "running" ? (
          <>
            {w.step_key ? stepName(w.step_key) : "The next step"} is running. Nothing is needed from you until something appears
            under “Needs you”.
          </>
        ) : null}
        {w.status !== "running" && w.status !== "waiting" && w.status !== "delivered" && w.reason ? <span>{humanizeSteps(w.reason)}</span> : null}
        {w.status === "waiting" ? <span className="muted">Stopped: see “Needs you” above.</span> : null}
      </p>
      {w.warnings.map((m) => (
        <p key={m} className="muted small">
          ⚠ {m}
        </p>
      ))}
      {error ? <ErrorNotice title="Workflow action failed" message={error.message} code={error.code} /> : null}
      {!final ? (
        <div className="form-actions start">
          {w.status === "paused" ? (
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
      {w.status === "failed" && w.step_key ? (
        <div className="form-actions start">
          <p className="small">
            The workflow stopped: {humanizeSteps(w.reason ?? "it failed")}. Continuing runs{" "}
            {stepName(w.step_key).toLowerCase()} again
            {detail.task.status === "failed" ? " (the task is retried first)" : ""}; everything done before it is kept.
          </p>
          <Button variant="primary" disabled={busy} onClick={() => act(() => resumeWorkflow(w.task_id))}>
            Continue the workflow
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
      <div className="step-table" role="list" aria-label="Workflow steps">
        {steps.map((a) => (
          <StepRow key={a.id} a={a} now={now} />
        ))}
      </div>
      <details className="small muted workflow-tech">
        <summary>Details</summary>
        <p>
          Workflow {w.definition}@{w.version} · automatic corrections {w.corrections} of {w.max_corrections}
        </p>
        <p>
        Agents chosen when it started:{" "}
        {BINDING_KEYS.map((k) => {
          const b = effectiveBinding(w.bindings, k);
          return `${BINDING_LABEL[k]} ${providerName(b.provider)} (${modelLabel(b.model)})`;
        }).join(" · ")}
        </p>
      </details>
    </Panel>
  );
}
