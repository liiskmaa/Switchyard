import { type FormEvent, useCallback, useState } from "react";

import { Badge, Button, EmptyState, ErrorNotice, KeyValue, Panel } from "../../components/ui";
import { toCoreError, type CoreRequestError } from "../../lib/core";
import { formatRelative, formatTimestamp, shortOid } from "../../lib/format";
import { navigate } from "../../stores/navigation";
import { useCoreQuery } from "../../stores/useCoreQuery";
import type { EventEnvelope, Project, Session, Workspace } from "../../types/protocol";
import { STATE_LABEL, isAgentEvent, listSessions, startSession, stateTone } from "../agents/api";
import { CleanupPanel } from "./CleanupPanel";
import {
  WORKSPACE_STATUS_LABEL,
  archiveWorkspace,
  completeWorkspace,
  createWorkspace,
  getWorkspace,
  isWorkspaceEvent,
  listWorkspaces,
  reasonParts,
  restoreWorkspace,
  verifyWorkspace,
  workspaceGitStatus,
  workspaceTone,
} from "./api";

/** Notice for warnings (uncommitted work, missing directories). */
export function WarnNotice({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="notice notice-warn" role="status">
      <div>
        <strong>{title}</strong>
        <p>{children}</p>
      </div>
    </div>
  );
}

/**
 * A project's workspaces: isolated Git worktrees on their own branches, each
 * able to run one Claude Code session. Core owns them; this view only asks.
 */
export function WorkspacesPanel({ project }: { project: Project }) {
  const [showArchived, setShowArchived] = useState(false);
  const [creating, setCreating] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const forProject = useCallback(
    (e: EventEnvelope) => e.project_id === project.id && (isWorkspaceEvent(e) || isAgentEvent(e)),
    [project.id],
  );
  const workspaces = useCoreQuery(
    `workspaces:${project.id}:${showArchived}`,
    () => listWorkspaces(project.id, showArchived),
    { refreshOn: forProject },
  );
  const sessions = useCoreQuery(`project-sessions:${project.id}`, () => listSessions({ project_id: project.id, limit: 200 }), {
    refreshOn: forProject,
  });

  const list = workspaces.data ?? [];
  const selected = list.find((w) => w.id === selectedId) ?? null;
  const sessionFor = (w: Workspace): Session | undefined =>
    (sessions.data ?? []).find((s) => s.id === w.active_session_id) ??
    (sessions.data ?? []).find((s) => s.workspace_id === w.id);

  return (
    <Panel
      title="Workspaces"
      actions={
        <Button icon="plus" onClick={() => setCreating(true)} disabled={!project.accessible || creating}>
          New workspace
        </Button>
      }
    >
      <p className="muted small">
        Each workspace is a separate Git worktree on its own <code>switchyard/…</code> branch, stored by Switchyard
        outside <code>{project.root_path}</code>. Agents run in a workspace, never in the project checkout, so several
        sessions can work on this project at once. This isolates source trees only: an allowed or approved shell command
        still runs with your user&apos;s permissions and can reach files elsewhere.
      </p>
      {creating ? (
        <CreateWorkspaceForm
          project={project}
          onCancel={() => setCreating(false)}
          onCreated={(w) => {
            setCreating(false);
            setSelectedId(w.id);
            workspaces.refresh();
          }}
        />
      ) : null}
      {workspaces.error ? (
        <ErrorNotice title="Cannot load workspaces" message={workspaces.error.message} code={workspaces.error.code} />
      ) : null}
      {workspaces.data && list.length === 0 && !creating ? (
        <EmptyState icon="branch" title="No workspaces yet">
          Create a workspace to start a Claude Code session on this project.
        </EmptyState>
      ) : null}
      {list.length > 0 ? (
        <div className="ws-table" role="table" aria-label="Workspaces">
          <div className="ws-row ws-head" role="row">
            <span role="columnheader">Workspace</span>
            <span role="columnheader">Branch</span>
            <span role="columnheader">Status</span>
            <span role="columnheader">Session</span>
            <span role="columnheader">Git</span>
            <span role="columnheader">Last activity</span>
          </div>
          {list.map((w) => {
            const s = sessionFor(w);
            return (
              <button
                key={w.id}
                type="button"
                role="row"
                className={`ws-row${w.id === selectedId ? " active" : ""}`}
                onClick={() => setSelectedId(w.id === selectedId ? null : w.id)}
                aria-expanded={w.id === selectedId}
              >
                <span className="ws-name">{w.name}</span>
                <span className="mono ws-branch" title={w.branch ?? ""}>
                  {w.kind === "legacy_checkout" ? <span className="muted">project checkout</span> : shortBranch(w.branch)}
                </span>
                <span>
                  <Badge tone={workspaceTone(w.status)}>{WORKSPACE_STATUS_LABEL[w.status]}</Badge>
                </span>
                <span>
                  {s && w.active_session_id === s.id ? (
                    <Badge tone={stateTone(s.state)}>{STATE_LABEL[s.state]}</Badge>
                  ) : s ? (
                    <span className="muted small">{STATE_LABEL[s.state]}</span>
                  ) : (
                    <span className="muted small">none</span>
                  )}
                </span>
                <span>{gitLabel(w)}</span>
                <span className="muted small">{formatRelative(w.last_activity_at_ms, Date.now())}</span>
              </button>
            );
          })}
        </div>
      ) : null}
      <label className="check small">
        <input type="checkbox" checked={showArchived} onChange={(e) => setShowArchived(e.target.checked)} /> Show archived,
        removed and legacy records
      </label>
      {selected ? (
        <WorkspaceDetail
          key={selected.id}
          workspace={selected}
          project={project}
          onChanged={workspaces.refresh}
          onArchived={() => setShowArchived(true)}
        />
      ) : null}
    </Panel>
  );
}

function shortBranch(branch: string | null): string {
  if (!branch) return "—";
  // switchyard/<uuid> → switchyard/…<last 8>
  const m = /^switchyard\/([0-9a-f-]{36})$/.exec(branch);
  return m?.[1] ? `switchyard/…${m[1].slice(-8)}` : branch;
}

function gitLabel(w: Workspace) {
  if (w.kind === "legacy_checkout" || w.status === "removed") return <span className="muted small">—</span>;
  if (!w.path_exists) return <Badge tone="error">missing</Badge>;
  if (w.dirty === null) return <span className="muted small">unknown</span>;
  return w.dirty ? <Badge tone="warn">changes</Badge> : <Badge tone="ok">clean</Badge>;
}

function CreateWorkspaceForm({
  project,
  onCancel,
  onCreated,
}: {
  project: Project;
  onCancel: () => void;
  onCreated: (w: Workspace) => void;
}) {
  const [name, setName] = useState("");
  const [baseRef, setBaseRef] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<CoreRequestError | null>(null);

  async function submit(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      onCreated(await createWorkspace(project.id, name.trim() || undefined, baseRef.trim() || undefined));
    } catch (err) {
      setError(toCoreError(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form className="add-project ws-create" onSubmit={(e) => void submit(e)}>
      <label htmlFor="ws-name">Name (optional)</label>
      <input
        id="ws-name"
        className="input"
        placeholder="e.g. Signup validation"
        value={name}
        maxLength={80}
        onChange={(e) => setName(e.target.value)}
        autoFocus
      />
      <label htmlFor="ws-base">Branch from (optional)</label>
      <input
        id="ws-base"
        className="input mono"
        placeholder="HEAD"
        value={baseRef}
        onChange={(e) => setBaseRef(e.target.value)}
        spellCheck={false}
      />
      <p className="muted small">
        Creates a new branch <code>switchyard/&lt;id&gt;</code> from that commit and checks it out in a new directory. The
        project checkout, its branch and its working files are not changed.
      </p>
      {error ? <ErrorNotice title="Could not create workspace" message={error.message} code={error.code} /> : null}
      <div className="form-actions">
        <Button variant="ghost" onClick={onCancel} disabled={busy}>
          Cancel
        </Button>
        <Button type="submit" variant="primary" disabled={busy}>
          {busy ? "Creating worktree…" : "Create workspace"}
        </Button>
      </div>
    </form>
  );
}

function WorkspaceDetail({
  workspace: w,
  project,
  onChanged,
  onArchived,
}: {
  workspace: Workspace;
  project: Project;
  onChanged: () => void;
  /** Keeps an archived workspace visible so its next steps stay reachable. */
  onArchived: () => void;
}) {
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<CoreRequestError | null>(null);
  const [confirmArchive, setConfirmArchive] = useState(false);
  const [cleaning, setCleaning] = useState(false);
  const [task, setTask] = useState("");
  const legacy = w.kind === "legacy_checkout";
  const forWorkspace = useCallback((e: EventEnvelope) => e.workspace_id === w.id, [w.id]);
  const detail = useCoreQuery(`workspace:${w.id}`, () => getWorkspace(w.id), { refreshOn: forWorkspace });
  const canInspectGit = !legacy && w.path_exists && w.status !== "removed" && w.status !== "provisioning";
  const git = useCoreQuery(`workspace-git:${w.id}`, () => workspaceGitStatus(w.id), {
    refreshOn: forWorkspace,
    enabled: canInspectGit,
  });
  const reason = reasonParts(w.status_reason);
  const acceptsSession = !legacy && (["ready", "dirty", "completed"] as string[]).includes(w.status);

  async function act(name: string, op: () => Promise<unknown>) {
    setBusy(name);
    setError(null);
    try {
      await op();
      onChanged();
      detail.refresh();
    } catch (e) {
      setError(toCoreError(e));
    } finally {
      setBusy(null);
    }
  }

  async function start() {
    setBusy("start");
    setError(null);
    try {
      const session = await startSession(w.id, project.id, task.trim() || undefined);
      setTask("");
      navigate("agents", { sessionId: session.id });
    } catch (e) {
      setError(toCoreError(e));
      onChanged();
    } finally {
      setBusy(null);
    }
  }

  const g = git.data;
  return (
    <div className="ws-detail">
      <h3>{w.name}</h3>
      {legacy ? (
        <WarnNotice title="Legacy record: the project checkout">
          Sessions from before workspace isolation ran directly in the project checkout. They are kept as history but
          cannot be resumed there, and Switchyard never modifies or cleans up this directory. Create a workspace to
          continue working.
        </WarnNotice>
      ) : null}
      {!legacy && !w.path_exists && w.status !== "removed" ? (
        <ErrorNotice title="Worktree directory is missing" message={`${w.path} does not exist. Nothing will run here until it is restored and re-checked.`} />
      ) : null}
      {w.status === "failed" && reason ? (
        <ErrorNotice title="This workspace needs attention" message={reason.detail || reason.code} code={reason.code} />
      ) : null}
      {w.status !== "failed" && reason && !legacy ? <p className="muted small">Note: {w.status_reason}</p> : null}
      {g && !g.clean ? (
        <WarnNotice title="Uncommitted work in this workspace">
          {g.staged + g.unstaged > 0 ? `${g.staged + g.unstaged} changed tracked file(s)` : ""}
          {g.staged + g.unstaged > 0 && g.untracked > 0 ? " and " : ""}
          {g.untracked > 0 ? `${g.untracked} untracked file(s)` : ""}. Switchyard never discards them; commit them in the
          workspace to keep them on its branch.
        </WarnNotice>
      ) : null}
      <KeyValue
        rows={[
          ["Status", <Badge tone={workspaceTone(w.status)}>{WORKSPACE_STATUS_LABEL[w.status]}</Badge>],
          ["Directory", <code className="path">{w.path}</code>],
          ["Branch", w.branch ? <code>{w.branch}</code> : <span className="muted">none (project checkout)</span>],
          [
            "Based on",
            w.base_commit ? (
              <span>
                <code>{w.base_ref ?? "HEAD"}</code> at <code>{shortOid(w.base_commit)}</code>
              </span>
            ) : (
              <span className="muted">—</span>
            ),
          ],
          ["Created", formatTimestamp(w.created_at_ms)],
          ["Last activity", formatRelative(w.last_activity_at_ms, Date.now())],
          ["Workspace ID", <code>{w.id}</code>],
        ]}
      />
      {canInspectGit ? (
        <div className="ws-git">
          <div className="git-summary">
            <strong className="small">Git status</strong>
            {git.loading && !g ? <span className="muted small">checking…</span> : null}
            {g ? (
              <>
                {g.clean ? <Badge tone="ok">Clean</Badge> : <Badge tone="warn">Uncommitted changes</Badge>}
                {!g.on_expected_branch ? <Badge tone="error">Unexpected branch: {g.branch ?? "detached"}</Badge> : null}
                {g.conflicted > 0 ? <Badge tone="error">{g.conflicted} conflicted</Badge> : null}
                <span className="muted small">
                  {g.staged} staged · {g.unstaged} unstaged · {g.untracked} untracked
                  {g.commits_since_base !== null ? ` · ${g.commits_since_base} commit(s) since base` : ""}
                  {g.head_oid ? ` · HEAD ${shortOid(g.head_oid)}` : ""}
                </span>
              </>
            ) : null}
            <Button variant="ghost" icon="refresh" onClick={git.refresh} disabled={git.loading}>
              Refresh
            </Button>
          </div>
          {git.error ? <ErrorNotice title="Cannot read Git status" message={git.error.message} code={git.error.code} /> : null}
        </div>
      ) : null}

      {w.active_session_id ? (
        <div className="agent-live">
          <Badge tone="accent">Session running</Badge>
          <span className="small">Claude Code is working in this workspace.</span>
          <Button variant="primary" onClick={() => navigate("agents", { sessionId: w.active_session_id })}>
            Open session
          </Button>
        </div>
      ) : acceptsSession ? (
        <div className="ws-start">
          <textarea
            className="input textarea"
            rows={3}
            placeholder="Optional first task, e.g. “Add input validation to the signup form and run the tests.”"
            value={task}
            onChange={(e) => setTask(e.target.value)}
            disabled={busy !== null}
          />
          <p className="muted small">
            Claude runs in this workspace&apos;s directory with its file tools confined to it. Switchyard answers each tool
            request under this project&apos;s policies; by default, file changes and shell commands ask you first.
          </p>
          <div className="form-actions start">
            <Button variant="primary" onClick={() => void start()} disabled={busy !== null}>
              {busy === "start" ? "Starting…" : "Start Claude session here"}
            </Button>
          </div>
        </div>
      ) : null}

      {(detail.data?.sessions ?? []).length > 0 ? (
        <ul className="mini-list">
          {(detail.data?.sessions ?? []).map((s) => (
            <li key={s.id}>
              <Badge tone={stateTone(s.state)}>{STATE_LABEL[s.state]}</Badge>
              <button type="button" className="link" onClick={() => navigate("agents", { sessionId: s.id })}>
                Session from {formatRelative(s.started_at_ms, Date.now())}
              </button>
            </li>
          ))}
        </ul>
      ) : null}

      {error ? <ErrorNotice title={`Could not ${busy ?? "complete the action"}`} message={error.message} code={error.code} /> : null}

      {!legacy && w.status !== "removed" ? (
        <div className="form-actions start ws-actions">
          <Button variant="ghost" icon="refresh" onClick={() => void act("re-check", () => verifyWorkspace(w.id))} disabled={busy !== null}>
            {busy === "re-check" ? "Checking…" : "Re-check worktree"}
          </Button>
          {w.status === "ready" || w.status === "dirty" ? (
            <Button variant="ghost" onClick={() => void act("mark as done", () => completeWorkspace(w.id))} disabled={busy !== null}>
              Mark as done
            </Button>
          ) : null}
          {w.status === "archived" ? (
            <>
              <Button onClick={() => void act("restore", () => restoreWorkspace(w.id))} disabled={busy !== null}>
                Restore
              </Button>
              <Button variant="danger" onClick={() => setCleaning(true)} disabled={busy !== null || cleaning}>
                Clean up…
              </Button>
            </>
          ) : (["ready", "dirty", "completed", "failed"] as string[]).includes(w.status) && !confirmArchive ? (
            <Button onClick={() => setConfirmArchive(true)} disabled={busy !== null}>
              Archive…
            </Button>
          ) : null}
        </div>
      ) : null}
      {confirmArchive && w.status !== "archived" ? (
        <div className="ws-confirm">
          <p className="small">
            Archiving stops new sessions in this workspace. Its directory, branch and any uncommitted work stay on disk;
            you can restore it later.
          </p>
          <div className="form-actions start">
            <Button
              onClick={() =>
                void act("archive", async () => {
                  await archiveWorkspace(w.id);
                  setConfirmArchive(false);
                  onArchived();
                })
              }
              disabled={busy !== null}
            >
              {busy === "archive" ? "Archiving…" : "Confirm archive"}
            </Button>
            <Button variant="ghost" onClick={() => setConfirmArchive(false)} disabled={busy !== null}>
              Cancel
            </Button>
          </div>
        </div>
      ) : null}
      {cleaning && w.status === "archived" ? (
        <CleanupPanel
          workspace={w}
          onInspected={git.refresh}
          onClose={() => setCleaning(false)}
          onDone={() => {
            setCleaning(false);
            onChanged();
          }}
        />
      ) : null}
    </div>
  );
}
