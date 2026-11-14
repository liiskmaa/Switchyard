import { useState } from "react";

import { Button } from "../../components/ui";
import { providerName } from "../../lib/agents";
import { useCoreQuery } from "../../stores/useCoreQuery";
import type { AgentProvider, ModelCatalog, RoleBinding, RoleBindings } from "../../types/protocol";
import { listAgents } from "../agents/api";
import { listModels } from "./api";
import { BINDING_LABEL, modelLabel, type BindingKey } from "./labels";

const CUSTOM = "__custom__";
const DEFAULT = "";

const ROLE_HINT: Record<BindingKey, string> = {
  investigate: "Reports findings",
  implement: "Changes the code",
  review: "Read-only verdict",
  correct: "Fixes review findings",
};

/**
 * The model choice for one role: one dropdown. It lists the CLI default, the
 * models the installed CLI offers, and a full model name already chosen.
 * "Other full model name…" turns it into a text field only while typing
 * (Enter or leaving the field keeps the name, Escape cancels).
 */
function ModelSelect({
  label,
  catalog,
  value,
  onChange,
}: {
  label: string;
  catalog: ModelCatalog | null;
  value: string | null;
  onChange: (model: string | null) => void;
}) {
  const [typing, setTyping] = useState(false);
  const [name, setName] = useState("");
  const listed = catalog?.models ?? [];
  const loading = catalog === null;
  const isListed = value === null || listed.some((m) => m.id === value);
  const def = listed.find((m) => m.is_default);
  if (typing) {
    const commit = () => {
      setTyping(false);
      if (name.trim() !== "") onChange(name.trim());
    };
    return (
      <input
        className="input mono"
        aria-label={`${label} full model name`}
        placeholder="Full model name, e.g. claude-sonnet-4-5 — Enter to keep, Esc to cancel"
        value={name}
        autoFocus
        onChange={(e) => setName(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            commit();
          } else if (e.key === "Escape") {
            setTyping(false);
          }
        }}
      />
    );
  }
  return (
    <select
      className="input select"
      aria-label={`${label} model`}
      value={value ?? DEFAULT}
      disabled={loading}
      onChange={(e) => {
        const v = e.target.value;
        if (v === CUSTOM) {
          setName(isListed ? "" : (value ?? ""));
          setTyping(true);
          return;
        }
        onChange(v === DEFAULT ? null : v);
      }}
    >
      {loading ? <option value={value ?? DEFAULT}>{value ?? "Loading models…"}</option> : null}
      <option value={DEFAULT}>CLI default{def ? ` · ${def.display_name}` : ""}</option>
      {listed.length > 0 ? (
        <optgroup label="Offered by the installed CLI">
          {listed.map((m) => (
            <option key={m.id} value={m.id}>
              {m.id}
              {m.display_name !== m.id && !m.display_name.startsWith(m.id) ? ` · ${m.display_name}` : ""}
            </option>
          ))}
        </optgroup>
      ) : null}
      {!loading && !isListed && value ? (
        <optgroup label={catalog.accepts_full_names ? "Full model name" : "Not offered by the CLI"}>
          <option value={value}>
            {value}
            {catalog.accepts_full_names ? " · full name" : " · not offered"}
          </option>
        </optgroup>
      ) : null}
      {catalog?.accepts_full_names ? <option value={CUSTOM}>Other full model name…</option> : null}
    </select>
  );
}

function ProviderSelect({ label, value, onChange }: { label: string; value: AgentProvider; onChange: (p: AgentProvider) => void }) {
  const agents = useCoreQuery("agents", () => listAgents(false));
  return (
    <select
      className="input select"
      aria-label={`${label} provider`}
      value={value}
      onChange={(e) => onChange(e.target.value as AgentProvider)}
    >
      {(agents.data ?? [{ provider: value, display_name: providerName(value), installed: true, error: null }]).map((a) => (
        <option key={a.provider} value={a.provider} disabled={!a.installed || a.error !== null}>
          {a.display_name}
          {!a.installed || a.error ? " (unavailable)" : ""}
        </option>
      ))}
    </select>
  );
}

/**
 * Provider and model per workflow role, as one aligned table. Correction
 * inherits the implementation's binding unless the user overrides it.
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
  const claude = useCoreQuery("models:claude_code", () => listModels("claude_code"));
  const codex = useCoreQuery("models:codex", () => listModels("codex"));
  const catalogs: Record<AgentProvider, ModelCatalog | null> = { claude_code: claude.data, codex: codex.data };
  const rows: BindingKey[] = ["investigate", "implement", "review"];
  const row = (key: BindingKey, b: RoleBinding, set: (b: RoleBinding) => void) => {
    const catalog = catalogs[b.provider];
    return (
      <div key={key} className="role-row">
        <div className="role-name">
          <span className="role-title">{BINDING_LABEL[key]}</span>
          <span className="role-hint">{ROLE_HINT[key]}</span>
        </div>
        <ProviderSelect label={BINDING_LABEL[key]} value={b.provider} onChange={(provider) => set({ provider, model: null })} />
        <div>
          <ModelSelect
            key={`${idPrefix}-${key}-${b.provider}`}
            label={BINDING_LABEL[key]}
            catalog={catalog}
            value={b.model}
            onChange={(model) => set({ ...b, model })}
          />
          {catalog?.error ? <p className="role-note">Model list unavailable: {catalog.error}</p> : null}
        </div>
      </div>
    );
  };
  return (
    <div className="role-table" role="group" aria-label="Workflow roles">
      <div className="role-row role-head" aria-hidden="true">
        <span>Role</span>
        <span>Provider</span>
        <span>Model</span>
      </div>
      {rows.map((key) => row(key, value[key] as RoleBinding, (b) => onChange({ ...value, [key]: b })))}
      {value.correct === null ? (
        <div className="role-row">
          <div className="role-name">
            <span className="role-title">{BINDING_LABEL.correct}</span>
            <span className="role-hint">{ROLE_HINT.correct}</span>
          </div>
          <div className="role-inherit">
            <span>
              Inherits Implementation · <strong>{providerName(value.implement.provider)}</strong> ·{" "}
              <span className="mono">{modelLabel(value.implement.model)}</span>
            </span>
            <Button variant="ghost" onClick={() => onChange({ ...value, correct: { ...value.implement } })}>
              Override
            </Button>
          </div>
        </div>
      ) : (
        <>
          {row("correct", value.correct, (b) => onChange({ ...value, correct: b }))}
          <div className="role-row role-row-tail">
            <span />
            <div>
              <Button variant="ghost" onClick={() => onChange({ ...value, correct: null })}>
                Inherit from Implementation
              </Button>
            </div>
          </div>
        </>
      )}
    </div>
  );
}

/** Where model choices come from, shown once under the table. */
export function ModelSourceNote() {
  return (
    <p className="role-note">
      Model choices are read from the installed CLIs: Claude Code&apos;s <code>--model</code> aliases and Codex&apos;s{" "}
      <code>model/list</code>. A full model name is passed to the CLI exactly; an unknown one fails the session and is
      never replaced. <em>CLI default</em> lets the CLI decide.
    </p>
  );
}
