# ADR 0005: Use the `git` CLI, not libgit2/gitoxide

- Status: accepted (2026-11-11)

## Decision

Repository inspection shells out to the user's `git` with `--no-optional-locks`,
`GIT_TERMINAL_PROMPT=0`, `LC_ALL=C`, redirecting environment variables (`GIT_DIR` and others) removed,
`stdin` closed, a 15 s timeout, and `kill_on_drop`. Status uses `--porcelain=v2 --branch`, which is
a stable machine format. Only record markers are parsed, never file names.

## Rationale

- The agents themselves use the `git` CLI. Using it too means Switchyard sees exactly what they see,
  including the user's config, `safe.directory`, hooks, and credential helpers.
- No large native dependency (libgit2, OpenSSL) and no feature gaps (worktrees, sparse checkout).
- `--no-optional-locks` ensures inspection never contends with an agent writing to the index.

## Consequences

- `git` must be installed (Xcode Command Line Tools on macOS). `SWITCHYARD_GIT` overrides the path.
  A missing executable is reported as `git_unavailable`.
- Process spawn overhead (about 5–20 ms per call) is irrelevant at UI refresh rates.
