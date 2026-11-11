import { EmptyState, PageHeader, Panel } from "../../components/ui";

export function AgentsView() {
  return (
    <div className="page">
      <PageHeader title="Agents" subtitle="Native coding agents supervised by Switchyard Core." />
      <Panel>
        <EmptyState icon="agents" title="Agent integration is not available yet">
          <p>
            Switchyard will run Claude Code and OpenAI Codex as native processes managed by Core, each in its own
            workspace and under central permissions. The adapter contract is documented in{" "}
            <code>docs/protocols/agent-adapter-contract.md</code>.
          </p>
          <p className="muted">No agents are configured and no sessions are running.</p>
        </EmptyState>
      </Panel>
    </div>
  );
}
