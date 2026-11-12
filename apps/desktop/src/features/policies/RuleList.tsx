import { useState } from "react";

import { Badge, Button, ErrorNotice } from "../../components/ui";
import { toCoreError, type CoreRequestError } from "../../lib/core";
import { formatRelative } from "../../lib/format";
import { useCoreQuery } from "../../stores/useCoreQuery";
import type { PolicyCategoryInfo, PolicyRule, Workspace } from "../../types/protocol";
import { archiveRule, listEvaluations, ruleHistory, setRuleEnabled } from "./api";
import { EvaluationList } from "./EvaluationList";
import { CATEGORY_LABEL, DECISION_LABEL, DECISION_TONE, describeConditions, scopeLabel } from "./labels";
import { RuleForm } from "./RuleForm";

function RuleDetail({ rule }: { rule: PolicyRule }) {
  const matches = useCoreQuery(`rule-evals:${rule.id}:${rule.revision}`, () =>
    listEvaluations({ rule_id: rule.id, limit: 10 }),
  );
  const history = useCoreQuery(`rule-history:${rule.id}:${rule.revision}`, () => ruleHistory(rule.id));
  return (
    <div className="rule-detail">
      <h4>Recent requests this rule matched</h4>
      <EvaluationList evaluations={matches.data ?? []} empty="No request has matched this rule yet." />
      <h4>History</h4>
      <ul className="mini-list">
        {(history.data?.history ?? []).map((h) => (
          <li key={h.revision}>
            <span className="mini-title">
              Revision {h.revision}: {h.change}
            </span>
            <span className="mini-sub">
              {DECISION_LABEL[h.rule.decision]} · {describeConditions(h.rule.category, h.rule.conditions, h.rule.decision)} ·{" "}
              {h.rule.enabled ? "enabled" : "disabled"} · {formatRelative(h.changed_at_ms, Date.now())}
              {h.changed_by ? ` by ${h.changed_by}` : ""}
            </span>
          </li>
        ))}
      </ul>
    </div>
  );
}

function RuleRow({
  rule,
  readOnly,
  categories,
  workspaces,
  onChanged,
}: {
  rule: PolicyRule;
  readOnly: boolean;
  categories: PolicyCategoryInfo[];
  workspaces: Workspace[];
  onChanged: () => void;
}) {
  const [mode, setMode] = useState<"view" | "edit" | "detail" | "archive">("view");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<CoreRequestError | null>(null);

  async function run(action: () => Promise<unknown>) {
    setBusy(true);
    setError(null);
    try {
      await action();
      setMode("view");
      onChanged();
    } catch (e) {
      setError(toCoreError(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <li className={`rule${rule.enabled ? "" : " rule-disabled"}`}>
      <div className="rule-head">
        <Badge tone={rule.enabled ? DECISION_TONE[rule.decision] : "neutral"}>{DECISION_LABEL[rule.decision]}</Badge>
        <span className="rule-what">
          {CATEGORY_LABEL[rule.category]} <span className="muted">{describeConditions(rule.category, rule.conditions, rule.decision)}</span>
        </span>
        <span className="muted small rule-scope">{scopeLabel(rule)}</span>
        {!rule.enabled ? <Badge>disabled</Badge> : null}
        <span className="muted small rule-rev" title="Revision; every change creates a new one">
          rev {rule.revision}
        </span>
      </div>
      <p className="small rule-description">{rule.description}</p>
      {mode === "edit" ? (
        <RuleForm
          projectId={rule.project_id}
          categories={categories}
          workspaces={workspaces}
          rule={rule}
          onDone={() => {
            setMode("view");
            onChanged();
          }}
          onCancel={() => setMode("view")}
        />
      ) : null}
      {mode === "archive" ? (
        <div className="ws-confirm">
          <p className="small">
            Archive this rule? It stops applying immediately. Its history and the decisions it made stay in the audit trail.
          </p>
          <div className="form-actions start">
            <Button variant="danger" onClick={() => void run(() => archiveRule(rule))} disabled={busy}>
              {busy ? "Archiving…" : "Archive rule"}
            </Button>
            <Button variant="ghost" onClick={() => setMode("view")} disabled={busy}>
              Keep
            </Button>
          </div>
        </div>
      ) : null}
      {error ? <ErrorNotice title="Change not saved" message={error.message} code={error.code} /> : null}
      <div className="rule-actions">
        <button type="button" className="link" onClick={() => setMode(mode === "detail" ? "view" : "detail")}>
          {mode === "detail" ? "Hide matches and history" : "Matches and history"}
        </button>
        {readOnly ? (
          <span className="muted small">Inherited from the project. To change it, view “The whole project”.</span>
        ) : mode === "view" || mode === "detail" ? (
          <>
            <button type="button" className="link" onClick={() => setMode("edit")} disabled={busy}>
              Edit
            </button>
            <button type="button" className="link" onClick={() => void run(() => setRuleEnabled(rule, !rule.enabled))} disabled={busy}>
              {rule.enabled ? "Disable" : "Enable"}
            </button>
            <button type="button" className="link" onClick={() => setMode("archive")} disabled={busy}>
              Archive
            </button>
          </>
        ) : null}
      </div>
      {mode === "detail" ? <RuleDetail rule={rule} /> : null}
    </li>
  );
}

export function RuleList({
  rules,
  readOnly = false,
  categories,
  workspaces,
  empty,
  onChanged,
}: {
  rules: PolicyRule[];
  readOnly?: boolean;
  categories: PolicyCategoryInfo[];
  workspaces: Workspace[];
  empty: string;
  onChanged: () => void;
}) {
  if (rules.length === 0) return <p className="muted small">{empty}</p>;
  return (
    <ul className="rules">
      {rules.map((r) => (
        <RuleRow key={r.id} rule={r} readOnly={readOnly} categories={categories} workspaces={workspaces} onChanged={onChanged} />
      ))}
    </ul>
  );
}
