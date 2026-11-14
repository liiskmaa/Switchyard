import { coreRequest } from "../../lib/core";
import type { CoreMethods, EventEnvelope, Integration } from "../../types/protocol";

export const listIntegrations = async (params: CoreMethods["integrations.list"]["params"]) =>
  (await coreRequest("integrations.list", params)).integrations;
export const getIntegration = (id: string) => coreRequest("integrations.get", { integration_id: id });
export const createIntegration = (params: CoreMethods["integrations.create"]["params"]) =>
  coreRequest("integrations.create", params);
export const refreshIntegration = (id: string) => coreRequest("integrations.refresh", { integration_id: id });
export const integrationChanges = (id: string) => coreRequest("integrations.changes", { integration_id: id });
export const integrationPatch = (id: string, path: string | undefined, uncommitted: boolean) =>
  coreRequest("integrations.patch", path ? { integration_id: id, path, uncommitted } : { integration_id: id, uncommitted });
export const commitWorkspace = (id: string, message: string) =>
  coreRequest("integrations.commit_workspace", { integration_id: id, message });
export const updateFromTarget = (id: string) => coreRequest("integrations.update_from_target", { integration_id: id });
export const abortResolution = (id: string) => coreRequest("integrations.abort_resolution", { integration_id: id });
export const runValidation = (id: string, authorize: { command_id: string; command_line: string }[]) =>
  coreRequest("integrations.validate", { integration_id: id, authorize });
export const cancelValidation = (id: string) => coreRequest("integrations.cancel_validation", { integration_id: id });
/** Approves exactly the commits the operator was shown. */
export const approveIntegration = (i: Integration) =>
  coreRequest("integrations.approve", {
    integration_id: i.id,
    source_commit: i.source_commit,
    target_commit: i.target_commit,
  });
export const cancelIntegration = (id: string, reason?: string) =>
  coreRequest("integrations.cancel", reason ? { integration_id: id, reason } : { integration_id: id });
export const integrationSettings = (projectId: string) => coreRequest("integrations.settings", { project_id: projectId });
export const setRequireReview = (projectId: string, requireReview: boolean) =>
  coreRequest("integrations.set_settings", { project_id: projectId, require_review: requireReview });
export const saveCommand = (params: CoreMethods["integrations.set_command"]["params"]) =>
  coreRequest("integrations.set_command", params);
export const deleteCommand = (commandId: string) => coreRequest("integrations.delete_command", { command_id: commandId });
export const projectEvents = async (projectId: string) =>
  (await coreRequest("events.list", { project_id: projectId, after_seq: 0, limit: 1000 })).events;

export const isIntegrationEvent = (e: EventEnvelope): boolean => e.event_type.startsWith("integration.");
export const eventOf = (e: EventEnvelope, id: string): boolean => e.payload["integration_id"] === id;
