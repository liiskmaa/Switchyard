<p align="center">
  <img src="apps/desktop/src-tauri/icons/128x128.png" width="96" height="96" alt="Switchyard icon">
</p>

<h1 align="center">Switchyard</h1>

<p align="center">
  A local-first desktop app for running native AI coding agents side by side on your repositories.<br>
  Each agent works in its own Git worktree, and every tool request is checked against your policies: shell commands and file changes ask you unless a rule says otherwise.
</p>

<p align="center">
  <img alt="Status: milestone 4" src="https://img.shields.io/badge/status-milestone%204-orange">
  <img alt="Platform: macOS" src="https://img.shields.io/badge/platform-macOS-lightgrey">
  <img alt="Rust 1.99" src="https://img.shields.io/badge/rust-1.99-b7410e">
  <img alt="Tauri 2" src="https://img.shields.io/badge/tauri-2-24c8db">
  <img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-blue">
</p>

---

Switchyard orchestrates the agents' **own CLIs**: Claude Code today, OpenAI Codex later. It is not
an agent and doesn't call model APIs. A long-running Rust process, **Switchyard Core**, supervises
the agent processes, keeps a durable journal of everything they do, and puts you in control of
every action that matters. The desktop app is a client: closing it never stops your agents.

## What works today

- **Native Claude Code sessions,** driven over Claude Code's streaming JSON protocol:
  - live output and tool activity;
  - interrupt, stop and resume;
  - verified against `claude` 2.1.322.
- **Central approvals and policies.** Every tool request from an agent goes through Switchyard:
  - **By default:** reads and searches inside the workspace run without asking; every shell
    command and file change is a pending request you approve or deny; unknown tools are blocked.
  - **Per project or workspace,** you can allow a path ("edits under `docs/`") or one exact command
    (`cargo test`), make reads ask, or block things. When rules disagree, the most restrictive one
    wins.
  - Requests expire as **denials**, never approvals. Approving one request never creates a rule.
  - Every decision is recorded with the rule revision and the reason.
- **Workspace isolation:**
  - Each session runs in its own Git worktree on its own `switchyard/<id>` branch, stored outside
    your repository.
  - Several agents can work on the same project at once without touching each other's files, or
    your checkout.
- **Safe workspace lifecycle:**
  - Workspaces outlive sessions.
  - Archiving keeps files.
  - Cleanup is explicit: it shows what would be deleted, refuses when there is uncommitted or
    untracked work, asks you to confirm the exact state you reviewed, and keeps the branch.
- **Crash-safe by design:**
  - SQLite (WAL) event journal.
  - Agents are never orphaned.
  - After a Core crash, workspaces and their changes are preserved and nothing restarts on its own.
- **Observed file changes:** real filesystem events per workspace, kept separate from what the
  agent says it did.

> **Isolation is about source trees, and policies are about what Switchyard answers; neither is a
> sandbox.** An allowed or approved shell command runs with your user's permissions and can reach
> files outside its workspace. Approve, and write rules, accordingly.

Not yet: Codex, merging workspace branches back, OS-level sandboxing.
See the [architecture overview](docs/architecture/overview.md).

## How it fits together

```text
┌──────────────────────────┐   Unix socket    ┌───────────────────────────────────────────┐
│ Switchyard Desktop       │   (0600, JSON    │ Switchyard Core (long-running)            │
│ Tauri 2 · React 19       │◄────lines)──────►│  projects · workspaces · sessions         │
│ a client, nothing more   │                  │  approvals · event journal (SQLite WAL)   │
└──────────────────────────┘                  │        │ supervises                       │
                                              │        ▼                                  │
                                              │  claude -p (stream-json)  cwd = worktree  │
                                              └───────────────────────────────────────────┘
Project (your repo) ──► Workspaces (Core-owned worktrees, one branch each) ──► Sessions (one live per workspace)
```

## Requirements

- macOS 11 or later. The Core crates also build on Linux; the desktop app targets macOS.
- [Rust](https://rustup.rs) 1.99.1. It's pinned in `rust-toolchain.toml` and rustup installs it
  automatically.
- Node `^20.19` or `≥ 22.12`, and npm `≥ 11` (`npm i -g npm@11`).
- Git and the Xcode Command Line Tools.
- For agent sessions: the [Claude Code](https://docs.anthropic.com/en/docs/claude-code) CLI, logged
  in (`claude auth login`). Switchyard uses your existing login and never stores credentials.

## Quick start

```sh
git clone https://github.com/liiskmaa/Switchyard.git
cd Switchyard
npm install
npm run dev          # builds Core, then launches the desktop app (Core starts automatically)
```

Then:

1. **Projects → Add repository** and pick a local Git repository.
2. **New workspace.** Switchyard creates a worktree and branch for it, leaving your checkout alone.
3. **Start Claude session here,** optionally with a first task.
4. Watch it work under **Agents**, and answer permission requests under **Approvals** or inline.
   Under **Policies**, decide what may run without asking.
5. When you're done: **Archive** the workspace, then **Clean up…** once its work is committed.

To try it without touching your real data, use a separate data directory:

```sh
SWITCHYARD_DATA_DIR=/tmp/sy-dev npm run dev
```

A release bundle with Core embedded as a sidecar is built with `npm run build`; it produces
`target/release/bundle/macos/Switchyard.app` (not code-signed).

## Development

```sh
npm test             # cargo test --workspace + vitest
npm run lint         # rustfmt --check, clippy -D warnings, tsc --noEmit (strict)
cargo test -p switchyard-core --test workspaces   # one integration suite
cargo run -p switchyard-core -- --log-format text # run Core by itself; the app will connect to it
```

- The tests use real temporary Git repositories and a deterministic fake Claude CLI built from
  recorded transcripts, so they need no network and no account.
- Opt-in tests drive the **real** CLI and make a few small paid requests:

  ```sh
  SWITCHYARD_REAL_CLAUDE=1 cargo test -p switchyard-core --test real_claude -- --ignored --nocapture
  ```

### Configuration

| Variable | Default | Purpose |
|----------|---------|---------|
| `SWITCHYARD_DATA_DIR` | `~/Library/Application Support/Switchyard` | Database, socket, logs, and `workspaces/` (worktrees) |
| `SWITCHYARD_SOCKET` | `<data dir>/core.sock` | Socket path (max 103 bytes) |
| `SWITCHYARD_CORE_BIN` | `switchyard-core` next to the desktop executable | Core executable the desktop launches |
| `SWITCHYARD_AUTO_LAUNCH_CORE` | `1` | `0` stops the desktop from starting Core |
| `SWITCHYARD_GIT` | `git` on `PATH` | Git executable used by Core |
| `SWITCHYARD_CLAUDE_BIN` | `claude` on `PATH`, then common install locations | Claude Code executable |
| `SWITCHYARD_APPROVAL_TIMEOUT_SECS` | `900` | Pending permission requests are denied after this long |
| `SWITCHYARD_LOG` | `info` | `tracing` filter |

Core CLI: `switchyard-core [--data-dir PATH] [--socket PATH] [--log-format text|json] [--version]`.

### Repository layout

```text
apps/desktop/                Tauri 2 app: React UI (src/) and the Rust bridge to Core (src-tauri/)
crates/switchyard-core/      Core executable: lifecycle, IPC server, workspaces, agent sessions
crates/switchyard-workspaces/ Workspace lifecycle, naming and cleanup-safety rules (no I/O)
crates/switchyard-policy/    Approval policy: operations, rules, deterministic evaluation (no I/O)
crates/switchyard-agents/    Agent model, process supervision, Claude Code adapter
crates/switchyard-git/       Git CLI: inspection and worktree management
crates/switchyard-protocol/  Versioned wire protocol (Rust); mirrored in apps/desktop/src/types
crates/switchyard-events/    Event types, journal trait, event bus
crates/switchyard-storage/   SQLite, migrations, repositories
crates/switchyard-client/    Async protocol client (desktop and tests)
migrations/                  SQL migrations, embedded into Core
docs/                        Architecture, ADRs, protocol specs
```

## Documentation

- [Architecture overview](docs/architecture/overview.md)
- [Workspaces: lifecycle, isolation, cleanup](docs/architecture/workspaces.md)
- [Approval policies](docs/architecture/policies.md)
- [Database schema](docs/architecture/database.md)
- [Core IPC protocol](docs/protocols/core-ipc.md)
- [Claude Code integration (verified CLI behaviour)](docs/architecture/agent-integration.md)
- [Agent adapter contract](docs/protocols/agent-adapter-contract.md)
- [Architectural decisions](docs/architecture/decisions.md) and [ADRs](docs/decisions/)

## Status and verification

This is early, milestone-based work. Milestone 1 built the foundation, milestone 2 added native
Claude Code sessions, milestone 3 added workspace isolation, and milestone 4 added approval
policies.

What each kind of check covers:

- **Automated tests** (`npm test`): Core, the IPC protocol, storage, the policy engine and
  workspaces, against real Git repositories and a deterministic fake Claude CLI that speaks the
  recorded stream-json protocol. The fake CLI never decides policy; Core does.
- **Real Claude Code tests** (opt-in, paid): sessions, two concurrent workspaces and policy
  decisions against `claude` 2.1.322.
- **Browser end-to-end tests** (a development harness kept outside the repository): the real React
  UI against a real Core, through a stand-in for the Tauri bridge, with the fake CLI.
- **Native app** (macOS, bundled `Switchyard.app`): Core auto-launch and reuse, the native IPC
  bridge with reconnection and event replay, protocol-mismatch handling and the native folder
  picker, plus a real Claude Code session driven from the native window (an allowed read, an
  approved and a denied write, a denied shell command, interrupt, stop and resume). These were
  manual checks on one macOS machine, not an automated suite.

Policies decide what Switchyard answers, approvals are per-request human decisions, and workspaces
separate source trees. None of them is an OS-level sandbox; that is not implemented.

## License

[MIT](LICENSE)
