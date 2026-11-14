import { useEffect, useState } from "react";

import { Badge, Button, ErrorNotice, KeyValue, Panel } from "../../components/ui";
import { toCoreError, type CoreRequestError } from "../../lib/core";
import { describeEvent, formatClock, formatRelative } from "../../lib/format";
import { useNow } from "../../lib/useNow";
import { navigate } from "../../stores/navigation";
import { useCoreQuery } from "../../stores/useCoreQuery";
import type { ChangedFileInfo, Integration, IntegrationDetail, Project, ValidationRunInfo } from "../../types/protocol";
import {
  abortResolution,
  approveIntegration,
  cancelIntegration,
  cancelValidation,
  commitWorkspace,
  eventOf,
  getIntegration,
  integrationChanges,
  integrationPatch,
  isIntegrationEvent,
  projectEvents,
  refreshIntegration,
  runValidation,
  updateFromTarget,
} from "./api";
import { RUN_TONE, STATUS_LABEL, STEP_TONE, consequences, integrationActions, shortOid, statusTone } from "./labels";

function useAction(onDone: () => void) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<CoreRequestError | null>(null);
  async function run(action: () => Promise<unknown>) {
    setBusy(true);
    setError(null);
    try {
      await action();
      onDone();
    } catch (err) {
      setError(toCoreError(err));
      onDone();
    } finally {
      setBusy(false);
    }
  }
  return { busy, error, run };
}

const KIND_TONE: Record<string, "ok" | "warn" | "error" | "neutral" | "accent"> = {
  added: "ok",
  modified: "accent",
  deleted: "error",
  renamed: "warn",
  copied: "warn",
};

function Patch({ text, truncated }: { text: string; truncated: boolean }) {
  if (!text) return <p className="muted small">No textual changes.</p>;
  return (
    <>
      <pre className="patch" aria-label="Patch">
        {text.split("\n").map((line, n) => {
          const cls = line.startsWith("+++") || line.startsWith("---")
            ? "patch-file"
            : line.startsWith("+")
              ? "patch-add"
              : line.startsWith("-")
                ? "patch-del"
                : line.startsWith("@@")
                  ? "patch-hunk"
                  : "";
          return (
            <span key={n} className={cls}>
              {line}
              {"\n"}
            </span>
          );
        })}
      </pre>
      {truncated ? <p className="muted small">The patch is longer than shown; open the file in your editor for the rest.</p> : null}
    </>
  );
}

function FileList({
  files,
  integrationId,
  uncommitted,
}: {
  files: ChangedFileInfo[];
  integrationId: string;
  uncommitted: boolean;
}) {
  const [open, setOpen] = useState<string | null>(null);
  const patch = useCoreQuery(
    `patch:${integrationId}:${uncommitted}:${open ?? ""}`,
    () => integrationPatch(integrationId, open ?? undefined, uncommitted),
    { enabled: open !== null },
  );
  return (
    <ul className="mini-list">
      {files.map((f) => (
        <li key={`${f.old_path ?? ""}${f.path}`}>
          <button type="button" className="link file-row" aria-expanded={open === f.path} onClick={() => setOpen(open === f.path ? null : f.path)}>
            <Badge tone={KIND_TONE[f.kind] ?? "neutral"}>{f.kind}</Badge>{" "}
            <code>{f.old_path ? `${f.old_path} → ${f.path}` : f.path}</code>{" "}
            <span className="muted small">
              {f.binary ? "binary" : `+${f.additions ?? 0} −${f.deletions ?? 0}`}
            </span>
          </button>
          {open === f.path ? (
            patch.error ? (
              <ErrorNotice title="Cannot load the patch" message={patch.error.message} code={patch.error.code} />
            ) : patch.data ? (
              <Patch text={patch.data.patch} truncated={patch.data.truncated} />
            ) : null
          ) : null}
        </li>
      ))}
    </ul>
  );
}

function Changes({ i }: { i: Integration }) {
  const changes = useCoreQuery(`changes:${i.id}:${i.source_commit}:${i.target_commit}:${i.updated_at_ms}`, () =>
    integrationChanges(i.id),
  );
  const c = changes.data;
  if (changes.error) return <ErrorNotice title="Cannot read the changes" message={changes.error.message} code={changes.error.code} />;
  if (!c) return null;
  return (
    <>
      <p className="small">
        {c.commits_total} commit(s), {c.files_total} file(s), +{c.additions} −{c.deletions}, measured from{" "}
        <code>{shortOid(c.base_commit)}</code> (the common ancestor with {i.target_branch}).
        {c.diverged ? ` ${i.target_branch} has commits the source does not contain.` : ""}
      </p>
      {c.commits.length > 0 ? (
        <ul className="mini-list">
          {c.commits.map((k) => (
            <li key={k.oid}>
              <span className="mini-title">
                <code>{shortOid(k.oid)}</code> {k.subject}
                {k.parents.length > 1 ? <Badge>merge</Badge> : null}
              </span>
              <span className="mini-sub">{k.author_name}</span>
            </li>
          ))}
        </ul>
      ) : null}
      <h3 className="policy-section">Files</h3>
      {c.files.length === 0 ? <p className="muted small">No committed changes.</p> : <FileList files={c.files} integrationId={i.id} uncommitted={false} />}
      {c.truncated ? <p className="muted small">Only the first {c.files.length} files are listed.</p> : null}
      {c.uncommitted.length > 0 ? (
        <>
          <h3 className="policy-section">Uncommitted in the workspace (not part of the integration)</h3>
          <FileList files={c.uncommitted} integrationId={i.id} uncommitted />
        </>
      ) : null}
    </>
  );
}

function Runs({ runs, source }: { runs: ValidationRunInfo[]; source: string }) {
  if (runs.length === 0) return <p className="muted small">No validation has run.</p>;
  return (
    <ul className="mini-list">
      {runs.slice(0, 5).map((r) => (
        <li key={r.id}>
          <span className="mini-title">
            <Badge tone={RUN_TONE[r.status]}>{r.status}</Badge> for <code>{shortOid(r.source_commit)}</code>
            {r.source_commit !== source ? <Badge tone="warn">stale: another commit</Badge> : null}
          </span>
          <ul className="findings">
            {r.steps.map((s) => (
              <li key={s.id} className="small">
                <Badge tone={STEP_TONE[s.status]}>{s.status}</Badge> <strong>{s.name}</strong> <code>{s.command_line}</code>
                {s.exit_code !== null ? ` · exit ${s.exit_code}` : ""}
                {s.authorized_by ? ` · authorized by ${s.authorized_by}` : ""}
                {s.detail ? <div className="muted">{s.detail}</div> : null}
                {s.stdout || s.stderr ? (
                  <details>
                    <summary>Output{s.stdout_truncated || s.stderr_truncated ? " (last 64 KiB)" : ""}</summary>
                    {s.stdout ? <pre className="patch">{s.stdout}</pre> : null}
                    {s.stderr ? <pre className="patch patch-del">{s.stderr}</pre> : null}
                  </details>
                ) : null}
              </li>
            ))}
          </ul>
        </li>
      ))}
    </ul>
  );
}

export function IntegrationDetailView({
  integrationId,
  project,
  onChanged,
}: {
  integrationId: string;
  project: Project | undefined;
  onChanged: () => void;
}) {
  const now = useNow();
  const detail = useCoreQuery(`integration:${integrationId}`, () => getIntegration(integrationId), {
    refreshOn: (e) => isIntegrationEvent(e) && eventOf(e, integrationId),
  });
  const history = useCoreQuery(
    `integration-events:${integrationId}`,
    async () => (project ? (await projectEvents(project.id)).filter((e) => eventOf(e, integrationId)) : []),
    { refreshOn: (e) => isIntegrationEvent(e) && eventOf(e, integrationId) },
  );
  const refresh = () => {
    detail.refresh();
    history.refresh();
    onChanged();
  };
  const action = useAction(refresh);
  const [message, setMessage] = useState("");
  const [authorized, setAuthorized] = useState<string[]>([]);
  const [confirming, setConfirming] = useState(false);
  // Authorizations are for the plan as shown: any change to it clears them.
  const planKey = JSON.stringify(detail.data?.plan ?? []);
  useEffect(() => setAuthorized([]), [planKey]);

  if (detail.error) return <ErrorNotice title="Cannot load the integration" message={detail.error.message} code={detail.error.code} />;
  const d: IntegrationDetail | null = detail.data;
  if (!d) return null;
  const i = d.integration;
  const can = integrationActions(i);
  const facts = i.preflight.facts;
  const summary = i.preflight.summary;
  const merge = facts?.merge;
  const latestForSource = d.runs.find((r) => r.source_commit === i.source_commit);
  const asks = d.plan.filter((p) => p.policy_decision === "ask");

  return (
    <div className="detail">
      <Panel title="Approval" actions={<Badge tone={statusTone(i.status)}>{STATUS_LABEL[i.status]}</Badge>}>
        <KeyValue
          rows={[
            ["Project", project?.name ?? i.project_id],
            [
              "Task",
              i.task_id ? (
                <button type="button" className="link" onClick={() => navigate("tasks", { taskId: i.task_id, taskProjectId: i.project_id })}>
                  Open the task
                </button>
              ) : (
                "None"
              ),
            ],
            ["Source", `${i.workspace_name ?? "workspace"} · ${i.source_branch}`],
            ["Source commit", <code key="s">{i.source_commit}</code>],
            ["Target", i.target_branch],
            ["Target commit", <code key="t">{i.target_commit}</code>],
            [
              "Changes",
              summary ? `${summary.commits_ahead} commit(s), ${summary.files_changed} file(s), +${summary.additions} −${summary.deletions}` : "—",
            ],
            [
              "Review",
              facts ? (facts.review.required ? (facts.review.satisfied ? "Approved for exactly this content" : facts.review.detail) : "Not required in this project") : "—",
            ],
            [
              "Validation",
              (facts?.validations_configured ?? 0) === 0
                ? "No validation commands are configured"
                : latestForSource
                  ? `${latestForSource.status} for this source commit`
                  : "Not run for this source commit",
            ],
            [
              "Merge",
              merge?.kind === "up_to_date"
                ? "The source contains the target: no conflicts"
                : merge?.kind === "conflicts"
                  ? `Conflicts in ${merge.files.join(", ")}`
                  : merge?.kind === "clean"
                    ? "The target has new commits (update the workspace)"
                    : "—",
            ],
            ["Checked", formatRelative(i.prepared_at_ms, now)],
          ]}
        />
        {i.blockers.length > 0 ? (
          <div className="notice notice-warn" role="status">
            <div>
              <strong>Not ready to approve</strong>
              <ul className="findings">
                {i.blockers.map((b) => (
                  <li key={b.code} className="small">
                    {b.message}
                  </li>
                ))}
              </ul>
            </div>
          </div>
        ) : null}
        {i.status === "integrated" ? (
          <p className="small">
            <strong>{i.target_branch}</strong> is at <code>{i.merge_commit}</code> ({i.method === "checkout_fast_forward" ? "checkout fast-forwarded" : "branch updated"}), approved by{" "}
            {i.approved_by}.
          </p>
        ) : null}
        {i.failure ? <ErrorNotice title="Integration failed" message={i.failure} /> : null}
        {can.approve ? (
          <>
            <h3 className="policy-section">If you approve</h3>
            <ul className="findings">
              {consequences(i).map((c) => (
                <li key={c} className="small">
                  {c}
                </li>
              ))}
            </ul>
            {confirming ? (
              <div className="form-actions start">
                <Button
                  variant="primary"
                  disabled={action.busy}
                  onClick={() =>
                    void action.run(async () => {
                      setConfirming(false);
                      await approveIntegration(i);
                    })
                  }
                >
                  Integrate {shortOid(i.source_commit)} into {i.target_branch}
                </Button>
                <Button variant="ghost" onClick={() => setConfirming(false)}>
                  Back
                </Button>
              </div>
            ) : (
              <div className="form-actions start">
                <Button variant="primary" onClick={() => setConfirming(true)}>
                  Approve these commits…
                </Button>
              </div>
            )}
            <p className="muted small">An agent's report or a review verdict never approves an integration; only this does.</p>
          </>
        ) : null}
        {action.error ? <ErrorNotice title="Action refused" message={action.error.message} code={action.error.code} /> : null}
        <div className="form-actions start">
          {can.refresh ? (
            <Button icon="refresh" disabled={action.busy} onClick={() => void action.run(() => refreshIntegration(i.id))}>
              Check again
            </Button>
          ) : null}
          {can.cancel ? (
            <Button variant="ghost" disabled={action.busy} onClick={() => void action.run(() => cancelIntegration(i.id))}>
              Reject integration
            </Button>
          ) : null}
        </div>
      </Panel>

      {can.commit || can.update || can.abort ? (
        <Panel title="Workspace">
          {can.commit ? (
            <div className="input-row">
              <input
                className="input"
                aria-label="Commit message"
                placeholder={i.status === "resolving" ? "Resolve conflicts with the target" : "Describe the change"}
                value={message}
                onChange={(e) => setMessage(e.target.value)}
              />
              <Button
                disabled={action.busy || message.trim() === ""}
                onClick={() =>
                  void action.run(async () => {
                    await commitWorkspace(i.id, message);
                    setMessage("");
                  })
                }
              >
                {i.status === "resolving" ? "Complete resolution" : "Commit workspace"}
              </Button>
            </div>
          ) : null}
          {can.commit ? (
            <p className="muted small">
              Commits everything in the workspace (untracked files included, ignored files not) on {i.source_branch}. Review the
              uncommitted files below first.
            </p>
          ) : null}
          {can.update ? (
            <>
              <Button disabled={action.busy} onClick={() => void action.run(() => updateFromTarget(i.id))}>
                Update workspace from {i.target_branch}
              </Button>
              <p className="muted small">
                Merges {i.target_branch} into the workspace branch. Conflicts are left in the workspace for you, or an agent you assign
                there, to resolve; {i.target_branch} itself is never changed.
              </p>
            </>
          ) : null}
          {can.abort ? (
            <Button variant="danger" disabled={action.busy} onClick={() => void action.run(() => abortResolution(i.id))}>
              Abort the merge (discard resolution edits)
            </Button>
          ) : null}
        </Panel>
      ) : null}

      <Panel title="Changes">
        <Changes i={i} />
      </Panel>

      <Panel title="Validation">
        {d.plan.length === 0 ? (
          <p className="muted small">No validation commands are configured for this project (see settings).</p>
        ) : (
          <ul className="mini-list">
            {d.plan.map((p) => (
              <li key={p.command_id}>
                <span className="mini-title">
                  {p.name} <code>{p.command_line}</code>{" "}
                  <Badge tone={p.policy_decision === "allow" ? "ok" : p.policy_decision === "deny" ? "error" : "warn"}>
                    {p.policy_decision === "allow" ? "allowed by policy" : p.policy_decision === "deny" ? "blocked by policy" : "needs your authorization"}
                  </Badge>
                </span>
                {p.policy_decision === "ask" ? (
                  <label className="check small">
                    <input
                      type="checkbox"
                      checked={authorized.includes(p.command_id)}
                      onChange={(e) =>
                        setAuthorized(e.target.checked ? [...authorized, p.command_id] : authorized.filter((x) => x !== p.command_id))
                      }
                    />{" "}
                    Authorize for this run (it runs with your user's permissions in the workspace)
                  </label>
                ) : null}
              </li>
            ))}
          </ul>
        )}
        <div className="form-actions start">
          {can.validate ? (
            <Button
              disabled={action.busy || asks.some((a) => !authorized.includes(a.command_id))}
              onClick={() =>
                void action.run(() =>
                  runValidation(
                    i.id,
                    d.plan
                      .filter((p) => authorized.includes(p.command_id))
                      .map((p) => ({ command_id: p.command_id, command_line: p.command_line })),
                  ),
                )
              }
            >
              Validate {shortOid(i.source_commit)}
            </Button>
          ) : null}
          {i.status === "validating" ? (
            <Button variant="danger" disabled={action.busy} onClick={() => void action.run(() => cancelValidation(i.id))}>
              Cancel validation
            </Button>
          ) : null}
        </div>
        <Runs runs={d.runs} source={i.source_commit} />
      </Panel>

      <Panel title="History">
        <ol className="timeline">
          {(history.data ?? []).map((e) => (
            <li key={e.seq} className="tl">
              <span className="tl-time">{formatClock(e.occurred_at_ms)}</span>
              <span className="muted small">{e.event_type.replace("integration.", "")}</span>
              <span className="tl-body">{describeEvent(e)}</span>
            </li>
          ))}
        </ol>
      </Panel>
    </div>
  );
}
