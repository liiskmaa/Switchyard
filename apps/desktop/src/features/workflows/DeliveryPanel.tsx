import { useState } from "react";

import { Badge, Button, ErrorNotice, Panel } from "../../components/ui";
import { toCoreError, type CoreRequestError } from "../../lib/core";
import { navigate } from "../../stores/navigation";
import { useCoreQuery } from "../../stores/useCoreQuery";
import type { DeliveryPhase, IntegrationDetail, TaskDetail, WorkflowInfo } from "../../types/protocol";
import {
  approveIntegration,
  commitWorkspace,
  eventOf,
  getIntegration,
  isIntegrationEvent,
  runValidation,
} from "../integrations/api";
import { Changes, Runs } from "../integrations/IntegrationDetailView";
import { consequences, shortOid } from "../integrations/labels";
import { isWorkflowEvent, requestCorrection, resumeWorkflow } from "./api";

/** Delivery phases in plain words. */
export const DELIVERY_LABEL: Record<DeliveryPhase, string> = {
  preparing: "Preparing the integration",
  needs_commit: "Commit the reviewed changes",
  validation_starting: "Starting validation",
  awaiting_validation_authorization: "Authorize validation",
  validating: "Validating",
  validation_failed: "Validation failed",
  blocked: "Delivery blocked",
  awaiting_approval: "Approve the integration",
  integrating: "Integrating",
  delivered: "Delivered",
  delivery_failed: "Integration failed",
  cancelled: "Integration cancelled",
};

function useRun(onDone: () => void) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<CoreRequestError | null>(null);
  const run = (f: () => Promise<unknown>) =>
    void (async () => {
      setBusy(true);
      setError(null);
      try {
        await f();
        onDone();
      } catch (e) {
        setError(toCoreError(e));
      } finally {
        setBusy(false);
      }
    })();
  return { busy, error, run };
}

function Commits({ d }: { d: IntegrationDetail }) {
  const i = d.integration;
  return (
    <dl className="kv delivery-commits">
      <div className="kv-row">
        <dt>Source</dt>
        <dd>
          {i.source_branch} at <code className="mono">{i.source_commit}</code>
        </dd>
      </div>
      <div className="kv-row">
        <dt>Target</dt>
        <dd>
          {i.target_branch} at <code className="mono">{i.target_commit}</code>
        </dd>
      </div>
    </dl>
  );
}

/** The final, explicit decision: the exact commits, the changes, validation and what approving does. */
function Approval({ d, w, onDone }: { d: IntegrationDetail; w: WorkflowInfo; onDone: () => void }) {
  const i = d.integration;
  const [confirming, setConfirming] = useState(false);
  const { busy, error, run } = useRun(onDone);
  // The commits the operator saw; approval sends exactly these.
  const [shown] = useState({ id: i.id, source_commit: i.source_commit, target_commit: i.target_commit });
  const moved = shown.source_commit !== i.source_commit || shown.target_commit !== i.target_commit;
  return (
    <>
      <Commits d={d} />
      <h3 className="policy-section">Validation</h3>
      {d.plan.length === 0 ? (
        <p className="role-warning small">No validation commands are configured for this project: nothing was validated.</p>
      ) : (
        <Runs runs={d.runs} source={i.source_commit} />
      )}
      <h3 className="policy-section">Changes</h3>
      <Changes i={i} />
      <h3 className="policy-section">What approving does</h3>
      <ul className="findings">
        {consequences(i).map((c) => (
          <li key={c} className="small">
            {c}
          </li>
        ))}
      </ul>
      {w.warnings.map((m) => (
        <p key={m} className="role-warning small">
          {m}
        </p>
      ))}
      {moved ? <p className="role-warning small">The commits changed since this view opened; reopen the task to review the new ones.</p> : null}
      {error ? <ErrorNotice title="The integration was not approved" message={error.message} code={error.code} /> : null}
      <div className="form-actions start">
        {!confirming ? (
          <Button variant="primary" disabled={busy || moved} onClick={() => setConfirming(true)}>
            Approve these commits…
          </Button>
        ) : (
          <>
            <Button variant="primary" disabled={busy || moved} onClick={() => run(() => approveIntegration(shown))}>
              {busy ? "Integrating…" : `Integrate into ${i.target_branch}`}
            </Button>
            <Button variant="ghost" disabled={busy} onClick={() => setConfirming(false)}>
              Back
            </Button>
          </>
        )}
      </div>
    </>
  );
}

/**
 * Delivery as part of the task (M8.2): what Switchyard is doing and the
 * decisions that are yours — committing the reviewed work, authorizing
 * validation commands, approving the exact commits.
 */
export function DeliveryPanel({ detail, onChanged }: { detail: TaskDetail; onChanged: () => void }) {
  const w = detail.workflow;
  const id = w?.integration_id ?? null;
  const integration = useCoreQuery(`delivery:${id ?? "none"}`, () => (id ? getIntegration(id) : Promise.resolve(null)), {
    refreshOn: (e) => (id !== null && isIntegrationEvent(e) && eventOf(e, id)) || (isWorkflowEvent(e) && e.task_id === detail.task.id),
  });
  const refresh = () => {
    integration.refresh();
    onChanged();
  };
  const action = useRun(refresh);
  const [message, setMessage] = useState(detail.task.title);
  const [authorized, setAuthorized] = useState<Record<string, boolean>>({});
  const [note, setNote] = useState("");
  if (!w || w.stage !== "delivery" || w.delivery === null) return null;
  const phase = w.delivery;
  const d = integration.data;
  const i = d?.integration;
  const failedStep = d?.runs[0]?.steps.find((s) => s.status !== "passed" && s.status !== "skipped");
  const tone = phase === "delivered" ? "ok" : ["blocked", "validation_failed", "delivery_failed"].includes(phase) ? "error" : "accent";
  return (
    <Panel
      title="Delivery"
      actions={
        <span className="delivery-head">
          <Badge tone={tone}>{DELIVERY_LABEL[phase]}</Badge>
          {i ? (
            <button
              type="button"
              className="link small"
              onClick={() => navigate("integrations", { integrationId: i.id, integrationProjectId: i.project_id })}
            >
              Open in Integrations
            </button>
          ) : null}
        </span>
      }
    >
      {w.reason ? <p className="small">{w.reason}</p> : null}
      {action.error ? <ErrorNotice title="Action failed" message={action.error.message} code={action.error.code} /> : null}

      {phase === "needs_commit" && i ? (
        <div className="delivery-step">
          <p className="muted small">
            The agents left their work uncommitted. Committing records exactly the reviewed content on {i.source_branch}; validation
            then runs on that commit.
          </p>
          <div className="input-row">
            <input
              className="input"
              aria-label="Commit message"
              value={message}
              onChange={(e) => setMessage(e.target.value)}
            />
            <Button
              variant="primary"
              disabled={action.busy || message.trim() === ""}
              onClick={() => action.run(() => commitWorkspace(i.id, message))}
            >
              Commit reviewed changes
            </Button>
          </div>
          <Changes i={i} />
        </div>
      ) : null}

      {phase === "awaiting_validation_authorization" && d && i ? (
        <div className="delivery-step">
          <p className="muted small">
            The project&apos;s policy asks before these commands run. They run in the task&apos;s workspace with your user&apos;s
            permissions.
          </p>
          <ul className="findings">
            {d.plan.map((p) => (
              <li key={p.command_id} className="small">
                <label className="check">
                  <input
                    type="checkbox"
                    disabled={p.policy_decision !== "ask"}
                    checked={p.policy_decision === "allow" || authorized[p.command_id] === true}
                    onChange={(e) => setAuthorized({ ...authorized, [p.command_id]: e.target.checked })}
                  />
                  <strong>{p.name}</strong> <code>{p.command_line}</code> <Badge>{p.policy_decision}</Badge>
                </label>
              </li>
            ))}
          </ul>
          <div className="form-actions start">
            <Button
              variant="primary"
              disabled={action.busy || d.plan.some((p) => p.policy_decision === "ask" && authorized[p.command_id] !== true)}
              onClick={() =>
                action.run(() =>
                  runValidation(
                    i.id,
                    d.plan
                      .filter((p) => p.policy_decision === "ask")
                      .map((p) => ({ command_id: p.command_id, command_line: p.command_line })),
                  ),
                )
              }
            >
              Authorize and validate
            </Button>
          </div>
        </div>
      ) : null}

      {(phase === "validating" || phase === "validation_starting") && d && i ? <Runs runs={d.runs} source={i.source_commit} /> : null}

      {phase === "validation_failed" && d && i ? (
        <div className="delivery-step">
          <Runs runs={d.runs} source={i.source_commit} />
          <textarea
            className="input textarea"
            rows={2}
            aria-label="Note for the correction"
            placeholder={failedStep ? `What to fix, e.g. “${failedStep.command_line} failed”` : "What to fix"}
            value={note}
            onChange={(e) => setNote(e.target.value)}
          />
          <div className="form-actions start">
            <Button
              variant="primary"
              disabled={action.busy}
              onClick={() =>
                action.run(() =>
                  requestCorrection(
                    detail.task.id,
                    note.trim() ||
                      (failedStep
                        ? `Validation failed: \`${failedStep.command_line}\` ${failedStep.status}${failedStep.exit_code !== null ? ` (exit ${failedStep.exit_code})` : ""}. Fix the implementation so it passes.`
                        : "Validation failed. Fix the implementation so it passes."),
                  ),
                )
              }
            >
              Send back for correction
            </Button>
            <Button disabled={action.busy} onClick={() => action.run(() => resumeWorkflow(detail.task.id))}>
              Run validation again
            </Button>
          </div>
        </div>
      ) : null}

      {phase === "awaiting_approval" && d ? <Approval d={d} w={w} onDone={refresh} /> : null}

      {phase === "integrating" && d ? <Commits d={d} /> : null}

      {phase === "delivered" && i ? (
        <div className="delivery-step">
          <p className="small">
            <strong>{i.target_branch}</strong> is at merge commit <code className="mono">{i.merge_commit}</code>
            {i.approved_by ? ` · approved by ${i.approved_by}` : ""}. The task was completed. Nothing was pushed or deployed.
          </p>
          <Commits d={d} />
        </div>
      ) : null}

      {(phase === "blocked" || phase === "delivery_failed" || phase === "cancelled") && w.status === "waiting" ? (
        <div className="form-actions start">
          <Button variant="primary" disabled={action.busy} onClick={() => action.run(() => resumeWorkflow(detail.task.id))}>
            Check again
          </Button>
          {phase !== "cancelled" ? (
            <Button disabled={action.busy} onClick={() => action.run(() => requestCorrection(detail.task.id))}>
              Send back for correction
            </Button>
          ) : null}
        </div>
      ) : null}
      {i ? (
        <p className="muted small">
          Integration {shortOid(i.id)} · {i.source_branch} → {i.target_branch}
        </p>
      ) : null}
    </Panel>
  );
}
