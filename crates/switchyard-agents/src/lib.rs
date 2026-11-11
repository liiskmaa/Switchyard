//! Native coding-agent integration for Switchyard Core.
//!
//! * [`model`] — vendor-neutral types: providers, capabilities, session
//!   states and their legal transitions, approval statuses.
//! * [`event`] — what an adapter reports to Core ([`AdapterOutput`]):
//!   durable agent events, ephemeral live output and permission requests.
//! * [`process`] — supervised child processes: controlled environment, no
//!   shell, bounded line reading, graceful-then-forced termination.
//! * [`claude`] — the Claude Code adapter. Claude's wire protocol types are
//!   private to that module; nothing outside it sees raw Claude JSON.

pub mod claude;
pub mod event;
pub mod model;
pub mod process;

pub use event::{AdapterOutput, AgentEvent, LiveChunk, LiveKind, PermissionRequest, ToolOutcome};
pub use model::{
    AgentProvider, ApprovalStatus, Capabilities, DecisionSource, PermissionDecision, SessionState, StateError,
};
