//! The Agent Client Protocol (ACP v1) as a pure state machine: JSON-RPC 2.0
//! over stdio, one message per line. Switchyard is the client.
//!
//! Verified against Gemini CLI 0.70.0 (`gemini --acp`); see
//! `docs/architecture/acp-integration.md`. Nothing here performs I/O: every
//! call returns a [`Step`] with the messages to write and the normalized
//! outputs for Core.
//!
//! Safety properties kept here:
//! * the client advertises no file system and no terminal capability, so
//!   the agent never asks Switchyard to read, write or run anything itself;
//! * a session that is not in the agent's `default` (ask) mode, or that
//!   switches modes, is refused: `yolo`/`autoEdit` would act without asking;
//! * a permission is only ever answered with an `allow_once` option or a
//!   rejection, never an "always" option;
//! * an interrupt cancels the turn and answers every pending permission
//!   request as cancelled, as ACP requires.

use std::collections::{HashMap, VecDeque};

use serde_json::{Value, json};

use crate::event::{
    AdapterOutput, AgentEvent, LiveChunk, LiveKind, PermissionRequest, TokenUsage, ToolOutcome, truncate,
};
use crate::model::PermissionDecision;

pub const PROTOCOL_VERSION: u64 = 1;
/// The only session mode Switchyard accepts: the agent asks before acting.
pub const ASK_MODE: &str = "default";
/// Longest tool output kept in an event.
const MAX_TOOL_OUTPUT: usize = 8 * 1024;

/// How the session is obtained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionMode {
    New,
    Load(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Initializing,
    Authenticating,
    OpeningSession,
    Ready,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Own {
    Initialize,
    Authenticate,
    Session,
    Prompt,
}

/// A permission request awaiting Switchyard's decision.
#[derive(Debug, Clone)]
struct Pending {
    id: Value,
    allow_once: Option<String>,
    reject_once: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct Tool {
    name: String,
    finished: bool,
}

#[derive(Debug, Default)]
struct Turn {
    /// Text of the assistant message being streamed.
    message: String,
    /// All assistant text of the turn (the result text).
    all_text: String,
    cancel_sent: bool,
}

/// Output of one protocol step.
#[derive(Debug, Default, PartialEq)]
pub struct Step {
    pub outputs: Vec<AdapterOutput>,
    /// Messages to write to stdin, in order.
    pub writes: Vec<Value>,
    /// The session cannot continue; the driver stops the process.
    pub fatal: Option<String>,
}

impl Step {
    fn event(&mut self, e: AgentEvent) {
        self.outputs.push(AdapterOutput::Event(e));
    }
    fn live(&mut self, kind: LiveKind, text: impl Into<String>) {
        self.outputs.push(AdapterOutput::Live(LiveChunk { kind, text: text.into() }));
    }
    fn warn(&mut self, message: impl Into<String>) {
        self.event(AgentEvent::Warning { message: message.into() });
    }
}

#[derive(Debug)]
pub struct Protocol {
    phase: Phase,
    mode: SessionMode,
    cwd: String,
    /// The login method to select (`authenticate`), if any.
    auth_method: Option<String>,
    client_version: String,
    next_id: u64,
    own: HashMap<u64, Own>,
    session_id: Option<String>,
    model: Option<String>,
    turn: Option<Turn>,
    queue: VecDeque<String>,
    interrupt_pending: bool,
    pending: HashMap<String, Pending>,
    tools: HashMap<String, Tool>,
    /// Makes native request ids unique per process (agents number from 0).
    instance: String,
}

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn raw_id(id: &Value) -> String {
    match id {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Text of an ACP content block (or of a tool call content entry).
fn content_text(c: &Value) -> Option<String> {
    match str_of(c, "type") {
        Some("text") => str_of(c, "text").map(str::to_owned),
        Some("content") => c.get("content").and_then(content_text),
        Some("diff") => Some(format!("diff of {}", str_of(c, "path").unwrap_or("a file"))),
        _ => None,
    }
}

impl Protocol {
    pub fn new(
        mode: SessionMode,
        cwd: impl Into<String>,
        auth_method: Option<String>,
        client_version: &str,
    ) -> Self {
        Self {
            phase: Phase::Initializing,
            mode,
            cwd: cwd.into(),
            auth_method,
            client_version: client_version.to_owned(),
            next_id: 1,
            own: HashMap::new(),
            session_id: None,
            model: None,
            turn: None,
            queue: VecDeque::new(),
            interrupt_pending: false,
            pending: HashMap::new(),
            tools: HashMap::new(),
            instance: uuid::Uuid::now_v7().simple().to_string(),
        }
    }

    pub fn request_key(&self, id: &Value) -> String {
        format!("{}/{}", self.instance, raw_id(id))
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub fn turn_running(&self) -> bool {
        self.turn.is_some()
    }

    fn request(&mut self, kind: Own, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.own.insert(id, kind);
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }

    /// The opening `initialize` request: no file system, no terminal.
    pub fn start(&mut self) -> Vec<Value> {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false },
            "clientInfo": { "name": "switchyard", "title": "Switchyard", "version": self.client_version },
        });
        vec![self.request(Own::Initialize, "initialize", params)]
    }

    fn open_session(&mut self, step: &mut Step) {
        self.phase = Phase::OpeningSession;
        let msg = match &self.mode {
            SessionMode::New => {
                let p = json!({ "cwd": self.cwd, "mcpServers": [] });
                self.request(Own::Session, "session/new", p)
            }
            SessionMode::Load(id) => {
                let p = json!({ "sessionId": id, "cwd": self.cwd, "mcpServers": [] });
                self.request(Own::Session, "session/load", p)
            }
        };
        step.writes.push(msg);
    }

    fn start_turn(&mut self, text: String, step: &mut Step) {
        let Some(session) = self.session_id.clone() else {
            self.queue.push_front(text);
            return;
        };
        self.turn = Some(Turn::default());
        let p = json!({ "sessionId": session, "prompt": [{ "type": "text", "text": text }] });
        let msg = self.request(Own::Prompt, "session/prompt", p);
        step.writes.push(msg);
    }

    /// A user message: a turn now, or queued until the session is ready or
    /// the running turn ends.
    pub fn send_user_message(&mut self, text: String) -> Step {
        let mut step = Step::default();
        if self.phase == Phase::Failed {
            step.fatal = Some("the agent session is not available".into());
            return step;
        }
        if self.phase == Phase::Ready && self.turn.is_none() && self.queue.is_empty() {
            self.start_turn(text, &mut step);
        } else {
            self.queue.push_back(text);
        }
        step
    }

    /// Cancels the running turn (`session/cancel`) and answers every pending
    /// permission request as cancelled.
    pub fn interrupt(&mut self) -> Result<Step, String> {
        let mut step = Step::default();
        let Some(turn) = &mut self.turn else { return Err("no turn is running".into()) };
        let Some(session) = self.session_id.clone() else {
            self.interrupt_pending = true;
            return Ok(step);
        };
        if !turn.cancel_sent {
            turn.cancel_sent = true;
            step.writes.push(
                json!({ "jsonrpc": "2.0", "method": "session/cancel", "params": { "sessionId": session } }),
            );
        }
        let keys: Vec<String> = self.pending.keys().cloned().collect();
        for key in keys {
            if let Some(p) = self.pending.remove(&key) {
                step.writes.push(json!({ "jsonrpc": "2.0", "id": p.id, "result": { "outcome": { "outcome": "cancelled" } } }));
                step.outputs.push(AdapterOutput::PermissionCancelled { native_request_id: key });
            }
        }
        Ok(step)
    }

    /// Answers a permission request: allow is the `allow_once` option for
    /// exactly this request; anything else rejects it.
    pub fn decide(
        &mut self,
        native_request_id: &str,
        decision: &PermissionDecision,
    ) -> Result<Value, String> {
        let p = self
            .pending
            .remove(native_request_id)
            .ok_or_else(|| format!("no pending request {native_request_id}"))?;
        let outcome = match (decision, &p.allow_once, &p.reject_once) {
            (PermissionDecision::Allow, Some(allow), _) => {
                json!({ "outcome": "selected", "optionId": allow })
            }
            (_, _, Some(reject)) => json!({ "outcome": "selected", "optionId": reject }),
            _ => json!({ "outcome": "cancelled" }),
        };
        Ok(json!({ "jsonrpc": "2.0", "id": p.id, "result": { "outcome": outcome } }))
    }

    /// Processes one stdout line.
    pub fn ingest(&mut self, line: &[u8]) -> Step {
        let mut step = Step::default();
        let msg: Value = match serde_json::from_slice(line) {
            Ok(v) => v,
            Err(e) => {
                if !line.iter().all(u8::is_ascii_whitespace) {
                    step.warn(format!("unparseable output line: {e}"));
                }
                return step;
            }
        };
        let method = str_of(&msg, "method").map(str::to_owned);
        match (msg.get("id"), method) {
            (Some(id), Some(method)) => self.agent_request(id.clone(), &method, &msg, &mut step),
            (None, Some(method)) => {
                self.notification(&method, msg.get("params").unwrap_or(&Value::Null), &mut step)
            }
            (Some(id), None) => self.response(id, &msg, &mut step),
            (None, None) => step.warn("output line is not a JSON-RPC message"),
        }
        step
    }

    fn fail(&mut self, step: &mut Step, reason: String) {
        self.phase = Phase::Failed;
        step.warn(reason.clone());
        step.fatal = Some(reason);
    }

    fn response(&mut self, id: &Value, msg: &Value, step: &mut Step) {
        let Some(kind) = id.as_u64().and_then(|n| self.own.remove(&n)) else {
            step.warn(format!("response to an unknown request {id}"));
            return;
        };
        let error = msg.get("error").map(|e| str_of(e, "message").unwrap_or("unknown error").to_owned());
        let result = msg.get("result").unwrap_or(&Value::Null);
        match kind {
            Own::Initialize => {
                if let Some(e) = error {
                    return self.fail(step, format!("the agent refused initialization: {e}"));
                }
                let version = result.get("protocolVersion").and_then(Value::as_u64);
                if version != Some(PROTOCOL_VERSION) {
                    return self.fail(
                        step,
                        format!("the agent speaks ACP version {version:?}, not {PROTOCOL_VERSION}"),
                    );
                }
                match self.auth_method.clone() {
                    Some(method) => {
                        let offered = result
                            .get("authMethods")
                            .and_then(Value::as_array)
                            .is_some_and(|a| a.iter().any(|m| str_of(m, "id") == Some(method.as_str())));
                        if !offered {
                            return self.fail(step, format!("the agent does not offer the {method} login"));
                        }
                        self.phase = Phase::Authenticating;
                        let m =
                            self.request(Own::Authenticate, "authenticate", json!({ "methodId": method }));
                        step.writes.push(m);
                    }
                    None => self.open_session(step),
                }
            }
            Own::Authenticate => {
                if let Some(e) = error {
                    return self.fail(step, format!("the agent's login failed: {e}"));
                }
                self.open_session(step);
            }
            Own::Session => {
                if let Some(e) = error {
                    let what = match &self.mode {
                        SessionMode::New => "cannot start an agent session".to_owned(),
                        SessionMode::Load(id) => format!("cannot resume agent session {id}"),
                    };
                    return self.fail(step, format!("{what}: {e}"));
                }
                let id = match &self.mode {
                    SessionMode::New => match str_of(result, "sessionId") {
                        Some(id) => id.to_owned(),
                        None => return self.fail(step, "the agent did not report a session id".into()),
                    },
                    SessionMode::Load(id) => id.clone(),
                };
                // Refuse a session that would act without asking.
                let current = result.get("modes").and_then(|m| str_of(m, "currentModeId"));
                if let Some(mode) = current
                    && mode != ASK_MODE
                {
                    return self.fail(
                        step,
                        format!("the agent opened the session in mode {mode:?}; Switchyard only runs it in {ASK_MODE:?}"),
                    );
                }
                self.model =
                    result.get("models").and_then(|m| str_of(m, "currentModelId")).map(str::to_owned);
                self.session_id = Some(id.clone());
                self.phase = Phase::Ready;
                step.event(AgentEvent::Ready {
                    native_session_id: id,
                    model: self.model.clone(),
                    cwd: Some(self.cwd.clone()),
                    tools: Vec::new(),
                });
                if std::mem::take(&mut self.interrupt_pending) {
                    self.turn = None;
                }
                if let Some(text) = self.queue.pop_front() {
                    self.start_turn(text, step);
                }
            }
            Own::Prompt => {
                let turn = self.turn.take().unwrap_or_default();
                self.flush_message_of(&turn, step);
                // Tools still open when the turn ends did not finish.
                let open: Vec<(String, String)> = self
                    .tools
                    .iter()
                    .filter(|(_, t)| !t.finished)
                    .map(|(id, t)| (id.clone(), t.name.clone()))
                    .collect();
                for (id, name) in open {
                    step.event(AgentEvent::ToolFinished {
                        tool_use_id: id,
                        tool_name: Some(name),
                        outcome: ToolOutcome::Rejected,
                        output: String::new(),
                        output_truncated: false,
                    });
                }
                self.tools.clear();
                // Requests still pending belong to the finished turn.
                for (key, _) in self.pending.drain() {
                    step.outputs.push(AdapterOutput::PermissionCancelled { native_request_id: key });
                }
                let stop = str_of(result, "stopReason").map(str::to_owned);
                if stop.as_deref() == Some("cancelled") {
                    step.event(AgentEvent::InterruptAcknowledged);
                }
                let usage = result.get("usage").map(|u| {
                    let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                    TokenUsage {
                        input_tokens: n("inputTokens"),
                        cached_input_tokens: n("cachedReadTokens"),
                        output_tokens: n("outputTokens"),
                        reasoning_output_tokens: n("thoughtTokens"),
                        total_tokens: n("totalTokens"),
                    }
                });
                let text = turn.all_text.trim();
                step.event(AgentEvent::TurnFinished {
                    success: error.is_none() && stop.as_deref() == Some("end_turn"),
                    interrupted: stop.as_deref() == Some("cancelled"),
                    stop_reason: stop.clone(),
                    reason: Some(match (&error, &stop) {
                        (Some(e), _) => format!("failed: {}", truncate(e, 1000).0),
                        (None, Some(s)) => s.clone(),
                        (None, None) => "ended".into(),
                    }),
                    duration_ms: None,
                    cost_usd: None,
                    turn_cost_usd: None,
                    usage,
                    num_turns: None,
                    result_text: (!text.is_empty()).then(|| text.to_owned()),
                });
                if self.phase == Phase::Ready
                    && let Some(text) = self.queue.pop_front()
                {
                    self.start_turn(text, step);
                }
            }
        }
    }

    /// The streamed assistant message is complete (a tool started, or the turn ended).
    fn flush_message(&mut self, step: &mut Step) {
        if let Some(turn) = &mut self.turn {
            let text = std::mem::take(&mut turn.message);
            if !text.trim().is_empty() {
                step.live(LiveKind::MessageStop, "");
                let (t, cut) = truncate(&text, 64 * 1024);
                step.event(AgentEvent::Message { message_id: None, text: t, truncated: cut });
            }
        }
    }

    fn flush_message_of(&self, turn: &Turn, step: &mut Step) {
        if !turn.message.trim().is_empty() {
            step.live(LiveKind::MessageStop, "");
            let (t, cut) = truncate(&turn.message, 64 * 1024);
            step.event(AgentEvent::Message { message_id: None, text: t, truncated: cut });
        }
    }

    fn notification(&mut self, method: &str, params: &Value, step: &mut Step) {
        if method != "session/update" {
            return;
        }
        // A loaded session replays its history before `session/load` answers: not new activity.
        if self.phase != Phase::Ready {
            return;
        }
        if str_of(params, "sessionId") != self.session_id.as_deref() {
            step.warn("an update for another session was ignored");
            return;
        }
        let Some(u) = params.get("update") else { return };
        match str_of(u, "sessionUpdate") {
            Some("agent_message_chunk") => {
                let Some(text) = u.get("content").and_then(content_text) else { return };
                if let Some(turn) = &mut self.turn {
                    if turn.message.is_empty() {
                        step.live(LiveKind::MessageStart, "");
                    }
                    turn.message.push_str(&text);
                    turn.all_text.push_str(&text);
                }
                step.live(LiveKind::Text, text);
            }
            Some("agent_thought_chunk") => {
                if let Some(text) = u.get("content").and_then(content_text) {
                    step.live(LiveKind::Thinking, text);
                }
            }
            Some("tool_call") => {
                self.flush_message(step);
                let id = str_of(u, "toolCallId").unwrap_or_default().to_owned();
                let name = str_of(u, "kind").unwrap_or("other").to_owned();
                let input = json!({
                    "title": str_of(u, "title"),
                    "kind": name,
                    "locations": u.get("locations").cloned().unwrap_or(Value::Null),
                });
                let (input, input_truncated) = crate::event::bound_json(&input, 8 * 1024);
                self.tools.insert(id.clone(), Tool { name: name.clone(), finished: false });
                step.event(AgentEvent::ToolStarted {
                    tool_use_id: id,
                    tool_name: name,
                    input,
                    input_truncated,
                });
            }
            Some("tool_call_update") => {
                let id = str_of(u, "toolCallId").unwrap_or_default().to_owned();
                let outcome = match str_of(u, "status") {
                    Some("completed") => ToolOutcome::Succeeded,
                    Some("failed") => ToolOutcome::Failed,
                    _ => return,
                };
                let Some(tool) = self.tools.get_mut(&id) else { return };
                if tool.finished {
                    return;
                }
                tool.finished = true;
                let output: String = u
                    .get("content")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(content_text).collect::<Vec<_>>().join("\n"))
                    .unwrap_or_default();
                let (output, output_truncated) = truncate(&output, MAX_TOOL_OUTPUT);
                step.event(AgentEvent::ToolFinished {
                    tool_use_id: id,
                    tool_name: Some(tool.name.clone()),
                    outcome,
                    output,
                    output_truncated,
                });
            }
            Some("current_mode_update") => {
                let mode = str_of(u, "currentModeId").unwrap_or_default();
                if mode != ASK_MODE {
                    self.fail(
                        step,
                        format!("the agent switched to mode {mode:?}, which acts without asking"),
                    );
                }
            }
            // Usage, available commands, plans and the user's replayed messages carry no activity.
            _ => {}
        }
    }

    fn agent_request(&mut self, id: Value, method: &str, msg: &Value, step: &mut Step) {
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        if method != "session/request_permission" {
            // No file system or terminal was offered: refuse anything else.
            step.writes.push(json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": -32601, "message": format!("{method} is not supported by Switchyard") },
            }));
            step.warn(format!("the agent asked for {method}, which Switchyard does not offer"));
            return;
        }
        let key = self.request_key(&id);
        let tool = params.get("toolCall").cloned().unwrap_or(Value::Null);
        let options = params.get("options").and_then(Value::as_array).cloned().unwrap_or_default();
        let option = |kind: &str| {
            options
                .iter()
                .find(|o| str_of(o, "kind") == Some(kind))
                .and_then(|o| str_of(o, "optionId"))
                .map(str::to_owned)
        };
        let pending =
            Pending { id: id.clone(), allow_once: option("allow_once"), reject_once: option("reject_once") };
        self.pending.insert(key.clone(), pending);
        if self.turn.is_none() || str_of(&params, "sessionId") != self.session_id.as_deref() {
            // Not for this session's running turn: reject at once.
            if let Ok(answer) = self.decide(&key, &PermissionDecision::Deny { message: String::new() }) {
                step.writes.push(answer);
            }
            step.warn("a permission request outside the running turn was rejected");
            return;
        }
        let kind = str_of(&tool, "kind").unwrap_or("other").to_owned();
        let content: Vec<Value> = tool
            .get("content")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter(|c| str_of(c, "type") == Some("diff"))
                    // A new file has no old text: `null`, or "" from Gemini's write_file (verified 0.70.0).
                    .map(|c| {
                        let new_file = c.get("oldText").is_none_or(|o| o.is_null() || o.as_str() == Some(""));
                        json!({ "path": str_of(c, "path"), "new_file": new_file })
                    })
                    .collect()
            })
            .unwrap_or_default();
        // Gemini names its tool in the call id (`write_file__call_…`, `replace__call_…`).
        let native_tool =
            str_of(&tool, "toolCallId").and_then(|id| id.split_once("__")).map(|(t, _)| t.to_owned());
        let input = json!({
            "title": str_of(&tool, "title"),
            "kind": kind,
            "tool": native_tool,
            "locations": tool.get("locations").cloned().unwrap_or(Value::Null),
            "diffs": content,
            "can_allow_once": self.pending.get(&key).is_some_and(|p| p.allow_once.is_some()),
        });
        step.outputs.push(AdapterOutput::PermissionRequested(PermissionRequest {
            native_request_id: key,
            tool_name: kind,
            tool_use_id: str_of(&tool, "toolCallId").map(str::to_owned),
            input,
            description: str_of(&tool, "title").map(str::to_owned),
            details: json!({ "options": options }),
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/gemini-0.70.0");

    struct Replay {
        outputs: Vec<AdapterOutput>,
        writes: Vec<Value>,
        fatal: Option<String>,
    }

    /// Replays a recorded session: sends one prompt once the session is
    /// ready, answers every permission request with `decision`, and
    /// optionally interrupts after the first streamed text.
    fn replay(name: &str, mode: SessionMode, decision: PermissionDecision, interrupt: bool) -> Replay {
        let text = std::fs::read_to_string(format!("{FIXTURES}/{name}.stdout.jsonl")).unwrap();
        let mut p = Protocol::new(mode, "/workspace/demo", Some("gemini-api-key".into()), "test");
        let mut r = Replay { outputs: Vec::new(), writes: p.start(), fatal: None };
        let mut prompted = false;
        let mut interrupted = false;
        for line in text.lines() {
            let step = p.ingest(line.as_bytes());
            r.writes.extend(step.writes);
            r.fatal = r.fatal.or(step.fatal);
            for o in step.outputs {
                if let AdapterOutput::PermissionRequested(req) = &o {
                    r.writes.push(p.decide(&req.native_request_id, &decision).unwrap());
                }
                if interrupt
                    && !interrupted
                    && matches!(
                        &o,
                        AdapterOutput::Live(LiveChunk { kind: LiveKind::Text | LiveKind::Thinking, .. })
                    )
                {
                    interrupted = true;
                    r.writes.extend(p.interrupt().unwrap().writes);
                }
                r.outputs.push(o);
            }
            if !prompted && p.session_id().is_some() {
                prompted = true;
                r.writes.extend(p.send_user_message("go".into()).writes);
            }
        }
        r
    }

    fn events(r: &Replay) -> Vec<&AgentEvent> {
        r.outputs
            .iter()
            .filter_map(|o| if let AdapterOutput::Event(e) = o { Some(e) } else { None })
            .collect()
    }

    fn method_of(v: &Value) -> Option<&str> {
        str_of(v, "method")
    }

    #[test]
    fn handshake_logs_in_opens_a_session_and_offers_no_file_or_terminal_access() {
        let r = replay("ask_all", SessionMode::New, PermissionDecision::Allow, false);
        assert_eq!(r.fatal, None);
        let methods: Vec<_> = r.writes.iter().filter_map(method_of).collect();
        assert_eq!(methods, ["initialize", "authenticate", "session/new", "session/prompt"]);
        assert_eq!(
            r.writes[0]["params"]["clientCapabilities"],
            json!({ "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false })
        );
        assert_eq!(r.writes[1]["params"], json!({ "methodId": "gemini-api-key" }));
        assert!(matches!(events(&r)[0], AgentEvent::Ready { model: Some(_), .. }));
    }

    #[test]
    fn a_permission_is_answered_with_allow_once_and_the_tool_runs() {
        let r = replay("ask_all", SessionMode::New, PermissionDecision::Allow, false);
        let req = r
            .outputs
            .iter()
            .find_map(|o| if let AdapterOutput::PermissionRequested(q) = o { Some(q) } else { None })
            .expect("a permission request");
        assert_eq!((req.tool_name.as_str(), req.input["title"].as_str()), ("execute", Some("ls -la")));
        assert_eq!(req.input["can_allow_once"], json!(true));
        let answer = r.writes.iter().find(|w| w.get("result").is_some()).unwrap();
        assert_eq!(answer["result"]["outcome"]["outcome"], "selected");
        assert_eq!(answer["result"]["outcome"]["optionId"], "proceed_once", "never an 'always' option");
        assert!(
            events(&r)
                .iter()
                .any(|e| matches!(e, AgentEvent::ToolFinished { outcome: ToolOutcome::Succeeded, .. }))
        );
        assert!(matches!(
            events(&r).last(),
            Some(AgentEvent::TurnFinished { success: true, usage: Some(u), .. }) if u.input_tokens > 0
        ));
    }

    #[test]
    fn rejected_requests_fail_their_tool_calls() {
        let r = replay("reject", SessionMode::New, PermissionDecision::Deny { message: "no".into() }, false);
        let answers: Vec<_> = r.writes.iter().filter(|w| w.get("result").is_some()).collect();
        assert_eq!(answers.len(), 2);
        assert!(answers.iter().all(|a| a["result"]["outcome"]["optionId"] == "cancel"));
        let failed = events(&r)
            .iter()
            .filter(|e| matches!(e, AgentEvent::ToolFinished { outcome: ToolOutcome::Failed, .. }))
            .count();
        assert_eq!(failed, 2);
    }

    #[test]
    fn an_interrupt_cancels_the_turn() {
        let r = replay("cancel", SessionMode::New, PermissionDecision::Allow, true);
        assert!(r.writes.iter().any(|w| method_of(w) == Some("session/cancel")));
        let ev = events(&r);
        assert!(ev.iter().any(|e| matches!(e, AgentEvent::InterruptAcknowledged)));
        assert!(matches!(
            ev.last(),
            Some(AgentEvent::TurnFinished { interrupted: true, success: false, .. })
        ));
    }

    #[test]
    fn a_loaded_session_ignores_its_replayed_history() {
        let text = std::fs::read_to_string(format!("{FIXTURES}/load.stdout.jsonl")).unwrap();
        let id = text
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .find_map(|v| v["params"]["sessionId"].as_str().map(str::to_owned))
            .unwrap();
        let r = replay("load", SessionMode::Load(id.clone()), PermissionDecision::Allow, false);
        assert_eq!(r.fatal, None);
        assert!(
            r.writes
                .iter()
                .any(|w| method_of(w) == Some("session/load") && w["params"]["sessionId"] == id.as_str())
        );
        // Only this turn's activity: no replayed tool calls from the earlier process.
        assert!(!events(&r).iter().any(|e| matches!(e, AgentEvent::ToolStarted { .. })));
        assert!(matches!(events(&r).last(), Some(AgentEvent::TurnFinished { success: true, .. })));
    }

    #[test]
    fn modes_that_act_without_asking_are_refused() {
        let mut p = Protocol::new(SessionMode::New, "/w", None, "t");
        p.start();
        p.ingest(br#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}"#);
        let s = p.ingest(
            br#"{"jsonrpc":"2.0","id":2,"result":{"sessionId":"s","modes":{"currentModeId":"yolo"}}}"#,
        );
        assert!(s.fatal.as_deref().unwrap().contains("yolo"));
        let mut p = Protocol::new(SessionMode::New, "/w", None, "t");
        p.start();
        p.ingest(br#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}"#);
        p.ingest(
            br#"{"jsonrpc":"2.0","id":2,"result":{"sessionId":"s","modes":{"currentModeId":"default"}}}"#,
        );
        let s = p.ingest(br#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"current_mode_update","currentModeId":"autoEdit"}}}"#);
        assert!(s.fatal.is_some());
        // Anything but a permission request is refused (no fs, no terminal).
        let s = p.ingest(br#"{"jsonrpc":"2.0","id":9,"method":"fs/write_text_file","params":{"sessionId":"s","path":"/x","content":"y"}}"#);
        assert_eq!(s.writes[0]["error"]["code"], -32601);
    }

    #[test]
    fn interrupting_answers_pending_permissions_as_cancelled() {
        let mut p = Protocol::new(SessionMode::New, "/w", None, "t");
        p.start();
        p.ingest(br#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}"#);
        p.ingest(
            br#"{"jsonrpc":"2.0","id":2,"result":{"sessionId":"s","modes":{"currentModeId":"default"}}}"#,
        );
        p.send_user_message("go".into());
        let s = p.ingest(br#"{"jsonrpc":"2.0","id":0,"method":"session/request_permission","params":{"sessionId":"s","toolCall":{"toolCallId":"t1","title":"touch x","kind":"execute"},"options":[{"optionId":"proceed_once","kind":"allow_once"},{"optionId":"cancel","kind":"reject_once"}]}}"#);
        let AdapterOutput::PermissionRequested(req) = &s.outputs[0] else { panic!() };
        let key = req.native_request_id.clone();
        let i = p.interrupt().unwrap();
        assert_eq!(method_of(&i.writes[0]), Some("session/cancel"));
        assert_eq!(i.writes[1]["result"]["outcome"]["outcome"], "cancelled");
        assert_eq!(i.outputs, vec![AdapterOutput::PermissionCancelled { native_request_id: key.clone() }]);
        assert!(p.decide(&key, &PermissionDecision::Allow).is_err(), "answered once");
    }
}
