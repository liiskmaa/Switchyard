import { useState } from "react";

import { Button, ErrorNotice } from "../../components/ui";
import { providerName } from "../../lib/agents";
import { toCoreError, type CoreRequestError } from "../../lib/core";
import { useCoreQuery } from "../../stores/useCoreQuery";
import type { WorkflowInfo } from "../../types/protocol";
import { sendNote } from "../tasks/api";
import { cancelWorkflow, isWorkflowEvent, previewNext, resumeWorkflow, setStepThrough } from "./api";
import { modelLabel, stepName } from "./labels";

const str = (v: unknown) => (typeof v === "string" ? v : null);

/**
 * Step-through (ADR 0043): the hand-off the workflow holds, with exactly what
 * the next role will receive. A note goes into that prompt; Continue starts it.
 */
export function NextStepPreview({ w, onChanged }: { w: WorkflowInfo; onChanged: () => void }) {
  const preview = useCoreQuery(`preview-next:${w.task_id}:${w.next_step ?? ""}`, () => previewNext(w.task_id), {
    refreshOn: (e) => isWorkflowEvent(e) || e.event_type === "task.message_recorded",
  });
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<CoreRequestError | null>(null);
  const act = (f: () => Promise<unknown>) =>
    void (async () => {
      setBusy(true);
      setError(null);
      try {
        await f();
        preview.refresh();
        onChanged();
      } catch (e) {
        setError(toCoreError(e));
      } finally {
        setBusy(false);
      }
    })();
  const p = preview.data;
  const ctx = p?.context ?? {};
  const inv = (ctx["investigation"] ?? null) as Record<string, unknown> | null;
  const files = Array.isArray(inv?.["files_read"]) ? (inv?.["files_read"] as unknown[]).map(String) : [];
  const findings = Array.isArray(ctx["review_findings"]) ? (ctx["review_findings"] as unknown[]).length : 0;
  const notes = Array.isArray(ctx["operator_notes"]) ? (ctx["operator_notes"] as unknown[]).map(String) : [];
  const tests = (ctx["test_results"] ?? null) as Record<string, unknown> | null;

  return (
    <div className="needs-card next-step">
      <p>
        <strong>Next: {w.next_step ? stepName(w.next_step) : "the next step"}</strong>
        {p ? <span className="muted small"> · {providerName(p.provider)} · {modelLabel(p.model ?? null)}</span> : null}
      </p>
      <p className="small muted">Step-through is on: nothing starts until you continue. This is what it will receive.</p>
      {preview.error ? <ErrorNotice title="Cannot build the preview" message={preview.error.message} code={preview.error.code} /> : null}
      {p ? (
        <ul className="mini-list small">
          {inv ? (
            <li>
              Investigation: {str(inv["summary"])}
              {files.length ? <span className="muted"> · starts from {files.length} file{files.length > 1 ? "s" : ""}: {files.slice(0, 6).join(", ")}{files.length > 6 ? "…" : ""}</span> : null}
            </li>
          ) : null}
          {findings ? <li>{findings} open finding{findings > 1 ? "s" : ""} to address</li> : null}
          {tests ? <li>Switchyard's latest test run: {str(tests["status"]) ?? "included"}</li> : null}
          {notes.length ? (
            <li>
              Your notes: {notes.map((n) => `“${n}”`).join(" ")}
            </li>
          ) : (
            <li className="muted">No notes from you yet.</li>
          )}
        </ul>
      ) : null}
      {p ? (
        <details className="small">
          <summary>The full prompt ({Math.round(p.prompt.length / 100) / 10} KB)</summary>
          <pre className="test-output prompt-preview">{p.prompt}</pre>
        </details>
      ) : null}
      <textarea
        className="input textarea"
        rows={2}
        aria-label="Note for the next step"
        placeholder="Add something for this step (it goes into the prompt above)"
        value={note}
        onChange={(e) => setNote(e.target.value)}
      />
      {error ? <ErrorNotice title="Not done" message={error.message} code={error.code} /> : null}
      <div className="form-actions start">
        <Button
          disabled={busy || note.trim() === ""}
          onClick={() =>
            act(async () => {
              await sendNote(w.task_id, note.trim());
              setNote("");
            })
          }
        >
          Add note
        </Button>
        <Button variant="primary" disabled={busy} onClick={() => act(() => resumeWorkflow(w.task_id))}>
          Continue
        </Button>
        <Button
          variant="ghost"
          disabled={busy}
          onClick={() =>
            act(async () => {
              await setStepThrough(w.task_id, false);
              await resumeWorkflow(w.task_id);
            })
          }
        >
          Continue without stopping
        </Button>
        <Button variant="ghost" disabled={busy} onClick={() => act(() => cancelWorkflow(w.task_id))}>
          Stop the workflow
        </Button>
      </div>
    </div>
  );
}
