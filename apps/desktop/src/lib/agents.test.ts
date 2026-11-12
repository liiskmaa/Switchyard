import { describe, expect, it } from "vitest";

import { CAPABILITY_LABELS, nativeIdLabel, providerName, providerShort, supported } from "./agents";
import { usageText } from "./format";

describe("agent helpers", () => {
  it("names both providers and passes unknown ones through", () => {
    expect(providerName("claude_code")).toBe("Claude Code");
    expect(providerName("codex")).toBe("Codex");
    expect(providerShort("claude_code")).toBe("Claude");
    expect(nativeIdLabel("codex")).toBe("Codex thread");
    expect(providerName("other")).toBe("other");
  });

  it("treats only `supported` as supported", () => {
    expect(supported("supported")).toBe(true);
    expect(supported("unknown")).toBe(false);
    expect(supported("unsupported")).toBe(false);
    expect(supported(undefined)).toBe(false);
  });

  it("labels every capability", () => {
    const keys = CAPABILITY_LABELS.map(([k]) => k);
    expect(new Set(keys).size).toBe(keys.length);
    expect(keys).toContain("cost");
    expect(keys).toContain("usage_tokens");
  });
});

describe("usageText", () => {
  it("shows reported token counts and nothing otherwise", () => {
    expect(usageText({ usage: { input_tokens: 15287, output_tokens: 45, total_tokens: 15332 } })).toBe("15,287 in / 45 out tokens");
    expect(usageText({})).toBe("");
    expect(usageText({ usage: null })).toBe("");
  });
});
