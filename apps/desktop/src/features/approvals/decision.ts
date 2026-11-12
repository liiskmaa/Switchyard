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
export function otherInputFields(input: unknown): [string, string][] {
  if (typeof input !== "object" || input === null || Array.isArray(input)) return [];
  return Object.entries(input as Record<string, unknown>)
    .filter(([k]) => !RENDERED.includes(k))
    .map(([k, v]) => [k, typeof v === "string" ? v : JSON.stringify(v)]);
}
