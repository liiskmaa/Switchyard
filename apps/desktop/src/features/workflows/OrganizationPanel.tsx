import { useState } from "react";

import { Badge, Button, ErrorNotice, Panel } from "../../components/ui";
import { providerName } from "../../lib/agents";
import { toCoreError, type CoreRequestError } from "../../lib/core";
import { navigate } from "../../stores/navigation";
import { useCoreQuery } from "../../stores/useCoreQuery";
import type {
  OrgAgentInfo,
  OrgContextEntry,
  OrganizationInfo,
  OrgTaskInfo,
  OrgTaskState,
  TaskDetail,
} from "../../types/protocol";
import { cancelWorkflow, getOrganization, isOrganizationEvent, pauseWorkflow, resumeWorkflow } from "./api";

type Tone = "ok" | "warn" | "error" | "accent" | "neutral";

const TASK_STATE: Record<OrgTaskState, [string, Tone]> = {
  planning: ["Being planned", "accent"],
  delegated: ["In progress", "accent"],
  waiting: ["Waiting for earlier work", "neutral"],
  ready: ["Up next", "neutral"],
  working: ["Working", "accent"],
  reported: ["Being checked", "accent"],
  accepted: ["Accepted", "ok"],
  cancelled: ["Cancelled", "neutral"],
};

const WORK_LABEL: Record<OrgTaskInfo["work"], string> = {
  coordination: "Coordination",
  implementation: "Implementation",
  investigation: "Investigation",
  documentation: "Documentation",
  testing: "Testing",
  review: "Review",
};

const STATUS_LABEL: Record<OrgContextEntry["status"], string> = {
  verified: "Verified",
  claim: "Claim",
  proposal: "Proposal",
  decided: "Decided",
  superseded: "Replaced",
};

const STATUS_TONE: Record<OrgContextEntry["status"], Tone> = {
  verified: "ok",
  claim: "neutral",
  proposal: "warn",
  decided: "accent",
  superseded: "neutral",
};

const model = (a: OrgAgentInfo) => `${providerName(a.provider)} · ${a.model ?? "CLI default model"}`;

function title(o: OrganizationInfo, key: string | null): string {
  if (!key) return "";
  return o.agents.find((a) => a.key === key)?.title ?? key;
}

function taskTitle(o: OrganizationInfo, key: string | null): string {
  if (!key) return "";
  return o.tasks.find((t) => t.key === key)?.title ?? key;
}

/** What the organization is doing right now, in one sentence. */
export function nowText(o: OrganizationInfo): string {
  if (o.status === "done") return "The team finished; delivery is yours.";
  if (o.status === "cancelled") return `Cancelled${o.reason ? `: ${o.reason}` : ""}`;
  if (o.status === "paused") return "Paused: the current turn finishes, nothing new starts.";
  if (o.status === "waiting") return o.reason ?? "Waiting for you.";
  const a = o.activity;
  if (a.kind === "supervising") {
    const what = a.items.includes("plan")
      ? "is planning the work"
      : a.items.some((i) => i.startsWith("observation"))
        ? "is deciding how to continue after an interruption"
        : a.items.some((i) => i.startsWith("review "))
          ? `is checking ${a.items.filter((i) => i.startsWith("review ")).map((i) => `“${taskTitle(o, i.slice(7))}”`).join(", ")}`
          : a.items.includes("scope finished")
            ? "is checking the finished scope"
            : "is reviewing";
    return `${title(o, a.agent)} ${what}.`;
  }
  if (a.kind === "working") return `${title(o, a.agent)} is working on “${taskTitle(o, a.task)}”.`;
  return "Starting the next step.";
}

function AgentNode({ o, agent, depth }: { o: OrganizationInfo; agent: OrgAgentInfo; depth: number }) {
  const reports = o.agents.filter((x) => x.reports_to === agent.key);
  const owned = o.tasks.filter((t) => t.owner === agent.key && t.work !== "coordination");
  const busy = o.activity.agent === agent.key && o.activity.kind !== "idle";
  return (
    <li className="org-node">
      <div className="org-agent" title={`Why this model: ${agent.selection}`}>
        <span className={`org-dot ${busy ? "busy" : agent.status}`} aria-hidden="true" />
        <span className="org-agent-main">
          <span className="org-agent-title">
            {agent.title}
            {agent.kind === "supervisor" ? <span className="muted small"> · supervisor</span> : null}
          </span>
          <span className="muted small">
            {model(agent)}
            {agent.scope.length ? ` · scope ${agent.scope.join(", ")}` : ""}
            {busy ? " · active now" : agent.status === "done" ? " · done" : ""}
          </span>
        </span>
      </div>
      {owned.length ? (
        <ul className="org-owned">
          {owned.map((t) => (
            <li key={t.key} className="small">
              {t.title} <span className="muted">· {TASK_STATE[t.state][0].toLowerCase()}</span>
              {t.corrections ? <span className="muted"> · {t.corrections} correction{t.corrections > 1 ? "s" : ""}</span> : null}
            </li>
          ))}
        </ul>
      ) : null}
      {reports.length ? (
        <ul className="org-tree" aria-label={`Reports to ${agent.title}`}>
          {reports.map((r) => (
            <AgentNode key={r.key} o={o} agent={r} depth={depth + 1} />
          ))}
        </ul>
      ) : null}
    </li>
  );
}

function TaskNode({ o, task, projectId }: { o: OrganizationInfo; task: OrgTaskInfo; projectId: string }) {
  const kids = o.tasks.filter((t) => t.parent === task.key);
  const [label, tone] = TASK_STATE[task.state];
  return (
    <li className="plan-node">
      <div className="plan-row">
        <span className="plan-main">
          {task.task_id && task.key !== "objective" ? (
            <button
              type="button"
              className="link plan-title"
              onClick={() => navigate("tasks", { taskProjectId: projectId, taskId: task.task_id })}
            >
              {task.title}
            </button>
          ) : (
            <span className="plan-title">{task.title}</span>
          )}
          <span className="muted small">
            {WORK_LABEL[task.work]} · {title(o, task.owner)}
            {task.depends_on.length ? ` · after ${task.depends_on.map((k) => taskTitle(o, k)).join(", ")}` : ""}
          </span>
        </span>
        <Badge tone={tone}>{label}</Badge>
      </div>
      {kids.length ? (
        <ul className="plan-tree">
          {kids.map((k) => (
            <TaskNode key={k.key} o={o} task={k} projectId={projectId} />
          ))}
        </ul>
      ) : null}
    </li>
  );
}

function ContextList({ entries, empty }: { entries: OrgContextEntry[]; empty: string }) {
  const [all, setAll] = useState(false);
  if (entries.length === 0) return <p className="muted small">{empty}</p>;
  const shown = all ? entries : entries.slice(0, 5);
  return (
    <>
      <ul className="ctx-list">
        {shown.map((e) => (
          <li key={e.seq} className={`ctx-item${e.status === "superseded" ? " superseded" : ""}`}>
            <Badge tone={STATUS_TONE[e.status]}>{STATUS_LABEL[e.status]}</Badge>
            <span className="ctx-text">{e.text}</span>
            <span className="muted small ctx-source">{e.source === "core" ? "Switchyard" : e.source === "user" ? "you" : e.source.replace("agent:", "")}</span>
          </li>
        ))}
      </ul>
      {entries.length > 5 ? (
        <button type="button" className="link small" onClick={() => setAll(!all)}>
          {all ? "Show fewer" : `Show all ${entries.length}`}
        </button>
      ) : null}
    </>
  );
}

/**
 * The agent organization working on a task (M8.3): what it does now, who is
 * in it and why, the work plan, and what was decided and found. Delivery
 * stays in its own panel; questions to you appear under "Needs you".
 */
export function OrganizationPanel({ detail, onChanged }: { detail: TaskDetail; onChanged: () => void }) {
  const taskId = detail.task.id;
  const org = useCoreQuery(`organization:${taskId}`, () => getOrganization(taskId), {
    refreshOn: isOrganizationEvent,
    intervalMs: 5_000,
  });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<CoreRequestError | null>(null);
  const o = org.data;
  if (!o) return org.error ? <ErrorNotice title="Cannot load the team" message={org.error.message} code={org.error.code} /> : null;
  const act = (f: () => Promise<unknown>) =>
    void (async () => {
      setBusy(true);
      setError(null);
      try {
        await f();
        org.refresh();
        onChanged();
      } catch (e) {
        setError(toCoreError(e));
      } finally {
        setBusy(false);
      }
    })();
  const lead = o.agents.find((a) => a.reports_to === null);
  const root = o.tasks.find((t) => t.parent === null);
  const decisions = o.context
    .filter((e) => ["decision", "instruction", "observation", "disagreement", "escalation", "deliverable"].includes(e.kind))
    .filter((e) => e.status !== "superseded")
    .reverse();
  const facts = o.context
    .filter((e) => ["finding", "risk", "progress"].includes(e.kind) && e.status !== "superseded")
    .reverse();
  const live = o.status === "running" || o.status === "paused" || o.status === "waiting";
  return (
    <Panel
      title="Team"
      actions={
        live ? (
          <span className="org-actions">
            {o.status === "running" ? (
              <Button variant="ghost" disabled={busy} onClick={() => act(() => pauseWorkflow(taskId))}>
                Pause
              </Button>
            ) : null}
            {o.status === "paused" ? (
              <Button disabled={busy} onClick={() => act(() => resumeWorkflow(taskId))}>
                Resume
              </Button>
            ) : null}
            <Button variant="ghost" disabled={busy} onClick={() => act(() => cancelWorkflow(taskId))}>
              Stop the team
            </Button>
          </span>
        ) : null
      }
    >
      <p className="org-now">{detail.workflow?.status === "delivered" ? "Delivered: the approved work is merged." : nowText(o)}</p>
      {o.final_summary ? <p className="small">{o.final_summary}</p> : null}
      {error ? <ErrorNotice title="Not done" message={error.message} code={error.code} /> : null}

      <div className="org-columns">
        <section aria-labelledby="org-people">
          <h3 id="org-people" className="policy-section">
            Who works on it
          </h3>
          {lead ? (
            <ul className="org-tree org-root">
              <AgentNode o={o} agent={lead} depth={0} />
            </ul>
          ) : null}
          <p className="muted small">Hover a name to see why Switchyard chose its model.</p>
        </section>
        <section aria-labelledby="org-plan">
          <h3 id="org-plan" className="policy-section">
            Work plan
          </h3>
          {root ? (
            <ul className="plan-tree plan-root">
              {o.tasks
                .filter((t) => t.parent === root.key)
                .map((t) => (
                  <TaskNode key={t.key} o={o} task={t} projectId={detail.task.project_id} />
                ))}
            </ul>
          ) : null}
          {o.tasks.length <= 1 ? <p className="muted small">The lead supervisor has not planned the work yet.</p> : null}
        </section>
      </div>

      <h3 className="policy-section">Decisions and supervision</h3>
      <ContextList entries={decisions} empty="No decisions yet." />
      <details className="small muted workflow-tech">
        <summary>Findings, risks and progress ({facts.length})</summary>
        <ContextList entries={facts} empty="Nothing recorded." />
      </details>
      <details className="small muted workflow-tech">
        <summary>Limits</summary>
        <p>
          {o.usage.agents} of {o.usage.max_agents} agents · {o.usage.tasks} of {o.usage.max_tasks} tasks ·{" "}
          {o.usage.supervision_turns} of {o.usage.max_supervision_turns} supervision turns
          {o.usage.cost_usd > 0 ? ` · $${o.usage.cost_usd.toFixed(2)} reported` : ""}
          {o.usage.tokens > 0 ? ` · ${o.usage.tokens.toLocaleString()} tokens reported` : ""}
        </p>
        <p>
          Agents work one after another in one workspace: a supervisor checks each result before the next step starts.
          Instructions reach an agent at its next turn.
        </p>
      </details>
    </Panel>
  );
}
