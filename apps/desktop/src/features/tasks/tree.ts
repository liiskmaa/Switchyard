import type { Task } from "../../types/protocol";

export interface TreeRow {
  task: Task;
  /** 0 for a top-level task. */
  depth: number;
  /** The last of its siblings (draws └ instead of ├). */
  last: boolean;
  /** Its parent is not in the list (filtered out): shown at the top with a note. */
  orphan: boolean;
  /** Subtasks in the list. */
  children: number;
}

/**
 * The tasks as a tree: each subtask right under its main task, in the list's
 * order. A subtask whose main task is not listed (for example a completed
 * one, hidden by the filter) stays at the top level and says so.
 */
export function taskTree(list: Task[]): TreeRow[] {
  const ids = new Set(list.map((t) => t.id));
  const kids = new Map<string, Task[]>();
  for (const t of list) {
    if (t.parent_task_id && ids.has(t.parent_task_id)) {
      kids.set(t.parent_task_id, [...(kids.get(t.parent_task_id) ?? []), t]);
    }
  }
  const out: TreeRow[] = [];
  const seen = new Set<string>();
  const walk = (t: Task, depth: number, last: boolean, orphan: boolean) => {
    if (seen.has(t.id)) return;
    seen.add(t.id);
    const children = kids.get(t.id) ?? [];
    out.push({ task: t, depth, last, orphan, children: children.length });
    children.forEach((c, i) => walk(c, depth + 1, i === children.length - 1, false));
  };
  const roots = list.filter((t) => !t.parent_task_id || !ids.has(t.parent_task_id));
  roots.forEach((t) => walk(t, 0, false, Boolean(t.parent_task_id)));
  return out;
}
