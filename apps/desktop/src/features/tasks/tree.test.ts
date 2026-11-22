import { describe, expect, it } from "vitest";

import type { Task } from "../../types/protocol";
import { taskTree } from "./tree";

const task = (id: string, parent: string | null = null) => ({ id, parent_task_id: parent, title: id }) as Task;

describe("task tree", () => {
  it("puts subtasks under their main task, nested, in list order", () => {
    const rows = taskTree([task("a"), task("b"), task("a1", "a"), task("a2", "a"), task("a1x", "a1")]);
    expect(rows.map((r) => [r.task.id, r.depth, r.last, r.children])).toEqual([
      ["a", 0, false, 2],
      ["a1", 1, false, 1],
      ["a1x", 2, true, 0],
      ["a2", 1, true, 0],
      ["b", 0, false, 0],
    ]);
  });

  it("a subtask whose main task is not listed stays on top and is marked", () => {
    const rows = taskTree([task("s", "gone")]);
    expect(rows[0]).toMatchObject({ depth: 0, orphan: true });
  });
});
