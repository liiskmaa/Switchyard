import { coreRequest } from "../../lib/core";
import type { AgentProvider, EventEnvelope, RoleBindings, RoleBindingsPatch } from "../../types/protocol";

export const workflowSettings = (projectId: string) => coreRequest("workflows.settings", { project_id: projectId });
export const saveWorkflowSettings = (projectId: string, bindings: RoleBindings) =>
  coreRequest("workflows.set_settings", { project_id: projectId, bindings });
export const listModels = async (provider: AgentProvider, refresh = false) =>
  (await coreRequest("agents.models", { provider, refresh })).catalog;
export const startWorkflow = (taskId: string, overrides?: RoleBindingsPatch) =>
  coreRequest("workflows.start", overrides ? { task_id: taskId, overrides } : { task_id: taskId });
export const pauseWorkflow = (taskId: string) => coreRequest("workflows.pause", { task_id: taskId });
export const resumeWorkflow = (taskId: string) => coreRequest("workflows.resume", { task_id: taskId });
export const cancelWorkflow = (taskId: string) => coreRequest("workflows.cancel", { task_id: taskId });
export const pendingApprovals = async (sessionId: string) =>
  (await coreRequest("approvals.list", { status: "pending", session_id: sessionId, limit: 20 })).approvals;

export const isWorkflowEvent = (e: EventEnvelope): boolean => e.event_type.startsWith("workflow.");
