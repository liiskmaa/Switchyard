# ADR 0020: Codex through its native app-server, one process per session

- Status: accepted (2026-11-12)

## Context

Milestone 5 adds OpenAI Codex next to Claude Code. Codex 0.166.0 offers `codex exec` (one-shot) and
`codex app-server` (a long-lived JSON-RPC server, used by Codex's IDE extension). Switchyard needs
multi-turn sessions, streaming, interruptible turns, resume and approval requests answered by a
client. Details are in `docs/architecture/codex-integration.md`.

## Decision

- Use `codex app-server --listen stdio://`, with the stable (non-experimental) API only.
- One app-server process per Switchyard session, cwd = the workspace, supervised by the shared
  process layer (no shell, filtered environment, own process group, kill-on-drop, graceful then forced
  termination). A per-user shared daemon is not used: Switchyard must own the process it supervises.
- One Codex thread per session. The thread id is the session's native id; Codex assigns it, so it is
  recorded when the thread starts. Resume launches a new process and calls `thread/resume`; a failed
  resume fails the session and never silently creates a new thread.
- The JSON-RPC protocol stays inside `switchyard_agents::codex`. Core sees the same `AdapterOutput`
  and the same session handle as for Claude; a provider-neutral `AgentSession` replaces the
  Claude-specific handle in Core.
- Messages sent during a running turn are queued by the adapter and become the next turn
  (`turn/steer` is not used).

## Consequences

- No new database tables: sessions already carry `provider` and `native_session_id`.
- The protocol is tied to the CLI release. The adapter records the verified version (0.166.0) and
  shows other versions as untested; the CLI labels `app-server` experimental.
- Codex threads are stored in the user's `~/.codex`, like Claude's sessions in `~/.claude`.
