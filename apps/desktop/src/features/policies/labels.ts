// User-facing wording for policies (ADR 0019): describe what Switchyard
// answers, never imply a sandbox.
import type {
  OperationCategory,
  PolicyConditions,
  PolicyDecision,
  PolicyEvaluation,
  PolicyReason,
  PolicyRule,
  PolicyRuleRef,
} from "../../types/protocol";
import type { Tone } from "../../components/ui";

export const DECISION_LABEL: Record<PolicyDecision, string> = {
  allow: "Allow automatically",
  ask: "Ask every time",
  deny: "Block",
};

export const DECISION_TONE: Record<PolicyDecision, Tone> = { allow: "ok", ask: "warn", deny: "error" };

export const CATEGORY_LABEL: Record<OperationCategory, string> = {
  file_read: "Read files",
  file_write: "Create or overwrite files",
  file_edit: "Edit files",
  file_delete: "Delete files",
  directory_list: "List directories",
  search: "Search files",
  shell: "Run shell commands",
  network: "Network access",
  external_tool: "External tools (MCP)",
  unknown: "Unrecognized operations",
};

export const REASON_LABEL: Record<PolicyReason, string> = {
  system_restriction: "Switchyard safety rule",
  rule: "Configured rule",
  default: "Built-in default",
  capped: "Limited by a safety rule",
  auto_mode: "Auto mode",
};

/** Categories whose effect on files is not known (no path condition). */
export function categoryNote(category: OperationCategory): string | null {
  switch (category) {
    case "shell":
      return "A command can read, write, delete and reach the network. Switchyard does not inspect what it does.";
    case "file_delete":
    case "directory_list":
      return category === "file_delete"
        ? "Claude Code deletes files with shell commands; Codex deletes them in patches, which this category covers."
        : "Neither agent has a separate tool for this; it happens through shell commands.";
    case "network":
    case "external_tool":
      return "These tools are not enabled for Claude Code or Codex sessions, so requests for them are blocked by default. A rule can only make them ask or stay blocked.";
    default:
      return null;
  }
}

/**
 * Plain-words conditions. A path condition matches a file or a directory and
 * everything under it. An allow rule never reaches outside the workspace
 * (Core caps it to ask), so it says so.
 */
export function describeConditions(category: OperationCategory, c: PolicyConditions, decision?: PolicyDecision): string {
  if (c.command !== undefined) return `exactly “${c.command}”`;
  if (c.path_prefix !== undefined) return `at or under ${c.path_prefix}`;
  if (category === "shell") return "any command";
  if (category === "network" || category === "external_tool") return "any request";
  return decision === "allow" ? "anywhere in the workspace" : "anywhere";
}

export function scopeLabel(rule: Pick<PolicyRule, "scope" | "workspace_name">): string {
  return rule.scope === "project" ? "Every workspace" : `Only in “${rule.workspace_name ?? "workspace"}”`;
}

export function ruleSummary(rule: Pick<PolicyRule, "decision" | "category" | "conditions">): string {
  return `${DECISION_LABEL[rule.decision]}: ${CATEGORY_LABEL[rule.category].toLowerCase()} ${describeConditions(
    rule.category,
    rule.conditions,
    rule.decision,
  )}`;
}

export function ruleRefLabel(r: PolicyRuleRef): string {
  return `${r.scope === "project" ? "Project" : "Workspace"} rule “${r.description}” (revision ${r.revision})`;
}

/** What an evaluation was about: the command or the first path. */
export function evaluationTarget(e: PolicyEvaluation): string {
  const op = e.input.operation;
  if (op?.command) return op.command;
  if (op?.paths && op.paths.length > 0) return op.paths.join(", ");
  if (op?.problem) return op.problem;
  return "";
}

/** Outcome of an evaluation including the human decision for `ask`. */
export function evaluationOutcome(e: PolicyEvaluation): { label: string; tone: Tone } {
  if (e.trigger !== "request") return { label: "Blocked after a policy change", tone: "error" };
  if (e.decision === "allow") return { label: "Allowed automatically", tone: "ok" };
  if (e.decision === "deny") return { label: "Blocked", tone: "error" };
  switch (e.approval_status) {
    case "pending":
      return { label: "Waiting for you", tone: "warn" };
    case "approved":
      return { label: "Asked · approved", tone: "ok" };
    case "denied":
      return { label: "Asked · denied", tone: "error" };
    case "expired":
      return { label: "Asked · expired (denied)", tone: "error" };
    case "cancelled":
      return { label: "Asked · cancelled", tone: "neutral" };
    default:
      return { label: "Asked", tone: "warn" };
  }
}

/** Client-side mirror of Core's exact-command check, for early feedback only. Core decides. */
export function commandProblem(command: string): string | null {
  if (command === "") return "Enter the exact command.";
  if (/^ | $| {2}/.test(command)) return "Separate words with single spaces.";
  const bad = [...command].find((c) => !/[A-Za-z0-9 _./:@%+,=-]/.test(c));
  if (bad !== undefined) return `“${bad}” is not allowed: only plain words, no quotes, variables, globs, pipes or separators.`;
  const first = command.split(" ")[0] ?? "";
  if (first.includes("=") || !/^[A-Za-z0-9._/]/.test(first)) return "The command must start with a program name.";
  return null;
}
