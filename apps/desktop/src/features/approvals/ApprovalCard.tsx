import { useState } from "react";

import { Badge, Button, ErrorNotice } from "../../components/ui";
import { toCoreError, type CoreRequestError } from "../../lib/core";
import { formatRelative } from "../../lib/format";
import type { Approval } from "../../types/protocol";
import { decideApproval } from "../agents/api";

function field(input: unknown, key: string): string | undefined {
  if (typeof input !== "object" || input === null) return undefined;
  const v = (input as Record<string, unknown>)[key];
  return typeof v === "string" ? v : undefined;
}

/** Renders the parts of a tool input an operator needs to judge it. */
export function ToolInput({ tool, input }: { tool: string; input: unknown }) {
  const command = field(input, "command");
  const path = field(input, "file_path") ?? field(input, "notebook_path");
  const content = field(input, "content");
  const oldString = field(input, "old_string");
  const newString = field(input, "new_string");
  if (tool === "Bash" && command) {
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
  const pending = approval.status === "pending";

  async function decide(decision: "approve" | "deny") {
    setBusy(decision);
    setError(null);
    try {
      // The UI shows the result only after Core confirms it.
      await decideApproval(approval.id, decision, decision === "deny" && note.trim() ? note.trim() : undefined);
      onDecided?.();
    } catch (e) {
      setError(toCoreError(e));
    } finally {
      setBusy(null);
    }
  }

  const expiresIn = Math.max(0, Math.round((approval.expires_at_ms - Date.now()) / 1000));
  return (
    <div className={`approval${pending ? " approval-pending" : ""}`}>
      <div className="approval-head">
        <Badge tone="accent">{approval.tool_name}</Badge>
        {projectName ? <span className="muted small">{projectName}</span> : null}
        <span className="muted small">Claude Code · requested {formatRelative(approval.requested_at_ms, Date.now())}</span>
        {pending ? (
          <span className="muted small approval-expiry">auto-denied in {Math.ceil(expiresIn / 60)} min</span>
        ) : (
          <Badge tone={approval.status === "approved" ? "ok" : approval.status === "denied" ? "error" : "neutral"}>
            {approval.status}
            {approval.decision_source && approval.decision_source !== "operator" ? ` · ${approval.decision_source}` : ""}
          </Badge>
        )}
      </div>
      {approval.description ? <p className="small">{approval.description}</p> : null}
      <ToolInput tool={approval.tool_name} input={approval.input} />
      {error ? <ErrorNotice title="Decision not recorded" message={error.message} code={error.code} /> : null}
      {pending ? (
        <div className="approval-actions">
          <input
            className="input"
            placeholder="Reason shown to the agent if denied (optional)"
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
        <p className="muted small">Note: {approval.decision_note}</p>
      ) : null}
    </div>
  );
}
