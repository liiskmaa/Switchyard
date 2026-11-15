import { describe, expect, it } from "vitest";

import { approvalIsStale, otherInputFields, requestAction, shortPath } from "./decision";

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

describe("otherInputFields for Codex input", () => {
  it("lists every flag except what the card already renders", () => {
    const shell = { command: "touch x", raw_command: "/bin/zsh -lc 'touch x'", cwd: "/ws", cwd_is_session_cwd: true, unexpected_fields: [] };
    expect(otherInputFields(shell).map(([k]) => k)).toEqual(["raw_command", "cwd", "cwd_is_session_cwd", "unexpected_fields"]);
    const patch = { changes: [{ path: "/ws/a", kind: "add" }], grant_root: "/", other_thread: false };
    expect(otherInputFields(patch, ["changes"])).toEqual([
      ["grant_root", "/"],
      ["other_thread", "false"],
    ]);
  });
});

describe("plain-language requests", () => {
  it("names the file relative to the workspace", () => {
    expect(shortPath("/data/workspaces/p1/w1/src/greeter.py")).toBe("src/greeter.py");
    expect(shortPath("/Users/me/notes/todo.md")).toBe("…/notes/todo.md");
  });
  it("describes patches, commands and file tools", () => {
    const patch = { changes: [{ path: "/d/workspaces/p/w/greeter.py", kind: "add" }] };
    expect(requestAction("apply_patch", patch)).toBe("create greeter.py");
    expect(requestAction("Bash", { command: "npm test" })).toBe("run `npm test`");
    expect(requestAction("Edit", { file_path: "/d/workspaces/p/w/a.ts" })).toBe("edit a.ts");
    expect(requestAction("WebFetch", {}, "Network access")).toBe("network access");
  });
});
