import { useState } from "react";

import { useCoreQuery } from "../../stores/useCoreQuery";
import { navigate, useNavigation } from "../../stores/navigation";
import { isApprovalEvent, pendingApprovals } from "../agents/api";
import { listProjects } from "../projects/api";
import { ApprovalCard } from "./ApprovalCard";

/** Requests shown in the dock before "show all". */
const SHOWN = 3;

/**
 * Pending approvals wherever you are: a panel in the corner of every page
 * (except Approvals itself) while any agent waits for a decision. Decisions
 * go through the same card as the Approvals page.
 */
export function ApprovalDock() {
  const { section } = useNavigation();
  const pending = useCoreQuery("dock-pending-approvals", () => pendingApprovals(), {
    refreshOn: isApprovalEvent,
    intervalMs: 5_000,
  });
  const projects = useCoreQuery("projects", listProjects);
  const [collapsed, setCollapsed] = useState(false);
  const list = pending.data ?? [];
  if (list.length === 0 || section === "approvals") return null;
  const project = (id: string) => projects.data?.find((p) => p.id === id)?.name;
  const name = (a: { project_id: string; workspace_name?: string | null | undefined }) =>
    [project(a.project_id), a.workspace_name].filter(Boolean).join(" · ") || undefined;

  return (
    <aside className={`approval-dock${collapsed ? " collapsed" : ""}`} aria-label="Approvals waiting for you">
      <button type="button" className="approval-dock-head" aria-expanded={!collapsed} onClick={() => setCollapsed(!collapsed)}>
        <span className="approval-dock-dot" aria-hidden="true" />
        <strong>
          {list.length} approval{list.length > 1 ? "s" : ""} waiting
        </strong>
        <span className="muted small">{collapsed ? "Show" : "Hide"}</span>
      </button>
      {collapsed ? null : (
        <div className="approval-dock-body">
          {list.slice(0, SHOWN).map((a) => (
            <ApprovalCard key={a.id} approval={a} projectName={name(a)} onDecided={pending.refresh} />
          ))}
          {list.length > SHOWN ? (
            <button type="button" className="link small" onClick={() => navigate("approvals")}>
              Show all {list.length}
            </button>
          ) : null}
        </div>
      )}
    </aside>
  );
}
