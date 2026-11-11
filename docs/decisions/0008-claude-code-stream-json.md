# ADR 0008: Drive Claude Code with bidirectional stream-json and stdio permission prompts

- Status: accepted (2026-11-11)

## Context

Switchyard must start, observe, steer, interrupt and resume real Claude Code sessions, and route
every permission decision through its own approval workflow.

## Options considered

| Option | Verdict |
|--------|---------|
| `claude -p` per turn (single-shot) plus `--resume` | Rejected: a new process per turn, and no in-band interrupt or permission channel. |
| Interactive TUI in a PTY | Rejected: screen scraping, with no structured events. |
| Agent SDK (TypeScript or Python) in a sidecar | Rejected: an extra runtime and process. The SDK drives the same CLI anyway. |
| **One long-lived `claude -p --input-format stream-json --output-format stream-json` process per session** | **Chosen.** It is multi-turn, structured, interruptible, and its session ID can be assigned. |
| Permissions via an MCP `--permission-prompt-tool mcp__…` server | Rejected for now. The return format is undocumented on reachable pages, it needs a server process per session, and the CLI waits up to 30 s for MCP start-up. |
| **Permissions via `--permission-prompt-tool stdio` (`can_use_tool` control requests)** | **Chosen.** It is a single pipe, the request carries `tool_use_id`, and it is what the official SDK uses. All behaviour was verified against CLI 2.1.322. |

## Consequences

- Switchyard depends on control messages that the CLI docs do not describe. Mitigations:
  - recorded fixtures and a fake CLI pin the shapes;
  - detection checks for the required flags;
  - an incompatible CLI fails visibly, and nothing is ever auto-allowed;
  - the tested version is shown in the UI.
- Steering a running turn is impossible. Mid-turn messages can only be queued, and the UI must say
  so.
- The CLI emits nothing until the first message, so "ready" means the process is alive (after a
  300 ms check); the native handshake (`agent.ready`) arrives with the first turn.
