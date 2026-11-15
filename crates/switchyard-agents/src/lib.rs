//! Native coding-agent integration for Switchyard Core.
//!
//! * [`model`] — vendor-neutral types: providers, capabilities, session
//!   states and their legal transitions, approval statuses.
//! * [`event`] — what an adapter reports to Core ([`AdapterOutput`]):
//!   durable agent events, ephemeral live output and permission requests.
//! * [`process`] — supervised child processes: controlled environment, no
//!   shell, bounded line reading, graceful-then-forced termination.
//! * [`session`] — the provider-neutral handle Core uses to drive any adapter.
//! * [`models`] — the model choices each installed CLI offers, and their validation.
//! * [`profiles`] — provider/model capability profiles and capability-aware selection.
//! * [`claude`] — the Claude Code adapter. Claude's wire protocol types are
//!   private to that module; nothing outside it sees raw Claude JSON.
//! * [`codex`] — the OpenAI Codex adapter (app-server JSON-RPC), likewise
//!   self-contained.

pub mod claude;
pub mod codex;
pub mod event;
pub mod model;
pub mod models;
pub mod process;
pub mod profiles;
pub mod session;

pub use event::{AdapterOutput, AgentEvent, LiveChunk, LiveKind, PermissionRequest, TokenUsage, ToolOutcome};
pub use model::{
    AgentProvider, ApprovalStatus, Capabilities, DecisionSource, PermissionDecision, SessionAccess,
    SessionState, StateError, Support,
};
pub use models::{ModelCatalog, ModelOption};
pub use session::{AgentSession, ExitReport, SessionError, SessionUpdate};

/// Classifies a permission request with the adapter that produced it.
pub fn classify(provider: AgentProvider, request: &PermissionRequest) -> switchyard_policy::Operation {
    match provider {
        AgentProvider::ClaudeCode => claude::classify(request),
        AgentProvider::Codex => codex::classify(request),
    }
}

/// What the provider's adapter supports (ADR 0022).
pub fn capabilities(provider: AgentProvider) -> Capabilities {
    match provider {
        AgentProvider::ClaudeCode => claude::capabilities(),
        AgentProvider::Codex => codex::capabilities(),
    }
}

/// Display name for a provider.
pub fn display_name(provider: AgentProvider) -> &'static str {
    match provider {
        AgentProvider::ClaudeCode => "Claude Code",
        AgentProvider::Codex => "Codex",
    }
}

/// CLI version each adapter was verified against.
pub fn tested_version(provider: AgentProvider) -> &'static str {
    match provider {
        AgentProvider::ClaudeCode => claude::TESTED_VERSION,
        AgentProvider::Codex => codex::TESTED_VERSION,
    }
}
