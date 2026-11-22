# Approval policies

**Approval policies are an authorization and workflow mechanism, not an OS-level sandbox.** They
decide what Switchyard answers when an agent asks to use a tool. An allowed or approved command
runs with your user's privileges. It can read and change files outside its workspace and reach the
network. Execution isolation is a separate, future mechanism ([ADR 0019](../decisions/0019-policy-threat-model.md)).

Design: [ADR 0016](../decisions/0016-approval-policy-model.md) (model and precedence),
[ADR 0017](../decisions/0017-normalized-operations.md) (operations),
[ADR 0018](../decisions/0018-policies-and-human-approvals.md) (approvals and audit).

## The path of one request

1. **Request.** Claude Code sends `can_use_tool`; every enabled tool reaches Switchyard, because
   the launch settings carry an `ask` rule for each one. Codex sends an approval request for every
   command and every patch, because its threads run with `approvalPolicy: "untrusted"` (ADR 0021).
2. **Classification.** The session's adapter (`switchyard_agents::classify`, chosen by the provider
   in Core's own session record) turns the request into an `Operation`: category, target paths,
   command, and unknown input fields. Unknown tools, tools not enabled at launch, malformed input,
   and for Codex mixed-kind patches, file moves and sub-agent requests get a `problem`. Codex
   commands are always `shell`, even when they only read (`cat`); its patches are `file_write`,
   `file_edit` or `file_delete` by change kind.
3. **Scope.** Core reads the session's project and workspace from its own records. It computes the
   **path facts** for each target, lexically and after following symlinks: inside the workspace,
   outside, or unresolvable.
4. **Evaluation.** In one SQLite transaction, Core loads the enabled, active rules of the project
   and of that workspace, together with the current global revision. It runs the pure evaluator,
   then writes a `policy_evaluations` row and `agent.permission_evaluated`.
5. **Answer:**
   - **allow:** the agent is told "allow" at once; no approval is created.
   - **ask:** an approval is created in the same transaction and a person decides, exactly as
     before milestone 4. Expiry, cancellation and shutdown all still deny.
   - **deny:** the agent is told "deny" with the explanation (Codex is only told "declined"; it does
     not receive the explanation). No approval is created, so there is
     nothing to approve.
6. **Fail closed.** If any step fails (classification, storage, recording), the agent is denied.

## Precedence

1. **System restrictions deny.** Nothing can change this for:
   - an unclassifiable or malformed request;
   - an unknown tool, or one not enabled at launch;
   - a missing workspace, or a workspace/project mismatch.
2. **Most restrictive matching rule:** deny > ask > allow, regardless of scope.
   - A workspace rule cannot loosen a project rule, and the reverse holds too.
   - Among equal decisions, the cited rule is the workspace rule first, then the oldest.
3. **No matching rule: the built-in default.**

   | Operation | Default |
   |-----------|---------|
   | Read files, search, list directories | Allow, inside the workspace |
   | Create, edit or delete files | Ask |
   | Shell commands | Ask |
   | Network, external (MCP) tools | Block (not enabled for Claude Code) |
   | Unrecognized | Block (system) |

   **Auto mode** (per project, off by default, [ADR 0037](../decisions/0037-policy-auto-mode.md)):
   with no matching rule, file changes and shell commands get `allow` (reason `auto_mode`)
   instead of the `ask` default, review sessions' shell commands included (their file changes stay
   denied; [ADR 0042](../decisions/0042-auto-mode-releases.md)). It never overrides a rule.

4. **Caps: an `allow` becomes `ask`** when:
   - a target is outside the workspace or unresolvable;
   - a write, edit or delete targets `.git`;
   - the input has unknown fields;
   - a shell command would run in the background (lifted in auto mode);
   - a shell command is not an exact match of a validated command rule (lifted when the `allow`
     came from auto mode);
   - in auto mode, a shell command commits, pushes, publishes or deploys (`release_action`,
     ADR 0042), unless an exact-command rule allows it.

## Rules

- **Scope:** a project rule applies to every workspace of the project. A workspace rule applies to
  that workspace only, and its workspace must belong to the project (checked by Core and by a SQL
  trigger).
- **Matching:** an operation category plus optional conditions:
  - `path_prefix`: a relative directory or file inside the workspace (file and search operations);
  - `command`: an exact shell command made of plain words (`shell` only).

  Nothing else is accepted, and nothing is executed or interpreted.
- **Matching leans restrictive:**
  - Block and Ask rules match paths case-insensitively, against the requested or the resolved
    path, and fire if any target matches. Their commands are compared with whitespace collapsed.
  - Allow rules need an exact, case-sensitive match of every target on both the requested and the
    resolved path, and a byte-exact command.
- **Not allowed:** `allow` for shell without an exact command, `allow` for network or external
  tools, and any rule for unrecognized operations. Duplicates (same scope, category and
  conditions) are refused as `policy_conflict`.
- **Revisions:**
  - Every change (create, update, enable, disable, archive) increments the rule's revision and the
    global policy revision, and appends a full snapshot to `policy_rule_revisions`.
  - Updates must name `expected_revision`, otherwise they fail with `stale_revision`.
  - Scope and category never change; create a new rule instead.

## Auto mode

- Switched per project with `policies.set_auto_mode` (Projects or Policies in the app). The change
  is versioned, journaled as `policy.settings_changed`, and takes the next global policy revision.
- Core reads it inside the evaluating transaction and records it in the input (`scope.auto_mode`).
  Validation commands (M7) follow it too: an `ask` command runs without authorization.
- Since ADR 0042 it also allows review sessions' shell commands and background commands.
- Still asking: commits, pushes, publishing and deployments (`release_action`), paths outside the
  workspace or unresolvable, `.git` writes, unknown input fields, and any configured Ask rule.
  Still blocked: Block rules, review sessions' file changes, network and external tools,
  everything the system denies.
- Turning it on does not answer pending approvals.
- **An auto-allowed shell command can do anything your user can**, anywhere on the machine. Use
  Block rules for commands you never want run.

## People and policies

- Approving or denying a request decides that request only. It never creates or edits a rule, and
  there are no "always allow" shortcuts.
- **A pending approval keeps its original evaluation.** After every rule change, Core re-evaluates
  the project's pending approvals:
  - **blocked now:** the approval is denied (source `policy`) and the agent is told why;
  - **allowed or asked now:** it stays pending for the person who was asked.
- When someone approves, Core re-checks once more in the same transaction. If the policy now blocks
  the request, the approval is denied and the call fails with `policy_denied`.

## Audit

| Question | Where |
|----------|-------|
| What did the agent request? | `policy_evaluations.input` (operation and path facts, never file contents); the approval's `request` for asks |
| Which project, workspace and session? | `policy_evaluations` columns, set from Core's records |
| Which policy revision, which rules? | `policy_revision`, `system_version`, `matched_rules` (each with its revision as evaluated), `deciding_rule_*`; `policy_rule_revisions` reconstructs the full rule set at any revision |
| Why? | `reason` and `explanation`, plus `restrictions` |
| Was a person involved, and what was final? | `approval_id` → `approvals` (status, source, decided by, note); allow and deny need no person |
| When? | `evaluated_at`, approval timestamps, journal `seq` |

All of these records are append-only, so editing a rule later never changes a past decision.

## What policies do not protect against

See [ADR 0019](../decisions/0019-policy-threat-model.md). In short:
- the effects of allowed commands (an exact `cargo test` runs whatever the build and the tests do);
- time-of-check to time-of-use changes to symlinks;
- Unicode normalization variants in path names;
- managed Claude settings;
- for Codex: an approved command or patch can run outside Codex's own sandbox (verified), what the
  model sends to OpenAI, and sub-agent spawning (sub-agents' requests are denied, see
  [codex-integration.md](codex-integration.md));
- anyone with access to the local user account.
