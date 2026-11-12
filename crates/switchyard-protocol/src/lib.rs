//! Switchyard Core IPC protocol, version [`PROTOCOL_VERSION`].
//!
//! Transport: a Unix domain socket carrying newline-delimited JSON frames
//! (one UTF-8 JSON object per line). See `docs/protocols/core-ipc.md`.
//!
//! * [`frame`] — the envelopes exchanged on the wire.
//! * [`methods`] — every request method with its typed params and result.
//! * [`model`] — data types that appear inside results.
//! * [`error`] — machine-readable error codes.
//! * [`codec`] — bounded line framing helpers.
//! * [`paths`] — shared resolution of the data directory and socket path.

pub mod codec;
pub mod error;
pub mod frame;
pub mod methods;
pub mod model;
pub mod paths;

pub use error::{ErrorCode, ProtocolError};
pub use frame::{ClientFrame, RequestId, ServerFrame};
pub use methods::Method;
pub use switchyard_events::EventEnvelope;

/// Wire protocol version implemented by this build.
///
/// Incremented on any incompatible change to frames, method names, params or
/// results. Additive, optional fields do not require a bump.
///
/// History:
/// - 1: milestones 1–2 (`sessions.start {project_id}` ran in the checkout).
/// - 2: milestone 3 workspaces; `sessions.start` requires `workspace_id`
///   (ADR 0015).
/// - 3: milestone 4 approval policies: requests may be allowed or denied by
///   policy without an approval; approvals carry their policy evaluation;
///   decision source `policy`; `approvals.decide` can fail with
///   `policy_denied`; `policies.*` methods (ADR 0016).
/// - 4: milestone 5 Codex: capabilities are `supported` / `unsupported` /
///   `unknown` instead of booleans, with new capabilities; `sessions.start`
///   accepts `provider: "codex"`; `agents.list` lists every provider;
///   `agent.turn_completed` may carry `usage` (ADR 0022).
pub const PROTOCOL_VERSION: u32 = 4;

/// Oldest protocol version this build can serve or use. Core rejects older
/// clients during `initialize`; the client rejects a Core that selects an
/// older version. Version 1 is not served: its `sessions.start` contract
/// (agent in the project checkout) no longer exists. Version 2 is not
/// served: a v2 client would assume every permission request becomes an
/// approval a person decides, and cannot parse the `policy` decision source.
/// Version 3 is not served: a v3 client reads capabilities as booleans and
/// would treat every non-empty string as supported.
pub const MIN_SUPPORTED_PROTOCOL_VERSION: u32 = 4;

/// Default upper bound for a single frame, in bytes.
pub const DEFAULT_MAX_FRAME_BYTES: usize = 1024 * 1024;
