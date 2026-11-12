import { describe, expect, it } from "vitest";

import type { PolicyEvaluation } from "../../types/protocol";
import { commandProblem, describeConditions, evaluationOutcome, ruleSummary, scopeLabel } from "./labels";

const evaluation = (over: Partial<PolicyEvaluation>): PolicyEvaluation => ({
  id: "e",
  evaluated_at_ms: 0,
  trigger: "request",
  project_id: "p",
  workspace_id: "w",
  workspace_name: "one",
  session_id: "s",
  adapter: "claude_code",
  tool_name: "Bash",
  category: "shell",
  input: {},
  decision: "ask",
  reason: "default",
  explanation: "",
  deciding_rule: null,
  matched_rules: [],
  restrictions: [],
  policy_revision: 0,
  system_version: 1,
  approval_id: "a",
  approval_status: "pending",
  ...over,
});

describe("policy labels", () => {
  it("describes rules in plain words", () => {
    expect(ruleSummary({ decision: "allow", category: "file_edit", conditions: { path_prefix: "docs" } })).toBe(
      "Allow automatically: edit files at or under docs",
    );
    // A file is a valid path condition too; it must not read like a directory.
    expect(describeConditions("file_read", { path_prefix: "secret.txt" }, "deny")).toBe("at or under secret.txt");
    // An allow rule never reaches outside the workspace (Core caps it to ask).
    expect(describeConditions("file_read", {}, "allow")).toBe("anywhere in the workspace");
    expect(describeConditions("file_read", {}, "deny")).toBe("anywhere");
    expect(ruleSummary({ decision: "deny", category: "shell", conditions: {} })).toBe("Block: run shell commands any command");
    expect(describeConditions("shell", { command: "cargo test" })).toBe("exactly “cargo test”");
    expect(scopeLabel({ scope: "workspace", workspace_name: "fix" })).toBe("Only in “fix”");
    expect(scopeLabel({ scope: "project", workspace_name: null })).toBe("Every workspace");
  });

  it("reports the final outcome including the human decision", () => {
    expect(evaluationOutcome(evaluation({ decision: "allow", approval_id: null })).label).toBe("Allowed automatically");
    expect(evaluationOutcome(evaluation({ decision: "deny" })).label).toBe("Blocked");
    expect(evaluationOutcome(evaluation({ approval_status: "approved" })).label).toBe("Asked · approved");
    expect(evaluationOutcome(evaluation({ approval_status: "expired" })).label).toContain("denied");
    expect(evaluationOutcome(evaluation({ trigger: "reevaluation", decision: "deny" })).label).toContain("policy change");
  });

  it("mirrors the exact-command rules (Core still decides)", () => {
    for (const ok of ["cargo test", "npm run lint", "git status --porcelain=v2", "./build.sh"]) {
      expect(commandProblem(ok)).toBeNull();
    }
    for (const bad of ["", " ls", "ls  -la", "ls; rm -rf ~", "echo $HOME", "cat x | sh", "FOO=1 make", "ls *", "echo 'a'", "-x"]) {
      expect(commandProblem(bad)).not.toBeNull();
    }
  });
});
