import { useState } from "react";

import { Badge, Button, ErrorNotice, Panel } from "../../components/ui";
import { toCoreError, type CoreRequestError } from "../../lib/core";
import { formatTimestamp } from "../../lib/format";
import { useCoreQuery } from "../../stores/useCoreQuery";
import { isPolicySettingsEvent, policySettings, setAutoMode } from "./api";

/** What auto mode does, in one place so every surface says the same thing (ADR 0037). */
export const AUTO_MODE_SUMMARY =
  "File changes and shell commands inside the workspace run without asking. Files outside the workspace, changes to .git, background commands and review sessions still ask, and your Ask and Block rules still win.";

const useSettings = (projectId: string) =>
  useCoreQuery(`policy-settings:${projectId}`, () => policySettings(projectId), { refreshOn: isPolicySettingsEvent });

/**
 * The project's auto mode switch. Turning it on is confirmed in place and
 * says exactly what changes; turning it off takes effect at once.
 */
export function AutoModePanel({ projectId }: { projectId: string }) {
  const settings = useSettings(projectId);
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<CoreRequestError | null>(null);
  const s = settings.data;
  if (settings.error) {
    return <ErrorNotice title="Cannot load auto mode" message={settings.error.message} code={settings.error.code} />;
  }
  if (!s) return null;

  async function change(enabled: boolean) {
    if (!s) return;
    setBusy(true);
    setError(null);
    try {
      await setAutoMode(projectId, enabled, s.revision);
      setConfirming(false);
      settings.refresh();
    } catch (e) {
      setError(toCoreError(e));
      settings.refresh();
    } finally {
      setBusy(false);
    }
  }

  return (
    <Panel
      title="Auto mode"
      className={s.auto_mode ? "auto-mode-on" : ""}
      actions={s.auto_mode ? <Badge tone="warn">On</Badge> : <Badge>Off</Badge>}
    >
      <p className="small">
        {s.auto_mode ? "Agents work without asking you. " : "Off: every file change and shell command asks you. "}
        <span className="muted">{AUTO_MODE_SUMMARY}</span>
      </p>
      {s.auto_mode && s.updated_at_ms ? (
        <p className="muted small">
          Turned on {formatTimestamp(s.updated_at_ms)}
          {s.updated_by ? ` by ${s.updated_by}` : ""}.
        </p>
      ) : null}
      {confirming ? (
        <div className="notice notice-warn" role="status">
          <div>
            <strong>Agents will run any command without asking</strong>
            <p>
              A command runs with your user&apos;s permissions and can change or delete files anywhere on this Mac and reach the
              network, whatever its workspace. Turn this on only for projects where that is acceptable. Requests already waiting
              still need your answer.
            </p>
          </div>
        </div>
      ) : null}
      {error ? <ErrorNotice title="Auto mode was not changed" message={error.message} code={error.code} /> : null}
      <div className="form-actions start">
        {s.auto_mode ? (
          <Button onClick={() => void change(false)} disabled={busy}>
            {busy ? "Turning off…" : "Turn off auto mode"}
          </Button>
        ) : confirming ? (
          <>
            <Button variant="danger" onClick={() => void change(true)} disabled={busy}>
              {busy ? "Turning on…" : "Turn on auto mode"}
            </Button>
            <Button variant="ghost" onClick={() => setConfirming(false)} disabled={busy}>
              Cancel
            </Button>
          </>
        ) : (
          <Button onClick={() => setConfirming(true)}>Turn on auto mode…</Button>
        )}
      </div>
    </Panel>
  );
}

/** A small marker wherever agents of the project work, shown only while auto mode is on. */
export function AutoModeBadge({ projectId }: { projectId: string }) {
  const settings = useSettings(projectId);
  if (!settings.data?.auto_mode) return null;
  return (
    <span title={AUTO_MODE_SUMMARY}>
      <Badge tone="warn">Auto mode</Badge>
    </span>
  );
}
