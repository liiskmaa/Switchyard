//! Evaluation semantics, scope isolation and adversarial cases for the pure
//! policy engine (ADR 0016).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use switchyard_policy::{
    Conditions, Decision, EvaluationInput, Operation, OperationCategory, PathFact, ReasonCode, RequestScope,
    Rule, RuleScope, evaluate,
};
use uuid::Uuid;

use Decision::{Allow, Ask, Deny};
use OperationCategory::*;

struct World {
    project: Uuid,
    other_project: Uuid,
    workspace: Uuid,
    other_workspace: Uuid,
    session: Uuid,
}

fn world() -> World {
    World {
        project: Uuid::now_v7(),
        other_project: Uuid::now_v7(),
        workspace: Uuid::now_v7(),
        other_workspace: Uuid::now_v7(),
        session: Uuid::now_v7(),
    }
}

fn inside(rel: &str) -> PathFact {
    PathFact::Inside { lexical: rel.into(), resolved: rel.into() }
}

fn op(category: OperationCategory, tool: &str, paths: &[&str], command: Option<&str>) -> Operation {
    Operation {
        adapter: "claude_code".into(),
        tool: tool.into(),
        category,
        paths: paths.iter().map(|p| (*p).to_owned()).collect(),
        command: command.map(Into::into),
        background: false,
        unexpected_fields: Vec::new(),
        problem: None,
    }
}

impl World {
    fn scope(&self) -> RequestScope {
        RequestScope {
            project_id: self.project,
            session_id: self.session,
            workspace_id: Some(self.workspace),
            workspace_project_id: Some(self.project),
            review_session: false,
        }
    }

    fn file(&self, category: OperationCategory, rel: &str) -> EvaluationInput {
        EvaluationInput {
            operation: op(category, "Tool", &[&format!("/ws/{rel}")], None),
            paths: vec![inside(rel)],
            scope: self.scope(),
        }
    }

    fn shell(&self, command: &str) -> EvaluationInput {
        EvaluationInput {
            operation: op(Shell, "Bash", &[], Some(command)),
            paths: vec![],
            scope: self.scope(),
        }
    }

    fn rule(&self, category: OperationCategory, decision: Decision) -> Rule {
        Rule {
            id: Uuid::now_v7(),
            revision: 1,
            project_id: self.project,
            workspace_id: None,
            category,
            decision,
            conditions: Conditions::default(),
            description: format!("{decision} {category}"),
            enabled: true,
            archived: false,
            created_at_ms: 1,
        }
    }
}

fn with_path(mut r: Rule, prefix: &str) -> Rule {
    r.conditions.path_prefix = Some(prefix.into());
    r
}

fn with_command(mut r: Rule, command: &str) -> Rule {
    r.conditions.command = Some(command.into());
    r
}

fn in_workspace(mut r: Rule, ws: Uuid) -> Rule {
    r.workspace_id = Some(ws);
    r
}

#[test]
fn built_in_defaults() {
    let w = world();
    let read = evaluate(&w.file(FileRead, "src/main.rs"), &[], 7);
    assert_eq!((read.decision, read.reason), (Allow, ReasonCode::Default));
    assert_eq!(read.policy_revision, 7);
    assert!(read.deciding_rule.is_none() && read.matched_rules.is_empty());
    assert_eq!(evaluate(&w.file(Search, ""), &[], 0).decision, Allow);
    for category in [FileWrite, FileEdit, FileDelete] {
        let e = evaluate(&w.file(category, "a.txt"), &[], 0);
        assert_eq!((e.decision, e.reason), (Ask, ReasonCode::Default), "{category}");
        assert!(e.requires_human());
    }
    assert_eq!(evaluate(&w.shell("ls"), &[], 0).decision, Ask, "bash asks by default");
    for category in [Network, ExternalTool] {
        let input =
            EvaluationInput { operation: op(category, "X", &[], None), paths: vec![], scope: w.scope() };
        let e = evaluate(&input, &[], 0);
        assert_eq!((e.decision, e.reason), (Deny, ReasonCode::Default), "blocked by default");
        // A rule can make it ask (never allow).
        assert_eq!(evaluate(&input, &[w.rule(category, Ask)], 0).decision, Ask);
    }
}

#[test]
fn unknown_and_malformed_operations_are_denied_whatever_the_rules() {
    let w = world();
    // Even a (stored, invalid) allow rule for `unknown` cannot help.
    let mut allow_unknown = w.rule(Unknown, Allow);
    allow_unknown.conditions = Conditions::default();
    let unknown =
        EvaluationInput { operation: op(Unknown, "Teleport", &[], None), paths: vec![], scope: w.scope() };
    let e = evaluate(&unknown, std::slice::from_ref(&allow_unknown), 0);
    assert_eq!((e.decision, e.reason), (Deny, ReasonCode::SystemRestriction));
    assert_eq!(e.restrictions[0].code, "unknown_operation");
    assert!(e.explanation.contains("Teleport"));

    // A request with a classification problem is denied even if its
    // category has an allow rule.
    let mut malformed = w.file(FileRead, "a.txt");
    malformed.operation.problem = Some("file_path is not a string".into());
    let e = evaluate(&malformed, &[w.rule(FileRead, Allow)], 0);
    assert_eq!(e.decision, Deny);
    assert_eq!(e.restrictions[0].code, "unclassified");

    // A path operation without path facts is malformed.
    let mut no_facts = w.file(FileEdit, "a.txt");
    no_facts.paths.clear();
    assert_eq!(evaluate(&no_facts, &[w.rule(FileEdit, Allow)], 0).decision, Deny);
}

#[test]
fn explicit_rules_and_precedence() {
    let w = world();
    let input = w.file(FileEdit, "docs/readme.md");
    let allow = with_path(w.rule(FileEdit, Allow), "docs");
    let ask = w.rule(FileEdit, Ask);
    let deny = with_path(w.rule(FileEdit, Deny), "docs/readme.md");

    let e = evaluate(&input, std::slice::from_ref(&allow), 3);
    assert_eq!((e.decision, e.reason), (Allow, ReasonCode::Rule));
    assert_eq!(e.deciding_rule.as_ref().map(|r| (r.id, r.revision)), Some((allow.id, 1)));

    assert_eq!(evaluate(&input, &[allow.clone(), ask.clone()], 0).decision, Ask, "ask beats allow");
    let e = evaluate(&input, &[allow.clone(), ask.clone(), deny.clone()], 0);
    assert_eq!(e.decision, Deny, "deny beats ask and allow");
    assert_eq!(e.deciding_rule.map(|r| r.id), Some(deny.id));
    assert_eq!(
        e.matched_rules.iter().map(|r| r.decision).collect::<Vec<_>>(),
        [Deny, Ask, Allow],
        "every matching rule is reported, most restrictive first"
    );
    // A deny rule for a read beats the built-in read allow.
    let e = evaluate(&w.file(FileRead, "secrets/key"), &[with_path(w.rule(FileRead, Deny), "secrets")], 0);
    assert_eq!(e.decision, Deny);
    // An ask rule turns the read default into a prompt.
    assert_eq!(evaluate(&w.file(FileRead, "x"), &[w.rule(FileRead, Ask)], 0).decision, Ask);
}

#[test]
fn conflicting_rules_are_deterministic_regardless_of_order() {
    let w = world();
    let input = w.file(FileWrite, "out/a.txt");
    let mut rules = vec![
        with_path(w.rule(FileWrite, Allow), "out"),
        in_workspace(with_path(w.rule(FileWrite, Deny), "out"), w.workspace),
        w.rule(FileWrite, Ask),
        with_path(w.rule(FileWrite, Deny), "out/a.txt"),
    ];
    rules[3].created_at_ms = 0;
    let first = evaluate(&input, &rules, 1);
    for rotation in 0..rules.len() {
        let mut r = rules.clone();
        r.rotate_left(rotation);
        r.reverse();
        let e = evaluate(&input, &r, 1);
        assert_eq!(e, first, "rotation {rotation}");
    }
    assert_eq!(first.decision, Deny);
    // Same decision: the workspace rule is cited before the older project rule.
    assert_eq!(first.deciding_rule.map(|r| r.scope), Some(RuleScope::Workspace));
}

#[test]
fn system_restrictions_win_over_configured_allow() {
    let w = world();
    let allow_reads = w.rule(FileRead, Allow);
    // Outside the workspace.
    let outside = EvaluationInput {
        operation: op(FileRead, "Read", &["/etc/passwd"], None),
        paths: vec![PathFact::Outside],
        scope: w.scope(),
    };
    let e = evaluate(&outside, std::slice::from_ref(&allow_reads), 0);
    assert_eq!((e.decision, e.reason), (Ask, ReasonCode::Capped));
    assert!(e.restrictions.iter().any(|r| r.code == "path_outside_workspace"));
    // Unresolvable path.
    let unresolved =
        EvaluationInput { paths: vec![PathFact::Unresolved { reason: "loop".into() }], ..outside.clone() };
    assert_eq!(evaluate(&unresolved, std::slice::from_ref(&allow_reads), 0).decision, Ask);
    // Git metadata.
    let e = evaluate(&w.file(FileWrite, ".git/config"), &[w.rule(FileWrite, Allow)], 0);
    assert_eq!(e.decision, Ask);
    assert!(e.restrictions.iter().any(|r| r.code == "git_metadata"));
    let e = evaluate(&w.file(FileEdit, "sub/.GIT/hooks/pre-commit"), &[w.rule(FileEdit, Allow)], 0);
    assert_eq!(e.decision, Ask, "case-insensitive .git");
    // Unknown input fields.
    let mut extra = w.file(FileEdit, "a.txt");
    extra.operation.unexpected_fields = vec!["new_mode".into()];
    assert_eq!(evaluate(&extra, &[w.rule(FileEdit, Allow)], 0).decision, Ask);
    // Scope mismatch and missing workspace are denials.
    let mut mismatch = w.file(FileRead, "a");
    mismatch.scope.workspace_project_id = Some(w.other_project);
    assert_eq!(evaluate(&mismatch, std::slice::from_ref(&allow_reads), 0).decision, Deny);
    let mut none = w.file(FileRead, "a");
    none.scope.workspace_id = None;
    assert_eq!(evaluate(&none, &[allow_reads], 0).decision, Deny);
}

#[test]
fn scope_limits_rules_to_their_project_and_workspace() {
    let w = world();
    let input = w.file(FileEdit, "a.txt");
    let mut other_project = w.rule(FileEdit, Allow);
    other_project.project_id = w.other_project;
    let other_ws = in_workspace(w.rule(FileEdit, Allow), w.other_workspace);
    let e = evaluate(&input, &[other_project.clone(), other_ws.clone()], 0);
    assert_eq!((e.decision, e.reason), (Ask, ReasonCode::Default), "foreign rules never apply");
    assert!(e.matched_rules.is_empty());
    // Foreign deny rules do not apply either.
    let mut foreign_deny = w.rule(FileRead, Deny);
    foreign_deny.project_id = w.other_project;
    let other_ws_deny = in_workspace(w.rule(FileRead, Deny), w.other_workspace);
    assert_eq!(evaluate(&w.file(FileRead, "a"), &[foreign_deny, other_ws_deny], 0).decision, Allow);

    // Project rules apply to every workspace of the project.
    let project_allow = w.rule(FileEdit, Allow);
    assert_eq!(evaluate(&input, std::slice::from_ref(&project_allow), 0).decision, Allow);
    // A workspace rule applies to its workspace only…
    let ws_allow = in_workspace(w.rule(FileEdit, Allow), w.workspace);
    assert_eq!(evaluate(&input, std::slice::from_ref(&ws_allow), 0).decision, Allow);
    let mut elsewhere = input.clone();
    elsewhere.scope.workspace_id = Some(w.other_workspace);
    assert_eq!(evaluate(&elsewhere, std::slice::from_ref(&ws_allow), 0).decision, Ask);
    // …and cannot loosen a project restriction.
    let project_ask = w.rule(FileEdit, Ask);
    assert_eq!(evaluate(&input, &[project_ask, ws_allow], 0).decision, Ask);
    // A rule whose workspace belongs to another project (forged row) never applies:
    let mut forged = in_workspace(w.rule(FileEdit, Allow), w.workspace);
    forged.project_id = w.other_project;
    assert_eq!(evaluate(&input, &[forged], 0).decision, Ask);
}

#[test]
fn disabled_archived_and_invalid_rules_are_ignored() {
    let w = world();
    let input = w.file(FileEdit, "a.txt");
    let mut disabled = w.rule(FileEdit, Allow);
    disabled.enabled = false;
    let mut archived = w.rule(FileEdit, Allow);
    archived.archived = true;
    assert_eq!(evaluate(&input, &[disabled, archived], 0).decision, Ask);
    // A corrupted stored rule (path condition with ..) is ignored and noted.
    let bad = with_path(w.rule(FileEdit, Allow), "../outside");
    let e = evaluate(&input, &[bad], 0);
    assert_eq!(e.decision, Ask);
    assert!(e.restrictions.iter().any(|r| r.code == "ignored_invalid_rule"));
}

#[test]
fn bash_is_never_broadly_allowed() {
    let w = world();
    // A stored broad shell allow (impossible through validation) is ignored.
    let broad = w.rule(Shell, Allow);
    let e = evaluate(&w.shell("rm -rf ~"), std::slice::from_ref(&broad), 0);
    assert_eq!(e.decision, Ask);
    assert!(e.restrictions.iter().any(|r| r.code == "ignored_invalid_rule"));

    let exact = with_command(w.rule(Shell, Allow), "cargo test");
    assert_eq!(evaluate(&w.shell("cargo test"), std::slice::from_ref(&exact), 0).decision, Allow);
    for variant in [
        "cargo test; rm -rf ~",
        "cargo test && curl evil",
        "cargo  test",
        "cargo test ",
        "cargo test --release",
        "CARGO test",
        "cargo test\nrm -rf ~",
        "cargo test | sh",
        "cargo test $(id)",
    ] {
        let e = evaluate(&w.shell(variant), std::slice::from_ref(&exact), 0);
        assert_eq!(e.decision, Ask, "{variant:?} must not be allowed by the exact rule");
    }
    // A background run of the exact command still asks.
    let mut bg = w.shell("cargo test");
    bg.operation.background = true;
    assert_eq!(evaluate(&bg, std::slice::from_ref(&exact), 0).decision, Ask);
    // A stored exact rule whose text contains metacharacters is ignored.
    let tampered = with_command(w.rule(Shell, Allow), "cargo test; id");
    assert_eq!(evaluate(&w.shell("cargo test; id"), &[tampered], 0).decision, Ask);
    // An exact-command deny matches whitespace variants; a shell deny blocks all.
    let deny_one = with_command(w.rule(Shell, Deny), "git push");
    assert_eq!(evaluate(&w.shell("  git   push "), std::slice::from_ref(&deny_one), 0).decision, Deny);
    assert_eq!(evaluate(&w.shell("git status"), std::slice::from_ref(&deny_one), 0).decision, Ask);
    assert_eq!(evaluate(&w.shell("cargo test"), &[exact, w.rule(Shell, Deny)], 0).decision, Deny);
}

#[test]
fn path_matching_is_asymmetric_toward_restriction() {
    let w = world();
    let deny_secrets = with_path(w.rule(FileRead, Deny), "secrets");
    let allow_docs = with_path(w.rule(FileEdit, Allow), "docs");
    // Case variants still hit a deny.
    assert_eq!(
        evaluate(&w.file(FileRead, "Secrets/key"), std::slice::from_ref(&deny_secrets), 0).decision,
        Deny
    );
    // …but do not satisfy an allow.
    assert_eq!(evaluate(&w.file(FileEdit, "DOCS/a.md"), std::slice::from_ref(&allow_docs), 0).decision, Ask);
    // Components, not string prefixes.
    assert_eq!(
        evaluate(&w.file(FileRead, "secretsx/a"), std::slice::from_ref(&deny_secrets), 0).decision,
        Allow
    );
    assert_eq!(evaluate(&w.file(FileEdit, "docsx/a.md"), std::slice::from_ref(&allow_docs), 0).decision, Ask);
    // A symlink inside docs that resolves into secrets: deny matches the
    // resolved path, allow requires both.
    let via_link = EvaluationInput {
        operation: op(FileRead, "Read", &["/ws/docs/link/key"], None),
        paths: vec![PathFact::Inside { lexical: "docs/link/key".into(), resolved: "secrets/key".into() }],
        scope: w.scope(),
    };
    assert_eq!(evaluate(&via_link, std::slice::from_ref(&deny_secrets), 0).decision, Deny);
    let edit_link =
        EvaluationInput { operation: op(FileEdit, "Edit", &["/ws/docs/link/key"], None), ..via_link.clone() };
    assert_eq!(evaluate(&edit_link, std::slice::from_ref(&allow_docs), 0).decision, Ask);
    // Multiple targets: deny if any matches, allow only if all match.
    let two = EvaluationInput {
        operation: op(FileEdit, "X", &["/ws/docs/a", "/ws/src/b"], None),
        paths: vec![inside("docs/a"), inside("src/b")],
        scope: w.scope(),
    };
    assert_eq!(evaluate(&two, std::slice::from_ref(&allow_docs), 0).decision, Ask);
    let deny_src = with_path(w.rule(FileEdit, Deny), "src");
    assert_eq!(evaluate(&two, &[deny_src], 0).decision, Deny);
}

#[test]
fn explanations_name_the_rule_and_survive_serialization() {
    let w = world();
    let mut rule = with_path(w.rule(FileEdit, Allow), "docs");
    rule.description = "Docs are fine".into();
    rule.revision = 4;
    let e = evaluate(&w.file(FileEdit, "docs/x.md"), std::slice::from_ref(&rule), 12);
    assert!(e.explanation.contains("Docs are fine"), "{}", e.explanation);
    let json = serde_json::to_value(&e).unwrap();
    assert_eq!(json["decision"], "allow");
    assert_eq!(json["reason"], "rule");
    assert_eq!(json["deciding_rule"]["revision"], 4);
    assert_eq!(json["policy_revision"], 12);
    let back: switchyard_policy::Evaluation = serde_json::from_value(json).unwrap();
    assert_eq!(back, e);
}

/// ADR 0027: a review session cannot change files, whatever the rules say,
/// and its commands always need a person.
#[test]
fn review_sessions_cannot_change_files_and_commands_always_ask() {
    let w = world();
    let review = |mut i: EvaluationInput| {
        i.scope.review_session = true;
        i
    };
    let allow_all = [
        w.rule(FileWrite, Allow),
        w.rule(FileEdit, Allow),
        w.rule(FileDelete, Allow),
        with_command(w.rule(Shell, Allow), "cargo test"),
    ];
    for category in [FileWrite, FileEdit, FileDelete] {
        let e = evaluate(&review(w.file(category, "src/a.rs")), &allow_all, 0);
        assert_eq!((e.decision, e.reason), (Deny, ReasonCode::SystemRestriction), "{category}");
        assert!(e.explanation.contains("review session"));
    }
    let shell = evaluate(&review(w.shell("cargo test")), &allow_all, 0);
    assert_eq!((shell.decision, shell.reason), (Ask, ReasonCode::Capped), "an exact rule cannot auto-allow");
    assert!(shell.restrictions.iter().any(|r| r.code == "review_session"));
    let read = evaluate(&review(w.file(FileRead, "src/a.rs")), &[], 0);
    assert_eq!(read.decision, Allow, "reading stays allowed");
    // Outside review sessions the same rules still apply.
    assert_eq!(evaluate(&w.file(FileWrite, "src/a.rs"), &allow_all, 0).decision, Allow);
    assert_eq!(evaluate(&w.shell("cargo test"), &allow_all, 0).decision, Allow);
}

#[test]
fn evaluations_recorded_before_review_sessions_still_load() {
    let w = world();
    let mut v = serde_json::to_value(w.file(FileRead, "a")).unwrap();
    v["scope"].as_object_mut().unwrap().remove("review_session");
    let back: EvaluationInput = serde_json::from_value(v).unwrap();
    assert!(!back.scope.review_session);
}
