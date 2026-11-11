# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Switchyard orchestrates **native** coding agents (Claude Code, Codex CLIs) from a separate long-running
Rust process, **Switchyard Core**. The Tauri desktop app is only a client.

- Milestone 1: Core, IPC, SQLite, the event engine, projects, and the desktop shell.
- Milestone 2: Claude Code sessions (`crates/switchyard-agents` and `crates/switchyard-core/src/agents`).
- Codex is not implemented.

The verified Claude CLI behaviour is in `docs/architecture/agent-integration.md`, and the contract in
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
SWITCHYARD_REAL_CLAUDE=1 cargo test -p switchyard-core --test real_claude -- --ignored   # real CLI, paid
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
  Incompatible changes bump `PROTOCOL_VERSION`.
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

- **Agents.**
  - Every session state change goes through `AgentService::transition`/`commit` (state, events and
    approvals in one transaction). Never update `sessions` rows elsewhere.
  - Claude wire JSON stays inside `switchyard_agents::claude::wire`. Core sees only `AdapterOutput`.
  - Token deltas go to `LiveHub` only, never the journal.
  - Approvals are settled through `approvals::decide` (SQL-guarded `pending → final`), committed
    before delivery; every non-operator path denies.
  - When CLI behaviour changes, re-record fixtures in `crates/switchyard-agents/tests/fixtures/`
    (sanitized; see its README) and keep `src/bin/fake_claude.rs` in sync with them.
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
