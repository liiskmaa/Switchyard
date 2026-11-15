import { useState } from "react";

import { Badge, Button, ErrorNotice } from "../../components/ui";
import { denialReasonReachesAgent, providerName } from "../../lib/agents";
import { toCoreError, type CoreRequestError } from "../../lib/core";
import { formatRelative } from "../../lib/format";
import type { Approval } from "../../types/protocol";
import { decideApproval } from "../agents/api";
import { approvalIsStale, otherInputFields, requestAction, shortPath } from "./decision";
import { CATEGORY_LABEL, ruleRefLabel } from "../policies/labels";

function field(input: unknown, key: string): string | undefined {
  if (typeof input !== "object" || input === null) return undefined;
  const v = (input as Record<string, unknown>)[key];
  return typeof v === "string" ? v : undefined;
}

/** Renders the parts of a tool input an operator needs to judge it. */
function OtherFields({ input, rendered = [] }: { input: unknown; rendered?: string[] }) {
  const extra = otherInputFields(input, rendered);
  if (extra.length === 0) return null;
  return (
    <dl className="tool-extra small">
      {extra.map(([k, v]) => (
        <div key={k}>
          <dt className="mono">{k}</dt>
          <dd className="mono">{v}</dd>
        </div>
      ))}
    </dl>
  );
}

interface PatchChange {
  path: string;
  kind: string;
  diff?: string;
  move_path?: string;
}

/** Codex patch changes, if the input carries a well-formed list. */
function patchChanges(input: unknown): PatchChange[] | null {
  if (typeof input !== "object" || input === null) return null;
  const changes = (input as Record<string, unknown>)["changes"];
  if (!Array.isArray(changes)) return null;
  const ok = changes.every(
    (c) => typeof c === "object" && c !== null && typeof (c as PatchChange).path === "string" && typeof (c as PatchChange).kind === "string",
  );
  return ok ? (changes as PatchChange[]) : null;
}

const CHANGE_LABEL: Record<string, string> = { add: "new file", update: "edit", delete: "delete" };

export function ToolInput({ tool, input }: { tool: string; input: unknown }) {
  // The remaining raw fields are under "Details" (TechnicalDetails).
  const patch = tool === "apply_patch" ? patchChanges(input) : null;
  return patch ? <PatchInput changes={patch} /> : <MainInput tool={tool} input={input} />;
}

function PatchInput({ changes }: { changes: PatchChange[] }) {
  return (
    <ul className="patch-changes">
      {changes.map((c, i) => (
        <li key={`${c.path}-${i}`} className="tool-input">
          <span>
            <Badge tone={c.kind === "delete" ? "error" : c.kind === "add" ? "ok" : "neutral"}>{CHANGE_LABEL[c.kind] ?? c.kind}</Badge>{" "}
            <code className="path" title={c.path}>
              {shortPath(c.path)}
            </code>
            {c.move_path ? (
              <>
                {" → "}
                <code className="path">{c.move_path}</code>
              </>
            ) : null}
          </span>
          {c.diff ? <pre className="code-block">{c.diff.length > 4000 ? `${c.diff.slice(0, 4000)}\n…` : c.diff}</pre> : null}
        </li>
      ))}
    </ul>
  );
}

function MainInput({ tool, input }: { tool: string; input: unknown }) {
  const command = field(input, "command");
  const path = field(input, "file_path") ?? field(input, "notebook_path");
  const content = field(input, "content");
  const oldString = field(input, "old_string");
  const newString = field(input, "new_string");
  if ((tool === "Bash" || tool === "shell") && command) {
    return <pre className="code-block">$ {command}</pre>;
  }
  if (path) {
    return (
      <div className="tool-input">
        <code className="path" title={path}>
          {shortPath(path)}
        </code>
        {oldString !== undefined ? (
          <>
            <pre className="code-block diff-old">{oldString}</pre>
            <pre className="code-block diff-new">{newString ?? ""}</pre>
          </>
        ) : content !== undefined ? (
          <pre className="code-block">{content.length > 4000 ? `${content.slice(0, 4000)}\n…` : content}</pre>
        ) : null}
      </div>
    );
  }
  return <pre className="code-block">{JSON.stringify(input, null, 2)}</pre>;
}


/** What approving reaches, as Core established it. Never implies a workspace limit that is not real. */
function ScopeNotice({ approval }: { approval: Approval }) {
  const scope = approval.policy?.scope;
  if (!scope) {
    return approval.status === "pending" ? (
      <p className="scope-note scope-unknown small">Switchyard did not record what this request would reach.</p>
    ) : null;
  }
  const label: Record<string, string> = {
    workspace: "Inside the workspace",
    outside_workspace: "Outside the workspace",
    unknown: "Scope unknown",
    unrestricted: "Not limited to the workspace",
  };
  const paths = [...scope.outside_paths, ...scope.unresolved_paths];
  return (
    <div className={`scope-note scope-${scope.kind} small`} role={scope.kind === "workspace" ? undefined : "note"}>
      <strong>{label[scope.kind] ?? scope.kind}.</strong> {scope.note}
      {paths.length > 0 ? (
        <ul>
          {paths.map((p) => (
            <li key={p}>
              <code className="path">{p}</code>
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}

export function ApprovalCard({
  approval,
  projectName,
  context,
  onDecided,
}: {
  approval: Approval;
  projectName?: string | undefined;
  /** What the request is for, in the user's words (usually the task title). */
  context?: string | undefined;
  onDecided?: () => void;
}) {
  const [busy, setBusy] = useState<"approve" | "deny" | null>(null);
  const [error, setError] = useState<CoreRequestError | null>(null);
  const [note, setNote] = useState("");
  const [noting, setNoting] = useState(false);
  // Core refused the decision because the request was already settled
  // (elsewhere, by expiry or by policy). Keep the explanation visible here and
  // stop offering a decision; reloading moves the card to the decided list.
  const [stale, setStale] = useState(false);
  const pending = approval.status === "pending";

  async function decide(decision: "approve" | "deny") {
    setBusy(decision);
    setError(null);
    try {
      // The UI shows the result only after Core confirms it.
      await decideApproval(approval.id, decision, decision === "deny" && note.trim() ? note.trim() : undefined);
      onDecided?.();
    } catch (e) {
      const err = toCoreError(e);
      setError(err);
      if (approvalIsStale(err.code)) setStale(true);
    } finally {
      setBusy(null);
    }
  }

  const expiresIn = Math.max(0, Math.round((approval.expires_at_ms - Date.now()) / 1000));
  const policy = approval.policy;
  const byPolicy = approval.decision_source === "policy";
  const agent = providerName(approval.provider);
  const action = requestAction(approval.tool_name, approval.input, policy ? CATEGORY_LABEL[policy.category] : undefined);
  const where = context ?? projectName;
  const outside = policy?.scope && policy.scope.kind !== "workspace";
  const outcome = approval.status === "approved" ? "approved" : byPolicy ? "denied by policy" : approval.status;

  if (!pending) {
    // A past decision: one line, details on request.
    return (
      <details className="approval approval-past">
        <summary>
          <span className="approval-line">
            {agent} asked to {action}
            {where ? <span className="muted"> · {where}</span> : null}
          </span>
          <Badge tone={approval.status === "approved" ? "ok" : approval.status === "denied" ? "error" : "neutral"}>{outcome}</Badge>
          <span className="muted small">{formatRelative(approval.requested_at_ms, Date.now())}</span>
        </summary>
        <div className="approval-details">
          {approval.decision_note ? (
            <p className="small">
              {byPolicy ? "Why" : "Note"}: {approval.decision_note}
            </p>
          ) : null}
          <ToolInput tool={approval.tool_name} input={approval.input} />
          <TechnicalDetails approval={approval} />
        </div>
      </details>
    );
  }

  return (
    <div className="approval approval-pending">
      <div className="approval-headline">
        <p className="approval-title">
          {agent} wants to {action}
        </p>
        <p className="muted small">
          {where ? `For ${where} · ` : ""}asked {formatRelative(approval.requested_at_ms, Date.now())}
          {stale ? " · no longer waiting" : ` · denied automatically in ${Math.ceil(expiresIn / 60)} min`}
        </p>
      </div>
      {outside ? <ScopeNotice approval={approval} /> : null}
      {approval.description ? <p className="small">{approval.description}</p> : null}
      <ToolInput tool={approval.tool_name} input={approval.input} />
      {error ? <ErrorNotice title="Decision not recorded" message={error.message} code={error.code} /> : null}
      {stale ? (
        <div className="approval-actions">
          <span className="muted small">This request is no longer waiting for a decision.</span>
          <Button onClick={() => onDecided?.()}>Reload</Button>
        </div>
      ) : (
        <>
          {noting ? (
            <input
              className="input"
              aria-label="Reason for denying"
              placeholder={
                denialReasonReachesAgent(approval.provider)
                  ? `Reason shown to ${agent} if you deny (optional)`
                  : `Note kept with the denial (${agent} is not told the reason)`
              }
              value={note}
              autoFocus
              onChange={(e) => setNote(e.target.value)}
              disabled={busy !== null}
            />
          ) : null}
          <div className="approval-actions">
            <TechnicalDetails approval={approval} />
            {!noting ? (
              <button type="button" className="link small" onClick={() => setNoting(true)}>
                Add a reason
              </button>
            ) : null}
            <Button variant="danger" onClick={() => void decide("deny")} disabled={busy !== null}>
              {busy === "deny" ? "Denying…" : "Deny"}
            </Button>
            <Button variant="primary" onClick={() => void decide("approve")} disabled={busy !== null}>
              {busy === "approve" ? "Approving…" : "Approve"}
            </Button>
          </div>
        </>
      )}
    </div>
  );
}

/** Everything Core recorded about the request, for those who want it. */
function TechnicalDetails({ approval }: { approval: Approval }) {
  const policy = approval.policy;
  return (
    <details className="approval-tech small">
      <summary>Details</summary>
      <dl className="kv">
        <div className="kv-row">
          <dt>Tool</dt>
          <dd className="mono">{approval.tool_name}</dd>
        </div>
        <div className="kv-row">
          <dt>Why it asks</dt>
          <dd>
            {policy ? policy.explanation : "Recorded before approval policies existed."}
            {policy?.deciding_rule ? ` Matching rule: ${ruleRefLabel(policy.deciding_rule)}.` : ""}
          </dd>
        </div>
        {policy?.scope ? (
          <div className="kv-row">
            <dt>Reaches</dt>
            <dd>{policy.scope.note}</dd>
          </div>
        ) : null}
        <div className="kv-row">
          <dt>Session</dt>
          <dd className="mono">
            {approval.session_id} · {approval.workspace_name ?? ""}
          </dd>
        </div>
      </dl>
      <OtherFields input={approval.input} rendered={["changes", "command", "file_path", "notebook_path", "content", "old_string", "new_string"]} />
    </details>
  );
}
