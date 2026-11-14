import { useEffect, useState } from "react";

import { Button, ErrorNotice, Panel } from "../../components/ui";
import { toCoreError, type CoreRequestError } from "../../lib/core";
import { useCoreQuery } from "../../stores/useCoreQuery";
import type { Project, RoleBindings } from "../../types/protocol";
import { saveWorkflowSettings, workflowSettings } from "./api";
import { RoleBindingsEditor } from "./RoleBindingsEditor";

/** Which provider and model runs each workflow role in this project (tasks can override them). */
export function WorkflowRolesPanel({ project }: { project: Project }) {
  const settings = useCoreQuery(`workflow-settings:${project.id}`, () => workflowSettings(project.id), {
    refreshOn: (e) => e.event_type === "workflow.settings_changed",
  });
  const [draft, setDraft] = useState<RoleBindings | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<CoreRequestError | null>(null);
  const [saved, setSaved] = useState(false);
  useEffect(() => setDraft(null), [settings.data?.revision]);
  const s = settings.data;
  if (settings.error) return <ErrorNotice title="Cannot load workflow roles" message={settings.error.message} code={settings.error.code} />;
  if (!s) return null;
  const value = draft ?? s.bindings;
  return (
    <Panel title="Workflow roles">
      <p className="muted small">
        The engineering workflow uses these for every new task in {project.name}
        {s.revision === 0 ? " (Switchyard's defaults until you save)" : ""}. A running workflow keeps the roles it started with.
      </p>
      <RoleBindingsEditor
        idPrefix="project-roles"
        value={value}
        onChange={(b) => {
          setDraft(b);
          setSaved(false);
        }}
      />
      {s.warnings.map((w) => (
        <p key={w} className="muted small">
          ⚠ {w}
        </p>
      ))}
      {error ? <ErrorNotice title="Cannot save the roles" message={error.message} code={error.code} /> : null}
      <div className="form-actions start">
        <Button
          variant="primary"
          disabled={busy || draft === null}
          onClick={() =>
            void (async () => {
              setBusy(true);
              setError(null);
              try {
                await saveWorkflowSettings(project.id, value);
                setSaved(true);
                settings.refresh();
              } catch (e) {
                setError(toCoreError(e));
              } finally {
                setBusy(false);
              }
            })()
          }
        >
          Save roles
        </Button>
        {saved ? <span className="muted small">Saved.</span> : null}
      </div>
    </Panel>
  );
}
