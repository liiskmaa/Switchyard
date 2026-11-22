import type { EventEnvelope } from "../types/protocol";
import { providerName } from "./agents";

export function formatRelative(thenMs: number, nowMs: number): string {
  const seconds = Math.round((nowMs - thenMs) / 1000);
  if (seconds < 5) return "just now";
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}

export function formatDuration(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const d = Math.floor(total / 86400);
  const h = Math.floor((total % 86400) / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  if (d > 0) return `${d}d ${h}h`;
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m ${s}s`;
  return `${s}s`;
}

// Formatters are built once: `toLocaleString(locale, options)` constructs a
// new Intl.DateTimeFormat per call, which made long timelines take seconds to
// render while an agent streamed.
const TIMESTAMP = new Intl.DateTimeFormat(undefined, {
  year: "numeric",
  month: "short",
  day: "2-digit",
  hour: "2-digit",
  minute: "2-digit",
  second: "2-digit",
});
const CLOCK = new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "numeric", second: "numeric" });

export function formatTimestamp(ms: number): string {
  return TIMESTAMP.format(ms);
}

/** Time of day, as `Date.prototype.toLocaleTimeString()` shows it. */
export function formatClock(ms: number): string {
  return CLOCK.format(ms);
}

const usd = (v: number): string => `~$${v.toFixed(4)}`;

/**
 * Cost text for an `agent.turn_completed` payload. `turn_cost_usd` is this turn's cost;
 * `cost_usd` is the agent's running total for the whole session (it carries across a
 * resume), so it is labelled as such and never shown as the turn's cost. Events journaled
 * before `turn_cost_usd` existed, and aborted turns, only have the session total.
 */
export function turnCostText(payload: Record<string, unknown>): string {
  const turn = payload["turn_cost_usd"];
  const total = payload["cost_usd"];
  const parts: string[] = [];
  if (typeof turn === "number") parts.push(`${usd(turn)} this turn`);
  if (typeof total === "number" && total > 0) parts.push(`${usd(total)} session total`);
  return parts.join(" · ");
}

const count = new Intl.NumberFormat("en-US");

/** Token counts from an `agent.turn_completed` payload's `usage`, if reported. */
export function usageText(payload: Record<string, unknown>): string {
  const u = payload["usage"];
  if (!u || typeof u !== "object") return "";
  const n = (k: string) => (typeof (u as Record<string, unknown>)[k] === "number" ? ((u as Record<string, unknown>)[k] as number) : null);
  const input = n("input_tokens");
  const output = n("output_tokens");
  if (input === null || output === null) return "";
  return `${count.format(input)} in / ${count.format(output)} out tokens`;
}

export const shortId = (id: string): string => id.slice(-8);
export const shortOid = (oid: string): string => oid.slice(0, 8);

const short = (oid: string | undefined): string => (oid ? oid.slice(0, 10) : "?");

function str(payload: Record<string, unknown>, key: string): string | undefined {
  const value = payload[key];
  return typeof value === "string" ? value : undefined;
}

/** One-line human description of a journal event. */
export function describeEvent(event: EventEnvelope): string {
  const p = event.payload;
  switch (event.event_type) {
    case "core.started":
      return `Core ${str(p, "core_version") ?? "?"} started`;
    case "core.stopping":
      return `Core stopping${str(p, "reason") ? ` (${str(p, "reason")})` : ""}`;
    case "project.registered":
      return `Registered ${str(p, "name") ?? "project"} at ${str(p, "root_path") ?? "?"}`;
    case "project.removed":
      return `Removed registration for ${str(p, "root_path") ?? "project"}`;
    case "agent.started":
      return `${p["resumed"] === true ? "Resumed" : "Started"} ${providerName(str(p, "provider") ?? "claude_code")} ${str(p, "cli_version") ?? ""} (pid ${String(p["pid"] ?? "?")})`;
    case "agent.ready":
      return `Agent ready${str(p, "model") ? ` · ${str(p, "model")}` : ""}`;
    case "agent.state_changed":
      return `${str(p, "from") ?? "?"} → ${str(p, "to") ?? "?"}`;
    case "agent.turn_started":
      return `${p["queued"] === true ? "Queued" : "Task"}: ${str(p, "prompt") ?? ""}`;
    case "agent.message":
      return str(p, "text") ?? "";
    case "agent.tool_started":
      return `${str(p, "tool_name") ?? "Tool"} ${toolSummary(str(p, "tool_name"), p["input"])}`;
    case "agent.tool_completed":
      return `${str(p, "tool_name") ?? "Tool"} completed`;
    case "agent.tool_failed":
      return `${str(p, "tool_name") ?? "Tool"} ${p["rejected"] === true ? "not run" : "failed"}: ${str(p, "output") ?? ""}`;
    case "agent.turn_completed":
      return p["interrupted"] === true
        ? "Turn interrupted"
        : p["success"] === true
          ? "Turn completed"
          : `Turn ended with an error (${str(p, "reason") ?? "unknown"})`;
    case "agent.permission_requested":
      return `Permission requested: ${str(p, "tool_name") ?? "tool"} ${toolSummary(str(p, "tool_name"), p["input"])}`;
    case "agent.permission_resolved":
      return str(p, "source") === "policy"
        ? `${str(p, "tool_name") ?? "Tool"} denied by a policy change: ${str(p, "note") ?? ""}`
        : `${str(p, "tool_name") ?? "Tool"} ${str(p, "status") ?? "resolved"} (${str(p, "source") ?? ""})`;
    case "agent.permission_evaluated": {
      const target = str(p, "target") ? ` ${str(p, "target")}` : "";
      const verdict =
        str(p, "decision") === "allow" ? "Allowed automatically" : str(p, "decision") === "deny" ? "Blocked" : "Needs approval";
      return `${verdict}: ${str(p, "tool_name") ?? "tool"}${target} — ${str(p, "explanation") ?? ""}`;
    }
    case "policy.rule_changed":
      return `Policy rule ${str(p, "change") ?? "changed"}: “${str(p, "description") ?? ""}” (${str(p, "decision") ?? "?"} ${str(p, "category") ?? "?"}, revision ${String(p["revision"] ?? "?")})`;
    case "policy.settings_changed":
      return p["auto_mode"] === true
        ? "Auto mode turned on: file changes and commands inside workspaces run without asking"
        : "Auto mode turned off: file changes and commands ask again";
    case "agent.interrupted":
      return "Interrupt acknowledged";
    case "agent.session_resumed":
      return `Session resumed (#${String(p["resume_count"] ?? "?")})`;
    case "agent.warning":
      return `Warning: ${str(p, "message") ?? ""}`;
    case "agent.exited":
      return `Process exited (${p["exit_code"] !== null && p["exit_code"] !== undefined ? `code ${String(p["exit_code"])}` : `signal ${String(p["exit_signal"] ?? "?")}`}) → ${str(p, "final_state") ?? ""}`;
    case "agent.failed":
      return `Agent failed: ${str(p, "detail") ?? str(p, "reason") ?? ""}`;
    case "workspace.created":
      return `Workspace “${str(p, "name") ?? "?"}” created on ${str(p, "branch") ?? "?"}`;
    case "workspace.state_changed":
      return `Workspace ${str(p, "from") ?? "?"} → ${str(p, "to") ?? "?"}${str(p, "reason") ? ` (${str(p, "reason")})` : ""}`;
    case "workspace.cleanup_requested":
      return p["allowed"] === true
        ? "Workspace cleanup confirmed"
        : `Workspace cleanup refused: ${Array.isArray(p["blockers"]) ? (p["blockers"] as string[]).join(", ") : ""}`;
    case "workspace.removed":
      return `Workspace removed from disk; branch ${str(p, "branch") ?? "?"} kept`;
    case "workspace.cleanup_failed":
      return `Workspace cleanup failed (${str(p, "reason") ?? "?"}); files preserved`;
    case "files.changed": {
      const changes = Array.isArray(p["changes"]) ? (p["changes"] as { path?: string; kind?: string }[]) : [];
      const head = changes
        .slice(0, 3)
        .map((c) => `${c.kind ?? "?"} ${c.path ?? "?"}`)
        .join(", ");
      const more = changes.length > 3 ? ` and ${changes.length - 3} more` : "";
      return `Files: ${head}${more}`;
    }
    case "task.created":
      return `Task “${str(p, "title") ?? "?"}” created${str(p, "origin") === "proposal" ? " from an approved proposal" : ""} (${str(p, "status") ?? "?"})`;
    case "task.updated":
      return `Task edited: ${Array.isArray(p["fields"]) ? (p["fields"] as string[]).join(", ") : ""}`;
    case "task.status_changed":
      return `Task ${str(p, "from") ?? "?"} → ${str(p, "to") ?? "?"}${str(p, "reason") ? ` (${str(p, "reason")})` : ""}`;
    case "task.dependency_added":
      return "Dependency added";
    case "task.dependency_removed":
      return "Dependency removed";
    case "task.assigned": {
      const model = str(p, "model");
      const step = str(p, "step_key");
      return `${str(p, "role") ?? "Work"} assigned to ${providerName(str(p, "provider") ?? "claude_code")}${model ? ` (${model})` : ""}${step ? ` as workflow step ${step}` : ""}`;
    }
    case "task.assignment_started":
      return `${p["resumed"] === true ? "Resumed" : "Started"} a ${str(p, "access") === "review" ? "read-only review" : "working"} session with task context v${String(p["context_version"] ?? "?")}`;
    case "task.assignment_ended":
      return `Assignment ${str(p, "status") ?? "ended"}${str(p, "outcome") ? `: ${str(p, "outcome")}` : ""}`;
    case "task.message_recorded":
      return `${MESSAGE_LABEL[str(p, "kind") ?? ""] ?? str(p, "kind") ?? "Message"} from ${str(p, "sender") ?? "?"}${str(p, "summary") ? `: ${str(p, "summary")}` : ""}`;
    case "task.review_recorded":
      return p["workspace_changed"] === true
        ? `Review recorded as ${str(p, "result") ?? "?"}: the workspace changed during the review (reviewer said ${str(p, "reported_result") ?? "?"})`
        : `Review: ${str(p, "result") ?? "?"} (${String(p["findings"] ?? 0)} findings)`;
    case "task.proposal_decided":
      return `Proposed subtask ${str(p, "decision") ?? "decided"}`;
    case "integration.created":
      return `Integration of ${str(p, "source_branch") ?? "?"} into ${str(p, "target_branch") ?? "?"} proposed by ${str(p, "created_by") ?? "?"}`;
    case "integration.prepared": {
      const blockers = Array.isArray(p["blockers"]) ? (p["blockers"] as string[]) : [];
      return `Checked ${short(str(p, "source_commit"))} → ${short(str(p, "target_commit"))}: ${blockers.length ? `blocked (${blockers.join(", ")})` : "ready for approval"}`;
    }
    case "integration.status_changed":
      return `Integration ${str(p, "from") ?? "?"} → ${str(p, "to") ?? "?"}${str(p, "reason") ? ` (${str(p, "reason")})` : ""}`;
    case "integration.workspace_changed": {
      const action = str(p, "action");
      const files = Array.isArray(p["files"]) ? (p["files"] as string[]).join(", ") : "?";
      const what =
        action === "committed"
          ? `committed the workspace (${short(str(p, "commit"))})`
          : action === "updated"
            ? `merged the target into the workspace (${short(str(p, "commit"))})`
            : action === "conflicts"
              ? `merging the target into the workspace conflicts in ${files}`
              : action === "aborted"
                ? "aborted the conflict resolution"
                : (action ?? "changed the workspace");
      return `${str(p, "by") ?? "Operator"} ${what}`;
    }
    case "integration.validation_started":
      return `Validation started for ${short(str(p, "source_commit"))}`;
    case "integration.validation_step_finished":
      return `${str(p, "name") ?? "Command"}: ${str(p, "status") ?? "?"}${typeof p["exit_code"] === "number" ? ` (exit ${String(p["exit_code"])})` : ""}`;
    case "integration.validation_finished":
      return `Validation ${str(p, "status") ?? "finished"} for ${short(str(p, "source_commit"))}`;
    case "integration.approved":
      return `${str(p, "approved_by") ?? "Operator"} approved ${short(str(p, "source_commit"))} onto ${short(str(p, "target_commit"))}`;
    case "integration.integrated":
      return `Integrated: ${str(p, "target_branch") ?? "?"} is now at ${short(str(p, "merge_commit"))}${p["recovered"] === true ? " (confirmed after a Core restart)" : ""}`;
    case "integration.failed":
      return `Integration failed: ${str(p, "reason") ?? "?"}${p["target_moved"] === true ? " (the target branch is somewhere unexpected)" : ""}`;
    case "integration.operation_started":
      return `Core started ${(str(p, "kind") ?? "an operation").replaceAll("_", " ")} (requested by ${str(p, "requested_by") ?? "?"})`;
    case "integration.operation_finished": {
      const kind = (str(p, "kind") ?? "operation").replaceAll("_", " ");
      const status = str(p, "status") ?? "?";
      const error = str(p, "error");
      return `${kind}: ${status}${error ? ` — ${error}` : ""}`;
    }
    case "integration.settings_changed":
      return `${str(p, "changed_by") ?? "Operator"} ${p["require_review_after"] === true ? "required" : "stopped requiring"} an approved review for integrations`;
    case "integration.validation_command_changed": {
      const after = p["after"] as Record<string, unknown> | null | undefined;
      const before = p["before"] as Record<string, unknown> | null | undefined;
      const name = (after?.["name"] ?? before?.["name"] ?? "?") as string;
      return `${str(p, "changed_by") ?? "Operator"} ${str(p, "action") ?? "changed"} validation command ${name}`;
    }
    case "workflow.started":
      return str(p, "definition") === "organization"
        ? `Agent organization started by ${str(p, "started_by") ?? "?"}`
        : `Engineering workflow started by ${str(p, "started_by") ?? "?"}`;
    case "workflow.stage_changed": {
      const model = str(p, "model");
      return `Workflow: ${str(p, "stage") ?? "?"} (${str(p, "step_key") ?? "?"}) by ${providerName(str(p, "provider") ?? "claude_code")}, ${model ?? "CLI default model"}`;
    }
    case "workflow.waiting":
      return `Workflow needs you: ${str(p, "reason") ?? "?"}`;
    case "workflow.paused":
      return `Workflow paused by ${str(p, "by") ?? "?"}`;
    case "workflow.resumed":
      return `Workflow resumed by ${str(p, "by") ?? "?"}`;
    case "workflow.completed":
      return `Workflow ready for delivery: ${str(p, "reason") ?? ""}`;
    case "workflow.delivery_changed":
      return `Delivery: ${str(p, "reason") ?? str(p, "phase") ?? "?"}`;
    case "workflow.tests_changed": {
      const phase = str(p, "phase");
      const head =
        phase === "passed" ? "Tests passed" : phase === "failed" ? "Tests failed" : phase === "needs_authorization" ? "Tests need your authorization" : "Running the tests";
      return `${head}: ${str(p, "reason") ?? ""}`;
    }
    case "workflow.planned": {
      const subtasks = Array.isArray(p["subtasks"]) ? (p["subtasks"] as unknown[]).map(String) : [];
      return `Split into ${subtasks.length} subtasks: ${subtasks.join(", ")}`;
    }
    case "workflow.subtask_completed":
      return `Subtask ${String(p["number"] ?? "?")} done: ${str(p, "title") ?? ""}`;
    case "workflow.delivered":
      return str(p, "reason") ?? "Delivered";
    case "workflow.failed":
      return `Workflow failed: ${str(p, "reason") ?? "?"}`;
    case "workflow.cancelled":
      return `Workflow cancelled: ${str(p, "reason") ?? "?"}`;
    case "workflow.settings_changed":
      return `${str(p, "changed_by") ?? "Operator"} changed the project's workflow roles and models`;
    case "organization.started":
      return "An agent organization was created for the task";
    case "organization.agent_added":
      return `${str(p, "added_by") ?? "?"} added ${str(p, "title") ?? "an agent"} (${providerName(str(p, "provider") ?? "claude_code")})`;
    case "organization.task_delegated":
      return `${str(p, "delegated_by") ?? "?"} delegated “${str(p, "title") ?? "?"}” to ${str(p, "owner") ?? "?"}`;
    case "organization.supervision_started":
      return `${str(p, "supervisor") ?? "A supervisor"} is reviewing ${str(p, "key") ?? "its scope"}`;
    case "organization.work_started":
      return `${str(p, "agent") ?? "A specialist"} started on ${str(p, "key") ?? "?"}`;
    case "organization.assessed":
      return `${str(p, "by") ?? "?"}: ${str(p, "verdict") ?? "?"} ${str(p, "key") ?? ""}`.trim();
    case "organization.observed":
      return `Switchyard noticed: ${str(p, "text") ?? "?"}`;
    case "organization.escalated":
      return `${str(p, "by") ?? "?"} asks ${str(p, "to") === "user" ? "you" : "its supervisor"}: ${str(p, "question") ?? "?"}`;
    case "organization.directive_rejected":
      return `${str(p, "by") ?? "?"}'s plan was not accepted: ${str(p, "error") ?? "?"}`;
    case "organization.context_added":
      return "Shared context updated";
    case "organization.status_changed":
      return `Organization ${str(p, "status") ?? "?"}${str(p, "reason") ? `: ${str(p, "reason")}` : ""}`;
    case "organization.settings_changed":
      return "The project's organization settings changed";
    default:
      return event.event_type;
  }
}

/** Human names of task message kinds. */
export const MESSAGE_LABEL: Record<string, string> = {
  task_assignment: "Assignment",
  status_update: "Status update",
  info_request: "Question",
  findings: "Findings",
  proposal: "Proposal",
  review_result: "Review",
  blocker: "Blocker",
  delegation_request: "Delegation request",
  completion_report: "Completion report",
  operator_note: "Operator note",
  malformed_report: "Unusable report",
};

/** Merges events by sequence number, keeping the newest `limit`, ascending. */
export function mergeEvents(current: EventEnvelope[], incoming: EventEnvelope[], limit: number): EventEnvelope[] {
  const bySeq = new Map<number, EventEnvelope>();
  for (const e of current) bySeq.set(e.seq, e);
  for (const e of incoming) bySeq.set(e.seq, e);
  const merged = [...bySeq.values()].sort((a, b) => a.seq - b.seq);
  return merged.length > limit ? merged.slice(merged.length - limit) : merged;
}

/** Short human summary of a tool input (command, path, pattern). */
export function toolSummary(tool: string | undefined, input: unknown): string {
  if (typeof input !== "object" || input === null) return "";
  const i = input as Record<string, unknown>;
  const pick = (k: string) => (typeof i[k] === "string" ? (i[k] as string) : undefined);
  switch (tool) {
    case "Bash":
      return pick("command") ?? "";
    case "Write":
    case "Edit":
    case "Read":
    case "NotebookEdit":
      return pick("file_path") ?? pick("notebook_path") ?? "";
    case "Glob":
    case "Grep":
      return pick("pattern") ?? "";
    default:
      return "";
  }
}
