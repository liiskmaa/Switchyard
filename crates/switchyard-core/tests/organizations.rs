//! Milestone 8.3: agent organizations (ADRs 0035–0036) through the fake CLIs.
//!
//! The fakes follow scripts: supervisor turns answer with the directive the
//! script gives, workers run the script's tools and report. These tests prove
//! Switchyard's machinery — creation of agents and subtasks, supervision
//! checkpoints with Core-verified evidence, interruption of out-of-scope work,
//! corrections delivered at the next turn, a multi-level hierarchy, limits,
//! the review gate and the hand-over to M7 delivery. They do not show actual
//! model collaboration: the fakes' "decisions" are scripted.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use serde_json::{Value, json};
use switchyard_client::CoreClient;
use switchyard_protocol::methods::{
    EventsList, EventsListParams, IntegrationIdParams, IntegrationsApprove, IntegrationsApproveParams,
    IntegrationsCommitWorkspace, IntegrationsCommitWorkspaceParams, IntegrationsGet, OrganizationsAnswer,
    OrganizationsAnswerParams, OrganizationsGet, OrganizationsSetSettings, OrganizationsSetSettingsParams,
    OrganizationsSettings, PoliciesCreate, PoliciesCreateParams, ProjectIdParams, ProjectsAdd,
    ProjectsAddParams, TaskDetail, TaskIdParams, TasksCreate, TasksCreateParams, TasksGet, WorkflowsStart,
    WorkflowsStartParams,
};
use switchyard_protocol::model::{
    OperationCategory, OrganizationInfo, PolicyConditions, PolicyDecision, RoleBinding, TaskKind, TaskStatus,
};
use uuid::Uuid;

use common::{TestCore, git_repo};

struct Fixture {
    core: TestCore,
    client: CoreClient,
    project_id: Uuid,
    repo: std::path::PathBuf,
}

async fn fixture() -> Fixture {
    let core = TestCore::start_with(|_| {}).await;
    let (client, _rx) = core.connect().await;
    let repo = git_repo(core.dir.path(), "org-repo");
    let project = client
        .call::<ProjectsAdd>(ProjectsAddParams { path: repo.display().to_string() })
        .await
        .unwrap()
        .project;
    // In-scope writes need no approval; anything else still asks.
    for (category, prefix) in [
        (OperationCategory::FileWrite, "src"),
        (OperationCategory::FileEdit, "src"),
        (OperationCategory::FileDelete, "src"),
        (OperationCategory::FileWrite, "docs"),
    ] {
        client
            .call::<PoliciesCreate>(PoliciesCreateParams {
                project_id: project.id,
                workspace_id: None,
                category,
                decision: PolicyDecision::Allow,
                conditions: PolicyConditions { path_prefix: Some(prefix.into()), command: None },
                description: format!("agents may change {prefix}/"),
                enabled: None,
            })
            .await
            .unwrap();
    }
    Fixture { core, client, project_id: project.id, repo }
}

fn hex(v: &Value) -> String {
    v.to_string().bytes().map(|b| format!("{b:02x}")).collect()
}

fn script(spec: &Value) -> String {
    format!("[fake:report:{}]", hex(spec))
}

/// A supervisor turn answering with this directive.
fn directive(d: Value) -> Value {
    json!({ "text": format!("Checked the work.\n\n```switchyard-org\n{d}\n```") })
}

fn implementation(summary: &str, files: &[&str]) -> Value {
    json!({ "type": "implementation", "summary": summary, "changed_files": files,
            "acceptance": [{ "criterion": "src/greet.txt greets", "met": true }] })
}

/// The demo scenario (also used in the desktop demonstration).
pub fn scenario() -> String {
    let coder = json!({ "implement": [
        // Turn 1 starts an out-of-scope file: Switchyard interrupts it.
        { "tools": ["write:src/greet.txt", "write:scratch.txt"], "report": implementation("Added the greeting", &["src/greet.txt"]) },
        // Turn 2, after the lead's correction.
        { "tool": "edit:src/greet.txt", "report": implementation("Greeting in Estonian, only in src/", &["src/greet.txt"]) },
    ]});
    let writer = json!({ "implement": [
        { "tool": "write:docs/greet.md", "report": implementation("Documented the greeting", &["docs/greet.md"]) },
    ]});
    let reviewer = json!({ "review": [
        { "report": { "type": "review", "summary": "The greeting and its documentation are correct.", "result": "approved",
            "acceptance": [{ "criterion": "Greets in Estonian", "met": true }] } },
    ]});
    let docs_lead = json!({ "supervise": [
        directive(json!({ "summary": "One writer is enough for the API documentation.",
            "agents": [{ "key": "writer", "title": "API documentation writer", "kind": "specialist",
                "responsibility": "Write the greeting's documentation", "scope": ["docs"], "capabilities": ["documentation"],
                "provider": "claude_code" }],
            "tasks": [{ "key": "api-docs", "title": "Document the greeting", "work": "documentation", "owner": "writer",
                "description": format!("Write docs/greet.md. {}", script(&writer)),
                "acceptance": ["docs/greet.md describes the greeting"] }] })),
        directive(json!({ "summary": "The documentation is accurate.",
            "assessments": [{ "task": "api-docs", "verdict": "accept", "evidence": "Read docs/greet.md." }] })),
        directive(json!({ "summary": "Documentation complete. Outstanding: none.", "done": true })),
    ]});
    let lead = json!({ "supervise": [
        // L1: plan.
        directive(json!({ "summary": "A small change: one implementation specialist.",
            "agents": [{ "key": "impl", "title": "Implementation specialist", "kind": "specialist",
                "responsibility": "Implement the greeting", "scope": ["src"], "capabilities": ["coding"], "provider": "codex" }],
            "tasks": [{ "key": "code", "title": "Implement the greeting", "work": "implementation", "owner": "impl",
                "description": format!("Create src/greet.txt. {}", script(&coder)),
                "acceptance": ["src/greet.txt greets in Estonian"] }],
            "context": [{ "kind": "decision", "status": "decided", "text": "The greeting lives in src/greet.txt." }] })),
        // L2: the interrupted out-of-scope turn.
        directive(json!({ "summary": "scratch.txt is outside the agreed scope.",
            "assessments": [{ "task": "code", "verdict": "correct",
                "instructions": "Keep every change inside src/. Do not create scratch.txt; finish src/greet.txt.",
                "evidence": "Switchyard observed the write to scratch.txt and interrupted the turn." }] })),
        // L3: accept, then grow the organization.
        directive(json!({ "summary": "The implementation meets its criteria. Documentation and an independent review next.",
            "assessments": [{ "task": "code", "verdict": "accept", "evidence": "Read src/greet.txt; only src/ changed." }],
            "agents": [
                { "key": "docs-lead", "title": "Documentation supervisor", "kind": "supervisor",
                  "responsibility": "Deliver the documentation", "scope": ["docs"], "capabilities": ["documentation"], "provider": "claude_code" },
                { "key": "reviewer", "title": "Independent reviewer", "kind": "specialist",
                  "responsibility": "Review the final result", "capabilities": ["review"], "provider": "claude_code" }],
            "tasks": [
                { "key": "docs", "title": "Document the greeting", "work": "coordination", "owner": "docs-lead",
                  "description": format!("Organize the documentation. {}", script(&docs_lead)), "depends_on": ["code"],
                  "acceptance": ["The greeting is documented in docs/"] },
                { "key": "review", "title": "Review the final result", "work": "review", "owner": "reviewer",
                  "description": format!("Review src/ and docs/. {}", script(&reviewer)), "depends_on": ["docs"],
                  "acceptance": ["The objective's criteria are met"] }],
            "context": [{ "kind": "risk", "text": "The greeting is not tested automatically." }] })),
        // L4: the documentation supervisor reports its scope.
        directive(json!({ "summary": "Documentation accepted.",
            "assessments": [{ "task": "docs", "verdict": "accept", "evidence": "docs/greet.md exists and matches." }] })),
        // L5: the review.
        directive(json!({ "summary": "The independent review approved.",
            "assessments": [{ "task": "review", "verdict": "accept" }] })),
        // L6: everything is finished.
        directive(json!({ "summary": "The greeting is implemented, documented and reviewed. Outstanding: no automated test.", "done": true })),
    ]});
    format!("Greet users in Estonian. {}", script(&lead))
}

async fn create_task(f: &Fixture, description: String) -> Uuid {
    f.client
        .call::<TasksCreate>(TasksCreateParams {
            project_id: f.project_id,
            title: "Greet users in Estonian".into(),
            description: Some(description),
            kind: Some(TaskKind::Implementation),
            priority: None,
            acceptance_criteria: vec!["Greets in Estonian".into(), "Documented".into()],
            depends_on: vec![],
            parent_task_id: None,
            publish: true,
        })
        .await
        .unwrap()
        .task
        .id
}

async fn start(f: &Fixture, task: Uuid) {
    f.client
        .call::<WorkflowsStart>(WorkflowsStartParams {
            task_id: task,
            overrides: Default::default(),
            workspace_id: None,
            definition: Some("organization".into()),
            lead: Some(RoleBinding { provider: "codex".into(), model: None }),
        })
        .await
        .unwrap();
}

async fn org(f: &Fixture, task: Uuid) -> OrganizationInfo {
    f.client.call::<OrganizationsGet>(TaskIdParams { task_id: task }).await.unwrap().organization.unwrap()
}

async fn detail(f: &Fixture, id: Uuid) -> TaskDetail {
    f.client.call::<TasksGet>(TaskIdParams { task_id: id }).await.unwrap()
}

async fn wait_org(
    f: &Fixture,
    task: Uuid,
    what: &str,
    pred: impl Fn(&OrganizationInfo, &TaskDetail) -> bool,
) -> (OrganizationInfo, TaskDetail) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    loop {
        let (o, d) = (org(f, task).await, detail(f, task).await);
        if pred(&o, &d) {
            return (o, d);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}: status {} ({:?}); activity {:?}; tasks {:?}; context {:?}",
            o.status,
            o.reason,
            o.activity,
            o.tasks.iter().map(|t| (&t.key, &t.state)).collect::<Vec<_>>(),
            o.context.iter().rev().take(6).map(|c| (&c.kind, &c.text)).collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn events(f: &Fixture, prefix: &str) -> Vec<Value> {
    f.client
        .call::<EventsList>(EventsListParams { limit: Some(1000), ..Default::default() })
        .await
        .unwrap()
        .events
        .into_iter()
        .filter(|e| e.event_type.starts_with(prefix))
        .map(|e| json!({ "type": e.event_type, "payload": e.payload }))
        .collect()
}

#[tokio::test]
async fn a_dynamic_organization_supervises_corrects_and_delivers() {
    let f = fixture().await;
    let task = create_task(&f, scenario()).await;
    start(&f, task).await;

    // The lead planned one specialist; Core chose the provider it asked for and says why.
    let (o, _) = wait_org(&f, task, "the delivery stage", |_, d| {
        d.workflow.as_ref().is_some_and(|w| {
            w.stage.as_deref() == Some("delivery") && w.delivery.as_deref() == Some("needs_commit")
        })
    })
    .await;
    let keys: Vec<&str> = o.agents.iter().map(|a| a.key.as_str()).collect();
    assert_eq!(keys, ["lead", "impl", "docs-lead", "reviewer", "writer"]);
    let writer = o.agents.iter().find(|a| a.key == "writer").unwrap();
    assert_eq!(
        (writer.depth, writer.reports_to.as_deref()),
        (3, Some("docs-lead")),
        "three levels of reporting"
    );
    assert!(
        o.agents.iter().find(|a| a.key == "impl").unwrap().selection.contains("requested by its supervisor")
    );
    assert!(
        o.tasks.iter().all(|t| t.state == "accepted"),
        "{:?}",
        o.tasks.iter().map(|t| (&t.key, &t.state)).collect::<Vec<_>>()
    );
    assert_eq!(o.status, "done");
    assert!(o.final_summary.as_deref().unwrap().contains("Outstanding"));

    // The out-of-scope write was interrupted before it happened, and corrected.
    let workspace = f
        .core
        .dir
        .path()
        .join("data")
        .join("workspaces")
        .join(f.project_id.to_string())
        .join(o.workspace_id.to_string());
    assert!(!workspace.join("scratch.txt").exists(), "the out-of-scope file was never written");
    let greet = std::fs::read_to_string(workspace.join("src/greet.txt")).unwrap();
    assert!(greet.contains("edited by fake codex"), "the correction turn ran: {greet}");
    assert!(workspace.join("docs/greet.md").exists());
    let observed = events(&f, "organization.observed").await;
    assert!(
        observed.iter().any(|e| e["payload"]["interrupted"] == true
            && e["payload"]["text"].as_str().unwrap().contains("scratch.txt")),
        "{observed:?}"
    );
    let assessed = events(&f, "organization.assessed").await;
    assert!(assessed.iter().any(|e| e["payload"]["verdict"] == "correct" && e["payload"]["key"] == "code"));
    let work = events(&f, "organization.work_started").await;
    let corrected = work
        .iter()
        .find(|e| {
            e["payload"]["key"] == "code" && !e["payload"]["instructions"].as_array().unwrap().is_empty()
        })
        .unwrap();
    assert!(corrected["payload"]["instructions"][0].as_str().unwrap().contains("inside src/"));

    // Shared context: verified observations from Core, decided decisions, labelled claims.
    assert!(
        o.context.iter().any(|c| c.kind == "observation" && c.status == "verified" && c.source == "core")
    );
    assert!(
        o.context.iter().any(|c| c.kind == "decision" && c.status == "decided" && c.source == "agent:lead")
    );
    assert!(o.context.iter().any(|c| c.kind == "deliverable" && c.status == "claim"));
    assert!(o.context.iter().any(|c| c.kind == "objective" && c.status == "verified"));

    // Subtasks are M6 tasks: created by Core from the supervisor's directive, completed on acceptance.
    let code = o.tasks.iter().find(|t| t.key == "code").unwrap().task_id.unwrap();
    let code_detail = detail(&f, code).await;
    assert_eq!(code_detail.task.status, TaskStatus::Completed);
    assert_eq!(code_detail.task.parent_task_id, Some(task));
    let step_keys: Vec<_> = code_detail.assignments.iter().map(|a| a.step_key.clone().unwrap()).collect();
    assert_eq!(step_keys, ["org:code#1", "org:code#2"]);
    // The specialist's context named its supervisor, its scope and the instruction.
    let ctx = code_detail.assignments[1].context.clone().unwrap();
    assert_eq!(ctx["organization"]["scope"], json!(["src"]));
    assert!(ctx["organization"]["instructions"][0].as_str().unwrap().contains("inside src/"));
    assert!(ctx["organization"]["supervisor"].as_str().unwrap().contains("Lead supervisor"));

    // Delivery stays with the user: commit, then approve the exact commits.
    let d = detail(&f, task).await;
    assert_eq!(d.task.status, TaskStatus::AwaitingReview);
    let integration = d.workflow.as_ref().unwrap().integration_id.unwrap();
    f.client
        .call::<IntegrationsCommitWorkspace>(IntegrationsCommitWorkspaceParams {
            integration_id: integration,
            message: "Greet in Estonian".into(),
        })
        .await
        .unwrap();
    wait_org(&f, task, "awaiting approval", |_, d| {
        d.workflow.as_ref().is_some_and(|w| w.delivery.as_deref() == Some("awaiting_approval"))
    })
    .await;
    let i = f
        .client
        .call::<IntegrationsGet>(IntegrationIdParams { integration_id: integration })
        .await
        .unwrap()
        .integration;
    f.client
        .call::<IntegrationsApprove>(IntegrationsApproveParams {
            integration_id: integration,
            source_commit: i.source_commit.clone(),
            target_commit: i.target_commit.clone(),
        })
        .await
        .unwrap();
    wait_org(&f, task, "delivered", |_, d| d.task.status == TaskStatus::Completed).await;
    let (_, d) = wait_org(&f, task, "the delivered workflow", |_, d| {
        d.workflow.as_ref().is_some_and(|w| w.status == "delivered")
    })
    .await;
    assert_eq!(d.workflow.unwrap().definition, "organization");
    assert!(f.repo.join("src/greet.txt").exists(), "M7 moved the target checkout");
}

#[tokio::test]
async fn limits_reject_a_directive_and_the_supervisor_sees_why() {
    let f = fixture().await;
    f.client
        .call::<OrganizationsSetSettings>(OrganizationsSetSettingsParams {
            project_id: f.project_id,
            limits: Some(json!({ "max_agents": 1 })),
            estimates: None,
            overrides: None,
        })
        .await
        .unwrap();
    let lead = json!({ "supervise": [
        directive(json!({ "summary": "Two specialists", "agents": [
            { "key": "a", "title": "A", "kind": "specialist", "responsibility": "r", "scope": ["src"] }],
            "tasks": [{ "key": "t", "title": "T", "work": "implementation", "owner": "a" }] })),
        directive(json!({ "summary": "Still too many", "agents": [
            { "key": "b", "title": "B", "kind": "specialist", "responsibility": "r", "scope": ["src"] }] })),
        directive(json!({ "summary": "Need more people", "escalate": { "to": "user", "question": "May I add a specialist?" } })),
        directive(json!({ "summary": "Nothing is needed after all.", "done": true })),
    ]});
    let task = create_task(&f, format!("Small change. {}", script(&lead))).await;
    start(&f, task).await;
    let (o, _) = wait_org(&f, task, "the escalation", |o, _| o.question.is_some()).await;
    assert!(o.question.unwrap().contains("May I add a specialist?"));
    assert_eq!(o.agents.len(), 1, "nothing beyond the limit was created");
    let rejected = events(&f, "organization.directive_rejected").await;
    assert_eq!(rejected.len(), 2);
    assert!(rejected[0]["payload"]["error"].as_str().unwrap().contains("its limit"));
    // The answer goes back to the lead; with review required, "done" without a review is refused,
    // and after the repairs the organization waits for the user.
    f.client
        .call::<OrganizationsAnswer>(OrganizationsAnswerParams {
            task_id: task,
            text: "No, keep it small.".into(),
        })
        .await
        .unwrap();
    let (o, _) = wait_org(&f, task, "a wait after the refused done", |o, _| {
        o.status == "waiting" && o.question.is_none()
    })
    .await;
    assert!(o.reason.unwrap().contains("rejected"), "the refused done is explained");
    let rejected = events(&f, "organization.directive_rejected").await;
    assert!(rejected.iter().any(|e| e["payload"]["error"].as_str().unwrap().contains("independent review")));
}

#[tokio::test]
async fn settings_report_verified_capabilities_and_no_invented_skills() {
    let f = fixture().await;
    let s =
        f.client.call::<OrganizationsSettings>(ProjectIdParams { project_id: f.project_id }).await.unwrap();
    assert!(!s.profiles.is_empty());
    for p in &s.profiles {
        assert!(p.summary.contains("permission_routing"), "{}", p.summary);
        assert!(p.summary.contains("none configured"), "no skill estimates are invented: {}", p.summary);
    }
    assert!(
        f.client
            .call::<OrganizationsSetSettings>(OrganizationsSetSettingsParams {
                project_id: f.project_id,
                limits: Some(json!({ "max_concurrent_sessions": 2 })),
                estimates: None,
                overrides: None,
            })
            .await
            .is_err(),
        "the runtime cannot run two sessions in one workspace"
    );
    let s = f
        .client
        .call::<OrganizationsSetSettings>(OrganizationsSetSettingsParams {
            project_id: f.project_id,
            limits: None,
            estimates: Some(json!([{ "provider": "codex", "skills": { "coding": "strong" } }])),
            overrides: None,
        })
        .await
        .unwrap();
    assert!(s.profiles.iter().any(|p| p.provider == "codex" && p.summary.contains("coding strong")));
}

async fn no_review_required(f: &Fixture) {
    f.client
        .call::<switchyard_protocol::methods::IntegrationsSetSettings>(
            switchyard_protocol::methods::IntegrationsSetSettingsParams {
                project_id: f.project_id,
                require_review: false,
            },
        )
        .await
        .unwrap();
}

/// Core stops while a specialist works: after the restart the organization
/// waits (nothing restarts by itself), replays change nothing, and resuming
/// runs the interrupted work again.
#[tokio::test]
async fn a_restart_interrupts_the_running_turn_and_waits() {
    let mut f = fixture().await;
    no_review_required(&f).await;
    // lib/ has no allow rule: the write waits for an approval across the restart.
    let worker = json!({ "implement": [
        { "tool": "write:lib/a.txt", "report": implementation("first", &["lib/a.txt"]) },
        { "report": implementation("after the restart", &[]) } ]});
    let lead = json!({ "supervise": [
        directive(json!({ "summary": "One specialist", "agents": [
            { "key": "impl", "title": "Implementer", "kind": "specialist", "responsibility": "r", "scope": ["lib"], "provider": "claude_code" }],
            "tasks": [{ "key": "code", "title": "Code", "work": "implementation", "owner": "impl",
                "description": format!("Do it. {}", script(&worker)) }] })),
        directive(json!({ "summary": "ok", "assessments": [{ "task": "code", "verdict": "accept" }] })),
        directive(json!({ "summary": "Finished. Outstanding: none.", "done": true })),
    ]});
    let task = create_task(&f, format!("Restartable. {}", script(&lead))).await;
    start(&f, task).await;
    wait_org(&f, task, "the work turn", |o, _| o.activity.kind == "working").await;
    // Restart while the specialist waits for the approval of its write.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while events(&f, "agent.permission_requested").await.is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "the write never asked");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    f.core.restart().await;
    let (client, _rx) = f.core.connect().await;
    f.client = client;
    let (o, _) = wait_org(&f, task, "a wait after the restart", |o, _| o.status == "waiting").await;
    assert!(o.reason.unwrap().contains("Core restarted"));
    let code = o.tasks.iter().find(|t| t.key == "code").unwrap().task_id.unwrap();
    assert_eq!(detail(&f, code).await.assignments.len(), 1, "nothing restarted automatically");
    f.core.restart().await;
    let (client, _rx) = f.core.connect().await;
    f.client = client;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(org(&f, task).await.status, "waiting");
    assert_eq!(detail(&f, code).await.assignments.len(), 1, "a replay creates nothing");
    f.client
        .call::<switchyard_protocol::methods::WorkflowsResume>(TaskIdParams { task_id: task })
        .await
        .unwrap();
    wait_org(&f, task, "delivery", |o, _| o.status == "done").await;
    assert_eq!(detail(&f, code).await.assignments.len(), 2);
}

#[tokio::test]
async fn pause_and_cancel_go_through_the_workflow() {
    let f = fixture().await;
    let lead = json!({ "supervise": [
        directive(json!({ "summary": "Escalate", "escalate": { "to": "user", "question": "Which language?" } })) ]});
    let task = create_task(&f, format!("Cancel me. {}", script(&lead))).await;
    start(&f, task).await;
    wait_org(&f, task, "the question", |o, _| o.question.is_some()).await;
    let w = f
        .client
        .call::<switchyard_protocol::methods::WorkflowsCancel>(
            switchyard_protocol::methods::WorkflowsCancelParams {
                task_id: task,
                reason: Some("not needed".into()),
            },
        )
        .await
        .unwrap()
        .workflow;
    assert_eq!((w.definition.as_str(), w.status.as_str()), ("organization", "cancelled"));
    let o = org(&f, task).await;
    assert_eq!(o.status, "cancelled");
}
