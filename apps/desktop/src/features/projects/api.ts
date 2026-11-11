import { coreRequest } from "../../lib/core";
import type { EventEnvelope } from "../../types/protocol";

export const listProjects = async () => (await coreRequest("projects.list", {})).projects;
export const addProject = async (path: string) => (await coreRequest("projects.add", { path })).project;
export const removeProject = (projectId: string) => coreRequest("projects.remove", { project_id: projectId });
export const gitStatus = (projectId: string) => coreRequest("projects.git_status", { project_id: projectId });

export const isProjectEvent = (e: EventEnvelope): boolean => e.event_type.startsWith("project.");
