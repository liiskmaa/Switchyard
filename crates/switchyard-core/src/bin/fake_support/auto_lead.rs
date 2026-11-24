//! An adaptive lead for the fakes (`{"supervise": {"auto_lead": PLAN}}`).
//!
//! Scripted supervisors answer with a fixed directive per turn, so a test that
//! injects a fault (a failed turn, a restart, a cancelled subtask) cannot
//! expect a sensible next directive. This lead reads each checkpoint the way
//! a reasonable lead would and decides from the team's current tasks:
//!
//! - no subtasks yet: send PLAN (a directive with agents and tasks);
//! - reported work: accept it, or correct it when the turn failed, was
//!   interrupted, is blocked or the review did not approve (escalating to the
//!   user after the correction limit; after the user's answer it starts that
//!   work afresh under a new key);
//! - work the user cancelled: start it again under `<key>-again` (and what
//!   waited for it), from PLAN's definition;
//! - everything accepted: done.
//!
//! It only reads the prompt Core rendered, as a real supervisor would.

use serde_json::{Value, json};

struct Task {
    key: String,
    state: String,
    corrections: u32,
}

/// The text of a `## head` section, up to the next one.
fn section<'a>(prompt: &'a str, head: &str) -> &'a str {
    let Some(i) = prompt.find(&format!("\n## {head}")) else { return "" };
    let rest = &prompt[i + 1..];
    let body = &rest[rest.find('\n').map_or(rest.len(), |n| n + 1)..];
    &body[..body.find("\n## ").unwrap_or(body.len())]
}

fn tasks(prompt: &str) -> Vec<Task> {
    section(prompt, "Tasks in your scope")
        .lines()
        .filter_map(|l| {
            let inner = &l[l.find("(key ")? + 5..];
            let parts: Vec<&str> = inner[..inner.find(')')?].split(", ").collect();
            Some(Task {
                key: parts.first()?.to_string(),
                state: parts.get(3)?.to_string(),
                corrections: parts
                    .iter()
                    .find_map(|p| p.strip_suffix(" corrections").and_then(|n| n.parse().ok()))
                    .unwrap_or(0),
            })
        })
        .collect()
}

fn block(d: &Value) -> String {
    format!("Decided.\n\n```switchyard-org\n{d}\n```")
}

/// The plan's definition of `key` (or of the task it is again of), renamed.
fn again(plan: &Value, key: &str, deps: impl Fn(&str) -> String) -> Option<Value> {
    let base = key.trim_end_matches("-again");
    let mut t = plan["tasks"].as_array()?.iter().find(|t| t["key"] == base)?.clone();
    let new = format!("{key}-again");
    t["key"] = json!(new);
    if let Some(d) = t["depends_on"].as_array() {
        t["depends_on"] = json!(d.iter().filter_map(Value::as_str).map(&deps).collect::<Vec<_>>());
    }
    if t["work"] == "implementation" {
        t["continues"] = json!(key);
    }
    Some(t)
}

pub fn decide(prompt: &str, plan: &Value) -> String {
    let all = tasks(prompt);
    let sub: Vec<&Task> = all.iter().filter(|t| t.key != "objective").collect();
    if sub.is_empty() {
        return block(plan);
    }
    let items: Vec<&str> = section(prompt, "What needs your attention now").lines().collect();
    let answered = items.iter().any(|i| i.contains("The user answered"));
    let state_of = |k: &str| all.iter().find(|t| t.key == k).map(|t| t.state.as_str());
    let mut assessments = Vec::new();
    let mut new_tasks = Vec::new();
    let mut escalate = None;
    // Tasks to run again under a new key (cancelled by the user, or given up on).
    let mut redo: Vec<String> = sub
        .iter()
        .filter(|t| t.state == "cancelled" && state_of(&format!("{}-again", t.key)).is_none())
        .map(|t| t.key.clone())
        .collect();
    for t in sub.iter().filter(|t| t.state == "reported") {
        let item = items.iter().find(|i| i.starts_with(&format!("- Task {}:", t.key))).copied().unwrap_or("");
        let bad = [
            "ended without a usable result",
            "was interrupted",
            "is blocked",
            "asks:",
            "challenges its instructions",
            "reviewed (changes_requested",
            "reviewed (rejected",
            "reviewed (blocked",
        ]
        .iter()
        .any(|b| item.contains(b));
        if !bad {
            assessments.push(json!({ "task": t.key, "verdict": "accept", "evidence": "Read the changes." }));
        } else if t.corrections < 2 {
            assessments.push(json!({ "task": t.key, "verdict": "correct",
                "instructions": "Finish the task and end with the report block." }));
        } else if answered {
            assessments
                .push(json!({ "task": t.key, "verdict": "cancel", "instructions": "Starting it afresh." }));
            redo.push(t.key.clone());
        } else {
            escalate = Some(json!({ "to": "user",
                "question": format!("Task {} keeps failing after 2 corrections: start it afresh?", t.key) }));
        }
    }
    // Work waiting for redone work waits for its new version instead.
    let mut i = 0;
    while i < redo.len() {
        let gone = redo[i].clone();
        for t in &sub {
            if t.state == "waiting" && !redo.contains(&t.key) {
                let deps_on = plan["tasks"].as_array().is_some_and(|ts| {
                    ts.iter().any(|p| {
                        p["key"] == t.key.trim_end_matches("-again")
                            && p["depends_on"].as_array().is_some_and(|d| {
                                d.iter().any(|x| x.as_str() == Some(gone.trim_end_matches("-again")))
                            })
                    })
                });
                if deps_on {
                    assessments.push(json!({ "task": t.key, "verdict": "cancel",
                        "instructions": format!("{gone} is done again first.") }));
                    redo.push(t.key.clone());
                }
            }
        }
        i += 1;
    }
    for k in &redo {
        let mapped = |d: &str| {
            let live = sub.iter().map(|t| t.key.as_str()).rfind(|x| x.trim_end_matches("-again") == d);
            let cur = live.unwrap_or(d).to_owned();
            if redo.contains(&cur) { format!("{cur}-again") } else { cur }
        };
        if let Some(t) = again(plan, k, mapped) {
            new_tasks.push(t);
        }
    }
    if let Some(e) = escalate {
        return block(&json!({ "summary": "A task keeps failing.", "escalate": e }));
    }
    if !assessments.is_empty() || !new_tasks.is_empty() {
        return block(
            &json!({ "summary": "Assessed the reported work.", "assessments": assessments, "tasks": new_tasks }),
        );
    }
    let open = sub.iter().any(|t| !matches!(t.state.as_str(), "accepted" | "cancelled"));
    if !open {
        return block(&json!({ "summary": "Every task is accepted. Outstanding: none.", "done": true }));
    }
    block(&json!({ "summary": "Waiting for the work in progress." }))
}
