import { useState } from "react";

import { Button, ErrorNotice } from "../../components/ui";
import { toCoreError, type CoreRequestError } from "../../lib/core";
import { formatRelative } from "../../lib/format";
import { useNow } from "../../lib/useNow";
import { navigate } from "../../stores/navigation";
import { useCoreQuery } from "../../stores/useCoreQuery";
import type { StuckItem } from "../../types/protocol";
import { cancelWorkflow, isStuckEvent, listStuck } from "./api";

/** The watchdog's findings (ADR 0045), refreshed as they change. */
export function useStuck() {
  return useCoreQuery("watchdog", listStuck, { refreshOn: isStuckEvent, intervalMs: 30_000 });
}

/** Scrolls to the task page's supervisor box. */
function askSupervisor() {
  document.querySelector<HTMLTextAreaElement>('textarea[aria-label="Message to the supervisor"]')?.focus();
}

/**
 * Why this task is stuck, what Switchyard already did, and what helps: never
 * a Resume that would only bounce back.
 */
export function StuckCard({ item, onChanged }: { item: StuckItem; onChanged: () => void }) {
  const now = useNow(30_000);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<CoreRequestError | null>(null);
  const stop = () =>
    void (async () => {
      setBusy(true);
      setError(null);
      try {
        await cancelWorkflow(item.task_id);
        onChanged();
      } catch (e) {
        setError(toCoreError(e));
      } finally {
        setBusy(false);
      }
    })();
  return (
    <div className="needs-card stuck-card" role="status">
      <p>
        <strong>Stuck: {item.title}</strong> <span className="muted small">since {formatRelative(item.since_ms, now)}</span>
      </p>
      <p className="small">{item.detail}</p>
      {item.remedy_applied ? <p className="small stuck-done">Switchyard did: {item.remedy_applied}</p> : null}
      {error ? <ErrorNotice title="Not done" message={error.message} code={error.code} /> : null}
      <div className="form-actions start">
        {item.session_id ? (
          <Button onClick={() => navigate("agents", { sessionId: item.session_id ?? null })}>Open the agent's session</Button>
        ) : null}
        {item.code === "resume_bounces" || item.code === "silent_agent" ? (
          <Button onClick={askSupervisor}>Ask the supervisor</Button>
        ) : null}
        {item.code === "resume_bounces" ? (
          <Button variant="ghost" disabled={busy} onClick={stop}>
            Stop the {item.kind === "team" ? "team" : "workflow"}
          </Button>
        ) : null}
      </div>
    </div>
  );
}
