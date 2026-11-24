//! Task scenario shared by the fake CLIs (`[fake:report:HEX]`).
//!
//! HEX is hex-encoded JSON keyed by assignment role: `{"implement": STEPS,
//! "review": STEPS, ...}`, where STEPS is one step or a list of steps, one per
//! turn of that role (the first turn answers the task context; later turns
//! answer operator notes). A step is `{"tool": "SCENARIO", "report": {...}}` (or
//! `"tools": [...]`, run in order)
//! or `{"text": "verbatim final message"}` or `{"fail": "reason"}` (the fake Claude ends the
//! turn as failed, saying nothing, like a usage limit) or `{"crash": true}` (the fake Claude
//! exits mid-turn); a role may instead be `{"auto_lead": PLAN, "faults": {"N": STEP}}`, an
//! adaptive supervisor (`auto_lead.rs`) whose turn N plays STEP instead; an optional top-level `"linger_ms"`
//! keeps the process alive that long after its input closes; `tool` uses the fake's own
//! scenario syntax (`write:FILE`, `cmd:COMMAND`, `tool:NAME|JSON`, ...) and runs
//! before the final message. The role and task id are read from the task
//! context in the prompt, exactly as Core rendered it. The turn count per task
//! and role is kept in a file, so a resumed session (a new process) continues
//! with the next step. The task supervisor's turns (ADR 0041) use the role
//! `supervisor`, a team's goal check (ADR 0046) the role `goal`.

use serde_json::Value;

#[path = "auto_lead.rs"]
mod auto_lead;

/// One turn of a task scenario.
pub struct Step {
    /// Tools run in order before the final message (`"tool"` or `"tools": [...]`).
    pub tools: Vec<String>,
    pub text: String,
    /// The turn fails with this reason and no message (`{"fail": "..."}`); only
    /// the fake Claude plays it.
    #[allow(dead_code)]
    pub fail: Option<String>,
    /// The process exits mid-turn (`{"crash": true}`), like a fatal CLI error;
    /// only the fake Claude plays it.
    #[allow(dead_code)]
    pub crash: bool,
}

/// The scenario of the task this process works on, kept across turns.
#[derive(Default)]
pub struct TaskScript {
    spec: Option<Value>,
    role: Option<String>,
    task_id: Option<String>,
    /// The prompt of the current turn (the adaptive lead reads it).
    prompt: String,
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// A string field of the task context in `prompt`, if it is one.
fn context_field(prompt: &str, field: &str) -> Option<String> {
    let ctx = &prompt[prompt.find("SWITCHYARD TASK CONTEXT")?..];
    let key = format!("\"{field}\": \"");
    let rest = &ctx[ctx.find(&key)? + key.len()..];
    Some(rest[..rest.find('"')?].to_owned())
}

/// Returns the number of earlier turns for `key` and counts this one.
fn count_turn(key: &str) -> usize {
    let dir = std::env::temp_dir().join("switchyard-fake-report");
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join(key);
    let n = std::fs::read_to_string(&file).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    let _ = std::fs::write(&file, (n + 1).to_string());
    n
}

impl TaskScript {
    /// Takes up a new task context (`arg` is the marker argument).
    pub fn start(&mut self, prompt: &str, arg: Option<&str>) {
        let spec = arg.and_then(decode_hex).and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        *self = Self {
            spec,
            role: context_field(prompt, "role"),
            task_id: context_field(prompt, "task_id"),
            prompt: prompt.to_owned(),
        };
        // The task supervisor's prompt (ADR 0041) has no task context: its role is `supervisor`.
        let side_role = if prompt.starts_with("SWITCHYARD TASK SUPERVISOR") {
            Some("supervisor")
        } else if prompt.starts_with("SWITCHYARD GOAL CHECK") {
            Some("goal")
        } else {
            None
        };
        if let (None, Some(role)) = (&self.role, side_role) {
            self.role = Some(role.into());
            self.task_id = prompt.find("Task id: ").map(|i| {
                prompt[i + 9..].chars().take_while(|c| c.is_ascii_hexdigit() || *c == '-').collect()
            });
        }
    }

    pub fn active(&self) -> bool {
        self.spec.is_some()
    }

    /// `"linger_ms"` in the scenario: how long the process keeps running after
    /// its input closes (a slow exit, to test races with stopping). Only the
    /// fake Claude implements it.
    #[allow(dead_code)]
    pub fn linger(&self) -> Option<std::time::Duration> {
        self.spec.as_ref()?.get("linger_ms")?.as_u64().map(std::time::Duration::from_millis)
    }

    /// The next turn of this role, if the scenario has one.
    pub fn next(&mut self) -> Option<Step> {
        let role = self.role.as_deref()?;
        let steps = self.spec.as_ref()?.get(role)?;
        let turn = count_turn(&format!("{}-{role}", self.task_id.as_deref()?));
        // The adaptive lead answers every turn; `faults` replaces given turns
        // (by index) with a scripted step, which may add tools before the
        // lead's answer or end the turn otherwise.
        let auto = steps.get("auto_lead");
        let step = match (steps, auto) {
            (_, Some(_)) => {
                steps.get("faults").and_then(|f| f.get(turn.to_string())).cloned().unwrap_or(Value::Null)
            }
            (Value::Array(list), _) => list.get(turn)?.clone(),
            (other, _) if turn == 0 => other.clone(),
            _ => return None,
        };
        let mut tools: Vec<String> =
            step.get("tool").and_then(Value::as_str).map(str::to_owned).into_iter().collect();
        if let Some(list) = step.get("tools").and_then(Value::as_array) {
            tools.extend(list.iter().filter_map(Value::as_str).map(str::to_owned));
        }
        let text = match (step.get("text"), step.get("report"), auto) {
            (Some(Value::String(t)), _, _) => t.clone(),
            (_, Some(report), _) => format!("Work summary.\n\n```switchyard-report\n{report}\n```"),
            (_, _, Some(plan)) => auto_lead::decide(&self.prompt, plan),
            _ => "No report in this step.".to_owned(),
        };
        let fail = step.get("fail").and_then(Value::as_str).map(str::to_owned);
        let crash = step.get("crash").and_then(Value::as_bool).unwrap_or(false);
        Some(Step { tools, text, fail, crash })
    }
}
