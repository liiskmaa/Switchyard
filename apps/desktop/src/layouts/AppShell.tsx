import type { ReactNode } from "react";

import { BrandMark, Icon, type IconName } from "../components/Icon";
import { StatusIndicator } from "../components/StatusIndicator";
import { pendingApprovals } from "../features/agents/api";
import { useCoreQuery } from "../stores/useCoreQuery";

export type Section = "dashboard" | "projects" | "tasks" | "integrations" | "agents" | "activity" | "approvals" | "policies" | "settings";

const primary: { id: Section; label: string; icon: IconName }[] = [
  { id: "dashboard", label: "Home", icon: "dashboard" },
  { id: "projects", label: "Projects", icon: "projects" },
  { id: "tasks", label: "Tasks", icon: "tasks" },
  { id: "integrations", label: "Integrations", icon: "branch" },
  { id: "agents", label: "Agents", icon: "agents" },
  { id: "activity", label: "Activity", icon: "activity" },
  { id: "approvals", label: "Approvals", icon: "approvals" },
  { id: "policies", label: "Policies", icon: "policy" },
];

function NavItem({
  id,
  label,
  icon,
  active,
  onSelect,
  count,
}: {
  id: Section;
  label: string;
  icon: IconName;
  active: boolean;
  onSelect: (s: Section) => void;
  count?: number | undefined;
}) {
  return (
    <button type="button" className={`nav-item${active ? " active" : ""}`} aria-current={active ? "page" : undefined} onClick={() => onSelect(id)}>
      <Icon name={icon} />
      <span>{label}</span>
      {count ? <span className="nav-count" aria-label={`${count} pending`}>{count}</span> : null}
    </button>
  );
}

export function AppShell({ section, onSelect, children }: { section: Section; onSelect: (s: Section) => void; children: ReactNode }) {
  const pending = useCoreQuery("nav-pending-approvals", () => pendingApprovals(), {
    refreshOn: (e) => e.event_type.startsWith("agent.permission_"),
    intervalMs: 30_000,
  });
  return (
    <div className="shell">
      <aside className="sidebar">
        <div className="brand">
          <BrandMark />
          <span>Switchyard</span>
        </div>
        <nav className="nav" aria-label="Main">
          {primary.map((item) => (
            <NavItem
              key={item.id}
              {...item}
              active={section === item.id}
              onSelect={onSelect}
              count={item.id === "approvals" ? pending.data?.length : undefined}
            />
          ))}
        </nav>
        <div className="sidebar-foot">
          <NavItem id="settings" label="Settings" icon="settings" active={section === "settings"} onSelect={onSelect} />
          <StatusIndicator />
        </div>
      </aside>
      <main className="main">{children}</main>
    </div>
  );
}
