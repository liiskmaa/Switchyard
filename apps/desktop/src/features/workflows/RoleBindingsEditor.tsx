import { useState } from "react";

import { providerName } from "../../lib/agents";
import { useCoreQuery } from "../../stores/useCoreQuery";
import type { AgentProvider, ModelCatalog, RoleBinding, RoleBindings } from "../../types/protocol";
import { listAgents } from "../agents/api";
import { listModels } from "./api";
import { BINDING_KEYS, BINDING_LABEL, type BindingKey } from "./labels";

const CUSTOM = "__custom__";

/** The models the installed CLI offers; the CLI default is always an explicit choice. */
function ModelSelect({
  id,
  catalog,
  value,
  onChange,
}: {
  id: string;
  catalog: ModelCatalog | null;
  value: string | null;
  onChange: (model: string | null) => void;
}) {
  const listed = catalog?.models ?? [];
  const isListed = value === null || listed.some((m) => m.id === value);
  const [custom, setCustom] = useState(!isListed);
  const def = listed.find((m) => m.is_default);
  return (
    <div>
      <select
        id={id}
        className="input select"
        aria-label={`${id} model`}
        value={custom ? CUSTOM : (value ?? "")}
        onChange={(e) => {
          const v = e.target.value;
          if (v === CUSTOM) {
            setCustom(true);
            return;
          }
          setCustom(false);
          onChange(v === "" ? null : v);
        }}
      >
        <option value="">CLI default{def ? ` (${def.display_name})` : ""}</option>
        {listed.map((m) => (
          <option key={m.id} value={m.id}>
            {m.display_name}
          </option>
        ))}
        {catalog?.accepts_full_names ? <option value={CUSTOM}>Full model name…</option> : null}
      </select>
      {custom ? (
        <input
          className="input mono"
          aria-label={`${id} full model name`}
          placeholder="claude-…"
          value={value ?? ""}
          onChange={(e) => onChange(e.target.value.trim() === "" ? null : e.target.value.trim())}
        />
      ) : null}
    </div>
  );
}

/**
 * Provider and actual model per workflow role. Model choices come from the
 * installed CLI (Claude's `--help` aliases, Codex's `model/list`); nothing is invented here.
 */
export function RoleBindingsEditor({
  value,
  onChange,
  idPrefix,
}: {
  value: RoleBindings;
  onChange: (b: RoleBindings) => void;
  idPrefix: string;
}) {
  const agents = useCoreQuery("agents", () => listAgents(false));
  const claude = useCoreQuery("models:claude_code", () => listModels("claude_code"));
  const codex = useCoreQuery("models:codex", () => listModels("codex"));
  const catalogs: Record<AgentProvider, ModelCatalog | null> = { claude_code: claude.data, codex: codex.data };
  const set = (key: BindingKey, b: RoleBinding | null) => {
    if (key === "correct") onChange({ ...value, correct: b });
    else if (b) onChange({ ...value, [key]: b });
  };
  return (
    <div className="role-bindings">
      {BINDING_KEYS.map((key) => {
        const sameAsImplement = key === "correct" && value.correct === null;
        const b = key === "correct" ? (value.correct ?? value.implement) : value[key];
        const id = `${idPrefix}-${key}`;
        const catalog = catalogs[b.provider];
        return (
          <div key={key} className="rule-form-grid role-binding-row">
            <div>
              <label htmlFor={`${id}-provider`}>{BINDING_LABEL[key]}</label>
              {key === "correct" ? (
                <label className="check small">
                  <input
                    type="checkbox"
                    checked={sameAsImplement}
                    onChange={(e) => set("correct", e.target.checked ? null : { ...value.implement })}
                  />{" "}
                  Same as implementation
                </label>
              ) : null}
            </div>
            <div>
              <select
                id={`${id}-provider`}
                className="input select"
                aria-label={`${BINDING_LABEL[key]} provider`}
                value={b.provider}
                disabled={sameAsImplement}
                onChange={(e) => set(key, { provider: e.target.value as AgentProvider, model: null })}
              >
                {(agents.data ?? []).map((a) => (
                  <option key={a.provider} value={a.provider} disabled={!a.installed || a.error !== null}>
                    {a.display_name}
                    {!a.installed || a.error ? " (unavailable)" : ""}
                  </option>
                ))}
              </select>
            </div>
            <div>
              {sameAsImplement ? (
                <p className="muted small">
                  {providerName(b.provider)} · {b.model ?? "CLI default"}
                </p>
              ) : (
                <ModelSelect
                  key={`${key}-${b.provider}`}
                  id={`${BINDING_LABEL[key]}`}
                  catalog={catalog}
                  value={b.model}
                  onChange={(model) => set(key, { ...b, model })}
                />
              )}
              {catalog?.error ? <p className="muted small">Models unavailable: {catalog.error}</p> : null}
            </div>
          </div>
        );
      })}
      <p className="muted small">
        Models are what the installed CLIs offer: Claude Code&apos;s <code>--model</code> aliases from its help (or a full model
        name), Codex&apos;s <code>model/list</code>. The chosen model is passed to the CLI exactly; an unknown one fails, it is
        never replaced.
      </p>
    </div>
  );
}
