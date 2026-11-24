//! The `switchyard-org` block a supervisor ends its turn with.
//!
//! A directive is what a supervisor *decides within its authority*: the
//! agents and subtasks it wants, its assessment of reported work, decisions
//! and findings for the shared context, an escalation, or that its scope is
//! done. Core validates it against the organization and its limits before
//! anything is created; a directive never grants permissions, approves a tool
//! request or delivers anything.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{AgentKind, ContextKind, ContextStatus, WorkKind};

/// Largest directive accepted (the fenced block's JSON). Agent text is
/// journaled up to 8 KiB per message, so this leaves room for a short preface.
pub const MAX_DIRECTIVE_BYTES: usize = 6 * 1024;
pub const FENCE: &str = "```switchyard-org";
const MAX_ITEMS: usize = 12;
const MAX_TEXT: usize = 2000;
const MAX_KEY: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentRequest {
    /// Short identifier, unique in the organization (`backend`, `docs-writer`).
    pub key: String,
    pub title: String,
    pub kind: AgentKind,
    /// The supervisor this agent reports to; defaults to the author.
    #[serde(default)]
    pub reports_to: Option<String>,
    pub responsibility: String,
    /// Workspace-relative directories or files this agent may change.
    #[serde(default)]
    pub scope: Vec<String>,
    /// What the work needs (`coding`, `review`, `documentation`, …).
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// A preferred provider/model; Core still checks it (availability,
    /// permission routing, limits) and records why it chose what it chose.
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRequest {
    /// Short identifier, unique in the organization.
    pub key: String,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    pub work: WorkKind,
    /// Agent key of the owner (a specialist; a supervisor for `coordination`).
    pub owner: String,
    /// Workspace-relative directories or files the work may change.
    #[serde(default)]
    pub scope: Vec<String>,
    #[serde(default)]
    pub acceptance: Vec<String>,
    /// Keys of tasks that must be accepted first.
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// The coordination task this belongs to; defaults to the one under review.
    #[serde(default)]
    pub parent: Option<String>,
    /// The existing task this one finishes or fixes. After the first plan,
    /// work that continues nothing is new scope: it waits for the user's
    /// approval (ADR 0046).
    #[serde(default)]
    pub continues: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// The deliverable meets its acceptance criteria.
    Accept,
    /// Send it back with instructions.
    Correct,
    /// Give it to another agent in the supervisor's scope.
    Reassign,
    /// Drop it, saying why.
    Cancel,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Correct => "correct",
            Self::Reassign => "reassign",
            Self::Cancel => "cancel",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assessment {
    pub task: String,
    pub verdict: Verdict,
    /// What to change (`correct`, `reassign`) or why (`cancel`).
    #[serde(default)]
    pub instructions: Option<String>,
    /// New owner (`reassign`).
    #[serde(default)]
    pub to: Option<String>,
    /// What the verdict rests on (files read, Core's verified evidence, …).
    #[serde(default)]
    pub evidence: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextNote {
    pub kind: ContextKind,
    pub text: String,
    /// `claim` (default), `proposal`, or `decided` (decisions only). Only
    /// Core records `verified`.
    #[serde(default)]
    pub status: Option<ContextStatus>,
    /// Task key the note is about.
    #[serde(default)]
    pub task: Option<String>,
    /// Sequence number of an earlier entry this one replaces.
    #[serde(default)]
    pub supersedes: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalateTo {
    Supervisor,
    User,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Escalation {
    pub to: EscalateTo,
    pub question: String,
}

/// A validated supervisor directive.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Directive {
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<AgentRequest>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<TaskRequest>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assessments: Vec<Assessment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context: Vec<ContextNote>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalate: Option<Escalation>,
    /// The supervisor's whole scope is finished (every subtask accepted or cancelled).
    #[serde(default)]
    pub done: bool,
    /// Top-level fields the schema does not know (kept, ignored).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unexpected_fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DirectiveError {
    #[error("the turn ended without a switchyard-org block")]
    Missing,
    #[error("the switchyard-org block is not closed")]
    Unterminated,
    #[error("the directive is larger than {MAX_DIRECTIVE_BYTES} bytes")]
    TooLarge,
    #[error("the directive is not valid: {0}")]
    Invalid(String),
}

const KNOWN: &[&str] = &["summary", "agents", "tasks", "assessments", "context", "escalate", "done"];

fn invalid(m: impl Into<String>) -> DirectiveError {
    DirectiveError::Invalid(m.into())
}

/// A key: lowercase letters, digits and `-`, 1–40 characters.
pub fn valid_key(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= MAX_KEY
        && k.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !k.starts_with('-')
}

/// A workspace-relative path: no absolute paths, no `..`, no `~`.
pub fn valid_scope(p: &str) -> bool {
    let p = p.trim();
    !p.is_empty()
        && p.len() <= 300
        && !p.starts_with('/')
        && !p.starts_with('~')
        && !p.contains('\\')
        && !p.contains('\0')
        && p.split('/').all(|c| c != "..")
}

fn text(field: &str, s: &str, max: usize) -> Result<(), DirectiveError> {
    if s.trim().is_empty() {
        return Err(invalid(format!("{field} is empty")));
    }
    if s.chars().count() > max {
        return Err(invalid(format!("{field} is longer than {max} characters")));
    }
    Ok(())
}

/// Finds the last `switchyard-org` block in `text` and validates its shape.
/// Whether it fits the organization is decided by [`super::decide`].
pub fn parse_directive(text_in: &str) -> Result<Directive, DirectiveError> {
    let start = text_in.rfind(FENCE).ok_or(DirectiveError::Missing)?;
    let after = &text_in[start + FENCE.len()..];
    let body_start = after.find('\n').ok_or(DirectiveError::Unterminated)? + 1;
    let body = &after[body_start..];
    let end = body.find("```").ok_or(DirectiveError::Unterminated)?;
    let json = body[..end].trim();
    if json.len() > MAX_DIRECTIVE_BYTES {
        return Err(DirectiveError::TooLarge);
    }
    let value: Value = serde_json::from_str(json).map_err(|e| invalid(e.to_string()))?;
    let obj = value.as_object().ok_or_else(|| invalid("not a JSON object"))?;
    let mut unexpected: Vec<String> = obj.keys().filter(|k| !KNOWN.contains(&k.as_str())).cloned().collect();
    unexpected.sort();
    let mut known = obj.clone();
    known.retain(|k, _| KNOWN.contains(&k.as_str()));
    let mut d: Directive =
        serde_json::from_value(Value::Object(known)).map_err(|e| invalid(e.to_string()))?;
    d.unexpected_fields = unexpected;
    validate(&d)?;
    Ok(d)
}

fn validate(d: &Directive) -> Result<(), DirectiveError> {
    text("summary", &d.summary, MAX_TEXT)?;
    for (what, n) in [
        ("agents", d.agents.len()),
        ("tasks", d.tasks.len()),
        ("assessments", d.assessments.len()),
        ("context notes", d.context.len()),
    ] {
        if n > MAX_ITEMS {
            return Err(invalid(format!("more than {MAX_ITEMS} {what}")));
        }
    }
    for a in &d.agents {
        if !valid_key(&a.key) {
            return Err(invalid(format!(
                "agent key {:?} must be 1–40 lowercase letters, digits or -",
                a.key
            )));
        }
        text("agent title", &a.title, 120)?;
        text("agent responsibility", &a.responsibility, MAX_TEXT)?;
        if let Some(r) = &a.reports_to
            && !valid_key(r)
        {
            return Err(invalid(format!("reports_to {r:?} is not an agent key")));
        }
        if let Some(bad) = a.scope.iter().find(|p| !valid_scope(p)) {
            return Err(invalid(format!("scope {bad:?} must be a relative path inside the workspace")));
        }
        if a.capabilities.len() > 8 || a.capabilities.iter().any(|c| !valid_key(c)) {
            return Err(invalid("capabilities must be up to 8 short lowercase names"));
        }
    }
    for t in &d.tasks {
        if !valid_key(&t.key) {
            return Err(invalid(format!("task key {:?} must be 1–40 lowercase letters, digits or -", t.key)));
        }
        text("task title", &t.title, 200)?;
        if let Some(desc) = &t.description {
            text("task description", desc, MAX_TEXT * 2)?;
        }
        if !valid_key(&t.owner) {
            return Err(invalid(format!("task {} has no valid owner", t.key)));
        }
        if let Some(bad) = t.scope.iter().find(|p| !valid_scope(p)) {
            return Err(invalid(format!("scope {bad:?} must be a relative path inside the workspace")));
        }
        if t.acceptance.len() > 20 {
            return Err(invalid("more than 20 acceptance criteria"));
        }
        for c in &t.acceptance {
            text("acceptance criterion", c, 500)?;
        }
        if t.depends_on.iter().any(|k| !valid_key(k) || *k == t.key) {
            return Err(invalid(format!("task {} has an invalid dependency", t.key)));
        }
    }
    for a in &d.assessments {
        if !valid_key(&a.task) {
            return Err(invalid("an assessment names no valid task"));
        }
        match a.verdict {
            Verdict::Correct | Verdict::Cancel => {
                text("instructions", a.instructions.as_deref().unwrap_or(""), MAX_TEXT)?
            }
            Verdict::Reassign => {
                if a.to.as_deref().is_none_or(|t| !valid_key(t)) {
                    return Err(invalid("a reassignment needs \"to\": the new owner's key"));
                }
            }
            Verdict::Accept => {}
        }
        if let Some(e) = &a.evidence {
            text("evidence", e, MAX_TEXT)?;
        }
    }
    for n in &d.context {
        text("context text", &n.text, MAX_TEXT)?;
        match n.status {
            Some(ContextStatus::Verified) => {
                return Err(invalid("only Switchyard marks context as verified; use claim"));
            }
            Some(ContextStatus::Superseded) => {
                return Err(invalid("use \"supersedes\" to replace an entry"));
            }
            Some(ContextStatus::Decided) if n.kind != ContextKind::Decision => {
                return Err(invalid("only a decision can be decided"));
            }
            _ => {}
        }
        if matches!(n.kind, ContextKind::Objective | ContextKind::Criterion | ContextKind::Observation) {
            return Err(invalid(format!("{} entries are recorded by Switchyard", n.kind.as_str())));
        }
    }
    if let Some(e) = &d.escalate {
        text("escalation question", &e.question, MAX_TEXT)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(json: &str) -> String {
        format!("I looked at the work.\n\n```switchyard-org\n{json}\n```\n")
    }

    #[test]
    fn parses_a_plan() {
        let d = parse_directive(&block(
            r#"{"summary": "Split it", "agents": [{"key": "impl", "title": "Implementer", "kind": "specialist",
            "responsibility": "Write the code", "scope": ["src"], "capabilities": ["coding"]}],
            "tasks": [{"key": "code", "title": "Write it", "work": "implementation", "owner": "impl",
            "scope": ["src"], "acceptance": ["It works"]}], "extra": 1}"#,
        ))
        .unwrap();
        assert_eq!((d.agents.len(), d.tasks.len()), (1, 1));
        assert_eq!(d.unexpected_fields, vec!["extra"]);
        assert_eq!(d.tasks[0].work, WorkKind::Implementation);
    }

    #[test]
    fn the_last_block_counts_and_shape_errors_are_reported() {
        let two = format!(
            "{}{}",
            block(r#"{"summary": "first"}"#),
            block(r#"{"summary": "second", "done": true}"#)
        );
        let d = parse_directive(&two).unwrap();
        assert!(d.done && d.summary == "second");
        assert_eq!(parse_directive("no block"), Err(DirectiveError::Missing));
        assert_eq!(parse_directive("```switchyard-org\n{}"), Err(DirectiveError::Unterminated));
        for bad in [
            r#"{"summary": ""}"#,
            r#"{"summary": "x", "agents": [{"key": "Bad Key", "title": "t", "kind": "specialist", "responsibility": "r"}]}"#,
            r#"{"summary": "x", "tasks": [{"key": "a", "title": "t", "work": "implementation", "owner": "b", "scope": ["../etc"]}]}"#,
            r#"{"summary": "x", "tasks": [{"key": "a", "title": "t", "work": "implementation", "owner": "b", "scope": ["/abs"]}]}"#,
            r#"{"summary": "x", "tasks": [{"key": "a", "title": "t", "work": "implementation", "owner": "b", "depends_on": ["a"]}]}"#,
            r#"{"summary": "x", "assessments": [{"task": "a", "verdict": "correct"}]}"#,
            r#"{"summary": "x", "assessments": [{"task": "a", "verdict": "reassign"}]}"#,
            r#"{"summary": "x", "context": [{"kind": "finding", "text": "t", "status": "verified"}]}"#,
            r#"{"summary": "x", "context": [{"kind": "finding", "text": "t", "status": "decided"}]}"#,
            r#"{"summary": "x", "context": [{"kind": "observation", "text": "t"}]}"#,
            r#"{"summary": "x", "agents": [{"key": "a", "title": "t", "kind": "boss", "responsibility": "r"}]}"#,
            r#"{"summary": "x", "agents": [{"key": "a", "title": "t", "kind": "specialist", "responsibility": "r", "power": 1}]}"#,
        ] {
            assert!(matches!(parse_directive(&block(bad)), Err(DirectiveError::Invalid(_))), "{bad}");
        }
        let big = format!(r#"{{"summary": "{}"}}"#, "x".repeat(MAX_DIRECTIVE_BYTES));
        assert_eq!(parse_directive(&block(&big)), Err(DirectiveError::TooLarge));
    }

    #[test]
    fn keys_and_scopes() {
        assert!(
            valid_key("docs-writer")
                && valid_key("a1")
                && !valid_key("")
                && !valid_key("-a")
                && !valid_key("A")
        );
        assert!(valid_scope("src/") && valid_scope("docs/api.md") && valid_scope("."));
        assert!(
            !valid_scope("../x") && !valid_scope("a/../../b") && !valid_scope("/etc") && !valid_scope("~/x")
        );
    }
}
