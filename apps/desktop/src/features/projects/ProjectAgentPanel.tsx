import { useState } from "react";

import { Badge, Button, ErrorNotice, Panel } from "../../components/ui";
import { toCoreError, type CoreRequestError } from "../../lib/core";
import { formatRelative } from "../../lib/format";
import { navigate } from "../../stores/navigation";
import { useCoreQuery } from "../../stores/useCoreQuery";
import type { Project } from "../../types/protocol";
import { STATE_LABEL, isAgentEvent, listSessions, startSession, stateTone } from "../agents/api";

export function ProjectAgentPanel({ project }: { project: Project }) {
  const sessions = useCoreQuery(`project-sessions:${project.id}`, () => listSessions({ project_id: project.id, limit: 5 }), {
    refreshOn: (e) => e.project_id === project.id && isAgentEvent(e),
  });
  const [task, setTask] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<CoreRequestError | null>(null);
  const live = (sessions.data ?? []).find((s) => s.attached);

  async function start() {
    setBusy(true);
    setError(null);
    try {
      const session = await startSession(project.id, task.trim() || undefined);
      setTask("");
      navigate("agents", { sessionId: session.id });
    } catch (e) {
      setError(toCoreError(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Panel title="Claude Code">
      {live ? (
        <div className="agent-live">
          <Badge tone={stateTone(live.state)}>{STATE_LABEL[live.state]}</Badge>
          <span className="small">A session is running in this project (pid {live.pid}).</span>
          <Button variant="primary" onClick={() => navigate("agents", { sessionId: live.id })}>
            Open session
          </Button>
        </div>
      ) : (
        <>
          <textarea
            className="input textarea"
            rows={3}
            placeholder="Optional first task, e.g. “Add input validation to the signup form and run the tests.”"
            value={task}
            onChange={(e) => setTask(e.target.value)}
            disabled={busy || !project.accessible}
          />
          <p className="muted small">
            Claude runs in <code>{project.root_path}</code> with its file tools confined to this directory. Every shell
            command and file change needs your approval in Switchyard; project and user Claude settings are not loaded.
          </p>
          <div className="form-actions start">
            <Button variant="primary" onClick={() => void start()} disabled={busy || !project.accessible}>
              {busy ? "Starting…" : "Start Claude session"}
            </Button>
          </div>
        </>
      )}
      {error ? <ErrorNotice title="Could not start Claude Code" message={error.message} code={error.code} /> : null}
      {(sessions.data ?? []).filter((s) => !s.attached).length > 0 ? (
        <ul className="mini-list">
          {(sessions.data ?? [])
            .filter((s) => !s.attached)
            .map((s) => (
              <li key={s.id}>
                <Badge tone={stateTone(s.state)}>{STATE_LABEL[s.state]}</Badge>
                <button type="button" className="link" onClick={() => navigate("agents", { sessionId: s.id })}>
                  Session from {formatRelative(s.started_at_ms, Date.now())}
                </button>
              </li>
            ))}
        </ul>
      ) : null}
    </Panel>
  );
}
