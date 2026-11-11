# ADR 0007: Toolchain and dependency versions

- Status: accepted (2026-11-11)

| Component | Version | Notes |
|-----------|---------|-------|
| Rust | 1.99.1 (pinned in `rust-toolchain.toml`), crate MSRV 1.90 | Edition 2024. Uses `File::try_lock` (stable since 1.89) for the instance lock. Tauri 2.12 requires ≥ 1.90 and rusqlite 0.41 requires ≥ 1.89. |
| Tauri | 2.12 (Rust), CLI and API 2.12.4, `tauri-plugin-dialog` 2.8 | Tauri 3 is still alpha (3.0.0-alpha.6) and was not used. |
| Tokio | 1.54 | Only the needed features are enabled per crate. |
| rusqlite | 0.41, `bundled` | See ADR 0003. |
| serde / serde_json / thiserror / tracing / uuid (v7) | current 1.x / 2.x / 0.1 / 1.29 | |
| React | 19.3 | |
| Vite | 8.3, `@vitejs/plugin-react` 6.3 | Vite 8 requires Node ^20.19 or ≥ 22.12. |
| TypeScript | 5.9.3 | TS 6 and 7 (native port) are available. 5.9 was chosen for ecosystem compatibility; upgrading is a mechanical follow-up. |
| Vitest | 5.1 | Unit tests for pure frontend logic. |
| nix | 0.31 (`signal` only) | Milestone 2: safe `kill(2)` wrapper for SIGTERM/SIGINT to agent processes (the workspace forbids `unsafe`). |
| notify | 8.2 (`macos_fsevent`) | Milestone 2: native filesystem events (FSEvents) for `files.changed`. 9.x is still a release candidate. |
| npm | ≥ 11 | npm 10.8 crashes resolving Vitest 5's optional peer set (`Cannot read properties of null (reading 'edgesOut')`). |

Not used: an HTTP server, a router or state-management library, an icon library, ORMs, CLI parsing
crates (`main.rs` parses five flags by hand), or anything from the excluded list (Postgres, Redis,
brokers, LangGraph, LiteLLM, vector databases).
