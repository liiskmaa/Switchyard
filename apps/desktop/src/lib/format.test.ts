import { describe, expect, it } from "vitest";

import type { EventEnvelope } from "../types/protocol";
import { describeEvent, formatClock, formatDuration, formatRelative, formatTimestamp, mergeEvents, turnCostText } from "./format";
import { toCoreError } from "./core";

const event = (seq: number, event_type = "project.registered", payload: Record<string, unknown> = {}): EventEnvelope => ({
  seq,
  id: `id-${seq}`,
  schema_version: 1,
  event_type,
  occurred_at_ms: 0,
  project_id: null,
  workspace_id: null,
  session_id: null,
  task_id: null,
  agent_id: null,
  causation_id: null,
  payload,
});

describe("formatting", () => {
  it("formats relative times", () => {
    expect(formatRelative(1000, 2000)).toBe("just now");
    expect(formatRelative(0, 90_000)).toBe("1m ago");
    expect(formatRelative(0, 3 * 3600_000)).toBe("3h ago");
  });

  it("formats durations", () => {
    expect(formatDuration(5_000)).toBe("5s");
    expect(formatDuration(65_000)).toBe("1m 5s");
    expect(formatDuration(90_061_000)).toBe("1d 1h");
  });

  it("describes known and unknown events", () => {
    expect(describeEvent(event(1, "project.registered", { name: "api", root_path: "/r/api" }))).toBe(
      "Registered api at /r/api",
    );
    expect(describeEvent(event(1, "future.event"))).toBe("future.event");
  });
});

describe("mergeEvents", () => {
  it("dedupes by seq, sorts ascending and keeps the newest", () => {
    const merged = mergeEvents([event(1), event(3)], [event(2), event(3), event(4)], 3);
    expect(merged.map((e) => e.seq)).toEqual([2, 3, 4]);
  });
});

describe("toCoreError", () => {
  it("maps bridge command errors", () => {
    const err = toCoreError({ code: "not_found", message: "project x not found" });
    expect(err.code).toBe("not_found");
    expect(err.message).toBe("project x not found");
  });

  it("wraps unknown failures", () => {
    expect(toCoreError("boom").code).toBe("client_error");
  });
});

describe("agent events", () => {
  it("summarises tools and file changes", async () => {
    const { toolSummary } = await import("./format");
    expect(toolSummary("Bash", { command: "cargo test" })).toBe("cargo test");
    expect(toolSummary("Write", { file_path: "/r/a.txt", content: "x" })).toBe("/r/a.txt");
    expect(toolSummary("Unknown", { x: 1 })).toBe("");
    expect(
      describeEvent(event(1, "files.changed", { changes: [{ path: "a", kind: "created" }, { path: "b", kind: "deleted" }] })),
    ).toBe("Files: created a, deleted b");
    expect(describeEvent(event(2, "agent.tool_failed", { tool_name: "Write", rejected: true, output: "no" }))).toBe(
      "Write not run: no",
    );
  });
});

describe("policy events", () => {
  it("say whether a request was decided automatically and why", () => {
    const allowed = event(1, "agent.permission_evaluated", {
      decision: "allow",
      tool_name: "Read",
      target: "/w/README.md",
      explanation: "Built-in default: reading files inside the workspace is allowed.",
    });
    expect(describeEvent(allowed)).toBe(
      "Allowed automatically: Read /w/README.md — Built-in default: reading files inside the workspace is allowed.",
    );
    expect(describeEvent(event(2, "agent.permission_evaluated", { decision: "deny", tool_name: "Teleport", explanation: "x" }))).toBe(
      "Blocked: Teleport — x",
    );
    expect(
      describeEvent(event(3, "agent.permission_resolved", { tool_name: "Bash", status: "denied", source: "policy", note: "No shell" })),
    ).toBe("Bash denied by a policy change: No shell");
    expect(
      describeEvent(
        event(4, "policy.rule_changed", { change: "created", description: "Docs", decision: "allow", category: "file_edit", revision: 1 }),
      ),
    ).toBe("Policy rule created: “Docs” (allow file_edit, revision 1)");
  });
});

describe("cached date formatters", () => {
  it("format exactly like the locale methods they replace", () => {
    for (const ms of [0, 1_794_430_000_000, Date.UTC(2026, 0, 2, 3, 4, 5)]) {
      expect(formatClock(ms)).toBe(new Date(ms).toLocaleTimeString());
      expect(formatTimestamp(ms)).toBe(
        new Date(ms).toLocaleString(undefined, { year: "numeric", month: "short", day: "2-digit", hour: "2-digit", minute: "2-digit", second: "2-digit" }),
      );
    }
  });
});

describe("turnCostText", () => {
  it("shows the turn's own cost and labels the running total as the session's", () => {
    expect(turnCostText({ turn_cost_usd: 0.00021418, cost_usd: 0.00224667 })).toBe("~$0.0002 this turn · ~$0.0022 session total");
  });
  it("never presents the session total as the turn's cost", () => {
    // Aborted turns and events journaled before turn_cost_usd existed only carry the total.
    expect(turnCostText({ cost_usd: 0.2663024 })).toBe("~$0.2663 session total");
  });
  it("shows nothing when no cost is known", () => {
    expect(turnCostText({ cost_usd: 0 })).toBe("");
    expect(turnCostText({})).toBe("");
  });
});

describe("task events", () => {
  it("describes the task lifecycle", () => {
    expect(describeEvent(event(1, "task.status_changed", { from: "ready", to: "assigned", reason: "implement assigned to codex" }))).toBe(
      "Task ready → assigned (implement assigned to codex)",
    );
    expect(describeEvent(event(2, "task.assigned", { role: "review", provider: "claude_code" }))).toBe(
      "review assigned to Claude Code",
    );
    expect(describeEvent(event(3, "task.assignment_started", { access: "review", context_version: 1, resumed: false }))).toBe(
      "Started a read-only review session with task context v1",
    );
    expect(describeEvent(event(4, "task.message_recorded", { kind: "proposal", sender: "agent", summary: "Split it" }))).toBe(
      "Proposal from agent: Split it",
    );
  });

  it("says when Core overrode a review verdict", () => {
    expect(
      describeEvent(event(5, "task.review_recorded", { result: "blocked", reported_result: "approved", workspace_changed: true, findings: 1 })),
    ).toBe("Review recorded as blocked: the workspace changed during the review (reviewer said approved)");
  });
});
