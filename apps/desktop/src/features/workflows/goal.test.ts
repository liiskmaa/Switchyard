import { describe, expect, it } from "vitest";

import type { OrganizationInfo } from "../../types/protocol";
import { goalChecklist } from "./goal";

function org(extra: Partial<OrganizationInfo>): OrganizationInfo {
  return {
    tasks: [{ key: "root", parent: null, acceptance: ["Each user has their own mailbox"] }],
    ...extra,
  } as unknown as OrganizationInfo;
}

describe("goalChecklist", () => {
  it("lists the user's criteria as not checked before the first goal check", () => {
    const c = goalChecklist(org({}));
    expect(c.verdict).toBeNull();
    expect(c.rows).toEqual([{ criterion: "Each user has their own mailbox", status: "unchecked", evidence: "" }]);
  });

  it("shows the referee's criteria, verdict and the stalled turns", () => {
    const c = goalChecklist(
      org({
        turns_since_progress: 4,
        goal: {
          verdict: "drifting",
          summary: "Mailboxes done; derived views are extra.",
          criteria: [{ criterion: "Per-user mailbox", status: "met", evidence: "mail-settings.ts" }],
          off_goal: ["derived-views"],
          at_ms: 5,
          by: "gemini",
        },
      }),
    );
    expect(c.verdict).toBe("drifting");
    expect(c.rows[0]?.status).toBe("met");
    expect(c.offGoal).toEqual(["derived-views"]);
    expect(c.stalledTurns).toBe(4);
  });
});
