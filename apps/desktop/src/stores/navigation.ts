import type { Section } from "../layouts/AppShell";
import { createStore, useStore } from "./store";

export interface Navigation {
  section: Section;
  /** Selected agent session in the Agents section. */
  sessionId: string | null;
  /** Selected project in the Projects section. */
  projectId: string | null;
}

export const navigationStore = createStore<Navigation>({ section: "dashboard", sessionId: null, projectId: null });

export const useNavigation = (): Navigation => useStore(navigationStore);

export function navigate(section: Section, extra: Partial<Omit<Navigation, "section">> = {}): void {
  navigationStore.set((n) => ({ ...n, ...extra, section }));
}
