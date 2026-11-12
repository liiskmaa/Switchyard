# ADR 0015: Protocol version 2 for workspace sessions

- Status: accepted (2026-11-11). The mechanism stands; the current version is 3 with minimum 3
  ([ADR 0016](0016-approval-policy-model.md)).
- Supersedes the "Protocol compatibility" section of [ADR 0012](0012-project-workspace-session.md).

## Context

Milestone 3 made `sessions.start` require `workspace_id`, so agents never run in the project
checkout. ADR 0012 kept `PROTOCOL_VERSION = 1` regardless. That was wrong in practice:

- Core answered `initialize` with `min(client, 1)` and accepted anything at or above 1. The client
  only rejected a selected version *higher* than its own.
- **New desktop → old Core:** the handshake succeeded at "v1". The first `workspaces.*` call then
  failed with `method_not_found`, and `sessions.start` with `invalid_params`. Both happened after
  the UI was already running, with no hint about the cause. This is a realistic situation, because
  Core outlives the desktop: an updated app finds the previous Core still running.
- **Old desktop → new Core:** the handshake also succeeded. The old UI's `sessions.start
  {project_id}` then failed per request with `invalid_params`. That was safe (no fallback to the
  checkout), but late and unclear.

## Decision

Bump to **protocol 2** and set `MIN_SUPPORTED_PROTOCOL_VERSION = 2` for both Core and the client
(they share the constant in `switchyard-protocol`). Version 1 isn't served, because the v1
`sessions.start` contract no longer exists. Negotiation happens in `initialize`, before any other
request.

- **Old client → new Core.** Core rejects `initialize` with `unsupported_protocol_version`.
  - The message names both sides and the required action.
  - `data` carries `min_supported`, `max_supported`, `client_protocol`, `client_name`,
    `client_version`, `core_version`, and `action: "update_client"`.
  - The connection stays uninitialized, so every other request gets `not_initialized`.
- **New client → old Core.** The old Core selects 1, and the client refuses it.
  - `switchyard-client` returns `ClientError::IncompatibleCore { core_protocol, required,
    core_version, core_pid, client_version }`. The message tells the operator to stop that Core
    (which stops its agent sessions but keeps workspaces) so the matching Core can start, or to
    install a matching Switchyard.
  - The client closes the connection and sends nothing else.
  - The desktop shows the message in its connection state. It doesn't stop the old Core by itself,
    because that would end running agents.
- **No capability framework.** Core and desktop ship together; a version number with a minimum on
  each side is enough. Additive changes (new optional fields, new methods, new events and error
  codes) still don't bump the version. Clients must ignore unknown fields.

## Consequences

- During an upgrade, a still-running old Core must be stopped before the new desktop can use it.
  The error says so.
- Tests cover both directions (`tests/ipc_protocol.rs`): an old client is refused, with structured
  data and `not_initialized` afterwards; the new client refuses a stand-in v1 Core during the
  handshake and sends nothing more.
