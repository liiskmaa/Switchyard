import { memo } from "react";
import type React from "react";

import { Badge, type Tone } from "../../components/ui";
import { providerName } from "../../lib/agents";
import { formatClock, formatTimestamp, toolSummary, turnCostText, usageText } from "../../lib/format";
import type { EventEnvelope } from "../../types/protocol";

const str = (p: Record<string, unknown>, k: string): string => (typeof p[k] === "string" ? (p[k] as string) : "");

const Item = memo(function Item({ e, agent }: { e: EventEnvelope; agent: string }) {
  const p = e.payload;
  const time = (
    <time className="tl-time" title={formatTimestamp(e.occurred_at_ms)}>
      {formatClock(e.occurred_at_ms)}
    </time>
  );
  const line = (tone: Tone, label: string, body: React.ReactNode) => (
    <li className={`tl tl-${tone}`}>
      {time}
      <Badge tone={tone}>{label}</Badge>
      <div className="tl-body">{body}</div>
    </li>
  );
  switch (e.event_type) {
    case "agent.turn_started":
      return line("accent", p["queued"] === true ? "queued" : "you", <p className="tl-prompt">{str(p, "prompt")}</p>);
    case "agent.message":
      return line("neutral", agent, <p className="tl-message">{str(p, "text")}</p>);
    case "agent.tool_started":
      return line(
        "neutral",
        str(p, "tool_name") || "tool",
        <code className="tl-tool">{toolSummary(str(p, "tool_name"), p["input"]) || JSON.stringify(p["input"]).slice(0, 200)}</code>,
      );
    case "agent.tool_completed":
      return line(
        "ok",
        "result",
        <details>
          <summary>{str(p, "tool_name")} succeeded</summary>
          <pre className="code-block">{str(p, "output")}</pre>
        </details>,
      );
    case "agent.tool_failed":
      return line(
        p["rejected"] === true ? "warn" : "error",
        p["rejected"] === true ? "not run" : "failed",
        <span>
          {str(p, "tool_name")}: {str(p, "output")}
        </span>,
      );
    case "agent.permission_requested":
      return line(
        "warn",
        "permission",
        <span>
          {str(p, "tool_name")} needs your approval
          {str(p, "explanation") ? <span className="muted"> · {str(p, "explanation")}</span> : null}
        </span>,
      );
    case "agent.permission_evaluated": {
      // `ask` is shown by the approval request that follows.
      const decision = str(p, "decision");
      if (decision === "ask") return null;
      return line(
        decision === "allow" ? "ok" : "error",
        decision === "allow" ? "auto-allowed" : "blocked",
        <span>
          {str(p, "tool_name")}
          <span className="muted"> · {str(p, "explanation")}</span>
        </span>,
      );
    }
    case "agent.permission_resolved":
      return line(
        str(p, "status") === "approved" ? "ok" : "error",
        str(p, "status"),
        <span>
          {str(p, "tool_name")} · {str(p, "source")}
          {str(p, "decided_by") ? ` by ${str(p, "decided_by")}` : ""}
        </span>,
      );
    case "agent.turn_completed":
      return line(
        p["interrupted"] === true ? "warn" : p["success"] === true ? "ok" : "error",
        "turn",
        <span className="muted">
          {p["interrupted"] === true ? "interrupted" : p["success"] === true ? "completed" : `error: ${str(p, "reason")}`}
          {typeof p["duration_ms"] === "number" ? ` · ${(p["duration_ms"] / 1000).toFixed(1)}s` : ""}
          {turnCostText(p) ? ` · ${turnCostText(p)}` : ""}
          {usageText(p) ? ` · ${usageText(p)}` : ""}
        </span>,
      );
    case "files.changed": {
      const changes = Array.isArray(p["changes"]) ? (p["changes"] as { path: string; kind: string; from?: string }[]) : [];
      return line(
        "accent",
        "files",
        <ul className="file-changes">
          {changes.map((c) => (
            <li key={`${c.kind}:${c.path}`}>
              <span className={`fc fc-${c.kind}`}>{c.kind}</span> <code>{c.path}</code>
              {c.from ? <span className="muted"> (from {c.from})</span> : null}
            </li>
          ))}
          {typeof p["dropped"] === "number" && p["dropped"] > 0 ? <li className="muted">and {p["dropped"]} more</li> : null}
        </ul>,
      );
    }
    case "agent.failed":
      return line("error", "failed", <span>{str(p, "detail") || str(p, "reason")}</span>);
    case "agent.warning":
      return line("warn", "warning", <span>{str(p, "message")}</span>);
    case "agent.exited":
      return line("neutral", "exited", <span className="muted">→ {str(p, "final_state")}</span>);
    case "agent.started":
      return line("neutral", p["resumed"] === true ? "resumed" : "started", <span className="muted">pid {String(p["pid"])} · {providerName(str(p, "provider") || "claude_code")} {str(p, "cli_version")}</span>);
    case "agent.interrupted":
      return line("warn", "interrupt", <span className="muted">acknowledged by the agent</span>);
    default:
      return null; // state changes, ready etc. are reflected in the header
  }
});

/** Memoized: live output re-renders the session view many times a second. */
export const SessionTimeline = memo(function SessionTimeline({ events, agent }: { events: EventEnvelope[]; agent: string }) {
  return (
    <ol className="timeline">
      {events.map((e) => (
        <Item key={e.seq} e={e} agent={agent} />
      ))}
    </ol>
  );
});
