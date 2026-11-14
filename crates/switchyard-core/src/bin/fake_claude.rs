//! `switchyard-fake-claude`: a deterministic stand-in for the Claude Code CLI
//! used by integration tests. It speaks the same stream-json protocol that
//! Switchyard verified against CLI 2.1.322 (message shapes follow the
//! recorded transcripts in `crates/switchyard-agents/tests/fixtures`).
//!
//! It is a test harness, not a simulator for the UI: it is never bundled and
//! Core only runs it when explicitly configured to.
//!
//! The scenario is chosen per user message with a marker `[fake:NAME]`:
//!
//! | NAME          | Behaviour |
//! |---------------|-----------|
//! | `text` (default) | streams "Hello from fake claude." and completes |
//! | `write:FILE`  | requests Write permission; on allow writes FILE in cwd |
//! | `multi`       | requests two permissions at once (a.txt, b.txt) |
//! | `slow`        | streams 60 chunks 50 ms apart; interruptible |
//! | `delay`       | waits 1.5 s before any output |
//! | `crash`       | streams a little, prints to stderr, exits with code 3 |
//! | `malformed`   | emits invalid lines, then a normal reply |
//! | `whoami`      | replies with its session id |
//! | `exit`        | completes the turn, then exits 0 on its own |
//! | `report:HEX` | task scenario: see `fake_support/report.rs` (a role's steps: an optional tool, then a report block) |
//! | `tool:NAME\|JSON` | requests permission for tool NAME with input JSON (`$CWD` is replaced by the working directory; NAME `-` omits the tool name; invalid JSON is sent as a string). On allow, `Write` writes its file; other tools report success. |
//!
//! With `SWITCHYARD_FAKE_CLAUDE_LOG=FILE`, every permission answer it receives
//! is appended to FILE as a JSON line (end-to-end test evidence).
//!
//! A `.fake-claude-fail` file in the working directory makes it fail at
//! start-up like an unauthenticated CLI.

use std::io::{BufRead, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use serde_json::{Value, json};

#[path = "fake_support/report.rs"]
mod report;

const VERSION: &str = "2.1.322 (Claude Code)";

fn out(v: &Value) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{v}");
    let _ = stdout.flush();
}

const FAKE_TURN_COST_USD: f64 = 0.001;

struct Fake {
    session_id: String,
    cwd: String,
    input: Receiver<Value>,
    last_error: bool,
    /// Running session total, like the real CLI's `total_cost_usd`.
    total_cost_usd: f64,
    request_seq: u32,
    /// User messages that arrived while a turn ran (processed next).
    queued: Vec<Value>,
    /// The task scenario of this process (`[fake:report:HEX]`).
    task: report::TaskScript,
}

enum Wait {
    Message(Value),
    Interrupted,
    Closed,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--version") {
        println!("{VERSION}");
        return;
    }
    if args.first().map(String::as_str) == Some("--help") {
        println!("Usage: claude [options]\n  --input-format --output-format --session-id --resume");
        println!("  --permission-prompt-tool --restricted --strict-mcp-config --include-partial-messages");
        println!("  --tools --settings --verbose --model");
        return;
    }
    if args.len() >= 2 && args[0] == "auth" && args[1] == "status" {
        println!(
            r#"{{"loggedIn":true,"authMethod":"fake","apiProvider":"firstParty","email":"never-read@example.invalid"}}"#
        );
        return;
    }
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let required = ["--input-format", "--output-format", "--permission-prompt-tool"];
    if required.iter().any(|f| flag(f).is_none()) || !args.iter().any(|a| a == "-p") {
        eprintln!("fake claude: missing required stream-json flags");
        std::process::exit(2);
    }
    let cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default();
    if std::path::Path::new(".fake-claude-fail").exists() {
        eprintln!("Invalid API key · Please run /login");
        std::process::exit(1);
    }
    let session_id = flag("--session-id").or_else(|| flag("--resume")).unwrap_or_else(|| "missing".into());

    let (tx, rx) = mpsc::channel();
    // Test evidence: with SWITCHYARD_FAKE_CLAUDE_LOG set, every permission
    // answer the agent receives is appended there as one JSON line.
    let answer_log = std::env::var_os("SWITCHYARD_FAKE_CLAUDE_LOG").filter(|v| !v.is_empty());
    let (log_session, log_cwd) = (session_id.clone(), cwd.clone());
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            if let Some(path) = &answer_log
                && v["type"] == "control_response"
                && let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path)
            {
                let entry = json!({"session_id": log_session, "cwd": log_cwd, "response": v["response"]});
                // One write per line: concurrent fake agents append to the same
                // log, and a line formatted in pieces could interleave.
                let _ = f.write_all(format!("{entry}\n").as_bytes());
            }
            if tx.send(v).is_err() {
                break;
            }
        }
    });
    let mut fake = Fake {
        session_id,
        cwd,
        input: rx,
        last_error: false,
        total_cost_usd: 0.0,
        request_seq: 0,
        queued: Vec::new(),
        task: report::TaskScript::default(),
    };
    fake.run();
    std::process::exit(i32::from(fake.last_error));
}

fn marker(text: &str) -> (&str, Option<&str>) {
    let Some(start) = text.find("[fake:") else { return ("text", None) };
    let rest = &text[start + 6..];
    let end = rest.find(']').unwrap_or(rest.len());
    let spec = &rest[..end];
    match spec.split_once(':') {
        Some((name, arg)) => (name, Some(arg)),
        None => (spec, None),
    }
}

impl Fake {
    fn run(&mut self) {
        loop {
            let next =
                if self.queued.is_empty() { self.input.recv().ok() } else { Some(self.queued.remove(0)) };
            let Some(message) = next else {
                if let Some(d) = self.task.linger() {
                    std::thread::sleep(d);
                }
                return;
            };
            match message["type"].as_str() {
                Some("user") => {
                    let text = message["message"]["content"].as_str().unwrap_or_default().to_owned();
                    if !self.turn(&text) {
                        return;
                    }
                }
                Some("control_request") if message["request"]["subtype"] == "interrupt" => {
                    // Nothing running: acknowledge.
                    self.ack_interrupt(&message);
                }
                _ => {}
            }
        }
    }

    fn base(&self, mut v: Value) -> Value {
        v["session_id"] = json!(self.session_id);
        v["uuid"] = json!(uuid::Uuid::now_v7().to_string());
        v
    }

    fn init(&self) {
        out(&self.base(json!({
            "type": "system", "subtype": "init", "cwd": self.cwd,
            "tools": ["Bash", "Edit", "Glob", "Grep", "NotebookEdit", "Read", "Write"],
            "mcp_servers": [], "model": "fake-model", "permissionMode": "default",
            "claude_code_version": "2.1.322", "apiKeySource": "none",
        })));
    }

    fn stream(&self, event: Value) {
        out(&self.base(json!({"type": "stream_event", "event": event, "parent_tool_use_id": null})));
    }

    fn assistant_text(&self, text: &str) {
        out(&self.base(json!({"type": "assistant", "parent_tool_use_id": null, "message": {
            "id": format!("msg_fake_{}", uuid::Uuid::now_v7().simple()), "type": "message", "role": "assistant",
            "model": "fake-model", "content": [{"type": "text", "text": text}]}})));
    }

    fn result(&mut self, success: bool, terminal: &str, text: &str) {
        self.last_error = !success;
        // Like 2.1.322: a finished turn reports its own usage since the last result and the
        // session's running total; an aborted turn reports no usage (`{}`).
        let aborted = terminal.starts_with("aborted");
        let usage = if aborted {
            json!({})
        } else {
            self.total_cost_usd += FAKE_TURN_COST_USD;
            json!({"fake-model": {"costUSD": FAKE_TURN_COST_USD}})
        };
        let mut v = json!({"type": "result", "subtype": if success { "success" } else { "error_during_execution" },
            "is_error": !success, "duration_ms": 10, "num_turns": 1, "total_cost_usd": self.total_cost_usd,
            "model_usage_since_last_result": usage,
            "stop_reason": if success { json!("end_turn") } else { Value::Null },
            "terminal_reason": terminal, "permission_denials": []});
        if success {
            v["result"] = json!(text);
        }
        out(&self.base(v));
    }

    fn ack_interrupt(&self, request: &Value) {
        out(&json!({"type": "control_response", "response": {"subtype": "success",
            "request_id": request["request_id"], "response": {"still_queued": []}}}));
    }

    fn interrupted(&mut self, terminal: &str) {
        out(&self.base(json!({"type": "user", "parent_tool_use_id": null,
            "message": {"role": "user", "content": [{"type": "text", "text": "[Request interrupted by user]"}]}})));
        self.result(false, terminal, "");
    }

    /// Waits for input; interrupts are reported, user messages are queued.
    fn wait(&mut self, timeout: Option<Duration>) -> Option<Wait> {
        loop {
            let message = match timeout {
                Some(t) => match self.input.recv_timeout(t) {
                    Ok(m) => m,
                    Err(RecvTimeoutError::Timeout) => return None,
                    Err(RecvTimeoutError::Disconnected) => return Some(Wait::Closed),
                },
                None => match self.input.recv() {
                    Ok(m) => m,
                    Err(_) => return Some(Wait::Closed),
                },
            };
            match message["type"].as_str() {
                Some("control_request") if message["request"]["subtype"] == "interrupt" => {
                    self.ack_interrupt(&message);
                    return Some(Wait::Interrupted);
                }
                Some("user") => self.queued.push(message),
                _ => return Some(Wait::Message(message)),
            }
        }
    }

    /// Streams `text` as one assistant message. Returns false if interrupted.
    fn say(&mut self, text: &str, delay: Option<Duration>) -> Result<(), ()> {
        self.stream(json!({"type": "message_start", "message": {"id": "msg_fake", "role": "assistant", "content": []}}));
        self.stream(
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        );
        let words: Vec<&str> = text.split_inclusive(' ').collect();
        for word in words {
            self.stream(json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": word}}));
            if let Some(d) = delay
                && let Some(Wait::Interrupted) | Some(Wait::Closed) = self.wait(Some(d))
            {
                self.stream(json!({"type": "message_stop"}));
                return Err(());
            }
        }
        self.stream(json!({"type": "content_block_stop", "index": 0}));
        self.assistant_text(text);
        self.stream(json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}));
        self.stream(json!({"type": "message_stop"}));
        Ok(())
    }

    fn tool_use(&mut self, file: &str) -> (String, String, Value) {
        self.request_seq += 1;
        let tool_use_id = format!("toolu_fake_{}", self.request_seq);
        let request_id = uuid::Uuid::now_v7().to_string();
        let input =
            json!({"file_path": format!("{}/{file}", self.cwd), "content": "written by fake claude\n"});
        out(&self.base(json!({"type": "assistant", "parent_tool_use_id": null, "message": {
            "id": "msg_fake_tool", "type": "message", "role": "assistant", "model": "fake-model",
            "content": [{"type": "tool_use", "id": tool_use_id, "name": "Write", "input": input}]}})));
        (tool_use_id, request_id, input)
    }

    fn ask(&self, tool_use_id: &str, request_id: &str, input: &Value, file: &str) {
        out(&json!({"type": "control_request", "request_id": request_id, "request": {
            "subtype": "can_use_tool", "tool_name": "Write", "display_name": "Write", "input": input,
            "description": file, "tool_use_id": tool_use_id,
            "permission_suggestions": [{"type": "setMode", "mode": "acceptEdits", "destination": "session"}]}}));
    }

    fn tool_result(&self, tool_use_id: &str, content: &str, is_error: bool) {
        out(&self.base(
            json!({"type": "user", "parent_tool_use_id": null, "message": {"role": "user", "content": [
            {"tool_use_id": tool_use_id, "type": "tool_result", "content": content, "is_error": is_error}]}}),
        ));
    }

    /// Runs permission-gated writes. Returns `Err(terminal)` if interrupted.
    fn gated_writes(&mut self, files: &[&str]) -> Result<(), String> {
        let calls: Vec<_> = files.iter().map(|f| (f.to_string(), self.tool_use(f))).collect();
        let mut pending: Vec<_> =
            calls.iter().map(|(f, (t, r, i))| (f.clone(), t.clone(), r.clone(), i.clone())).collect();
        for (file, tool, request, input) in &pending {
            self.ask(tool, request, input, file);
        }
        while !pending.is_empty() {
            match self.wait(None) {
                Some(Wait::Message(m)) if m["type"] == "control_response" => {
                    let id = m["response"]["request_id"].as_str().unwrap_or_default().to_owned();
                    let Some(pos) = pending.iter().position(|p| p.2 == id) else { continue };
                    let (file, tool, _, _) = pending.remove(pos);
                    let decision = &m["response"]["response"];
                    if decision["behavior"] == "allow" {
                        let path = std::path::Path::new(&self.cwd).join(&file);
                        match std::fs::write(&path, "written by fake claude\n") {
                            Ok(()) => self.tool_result(
                                &tool,
                                &format!("File created successfully at: {}", path.display()),
                                false,
                            ),
                            Err(e) => self.tool_result(&tool, &format!("write failed: {e}"), true),
                        }
                    } else {
                        let message = decision["message"].as_str().unwrap_or("denied");
                        self.tool_result(&tool, message, true);
                    }
                }
                Some(Wait::Interrupted) => {
                    for (_, tool, request, _) in &pending {
                        out(&json!({"type": "control_cancel_request", "request_id": request}));
                        self.tool_result(tool, "The user doesn't want to proceed with this tool use.", true);
                    }
                    return Err("aborted_tools".into());
                }
                Some(Wait::Closed) | None => return Err("aborted_tools".into()),
                Some(Wait::Message(_)) => {}
            }
        }
        Ok(())
    }

    /// One permission-gated call of an arbitrary tool (policy tests).
    fn gated_tool(&mut self, spec: &str) -> Result<(), String> {
        let (name, raw) = spec.split_once('|').unwrap_or((spec, "{}"));
        let text = raw.replace("$CWD", &self.cwd);
        let input: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
        self.request_seq += 1;
        let tool_use_id = format!("toolu_fake_{}", self.request_seq);
        let request_id = uuid::Uuid::now_v7().to_string();
        out(&self.base(json!({"type": "assistant", "parent_tool_use_id": null, "message": {
            "id": "msg_fake_tool", "type": "message", "role": "assistant", "model": "fake-model",
            "content": [{"type": "tool_use", "id": tool_use_id, "name": name, "input": input}]}})));
        let mut request = json!({"subtype": "can_use_tool", "input": input, "tool_use_id": tool_use_id,
            "description": name});
        if name != "-" {
            request["tool_name"] = json!(name);
            request["display_name"] = json!(name);
        }
        out(&json!({"type": "control_request", "request_id": request_id, "request": request}));
        loop {
            match self.wait(None) {
                Some(Wait::Message(m)) if m["type"] == "control_response" => {
                    if m["response"]["request_id"].as_str() != Some(request_id.as_str()) {
                        continue;
                    }
                    let decision = &m["response"]["response"];
                    if decision["behavior"] == "allow" {
                        let path = input.get("file_path").and_then(Value::as_str);
                        let result = match (name, path) {
                            // Like Claude Code's Write, parent directories are created.
                            ("Write", Some(path)) => std::path::Path::new(path)
                                .parent()
                                .map_or(Ok(()), std::fs::create_dir_all)
                                .and_then(|()| std::fs::write(path, "written by fake claude\n"))
                                .map(|()| format!("File created successfully at: {path}"))
                                .map_err(|e| format!("write failed: {e}")),
                            _ => Ok(format!("{name} ran")),
                        };
                        match result {
                            Ok(text) => self.tool_result(&tool_use_id, &text, false),
                            Err(text) => self.tool_result(&tool_use_id, &text, true),
                        }
                    } else {
                        let message = decision["message"].as_str().unwrap_or("denied");
                        self.tool_result(&tool_use_id, message, true);
                    }
                    return Ok(());
                }
                Some(Wait::Interrupted) => {
                    out(&json!({"type": "control_cancel_request", "request_id": request_id}));
                    self.tool_result(
                        &tool_use_id,
                        "The user doesn't want to proceed with this tool use.",
                        true,
                    );
                    return Err("aborted_tools".into());
                }
                Some(Wait::Closed) | None => return Err("aborted_tools".into()),
                Some(Wait::Message(_)) => {}
            }
        }
    }

    /// One turn. Returns false when the process should exit.
    fn turn(&mut self, text: &str) -> bool {
        let (name, arg) = marker(text);
        if name == "report" {
            self.task.start(text, arg);
        }
        if self.task.active() {
            return self.task_turn();
        }
        if name == "delay" {
            std::thread::sleep(Duration::from_millis(1500));
        }
        self.init();
        let outcome = match name {
            "write" => self.gated_writes(&[arg.unwrap_or("fake-output.txt")]),
            "multi" => self.gated_writes(&["a.txt", "b.txt"]),
            "tool" => self.gated_tool(arg.unwrap_or("-")),
            "slow" => {
                let long = (1..=60).map(|i| format!("chunk{i} ")).collect::<String>();
                self.say(long.trim_end(), Some(Duration::from_millis(50)))
                    .map_err(|()| "aborted_streaming".to_owned())
            }
            "crash" => {
                self.stream(json!({"type": "message_start", "message": {"id": "msg_fake", "content": []}}));
                self.stream(json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "about to crash"}}));
                eprintln!("fake claude: simulated fatal error");
                std::process::exit(3);
            }
            "malformed" => {
                let mut stdout = std::io::stdout().lock();
                let _ = writeln!(stdout, "this is not json");
                let _ = writeln!(stdout, "[1,2,3]");
                let _ = writeln!(stdout, "{{\"type\":\"future_message\",\"x\":1}}");
                let _ = stdout.flush();
                Ok(())
            }
            _ => Ok(()),
        };
        if let Err(terminal) = outcome {
            self.interrupted(&terminal);
            return true;
        }
        let reply = match name {
            "whoami" => format!("session {}", self.session_id),
            "write" | "multi" | "tool" => "DONE".to_owned(),
            "slow" => "finished slow output".to_owned(),
            _ => "Hello from fake claude.".to_owned(),
        };
        if self.say(&reply, None).is_err() {
            self.interrupted("aborted_streaming");
            return true;
        }
        self.result(true, "completed", &reply);
        name != "exit"
    }

    /// One turn of the task scenario: an optional tool, then the final message.
    fn task_turn(&mut self) -> bool {
        self.init();
        let step =
            self.task.next().unwrap_or(report::Step { tool: None, text: "Nothing more to do.".into() });
        if let Some(tool) = &step.tool {
            let (name, arg) = tool.split_once(':').unwrap_or((tool.as_str(), ""));
            let outcome = match name {
                "write" => self.gated_writes(&[arg]),
                _ => self.gated_tool(arg),
            };
            if let Err(terminal) = outcome {
                self.interrupted(&terminal);
                return true;
            }
        }
        if self.say(&step.text, None).is_err() {
            self.interrupted("aborted_streaming");
            return true;
        }
        self.result(true, "completed", &step.text);
        true
    }
}
