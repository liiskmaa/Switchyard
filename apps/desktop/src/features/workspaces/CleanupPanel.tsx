import { useState } from "react";

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${v.toFixed(1)} ${units[i] ?? "TB"}`;
}

import { Button, ErrorNotice } from "../../components/ui";
import { toCoreError, type CoreRequestError } from "../../lib/core";
import { useCoreQuery } from "../../stores/useCoreQuery";
import type { CleanupReport, Workspace } from "../../types/protocol";
import { checkCleanup, cleanupWorkspace } from "./api";
import { WarnNotice } from "./WorkspacesPanel";

/**
 * Explicit cleanup of an archived workspace. Core inspects the worktree and
 * returns blockers (cleanup impossible) and warnings (what will be deleted).
 * The operator confirms the exact report they reviewed; Core refuses if the
 * workspace changed since.
 */
export function CleanupPanel({
  workspace,
  onInspected,
  onClose,
  onDone,
}: {
  workspace: Workspace;
  /** Called when the workspace is re-inspected, so other views refresh too. */
  onInspected: () => void;
  onClose: () => void;
  onDone: () => void;
}) {
  const check = useCoreQuery(`cleanup:${workspace.id}`, async () => (await checkCleanup(workspace.id)).report);
  const [acknowledged, setAcknowledged] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<CoreRequestError | null>(null);
  const [refused, setRefused] = useState<CleanupReport | null>(null);
  const report = refused ?? check.data;

  async function confirm() {
    if (!report) return;
    setBusy(true);
    setError(null);
    try {
      await cleanupWorkspace(workspace.id, report.confirmation);
      onDone();
    } catch (e) {
      const err = toCoreError(e);
      setError(err);
      const data = err.data as { report?: CleanupReport } | undefined;
      if (data?.report) setRefused(data.report);
      setAcknowledged(false);
      onInspected();
    } finally {
      setBusy(false);
    }
  }

  function recheck() {
    setRefused(null);
    setError(null);
    setAcknowledged(false);
    check.refresh();
    onInspected();
  }

  return (
    <div className="ws-cleanup">
      <h4>Clean up “{workspace.name}”</h4>
      <p className="small">
        Cleanup deletes the worktree directory <code className="path">{workspace.path}</code> and Git&apos;s record of it.
        The branch <code>{workspace.branch}</code> and every commit on it are kept. Uncommitted or untracked work always
        blocks cleanup; Switchyard never discards it.
      </p>
      {check.error ? <ErrorNotice title="Cannot inspect the workspace" message={check.error.message} code={check.error.code} /> : null}
      {!report ? <p className="muted small">Inspecting the worktree…</p> : null}
      {report && report.blockers.length > 0 ? (
        <div className="notice notice-error" role="alert">
          <div>
            <strong>Cleanup is not possible; the workspace is preserved</strong>
            <ul className="findings">
              {report.blockers.map((b) => (
                <li key={b.code}>
                  {b.message} <code className="code-tag">{b.code}</code>
                </li>
              ))}
            </ul>
          </div>
        </div>
      ) : null}
      {report && report.warnings.length > 0 ? (
        <WarnNotice title="Review before confirming">
          <span className="findings">
            {report.warnings.map((w) => (
              <span key={w.code} className="finding">
                {w.message}
              </span>
            ))}
          </span>
          {report.ignored_files.length > 0 ? (
            <span className="finding mono small">
              {report.ignored_files.join("  ")}
              {report.ignored_total > report.ignored_files.length
                ? `  … and ${report.ignored_total - report.ignored_files.length} more`
                : ""}
            </span>
          ) : null}
        </WarnNotice>
      ) : null}
      {error ? <ErrorNotice title="Cleanup refused" message={error.message} code={error.code} /> : null}
      {report?.allowed && report.entries_total !== null ? (
        <p className="small">
          Removal deletes {report.entries_total} file system entr{report.entries_total === 1 ? "y" : "ies"}
          {report.bytes_total !== null ? ` (${formatBytes(report.bytes_total)})` : ""}, exactly as scanned now. If anything
          in the directory changes before removal, Switchyard refuses and asks for a new review.
        </p>
      ) : null}
      {report?.allowed ? (
        <label className="check small">
          <input type="checkbox" checked={acknowledged} onChange={(e) => setAcknowledged(e.target.checked)} disabled={busy} />{" "}
          I reviewed this report. Delete {report.will_remove_directory ? "the worktree directory" : "the stale worktree record"}{" "}
          and keep branch <code>{report.branch}</code>.
        </label>
      ) : null}
      <div className="form-actions start">
        {report?.allowed ? (
          <Button variant="danger" onClick={() => void confirm()} disabled={!acknowledged || busy}>
            {busy ? "Cleaning up…" : report.will_remove_directory ? "Delete worktree directory" : "Remove stale record"}
          </Button>
        ) : null}
        <Button variant="ghost" icon="refresh" onClick={recheck} disabled={busy}>
          Inspect again
        </Button>
        <Button variant="ghost" onClick={onClose} disabled={busy}>
          Cancel
        </Button>
      </div>
    </div>
  );
}
