import { EmptyState, ErrorNotice, PageHeader, Panel } from "../../components/ui";
import { coreRequest } from "../../lib/core";
import { useCoreQuery } from "../../stores/useCoreQuery";
import { isApprovalEvent } from "../agents/api";
import { listProjects } from "../projects/api";
import { ApprovalCard } from "./ApprovalCard";

const fetchAll = async () => (await coreRequest("approvals.list", { limit: 100 })).approvals;

export function ApprovalsView() {
  const approvals = useCoreQuery("approvals", fetchAll, { refreshOn: isApprovalEvent, intervalMs: 15_000 });
  const projects = useCoreQuery("projects", listProjects);
  const project = (id: string) => projects.data?.find((p) => p.id === id)?.name;
  const name = (a: { project_id: string; workspace_name?: string | null | undefined }) =>
    [project(a.project_id), a.workspace_name].filter(Boolean).join(" · ") || undefined;
  const all = approvals.data ?? [];
  const pending = all.filter((a) => a.status === "pending");
  const decided = all.filter((a) => a.status !== "pending").slice(0, 30);

  return (
    <div className="page">
      <PageHeader
        title="Approvals"
        subtitle="Tool permission requests from agents. Nothing runs until Switchyard records a decision; unanswered requests are denied when they expire."
      />
      {approvals.error ? <ErrorNotice title="Cannot load approvals" message={approvals.error.message} code={approvals.error.code} /> : null}
      <Panel title={`Pending (${pending.length})`}>
        {pending.length === 0 ? (
          <EmptyState icon="approvals" title="No pending approvals">
            Requests appear here when a running agent wants to use a tool that needs permission.
          </EmptyState>
        ) : (
          pending.map((a) => <ApprovalCard key={a.id} approval={a} projectName={name(a)} onDecided={approvals.refresh} />)
        )}
      </Panel>
      {decided.length > 0 ? (
        <Panel title="Recent decisions">
          {decided.map((a) => (
            <ApprovalCard key={a.id} approval={a} projectName={name(a)} />
          ))}
        </Panel>
      ) : null}
    </div>
  );
}
