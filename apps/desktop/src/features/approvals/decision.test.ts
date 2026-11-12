import { describe, expect, it } from "vitest";

import { approvalIsStale, otherInputFields } from "./decision";

describe("approvalIsStale", () => {
  it("reloads after the approval was settled elsewhere or denied by policy", () => {
    expect(approvalIsStale("conflict")).toBe(true);
    expect(approvalIsStale("policy_denied")).toBe(true);
  });
  it("keeps the card for transient failures so the person can retry", () => {
    expect(approvalIsStale("core_unavailable")).toBe(false);
    expect(approvalIsStale("internal")).toBe(false);
  });
});

describe("otherInputFields", () => {
  it("surfaces flags next to the command so they cannot be approved unseen", () => {
    expect(otherInputFields({ command: "make", description: "x", run_in_background: true, dangerouslyDisableSandbox: true })).toEqual([
      ["run_in_background", "true"],
      ["dangerouslyDisableSandbox", "true"],
    ]);
    expect(otherInputFields({ file_path: "/a", old_string: "a", new_string: "b", replace_all: true })).toEqual([["replace_all", "true"]]);
    expect(otherInputFields({ command: "ls" })).toEqual([]);
    expect(otherInputFields("not an object")).toEqual([]);
  });
});
