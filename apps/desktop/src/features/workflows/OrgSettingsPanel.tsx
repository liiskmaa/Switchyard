import { useState } from "react";

import { Button, ErrorNotice, Panel } from "../../components/ui";
import { providerName } from "../../lib/agents";
import { toCoreError, type CoreRequestError } from "../../lib/core";
import { useCoreQuery } from "../../stores/useCoreQuery";
import type { AgentProvider, OrgEstimate, OrgLimits, Project, SkillLevel } from "../../types/protocol";
import { orgSettings, saveOrgSettings } from "./api";

const LEVELS: SkillLevel[] = ["unknown", "basic", "good", "strong", "unsupported"];
const LEVEL_LABEL: Record<SkillLevel, string> = {
  unknown: "Not set",
  basic: "Basic",
  good: "Good",
  strong: "Strong",
  unsupported: "Never use for this",
};
const LIMITS: { key: keyof OrgLimits; label: string }[] = [
  { key: "max_agents", label: "Agents per task" },
  { key: "max_depth", label: "Levels of supervision" },
  { key: "max_tasks", label: "Subtasks per task" },
  { key: "max_corrections", label: "Corrections per subtask" },
  { key: "max_supervision_turns", label: "Supervision turns" },
  { key: "max_minutes", label: "Minutes before it pauses" },
];

/**
 * Project settings for agent organizations: limits, and the user's own view
 * of what each installed provider is good at. Switchyard ships no such
 * estimates; without them, it chooses by verified capabilities only.
 */
export function OrgSettingsPanel({ project }: { project: Project }) {
  const s = useCoreQuery(`org-settings:${project.id}`, () => orgSettings(project.id));
  const [limits, setLimits] = useState<Partial<OrgLimits> | null>(null);
  const [estimates, setEstimates] = useState<OrgEstimate[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<CoreRequestError | null>(null);
  const data = s.data;
  if (!data) return null;
  const l = { ...data.limits, ...(limits ?? {}) };
  const est = estimates ?? data.estimates;
  const providers = Array.from(new Set(data.profiles.map((p) => p.provider))) as AgentProvider[];
  const level = (provider: string, skill: string): SkillLevel =>
    est.find((e) => e.provider === provider && !e.model)?.skills[skill] ?? "unknown";
  const setLevel = (provider: string, skill: string, v: SkillLevel) => {
    const next = est.filter((e) => !(e.provider === provider && !e.model));
    const current = est.find((e) => e.provider === provider && !e.model);
    const skills = { ...(current?.skills ?? {}) };
    if (v === "unknown") delete skills[skill];
    else skills[skill] = v;
    if (Object.keys(skills).length) next.push({ ...(current ?? { provider }), provider, skills });
    setEstimates(next);
  };
  const dirty = limits !== null || estimates !== null;
  return (
    <Panel title="Agent teams">
      <p className="small muted">
        When a task is started with a team, its lead supervisor brings in agents for the work. Switchyard checks every plan
        against these limits and picks each agent&apos;s provider by what is verified (permission routing, resuming) and by
        your estimates below. It does not guess what a model is good at.
      </p>
      <div className="org-limits">
        {LIMITS.map(({ key, label }) => (
          <label key={key} className="field">
            <span className="small">{label}</span>
            <input
              className="input"
              type="number"
              min={1}
              value={String(l[key] ?? "")}
              onChange={(e) => setLimits({ ...(limits ?? {}), [key]: Number(e.target.value) })}
            />
          </label>
        ))}
      </div>
      <h3 className="policy-section">Your estimates</h3>
      <div className="org-estimates" role="group" aria-label="Capability estimates">
        <span />
        {providers.map((p) => (
          <strong key={p} className="small">
            {providerName(p)}
          </strong>
        ))}
        {data.skills.map((skill) => (
          <div key={skill} className="org-estimate-row">
            <span className="small">{skill.replace("-", " ")}</span>
            {providers.map((p) => (
              <select
                key={p}
                className="input"
                aria-label={`${providerName(p)}: ${skill}`}
                value={level(p, skill)}
                onChange={(e) => setLevel(p, skill, e.target.value as SkillLevel)}
              >
                {LEVELS.map((v) => (
                  <option key={v} value={v}>
                    {LEVEL_LABEL[v]}
                  </option>
                ))}
              </select>
            ))}
          </div>
        ))}
      </div>
      {error ? <ErrorNotice title="Not saved" message={error.message} code={error.code} /> : null}
      <div className="form-actions start">
        <Button
          variant="primary"
          disabled={!dirty || busy}
          onClick={() =>
            void (async () => {
              setBusy(true);
              setError(null);
              try {
                await saveOrgSettings(project.id, {
                  ...(limits ? { limits: l } : {}),
                  ...(estimates ? { estimates } : {}),
                });
                setLimits(null);
                setEstimates(null);
                s.refresh();
              } catch (e) {
                setError(toCoreError(e));
              } finally {
                setBusy(false);
              }
            })()
          }
        >
          Save
        </Button>
        {dirty ? (
          <Button
            variant="ghost"
            onClick={() => {
              setLimits(null);
              setEstimates(null);
            }}
          >
            Discard changes
          </Button>
        ) : null}
      </div>
    </Panel>
  );
}
