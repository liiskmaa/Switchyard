# ADR 0006: Crate layout and deviations from the initial proposal

- Status: accepted (2026-11-11)

The initial proposal listed `switchyard-{core,protocol,events,storage,agents,workspaces,permissions}`
and top-level `tests/integration` and `tests/fixtures`.

## Deviations

| Proposal | Implemented | Reason |
|----------|-------------|--------|
| `switchyard-agents` | created in milestone 2 | Vendor-neutral agent model, process supervision and the Claude Code adapter. Session orchestration (approvals, journal, live output, file watching) lives in `switchyard-core/src/agents`, because it needs storage and the event bus. |
| `switchyard-permissions` | created in milestone 4 as `switchyard-policy` | Milestone 2 approvals were per-request operator decisions. Milestone 4 added the pure policy engine as `switchyard-policy` ([ADR 0016](0016-approval-policy-model.md)); persistence stays in `switchyard-storage` and the integration in `switchyard-core/src/policy`. |
| `switchyard-workspaces` | created in milestone 3, as a pure domain crate | It holds the workspace lifecycle, naming and layout rules, and the cleanup safety evaluation, with no I/O, so storage, protocol and Core share one definition. Git mechanics (`worktree add/list/remove`) stay in `switchyard-git::worktree`. Orchestration (provisioning, validation, reconciliation, cleanup) lives in `switchyard-core/src/workspaces`, because it needs storage and the event bus. |
| — | `switchyard-client` | The desktop bridge and the integration tests need the same protocol client. Putting it in `protocol` would pull socket I/O into a types crate. |
| `tests/integration`, `tests/fixtures` | `crates/*/tests/` | Cargo runs integration tests per crate, and `CARGO_BIN_EXE_*` (needed to run the real Core binary) is only available to the binary's own crate. Fixtures, such as Git repositories, are generated at runtime in temporary directories instead of being committed. |
| `apps/desktop/src/{components,features,layouts,stores,types}` | as proposed, plus `src/lib` | `lib` holds the runtime bridge wrapper (`core.ts`) and pure helpers. |
