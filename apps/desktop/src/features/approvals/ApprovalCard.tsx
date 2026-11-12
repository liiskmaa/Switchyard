import { useState } from "react";

import { Badge, Button, ErrorNotice } from "../../components/ui";
import { denialReasonReachesAgent, providerName } from "../../lib/agents";
import { toCoreError, type CoreRequestError } from "../../lib/core";
import { formatRelative } from "../../lib/format";
import type { Approval } from "../../types/protocol";
import { decideApproval } from "../agents/api";
import { approvalIsStale, otherInputFields } from "./decision";
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
  // The JSON fallback already shows every field.
  const patch = tool === "apply_patch" ? patchChanges(input) : null;
  const special =
    ((tool === "Bash" || tool === "shell") && field(input, "command") !== undefined) ||
    field(input, "file_path") !== undefined ||
    field(input, "notebook_path") !== undefined ||
    patch !== null;
  return (
    <>
      {patch ? <PatchInput changes={patch} /> : <MainInput tool={tool} input={input} />}
      {special ? <OtherFields input={input} rendered={patch ? ["changes"] : []} /> : null}
    </>
  );
}

function PatchInput({ changes }: { changes: PatchChange[] }) {
  return (
    <ul className="patch-changes">
      {changes.map((c, i) => (
        <li key={`${c.path}-${i}`} className="tool-input">
          <span>
            <Badge tone={c.kind === "delete" ? "error" : c.kind === "add" ? "ok" : "neutral"}>{CHANGE_LABEL[c.kind] ?? c.kind}</Badge>{" "}
            <code className="path">{c.path}</code>
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
        <code className="path">{path}</code>
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


export function ApprovalCard({
  approval,
  projectName,
  onDecided,
}: {
  approval: Approval;
  projectName?: string | undefined;
  onDecided?: () => void;
}) {
  const [busy, setBusy] = useState<"approve" | "deny" | null>(null);
  const [error, setError] = useState<CoreRequestError | null>(null);
  const [note, setNote] = useState("");
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
  return (
    <div className={`approval${pending ? " approval-pending" : ""}`}>
      <div className="approval-head">
        <Badge tone="accent">{approval.tool_name}</Badge>
        {policy ? <span className="small">{CATEGORY_LABEL[policy.category]}</span> : null}
        <span className="muted small">
          {projectName ? `${projectName} · ` : ""}session {approval.session_id.slice(-6)} · {providerName(approval.provider)} · requested{" "}
          {formatRelative(approval.requested_at_ms, Date.now())}
        </span>
        {pending && stale ? (
          <Badge>no longer waiting</Badge>
        ) : pending ? (
          <span className="small approval-expiry">
            <strong>Awaiting your decision</strong>
            <span className="muted"> · auto-denied in {Math.ceil(expiresIn / 60)} min</span>
          </span>
        ) : (
          <Badge tone={approval.status === "approved" ? "ok" : approval.status === "denied" ? "error" : "neutral"}>
            {byPolicy ? "denied by policy" : approval.status}
            {approval.decision_source && approval.decision_source !== "operator" && !byPolicy ? ` · ${approval.decision_source}` : ""}
          </Badge>
        )}
      </div>
      {policy ? (
        <div className="approval-policy small">
          <p>
            <span className="muted">{pending ? "Policy result: ask every time." : "Policy result when requested: ask every time."}</span>{" "}
            {policy.explanation}
          </p>
          {policy.deciding_rule ? <p className="muted">Matching rule: {ruleRefLabel(policy.deciding_rule)}</p> : null}
        </div>
      ) : (
        <p className="muted small">Recorded before approval policies existed.</p>
      )}
      {approval.description ? <p className="small">{approval.description}</p> : null}
      <ToolInput tool={approval.tool_name} input={approval.input} />
      {error ? <ErrorNotice title="Decision not recorded" message={error.message} code={error.code} /> : null}
      {pending && stale ? (
        <div className="approval-actions">
          <span className="muted small">This request is no longer waiting for a decision.</span>
          <Button onClick={() => onDecided?.()}>Reload approvals</Button>
        </div>
      ) : pending ? (
        <div className="approval-actions">
          <input
            className="input"
            placeholder={
              denialReasonReachesAgent(approval.provider)
                ? "Reason shown to the agent if denied (optional)"
                : `Note kept with a denial (${providerName(approval.provider)} is not told the reason)`
            }
            value={note}
            onChange={(e) => setNote(e.target.value)}
            disabled={busy !== null}
          />
          <Button variant="danger" onClick={() => void decide("deny")} disabled={busy !== null}>
            {busy === "deny" ? "Denying…" : "Deny"}
          </Button>
          <Button variant="primary" onClick={() => void decide("approve")} disabled={busy !== null}>
            {busy === "approve" ? "Approving…" : "Approve"}
          </Button>
        </div>
      ) : approval.decision_note ? (
        <p className="muted small">{byPolicy ? "Why" : "Note"}: {approval.decision_note}</p>
      ) : null}
    </div>
  );
}
