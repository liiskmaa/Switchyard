/**
 * Decision errors after which the approval shown is out of date: it was
 * settled elsewhere (another window, expiry, a policy change) or the current
 * policy denied it while approving. The view must reload it instead of still
 * offering Approve/Deny.
 */
export function approvalIsStale(code: string): boolean {
  return code === "conflict" || code === "policy_denied" || code === "not_found";
}

/** Input fields `ToolInput` renders specially for each kind of input. */
const RENDERED = ["command", "description", "file_path", "notebook_path", "content", "old_string", "new_string"];

/**
 * Every other input field, so a person deciding sees flags such as
 * `run_in_background` or `dangerouslyDisableSandbox` and not only the command.
 */
export function otherInputFields(input: unknown, alsoRendered: string[] = []): [string, string][] {
  if (typeof input !== "object" || input === null || Array.isArray(input)) return [];
  return Object.entries(input as Record<string, unknown>)
    .filter(([k]) => !RENDERED.includes(k) && !alsoRendered.includes(k))
    .map(([k, v]) => [k, typeof v === "string" ? v : JSON.stringify(v)]);
}

/** A path as the user knows it: relative to the task's workspace when it is inside one. */
export function shortPath(path: string): string {
  const m = /\/workspaces\/[^/]+\/[^/]+\/(.+)$/.exec(path);
  if (m?.[1]) return m[1];
  const parts = path.split("/").filter(Boolean);
  return parts.length > 2 ? `…/${parts.slice(-2).join("/")}` : path;
}

function str(input: unknown, key: string): string | undefined {
  if (typeof input !== "object" || input === null) return undefined;
  const v = (input as Record<string, unknown>)[key];
  return typeof v === "string" ? v : undefined;
}

const PATCH_VERB: Record<string, string> = { add: "create", update: "edit", delete: "delete" };

/**
 * One plain sentence for a permission request, e.g. "create greeter.py" or
 * "run `npm test`". The agent's name is added by the caller.
 */
export function requestAction(tool: string, input: unknown, category?: string): string {
  if (tool === "apply_patch" && typeof input === "object" && input !== null) {
    const changes = (input as { changes?: unknown }).changes;
    if (Array.isArray(changes) && changes.length > 0) {
      const first = changes[0] as { path?: unknown; kind?: unknown };
      const verb = PATCH_VERB[String(first.kind)] ?? "change";
      const target = typeof first.path === "string" ? shortPath(first.path) : "files";
      return changes.length === 1 ? `${verb} ${target}` : `${verb} ${target} and ${changes.length - 1} more file(s)`;
    }
  }
  const command = str(input, "command");
  if ((tool === "Bash" || tool === "shell") && command) {
    const line = command.length > 60 ? `${command.slice(0, 60)}…` : command;
    return `run \`${line}\``;
  }
  const path = str(input, "file_path") ?? str(input, "notebook_path");
  if (path) {
    const verb = tool === "Write" ? "write" : tool === "Read" ? "read" : tool === "NotebookEdit" ? "edit the notebook" : "edit";
    return `${verb} ${shortPath(path)}`;
  }
  if (category) return category.charAt(0).toLowerCase() + category.slice(1);
  return `use ${tool}`;
}
