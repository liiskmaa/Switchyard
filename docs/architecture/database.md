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

### `approvals` (v2)
Permission requests and their decisions, settled exactly once (`status = 'pending'` guard). Each
records its source: operator, timeout, agent cancelled or exited, Core shutdown or restart.

### `agents`
One row per detected executable and vendor.

### `events`
- The append-only journal (triggers block `UPDATE`/`DELETE`).
- Its context columns (`project_id`, `workspace_id`, `session_id`, …) have no foreign keys: the
  history outlives entities.
- Indexes per project, type and session.

### Reserved, not written yet
`tasks`, `messages`, `findings`, `permissions`, `decisions`.
