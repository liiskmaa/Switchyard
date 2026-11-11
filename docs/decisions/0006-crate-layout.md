# ADR 0006: Crate layout and deviations from the initial proposal

- Status: accepted (2026-11-11)

The initial proposal listed `switchyard-{core,protocol,events,storage,agents,workspaces,permissions}`
and top-level `tests/integration` and `tests/fixtures`.

## Deviations

| Proposal | Implemented | Reason |
|----------|-------------|--------|
| `switchyard-agents` | not created | No adapter is implemented in this milestone. An empty crate would have no responsibility. The contract is specified in `docs/protocols/agent-adapter-contract.md`. |
| `switchyard-permissions` | not created | No enforcement exists yet. The `permissions` table is reserved in the schema. |
| `switchyard-workspaces` | `switchyard-git` | The only current need is read-only repository validation and status. Worktree management will become `switchyard-workspaces` and depend on `switchyard-git`. |
| — | `switchyard-client` | The desktop bridge and the integration tests need the same protocol client. Putting it in `protocol` would pull socket I/O into a types crate. |
| `tests/integration`, `tests/fixtures` | `crates/*/tests/` | Cargo runs integration tests per crate, and `CARGO_BIN_EXE_*` (needed to run the real Core binary) is only available to the binary's own crate. Fixtures, such as Git repositories, are generated at runtime in temporary directories instead of being committed. |
| `apps/desktop/src/{components,features,layouts,stores,types}` | as proposed, plus `src/lib` | `lib` holds the runtime bridge wrapper (`core.ts`) and pure helpers. |
