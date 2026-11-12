# ADR 0022: Tri-state agent capabilities, protocol version 4

- Status: accepted (2026-11-12)

## Context

Capabilities were booleans, so the UI could not tell "not supported" from "not verified". With two
providers that differ (Codex reports tokens but not cost; Claude delivers denial reasons; Codex
assigns its own session id), a boolean would either hide differences or overstate support.

## Decision

- Each capability is `supported`, `unsupported` or `unknown`. A capability is `supported` only if the
  adapter implements it and it was verified against the real CLI.
- New capabilities: `tool_activity`, `usage_tokens`, `cost`, `denial_reason_to_agent`,
  `file_read_tool`.
- `agent.turn_completed` gains an optional `usage` object (tokens), omitted when unreported.
- The capability shape is an incompatible change: `PROTOCOL_VERSION` and
  `MIN_SUPPORTED_PROTOCOL_VERSION` become 4 (ADR 0015).
- `sessions.start` accepts `provider: "claude_code" | "codex"`; `agents.list` returns one entry per
  provider.

## Consequences

- Core logic checks `is_supported()`, so an unknown capability is never used.
- Older desktop clients are refused during `initialize`, as intended by ADR 0015.
