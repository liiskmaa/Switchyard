import { describe, expect, it } from "vitest";

import type { Integration, IntegrationStatus } from "../../types/protocol";
import { consequences, integrationActions } from "./labels";

const integration = (status: IntegrationStatus, blockers: string[] = [], configured = 1): Integration => ({
  id: "i",
  project_id: "p",
  task_id: null,
  workspace_id: "w",
  workspace_name: "w",
  source_branch: "switchyard/w",
  source_commit: "a".repeat(40),
  target_branch: "main",
  target_commit: "b".repeat(40),
  base_commit: null,
  status,
  status_reason: null,
  blockers: blockers.map((code) => ({ code, message: code })),
  preflight: {
    facts: {
      workspace_valid: { Ok: null },
      live_session: false,
      uncommitted_changes: blockers.includes("uncommitted_changes"),
      workspace_busy: [],
      source_ahead: 1,
      has_changes: true,
      merge: { kind: "up_to_date" },
      target_checkout: { path: "/repo", has_local_changes: false, busy: [] },
      identity: { Ok: "A <a@b>" },
      task: null,
      review: { required: false, satisfied: false, detail: "" },
      validations_configured: configured,
      validation_passed: false,
      validation_failed: false,
    },
  },
  prepared_at_ms: 0,
  approved_by: null,
  approved_at_ms: null,
  merge_commit: null,
  method: null,
  failure: null,
  revision: 1,
  created_at_ms: 0,
  updated_at_ms: 0,
  completed_at_ms: null,
});

describe("integrationActions", () => {
  it("offers approval only when ready with no blockers", () => {
    expect(integrationActions(integration("ready")).approve).toBe(true);
    expect(integrationActions(integration("ready", ["validation_required"])).approve).toBe(false);
    for (const s of ["open", "conflicted", "resolving", "validating", "integrating", "integrated", "failed"] as IntegrationStatus[]) {
      expect(integrationActions(integration(s)).approve).toBe(false);
    }
  });

  it("asks for a commit before validating uncommitted work", () => {
    const a = integrationActions(integration("open", ["uncommitted_changes", "validation_required"]));
    expect(a.commit).toBe(true);
    expect(a.validate).toBe(false);
    expect(integrationActions(integration("open", ["validation_required"])).validate).toBe(true);
    expect(integrationActions(integration("open", ["validation_required"], 0)).validate).toBe(false);
  });

  it("never cancels a running integration step", () => {
    expect(integrationActions(integration("integrating")).cancel).toBe(false);
    expect(integrationActions(integration("validating")).cancel).toBe(false);
  });
});

describe("consequences", () => {
  it("names both exact commits and the checkout, and says nothing is pushed", () => {
    const text = consequences(integration("ready")).join(" ");
    expect(text).toContain("aaaaaaaaaa");
    expect(text).toContain("bbbbbbbbbb");
    expect(text).toContain("/repo");
    expect(text).toContain("Nothing is pushed");
  });
});
