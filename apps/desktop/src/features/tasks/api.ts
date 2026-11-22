import { coreRequest } from "../../lib/core";
import type { AgentProvider, CoreMethods, EventEnvelope, Task, TaskRole } from "../../types/protocol";

export const listTasks = async (projectId: string, includeClosed: boolean) =>
  (await coreRequest("tasks.list", { project_id: projectId, include_closed: includeClosed, limit: 500 })).tasks;
export const getTask = (taskId: string) => coreRequest("tasks.get", { task_id: taskId });
export const createTask = (params: CoreMethods["tasks.create"]["params"]) => coreRequest("tasks.create", params);
export const updateTask = (task: Task, change: Omit<CoreMethods["tasks.update"]["params"], "task_id" | "expected_revision">) =>
  coreRequest("tasks.update", { task_id: task.id, expected_revision: task.revision, ...change });
export const publishTask = (taskId: string) => coreRequest("tasks.publish", { task_id: taskId });
export const addDependency = (taskId: string, dependsOn: string) =>
  coreRequest("tasks.add_dependency", { task_id: taskId, depends_on: dependsOn });
export const removeDependency = (taskId: string, dependsOn: string) =>
  coreRequest("tasks.remove_dependency", { task_id: taskId, depends_on: dependsOn });
export const assignTask = (taskId: string, role: TaskRole, provider: AgentProvider, workspaceId: string) =>
  coreRequest("tasks.assign", { task_id: taskId, role, provider, workspace_id: workspaceId, start: true });
export const cancelAssignment = (assignmentId: string) => coreRequest("tasks.cancel_assignment", { assignment_id: assignmentId });
export const sendNote = (taskId: string, text: string) => coreRequest("tasks.note", { task_id: taskId, text });
export const decideTask = (action: "complete" | "fail" | "cancel" | "retry", taskId: string, note?: string) =>
  coreRequest(`tasks.${action}`, note ? { task_id: taskId, note } : { task_id: taskId });
export const decideProposal = (proposalId: string, approve: boolean) =>
  coreRequest("tasks.decide_proposal", { proposal_id: proposalId, approve });
export const taskEvents = async (taskId: string) =>
  (await coreRequest("events.list", { task_id: taskId, after_seq: 0, limit: 500 })).events;

/** Events that can change what the Tasks view shows. */
export const isTaskEvent = (e: EventEnvelope): boolean => e.event_type.startsWith("task.");
