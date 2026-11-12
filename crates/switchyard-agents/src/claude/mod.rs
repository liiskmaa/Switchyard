//! Claude Code adapter.
//!
//! Drives the user's installed `claude` CLI in print mode with streaming JSON
//! on both stdin and stdout (verified against CLI 2.1.322; see
//! `docs/architecture/agent-integration.md`):
//!
//! * one long-lived process per Switchyard session, several turns per
//!   process (`--input-format stream-json`);
//! * `--session-id <uuid>` chosen by Switchyard, `--resume <id>` to continue;
//! * permission prompts delivered to Switchyard as `can_use_tool` control
//!   requests (`--permission-prompt-tool stdio`) and answered on stdin;
//! * `interrupt` control requests end the running turn without killing the
//!   process;
//! * `--restricted` + `--strict-mcp-config` + an explicit tool list so that
//!   repository settings, hooks and MCP servers cannot bypass Switchyard,
//!   and an `ask` rule for every enabled tool so each tool use reaches
//!   Switchyard's policy evaluation (ADR 0017);
//! * [`classify`] maps each request to a vendor-neutral operation.

mod classify;
mod detect;
mod launch;
mod session;
mod wire;

pub use classify::{ADAPTER, classify, enabled_tools};
pub use detect::{AuthStatus, ClaudeInstallation, DetectError, auth_status, detect, locate};
pub use launch::{LaunchSpec, NativeSession, TOOLS, build_args, build_env};
pub use session::{ClaudeConfig, spawn_session};
pub use wire::Normalizer;

use crate::model::{Capabilities, Support};

/// CLI version the adapter was verified against.
pub const TESTED_VERSION: &str = "2.1.322";

/// CLI flags the adapter depends on; detection refuses CLIs lacking any.
pub const REQUIRED_FLAGS: &[&str] = &[
    "--input-format",
    "--output-format",
    "--session-id",
    "--resume",
    "--permission-prompt-tool",
    "--restricted",
    "--strict-mcp-config",
    "--include-partial-messages",
    "--tools",
    "--settings",
];

pub fn capabilities() -> Capabilities {
    use Support::*;
    Capabilities {
        multi_turn: Supported,
        streaming_text: Supported,
        tool_activity: Supported,
        interrupt_turn: Supported,
        queue_message_during_turn: Supported,
        // Verified: a message sent mid-turn is queued and becomes the next
        // turn; it does not alter the running one.
        steer_running_turn: Unsupported,
        resume_session: Supported,
        permission_requests: Supported,
        // The deny message is delivered to the model with `can_use_tool`.
        denial_reason_to_agent: Supported,
        caller_assigned_session_id: Supported,
        // Claude reports usage, but Switchyard does not surface it for Claude.
        usage_tokens: Unknown,
        cost: Supported,
        file_read_tool: Supported,
    }
}
