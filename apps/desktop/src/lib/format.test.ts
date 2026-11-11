import { describe, expect, it } from "vitest";

import type { EventEnvelope } from "../types/protocol";
import { describeEvent, formatDuration, formatRelative, mergeEvents } from "./format";
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
