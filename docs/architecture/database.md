# Database schema

- **Engine:** SQLite (bundled), WAL, `synchronous=FULL`, foreign keys on.
- **Migrations:** `migrations/000N_*.sql`, embedded and tracked with `PRAGMA user_version`. They
  are forward-only and never edited once applied.
- **Conventions:** ids are UUIDv7 text, times are Unix ms, enums are `TEXT` with `CHECK`, and
  structured payloads are JSON text.

| Version | File | Content |
|---|---|---|
| 1 | `0001_initial.sql` | All tables; only `projects` and `events` written |
| 2 | `0002_agent_sessions.sql` | `sessions` and `approvals` rebuilt for the agent lifecycle; agent/workspace indexes |
| 3 | `0003_workspaces.sql` | `workspaces` rebuilt for isolation; legacy records; session/workspace guards |
| 4 | `0004_policies.sql` | Approval policies: `policy_rules`, `policy_rule_revisions`, `policy_evaluations`; `approvals` rebuilt (`evaluation_id`, source `policy`); the never-written `permissions` table dropped |

## Tables in use

### `projects`
Registered repositories: `id`, `name`, `root_path` (unique, canonical), timestamps.

### `workspaces` (v3)

| Column | Notes |
|---|---|
| `id` | Stable id, independent of branch and path |
| `project_id` | → `projects` (`ON DELETE CASCADE`; removal is refused while worktrees remain) |
| `kind` | `worktree` \| `legacy_checkout` |
| `name` | Display label, 1–80 characters |
| `path` | **Unique.** Canonical working directory = worktree root = agent cwd. The spec's `path` and `worktree_path` are the same value, so there is one column |
| `branch` | `switchyard/<id>`; `NULL` only for legacy rows. Unique per project |
| `base_ref`, `base_commit` | Revision requested and the commit it resolved to |
| `status` | See [workspaces.md](workspaces.md) |
| `status_reason` | `code: detail` |
| `dirty`, `git_checked_at` | Last Git observation |
| `created_at`, `updated_at`, `last_activity_at`, `archived_at` | `archived_at` is required for `archived`/`cleanup_pending`/`removed` |

Constraints: a `worktree` row must have `branch` and `base_commit`. Indexes:
`workspaces_project (project_id, created_at)`; unique `workspaces_branch (project_id, branch)`.

### `sessions` (v2, with v3 guards)

- **Contents:** one agent execution: provider, native session id (unique per provider), `cwd`,
  state, pid, timing, exit status and failure.
- **`workspace_id` → `workspaces`.**
  - Trigger `sessions_require_workspace` rejects new rows without one.
  - Unique partial index `sessions_live_workspace` allows at most one session per workspace in a
    live state (`starting`, `initializing`, `idle`, `running`, `waiting_for_approval`,
    `interrupted`).

### `approvals` (v4)
- Permission requests that a policy evaluation answered with `ask`. Each is settled exactly once
  (`status = 'pending'` guard) and records its source:
  - operator, timeout, agent cancelled or exited, Core shutdown or restart;
  - or `policy`, when a policy change denied it (a `CHECK` allows `policy` only with `denied`).
- `evaluation_id` links to the evaluation that created the approval. It is `NULL` for approvals
  recorded before v4; those were carried over unchanged, pending ones included.

### `policy_rules` (v4)
- Current state of each configured rule: `project_id`; `workspace_id` (`NULL` = project rule);
  `category`; `decision`; `conditions` JSON (`path_prefix`, `command`); `description`; `enabled`;
  `status` (`active` or `archived`); and `revision`.
- `CHECK`s forbid `unknown`, a shell `allow` without a command, and `allow` for network or
  external tools.
- Triggers:
  - a workspace rule's workspace must belong to the rule's project;
  - scope and category never change.

### `policy_rule_revisions` (v4)
- Append-only (triggers): one full rule snapshot per change (`created`, `updated`, `enabled`,
  `disabled`, `archived`).
- `policy_revision` is the global policy revision (`UNIQUE`, `MAX + 1` inside the changing
  transaction).
- No foreign keys: history survives a project's removal.

### `policy_evaluations` (v4)
- Append-only (triggers); no foreign keys.
- One row per evaluation (trigger `request`, `reevaluation` or `decision_check`), recording:
  - project, workspace, session and agent;
  - adapter, tool and native request id;
  - category;
  - `input`: the normalized operation and path facts, with no file contents;
  - decision, reason and explanation;
  - the deciding rule and its revision;
  - the matched rules as evaluated;
  - restrictions;
  - the global policy revision and system policy version;
  - the approval it created or settled.

### `agents`
One row per detected executable and vendor.

### `events`
- The append-only journal (triggers block `UPDATE`/`DELETE`).
- Its context columns (`project_id`, `workspace_id`, `session_id`, …) have no foreign keys: the
  history outlives entities.
- Indexes per project, type and session.

### Tasks (migration 0005, milestone 6)
- `tasks`: status, revision (optimistic edits), result, origin.
- `task_dependencies`: same-project trigger, no self-edges; cycles are checked in Core.
- `task_assignments`: at most one open assignment per task, enforced by a unique partial index;
  the context as sent; `last_event_seq` for idempotent replay.
- `messages`: one agent message per turn, enforced by a unique index on
  `(assignment_id, causation_id)`.
- `findings` and `task_proposals`.

Migration 0005 replaces the never-written milestone-1 placeholders `tasks`, `messages` and
`findings`, adds `sessions.access` (`write` | `review`) and an `events_task` index. See
[tasks.md](tasks.md).

### Integrations (migration 0006, milestone 7)
- `integrations`:
  - bound `source_commit` / `target_commit`;
  - `preflight` (facts and blockers);
  - `merge_commit` recorded before the branch moves;
  - one unfinished integration per workspace (unique partial index);
  - status changes guarded by status and revision.
- `integration_settings` (`require_review`) and `validation_commands` (argv, timeout).
- `validation_runs` (bound to a source commit) and `validation_steps` (policy decision, authorizer,
  exit status, bounded output).
- `task_assignments.review_tree`: the Git tree a review saw. See [integrations.md](integrations.md).

### Integration operations (migration 0007)
- `integration_operations`: each Core-owned operation (ADR 0032) with kind, status
  (`running` | `completed` | `failed` | `interrupted`), requester, error and the workspace status it
  replaced. At most one `running` per integration (unique partial index). Rows still `running` at
  startup were interrupted and are settled from Git.

### Workflows (migration 0008)
- `task_workflows`: one `engineering@1` run per task: snapshotted role bindings (`config`), the pure
  workflow state, status, stage, current step, reason, the last journal event processed and a
  revision guard. See [workflows.md](workflows.md).
- `workflow_settings`: a project's provider + model per role.
- `task_assignments.origin` (`operator` | `workflow`), `step_key` (unique per task) and `model`.

### Reserved, not written yet
`decisions`.
