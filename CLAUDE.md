# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Switchyard orchestrates **native** coding agents (Claude Code, Codex CLIs) from a separate long-running
Rust process, **Switchyard Core**. The Tauri desktop app is only a client.

- Milestone 1: Core, IPC, SQLite, the event engine, projects, and the desktop shell.
- Milestone 2: Claude Code sessions (`crates/switchyard-agents` and `crates/switchyard-core/src/agents`).
- Milestone 3: workspace isolation: Git worktrees per workspace (`crates/switchyard-workspaces`,
  `crates/switchyard-git/src/worktree.rs`, `crates/switchyard-core/src/workspaces`). See
  `docs/architecture/workspaces.md` and ADRs 0012–0014.
- Milestone 4: approval policies (`crates/switchyard-policy`, `crates/switchyard-agents/src/claude/classify.rs`,
  `crates/switchyard-core/src/policy`, `migrations/0004_policies.sql`). See `docs/architecture/policies.md`
  and ADRs 0016–0019.
- Milestone 5: Codex sessions through `codex app-server` (JSON-RPC over stdio) in
  `crates/switchyard-agents/src/codex` (`protocol.rs` is a pure state machine, `classify.rs` maps
  approvals to operations). See `docs/architecture/codex-integration.md` and ADRs 0020–0022.
- Milestone 6: tasks and multi-agent collaboration (`crates/switchyard-tasks` is pure: state machine,
  graph, report parser, context renderer; `crates/switchyard-storage/src/tasks.rs`,
  `crates/switchyard-core/src/tasks/`, `migrations/0005_tasks.sql`, `apps/desktop/src/features/tasks`).
  See `docs/architecture/tasks.md` and ADRs 0023–0027.
- Milestone 7: change integration (`crates/switchyard-integrations` is pure: state machine, blockers,
  validation rules; `crates/switchyard-git/src/integrate.rs`, `crates/switchyard-core/src/integrations/`,
  `migrations/0006_integrations.sql`, `migrations/0007_integration_operations.sql`,
  `apps/desktop/src/features/integrations`). See `docs/architecture/integrations.md` and ADRs 0028–0032.
- Milestone 8.1: workflow orchestration (`crates/switchyard-tasks/src/workflow.rs` is the pure
  `engineering@1` decision function; `crates/switchyard-core/src/tasks/workflow.rs` the driver;
  `crates/switchyard-agents/src/models.rs` + `codex/models.rs` the model choices of the installed CLIs;
  `migrations/0008_workflows.sql`; `apps/desktop/src/features/workflows`). See
  `docs/architecture/workflows.md` and ADR 0033.
- Milestone 8.3: agent organizations (`crates/switchyard-tasks/src/organization/` is the pure
  `organization@1` decision function, directive parser and prompt renderer;
  `crates/switchyard-agents/src/profiles.rs` capability profiles and selection;
  `crates/switchyard-core/src/tasks/organization.rs` the driver; `migrations/0010_organizations.sql`;
  `apps/desktop/src/features/workflows/OrganizationPanel.tsx`). See `docs/architecture/organizations.md`
  and ADRs 0035–0036.
- engineering@2 (ADRs 0037–0040): per-project auto mode (`switchyard-policy`, `PolicyService::set_auto_mode`,
  `migrations/0011_policy_auto_mode.sql`); review findings and failed tests go to reasoning (`triage` reports, no
  correction role); Core's test gate before every review (`crates/switchyard-core/src/tasks/tests.rs`,
  `migrations/0012_workflow_tests.sql`); reasoning's plan of 2–8 subtasks runs in turn (`s<n>/` step keys); the task
  page's flow display (`apps/desktop/src/features/workflows/flow.ts`, `TaskFlow.tsx`); Core start/stop/restart from
  Settings (`apps/desktop/src-tauri/src/commands.rs`: `core_stop`, `core_start`, `core_restart`).
- ADR 0043: context v4 `investigation` (report + `files_read` from the journal) for coding and review;
  step-through mode (`WorkflowState.step_through`, `workflows.set_step_through`, `workflows.preview_next`
  built by `tasks/assign.rs::build_context`, the same code that starts a step).
- Stuck-work watchdog (ADR 0045): pure `crates/switchyard-tasks/src/watchdog.rs` (`check`, `Limits`), Core loop
  `crates/switchyard-core/src/tasks/watchdog.rs` (`watchdog.list`, `task.stuck_changed`), team input
  `organization::Input::Nudge`, UI `features/workflows/StuckCard.tsx`. Its only automatic remedies: end an orphaned
  workflow step, hand a bouncing team to its lead; never stop a session or approve anything.
- Gemini CLI over ACP (ADR 0044): `crates/switchyard-agents/src/acp/` (pure `protocol.rs`, `launch.rs` with the admin
  policy, `classify.rs`, `models.rs`), provider `gemini`, `migrations/0014_gemini_provider.sql` (the runner's
  `-- switchyard: foreign_keys=off` marker for table rebuilds), `src/bin/fake_gemini.rs`. See
  `docs/architecture/acp-integration.md`.
- Task supervisor (ADR 0041): `crates/switchyard-tasks/src/supervisor.rs` (pure prompt + reply parser),
  `crates/switchyard-core/src/tasks/supervisor.rs`, `migrations/0013_task_supervisor.sql`, the pure
  `workflow::Input::Directed`, `apps/desktop/src/features/workflows/SupervisorBox.tsx`.

The verified Claude CLI behaviour is in `docs/architecture/agent-integration.md`, the verified Codex
app-server behaviour (0.166.0) in `docs/architecture/codex-integration.md`, and the contract in
`docs/protocols/agent-adapter-contract.md`.

## Commands

```sh
npm install                      # requires npm >= 11 (npm 10.8 crashes resolving the tree)
npm run dev                      # cargo build -p switchyard-core && tauri dev (Vite on :1420)
npm test                         # cargo test --workspace && vitest
npm run lint                     # cargo fmt --check, clippy --all-targets -D warnings, tsc --noEmit
npm run build                    # release bundle with Core as a Tauri externalBin sidecar

cargo test -p switchyard-core --test events             # one integration test file
cargo test -p switchyard-core --test projects vanished  # one test by name filter
cargo test -p switchyard-storage projects::tests        # one module's unit tests
npm run test:web                                        # vitest only (apps/desktop/src/**/*.test.ts)
cargo test -p switchyard-core --test agents             # agent sessions via the fake CLI
cargo test -p switchyard-agents claude::wire            # parser against recorded real transcripts
cargo test -p switchyard-core --test workspaces         # worktrees, concurrent sessions, cleanup, recovery
cargo test -p switchyard-git --test worktrees           # git worktree operations on temp repos
cargo test -p switchyard-core --test codex              # Codex sessions via the fake app-server
cargo test -p switchyard-core --test tasks              # tasks via both fakes ([fake:report:HEX] scenarios)
cargo test -p switchyard-core --test integrations       # integration: inspection, validation, approval, operations, recovery
cargo test -p switchyard-core --test workflows          # engineering@1 workflow via both fakes (models, limits, restarts)
cargo test -p switchyard-core --test delivery           # M8.2: workflow → M7 integration → delivered (commit, validation, approval)
cargo test -p switchyard-core --test organizations      # M8.3: dynamic organization via both fakes (supervision, interruption, limits, restart)
cargo test -p switchyard-core --test supervisor         # ADR 0041: task supervisor answers, routes, follow-ups via the fakes
cargo test -p switchyard-core --test watchdog           # ADR 0045: stuck-work watchdog with short limits
cargo test -p switchyard-core --test gemini             # ADR 0044: Gemini over ACP via the fake (asks, answers, cancel, resume)
SWITCHYARD_REAL_GEMINI=1 cargo test -p switchyard-core --test real_gemini -- --ignored   # real Gemini CLI 0.70.0, API key
cargo test -p switchyard-git --test integrate           # Git integration primitives on temp repos
SWITCHYARD_REAL_CLAUDE=1 cargo test -p switchyard-core --test real_claude -- --ignored   # real CLI, paid
SWITCHYARD_REAL_CODEX=1 cargo test -p switchyard-core --test real_codex -- --ignored     # real CLI, paid
SWITCHYARD_REAL_CLAUDE=1 SWITCHYARD_REAL_CODEX=1 cargo test -p switchyard-core --test real_tasks -- --ignored
SWITCHYARD_REAL_CLAUDE=1 SWITCHYARD_REAL_CODEX=1 cargo test -p switchyard-core --test real_integration -- --ignored
```

Rust is pinned to 1.99.1 (`rust-toolchain.toml`), edition 2024. Clippy warns on `unwrap`/`expect`
outside tests (`clippy.toml` allows them in tests; integration-test files add
`#![allow(clippy::unwrap_used, clippy::expect_used)]`). `unsafe` is forbidden workspace-wide.

When running Core or the app manually, set `SWITCHYARD_DATA_DIR` (and, if that path is long,
`SWITCHYARD_SOCKET`, max 103 bytes) so you don't touch the user's real
`~/Library/Application Support/Switchyard`.

## Architecture rules that span files

- **Core owns everything long-lived.** The webview never touches the socket, SQLite or processes. It
  calls `invoke("core_request", {method, params})`, and `apps/desktop/src-tauri/src/commands.rs`
  forwards only methods in `ALLOWED_METHODS`. A new Core method the UI needs must be added to that
  list **and** to `CoreMethods` in `apps/desktop/src/types/protocol.ts`.
- **Adding a protocol method** touches: `crates/switchyard-protocol/src/methods.rs` (the `method!`
  macro: name + params + result), the dispatch match in `crates/switchyard-core/src/handlers/mod.rs`,
  a handler module, the TS mirror, and `docs/protocols/core-ipc.md`. Keep all of them in sync.
  Incompatible changes bump `PROTOCOL_VERSION` (currently 4) and, if the old contract can no longer
  be served, `MIN_SUPPORTED_PROTOCOL_VERSION`; both sides refuse an unsupported version during
  `initialize` (ADR 0015). Never ship an incompatible change under the same version.
- **Events:** add a variant to `DomainEvent` (`crates/switchyard-events/src/envelope.rs`, with a
  `#[serde(rename = "x.y")]` that matches `event_type()`). Domain writes must go through
  `EventBus::publish_with`, with the state change and `events::append` in **one SQLite transaction**
  (see `handlers/projects.rs`). Never write domain state and then call `publish` separately. Update
  the event table in `core-ipc.md` and `describeEvent` in `apps/desktop/src/lib/format.ts`.
- **Storage:** sync functions over `&Connection` in `crates/switchyard-storage`. Async code calls them
  via `Storage::call` (spawn_blocking) or inside `publish_with` via `Storage::with_conn`. Schema
  changes are **new** files `migrations/000N_*.sql` plus an entry in `MIGRATIONS`
  (`crates/switchyard-storage/src/migrations.rs`). Never edit an applied migration. `events` is
  append-only (enforced by triggers).
- **Git:** via the `git` CLI in `crates/switchyard-git` (`--no-optional-locks`, no prompts,
  timeouts). Read-only except worktree add/remove and `integrate.rs` (object creation, one
  compare-and-swap `update-ref`, commits/merges inside Core-owned workspaces, `merge --ff-only` of a
  clean target checkout); writes run with hooks disabled. Never push, rebase, reset, force or delete.
  Map `GitError` to protocol codes in `crates/switchyard-core/src/handlers/errors.rs`
  (integration steps: `integrations::git_err`).
- **Shutdown ordering matters:** server stops accepting → journals `core.stopping` → sets
  `state.closing` → subscription tasks drain → each connection sends a `shutdown` frame. Connections
  must wait on `closing`, not `shutdown`, or clients miss `core.stopping`.
- `tokio::sync::watch::Ref` is not `Send`. Use the `wait_shutdown` / `wait_closing` helpers in
  `state.rs` inside `select!`.

- **Workspaces.** Project (registered repo, never an agent cwd) → workspaces (Core-owned worktrees
  at `<data>/workspaces/<project>/<id>`, branch `switchyard/<id>`) → sessions (at most one live per
  workspace).
  - Status changes go through `WorkspaceService::set_status` (validated against
    `WorkspaceStatus::can_transition` inside the transaction, with `workspace.state_changed`).
    Session start/resume attach the workspace with `workspaces::attach_in_tx` in the same
    transaction that writes the session.
  - Before any agent runs, `validate_for_session` checks the path/identity/branch; never fall back
    to the project root.
  - Cleanup confirmations are bound to a full `lstat` scan of the worktree
    (`workspaces/fingerprint.rs`), repeated right before `git worktree remove`; Git's view alone
    is not enough (it lists ignored directories as one entry).
  - Never delete, prune, force-remove or discard anything automatically. Cleanup only via
    `workspaces.cleanup` with the confirmation from `cleanup_check`; branches are always kept.
    Reconciliation only changes statuses.
  - Git mutations hold the per-project `repo_lock`; never hold the storage mutex across Git calls.
- **Agents.**
  - Every session state change goes through `AgentService::transition`/`commit` or
    `journal::commit_checked` (state, events, approvals and the workspace attachment in one
    transaction). Never update `sessions` rows elsewhere.
  - Claude wire JSON stays inside `switchyard_agents::claude::wire`, Codex JSON-RPC inside
    `switchyard_agents::codex` (`protocol.rs`, `classify.rs`). Core sees only `AdapterOutput` and the
    provider-neutral `AgentSession`; provider differences go through `switchyard_agents::{classify,
    capabilities}` dispatch. Capabilities are `supported`/`unsupported`/`unknown` (ADR 0022); only
    report `supported` for behaviour verified against the real CLI.
  - Codex threads are always opened with `approvalPolicy: "untrusted"` and `sandbox:
    "workspace-write"`, and tools that act without approval are disabled (`codex/launch.rs`, ADR
    0021, including `view_image` and `unified_exec`, found by the M5.1 audit). Approved Codex commands
    run without Codex's sandbox; never describe the sandbox setting as a boundary. Untested Codex
    versions are refused unless `allow_untested_codex`. Never answer with `acceptForSession` or a policy amendment; never fall back to a new thread
    when `thread/resume` fails.
  - Token deltas go to `LiveHub` only, never the journal.
  - Approvals are settled through `approvals::decide` (SQL-guarded `pending → final`), committed
    before delivery; every non-operator path denies.
- **Policies.**
  - Every `can_use_tool` request goes through `AgentService::on_permission_requested`: classify
    (adapter) → path facts (`policy/paths.rs`) → `policy::evaluate_in_tx` inside the same transaction
    that writes the outcome. Never answer an agent before the evaluation is committed; on any error,
    deny. There must be no second path from adapter to approval.
  - The evaluator (`switchyard-policy`) stays pure. A new tool needs a classifier entry (with its
    known input fields) and must be in both `TOOLS` and the `SETTINGS` ask list (a test checks this).
  - Rule changes go through `PolicyService` (rule + revision snapshot + `policy.rule_changed` in one
    transaction) and are followed by `AgentService::reevaluate_pending`, which may only deny.
  - Human decisions never create rules. Policies are not a sandbox: never word UI or docs as if
    they were.
  - Auto mode (ADR 0037, ADR 0042) is per project, read by Core inside the evaluating transaction
    (never from a caller), and only replaces the `ask` default for file changes and shell (review
    sessions' shell and background commands included); rules and the path/.git/unknown-field caps
    still apply, and `release_action` (commit, push, publish, deploy) always asks unless an
    exact-command rule decides. Review sessions' file changes stay denied. Changes go through `PolicyService::set_auto_mode`
    (setting + history row on the global policy revision + `policy.settings_changed`).
  - When CLI behaviour changes, re-record fixtures in `crates/switchyard-agents/tests/fixtures/`
    (sanitized; see its README) and keep `src/bin/fake_claude.rs` / `src/bin/fake_codex.rs` in sync.
    Integration tests never run the real CLIs: `tests/common` points both at the fakes and its
    `guard` refuses to start any test Core with another executable (a wrapper inside the test's own
    temp dir is allowed). Real-CLI tests go through `TestCore::start_real`, which requires
    `SWITCHYARD_REAL_CLAUDE=1` / `SWITCHYARD_REAL_CODEX=1` exactly. Never start a test Core with
    `switchyard_core::start` directly; use `common::start_core`.
- **Tasks.**
  - Task changes go through `TaskService` (`journal::commit_checked`, rows plus `task.*` events in one
    transaction; status via `tasks::set_status`, which validates `TaskStatus::transition` and
    re-evaluates dependents). Never update `tasks`/`task_assignments` rows elsewhere.
  - The reactor (`tasks/reactor.rs`) follows `agent.turn_completed`/`agent.exited`/`agent.failed`
    through the bus; every reaction calls `advance_assignment_seq` in its transaction (replay-safe).
  - Agent reports are claims: never complete, assign, create tasks or approve anything from a report.
    Only operator methods do. Review sessions (`SessionAccess::Review`) are denied file mutations by
    the system policy and are fingerprinted before/after; keep both.
  - Changes to the task context or report format bump `CONTEXT_VERSION` and keep the fakes'
    `src/bin/fake_support/report.rs` in sync.
- **Integrations.**
  - Changes go through `IntegrationService` (`journal::commit_checked`, `integrations::transition`
    validates `IntegrationStatus::transition`, guarded by status + revision). Only operator IPC moves
    a target branch, via `approve` with the exact bound commits.
  - Preparation (`facts.rs`) reads every fact from Git and Core's records, never from agent output.
    The source must contain the target; conflicts are resolved in the workspace, never on the target.
  - Every Git-changing step (approve, workspace commit/update/abort, a validation run) is a Core-owned
    operation (`operations.rs`, ADR 0032): start it with `run_op`/`spawn_op`, never do the work in the
    request future (a client disconnect aborts that). One per integration; mark the workspace with
    `begin_workspace_step` and release it on every path; shutdown waits, then stops; startup and
    `refresh` settle stranded ones from Git.
  - Merges in a working tree refuse when any untracked/ignored file is at, under, or an ancestor of a
    path the merge writes (`paths_written_by_merge`, `untracked_files_at_risk`); commits refuse new
    conflict blocks in any file (`files_with_new_conflict_blocks`). Read Git answers by exact exit
    code, never "failed without output means no".
  - Validation commands go through `switchyard_policy::evaluate`; `ask` needs explicit operator
    authorization bound to the exact command line. Never run them through a shell. Commands, decisions,
    steps and the run's config are one transaction; the runner executes exactly those steps and a run
    passes only with every step's success recorded. Settings/command changes are journaled in the same
    transaction (`integration.settings_changed`, `integration.validation_command_changed`).
  - Record the merge commit before moving the target (`integrating`); recovery settles from the
    target branch tip.
- **Workflows.**
  - Workflow decisions stay pure (`switchyard_tasks::workflow::decide`). The driver commits the new
    state, its `workflow.*` events, `last_event_seq` and the next step's assignment in one
    transaction, and only then starts the session. Never start a workflow step any other way.
  - Workflow assignments use `create_assignment`/`start_assignment` like the operator's, with a
    unique `step_key`; never bypass them. A Core restart makes a running step `interrupted` (wait);
    never restart agents automatically.
  - Delivery (M8.2, ADR 0034) goes only through `IntegrationService` (create/refresh/validate);
    the delivery state is derived from the integration record (`delivery_fact`), never stored twice.
    The workflow never commits, never approves an integration, never runs `ask`/`deny` commands,
    never pushes; it completes the task only after M7 reports `integrated`.
  - Models are passed to the CLI exactly; choices come only from the installed CLI
    (`agents.models`). Never add a model list to Switchyard, never substitute a model.
  - engineering@2 flags live in the stored state (`review_triage`, `test_gate`, set at start); never change the
    routing of a workflow started under the old rules. Reasoning's `triage` report and the plan are claims that only
    route work; they never approve, complete or create tasks outside the workflow's own plan (ADR 0040).
  - The test gate runs only commands the implementer's report names, each through `evaluate_command` (the
    validation policy path), never through a shell; `ask` needs the operator's resume; results and the decision are
    one transaction. Never let a review session run tests (it must stay read-only).
- **Gemini (ACP).** Every Gemini action must ask Switchyard: keep the admin policy's shell rule with its
  `argsPattern` (Gemini's "known safe command" heuristic turns a pattern-less ask into allow), stop the session on a
  policy load error, accept only the `default` session mode, offer no fs/terminal capability, answer only
  `allow_once` or reject. The API key is passed to the process only; never log or store it. Re-verify against a
  new CLI version before raising `acp::TESTED_VERSION`.
- **Task supervisor.** A read-only session in its own workspace (never the task's). Its
  `switchyard-supervisor` block only requests a route; Core sends an instruction to the working
  agent of the same role (`tasks.note`), refuses other routes while an agent works, and otherwise
  applies `Input::Directed` through the workflow driver. Delivered tasks get a follow-up task. Never
  push, deploy, merge, approve or answer a permission request from it.
- **Organizations.**
  - Decisions stay pure (`switchyard_tasks::organization::decide`); `decide_in_tx` commits state,
    `organization.*` events, subtasks (proposal path, decided by `organization:<key>`), closed subtasks
    and context in one transaction before anything starts. Never act on a directive Core has not
    validated against the organization and its limits.
  - Supervisors run their own read-only sessions (not assignments); specialists use ordinary M6
    assignments (`origin: workflow`, step key `org:<task>#n`). One session at a time per organization.
  - Evidence for supervisors comes from Git and Core's records (`evidence`), never from reports alone.
    Only Core and the user write `verified` context.
  - Instructions reach the next turn's context; never claim a running turn was redirected. Core may
    interrupt a turn that leaves its scope; it never answers a permission request on that path.
  - No built-in skill estimates: selection uses verified capabilities and the user's estimates only.
  - The objective completes only through M7 delivery; the lead's "done" passes the review gate first.
- Text edits made with ad-hoc scripts can silently no-op after `cargo fmt` reflows code. Verify
  behaviour-only edits (guards, checks) with a test.

## Conventions

- No fake data or simulated agent activity in the UI. Unimplemented sections render explicit "not
  available yet" empty states.
- IDs are UUIDv7 (`switchyard_events::new_id`); timestamps are Unix ms (`now_ms`).
- Frontend: strict TS (`exactOptionalPropertyTypes`, `noUncheckedIndexedAccess`), no router or state
  library. Use `createStore`/`useStore` (`src/stores/store.ts`) and `useCoreQuery` (refetches on
  reconnect and on matching events). Styling is plain CSS tokens in `src/styles.css`.
- Significant decisions get an ADR in `docs/decisions/` and a row in `docs/architecture/decisions.md`.
