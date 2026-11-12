import { useCallback } from "react";

import { Badge, Button, ErrorNotice, KeyValue, Panel } from "../../components/ui";
import { formatRelative, shortOid } from "../../lib/format";
import { useCoreQuery } from "../../stores/useCoreQuery";
import type { EventEnvelope, Project } from "../../types/protocol";
import { gitStatus } from "./api";

export function GitStatusPanel({ project }: { project: Project }) {
  const refreshOn = useCallback((e: EventEnvelope) => e.project_id === project.id, [project.id]);
  const status = useCoreQuery(`git:${project.id}`, () => gitStatus(project.id), { refreshOn });
  const s = status.data;

  return (
    <Panel
      title="Git status · project checkout"
      actions={
        <Button variant="ghost" icon="refresh" onClick={status.refresh} disabled={status.loading}>
          {status.loading ? "Checking…" : "Refresh"}
        </Button>
      }
    >
      {status.error ? (
        <ErrorNotice title="Cannot read repository status" message={status.error.message} code={status.error.code} />
      ) : !s ? (
        <p className="muted">Reading repository…</p>
      ) : (
        <>
          <div className="git-summary">
            {s.clean ? <Badge tone="ok">Clean</Badge> : <Badge tone="warn">Uncommitted changes</Badge>}
            {s.detached ? <Badge tone="warn">Detached HEAD</Badge> : null}
            {s.conflicted > 0 ? <Badge tone="error">{s.conflicted} conflicted</Badge> : null}
          </div>
          <KeyValue
            rows={[
              ["Branch", s.branch ? <code>{s.branch}</code> : <span className="muted">detached</span>],
              ["HEAD", s.head_oid ? <code>{shortOid(s.head_oid)}</code> : <span className="muted">no commits yet</span>],
              ["Upstream", s.upstream ? <code>{s.upstream}</code> : <span className="muted">none</span>],
              [
                "Ahead / behind",
                s.ahead !== null && s.behind !== null ? `${s.ahead} / ${s.behind}` : <span className="muted">n/a</span>,
              ],
            ]}
          />
          <div className="counts">
            <Count label="Staged" value={s.staged} />
            <Count label="Unstaged" value={s.unstaged} />
            <Count label="Untracked" value={s.untracked} />
            <Count label="Conflicted" value={s.conflicted} tone={s.conflicted > 0 ? "error" : undefined} />
          </div>
          <p className="muted small">Checked {formatRelative(s.checked_at_ms, Date.now())}</p>
        </>
      )}
    </Panel>
  );
}

function Count({ label, value, tone }: { label: string; value: number; tone?: "error" | undefined }) {
  return (
    <div className={`count${tone ? ` count-${tone}` : ""}`}>
      <span className="count-value">{value}</span>
      <span className="count-label">{label}</span>
    </div>
  );
}
