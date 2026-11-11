import type { ReactNode } from "react";

import { BrandMark, Icon, type IconName } from "../components/Icon";
import { StatusIndicator } from "../components/StatusIndicator";

export type Section = "dashboard" | "projects" | "agents" | "activity" | "approvals" | "settings";

const primary: { id: Section; label: string; icon: IconName }[] = [
  { id: "dashboard", label: "Dashboard", icon: "dashboard" },
  { id: "projects", label: "Projects", icon: "projects" },
  { id: "agents", label: "Agents", icon: "agents" },
  { id: "activity", label: "Activity", icon: "activity" },
  { id: "approvals", label: "Approvals", icon: "approvals" },
];

function NavItem({ id, label, icon, active, onSelect }: { id: Section; label: string; icon: IconName; active: boolean; onSelect: (s: Section) => void }) {
  return (
    <button type="button" className={`nav-item${active ? " active" : ""}`} aria-current={active ? "page" : undefined} onClick={() => onSelect(id)}>
      <Icon name={icon} />
      <span>{label}</span>
    </button>
  );
}

export function AppShell({ section, onSelect, children }: { section: Section; onSelect: (s: Section) => void; children: ReactNode }) {
  return (
    <div className="shell">
      <aside className="sidebar">
        <div className="brand">
          <BrandMark />
          <span>Switchyard</span>
        </div>
        <nav className="nav" aria-label="Main">
          {primary.map((item) => (
            <NavItem key={item.id} {...item} active={section === item.id} onSelect={onSelect} />
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
