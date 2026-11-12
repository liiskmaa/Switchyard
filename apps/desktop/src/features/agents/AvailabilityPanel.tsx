import { useState } from "react";

import { Badge, Button, ErrorNotice, KeyValue, Panel } from "../../components/ui";
import { CAPABILITY_LABELS, SUPPORT_LABEL, SUPPORT_MARK, loginCommand } from "../../lib/agents";
import { toCoreError } from "../../lib/core";
import { useCoreQuery } from "../../stores/useCoreQuery";
import type { AgentAvailability } from "../../types/protocol";
import { listAgents } from "./api";

/** Installation, sign-in and verified capabilities of every agent Switchyard can run. */
export function AvailabilityPanel() {
  const agents = useCoreQuery("agents", () => listAgents(false));
  const [refreshing, setRefreshing] = useState(false);
  const [refreshError, setRefreshError] = useState<string | null>(null);

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
      title="Agents"
      actions={
        <Button variant="ghost" icon="refresh" onClick={() => void refresh()} disabled={refreshing}>
          {refreshing ? "Checking…" : "Re-check"}
        </Button>
      }
    >
      {agents.error ? <ErrorNotice title="Cannot query agents" message={agents.error.message} code={agents.error.code} /> : null}
      {refreshError ? <ErrorNotice title="Detection failed" message={refreshError} /> : null}
      {!agents.data ? (
        <p className="muted">Checking installations…</p>
      ) : (
        <div className="agent-grid">
          {agents.data.map((a) => (
            <AgentCard key={a.provider} agent={a} />
          ))}
        </div>
      )}
    </Panel>
  );
}

function AgentCard({ agent }: { agent: AgentAvailability }) {
  const login = loginCommand(agent.provider);
  const untested = agent.version !== null && agent.version !== agent.tested_version;
  return (
    <section className="agent-card" aria-label={agent.display_name}>
      <h3 className="agent-card-title">{agent.display_name}</h3>
      <KeyValue
        rows={[
          [
            "Status",
            agent.error ? (
              <Badge tone="error">{agent.installed ? "Unusable" : "Not installed"}</Badge>
            ) : (
              <Badge tone="ok">Available</Badge>
            ),
          ],
          [
            "Version",
            agent.version ? (
              <span>
                {agent.version} {untested ? <Badge tone="warn">untested; verified with {agent.tested_version}</Badge> : <span className="muted">(verified)</span>}
              </span>
            ) : (
              "—"
            ),
          ],
          ["Executable", agent.executable ? <code className="path">{agent.executable}</code> : "—"],
          [
            "Authentication",
            agent.auth ? (
              agent.auth.logged_in ? (
                <Badge tone="ok">Logged in{agent.auth.method ? ` · ${agent.auth.method}` : ""}</Badge>
              ) : (
                <Badge tone="error">Not logged in{login ? ` — run \`${login}\`` : ""}</Badge>
              )
            ) : (
              <span className="muted">unknown</span>
            ),
          ],
        ]}
      />
      {agent.error ? <ErrorNotice title={`${agent.display_name} cannot be used`} message={agent.error} /> : null}
      <ul className="capabilities" aria-label={`${agent.display_name} capabilities`}>
        {CAPABILITY_LABELS.map(([key, label]) => {
          const s = agent.capabilities[key];
          return (
            <li key={key} className={`cap-${s}`} title={SUPPORT_LABEL[s]}>
              <span aria-hidden="true">{SUPPORT_MARK[s]}</span> {label}
              <span className="sr-only"> — {SUPPORT_LABEL[s]}</span>
            </li>
          );
        })}
      </ul>
    </section>
  );
}
