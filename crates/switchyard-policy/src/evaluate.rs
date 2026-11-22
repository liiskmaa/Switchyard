//! Deterministic, I/O-free policy evaluation (ADR 0016).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::{
    Decision, EvaluationInput, OperationCategory, PathFact, Rule, RuleScope, SYSTEM_POLICY_VERSION,
};
use crate::validate::{normalize_path_prefix, validate_command, validate_stored};

/// Why the final decision was reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonCode {
    /// A system restriction denied the operation.
    SystemRestriction,
    /// A configured rule decided.
    Rule,
    /// No configured rule matched; the built-in default decided.
    Default,
    /// An `allow` (from a rule, a default or auto mode) was capped at `ask`
    /// by a system restriction.
    Capped,
    /// No configured rule matched and the project's auto mode allowed it
    /// (ADR 0037).
    AutoMode,
}

impl ReasonCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SystemRestriction => "system_restriction",
            Self::Rule => "rule",
            Self::Default => "default",
            Self::Capped => "capped",
            Self::AutoMode => "auto_mode",
        }
    }
}

/// A rule as it was when it was evaluated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleRef {
    pub id: Uuid,
    pub revision: u32,
    pub scope: RuleScope,
    pub workspace_id: Option<Uuid>,
    pub decision: Decision,
    pub description: String,
}

impl RuleRef {
    fn of(rule: &Rule) -> Self {
        Self {
            id: rule.id,
            revision: rule.revision,
            scope: rule.scope(),
            workspace_id: rule.workspace_id,
            decision: rule.decision,
            description: rule.description.clone(),
        }
    }
}

/// A system restriction that applied (denied, capped, or a rule ignored).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Restriction {
    pub code: String,
    pub message: String,
}

/// The explained result of one evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evaluation {
    pub decision: Decision,
    pub reason: ReasonCode,
    /// One or two sentences for people (UI, agent denial message).
    pub explanation: String,
    /// The rule that decided (`reason` = `rule`, or `capped` from a rule).
    pub deciding_rule: Option<RuleRef>,
    /// Every configured rule that matched, most restrictive first.
    pub matched_rules: Vec<RuleRef>,
    pub restrictions: Vec<Restriction>,
    /// Global policy revision the rules were read at.
    pub policy_revision: i64,
    pub system_version: u32,
}

impl Evaluation {
    pub fn requires_human(&self) -> bool {
        self.decision == Decision::Ask
    }
}

fn restriction(code: &str, message: impl Into<String>) -> Restriction {
    Restriction { code: code.into(), message: message.into() }
}

/// System denial helper.
fn system_deny(revision: i64, code: &str, message: String) -> Evaluation {
    Evaluation {
        decision: Decision::Deny,
        reason: ReasonCode::SystemRestriction,
        explanation: format!("Blocked by Switchyard: {message}."),
        deciding_rule: None,
        matched_rules: Vec::new(),
        restrictions: vec![restriction(code, message)],
        policy_revision: revision,
        system_version: SYSTEM_POLICY_VERSION,
    }
}

/// Splits a relative path into components.
fn components(path: &str) -> impl Iterator<Item = &str> {
    path.split('/').filter(|c| !c.is_empty())
}

/// `path` equals `prefix` or lies below it, compared by whole components.
fn under(path: &str, prefix: &str, case_sensitive: bool) -> bool {
    let mut p = components(path);
    for want in components(prefix) {
        match p.next() {
            Some(have) if case_sensitive && have == want => {}
            Some(have) if !case_sensitive && have.eq_ignore_ascii_case(want) => {}
            _ => return false,
        }
    }
    true
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether `rule`'s conditions match. Restrictive rules (deny/ask) match
/// liberally; `allow` rules match strictly (ADR 0016).
fn conditions_match(rule: &Rule, input: &EvaluationInput) -> bool {
    let restrictive = rule.decision != Decision::Allow;
    if let Some(prefix) = &rule.conditions.path_prefix {
        let prefix = normalize_path_prefix(prefix);
        let fact_matches = |fact: &PathFact| match fact {
            PathFact::Inside { lexical, resolved } if restrictive => {
                under(lexical, &prefix, false) || under(resolved, &prefix, false)
            }
            PathFact::Inside { lexical, resolved } => {
                under(lexical, &prefix, true) && under(resolved, &prefix, true)
            }
            PathFact::Outside | PathFact::Unresolved { .. } => false,
        };
        let matched = if input.paths.is_empty() {
            false
        } else if restrictive {
            input.paths.iter().any(fact_matches)
        } else {
            input.paths.iter().all(fact_matches)
        };
        if !matched {
            return false;
        }
    }
    if let Some(command) = &rule.conditions.command {
        let Some(requested) = &input.operation.command else { return false };
        let matched =
            if restrictive { collapse_ws(requested) == collapse_ws(command) } else { requested == command };
        if !matched {
            return false;
        }
    }
    true
}

fn is_git_metadata(fact: &PathFact) -> bool {
    match fact {
        PathFact::Inside { lexical, resolved } => {
            [lexical, resolved].iter().any(|p| components(p).any(|c| c.eq_ignore_ascii_case(".git")))
        }
        _ => false,
    }
}

/// Restrictions that cap an `allow` at `ask`.
fn caps(input: &EvaluationInput, deciding: Option<&Rule>, auto: bool) -> Vec<Restriction> {
    let op = &input.operation;
    let mut out = Vec::new();
    if op.category.is_path_category() {
        for (i, fact) in input.paths.iter().enumerate() {
            let path = op.paths.get(i).map_or("the workspace", String::as_str);
            match fact {
                PathFact::Inside { .. } => {}
                PathFact::Outside => out
                    .push(restriction("path_outside_workspace", format!("{path} is outside the workspace"))),
                PathFact::Unresolved { reason } => out.push(restriction(
                    "path_unresolved",
                    format!("cannot establish where {path} points ({reason})"),
                )),
            }
        }
        if op.category.is_mutation() && input.paths.iter().any(is_git_metadata) {
            out.push(restriction("git_metadata", "the target is Git metadata (.git)"));
        }
    }
    if !op.unexpected_fields.is_empty() {
        out.push(restriction(
            "unexpected_fields",
            format!(
                "the request has input fields Switchyard does not know: {}",
                op.unexpected_fields.join(", ")
            ),
        ));
    }
    if op.category == OperationCategory::Shell && input.scope.review_session && !auto {
        out.push(restriction(
            "review_session",
            "this is a review session: commands are never allowed automatically, and an approved command could still change files",
        ));
    }
    if op.category == OperationCategory::Shell {
        // Auto mode allows background commands too (ADR 0042).
        if op.background && !auto {
            out.push(restriction("background_command", "the command would keep running in the background"));
        }
        // Auto mode never releases work by itself: commits, pushes, publishing
        // and deployments always ask (ADR 0042). An exact-command rule the
        // operator wrote still decides for its command.
        if auto
            && exact_rule_absent(deciding)
            && let Some(what) = op.command.as_deref().and_then(crate::release::release_action)
        {
            out.push(restriction(
                "release_action",
                format!("{what} commits, pushes, publishes or deploys, and auto mode leaves that to you"),
            ));
        }
        let exact = deciding.and_then(|r| r.conditions.command.as_deref());
        let ok = match (exact, op.command.as_deref()) {
            (Some(rule), Some(requested)) => rule == requested && validate_command(requested).is_ok(),
            // Auto mode allows any foreground command (ADR 0037).
            (None, Some(_)) => auto,
            _ => false,
        };
        if !ok {
            out.push(restriction(
                "shell_not_exact",
                "shell commands are only allowed automatically by an exact-command rule",
            ));
        }
    }
    out
}

fn exact_rule_absent(deciding: Option<&Rule>) -> bool {
    deciding.and_then(|r| r.conditions.command.as_deref()).is_none()
}

/// Auto mode (ADR 0037, ADR 0042) replaces the `ask` default of file changes
/// and shell commands with `allow`, including shell commands of review
/// sessions (their file changes stay denied, and Core fingerprints a review's
/// workspace). It never overrides a configured rule, and its `allow` is still
/// capped: outside the workspace, Git metadata, unknown input fields, and
/// commits, pushes, publishing and deployments.
fn auto_mode_applies(input: &EvaluationInput) -> bool {
    let category = input.operation.category;
    input.scope.auto_mode
        && (category == OperationCategory::Shell || (category.is_mutation() && !input.scope.review_session))
}

/// Evaluates one request against the configured rules.
///
/// `rules` may contain rules of any scope; only those that apply to the
/// request's project and workspace are considered. `policy_revision` is the
/// global revision the rules were read at.
pub fn evaluate(input: &EvaluationInput, rules: &[Rule], policy_revision: i64) -> Evaluation {
    let op = &input.operation;
    let scope = &input.scope;

    // 1. System restrictions that deny.
    if let Some(problem) = &op.problem {
        return system_deny(
            policy_revision,
            "unclassified",
            format!("the request could not be classified: {problem}"),
        );
    }
    if op.category == OperationCategory::Unknown {
        return system_deny(
            policy_revision,
            "unknown_operation",
            format!(
                "{} is not a recognized operation",
                if op.tool.is_empty() { "this tool" } else { &op.tool }
            ),
        );
    }
    let Some(workspace_id) = scope.workspace_id else {
        return system_deny(policy_revision, "no_workspace", "the session has no workspace".into());
    };
    if scope.workspace_project_id != Some(scope.project_id) {
        return system_deny(
            policy_revision,
            "scope_mismatch",
            "the session's workspace does not belong to its project".into(),
        );
    }
    if scope.review_session && op.category.is_mutation() {
        return system_deny(policy_revision, "review_session", "a review session cannot change files".into());
    }
    if op.category.is_path_category() && (input.paths.is_empty() || input.paths.len() < op.paths.len()) {
        return system_deny(policy_revision, "malformed", "the request has no resolvable target path".into());
    }

    // 2. Applicable, matching configured rules.
    let mut restrictions = Vec::new();
    let mut matched: Vec<&Rule> = Vec::new();
    for rule in rules {
        if !rule.enabled
            || rule.archived
            || rule.project_id != scope.project_id
            || rule.workspace_id.is_some_and(|w| w != workspace_id)
            || rule.category != op.category
        {
            continue;
        }
        if let Err(e) = validate_stored(rule) {
            restrictions
                .push(restriction("ignored_invalid_rule", format!("rule {} was ignored: {e}", rule.id)));
            continue;
        }
        if conditions_match(rule, input) {
            matched.push(rule);
        }
    }
    // Most restrictive first; then workspace before project; then oldest.
    matched.sort_by(|a, b| {
        b.decision
            .restrictiveness()
            .cmp(&a.decision.restrictiveness())
            .then_with(|| b.workspace_id.is_some().cmp(&a.workspace_id.is_some()))
            .then_with(|| a.created_at_ms.cmp(&b.created_at_ms))
            .then_with(|| a.id.cmp(&b.id))
    });
    let deciding = matched.first().copied();
    let matched_rules: Vec<RuleRef> = matched.iter().map(|r| RuleRef::of(r)).collect();

    // 3./4. Decision from the most restrictive rule, else the default.
    let (mut decision, mut reason, mut explanation) = match deciding {
        Some(rule) => {
            let verb = match rule.decision {
                Decision::Allow => "allows",
                Decision::Ask => "requires approval for",
                Decision::Deny => "blocks",
            };
            let scope_text = if rule.workspace_id.is_some() { "Workspace rule" } else { "Project rule" };
            (
                rule.decision,
                ReasonCode::Rule,
                format!("{scope_text} \u{201c}{}\u{201d} {verb} this.", rule.description.trim()),
            )
        }
        None if auto_mode_applies(input) => (
            Decision::Allow,
            ReasonCode::AutoMode,
            format!(
                "Auto mode is on for this project: {} inside the workspace is allowed.",
                op.category.phrase()
            ),
        ),
        None => {
            let d = op.category.default_decision();
            let text = match d {
                Decision::Allow => {
                    format!("Built-in default: {} inside the workspace is allowed.", op.category.phrase())
                }
                Decision::Ask => format!("Built-in default: {} requires approval.", op.category.phrase()),
                Decision::Deny => format!("Built-in default: {} is blocked.", op.category.phrase()),
            };
            (d, ReasonCode::Default, text)
        }
    };

    // 5. Caps: an allow becomes ask.
    if decision == Decision::Allow {
        let capped = caps(input, deciding, reason == ReasonCode::AutoMode);
        if !capped.is_empty() {
            let why = capped.iter().map(|c| c.message.as_str()).collect::<Vec<_>>().join("; ");
            explanation = format!("{explanation} Approval is still required: {why}.");
            decision = Decision::Ask;
            reason = ReasonCode::Capped;
            restrictions.extend(capped);
        }
    }

    Evaluation {
        decision,
        reason,
        explanation,
        deciding_rule: deciding.map(RuleRef::of),
        matched_rules,
        restrictions,
        policy_revision,
        system_version: SYSTEM_POLICY_VERSION,
    }
}

/// The compiled-in restrictions, for display (`policies.effective`).
pub fn system_restrictions() -> Vec<Restriction> {
    vec![
        restriction(
            "unknown_operation",
            "Unrecognized tools, malformed requests and tools Switchyard did not enable are blocked.",
        ),
        restriction("scope", "Requests are evaluated only for the session's own project and workspace."),
        restriction(
            "path_outside_workspace",
            "Nothing outside the session's workspace is allowed automatically.",
        ),
        restriction("git_metadata", "Changes to Git metadata (.git) are never allowed automatically."),
        restriction(
            "shell_not_exact",
            "Shell commands are allowed automatically only by an exact, plain command rule, or by the project's auto mode; without auto mode, background commands always ask.",
        ),
        restriction("unexpected_fields", "Requests with input fields Switchyard does not know always ask."),
        restriction(
            "auto_mode",
            "Auto mode allows file changes and shell commands inside the workspace, background commands and review sessions' commands included; configured rules and the restrictions above still apply.",
        ),
        restriction(
            "release_action",
            "Auto mode never commits, pushes, publishes or deploys by itself: git commit, git push, package publishing, deployments and releases always ask.",
        ),
        restriction(
            "review_session",
            "Review sessions cannot change files; without auto mode, their shell commands always ask.",
        ),
        restriction("fail_closed", "If a request cannot be evaluated or recorded, it is denied."),
    ]
}
