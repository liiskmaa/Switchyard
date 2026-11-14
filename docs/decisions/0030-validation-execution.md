# ADR 0030: Validation commands through the policy evaluator, bound to the source commit

- Status: accepted (2026-11-14)

## Context

Validation (tests, lint, build) runs project code. It must go through the same permission system
as an agent's shell command, run in the right place, be bound to what is being integrated, and be
cancellable. It must not become a CI system.

## Decision

- The operator configures per-project commands as **argument vectors** (program plus arguments),
  each with a timeout. They are run directly, never through a shell. `command_line` (shell-quoted)
  is used only for display and policy matching.
- Before a run, each enabled command is evaluated by the M4 evaluator with the project's rules, as a
  `shell` operation in the workspace scope:
  - `allow` (an exact-command rule) runs;
  - `deny` never runs;
  - `ask`, the default for shell commands, runs only if the operator authorizes that command for
    this run. The authorization names the command and the exact command line the operator saw;
    if the command was edited in between, it does not authorize the new one.

  If any command is refused, nothing runs. The decision, its explanation and rule, the policy
  revision and who authorized (`policy` or the operator) are stored with each step. Exact-command
  rules cannot contain quotes or metacharacters (M4 grammar), so such commands always need the
  operator's authorization.
- A run is bound to the integration's current source commit. It requires the workspace to be clean,
  at `S` and up to date. It runs in the workspace, which stays `integrating` meanwhile (no session can
  attach). Commands run one after another and stop at the first failure (the rest are `skipped`).
- Each command runs in its own process group. A timeout or a cancellation sends SIGTERM to the
  group, then SIGKILL after 3 s (always, even if the leader exited), and remaining group members are
  killed after a normal exit too. Output is collected for at most 5 s more, so a process outside the
  group that holds the pipes cannot keep the run alive. stdout and stderr keep their last 64 KiB each
  (marked truncated).
- After a run, Core checks that HEAD is still `S` and that the workspace content equals `tree(S)`.
  A run whose commands changed non-ignored files or the commit does not count as passed.
- **One snapshot.** The enabled commands, each one's policy decision, the authorization check, the
  steps and the run's configuration (command, command line, timeout, decision of each step) are read and
  written in one transaction. The runner executes exactly the steps recorded there; it does not read the
  configuration again. A command edit cannot fall between them.
- A passed run counts only for its exact source commit **and configuration**. Any new commit, an
  edited, added, disabled or reordered command, a changed timeout, or a rule change that alters a
  decision makes it stale, and validation is required again.
- **Fail closed.** A run is `passed` only if it had at least one step and every step's success was both
  observed and recorded. A command whose start cannot be recorded does not run. A result that cannot
  be recorded fails the run. If the final `passed` cannot be recorded, Core records `failed`; if even
  that fails, the run stays `running`, which counts for nothing, and is marked `interrupted` at the next
  start. If the configuration cannot be read, no run starts.
- After a run, if the integration cannot be prepared again (for example the target branch was
  deleted), it becomes `failed` with the reason instead of staying `validating`.
- A validation run is a Core-owned operation (ADR 0032). Core shutdown stops it like a cancellation.
- Settings and command changes are journaled in the same transaction as the change:
  `integration.settings_changed` (review requirement before/after, revision, who) and
  `integration.validation_command_changed` (created/updated/deleted, before/after, who). Open
  integrations are checked again afterwards.

## Consequences

- Validation is as authorized as any agent command, and its audit lives with the integration.
- Approved commands execute code from the workspace (for example a test suite an agent wrote) with
  the user's permissions. Authorizing validation is authorizing that code.
- Commands inherit Core's environment. A Core started by the desktop app may have a minimal `PATH`,
  so absolute program paths are the reliable choice.
- After a Core restart, a run in progress is marked `interrupted`. Its processes are no longer
  supervised, the same as agent processes. The integration is prepared again.
