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
export const setStepThrough = (taskId: string, on: boolean) => coreRequest("workflows.set_step_through", { task_id: taskId, on });
export const previewNext = (taskId: string) => coreRequest("workflows.preview_next", { task_id: taskId });
export const cancelWorkflow = (taskId: string) => coreRequest("workflows.cancel", { task_id: taskId });
export const requestCorrection = (taskId: string, note?: string) =>
  coreRequest("workflows.request_correction", note ? { task_id: taskId, note } : { task_id: taskId });
export const pendingApprovals = async (sessionId: string) =>
  (await coreRequest("approvals.list", { status: "pending", session_id: sessionId, limit: 20 })).approvals;

export const isWorkflowEvent = (e: EventEnvelope): boolean => e.event_type.startsWith("workflow.");

// --- the task supervisor (ADR 0041) -----------------------------------------------
export const askSupervisor = (taskId: string, text: string) => coreRequest("supervisor.ask", { task_id: taskId, text });
export const getSupervisor = (taskId: string) => coreRequest("supervisor.get", { task_id: taskId });
export const organizationEvents = async (taskId: string) =>
  (await coreRequest("events.list", { task_id: taskId, after_seq: 0, limit: 1000, event_types: ORG_EVENTS })).events;
const ORG_EVENTS = [
  "organization.task_delegated",
  "organization.supervision_started",
  "organization.work_started",
  "organization.assessed",
  "organization.escalated",
  "organization.status_changed",
];
export const listStuck = async () => (await coreRequest("watchdog.list", {})).items;
export const isStuckEvent = (e: EventEnvelope): boolean => e.event_type === "task.stuck_changed";
export const isSupervisorEvent = (e: EventEnvelope): boolean => e.event_type.startsWith("supervisor.");

// --- agent organizations (M8.3) ---------------------------------------------------
export const startOrganization = (taskId: string, lead?: { provider: AgentProvider; model: string | null }) =>
  coreRequest(
    "workflows.start",
    lead ? { task_id: taskId, definition: "organization", lead } : { task_id: taskId, definition: "organization" },
  );
export const getOrganization = async (taskId: string) =>
  (await coreRequest("organizations.get", { task_id: taskId })).organization;
export const answerOrganization = (taskId: string, text: string) =>
  coreRequest("organizations.answer", { task_id: taskId, text });
export const orgSettings = (projectId: string) => coreRequest("organizations.settings", { project_id: projectId });
export const saveOrgSettings = (
  projectId: string,
  patch: { limits?: Partial<import("../../types/protocol").OrgLimits>; estimates?: import("../../types/protocol").OrgEstimate[] },
) => coreRequest("organizations.set_settings", { project_id: projectId, ...patch });
export const isOrganizationEvent = (e: EventEnvelope): boolean =>
  e.event_type.startsWith("organization.") || e.event_type.startsWith("workflow.") || e.event_type.startsWith("task.");
