# ADR 0011: Agent workspace boundary

- Status: accepted (2026-11-11)

## Context

Setting the working directory alone does not confine Claude Code:

- Bash runs arbitrary commands.
- Claude's sandbox is off by default.
- Repository and user settings can add allow rules, hooks and MCP servers.

## Decision

Every session is launched with:

- `--restricted`, which:
  - ignores user, project and local settings, so repository-provided allow rules and hooks are not
    loaded;
  - confines the file tools to the working directory;
  - refuses `bypassPermissions`.
- `--strict-mcp-config` with no MCP servers. Without it, account-level connectors were observed
  loading.
- An explicit tool list, `Read,Write,Edit,Glob,Grep,Bash,NotebookEdit`. Task, web and
  harness-specific tools are not offered.
- `--settings '{"permissions":{"ask":["Bash"]}}'`, so **every** shell command (including `cat` and
  `ls` of paths outside the project) needs an operator decision in Switchyard. Verified.
  **Extended by [ADR 0017](0017-normalized-operations.md):** the `ask` rule now names every enabled
  tool, and each request is decided by Switchyard's policy engine (ADR 0016) before any operator
  is asked.
- `--permission-mode default`. Never `acceptEdits`, never bypass.
- The working directory is the project's primary workspace, a new `workspaces` row of kind
  `primary`, which is the anchor for future worktree isolation. **Superseded by
  [ADR 0012](0012-project-workspace-session.md):** sessions now run in a Core-owned Git worktree,
  and the milestone-2 primary rows became `legacy_checkout` records.

## Consequences and limits

- No OS-level isolation is claimed. An approved Bash command runs with the user's privileges.
  Claude's own sandbox, macOS seatbelt, or containers are future options.
- Managed (enterprise) Claude settings still apply and could allow tools. This cannot be overridden.
- CLAUDE.md memory files in the repository are still read by Claude. They are instructions, not
  permissions.
- Registering Switchyard's own repository grants it nothing extra. Every edit still needs approval.
