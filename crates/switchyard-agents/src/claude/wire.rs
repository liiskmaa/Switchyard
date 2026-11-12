//! Claude Code stream-json wire format (private to the adapter).
//!
//! Parsing is deliberately lenient: each line is decoded as untyped JSON and
//! only the fields Switchyard relies on are read. Unknown message types and
//! extra fields are ignored, so additive CLI changes do not break sessions.

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use crate::event::{
    AdapterOutput, AgentEvent, LiveChunk, LiveKind, MAX_DURABLE_INPUT, MAX_DURABLE_TEXT, PermissionRequest,
    ToolOutcome, bound_json, truncate,
};
use crate::model::PermissionDecision;

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// Converts Claude output lines into [`AdapterOutput`]s. One per session.
#[derive(Debug, Default)]
pub struct Normalizer {
    ready: bool,
    native_session_id: Option<String>,
    /// tool_use_id → tool name, for completing tool events.
    tools: HashMap<String, String>,
    /// tool_use_ids whose permission was denied or cancelled.
    rejected: HashSet<String>,
    /// native permission request id → tool_use_id.
    pending_permissions: HashMap<String, Option<String>>,
    interrupt_ids: HashSet<String>,
    next_control_id: u64,
}

impl Normalizer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn native_session_id(&self) -> Option<&str> {
        self.native_session_id.as_deref()
    }

    /// Records that a tool call will not run (permission denied/cancelled),
    /// so its error result is reported as `Rejected`, not `Failed`.
    pub fn mark_rejected(&mut self, native_request_id: &str) {
        if let Some(Some(tool_use_id)) = self.pending_permissions.remove(native_request_id) {
            self.rejected.insert(tool_use_id);
        }
    }

    pub fn is_pending_permission(&self, native_request_id: &str) -> bool {
        self.pending_permissions.contains_key(native_request_id)
    }

    /// Parses one stdout line.
    pub fn ingest(&mut self, line: &[u8]) -> Vec<AdapterOutput> {
        let message: Value = match serde_json::from_slice(line) {
            Ok(v @ Value::Object(_)) => v,
            Ok(_) | Err(_) => {
                let (preview, _) = truncate(&String::from_utf8_lossy(line), 200);
                return vec![AdapterOutput::Event(AgentEvent::Warning {
                    message: format!("unparseable output line: {preview}"),
                })];
            }
        };
        match message.get("type").and_then(Value::as_str) {
            Some("system") => self.system(&message),
            Some("stream_event") => self.stream_event(&message),
            Some("assistant") => self.assistant(&message),
            Some("user") => self.user(&message),
            Some("result") => vec![self.result(&message)],
            Some("control_request") => self.control_request(&message),
            Some("control_response") => self.control_response(&message),
            Some("control_cancel_request") => self.control_cancel(&message),
            _ => Vec::new(),
        }
    }

    fn system(&mut self, m: &Value) -> Vec<AdapterOutput> {
        match m.get("subtype").and_then(Value::as_str) {
            // `init` is repeated at the start of every turn; report it once.
            Some("init") if !self.ready => {
                let Some(id) = str_field(m, "session_id") else { return Vec::new() };
                self.ready = true;
                self.native_session_id = Some(id.clone());
                let tools = m
                    .get("tools")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(|t| t.as_str().map(str::to_owned)).collect())
                    .unwrap_or_default();
                vec![AdapterOutput::Event(AgentEvent::Ready {
                    native_session_id: id,
                    model: str_field(m, "model"),
                    cwd: str_field(m, "cwd"),
                    tools,
                })]
            }
            Some("api_retry") => vec![AdapterOutput::Event(AgentEvent::Warning {
                message: format!(
                    "API retry {}/{}: {}",
                    m.get("attempt").and_then(Value::as_u64).unwrap_or(0),
                    m.get("max_retries").and_then(Value::as_u64).unwrap_or(0),
                    str_field(m, "error").unwrap_or_else(|| "unknown error".into())
                ),
            })],
            _ => Vec::new(),
        }
    }

    fn stream_event(&mut self, m: &Value) -> Vec<AdapterOutput> {
        // Sub-agent output (parent_tool_use_id set) is not streamed live.
        if m.get("parent_tool_use_id").is_some_and(|p| !p.is_null()) {
            return Vec::new();
        }
        let Some(event) = m.get("event") else { return Vec::new() };
        let live = |kind, text: &str| vec![AdapterOutput::Live(LiveChunk { kind, text: text.to_owned() })];
        match event.get("type").and_then(Value::as_str) {
            Some("message_start") => live(LiveKind::MessageStart, ""),
            Some("message_stop") => live(LiveKind::MessageStop, ""),
            Some("content_block_delta") => {
                let delta = event.get("delta").unwrap_or(&Value::Null);
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => delta
                        .get("text")
                        .and_then(Value::as_str)
                        .map(|t| live(LiveKind::Text, t))
                        .unwrap_or_default(),
                    Some("thinking_delta") => delta
                        .get("thinking")
                        .and_then(Value::as_str)
                        .map(|t| live(LiveKind::Thinking, t))
                        .unwrap_or_default(),
                    _ => Vec::new(),
                }
            }
            _ => Vec::new(),
        }
    }

    fn assistant(&mut self, m: &Value) -> Vec<AdapterOutput> {
        let message = m.get("message").unwrap_or(&Value::Null);
        let message_id = str_field(message, "id");
        let blocks = message.get("content").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut out = Vec::new();
        for block in &blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    let text = block.get("text").and_then(Value::as_str).unwrap_or_default();
                    if text.trim().is_empty() {
                        continue;
                    }
                    let (text, truncated) = truncate(text, MAX_DURABLE_TEXT);
                    out.push(AdapterOutput::Event(AgentEvent::Message {
                        message_id: message_id.clone(),
                        text,
                        truncated,
                    }));
                }
                Some("tool_use") => {
                    let (Some(id), Some(name)) = (str_field(block, "id"), str_field(block, "name")) else {
                        continue;
                    };
                    self.tools.insert(id.clone(), name.clone());
                    let (input, input_truncated) =
                        bound_json(block.get("input").unwrap_or(&Value::Null), MAX_DURABLE_INPUT);
                    out.push(AdapterOutput::Event(AgentEvent::ToolStarted {
                        tool_use_id: id,
                        tool_name: name,
                        input,
                        input_truncated,
                    }));
                }
                _ => {}
            }
        }
        out
    }

    fn user(&mut self, m: &Value) -> Vec<AdapterOutput> {
        let content = m.get("message").and_then(|msg| msg.get("content"));
        let Some(blocks) = content.and_then(Value::as_array) else { return Vec::new() };
        let mut out = Vec::new();
        for block in blocks {
            if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                continue;
            }
            let Some(tool_use_id) = str_field(block, "tool_use_id") else { continue };
            let is_error = block.get("is_error").and_then(Value::as_bool).unwrap_or(false);
            let outcome = if self.rejected.remove(&tool_use_id) {
                ToolOutcome::Rejected
            } else if is_error {
                ToolOutcome::Failed
            } else {
                ToolOutcome::Succeeded
            };
            let text = tool_result_text(block.get("content").unwrap_or(&Value::Null));
            let (output, output_truncated) = truncate(&text, MAX_DURABLE_TEXT);
            out.push(AdapterOutput::Event(AgentEvent::ToolFinished {
                tool_name: self.tools.remove(&tool_use_id),
                tool_use_id,
                outcome,
                output,
                output_truncated,
            }));
        }
        out
    }

    fn result(&mut self, m: &Value) -> AdapterOutput {
        let subtype = str_field(m, "subtype");
        let is_error = m.get("is_error").and_then(Value::as_bool).unwrap_or(true);
        let terminal = str_field(m, "terminal_reason");
        let interrupted = terminal.as_deref().is_some_and(|r| r.starts_with("aborted"));
        let result_text = str_field(m, "result").map(|t| truncate(&t, MAX_DURABLE_TEXT).0);
        AdapterOutput::Event(AgentEvent::TurnFinished {
            success: subtype.as_deref() == Some("success") && !is_error,
            interrupted,
            stop_reason: str_field(m, "stop_reason"),
            reason: terminal.or(subtype),
            duration_ms: m.get("duration_ms").and_then(Value::as_u64),
            cost_usd: m.get("total_cost_usd").and_then(Value::as_f64),
            turn_cost_usd: turn_cost(m),
            num_turns: m.get("num_turns").and_then(Value::as_u64),
            result_text,
        })
    }

    fn control_request(&mut self, m: &Value) -> Vec<AdapterOutput> {
        let request = m.get("request").unwrap_or(&Value::Null);
        let Some(request_id) = str_field(m, "request_id") else { return Vec::new() };
        match request.get("subtype").and_then(Value::as_str) {
            Some("can_use_tool") => {
                let tool_use_id = str_field(request, "tool_use_id");
                self.pending_permissions.insert(request_id.clone(), tool_use_id.clone());
                let details = json!({
                    "display_name": request.get("display_name"),
                    "permission_suggestions": request.get("permission_suggestions"),
                    "blocked_path": request.get("blocked_path"),
                    "decision_reason": request.get("decision_reason"),
                });
                vec![AdapterOutput::PermissionRequested(PermissionRequest {
                    native_request_id: request_id,
                    tool_name: str_field(request, "tool_name").unwrap_or_else(|| "unknown".into()),
                    tool_use_id,
                    input: request.get("input").cloned().unwrap_or(Value::Null),
                    description: str_field(request, "description"),
                    details,
                })]
            }
            other => vec![AdapterOutput::Event(AgentEvent::Warning {
                message: format!("unsupported control request {other:?} (request {request_id})"),
            })],
        }
    }

    fn control_response(&mut self, m: &Value) -> Vec<AdapterOutput> {
        let response = m.get("response").unwrap_or(&Value::Null);
        let id = str_field(response, "request_id").unwrap_or_default();
        if self.interrupt_ids.remove(&id) {
            return vec![AdapterOutput::Event(AgentEvent::InterruptAcknowledged)];
        }
        if response.get("subtype").and_then(Value::as_str) == Some("error") {
            return vec![AdapterOutput::Event(AgentEvent::Warning {
                message: format!(
                    "control request {id} failed: {}",
                    str_field(response, "error").unwrap_or_default()
                ),
            })];
        }
        Vec::new()
    }

    fn control_cancel(&mut self, m: &Value) -> Vec<AdapterOutput> {
        let Some(id) = str_field(m, "request_id") else { return Vec::new() };
        if !self.pending_permissions.contains_key(&id) {
            return Vec::new();
        }
        self.mark_rejected(&id);
        vec![AdapterOutput::PermissionCancelled { native_request_id: id }]
    }

    // --- outbound -----------------------------------------------------------

    /// A user turn.
    pub fn encode_user_message(&self, text: &str) -> Value {
        json!({
            "type": "user",
            "message": {"role": "user", "content": text},
            "parent_tool_use_id": null,
            "session_id": self.native_session_id.as_deref().unwrap_or(""),
        })
    }

    /// The answer to a `can_use_tool` request. Allow passes the original
    /// input unchanged (`updatedInput` is required by the CLI).
    pub fn encode_permission(
        &mut self,
        native_request_id: &str,
        decision: &PermissionDecision,
        original_input: &Value,
    ) -> Value {
        let response = match decision {
            PermissionDecision::Allow => {
                self.pending_permissions.remove(native_request_id);
                json!({"behavior": "allow", "updatedInput": original_input})
            }
            PermissionDecision::Deny { message } => {
                self.mark_rejected(native_request_id);
                json!({"behavior": "deny", "message": message})
            }
        };
        json!({
            "type": "control_response",
            "response": {"subtype": "success", "request_id": native_request_id, "response": response},
        })
    }

    pub fn encode_interrupt(&mut self) -> Value {
        self.next_control_id += 1;
        let id = format!("switchyard-interrupt-{}", self.next_control_id);
        self.interrupt_ids.insert(id.clone());
        json!({"type": "control_request", "request_id": id, "request": {"subtype": "interrupt"}})
    }
}

fn tool_result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => {
            parts.iter().filter_map(|p| p.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n")
        }
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Cost of one turn: the sum of `costUSD` over `model_usage_since_last_result`
/// (one entry per model used since the previous `result`). `total_cost_usd` is the
/// session's running total instead. `None` when the CLI reports no usage for the
/// turn (an aborted turn reports `{}`).
fn turn_cost(m: &Value) -> Option<f64> {
    let models = m.get("model_usage_since_last_result")?.as_object()?;
    let costs: Vec<f64> = models.values().filter_map(|u| u.get("costUSD").and_then(Value::as_f64)).collect();
    if costs.is_empty() { None } else { Some(costs.iter().sum()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/claude-2.1.322");

    fn replay(name: &str) -> (Normalizer, Vec<AdapterOutput>) {
        let text = std::fs::read_to_string(format!("{FIXTURES}/{name}.stdout.jsonl")).unwrap();
        let mut n = Normalizer::new();
        let mut out = Vec::new();
        for line in text.lines() {
            out.extend(n.ingest(line.as_bytes()));
        }
        (n, out)
    }

    fn events(out: &[AdapterOutput]) -> Vec<&AgentEvent> {
        out.iter().filter_map(|o| if let AdapterOutput::Event(e) = o { Some(e) } else { None }).collect()
    }

    #[test]
    fn write_allow_transcript() {
        let (n, out) = replay("write_allow");
        let ev = events(&out);
        assert!(matches!(ev[0], AgentEvent::Ready { .. }));
        assert_eq!(
            ev.iter().filter(|e| matches!(e, AgentEvent::Ready { .. })).count(),
            1,
            "init per turn reported once"
        );
        assert!(n.native_session_id().is_some());
        let perms: Vec<_> =
            out.iter().filter(|o| matches!(o, AdapterOutput::PermissionRequested(_))).collect();
        assert_eq!(perms.len(), 1);
        let AdapterOutput::PermissionRequested(p) = perms[0] else { unreachable!() };
        assert_eq!(p.tool_name, "Write");
        assert!(p.tool_use_id.as_deref().is_some_and(|id| id.starts_with("toolu_")));
        assert_eq!(p.input["content"], "switchyard");
        assert!(
            ev.iter().any(|e| matches!(e, AgentEvent::ToolStarted { tool_name, .. } if tool_name == "Write"))
        );
        assert!(ev.iter().any(|e| matches!(e, AgentEvent::ToolFinished { outcome: ToolOutcome::Succeeded, tool_name: Some(t), .. } if t == "Write")));
        assert!(ev.iter().any(|e| matches!(e, AgentEvent::Message { text, .. } if text.starts_with("DONE"))));
        let turns: Vec<_> =
            ev.iter().filter(|e| matches!(e, AgentEvent::TurnFinished { success: true, .. })).collect();
        assert_eq!(turns.len(), 2);
        let live_text: String = out
            .iter()
            .filter_map(|o| match o {
                AdapterOutput::Live(c) if c.kind == LiveKind::Text => Some(c.text.as_str()),
                _ => None,
            })
            .collect();
        assert!(live_text.contains("DONE") && live_text.contains("SECOND"));
    }

    #[test]
    fn denied_tool_is_reported_as_rejected() {
        let text = std::fs::read_to_string(format!("{FIXTURES}/write_deny.stdout.jsonl")).unwrap();
        let mut n = Normalizer::new();
        let mut finished = Vec::new();
        for line in text.lines() {
            for o in n.ingest(line.as_bytes()) {
                if let AdapterOutput::PermissionRequested(p) = &o {
                    let msg = n.encode_permission(
                        &p.native_request_id,
                        &PermissionDecision::Deny { message: "no".into() },
                        &p.input,
                    );
                    assert_eq!(msg["response"]["response"]["behavior"], "deny");
                }
                if let AdapterOutput::Event(e @ AgentEvent::ToolFinished { .. }) = o {
                    finished.push(e);
                }
            }
        }
        assert!(matches!(&finished[..], [AgentEvent::ToolFinished { outcome: ToolOutcome::Rejected, .. }]));
    }

    fn turn_costs(out: &[AdapterOutput]) -> Vec<(Option<f64>, Option<f64>)> {
        events(out)
            .into_iter()
            .filter_map(|e| match e {
                AgentEvent::TurnFinished { cost_usd, turn_cost_usd, .. } => Some((*cost_usd, *turn_cost_usd)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn turn_cost_is_per_turn_and_cost_usd_is_the_session_total() {
        let close = |a: Option<f64>, b: f64| a.is_some_and(|a| (a - b).abs() < 1e-9);
        // Two turns in one process: the total grows, each turn reports its own cost.
        let two = turn_costs(&replay("write_allow").1);
        assert_eq!(two.len(), 2);
        assert!(close(two[0].0, 0.00203249) && close(two[0].1, 0.00203249));
        assert!(close(two[1].0, 0.00224667) && close(two[1].1, 0.00021418));
        // A resumed process carries the earlier total; the turn itself is small.
        let resumed = turn_costs(&replay("resume").1);
        assert!(close(resumed[0].0, 0.00453177) && close(resumed[0].1, 0.00028583));
        // An aborted turn reports no usage: unknown, not zero.
        let interrupted = turn_costs(&replay("interrupt").1);
        assert_eq!(interrupted[0].1, None);
    }

    #[test]
    fn interrupt_transcript_reports_interrupted_turn() {
        let (_, out) = replay("interrupt");
        let ev = events(&out);
        assert!(
            ev.iter()
                .any(|e| matches!(e, AgentEvent::TurnFinished { interrupted: true, success: false, .. }))
        );
        assert!(
            ev.iter()
                .any(|e| matches!(e, AgentEvent::TurnFinished { success: true, interrupted: false, .. }))
        );
    }

    #[test]
    fn cancelled_permission_is_reported_and_tool_rejected() {
        let (_, out) = replay("interrupt_pending_permission");
        let requested = out
            .iter()
            .find_map(|o| match o {
                AdapterOutput::PermissionRequested(p) => Some(p.native_request_id.clone()),
                _ => None,
            })
            .unwrap();
        assert!(out.iter().any(|o| matches!(o, AdapterOutput::PermissionCancelled { native_request_id } if *native_request_id == requested)));
        let ev = events(&out);
        assert!(
            ev.iter().any(|e| matches!(e, AgentEvent::ToolFinished { outcome: ToolOutcome::Rejected, .. }))
        );
        assert!(ev.iter().any(|e| matches!(e, AgentEvent::TurnFinished { interrupted: true, reason: Some(r), .. } if r == "aborted_tools")));
    }

    #[test]
    fn parallel_bash_requests_and_resume_identity() {
        let (_, out) = replay("bash_ask");
        let perms: Vec<_> = out
            .iter()
            .filter_map(|o| match o {
                AdapterOutput::PermissionRequested(p) => Some(p),
                _ => None,
            })
            .collect();
        assert_eq!(perms.len(), 2);
        assert!(perms.iter().all(|p| p.tool_name == "Bash"));
        // The resume transcript was recorded with `--resume 79affdf0-…` (see
        // the fixture README): the CLI keeps reporting that same id.
        let (n, out) = replay("resume");
        assert_eq!(n.native_session_id(), Some("79affdf0-a2dd-4729-8a50-88aef6a07705"));
        assert!(
            events(&out).iter().any(|e| matches!(e, AgentEvent::Message { text, .. } if text == "notes.txt"))
        );
    }

    #[test]
    fn queued_message_transcript_has_two_turns() {
        let (_, out) = replay("queued_message");
        let turns = events(&out).into_iter().filter(|e| matches!(e, AgentEvent::TurnFinished { .. })).count();
        assert_eq!(turns, 2);
    }

    #[test]
    fn malformed_and_unknown_lines_are_safe() {
        let mut n = Normalizer::new();
        assert!(matches!(&n.ingest(b"{not json")[..], [AdapterOutput::Event(AgentEvent::Warning { .. })]));
        assert!(matches!(&n.ingest(b"[1,2]")[..], [AdapterOutput::Event(AgentEvent::Warning { .. })]));
        assert!(n.ingest(br#"{"type":"brand_new_type","x":1}"#).is_empty());
        assert!(n.ingest(br#"{"type":"assistant"}"#).is_empty());
        assert!(n.ingest(br#"{"type":"control_request","request":{"subtype":"can_use_tool"}}"#).is_empty());
    }

    #[test]
    fn outbound_encodings() {
        let mut n = Normalizer::new();
        let user = n.encode_user_message("hi");
        assert_eq!(user["message"]["content"], "hi");
        let allow = n.encode_permission("r1", &PermissionDecision::Allow, &json!({"a": 1}));
        assert_eq!(allow["response"]["response"], json!({"behavior": "allow", "updatedInput": {"a": 1}}));
        let int = n.encode_interrupt();
        let id = int["request_id"].as_str().unwrap().to_owned();
        let ack = n.ingest(json!({"type":"control_response","response":{"subtype":"success","request_id": id,"response":{}}}).to_string().as_bytes());
        assert_eq!(ack, vec![AdapterOutput::Event(AgentEvent::InterruptAcknowledged)]);
    }
}
