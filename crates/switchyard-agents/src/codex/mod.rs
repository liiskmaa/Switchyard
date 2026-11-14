//! OpenAI Codex adapter.
//!
//! Drives the user's installed `codex` CLI through its native app-server
//! (`codex app-server --listen stdio://`, JSON-RPC 2.0 without the
//! `jsonrpc` field, one message per line). Verified against codex-cli
//! 0.166.0; see `docs/architecture/codex-integration.md` and ADRs 0020–0021.
//!
//! * one app-server process and one Codex thread per Switchyard session;
//! * `approvalPolicy: "untrusted"` + `sandbox: "workspace-write"` so every
//!   command and patch is sent to Switchyard as an approval request;
//! * tools that would act without an approval request are disabled;
//! * [`classify`] maps each request to a vendor-neutral operation.

mod classify;
mod detect;
mod launch;
mod models;
mod protocol;
mod session;

pub use classify::{ADAPTER, classify};
pub use detect::{AuthStatus, CodexInstallation, DetectError, auth_status, detect, locate};
pub use launch::{CONFIG_OVERRIDES, DISABLED_FEATURES, LaunchSpec, build_args, build_env};
pub use models::list_models;
pub use protocol::{APPROVAL_POLICY, Protocol, SANDBOX, Step, ThreadMode, unwrap_shell};
pub use session::{CodexConfig, spawn_session};

use crate::model::{Capabilities, Support};

/// CLI version the adapter was verified against.
pub const TESTED_VERSION: &str = "0.166.0";

pub fn capabilities() -> Capabilities {
    use Support::*;
    Capabilities {
        multi_turn: Supported,
        streaming_text: Supported,
        tool_activity: Supported,
        interrupt_turn: Supported,
        // Held by the adapter and started when the running turn completes.
        queue_message_during_turn: Supported,
        // Codex has `turn/steer`; Switchyard does not use or verify it.
        steer_running_turn: Unknown,
        resume_session: Supported,
        permission_requests: Supported,
        // Codex tells the model only that the request was rejected.
        denial_reason_to_agent: Unsupported,
        // Codex assigns the thread id.
        caller_assigned_session_id: Unsupported,
        usage_tokens: Supported,
        cost: Unsupported,
        // Codex reads files with shell commands.
        file_read_tool: Unsupported,
    }
}
