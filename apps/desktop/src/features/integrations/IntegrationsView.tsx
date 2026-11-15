import { useEffect, useState } from "react";

import { Badge, Button, EmptyState, ErrorNotice, PageHeader, Panel } from "../../components/ui";
import { toCoreError, type CoreRequestError } from "../../lib/core";
import { formatRelative } from "../../lib/format";
import { useNow } from "../../lib/useNow";
import { navigate, useNavigation } from "../../stores/navigation";
import { useCoreQuery } from "../../stores/useCoreQuery";
import type { IntegrationSettingsInfo } from "../../types/protocol";
import { listProjects } from "../projects/api";
import { deleteCommand, integrationSettings, isIntegrationEvent, listIntegrations, saveCommand, setRequireReview } from "./api";
import { IntegrationDetailView } from "./IntegrationDetailView";
import { STATUS_LABEL, shortOid, statusTone } from "./labels";

function Settings({ projectId, settings, onChanged }: { projectId: string; settings: IntegrationSettingsInfo; onChanged: () => void }) {
  const [name, setName] = useState("");
  const [argv, setArgv] = useState("");
  const [timeout, setTimeoutSecs] = useState("600");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<CoreRequestError | null>(null);
  async function run(a: () => Promise<unknown>) {
    setBusy(true);
    setError(null);
    try {
      await a();
      onChanged();
    } catch (e) {
      setError(toCoreError(e));
    } finally {
      setBusy(false);
    }
  }
  const args = argv.split("\n").filter((a) => a !== "");
  return (
    <Panel title="Integration settings">
      <label className="check">
        <input
          type="checkbox"
          checked={settings.require_review}
          disabled={busy}
          onChange={(e) => void run(() => setRequireReview(projectId, e.target.checked))}
        />{" "}
        Require an approved review of exactly the integrated content
      </label>
      <h3 className="policy-section">Validation commands</h3>
      <p className="muted small">
        Each runs in the workspace for the exact source commit, directly (never through a shell), after the policy allows it or you
        authorize it. Shell rules in Policies decide which run without asking.
      </p>
      <ul className="mini-list">
        {settings.commands.map((c) => (
          <li key={c.id}>
            <span className="mini-title">
              {c.name} <code>{c.command_line}</code> {c.enabled ? null : <Badge>disabled</Badge>}
            </span>
            <span className="mini-sub">timeout {c.timeout_secs} s</span>
            <Button variant="ghost" disabled={busy} onClick={() => void run(() => deleteCommand(c.id))}>
              Remove
            </Button>
          </li>
        ))}
      </ul>
      <form
        className="command-form"
        aria-label="Add a validation command"
        onSubmit={(e) => {
          e.preventDefault();
          void run(async () => {
            await saveCommand({ project_id: projectId, name, argv: args, timeout_secs: Number(timeout) });
            setName("");
            setArgv("");
          });
        }}
      >
        <div className="field">
          <label htmlFor="cmd-name">Name</label>
          <input id="cmd-name" className="input" value={name} onChange={(e) => setName(e.target.value)} placeholder="Tests" />
        </div>
        <div className="field">
          <label htmlFor="cmd-timeout">Timeout (seconds)</label>
          <input id="cmd-timeout" className="input" inputMode="numeric" value={timeout} onChange={(e) => setTimeoutSecs(e.target.value)} />
        </div>
        <div className="field field-wide">
          <label htmlFor="cmd-argv">Program and arguments, one per line</label>
          <textarea
            id="cmd-argv"
            className="input textarea mono"
            rows={3}
            value={argv}
            onChange={(e) => setArgv(e.target.value)}
            placeholder={"cargo\ntest"}
          />
          <span className="muted small">Run directly, never through a shell: put each argument on its own line.</span>
        </div>
        {error ? (
          <div className="field-wide">
            <ErrorNotice title="Cannot save" message={error.message} code={error.code} />
          </div>
        ) : null}
        <div className="form-actions start field-wide">
          <Button type="submit" disabled={busy || name.trim() === "" || args.length === 0}>
            Add command
          </Button>
        </div>
      </form>
    </Panel>
  );
}

/**
 * Integrations: delivering a workspace's reviewed and validated commits into a
 * target branch, one exact pair of commits at a time, only on your approval.
 */
export function IntegrationsView() {
  const { integrationId, integrationProjectId } = useNavigation();
  const projects = useCoreQuery("projects", listProjects);
  const [includeClosed, setIncludeClosed] = useState(true);
  const [showSettings, setShowSettings] = useState(false);
  const now = useNow();
  const projectId = integrationProjectId ?? "";

  useEffect(() => {
    const first = projects.data?.[0];
    if (!projectId && first) navigate("integrations", { integrationProjectId: first.id, integrationId: null });
  }, [projects.data, projectId]);

  const enabled = projectId !== "";
  const list = useCoreQuery(
    `integrations:${projectId}:${includeClosed}`,
    () => listIntegrations({ project_id: projectId, include_closed: includeClosed, limit: 200 }),
    { enabled, refreshOn: isIntegrationEvent },
  );
  const settings = useCoreQuery(`integration-settings:${projectId}`, () => integrationSettings(projectId), { enabled });
  const project = projects.data?.find((p) => p.id === projectId);
  const items = list.data ?? [];

  if (projects.data && projects.data.length === 0) {
    return (
      <div className="page">
        <PageHeader title="Integrations" />
        <EmptyState icon="projects" title="No projects yet">
          Register a repository under Projects first.
        </EmptyState>
      </div>
    );
  }

  return (
    <div className="page">
      <PageHeader
        title="Integrations"
        subtitle="Deliver a workspace's committed work into a target branch. Switchyard checks it against Git, validates it, and integrates exactly the commits you approve. Nothing is pushed."
        actions={
          <Button icon="settings" onClick={() => setShowSettings(!showSettings)}>
            {showSettings ? "Hide integration settings" : "Integration settings"}
          </Button>
        }
      />
      <div className="policy-scope">
        <label htmlFor="int-project">Project</label>
        <select
          id="int-project"
          className="input select"
          value={projectId}
          onChange={(e) => navigate("integrations", { integrationProjectId: e.target.value, integrationId: null })}
        >
          {(projects.data ?? []).map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </select>
        <label className="check">
          <input type="checkbox" checked={includeClosed} onChange={(e) => setIncludeClosed(e.target.checked)} /> Show integrated and
          rejected
        </label>
      </div>
      {showSettings && settings.data ? <Settings projectId={projectId} settings={settings.data} onChanged={settings.refresh} /> : null}
      {list.error ? <ErrorNotice title="Cannot load integrations" message={list.error.message} code={list.error.code} /> : null}
      <div className="split">
        <div className="list" role="listbox" aria-label="Integrations">
          {items.length === 0 && list.data ? (
            <EmptyState icon="branch" title="No integrations">
              Create one from a task that was implemented and reviewed (Tasks → Delivery).
            </EmptyState>
          ) : null}
          {items.map((i) => (
            <button
              key={i.id}
              type="button"
              role="option"
              aria-selected={i.id === integrationId}
              className={`list-item${i.id === integrationId ? " active" : ""}`}
              onClick={() => navigate("integrations", { integrationId: i.id })}
            >
              <span className="list-item-title">
                {i.workspace_name ?? "workspace"} → {i.target_branch}
                <Badge tone={statusTone(i.status)}>{STATUS_LABEL[i.status]}</Badge>
              </span>
              <span className="list-item-sub">
                {shortOid(i.source_commit)} onto {shortOid(i.target_commit)} · {formatRelative(i.updated_at_ms, now)}
              </span>
            </button>
          ))}
        </div>
        {integrationId ? (
          <IntegrationDetailView integrationId={integrationId} project={project} onChanged={list.refresh} />
        ) : (
          <EmptyState icon="branch" title="Select an integration">
            Its exact commits, changes, validation and approval appear here.
          </EmptyState>
        )}
      </div>
    </div>
  );
}
