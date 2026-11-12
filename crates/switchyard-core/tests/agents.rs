#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Agent sessions end to end through the IPC protocol, using the fake
//! Claude Code CLI (`switchyard-fake-claude`). No network, no credentials.

mod common;

use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;
use switchyard_client::{ClientError, CoreClient, Notification};
use switchyard_protocol::ErrorCode;
use switchyard_protocol::EventEnvelope;
use switchyard_protocol::methods::{
    AgentsList, AgentsListParams, ApprovalDecision, ApprovalsDecide, ApprovalsDecideParams, ApprovalsList,
    ApprovalsListParams, Delivery, EventsList, EventsListParams, LiveSubscribe, LiveSubscribeParams,
    ProjectIdParams, ProjectsAdd, ProjectsAddParams, ProjectsRemove, SessionIdParams, SessionsGet,
    SessionsInterrupt, SessionsList, SessionsListParams, SessionsResume, SessionsResumeParams, SessionsSend,
    SessionsSendParams, SessionsStart, SessionsStartParams, SessionsStop, WorkspacesCreate,
    WorkspacesCreateParams,
};
use switchyard_protocol::model::{
    Approval, ApprovalStatus, DecisionSource, LiveKind, Session, SessionState, Workspace,
};
use tokio::sync::mpsc;
use uuid::Uuid;

use common::{TestCore, git_repo};

struct Fixture {
    core: TestCore,
    client: CoreClient,
    rx: mpsc::Receiver<Notification>,
    repo: PathBuf,
    project_id: Uuid,
    /// The workspace sessions run in, and its directory (the agent's cwd).
    workspace_id: Uuid,
    ws: PathBuf,
}

async fn fixture_with(configure: impl FnOnce(&mut switchyard_core::CoreConfig)) -> Fixture {
    let core = TestCore::start_with(configure).await;
    let (client, rx) = core.connect().await;
    let repo = git_repo(core.dir.path(), "agent-repo");
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

fn start_params(workspace_id: Uuid) -> SessionsStartParams {
    SessionsStartParams { workspace_id, project_id: None, provider: None, prompt: None, model: None }
}

async fn fixture() -> Fixture {
    fixture_with(|_| {}).await
}

fn code(err: ClientError) -> ErrorCode {
    match err {
        ClientError::Remote(e) => e.code,
        other => panic!("expected remote error, got {other:?}"),
    }
}

async fn start(f: &Fixture) -> Session {
    f.client.call::<SessionsStart>(start_params(f.workspace_id)).await.unwrap().session
}

async fn send(f: &Fixture, session: Uuid, text: &str) -> Result<Delivery, ClientError> {
    f.client
        .call::<SessionsSend>(SessionsSendParams {
            session_id: session,
            text: text.into(),
            queue_if_busy: false,
        })
        .await
        .map(|r| r.delivery)
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

fn of_type<'a>(events: &'a [EventEnvelope], t: &str) -> Vec<&'a EventEnvelope> {
    events.iter().filter(|e| e.event_type == t).collect()
}

/// Polls until `pred` holds for the session's journal.
async fn wait_events(
    f: &Fixture,
    id: Uuid,
    what: &str,
    pred: impl Fn(&[EventEnvelope]) -> bool,
) -> Vec<EventEnvelope> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
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
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
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

async fn pending(f: &Fixture, id: Uuid) -> Vec<Approval> {
    f.client
        .call::<ApprovalsList>(ApprovalsListParams {
            status: Some(ApprovalStatus::Pending),
            session_id: Some(id),
            ..Default::default()
        })
        .await
        .unwrap()
        .approvals
}

async fn wait_pending(f: &Fixture, id: Uuid, n: usize) -> Vec<Approval> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let p = pending(f, id).await;
        if p.len() == n {
            return p;
        }
        assert!(tokio::time::Instant::now() < deadline, "expected {n} pending approvals, have {}", p.len());
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

async fn decide(
    f: &Fixture,
    approval: Uuid,
    decision: ApprovalDecision,
    note: Option<&str>,
) -> Result<Approval, ClientError> {
    f.client
        .call::<ApprovalsDecide>(ApprovalsDecideParams {
            approval_id: approval,
            decision,
            note: note.map(str::to_owned),
        })
        .await
        .map(|r| r.approval)
}

fn turns_done(evs: &[EventEnvelope]) -> usize {
    of_type(evs, "agent.turn_completed").len()
}

#[tokio::test]
async fn detects_cli_version_and_auth_without_account_identity() {
    let f = fixture().await;
    let agents = f.client.call::<AgentsList>(AgentsListParams { refresh: true }).await.unwrap().agents;
    let claude = &agents[0];
    assert!(claude.installed && claude.error.is_none());
    assert_eq!(claude.version.as_deref(), Some("2.1.322"));
    assert_eq!(claude.executable.as_deref(), Some(common::FAKE_CLAUDE));
    let auth = claude.auth.as_ref().unwrap();
    assert!(auth.logged_in);
    assert_eq!(auth.method.as_deref(), Some("fake"));
    let raw = serde_json::to_string(&agents).unwrap();
    assert!(!raw.contains("never-read@example.invalid"), "account identity must not be exposed");
    assert!(claude.capabilities.interrupt_turn && !claude.capabilities.steer_running_turn);
}

#[tokio::test]
async fn missing_cli_is_reported_and_start_refused() {
    let f = fixture_with(|c| c.claude_executable = Some(PathBuf::from("/nonexistent/claude"))).await;
    let agents = f.client.call::<AgentsList>(AgentsListParams { refresh: true }).await.unwrap().agents;
    assert!(agents[0].error.is_some());
    let err = f.client.call::<SessionsStart>(start_params(f.workspace_id)).await.unwrap_err();
    assert_eq!(code(err), ErrorCode::AgentUnavailable);
}

#[tokio::test]
async fn session_streams_live_output_and_journals_complete_messages_only() {
    let mut f = fixture().await;
    let s = start(&f).await;
    assert_eq!(s.state, SessionState::Idle);
    assert!(s.attached && s.pid.is_some());
    assert_eq!(s.cwd, f.ws.display().to_string());
    assert_eq!(s.workspace_id, Some(f.workspace_id));
    assert_ne!(f.ws, f.repo, "sessions never run in the project checkout");
    let native = s.native_session_id.clone().unwrap();
    assert_ne!(native, s.id.to_string(), "Switchyard and native ids are distinct, mapped identities");

    let sub = f.client.call::<LiveSubscribe>(LiveSubscribeParams { session_id: Some(s.id) }).await.unwrap();
    assert_eq!(send(&f, s.id, "say hello").await.unwrap(), Delivery::StartedTurn);

    let mut text = String::new();
    while !text.contains("fake claude.") {
        match common::next_notification(&mut f.rx).await {
            Notification::Live { subscription_id, output } if subscription_id == sub.subscription_id => {
                assert_eq!(output.session_id, s.id);
                if output.kind == LiveKind::Text {
                    text.push_str(&output.text);
                }
            }
            _ => {}
        }
    }
    assert_eq!(text, "Hello from fake claude.");

    let evs = wait_events(&f, s.id, "turn completion", |e| turns_done(e) == 1).await;
    let ready = of_type(&evs, "agent.ready");
    assert_eq!(ready[0].payload["native_session_id"], native.as_str());
    let messages = of_type(&evs, "agent.message");
    assert_eq!(messages.len(), 1, "one durable message, not one event per token");
    assert_eq!(messages[0].payload["text"], "Hello from fake claude.");
    assert!(
        evs.iter().all(|e| e.context.session_id == Some(s.id) && e.context.project_id == Some(f.project_id))
    );
    assert!(evs.iter().all(|e| e.context.agent_id == Some(s.agent_id)));
    let started = of_type(&evs, "agent.started")[0];
    assert_eq!(started.payload["cli_version"], "2.1.322");
    wait_state(&f, s.id, SessionState::Idle).await;
}

#[tokio::test]
async fn ephemeral_output_does_not_flood_the_journal() {
    let f = fixture().await;
    let s = start(&f).await;
    send(&f, s.id, "[fake:slow]").await.unwrap();
    let evs = wait_events(&f, s.id, "slow turn", |e| turns_done(e) == 1).await;
    // 60 streamed chunks + final message: a handful of durable events.
    assert!(evs.len() < 15, "journal has {} events", evs.len());
    assert_eq!(of_type(&evs, "agent.message").len(), 2);
}

#[tokio::test]
async fn approved_permission_runs_the_tool_and_file_change_is_observed() {
    let f = fixture().await;
    let s = start(&f).await;
    send(&f, s.id, "[fake:write:approved.txt]").await.unwrap();
    let approvals = wait_pending(&f, s.id, 1).await;
    let a = &approvals[0];
    assert_eq!(a.tool_name, "Write");
    assert_eq!(a.project_id, f.project_id);
    assert!(a.tool_use_id.as_deref().is_some_and(|t| t.starts_with("toolu_fake")));
    assert_eq!(a.input["content"], "written by fake claude\n");
    assert!(a.expires_at_ms > a.requested_at_ms);
    assert_eq!(session(&f, s.id).await.state, SessionState::WaitingForApproval);
    assert!(!f.ws.join("approved.txt").exists(), "nothing runs before the decision");

    let decided = decide(&f, a.id, ApprovalDecision::Approve, None).await.unwrap();
    assert_eq!(decided.status, ApprovalStatus::Approved);
    assert_eq!(decided.decision_source, Some(DecisionSource::Operator));
    assert_eq!(decided.decided_by.as_deref(), Some("integration-test"));
    let again = decide(&f, a.id, ApprovalDecision::Deny, None).await.unwrap_err();
    assert_eq!(code(again), ErrorCode::Conflict, "a decision is final");

    let evs = wait_events(&f, s.id, "turn and file change", |e| {
        turns_done(e) == 1 && !of_type(e, "files.changed").is_empty()
    })
    .await;
    assert!(f.ws.join("approved.txt").is_file());
    let tool = of_type(&evs, "agent.tool_completed")[0];
    let started = of_type(&evs, "agent.tool_started")[0];
    assert_eq!(tool.context.causation_id, Some(started.id));
    let resolved = of_type(&evs, "agent.permission_resolved")[0];
    assert_eq!(resolved.payload["status"], "approved");
    let changes: Vec<&Value> = of_type(&evs, "files.changed")
        .iter()
        .flat_map(|e| e.payload["changes"].as_array().unwrap().iter())
        .collect();
    assert!(changes.iter().any(|c| c["path"] == "approved.txt" && c["kind"] == "created"), "{changes:?}");
    assert!(changes.iter().all(|c| !c["path"].as_str().unwrap().starts_with(".git")));
    wait_state(&f, s.id, SessionState::Idle).await;
}

#[tokio::test]
async fn denied_permission_reaches_the_agent_and_nothing_is_written() {
    let f = fixture().await;
    let s = start(&f).await;
    send(&f, s.id, "[fake:write:denied.txt]").await.unwrap();
    let a = wait_pending(&f, s.id, 1).await.remove(0);
    decide(&f, a.id, ApprovalDecision::Deny, Some("not in this repo")).await.unwrap();
    let evs = wait_events(&f, s.id, "turn", |e| turns_done(e) == 1).await;
    assert!(!f.ws.join("denied.txt").exists());
    let failed = of_type(&evs, "agent.tool_failed")[0];
    assert_eq!(failed.payload["rejected"], true);
    assert_eq!(failed.payload["output"], "not in this repo", "the agent received the operator's message");
    assert_eq!(of_type(&evs, "agent.tool_completed").len(), 0);
}

#[tokio::test]
async fn concurrent_requests_keep_waiting_until_all_are_decided() {
    let f = fixture().await;
    let s = start(&f).await;
    send(&f, s.id, "[fake:multi]").await.unwrap();
    let approvals = wait_pending(&f, s.id, 2).await;
    decide(&f, approvals[0].id, ApprovalDecision::Approve, None).await.unwrap();
    assert_eq!(session(&f, s.id).await.state, SessionState::WaitingForApproval);
    decide(&f, approvals[1].id, ApprovalDecision::Deny, None).await.unwrap();
    wait_events(&f, s.id, "turn", |e| turns_done(e) == 1).await;
    let written = ["a.txt", "b.txt"].iter().filter(|n| f.ws.join(n).exists()).count();
    assert_eq!(written, 1);
}

#[tokio::test]
async fn turn_completion_journals_the_turn_cost_and_the_session_total_separately() {
    let f = fixture().await;
    let s = start(&f).await;
    send(&f, s.id, "first").await.unwrap();
    wait_events(&f, s.id, "first turn", |e| turns_done(e) == 1).await;
    wait_state(&f, s.id, SessionState::Idle).await;
    send(&f, s.id, "second").await.unwrap();
    let evs = wait_events(&f, s.id, "second turn", |e| turns_done(e) == 2).await;
    let costs: Vec<_> = of_type(&evs, "agent.turn_completed")
        .iter()
        .map(|e| (e.payload["turn_cost_usd"].as_f64().unwrap(), e.payload["cost_usd"].as_f64().unwrap()))
        .collect();
    let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
    // The fake CLI charges 0.001 per turn, like the real CLI's per-result usage.
    assert!(close(costs[0].0, 0.001) && close(costs[0].1, 0.001), "{costs:?}");
    assert!(
        close(costs[1].0, 0.001) && close(costs[1].1, 0.002),
        "turn cost is not the running total: {costs:?}"
    );
}

#[tokio::test]
async fn interrupting_a_streaming_turn_keeps_the_session_usable() {
    let f = fixture().await;
    let s = start(&f).await;
    send(&f, s.id, "[fake:slow]").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let after =
        f.client.call::<SessionsInterrupt>(SessionIdParams { session_id: s.id }).await.unwrap().session;
    assert!(after.attached);
    let evs = wait_events(&f, s.id, "interrupt", |e| turns_done(e) == 1).await;
    let aborted = &of_type(&evs, "agent.turn_completed")[0].payload;
    assert_eq!(aborted["interrupted"], true);
    assert!(aborted.get("turn_cost_usd").is_none(), "an aborted turn reports no turn cost: {aborted}");
    assert_eq!(of_type(&evs, "agent.interrupted").len(), 1);
    wait_state(&f, s.id, SessionState::Interrupted).await;
    assert_eq!(send(&f, s.id, "again").await.unwrap(), Delivery::StartedTurn);
    wait_events(&f, s.id, "second turn", |e| turns_done(e) == 2).await;
    let err = f.client.call::<SessionsInterrupt>(SessionIdParams { session_id: s.id }).await;
    wait_state(&f, s.id, SessionState::Idle).await;
    let _ = err;
    let idle_err =
        f.client.call::<SessionsInterrupt>(SessionIdParams { session_id: s.id }).await.unwrap_err();
    assert_eq!(code(idle_err), ErrorCode::InvalidState);
}

#[tokio::test]
async fn interrupt_while_waiting_for_approval_cancels_the_request() {
    let f = fixture().await;
    let s = start(&f).await;
    send(&f, s.id, "[fake:write:never.txt]").await.unwrap();
    let a = wait_pending(&f, s.id, 1).await.remove(0);
    f.client.call::<SessionsInterrupt>(SessionIdParams { session_id: s.id }).await.unwrap();
    wait_state(&f, s.id, SessionState::Interrupted).await;
    let all = f
        .client
        .call::<ApprovalsList>(ApprovalsListParams { session_id: Some(s.id), ..Default::default() })
        .await
        .unwrap()
        .approvals;
    assert_eq!(all[0].status, ApprovalStatus::Cancelled);
    assert_eq!(all[0].decision_source, Some(DecisionSource::AgentCancelled));
    assert!(!f.ws.join("never.txt").exists());
    let late = decide(&f, a.id, ApprovalDecision::Approve, None).await.unwrap_err();
    assert_eq!(code(late), ErrorCode::Conflict, "cancelled requests cannot be approved");
}

#[tokio::test]
async fn messages_during_a_turn_require_explicit_queueing() {
    let f = fixture().await;
    let s = start(&f).await;
    send(&f, s.id, "[fake:slow]").await.unwrap();
    let busy = send(&f, s.id, "next").await.unwrap_err();
    assert_eq!(code(busy), ErrorCode::InvalidState);
    let queued = f
        .client
        .call::<SessionsSend>(SessionsSendParams {
            session_id: s.id,
            text: "next".into(),
            queue_if_busy: true,
        })
        .await
        .unwrap();
    assert_eq!(queued.delivery, Delivery::QueuedForNextTurn);
    let evs = wait_events(&f, s.id, "two turns", |e| turns_done(e) == 2).await;
    let started = of_type(&evs, "agent.turn_started");
    assert_eq!(
        started.iter().map(|e| e.payload["queued"].as_bool().unwrap()).collect::<Vec<_>>(),
        [false, true]
    );
    wait_state(&f, s.id, SessionState::Idle).await;
}

#[tokio::test]
async fn crash_is_reported_with_exit_code_and_stderr() {
    let f = fixture().await;
    let s = start(&f).await;
    send(&f, s.id, "[fake:crash]").await.unwrap();
    let failed = wait_state(&f, s.id, SessionState::Failed).await;
    assert!(!failed.attached && failed.pid.is_none());
    assert_eq!(failed.exit_code, Some(3));
    let failure = failed.failure.unwrap();
    assert!(failure.contains("exit code 3") && failure.contains("simulated fatal error"), "{failure}");
    let evs = events(&f, s.id).await;
    assert_eq!(of_type(&evs, "agent.failed").len(), 1);
    assert_eq!(of_type(&evs, "agent.exited")[0].payload["final_state"], "failed");
    let err = send(&f, s.id, "hello?").await.unwrap_err();
    assert_eq!(code(err), ErrorCode::InvalidState);
}

#[tokio::test]
async fn startup_failure_is_reported_accurately() {
    let f = fixture().await;
    std::fs::write(f.ws.join(".fake-claude-fail"), "").unwrap();
    let err = f.client.call::<SessionsStart>(start_params(f.workspace_id)).await.unwrap_err();
    let ClientError::Remote(e) = err else { panic!("expected remote error") };
    assert_eq!(e.code, ErrorCode::AgentFailed);
    let id: Uuid = serde_json::from_value(e.data.unwrap()["session_id"].clone()).unwrap();
    let s = session(&f, id).await;
    assert_eq!(s.state, SessionState::Failed);
    assert!(s.failure.unwrap().contains("Invalid API key"));
}

#[tokio::test]
async fn malformed_output_is_tolerated_and_reported() {
    let f = fixture().await;
    let s = start(&f).await;
    send(&f, s.id, "[fake:malformed]").await.unwrap();
    let evs = wait_events(&f, s.id, "turn", |e| turns_done(e) == 1).await;
    assert_eq!(of_type(&evs, "agent.warning").len(), 2, "invalid JSON and non-object lines");
    assert_eq!(session(&f, s.id).await.state, SessionState::Idle);
}

#[tokio::test]
async fn one_live_session_per_workspace_and_project_removal_is_guarded() {
    let f = fixture().await;
    let s = start(&f).await;
    let dup = f.client.call::<SessionsStart>(start_params(f.workspace_id)).await.unwrap_err();
    assert_eq!(code(dup), ErrorCode::WorkspaceOccupied);
    // The same project can run another session in another workspace.
    let other_ws = create_workspace(&f.client, f.project_id).await;
    let second = f.client.call::<SessionsStart>(start_params(other_ws.id)).await.unwrap().session;
    assert_eq!(second.workspace_id, Some(other_ws.id));
    let rm = f.client.call::<ProjectsRemove>(ProjectIdParams { project_id: f.project_id }).await.unwrap_err();
    assert_eq!(code(rm), ErrorCode::Conflict);
    let other = f
        .client
        .call::<SessionsStart>(SessionsStartParams {
            provider: Some("codex".into()),
            ..start_params(f.workspace_id)
        })
        .await
        .unwrap_err();
    assert_eq!(code(other), ErrorCode::Unsupported);
    let _ = s;
}

#[tokio::test]
async fn stop_then_resume_keeps_identity_mapping() {
    let f = fixture().await;
    let s = start(&f).await;
    let native = s.native_session_id.clone().unwrap();
    send(&f, s.id, "hi").await.unwrap();
    wait_events(&f, s.id, "turn", |e| turns_done(e) == 1).await;
    let stopped = f.client.call::<SessionsStop>(SessionIdParams { session_id: s.id }).await.unwrap().session;
    assert_eq!(stopped.state, SessionState::Stopped);
    assert!(!stopped.attached && stopped.pid.is_none() && stopped.ended_at_ms.is_some());

    let resumed = f
        .client
        .call::<SessionsResume>(SessionsResumeParams {
            session_id: s.id,
            prompt: Some("[fake:whoami]".into()),
        })
        .await
        .unwrap()
        .session;
    assert_eq!(resumed.id, s.id);
    assert_eq!(resumed.native_session_id.as_deref(), Some(native.as_str()));
    assert_eq!(resumed.resume_count, 1);
    assert_ne!(resumed.pid, s.pid);
    let evs = wait_events(&f, s.id, "resumed turn", |e| turns_done(e) == 2).await;
    let resumed_ev = of_type(&evs, "agent.session_resumed");
    assert_eq!(resumed_ev[0].payload["native_session_id"], native.as_str());
    let last = of_type(&evs, "agent.message").last().unwrap().payload["text"].clone();
    assert_eq!(last, format!("session {native}"), "the new process continued the same native session");

    let dup = f
        .client
        .call::<SessionsResume>(SessionsResumeParams { session_id: s.id, prompt: None })
        .await
        .unwrap_err();
    assert_eq!(code(dup), ErrorCode::Conflict, "never two processes for one session");
}

#[tokio::test]
async fn reconnecting_client_sees_the_same_live_session_and_pending_approval() {
    let f = fixture().await;
    let s = start(&f).await;
    send(&f, s.id, "[fake:write:after-reconnect.txt]").await.unwrap();
    wait_pending(&f, s.id, 1).await;
    let Fixture { core, client, rx, repo, project_id, workspace_id, ws } = f;
    drop(rx);
    client.close().await;
    drop(client);
    tokio::time::sleep(Duration::from_millis(200)).await;

    let (client, rx) = core.connect().await;
    let f = Fixture { core, client, rx, repo, project_id, workspace_id, ws };
    let sessions = f
        .client
        .call::<SessionsList>(SessionsListParams { live_only: true, ..Default::default() })
        .await
        .unwrap()
        .sessions;
    assert_eq!(sessions.len(), 1, "no duplicate process after reconnect");
    assert_eq!((sessions[0].id, sessions[0].pid), (s.id, s.pid));
    assert_eq!(sessions[0].state, SessionState::WaitingForApproval);
    assert_eq!(sessions[0].pending_approvals, 1);
    let a = pending(&f, s.id).await.remove(0);
    decide(&f, a.id, ApprovalDecision::Approve, None).await.unwrap();
    wait_events(&f, s.id, "turn", |e| turns_done(e) == 1).await;
    assert!(f.ws.join("after-reconnect.txt").is_file());
}

#[tokio::test]
async fn unanswered_requests_expire_as_denials() {
    let f = fixture_with(|c| c.approval_timeout = Duration::from_millis(500)).await;
    let s = start(&f).await;
    send(&f, s.id, "[fake:write:expired.txt]").await.unwrap();
    let evs = wait_events(&f, s.id, "expiry", |e| turns_done(e) == 1).await;
    let resolved = of_type(&evs, "agent.permission_resolved")[0];
    assert_eq!(
        (resolved.payload["status"].as_str(), resolved.payload["source"].as_str()),
        (Some("expired"), Some("timeout"))
    );
    assert_eq!(of_type(&evs, "agent.tool_failed")[0].payload["rejected"], true);
    assert!(!f.ws.join("expired.txt").exists());
}

#[tokio::test]
async fn core_shutdown_stops_sessions_and_cancels_approvals_durably() {
    let mut f = fixture().await;
    let s = start(&f).await;
    send(&f, s.id, "[fake:write:shutdown.txt]").await.unwrap();
    wait_pending(&f, s.id, 1).await;
    f.core.restart().await;
    let (client, rx) = f.core.connect().await;
    f.client = client;
    f.rx = rx;
    let after = session(&f, s.id).await;
    assert_eq!(after.state, SessionState::Stopped);
    assert_eq!(after.failure.as_deref(), Some("core_shutdown"));
    assert!(!after.attached);
    let all = f
        .client
        .call::<ApprovalsList>(ApprovalsListParams { session_id: Some(s.id), ..Default::default() })
        .await
        .unwrap()
        .approvals;
    assert_eq!(
        (all[0].status, all[0].decision_source),
        (ApprovalStatus::Cancelled, Some(DecisionSource::CoreShutdown))
    );
    assert!(!f.ws.join("shutdown.txt").exists());
}
