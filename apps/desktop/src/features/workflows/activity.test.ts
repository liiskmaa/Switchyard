import { describe, expect, it } from "vitest";

import type { EventEnvelope } from "../../types/protocol";
import { activityOf } from "./activity";

let seq = 0;
const ev = (event_type: string, at: number, payload: Record<string, unknown> = {}): EventEnvelope =>
  ({ seq: ++seq, event_type, occurred_at_ms: at, payload }) as unknown as EventEnvelope;

describe("live activity", () => {
  it("shows the running command and how long it has run", () => {
    const a = activityOf([
      ev("agent.turn_started", 1000),
      ev("agent.message", 2000, { text: "Checking the tests." }),
      ev("agent.tool_started", 3000, { tool_use_id: "t1", tool_name: "Bash", input: { command: "npx vitest run" } }),
    ]);
    expect(a.state).toBe("running_tool");
    expect(a.now).toBe("Running npx vitest run");
    expect(a.sinceMs).toBe(3000);
    expect(a.recent.map((r) => r.text)).toEqual(["npx vitest run", "Checking the tests.", "Started a turn"]);
  });

  it("thinks between tools, waits on an ask, and is idle after its turn", () => {
    const base = [
      ev("agent.turn_started", 1000),
      ev("agent.tool_started", 2000, { tool_use_id: "t1", tool_name: "Read", input: { file_path: "/w/src/a.ts" } }),
      ev("agent.tool_completed", 2500, { tool_use_id: "t1" }),
    ];
    expect(activityOf(base)).toMatchObject({ state: "thinking", sinceMs: 2500 });
    expect(activityOf(base).recent[0]?.text).toBe("Read src/a.ts");
    const asking = [...base, ev("agent.permission_evaluated", 3000, { decision: "ask" })];
    expect(activityOf(asking).state).toBe("waiting_approval");
    const allowed = [...base, ev("agent.permission_evaluated", 3000, { decision: "allow" })];
    expect(activityOf(allowed).state).toBe("thinking");
    expect(activityOf([...base, ev("agent.turn_completed", 4000)])).toMatchObject({ state: "idle", now: "Finished its turn" });
  });
});
