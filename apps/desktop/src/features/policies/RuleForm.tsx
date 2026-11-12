import { useState } from "react";

import { Button, ErrorNotice } from "../../components/ui";
import { toCoreError, type CoreRequestError } from "../../lib/core";
import type { OperationCategory, PolicyCategoryInfo, PolicyConditions, PolicyDecision, PolicyRule, Workspace } from "../../types/protocol";
import { WarnNotice } from "../workspaces/WorkspacesPanel";
import { createRule, updateRule } from "./api";
import { CATEGORY_LABEL, DECISION_LABEL, categoryNote, commandProblem } from "./labels";

const DECISIONS: PolicyDecision[] = ["allow", "ask", "deny"];

/**
 * Creates a rule, or edits one (`rule` set). Scope and operation are fixed
 * once a rule exists; Core validates everything and has the final word.
 */
export function RuleForm({
  projectId,
  categories,
  workspaces,
  rule,
  defaultWorkspaceId,
  onDone,
  onCancel,
}: {
  projectId: string;
  categories: PolicyCategoryInfo[];
  workspaces: Workspace[];
  rule?: PolicyRule;
  defaultWorkspaceId?: string | undefined;
  onDone: () => void;
  onCancel: () => void;
}) {
  const configurable = categories.filter((c) => c.configurable);
  const [category, setCategory] = useState<OperationCategory>(rule?.category ?? configurable[0]?.category ?? "file_read");
  const [workspaceId, setWorkspaceId] = useState<string>(rule?.workspace_id ?? defaultWorkspaceId ?? "");
  const [decision, setDecision] = useState<PolicyDecision>(rule?.decision ?? "ask");
  const [path, setPath] = useState(rule?.conditions.path_prefix ?? "");
  const [command, setCommand] = useState(rule?.conditions.command ?? "");
  const [description, setDescription] = useState(rule?.description ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<CoreRequestError | null>(null);

  const info = categories.find((c) => c.category === category);
  const allowBlocked = info !== undefined && !info.allow_supported;
  const shellAllow = category === "shell" && decision === "allow";
  const commandIssue = info?.command_condition && (command !== "" || shellAllow) ? commandProblem(command) : null;
  const note = categoryNote(category);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    const conditions: PolicyConditions = {};
    if (info?.path_condition && path.trim() !== "") conditions.path_prefix = path.trim();
    if (info?.command_condition && command !== "") conditions.command = command;
    try {
      if (rule) {
        await updateRule(rule, { decision, conditions, description });
      } else {
        await createRule({
          project_id: projectId,
          ...(workspaceId ? { workspace_id: workspaceId } : {}),
          category,
          decision,
          conditions,
          description,
        });
      }
      onDone();
    } catch (err) {
      setError(toCoreError(err));
    } finally {
      setBusy(false);
    }
  }

  const usable = workspaces.filter((w) => w.kind === "worktree" && w.status !== "removed");
  return (
    <form className="add-project rule-form" onSubmit={(e) => void submit(e)}>
      <div className="rule-form-grid">
        <label htmlFor="rule-scope">Applies to</label>
        <select
          id="rule-scope"
          className="input select"
          value={workspaceId}
          onChange={(e) => setWorkspaceId(e.target.value)}
          disabled={busy || rule !== undefined}
        >
          <option value="">Every workspace of this project</option>
          {usable.map((w) => (
            <option key={w.id} value={w.id}>
              Only workspace “{w.name}”
            </option>
          ))}
        </select>
        <label htmlFor="rule-category">Operation</label>
        <select
          id="rule-category"
          className="input select"
          value={category}
          onChange={(e) => setCategory(e.target.value as OperationCategory)}
          disabled={busy || rule !== undefined}
        >
          {configurable.map((c) => (
            <option key={c.category} value={c.category}>
              {CATEGORY_LABEL[c.category]}
            </option>
          ))}
        </select>
        <span className="rule-form-label">Decision</span>
        <div className="segmented" role="radiogroup" aria-label="Decision">
          {DECISIONS.map((d) => (
            <label key={d} className={`segment${decision === d ? " active" : ""}${d === "allow" && allowBlocked ? " disabled" : ""}`}>
              <input
                type="radio"
                name="decision"
                value={d}
                checked={decision === d}
                disabled={busy || (d === "allow" && allowBlocked)}
                onChange={() => setDecision(d)}
              />
              {DECISION_LABEL[d]}
            </label>
          ))}
        </div>
        {info?.path_condition ? (
          <>
            <label htmlFor="rule-path">Only under (optional)</label>
            <input
              id="rule-path"
              className="input mono"
              placeholder="e.g. docs or src/generated — relative to the workspace"
              value={path}
              onChange={(e) => setPath(e.target.value)}
              spellCheck={false}
              disabled={busy}
            />
          </>
        ) : null}
        {info?.command_condition ? (
          <>
            <label htmlFor="rule-command">{shellAllow ? "Exact command" : "Exact command (optional)"}</label>
            <input
              id="rule-command"
              className="input mono"
              placeholder="e.g. cargo test"
              value={command}
              onChange={(e) => setCommand(e.target.value)}
              spellCheck={false}
              disabled={busy}
            />
          </>
        ) : null}
        <label htmlFor="rule-description">Description</label>
        <input
          id="rule-description"
          className="input"
          placeholder="Why this rule exists"
          value={description}
          maxLength={500}
          onChange={(e) => setDescription(e.target.value)}
          disabled={busy}
        />
      </div>
      {note ? <p className="muted small">{note}</p> : null}
      {allowBlocked ? <p className="muted small">{CATEGORY_LABEL[category]} can only be set to ask or block.</p> : null}
      {commandIssue ? <p className="small text-warn">{commandIssue}</p> : null}
      {shellAllow ? (
        <WarnNotice title="This command will run without asking">
          Only this exact text matches; anything else (extra arguments, a second command) still asks. The command runs with your
          user&apos;s permissions and can change files outside the workspace, and it runs whatever code it is given (for example
          build scripts and tests the agent edited).
        </WarnNotice>
      ) : decision === "allow" && info?.path_condition ? (
        <p className="muted small">
          Allowed only inside the session&apos;s workspace; anything outside it, Git metadata (.git) or an unrecognized request
          still asks.
        </p>
      ) : null}
      {error ? <ErrorNotice title="Rule not saved" message={error.message} code={error.code} /> : null}
      <div className="form-actions">
        <Button variant="ghost" onClick={onCancel} disabled={busy}>
          Cancel
        </Button>
        <Button type="submit" variant="primary" disabled={busy || description.trim() === "" || commandIssue !== null}>
          {busy ? "Saving…" : rule ? "Save changes" : "Add rule"}
        </Button>
      </div>
    </form>
  );
}
