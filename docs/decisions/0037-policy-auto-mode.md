# ADR 0037: Per-project auto mode

- Status: accepted (2026-11-22)

## Context

With the M4 defaults, every file change and every shell command asks a person. Rules can allow a
path, or one exact shell command, but a real session runs many different commands (`ls`, `npm test`,
`git diff`, pipelines). Codex reads files with shell commands too. In practice the operator approves
request after request by hand, which is slow and teaches them to approve without reading.

ADR 0016 deliberately refused `allow` for shell without an exact command. The operator asked for a
mode that stops asking, accepting that an approved command runs as their user anyway (ADR 0019).

## Decision

1. **A per-project switch, off by default.** `policies.set_auto_mode {project_id, enabled,
   expected_revision}` turns it on or off. It is versioned (`stale_revision` on a lost race),
   journaled (`policy.settings_changed`) and takes the next **global policy revision**, shared with
   rule changes, so an evaluation's `policy_revision` still names one exact policy state.
2. **It replaces only the `ask` default.** When no configured rule matches a file change
   (`file_write`, `file_edit`, `file_delete`) or a shell command, the decision is `allow` with the
   reason `auto_mode` instead of the `ask` default. Reads keep their own default.
3. **Everything else still applies:**
   - configured Ask and Block rules win (most restrictive matching rule, unchanged);
   - system denials (unknown tools, malformed requests, network and external tools, scope
     mismatches) are unchanged;
   - caps still turn the `allow` into `ask`: targets outside the workspace or unresolvable, writes to
     `.git`, unknown input fields and background commands. Only the exact-command cap is lifted,
     and only for an `allow` that came from auto mode;
   - review sessions are unaffected: file changes denied, commands ask.
4. **Core reads it, never the caller.** `evaluate_in_tx` reads the setting in the transaction that
   loads the rules, and records it in the evaluated input (`scope.auto_mode`). Validation commands
   (M7) read it the same way, so an `ask` command runs without operator authorization while auto
   mode is on; a blocked one still never runs.
5. **Pending approvals stay pending.** Turning auto mode on never answers a request a person was
   already asked; `reevaluate_pending` still may only deny. Turning it off needs no re-evaluation.
6. **Visible.** The project page and Policies show the switch; turning it on is confirmed in place
   with what it means. Sessions and tasks of the project carry an "Auto mode" badge while it is on.

## Consequences

- An agent in an auto-mode project can run any foreground command without a person seeing it
  first. A shell command's effects cannot be confined to the workspace (ADR 0019): it can change or
  delete files anywhere the user can and reach the network. The UI and docs say so.
- The audit trail stays complete: every evaluation records `reason: auto_mode`, the setting as
  evaluated, and the global revision.
- `SYSTEM_POLICY_VERSION` is 3. Migration 0011 adds the settings tables and rebuilds
  `policy_evaluations` so `reason` accepts `auto_mode` (rows copied unchanged).
- Not done: a per-task or per-session switch, a global switch, an expiry, or a built-in deny list of
  "dangerous" commands. Operators express those with Block rules.
