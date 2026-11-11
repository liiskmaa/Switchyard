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
//!   and a `Bash` ask rule so every shell command needs a decision.

mod detect;
mod launch;
mod session;
mod wire;

pub use detect::{AuthStatus, ClaudeInstallation, DetectError, auth_status, detect, locate};
pub use launch::{LaunchSpec, NativeSession, TOOLS, build_args, build_env};
pub use session::{ClaudeConfig, ClaudeSession, ExitReport, SessionError, SessionUpdate, spawn_session};
pub use wire::Normalizer;

use crate::model::Capabilities;

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
    Capabilities {
        multi_turn: true,
        streaming_text: true,
        interrupt_turn: true,
        queue_message_during_turn: true,
        // Verified: a message sent mid-turn is queued and becomes the next
        // turn; it does not alter the running one.
        steer_running_turn: false,
        resume_session: true,
        permission_requests: true,
        caller_assigned_session_id: true,
    }
}
