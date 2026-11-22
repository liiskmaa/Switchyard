import { Badge } from "../../components/ui";
import { formatRelative } from "../../lib/format";
import type { TaskDetail, TestResult } from "../../types/protocol";
import { stepName } from "./labels";

const STATUS_TONE: Record<string, "ok" | "warn" | "error" | "neutral"> = {
  passed: "ok",
  failed: "error",
  timed_out: "error",
  error: "error",
  refused: "error",
  skipped: "neutral",
  cancelled: "neutral",
};

const STATUS_TEXT: Record<string, string> = {
  passed: "Passed",
  failed: "Failed",
  timed_out: "Timed out",
  error: "Could not start",
  refused: "Blocked by policy",
  skipped: "Not run",
  cancelled: "Stopped",
};

function output(r: TestResult): string {
  return [r.stdout_tail, r.stderr_tail, r.detail].filter((s) => s && s.trim() !== "").join("\n");
}

/**
 * The latest run of the workflow's test gate (ADR 0039): Switchyard's own
 * run of the commands the implementer named, never an agent's claim.
 */
export function TestResults({ detail, now }: { detail: TaskDetail; now: number }) {
  const runs = detail.test_runs ?? [];
  const run = runs[runs.length - 1];
  if (!run) return null;
  const tone = run.status === "passed" ? "ok" : run.status === "failed" ? "error" : "warn";
  const label = run.status === "passed" ? "Tests passed" : run.status === "failed" ? "Tests failed" : "Tests need you";
  return (
    <section className="test-results" aria-label="Test results">
      <h3 className="policy-section">
        Tests <Badge tone={tone}>{label}</Badge>
      </h3>
      <p className="muted small">
        Run by Switchyard for {stepName(run.step_key).toLowerCase()} {formatRelative(run.ended_at_ms, now)}
        {run.ui_change ? " · the change touches UI files, so browser tests are required" : ""}
        {run.authorized_by ? ` · authorized by ${run.authorized_by}` : ""}. {run.summary}.
      </p>
      <ul className="mini-list">
        {run.results.map((r, i) => {
          const out = output(r);
          return (
            <li key={`${r.command}-${i}`} className="test-result">
              <span className="mini-title">
                <Badge>{r.kind}</Badge> <code>{r.command}</code>{" "}
                <Badge tone={STATUS_TONE[r.status] ?? "neutral"}>{STATUS_TEXT[r.status] ?? r.status}</Badge>
              </span>
              {out ? (
                <details className="small">
                  <summary>Output{r.exit_code !== undefined && r.exit_code !== null ? ` (exit code ${r.exit_code})` : ""}</summary>
                  <pre className="test-output">{out}</pre>
                </details>
              ) : null}
            </li>
          );
        })}
      </ul>
    </section>
  );
}
