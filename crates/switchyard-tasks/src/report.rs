//! The `switchyard-report` block an agent ends a turn with (ADR 0025).
//!
//! The task context tells the agent to finish its final message with exactly
//! one fenced block:
//!
//! ````text
//! ```switchyard-report
//! { "type": "findings", "summary": "…", … }
//! ```
//! ````
//!
//! Core parses and validates it. A report is what the agent *says*: findings,
//! a proposal, a review verdict, a question. It never authorizes anything.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::model::{ReviewResult, Role, Severity, TaskKind};

/// Largest report accepted (the fenced block's JSON).
pub const MAX_REPORT_BYTES: usize = 6 * 1024;
const MAX_SUMMARY: usize = 2000;
const MAX_TEXT: usize = 4000;
const MAX_FINDINGS: usize = 50;
const MAX_SUBTASKS: usize = 10;
const FENCE: &str = "```switchyard-report";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportKind {
    /// Investigation results.
    Findings,
    /// A plan, optionally with proposed subtasks.
    Proposal,
    /// Implementation done.
    Implementation,
    /// A review verdict.
    Review,
    /// The agent cannot continue.
    Blocker,
    /// The agent needs an answer from the operator.
    Question,
    /// Reasoning's decision on a review's findings (workflow, ADR 0038):
    /// `decision` is `fix` (send them to implementation) or `dispute` (send
    /// the reasoning back to the reviewer).
    Triage,
}

/// Which kind of test a command runs (ADR 0039). Unit, integration and
/// regression tests are always required; browser tests when the change
/// touches the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestKind {
    Unit,
    Integration,
    Regression,
    Browser,
}

impl TestKind {
    pub const REQUIRED: [TestKind; 3] = [TestKind::Unit, TestKind::Integration, TestKind::Regression];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unit => "unit",
            Self::Integration => "integration",
            Self::Regression => "regression",
            Self::Browser => "browser",
        }
    }
}

/// A test command the implementer names (ADR 0039). Core runs it, without a
/// shell, after the policy allows it; the agent never runs it on its behalf.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestCommand {
    pub kind: TestKind,
    /// The program and its arguments.
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Seconds; Core caps it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u32>,
}

/// Most test commands one implementation report may name.
pub const MAX_TESTS: usize = 20;

/// What reasoning decided about a review's findings (ADR 0038).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriageDecision {
    /// The findings should be addressed: implementation fixes them.
    Fix,
    /// The findings should not be addressed as reported: the reviewer gets
    /// the reasoning and reviews again.
    Dispute,
}

impl TriageDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fix => "fix",
            Self::Dispute => "dispute",
        }
    }
}

impl ReportKind {
    /// Report types each role may send.
    pub fn allowed_for(self, role: Role) -> bool {
        use ReportKind::*;
        match role {
            Role::Investigate => matches!(self, Findings | Proposal | Triage | Blocker | Question),
            Role::Implement => matches!(self, Implementation | Proposal | Blocker | Question),
            Role::Review => matches!(self, Review | Blocker | Question),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportFinding {
    pub severity: Severity,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

/// A dependency of a proposed subtask: an earlier subtask of the same report
/// (by index) or an existing task (by id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SubtaskDependency {
    Index(usize),
    Task(Uuid),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposedSubtask {
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default = "default_kind")]
    pub kind: TaskKind,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub depends_on: Vec<SubtaskDependency>,
}

fn default_kind() -> TaskKind {
    TaskKind::General
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceCheck {
    pub criterion: String,
    pub met: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A validated report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    #[serde(rename = "type")]
    pub kind: ReportKind,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<ReportFinding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proposed_subtasks: Vec<ProposedSubtask>,
    /// Review verdict (only for `review`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ReviewResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acceptance: Vec<AcceptanceCheck>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    /// The test commands to run before review (only for `implementation`, ADR 0039).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<TestCommand>,
    /// Reasoning's decision (only for `triage`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<TriageDecision>,
    /// Top-level fields the schema does not know (kept for the operator).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unexpected_fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReportError {
    #[error("the turn ended without a switchyard-report block")]
    Missing,
    #[error("the switchyard-report block is not closed")]
    Unterminated,
    #[error("the report is larger than {MAX_REPORT_BYTES} bytes")]
    TooLarge,
    #[error("the report is not valid: {0}")]
    Invalid(String),
    #[error("a {role} assignment cannot send a {kind:?} report")]
    WrongKind { role: Role, kind: ReportKind },
}

const KNOWN: &[&str] = &[
    "type",
    "summary",
    "details",
    "findings",
    "proposed_subtasks",
    "result",
    "acceptance",
    "changed_files",
    "question",
    "decision",
    "tests",
];

/// Finds the last `switchyard-report` block in `text` and validates it for
/// an assignment with `role`.
pub fn parse_report(text: &str, role: Role) -> Result<Report, ReportError> {
    let start = text.rfind(FENCE).ok_or(ReportError::Missing)?;
    let after = &text[start + FENCE.len()..];
    let body_start = after.find('\n').ok_or(ReportError::Unterminated)? + 1;
    let body = &after[body_start..];
    let end = body.find("```").ok_or(ReportError::Unterminated)?;
    let json = body[..end].trim();
    if json.len() > MAX_REPORT_BYTES {
        return Err(ReportError::TooLarge);
    }
    let value: Value = serde_json::from_str(json).map_err(|e| ReportError::Invalid(e.to_string()))?;
    let obj = value.as_object().ok_or_else(|| ReportError::Invalid("not a JSON object".into()))?;
    let mut unexpected: Vec<String> = obj.keys().filter(|k| !KNOWN.contains(&k.as_str())).cloned().collect();
    unexpected.sort();
    let mut known = obj.clone();
    known.retain(|k, _| KNOWN.contains(&k.as_str()));
    let mut report: Report =
        serde_json::from_value(Value::Object(known)).map_err(|e| ReportError::Invalid(e.to_string()))?;
    report.unexpected_fields = unexpected;
    validate(&report, role)?;
    Ok(report)
}

fn too_long(field: &str, s: &str, max: usize) -> Result<(), ReportError> {
    if s.chars().count() > max {
        return Err(ReportError::Invalid(format!("{field} is longer than {max} characters")));
    }
    Ok(())
}

fn validate(r: &Report, role: Role) -> Result<(), ReportError> {
    if !r.kind.allowed_for(role) {
        return Err(ReportError::WrongKind { role, kind: r.kind });
    }
    if r.summary.trim().is_empty() {
        return Err(ReportError::Invalid("summary is empty".into()));
    }
    too_long("summary", &r.summary, MAX_SUMMARY)?;
    if let Some(d) = &r.details {
        too_long("details", d, MAX_TEXT)?;
    }
    if r.findings.len() > MAX_FINDINGS {
        return Err(ReportError::Invalid(format!("more than {MAX_FINDINGS} findings")));
    }
    for f in &r.findings {
        if f.title.trim().is_empty() {
            return Err(ReportError::Invalid("a finding has no title".into()));
        }
        too_long("finding title", &f.title, 300)?;
        for t in [&f.explanation, &f.suggestion].into_iter().flatten() {
            too_long("finding text", t, MAX_TEXT)?;
        }
    }
    match (r.kind, r.result) {
        (ReportKind::Review, None) => return Err(ReportError::Invalid("a review needs a result".into())),
        (k, Some(_)) if k != ReportKind::Review => {
            return Err(ReportError::Invalid("only a review has a result".into()));
        }
        _ => {}
    }
    match (r.kind, r.decision) {
        (ReportKind::Triage, None) => {
            return Err(ReportError::Invalid("a triage report needs a decision: fix or dispute".into()));
        }
        (k, Some(_)) if k != ReportKind::Triage => {
            return Err(ReportError::Invalid("only a triage report has a decision".into()));
        }
        _ => {}
    }
    if !r.tests.is_empty() && r.kind != ReportKind::Implementation {
        return Err(ReportError::Invalid("only an implementation report names test commands".into()));
    }
    if r.tests.len() > MAX_TESTS {
        return Err(ReportError::Invalid(format!("more than {MAX_TESTS} test commands")));
    }
    for (i, t) in r.tests.iter().enumerate() {
        if t.command.is_empty() || t.command.iter().any(|a| a.is_empty() || a.contains('\0')) {
            return Err(ReportError::Invalid(format!("test {i} has an empty command or argument")));
        }
        if t.command.len() > 64 || t.command.iter().any(|a| a.chars().count() > 1000) {
            return Err(ReportError::Invalid(format!("test {i}'s command is too long")));
        }
        if let Some(n) = &t.name {
            too_long("test name", n, 200)?;
        }
    }
    if r.kind == ReportKind::Triage && !r.findings.is_empty() {
        return Err(ReportError::Invalid(
            "a triage report has no findings of its own: put the reasoning in summary and details".into(),
        ));
    }
    if !r.proposed_subtasks.is_empty() && r.kind != ReportKind::Proposal {
        return Err(ReportError::Invalid("only a proposal can propose subtasks".into()));
    }
    if r.proposed_subtasks.len() > MAX_SUBTASKS {
        return Err(ReportError::Invalid(format!("more than {MAX_SUBTASKS} proposed subtasks")));
    }
    for (i, s) in r.proposed_subtasks.iter().enumerate() {
        if s.title.trim().is_empty() {
            return Err(ReportError::Invalid(format!("proposed subtask {i} has no title")));
        }
        too_long("subtask title", &s.title, 200)?;
        for d in &s.depends_on {
            if let SubtaskDependency::Index(j) = d
                && *j >= i
            {
                return Err(ReportError::Invalid(format!(
                    "proposed subtask {i} depends on subtask {j}, which is not an earlier one"
                )));
            }
        }
    }
    if r.kind == ReportKind::Question && r.question.as_deref().is_none_or(|q| q.trim().is_empty()) {
        return Err(ReportError::Invalid("a question report needs a question".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrap(json: &str) -> String {
        format!("Here is my work.\n\n```switchyard-report\n{json}\n```\n")
    }

    #[test]
    fn a_review_report_is_parsed() {
        let r = parse_report(
            &wrap(r#"{"type":"review","summary":"One problem","result":"changes_requested",
                "findings":[{"severity":"high","title":"Off by one","path":"src/a.rs","line":3,"suggestion":"use <="}],
                "acceptance":[{"criterion":"tests pass","met":false}]}"#),
            Role::Review,
        )
        .unwrap();
        assert_eq!(r.result, Some(ReviewResult::ChangesRequested));
        assert_eq!(r.findings[0].severity, Severity::High);
        assert_eq!(r.findings[0].line, Some(3));
        assert!(!r.acceptance[0].met);
    }

    #[test]
    fn the_last_block_wins_and_unknown_fields_are_kept_aside() {
        let text = format!(
            "{}\nlater:\n{}",
            wrap(r#"{"type":"findings","summary":"old"}"#),
            wrap(r#"{"type":"findings","summary":"new","mood":"good"}"#)
        );
        let r = parse_report(&text, Role::Investigate).unwrap();
        assert_eq!(r.summary, "new");
        assert_eq!(r.unexpected_fields, vec!["mood"]);
    }

    #[test]
    fn proposals_carry_validated_subtasks() {
        let id = Uuid::now_v7();
        let r = parse_report(
            &wrap(&format!(
                r#"{{"type":"proposal","summary":"Plan","proposed_subtasks":[
                {{"title":"Implement parser","kind":"implementation","acceptance_criteria":["tests"]}},
                {{"title":"Review parser","depends_on":[0,"{id}"]}}]}}"#
            )),
            Role::Investigate,
        )
        .unwrap();
        assert_eq!(r.proposed_subtasks.len(), 2);
        assert_eq!(
            r.proposed_subtasks[1].depends_on,
            vec![SubtaskDependency::Index(0), SubtaskDependency::Task(id)]
        );
        let forward =
            wrap(r#"{"type":"proposal","summary":"x","proposed_subtasks":[{"title":"a","depends_on":[0]}]}"#);
        assert!(matches!(parse_report(&forward, Role::Investigate), Err(ReportError::Invalid(_))));
    }

    #[test]
    fn invalid_reports_are_refused() {
        assert_eq!(parse_report("no block here", Role::Review), Err(ReportError::Missing));
        assert_eq!(
            parse_report("```switchyard-report\n{\"type\":", Role::Review),
            Err(ReportError::Unterminated)
        );
        let cases = [
            (r#"{"type":"review","summary":"x"}"#, Role::Review),
            (r#"{"type":"findings","summary":"x","result":"approved"}"#, Role::Investigate),
            (r#"{"type":"findings","summary":"  "}"#, Role::Investigate),
            (r#"{"type":"teleport","summary":"x"}"#, Role::Investigate),
            (r#"{"type":"question","summary":"x"}"#, Role::Investigate),
            (r#"[1,2]"#, Role::Investigate),
            (r#"{"type":"findings","summary":"x","proposed_subtasks":[{"title":"a"}]}"#, Role::Investigate),
        ];
        for (json, role) in cases {
            assert!(parse_report(&wrap(json), role).is_err(), "{json}");
        }
        let big = format!(r#"{{"type":"findings","summary":"{}"}}"#, "x".repeat(MAX_REPORT_BYTES));
        assert_eq!(parse_report(&wrap(&big), Role::Investigate), Err(ReportError::TooLarge));
    }

    #[test]
    fn a_role_can_only_send_its_kinds_of_report() {
        let impl_report = wrap(r#"{"type":"implementation","summary":"done"}"#);
        assert!(parse_report(&impl_report, Role::Implement).is_ok());
        assert!(matches!(parse_report(&impl_report, Role::Review), Err(ReportError::WrongKind { .. })));
        let review = wrap(r#"{"type":"review","summary":"ok","result":"approved"}"#);
        assert!(matches!(parse_report(&review, Role::Implement), Err(ReportError::WrongKind { .. })));
    }

    #[test]
    fn reasoning_reports_a_triage_decision() {
        let r = parse_report(
            &wrap(r#"{"type":"triage","decision":"dispute","summary":"Checked upstream","details":"why"}"#),
            Role::Investigate,
        )
        .unwrap();
        assert_eq!((r.kind, r.decision), (ReportKind::Triage, Some(TriageDecision::Dispute)));
        for bad in [
            r#"{"type":"triage","summary":"no decision"}"#,
            r#"{"type":"findings","decision":"fix","summary":"decision on findings"}"#,
            r#"{"type":"triage","decision":"maybe","summary":"x"}"#,
            r#"{"type":"triage","decision":"fix","summary":"x","findings":[{"severity":"low","title":"t"}]}"#,
        ] {
            assert!(parse_report(&wrap(bad), Role::Investigate).is_err(), "{bad}");
        }
        assert!(matches!(
            parse_report(&wrap(r#"{"type":"triage","decision":"fix","summary":"x"}"#), Role::Review),
            Err(ReportError::WrongKind { .. })
        ));
    }

    #[test]
    fn implementation_reports_name_their_test_commands() {
        let r = parse_report(
            &wrap(
                r#"{"type":"implementation","summary":"done","tests":[
                {"kind":"unit","command":["cargo","test","--lib"]},
                {"kind":"browser","command":["npx","playwright","test"],"name":"UI","timeout_secs":600}]}"#,
            ),
            Role::Implement,
        )
        .unwrap();
        assert_eq!(r.tests.len(), 2);
        assert_eq!((r.tests[1].kind, r.tests[1].timeout_secs), (TestKind::Browser, Some(600)));
        for bad in [
            r#"{"type":"implementation","summary":"x","tests":[{"kind":"unit","command":[]}]}"#,
            r#"{"type":"implementation","summary":"x","tests":[{"kind":"unit","command":["a",""]}]}"#,
            r#"{"type":"implementation","summary":"x","tests":[{"kind":"smoke","command":["a"]}]}"#,
        ] {
            assert!(parse_report(&wrap(bad), Role::Implement).is_err(), "{bad}");
        }
        let findings = r#"{"type":"findings","summary":"x","tests":[{"kind":"unit","command":["a"]}]}"#;
        assert!(parse_report(&wrap(findings), Role::Investigate).is_err());
    }
}
