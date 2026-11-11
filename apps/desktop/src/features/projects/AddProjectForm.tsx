import { open } from "@tauri-apps/plugin-dialog";
import { type FormEvent, useState } from "react";

import { Button, ErrorNotice } from "../../components/ui";
import { toCoreError, type CoreRequestError } from "../../lib/core";
import type { Project } from "../../types/protocol";
import { addProject } from "./api";

export function AddProjectForm({ onAdded, onCancel }: { onAdded: (p: Project) => void; onCancel: () => void }) {
  const [path, setPath] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<CoreRequestError | null>(null);

  async function submit(target: string) {
    if (!target.trim()) return;
    setBusy(true);
    setError(null);
    try {
      onAdded(await addProject(target.trim()));
    } catch (e) {
      setError(toCoreError(e));
    } finally {
      setBusy(false);
    }
  }

  async function browse() {
    try {
      const selected = await open({ directory: true, multiple: false, title: "Choose a Git repository" });
      if (typeof selected === "string") {
        setPath(selected);
        await submit(selected);
      }
    } catch (e) {
      setError(toCoreError(e));
    }
  }

  const onSubmit = (e: FormEvent) => {
    e.preventDefault();
    void submit(path);
  };

  return (
    <form className="add-project" onSubmit={onSubmit}>
      <label htmlFor="repo-path">Repository root</label>
      <div className="input-row">
        <input
          id="repo-path"
          className="input mono"
          placeholder="/path/to/repository"
          value={path}
          onChange={(e) => setPath(e.target.value)}
          spellCheck={false}
          autoComplete="off"
          autoFocus
        />
        <Button icon="folder" onClick={() => void browse()} disabled={busy}>
          Browse…
        </Button>
      </div>
      <p className="muted small">
        Must be the top-level directory of an existing, non-bare Git repository. Switchyard only records the location;
        nothing in the repository is modified.
      </p>
      {error ? <ErrorNotice title="Could not add repository" message={error.message} code={error.code} /> : null}
      <div className="form-actions">
        <Button variant="ghost" onClick={onCancel} disabled={busy}>
          Cancel
        </Button>
        <Button type="submit" variant="primary" disabled={busy || !path.trim()}>
          {busy ? "Adding…" : "Add repository"}
        </Button>
      </div>
    </form>
  );
}
