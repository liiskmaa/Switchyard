import { EmptyState, PageHeader, Panel } from "../../components/ui";

export function ApprovalsView() {
  return (
    <div className="page">
      <PageHeader title="Approvals" subtitle="Requests from agents that need an operator decision." />
      <Panel>
        <EmptyState icon="approvals" title="No pending approvals">
          Approval requests (commands, file changes, permission escalations) will be routed here once agent sessions
          exist. Nothing can request approval yet.
        </EmptyState>
      </Panel>
    </div>
  );
}
