//! Milestone 6: task orchestration through the fake Claude and Codex CLIs.
//!
//! The fakes follow `[fake:report:HEX]` task scenarios (see
//! `src/bin/fake_support/report.rs`): per role, an optional tool call and then
//! a `switchyard-report` block, exactly what a real agent is asked to send.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};
use switchyard_client::CoreClient;
use switchyard_protocol::ErrorCode;
use switchyard_protocol::methods::{
    ApprovalDecision, ApprovalsDecide, ApprovalsDecideParams, ApprovalsList, ApprovalsListParams, EventsList,
    EventsListParams, ProjectsAdd, ProjectsAddParams, SessionIdParams, SessionsGet, SessionsStop,
    TaskAssignmentIdParams, TaskDecisionParams, TaskDependencyParams, TaskDetail, TaskIdParams,
    TasksAddDependency, TasksAssign, TasksAssignParams, TasksCancelAssignment, TasksComplete, TasksCreate,
    TasksCreateParams, TasksDecideProposal, TasksDecideProposalParams, TasksGet, TasksList, TasksListParams,
    TasksNote, TasksNoteParams, TasksPublish, TasksRetry, TasksUpdate, TasksUpdateParams, WorkspacesCreate,
    WorkspacesCreateParams,
};
use switchyard_protocol::model::{
    Approval, ApprovalStatus, AssignmentStatus, MessageKind, Role, SessionAccess, SessionState, Task,
    TaskKind, TaskReadiness, TaskStatus,
};
use uuid::Uuid;

use common::{TestCore, git_repo};

struct Fixture {
    core: TestCore,
    client: CoreClient,
    project_id: Uuid,
    workspace_id: Uuid,
    ws: PathBuf,
}

async fn fixture() -> Fixture {
    let core = TestCore::start_with(|_| {}).await;
    let (client, _rx) = core.connect().await;
    let repo = git_repo(core.dir.path(), "tasks-repo");
    let project = client
        .call::<ProjectsAdd>(ProjectsAddParams { path: repo.display().to_string() })
        .await
        .unwrap()
        .project;
    let (workspace_id, ws) = workspace(&client, project.id).await;
    Fixture { core, client, project_id: project.id, workspace_id, ws }
}

async fn workspace(client: &CoreClient, project_id: Uuid) -> (Uuid, PathBuf) {
    let w = client
        .call::<WorkspacesCreate>(WorkspacesCreateParams { project_id, name: None, base_ref: None })
        .await
        .unwrap()
        .workspace;
    (w.id, PathBuf::from(w.path))
}

/// The marker that makes the fakes follow `spec` (keyed by role).
fn script(spec: Value) -> String {
    let hex: String = spec.to_string().bytes().map(|b| format!("{b:02x}")).collect();
    format!("[fake:report:{hex}]")
}

fn create_params(f: &Fixture, title: &str, description: &str) -> TasksCreateParams {
    TasksCreateParams {
        project_id: f.project_id,
        title: title.into(),
        description: Some(description.into()),
        kind: Some(TaskKind::Implementation),
        priority: None,
        acceptance_criteria: vec!["feature.txt exists".into()],
        depends_on: vec![],
        parent_task_id: None,
        publish: true,
    }
}

async fn create(f: &Fixture, title: &str, spec: Value) -> Task {
    f.client.call::<TasksCreate>(create_params(f, title, &script(spec))).await.unwrap().task
}

async fn detail(f: &Fixture, id: Uuid) -> TaskDetail {
    f.client.call::<TasksGet>(TaskIdParams { task_id: id }).await.unwrap()
}

async fn wait_task(f: &Fixture, id: Uuid, what: &str, pred: impl Fn(&TaskDetail) -> bool) -> TaskDetail {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let d = detail(f, id).await;
        if pred(&d) {
            return d;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}; task is {} ({:?}); messages {:?}",
            d.task.status,
            d.task.status_reason,
            d.messages.iter().map(|m| (m.kind, m.payload.clone())).collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

async fn wait_status(f: &Fixture, id: Uuid, status: TaskStatus) -> TaskDetail {
    wait_task(f, id, &format!("status {status}"), |d| d.task.status == status).await
}

async fn assign(f: &Fixture, task: Uuid, role: Role, provider: &str) -> Uuid {
    f.client
        .call::<TasksAssign>(TasksAssignParams {
            task_id: task,
            role,
            provider: provider.into(),
            workspace_id: f.workspace_id,
            start: true,
            model: None,
        })
        .await
        .unwrap()
        .assignment
        .id
}

async fn pending_approval(f: &Fixture, session: Uuid) -> Approval {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
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
            return a;
        }
        assert!(tokio::time::Instant::now() < deadline, "no pending approval");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

async fn decide(f: &Fixture, approval: Uuid, decision: ApprovalDecision) {
    f.client
        .call::<ApprovalsDecide>(ApprovalsDecideParams { approval_id: approval, decision, note: None })
        .await
        .unwrap();
}

fn session_of(d: &TaskDetail, assignment: Uuid) -> Uuid {
    d.assignments.iter().find(|a| a.id == assignment).and_then(|a| a.session_id).unwrap()
}

async fn task_events(f: &Fixture, task: Uuid) -> Vec<String> {
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
        .collect()
}

fn report(kind: &str, summary: &str) -> Value {
    json!({ "type": kind, "summary": summary })
}

#[tokio::test]
async fn dependencies_block_tasks_and_cycles_are_refused() {
    let f = fixture().await;
    let a = create(&f, "A", json!({})).await;
    let mut p = create_params(&f, "B", "");
    p.depends_on = vec![a.id];
    let b = f.client.call::<TasksCreate>(p).await.unwrap().task;
    assert_eq!(a.status, TaskStatus::Ready);
    assert_eq!(b.status, TaskStatus::Blocked, "B waits for A");
    assert!(b.status_reason.as_deref().unwrap().starts_with("dependencies:"));
    assert_eq!(detail(&f, b.id).await.readiness, TaskReadiness::Waiting);

    let cycle = f
        .client
        .call::<TasksAddDependency>(TaskDependencyParams { task_id: a.id, depends_on: b.id })
        .await
        .unwrap_err();
    assert!(
        matches!(cycle, switchyard_client::ClientError::Remote(ref e) if e.code == ErrorCode::InvalidParams && e.message.contains("cycle")),
        "{cycle:?}"
    );
    let own = f
        .client
        .call::<TasksAddDependency>(TaskDependencyParams { task_id: a.id, depends_on: a.id })
        .await
        .unwrap_err();
    assert!(
        matches!(own, switchyard_client::ClientError::Remote(ref e) if e.code == ErrorCode::InvalidParams),
        "{own:?}"
    );

    // Edits are optimistic: a stale revision is refused.
    let edit = |rev| TasksUpdateParams {
        task_id: a.id,
        expected_revision: rev,
        title: Some("A2".into()),
        description: None,
        kind: None,
        priority: None,
        acceptance_criteria: None,
    };
    let updated = f.client.call::<TasksUpdate>(edit(a.revision)).await.unwrap().task;
    assert_eq!(updated.title, "A2");
    let stale = f.client.call::<TasksUpdate>(edit(a.revision)).await.unwrap_err();
    assert!(
        matches!(stale, switchyard_client::ClientError::Remote(ref e) if e.code == ErrorCode::Conflict),
        "{stale:?}"
    );

    // Publishing twice, or completing work nobody reported, is refused.
    let again = f.client.call::<TasksPublish>(TaskIdParams { task_id: a.id }).await.unwrap_err();
    assert!(
        matches!(again, switchyard_client::ClientError::Remote(ref e) if e.code == ErrorCode::InvalidState)
    );
    let early =
        f.client.call::<TasksComplete>(TaskDecisionParams { task_id: a.id, note: None }).await.unwrap_err();
    assert!(
        matches!(early, switchyard_client::ClientError::Remote(ref e) if e.code == ErrorCode::InvalidState)
    );

    // Cancelling A breaks B's dependency; B stays blocked.
    f.client
        .call::<switchyard_protocol::methods::TasksCancel>(TaskDecisionParams { task_id: a.id, note: None })
        .await
        .unwrap();
    let d = detail(&f, b.id).await;
    assert_eq!((d.task.status, d.readiness), (TaskStatus::Blocked, TaskReadiness::Broken));

    let events = task_events(&f, b.id).await;
    assert!(
        events.contains(&"task.created".to_owned()) && events.contains(&"task.dependency_added".to_owned())
    );
    let listed = f
        .client
        .call::<TasksList>(TasksListParams { project_id: Some(f.project_id), ..Default::default() })
        .await
        .unwrap();
    assert_eq!(listed.tasks.len(), 1, "closed tasks are hidden by default");
}

/// Claude investigates, Codex implements (the operator approves its patch),
/// Claude reviews and asks for changes, Codex fixes them in the same thread,
/// Claude approves, the operator completes, and the dependent task becomes ready.
#[tokio::test]
async fn investigate_implement_review_loop_across_providers() {
    let f = fixture().await;
    let spec = json!({
        "investigate": { "report": {
            "type": "findings", "summary": "The feature needs one new file.",
            "findings": [{ "severity": "info", "title": "No feature file yet", "path": "feature.txt" }]
        }},
        "implement": [
            { "tool": "write:feature.txt", "report": {
                "type": "implementation", "summary": "Added feature.txt.", "changed_files": ["feature.txt"],
                "acceptance": [{ "criterion": "feature.txt exists", "met": true }] } },
            { "tool": "edit:feature.txt", "report": {
                "type": "implementation", "summary": "Addressed the review.", "changed_files": ["feature.txt"] } }
        ],
        "review": [
            { "report": { "type": "review", "summary": "Needs a second line.", "result": "changes_requested",
                "findings": [{ "severity": "medium", "title": "Only one line", "path": "feature.txt", "line": 1,
                               "suggestion": "add a second line" }] } },
            { "report": { "type": "review", "summary": "Looks good.", "result": "approved",
                "acceptance": [{ "criterion": "feature.txt exists", "met": true }] } }
        ]
    });
    let task = create(&f, "Add the feature", spec).await;
    let mut dependent = create_params(&f, "Document the feature", "");
    dependent.depends_on = vec![task.id];
    let dependent = f.client.call::<TasksCreate>(dependent).await.unwrap().task;
    assert_eq!(dependent.status, TaskStatus::Blocked);

    // 1. Claude investigates.
    let inv = assign(&f, task.id, Role::Investigate, "claude_code").await;
    let d = wait_status(&f, task.id, TaskStatus::AwaitingReview).await;
    let a = d.assignments.iter().find(|a| a.id == inv).unwrap();
    assert_eq!(a.status, AssignmentStatus::Reported);
    let ctx = a.context.as_ref().unwrap();
    assert_eq!((ctx["version"].as_u64(), ctx["role"].as_str()), (Some(1), Some("investigate")));
    assert!(d.messages.iter().any(|m| m.kind == MessageKind::Findings && m.sender == "agent"));
    assert_eq!(d.findings.len(), 1);

    // 2. Codex implements; its patch waits for the operator.
    let imp = assign(&f, task.id, Role::Implement, "codex").await;
    let d = wait_status(&f, task.id, TaskStatus::InProgress).await;
    let codex_session = session_of(&d, imp);
    let approval = pending_approval(&f, codex_session).await;
    assert_eq!(approval.tool_name, "apply_patch");
    decide(&f, approval.id, ApprovalDecision::Approve).await;
    let d = wait_status(&f, task.id, TaskStatus::AwaitingReview).await;
    assert!(f.ws.join("feature.txt").exists(), "the approved patch was applied in the workspace");
    assert!(d.messages.iter().any(|m| m.kind == MessageKind::CompletionReport));

    // 3. Claude reviews in a read-only session and requests changes.
    let rev = assign(&f, task.id, Role::Review, "claude_code").await;
    let d = wait_task(&f, task.id, "a review verdict", |d| {
        d.assignments.iter().any(|a| a.id == rev && a.status == AssignmentStatus::Reported)
    })
    .await;
    let review_session = session_of(&d, rev);
    let s =
        f.client.call::<SessionsGet>(SessionIdParams { session_id: review_session }).await.unwrap().session;
    assert_eq!(s.access, SessionAccess::Review);
    assert_eq!(d.task.status, TaskStatus::AwaitingReview);
    assert_eq!(d.task.status_reason.as_deref(), Some("review: changes_requested"));
    let open: Vec<_> = d.findings.iter().filter(|f| f.status == "open").collect();
    assert_eq!(open.len(), 1, "the review supersedes the investigation's findings");
    assert_eq!(open[0].title, "Only one line");

    // 4. Codex addresses the findings in the same thread (resumed); the
    // context carries the open findings.
    let fix = assign(&f, task.id, Role::Implement, "codex").await;
    let d = wait_status(&f, task.id, TaskStatus::InProgress).await;
    assert_eq!(session_of(&d, fix), codex_session, "the earlier Codex session is resumed");
    let ctx = d.assignments.iter().find(|a| a.id == fix).unwrap().context.clone().unwrap();
    assert_eq!(ctx["review_findings"][0]["title"], "Only one line");
    let approval = pending_approval(&f, codex_session).await;
    decide(&f, approval.id, ApprovalDecision::Approve).await;
    wait_status(&f, task.id, TaskStatus::AwaitingReview).await;

    // 5. A second review approves.
    assign(&f, task.id, Role::Review, "claude_code").await;
    let d =
        wait_task(&f, task.id, "approval", |d| d.task.status_reason.as_deref() == Some("review: approved"))
            .await;
    assert_eq!(d.task.status, TaskStatus::AwaitingReview, "an approving review never completes a task");
    assert_eq!(detail(&f, dependent.id).await.task.status, TaskStatus::Blocked);

    // 6. Only the operator completes it; the dependent becomes ready.
    let done = f
        .client
        .call::<TasksComplete>(TaskDecisionParams { task_id: task.id, note: Some("ship it".into()) })
        .await
        .unwrap()
        .task;
    assert_eq!(done.status, TaskStatus::Completed);
    assert_eq!(done.result.as_ref().unwrap()["summary"], "Looks good.");
    assert_eq!(detail(&f, dependent.id).await.task.status, TaskStatus::Ready);
    assert!(detail(&f, task.id).await.findings.iter().all(|f| f.status != "open"));

    let events = task_events(&f, task.id).await;
    for t in [
        "task.assigned",
        "task.assignment_started",
        "task.assignment_ended",
        "task.message_recorded",
        "task.review_recorded",
        "task.status_changed",
    ] {
        assert!(events.iter().any(|e| e == t), "missing {t}");
    }
}

/// A review session cannot change files: the policy denies every file change
/// automatically (no person is asked), so nothing is written.
#[tokio::test]
async fn review_sessions_cannot_change_files() {
    let f = fixture().await;
    let spec = json!({
        "implement": { "report": report("implementation", "Nothing to change.") },
        "review": { "tool": "write:sneaky.txt", "report": {
            "type": "review", "summary": "Fine.", "result": "approved" } }
    });
    let task = create(&f, "T", spec).await;
    assign(&f, task.id, Role::Implement, "claude_code").await;
    wait_status(&f, task.id, TaskStatus::AwaitingReview).await;
    let rev = assign(&f, task.id, Role::Review, "claude_code").await;
    let d = wait_task(&f, task.id, "the review", |d| {
        d.assignments.iter().any(|a| a.id == rev && a.status == AssignmentStatus::Reported)
    })
    .await;
    assert!(!f.ws.join("sneaky.txt").exists(), "a review session wrote a file");
    let session = session_of(&d, rev);
    let approvals = f
        .client
        .call::<ApprovalsList>(ApprovalsListParams { session_id: Some(session), ..Default::default() })
        .await
        .unwrap()
        .approvals;
    assert!(approvals.is_empty(), "the write was denied without asking a person: {approvals:?}");
    let evs = f
        .client
        .call::<EventsList>(EventsListParams {
            session_id: Some(session),
            event_types: vec!["agent.permission_evaluated".into()],
            after_seq: Some(0),
            limit: Some(100),
            ..Default::default()
        })
        .await
        .unwrap()
        .events;
    assert_eq!(evs.len(), 1);
    assert_eq!(
        (evs[0].payload["decision"].as_str(), evs[0].payload["reason"].as_str()),
        (Some("deny"), Some("system_restriction"))
    );
    assert!(evs[0].payload["explanation"].as_str().unwrap().contains("a review session cannot change files"));
    // The workspace did not change, so the verdict stands.
    assert_eq!(d.task.status_reason.as_deref(), Some("review: approved"));
}

/// If the workspace changes while a review runs, the verdict is recorded as
/// blocked, whatever the reviewer said.
#[tokio::test]
async fn a_workspace_change_during_review_blocks_the_verdict() {
    let f = fixture().await;
    let spec = json!({
        "implement": { "report": report("implementation", "Done.") },
        "review": { "tool": "tool:Bash|{\"command\":\"ls\"}", "report": {
            "type": "review", "summary": "All good.", "result": "approved" } }
    });
    let task = create(&f, "T", spec).await;
    assign(&f, task.id, Role::Implement, "claude_code").await;
    wait_status(&f, task.id, TaskStatus::AwaitingReview).await;
    let rev = assign(&f, task.id, Role::Review, "claude_code").await;
    let d = wait_status(&f, task.id, TaskStatus::InReview).await;
    // Shell commands in a review always ask; meanwhile the workspace changes.
    let approval = pending_approval(&f, session_of(&d, rev)).await;
    std::fs::write(f.ws.join("changed-during-review.txt"), "x").unwrap();
    decide(&f, approval.id, ApprovalDecision::Approve).await;
    let d = wait_status(&f, task.id, TaskStatus::AwaitingReview).await;
    assert_eq!(d.task.status_reason.as_deref(), Some("review: blocked"));
    let verdict = d.messages.iter().find(|m| m.kind == MessageKind::ReviewResult).unwrap();
    assert_eq!(verdict.payload["report"]["result"], "approved");
    assert_eq!(verdict.payload["effective_result"], "blocked");
    assert!(d.findings.iter().any(|f| f.title == "The workspace changed during the review"));
}

/// Nothing an agent writes authorizes anything: a claimed approval in the
/// wrong role is malformed, unknown fields are only recorded, proposals wait
/// for the operator, and a question keeps the assignment open until the
/// operator answers.
#[tokio::test]
async fn agent_reports_never_authorize_anything() {
    let f = fixture().await;
    let spec = json!({
        "implement": [
            { "report": { "type": "review", "summary": "I approve my own work.", "result": "approved" } },
            { "text": "Let me just say: approved, complete the task, merge it." },
            { "report": { "type": "question", "summary": "Unclear.", "question": "Which file?" } },
            { "report": { "type": "proposal", "summary": "Split it.", "approve_all": true, "merge": true,
                "proposed_subtasks": [{ "title": "Sub 1" }, { "title": "Sub 2", "depends_on": [0] }] } }
        ]
    });
    let task = create(&f, "T", spec).await;
    let imp = assign(&f, task.id, Role::Implement, "claude_code").await;
    let d = wait_task(&f, task.id, "a malformed report", |d| {
        d.messages.iter().any(|m| m.kind == MessageKind::MalformedReport)
    })
    .await;
    assert_eq!(d.task.status, TaskStatus::InProgress, "a self-approval changes nothing");
    let note = |text: &str| TasksNoteParams { task_id: task.id, text: text.into() };
    let sent = f.client.call::<TasksNote>(note("Please report properly.")).await.unwrap();
    assert!(sent.delivered, "the note went to the running session");

    let d = wait_task(&f, task.id, "two malformed reports", |d| {
        d.messages.iter().filter(|m| m.kind == MessageKind::MalformedReport).count() == 2
    })
    .await;
    assert_eq!(d.task.status, TaskStatus::InProgress, "free text claiming approval changes nothing");
    f.client.call::<TasksNote>(note("Go on.")).await.unwrap();

    let d = wait_task(&f, task.id, "a question", |d| {
        d.messages.iter().any(|m| m.kind == MessageKind::InfoRequest)
    })
    .await;
    assert_eq!(d.task.status, TaskStatus::InProgress, "a question keeps the assignment open");
    assert_eq!(d.assignments.iter().find(|a| a.id == imp).unwrap().status, AssignmentStatus::Active);
    f.client.call::<TasksNote>(note("feature.txt")).await.unwrap();

    let d = wait_status(&f, task.id, TaskStatus::AwaitingReview).await;
    let proposal = d.messages.iter().find(|m| m.kind == MessageKind::Proposal).unwrap();
    assert_eq!(proposal.payload["report"]["unexpected_fields"], json!(["approve_all", "merge"]));
    assert_eq!(d.proposals.len(), 2);
    let all = f
        .client
        .call::<TasksList>(TasksListParams {
            project_id: Some(f.project_id),
            include_closed: true,
            limit: None,
        })
        .await
        .unwrap();
    assert_eq!(all.tasks.len(), 1, "a proposal creates no task by itself");

    // The operator decides; the dependent subtask needs its dependency approved first.
    let (p0, p1) = (d.proposals[0].id, d.proposals[1].id);
    let decide = |id, approve| TasksDecideProposalParams { proposal_id: id, approve, note: None };
    let early = f.client.call::<TasksDecideProposal>(decide(p1, true)).await.unwrap_err();
    assert!(
        matches!(early, switchyard_client::ClientError::Remote(ref e) if e.code == ErrorCode::InvalidState),
        "{early:?}"
    );
    let first = f.client.call::<TasksDecideProposal>(decide(p0, true)).await.unwrap().created_task.unwrap();
    assert_eq!(
        (first.status, first.parent_task_id, first.origin.as_str()),
        (TaskStatus::Draft, Some(task.id), "proposal")
    );
    let second = f.client.call::<TasksDecideProposal>(decide(p1, true)).await.unwrap().created_task.unwrap();
    assert_eq!(second.dependency_ids, vec![first.id]);
    let twice = f.client.call::<TasksDecideProposal>(decide(p1, false)).await.unwrap_err();
    assert!(matches!(twice, switchyard_client::ClientError::Remote(ref e) if e.code == ErrorCode::Conflict));
}

/// A session that ends before reporting fails the assignment and the task;
/// the operator can retry.
#[tokio::test]
async fn a_session_ending_mid_assignment_fails_it_and_retry_reopens_the_task() {
    let f = fixture().await;
    let spec = json!({ "implement": { "tool": "tool:Bash|{\"command\":\"make\"}", "report": report("implementation", "x") } });
    let task = create(&f, "T", spec).await;
    let imp = assign(&f, task.id, Role::Implement, "claude_code").await;
    let d = wait_status(&f, task.id, TaskStatus::InProgress).await;
    let session = session_of(&d, imp);
    pending_approval(&f, session).await;
    f.client.call::<SessionsStop>(SessionIdParams { session_id: session }).await.unwrap();
    let d = wait_status(&f, task.id, TaskStatus::Failed).await;
    assert_eq!(d.assignments[0].status, AssignmentStatus::Failed);
    assert!(d.task.failure.as_deref().unwrap().contains("before the assignment reported"));
    let retried =
        f.client.call::<TasksRetry>(TaskDecisionParams { task_id: task.id, note: None }).await.unwrap().task;
    assert_eq!(retried.status, TaskStatus::Ready);
}

/// After a Core restart, an assignment whose session did not survive fails
/// (replayed from the journal), and reported work is not processed twice.
#[tokio::test]
async fn assignments_recover_after_a_core_restart() {
    let mut f = fixture().await;
    let done =
        create(&f, "Reported", json!({ "investigate": { "report": report("findings", "found it") } })).await;
    assign(&f, done.id, Role::Investigate, "claude_code").await;
    wait_status(&f, done.id, TaskStatus::AwaitingReview).await;
    let messages_before = detail(&f, done.id).await.messages.len();

    let (other_ws, _) = workspace(&f.client, f.project_id).await;
    // Its only turn ends without a report, so the assignment stays active
    // with an idle session when Core stops.
    let running = create(&f, "Running", json!({ "implement": { "text": "still thinking" } })).await;
    let a = f
        .client
        .call::<TasksAssign>(TasksAssignParams {
            task_id: running.id,
            role: Role::Implement,
            provider: "claude_code".into(),
            workspace_id: other_ws,
            start: true,
            model: None,
        })
        .await
        .unwrap()
        .assignment;
    wait_task(&f, running.id, "an unusable turn", |d| {
        d.messages.iter().any(|m| m.kind == MessageKind::MalformedReport)
    })
    .await;

    f.core.restart_with(|_| {}).await;
    let (client, _rx) = f.core.connect().await;
    f.client = client;
    let d = wait_status(&f, running.id, TaskStatus::Failed).await;
    assert_eq!(d.assignments[0].status, AssignmentStatus::Failed);
    let s = f
        .client
        .call::<SessionsGet>(SessionIdParams { session_id: a.session_id.unwrap() })
        .await
        .unwrap()
        .session;
    assert!(matches!(s.state, SessionState::Failed | SessionState::Stopped), "{:?}", s.state);
    // Restart once more: replaying the journal changes nothing.
    f.core.restart_with(|_| {}).await;
    let (client, _rx) = f.core.connect().await;
    f.client = client;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(detail(&f, done.id).await.messages.len(), messages_before);
    assert_eq!(detail(&f, running.id).await.messages.len(), d.messages.len());
}

/// One task works in a workspace at a time; a cancelled assignment frees it.
#[tokio::test]
async fn one_task_per_workspace_and_cancel_frees_it() {
    let f = fixture().await;
    let spec = json!({ "implement": { "tool": "tool:Bash|{\"command\":\"make\"}", "report": report("implementation", "x") } });
    let first = create(&f, "First", spec.clone()).await;
    let second = create(&f, "Second", spec).await;
    let a = assign(&f, first.id, Role::Implement, "claude_code").await;
    let busy = f
        .client
        .call::<TasksAssign>(TasksAssignParams {
            task_id: second.id,
            role: Role::Implement,
            provider: "codex".into(),
            workspace_id: f.workspace_id,
            start: true,
            model: None,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(busy, switchyard_client::ClientError::Remote(ref e) if e.code == ErrorCode::Conflict),
        "{busy:?}"
    );
    let unknown = f
        .client
        .call::<TasksAssign>(TasksAssignParams {
            task_id: second.id,
            role: Role::Implement,
            provider: "gemini".into(),
            workspace_id: f.workspace_id,
            start: true,
            model: None,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(unknown, switchyard_client::ClientError::Remote(ref e) if e.code == ErrorCode::InvalidParams)
    );
    let d = wait_status(&f, first.id, TaskStatus::InProgress).await;
    let session = session_of(&d, a);
    pending_approval(&f, session).await;
    let cancelled = f
        .client
        .call::<TasksCancelAssignment>(TaskAssignmentIdParams { assignment_id: a, reason: None })
        .await
        .unwrap();
    assert_eq!(cancelled.assignment.status, AssignmentStatus::Cancelled);
    assert_eq!(cancelled.task.status, TaskStatus::Ready);
    let s = f.client.call::<SessionsGet>(SessionIdParams { session_id: session }).await.unwrap().session;
    assert!(s.state.is_terminal(), "the session was stopped: {:?}", s.state);
    // The pending approval was cancelled with the session, never approved.
    let approvals = f
        .client
        .call::<ApprovalsList>(ApprovalsListParams { session_id: Some(session), ..Default::default() })
        .await
        .unwrap()
        .approvals;
    assert!(approvals.iter().all(|a| a.status == ApprovalStatus::Cancelled), "{approvals:?}");
    assign(&f, second.id, Role::Implement, "codex").await;
}

/// The next assignment can start right after a report: it waits for Core's
/// own stop of the reported session (here a slow-exiting agent), never failing
/// with `workspace_occupied` because of it.
#[tokio::test]
async fn the_next_assignment_waits_for_the_reported_session_to_stop() {
    let f = fixture().await;
    let spec = json!({
        "linger_ms": 1200,
        "investigate": { "report": report("findings", "found") },
        "implement": { "report": report("implementation", "done") }
    });
    let task = create(&f, "T", spec).await;
    let inv = assign(&f, task.id, Role::Investigate, "claude_code").await;
    let d = wait_status(&f, task.id, TaskStatus::AwaitingReview).await;
    let old = session_of(&d, inv);
    let s = f.client.call::<SessionsGet>(SessionIdParams { session_id: old }).await.unwrap().session;
    assert!(!s.state.is_terminal(), "the reported session is still exiting: {:?}", s.state);
    let imp = assign(&f, task.id, Role::Implement, "claude_code").await;
    let d = wait_status(&f, task.id, TaskStatus::AwaitingReview).await;
    assert_ne!(session_of(&d, imp), old);
    let s = f.client.call::<SessionsGet>(SessionIdParams { session_id: old }).await.unwrap().session;
    assert!(s.state.is_terminal(), "{:?}", s.state);
}

/// Core never stops a session it does not own for a reported assignment: a
/// session the operator resumed by hand keeps running, and the new
/// assignment is refused with `workspace_occupied`.
#[tokio::test]
async fn an_operator_resumed_session_is_never_stopped_by_a_new_assignment() {
    let f = fixture().await;
    let spec = json!({
        "investigate": { "report": report("findings", "found") },
        "implement": { "report": report("implementation", "done") }
    });
    let task = create(&f, "T", spec).await;
    let inv = assign(&f, task.id, Role::Investigate, "claude_code").await;
    let d = wait_status(&f, task.id, TaskStatus::AwaitingReview).await;
    let old = session_of(&d, inv);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !f
        .client
        .call::<SessionsGet>(SessionIdParams { session_id: old })
        .await
        .unwrap()
        .session
        .state
        .is_terminal()
    {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    f.client
        .call::<switchyard_protocol::methods::SessionsResume>(
            switchyard_protocol::methods::SessionsResumeParams { session_id: old, prompt: None },
        )
        .await
        .unwrap();
    let refused = f
        .client
        .call::<TasksAssign>(TasksAssignParams {
            task_id: task.id,
            role: Role::Implement,
            provider: "claude_code".into(),
            workspace_id: f.workspace_id,
            start: true,
            model: None,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(refused, switchyard_client::ClientError::Remote(ref e) if e.code == ErrorCode::WorkspaceOccupied),
        "{refused:?}"
    );
    let s = f.client.call::<SessionsGet>(SessionIdParams { session_id: old }).await.unwrap().session;
    assert!(s.attached && !s.state.is_terminal(), "the operator's session keeps running: {:?}", s.state);
    let d = detail(&f, task.id).await;
    assert_eq!(d.task.status, TaskStatus::AwaitingReview, "the task returns to the operator");
    assert_eq!(d.assignments.last().unwrap().status, AssignmentStatus::Failed);
}

/// Two concurrent starts of one assignment: exactly one wins; the other is
/// refused and never fails the winner's assignment or orphans its session.
#[tokio::test]
async fn a_concurrent_second_start_never_fails_the_running_assignment() {
    let f = fixture().await;
    let spec = json!({ "implement": { "tool": "tool:Bash|{\"command\":\"make\"}", "report": report("implementation", "x") } });
    let task = create(&f, "T", spec).await;
    let a = f
        .client
        .call::<TasksAssign>(TasksAssignParams {
            task_id: task.id,
            role: Role::Implement,
            provider: "claude_code".into(),
            workspace_id: f.workspace_id,
            start: false,
            model: None,
        })
        .await
        .unwrap()
        .assignment;
    let start = || {
        f.client.call::<switchyard_protocol::methods::TasksStartAssignment>(TaskAssignmentIdParams {
            assignment_id: a.id,
            reason: None,
        })
    };
    let (first, second) = tokio::join!(start(), start());
    let (won, lost) = match (first, second) {
        (Ok(w), Err(l)) | (Err(l), Ok(w)) => (w, l),
        other => panic!("exactly one start must win: {other:?}"),
    };
    assert!(
        matches!(lost, switchyard_client::ClientError::Remote(ref e) if e.code == ErrorCode::Conflict || e.code == ErrorCode::InvalidState),
        "{lost:?}"
    );
    let d = detail(&f, task.id).await;
    assert_eq!(d.task.status, TaskStatus::InProgress, "the winner's assignment is untouched");
    let live = d.assignments.iter().find(|x| x.id == a.id).unwrap();
    assert_eq!((live.status, live.session_id), (AssignmentStatus::Active, won.assignment.session_id));
    pending_approval(&f, live.session_id.unwrap()).await;
}

/// A review that ends with a blocker, or whose session ends, is still checked
/// against the workspace: a change is recorded as a finding.
#[tokio::test]
async fn every_end_of_a_review_checks_the_workspace() {
    let f = fixture().await;
    let spec = json!({
        "implement": { "report": report("implementation", "Done.") },
        "review": [
            { "tool": "tool:Bash|{\"command\":\"ls\"}", "report": report("blocker", "Cannot finish.") },
            { "tool": "tool:Bash|{\"command\":\"ls\"}", "report": { "type": "review", "summary": "ok", "result": "approved" } }
        ]
    });
    let task = create(&f, "T", spec).await;
    assign(&f, task.id, Role::Implement, "claude_code").await;
    wait_status(&f, task.id, TaskStatus::AwaitingReview).await;

    // 1. A blocker after the workspace changed.
    let rev = assign(&f, task.id, Role::Review, "claude_code").await;
    let d = wait_status(&f, task.id, TaskStatus::InReview).await;
    let approval = pending_approval(&f, session_of(&d, rev)).await;
    std::fs::write(f.ws.join("changed-1.txt"), "x").unwrap();
    decide(&f, approval.id, ApprovalDecision::Approve).await;
    let d = wait_status(&f, task.id, TaskStatus::AwaitingReview).await;
    assert!(
        d.task.status_reason.as_deref().unwrap().contains("the workspace changed during the review"),
        "{:?}",
        d.task.status_reason
    );
    assert_eq!(d.findings.iter().filter(|f| f.title == "The workspace changed during the review").count(), 1);

    // 2. The review session ends (stopped) after the workspace changed.
    let rev = assign(&f, task.id, Role::Review, "claude_code").await;
    let d = wait_status(&f, task.id, TaskStatus::InReview).await;
    let session = session_of(&d, rev);
    pending_approval(&f, session).await;
    std::fs::write(f.ws.join("changed-2.txt"), "x").unwrap();
    f.client.call::<SessionsStop>(SessionIdParams { session_id: session }).await.unwrap();
    let d = wait_task(&f, task.id, "the review to end", |d| {
        d.assignments.iter().any(|a| a.id == rev && a.status == AssignmentStatus::Failed)
    })
    .await;
    assert_eq!(d.task.status, TaskStatus::AwaitingReview);
    let ended = d.messages.iter().rev().find(|m| m.payload["event"] == "session_ended").unwrap();
    assert_eq!(ended.payload["workspace_changed"], true);
    assert_eq!(
        d.findings
            .iter()
            .filter(|f| f.title == "The workspace changed during the review" && f.status == "open")
            .count(),
        2
    );
}
