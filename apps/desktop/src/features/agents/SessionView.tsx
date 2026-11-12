import { type FormEvent, useCallback, useEffect, useMemo, useState } from "react";

import { Badge, Button, EmptyState, ErrorNotice, KeyValue, Panel } from "../../components/ui";
import { toCoreError, type CoreRequestError } from "../../lib/core";
import { formatTimestamp, mergeEvents } from "../../lib/format";
import { liveStore, useCoreEvents } from "../../stores/core";
import { useStore } from "../../stores/store";
import { useCoreQuery } from "../../stores/useCoreQuery";
import { TERMINAL_STATES, type EventEnvelope, type Session } from "../../types/protocol";
import { ApprovalCard } from "../approvals/ApprovalCard";
import { WORKSPACE_STATUS_LABEL, getWorkspace, workspaceTone } from "../workspaces/api";
import {
  STATE_LABEL,
  getSession,
  interruptSession,
  pendingApprovals,
  resumeSession,
  sendMessage,
  sessionEvents,
  stateTone,
  stopSession,
} from "./api";
import { SessionTimeline } from "./SessionTimeline";

export function SessionView({ sessionId, projectName }: { sessionId: string; projectName?: string | undefined }) {
  const forSession = useCallback((e: EventEnvelope) => e.session_id === sessionId, [sessionId]);
  const detail = useCoreQuery(`session:${sessionId}`, () => getSession(sessionId), { refreshOn: forSession });
  const approvals = useCoreQuery(`approvals:${sessionId}`, () => pendingApprovals(sessionId), { refreshOn: forSession });
  const history = useCoreQuery(`events:${sessionId}`, () => sessionEvents(sessionId));
  const [events, setEvents] = useState<EventEnvelope[]>([]);
  useEffect(() => setEvents(history.data ?? []), [history.data]);
  useCoreEvents(
    useCallback((e: EventEnvelope) => {
      if (e.session_id === sessionId) setEvents((cur) => mergeEvents(cur, [e], 2000));
    }, [sessionId]),
  );

  const live = useStore(liveStore)[sessionId];
  const session = detail.data?.session;
  // Prefer the store; fall back to Core's snapshot after (re)connecting.
  const liveText = live?.text || detail.data?.live_text || "";
  const visible = useMemo(() => events, [events]);

  if (detail.error) return <ErrorNotice title="Cannot load session" message={detail.error.message} code={detail.error.code} />;
  if (!session) return <p className="muted">Loading session…</p>;

  const running = session.state === "running" || session.state === "waiting_for_approval";
  return (
    <div className="session">
      <SessionHeader session={session} projectName={projectName} onChanged={detail.refresh} />
      {(approvals.data ?? []).map((a) => (
        <ApprovalCard
          key={a.id}
          approval={a}
          projectName={[projectName, a.workspace_name].filter(Boolean).join(" · ") || undefined}
          onDecided={approvals.refresh}
        />
      ))}
      <Panel title="Activity">
        {visible.length === 0 ? (
          <EmptyState icon="activity" title="No activity yet">Send a task to start the first turn.</EmptyState>
        ) : (
          <SessionTimeline events={visible} />
        )}
        {running && (liveText || live?.thinking) ? (
          <div className="live">
            <span className="live-label">
              <span className="dot dot-ok" /> streaming{live?.thinking && !liveText ? " · thinking…" : ""}
            </span>
            <p className="tl-message">{liveText}</p>
          </div>
        ) : null}
        {live?.gap && running ? (
          <p className="muted small">Some streamed output was missed while disconnected; complete messages appear above when each finishes.</p>
        ) : null}
      </Panel>
      <Composer session={session} />
    </div>
  );
}

function SessionHeader({ session, projectName, onChanged }: { session: Session; projectName?: string | undefined; onChanged: () => void }) {
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<CoreRequestError | null>(null);
  const workspaceId = session.workspace_id;
  const forWorkspace = useCallback(
    (e: EventEnvelope) => workspaceId !== null && e.workspace_id === workspaceId,
    [workspaceId],
  );
  const workspace = useCoreQuery(
    `session-workspace:${workspaceId ?? "none"}`,
    async () => (workspaceId ? (await getWorkspace(workspaceId)).workspace : null),
    { refreshOn: forWorkspace, enabled: workspaceId !== null },
  );
  const ws = workspace.data;
  const legacy = workspaceId === null || ws?.kind === "legacy_checkout";
  const terminal = TERMINAL_STATES.includes(session.state);
  const running = session.state === "running" || session.state === "waiting_for_approval";

  async function act(name: string, op: () => Promise<unknown>) {
    setBusy(name);
    setError(null);
    try {
      await op();
      onChanged();
    } catch (e) {
      setError(toCoreError(e));
    } finally {
      setBusy(null);
    }
  }

  return (
    <Panel
      title={`Claude Code${projectName ? ` · ${projectName}` : ""}`}
      actions={
        <div className="page-actions">
          {session.attached ? (
            <>
              <Button
                variant="ghost"
                onClick={() => void act("interrupt", () => interruptSession(session.id))}
                disabled={!running || busy !== null}
                title={running ? "End the current turn; the session stays open" : "Nothing is running"}
              >
                {busy === "interrupt" ? "Interrupting…" : "Interrupt"}
              </Button>
              <Button variant="danger" onClick={() => void act("stop", () => stopSession(session.id))} disabled={busy !== null}>
                {busy === "stop" ? "Stopping…" : "Stop session"}
              </Button>
            </>
          ) : terminal && !legacy && session.native_session_id && session.capabilities.resume_session ? (
            <Button variant="primary" onClick={() => void act("resume", () => resumeSession(session.id))} disabled={busy !== null}>
              {busy === "resume" ? "Resuming…" : "Resume session"}
            </Button>
          ) : null}
        </div>
      }
    >
      <KeyValue
        rows={[
          ["State", <Badge tone={stateTone(session.state)}>{STATE_LABEL[session.state]}</Badge>],
          ["Process", session.attached && session.pid ? `pid ${session.pid}` : <span className="muted">not running</span>],
          [
            "Workspace",
            ws ? (
              <span>
                {ws.name} <Badge tone={workspaceTone(ws.status)}>{WORKSPACE_STATUS_LABEL[ws.status]}</Badge>
              </span>
            ) : (
              <span className="muted">{workspaceId ? "loading…" : "none"}</span>
            ),
          ],
          ["Branch", ws?.branch ? <code>{ws.branch}</code> : <span className="muted">{legacy ? "project checkout" : "—"}</span>],
          ["Working directory", <code className="path">{session.cwd}</code>],
          ["Claude session", <code>{session.native_session_id ?? "—"}</code>],
          ["Switchyard session", <code>{session.id}</code>],
          ["Model", session.model ?? <span className="muted">CLI default</span>],
          [
            "Started",
            `${formatTimestamp(session.started_at_ms)}${session.ended_at_ms ? ` · ended ${formatTimestamp(session.ended_at_ms)}` : ""}${session.resume_count > 0 ? ` · resumed ${session.resume_count}×` : ""}`,
          ],
        ]}
      />
      {legacy && terminal ? (
        <p className="muted small">
          This session ran in the project checkout before workspace isolation. It is kept as history and cannot be
          resumed; start a new session in a workspace instead.
        </p>
      ) : null}
      {session.failure && session.state === "failed" ? (
        <ErrorNotice title="Session failed" message={session.failure} />
      ) : session.failure ? (
        <p className="muted small">Ended: {session.failure}</p>
      ) : null}
      {error ? <ErrorNotice title={`Could not ${busy ?? "complete the action"}`} message={error.message} code={error.code} /> : null}
    </Panel>
  );
}

function Composer({ session }: { session: Session }) {
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<CoreRequestError | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const running = session.state === "running" || session.state === "waiting_for_approval";
  const ready = session.attached && (session.state === "idle" || session.state === "interrupted");
  const canQueue = session.attached && running && session.capabilities.queue_message_during_turn;

  async function submit(queue: boolean) {
    if (!text.trim()) return;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const { delivery } = await sendMessage(session.id, text.trim(), queue);
      setText("");
      setNotice(delivery === "queued_for_next_turn" ? "Queued: Claude will start it when the current turn finishes." : null);
    } catch (e) {
      setError(toCoreError(e));
    } finally {
      setBusy(false);
    }
  }

  const onSubmit = (e: FormEvent) => {
    e.preventDefault();
    void submit(false);
  };

  if (!session.attached) {
    return (
      <p className="muted small composer-note">
        This session has no running process.{" "}
        {session.native_session_id ? "Resume it to continue the same Claude conversation." : ""}
      </p>
    );
  }
  return (
    <form className="composer" onSubmit={onSubmit}>
      <textarea
        className="input textarea"
        aria-label={running ? "Follow-up message for the next turn" : "Message to Claude"}
        placeholder={running ? "A turn is running. You can queue a follow-up for the next turn." : "Describe a development task for Claude…"}
        value={text}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) void submit(running);
        }}
        rows={3}
        disabled={busy}
      />
      <div className="composer-actions">
        <span className="muted small">
          {running
            ? session.capabilities.steer_running_turn
              ? ""
              : "Claude Code cannot change a running turn. Queue a follow-up, or interrupt."
            : "⌘↩ to send"}
        </span>
        {running ? (
          <Button type="button" onClick={() => void submit(true)} disabled={busy || !canQueue || !text.trim()}>
            Queue for next turn
          </Button>
        ) : (
          <Button type="submit" variant="primary" disabled={busy || !ready || !text.trim()}>
            {busy ? "Sending…" : "Send"}
          </Button>
        )}
      </div>
      {notice ? <p className="muted small">{notice}</p> : null}
      {error ? <ErrorNotice title="Message not sent" message={error.message} code={error.code} /> : null}
    </form>
  );
}
