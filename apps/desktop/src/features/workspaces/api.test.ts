import { describe, expect, it } from "vitest";

import { describeEvent } from "../../lib/format";
import type { EventEnvelope } from "../../types/protocol";
import { WORKSPACE_STATUS_LABEL, isWorkspaceEvent, reasonParts, workspaceTone } from "./api";

const event = (event_type: string, payload: Record<string, unknown>): EventEnvelope => ({
  seq: 1,
  id: "e",
  schema_version: 1,
  event_type,
  occurred_at_ms: 0,
  project_id: "p",
  workspace_id: "w",
  session_id: null,
  task_id: null,
  agent_id: null,
  causation_id: null,
  payload,
});

describe("workspace helpers", () => {
  it("splits status reasons into code and detail", () => {
    expect(reasonParts("missing_worktree: /x does not exist")).toEqual({ code: "missing_worktree", detail: "/x does not exist" });
    expect(reasonParts("legacy_checkout")).toEqual({ code: "legacy_checkout", detail: "" });
    expect(reasonParts(null)).toBeNull();
  });

  it("labels every status and flags failures", () => {
    expect(Object.keys(WORKSPACE_STATUS_LABEL)).toHaveLength(10);
    expect(workspaceTone("failed")).toBe("error");
    expect(workspaceTone("active")).toBe("accent");
    expect(workspaceTone("dirty")).toBe("warn");
  });

  it("refreshes on workspace, session and file events only", () => {
    expect(isWorkspaceEvent(event("workspace.removed", {}))).toBe(true);
    expect(isWorkspaceEvent(event("files.changed", {}))).toBe(true);
    expect(isWorkspaceEvent(event("agent.message", {}))).toBe(false);
  });

  it("describes workspace events", () => {
    expect(describeEvent(event("workspace.removed", { branch: "switchyard/x" }))).toBe(
      "Workspace removed from disk; branch switchyard/x kept",
    );
    expect(describeEvent(event("workspace.cleanup_requested", { allowed: false, blockers: ["untracked_files"] }))).toBe(
      "Workspace cleanup refused: untracked_files",
    );
    expect(describeEvent(event("workspace.state_changed", { from: "ready", to: "active", reason: null }))).toBe(
      "Workspace ready → active",
    );
  });
});
