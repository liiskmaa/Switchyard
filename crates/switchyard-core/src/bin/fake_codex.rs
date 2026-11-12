//! `switchyard-fake-codex`: a deterministic stand-in for the Codex CLI used by
//! integration tests. It answers `--version`, `login status` and
//! `app-server --help`, and `app-server` speaks the JSON-RPC protocol
//! Switchyard verified against codex-cli 0.166.0 (message shapes follow the
//! recorded sessions; see `docs/architecture/codex-integration.md`).
//!
//! It is a test harness, not a simulator for the UI: it is never bundled and
//! Core only runs it when explicitly configured to. It never runs a command:
//! an approved `cmd:touch FILE` creates FILE, every other command only
//! reports success.
//!
//! The scenario is chosen per user message with a marker `[fake:NAME]`:
//!
//! | NAME | Behaviour |
//! |---|---|
//! | `text` (default) | streams "Hello from fake codex." and completes |
//! | `write:FILE` | patch adding FILE; written on accept |
//! | `edit:FILE` | patch updating FILE (appends a line on accept) |
//! | `mixed` | one patch that adds a.txt and updates b.txt |
//! | `cmd:COMMAND` | command approval for `/bin/zsh -lc 'COMMAND'` |
//! | `slow` | streams 60 deltas 50 ms apart; interruptible |
//! | `pending` | command approval, then waits; interruptible |
//! | `perm` | `item/permissions/requestApproval` |
//! | `subagent` | a child thread's `turn/started`, then a command approval from it |
//! | `move` | a patch that moves README.md |
//! | `newreq` | a server request with an unknown method |
//! | `garbage` | malformed, truncated and unknown messages, then a reply |
//! | `crashpending` | a command approval, then exit 3 before any answer |
//! | `dupid` | two approval requests with the same JSON-RPC id |
//! | `unmediated` | an `mcpToolCall` item that never asks |
//! | `whoami` | replies with its thread id |
//! | `remember` | replies with every earlier user message of the thread |
//! | `crash` | streams a little, writes to stderr, exits with code 3 |
//! | `exit` | completes the turn, then exits 0 |
//!
//! Threads are stored under `$TMPDIR/switchyard-fake-codex/` so a new
//! process can resume them; every approval answer it receives is appended
//! to `answers-<thread>.jsonl` there (test evidence).

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use serde_json::{Value, json};

const VERSION: &str = "codex-cli 0.166.0";

fn out(v: &Value) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{v}");
    let _ = stdout.flush();
}

fn store() -> PathBuf {
    let dir = std::env::temp_dir().join("switchyard-fake-codex");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn thread_file(id: &str) -> PathBuf {
    store().join(format!("thread-{id}.json"))
}

fn record_answer(thread: &str, answer: &Value) {
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(store().join(format!("answers-{thread}.jsonl")))
    {
        let _ = writeln!(f, "{answer}");
    }
}

struct Fake {
    input: Receiver<Value>,
    cwd: String,
    thread: Option<String>,
    /// User messages of this thread (persisted for resume).
    history: Vec<String>,
    next_request: u64,
    /// Requests that arrived while a turn ran.
    backlog: Vec<Value>,
}

enum Wait {
    Answer(Value),
    Interrupted(Value),
    Closed,
}

fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

impl Fake {
    fn thread(&self) -> String {
        self.thread.clone().unwrap_or_default()
    }

    fn notify(&self, method: &str, params: Value) {
        out(&json!({ "method": method, "params": params }));
    }

    fn respond(&self, id: &Value, result: Value) {
        out(&json!({ "id": id, "result": result }));
    }

    fn error(&self, id: &Value, code: i64, message: &str) {
        out(&json!({ "id": id, "error": { "code": code, "message": message } }));
    }

    fn save(&self) {
        let _ = std::fs::write(
            thread_file(&self.thread()),
            json!({ "cwd": self.cwd, "history": self.history }).to_string(),
        );
    }

    /// Sends a server request and waits for its answer (or an interrupt).
    fn ask(&mut self, method: &str, params: Value) -> Wait {
        let id = self.next_request;
        self.next_request += 1;
        out(&json!({ "id": id, "method": method, "params": params }));
        loop {
            match self.input.recv() {
                Err(_) => return Wait::Closed,
                Ok(m) if m.get("id") == Some(&json!(id)) && m.get("method").is_none() => {
                    record_answer(&self.thread(), &json!({ "method": method, "answer": m }));
                    return Wait::Answer(m);
                }
                Ok(m)
                    if m["method"] == "turn/interrupt" && m["params"]["threadId"] != json!(self.thread()) =>
                {
                    // A sub-agent's turn: acknowledge, record, keep waiting.
                    record_answer(&self.thread(), &json!({ "child_interrupt": m["params"] }));
                    if let Some(i) = m.get("id") {
                        self.respond(i, json!({}));
                    }
                }
                Ok(m) if m["method"] == "turn/interrupt" => {
                    // Like Codex: the turn ends, then the request is resolved.
                    return Wait::Interrupted(json!({ "pending": id, "request": m }));
                }
                Ok(m) => self.backlog.push(m),
            }
        }
    }

    /// Checks for an interrupt without blocking longer than `wait`.
    fn interrupted(&mut self, wait: Duration) -> Option<Value> {
        match self.input.recv_timeout(wait) {
            Ok(m) if m["method"] == "turn/interrupt" => Some(m),
            Ok(m) => {
                self.backlog.push(m);
                None
            }
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => None,
        }
    }

    fn message(&self, turn: &str, text: &str) {
        let id = format!("msg_{}", new_id());
        let t = self.thread();
        self.notify("item/started", json!({ "threadId": t, "turnId": turn, "item": { "type": "agentMessage", "id": id, "text": "" } }));
        for chunk in text.split_inclusive(' ') {
            self.notify(
                "item/agentMessage/delta",
                json!({ "threadId": t, "turnId": turn, "itemId": id, "delta": chunk }),
            );
        }
        self.notify("item/completed", json!({ "threadId": t, "turnId": turn, "item": { "type": "agentMessage", "id": id, "text": text } }));
    }

    fn finish(&self, turn: &str, status: &str) {
        let t = self.thread();
        if status == "completed" {
            let usage = json!({ "totalTokens": 120, "inputTokens": 100, "cachedInputTokens": 40, "outputTokens": 20, "reasoningOutputTokens": 5 });
            self.notify(
                "thread/tokenUsage/updated",
                json!({ "threadId": t, "turnId": turn, "tokenUsage": { "total": usage, "last": usage } }),
            );
        }
        self.notify("turn/completed", json!({ "threadId": t, "turn": { "id": turn, "status": status, "items": [], "error": null, "durationMs": 10 } }));
    }

    fn ack_interrupt(&self, request: &Value) {
        if let Some(id) = request.get("id") {
            self.respond(id, json!({}));
        }
    }

    fn patch(&mut self, turn: &str, changes: Vec<(String, &str)>) -> Option<String> {
        let t = self.thread();
        let item = format!("exec-{}", new_id());
        let list: Vec<Value> = changes
            .iter()
            .map(|(p, kind)| {
                let path = if p.starts_with('/') { p.clone() } else { format!("{}/{}", self.cwd, p) };
                let kind = if *kind == "move" {
                    json!({ "type": "update", "move_path": format!("{}/moved.txt", self.cwd) })
                } else {
                    json!({ "type": kind })
                };
                json!({ "path": path, "kind": kind, "diff": "+fake\n" })
            })
            .collect();
        self.notify("item/started", json!({ "threadId": t, "turnId": turn, "item": { "type": "fileChange", "id": item, "changes": list, "status": "inProgress" } }));
        let answer = self.ask(
            "item/fileChange/requestApproval",
            json!({ "threadId": t, "turnId": turn, "itemId": item, "startedAtMs": 0, "reason": null, "grantRoot": null }),
        );
        let decision = match &answer {
            Wait::Answer(m) => m["result"]["decision"].as_str().unwrap_or("").to_owned(),
            Wait::Interrupted(m) => return Some(m.to_string()),
            Wait::Closed => return Some(String::new()),
        };
        let accepted = decision == "accept";
        if accepted {
            for (p, kind) in &changes {
                let path = PathBuf::from(&self.cwd).join(p);
                match *kind {
                    "add" => {
                        let _ = std::fs::write(&path, "written by fake codex\n");
                    }
                    "update" => {
                        let mut f = std::fs::OpenOptions::new().append(true).create(true).open(&path).ok();
                        if let Some(f) = f.as_mut() {
                            let _ = writeln!(f, "edited by fake codex");
                        }
                    }
                    _ => {}
                }
            }
        }
        let status = if accepted { "completed" } else { "declined" };
        self.notify("item/completed", json!({ "threadId": t, "turnId": turn, "item": { "type": "fileChange", "id": item, "changes": list, "status": status } }));
        self.notify("serverRequest/resolved", json!({ "threadId": t, "requestId": self.next_request - 1 }));
        self.message(turn, if accepted { "DONE" } else { "The change was rejected." });
        None
    }

    fn command(&mut self, turn: &str, command: &str, thread: Option<String>) -> Option<Value> {
        let t = thread.unwrap_or_else(|| self.thread());
        let item = format!("exec-{}", new_id());
        let wrapped = format!("/bin/zsh -lc '{}'", command.replace('\'', "'\\''"));
        self.notify("item/started", json!({ "threadId": t, "turnId": turn, "item": { "type": "commandExecution", "id": item, "command": wrapped, "cwd": self.cwd, "status": "inProgress", "commandActions": [{ "type": "unknown", "command": command }] } }));
        let answer = self.ask(
            "item/commandExecution/requestApproval",
            json!({ "kind": "command", "threadId": t, "turnId": turn, "itemId": item, "startedAtMs": 0, "environmentId": "local", "command": wrapped, "cwd": self.cwd, "commandActions": [{ "type": "unknown", "command": command }] }),
        );
        let accepted = match &answer {
            Wait::Answer(m) => m["result"]["decision"] == "accept",
            Wait::Interrupted(m) => return Some(m.clone()),
            Wait::Closed => return Some(Value::Null),
        };
        if accepted && let Some(file) = command.strip_prefix("touch ") {
            let _ = std::fs::write(PathBuf::from(&self.cwd).join(file.trim()), "");
        }
        let (status, output, code) =
            if accepted { ("completed", "ok\n", json!(0)) } else { ("declined", "", Value::Null) };
        self.notify("item/completed", json!({ "threadId": t, "turnId": turn, "item": { "type": "commandExecution", "id": item, "command": wrapped, "cwd": self.cwd, "status": status, "aggregatedOutput": output, "exitCode": code } }));
        self.notify("serverRequest/resolved", json!({ "threadId": t, "requestId": self.next_request - 1 }));
        if thread_is_main(&t, &self.thread()) {
            self.message(turn, if accepted { "Command finished." } else { "The command was rejected." });
        }
        None
    }

    fn turn(&mut self, id: &Value, params: &Value) {
        let text = params["input"][0]["text"].as_str().unwrap_or_default().to_owned();
        let turn = new_id();
        self.respond(
            id,
            json!({ "turn": { "id": turn, "status": "inProgress", "items": [], "error": null } }),
        );
        let t = self.thread();
        self.notify(
            "turn/started",
            json!({ "threadId": t, "turn": { "id": turn, "status": "inProgress", "items": [] } }),
        );
        let earlier = self.history.clone();
        self.history.push(text.clone());
        self.save();
        let (scenario, arg) = marker(&text);
        let mut interrupted: Option<Value> = None;
        match scenario {
            "write" => {
                if let Some(m) = self.patch(&turn, vec![(arg.unwrap_or("out.txt").to_owned(), "add")]) {
                    interrupted = serde_json::from_str(&m).ok();
                }
            }
            "edit" => {
                if let Some(m) = self.patch(&turn, vec![(arg.unwrap_or("README.md").to_owned(), "update")]) {
                    interrupted = serde_json::from_str(&m).ok();
                }
            }
            "mixed" => {
                if let Some(m) =
                    self.patch(&turn, vec![("a.txt".to_owned(), "add"), ("b.txt".to_owned(), "update")])
                {
                    interrupted = serde_json::from_str(&m).ok();
                }
            }
            "cmd" => interrupted = self.command(&turn, arg.unwrap_or("true"), None),
            "move" => {
                if let Some(m) = self.patch(&turn, vec![("README.md".to_owned(), "move")]) {
                    interrupted = serde_json::from_str(&m).ok();
                }
            }
            "newreq" => {
                let answer = self.ask(
                    "item/brandNew/requestApproval",
                    json!({ "threadId": t, "turnId": turn, "itemId": "n1" }),
                );
                if let Wait::Interrupted(m) = answer {
                    interrupted = Some(m);
                } else {
                    self.message(&turn, "Unknown request answered.");
                }
            }
            "garbage" => {
                out(&json!("not an object"));
                {
                    let mut stdout = std::io::stdout().lock();
                    let _ = writeln!(
                        stdout,
                        "{{\"id\": 77, \"method\": \"item/commandExecution/requestApproval\", \"params\": {{\"kind\""
                    );
                    let _ = writeln!(stdout, "this is not json");
                    let _ = stdout.flush();
                }
                self.notify("item/brandNewNotification", json!({ "threadId": t, "x": 1 }));
                self.notify(
                    "item/started",
                    json!({ "threadId": t, "turnId": turn, "item": { "type": "brandNewItem", "id": "z1" } }),
                );
                self.notify("item/completed", json!({ "threadId": t, "turnId": turn, "item": { "type": "brandNewItem", "id": "z1", "status": "completed" } }));
                out(&json!({ "id": 9999, "result": { "unexpected": true } }));
                self.message(&turn, "Still here.");
            }
            "crashpending" => {
                out(
                    &json!({ "id": self.next_request, "method": "item/commandExecution/requestApproval", "params": {
                    "kind": "command", "threadId": t, "turnId": turn, "itemId": "c9", "command": "/bin/zsh -lc 'touch crashed.txt'", "cwd": self.cwd } }),
                );
                std::thread::sleep(Duration::from_millis(500));
                eprintln!("fake codex: crashed with a pending approval");
                std::process::exit(3);
            }
            "dupid" => {
                for _ in 0..2 {
                    out(&json!({ "id": 42, "method": "item/commandExecution/requestApproval", "params": {
                        "kind": "command", "threadId": t, "turnId": turn, "itemId": "d1", "command": "/bin/zsh -lc 'touch dup.txt'", "cwd": self.cwd } }));
                }
                // Wait for the adapter to give up (stdin closes).
                while self.input.recv().is_ok() {}
                return;
            }
            "pending" => interrupted = self.command(&turn, "sleep 100", None),
            "subagent" => {
                let child = new_id();
                // Like Codex: the child thread's turn starts before its first request.
                self.notify(
                    "turn/started",
                    json!({ "threadId": child, "turn": { "id": format!("child-turn-{child}"), "status": "inProgress", "items": [] } }),
                );
                interrupted = self.command(&turn, "touch from-subagent.txt", Some(child));
                if interrupted.is_none() {
                    self.message(&turn, "Sub-agent finished.");
                }
            }
            "perm" => {
                let answer = self.ask(
                    "item/permissions/requestApproval",
                    json!({ "threadId": t, "turnId": turn, "itemId": "p1", "environmentId": null, "startedAtMs": 0, "cwd": self.cwd, "reason": "more access", "permissions": { "network": { "enabled": true } } }),
                );
                if let Wait::Interrupted(m) = answer {
                    interrupted = Some(m);
                } else {
                    self.message(&turn, "Permissions answered.");
                }
            }
            "unmediated" => {
                self.notify("item/started", json!({ "threadId": t, "turnId": turn, "item": { "type": "mcpToolCall", "id": "mcp1", "server": "x", "tool": "y", "status": "inProgress", "arguments": {} } }));
                self.notify("item/completed", json!({ "threadId": t, "turnId": turn, "item": { "type": "mcpToolCall", "id": "mcp1", "server": "x", "tool": "y", "status": "completed", "arguments": {} } }));
                self.message(&turn, "Called a tool.");
            }
            "slow" => {
                let item = format!("msg_{}", new_id());
                self.notify("item/started", json!({ "threadId": t, "turnId": turn, "item": { "type": "agentMessage", "id": item, "text": "" } }));
                for i in 0..60 {
                    if let Some(m) = self.interrupted(Duration::from_millis(50)) {
                        interrupted = Some(json!({ "request": m }));
                        break;
                    }
                    self.notify("item/agentMessage/delta", json!({ "threadId": t, "turnId": turn, "itemId": item, "delta": format!("chunk{i} ") }));
                }
                if interrupted.is_none() {
                    self.notify("item/completed", json!({ "threadId": t, "turnId": turn, "item": { "type": "agentMessage", "id": item, "text": "slow done" } }));
                }
            }
            "whoami" => self.message(&turn, &format!("thread {t}")),
            "remember" => self.message(&turn, &format!("earlier: {}", earlier.join(" | "))),
            "crash" => {
                self.notify(
                    "item/agentMessage/delta",
                    json!({ "threadId": t, "turnId": turn, "itemId": "m", "delta": "about to crash" }),
                );
                eprintln!("fake codex: simulated crash");
                std::process::exit(3);
            }
            _ => self.message(&turn, "Hello from fake codex."),
        }
        if let Some(i) = interrupted {
            if let Some(req) = i.get("request") {
                self.ack_interrupt(req);
            }
            self.finish(&turn, "interrupted");
            if let Some(p) = i.get("pending") {
                self.notify("serverRequest/resolved", json!({ "threadId": t, "requestId": p }));
            }
            return;
        }
        self.finish(&turn, "completed");
        if scenario == "exit" {
            std::process::exit(0);
        }
    }

    fn handle(&mut self, m: Value) {
        let Some(method) = m.get("method").and_then(Value::as_str).map(str::to_owned) else { return };
        let id = m.get("id").cloned();
        let params = m.get("params").cloned().unwrap_or(Value::Null);
        let Some(id) = id else { return }; // notifications (initialized)
        match method.as_str() {
            "initialize" => self.respond(
                &id,
                json!({ "userAgent": "fake/0.166.0", "codexHome": "/nonexistent", "platformFamily": "unix", "platformOs": "macos" }),
            ),
            "thread/start" | "thread/resume" => {
                // The adapter must always ask for the safe configuration.
                if params["approvalPolicy"] != "untrusted" || params["sandbox"] != "workspace-write" {
                    return self.error(&id, -32602, "fake codex: unexpected approval policy or sandbox");
                }
                let cwd = params["cwd"].as_str().unwrap_or_default().to_owned();
                let thread = if method == "thread/resume" {
                    let wanted = params["threadId"].as_str().unwrap_or_default().to_owned();
                    let Ok(saved) = std::fs::read_to_string(thread_file(&wanted)) else {
                        return self.error(&id, -32600, &format!("no rollout found for thread id {wanted}"));
                    };
                    let saved: Value = serde_json::from_str(&saved).unwrap_or_default();
                    self.history = saved["history"]
                        .as_array()
                        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
                        .unwrap_or_default();
                    wanted
                } else {
                    new_id()
                };
                self.cwd = cwd.clone();
                self.thread = Some(thread.clone());
                self.save();
                self.respond(
                    &id,
                    json!({ "thread": { "id": thread, "cwd": cwd }, "model": "fake-codex-model", "cwd": cwd, "approvalPolicy": "untrusted" }),
                );
                self.notify("thread/started", json!({ "thread": { "id": thread } }));
            }
            "turn/start" => {
                if self.thread.as_deref() != params["threadId"].as_str() {
                    return self.error(&id, -32600, "unknown thread");
                }
                self.turn(&id, &params);
            }
            "turn/interrupt" => self.error(&id, -32600, "no active turn"),
            _ => self.error(&id, -32601, &format!("method not found: {method}")),
        }
    }
}

fn thread_is_main(t: &str, main: &str) -> bool {
    t == main
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

fn serve() {
    if std::path::Path::new(".fake-codex-fail").exists() {
        eprintln!("fake codex: Not logged in");
        std::process::exit(1);
    }
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            match serde_json::from_str::<Value>(&line) {
                Ok(v) => {
                    if tx.send(v).is_err() {
                        break;
                    }
                }
                Err(_) => eprintln!("fake codex: invalid input line"),
            }
        }
    });
    let mut fake = Fake {
        input: rx,
        cwd: String::new(),
        thread: None,
        history: Vec::new(),
        next_request: 0,
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
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["--version"] => println!("{VERSION}"),
        ["login", "status"] => eprintln!("Logged in using ChatGPT"),
        ["app-server", "--help"] => println!(
            "Run the app server\n\nOptions:\n      --listen <URL>\n          Transport endpoint URL. Supported values: `stdio://` (default), `unix://`, `off`"
        ),
        ["app-server", ..] => serve(),
        _ => {
            eprintln!("fake codex: unsupported arguments {args:?}");
            std::process::exit(2);
        }
    }
}
