import { ConnectionNotice } from "./components/ConnectionNotice";
import { ActivityView } from "./features/activity/ActivityView";
import { AgentsView } from "./features/agents/AgentsView";
import { ApprovalsView } from "./features/approvals/ApprovalsView";
import { DashboardView } from "./features/dashboard/DashboardView";
import { ProjectsView } from "./features/projects/ProjectsView";
import { SettingsView } from "./features/settings/SettingsView";
import { AppShell } from "./layouts/AppShell";
import { navigate, useNavigation } from "./stores/navigation";

export function App() {
  const { section } = useNavigation();
  return (
    <AppShell section={section} onSelect={(s) => navigate(s)}>
      <ConnectionNotice />
      {section === "dashboard" ? <DashboardView onOpenProjects={() => navigate("projects")} /> : null}
      {section === "projects" ? <ProjectsView /> : null}
      {section === "agents" ? <AgentsView /> : null}
      {section === "activity" ? <ActivityView /> : null}
      {section === "approvals" ? <ApprovalsView /> : null}
      {section === "settings" ? <SettingsView /> : null}
    </AppShell>
  );
}
