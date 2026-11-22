#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Gemini CLI sessions over ACP end to end (ADR 0044), using the fake Gemini
//! (`switchyard-fake-gemini`). Core, storage, the policy engine, the approval
//! manager and Git worktrees are real; only the Gemini CLI is a deterministic
//! stand-in. No network, no credentials.

mod common;

use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;
use switchyard_client::{CoreClient, Notification};
use switchyard_protocol::EventEnvelope;
use switchyard_protocol::methods::{
    AgentsList, AgentsListParams, AgentsModels, AgentsModelsParams, ApprovalDecision, ApprovalsDecide,
    ApprovalsDecideParams, ApprovalsList, ApprovalsListParams, Delivery, EventsList, EventsListParams,
    PoliciesEvaluations, PoliciesEvaluationsParams, ProjectsAdd, ProjectsAddParams, SessionIdParams,
    SessionsGet, SessionsInterrupt, SessionsResume, SessionsResumeParams, SessionsSend, SessionsSendParams,
    SessionsStart, SessionsStartParams, SessionsStop, WorkspacesCreate, WorkspacesCreateParams,
};
use switchyard_protocol::model::{
    Approval, ApprovalStatus, OperationCategory, Session, SessionState, Workspace,
};
use tokio::sync::mpsc;
use uuid::Uuid;

use common::{TestCore, git_repo};

#[allow(dead_code)]
struct Fixture {
    core: TestCore,
    client: CoreClient,
    rx: mpsc::Receiver<Notification>,
    repo: PathBuf,
    project_id: Uuid,
    workspace_id: Uuid,
    ws: PathBuf,
}

async fn fixture() -> Fixture {
    let core = TestCore::start().await;
    let (client, rx) = core.connect().await;
    let repo = git_repo(core.dir.path(), "gemini-repo");
    let project = client
        .call::<ProjectsAdd>(ProjectsAddParams { path: repo.display().to_string() })
        .await
        .unwrap()
        .project;
    let workspace = create_workspace(&client, project.id).await;
    let ws = PathBuf::from(&workspace.path);
    Fixture { core, client, rx, repo, project_id: project.id, workspace_id: workspace.id, ws }
}

async fn create_workspace(client: &CoreClient, project_id: Uuid) -> Workspace {
    client
        .call::<WorkspacesCreate>(WorkspacesCreateParams { project_id, name: None, base_ref: None })
        .await
        .unwrap()
        .workspace
}

fn gemini_params(workspace_id: Uuid) -> SessionsStartParams {
    SessionsStartParams {
        workspace_id,
        project_id: None,
        provider: Some("gemini".into()),
        prompt: None,
        model: None,
    }
}

async fn start(f: &Fixture, workspace_id: Uuid) -> Session {
    f.client.call::<SessionsStart>(gemini_params(workspace_id)).await.unwrap().session
}

async fn send(f: &Fixture, id: Uuid, text: &str, queue: bool) -> Delivery {
    f.client
        .call::<SessionsSend>(SessionsSendParams { session_id: id, text: text.into(), queue_if_busy: queue })
        .await
        .unwrap()
        .delivery
}

async fn session(f: &Fixture, id: Uuid) -> Session {
    f.client.call::<SessionsGet>(SessionIdParams { session_id: id }).await.unwrap().session
}

async fn events(f: &Fixture, id: Uuid) -> Vec<EventEnvelope> {
    f.client
        .call::<EventsList>(EventsListParams {
            session_id: Some(id),
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

fn turns(evs: &[EventEnvelope]) -> usize {
    of_type(evs, "agent.turn_completed").len()
}

async fn wait_events(
    f: &Fixture,
    id: Uuid,
    what: &str,
    pred: impl Fn(&[EventEnvelope]) -> bool,
) -> Vec<EventEnvelope> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let evs = events(f, id).await;
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

async fn wait_state(f: &Fixture, id: Uuid, state: SessionState) -> Session {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let s = session(f, id).await;
        if s.state == state {
            return s;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {state}; session is {}",
            s.state
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

async fn approvals(f: &Fixture, id: Uuid, status: Option<ApprovalStatus>) -> Vec<Approval> {
    f.client
        .call::<ApprovalsList>(ApprovalsListParams { status, session_id: Some(id), ..Default::default() })
        .await
        .unwrap()
        .approvals
}

async fn wait_pending(f: &Fixture, id: Uuid) -> Approval {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(a) = approvals(f, id, Some(ApprovalStatus::Pending)).await.pop() {
            return a;
        }
        assert!(tokio::time::Instant::now() < deadline, "no pending approval");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

async fn decide(f: &Fixture, approval: Uuid, decision: ApprovalDecision) -> Approval {
    f.client
        .call::<ApprovalsDecide>(ApprovalsDecideParams { approval_id: approval, decision, note: None })
        .await
        .unwrap()
        .approval
}

/// Approval answers the fake Codex actually received (its own log).
fn answers(thread: &str) -> Vec<Value> {
    let path = std::env::temp_dir().join("switchyard-fake-gemini").join(format!("answers-{thread}.jsonl"));
    // Strict: a malformed line fails the test; a line being written is awaited.
    common::read_jsonl(&path)
}

async fn wait_answers(thread: &str, n: usize) -> Vec<Value> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let a = answers(thread);
        if a.len() >= n {
            return a;
        }
        assert!(tokio::time::Instant::now() < deadline, "expected {n} answers, have {a:?}");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

/// The option the fake received (`proceed_once`, `cancel`) or `cancelled`.
fn decision_of(answer: &Value) -> Value {
    let o = &answer["result"]["outcome"];
    if o["outcome"] == "cancelled" { Value::from("cancelled") } else { o["optionId"].clone() }
}

async fn evaluations(f: &Fixture, session: Uuid) -> Vec<switchyard_protocol::model::PolicyEvaluation> {
    f.client
        .call::<PoliciesEvaluations>(PoliciesEvaluationsParams {
            session_id: Some(session),
            ..Default::default()
        })
        .await
        .unwrap()
        .evaluations
}

// --- detection and start ----------------------------------------------------

#[tokio::test]
async fn gemini_is_detected_next_to_claude_and_codex_with_honest_capabilities() {
    let f = fixture().await;
    let agents = f.client.call::<AgentsList>(AgentsListParams { refresh: true }).await.unwrap().agents;
    let providers: Vec<&str> = agents.iter().map(|a| a.provider.as_str()).collect();
    assert_eq!(providers, ["claude_code", "codex", "gemini"]);
    let g = &agents[2];
    assert!(g.installed && g.error.is_none(), "{g:?}");
    assert_eq!((g.version.as_deref(), g.tested_version.as_str()), (Some("0.70.0"), "0.70.0"));
    assert_eq!(g.display_name, "Gemini");
    let c = &g.capabilities;
    assert!(
        c.multi_turn.is_supported()
            && c.resume_session.is_supported()
            && c.permission_requests.is_supported()
    );
    assert!(!c.cost.is_supported() && !c.denial_reason_to_agent.is_supported());
    let models = f
        .client
        .call::<AgentsModels>(AgentsModelsParams { provider: "gemini".into(), refresh: true })
        .await
        .unwrap()
        .catalog;
    assert_eq!(models.error, None);
    let ids: Vec<&str> = models.models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        ["auto", "gemini-fake-flash", "gemini-fake-pro"],
        "from the CLI's session, never a list of ours"
    );
    // Switchyard's admin policy is written into its own directory.
    let policy =
        std::fs::read_to_string(f.core.dir.path().join("data/gemini/policy/switchyard.toml")).unwrap();
    assert!(policy.contains("argsPattern = \".*\""));
}

#[tokio::test]
async fn a_session_streams_text_and_reports_usage() {
    let f = fixture().await;
    let s = start(&f, f.workspace_id).await;
    assert!(s.native_session_id.is_some(), "the ACP session id");
    send(&f, s.id, "hello", false).await;
    let evs = wait_events(&f, s.id, "turn", |e| turns(e) == 1).await;
    let msg = of_type(&evs, "agent.message")[0].payload["text"].as_str().unwrap().to_owned();
    assert_eq!(msg, "Hello from fake gemini.");
    let done = &of_type(&evs, "agent.turn_completed")[0].payload;
    assert_eq!(done["usage"]["input_tokens"], 10);
    assert_eq!(done["interrupted"], false);
}

#[tokio::test]
async fn shell_asks_and_runs_only_when_approved_with_allow_once() {
    let f = fixture().await;
    let s = start(&f, f.workspace_id).await;
    let session_id = s.native_session_id.clone().unwrap();
    send(&f, s.id, "[fake:cmd:touch approved.txt]", false).await;
    let a = wait_pending(&f, s.id).await;
    assert_eq!(a.tool_name, "execute");
    decide(&f, a.id, ApprovalDecision::Approve).await;
    wait_events(&f, s.id, "turn", |e| turns(e) == 1).await;
    assert!(f.ws.join("approved.txt").exists());
    send(&f, s.id, "[fake:cmd:touch denied.txt]", false).await;
    let a = wait_pending(&f, s.id).await;
    decide(&f, a.id, ApprovalDecision::Deny).await;
    wait_events(&f, s.id, "second turn", |e| turns(e) == 2).await;
    assert!(!f.ws.join("denied.txt").exists(), "a denied command never runs");
    let got: Vec<Value> = wait_answers(&session_id, 2).await.iter().map(decision_of).collect();
    assert_eq!(got, [Value::from("proceed_once"), Value::from("cancel")], "never an 'always' option");
}

#[tokio::test]
async fn a_new_file_is_a_file_write_and_lands_in_the_workspace() {
    let f = fixture().await;
    let s = start(&f, f.workspace_id).await;
    send(&f, s.id, "[fake:write:hello.txt]", false).await;
    let a = wait_pending(&f, s.id).await;
    let evs = evaluations(&f, s.id).await;
    assert_eq!(evs.last().unwrap().category, OperationCategory::FileWrite);
    decide(&f, a.id, ApprovalDecision::Approve).await;
    wait_events(&f, s.id, "turn", |e| turns(e) == 1).await;
    assert!(f.ws.join("hello.txt").exists());
    assert!(!f.repo.join("hello.txt").exists(), "never the project checkout");
}

#[tokio::test]
async fn a_mode_that_acts_without_asking_stops_the_session() {
    let f = fixture().await;
    let s = start(&f, f.workspace_id).await;
    send(&f, s.id, "[fake:yolo]", false).await;
    let failed = wait_state(&f, s.id, SessionState::Failed).await;
    assert!(failed.failure.as_deref().unwrap_or_default().contains("yolo"), "{:?}", failed.failure);
}

#[tokio::test]
async fn interrupting_cancels_the_turn_and_a_pending_approval() {
    let f = fixture().await;
    let s = start(&f, f.workspace_id).await;
    let session_id = s.native_session_id.clone().unwrap();
    send(&f, s.id, "[fake:pending]", false).await;
    let a = wait_pending(&f, s.id).await;
    f.client.call::<SessionsInterrupt>(SessionIdParams { session_id: s.id }).await.unwrap();
    let evs = wait_events(&f, s.id, "cancelled turn", |e| turns(e) == 1).await;
    assert_eq!(of_type(&evs, "agent.turn_completed")[0].payload["interrupted"], true);
    let decided = approvals(&f, s.id, None).await.into_iter().find(|x| x.id == a.id).unwrap();
    assert_ne!(decided.status, ApprovalStatus::Pending);
    assert_eq!(decision_of(&wait_answers(&session_id, 1).await[0]), Value::from("cancelled"));
}

#[tokio::test]
async fn stop_and_resume_continue_the_same_session() {
    let f = fixture().await;
    let s = start(&f, f.workspace_id).await;
    let native = s.native_session_id.clone().unwrap();
    send(&f, s.id, "hello", false).await;
    wait_events(&f, s.id, "turn", |e| turns(e) == 1).await;
    f.client.call::<SessionsStop>(SessionIdParams { session_id: s.id }).await.unwrap();
    let resumed = f
        .client
        .call::<SessionsResume>(SessionsResumeParams { session_id: s.id, prompt: None })
        .await
        .unwrap()
        .session;
    assert_eq!(resumed.native_session_id.as_deref(), Some(native.as_str()));
    send(&f, s.id, "[fake:whoami]", false).await;
    let evs = wait_events(&f, s.id, "resumed turn", |e| turns(e) == 2).await;
    let last = of_type(&evs, "agent.message").last().unwrap().payload["text"].as_str().unwrap().to_owned();
    assert_eq!(last, native, "session/load continued the same ACP session");
}
