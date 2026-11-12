//! The Codex app-server JSON-RPC protocol, as a pure state machine.
//!
//! [`Protocol`] consumes stdout lines and produces vendor-neutral
//! [`AdapterOutput`]s plus the JSON messages to write to stdin. It never does
//! I/O, so every sequence can be tested without a process. Raw Codex JSON
//! never leaves this module (and `classify`, which reads the normalized
//! request built here).
//!
//! Verified against codex-cli 0.166.0; see `docs/architecture/codex-integration.md`.

use std::collections::{HashMap, VecDeque};

use serde_json::{Map, Value, json};

use crate::event::{
    AdapterOutput, AgentEvent, LiveChunk, LiveKind, MAX_DURABLE_INPUT, MAX_DURABLE_TEXT, PermissionRequest,
    TokenUsage, ToolOutcome, bound_json, truncate,
};
use crate::model::PermissionDecision;

/// Approval policy sent with every thread (ADR 0021): ask before every
/// command and every patch.
pub const APPROVAL_POLICY: &str = "untrusted";
/// Codex sandbox for commands that run inside it (ADR 0021).
pub const SANDBOX: &str = "workspace-write";
/// Name reported in `initialize.clientInfo`.
pub const CLIENT_NAME: &str = "switchyard";

/// Server requests whose decision is a `{decision}` object.
const COMMAND_APPROVAL: &str = "item/commandExecution/requestApproval";
const FILE_CHANGE_APPROVAL: &str = "item/fileChange/requestApproval";
const PERMISSIONS_APPROVAL: &str = "item/permissions/requestApproval";
const MCP_ELICITATION: &str = "mcpServer/elicitation/request";

/// Command-approval fields this adapter understands. Anything else makes the
/// request ineligible for automatic approval.
const KNOWN_COMMAND_FIELDS: &[&str] = &[
    "kind",
    "threadId",
    "turnId",
    "itemId",
    "startedAtMs",
    "approvalId",
    "environmentId",
    "reason",
    "networkApprovalContext",
    "command",
    "cwd",
    "commandActions",
    "proposedExecpolicyAmendment",
    "proposedNetworkPolicyAmendments",
    "availableDecisions",
];
const KNOWN_FILE_CHANGE_FIELDS: &[&str] =
    &["threadId", "turnId", "itemId", "startedAtMs", "reason", "grantRoot"];

/// Most file-change items remembered while waiting for their approval.
const MAX_REMEMBERED_ITEMS: usize = 256;

/// Item types that are not user-facing activity.
const SILENT_ITEMS: &[&str] = &["userMessage", "reasoning", "plan", "contextCompaction"];

/// How the thread is obtained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadMode {
    New,
    Resume(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Initializing,
    OpeningThread,
    Ready,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OwnRequest {
    Initialize,
    Thread,
    TurnStart,
    TurnInterrupt,
}

/// A server request awaiting our answer.
#[derive(Debug, Clone)]
struct ServerRequest {
    /// The JSON-RPC id exactly as received (echoed in the response).
    id: Value,
    method: String,
}

#[derive(Debug, Default)]
struct Turn {
    id: Option<String>,
    usage: Option<TokenUsage>,
    last_message: Option<String>,
}

/// Output of one protocol step.
#[derive(Debug, Default, PartialEq)]
pub struct Step {
    pub outputs: Vec<AdapterOutput>,
    /// Messages to write to stdin, in order.
    pub writes: Vec<Value>,
    /// The session cannot continue (e.g. the thread could not be resumed).
    /// The driver stops the process and reports this as the failure.
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
    mode: ThreadMode,
    cwd: String,
    model: Option<String>,
    client_version: String,
    next_id: u64,
    own: HashMap<u64, OwnRequest>,
    thread_id: Option<String>,
    /// The running turn (`turn/start` sent and not yet completed).
    turn: Option<Turn>,
    /// User messages waiting for the thread or for the running turn to end.
    queue: VecDeque<String>,
    /// An interrupt was requested before the turn id was known.
    interrupt_pending: bool,
    /// fileChange items by id (their paths are not repeated in the approval).
    file_items: HashMap<String, Value>,
    file_item_order: VecDeque<String>,
    /// Unanswered server requests by native request id.
    server_requests: HashMap<String, ServerRequest>,
}

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// The native request id Switchyard stores for a JSON-RPC id.
pub fn request_key(id: &Value) -> String {
    match id {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Codex runs commands as `<shell> -lc '<script>'`. Returns the script when
/// the command is exactly that form, so policy rules match what the user
/// asked for; otherwise `None` (the full command is used).
pub fn unwrap_shell(command: &str) -> Option<String> {
    const SHELLS: &[&str] = &["/bin/zsh", "/bin/bash", "/bin/sh", "/usr/bin/zsh", "/usr/bin/bash"];
    let rest = SHELLS.iter().find_map(|sh| {
        let r = command.strip_prefix(sh)?;
        r.strip_prefix(" -lc ").or_else(|| r.strip_prefix(" -c "))
    })?;
    parse_single_word(rest)
}

/// Parses exactly one POSIX shell word made of bare safe characters,
/// single-quoted segments and `\'`. Anything else (spaces outside quotes,
/// double quotes, `$`, globbing) is rejected.
fn parse_single_word(word: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = word.chars().peekable();
    if word.is_empty() {
        return None;
    }
    while let Some(c) = chars.next() {
        match c {
            '\'' => loop {
                match chars.next()? {
                    '\'' => break,
                    ch => out.push(ch),
                }
            },
            '\\' => {
                if chars.next()? != '\'' {
                    return None;
                }
                out.push('\'');
            }
            c if c.is_ascii_alphanumeric() || "_-./,:=+@%".contains(c) => out.push(c),
            _ => return None,
        }
    }
    (!out.is_empty()).then_some(out)
}

fn usage_of(breakdown: &Value) -> TokenUsage {
    let n = |k: &str| breakdown.get(k).and_then(Value::as_u64).unwrap_or(0);
    TokenUsage {
        input_tokens: n("inputTokens"),
        cached_input_tokens: n("cachedInputTokens"),
        output_tokens: n("outputTokens"),
        reasoning_output_tokens: n("reasoningOutputTokens"),
        total_tokens: n("totalTokens"),
    }
}

fn add_usage(a: TokenUsage, b: TokenUsage) -> TokenUsage {
    TokenUsage {
        input_tokens: a.input_tokens + b.input_tokens,
        cached_input_tokens: a.cached_input_tokens + b.cached_input_tokens,
        output_tokens: a.output_tokens + b.output_tokens,
        reasoning_output_tokens: a.reasoning_output_tokens + b.reasoning_output_tokens,
        total_tokens: a.total_tokens + b.total_tokens,
    }
}

fn change_kind(change: &Value) -> (String, Option<String>) {
    match change.get("kind") {
        Some(Value::String(s)) => (s.clone(), None),
        Some(Value::Object(o)) => (
            o.get("type").and_then(Value::as_str).unwrap_or("unknown").to_owned(),
            o.get("move_path").or_else(|| o.get("movePath")).and_then(Value::as_str).map(str::to_owned),
        ),
        _ => ("unknown".to_owned(), None),
    }
}

/// Paths and kinds of a fileChange item, for events and classification.
fn summarize_changes(item: &Value, with_diff: bool) -> Value {
    let changes = item.get("changes").and_then(Value::as_array).cloned().unwrap_or_default();
    Value::Array(
        changes
            .iter()
            .map(|c| {
                let (kind, move_path) = change_kind(c);
                let mut o = Map::new();
                o.insert("path".into(), c.get("path").cloned().unwrap_or(Value::Null));
                o.insert("kind".into(), Value::String(kind));
                if let Some(m) = move_path {
                    o.insert("move_path".into(), Value::String(m));
                }
                if with_diff && let Some(d) = c.get("diff").and_then(Value::as_str) {
                    o.insert("diff".into(), Value::String(truncate(d, 4096).0));
                }
                Value::Object(o)
            })
            .collect(),
    )
}

impl Protocol {
    pub fn new(
        mode: ThreadMode,
        cwd: impl Into<String>,
        model: Option<String>,
        client_version: &str,
    ) -> Self {
        Self {
            phase: Phase::Initializing,
            mode,
            cwd: cwd.into(),
            model,
            client_version: client_version.to_owned(),
            next_id: 1,
            own: HashMap::new(),
            thread_id: None,
            turn: None,
            queue: VecDeque::new(),
            interrupt_pending: false,
            file_items: HashMap::new(),
            file_item_order: VecDeque::new(),
            server_requests: HashMap::new(),
        }
    }

    pub fn thread_id(&self) -> Option<&str> {
        self.thread_id.as_deref()
    }

    pub fn turn_running(&self) -> bool {
        self.turn.is_some()
    }

    fn request(&mut self, kind: OwnRequest, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.own.insert(id, kind);
        json!({ "id": id, "method": method, "params": params })
    }

    /// The opening `initialize` request.
    pub fn start(&mut self) -> Vec<Value> {
        let params = json!({
            "clientInfo": { "name": CLIENT_NAME, "title": "Switchyard", "version": self.client_version },
        });
        vec![self.request(OwnRequest::Initialize, "initialize", params)]
    }

    fn thread_params(&self) -> Value {
        let mut p = json!({ "cwd": self.cwd, "approvalPolicy": APPROVAL_POLICY, "sandbox": SANDBOX });
        if let Some(m) = &self.model {
            p["model"] = json!(m);
        }
        if let ThreadMode::Resume(id) = &self.mode {
            p["threadId"] = json!(id);
        }
        p
    }

    fn start_turn(&mut self, text: String, step: &mut Step) {
        let Some(thread) = self.thread_id.clone() else {
            self.queue.push_front(text);
            return;
        };
        self.turn = Some(Turn::default());
        let msg = self.request(
            OwnRequest::TurnStart,
            "turn/start",
            json!({ "threadId": thread, "input": [{ "type": "text", "text": text }] }),
        );
        step.writes.push(msg);
    }

    /// A user message: starts a turn now, or queues it until the thread is
    /// ready or the running turn ends.
    pub fn send_user_message(&mut self, text: String) -> Step {
        let mut step = Step::default();
        if self.phase == Phase::Failed {
            step.fatal = Some("the Codex thread is not available".into());
            return step;
        }
        if self.phase == Phase::Ready && self.turn.is_none() && self.queue.is_empty() {
            self.start_turn(text, &mut step);
        } else {
            self.queue.push_back(text);
        }
        step
    }

    /// Ends the running turn. Before the turn id is known, the interrupt is
    /// sent as soon as `turn/start` answers.
    pub fn interrupt(&mut self) -> Result<Step, String> {
        let mut step = Step::default();
        let Some(turn) = &self.turn else { return Err("no turn is running".into()) };
        match (&turn.id, &self.thread_id) {
            (Some(turn_id), Some(thread)) => {
                let params = json!({ "threadId": thread, "turnId": turn_id });
                let msg = self.request(OwnRequest::TurnInterrupt, "turn/interrupt", params);
                step.writes.push(msg);
            }
            _ => self.interrupt_pending = true,
        }
        Ok(step)
    }

    /// Answers a server request. Allow is `accept` for exactly this request;
    /// deny is `decline` (or the method's negative answer).
    pub fn decide(
        &mut self,
        native_request_id: &str,
        decision: &PermissionDecision,
    ) -> Result<Value, String> {
        let req = self
            .server_requests
            .remove(native_request_id)
            .ok_or_else(|| format!("no pending request {native_request_id}"))?;
        let allow = matches!(decision, PermissionDecision::Allow);
        let result = match req.method.as_str() {
            COMMAND_APPROVAL | FILE_CHANGE_APPROVAL => {
                json!({ "decision": if allow { "accept" } else { "decline" } })
            }
            // Never granted: these are classified `unknown` and denied.
            PERMISSIONS_APPROVAL => json!({ "permissions": {}, "scope": "turn" }),
            MCP_ELICITATION => json!({ "action": "decline", "content": null }),
            _ => {
                return Ok(json!({
                    "id": req.id,
                    "error": { "code": -32601, "message": "not supported by Switchyard" },
                }));
            }
        };
        Ok(json!({ "id": req.id, "result": result }))
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
            (Some(id), Some(method)) => self.server_request(id.clone(), &method, &msg, &mut step),
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
            OwnRequest::Initialize => {
                if let Some(e) = error {
                    return self.fail(step, format!("Codex app-server refused initialization: {e}"));
                }
                step.writes.push(json!({ "method": "initialized", "params": {} }));
                let (method, params) = match &self.mode {
                    ThreadMode::New => ("thread/start", self.thread_params()),
                    ThreadMode::Resume(_) => ("thread/resume", self.thread_params()),
                };
                self.phase = Phase::OpeningThread;
                let m = self.request(OwnRequest::Thread, method, params);
                step.writes.push(m);
            }
            OwnRequest::Thread => {
                if let Some(e) = error {
                    let what = match &self.mode {
                        ThreadMode::New => "cannot start a Codex thread".to_owned(),
                        ThreadMode::Resume(id) => format!("cannot resume Codex thread {id}"),
                    };
                    return self.fail(step, format!("{what}: {e}"));
                }
                let Some(thread_id) = result.get("thread").and_then(|t| str_of(t, "id")) else {
                    return self.fail(step, "Codex did not report a thread id".into());
                };
                if let ThreadMode::Resume(expected) = &self.mode
                    && expected != thread_id
                {
                    return self.fail(
                        step,
                        format!(
                            "Codex resumed thread {thread_id} instead of {expected}; refusing to continue"
                        ),
                    );
                }
                if let Some(cwd) = str_of(result, "cwd")
                    && cwd != self.cwd
                {
                    return self.fail(
                        step,
                        format!("Codex reported working directory {cwd}, expected {}", self.cwd),
                    );
                }
                self.thread_id = Some(thread_id.to_owned());
                self.phase = Phase::Ready;
                step.event(AgentEvent::Ready {
                    native_session_id: thread_id.to_owned(),
                    model: str_of(result, "model").map(str::to_owned),
                    cwd: str_of(result, "cwd").map(str::to_owned),
                    tools: vec!["shell".into(), "apply_patch".into()],
                });
                if let Some(text) = self.queue.pop_front() {
                    self.start_turn(text, step);
                }
            }
            OwnRequest::TurnStart => {
                if let Some(e) = error {
                    self.turn = None;
                    step.event(AgentEvent::TurnFinished {
                        success: false,
                        interrupted: false,
                        stop_reason: None,
                        reason: Some(format!("turn not started: {e}")),
                        duration_ms: None,
                        cost_usd: None,
                        turn_cost_usd: None,
                        usage: None,
                        num_turns: None,
                        result_text: None,
                    });
                    self.next_queued(step);
                    return;
                }
                let turn_id = result.get("turn").and_then(|t| str_of(t, "id")).map(str::to_owned);
                if let Some(turn) = &mut self.turn
                    && turn.id.is_none()
                {
                    turn.id = turn_id;
                }
                if std::mem::take(&mut self.interrupt_pending)
                    && let Ok(s) = self.interrupt()
                {
                    step.writes.extend(s.writes);
                }
            }
            OwnRequest::TurnInterrupt => match error {
                None => step.event(AgentEvent::InterruptAcknowledged),
                Some(e) => step.warn(format!("interrupt was not accepted: {e}")),
            },
        }
    }

    fn next_queued(&mut self, step: &mut Step) {
        if self.turn.is_none()
            && self.phase == Phase::Ready
            && let Some(text) = self.queue.pop_front()
        {
            self.start_turn(text, step);
        }
    }

    fn own_thread(&self, params: &Value) -> bool {
        match (str_of(params, "threadId"), &self.thread_id) {
            (Some(t), Some(mine)) => t == mine,
            _ => false,
        }
    }

    fn remember_file_item(&mut self, id: &str, item: &Value) {
        if self.file_items.insert(id.to_owned(), item.clone()).is_none() {
            self.file_item_order.push_back(id.to_owned());
        }
        while self.file_item_order.len() > MAX_REMEMBERED_ITEMS {
            if let Some(old) = self.file_item_order.pop_front() {
                self.file_items.remove(&old);
            }
        }
    }

    fn notification(&mut self, method: &str, params: &Value, step: &mut Step) {
        match method {
            "warning" | "configWarning" => {
                let text = str_of(params, "message").or_else(|| str_of(params, "summary")).unwrap_or(method);
                step.warn(format!("Codex: {}", truncate(text, 1000).0));
                return;
            }
            "serverRequest/resolved" => {
                if let Some(id) = params.get("requestId") {
                    let key = request_key(id);
                    if self.server_requests.remove(&key).is_some() {
                        step.outputs.push(AdapterOutput::PermissionCancelled { native_request_id: key });
                    }
                }
                return;
            }
            _ => {}
        }
        if !self.own_thread(params) {
            // Sub-agent threads, other threads, and thread-less notifications
            // (account, MCP status, rate limits) are not session activity.
            if method == "error" && str_of(params, "threadId").is_none() {
                self.error_notification(params, step);
            }
            return;
        }
        match method {
            "item/agentMessage/delta" => {
                if let Some(d) = str_of(params, "delta") {
                    step.live(LiveKind::Text, d);
                }
            }
            "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
                if let Some(d) = str_of(params, "delta") {
                    step.live(LiveKind::Thinking, d);
                }
            }
            "item/started" => self.item_started(params.get("item").unwrap_or(&Value::Null), step),
            "item/completed" => self.item_completed(params.get("item").unwrap_or(&Value::Null), step),
            "turn/started" => {
                let id = params.get("turn").and_then(|t| str_of(t, "id")).map(str::to_owned);
                if let Some(turn) = &mut self.turn
                    && turn.id.is_none()
                {
                    turn.id = id;
                }
            }
            "thread/tokenUsage/updated" => {
                let for_turn = str_of(params, "turnId");
                if let Some(turn) = &mut self.turn
                    && turn.id.as_deref().is_some_and(|t| Some(t) == for_turn)
                    && let Some(last) = params.get("tokenUsage").and_then(|u| u.get("last"))
                {
                    let u = usage_of(last);
                    turn.usage = Some(turn.usage.map_or(u, |prev| add_usage(prev, u)));
                }
            }
            "turn/completed" => self.turn_completed(params.get("turn").unwrap_or(&Value::Null), step),
            "error" => self.error_notification(params, step),
            _ => {}
        }
    }

    fn error_notification(&mut self, params: &Value, step: &mut Step) {
        if params.get("willRetry").and_then(Value::as_bool) == Some(true) {
            return;
        }
        let message = params.get("error").and_then(|e| str_of(e, "message")).unwrap_or("unknown error");
        step.warn(format!("Codex error: {}", truncate(message, 1000).0));
    }

    fn item_started(&mut self, item: &Value, step: &mut Step) {
        let (Some(kind), Some(id)) = (str_of(item, "type"), str_of(item, "id")) else { return };
        match kind {
            "agentMessage" => step.live(LiveKind::MessageStart, ""),
            "commandExecution" => {
                let raw = str_of(item, "command").unwrap_or_default();
                let command = unwrap_shell(raw).unwrap_or_else(|| raw.to_owned());
                let input = json!({ "command": command, "cwd": item.get("cwd") });
                let (input, input_truncated) = bound_json(&input, MAX_DURABLE_INPUT);
                step.event(AgentEvent::ToolStarted {
                    tool_use_id: id.to_owned(),
                    tool_name: "shell".into(),
                    input,
                    input_truncated,
                });
            }
            "fileChange" => {
                self.remember_file_item(id, item);
                let (input, input_truncated) =
                    bound_json(&json!({ "changes": summarize_changes(item, false) }), MAX_DURABLE_INPUT);
                step.event(AgentEvent::ToolStarted {
                    tool_use_id: id.to_owned(),
                    tool_name: "apply_patch".into(),
                    input,
                    input_truncated,
                });
            }
            k if SILENT_ITEMS.contains(&k) => {}
            other => {
                let (input, input_truncated) = bound_json(item, MAX_DURABLE_INPUT);
                step.event(AgentEvent::ToolStarted {
                    tool_use_id: id.to_owned(),
                    tool_name: other.to_owned(),
                    input,
                    input_truncated,
                });
                step.warn(format!(
                    "Codex started a {other} item, which does not pass through Switchyard approval"
                ));
            }
        }
    }

    fn item_completed(&mut self, item: &Value, step: &mut Step) {
        let (Some(kind), Some(id)) = (str_of(item, "type"), str_of(item, "id")) else { return };
        let outcome = match str_of(item, "status") {
            Some("completed") => ToolOutcome::Succeeded,
            Some("declined") => ToolOutcome::Rejected,
            _ => ToolOutcome::Failed,
        };
        match kind {
            "agentMessage" => {
                let text = str_of(item, "text").unwrap_or_default();
                let (bounded, truncated) = truncate(text, MAX_DURABLE_TEXT);
                if let Some(turn) = &mut self.turn {
                    turn.last_message = Some(bounded.clone());
                }
                step.live(LiveKind::MessageStop, "");
                if !text.trim().is_empty() {
                    step.event(AgentEvent::Message {
                        message_id: Some(id.to_owned()),
                        text: bounded,
                        truncated,
                    });
                }
            }
            "commandExecution" => {
                let mut output = str_of(item, "aggregatedOutput").unwrap_or_default().to_owned();
                if let Some(code) = item.get("exitCode").and_then(Value::as_i64) {
                    output = format!("exit code {code}\n{output}");
                }
                let (output, output_truncated) = truncate(&output, MAX_DURABLE_TEXT);
                step.event(AgentEvent::ToolFinished {
                    tool_use_id: id.to_owned(),
                    tool_name: Some("shell".into()),
                    outcome,
                    output,
                    output_truncated,
                });
            }
            "fileChange" => {
                self.file_items.remove(id);
                let paths: Vec<String> = summarize_changes(item, false)
                    .as_array()
                    .map(|a| a.iter().filter_map(|c| str_of(c, "path").map(str::to_owned)).collect())
                    .unwrap_or_default();
                step.event(AgentEvent::ToolFinished {
                    tool_use_id: id.to_owned(),
                    tool_name: Some("apply_patch".into()),
                    outcome,
                    output: truncate(&paths.join("\n"), MAX_DURABLE_TEXT).0,
                    output_truncated: false,
                });
            }
            k if SILENT_ITEMS.contains(&k) => {}
            other => {
                step.event(AgentEvent::ToolFinished {
                    tool_use_id: id.to_owned(),
                    tool_name: Some(other.to_owned()),
                    outcome,
                    output: String::new(),
                    output_truncated: false,
                });
            }
        }
    }

    fn turn_completed(&mut self, turn: &Value, step: &mut Step) {
        let state = self.turn.take().unwrap_or_default();
        self.interrupt_pending = false;
        let status = str_of(turn, "status").unwrap_or("failed");
        let error = turn.get("error").and_then(|e| str_of(e, "message")).map(str::to_owned);
        step.event(AgentEvent::TurnFinished {
            success: status == "completed",
            interrupted: status == "interrupted",
            stop_reason: None,
            reason: Some(match (status, error) {
                ("failed", Some(e)) => format!("failed: {}", truncate(&e, 1000).0),
                (s, _) => s.to_owned(),
            }),
            duration_ms: turn.get("durationMs").and_then(Value::as_u64),
            cost_usd: None,
            turn_cost_usd: None,
            usage: state.usage,
            num_turns: None,
            result_text: state.last_message,
        });
        self.next_queued(step);
    }

    fn server_request(&mut self, id: Value, method: &str, msg: &Value, step: &mut Step) {
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let key = request_key(&id);
        self.server_requests.insert(key.clone(), ServerRequest { id, method: method.to_owned() });
        let other_thread = str_of(&params, "threadId").is_some() && !self.own_thread(&params);
        let unknown_fields = |known: &[&str]| -> Vec<String> {
            params
                .as_object()
                .map(|o| o.keys().filter(|k| !known.contains(&k.as_str())).cloned().collect())
                .unwrap_or_default()
        };
        let (tool_name, tool_use_id, input, description) = match method {
            COMMAND_APPROVAL => {
                let raw = str_of(&params, "command").unwrap_or_default();
                let cwd = str_of(&params, "cwd");
                let input = json!({
                    "kind": str_of(&params, "kind").unwrap_or("command"),
                    "command": unwrap_shell(raw).unwrap_or_else(|| raw.to_owned()),
                    "raw_command": raw,
                    "cwd": cwd,
                    "cwd_is_session_cwd": cwd.is_none_or(|c| c == self.cwd),
                    "network": params.get("networkApprovalContext").filter(|v| !v.is_null()),
                    "other_thread": other_thread,
                    "unexpected_fields": unknown_fields(KNOWN_COMMAND_FIELDS),
                });
                ("shell", str_of(&params, "itemId"), input, str_of(&params, "reason"))
            }
            FILE_CHANGE_APPROVAL => {
                let item_id = str_of(&params, "itemId").unwrap_or_default();
                let changes = self.file_items.get(item_id).map(|item| summarize_changes(item, true));
                let input = json!({
                    "changes": changes,
                    "grant_root": params.get("grantRoot").filter(|v| !v.is_null()),
                    "other_thread": other_thread,
                    "unexpected_fields": unknown_fields(KNOWN_FILE_CHANGE_FIELDS),
                });
                ("apply_patch", Some(item_id), input, str_of(&params, "reason"))
            }
            other => {
                let (preview, _) = bound_json(&params, 4096);
                (other, str_of(&params, "itemId"), json!({ "request": preview }), None)
            }
        };
        step.outputs.push(AdapterOutput::PermissionRequested(PermissionRequest {
            native_request_id: key,
            tool_name: tool_name.to_owned(),
            tool_use_id: tool_use_id.map(str::to_owned),
            input,
            description: description.map(str::to_owned),
            details: json!({ "method": method }),
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(v: Value) -> Vec<u8> {
        serde_json::to_vec(&v).unwrap()
    }

    /// A protocol past the handshake with thread `t1`.
    fn ready() -> Protocol {
        let mut p = Protocol::new(ThreadMode::New, "/ws", None, "0.1.0");
        let init = p.start();
        assert_eq!(init[0]["method"], "initialize");
        let s = p.ingest(&line(json!({"id": 1, "result": {"userAgent": "x"}})));
        assert_eq!(s.writes[0]["method"], "initialized");
        assert_eq!(s.writes[1]["method"], "thread/start");
        assert_eq!(s.writes[1]["params"]["approvalPolicy"], "untrusted");
        assert_eq!(s.writes[1]["params"]["sandbox"], "workspace-write");
        assert_eq!(s.writes[1]["params"]["cwd"], "/ws");
        let s =
            p.ingest(&line(json!({"id": 2, "result": {"thread": {"id": "t1"}, "model": "m", "cwd": "/ws"}})));
        assert!(
            matches!(&s.outputs[0], AdapterOutput::Event(AgentEvent::Ready { native_session_id, .. }) if native_session_id == "t1")
        );
        p
    }

    fn events(s: &Step) -> Vec<&AgentEvent> {
        s.outputs
            .iter()
            .filter_map(|o| if let AdapterOutput::Event(e) = o { Some(e) } else { None })
            .collect()
    }

    #[test]
    fn shell_unwrapping_is_exact() {
        assert_eq!(unwrap_shell("/bin/zsh -lc 'cat notes.txt'").as_deref(), Some("cat notes.txt"));
        assert_eq!(unwrap_shell("/bin/zsh -lc pwd").as_deref(), Some("pwd"));
        assert_eq!(unwrap_shell("/bin/zsh -lc 'echo '\\''hi'\\'''").as_deref(), Some("echo 'hi'"));
        assert_eq!(unwrap_shell("/bin/zsh -lc 'a' extra"), None, "a second word is not unwrapped");
        assert_eq!(unwrap_shell("/bin/zsh -lc \"x\""), None);
        assert_eq!(unwrap_shell("/bin/zsh -lc $(x)"), None);
        assert_eq!(unwrap_shell("/opt/evil -lc 'x'"), None);
        assert_eq!(unwrap_shell("/bin/zsh -lc 'unterminated"), None);
    }

    #[test]
    fn messages_wait_for_the_thread_and_for_the_running_turn() {
        let mut p = Protocol::new(ThreadMode::New, "/ws", None, "0");
        p.start();
        assert!(p.send_user_message("early".into()).writes.is_empty(), "queued until the thread exists");
        p.ingest(&line(json!({"id": 1, "result": {}})));
        let s = p.ingest(&line(json!({"id": 2, "result": {"thread": {"id": "t1"}, "cwd": "/ws"}})));
        let start =
            s.writes.iter().find(|w| w["method"] == "turn/start").expect("queued message starts a turn");
        assert_eq!(start["params"]["input"][0]["text"], "early");
        assert!(p.send_user_message("second".into()).writes.is_empty(), "a turn is running");
        p.ingest(&line(json!({"id": 3, "result": {"turn": {"id": "u1"}}})));
        let s = p.ingest(&line(json!({"method": "turn/completed", "params": {"threadId": "t1", "turn": {"id": "u1", "status": "completed", "durationMs": 5}}})));
        assert!(matches!(
            events(&s)[0],
            AgentEvent::TurnFinished { success: true, duration_ms: Some(5), .. }
        ));
        assert_eq!(
            s.writes[0]["params"]["input"][0]["text"], "second",
            "next queued turn starts after completion"
        );
    }

    #[test]
    fn resume_failure_is_fatal_and_never_starts_a_new_thread() {
        let mut p = Protocol::new(ThreadMode::Resume("old".into()), "/ws", None, "0");
        p.start();
        let s = p.ingest(&line(json!({"id": 1, "result": {}})));
        assert_eq!(s.writes[1]["method"], "thread/resume");
        assert_eq!(s.writes[1]["params"]["threadId"], "old");
        let s = p.ingest(&line(json!({"id": 2, "error": {"code": -32600, "message": "no rollout found"}})));
        assert!(s.fatal.as_deref().is_some_and(|f| f.contains("cannot resume Codex thread old")));
        assert!(s.writes.is_empty(), "no thread/start fallback");
        assert!(p.send_user_message("x".into()).fatal.is_some());
    }

    #[test]
    fn resume_into_a_different_thread_or_cwd_is_refused() {
        let mut p = Protocol::new(ThreadMode::Resume("old".into()), "/ws", None, "0");
        p.start();
        p.ingest(&line(json!({"id": 1, "result": {}})));
        let s = p.ingest(&line(json!({"id": 2, "result": {"thread": {"id": "other"}, "cwd": "/ws"}})));
        assert!(s.fatal.is_some());
        let mut p = Protocol::new(ThreadMode::New, "/ws", None, "0");
        p.start();
        p.ingest(&line(json!({"id": 1, "result": {}})));
        let s = p.ingest(&line(json!({"id": 2, "result": {"thread": {"id": "t"}, "cwd": "/elsewhere"}})));
        assert!(s.fatal.as_deref().is_some_and(|f| f.contains("/elsewhere")));
    }

    #[test]
    fn approvals_are_answered_once_with_accept_or_decline_only() {
        let mut p = ready();
        let s = p.ingest(&line(json!({"id": 7, "method": COMMAND_APPROVAL, "params": {
            "kind": "command", "threadId": "t1", "turnId": "u", "itemId": "i1",
            "command": "/bin/zsh -lc 'touch x'", "cwd": "/ws", "startedAtMs": 1}})));
        let AdapterOutput::PermissionRequested(r) = &s.outputs[0] else { panic!("{s:?}") };
        assert_eq!((r.native_request_id.as_str(), r.tool_name.as_str()), ("7", "shell"));
        assert_eq!(r.input["command"], "touch x");
        assert_eq!(r.input["cwd_is_session_cwd"], true);
        let answer = p.decide("7", &PermissionDecision::Allow).unwrap();
        assert_eq!(answer, json!({"id": 7, "result": {"decision": "accept"}}));
        assert!(p.decide("7", &PermissionDecision::Allow).is_err(), "a request is answered once");

        p.ingest(&line(json!({"id": "s", "method": COMMAND_APPROVAL, "params": {"kind": "command", "threadId": "t1", "command": "x"}})));
        let deny = p.decide("s", &PermissionDecision::Deny { message: "no".into() }).unwrap();
        assert_eq!(deny, json!({"id": "s", "result": {"decision": "decline"}}));
    }

    #[test]
    fn file_change_requests_carry_paths_from_their_item() {
        let mut p = ready();
        p.ingest(&line(json!({"method": "item/started", "params": {"threadId": "t1", "item": {
            "type": "fileChange", "id": "f1", "status": "inProgress",
            "changes": [{"path": "/ws/a.txt", "kind": {"type": "add"}, "diff": "+x"}]}}})));
        let s = p.ingest(&line(json!({"id": 3, "method": FILE_CHANGE_APPROVAL, "params": {"threadId": "t1", "turnId": "u", "itemId": "f1"}})));
        let AdapterOutput::PermissionRequested(r) = &s.outputs[0] else { panic!() };
        assert_eq!(r.input["changes"][0]["path"], "/ws/a.txt");
        assert_eq!(r.input["changes"][0]["kind"], "add");
        // Unknown item: no paths (the classifier denies it).
        let s = p.ingest(&line(
            json!({"id": 4, "method": FILE_CHANGE_APPROVAL, "params": {"threadId": "t1", "itemId": "nope"}}),
        ));
        let AdapterOutput::PermissionRequested(r) = &s.outputs[0] else { panic!() };
        assert!(r.input["changes"].is_null());
    }

    #[test]
    fn unsupported_server_requests_get_negative_answers() {
        let mut p = ready();
        for (id, method) in
            [(10, PERMISSIONS_APPROVAL), (11, MCP_ELICITATION), (12, "item/tool/requestUserInput")]
        {
            let s = p.ingest(&line(json!({"id": id, "method": method, "params": {"threadId": "t1"}})));
            assert!(matches!(&s.outputs[0], AdapterOutput::PermissionRequested(r) if r.tool_name == method));
        }
        let deny = PermissionDecision::Deny { message: String::new() };
        assert_eq!(p.decide("10", &deny).unwrap()["result"], json!({"permissions": {}, "scope": "turn"}));
        assert_eq!(p.decide("11", &deny).unwrap()["result"]["action"], "decline");
        assert_eq!(p.decide("12", &deny).unwrap()["error"]["code"], -32601);
        // Even a (mistaken) allow never grants these.
        p.ingest(&line(json!({"id": 13, "method": PERMISSIONS_APPROVAL, "params": {"threadId": "t1"}})));
        assert_eq!(p.decide("13", &PermissionDecision::Allow).unwrap()["result"]["permissions"], json!({}));
    }

    #[test]
    fn sub_agent_activity_is_ignored_and_its_requests_are_marked() {
        let mut p = ready();
        let s = p.ingest(&line(
            json!({"method": "item/agentMessage/delta", "params": {"threadId": "child", "delta": "x"}}),
        ));
        assert!(s.outputs.is_empty());
        let s = p.ingest(&line(json!({"id": 9, "method": COMMAND_APPROVAL, "params": {"kind": "command", "threadId": "child", "command": "x"}})));
        let AdapterOutput::PermissionRequested(r) = &s.outputs[0] else { panic!() };
        assert_eq!(r.input["other_thread"], true);
    }

    #[test]
    fn interrupt_resolves_pending_requests_and_reports_the_turn() {
        let mut p = ready();
        p.send_user_message("go".into());
        p.ingest(&line(json!({"id": 3, "result": {"turn": {"id": "u1"}}})));
        p.ingest(&line(json!({"id": 0, "method": COMMAND_APPROVAL, "params": {"kind": "command", "threadId": "t1", "command": "x"}})));
        let s = p.interrupt().unwrap();
        assert_eq!(s.writes[0]["method"], "turn/interrupt");
        assert_eq!(s.writes[0]["params"], json!({"threadId": "t1", "turnId": "u1"}));
        let s = p.ingest(&line(json!({"id": 4, "result": {}})));
        assert_eq!(events(&s), vec![&AgentEvent::InterruptAcknowledged]);
        let s = p.ingest(&line(json!({"method": "turn/completed", "params": {"threadId": "t1", "turn": {"id": "u1", "status": "interrupted"}}})));
        assert!(matches!(events(&s)[0], AgentEvent::TurnFinished { interrupted: true, success: false, .. }));
        let s = p.ingest(&line(
            json!({"method": "serverRequest/resolved", "params": {"threadId": "t1", "requestId": 0}}),
        ));
        assert_eq!(s.outputs, vec![AdapterOutput::PermissionCancelled { native_request_id: "0".into() }]);
        assert!(p.decide("0", &PermissionDecision::Allow).is_err(), "a resolved request cannot be answered");
    }

    #[test]
    fn interrupt_before_the_turn_id_is_known_is_sent_once_it_is() {
        let mut p = ready();
        p.send_user_message("go".into());
        assert!(p.interrupt().unwrap().writes.is_empty());
        let s = p.ingest(&line(json!({"id": 3, "result": {"turn": {"id": "u1"}}})));
        assert_eq!(s.writes[0]["method"], "turn/interrupt");
    }

    #[test]
    fn usage_sums_this_turns_calls_only() {
        let mut p = ready();
        p.send_user_message("go".into());
        p.ingest(&line(json!({"id": 3, "result": {"turn": {"id": "u1"}}})));
        let usage = |turn: &str, total: u64| {
            line(json!({"method": "thread/tokenUsage/updated", "params": {"threadId": "t1", "turnId": turn,
                "tokenUsage": {"total": {"totalTokens": 999}, "last": {"totalTokens": total, "inputTokens": total, "outputTokens": 0, "cachedInputTokens": 0, "reasoningOutputTokens": 0}}}}))
        };
        p.ingest(&usage("previous", 500)); // replayed on resume for an older turn
        p.ingest(&usage("u1", 10));
        p.ingest(&usage("u1", 15));
        let s = p.ingest(&line(json!({"method": "turn/completed", "params": {"threadId": "t1", "turn": {"id": "u1", "status": "completed"}}})));
        let AgentEvent::TurnFinished { usage, cost_usd, .. } = events(&s)[0] else { panic!() };
        assert_eq!(usage.map(|u| u.total_tokens), Some(25));
        assert_eq!(*cost_usd, None, "Codex reports no cost");
    }

    #[test]
    fn items_map_to_tool_activity_and_messages() {
        let mut p = ready();
        p.send_user_message("go".into());
        p.ingest(&line(json!({"id": 3, "result": {"turn": {"id": "u1"}}})));
        let s = p.ingest(&line(json!({"method": "item/started", "params": {"threadId": "t1", "item": {
            "type": "commandExecution", "id": "c1", "command": "/bin/zsh -lc ls", "cwd": "/ws", "status": "inProgress"}}})));
        assert!(
            matches!(events(&s)[0], AgentEvent::ToolStarted { tool_name, input, .. } if tool_name == "shell" && input["command"] == "ls")
        );
        let s = p.ingest(&line(json!({"method": "item/completed", "params": {"threadId": "t1", "item": {
            "type": "commandExecution", "id": "c1", "status": "declined"}}})));
        assert!(matches!(events(&s)[0], AgentEvent::ToolFinished { outcome: ToolOutcome::Rejected, .. }));
        let s = p.ingest(&line(
            json!({"method": "item/agentMessage/delta", "params": {"threadId": "t1", "delta": "Hel"}}),
        ));
        assert_eq!(
            s.outputs,
            vec![AdapterOutput::Live(LiveChunk { kind: LiveKind::Text, text: "Hel".into() })]
        );
        let s = p.ingest(&line(json!({"method": "item/completed", "params": {"threadId": "t1", "item": {
            "type": "agentMessage", "id": "m1", "text": "Hello"}}})));
        assert!(matches!(events(&s)[0], AgentEvent::Message { text, .. } if text == "Hello"));
        let s = p.ingest(&line(json!({"method": "item/started", "params": {"threadId": "t1", "item": {"type": "mcpToolCall", "id": "x"}}})));
        assert!(events(&s).iter().any(
            |e| matches!(e, AgentEvent::Warning { message } if message.contains("does not pass through"))
        ));
        let s = p.ingest(b"not json");
        assert!(matches!(events(&s)[0], AgentEvent::Warning { .. }));
    }

    // --- replays of real app-server output (tests/fixtures/codex-0.166.0) ---

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/codex-0.166.0");

    struct Replay {
        outputs: Vec<AdapterOutput>,
        protocol: Protocol,
    }

    /// Feeds a recording through the protocol. Prompts are queued up front
    /// (each starts when the previous turn completes); server requests are
    /// answered with `decide`; `interrupt_before` lists response ids before
    /// which an interrupt is requested.
    fn replay(
        name: &str,
        mode: ThreadMode,
        prompts: &[&str],
        decide: impl Fn(&PermissionRequest) -> PermissionDecision,
        interrupt_before: &[u64],
    ) -> Replay {
        let text = std::fs::read_to_string(format!("{FIXTURES}/{name}.stdout.jsonl")).unwrap();
        let mut p = Protocol::new(mode, "/workspace/demo", None, "0");
        p.start();
        for prompt in prompts {
            assert!(p.send_user_message((*prompt).into()).fatal.is_none());
        }
        let mut outputs = Vec::new();
        for line in text.lines() {
            let v: Value = serde_json::from_str(line).unwrap();
            if v.get("method").is_none()
                && let Some(id) = v.get("id").and_then(Value::as_u64)
                && interrupt_before.contains(&id)
            {
                let _ = p.interrupt();
            }
            let step = p.ingest(line.as_bytes());
            assert!(step.fatal.is_none(), "{name}: {:?}", step.fatal);
            for o in &step.outputs {
                if let AdapterOutput::PermissionRequested(r) = o {
                    p.decide(&r.native_request_id, &decide(r)).unwrap();
                }
            }
            outputs.extend(step.outputs);
        }
        Replay { outputs, protocol: p }
    }

    fn evs(r: &Replay) -> Vec<&AgentEvent> {
        r.outputs
            .iter()
            .filter_map(|o| if let AdapterOutput::Event(e) = o { Some(e) } else { None })
            .collect()
    }

    fn requests(r: &Replay) -> Vec<&PermissionRequest> {
        r.outputs
            .iter()
            .filter_map(|o| if let AdapterOutput::PermissionRequested(p) = o { Some(p) } else { None })
            .collect()
    }

    fn turns(r: &Replay) -> Vec<(bool, bool)> {
        evs(r)
            .iter()
            .filter_map(|e| match e {
                AgentEvent::TurnFinished { success, interrupted, .. } => Some((*success, *interrupted)),
                _ => None,
            })
            .collect()
    }

    fn deny() -> PermissionDecision {
        PermissionDecision::Deny { message: String::new() }
    }

    #[test]
    fn recorded_approvals_session() {
        let r = replay(
            "approvals",
            ThreadMode::New,
            &["read", "read outside", "write", "touch"],
            |req| if req.tool_name == "apply_patch" { PermissionDecision::Allow } else { deny() },
            &[],
        );
        assert!(
            matches!(evs(&r)[0], AgentEvent::Ready { native_session_id, .. } if native_session_id == "01a1ce51-38a7-7442-8d69-30c331815bea")
        );
        assert_eq!(turns(&r), vec![(true, false); 4]);
        let reqs = requests(&r);
        let tools: Vec<&str> = reqs.iter().map(|q| q.tool_name.as_str()).collect();
        assert_eq!(tools, ["shell", "apply_patch", "shell"]);
        assert_eq!(reqs[0].input["command"], "cat notes.txt", "the zsh wrapper is unwrapped");
        assert_eq!(reqs[0].input["raw_command"], "/bin/zsh -lc 'cat notes.txt'");
        assert_eq!(reqs[0].input["cwd_is_session_cwd"], true);
        assert_eq!(
            reqs[1].input["changes"][0]["path"], "/workspace/demo/hello.txt",
            "paths come from the item"
        );
        assert_eq!(reqs[1].input["changes"][0]["kind"], "add");
        assert_eq!(reqs[2].input["command"], "touch made-by-shell.txt");
        for q in &reqs {
            assert_eq!(
                q.input["unexpected_fields"],
                json!([]),
                "0.166.0 sends only known fields: {}",
                q.input
            );
        }
        let ops: Vec<_> = reqs.iter().map(|q| super::super::classify(q)).collect();
        assert_eq!(
            ops.iter().map(|o| o.category).collect::<Vec<_>>(),
            [
                switchyard_policy::OperationCategory::Shell,
                switchyard_policy::OperationCategory::FileWrite,
                switchyard_policy::OperationCategory::Shell
            ]
        );
        assert!(ops.iter().all(|o| o.problem.is_none() && o.unexpected_fields.is_empty()));
        let outcomes: Vec<&ToolOutcome> = evs(&r)
            .iter()
            .filter_map(
                |e| if let AgentEvent::ToolFinished { outcome, .. } = e { Some(outcome) } else { None },
            )
            .collect();
        assert_eq!(outcomes, [&ToolOutcome::Rejected, &ToolOutcome::Succeeded, &ToolOutcome::Rejected]);
        assert!(
            evs(&r)
                .iter()
                .any(|e| matches!(e, AgentEvent::TurnFinished { usage: Some(u), .. } if u.total_tokens > 0))
        );
        assert!(
            r.outputs
                .iter()
                .any(|o| matches!(o, AdapterOutput::Live(LiveChunk { kind: LiveKind::Text, .. })))
        );
        assert!(!r.protocol.turn_running());
    }

    #[test]
    fn recorded_interrupt() {
        let r = replay("interrupt", ThreadMode::New, &["essay"], |_| deny(), &[4]);
        assert_eq!(turns(&r), vec![(false, true)]);
        assert!(evs(&r).contains(&&AgentEvent::InterruptAcknowledged));
    }

    #[test]
    fn recorded_interrupt_with_a_pending_approval() {
        // Leave the request unanswered: the interrupt resolves it.
        let text =
            std::fs::read_to_string(format!("{FIXTURES}/interrupt_pending_approval.stdout.jsonl")).unwrap();
        let mut p = Protocol::new(ThreadMode::New, "/workspace/demo", None, "0");
        p.start();
        p.send_user_message("touch".into());
        let mut out = Vec::new();
        for line in text.lines() {
            if line.starts_with("{\"id\":4,") {
                p.interrupt().unwrap();
            }
            out.extend(p.ingest(line.as_bytes()).outputs);
        }
        let requested = out.iter().filter(|o| matches!(o, AdapterOutput::PermissionRequested(_))).count();
        let cancelled = out.iter().filter(|o| matches!(o, AdapterOutput::PermissionCancelled { .. })).count();
        assert_eq!((requested, cancelled), (1, 1));
        assert!(
            out.iter().any(|o| matches!(
                o,
                AdapterOutput::Event(AgentEvent::TurnFinished { interrupted: true, .. })
            ))
        );
    }

    #[test]
    fn recorded_resume() {
        let r = replay(
            "resume",
            ThreadMode::Resume("01a1ce55-8f24-7a91-ad6b-38c45b28f457".into()),
            &["what did rg print"],
            |_| deny(),
            &[],
        );
        assert!(
            matches!(evs(&r)[0], AgentEvent::Ready { native_session_id, .. } if native_session_id == "01a1ce55-8f24-7a91-ad6b-38c45b28f457")
        );
        assert!(
            evs(&r)
                .iter()
                .any(|e| matches!(e, AgentEvent::Message { text, .. } if text.contains("sapphire")))
        );
        // The replayed usage of the earlier turn is not counted for this one.
        let usage = evs(&r)
            .iter()
            .find_map(|e| if let AgentEvent::TurnFinished { usage, .. } = e { *usage } else { None });
        assert_eq!(usage.map(|u| u.total_tokens), Some(19500));
    }

    #[test]
    fn recorded_sub_agent_requests_are_marked_and_its_events_ignored() {
        let r = replay("subagent", ThreadMode::New, &["spawn"], |_| deny(), &[]);
        let reqs = requests(&r);
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].input["other_thread"], true);
        let op = super::super::classify(reqs[0]);
        assert!(op.problem.as_deref().is_some_and(|p| p.contains("sub-agent")));
        // The recording ends after the child's turn/completed (the research
        // client stopped there); that must not end the session's own turn.
        assert!(turns(&r).is_empty(), "a sub-agent's turn/completed is not the session's");
        assert!(r.protocol.turn_running());
        assert!(evs(&r).iter().any(
            |e| matches!(e, AgentEvent::Warning { message } if message.contains("collabAgentToolCall"))
        ));
    }

    #[test]
    fn recorded_hardened_session_has_no_unmediated_tools() {
        let r = replay("hardened_tools", ThreadMode::New, &["tools", "search", "touch"], |_| deny(), &[]);
        assert_eq!(turns(&r).len(), 3);
        assert!(
            !evs(&r).iter().any(|e| matches!(e, AgentEvent::ToolStarted { tool_name, .. } if tool_name != "shell" && tool_name != "apply_patch")),
            "no web search, MCP or other tool ran"
        );
    }
}
