//! Agent Client Protocol (ACP) adapter, with Gemini CLI as its first agent.
//!
//! Drives the user's installed `gemini --acp` (ACP v1: JSON-RPC 2.0 over
//! stdio). Verified against Gemini CLI 0.70.0; see
//! `docs/architecture/acp-integration.md` and ADR 0043.
//!
//! * one process and one ACP session per Switchyard session;
//! * Switchyard's admin-tier policy makes every file and shell tool ask
//!   Switchyard first, and denies tools that would act outside it;
//! * the session must stay in the agent's ask mode; no file system or
//!   terminal capability is offered;
//! * [`classify`] maps each request to a vendor-neutral operation.

mod classify;
mod detect;
mod launch;
mod models;
mod protocol;
mod session;

pub use classify::{ADAPTER, classify};
pub use detect::{DetectError, GeminiInstallation, detect, locate};
pub use launch::{
    API_KEY_VAR, AUTH_METHOD, LaunchSpec, POLICY_TOML, api_key_from_dotenv, build_args, build_env,
};
pub use models::list_models;
pub use protocol::{Protocol, SessionMode, Step};
pub use session::{GeminiConfig, spawn_session};

use crate::model::{Capabilities, Support};

/// CLI version the adapter was verified against.
pub const TESTED_VERSION: &str = "0.70.0";

pub fn capabilities() -> Capabilities {
    use Support::*;
    Capabilities {
        multi_turn: Supported,
        streaming_text: Supported,
        tool_activity: Supported,
        interrupt_turn: Supported,
        // Held by the adapter and started when the running turn ends.
        queue_message_during_turn: Supported,
        steer_running_turn: Unsupported,
        // `session/load` (verified).
        resume_session: Supported,
        permission_requests: Supported,
        // ACP answers a permission with an option, without a message.
        denial_reason_to_agent: Unsupported,
        caller_assigned_session_id: Unsupported,
        usage_tokens: Supported,
        cost: Unsupported,
        // Reads go through `read_file`, which asks.
        file_read_tool: Supported,
    }
}
