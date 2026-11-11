import { useState } from "react";

import { ConnectionNotice } from "./components/ConnectionNotice";
import { ActivityView } from "./features/activity/ActivityView";
import { AgentsView } from "./features/agents/AgentsView";
import { ApprovalsView } from "./features/approvals/ApprovalsView";
import { DashboardView } from "./features/dashboard/DashboardView";
import { ProjectsView } from "./features/projects/ProjectsView";
import { SettingsView } from "./features/settings/SettingsView";
import { AppShell, type Section } from "./layouts/AppShell";

export function App() {
  const [section, setSection] = useState<Section>("dashboard");
  return (
    <AppShell section={section} onSelect={setSection}>
      <ConnectionNotice />
      {section === "dashboard" ? <DashboardView onOpenProjects={() => setSection("projects")} /> : null}
      {section === "projects" ? <ProjectsView /> : null}
      {section === "agents" ? <AgentsView /> : null}
      {section === "activity" ? <ActivityView /> : null}
      {section === "approvals" ? <ApprovalsView /> : null}
      {section === "settings" ? <SettingsView /> : null}
    </AppShell>
  );
}
