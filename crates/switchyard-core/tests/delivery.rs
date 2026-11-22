//! Milestone 8.2: the engineering workflow carries one task through M7's
//! integration to the target branch (ADR 0034), with the fake CLIs and real
//! temporary Git repositories.
//!
//! The operator still makes every decision M7 and ADR 0029 reserve for them:
//! committing the reviewed work, authorizing `ask` validation commands and
//! approving the exact commits. Everything else runs by itself.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use switchyard_client::{ClientError, CoreClient};
use switchyard_protocol::ErrorCode;
use switchyard_protocol::methods::{
    ApprovalDecision, ApprovalsDecide, ApprovalsDecideParams, ApprovalsList, ApprovalsListParams, EventsList,
    EventsListParams, IntegrationDetail, IntegrationIdParams, IntegrationsApprove, IntegrationsApproveParams,
    IntegrationsCommitWorkspace, IntegrationsCommitWorkspaceParams, IntegrationsGet, IntegrationsList,
    IntegrationsListParams, IntegrationsSetCommand, IntegrationsSetCommandParams, IntegrationsValidate,
    IntegrationsValidateParams, PoliciesCreate, PoliciesCreateParams, ProjectsAdd, ProjectsAddParams,
    TaskDetail, TaskIdParams, TasksCreate, TasksCreateParams, TasksGet, ValidationAuthorization,
    WorkflowsRequestCorrection, WorkflowsRequestCorrectionParams, WorkflowsStart, WorkflowsStartParams,
};
use switchyard_protocol::model::{
    ApprovalStatus, IntegrationStatus, OperationCategory, PolicyConditions, PolicyDecision, RoleBinding,
    RoleBindingsPatch, TaskKind, TaskStatus,
};
use uuid::Uuid;

use common::{TestCore, git_repo};

struct Fixture {
    core: TestCore,
    client: CoreClient,
    project_id: Uuid,
    repo: PathBuf,
}

async fn fixture() -> Fixture {
    let core = TestCore::start_with(|_| {}).await;
    let (client, _rx) = core.connect().await;
    let repo = git_repo(core.dir.path(), "delivery-repo");
    let project = client
        .call::<ProjectsAdd>(ProjectsAddParams { path: repo.display().to_string() })
        .await
        .unwrap()
        .project;
    common::allow_test_command(&client, project.id, common::PASSING_TEST).await;
    Fixture { core, client, project_id: project.id, repo }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn script(spec: &Value) -> String {
    let hex: String = spec.to_string().bytes().map(|b| format!("{b:02x}")).collect();
    format!("[fake:report:{hex}]")
}

/// An agent report; an implementation names passing tests for every required
/// kind (ADR 0039), which the fixture's policy allows.
fn report(kind: &str, summary: &str) -> Value {
    let mut r = json!({ "type": kind, "summary": summary });
    if kind == "implementation" {
        r["tests"] = common::passing_tests();
    }
    r
}

fn review(result: &str) -> Value {
    json!({ "type": "review", "summary": format!("review: {result}"), "result": result })
}

/// Claude in every role: its writes ask for approval, which the tests give.
fn claude_everywhere() -> RoleBindingsPatch {
    let c = || Some(RoleBinding { provider: "claude_code".into(), model: None });
    RoleBindingsPatch { investigate: c(), implement: c(), review: c(), correct: None }
}

async fn start_task(f: &Fixture, spec: &Value) -> Uuid {
    let task = f
        .client
        .call::<TasksCreate>(TasksCreateParams {
            project_id: f.project_id,
            title: "Add the feature".into(),
            description: Some(format!("feature.txt must exist. {}", script(spec))),
            kind: Some(TaskKind::Implementation),
            priority: None,
            acceptance_criteria: vec!["feature.txt exists".into()],
            depends_on: vec![],
            parent_task_id: None,
            publish: true,
        })
        .await
        .unwrap()
        .task
        .id;
    f.client
        .call::<WorkflowsStart>(WorkflowsStartParams {
            task_id: task,
            overrides: claude_everywhere(),
            workspace_id: None,
            definition: None,
            lead: None,
        })
        .await
        .unwrap();
    task
}

async fn detail(f: &Fixture, id: Uuid) -> TaskDetail {
    f.client.call::<TasksGet>(TaskIdParams { task_id: id }).await.unwrap()
}

fn phase(d: &TaskDetail) -> String {
    let w = d.workflow.as_ref();
    format!(
        "{}/{}",
        w.map(|w| w.status.as_str()).unwrap_or("none"),
        w.and_then(|w| w.delivery.as_deref()).unwrap_or("-")
    )
}

/// Waits until the workflow is `status/delivery`, e.g. `waiting/needs_commit`.
async fn wait_phase(f: &Fixture, id: Uuid, want: &str) -> TaskDetail {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let d = detail(f, id).await;
        if phase(&d) == want {
            return d;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {want}; at {} ({:?}); steps {:?}",
            phase(&d),
            d.workflow.as_ref().and_then(|w| w.reason.clone()),
            d.assignments.iter().map(|a| (a.step_key.clone(), a.status.to_string())).collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

/// Approves the pending permission request of the running step `key` (an agent's write).
async fn approve_write(f: &Fixture, task: Uuid, key: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let d = detail(f, task).await;
        if let Some(session) =
            d.assignments.iter().find(|a| a.step_key.as_deref() == Some(key)).and_then(|a| a.session_id)
        {
            let pending = f
                .client
                .call::<ApprovalsList>(ApprovalsListParams {
                    status: Some(ApprovalStatus::Pending),
                    session_id: Some(session),
                    ..Default::default()
                })
                .await
                .unwrap()
                .approvals;
            if let Some(a) = pending.first() {
                f.client
                    .call::<ApprovalsDecide>(ApprovalsDecideParams {
                        approval_id: a.id,
                        decision: ApprovalDecision::Approve,
                        note: None,
                    })
                    .await
                    .unwrap();
                return;
            }
        }
        assert!(tokio::time::Instant::now() < deadline, "no write to approve for {key}");
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

async fn integration(f: &Fixture, d: &TaskDetail) -> IntegrationDetail {
    let id = d.workflow.as_ref().unwrap().integration_id.expect("the workflow prepared an integration");
    f.client.call::<IntegrationsGet>(IntegrationIdParams { integration_id: id }).await.unwrap()
}

/// The operator commits the reviewed work (ADR 0029: Switchyard never does).
async fn commit(f: &Fixture, d: &TaskDetail) {
    let i = integration(f, d).await.integration;
    f.client
        .call::<IntegrationsCommitWorkspace>(IntegrationsCommitWorkspaceParams {
            integration_id: i.id,
            message: "Add the feature".into(),
        })
        .await
        .unwrap();
}

async fn approve(f: &Fixture, d: &TaskDetail) -> Result<(), ClientError> {
    let i = integration(f, d).await.integration;
    f.client
        .call::<IntegrationsApprove>(IntegrationsApproveParams {
            integration_id: i.id,
            source_commit: i.source_commit.clone(),
            target_commit: i.target_commit.clone(),
        })
        .await
        .map(|_| ())
}

async fn add_command(f: &Fixture, name: &str, argv: &[&str]) -> (Uuid, String) {
    let s = f
        .client
        .call::<IntegrationsSetCommand>(IntegrationsSetCommandParams {
            project_id: f.project_id,
            command_id: None,
            name: name.into(),
            argv: argv.iter().map(|s| s.to_string()).collect(),
            timeout_secs: 60,
            enabled: true,
            position: None,
        })
        .await
        .unwrap();
    let c = s.commands.iter().find(|c| c.name == name).unwrap();
    (c.id, c.command_line.clone())
}

async fn allow(f: &Fixture, command: &str) {
    f.client
        .call::<PoliciesCreate>(PoliciesCreateParams {
            project_id: f.project_id,
            workspace_id: None,
            category: OperationCategory::Shell,
            decision: PolicyDecision::Allow,
            conditions: PolicyConditions { path_prefix: None, command: Some(command.into()) },
            description: format!("allow {command}"),
            enabled: None,
        })
        .await
        .unwrap();
}

async fn workflow_events(f: &Fixture, task: Uuid) -> Vec<String> {
    f.client
        .call::<EventsList>(EventsListParams {
            task_id: Some(task),
            after_seq: Some(0),
            limit: Some(1000),
            ..Default::default()
        })
        .await
        .unwrap()
        .events
        .into_iter()
        .map(|e| e.event_type)
        .filter(|t| t.starts_with("workflow.") || t.starts_with("integration."))
        .collect()
}

fn spec_writing(file: &str) -> Value {
    json!({
        "investigate": { "report": report("findings", "one file is needed") },
        "implement": { "tool": format!("write:{file}"), "report": report("implementation", "wrote it") },
        "review": { "report": review("approved") }
    })
}

/// One task from creation to delivered: agents, the operator's commit,
/// automatic validation (allowed by policy), the operator's approval of the
/// exact commits, M7's integration, then the task is completed.
#[tokio::test]
async fn one_task_is_delivered_after_the_operators_commit_and_approval() {
    let f = fixture().await;
    add_command(&f, "feature exists", &["/bin/test", "-f", "feature.txt"]).await;
    allow(&f, "/bin/test -f feature.txt").await;
    let main_before = git(&f.repo, &["rev-parse", "main"]);
    let task = start_task(&f, &spec_writing("feature.txt")).await;
    approve_write(&f, task, "implement#1").await;

    // Review approved: Core prepared the integration by itself, and waits for
    // the operator's commit of the reviewed work.
    let d = wait_phase(&f, task, "waiting/needs_commit").await;
    let i = integration(&f, &d).await;
    assert_eq!(i.integration.status, IntegrationStatus::Open);
    assert_eq!(i.integration.target_branch, "main");
    assert_eq!(i.integration.target_commit, main_before);
    assert!(i.runs.is_empty(), "nothing is validated before the commit");

    commit(&f, &d).await;
    // The command is allowed: validation runs by itself, then the operator decides.
    let d = wait_phase(&f, task, "waiting/awaiting_approval").await;
    let i = integration(&f, &d).await;
    assert_eq!(i.integration.status, IntegrationStatus::Ready);
    assert_eq!(i.runs.len(), 1);
    assert_eq!(i.runs[0].status.to_string(), "passed");
    assert_eq!(i.runs[0].source_commit, i.integration.source_commit);
    assert_eq!(i.runs[0].requested_by.as_deref(), Some("workflow"));
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), main_before, "nothing moved without approval");
    assert_eq!(d.task.status, TaskStatus::AwaitingReview, "not completed before delivery");

    approve(&f, &d).await.unwrap();
    let d = wait_phase(&f, task, "delivered/delivered").await;
    let i = integration(&f, &d).await.integration;
    assert_eq!(i.status, IntegrationStatus::Integrated);
    let merge = i.merge_commit.clone().unwrap();
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), merge);
    assert_eq!(
        git(&f.repo, &["log", "-1", "--format=%P", "main"]),
        format!("{main_before} {}", i.source_commit)
    );
    // The task is completed only now, in the approver's name.
    let d = wait_for_task(&f, task, TaskStatus::Completed).await;
    assert!(d.task.result.as_ref().unwrap()["note"].as_str().unwrap().contains(&merge));
    let events = workflow_events(&f, task).await;
    assert_eq!(events.iter().filter(|e| *e == "integration.created").count(), 1);
    assert_eq!(events.iter().filter(|e| *e == "integration.validation_started").count(), 1);
    assert_eq!(events.iter().filter(|e| *e == "workflow.delivered").count(), 1);
    assert!(d.workflow.unwrap().reason.unwrap().contains(&merge));
}

async fn wait_for_task(f: &Fixture, id: Uuid, status: TaskStatus) -> TaskDetail {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let d = detail(f, id).await;
        if d.task.status == status {
            return d;
        }
        assert!(tokio::time::Instant::now() < deadline, "task is {}", d.task.status);
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

/// Commands the policy asks about wait for the operator's authorization;
/// nothing runs until then.
#[tokio::test]
async fn ask_commands_wait_for_the_operators_authorization() {
    let f = fixture().await;
    let (cmd, line) = add_command(&f, "feature exists", &["/bin/test", "-f", "feature.txt"]).await;
    let task = start_task(&f, &spec_writing("feature.txt")).await;
    approve_write(&f, task, "implement#1").await;
    let d = wait_phase(&f, task, "waiting/needs_commit").await;
    commit(&f, &d).await;
    let d = wait_phase(&f, task, "waiting/awaiting_validation_authorization").await;
    let i = integration(&f, &d).await;
    assert!(i.runs.is_empty(), "an ask command never runs by itself");
    assert!(approve(&f, &d).await.is_err(), "no approval without validation");
    f.client
        .call::<IntegrationsValidate>(IntegrationsValidateParams {
            integration_id: i.integration.id,
            authorize: vec![ValidationAuthorization { command_id: cmd, command_line: line }],
        })
        .await
        .unwrap();
    let d = wait_phase(&f, task, "waiting/awaiting_approval").await;
    approve(&f, &d).await.unwrap();
    wait_phase(&f, task, "delivered/delivered").await;
}

/// A failed validation cannot be approved; the operator sends the work back,
/// the correction is reviewed again and delivery continues on the new commit.
#[tokio::test]
async fn a_failed_validation_goes_back_for_a_correction_then_delivers() {
    let f = fixture().await;
    add_command(&f, "fix exists", &["/bin/test", "-f", "fixed.txt"]).await;
    allow(&f, "/bin/test -f fixed.txt").await;
    let spec = json!({
        "investigate": { "report": report("findings", "found") },
        "implement": [ { "tool": "write:feature.txt", "report": report("implementation", "first try") },
                       { "tool": "write:fixed.txt", "report": report("implementation", "fixed") } ],
        "review": [ { "report": review("approved") }, { "report": review("approved") } ]
    });
    let task = start_task(&f, &spec).await;
    approve_write(&f, task, "implement#1").await;
    let d = wait_phase(&f, task, "waiting/needs_commit").await;
    commit(&f, &d).await;
    let d = wait_phase(&f, task, "waiting/validation_failed").await;
    let w = d.workflow.as_ref().unwrap();
    assert!(w.reason.as_deref().unwrap().contains("/bin/test -f fixed.txt"), "{:?}", w.reason);
    let err = approve(&f, &d).await.unwrap_err();
    assert!(matches!(err, ClientError::Remote(ref e) if e.code == ErrorCode::InvalidState), "{err:?}");

    f.client
        .call::<WorkflowsRequestCorrection>(WorkflowsRequestCorrectionParams {
            task_id: task,
            note: Some("Validation failed: fixed.txt is missing.".into()),
        })
        .await
        .unwrap();
    approve_write(&f, task, "implement#2").await;
    let d = wait_phase(&f, task, "waiting/needs_commit").await;
    let keys: Vec<_> = d.assignments.iter().filter_map(|a| a.step_key.clone()).collect();
    assert_eq!(keys, ["investigate#1", "implement#1", "review#1", "implement#2", "review#2"]);
    let correction = d.assignments.iter().find(|a| a.step_key.as_deref() == Some("implement#2")).unwrap();
    assert!(correction.context.as_ref().unwrap().to_string().contains("fixed.txt is missing"));
    commit(&f, &d).await;
    let d = wait_phase(&f, task, "waiting/awaiting_approval").await;
    let i = integration(&f, &d).await;
    assert_eq!(i.runs.len(), 2, "one run per source commit");
    assert_eq!(i.runs[0].status.to_string(), "passed");
    approve(&f, &d).await.unwrap();
    wait_phase(&f, task, "delivered/delivered").await;
    let lists = f.client.call::<IntegrationsList>(IntegrationsListParams {
        task_id: Some(task),
        include_closed: true,
        ..Default::default()
    });
    assert_eq!(lists.await.unwrap().integrations.len(), 1, "one integration throughout");
}

/// Without configured validation commands nothing is validated, and the
/// workflow says so; the operator's approval is the explicit decision.
#[tokio::test]
async fn without_validation_commands_the_approval_is_explicit() {
    let f = fixture().await;
    let task = start_task(&f, &spec_writing("feature.txt")).await;
    approve_write(&f, task, "implement#1").await;
    let d = wait_phase(&f, task, "waiting/needs_commit").await;
    commit(&f, &d).await;
    let d = wait_phase(&f, task, "waiting/awaiting_approval").await;
    let reason = d.workflow.as_ref().unwrap().reason.clone().unwrap();
    assert!(reason.contains("no validation commands are configured"), "{reason}");
    approve(&f, &d).await.unwrap();
    wait_phase(&f, task, "delivered/delivered").await;
}

/// Core restarts while delivery waits for approval, and again after
/// delivery: nothing is created or run twice, and the integration identity
/// and commit bindings stay the same.
#[tokio::test]
async fn restarts_and_replays_do_not_duplicate_delivery() {
    let mut f = fixture().await;
    add_command(&f, "feature exists", &["/bin/test", "-f", "feature.txt"]).await;
    allow(&f, "/bin/test -f feature.txt").await;
    let task = start_task(&f, &spec_writing("feature.txt")).await;
    approve_write(&f, task, "implement#1").await;
    let d = wait_phase(&f, task, "waiting/needs_commit").await;
    commit(&f, &d).await;
    let before = wait_phase(&f, task, "waiting/awaiting_approval").await;
    let bound = integration(&f, &before).await.integration;
    for _ in 0..2 {
        f.core.restart().await;
        f.client = f.core.connect().await.0;
        let d = wait_phase(&f, task, "waiting/awaiting_approval").await;
        let i = integration(&f, &d).await;
        assert_eq!(
            (i.integration.id, &i.integration.source_commit, &i.integration.target_commit),
            (bound.id, &bound.source_commit, &bound.target_commit)
        );
        assert_eq!(i.runs.len(), 1, "validation is not repeated after a restart");
    }
    approve(&f, &before).await.unwrap();
    wait_phase(&f, task, "delivered/delivered").await;
    f.core.restart().await;
    f.client = f.core.connect().await.0;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let d = detail(&f, task).await;
    assert_eq!(phase(&d), "delivered/delivered");
    assert_eq!(d.task.status, TaskStatus::Completed);
    let events = workflow_events(&f, task).await;
    for once in [
        "integration.created",
        "integration.validation_started",
        "integration.integrated",
        "workflow.delivered",
    ] {
        assert_eq!(events.iter().filter(|e| *e == once).count(), 1, "{once}: {events:?}");
    }
}

/// The approval is a Core-owned operation: the approving client disconnects
/// right away and the delivery still completes.
#[tokio::test]
async fn delivery_completes_when_the_approving_client_disconnects() {
    let f = fixture().await;
    let task = start_task(&f, &spec_writing("feature.txt")).await;
    approve_write(&f, task, "implement#1").await;
    let d = wait_phase(&f, task, "waiting/needs_commit").await;
    commit(&f, &d).await;
    let d = wait_phase(&f, task, "waiting/awaiting_approval").await;
    let i = integration(&f, &d).await.integration;
    let (other, _rx) = f.core.connect().await;
    let sent = tokio::spawn(async move {
        other
            .call::<IntegrationsApprove>(IntegrationsApproveParams {
                integration_id: i.id,
                source_commit: i.source_commit.clone(),
                target_commit: i.target_commit.clone(),
            })
            .await
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    sent.abort(); // drops the client and its connection
    wait_phase(&f, task, "delivered/delivered").await;
    wait_for_task(&f, task, TaskStatus::Completed).await;
}
