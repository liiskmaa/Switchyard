import { useEffect, useState } from "react";

import { Button, ErrorNotice } from "../../components/ui";
import { toCoreError, type CoreRequestError } from "../../lib/core";
import { useCoreQuery } from "../../stores/useCoreQuery";
import type { Project, RoleBindings } from "../../types/protocol";
import { saveWorkflowSettings, workflowSettings } from "./api";
import { ModelSourceNote, RoleBindingsEditor } from "./RoleBindingsEditor";

type SaveState = "saved" | "defaults" | "dirty" | "saving" | "error";

const SAVE_TEXT: Record<SaveState, string> = {
  saved: "Saved",
  defaults: "Switchyard's defaults — not saved for this project yet",
  dirty: "Unsaved changes",
  saving: "Saving…",
  error: "Not saved",
};

/** Which provider and model runs each workflow role in this project (tasks can override them). */
export function WorkflowRolesPanel({ project }: { project: Project }) {
  const settings = useCoreQuery(`workflow-settings:${project.id}`, () => workflowSettings(project.id), {
    refreshOn: (e) => e.event_type === "workflow.settings_changed",
  });
  const [draft, setDraft] = useState<RoleBindings | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<CoreRequestError | null>(null);
  const [resets, setResets] = useState(0);
  // A new saved revision replaces the draft (and resets the editor's custom-model state).
  const revision = settings.data?.revision;
  useEffect(() => setDraft(null), [revision]);
  const s = settings.data;
  if (settings.error) {
    return <ErrorNotice title="Cannot load workflow roles" message={settings.error.message} code={settings.error.code} />;
  }
  if (!s) return null;
  const value = draft ?? s.bindings;
  // Same rule as Core's warning, applied to the draft as it is edited.
  const sameProvider = value.implement.provider === value.review.provider;
  const state: SaveState = busy ? "saving" : error ? "error" : draft ? "dirty" : s.revision === 0 ? "defaults" : "saved";

  async function save() {
    setBusy(true);
    setError(null);
    try {
      await saveWorkflowSettings(project.id, value);
      settings.refresh();
    } catch (e) {
      setError(toCoreError(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="section" aria-labelledby="roles-heading">
      <div className="section-head">
        <div>
          <h2 id="roles-heading">Workflow roles</h2>
          <p className="muted small">
            Who runs each step of the engineering workflow for new tasks in {project.name}. Tasks can override these; a
            running workflow keeps the roles it started with.
          </p>
        </div>
      </div>
      <RoleBindingsEditor
        key={`${revision}-${resets}`}
        idPrefix="project-roles"
        value={value}
        onChange={(b) => {
          setDraft(b);
          setError(null);
        }}
      />
      {sameProvider ? (
        <p className="role-warning small">
          Implementation and review use the same provider, so the review is less independent. Consider a different
          provider for review.
        </p>
      ) : null}
      {error ? <ErrorNotice title="The roles were not saved" message={error.message} code={error.code} /> : null}
      <div className="save-bar">
        <span className={`save-state save-${state}`} role="status">
          {SAVE_TEXT[state]}
        </span>
        <div className="form-actions">
          <Button
            variant="ghost"
            disabled={busy || draft === null}
            onClick={() => {
              setDraft(null);
              setError(null);
              setResets((n) => n + 1);
            }}
          >
            Discard
          </Button>
          <Button variant="primary" disabled={busy || (draft === null && s.revision > 0)} onClick={() => void save()}>
            {busy ? "Saving…" : s.revision === 0 && draft === null ? "Save defaults" : "Save roles"}
          </Button>
        </div>
      </div>
      <ModelSourceNote />
    </section>
  );
}
