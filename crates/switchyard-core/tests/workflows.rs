//! Milestone 8.1: the `engineering@1` workflow drives a task through the fake
//! CLIs without any manual assignment (ADR 0033).
//!
//! Scenarios use the fakes' `[fake:report:HEX]` task scripts: per role a list
//! of turns, each an optional tool call and a final message. The fakes report
//! back the model they were launched with, which proves the configured model
//! reached the native adapter.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use serde_json::{Value, json};
use switchyard_client::CoreClient;
use switchyard_protocol::ErrorCode;
use switchyard_protocol::methods::{
    AgentsModels, AgentsModelsParams, ApprovalDecision, ApprovalsDecide, ApprovalsDecideParams,
    ApprovalsList, ApprovalsListParams, EventsList, EventsListParams, IntegrationsList,
    IntegrationsListParams, ProjectIdParams, ProjectsAdd, ProjectsAddParams, SessionIdParams, SessionsGet,
    TaskDetail, TaskIdParams, TasksAssign, TasksAssignParams, TasksCreate, TasksCreateParams, TasksGet,
    WorkflowsCancel, WorkflowsCancelParams, WorkflowsPause, WorkflowsResume, WorkflowsSetSettings,
    WorkflowsSetSettingsParams, WorkflowsSettings, WorkflowsStart, WorkflowsStartParams,
};
use switchyard_protocol::model::{
    ApprovalStatus, Role, RoleBinding, RoleBindings, RoleBindingsPatch, SessionAccess, TaskKind, TaskStatus,
    WorkflowInfo,
};
use uuid::Uuid;

use common::{TestCore, git_repo};

struct Fixture {
    core: TestCore,
    client: CoreClient,
    project_id: Uuid,
}

async fn fixture() -> Fixture {
    let core = TestCore::start_with(|_| {}).await;
    let (client, _rx) = core.connect().await;
    let repo = git_repo(core.dir.path(), "workflow-repo");
    let project = client
        .call::<ProjectsAdd>(ProjectsAddParams { path: repo.display().to_string() })
        .await
        .unwrap()
        .project;
    Fixture { core, client, project_id: project.id }
}

fn script(spec: &Value) -> String {
    let hex: String = spec.to_string().bytes().map(|b| format!("{b:02x}")).collect();
    format!("[fake:report:{hex}]")
}

fn report(kind: &str, summary: &str) -> Value {
    json!({ "type": kind, "summary": summary })
}

fn review(result: &str) -> Value {
    let mut r = json!({ "type": "review", "summary": format!("review: {result}"), "result": result });
    if result != "approved" {
        r["findings"] =
            json!([{ "severity": "medium", "title": "Needs another line", "path": "feature.txt" }]);
    }
    r
}

/// The protocol error Core answered with.
fn remote(e: switchyard_client::ClientError) -> switchyard_protocol::ProtocolError {
    match e {
        switchyard_client::ClientError::Remote(p) => p,
        other => panic!("expected an error from Core, got {other:?}"),
    }
}

fn binding(provider: &str, model: Option<&str>) -> RoleBinding {
    RoleBinding { provider: provider.into(), model: model.map(str::to_owned) }
}

async fn create_task(f: &Fixture, spec: &Value) -> Uuid {
    f.client
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
        .id
}

async fn start(f: &Fixture, task: Uuid, overrides: RoleBindingsPatch) -> Result<WorkflowInfo, ErrorCode> {
    f.client
        .call::<WorkflowsStart>(WorkflowsStartParams {
            task_id: task,
            overrides,
            workspace_id: None,
            definition: None,
            lead: None,
        })
        .await
        .map(|r| r.workflow)
        .map_err(|e| remote(e).code)
}

async fn detail(f: &Fixture, id: Uuid) -> TaskDetail {
    f.client.call::<TasksGet>(TaskIdParams { task_id: id }).await.unwrap()
}

async fn wait_for(f: &Fixture, id: Uuid, what: &str, pred: impl Fn(&TaskDetail) -> bool) -> TaskDetail {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let d = detail(f, id).await;
        if pred(&d) {
            return d;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}; workflow {:?}; steps {:?}; messages {:?}",
            d.workflow.as_ref().map(|w| (&w.status, &w.step_key, &w.reason)),
            steps(&d),
            d.messages.iter().map(|m| (m.kind, m.payload.clone())).collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

fn status(d: &TaskDetail) -> &str {
    d.workflow.as_ref().map(|w| w.status.as_str()).unwrap_or("none")
}

/// `(step key, assignment status)` in creation order.
fn steps(d: &TaskDetail) -> Vec<(String, String)> {
    d.assignments
        .iter()
        .map(|a| (a.step_key.clone().unwrap_or_else(|| "manual".into()), a.status.to_string()))
        .collect()
}

fn keys(d: &TaskDetail) -> Vec<String> {
    steps(d).into_iter().map(|(k, _)| k).collect()
}

/// The review approved and delivery reached its first gate (M8.2: the
/// workflow continues into delivery instead of finishing).
async fn wait_delivery(f: &Fixture, id: Uuid) -> TaskDetail {
    wait_for(f, id, "the delivery gate", |d| {
        d.workflow.as_ref().is_some_and(|w| {
            w.stage.as_deref() == Some("delivery")
                && w.status == "waiting"
                && w.delivery.as_deref().is_some_and(|p| p != "preparing")
        })
    })
    .await
}

fn delivery(d: &TaskDetail) -> &str {
    d.workflow.as_ref().and_then(|w| w.delivery.as_deref()).unwrap_or("none")
}

async fn wait_status(f: &Fixture, id: Uuid, want: &str) -> TaskDetail {
    wait_for(f, id, &format!("workflow {want}"), |d| status(d) == want).await
}

/// Approves the pending permission request of the running step `key`.
async fn approve_step(f: &Fixture, task: Uuid, key: &str) {
    let d = wait_for(f, task, &format!("{key} running"), |d| {
        d.assignments.iter().any(|a| a.step_key.as_deref() == Some(key) && a.session_id.is_some())
    })
    .await;
    let session =
        d.assignments.iter().find(|a| a.step_key.as_deref() == Some(key)).unwrap().session_id.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let approval = loop {
        let list = f
            .client
            .call::<ApprovalsList>(ApprovalsListParams {
                status: Some(ApprovalStatus::Pending),
                session_id: Some(session),
                ..Default::default()
            })
            .await
            .unwrap()
            .approvals;
        if let Some(a) = list.into_iter().next() {
            break a;
        }
        assert!(tokio::time::Instant::now() < deadline, "no approval for {key}");
        tokio::time::sleep(Duration::from_millis(40)).await;
    };
    f.client
        .call::<ApprovalsDecide>(ApprovalsDecideParams {
            approval_id: approval.id,
            decision: ApprovalDecision::Approve,
            note: None,
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
        .filter(|t| t.starts_with("workflow."))
        .collect()
}

/// One task: investigation, implementation (with a write the operator
/// approves), a review that requests changes, a correction with the
/// findings, a re-review that approves: the delivery gate, with no
/// `tasks.assign` call at all.
#[tokio::test]
async fn one_task_reaches_the_delivery_gate_through_a_correction_round() {
    let f = fixture().await;
    let spec = json!({
        "investigate": { "report": { "type": "findings", "summary": "One new file is needed.",
            "findings": [{ "severity": "info", "title": "No feature file", "path": "feature.txt" }] } },
        "implement": [
            { "tool": "write:feature.txt", "report": report("implementation", "Added feature.txt.") },
            { "report": report("implementation", "Addressed the review.") }
        ],
        "review": [ { "report": review("changes_requested") }, { "report": review("approved") } ]
    });
    let task = create_task(&f, &spec).await;
    let wf = start(&f, task, RoleBindingsPatch::default()).await.unwrap();
    assert_eq!((wf.definition.as_str(), wf.version), ("engineering", 1));
    assert_eq!(wf.bindings.implement.provider, "codex", "Switchyard's default bindings");

    // Codex asks before writing: the normal approval path still applies.
    approve_step(&f, task, "implement#1").await;
    let d = wait_delivery(&f, task).await;
    assert_eq!(keys(&d), ["investigate#1", "implement#1", "review#1", "implement#2", "review#2"]);
    assert!(d.assignments.iter().all(|a| a.origin == "workflow" && a.status.to_string() == "reported"));
    let w = d.workflow.as_ref().unwrap();
    assert_eq!((w.stage.as_deref(), w.corrections), (Some("delivery"), 1));
    // The reviewed work is uncommitted: only the operator commits (ADR 0029).
    assert_eq!(delivery(&d), "needs_commit", "{:?}", w.reason);

    // The correction got the review's findings in its context.
    let correction = d.assignments.iter().find(|a| a.step_key.as_deref() == Some("implement#2")).unwrap();
    let findings = &correction.context.as_ref().unwrap()["review_findings"];
    assert!(findings.to_string().contains("Needs another line"), "{findings}");

    // Reviews ran read-only; implementation could write.
    for a in &d.assignments {
        let session =
            f.client.call::<SessionsGet>(SessionIdParams { session_id: a.session_id.unwrap() }).await;
        let access = session.unwrap().session.access;
        let want = if a.role == Role::Review { SessionAccess::Review } else { SessionAccess::Write };
        assert_eq!(access, want, "{:?}", a.step_key);
    }

    // The delivery gate: an integration was prepared, nothing was committed,
    // completed or integrated automatically.
    assert_eq!(d.task.status, TaskStatus::AwaitingReview);
    let integrations = f
        .client
        .call::<IntegrationsList>(IntegrationsListParams { task_id: Some(task), ..Default::default() })
        .await
        .unwrap();
    assert_eq!(integrations.integrations.len(), 1);
    assert_eq!(integrations.integrations[0].status.to_string(), "open");
    assert_eq!(d.workflow.as_ref().unwrap().integration_id, Some(integrations.integrations[0].id));
    let events = workflow_events(&f, task).await;
    assert_eq!(events.first().map(String::as_str), Some("workflow.started"));
    assert_eq!(events.iter().filter(|e| *e == "workflow.stage_changed").count(), 5);
    assert!(events.contains(&"workflow.delivery_changed".to_owned()), "{events:?}");
    assert_eq!(events.last().map(String::as_str), Some("workflow.waiting"));
    // The approved write really happened in the workflow's workspace.
    let written = std::fs::read_to_string(workspace_path(&f, &d).join("feature.txt")).unwrap();
    assert_eq!(written, "written by fake codex\n");
}

fn workspace_path(f: &Fixture, d: &TaskDetail) -> std::path::PathBuf {
    let _ = f;
    let ctx = d.assignments[0].context.as_ref().unwrap();
    std::path::PathBuf::from(ctx["workspace_path"].as_str().unwrap())
}

/// After two correction rounds the workflow stops and asks; resuming allows
/// exactly one more round.
#[tokio::test]
async fn correction_rounds_are_bounded_then_the_operator_decides() {
    let f = fixture().await;
    let implement: Vec<Value> =
        (0..4).map(|i| json!({ "report": report("implementation", &format!("try {i}")) })).collect();
    let spec = json!({
        "investigate": { "report": report("findings", "found") },
        "implement": implement,
        "review": [ { "report": review("changes_requested") }, { "report": review("changes_requested") },
                    { "report": review("changes_requested") }, { "report": review("approved") } ]
    });
    let task = create_task(&f, &spec).await;
    start(&f, task, claude_everywhere()).await.unwrap();
    let d = wait_status(&f, task, "waiting").await;
    assert_eq!(
        keys(&d),
        ["investigate#1", "implement#1", "review#1", "implement#2", "review#2", "implement#3", "review#3"]
    );
    let w = d.workflow.as_ref().unwrap();
    assert!(w.reason.as_deref().unwrap().contains("2 correction rounds"), "{:?}", w.reason);
    assert_eq!(w.resume_action.as_deref(), Some("start implement#4"));
    f.client.call::<WorkflowsResume>(TaskIdParams { task_id: task }).await.unwrap();
    let d = wait_delivery(&f, task).await;
    assert_eq!(keys(&d).last().map(String::as_str), Some("review#4"));
}

fn claude_everywhere() -> RoleBindingsPatch {
    RoleBindingsPatch {
        investigate: Some(binding("claude_code", None)),
        implement: Some(binding("claude_code", None)),
        review: Some(binding("claude_code", None)),
        correct: None,
    }
}

/// A turn without a usable report gets at most two automatic repair notes;
/// a usable report afterwards continues the workflow.
#[tokio::test]
async fn unusable_reports_get_two_repair_notes_then_wait() {
    let f = fixture().await;
    let spec = json!({
        "investigate": [ { "text": "I looked around." }, { "text": "Still no block." },
                         { "text": "Nothing structured." }, { "report": report("findings", "finally") } ],
        "implement": { "report": report("implementation", "done") },
        "review": { "report": review("approved") }
    });
    let task = create_task(&f, &spec).await;
    start(&f, task, claude_everywhere()).await.unwrap();
    let d = wait_status(&f, task, "waiting").await;
    let w = d.workflow.as_ref().unwrap();
    assert!(w.reason.as_deref().unwrap().contains("2 repair notes"), "{:?}", w.reason);
    let notes = d.messages.iter().filter(|m| m.kind.to_string() == "operator_note").count();
    assert_eq!(notes, 2, "exactly two automatic repair notes");
    assert_eq!(keys(&d), ["investigate#1"], "no new assignment for an unusable report");
    // Resuming sends one more repair note; the agent then reports.
    f.client.call::<WorkflowsResume>(TaskIdParams { task_id: task }).await.unwrap();
    let d = wait_delivery(&f, task).await;
    assert_eq!(keys(&d), ["investigate#1", "implement#1", "review#1"]);
}

/// A session that fails is retried once with the same provider and model,
/// then the workflow waits; a missing report is never taken as success.
#[tokio::test]
async fn a_failed_session_is_retried_once_then_waits() {
    let f = fixture().await;
    let spec = json!({
        "investigate": { "report": report("findings", "found") },
        "implement": { "report": report("implementation", "done") },
        "review": { "report": review("approved") }
    });
    let task = create_task(&f, &spec).await;
    let wf = start(&f, task, claude_everywhere()).await.unwrap();
    // The fake Claude exits with an auth error in this workspace.
    let ws = workspace_dir(&f, wf.workspace_id.unwrap()).await;
    std::fs::write(ws.join(".fake-claude-fail"), "").unwrap();
    let d = wait_status(&f, task, "waiting").await;
    assert_eq!(
        steps(&d),
        [
            ("investigate#1".to_owned(), "failed".to_owned()),
            ("investigate#1@2".to_owned(), "failed".to_owned())
        ]
    );
    assert!(
        d.assignments.iter().all(|a| a.provider == "claude_code" && a.model.is_none()),
        "no provider fallback"
    );
    assert!(d.workflow.unwrap().reason.unwrap().contains("already retried"));
    std::fs::remove_file(ws.join(".fake-claude-fail")).unwrap();
    f.client.call::<WorkflowsResume>(TaskIdParams { task_id: task }).await.unwrap();
    let d = wait_delivery(&f, task).await;
    assert_eq!(keys(&d)[..3], ["investigate#1", "investigate#1@2", "investigate#1@3"]);
}

async fn workspace_dir(f: &Fixture, id: Uuid) -> std::path::PathBuf {
    let w = f
        .client
        .call::<switchyard_protocol::methods::WorkspacesGet>(
            switchyard_protocol::methods::WorkspaceIdParams { workspace_id: id },
        )
        .await
        .unwrap()
        .workspace;
    std::path::PathBuf::from(w.path)
}

/// Core stops while a step runs: after the restart the workflow waits (the
/// agent is not restarted automatically), replays create nothing twice, and
/// resuming continues to the delivery gate.
#[tokio::test]
async fn a_restart_interrupts_without_duplicating_steps() {
    let mut f = fixture().await;
    let spec = json!({
        "investigate": { "report": report("findings", "found") },
        // implement#1 waits for an approval that never comes before the restart.
        "implement": [ { "tool": "write:feature.txt", "report": report("implementation", "first") },
                       { "report": report("implementation", "after the restart") } ],
        "review": { "report": review("approved") }
    });
    let task = create_task(&f, &spec).await;
    start(&f, task, claude_everywhere()).await.unwrap();
    wait_for(&f, task, "implement#1 running", |d| {
        d.assignments.iter().any(|a| a.step_key.as_deref() == Some("implement#1") && a.session_id.is_some())
    })
    .await;
    f.core.restart().await;
    let (client, _rx) = f.core.connect().await;
    f.client = client;
    let d = wait_status(&f, task, "waiting").await;
    assert!(d.workflow.as_ref().unwrap().reason.as_deref().unwrap().contains("Core restarted"));
    assert_eq!(keys(&d), ["investigate#1", "implement#1"], "nothing restarted automatically");
    // A second restart replays the same events: still nothing new.
    f.core.restart().await;
    let (client, _rx) = f.core.connect().await;
    f.client = client;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let d = detail(&f, task).await;
    assert_eq!((status(&d), keys(&d).len()), ("waiting", 2));

    f.client.call::<WorkflowsResume>(TaskIdParams { task_id: task }).await.unwrap();
    let d = wait_delivery(&f, task).await;
    assert_eq!(keys(&d), ["investigate#1", "implement#1", "implement#1@2", "review#1"]);
    // And after completion, a restart changes nothing.
    f.core.restart().await;
    let (client, _rx) = f.core.connect().await;
    f.client = client;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let after = detail(&f, task).await;
    assert_eq!((status(&after), keys(&after)), ("waiting", keys(&d)));
    assert_eq!(
        after.workflow.unwrap().integration_id,
        d.workflow.unwrap().integration_id,
        "same integration"
    );
}

/// Two resumes at the same moment: exactly one creates the next step.
#[tokio::test]
async fn concurrent_resumes_create_one_step() {
    let f = fixture().await;
    let spec = json!({
        "investigate": { "report": report("findings", "found") },
        "implement": [ { "report": report("implementation", "done") },
                       { "report": report("implementation", "corrected") } ],
        "review": [ { "report": review("rejected") }, { "report": review("approved") } ]
    });
    let task = create_task(&f, &spec).await;
    start(&f, task, claude_everywhere()).await.unwrap();
    let d = wait_status(&f, task, "waiting").await;
    assert!(d.workflow.unwrap().reason.unwrap().contains("rejected"));
    let (a, b) = tokio::join!(
        f.client.call::<WorkflowsResume>(TaskIdParams { task_id: task }),
        f.client.call::<WorkflowsResume>(TaskIdParams { task_id: task })
    );
    assert_eq!([a.is_ok(), b.is_ok()].iter().filter(|ok| **ok).count(), 1, "{a:?} {b:?}");
    let d = wait_delivery(&f, task).await;
    assert_eq!(keys(&d), ["investigate#1", "implement#1", "review#1", "implement#2", "review#2"]);
}

/// The configured model is passed to the native adapter: each fake reports
/// the model it was launched with, and Core records it as the session's.
#[tokio::test]
async fn configured_models_reach_the_native_adapters() {
    let f = fixture().await;
    let catalog = f
        .client
        .call::<AgentsModels>(AgentsModelsParams { provider: "codex".into(), refresh: true })
        .await
        .unwrap()
        .catalog;
    let ids: Vec<_> = catalog.models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["fake-codex-default", "fake-codex-large"], "from model/list; hidden models excluded");
    let claude = f
        .client
        .call::<AgentsModels>(AgentsModelsParams { provider: "claude_code".into(), refresh: false })
        .await
        .unwrap()
        .catalog;
    assert_eq!(claude.models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["fable", "opus", "sonnet"]);

    let spec = json!({
        "investigate": { "report": report("findings", "found") },
        "implement": { "report": report("implementation", "done") },
        "review": { "report": review("approved") }
    });
    let task = create_task(&f, &spec).await;
    let overrides = RoleBindingsPatch {
        investigate: Some(binding("claude_code", Some("opus"))),
        implement: Some(binding("codex", Some("fake-codex-large"))),
        review: Some(binding("claude_code", Some("claude-sonnet-4-5"))),
        correct: None,
    };
    start(&f, task, overrides).await.unwrap();
    let d = wait_delivery(&f, task).await;
    let models: Vec<_> = d
        .assignments
        .iter()
        .map(|a| (a.provider.as_str(), a.model.clone(), a.actual_model.clone()))
        .collect();
    assert_eq!(
        models,
        [
            ("claude_code", Some("opus".into()), Some("opus".into())),
            ("codex", Some("fake-codex-large".into()), Some("fake-codex-large".into())),
            ("claude_code", Some("claude-sonnet-4-5".into()), Some("claude-sonnet-4-5".into())),
        ]
    );
}

/// Unsupported models and providers are refused before anything starts.
#[tokio::test]
async fn unsupported_models_and_providers_are_refused() {
    let f = fixture().await;
    let task = create_task(&f, &json!({})).await;
    let bad = |b: RoleBinding| RoleBindingsPatch { review: Some(b), ..Default::default() };
    let err = f
        .client
        .call::<WorkflowsStart>(WorkflowsStartParams {
            task_id: task,
            overrides: bad(binding("claude_code", Some("gpt-6-luna"))),
            workspace_id: None,
            definition: None,
            lead: None,
        })
        .await
        .map_err(remote)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidParams);
    assert!(
        err.message.contains("does not offer model") && err.message.starts_with("review:"),
        "{}",
        err.message
    );
    assert_eq!(
        start(&f, task, bad(binding("codex", Some("nonexistent")))).await.unwrap_err(),
        ErrorCode::InvalidParams
    );
    assert_eq!(start(&f, task, bad(binding("gemini", None))).await.unwrap_err(), ErrorCode::InvalidParams);
    let d = detail(&f, task).await;
    assert!(d.workflow.is_none() && d.assignments.is_empty(), "nothing was started");
    // A manual takeover assignment validates its model the same way.
    let ws = f
        .client
        .call::<switchyard_protocol::methods::WorkspacesCreate>(
            switchyard_protocol::methods::WorkspacesCreateParams {
                project_id: f.project_id,
                name: None,
                base_ref: None,
            },
        )
        .await
        .unwrap()
        .workspace;
    let err = f
        .client
        .call::<TasksAssign>(TasksAssignParams {
            task_id: task,
            role: Role::Investigate,
            provider: "codex".into(),
            workspace_id: ws.id,
            start: false,
            model: Some("gpt-made-up".into()),
        })
        .await
        .map_err(remote)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidParams);
}

/// Project bindings are saved and journaled; a running workflow keeps the
/// bindings it started with when the project's settings change.
#[tokio::test]
async fn project_bindings_are_snapshotted_when_a_workflow_starts() {
    let f = fixture().await;
    let defaults =
        f.client.call::<WorkflowsSettings>(ProjectIdParams { project_id: f.project_id }).await.unwrap();
    assert_eq!(defaults.revision, 0);
    let mine = RoleBindings {
        investigate: binding("claude_code", Some("sonnet")),
        implement: binding("claude_code", Some("opus")),
        review: binding("claude_code", Some("fable")),
        correct: None,
    };
    let saved = f
        .client
        .call::<WorkflowsSetSettings>(WorkflowsSetSettingsParams {
            project_id: f.project_id,
            bindings: mine.clone(),
        })
        .await
        .unwrap();
    assert_eq!((saved.revision, saved.bindings.clone()), (1, mine.clone()));
    assert!(
        saved.warnings.iter().any(|w| w.contains("both use")),
        "same provider for implementation and review"
    );

    let spec = json!({
        "investigate": { "report": report("findings", "found") },
        // implement#1 waits for an approval, so the settings change lands mid-run.
        "implement": { "tool": "write:feature.txt", "report": report("implementation", "done") },
        "review": { "report": review("approved") }
    });
    let task = create_task(&f, &spec).await;
    let wf = start(&f, task, RoleBindingsPatch::default()).await.unwrap();
    assert_eq!(wf.bindings, mine, "the project's bindings, snapshotted");
    wait_for(&f, task, "implement#1 running", |d| {
        d.assignments.iter().any(|a| a.step_key.as_deref() == Some("implement#1") && a.session_id.is_some())
    })
    .await;
    let changed = RoleBindings { review: binding("codex", None), ..mine.clone() };
    f.client
        .call::<WorkflowsSetSettings>(WorkflowsSetSettingsParams {
            project_id: f.project_id,
            bindings: changed,
        })
        .await
        .unwrap();
    approve_step(&f, task, "implement#1").await;
    let d = wait_delivery(&f, task).await;
    let review = d.assignments.iter().find(|a| a.step_key.as_deref() == Some("review#1")).unwrap();
    assert_eq!((review.provider.as_str(), review.model.as_deref()), ("claude_code", Some("fable")));
    let events = f
        .client
        .call::<EventsList>(EventsListParams {
            project_id: Some(f.project_id),
            event_types: vec!["workflow.settings_changed".into()],
            after_seq: Some(0),
            limit: Some(100),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(events.events.len(), 2);
}

/// Pause holds the next step; cancel stops the workflow and its assignment;
/// a manual takeover is refused while the workflow runs.
#[tokio::test]
async fn pause_cancel_and_takeover() {
    let f = fixture().await;
    let spec = json!({
        "investigate": { "report": report("findings", "found") },
        "implement": { "tool": "write:feature.txt", "report": report("implementation", "done") },
        "review": { "report": review("approved") }
    });
    let task = create_task(&f, &spec).await;
    let wf = start(&f, task, claude_everywhere()).await.unwrap();
    let paused = f.client.call::<WorkflowsPause>(TaskIdParams { task_id: task }).await;
    // The investigation may already be done; either way nothing new starts while paused.
    if paused.is_ok() {
        let d = wait_for(&f, task, "investigation reported", |d| {
            d.assignments.first().is_some_and(|a| a.status.to_string() == "reported")
        })
        .await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let d2 = detail(&f, task).await;
        assert_eq!((status(&d2), keys(&d2).len()), ("paused", d.assignments.len()));
        f.client.call::<WorkflowsResume>(TaskIdParams { task_id: task }).await.unwrap();
    }
    wait_for(&f, task, "implement#1 running", |d| {
        d.assignments.iter().any(|a| a.step_key.as_deref() == Some("implement#1") && a.session_id.is_some())
    })
    .await;
    let takeover = f
        .client
        .call::<TasksAssign>(TasksAssignParams {
            task_id: task,
            role: Role::Implement,
            provider: "claude_code".into(),
            workspace_id: wf.workspace_id.unwrap(),
            start: false,
            model: None,
        })
        .await
        .map_err(remote)
        .unwrap_err();
    assert_eq!(takeover.code, ErrorCode::InvalidState);
    assert!(takeover.message.contains("workflow is running"));
    f.client
        .call::<WorkflowsCancel>(WorkflowsCancelParams { task_id: task, reason: Some("not needed".into()) })
        .await
        .unwrap();
    let d = wait_for(&f, task, "the assignment cancelled", |d| {
        d.assignments.iter().all(|a| a.status.to_string() != "active" && a.status.to_string() != "pending")
    })
    .await;
    assert_eq!(status(&d), "cancelled");
    assert!(workflow_events(&f, task).await.contains(&"workflow.cancelled".to_owned()));
}
