# ADR 0010: Agent process ownership, shutdown and recovery

- Status: accepted (2026-11-11)

## Decision

- **Ownership.**
  - Agent processes are children of Core only. Each runs in its own process group (so it is immune
    to terminal signals aimed at Core), has piped stdio, and uses `kill_on_drop`.
  - The desktop never owns an agent.
  - One attached session per project.
- **Desktop disconnect.** Nothing happens to agents. Pending approvals stay pending in Core until
  someone decides or they expire.
- **Graceful Core shutdown.** This happens before `core.stopping`, so that connected clients see it:
  1. Cancel pending approvals (`core_shutdown`) and deny them to the agent.
  2. Interrupt running turns.
  3. Close stdin.
  4. SIGTERM after 5 s.
  5. SIGKILL after another 5 s.
  6. Mark the session `stopped` (`core_shutdown`).
- **Core crash.** Recovery of the running process is **not** attempted:
  - The agent exits when its stdin pipe closes; this is verified, and no orphan remains.
  - On the next start, Core marks live sessions `failed` (`core_restarted`) and cancels their
    approvals (`core_restart`).
- **Recovery is explicit.** `sessions.resume` starts a new process with `--resume <native id>`. It
  keeps the Switchyard session ID and the native ID, and increments `resume_count`. It is refused
  while a process is attached, so there are never duplicates.

## Rejected

- **Re-adopting a running process after a Core crash.** The stdio pipes are gone, so Core could not
  talk to it. Detaching it in a PTY or socket would make Switchyard a terminal multiplexer.
- **Automatic resume on start-up.** It could duplicate work the user didn't ask for and spend money.
