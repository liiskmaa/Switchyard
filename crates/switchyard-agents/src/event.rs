//! What adapters report to Core, in vendor-neutral terms.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Upper bound for text kept in a durable event (assistant message, tool
/// output summary). Longer content is truncated and flagged.
pub const MAX_DURABLE_TEXT: usize = 8 * 1024;
/// Upper bound for serialized tool input kept in a durable event.
pub const MAX_DURABLE_INPUT: usize = 16 * 1024;

/// Truncates `text` to at most `max` bytes on a char boundary.
pub fn truncate(text: &str, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text.to_owned(), false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}

/// Bounds a JSON value's serialized size. Oversized values are replaced by a
/// truncated string preview.
pub fn bound_json(value: &Value, max: usize) -> (Value, bool) {
    let serialized = value.to_string();
    if serialized.len() <= max {
        return (value.clone(), false);
    }
    let (preview, _) = truncate(&serialized, max);
    (Value::String(preview), true)
}

/// Token counts for one turn, as reported by the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    /// Part of `input_tokens` served from the provider's cache.
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    /// Part of `output_tokens` spent on reasoning.
    pub reasoning_output_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcome {
    Succeeded,
    Failed,
    /// The tool did not run because permission was denied or the turn was
    /// interrupted before it ran.
    Rejected,
}

/// Durable, normalized agent activity. Each becomes one journal event.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    /// The agent finished its handshake and reported its native identity.
    Ready {
        native_session_id: String,
        model: Option<String>,
        cwd: Option<String>,
        tools: Vec<String>,
    },
    /// A complete assistant text message (never individual tokens).
    Message {
        message_id: Option<String>,
        text: String,
        truncated: bool,
    },
    ToolStarted {
        tool_use_id: String,
        tool_name: String,
        input: Value,
        input_truncated: bool,
    },
    ToolFinished {
        tool_use_id: String,
        tool_name: Option<String>,
        outcome: ToolOutcome,
        output: String,
        output_truncated: bool,
    },
    /// The agent finished a turn (successfully, with an error, or interrupted).
    TurnFinished {
        success: bool,
        interrupted: bool,
        stop_reason: Option<String>,
        /// Vendor detail such as Claude's `terminal_reason`.
        reason: Option<String>,
        duration_ms: Option<u64>,
        /// Running total the agent reports for the whole session so far (Claude's
        /// `total_cost_usd`; it carries across a resume). Not the cost of this turn.
        cost_usd: Option<f64>,
        /// Cost of this turn alone, when the agent reports it (Claude's
        /// `model_usage_since_last_result`). `None` when unreported, e.g. an aborted turn.
        turn_cost_usd: Option<f64>,
        /// Token counts for this turn, when the adapter reports them.
        usage: Option<TokenUsage>,
        num_turns: Option<u64>,
        result_text: Option<String>,
    },
    /// The agent acknowledged an interrupt request.
    InterruptAcknowledged,
    /// Vendor-reported problem that does not end the session (retries,
    /// malformed output lines).
    Warning {
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveKind {
    /// Assistant text delta.
    Text,
    /// Extended-thinking delta (shown dimmed, if at all).
    Thinking,
    /// A new assistant message started (resets the in-progress buffer).
    MessageStart,
    /// The in-progress assistant message is complete.
    MessageStop,
}

/// Ephemeral output. Never journaled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveChunk {
    pub kind: LiveKind,
    pub text: String,
}

/// A permission request from the agent awaiting Switchyard's decision.
#[derive(Debug, Clone, PartialEq)]
pub struct PermissionRequest {
    /// The vendor's request id; echoed back with the decision.
    pub native_request_id: String,
    pub tool_name: String,
    pub tool_use_id: Option<String>,
    pub input: Value,
    pub description: Option<String>,
    /// Vendor-provided extra context (e.g. Claude permission suggestions).
    pub details: Value,
}

/// Everything an adapter session reports, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum AdapterOutput {
    Event(AgentEvent),
    Live(LiveChunk),
    PermissionRequested(PermissionRequest),
    /// The agent withdrew an unanswered permission request.
    PermissionCancelled {
        native_request_id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_respects_char_boundaries() {
        let (t, cut) = truncate("héllo", 2);
        assert_eq!((t.as_str(), cut), ("h", true));
        assert_eq!(truncate("abc", 10), ("abc".to_owned(), false));
        let (v, cut) = bound_json(&serde_json::json!({"k": "x".repeat(100)}), 20);
        assert!(cut && v.as_str().is_some_and(|s| s.len() <= 20));
    }
}
