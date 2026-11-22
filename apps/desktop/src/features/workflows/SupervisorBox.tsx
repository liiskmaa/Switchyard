import { useState } from "react";

import { Badge, Button, ErrorNotice, Panel } from "../../components/ui";
import { toCoreError, type CoreRequestError } from "../../lib/core";
import { formatClock } from "../../lib/format";
import { useNow } from "../../lib/useNow";
import { navigate } from "../../stores/navigation";
import { useCoreQuery } from "../../stores/useCoreQuery";
import type { SupervisorMessage } from "../../types/protocol";
import { askSupervisor, getSupervisor, isSupervisorEvent } from "./api";
import { formatDuration } from "./flow";

const ROUTE_LABEL: Record<string, string> = {
  reasoning: "Reasoning",
  coding: "Coding",
  review: "Review",
  tests: "Tests",
};

function Message({ m }: { m: SupervisorMessage }) {
  const from = m.sender === "operator" ? "You" : m.sender === "supervisor" ? "Supervisor" : "Switchyard";
  const routed = m.route && m.route !== "none";
  const refused = m.outcome?.startsWith("Nothing was routed") ?? false;
  return (
    <li className={`supervisor-message from-${m.sender}`}>
      <span className="supervisor-meta small muted">
        {from} · {formatClock(m.created_at_ms)}
      </span>
      <div className="tl-message">{m.text}</div>
      {routed || m.outcome ? (
        <div className="supervisor-route small">
          {routed ? (
            <>
              <Badge tone={refused ? "warn" : "ok"}>→ {ROUTE_LABEL[m.route ?? ""] ?? m.route}</Badge>{" "}
            </>
          ) : null}
          {m.outcome ? <span>{m.outcome}</span> : null}
          {m.follow_up_task_id ? (
            <>
              {" "}
              <button type="button" className="link" onClick={() => navigate("tasks", { taskId: m.follow_up_task_id ?? null })}>
                Open the follow-up task
              </button>
            </>
          ) : null}
        </div>
      ) : null}
    </li>
  );
}

/**
 * Ask the task's supervisor (ADR 0041): a read-only agent that answers
 * questions about the task and can send requested work to reasoning, coding,
 * review or the tests. Switchyard checks every route against the workflow;
 * nothing here pushes, deploys or approves.
 */
export function SupervisorBox({ taskId }: { taskId: string }) {
  const q = useCoreQuery(`supervisor:${taskId}`, () => getSupervisor(taskId), {
    refreshOn: (e) => isSupervisorEvent(e) && e.task_id === taskId,
    // A missed event (reconnects) must not leave "answering…" up forever.
    intervalMs: 5_000,
  });
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<CoreRequestError | null>(null);
  const info = q.data;
  const thinking = info?.thinking ?? false;
  const now = useNow(1000);
  const messages = info?.messages ?? [];
  const asked = thinking ? messages[messages.length - 1] : undefined;

  const send = () =>
    void (async () => {
      if (text.trim() === "") return;
      setBusy(true);
      setError(null);
      try {
        await askSupervisor(taskId, text.trim());
        setText("");
        q.refresh();
      } catch (e) {
        setError(toCoreError(e));
      } finally {
        setBusy(false);
      }
    })();

  return (
    <Panel title="Ask the supervisor">
      <p className="muted small">
        Ask about this task, or ask for work: the supervisor reads the task and its changes, answers, and can send the work to
        reasoning, coding, review or the tests. Switchyard checks every request against the workflow first. It never pushes,
        deploys or approves anything.
      </p>
      {messages.length > 0 ? (
        <ol className="supervisor-thread">
          {messages.map((m) => (
            <Message key={m.id} m={m} />
          ))}
        </ol>
      ) : null}
      {thinking ? (
        <p className="small muted" role="status">
          The supervisor is reading the task and answering
          {asked ? ` (${formatDuration(now - asked.created_at_ms)})` : ""}…
        </p>
      ) : null}
      {q.error ? <ErrorNotice title="Cannot load the conversation" message={q.error.message} code={q.error.code} /> : null}
      {error ? <ErrorNotice title="Not sent" message={error.message} code={error.code} /> : null}
      <form
        className="composer"
        onSubmit={(e) => {
          e.preventDefault();
          send();
        }}
      >
        <textarea
          className="input textarea"
          aria-label="Message to the supervisor"
          rows={3}
          value={text}
          disabled={thinking}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
              e.preventDefault();
              send();
            }
          }}
          placeholder={
            thinking ? "Wait for the answer…" : "e.g. “Why did the tests fail?” or “Have review check the error handling again.”"
          }
        />
        <div className="composer-actions">
          <span className="muted small">⌘↵ to send</span>
          <Button type="submit" variant="primary" disabled={busy || thinking || text.trim() === ""}>
            Ask
          </Button>
        </div>
      </form>
    </Panel>
  );
}
