import { coreRequest } from "../../lib/core";
import type { CoreMethods, EventEnvelope, PolicyConditions, PolicyDecision, PolicyRule } from "../../types/protocol";

export const effectivePolicy = (projectId: string, workspaceId?: string) =>
  coreRequest("policies.effective", workspaceId ? { project_id: projectId, workspace_id: workspaceId } : { project_id: projectId });
export const listRules = async (projectId: string) =>
  (await coreRequest("policies.list", { project_id: projectId })).rules;
export const ruleHistory = (ruleId: string) => coreRequest("policies.get", { rule_id: ruleId });
export const createRule = (params: CoreMethods["policies.create"]["params"]) => coreRequest("policies.create", params);
export const updateRule = (
  rule: PolicyRule,
  change: { decision?: PolicyDecision; conditions?: PolicyConditions; description?: string },
) => coreRequest("policies.update", { rule_id: rule.id, expected_revision: rule.revision, ...change });
export const setRuleEnabled = (rule: PolicyRule, enabled: boolean) =>
  coreRequest("policies.set_enabled", { rule_id: rule.id, expected_revision: rule.revision, enabled });
export const archiveRule = (rule: PolicyRule) =>
  coreRequest("policies.archive", { rule_id: rule.id, expected_revision: rule.revision });
export const listEvaluations = async (params: CoreMethods["policies.evaluations"]["params"]) =>
  (await coreRequest("policies.evaluations", params)).evaluations;

export const isPolicyEvent = (e: EventEnvelope): boolean => e.event_type === "policy.rule_changed";
export const isEvaluationEvent = (e: EventEnvelope): boolean =>
  e.event_type === "agent.permission_evaluated" || e.event_type === "agent.permission_resolved";
