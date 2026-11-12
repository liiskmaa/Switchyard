import { coreRequest } from "../../lib/core";
import type { EventEnvelope, WorkspaceStatus } from "../../types/protocol";

export const listWorkspaces = async (projectId: string, includeArchived: boolean) =>
  (await coreRequest("workspaces.list", { project_id: projectId, include_archived: includeArchived })).workspaces;
export const getWorkspace = (workspaceId: string) => coreRequest("workspaces.get", { workspace_id: workspaceId });
export const createWorkspace = async (projectId: string, name?: string, baseRef?: string) =>
  (
    await coreRequest("workspaces.create", {
      project_id: projectId,
      ...(name ? { name } : {}),
      ...(baseRef ? { base_ref: baseRef } : {}),
    })
  ).workspace;
export const workspaceGitStatus = (workspaceId: string) => coreRequest("workspaces.git_status", { workspace_id: workspaceId });
export const verifyWorkspace = (workspaceId: string) => coreRequest("workspaces.verify", { workspace_id: workspaceId });
export const archiveWorkspace = (workspaceId: string) => coreRequest("workspaces.archive", { workspace_id: workspaceId });
export const restoreWorkspace = (workspaceId: string) => coreRequest("workspaces.restore", { workspace_id: workspaceId });
export const completeWorkspace = (workspaceId: string) => coreRequest("workspaces.complete", { workspace_id: workspaceId });
export const checkCleanup = (workspaceId: string) => coreRequest("workspaces.cleanup_check", { workspace_id: workspaceId });
export const cleanupWorkspace = (workspaceId: string, confirmation: string) =>
  coreRequest("workspaces.cleanup", { workspace_id: workspaceId, confirmation });

/** Events that can change what a project's workspace list shows. */
export const isWorkspaceEvent = (e: EventEnvelope): boolean =>
  e.event_type.startsWith("workspace.") ||
  e.event_type === "agent.state_changed" ||
  e.event_type === "agent.exited" ||
  e.event_type === "files.changed";

export const WORKSPACE_STATUS_LABEL: Record<WorkspaceStatus, string> = {
  provisioning: "Provisioning",
  ready: "Ready",
  active: "Active",
  dirty: "Uncommitted changes",
  integrating: "Integrating",
  completed: "Done",
  failed: "Needs attention",
  cleanup_pending: "Cleaning up",
  archived: "Archived",
  removed: "Removed",
};

export function workspaceTone(status: WorkspaceStatus): "ok" | "warn" | "error" | "neutral" | "accent" {
  switch (status) {
    case "active":
      return "accent";
    case "ready":
    case "completed":
      return "ok";
    case "dirty":
    case "provisioning":
    case "cleanup_pending":
    case "integrating":
      return "warn";
    case "failed":
      return "error";
    case "archived":
    case "removed":
      return "neutral";
  }
}

/** The machine-readable part of a status reason (`code: detail`). */
export function reasonParts(reason: string | null): { code: string; detail: string } | null {
  if (!reason) return null;
  const i = reason.indexOf(":");
  return i > 0 ? { code: reason.slice(0, i), detail: reason.slice(i + 1).trim() } : { code: reason, detail: "" };
}
