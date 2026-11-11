import { coreRequest } from "../../lib/core";
import type { EventEnvelope, SessionState } from "../../types/protocol";

export const listAgents = async (refresh = false) => (await coreRequest("agents.list", { refresh })).agents;
export const listSessions = async (params: { project_id?: string; live_only?: boolean; limit?: number } = {}) =>
  (await coreRequest("sessions.list", params)).sessions;
export const getSession = (sessionId: string) => coreRequest("sessions.get", { session_id: sessionId });
export const startSession = async (projectId: string, prompt?: string) =>
  (await coreRequest("sessions.start", prompt ? { project_id: projectId, prompt } : { project_id: projectId })).session;
export const sendMessage = (sessionId: string, text: string, queueIfBusy: boolean) =>
  coreRequest("sessions.send", { session_id: sessionId, text, queue_if_busy: queueIfBusy });
export const interruptSession = (sessionId: string) => coreRequest("sessions.interrupt", { session_id: sessionId });
export const stopSession = (sessionId: string) => coreRequest("sessions.stop", { session_id: sessionId });
export const resumeSession = (sessionId: string) => coreRequest("sessions.resume", { session_id: sessionId });
export const sessionEvents = async (sessionId: string) =>
  (await coreRequest("events.list", { session_id: sessionId, after_seq: 0, limit: 1000 })).events;
export const pendingApprovals = async (sessionId?: string) =>
  (await coreRequest("approvals.list", sessionId ? { status: "pending", session_id: sessionId } : { status: "pending" }))
    .approvals;
export const decideApproval = (approvalId: string, decision: "approve" | "deny", note?: string) =>
  coreRequest("approvals.decide", note ? { approval_id: approvalId, decision, note } : { approval_id: approvalId, decision });

export const isAgentEvent = (e: EventEnvelope): boolean => e.event_type.startsWith("agent.");
export const isApprovalEvent = (e: EventEnvelope): boolean => e.event_type.startsWith("agent.permission_");

export const STATE_LABEL: Record<SessionState, string> = {
  starting: "Starting",
  initializing: "Initializing",
  idle: "Idle",
  running: "Running",
  waiting_for_approval: "Waiting for approval",
  interrupted: "Interrupted",
  completed: "Completed",
  failed: "Failed",
  stopped: "Stopped",
};

export function stateTone(state: SessionState): "ok" | "warn" | "error" | "neutral" | "accent" {
  switch (state) {
    case "running":
      return "accent";
    case "waiting_for_approval":
    case "starting":
    case "initializing":
    case "interrupted":
      return "warn";
    case "idle":
    case "completed":
      return "ok";
    case "failed":
      return "error";
    case "stopped":
      return "neutral";
  }
}
