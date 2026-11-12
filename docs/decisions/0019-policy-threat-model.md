# ADR 0019: Approval policies: threat model and limitations

- Status: accepted (2026-11-11)

## Statement

**Approval policies are an authorization and workflow mechanism, not an OS-level sandbox.** They
decide whether Switchyard tells the agent "yes" to a requested tool use. They do not constrain what
happens once a tool runs. An allowed or approved command runs with the user's operating-system
privileges, and it can read and write files outside its workspace, reach the network, and start
processes. Execution isolation (sandboxing, containers) is a separate future mechanism. It is not
provided, and the UI and documentation must not imply that it is.

## What the policy engine does protect against

- **Accidental automatic approval:**
  - Unknown tools, malformed requests, tools not enabled at launch, and evaluation or database
    failures are denied.
  - Conflicting rules resolve to the most restrictive one.
  - There is no broad Bash allow.
- **Scope confusion:**
  - Rules of one project never apply to another.
  - Workspace rules never apply to another workspace.
  - Workspace rules can't be created for a workspace of another project (Core check plus a SQL
    trigger).
  - The project, workspace and session of a request come from Core's own records, never from the
    adapter.
- **Unnoticed loosening:**
  - Pending approvals are never upgraded.
  - Rule history and decision history are append-only.
  - Every automatic decision is visible in the session timeline and in the policy history.
- **Overriding human authority:**
  - A policy never approves a request that is already waiting for a person.
  - A person cannot approve something a policy denies.

## What it does not protect against

- **Effects of allowed commands.**
  - An exact-command rule such as `cargo test` runs build scripts and tests from the workspace,
    which the agent may have edited, possibly with an approved write. Allowing a command trusts
    everything that the command executes.
  - The command name is resolved through `PATH` and the shell at run time.
- **The agent's shell.** Claude's Bash tool keeps its working directory between commands. An
  exact-match allow says nothing about which directory it runs in.
- **Paths:**
  - Path facts are computed when the request arrives. A symlink created between evaluation and
    execution can redirect a file operation (time-of-check to time-of-use).
  - Case-insensitive matching covers ASCII case only, not Unicode normalization variants.
  - Matching is deliberately biased toward `ask`/`deny` when in doubt (ADR 0016), but it is not a
    filesystem permission.
- **The CLI itself.** Switchyard trusts that Claude Code honours `can_use_tool` answers, `--tools`,
  `--restricted` and the settings `ask` rules. Managed (enterprise) Claude settings still apply and
  could change what reaches Switchyard; that is a known limitation of ADR 0011.
- **The local user.** Anyone who can connect to Core's socket (`0600`, same UID) or edit the SQLite
  file can change policies. Policies are not a protection against the machine's own user.

## Consequences

- UI copy says "Allow automatically", "Ask every time" and "Block". It never says "safe", "sandboxed"
  or "protected".
- The Policies view carries a short, permanent note that allowed commands run with the user's
  permissions.
