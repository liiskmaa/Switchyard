import { useState } from "react";

import { Badge, Button, ErrorNotice, KeyValue, Panel } from "../../components/ui";
import { toCoreError } from "../../lib/core";
import { useCoreQuery } from "../../stores/useCoreQuery";
import type { Capabilities } from "../../types/protocol";
import { listAgents } from "./api";

const CAPABILITY_LABELS: [keyof Capabilities, string][] = [
  ["multi_turn", "Multiple turns per session"],
  ["streaming_text", "Streaming output"],
  ["permission_requests", "Permission requests routed to Switchyard"],
  ["interrupt_turn", "Interrupt a running turn"],
  ["queue_message_during_turn", "Queue a message for the next turn"],
  ["steer_running_turn", "Change a running turn"],
  ["resume_session", "Resume a finished session"],
];

export function AvailabilityPanel() {
  const agents = useCoreQuery("agents", () => listAgents(false));
  const [refreshing, setRefreshing] = useState(false);
  const [refreshError, setRefreshError] = useState<string | null>(null);
  const claude = agents.data?.[0];

  async function refresh() {
    setRefreshing(true);
    setRefreshError(null);
    try {
      await listAgents(true);
      agents.refresh();
    } catch (e) {
      setRefreshError(toCoreError(e).message);
    } finally {
      setRefreshing(false);
    }
  }

  return (
    <Panel
      title="Claude Code"
      actions={
        <Button variant="ghost" icon="refresh" onClick={() => void refresh()} disabled={refreshing}>
          {refreshing ? "Checking…" : "Re-check"}
        </Button>
      }
    >
      {agents.error ? <ErrorNotice title="Cannot query agents" message={agents.error.message} code={agents.error.code} /> : null}
      {refreshError ? <ErrorNotice title="Detection failed" message={refreshError} /> : null}
      {!claude ? (
        <p className="muted">Checking installation…</p>
      ) : (
        <>
          <KeyValue
            rows={[
              [
                "Status",
                claude.error ? (
                  <Badge tone="error">{claude.installed ? "Unusable" : "Not installed"}</Badge>
                ) : (
                  <Badge tone="ok">Available</Badge>
                ),
              ],
              ["Version", claude.version ? `${claude.version} (verified with ${claude.tested_version})` : "—"],
              ["Executable", claude.executable ? <code className="path">{claude.executable}</code> : "—"],
              [
                "Authentication",
                claude.auth ? (
                  claude.auth.logged_in ? (
                    <Badge tone="ok">Logged in{claude.auth.method ? ` · ${claude.auth.method}` : ""}</Badge>
                  ) : (
                    <Badge tone="error">Not logged in — run `claude auth login`</Badge>
                  )
                ) : (
                  <span className="muted">unknown</span>
                ),
              ],
            ]}
          />
          {claude.error ? <ErrorNotice title="Claude Code cannot be used" message={claude.error} /> : null}
          <ul className="capabilities">
            {CAPABILITY_LABELS.map(([key, label]) => (
              <li key={key} className={claude.capabilities[key] ? "cap-yes" : "cap-no"}>
                <span aria-hidden="true">{claude.capabilities[key] ? "✓" : "✕"}</span> {label}
              </li>
            ))}
          </ul>
        </>
      )}
    </Panel>
  );
}
