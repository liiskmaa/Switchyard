# ADR 0027: Review sessions cannot change files; the workspace is verified

- Status: accepted (2026-11-13)

## Context

A reviewer reads the implementer's work in the same workspace. A review is worthless if the
reviewer can change what it reviews. Neither CLI offers a read-only mode that Switchyard can rely on
(Codex's sandbox does not apply to approved commands; ADR 0021). Workspaces are not a sandbox
(ADR 0019).

## Decision

- Sessions have an `access` (`write` or `review`, stored on the session). Review assignments start
  their sessions with `review`.
- Policy (system policy version 2): in a review session every file mutation (`file_write`,
  `file_edit`, `file_delete`) is denied as a system restriction (`review_session`) before any rule is
  consulted. Rules cannot loosen it, and no person is asked. Shell commands are capped at `ask` (a
  rule cannot allow them), so a person sees every command.
- Core takes a full `lstat` fingerprint of the workspace (`workspaces::fingerprint::scan`, the same
  scan cleanup uses) when the review starts, and again when the verdict arrives. If they differ, the
  verdict is recorded as `blocked` (the reported verdict is kept), with a high-severity finding.
- A verdict (`approved`, `changes_requested`, `rejected`, `blocked`) moves the task back to
  `awaiting_review`. The operator then completes the task, assigns another implementation (open
  findings go into its context), or fails it.

## Consequences

- An approved shell command in a review still runs with the user's permissions, and could change
  files. The fingerprint check detects changes inside the workspace, not outside it. The UI and docs
  say this plainly; review access is not a sandbox.
- A review verdict is advice to the operator, never an approval.
