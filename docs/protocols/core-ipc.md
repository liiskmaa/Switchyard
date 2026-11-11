# Switchyard Core IPC protocol — version 1

This is the protocol between Switchyard Core and its clients (the desktop app, tests, and future
tools). The source of truth is `crates/switchyard-protocol`, and the TypeScript mirror is
`apps/desktop/src/types/protocol.ts`. This document and those two files must stay in sync.

## Transport

| Property    | Value |
|-------------|-------|
| Socket      | Unix domain stream socket |
| Location    | `$SWITCHYARD_SOCKET`, otherwise `<data dir>/core.sock` |
| Data dir    | `$SWITCHYARD_DATA_DIR`, otherwise `~/Library/Application Support/Switchyard` (macOS) or `$XDG_DATA_HOME/switchyard` / `~/.local/share/switchyard` |
| Access      | The socket is mode `0600` inside a `0700` data dir. Core also rejects peers whose UID differs from the socket owner (`getpeereid`). |
| Framing     | Newline-delimited JSON (JSON Lines): one UTF-8 JSON object per line, terminated by `\n`. `\r\n` is accepted and blank lines are ignored. |
| Frame limit | 1 MiB (`DEFAULT_MAX_FRAME_BYTES`). An oversized frame gets a `frame_too_large` error and the connection is closed, because the stream cannot be resynchronised. |

Socket paths are limited to 103 bytes (the `sun_path` limit on macOS). Core refuses to start with a
longer path; set `SWITCHYARD_SOCKET` to a shorter one if needed.

## Frames

Every frame has a `type` discriminator.

### Client → Core

```json
{"type":"request","id":1,"method":"system.health","params":{}}
```

- `id` is a client-chosen unsigned integer that is echoed in the reply. It should be unique among the
  client's in-flight requests.
- `params` may be omitted. Omitted params mean `{}`.

### Core → client

| `type` | Shape | Meaning |
|--------|-------|---------|
| `response` | `{"type":"response","id":1,"result":{…}}` | Request `id` succeeded. |
| `error` | `{"type":"error","id":1,"error":{"code":"not_found","message":"…","data":{…}}}` | Request `id` failed. `id` is `null` when the failing frame could not be attributed to a request (for example, invalid JSON). `data` is optional. |
| `event` | `{"type":"event","subscription_id":"…","event":{EventEnvelope}}` | An event on an active subscription. |
| `subscription_closed` | `{"type":"subscription_closed","subscription_id":"…","error":{…}}` | Core ended a subscription on its own. `error.data.last_seq` tells the client where to resume. |
| `live` | `{"type":"live","subscription_id":"…","output":{"session_id":"…","seq":42,"kind":"text","text":"Hel"}}` | Ephemeral agent output, sent only on `live.subscribe` subscriptions. `kind` is `text`, `thinking`, `message_start`, `message_stop` or `gap` (which carries `missed: n`). Not journaled, and lossy for slow clients. |
| `shutdown` | `{"type":"shutdown","reason":"SIGTERM"}` | Core is stopping. The connection closes after this frame. |

Requests on one connection run concurrently, so responses can arrive in a different order from the
requests. Clients correlate by `id`.

## Connection lifecycle

1. Connect to the socket.
2. Send `initialize`. Every other method fails with `not_initialized` until `initialize` succeeds. A
   second `initialize` fails with `already_initialized`.
3. Make requests and receive events.
4. Either side may close the connection at any time. Closing the connection ends all of its
   subscriptions and cancels its in-flight requests. **It never affects Core itself or anything Core
   manages.**

### Version negotiation

The client sends the highest protocol version it speaks. Core answers with
`min(client_version, PROTOCOL_VERSION)` if that is at least `MIN_SUPPORTED_PROTOCOL_VERSION`.
Otherwise it fails with `unsupported_protocol_version`, and `data` contains
`{min_supported, max_supported}`. A client must reject an answer higher than it requested.

Additive optional fields do not bump the version; any incompatible change does.

### Reconnection

The protocol is stateless across connections. To reconnect, a client opens a new connection and
calls `initialize` again. It then resubscribes with `after_seq` set to the last `seq` it processed.
Core replays the missed events from the journal before continuing live, so no event is lost and none
is duplicated. `core.instance_id` (from `initialize`) changes whenever Core restarts, which lets a
client detect a restart. `seq` stays continuous across restarts because it lives in the database.

## Methods

| Method | Params | Result |
|--------|--------|--------|
| `initialize` | `{protocol_version: u32, client: {name, version}}` | `{protocol_version, core: CoreInfo}` |
| `system.health` | — | `HealthReport` |
| `system.shutdown` | `{reason?: string}` | `{accepted: true}`. Core then shuts down gracefully. |
| `projects.list` | — | `{projects: Project[]}`, sorted by name |
| `projects.get` | `{project_id}` | `{project: Project}` |
| `projects.add` | `{path: string}` (absolute) | `{project: Project}` |
| `projects.remove` | `{project_id}` | `{project_id}`. Removes the registration only; files are untouched. |
| `projects.git_status` | `{project_id}` | `GitStatusReport` |
| `events.list` | `{after_seq?, project_id?, event_types?: string[], limit?: 1..=1000 (default 100)}` | `{events: EventEnvelope[]}` in ascending `seq` order. Without any filter it returns the **latest** `limit` events; with filters it returns the first `limit` events after `after_seq`. |
| `events.subscribe` | `{after_seq?, project_id?, event_types?}` | `{subscription_id}` |
| `events.unsubscribe` | `{subscription_id}` | `{}` |
| `agents.list` | `{refresh?: bool}` | `{agents: [AgentAvailability]}` (Claude Code: installed, version, tested version, executable, error, auth `{logged_in, method, provider}`, capabilities) |
| `sessions.start` | `{project_id, prompt?, model?, provider?: "claude_code"}` | `{session: Session}`. Launches Claude in the project root. `conflict` if the project already has an attached session; `agent_unavailable` or `agent_failed` (with `data.session_id`) on failure. |
| `sessions.list` | `{project_id?, live_only?, limit?}` | `{sessions: [Session]}`, newest first |
| `sessions.get` | `{session_id}` | `{session, live_text, live_seq}`. `live_text` is the message currently streaming (ephemeral). |
| `sessions.send` | `{session_id, text, queue_if_busy?: bool}` | `{delivery: "started_turn" \| "queued_for_next_turn"}`. Without `queue_if_busy`, sending during a turn fails with `invalid_state`. |
| `sessions.interrupt` | `{session_id}` | `{session}`. The state becomes `interrupted` when the agent confirms. |
| `sessions.stop` | `{session_id}` | `{session}` once the process has exited (state `stopped`) |
| `sessions.resume` | `{session_id, prompt?}` | `{session}`. Continues a terminal session in a new process with the same native ID; `conflict` if it is already attached. |
| `approvals.list` | `{status?, session_id?, project_id?, limit?}` | `{approvals: [Approval]}`, newest first |
| `approvals.decide` | `{approval_id, decision: "approve" \| "deny", note?}` | `{approval}` after the decision is durable. `conflict` if it is no longer pending. `note` is shown to the agent on deny. |
| `live.subscribe` | `{session_id?}` | `{subscription_id}`. Delivers `live` frames. |
| `live.unsubscribe` | `{subscription_id}` | `{}` |

Unknown fields in method params are rejected with `invalid_params`.

### `events.subscribe` semantics

- Core always sends the `response` before any `event` frame for that subscription.
- With `after_seq`, stored events with `seq > after_seq` are replayed, then live events follow. There
  are no gaps or duplicates.
- Without `after_seq`, only events published after the subscription are delivered.
- Delivery is ordered by `seq` and lossless. If a client reads slowly, Core's per-connection queue
  fills and the subscription falls behind the live channel. Core then catches up from the journal
  instead of dropping events (see ADR 0004).
- Each connection can hold at most 16 subscriptions (`overloaded` beyond that).

### Data types

```text
CoreInfo         { version, instance_id: uuid, started_at_ms, pid }
HealthReport     { status: "ok"|"degraded", core: CoreInfo, protocol_version, uptime_ms,
                   database: { initialized, path, journal_mode?, schema_version?, error? },
                   projects_count?, active_sessions_count?, last_event_seq?, connected_clients }
Project          { id, name, root_path, created_at_ms, updated_at_ms, accessible }
GitStatusReport  { project_id, branch?, head_oid?, detached, upstream?, ahead?, behind?,
                   staged, unstaged, untracked, conflicted, clean, checked_at_ms }
EventEnvelope    { seq, id: uuid, schema_version, event_type, occurred_at_ms,
                   project_id?, workspace_id?, session_id?, task_id?, agent_id?, causation_id?,
                   payload: object }
```

The health figures come from a live database query on every call. If that query fails, the counts
are `null`, `database.error` is set, and `status` is `degraded`. They are never cached or estimated.

`Project.accessible` is computed when a project is read. If the directory is missing or unreadable,
the registration is kept and `projects.git_status` returns `repository_unavailable`.

### Event types (schema version 1)

| `event_type` | Payload |
|--------------|---------|
| `core.started` | `{core_version, instance_id}` |
| `core.stopping` | `{instance_id, reason}` |
| `project.registered` | `{project_id, name, root_path}` |
| `project.removed` | `{project_id, root_path}` |
| `agent.started` | `{provider, cli_version, pid, cwd, native_session_id, resumed}` |
| `agent.ready` | `{native_session_id, model, tools}` |
| `agent.state_changed` | `{from, to}` |
| `agent.turn_started` | `{prompt, prompt_truncated, queued}` |
| `agent.message` | `{message_id, text, truncated}` (complete messages only) |
| `agent.tool_started` | `{tool_use_id, tool_name, input, input_truncated}` |
| `agent.tool_completed` / `agent.tool_failed` | `{tool_use_id, tool_name, output, output_truncated[, rejected]}`; `causation_id` is the matching `tool_started` |
| `agent.turn_completed` | `{success, interrupted, reason, stop_reason, duration_ms, cost_usd, result}` |
| `agent.permission_requested` | `{approval_id, tool_name, tool_use_id, input, description, expires_at_ms}` |
| `agent.permission_resolved` | `{approval_id, tool_name, status, source, decided_by, note}` |
| `agent.interrupted`, `agent.session_resumed`, `agent.warning`, `agent.exited`, `agent.failed` | See `crates/switchyard-events/src/envelope.rs` |
| `files.changed` | `{changes: [{path, kind: created\|modified\|deleted\|renamed, from?}], dropped}`: what the filesystem reported (debounced, `.git` and ignored paths excluded) |

Agent events carry `project_id`, `workspace_id`, `session_id` and `agent_id` in the envelope. `events.list` also accepts `session_id`.

Clients must tolerate unknown event types and payload fields.

## Error codes

| Code | When |
|------|------|
| `parse_error` | Invalid JSON or an unknown frame shape. The connection stays open. |
| `frame_too_large` | The frame exceeds the limit. The connection is closed. |
| `not_initialized`, `already_initialized`, `unsupported_protocol_version` | Handshake errors. |
| `method_not_found`, `invalid_params` | Request errors. |
| `not_found`, `conflict` | The entity does not exist, or already exists (for example, a repository is already registered). |
| `invalid_path` | The path is not absolute, does not exist, or is not a directory. |
| `permission_denied` | Core cannot read the path. |
| `not_a_git_repository` | The path is not a Git working tree, is a bare repository, or is a subdirectory of a repository (`data.repository_root` is given in that last case). |
| `repository_unavailable` | A registered repository is gone or unreadable. |
| `git_unavailable`, `git_failed` | `git` cannot be executed, or it failed or timed out (15 s). |
| `agent_unavailable` | The Claude Code CLI is missing, unusable or lacks required flags. |
| `invalid_state` | The operation does not fit the session state (for example, sending during a turn without `queue_if_busy`, or interrupting an idle session). |
| `unsupported` | The agent or provider cannot do this. |
| `agent_failed` | The agent process failed to start or to accept a command. |
| `shutting_down` | Core is stopping. |
| `overloaded` | More than 32 concurrent requests, or too many subscriptions, on one connection. |
| `internal` | An unexpected failure. Details are logged by Core. |

## Example session

```text
→ {"type":"request","id":1,"method":"initialize","params":{"protocol_version":1,"client":{"name":"cli","version":"0.1.0"}}}
← {"type":"response","id":1,"result":{"protocol_version":1,"core":{"version":"0.1.0","instance_id":"01a1…","started_at_ms":1794354584000,"pid":72726}}}
→ {"type":"request","id":2,"method":"events.subscribe","params":{"after_seq":0}}
← {"type":"response","id":2,"result":{"subscription_id":"01a1…"}}
← {"type":"event","subscription_id":"01a1…","event":{"seq":1,"event_type":"core.started",…}}
→ {"type":"request","id":3,"method":"projects.add","params":{"path":"/Users/me/src/api"}}
← {"type":"event","subscription_id":"01a1…","event":{"seq":2,"event_type":"project.registered",…}}
← {"type":"response","id":3,"result":{"project":{…}}}
```

## Tests

`crates/switchyard-core/tests/` holds the protocol tests:

- `ipc_protocol.rs`: raw wire behaviour (handshake, negotiation, errors, frame limits, health).
- `projects.rs`: project registration end to end.
- `events.rs`: delivery, replay, filters and shutdown notification.
- `process.rs`: the real binary, covering disconnect and reconnect, the single-instance lock,
  stale-socket recovery and signals.
