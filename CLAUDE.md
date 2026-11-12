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
SWITCHYARD_REAL_CLAUDE=1 cargo test -p switchyard-core --test real_claude -- --ignored   # real CLI, paid
SWITCHYARD_REAL_CODEX=1 cargo test -p switchyard-core --test real_codex -- --ignored     # real CLI, paid
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
- **Git:** read-only, via the `git` CLI in `crates/switchyard-git` (`--no-optional-locks`, no prompts,
  timeouts). Map `GitError` to protocol codes in `crates/switchyard-core/src/handlers/errors.rs`.
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
  - When CLI behaviour changes, re-record fixtures in `crates/switchyard-agents/tests/fixtures/`
    (sanitized; see its README) and keep `src/bin/fake_claude.rs` / `src/bin/fake_codex.rs` in sync.
    Integration tests never run the real CLIs: `tests/common` points both at the fakes and its
    `guard` refuses to start any test Core with another executable (a wrapper inside the test's own
    temp dir is allowed). Real-CLI tests go through `TestCore::start_real`, which requires
    `SWITCHYARD_REAL_CLAUDE=1` / `SWITCHYARD_REAL_CODEX=1` exactly. Never start a test Core with
    `switchyard_core::start` directly; use `common::start_core`.
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
