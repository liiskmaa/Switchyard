import { useEffect, useState } from "react";

import { Badge, Button, EmptyState, ErrorNotice, PageHeader, Panel } from "../../components/ui";
import { useCoreQuery } from "../../stores/useCoreQuery";
import { listProjects } from "../projects/api";
import { listWorkspaces } from "../workspaces/api";
import { effectivePolicy, isEvaluationEvent, isPolicyEvent, listEvaluations, listRules } from "./api";
import { AutoModePanel } from "./AutoMode";
import { EvaluationList } from "./EvaluationList";
import { CATEGORY_LABEL, DECISION_LABEL, DECISION_TONE } from "./labels";
import { RuleForm } from "./RuleForm";
import { RuleList } from "./RuleList";

/**
 * Approval policies of one project: what is always enforced, the built-in
 * defaults, project rules (inherited by every workspace), workspace rules,
 * and the recorded decisions. Core evaluates every request; this view only
 * shows and edits rules.
 */
export function PoliciesView() {
  const projects = useCoreQuery("projects", listProjects);
  const [projectId, setProjectId] = useState<string>("");
  const [workspaceId, setWorkspaceId] = useState<string>("");
  const [adding, setAdding] = useState(false);

  useEffect(() => {
    const first = projects.data?.[0];
    if (!projectId && first) setProjectId(first.id);
  }, [projects.data, projectId]);

  const enabled = projectId !== "";
  const workspaces = useCoreQuery(`policy-ws:${projectId}`, () => listWorkspaces(projectId, true), { enabled });
  const effective = useCoreQuery(`policy-eff:${projectId}:${workspaceId}`, () => effectivePolicy(projectId, workspaceId || undefined), {
    enabled,
    refreshOn: isPolicyEvent,
  });
  const allRules = useCoreQuery(`policy-rules:${projectId}`, () => listRules(projectId), { enabled, refreshOn: isPolicyEvent });
  const evaluations = useCoreQuery(
    `policy-evals:${projectId}:${workspaceId}`,
    () => listEvaluations(workspaceId ? { workspace_id: workspaceId, limit: 50 } : { project_id: projectId, limit: 50 }),
    { enabled, refreshOn: isEvaluationEvent, intervalMs: 30_000 },
  );

  const refresh = () => {
    effective.refresh();
    allRules.refresh();
    evaluations.refresh();
  };
  const ws = workspaces.data ?? [];
  const selectedWs = ws.find((w) => w.id === workspaceId);
  const eff = effective.data;
  const workspaceRules = (allRules.data ?? []).filter((r) => r.scope === "workspace");

  if (projects.data && projects.data.length === 0) {
    return (
      <div className="page">
        <PageHeader title="Policies" />
        <EmptyState icon="policy" title="No projects yet">
          Register a repository under Projects; its policies are managed here.
        </EmptyState>
      </div>
    );
  }

  return (
    <div className="page">
      <PageHeader
        title="Policies"
        subtitle="Choose which agent requests run without asking, which ask you every time, and which are blocked. Switchyard evaluates every request; your decision on one request never changes a rule."
      />
      <div className="notice notice-warn policy-limit" role="note">
        <div>
          <strong>Policies decide what Switchyard answers, not what a command can do.</strong>
          <p>
            An allowed or approved command runs with your user&apos;s permissions and can read or change files outside its workspace
            and reach the network. Workspaces separate source trees; they are not a sandbox.
          </p>
        </div>
      </div>
      <div className="policy-scope">
        <label htmlFor="policy-project">Project</label>
        <select
          id="policy-project"
          className="input select"
          value={projectId}
          onChange={(e) => {
            setProjectId(e.target.value);
            setWorkspaceId("");
            setAdding(false);
          }}
        >
          {(projects.data ?? []).map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </select>
        <label htmlFor="policy-workspace">View as</label>
        <select id="policy-workspace" className="input select" value={workspaceId} onChange={(e) => setWorkspaceId(e.target.value)}>
          <option value="">The whole project</option>
          {ws
            .filter((w) => w.kind === "worktree")
            .map((w) => (
              <option key={w.id} value={w.id}>
                Workspace “{w.name}”{w.status === "removed" ? " (removed)" : ""}
              </option>
            ))}
        </select>
      </div>
      {effective.error ? <ErrorNotice title="Cannot load policies" message={effective.error.message} code={effective.error.code} /> : null}

      {eff ? (
        <>
          <AutoModePanel projectId={projectId} />
          <Panel
            title={selectedWs ? `Rules for workspace “${selectedWs.name}”` : "Rules"}
            actions={
              adding ? null : (
                <Button icon="plus" onClick={() => setAdding(true)}>
                  Add rule
                </Button>
              )
            }
          >
            {adding ? (
              <RuleForm
                projectId={projectId}
                categories={eff.categories}
                workspaces={ws}
                defaultWorkspaceId={workspaceId || undefined}
                onDone={() => {
                  setAdding(false);
                  refresh();
                }}
                onCancel={() => setAdding(false)}
              />
            ) : null}
            <h3 className="policy-section">{selectedWs ? "Inherited from the project" : "Every workspace"}</h3>
            <RuleList
              rules={eff.project_rules}
              readOnly={selectedWs !== undefined}
              categories={eff.categories}
              workspaces={ws}
              empty="No project rules. The built-in defaults below apply."
              onChanged={refresh}
            />
            <h3 className="policy-section">{selectedWs ? "Only in this workspace" : "Single-workspace rules"}</h3>
            <RuleList
              rules={selectedWs ? eff.workspace_rules : workspaceRules}
              categories={eff.categories}
              workspaces={ws}
              empty={selectedWs ? "No rules only for this workspace." : "No rule is limited to one workspace."}
              onChanged={refresh}
            />
            <p className="muted small">
              When several rules match a request, the most restrictive one wins (block, then ask, then allow), wherever it is
              defined. A workspace rule cannot loosen a project rule.
            </p>
          </Panel>

          <div className="grid-2">
            <Panel title="Built-in defaults (when no rule matches)">
              <ul className="mini-list">
                {eff.categories
                  .filter((c) => c.configurable)
                  .map((c) => (
                    <li key={c.category} className="policy-default">
                      <span className="mini-title">{CATEGORY_LABEL[c.category]}</span>
                      <Badge tone={DECISION_TONE[c.default_decision]}>
                        {DECISION_LABEL[c.default_decision]}
                        {c.default_decision === "allow" ? " inside the workspace" : ""}
                      </Badge>
                    </li>
                  ))}
                <li className="policy-default">
                  <span className="mini-title">{CATEGORY_LABEL.unknown}</span>
                  <Badge tone="error">{DECISION_LABEL.deny}</Badge>
                </li>
              </ul>
            </Panel>
            <Panel title="Always enforced by Switchyard">
              <ul className="findings">
                {eff.system_restrictions.map((r) => (
                  <li key={r.code} className="small">
                    {r.message}
                  </li>
                ))}
              </ul>
              <p className="muted small">These cannot be changed by rules. Policy revision {eff.policy_revision}.</p>
            </Panel>
          </div>

          <Panel title="Recent decisions">
            {evaluations.error ? (
              <ErrorNotice title="Cannot load decisions" message={evaluations.error.message} code={evaluations.error.code} />
            ) : null}
            <EvaluationList
              evaluations={evaluations.data ?? []}
              empty="Decisions appear here as agents in this project request tools."
            />
          </Panel>
        </>
      ) : null}
    </div>
  );
}
