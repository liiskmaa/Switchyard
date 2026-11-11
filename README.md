# Switchyard

Switchyard is a local-first orchestration platform for **native** AI coding agents, starting with
Claude Code and OpenAI Codex. It runs the agents' own CLIs as supervised processes, coordinates their
work, records everything in a durable event journal, and puts a human operator in control. It is not
a coding agent itself.

> **Status: milestone 1 (foundation).** Working today: a separate Core process with IPC, SQLite
> persistence, the event engine, project registration with Git status, and the desktop shell.
> **No agent integration yet.** See [docs/architecture/agent-integration.md](docs/architecture/agent-integration.md).

## Architecture in one paragraph

`switchyard-core` is a long-running Rust/Tokio process. It owns SQLite (WAL), the event bus, project
registrations and, later, agent processes. The Tauri 2 + React desktop app is a client: it talks to
Core over a `0600` Unix domain socket using a versioned JSON-lines protocol, and starts Core if it is
not running. Closing the desktop never stops Core; reopening it reconnects and replays missed events.
Details: [docs/architecture/overview.md](docs/architecture/overview.md) ·
[protocol](docs/protocols/core-ipc.md) · [decisions](docs/architecture/decisions.md).

```text
apps/desktop/            Tauri 2 app: React 19 UI (src/) + Rust bridge (src-tauri/)
crates/switchyard-core/      Core executable: lifecycle, socket server, request handlers
crates/switchyard-protocol/  Wire frames, typed methods, error codes, framing, paths
crates/switchyard-events/    Event types, journal trait, event bus
crates/switchyard-storage/   SQLite, migrations, repositories, event journal
crates/switchyard-git/       Read-only git CLI inspection
crates/switchyard-client/    Async protocol client (desktop + tests)
migrations/              SQL migrations (embedded into Core at compile time)
docs/                    Architecture, ADRs, protocol specs
scripts/                 Build helpers
```

## Requirements

- macOS 11+ (the initial target platform; the Core crates also build on Linux)
- Rust **1.99.1** (pinned in `rust-toolchain.toml`; rustup installs it automatically)
- Node **^20.19 or ≥ 22.12** and **npm ≥ 11** (npm 10.8 fails to resolve the dependency tree; run
  `npm i -g npm@11`)
- Xcode Command Line Tools (provides `git` and the macOS SDK)

## Setup

```sh
npm install                 # installs the desktop workspace's JS dependencies
cargo build --workspace     # first build compiles Tauri and SQLite (a few minutes)
```

## Development

```sh
npm run dev                 # builds switchyard-core, then runs `tauri dev` (Vite on :1420)
```

The desktop app starts Core automatically (`target/debug/switchyard-core`), logging to
`<data dir>/logs/core.log`. To run Core yourself, for example to watch its logs:

```sh
cargo run -p switchyard-core -- --log-format text      # Ctrl-C shuts it down gracefully
SWITCHYARD_LOG=switchyard_core=debug cargo run -p switchyard-core
```

Then launch the desktop app with `npm run dev`. It connects to the running Core instead of starting
another one.

### Configuration

| Variable | Default | Purpose |
|----------|---------|---------|
| `SWITCHYARD_DATA_DIR` | `~/Library/Application Support/Switchyard` | Database, socket, lock file, logs |
| `SWITCHYARD_SOCKET` | `<data dir>/core.sock` | Socket path (max 103 bytes) |
| `SWITCHYARD_CORE_BIN` | `switchyard-core` next to the desktop executable | Core executable the desktop launches |
| `SWITCHYARD_AUTO_LAUNCH_CORE` | `1` | `0` stops the desktop from starting Core |
| `SWITCHYARD_GIT` | `git` on `PATH` | Git executable used by Core |
| `SWITCHYARD_LOG` | `info` | `tracing` filter for Core and the desktop |

Core CLI: `switchyard-core [--data-dir PATH] [--socket PATH] [--log-format text|json] [--version]`.
Exit codes: `0` clean shutdown, `1` runtime error, `2` usage or configuration error, `3` another Core
already owns the data directory.

Use an isolated data directory to experiment without touching your real one:

```sh
SWITCHYARD_DATA_DIR=/tmp/sy-dev npm run dev
```

## Testing and checks

```sh
npm test                                  # cargo test --workspace + vitest
cargo test -p switchyard-core             # Core integration tests (in-process and real binary)
cargo test -p switchyard-core --test projects register_list   # a single test by name filter
cargo test -p switchyard-storage events::tests                 # one module's unit tests
npm run test:web                          # frontend unit tests only (vitest)
npm run lint                              # rustfmt --check, clippy -D warnings, tsc --noEmit
```

Git-based tests create temporary repositories with the `git` CLI, so `git` must be installed.

## Building a release bundle

```sh
npm run build
```

This builds `switchyard-core` in release mode, copies it to
`apps/desktop/src-tauri/binaries/switchyard-core-<target-triple>` (`scripts/prepare-core-sidecar.mjs`),
and runs `tauri build` with `tauri.bundle.conf.json`, which bundles Core as an `externalBin` inside
`Switchyard.app/Contents/MacOS/`. Output:
`target/release/bundle/macos/Switchyard.app`. The bundle is not code-signed.

## Verification

Besides the automated tests, milestone 1 was checked by hand on macOS (2026-11-11) with an isolated
data directory:

- The desktop app launched, auto-started Core as a separate process in its own process group, and
  connected.
- A repository registered through Core appeared in Core's journal and was delivered to the desktop
  subscription.
- After the desktop quit, Core kept running. After the desktop restarted, it reconnected to the
  **same** Core instance (same pid and instance id).
- After Core was stopped with `system.shutdown`, the desktop received the `shutdown` frame, started a
  new Core and reconnected. The registered project was still there.
- `npm run build` produced `Switchyard.app`. Launched directly, it started its bundled
  `Contents/MacOS/switchyard-core` and connected.

The UI was not inspected visually in that session, because screen capture was unavailable to the
tooling. Visual QA and click-through of the project registration form are still open.

## Documentation

- [Architecture overview](docs/architecture/overview.md)
- [Architectural decisions](docs/architecture/decisions.md) and [ADRs](docs/decisions/)
- [Core IPC protocol](docs/protocols/core-ipc.md)
- [Agent integration research](docs/architecture/agent-integration.md)
- [Agent adapter contract (draft)](docs/protocols/agent-adapter-contract.md)
