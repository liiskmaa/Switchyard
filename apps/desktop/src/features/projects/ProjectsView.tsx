import { useEffect, useState } from "react";

import { Badge, Button, EmptyState, ErrorNotice, PageHeader, Panel } from "../../components/ui";
import { toCoreError, type CoreRequestError } from "../../lib/core";
import { formatTimestamp } from "../../lib/format";
import { useCoreQuery } from "../../stores/useCoreQuery";
import type { Project } from "../../types/protocol";
import { AddProjectForm } from "./AddProjectForm";
import { GitStatusPanel } from "./GitStatusPanel";
import { WorkspacesPanel } from "../workspaces/WorkspacesPanel";
import { OrgSettingsPanel } from "../workflows/OrgSettingsPanel";
import { WorkflowRolesPanel } from "../workflows/WorkflowRolesPanel";
import { isProjectEvent, listProjects, removeProject } from "./api";

export function ProjectsView() {
  const projects = useCoreQuery("projects", listProjects, { refreshOn: isProjectEvent });
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);

  const list = projects.data ?? [];
  const selected = list.find((p) => p.id === selectedId) ?? null;

  useEffect(() => {
    if (!selected && list.length > 0 && !adding) setSelectedId(list[0]?.id ?? null);
  }, [selected, list, adding]);

  return (
    <div className="page">
      <PageHeader
        title="Projects"
        subtitle="Local Git repositories registered with Switchyard Core."
        actions={
          <Button variant="primary" icon="plus" onClick={() => setAdding(true)}>
            Add repository
          </Button>
        }
      />
      {projects.error ? <ErrorNotice title="Cannot load projects" message={projects.error.message} code={projects.error.code} /> : null}
      {list.length > 1 ? (
        <nav className="project-tabs" role="tablist" aria-label="Projects">
          {list.map((p) => (
            <button
              key={p.id}
              type="button"
              role="tab"
              aria-selected={p.id === selectedId && !adding}
              title={p.root_path}
              className={`project-tab${p.id === selectedId && !adding ? " active" : ""}`}
              onClick={() => {
                setAdding(false);
                setSelectedId(p.id);
              }}
            >
              {p.name}
              {!p.accessible ? <Badge tone="error">missing</Badge> : null}
            </button>
          ))}
        </nav>
      ) : null}
      {adding ? (
        <Panel title="Add repository">
          <AddProjectForm
            onCancel={() => setAdding(false)}
            onAdded={(p) => {
              setAdding(false);
              setSelectedId(p.id);
              projects.refresh();
            }}
          />
        </Panel>
      ) : selected ? (
        <ProjectDetail
          key={selected.id}
          project={selected}
          onRemoved={() => {
            setSelectedId(null);
            projects.refresh();
          }}
        />
      ) : list.length === 0 && projects.data ? (
        <EmptyState icon="projects" title="No repositories registered">
          Add an existing local Git repository to get started.
        </EmptyState>
      ) : null}
    </div>
  );
}

function ProjectDetail({ project, onRemoved }: { project: Project; onRemoved: () => void }) {
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<CoreRequestError | null>(null);

  async function remove() {
    setBusy(true);
    setError(null);
    try {
      await removeProject(project.id);
      onRemoved();
    } catch (e) {
      setError(toCoreError(e));
      setBusy(false);
    }
  }

  return (
    <div className="project-detail">
      <header className="project-head">
        <div className="project-title">
          <h2>{project.name}</h2>
          <code className="path project-path">{project.root_path}</code>
        </div>
        <div className="project-meta small">
          {project.accessible ? <Badge tone="ok">Accessible</Badge> : <Badge tone="error">Path not accessible</Badge>}
          <span>Registered {formatTimestamp(project.created_at_ms)}</span>
          <span className="mono" title={`Project ID ${project.id}`}>
            ID {project.id.slice(0, 8)}…
          </span>
        </div>
      </header>
      <WorkflowRolesPanel project={project} />
      <OrgSettingsPanel project={project} />
      <WorkspacesPanel project={project} />
      <GitStatusPanel project={project} />
      <Panel title="Registration">
        <p className="muted small">
          Removing the registration deletes Switchyard&apos;s record of this project. The repository and its files stay
          on disk untouched. A project with workspaces must have them archived and cleaned up first, so no worktree is
          left behind unrecorded.
        </p>
        {error ? <ErrorNotice title="Could not remove registration" message={error.message} code={error.code} /> : null}
        <div className="form-actions start">
          {confirming ? (
            <>
              <Button variant="danger" onClick={() => void remove()} disabled={busy}>
                {busy ? "Removing…" : "Confirm removal"}
              </Button>
              <Button variant="ghost" onClick={() => setConfirming(false)} disabled={busy}>
                Cancel
              </Button>
            </>
          ) : (
            <Button variant="danger" onClick={() => setConfirming(true)}>
              Remove registration
            </Button>
          )}
        </div>
      </Panel>
    </div>
  );
}
