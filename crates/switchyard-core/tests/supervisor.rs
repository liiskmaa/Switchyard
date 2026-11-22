//! The task supervisor (ADR 0041): the operator's questions and requests
//! about a task, answered by a read-only agent through the fake CLIs, and its
//! routes checked against the workflow before anything starts.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use serde_json::{Value, json};
use switchyard_client::CoreClient;
use switchyard_protocol::methods::{
    ProjectsAdd, ProjectsAddParams, SessionsList, SessionsListParams, SupervisorAsk, SupervisorAskParams,
    SupervisorGet, SupervisorInfo, TaskDecisionParams, TaskDetail, TaskIdParams, TasksAssign,
    TasksAssignParams, TasksComplete, TasksCreate, TasksCreateParams, TasksGet, WorkflowsStart,
    WorkflowsStartParams, WorkspacesCreate, WorkspacesCreateParams,
};
use switchyard_protocol::model::{Role, RoleBinding, RoleBindingsPatch, SessionAccess, TaskKind, TaskStatus};
use uuid::Uuid;

use common::{TestCore, git_repo};

struct Fixture {
    #[allow(dead_code)]
    core: TestCore,
    client: CoreClient,
    project_id: Uuid,
}

async fn fixture() -> Fixture {
    let core = TestCore::start_with(|_| {}).await;
    let (client, _rx) = core.connect().await;
    let repo = git_repo(core.dir.path(), "supervisor-repo");
    let project = client
        .call::<ProjectsAdd>(ProjectsAddParams { path: repo.display().to_string() })
        .await
        .unwrap()
        .project;
    common::allow_test_command(&client, project.id, common::PASSING_TEST).await;
    Fixture { core, client, project_id: project.id }
}

fn script(spec: &Value) -> String {
    let hex: String = spec.to_string().bytes().map(|b| format!("{b:02x}")).collect();
    format!("[fake:report:{hex}]")
}

fn report(kind: &str, summary: &str) -> Value {
    let mut r = json!({ "type": kind, "summary": summary });
    if kind == "implementation" {
        r["tests"] = common::passing_tests();
    }
    r
}

fn routed(route: &str, instruction: &str, prose: &str) -> Value {
    json!({ "text": format!(
        "{prose}\n\n```switchyard-supervisor\n{}\n```",
        json!({ "route": route, "instruction": instruction })
    ) })
}

fn claude() -> RoleBinding {
    RoleBinding { provider: "claude_code".into(), model: None }
}

async fn start(f: &Fixture, spec: &Value) -> Uuid {
    let task = f
        .client
        .call::<TasksCreate>(TasksCreateParams {
            project_id: f.project_id,
            title: "Mailbox per user".into(),
            description: Some(format!("Each user sets up their own mailbox. {}", script(spec))),
            kind: Some(TaskKind::Implementation),
            priority: None,
            acceptance_criteria: vec![],
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
            overrides: RoleBindingsPatch {
                investigate: Some(claude()),
                implement: Some(claude()),
                review: Some(claude()),
                correct: None,
            },
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

async fn wait_task(f: &Fixture, id: Uuid, what: &str, pred: impl Fn(&TaskDetail) -> bool) -> TaskDetail {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let d = detail(f, id).await;
        if pred(&d) {
            return d;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}; workflow {:?}",
            d.workflow.as_ref().map(|w| (&w.status, &w.step_key, &w.reason))
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

/// Asks and waits for the answer; returns the conversation.
async fn ask(f: &Fixture, task: Uuid, text: &str) -> SupervisorInfo {
    let before = f.client.call::<SupervisorGet>(TaskIdParams { task_id: task }).await.unwrap().messages.len();
    let asked = f
        .client
        .call::<SupervisorAsk>(SupervisorAskParams { task_id: task, text: text.into() })
        .await
        .unwrap();
    assert!(asked.thinking, "the answer comes in the background");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let s = f.client.call::<SupervisorGet>(TaskIdParams { task_id: task }).await.unwrap();
        if !s.thinking && s.messages.len() >= before + 2 {
            return s;
        }
        assert!(tokio::time::Instant::now() < deadline, "no answer to {text:?}: {:?}", s.messages);
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

#[tokio::test]
async fn the_supervisor_answers_and_routes_only_what_the_workflow_allows() {
    let f = fixture().await;
    let spec = json!({
        "investigate": [
            { "report": { "type": "question", "summary": "Scope?", "question": "Per user or per workspace?" } },
            { "report": report("findings", "per user") }
        ],
        "implement": { "report": report("implementation", "done") },
        "review": [
            { "report": { "type": "review", "summary": "fine", "result": "approved" } },
            { "report": { "type": "review", "summary": "still fine", "result": "approved" } }
        ],
        "supervisor": [
            { "text": "Reasoning asked whether mailboxes are per user; it waits for your answer." },
            routed("coding", "Add a retention setting.", "Coding should add it."),
            routed("reasoning", "Per user: each member connects a private mailbox.", "I'll answer reasoning."),
            routed("review", "Check the mailbox ownership change once more.", "Review will look again.")
        ]
    });
    let task = start(&f, &spec).await;
    wait_task(&f, task, "the question", |d| d.workflow.as_ref().is_some_and(|w| w.question.is_some())).await;

    // A question: only an answer, nothing routed.
    let s = ask(&f, task, "What is happening?").await;
    let a = s.messages.last().unwrap();
    assert_eq!(a.sender, "supervisor");
    assert_eq!(
        a.text, "Reasoning asked whether mailboxes are per user; it waits for your answer.",
        "once, verbatim"
    );
    assert_eq!((a.route.as_deref(), a.outcome.as_deref()), (None, None));
    // It ran read-only, in its own workspace, never the task's.
    let ws = s.workspace_id.unwrap();
    let d = detail(&f, task).await;
    assert_ne!(Some(ws), d.workflow.as_ref().unwrap().workspace_id);

    // Coding while reasoning still has the step open: refused, nothing starts.
    let s = ask(&f, task, "Also add a retention setting").await;
    let a = s.messages.last().unwrap();
    assert_eq!(a.route.as_deref(), Some("coding"));
    let outcome = a.outcome.as_deref().unwrap();
    assert!(outcome.starts_with("Nothing was routed") && outcome.contains("investigate#1"), "{outcome}");
    assert_eq!(detail(&f, task).await.assignments.len(), 1);

    // The working role: the instruction goes to its agent, which answers and the workflow continues.
    let s = ask(&f, task, "Tell reasoning: per user").await;
    let a = s.messages.last().unwrap();
    assert_eq!(a.outcome.as_deref(), Some("Sent to the agent working on investigate#1."));
    let d = wait_task(&f, task, "the delivery gate", |d| {
        d.workflow.as_ref().is_some_and(|w| w.stage.as_deref() == Some("delivery") && w.status == "waiting")
    })
    .await;
    let keys: Vec<_> = d.assignments.iter().filter_map(|a| a.step_key.clone()).collect();
    assert_eq!(keys, ["investigate#1", "implement#1", "review#1"]);

    // Nothing is working now: a review request starts a new review round.
    let s = ask(&f, task, "Have review look again").await;
    let a = s.messages.last().unwrap();
    assert_eq!(a.outcome.as_deref(), Some("Started review#2 with the instruction."), "{a:?}");
    let d = wait_task(&f, task, "review#2", |d| {
        d.assignments
            .iter()
            .any(|a| a.step_key.as_deref() == Some("review#2") && a.status.to_string() == "reported")
    })
    .await;
    // The instruction reached review#2 as an operator note.
    assert!(d.messages.iter().any(|m| m.kind.to_string() == "operator_note"
        && m.payload["text"].as_str().is_some_and(|t| t.contains("Check the mailbox ownership"))));

    // Every supervisor session was read-only, in the supervisor's workspace.
    let list = f
        .client
        .call::<SessionsList>(SessionsListParams {
            project_id: None,
            workspace_id: Some(ws),
            live_only: false,
            limit: None,
        })
        .await
        .unwrap();
    assert!(!list.sessions.is_empty());
    assert!(list.sessions.iter().all(|s| s.access == SessionAccess::Review));
}

#[tokio::test]
async fn one_question_at_a_time_and_empty_messages_are_refused() {
    let f = fixture().await;
    let spec = json!({
        "investigate": { "report": { "type": "question", "summary": "x", "question": "x?" } },
        "supervisor": { "text": "Fine." }
    });
    let task = start(&f, &spec).await;
    let empty =
        f.client.call::<SupervisorAsk>(SupervisorAskParams { task_id: task, text: "  ".into() }).await;
    assert!(empty.is_err());
    f.client.call::<SupervisorAsk>(SupervisorAskParams { task_id: task, text: "One?".into() }).await.unwrap();
    let second =
        f.client.call::<SupervisorAsk>(SupervisorAskParams { task_id: task, text: "Two?".into() }).await;
    // Either still thinking (refused) or already answered (accepted): never two open questions.
    let s = f.client.call::<SupervisorGet>(TaskIdParams { task_id: task }).await.unwrap();
    let operator = s.messages.iter().filter(|m| m.sender == "operator").count();
    assert!(second.is_err() || operator == 2);
}

/// Work requested on a delivered task becomes a linked follow-up task with
/// its own workflow; the delivered task itself never changes.
#[tokio::test]
async fn work_on_a_completed_task_opens_a_follow_up() {
    let f = fixture().await;
    let spec = json!({
        "implement": { "report": report("implementation", "done") },
        "supervisor": routed("coding", "Also expire old mail after 30 days.", "That is new work.")
    });
    let task = f
        .client
        .call::<TasksCreate>(TasksCreateParams {
            project_id: f.project_id,
            title: "Mailbox per user".into(),
            description: Some(format!("Each user sets up their own mailbox. {}", script(&spec))),
            kind: Some(TaskKind::Implementation),
            priority: None,
            acceptance_criteria: vec![],
            depends_on: vec![],
            parent_task_id: None,
            publish: true,
        })
        .await
        .unwrap()
        .task
        .id;
    let ws = f
        .client
        .call::<WorkspacesCreate>(WorkspacesCreateParams {
            project_id: f.project_id,
            name: None,
            base_ref: None,
        })
        .await
        .unwrap()
        .workspace
        .id;
    f.client
        .call::<TasksAssign>(TasksAssignParams {
            task_id: task,
            role: Role::Implement,
            provider: "claude_code".into(),
            workspace_id: ws,
            start: true,
            model: None,
        })
        .await
        .unwrap();
    wait_task(&f, task, "awaiting review", |d| d.task.status == TaskStatus::AwaitingReview).await;
    f.client.call::<TasksComplete>(TaskDecisionParams { task_id: task, note: None }).await.unwrap();

    let s = ask(&f, task, "Also expire old mail after 30 days").await;
    let a = s.messages.last().unwrap();
    let follow = a.follow_up_task_id.expect("a follow-up task");
    assert!(a.outcome.as_deref().unwrap().contains("follow-up task"), "{a:?}");
    let d = detail(&f, follow).await;
    assert!(d.task.description.as_deref().unwrap().contains(&task.to_string()), "it links back");
    assert!(d.task.description.as_deref().unwrap().contains("expire old mail"));
    assert!(d.workflow.is_some(), "its workflow started");
    assert_eq!(detail(&f, task).await.task.status, TaskStatus::Completed, "the delivered task is unchanged");
}
