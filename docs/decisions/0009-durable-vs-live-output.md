# ADR 0009: Separate durable agent events from ephemeral live output

- Status: accepted (2026-11-11)

## Decision

- **Durable** (SQLite journal, `synchronous=FULL`): lifecycle and state changes, complete assistant
  messages, tool start and finish (input bounded to 16 KB, output to 8 KB), permission requests and
  resolutions, turn results, warnings, exits and failures, and debounced file-change batches. A
  verified real session of four turns produced about 30 durable events.
- **Ephemeral** (`LiveHub`): text and thinking deltas, plus message start and stop. It is a
  `tokio::broadcast` ring (1024 entries) that is **lossy by design**:
  - Publishers never wait.
  - A slow subscriber receives a `gap` frame telling it how many chunks it missed.
  - Each session keeps only the text of the message currently streaming (at most 64 KB), exposed as
    `sessions.get.live_text`, so a reconnecting UI can show in-progress output.
- **Wire.** Clients opt in with `live.subscribe`, and output arrives in `live` frames. Clients that
  don't subscribe never receive the new frame type, so the protocol version stays 1.

## Consequences

- No token ever reaches SQLite (tested: 60 streamed chunks produced fewer than 15 durable events).
- After a reconnect the UI shows the full durable history plus the in-progress snapshot. It tells
  the user that fragments streamed while disconnected may be missing; once a message completes it
  is in the journal.
- A slow UI cannot slow down an agent. The agent's own stdout reader is bounded by a 256-entry
  channel into Core's supervisor, not by any client.
