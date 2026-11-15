import { useState } from "react";

import { Badge, Button, EmptyState, ErrorNotice, Panel } from "../../components/ui";
import { providerName } from "../../lib/agents";
import { toCoreError, type CoreRequestError } from "../../lib/core";
import { MESSAGE_LABEL, describeEvent, formatClock, formatRelative, shortId } from "../../lib/format";
import { useNow } from "../../lib/useNow";
import { navigate } from "../../stores/navigation";
import { useCoreQuery } from "../../stores/useCoreQuery";
import type { AgentProvider, Task, TaskDetail, TaskMessage, TaskRole, Workspace } from "../../types/protocol";
import { listAgents } from "../agents/api";
import {
  addDependency,
  assignTask,
  cancelAssignment,
  decideProposal,
  decideTask,
  getTask,
  isTaskEvent,
  publishTask,
  removeDependency,
  sendNote,
  taskEvents,
} from "./api";
import { createIntegration, isIntegrationEvent, listIntegrations } from "../integrations/api";
import { STATUS_LABEL as INTEGRATION_LABEL, shortOid, statusTone as integrationTone } from "../integrations/labels";
import { AGENT_KINDS, ROLE_LABEL, SEVERITY_TONE, STATUS_LABEL, statusTone, taskActions } from "./labels";
import { isWorkflowEvent } from "../workflows/api";
import { modelLabel, workflowStageText } from "../workflows/labels";
import { WorkflowPanel } from "../workflows/WorkflowPanel";
import { TaskNeedsYou, deliveryNeedsYou } from "../workflows/TaskNeedsYou";
import { DeliveryPanel } from "../workflows/DeliveryPanel";

/** History events shown before "Show all". */
const HISTORY_PREVIEW = 20;

function useAction(onDone: () => void) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<CoreRequestError | null>(null);
  async function run(action: () => Promise<unknown>) {
    setBusy(true);
    setError(null);
    try {
      await action();
      onDone();
    } catch (err) {
      setError(toCoreError(err));
    } finally {
      setBusy(false);
    }
  }
  return { busy, error, run };
}

function messageText(m: TaskMessage): string {
  const p = m.payload;
  const report = p["report"] as Record<string, unknown> | undefined;
  if (report && typeof report["summary"] === "string") {
    const q = typeof report["question"] === "string" ? `\nQuestion: ${report["question"]}` : "";
    return `${report["summary"]}${q}`;
  }
  for (const key of ["text", "error", "reason"]) {
    if (typeof p[key] === "string") return p[key] as string;
  }
  if (typeof p["action"] === "string") return `${p["action"]}${typeof p["note"] === "string" ? `: ${p["note"]}` : ""}`;
  if (typeof p["role"] === "string") return `${p["role"]} → ${providerName(String(p["provider"] ?? ""))}`;
  return "";
}

/** Delivery is separate from completion: a task can be completed and not integrated, or the reverse. */
function Delivery({ task }: { task: Task }) {
  const integrations = useCoreQuery(
    `task-integrations:${task.id}`,
    () => listIntegrations({ task_id: task.id, include_closed: true, limit: 50 }),
    { refreshOn: isIntegrationEvent },
  );
  const [target, setTarget] = useState("");
  const [error, setError] = useState<string | null>(null);
  const eligible = task.status === "awaiting_review" || task.status === "completed";
  const list = integrations.data ?? [];
  const open = list.find((i) => i.status !== "integrated" && i.status !== "cancelled");
  return (
    <Panel title="Delivery">
      {list.length === 0 ? <p className="muted small">Not integrated into any branch.</p> : null}
      <ul className="mini-list">
        {list.map((i) => (
          <li key={i.id}>
            <button
              type="button"
              className="link"
              onClick={() => navigate("integrations", { integrationId: i.id, integrationProjectId: i.project_id })}
            >
              {i.target_branch} <Badge tone={integrationTone(i.status)}>{INTEGRATION_LABEL[i.status]}</Badge>{" "}
              <span className="muted small">
                {i.status === "integrated" ? `merge ${shortOid(i.merge_commit)}` : `${shortOid(i.source_commit)} onto ${shortOid(i.target_commit)}`}
              </span>
            </button>
          </li>
        ))}
      </ul>
      {eligible && !open ? (
        <div className="input-row">
          <input
            className="input"
            aria-label="Target branch"
            placeholder="Target branch (default: the project checkout's branch)"
            value={target}
            onChange={(e) => setTarget(e.target.value)}
          />
          <Button
            onClick={() =>
              void (async () => {
                setError(null);
                try {
                  const r = await createIntegration(target.trim() ? { task_id: task.id, target_branch: target.trim() } : { task_id: task.id });
                  navigate("integrations", { integrationId: r.integration.id, integrationProjectId: r.integration.project_id });
                } catch (e) {
                  setError(toCoreError(e).message);
                }
              })()
            }
          >
            Prepare integration
          </Button>
        </div>
      ) : null}
      {!eligible && list.length === 0 ? (
        <p className="muted small">Integration becomes possible once implemented work was reported (awaiting review) or completed.</p>
      ) : null}
      {error ? <ErrorNotice title="Cannot prepare the integration" message={error} /> : null}
    </Panel>
  );
}

function Assign({ task, roles, workspaces, onDone }: { task: Task; roles: TaskRole[]; workspaces: Workspace[]; onDone: () => void }) {
  const agents = useCoreQuery("agents", () => listAgents(false));
  // Only the task's own project: Core refuses any other workspace anyway.
  const usable = workspaces.filter(
    (w) => w.project_id === task.project_id && w.kind === "worktree" && (w.status === "ready" || w.status === "dirty"),
  );
  const lastWs = task.open_assignment?.workspace_id;
  const [role, setRole] = useState<TaskRole>(roles[0] ?? "implement");
  const [provider, setProvider] = useState<AgentProvider>("claude_code");
  const [workspaceId, setWorkspaceId] = useState<string>(lastWs ?? "");
  const { busy, error, run } = useAction(onDone);
  const ws = workspaceId || usable[0]?.id || "";
  const available = (agents.data ?? []).filter((a) => a.installed && !a.error);
  return (
    <div className="ws-start">
      <div className="rule-form-grid">
        <div>
          <label htmlFor="assign-role">Role</label>
          <select id="assign-role" className="input select" value={role} onChange={(e) => setRole(e.target.value as TaskRole)}>
            {roles.map((r) => (
              <option key={r} value={r}>
                {ROLE_LABEL[r]}
              </option>
            ))}
          </select>
        </div>
        <div>
          <label htmlFor="assign-agent">Agent</label>
          <select
            id="assign-agent"
            className="input select"
            value={provider}
            onChange={(e) => setProvider(e.target.value as AgentProvider)}
          >
            {(agents.data ?? []).map((a) => (
              <option key={a.provider} value={a.provider} disabled={!a.installed || a.error !== null}>
                {a.display_name}
                {!a.installed || a.error ? " (unavailable)" : ""}
              </option>
            ))}
          </select>
        </div>
        <div>
          <label htmlFor="assign-ws">Workspace</label>
          <select id="assign-ws" className="input select" value={ws} onChange={(e) => setWorkspaceId(e.target.value)}>
            {usable.map((w) => (
              <option key={w.id} value={w.id}>
                {w.name}
              </option>
            ))}
          </select>
        </div>
      </div>
      {role === "review" ? (
        <p className="muted small">
          The reviewer works in the same workspace in a read-only session: Switchyard blocks every file change and asks you before
          each command. It scans the workspace before and after the review; if anything changed, the verdict is recorded as blocked.
          Approved commands still run with your user&apos;s permissions.
        </p>
      ) : null}
      {usable.length === 0 ? <p className="muted small">Create a workspace for this project under Projects first.</p> : null}
      {error ? <ErrorNotice title="Cannot assign" message={error.message} code={error.code} /> : null}
      <div className="form-actions start">
        <Button
          variant="primary"
          icon="agents"
          disabled={busy || ws === "" || available.length === 0}
          onClick={() => void run(() => assignTask(task.id, role, provider, ws))}
        >
          Assign and start
        </Button>
      </div>
    </div>
  );
}

export function TaskDetailView({
  taskId,
  tasks,
  workspaces,
  onChanged,
}: {
  taskId: string;
  tasks: Task[];
  workspaces: Workspace[];
  onChanged: () => void;
}) {
  const now = useNow();
  const concerns = (e: Parameters<typeof isTaskEvent>[0]) => (isTaskEvent(e) || isWorkflowEvent(e)) && e.task_id === taskId;
  const detail = useCoreQuery(`task:${taskId}`, () => getTask(taskId), { refreshOn: concerns });
  const events = useCoreQuery(`task-events:${taskId}`, () => taskEvents(taskId), { refreshOn: concerns });
  const refresh = () => {
    detail.refresh();
    events.refresh();
    onChanged();
  };
  const action = useAction(refresh);
  const deps = useAction(refresh);
  const [note, setNote] = useState("");
  const [decisionNote, setDecisionNote] = useState("");
  const [newDep, setNewDep] = useState("");
  const [tab, setTab] = useState<"messages" | "findings" | "assignments" | "dependencies" | "history">("messages");
  const [fullDescription, setFullDescription] = useState(false);
  const [systemMessages, setSystemMessages] = useState(false);
  const [allHistory, setAllHistory] = useState(false);

  if (detail.error) return <ErrorNotice title="Cannot load the task" message={detail.error.message} code={detail.error.code} />;
  const d: TaskDetail | null = detail.data;
  if (!d) return null;
  const t = d.task;
  const can = taskActions(t);
  const wsName = (id: string) => workspaces.find((w) => w.id === id)?.name ?? shortId(id);
  const open = t.open_assignment;
  const pendingProposals = d.proposals.filter((p) => p.status === "pending");
  const openFindings = d.findings.filter((f) => f.status === "open");
  // The conversation: what agents reported or asked, and what the operator said.
  // Assignment bookkeeping and Core's status notes are audit detail.
  const isSystem = (m: TaskMessage) => m.kind === "task_assignment" || (m.sender === "core" && m.kind === "status_update");
  const conversation = d.messages.filter((m) => systemMessages || !isSystem(m));
  const hiddenMessages = d.messages.filter(isSystem).length;
  const history = [...(events.data ?? [])].reverse();
  const workflowActive = d.workflow !== null && ["pending", "running", "waiting", "paused"].includes(d.workflow.status);
  const shownHistory = allHistory ? history : history.slice(0, HISTORY_PREVIEW);

  return (
    <div className="detail">
      <header className="task-header">
        <div className="task-header-main">
          <h2 className="task-title">{t.title}</h2>
          <div className="task-meta small">
            <Badge tone={workflowActive ? "accent" : statusTone(t.status)}>
              {d.workflow && workflowActive ? workflowStageText(d.workflow) : STATUS_LABEL[t.status]}
            </Badge>
            <span>Updated {formatRelative(t.updated_at_ms, now)}</span>
            {d.dependencies.length > 0 ? <span>Dependencies {d.readiness}</span> : null}
            {t.origin === "proposal" ? <span>From an approved proposal</span> : null}
          </div>
        </div>
        <div className="task-header-actions">
          {can.publish ? (
            <Button variant="primary" disabled={action.busy} onClick={() => void action.run(() => publishTask(t.id))}>
              Publish
            </Button>
          ) : null}
          {can.complete && !workflowActive ? (
            <Button variant="primary" disabled={action.busy} onClick={() => void action.run(() => decideTask("complete", t.id, decisionNote || undefined))}>
              Accept and complete
            </Button>
          ) : null}
          {can.retry || can.fail || can.cancel ? (
            <details className="menu">
              <summary className="btn btn-ghost">More</summary>
              <div className="menu-pop">
                <input
                  className="input"
                  aria-label="Note for your decision"
                  placeholder="Optional note"
                  value={decisionNote}
                  onChange={(e) => setDecisionNote(e.target.value)}
                />
                {can.retry ? (
                  <Button disabled={action.busy} onClick={() => void action.run(() => decideTask("retry", t.id, decisionNote || undefined))}>
                    Retry the task
                  </Button>
                ) : null}
                {can.cancel ? (
                  <Button disabled={action.busy} onClick={() => void action.run(() => decideTask("cancel", t.id, decisionNote || undefined))}>
                    Cancel the task
                  </Button>
                ) : null}
                {can.fail ? (
                  <Button variant="danger" disabled={action.busy} onClick={() => void action.run(() => decideTask("fail", t.id, decisionNote || undefined))}>
                    Mark as failed
                  </Button>
                ) : null}
              </div>
            </details>
          ) : null}
        </div>
      </header>
      {action.error ? <ErrorNotice title="Action failed" message={action.error.message} code={action.error.code} /> : null}
      {!d.workflow && (t.status_reason || t.failure) ? (
        <p className="muted small task-status-line">
          {t.status_reason}
          {t.failure ? ` · ${t.failure}` : ""}
        </p>
      ) : null}

      <TaskNeedsYou detail={d} onChanged={refresh} />
      <WorkflowPanel detail={d} onChanged={refresh} />
      {deliveryNeedsYou(d) ? null : <DeliveryPanel detail={d} onChanged={refresh} />}

      <details className="panel task-request" open={!d.workflow}>
        <summary className="task-request-summary">What was asked</summary>
        <div className="panel-body">
          {t.description ? (
            <div className="task-description-block">
              <p className="task-description">
                {!fullDescription && t.description.length > 320 ? `${t.description.slice(0, 320)}…` : t.description}
              </p>
              {t.description.length > 320 ? (
                <button type="button" className="link small" onClick={() => setFullDescription(!fullDescription)}>
                  {fullDescription ? "Show less" : "Show the full description"}
                </button>
              ) : null}
            </div>
          ) : (
            <p className="muted small">No description.</p>
          )}
          {t.acceptance_criteria.length > 0 ? (
            <>
              <h3 className="policy-section">Done when</h3>
              <ul className="findings">
                {t.acceptance_criteria.map((c) => (
                  <li key={c} className="small">
                    {c}
                  </li>
                ))}
              </ul>
            </>
          ) : null}
          <p className="muted small">
            {t.kind} · {t.priority} priority
            {d.workflow ? ` · workflow ${d.workflow.definition}@${d.workflow.version}` : ""}
          </p>
          {t.result ? (
            <p className="small">
              <strong>Result:</strong> {String(t.result["note"] ?? t.result["summary"] ?? "")}
            </p>
          ) : null}
        </div>
      </details>



      {pendingProposals.length > 0 || d.proposals.some((p) => !p.decided_by?.startsWith("organization:")) ? (
        <Panel title="Proposed subtasks">
          <p className="muted small">An agent proposed these. Nothing is created until you approve; approved subtasks start as drafts.</p>
          <ul className="mini-list">
            {d.proposals.filter((p) => !p.decided_by?.startsWith("organization:")).map((p) => (
              <li key={p.id}>
                <span className="mini-title">
                  {p.position + 1}. {p.proposal.title}{" "}
                  <Badge tone={p.status === "approved" ? "ok" : p.status === "rejected" ? "neutral" : "warn"}>{p.status}</Badge>
                </span>
                {p.proposal.description ? <span className="mini-sub">{p.proposal.description}</span> : null}
                {p.status === "pending" ? (
                  <div className="form-actions start">
                    <Button disabled={action.busy} onClick={() => void action.run(() => decideProposal(p.id, true))}>
                      Approve as draft
                    </Button>
                    <Button variant="ghost" disabled={action.busy} onClick={() => void action.run(() => decideProposal(p.id, false))}>
                      Reject
                    </Button>
                  </div>
                ) : null}
              </li>
            ))}
          </ul>
        </Panel>
      ) : null}





      {d.workflow && d.workflow.status !== "completed" ? null : <Delivery task={t} />}




      <section className="panel task-tabs-panel">
        <nav className="project-tabs task-tabs" role="tablist" aria-label="Task details">
          {(
            [
              ["messages", `Messages${conversation.length ? ` (${conversation.length})` : ""}`],
              ["findings", `Findings${openFindings.length ? ` (${openFindings.length} open)` : ""}`],
              ["assignments", `Assignments${d.assignments.length ? ` (${d.assignments.length})` : ""}`],
              ["dependencies", "Dependencies"],
              ["history", "History"],
            ] as const
          ).map(([key, label]) => (
            <button
              key={key}
              type="button"
              role="tab"
              aria-selected={tab === key}
              className={`project-tab${tab === key ? " active" : ""}`}
              onClick={() => setTab(key)}
            >
              {label}
            </button>
          ))}
        </nav>
        <div className="panel-body">
          {tab === "messages" ? (
            <>
                <p className="muted small">Messages from agents are claims or requests. They never approve, start or complete anything.</p>
                {hiddenMessages > 0 ? (
                  <label className="check small">
                    <input type="checkbox" checked={systemMessages} onChange={(e) => setSystemMessages(e.target.checked)} /> Show
                    system messages ({hiddenMessages})
                  </label>
                ) : null}
                {conversation.length === 0 ? (
                  <EmptyState icon="activity" title="No messages yet" />
                ) : (
                  <ol className="timeline">
                    {conversation.map((m) => (
                      <li key={m.id} className="tl">
                        <span className="tl-time">{formatClock(m.created_at_ms)}</span>
                        <Badge tone={m.kind === "malformed_report" || m.kind === "blocker" ? "warn" : AGENT_KINDS.has(m.kind) ? "accent" : "neutral"}>
                          {MESSAGE_LABEL[m.kind] ?? m.kind}
                        </Badge>
                        <span className="tl-body">
                          <span className="muted small">
                            {m.sender}
                            {m.delivery === "pending" ? " · waits for the next assignment" : m.delivery === "delivered" ? " · delivered" : ""}
                          </span>
                          <div className="tl-message">{messageText(m)}</div>
                        </span>
                      </li>
                    ))}
                  </ol>
                )}
                {!["completed", "cancelled"].includes(t.status) ? (
                  <form
                    className="composer"
                    onSubmit={(e) => {
                      e.preventDefault();
                      void action.run(async () => {
                        await sendNote(t.id, note);
                        setNote("");
                      });
                    }}
                  >
                    <textarea
                      className="input textarea"
                      aria-label="Note to the agent"
                      rows={2}
                      value={note}
                      onChange={(e) => setNote(e.target.value)}
                      placeholder={open?.status === "active" ? "Answer or guide the working agent" : "A note for the next assignment"}
                    />
                    <div className="composer-actions">
                      <Button type="submit" disabled={action.busy || note.trim() === ""}>
                        Send note
                      </Button>
                    </div>
                  </form>
                ) : null}
            </>
          ) : null}
          {tab === "findings" ? (
            <>
                {d.findings.length === 0 ? (
                  <p className="muted small">No findings.</p>
                ) : (
                  <ul className="findings">
                    {d.findings.map((f) => (
                      <li key={f.id} className={`finding small${f.status !== "open" ? " muted" : ""}`}>
                        <Badge tone={SEVERITY_TONE[f.severity]}>{f.severity}</Badge> <strong>{f.title}</strong>
                        {f.path ? (
                          <code className="code-tag">
                            {f.path}
                            {f.line ? `:${f.line}` : ""}
                          </code>
                        ) : null}
                        {f.status !== "open" ? ` (${f.status})` : ""}
                        {f.explanation ? <div>{f.explanation}</div> : null}
                        {f.suggestion ? <div className="muted">Suggestion: {f.suggestion}</div> : null}
                      </li>
                    ))}
                  </ul>
                )}
            </>
          ) : null}
          {tab === "assignments" ? (
            <>
                {open && d.workflow && open.origin === "workflow" ? (
                  <p className="muted small">The workflow manages this task&apos;s assignments.</p>
                ) : open ? (
                  <div className="ws-row">
                    <span>
                      {ROLE_LABEL[open.role]} by {providerName(open.provider)} in workspace “{wsName(open.workspace_id)}” · {open.status}
                    </span>
                    <div className="form-actions start">
                      {open.session_id ? (
                        <Button icon="agents" onClick={() => navigate("agents", { sessionId: open.session_id })}>
                          Open session
                        </Button>
                      ) : null}
                      <Button variant="danger" disabled={action.busy} onClick={() => void action.run(() => cancelAssignment(open.id))}>
                        Cancel assignment
                      </Button>
                    </div>
                  </div>
                ) : can.assign.length > 0 && d.workflow ? (
                  <details className="advanced">
                    <summary>Advanced: assign manually (take over from the workflow)</summary>
                    <p className="muted small">Pause or cancel the workflow first; a running workflow refuses manual assignments.</p>
                    <Assign task={t} roles={can.assign} workspaces={workspaces} onDone={refresh} />
                  </details>
                ) : can.assign.length > 0 ? (
                  <Assign task={t} roles={can.assign} workspaces={workspaces} onDone={refresh} />
                ) : (
                  <p className="muted small">No agent is working on this task.</p>
                )}
                {d.assignments.length > 0 ? (
                  <ul className="mini-list">
                    {[...d.assignments].reverse().map((a) => (
                      <li key={a.id}>
                        <span className="mini-title">
                          {a.step_key ? <code className="code-tag">{a.step_key}</code> : null} {ROLE_LABEL[a.role]} · {providerName(a.provider)} ·{" "}
                          {modelLabel(a.model)}
                          {a.actual_model && a.actual_model !== a.model ? ` (runs ${a.actual_model})` : ""} · {a.status}
                          {a.origin === "operator" ? " · manual" : ""}
                        </span>
                        <span className="mini-sub">
                          {wsName(a.workspace_id)}
                          {a.context_version ? ` · context v${a.context_version}` : ""}
                          {a.outcome ? ` · ${a.outcome}` : ""} · {formatRelative(a.created_at_ms, now)}
                        </span>
                      </li>
                    ))}
                  </ul>
                ) : null}
            </>
          ) : null}
          {tab === "dependencies" ? (
            <>
                {d.dependencies.length === 0 ? <p className="muted small">This task does not wait for other tasks.</p> : null}
                <ul className="mini-list">
                  {d.dependencies.map((dep) => (
                    <li key={dep.id}>
                      <span className="mini-title">
                        {dep.title} <Badge tone={statusTone(dep.status)}>{STATUS_LABEL[dep.status]}</Badge>
                      </span>
                      {can.dependencies ? (
                        <Button variant="ghost" disabled={action.busy} onClick={() => void deps.run(() => removeDependency(t.id, dep.id))}>
                          Remove
                        </Button>
                      ) : null}
                    </li>
                  ))}
                </ul>
                {can.dependencies ? (
                  <div className="input-row">
                    <select className="input select" aria-label="Add a dependency" value={newDep} onChange={(e) => setNewDep(e.target.value)}>
                      <option value="">Wait for another task…</option>
                      {tasks
                        .filter((o) => o.id !== t.id && !t.dependency_ids.includes(o.id))
                        .map((o) => (
                          <option key={o.id} value={o.id}>
                            {o.title}
                          </option>
                        ))}
                    </select>
                    <Button
                      disabled={deps.busy || newDep === ""}
                      onClick={() =>
                        void deps.run(async () => {
                          await addDependency(t.id, newDep);
                          setNewDep("");
                        })
                      }
                    >
                      Add
                    </Button>
                  </div>
                ) : null}
                {deps.error ? <ErrorNotice title="Cannot change dependencies" message={deps.error.message} code={deps.error.code} /> : null}
                {d.dependents.length > 0 ? (
                  <p className="muted small">Waiting for this task: {d.dependents.map((x) => x.title).join(", ")}</p>
                ) : null}
            </>
          ) : null}
          {tab === "history" ? (
            <>
                <p className="muted small">Newest first: every change to this task is journaled (the audit trail).</p>
                <ol className="timeline">
                  {shownHistory.map((e) => (
                    <li key={e.seq} className="tl">
                      <span className="tl-time">{formatClock(e.occurred_at_ms)}</span>
                      <span className="muted small">{e.event_type.replace("task.", "")}</span>
                      <span className="tl-body">{describeEvent(e)}</span>
                    </li>
                  ))}
                </ol>
                {history.length > HISTORY_PREVIEW ? (
                  <button type="button" className="link small" onClick={() => setAllHistory(!allHistory)}>
                    {allHistory ? "Show the latest only" : `Show all ${history.length} events`}
                  </button>
                ) : null}
            </>
          ) : null}
        </div>
      </section>
    </div>
  );
}
