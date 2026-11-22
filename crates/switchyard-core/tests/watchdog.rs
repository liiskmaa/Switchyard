#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The stuck-work watchdog (ADR 0045) end to end, with short limits.

mod common;

use std::time::Duration;

use common::{TestCore, git_repo};
use serde_json::{Value, json};
use switchyard_client::CoreClient;
use switchyard_protocol::methods::{
    ApprovalDecision, ApprovalsDecide, ApprovalsDecideParams, ApprovalsList, ApprovalsListParams, EventsList,
    EventsListParams, ProjectsAdd, ProjectsAddParams, TasksCreate, TasksCreateParams, WatchdogList,
    WatchdogListParams, WorkflowsStart, WorkflowsStartParams,
};
use switchyard_protocol::model::{ApprovalStatus, RoleBinding, RoleBindingsPatch, StuckItem, TaskKind};

fn script(spec: &Value) -> String {
    let hex: String = spec.to_string().bytes().map(|b| format!("{b:02x}")).collect();
    format!("[fake:report:{hex}]")
}

async fn stuck(c: &CoreClient) -> Vec<StuckItem> {
    c.call::<WatchdogList>(WatchdogListParams {}).await.unwrap().items
}

async fn wait_until<F: Fn(&[StuckItem]) -> bool>(c: &CoreClient, what: &str, pred: F) -> Vec<StuckItem> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let items = stuck(c).await;
        if pred(&items) {
            return items;
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for {what}: {items:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn an_unanswered_approval_is_reported_and_cleared_once_decided() {
    let core = TestCore::start_with(|c| {
        c.watchdog_interval = Duration::from_millis(200);
        c.watchdog_limits.approval_wait_ms = 0;
    })
    .await;
    let (c, _rx) = core.connect().await;
    let repo = git_repo(core.dir.path(), "watchdog-repo");
    let project =
        c.call::<ProjectsAdd>(ProjectsAddParams { path: repo.display().to_string() }).await.unwrap().project;
    let spec = json!({
        "investigate": { "report": { "type": "findings", "summary": "found" } },
        "implement": { "tool": "write:feature.txt", "report": { "type": "implementation", "summary": "done" } },
    });
    let task = c
        .call::<TasksCreate>(TasksCreateParams {
            project_id: project.id,
            title: "Add the feature".into(),
            description: Some(script(&spec)),
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
    let claude = || Some(RoleBinding { provider: "claude_code".into(), model: None });
    c.call::<WorkflowsStart>(WorkflowsStartParams {
        task_id: task,
        overrides: RoleBindingsPatch {
            investigate: claude(),
            implement: claude(),
            review: claude(),
            correct: None,
        },
        workspace_id: None,
        definition: None,
        lead: None,
    })
    .await
    .unwrap();

    let items = wait_until(&c, "the waiting approval", |i| {
        i.iter().any(|x| x.task_id == task && x.code == "approval_waiting")
    })
    .await;
    let item = items.iter().find(|x| x.task_id == task).unwrap();
    assert_eq!(item.kind, "workflow");
    assert!(item.session_id.is_some(), "the open step's session, to open it");
    assert_eq!(item.remedy_applied, None, "nothing automatic for a waiting approval");

    let pending = c
        .call::<ApprovalsList>(ApprovalsListParams {
            status: Some(ApprovalStatus::Pending),
            ..Default::default()
        })
        .await
        .unwrap()
        .approvals;
    c.call::<ApprovalsDecide>(ApprovalsDecideParams {
        approval_id: pending[0].id,
        decision: ApprovalDecision::Deny,
        note: None,
    })
    .await
    .unwrap();
    wait_until(&c, "the finding to clear", |i| {
        !i.iter().any(|x| x.task_id == task && x.code == "approval_waiting")
    })
    .await;

    let events = c
        .call::<EventsList>(EventsListParams {
            task_id: Some(task),
            after_seq: Some(0),
            limit: Some(1000),
            event_types: vec!["task.stuck_changed".into()],
            ..Default::default()
        })
        .await
        .unwrap()
        .events;
    assert!(events.iter().any(|e| e.payload["code"] == "approval_waiting"), "journaled when found");
    assert!(events.iter().any(|e| e.payload["code"].is_null()), "journaled when cleared");
}
