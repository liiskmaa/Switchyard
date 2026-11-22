//! `switchyard-fake-gemini`: a deterministic stand-in for Gemini CLI used by
//! integration tests. It answers `--version` and `--help`, and `--acp` speaks
//! the Agent Client Protocol as Switchyard verified it against Gemini CLI
//! 0.70.0 (message shapes follow the recorded sessions in
//! `crates/switchyard-agents/tests/fixtures/gemini-0.70.0`).
//!
//! It is a test harness: never bundled, run only when Core is configured to.
//! It never runs a command: an allowed `cmd:touch FILE` creates FILE, every
//! other command only reports success.
//!
//! The scenario is chosen per prompt with a marker `[fake:NAME]`:
//!
//! | NAME | Behaviour |
//! |---|---|
//! | `text` (default) | streams "Hello from fake gemini." and ends the turn |
//! | `write:FILE` | an `edit` tool call creating FILE; written when allowed |
//! | `read:FILE` | a `read` tool call for FILE |
//! | `cmd:COMMAND` | an `execute` tool call for COMMAND |
//! | `slow` | streams 60 chunks 50 ms apart; cancellable |
//! | `pending` | a permission request, then waits; cancellable |
//! | `yolo` | reports a switch to the `yolo` mode (Switchyard must stop) |
//! | `whoami` | replies with its session id |
//! | `report:HEX` | task scenario: see `fake_support/report.rs` |
//!
//! Sessions are stored under `$TMPDIR/switchyard-fake-gemini/` so a new
//! process can load them; every permission answer is appended to
//! `answers-<session>.jsonl` there (test evidence).

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use serde_json::{Value, json};

#[path = "fake_support/report.rs"]
mod report;

const VERSION: &str = "0.70.0";

fn out(v: &Value) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{v}");
    let _ = stdout.flush();
}

fn store() -> PathBuf {
    let dir = std::env::temp_dir().join("switchyard-fake-gemini");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn record_answer(session: &str, answer: &Value) {
    let path = store().join(format!("answers-{session}.jsonl"));
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{answer}");
    }
}

fn marker(text: &str) -> (&str, Option<&str>) {
    let Some(start) = text.find("[fake:") else { return ("text", None) };
    let rest = &text[start + 6..];
    let Some(end) = rest.find(']') else { return ("text", None) };
    let body = &rest[..end];
    match body.split_once(':') {
        Some((name, arg)) => (name, Some(arg)),
        None => (body, None),
    }
}

enum Wait {
    Answer(Value),
    Cancelled,
    Closed,
}

struct Fake {
    input: Receiver<Value>,
    cwd: String,
    model: String,
    session: Option<String>,
    next_request: u64,
    next_tool: u64,
    task: report::TaskScript,
    backlog: Vec<Value>,
}

impl Fake {
    fn session(&self) -> String {
        self.session.clone().unwrap_or_default()
    }

    fn update(&self, update: Value) {
        out(&json!({ "jsonrpc": "2.0", "method": "session/update",
            "params": { "sessionId": self.session(), "update": update } }));
    }

    fn say(&self, text: &str) {
        self.update(
            json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": text } }),
        );
    }

    fn session_result(&self) -> Value {
        json!({
            "modes": { "availableModes": [
                { "id": "default", "name": "Default", "description": "Prompts for approval" },
                { "id": "yolo", "name": "YOLO", "description": "Auto-approves all tools" }],
                "currentModeId": "default" },
            "models": { "availableModels": [
                { "modelId": "auto", "name": "Auto" },
                { "modelId": "gemini-fake-flash", "name": "gemini-fake-flash" },
                { "modelId": "gemini-fake-pro", "name": "gemini-fake-pro" }],
                "currentModelId": self.model },
        })
    }

    /// Asks permission for a tool call; waits for the answer or a cancel.
    fn ask(&mut self, tool_id: &str, kind: &str, title: &str, locations: Value, content: Value) -> Wait {
        let id = self.next_request;
        self.next_request += 1;
        out(&json!({ "jsonrpc": "2.0", "id": id, "method": "session/request_permission", "params": {
            "sessionId": self.session(),
            "toolCall": { "toolCallId": tool_id, "title": title, "kind": kind, "status": "pending",
                "locations": locations, "content": content },
            "options": [
                { "optionId": "proceed_always", "name": "Always allow", "kind": "allow_always" },
                { "optionId": "proceed_once", "name": "Allow", "kind": "allow_once" },
                { "optionId": "cancel", "name": "Reject", "kind": "reject_once" }] } }));
        loop {
            match self.input.recv() {
                Err(_) => return Wait::Closed,
                Ok(m) if m.get("id") == Some(&json!(id)) && m.get("method").is_none() => {
                    record_answer(&self.session(), &m);
                    return Wait::Answer(m);
                }
                Ok(m) if m.get("method").and_then(Value::as_str) == Some("session/cancel") => {
                    // ACP: the client answers the pending request as cancelled.
                    if let Ok(a) = self.input.recv_timeout(Duration::from_secs(2)) {
                        record_answer(&self.session(), &a);
                    }
                    return Wait::Cancelled;
                }
                Ok(m) => self.backlog.push(m),
            }
        }
    }

    /// Runs one tool scenario; `false` when the turn was cancelled.
    fn tool(&mut self, spec: &str) -> Result<bool, ()> {
        self.next_tool += 1;
        let tool_id = format!("fake-{}", self.next_tool);
        let (name, arg) = spec.split_once(':').unwrap_or((spec, ""));
        let cwd = self.cwd.clone();
        let abs = |p: &str| if p.starts_with('/') { p.to_owned() } else { format!("{cwd}/{p}") };
        let (kind, title, locations, content) = match name {
            "write" => (
                "edit",
                format!("Writing to {arg}"),
                json!([{ "path": abs(arg) }]),
                json!([{ "type": "diff", "path": abs(arg), "oldText": null, "newText": "written by fake gemini\n" }]),
            ),
            "read" => ("read", arg.to_owned(), json!([{ "path": abs(arg) }]), json!([])),
            "cmd" => ("execute", arg.to_owned(), json!([]), json!([])),
            _ => ("other", spec.to_owned(), json!([]), json!([])),
        };
        self.update(
            json!({ "sessionUpdate": "tool_call", "toolCallId": tool_id, "title": title, "kind": kind,
            "status": "pending", "locations": locations }),
        );
        let allowed = match self.ask(&tool_id, kind, &title, locations, content) {
            Wait::Closed => return Err(()),
            Wait::Cancelled => return Ok(false),
            Wait::Answer(a) => {
                a["result"]["outcome"]["outcome"] == "selected"
                    && a["result"]["outcome"]["optionId"] == "proceed_once"
            }
        };
        if allowed {
            match name {
                "write" => {
                    let _ = std::fs::write(abs(arg), "written by fake gemini\n");
                }
                "cmd" => {
                    if let Some(f) = arg.strip_prefix("touch ") {
                        let _ = std::fs::write(abs(f.trim()), "");
                    }
                }
                _ => {}
            }
        }
        self.update(json!({ "sessionUpdate": "tool_call_update", "toolCallId": tool_id,
            "status": if allowed { "completed" } else { "failed" },
            "content": [{ "type": "content", "content": { "type": "text", "text": if allowed { "done" } else { "rejected" } } }] }));
        Ok(true)
    }

    /// Streams text; `false` when cancelled meanwhile.
    fn stream(&mut self, chunks: &[String], delay: Duration) -> Result<bool, ()> {
        for c in chunks {
            self.say(c);
            match self.input.recv_timeout(delay) {
                Ok(m) if m.get("method").and_then(Value::as_str) == Some("session/cancel") => {
                    return Ok(false);
                }
                Ok(m) => self.backlog.push(m),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Err(()),
            }
        }
        Ok(true)
    }

    fn prompt(&mut self, id: &Value, text: &str) {
        let (name, arg) = marker(text);
        if name == "report" {
            self.task.start(text, arg);
        }
        let finished: Result<bool, ()> = if self.task.active() {
            match self.task.next() {
                Some(step) => {
                    let mut ok = Ok(true);
                    for t in &step.tools {
                        ok = self.tool(t);
                        if !matches!(ok, Ok(true)) {
                            break;
                        }
                    }
                    if matches!(ok, Ok(true)) {
                        self.say(&step.text);
                    }
                    ok
                }
                None => {
                    self.say("No more steps in this scenario.");
                    Ok(true)
                }
            }
        } else {
            match name {
                "write" | "read" | "cmd" => {
                    let spec = format!("{name}:{}", arg.unwrap_or("fake-output.txt"));
                    let r = self.tool(&spec);
                    if r == Ok(true) {
                        self.say("DONE");
                    }
                    r
                }
                "slow" => {
                    let chunks: Vec<String> = (1..=60).map(|i| format!("chunk{i} ")).collect();
                    self.stream(&chunks, Duration::from_millis(50))
                }
                "pending" => {
                    self.next_tool += 1;
                    let id = format!("fake-{}", self.next_tool);
                    match self.ask(&id, "execute", "sleep 100", json!([]), json!([])) {
                        Wait::Closed => Err(()),
                        Wait::Cancelled => Ok(false),
                        Wait::Answer(_) => Ok(true),
                    }
                }
                "yolo" => {
                    self.update(json!({ "sessionUpdate": "current_mode_update", "currentModeId": "yolo" }));
                    Ok(true)
                }
                "whoami" => {
                    let s = self.session();
                    self.say(&s);
                    Ok(true)
                }
                _ => {
                    self.say("Hello from fake gemini.");
                    Ok(true)
                }
            }
        };
        match finished {
            Err(()) => std::process::exit(0),
            Ok(true) => out(&json!({ "jsonrpc": "2.0", "id": id, "result": { "stopReason": "end_turn",
                "usage": { "inputTokens": 10, "outputTokens": 5, "cachedReadTokens": 2, "thoughtTokens": 1, "totalTokens": 15 } } })),
            Ok(false) => out(&json!({ "jsonrpc": "2.0", "id": id, "result": { "stopReason": "cancelled" } })),
        }
    }

    fn handle(&mut self, m: Value) {
        let id = m.get("id").cloned().unwrap_or(Value::Null);
        let p = m.get("params").cloned().unwrap_or(Value::Null);
        match m.get("method").and_then(Value::as_str) {
            Some("initialize") => out(&json!({ "jsonrpc": "2.0", "id": id, "result": {
                "protocolVersion": 1,
                "authMethods": [{ "id": "oauth-personal", "name": "Log in with Google" },
                                { "id": "gemini-api-key", "name": "Gemini API key" }],
                "agentInfo": { "name": "gemini-cli", "title": "Gemini CLI", "version": VERSION },
                "agentCapabilities": { "loadSession": true } } })),
            Some("authenticate") => out(&json!({ "jsonrpc": "2.0", "id": id, "result": {} })),
            Some("session/new") => {
                self.cwd = p["cwd"].as_str().unwrap_or_default().to_owned();
                let sid = uuid::Uuid::now_v7().to_string();
                let _ = std::fs::write(store().join(&sid), &self.cwd);
                self.session = Some(sid.clone());
                let mut r = self.session_result();
                r["sessionId"] = json!(sid);
                out(&json!({ "jsonrpc": "2.0", "id": id, "result": r }));
            }
            Some("session/load") => {
                let sid = p["sessionId"].as_str().unwrap_or_default().to_owned();
                if std::fs::read_to_string(store().join(&sid)).is_err() {
                    out(
                        &json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": "session not found" } }),
                    );
                    return;
                }
                self.cwd = p["cwd"].as_str().unwrap_or_default().to_owned();
                self.session = Some(sid);
                // History replay comes before the answer, as in the real CLI.
                self.update(json!({ "sessionUpdate": "user_message_chunk", "content": { "type": "text", "text": "(earlier)" } }));
                out(&json!({ "jsonrpc": "2.0", "id": id, "result": self.session_result() }));
            }
            Some("session/prompt") => {
                let text: String = p["prompt"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n"))
                    .unwrap_or_default();
                self.prompt(&id, &text);
            }
            Some("session/cancel") => {}
            Some(other) if !id.is_null() => out(&json!({ "jsonrpc": "2.0", "id": id,
                "error": { "code": -32601, "message": format!("fake gemini: {other} not supported") } })),
            _ => {}
        }
    }
}

fn serve(model: String) {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if let Ok(v) = serde_json::from_str::<Value>(&line)
                && tx.send(v).is_err()
            {
                break;
            }
        }
    });
    let mut fake = Fake {
        input: rx,
        cwd: String::new(),
        model,
        session: None,
        next_request: 0,
        next_tool: 0,
        task: report::TaskScript::default(),
        backlog: Vec::new(),
    };
    loop {
        let next =
            if fake.backlog.is_empty() { fake.input.recv().ok() } else { Some(fake.backlog.remove(0)) };
        let Some(m) = next else { break };
        fake.handle(m);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version") => println!("{VERSION}"),
        Some("--help") => println!(
            "Usage: gemini [options]\n      --acp                       Starts the agent in ACP mode\n      --admin-policy              Additional admin policy files"
        ),
        Some("--acp") => {
            // Refuse to run without the admin policy Switchyard passes.
            let policy = args.iter().position(|a| a == "--admin-policy").and_then(|i| args.get(i + 1));
            let Some(dir) = policy else {
                eprintln!("fake gemini: no --admin-policy");
                std::process::exit(2);
            };
            if std::fs::read_to_string(PathBuf::from(dir).join("switchyard.toml")).is_err() {
                eprintln!("[ADMIN] Policy file error in switchyard.toml: missing");
            }
            let model = args
                .iter()
                .position(|a| a == "-m")
                .and_then(|i| args.get(i + 1))
                .cloned()
                .unwrap_or_else(|| "auto".into());
            serve(model);
        }
        _ => {
            eprintln!("fake gemini: unsupported arguments {args:?}");
            std::process::exit(2);
        }
    }
}
