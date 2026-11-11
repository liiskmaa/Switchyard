import { describe, expect, it } from "vitest";

import type { LiveOutput } from "../types/protocol";
import { applyLive, type LiveState } from "./core";

const out = (seq: number, kind: LiveOutput["kind"], text = "", session_id = "s1"): LiveOutput => ({ session_id, seq, kind, text });

describe("applyLive", () => {
  it("accumulates text per message and resets on message_start", () => {
    let state: Record<string, LiveState> = {};
    for (const o of [out(1, "message_start"), out(2, "text", "Hel"), out(3, "text", "lo")]) state = applyLive(state, o);
    expect(state["s1"]?.text).toBe("Hello");
    state = applyLive(state, out(4, "message_start"));
    expect(state["s1"]?.text).toBe("");
  });

  it("ignores duplicate or out-of-order chunks", () => {
    let state = applyLive({}, out(5, "text", "a"));
    state = applyLive(state, out(5, "text", "a"));
    state = applyLive(state, out(4, "text", "b"));
    expect(state["s1"]?.text).toBe("a");
  });

  it("marks gaps without inventing text", () => {
    let state = applyLive({}, out(1, "text", "x"));
    state = applyLive(state, { session_id: "", seq: 0, kind: "gap", text: "", missed: 3 });
    expect(state["s1"]).toMatchObject({ text: "x", gap: true });
  });
});
