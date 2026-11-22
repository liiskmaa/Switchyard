#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Approval policies end to end (ADRs 0016–0019): every agent permission
//! request goes through policy evaluation in Core; `allow` and `deny` are
//! answered without an approval, `ask` creates one; nothing fails open.
//!
//! The fake CLI's `[fake:tool:NAME|JSON]` scenario sends one `can_use_tool`
//! request exactly as Claude Code would (including malformed ones).

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use switchyard_client::{ClientError, CoreClient, Notification};
use switchyard_protocol::methods::{
    ApprovalDecision, ApprovalsDecide, ApprovalsDecideParams, ApprovalsList, ApprovalsListParams, EventsList,
    EventsListParams, PoliciesArchive, PoliciesArchiveParams, PoliciesCreate, PoliciesCreateParams,
    PoliciesEffective, PoliciesEffectiveParams, PoliciesEvaluations, PoliciesEvaluationsParams, PoliciesGet,
    PoliciesList, PoliciesListParams, PoliciesSetAutoMode, PoliciesSetAutoModeParams, PoliciesSetEnabled,
    PoliciesSetEnabledParams, PoliciesSettings, PoliciesSettingsParams, PoliciesUpdate, PoliciesUpdateParams,
    PolicyRuleIdParams, ProjectsAdd, ProjectsAddParams, SessionsSend, SessionsSendParams, SessionsStart,
    SessionsStartParams, WorkspacesCreate, WorkspacesCreateParams,
};
use switchyard_protocol::model::{
    Approval, ApprovalStatus, DecisionSource, OperationCategory, PolicyConditions, PolicyDecision,
    PolicyEvaluation, PolicyReason, PolicyRule, RuleScope,
};
use switchyard_protocol::{ErrorCode, EventEnvelope};
use tokio::sync::mpsc;
use uuid::Uuid;

use common::{TestCore, git_repo};

struct Fixture {
    core: TestCore,
    client: CoreClient,
    _rx: mpsc::Receiver<Notification>,
    project_id: Uuid,
    workspace_id: Uuid,
    ws: PathBuf,
}

async fn fixture_with(configure: impl FnOnce(&mut switchyard_core::CoreConfig)) -> Fixture {
    let core = TestCore::start_with(configure).await;
    let (client, rx) = core.connect().await;
    let repo = git_repo(core.dir.path(), "policy-repo");
    let project_id = add_project(&client, &repo).await;
    let (workspace_id, ws) = workspace(&client, project_id).await;
    Fixture { core, client, _rx: rx, project_id, workspace_id, ws }
}

async fn fixture() -> Fixture {
    fixture_with(|_| {}).await
}

async fn add_project(client: &CoreClient, repo: &Path) -> Uuid {
    client
        .call::<ProjectsAdd>(ProjectsAddParams { path: repo.display().to_string() })
        .await
        .unwrap()
        .project
        .id
}

async fn workspace(client: &CoreClient, project_id: Uuid) -> (Uuid, PathBuf) {
    let w = client
        .call::<WorkspacesCreate>(WorkspacesCreateParams { project_id, name: None, base_ref: None })
        .await
        .unwrap()
        .workspace;
    (w.id, PathBuf::from(w.path))
}

async fn start(client: &CoreClient, workspace_id: Uuid) -> Uuid {
    client
        .call::<SessionsStart>(SessionsStartParams {
            workspace_id,
            project_id: None,
            provider: None,
            prompt: None,
            model: None,
        })
        .await
        .unwrap()
        .session
        .id
}

fn code(err: ClientError) -> ErrorCode {
    match err {
        ClientError::Remote(e) => e.code,
        other => panic!("expected remote error, got {other:?}"),
    }
}

async fn send(client: &CoreClient, session: Uuid, text: &str) {
    client
        .call::<SessionsSend>(SessionsSendParams {
            session_id: session,
            text: text.into(),
            queue_if_busy: true,
        })
        .await
        .unwrap();
}

/// One tool request in its own turn: `[fake:tool:NAME|JSON]`.
fn tool(name: &str, input: &str) -> String {
    format!("[fake:tool:{name}|{input}]")
}

async fn events(client: &CoreClient, session: Uuid) -> Vec<EventEnvelope> {
    client
        .call::<EventsList>(EventsListParams {
            session_id: Some(session),
            after_seq: Some(0),
            limit: Some(1000),
            ..Default::default()
        })
        .await
        .unwrap()
        .events
}

fn of_type<'a>(evs: &'a [EventEnvelope], t: &str) -> Vec<&'a EventEnvelope> {
    evs.iter().filter(|e| e.event_type == t).collect()
}

async fn wait_events(
    client: &CoreClient,
    session: Uuid,
    what: &str,
    pred: impl Fn(&[EventEnvelope]) -> bool,
) -> Vec<EventEnvelope> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let evs = events(client, session).await;
        if pred(&evs) {
            return evs;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}; got {:?}",
            evs.iter().map(|e| &e.event_type).collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

/// Waits until `n` turns have completed in the session.
async fn turns(client: &CoreClient, session: Uuid, n: usize) -> Vec<EventEnvelope> {
    wait_events(client, session, &format!("{n} completed turns"), |e| {
        of_type(e, "agent.turn_completed").len() >= n
    })
    .await
}

async fn approvals(client: &CoreClient, session: Uuid, status: Option<ApprovalStatus>) -> Vec<Approval> {
    client
        .call::<ApprovalsList>(ApprovalsListParams {
            status,
            session_id: Some(session),
            ..Default::default()
        })
        .await
        .unwrap()
        .approvals
}

async fn wait_pending(client: &CoreClient, session: Uuid, n: usize) -> Vec<Approval> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let p = approvals(client, session, Some(ApprovalStatus::Pending)).await;
        if p.len() == n {
            return p;
        }
        assert!(tokio::time::Instant::now() < deadline, "expected {n} pending approvals, have {}", p.len());
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

async fn decide(client: &CoreClient, id: Uuid, decision: ApprovalDecision) -> Result<Approval, ClientError> {
    client
        .call::<ApprovalsDecide>(ApprovalsDecideParams { approval_id: id, decision, note: None })
        .await
        .map(|r| r.approval)
}

async fn evaluations(client: &CoreClient, session: Uuid) -> Vec<PolicyEvaluation> {
    client
        .call::<PoliciesEvaluations>(PoliciesEvaluationsParams {
            session_id: Some(session),
            ..Default::default()
        })
        .await
        .unwrap()
        .evaluations
}

fn create(
    project_id: Uuid,
    workspace_id: Option<Uuid>,
    category: OperationCategory,
    decision: PolicyDecision,
    path_prefix: Option<&str>,
    command: Option<&str>,
    description: &str,
) -> PoliciesCreateParams {
    PoliciesCreateParams {
        project_id,
        workspace_id,
        category,
        decision,
        conditions: PolicyConditions {
            path_prefix: path_prefix.map(Into::into),
            command: command.map(Into::into),
        },
        description: description.into(),
        enabled: None,
    }
}

async fn add_rule(client: &CoreClient, p: PoliciesCreateParams) -> PolicyRule {
    client.call::<PoliciesCreate>(p).await.unwrap().rule
}

fn last_tool_outcome(evs: &[EventEnvelope]) -> (&'static str, Value) {
    let last = evs
        .iter()
        .rev()
        .find(|e| e.event_type == "agent.tool_completed" || e.event_type == "agent.tool_failed")
        .expect("a tool outcome");
    (if last.event_type == "agent.tool_completed" { "completed" } else { "failed" }, last.payload.clone())
}

fn db(f: &Fixture) -> rusqlite::Connection {
    let path = common::config_for(f.core.dir.path()).paths.database;
    let c = rusqlite::Connection::open(path).unwrap();
    c.busy_timeout(Duration::from_secs(5)).unwrap();
    c
}

// --- defaults ------------------------------------------------------------------

/// Reads and searches inside the workspace are allowed by the built-in
/// default without an approval, recorded with Core's own scope ids, and the
/// tool runs. File contents never enter the policy records.
#[tokio::test]
async fn reads_inside_the_workspace_are_allowed_automatically_and_audited() {
    let f = fixture().await;
    let s = start(&f.client, f.workspace_id).await;
    send(&f.client, s, &tool("Read", r#"{"file_path":"$CWD/README.md"}"#)).await;
    send(&f.client, s, &tool("Grep", r#"{"pattern":"x","output_mode":"content"}"#)).await;
    let evs = turns(&f.client, s, 2).await;
    assert_eq!(of_type(&evs, "agent.tool_completed").len(), 2, "both tools ran");
    assert!(of_type(&evs, "agent.permission_requested").is_empty(), "no approval was needed");
    assert!(approvals(&f.client, s, None).await.is_empty());

    let evals = evaluations(&f.client, s).await;
    assert_eq!(evals.len(), 2);
    for e in &evals {
        assert_eq!((e.decision, e.reason), (PolicyDecision::Allow, PolicyReason::Default));
        assert_eq!((e.project_id, e.workspace_id, e.session_id), (f.project_id, Some(f.workspace_id), s));
        assert_eq!(e.trigger, "request");
        assert!(e.approval_id.is_none());
        assert!(e.explanation.contains("Built-in default"), "{}", e.explanation);
    }
    assert_eq!(evals[1].category, OperationCategory::FileRead);
    assert_eq!(evals[1].input["paths"][0]["location"], "inside");
    let journaled = of_type(&evs, "agent.permission_evaluated");
    assert_eq!(journaled.len(), 2);
    assert_eq!(journaled[0].payload["decision"], "allow");
    assert_eq!(journaled[0].context.session_id, Some(s));
    assert_eq!(journaled[0].context.workspace_id, Some(f.workspace_id));
}

/// Bash always asks by default; a human approval applies to that one
/// request and creates no rule; a second decision is impossible.
#[tokio::test]
async fn bash_asks_and_a_human_approval_covers_only_that_request() {
    let f = fixture().await;
    let s = start(&f.client, f.workspace_id).await;
    send(&f.client, s, &tool("Bash", r#"{"command":"ls","description":"list"}"#)).await;
    let pending = wait_pending(&f.client, s, 1).await;
    let policy = pending[0].policy.as_ref().expect("approval carries its evaluation");
    assert_eq!((policy.category, policy.reason), (OperationCategory::Shell, PolicyReason::Default));
    assert!(policy.explanation.contains("requires approval"), "{}", policy.explanation);
    let approved = decide(&f.client, pending[0].id, ApprovalDecision::Approve).await.unwrap();
    assert_eq!(approved.status, ApprovalStatus::Approved);
    assert_eq!(
        code(decide(&f.client, pending[0].id, ApprovalDecision::Deny).await.unwrap_err()),
        ErrorCode::Conflict
    );
    turns(&f.client, s, 1).await;

    // The same command asks again: approving did not create a rule.
    send(&f.client, s, &tool("Bash", r#"{"command":"ls","description":"list"}"#)).await;
    let again = wait_pending(&f.client, s, 1).await;
    assert_ne!(again[0].id, pending[0].id);
    let rules = f
        .client
        .call::<PoliciesList>(PoliciesListParams { project_id: f.project_id, include_archived: true })
        .await
        .unwrap();
    assert!(rules.rules.is_empty(), "no rule was created by a decision");
    assert_eq!(rules.policy_revision, 0);
    let denied = decide(&f.client, again[0].id, ApprovalDecision::Deny).await.unwrap();
    assert_eq!(denied.status, ApprovalStatus::Denied);
    let evs = turns(&f.client, s, 2).await;
    assert_eq!(last_tool_outcome(&evs).0, "failed", "the denial reached the agent");
}

// --- unknown and malformed ---------------------------------------------------------

/// Unknown tools, tools not enabled at launch and malformed requests are
/// denied by the system: no approval, the agent is told, nothing runs.
#[tokio::test]
async fn unknown_disabled_and_malformed_requests_are_denied_without_an_approval() {
    let f = fixture().await;
    let s = start(&f.client, f.workspace_id).await;
    let cases = [
        ("Teleport", r#"{"to":"mars"}"#),
        ("-", r#"{"file_path":"$CWD/a"}"#),
        ("Read", r#""not an object""#),
        ("Read", r#"{"path":"$CWD/a"}"#),
        ("Write", r#"{"file_path":42,"content":"x"}"#),
        ("Bash", r#"{"command":""}"#),
        ("WebFetch", r#"{"url":"https://example.invalid"}"#),
        ("mcp__srv__run", r#"{}"#),
        ("Task", r#"{"prompt":"x"}"#),
    ];
    // Even a permissive rule for the category does not help.
    add_rule(
        &f.client,
        create(
            f.project_id,
            None,
            OperationCategory::FileRead,
            PolicyDecision::Allow,
            None,
            None,
            "reads ok",
        ),
    )
    .await;
    for (i, (name, input)) in cases.iter().enumerate() {
        send(&f.client, s, &tool(name, input)).await;
        let evs = turns(&f.client, s, i + 1).await;
        let (outcome, payload) = last_tool_outcome(&evs);
        assert_eq!(outcome, "failed", "{name} {input}");
        assert_eq!(payload["rejected"], true, "{name}: rejected by a denial");
    }
    assert!(approvals(&f.client, s, None).await.is_empty(), "no denial offers an approval");
    let evals = evaluations(&f.client, s).await;
    assert_eq!(evals.len(), cases.len());
    for e in &evals {
        assert_eq!((e.decision, e.reason), (PolicyDecision::Deny, PolicyReason::SystemRestriction), "{e:?}");
        assert!(e.explanation.starts_with("Blocked by Switchyard"), "{}", e.explanation);
    }
    assert!(!f.ws.join("a").exists());
}

// --- configured rules ----------------------------------------------------------------

/// An allow rule bypasses the approval; a deny rule blocks without one; a
/// broad ask beats a narrow allow; Bash runs automatically only for the
/// exact command a rule names.
#[tokio::test]
async fn configured_rules_allow_ask_and_deny() {
    let f = fixture().await;
    let s = start(&f.client, f.workspace_id).await;
    let docs = add_rule(
        &f.client,
        create(
            f.project_id,
            None,
            OperationCategory::FileWrite,
            PolicyDecision::Allow,
            Some("docs/"),
            None,
            "Docs may be written",
        ),
    )
    .await;
    assert_eq!(docs.conditions.path_prefix.as_deref(), Some("docs"), "normalized");
    std::fs::create_dir_all(f.ws.join("docs")).unwrap();
    send(&f.client, s, &tool("Write", r#"{"file_path":"$CWD/docs/a.md","content":"x"}"#)).await;
    let evs = turns(&f.client, s, 1).await;
    assert_eq!(last_tool_outcome(&evs).0, "completed");
    assert!(f.ws.join("docs/a.md").exists(), "allowed automatically");
    let evals = evaluations(&f.client, s).await;
    assert_eq!(evals[0].deciding_rule.as_ref().map(|r| (r.id, r.revision)), Some((docs.id, 1)));
    assert!(evals[0].explanation.contains("Docs may be written"));

    // Outside the rule's path: the default asks.
    send(&f.client, s, &tool("Write", r#"{"file_path":"$CWD/b.txt","content":"x"}"#)).await;
    let p = wait_pending(&f.client, s, 1).await;
    decide(&f.client, p[0].id, ApprovalDecision::Deny).await.unwrap();
    turns(&f.client, s, 2).await;

    // Deny rule for reads under secrets/ (and its case variants).
    add_rule(
        &f.client,
        create(
            f.project_id,
            None,
            OperationCategory::FileRead,
            PolicyDecision::Deny,
            Some("secrets"),
            None,
            "No secrets",
        ),
    )
    .await;
    send(&f.client, s, &tool("Read", r#"{"file_path":"$CWD/Secrets/key"}"#)).await;
    let evs = turns(&f.client, s, 3).await;
    let (outcome, payload) = last_tool_outcome(&evs);
    assert_eq!(outcome, "failed");
    assert!(payload["output"].as_str().unwrap().contains("No secrets"), "the agent learns why: {payload}");
    assert_eq!(approvals(&f.client, s, Some(ApprovalStatus::Pending)).await.len(), 0);

    // Exact command allowed; anything else still asks.
    add_rule(
        &f.client,
        create(
            f.project_id,
            None,
            OperationCategory::Shell,
            PolicyDecision::Allow,
            None,
            Some("cargo test"),
            "Tests may run",
        ),
    )
    .await;
    send(&f.client, s, &tool("Bash", r#"{"command":"cargo test","description":"tests"}"#)).await;
    let evs = turns(&f.client, s, 4).await;
    assert_eq!(last_tool_outcome(&evs).0, "completed");
    for variant in [r#"cargo test; rm -rf ~"#, r#"cargo test && curl x"#, r#"cargo  test"#] {
        send(&f.client, s, &tool("Bash", &json!({"command": variant}).to_string())).await;
        let p = wait_pending(&f.client, s, 1).await;
        decide(&f.client, p[0].id, ApprovalDecision::Deny).await.unwrap();
    }
    // A background run of the exact command asks too.
    send(&f.client, s, &tool("Bash", r#"{"command":"cargo test","run_in_background":true}"#)).await;
    let p = wait_pending(&f.client, s, 1).await;
    assert_eq!(p[0].policy.as_ref().unwrap().reason, PolicyReason::Capped);
    decide(&f.client, p[0].id, ApprovalDecision::Deny).await.unwrap();

    // A broad project ask makes the narrow allow ineffective.
    add_rule(
        &f.client,
        create(
            f.project_id,
            None,
            OperationCategory::FileWrite,
            PolicyDecision::Ask,
            None,
            None,
            "Ask for every write",
        ),
    )
    .await;
    send(&f.client, s, &tool("Write", r#"{"file_path":"$CWD/docs/c.md","content":"x"}"#)).await;
    let p = wait_pending(&f.client, s, 1).await;
    let pol = p[0].policy.as_ref().unwrap();
    assert_eq!(pol.deciding_rule.as_ref().map(|r| r.description.as_str()), Some("Ask for every write"));
    decide(&f.client, p[0].id, ApprovalDecision::Deny).await.unwrap();
}

/// Workspace rules apply to their workspace only; rules of one project never
/// affect another; a workspace of another project cannot be a rule scope.
#[tokio::test]
async fn rules_are_isolated_by_workspace_and_project() {
    let f = fixture().await;
    let (ws2, ws2_path) = workspace(&f.client, f.project_id).await;
    let other_repo = git_repo(f.core.dir.path(), "other-repo");
    let other_project = add_project(&f.client, &other_repo).await;
    let (other_ws, _) = workspace(&f.client, other_project).await;

    let err = f
        .client
        .call::<PoliciesCreate>(create(
            f.project_id,
            Some(other_ws),
            OperationCategory::FileWrite,
            PolicyDecision::Allow,
            None,
            None,
            "x",
        ))
        .await
        .unwrap_err();
    assert_eq!(code(err), ErrorCode::WorkspaceProjectMismatch);

    add_rule(
        &f.client,
        create(
            f.project_id,
            Some(f.workspace_id),
            OperationCategory::FileWrite,
            PolicyDecision::Allow,
            None,
            None,
            "ws1 writes",
        ),
    )
    .await;
    add_rule(
        &f.client,
        create(
            other_project,
            None,
            OperationCategory::FileWrite,
            PolicyDecision::Allow,
            None,
            None,
            "other project writes",
        ),
    )
    .await;
    add_rule(
        &f.client,
        create(
            other_project,
            Some(other_ws),
            OperationCategory::FileRead,
            PolicyDecision::Deny,
            None,
            None,
            "other reads blocked",
        ),
    )
    .await;

    // Workspace 1: allowed by its workspace rule.
    let s1 = start(&f.client, f.workspace_id).await;
    send(&f.client, s1, &tool("Write", r#"{"file_path":"$CWD/one.txt","content":"x"}"#)).await;
    turns(&f.client, s1, 1).await;
    assert!(f.ws.join("one.txt").exists());

    // Workspace 2 of the same project: the ws1 rule and the other project's
    // rules do not apply, so the default asks; reads stay allowed.
    let s2 = start(&f.client, ws2).await;
    send(&f.client, s2, &tool("Write", r#"{"file_path":"$CWD/two.txt","content":"x"}"#)).await;
    let p = wait_pending(&f.client, s2, 1).await;
    assert_eq!(p[0].policy.as_ref().unwrap().reason, PolicyReason::Default);
    assert_eq!(p[0].workspace_id, Some(ws2));
    decide(&f.client, p[0].id, ApprovalDecision::Deny).await.unwrap();
    send(&f.client, s2, &tool("Read", r#"{"file_path":"$CWD/README.md"}"#)).await;
    let evs = turns(&f.client, s2, 2).await;
    assert_eq!(last_tool_outcome(&evs).0, "completed");
    assert!(!ws2_path.join("two.txt").exists());

    // The effective view separates inherited project rules and local ones.
    let eff = f
        .client
        .call::<PoliciesEffective>(PoliciesEffectiveParams {
            project_id: f.project_id,
            workspace_id: Some(f.workspace_id),
        })
        .await
        .unwrap();
    assert!(eff.project_rules.is_empty());
    assert_eq!(eff.workspace_rules.len(), 1);
    assert_eq!(eff.workspace_rules[0].scope, RuleScope::Workspace);
    assert!(!eff.system_restrictions.is_empty());
    let shell = eff.categories.iter().find(|c| c.category == OperationCategory::Shell).unwrap();
    assert_eq!((shell.default_decision, shell.command_condition), (PolicyDecision::Ask, true));
    let err = f
        .client
        .call::<PoliciesEffective>(PoliciesEffectiveParams {
            project_id: f.project_id,
            workspace_id: Some(other_ws),
        })
        .await
        .unwrap_err();
    assert_eq!(code(err), ErrorCode::WorkspaceProjectMismatch);
}

// --- pending approvals and policy changes ------------------------------------------

/// A pending approval keeps its original evaluation: a new allow rule does
/// not approve it; a new deny rule denies it (source `policy`) and tells the
/// agent.
#[tokio::test]
async fn policy_changes_never_upgrade_a_pending_approval_but_can_deny_it() {
    let f = fixture().await;
    let s = start(&f.client, f.workspace_id).await;
    send(&f.client, s, &tool("Bash", r#"{"command":"ls"}"#)).await;
    let p = wait_pending(&f.client, s, 1).await;
    let original_revision = p[0].policy.as_ref().unwrap().policy_revision;

    add_rule(
        &f.client,
        create(
            f.project_id,
            None,
            OperationCategory::Shell,
            PolicyDecision::Allow,
            None,
            Some("ls"),
            "ls is fine",
        ),
    )
    .await;
    let still = wait_pending(&f.client, s, 1).await;
    assert_eq!(still[0].id, p[0].id, "still pending: a person was asked and still decides");
    assert_eq!(
        still[0].policy.as_ref().unwrap().policy_revision,
        original_revision,
        "bound to its evaluation"
    );

    add_rule(
        &f.client,
        create(f.project_id, None, OperationCategory::Shell, PolicyDecision::Deny, None, None, "No shell"),
    )
    .await;
    let settled = approvals(&f.client, s, None).await;
    assert_eq!(settled.len(), 1);
    assert_eq!(
        (settled[0].status, settled[0].decision_source),
        (ApprovalStatus::Denied, Some(DecisionSource::Policy))
    );
    assert!(settled[0].decision_note.as_deref().unwrap().contains("No shell"));
    let evs = turns(&f.client, s, 1).await;
    assert_eq!(last_tool_outcome(&evs).0, "failed", "the agent was told");
    let evals = evaluations(&f.client, s).await;
    assert_eq!(evals[0].trigger, "reevaluation");
    assert_eq!((evals[0].decision, evals[0].approval_id), (PolicyDecision::Deny, Some(p[0].id)));
    assert_eq!(evals[0].approval_status, Some(ApprovalStatus::Denied));
    assert_eq!(evals[1].trigger, "request", "the original evaluation is unchanged");
    assert_eq!(evals[1].decision, PolicyDecision::Ask);
    assert_eq!(
        code(decide(&f.client, p[0].id, ApprovalDecision::Approve).await.unwrap_err()),
        ErrorCode::Conflict
    );
}

/// A human cannot approve what the policy blocks at decision time, even when
/// the blocking rule appeared without Core noticing (written straight into
/// the database, standing in for a race with a concurrent change).
#[tokio::test]
async fn approval_is_refused_when_the_current_policy_denies() {
    let f = fixture().await;
    let s = start(&f.client, f.workspace_id).await;
    send(&f.client, s, &tool("Write", r#"{"file_path":"$CWD/x.txt","content":"x"}"#)).await;
    let p = wait_pending(&f.client, s, 1).await;
    let rule = json!({"id": Uuid::now_v7(), "project_id": f.project_id, "workspace_id": null, "category": "file_write",
        "decision": "deny", "conditions": {}, "description": "Frozen", "enabled": true, "archived": false,
        "revision": 1, "created_at_ms": 1, "updated_at_ms": 1, "archived_at_ms": null});
    let c = db(&f);
    c.execute(
        "INSERT INTO policy_rules (id, project_id, category, decision, conditions, description, enabled, status, \
         revision, created_at, updated_at) VALUES (?1, ?2, 'file_write', 'deny', '{}', 'Frozen', 1, 'active', 1, 1, 1)",
        [rule["id"].as_str().unwrap().to_owned(), f.project_id.to_string()],
    )
    .unwrap();
    c.execute(
        "INSERT INTO policy_rule_revisions (rule_id, revision, policy_revision, project_id, change, rule, changed_at) \
         VALUES (?1, 1, 1, ?2, 'created', ?3, 1)",
        [rule["id"].as_str().unwrap().to_owned(), f.project_id.to_string(), rule.to_string()],
    )
    .unwrap();
    drop(c);
    let err = decide(&f.client, p[0].id, ApprovalDecision::Approve).await.unwrap_err();
    assert_eq!(code(err), ErrorCode::PolicyDenied);
    let a = approvals(&f.client, s, None).await;
    assert_eq!((a[0].status, a[0].decision_source), (ApprovalStatus::Denied, Some(DecisionSource::Policy)));
    let evs = turns(&f.client, s, 1).await;
    assert_eq!(last_tool_outcome(&evs).0, "failed");
    assert!(!f.ws.join("x.txt").exists(), "nothing was written");
    assert_eq!(evaluations(&f.client, s).await[0].trigger, "decision_check");
}

/// Expiry still denies approvals created by an `ask` evaluation.
#[tokio::test]
async fn policy_created_approvals_still_expire_as_denials() {
    let f = fixture_with(|c| c.approval_timeout = Duration::from_millis(400)).await;
    let s = start(&f.client, f.workspace_id).await;
    send(&f.client, s, &tool("Edit", r#"{"file_path":"$CWD/README.md","old_string":"a","new_string":"b"}"#))
        .await;
    let evs = turns(&f.client, s, 1).await;
    assert_eq!(last_tool_outcome(&evs).0, "failed");
    let a = approvals(&f.client, s, None).await;
    assert_eq!((a[0].status, a[0].decision_source), (ApprovalStatus::Expired, Some(DecisionSource::Timeout)));
    assert!(a[0].policy.is_some());
}

// --- failures fail closed ---------------------------------------------------------

/// If the evaluation cannot be recorded, the request is denied even when the
/// policy would allow it.
#[tokio::test]
async fn database_failure_denies_instead_of_allowing() {
    let f = fixture().await;
    let s = start(&f.client, f.workspace_id).await;
    let c = db(&f);
    c.execute_batch(
        "CREATE TRIGGER test_break_evaluations BEFORE INSERT ON policy_evaluations \
         BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
    )
    .unwrap();
    send(&f.client, s, &tool("Read", r#"{"file_path":"$CWD/README.md"}"#)).await;
    send(&f.client, s, &tool("Write", r#"{"file_path":"$CWD/w.txt","content":"x"}"#)).await;
    let evs = turns(&f.client, s, 2).await;
    let failed = of_type(&evs, "agent.tool_failed");
    assert_eq!(failed.len(), 2, "both denied");
    assert!(failed[0].payload["output"].as_str().unwrap().contains("could not evaluate or record"));
    assert!(approvals(&f.client, s, None).await.is_empty());
    assert!(!f.ws.join("w.txt").exists());
    c.execute_batch("DROP TRIGGER test_break_evaluations;").unwrap();
    send(&f.client, s, &tool("Read", r#"{"file_path":"$CWD/README.md"}"#)).await;
    let evs = turns(&f.client, s, 3).await;
    assert_eq!(last_tool_outcome(&evs).0, "completed", "recovers once recording works");
}

// --- rule management, revisions and audit ------------------------------------------------

#[tokio::test]
async fn invalid_and_conflicting_rules_are_rejected() {
    let f = fixture().await;
    let p = f.project_id;
    let cases = [
        (
            create(p, None, OperationCategory::Shell, PolicyDecision::Allow, None, None, "all bash"),
            ErrorCode::UnsupportedOperation,
        ),
        (
            create(p, None, OperationCategory::Network, PolicyDecision::Allow, None, None, "net"),
            ErrorCode::UnsupportedOperation,
        ),
        (
            create(p, None, OperationCategory::Unknown, PolicyDecision::Deny, None, None, "x"),
            ErrorCode::UnsupportedOperation,
        ),
        (
            create(
                p,
                None,
                OperationCategory::Shell,
                PolicyDecision::Allow,
                None,
                Some("cargo test; id"),
                "x",
            ),
            ErrorCode::InvalidRule,
        ),
        (
            create(p, None, OperationCategory::FileRead, PolicyDecision::Allow, Some("../x"), None, "x"),
            ErrorCode::InvalidRule,
        ),
        (
            create(p, None, OperationCategory::FileRead, PolicyDecision::Allow, Some("/etc"), None, "x"),
            ErrorCode::InvalidRule,
        ),
        (
            create(p, None, OperationCategory::Shell, PolicyDecision::Ask, Some("src"), None, "x"),
            ErrorCode::InvalidRule,
        ),
        (
            create(p, None, OperationCategory::FileRead, PolicyDecision::Ask, None, None, "  "),
            ErrorCode::InvalidRule,
        ),
        (
            create(Uuid::now_v7(), None, OperationCategory::FileRead, PolicyDecision::Ask, None, None, "x"),
            ErrorCode::ProjectNotFound,
        ),
        (
            create(
                p,
                Some(Uuid::now_v7()),
                OperationCategory::FileRead,
                PolicyDecision::Ask,
                None,
                None,
                "x",
            ),
            ErrorCode::WorkspaceNotFound,
        ),
    ];
    for (params, expected) in cases {
        let err = f.client.call::<PoliciesCreate>(params.clone()).await.unwrap_err();
        assert_eq!(code(err), expected, "{params:?}");
    }
    add_rule(
        &f.client,
        create(p, None, OperationCategory::FileEdit, PolicyDecision::Allow, Some("docs"), None, "a"),
    )
    .await;
    let err = f
        .client
        .call::<PoliciesCreate>(create(
            p,
            None,
            OperationCategory::FileEdit,
            PolicyDecision::Deny,
            Some("docs/"),
            None,
            "b",
        ))
        .await
        .unwrap_err();
    assert_eq!(code(err), ErrorCode::PolicyConflict);
    // Unknown condition fields are rejected at the protocol level.
    let raw = f
        .client
        .call_raw(
            "policies.create",
            json!({"project_id": p, "category": "file_read", "decision": "ask", "description": "x",
                   "conditions": {"script": "rm -rf /"}}),
        )
        .await
        .unwrap_err();
    assert_eq!(code(raw), ErrorCode::InvalidParams);
    let rules = f
        .client
        .call::<PoliciesList>(PoliciesListParams { project_id: p, include_archived: true })
        .await
        .unwrap();
    assert_eq!(rules.rules.len(), 1, "nothing invalid was stored");
}

/// Every change is a revision; stale writers are refused; history and past
/// evaluations stay as they were; everything survives a restart.
#[tokio::test]
async fn rule_changes_are_revisioned_audited_and_survive_restart() {
    let mut f = fixture().await;
    let rule = add_rule(
        &f.client,
        create(
            f.project_id,
            None,
            OperationCategory::FileEdit,
            PolicyDecision::Allow,
            None,
            None,
            "Edits ok",
        ),
    )
    .await;
    let s = start(&f.client, f.workspace_id).await;
    send(&f.client, s, &tool("Edit", r#"{"file_path":"$CWD/README.md","old_string":"a","new_string":"b"}"#))
        .await;
    turns(&f.client, s, 1).await;
    let before = evaluations(&f.client, s).await;
    assert_eq!(before[0].decision, PolicyDecision::Allow);

    let updated = f
        .client
        .call::<PoliciesUpdate>(PoliciesUpdateParams {
            rule_id: rule.id,
            expected_revision: 1,
            decision: Some(PolicyDecision::Ask),
            conditions: None,
            description: Some("Edits need approval".into()),
        })
        .await
        .unwrap();
    assert_eq!((updated.rule.revision, updated.policy_revision), (2, 2));
    let stale = f
        .client
        .call::<PoliciesUpdate>(PoliciesUpdateParams {
            rule_id: rule.id,
            expected_revision: 1,
            decision: Some(PolicyDecision::Allow),
            conditions: None,
            description: None,
        })
        .await
        .unwrap_err();
    assert_eq!(code(stale), ErrorCode::StaleRevision);
    let disabled = f
        .client
        .call::<PoliciesSetEnabled>(PoliciesSetEnabledParams {
            rule_id: rule.id,
            expected_revision: 2,
            enabled: false,
        })
        .await
        .unwrap();
    assert!(!disabled.rule.enabled);
    let archived = f
        .client
        .call::<PoliciesArchive>(PoliciesArchiveParams { rule_id: rule.id, expected_revision: 3 })
        .await
        .unwrap();
    assert!(archived.rule.archived);
    assert_eq!(archived.policy_revision, 4);

    // The earlier evaluation still says what it said, with the old revision.
    let after = evaluations(&f.client, s).await;
    assert_eq!(after, before, "history does not change when rules change");
    assert_eq!(
        after[0].deciding_rule.as_ref().map(|r| (r.revision, r.description.as_str())),
        Some((1, "Edits ok"))
    );

    f.core.restart_with(|_| {}).await;
    let (client, _rx) = f.core.connect().await;
    let got = client.call::<PoliciesGet>(PolicyRuleIdParams { rule_id: rule.id }).await.unwrap();
    assert_eq!(
        got.history.iter().map(|h| (h.revision, h.policy_revision, h.change.as_str())).collect::<Vec<_>>(),
        [(1, 1, "created"), (2, 2, "updated"), (3, 3, "disabled"), (4, 4, "archived")]
    );
    assert_eq!(got.history[0].rule.decision, PolicyDecision::Allow);
    assert_eq!(got.history[1].rule.description, "Edits need approval");
    let list = client
        .call::<PoliciesList>(PoliciesListParams { project_id: f.project_id, include_archived: false })
        .await
        .unwrap();
    assert!(list.rules.is_empty(), "archived rules are hidden by default");
    assert_eq!(list.policy_revision, 4);
    let after_restart = client
        .call::<PoliciesEvaluations>(PoliciesEvaluationsParams { session_id: Some(s), ..Default::default() })
        .await
        .unwrap()
        .evaluations;
    assert_eq!(after_restart, before);
    // Rule changes are journaled with the project (and no session).
    let journal = client
        .call::<EventsList>(EventsListParams {
            project_id: Some(f.project_id),
            after_seq: Some(0),
            limit: Some(1000),
            ..Default::default()
        })
        .await
        .unwrap()
        .events;
    let changes: Vec<_> = journal
        .iter()
        .filter(|e| e.event_type == "policy.rule_changed")
        .map(|e| e.payload["change"].clone())
        .collect();
    assert_eq!(changes, ["created", "updated", "disabled", "archived"]);
    // Archived rules cannot be edited.
    let err = client
        .call::<PoliciesSetEnabled>(PoliciesSetEnabledParams {
            rule_id: rule.id,
            expected_revision: 4,
            enabled: true,
        })
        .await
        .unwrap_err();
    assert_eq!(code(err), ErrorCode::InvalidRule);
}

/// Two clients edit the same revision at the same time: exactly one update
/// wins, the other is refused as `stale_revision` and cannot overwrite it,
/// and only the winner appears in the history and the journal.
#[tokio::test]
async fn concurrent_edits_of_one_revision_admit_exactly_one() {
    let f = fixture().await;
    let rule = add_rule(
        &f.client,
        create(f.project_id, None, OperationCategory::Shell, PolicyDecision::Ask, None, None, "Shell asks"),
    )
    .await;
    let (other, _orx) = f.core.connect().await;
    // Both clients start from the same revision.
    let seen_a = f.client.call::<PoliciesGet>(PolicyRuleIdParams { rule_id: rule.id }).await.unwrap().rule;
    let seen_b = other.call::<PoliciesGet>(PolicyRuleIdParams { rule_id: rule.id }).await.unwrap().rule;
    assert_eq!((seen_a.revision, seen_b.revision), (1, 1));

    let update = |client: &CoreClient, description: &str, decision: PolicyDecision| {
        let client = client.clone();
        let params = PoliciesUpdateParams {
            rule_id: rule.id,
            expected_revision: 1,
            decision: Some(decision),
            conditions: None,
            description: Some(description.into()),
        };
        async move { client.call::<PoliciesUpdate>(params).await }
    };
    let (a, b) = tokio::join!(
        update(&f.client, "A: block shell", PolicyDecision::Deny),
        update(&other, "B: shell asks (edited)", PolicyDecision::Ask)
    );
    let (winner, loser) = match (a, b) {
        (Ok(w), Err(l)) => (w.rule, l),
        (Err(l), Ok(w)) => (w.rule, l),
        (a, b) => panic!("exactly one update must win: {a:?} / {b:?}"),
    };
    assert_eq!(code(loser), ErrorCode::StaleRevision);
    assert_eq!(winner.revision, 2);

    // The stale client cannot change it by other means either.
    let toggled = other
        .call::<PoliciesSetEnabled>(PoliciesSetEnabledParams {
            rule_id: rule.id,
            expected_revision: 1,
            enabled: false,
        })
        .await
        .unwrap_err();
    assert_eq!(code(toggled), ErrorCode::StaleRevision);
    let archived = other
        .call::<PoliciesArchive>(PoliciesArchiveParams { rule_id: rule.id, expected_revision: 1 })
        .await
        .unwrap_err();
    assert_eq!(code(archived), ErrorCode::StaleRevision);

    // Reloading shows the winner, from either client.
    for c in [&f.client, &other] {
        let got = c.call::<PoliciesGet>(PolicyRuleIdParams { rule_id: rule.id }).await.unwrap();
        assert_eq!(got.rule, winner);
        assert_eq!(got.history.iter().map(|h| h.revision).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(got.history[1].rule.description, winner.description);
    }
    let journal = f
        .client
        .call::<EventsList>(EventsListParams {
            project_id: Some(f.project_id),
            event_types: vec!["policy.rule_changed".into()],
            after_seq: Some(0),
            limit: Some(100),
            ..Default::default()
        })
        .await
        .unwrap()
        .events;
    assert_eq!(journal.len(), 2, "one create, one winning update");
}

/// A block rule is not bypassed by spelling the path differently: a decomposed
/// (NFD) spelling of an existing NFC directory on APFS resolves to the on-disk
/// name, and block rules match the requested or the resolved path; a sibling
/// that only shares a string prefix ("secrets-public") is not matched.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn block_rules_match_unicode_variants_by_resolved_path_and_whole_components() {
    let f = fixture().await;
    let s = start(&f.client, f.workspace_id).await;
    add_rule(
        &f.client,
        create(
            f.project_id,
            None,
            OperationCategory::FileRead,
            PolicyDecision::Deny,
            Some("r\u{e9}sum\u{e9}"),
            None,
            "No CVs",
        ),
    )
    .await;
    std::fs::create_dir_all(f.ws.join("r\u{e9}sum\u{e9}")).unwrap();
    std::fs::write(f.ws.join("r\u{e9}sum\u{e9}/cv.txt"), "x").unwrap();
    // NFD spelling of the same directory (e + U+0301).
    send(&f.client, s, &tool("Read", "{\"file_path\":\"$CWD/re\u{301}sume\u{301}/cv.txt\"}")).await;
    turns(&f.client, s, 1).await;
    let e = &evaluations(&f.client, s).await[0];
    assert_eq!(e.decision, PolicyDecision::Deny, "{}", e.explanation);

    add_rule(
        &f.client,
        create(
            f.project_id,
            None,
            OperationCategory::FileRead,
            PolicyDecision::Deny,
            Some("secrets"),
            None,
            "No secrets",
        ),
    )
    .await;
    std::fs::create_dir_all(f.ws.join("secrets-public")).unwrap();
    std::fs::write(f.ws.join("secrets-public/readme"), "x").unwrap();
    send(&f.client, s, &tool("Read", r#"{"file_path":"$CWD/secrets-public/readme"}"#)).await;
    turns(&f.client, s, 2).await;
    let e = &evaluations(&f.client, s).await[0];
    assert_eq!(e.decision, PolicyDecision::Allow, "a string prefix is not a path prefix: {}", e.explanation);
}

// --- auto mode (ADR 0037) -------------------------------------------------------------

async fn set_auto(f: &Fixture, enabled: bool, expected_revision: u32) -> Result<u32, ClientError> {
    f.client
        .call::<PoliciesSetAutoMode>(PoliciesSetAutoModeParams {
            project_id: f.project_id,
            enabled,
            expected_revision,
        })
        .await
        .map(|r| r.settings.revision)
}

/// With auto mode on, file changes and any shell command inside the workspace
/// are answered `allow` without an approval; files outside the workspace and
/// configured ask rules still ask; turning it off restores asking. Every
/// change is versioned and journaled.
#[tokio::test]
async fn auto_mode_allows_changes_and_commands_without_asking() {
    let f = fixture().await;
    let settings =
        f.client.call::<PoliciesSettings>(PoliciesSettingsParams { project_id: f.project_id }).await.unwrap();
    assert_eq!((settings.auto_mode, settings.revision), (false, 0));
    assert_eq!(code(set_auto(&f, true, 3).await.unwrap_err()), ErrorCode::StaleRevision);
    assert_eq!(code(set_auto(&f, false, 0).await.unwrap_err()), ErrorCode::InvalidParams, "already off");
    let rev = set_auto(&f, true, 0).await.unwrap();
    assert_eq!(rev, 1);
    add_rule(
        &f.client,
        create(
            f.project_id,
            None,
            OperationCategory::Shell,
            PolicyDecision::Ask,
            None,
            Some("git push"),
            "Ask before pushing",
        ),
    )
    .await;

    let s = start(&f.client, f.workspace_id).await;
    send(&f.client, s, &tool("Write", r#"{"file_path":"$CWD/auto.txt","content":"x"}"#)).await;
    send(&f.client, s, &tool("Bash", r#"{"command":"ls && echo done","description":"list"}"#)).await;
    let evs = turns(&f.client, s, 2).await;
    assert!(of_type(&evs, "agent.permission_requested").is_empty(), "nothing asked");
    assert!(approvals(&f.client, s, None).await.is_empty());
    let evals = evaluations(&f.client, s).await;
    assert_eq!(evals.len(), 2, "{:?}", evs.iter().map(|e| (&e.event_type, &e.payload)).collect::<Vec<_>>());
    for e in &evals {
        assert_eq!(
            (e.decision, e.reason),
            (PolicyDecision::Allow, PolicyReason::AutoMode),
            "{}",
            e.explanation
        );
    }

    let recorded: i64 = db(&f)
        .query_row(
            "SELECT COUNT(*) FROM policy_evaluations WHERE json_extract(input, '$.scope.auto_mode') = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(recorded, 2, "auto mode is recorded with each evaluated input");

    // Outside the workspace, and a configured ask rule, still ask.
    send(&f.client, s, &tool("Write", r#"{"file_path":"$CWD/../outside.txt","content":"x"}"#)).await;
    let p = wait_pending(&f.client, s, 1).await;
    assert_eq!(p[0].policy.as_ref().unwrap().reason, PolicyReason::Capped);
    decide(&f.client, p[0].id, ApprovalDecision::Deny).await.unwrap();
    turns(&f.client, s, 3).await;
    send(&f.client, s, &tool("Bash", r#"{"command":"git push"}"#)).await;
    let p = wait_pending(&f.client, s, 1).await;
    assert_eq!(p[0].policy.as_ref().unwrap().reason, PolicyReason::Rule);
    decide(&f.client, p[0].id, ApprovalDecision::Deny).await.unwrap();
    turns(&f.client, s, 4).await;

    // Off again: the same command asks.
    assert_eq!(set_auto(&f, false, rev).await.unwrap(), 2);
    send(&f.client, s, &tool("Bash", r#"{"command":"ls && echo done"}"#)).await;
    let p = wait_pending(&f.client, s, 1).await;
    assert_eq!(p[0].policy.as_ref().unwrap().reason, PolicyReason::Default);

    // Journaled with the project, and sharing the global revision with rules.
    let changes: Vec<(bool, i64)> = db(&f)
        .prepare("SELECT auto_mode, policy_revision FROM policy_settings_revisions ORDER BY revision")
        .unwrap()
        .query_map([], |r| Ok((r.get::<_, i64>(0)? == 1, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(changes, [(true, 1), (false, 3)], "the rule took revision 2");
    let all = f
        .client
        .call::<EventsList>(EventsListParams { after_seq: Some(0), limit: Some(1000), ..Default::default() })
        .await
        .unwrap()
        .events;
    let journaled = of_type(&all, "policy.settings_changed");
    assert_eq!(journaled.len(), 2);
    assert_eq!(journaled[0].payload["auto_mode"], true);
    assert_eq!(journaled[0].context.project_id, Some(f.project_id));
}

/// Settings exist only for registered projects.
#[tokio::test]
async fn auto_mode_settings_need_a_registered_project() {
    let f = fixture().await;
    let missing = Uuid::now_v7();
    let err =
        f.client.call::<PoliciesSettings>(PoliciesSettingsParams { project_id: missing }).await.unwrap_err();
    assert_eq!(code(err), ErrorCode::ProjectNotFound);
    let err = f
        .client
        .call::<PoliciesSetAutoMode>(PoliciesSetAutoModeParams {
            project_id: missing,
            enabled: true,
            expected_revision: 0,
        })
        .await
        .unwrap_err();
    assert_eq!(code(err), ErrorCode::ProjectNotFound);
}
