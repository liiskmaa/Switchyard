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
pub const PROTOCOL_VERSION: u32 = 2;

/// Oldest protocol version this build can serve or use. Core rejects older
/// clients during `initialize`; the client rejects a Core that selects an
/// older version. Version 1 is not served: its `sessions.start` contract
/// (agent in the project checkout) no longer exists.
pub const MIN_SUPPORTED_PROTOCOL_VERSION: u32 = 2;

/// Default upper bound for a single frame, in bytes.
pub const DEFAULT_MAX_FRAME_BYTES: usize = 1024 * 1024;
