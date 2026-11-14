import type { Section } from "../layouts/AppShell";
import { createStore, useStore } from "./store";

export interface Navigation {
  section: Section;
  /** Selected agent session in the Agents section. */
  sessionId: string | null;
  /** Selected project in the Projects section. */
  projectId: string | null;
  /** Selected task in the Tasks section. */
  taskId: string | null;
  /** Project shown in the Tasks section (kept with `taskId` across navigation). */
  taskProjectId: string | null;
  /** Selected integration and its project in the Integrations section. */
  integrationId: string | null;
  integrationProjectId: string | null;
}

export const navigationStore = createStore<Navigation>({ section: "dashboard", sessionId: null, projectId: null, taskId: null, taskProjectId: null, integrationId: null, integrationProjectId: null });

export const useNavigation = (): Navigation => useStore(navigationStore);

export function navigate(section: Section, extra: Partial<Omit<Navigation, "section">> = {}): void {
  navigationStore.set((n) => ({ ...n, ...extra, section }));
}
