# ADR 0001: Orchestration runs in a separate Core process

- Status: accepted (2026-11-11)

## Context

Agent sessions (Claude Code, Codex) run for minutes to hours. The operator must be able to close or
restart the UI without killing them, and other clients (CLI, a second window, tests) need the same
view of state.

## Decision

All orchestration state lives in `switchyard-core`, a standalone executable. The Tauri desktop app is
an IPC client of Core. The desktop starts Core on demand, in its own process group, and never waits
on it or kills it.

## Consequences

- Closing the UI does not affect agents. Reconnecting replays missed events.
- Two processes have to be built, versioned and shipped. The protocol has a version handshake for
  this reason (ADR 0002).
- A single-instance lock (`flock` on `<data dir>/core.lock`) is required so that two Cores never
  share a database.
- Core is not yet registered with launchd, so it does not start at login or restart after a crash on
  its own. The desktop relaunches it when needed. A LaunchAgent is a possible later step.
- Rejected: running orchestration inside the Tauri process. That couples agent lifetime to the
  window and makes headless use impossible.
