# ADR 0016: Approval policy model and precedence

- Status: accepted (2026-11-11)
- Related: [0017](0017-normalized-operations.md) (what is evaluated),
  [0018](0018-policies-and-human-approvals.md) (how results meet approvals),
  [0019](0019-policy-threat-model.md) (what policies do not protect against).

## Context

Since milestone 2, every agent permission request that reaches Switchyard needs an operator decision.
Reads inside the working directory never reached Switchyard: Claude allowed them by itself. Milestone 4
adds configurable rules so that routine requests need fewer decisions, without granting agents more
than the system permits.

### Existing infrastructure

- **Reused:**
  - `approvals`, with its SQL-guarded `pending → final` transition, its expiry timer, cancellation and
    shutdown paths, and the `agent.permission_*` events;
  - the transactional outbox (`journal::commit`).
- **Replaced:** the `permissions` table that migration 0001 reserved (`rule TEXT`, `effect`,
  nullable project or agent).
  - No Core version has ever read or written it.
  - Its free-text `rule` column is the "policy language" this design avoids.
  - It cannot express a workspace scope, a revision, or an enabled flag.
  - Migration 0004 drops it, but only after checking that it is empty: a non-empty table aborts the
    migration instead of losing data.

## Decision

### Three layers, never mixed

1. **System restrictions:** compiled into Core, not stored, not configurable, with no UI control to
   disable them. Each produces `deny` or caps `allow` at `ask` (ADR 0017). They are versioned by
   `SYSTEM_POLICY_VERSION`, which is recorded with every evaluation.
2. **Configured rules:** structured rows, scoped to one project or to one workspace of that project.
3. **Human decisions:** apply to a single approval only. They never create or change a rule
   (ADR 0018).

**Built-in defaults** are part of layer 1 but behave differently: they apply only when no configured
rule matches. They are listed read-only in the UI:

| Category | Default |
|----------|---------|
| `file_read`, `search`, `directory_list` | allow, if every target is inside the session's workspace; otherwise ask |
| `file_write`, `file_edit`, `file_delete` | ask |
| `shell` | ask |
| `network`, `external_tool` | deny (the Claude adapter does not enable these tools, so a request is unexpected; a rule may relax this to ask, never to allow) |
| `unknown` | deny (a system restriction, not a default; no rule can target it) |

**Why reads default to allow.** Until now Claude allowed Read, Glob and Grep inside the working
directory natively; Switchyard never saw them. Milestone 4 routes them through Switchyard (ADR 0017),
so the default reproduces the old behaviour: no new capability is granted. What is new is that reads
are now evaluated, recorded, and can be blocked or made to ask by a rule. Reads outside the workspace
are refused by Claude itself under `--restricted` (verified with 2.1.322). If one ever reaches
Switchyard, the default is `ask`.

### Rules

| Field | Meaning |
|-------|---------|
| `id` | UUIDv7, stable for the rule's whole life |
| `project_id` | Always set. A workspace rule also carries its project. |
| `workspace_id` | `NULL` = project rule. Otherwise it must name a workspace of `project_id`; Core checks this and a trigger enforces it. |
| `category` | A normalized operation category (ADR 0017), not a vendor tool name, so rules carry over to other adapters |
| `decision` | `allow`, `ask` or `deny` |
| `conditions` | `{path_prefix?: "rel/dir", command?: "exact command"}`. Nothing else is accepted. |
| `description` | Required human text |
| `enabled`, `status` (`active` or `archived`) | Archived rules are kept for history and never evaluated |
| `revision` | Starts at 1 and increments on every change to the rule |

- **No session scope.** A session already lives in exactly one workspace, and a workspace has at
  most one live session, so a workspace rule covers the use case. A session scope would add a third
  layer of precedence for no new capability.
- **No tool-name field and no wildcards.** Categories plus the two conditions are the whole
  matching vocabulary. Nothing in a rule is executable, interpreted or pattern-matched beyond exact
  path-component prefixes and exact command equality.

### Validation (on create and update; the evaluator re-checks)

- **`path_prefix`:** only for path categories. It must be relative and normalized: no `..`, no `.`,
  no empty components, no leading `/`, and no NUL. At most 512 bytes.
- **`command`:** only for `shell`. It is compared byte for byte. It is validated against an
  allowlist rather than a list of forbidden characters:
  - Words are separated by single spaces. Each word uses only ASCII letters, digits and
    `_ . / : @ % + , = -`.
  - The first word starts with a letter, a digit, `.`, `_` or `/`, and contains no `=` (this rules
    out `VAR=value cmd`). No word starts with `=` (zsh expands `=cmd`).
  - So quotes, `$`, backquotes, `;`, `&`, `|`, redirections, globs, `~`, `!`, `#`, braces and
    non-ASCII text can never appear in an allowed command.
  - At most 512 bytes.
- **A `shell` rule with `allow` must have `command`.** There is no broad Bash allow.
- **`allow` is refused for `network`, `external_tool` and `unknown`.** No rule of any kind may
  target `unknown`.
- **Conflicts:** an active rule with the same scope, category and conditions as another active rule
  is rejected (`policy_conflict`). The rule should be edited instead. Duplicates would make history
  harder to read; evaluation would still be deterministic without this check.

### Evaluation and precedence (pure function, `switchyard-policy::evaluate`)

1. **System restrictions** (deny): unclassified or malformed operation, `unknown` category, a tool
   the adapter did not enable, a missing workspace, or a workspace/project mismatch. Nothing below
   can change this result.
2. Collect the enabled, active rules whose scope applies:
   - project rules of the session's project;
   - workspace rules of the session's workspace only.

   Keep those whose category and conditions match.
3. **Most restrictive matching rule wins: deny > ask > allow.** Scope does not matter: a workspace
   `allow` cannot loosen a project `deny`/`ask`, and a project `allow` cannot loosen a workspace
   `deny`/`ask`. Among rules with the same decision, the deciding rule is the workspace rule first,
   then the oldest by `(created_at, id)`. This affects only which rule is cited; the decision is the
   same either way.
4. **No matching rule:** the built-in default for the category.
5. **Caps** (system restrictions that turn `allow` into `ask`, never into `allow`). They apply
   whether the `allow` came from a rule or from a default:
   - a target path outside the workspace, or one that can't be resolved;
   - a write, edit or delete under a `.git` path component;
   - tool input fields the adapter does not know;
   - a shell command running in the background;
   - for `shell`: anything other than an exact match of a validated command.
6. **Fail closed:** any internal inconsistency is a `deny`.

**Matching is asymmetric on purpose.** For `deny` and `ask` rules, `path_prefix` matches
case-insensitively, against both the lexical path and the symlink-resolved path. For `allow` rules,
it must match case-sensitively on both. Ambiguity therefore always lands on the restrictive side.
This matters on macOS's case-insensitive APFS and with symlinks inside the workspace.

A `deny` or `ask` path condition matches when **any** target of the operation matches it. An
`allow` path condition matches only when **every** target does. An operation with no path targets
never matches a rule that has a path condition.

A `command` condition on a `deny` or `ask` rule is compared after trimming and collapsing runs of
whitespace. An `allow` command must match byte for byte. A deny for one exact command blocks only that
text; to block the shell, use a `shell` deny without a command.

There is no "last rule wins" and no rule priority field.

### Revisions

- Every rule change is one transaction that:
  - updates `policy_rules`;
  - appends a full snapshot to `policy_rule_revisions` (append-only, enforced by triggers);
  - takes the next **global policy revision** (`MAX + 1`, `UNIQUE`);
  - journals `policy.rule_changed`.
- An update must name the `expected_revision` of the rule. Otherwise it fails with `stale_revision`,
  so two editors never silently overwrite each other.
- Every evaluation records:
  - the global policy revision it saw;
  - `SYSTEM_POLICY_VERSION`;
  - each matched rule's id, revision, decision, scope and description at that moment.

  The rule set behind any past decision can be reconstructed from `policy_rule_revisions`. Editing
  a rule later does not change history.

### Protocol

These changes are incompatible for v2 clients:
- new approval decision source `policy`;
- `approvals.decide` can be refused with `policy_denied`;
- requests that used to create approvals may now be decided automatically.

The protocol therefore moves to **version 3**, with minimum 3 on both sides (the ADR 0015
mechanism). New methods: `policies.list`, `get`, `create`, `update`, `set_enabled`, `archive`,
`effective`, `evaluations`.

## Consequences

- Configuring a policy can only remove prompts for path-scoped file operations inside a workspace
  and for exact, metacharacter-free shell commands. Everything else still asks or is denied.
- A broad project `ask` makes narrower `allow` rules ineffective, because the most restrictive rule
  wins. Since the built-in default for writes and shell is already `ask`, broad `ask` rules are only
  needed to tighten reads. The UI explains this.
- Rules are per project. Global user rules are deferred: they would need a fourth scope and are not
  needed to make the model useful.
