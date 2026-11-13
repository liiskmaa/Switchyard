#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Codex sessions end to end through the IPC protocol, using the fake Codex
//! app-server (`switchyard-fake-codex`). Core, storage, the policy engine,
//! the approval manager and Git worktrees are real; only the Codex CLI is a
//! deterministic stand-in. No network, no credentials. Real-CLI tests are in
//! `real_codex.rs`.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use switchyard_client::{CoreClient, Notification};
use switchyard_protocol::ErrorCode;
use switchyard_protocol::EventEnvelope;
use switchyard_protocol::methods::{
    AgentsList, AgentsListParams, ApprovalDecision, ApprovalsDecide, ApprovalsDecideParams, ApprovalsList,
    ApprovalsListParams, Delivery, EventsList, EventsListParams, LiveSubscribe, LiveSubscribeParams,
    PoliciesCreate, PoliciesCreateParams, PoliciesEvaluations, PoliciesEvaluationsParams, ProjectsAdd,
    ProjectsAddParams, SessionIdParams, SessionsGet, SessionsInterrupt, SessionsResume, SessionsResumeParams,
    SessionsSend, SessionsSendParams, SessionsStart, SessionsStartParams, SessionsStop, WorkspacesCreate,
    WorkspacesCreateParams,
};
use switchyard_protocol::model::{
    Approval, ApprovalStatus, DecisionSource, LiveKind, OperationCategory, PolicyConditions, PolicyDecision,
    Session, SessionState, Workspace,
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
    workspace_id: Uuid,
    ws: PathBuf,
}

async fn fixture() -> Fixture {
    let core = TestCore::start().await;
    let (client, rx) = core.connect().await;
    let repo = git_repo(core.dir.path(), "codex-repo");
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

fn codex_params(workspace_id: Uuid) -> SessionsStartParams {
    SessionsStartParams {
        workspace_id,
        project_id: None,
        provider: Some("codex".into()),
        prompt: None,
        model: None,
    }
}

async fn start(f: &Fixture, workspace_id: Uuid) -> Session {
    f.client.call::<SessionsStart>(codex_params(workspace_id)).await.unwrap().session
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
    let path = std::env::temp_dir().join("switchyard-fake-codex").join(format!("answers-{thread}.jsonl"));
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

fn decision_of(answer: &Value) -> Value {
    answer["answer"]["result"]["decision"].clone()
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

fn rule(
    project_id: Uuid,
    workspace_id: Option<Uuid>,
    category: OperationCategory,
    decision: PolicyDecision,
    command: Option<&str>,
    description: &str,
) -> PoliciesCreateParams {
    PoliciesCreateParams {
        project_id,
        workspace_id,
        category,
        decision,
        conditions: PolicyConditions { path_prefix: None, command: command.map(Into::into) },
        description: description.into(),
        enabled: None,
    }
}

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap()
}

// --- detection and start ----------------------------------------------------

#[tokio::test]
async fn codex_is_detected_next_to_claude_with_honest_capabilities() {
    let f = fixture().await;
    let agents = f.client.call::<AgentsList>(AgentsListParams { refresh: true }).await.unwrap().agents;
    let providers: Vec<&str> = agents.iter().map(|a| a.provider.as_str()).collect();
    assert_eq!(providers, ["claude_code", "codex"]);
    let codex = &agents[1];
    assert!(codex.installed && codex.error.is_none(), "{codex:?}");
    assert_eq!(codex.version.as_deref(), Some("0.166.0"));
    assert_eq!(codex.tested_version, "0.166.0");
    assert_eq!(codex.display_name, "Codex");
    let auth = codex.auth.as_ref().unwrap();
    assert!(auth.logged_in);
    assert_eq!(auth.method.as_deref(), Some("chatgpt"));
    let c = &codex.capabilities;
    assert!(
        c.multi_turn.is_supported()
            && c.resume_session.is_supported()
            && c.permission_requests.is_supported()
    );
    assert!(c.usage_tokens.is_supported());
    assert!(
        !c.cost.is_supported()
            && !c.denial_reason_to_agent.is_supported()
            && !c.file_read_tool.is_supported()
    );
    assert_eq!(c.steer_running_turn, switchyard_agents::Support::Unknown);
}

#[tokio::test]
async fn session_runs_in_its_workspace_with_the_thread_id_codex_assigned() {
    let f = fixture().await;
    let s = start(&f, f.workspace_id).await;
    assert_eq!(s.provider, "codex");
    assert_eq!(s.state, SessionState::Idle, "idle only after the thread is open");
    assert_eq!(s.cwd, f.ws.display().to_string());
    assert_ne!(f.ws, f.repo);
    let thread = s.native_session_id.clone().expect("thread id recorded before the session is idle");
    assert_ne!(thread, s.id.to_string());
    let evs = events(&f, s.id).await;
    let ready = of_type(&evs, "agent.ready");
    assert_eq!(ready[0].payload["native_session_id"], thread);
    assert_eq!(ready[0].payload["model"], "fake-codex-model");
    assert_eq!(of_type(&evs, "agent.started")[0].payload["provider"], "codex");
    for e in &evs {
        assert_eq!(e.context.workspace_id, Some(f.workspace_id), "{} lacks workspace identity", e.event_type);
        assert_eq!(e.context.project_id, Some(f.project_id));
    }
}

#[tokio::test]
async fn unknown_provider_is_rejected() {
    let f = fixture().await;
    let err = f
        .client
        .call::<SessionsStart>(SessionsStartParams {
            provider: Some("gemini".into()),
            ..codex_params(f.workspace_id)
        })
        .await
        .unwrap_err();
    assert!(
        matches!(err, switchyard_client::ClientError::Remote(ref e) if e.code == ErrorCode::InvalidParams),
        "{err:?}"
    );
}

// --- turns, streaming, usage ------------------------------------------------

#[tokio::test]
async fn streams_live_text_and_journals_complete_messages_and_usage() {
    let mut f = fixture().await;
    let s = start(&f, f.workspace_id).await;
    let sub = f.client.call::<LiveSubscribe>(LiveSubscribeParams { session_id: Some(s.id) }).await.unwrap();
    assert_eq!(send(&f, s.id, "say hello", false).await, Delivery::StartedTurn);
    let mut text = String::new();
    while !text.contains("fake codex.") {
        if let Notification::Live { subscription_id, output } = common::next_notification(&mut f.rx).await
            && subscription_id == sub.subscription_id
            && output.kind == LiveKind::Text
        {
            text.push_str(&output.text);
        }
    }
    let evs = wait_events(&f, s.id, "turn", |e| turns(e) == 1).await;
    assert_eq!(of_type(&evs, "agent.message")[0].payload["text"], "Hello from fake codex.");
    let done = &of_type(&evs, "agent.turn_completed")[0].payload;
    assert_eq!(done["success"], true);
    assert_eq!(done["usage"]["total_tokens"], 120);
    assert!(
        done["cost_usd"].is_null() && done.get("turn_cost_usd").is_none(),
        "Codex reports no cost: {done}"
    );
    assert!(of_type(&evs, "agent.message").len() == 1, "deltas are not journaled");
    wait_state(&f, s.id, SessionState::Idle).await;
}

#[tokio::test]
async fn multiple_turns_and_a_queued_message() {
    let f = fixture().await;
    let s = start(&f, f.workspace_id).await;
    send(&f, s.id, "[fake:slow]", false).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(send(&f, s.id, "[fake:whoami]", true).await, Delivery::QueuedForNextTurn);
    let evs = wait_events(&f, s.id, "two turns", |e| turns(e) == 2).await;
    let messages: Vec<&str> =
        of_type(&evs, "agent.message").iter().map(|e| e.payload["text"].as_str().unwrap()).collect();
    assert_eq!(
        messages,
        ["slow done", &format!("thread {}", session(&f, s.id).await.native_session_id.unwrap())]
    );
    wait_state(&f, s.id, SessionState::Idle).await;
    send(&f, s.id, "third", false).await;
    wait_events(&f, s.id, "third turn", |e| turns(e) == 3).await;
}

// --- approvals through the central policy engine ------------------------------

#[tokio::test]
async fn approved_patch_writes_only_into_the_workspace() {
    let f = fixture().await;
    let s = start(&f, f.workspace_id).await;
    let thread = s.native_session_id.clone().unwrap();
    send(&f, s.id, "[fake:write:notes.txt]", false).await;
    let a = wait_pending(&f, s.id).await;
    assert_eq!(a.tool_name, "apply_patch");
    assert_eq!(a.workspace_id, Some(f.workspace_id));
    assert_eq!(a.input["changes"][0]["kind"], "add");
    let policy = a.policy.as_ref().expect("the approval carries its evaluation");
    assert_eq!(policy.category, OperationCategory::FileWrite);
    assert_eq!(wait_state(&f, s.id, SessionState::WaitingForApproval).await.pending_approvals, 1);
    assert!(!f.ws.join("notes.txt").exists(), "nothing written before the decision");

    decide(&f, a.id, ApprovalDecision::Approve).await;
    let ans = wait_answers(&thread, 1).await;
    assert_eq!(decision_of(&ans[0]), "accept", "exactly this request, never acceptForSession");
    let evs = wait_events(&f, s.id, "turn", |e| turns(e) == 1).await;
    assert!(f.ws.join("notes.txt").exists());
    assert!(!f.repo.join("notes.txt").exists(), "the project checkout is untouched");
    assert_eq!(of_type(&evs, "agent.tool_completed")[0].payload["tool_name"], "apply_patch");
    let resolved = of_type(&evs, "agent.permission_resolved");
    assert_eq!(resolved[0].payload["status"], "approved");
    assert!(
        evaluations(&f, s.id)
            .await
            .iter()
            .any(|e| e.approval_id == Some(a.id) && e.decision == PolicyDecision::Ask),
        "the evaluation is durably linked to the approval"
    );
}

#[tokio::test]
async fn denied_command_is_declined_and_never_runs() {
    let f = fixture().await;
    let s = start(&f, f.workspace_id).await;
    let thread = s.native_session_id.clone().unwrap();
    send(&f, s.id, "[fake:cmd:touch denied.txt]", false).await;
    let a = wait_pending(&f, s.id).await;
    assert_eq!(a.tool_name, "shell");
    assert_eq!(a.input["command"], "touch denied.txt", "the shell wrapper is shown unwrapped");
    assert_eq!(a.input["raw_command"], "/bin/zsh -lc 'touch denied.txt'");
    assert_eq!(a.policy.as_ref().unwrap().category, OperationCategory::Shell);
    decide(&f, a.id, ApprovalDecision::Deny).await;
    let ans = wait_answers(&thread, 1).await;
    assert_eq!(decision_of(&ans[0]), "decline");
    let evs = wait_events(&f, s.id, "turn", |e| turns(e) == 1).await;
    assert!(!f.ws.join("denied.txt").exists());
    assert_eq!(of_type(&evs, "agent.tool_failed")[0].payload["rejected"], true);
    // A second decision cannot produce a second answer.
    let again = f
        .client
        .call::<ApprovalsDecide>(ApprovalsDecideParams {
            approval_id: a.id,
            decision: ApprovalDecision::Approve,
            note: None,
        })
        .await;
    assert!(again.is_err());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(answers(&thread).len(), 1);
}

#[tokio::test]
async fn exact_shell_rule_allows_without_a_person_and_never_reads_as_a_file_read() {
    let f = fixture().await;
    f.client
        .call::<PoliciesCreate>(rule(
            f.project_id,
            None,
            OperationCategory::Shell,
            PolicyDecision::Allow,
            Some("touch allowed.txt"),
            "Allow touching allowed.txt",
        ))
        .await
        .unwrap();
    let s = start(&f, f.workspace_id).await;
    let thread = s.native_session_id.clone().unwrap();
    send(&f, s.id, "[fake:cmd:touch allowed.txt]", false).await;
    let ans = wait_answers(&thread, 1).await;
    assert_eq!(decision_of(&ans[0]), "accept");
    wait_events(&f, s.id, "turn", |e| turns(e) == 1).await;
    assert!(f.ws.join("allowed.txt").exists());
    assert!(approvals(&f, s.id, None).await.is_empty(), "allowed by rule: no approval");
    // `cat` is still a shell command: it asks, it is not a built-in read.
    send(&f, s.id, "[fake:cmd:cat README.md]", false).await;
    let a = wait_pending(&f, s.id).await;
    assert_eq!(a.policy.unwrap().category, OperationCategory::Shell);
    decide(&f, a.id, ApprovalDecision::Deny).await;
}

#[tokio::test]
async fn unsupported_requests_fail_closed_and_are_audited() {
    let f = fixture().await;
    let s = start(&f, f.workspace_id).await;
    let thread = s.native_session_id.clone().unwrap();

    // A patch mixing an addition and an edit.
    send(&f, s.id, "[fake:mixed]", false).await;
    let ans = wait_answers(&thread, 1).await;
    assert_eq!(decision_of(&ans[0]), "decline");
    wait_events(&f, s.id, "mixed turn", |e| turns(e) == 1).await;
    assert!(!f.ws.join("a.txt").exists());

    // A permission-grant request: answered with an empty grant.
    send(&f, s.id, "[fake:perm]", false).await;
    let ans = wait_answers(&thread, 2).await;
    assert_eq!(ans[1]["answer"]["result"], serde_json::json!({"permissions": {}, "scope": "turn"}));
    wait_events(&f, s.id, "perm turn", |e| turns(e) == 2).await;

    // A sub-agent's command.
    send(&f, s.id, "[fake:subagent]", false).await;
    // The child's turn is interrupted (M5.1) and its request declined.
    let ans = wait_answers(&thread, 4).await;
    assert!(ans[2..].iter().any(|a| a.get("child_interrupt").is_some()));
    assert!(ans[2..].iter().any(|a| decision_of(a) == "decline"));
    wait_events(&f, s.id, "subagent turn", |e| turns(e) == 3).await;
    assert!(!f.ws.join("from-subagent.txt").exists());

    assert!(approvals(&f, s.id, None).await.is_empty(), "none of these was offered to a person");
    let evals = evaluations(&f, s.id).await;
    assert_eq!(evals.len(), 3);
    for e in &evals {
        assert_eq!(e.decision, PolicyDecision::Deny, "{e:?}");
        assert_eq!(e.adapter, "codex");
    }
}

#[tokio::test]
async fn activity_outside_approval_is_recorded_as_a_warning() {
    let f = fixture().await;
    let s = start(&f, f.workspace_id).await;
    send(&f, s.id, "[fake:unmediated]", false).await;
    let evs = wait_events(&f, s.id, "turn", |e| turns(e) == 1).await;
    assert!(of_type(&evs, "agent.warning").iter().any(|e| {
        e.payload["message"].as_str().unwrap().contains("does not pass through Switchyard approval")
    }));
    assert_eq!(of_type(&evs, "agent.tool_started")[0].payload["tool_name"], "mcpToolCall");
}

// --- interrupt, stop, resume -------------------------------------------------

#[tokio::test]
async fn interrupting_a_streaming_turn() {
    let f = fixture().await;
    let s = start(&f, f.workspace_id).await;
    send(&f, s.id, "[fake:slow]", false).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    f.client.call::<SessionsInterrupt>(SessionIdParams { session_id: s.id }).await.unwrap();
    let evs = wait_events(&f, s.id, "interrupt", |e| turns(e) == 1).await;
    assert_eq!(of_type(&evs, "agent.turn_completed")[0].payload["interrupted"], true);
    assert_eq!(of_type(&evs, "agent.interrupted").len(), 1);
    wait_state(&f, s.id, SessionState::Interrupted).await;
    send(&f, s.id, "again", false).await;
    wait_events(&f, s.id, "next turn", |e| turns(e) == 2).await;
}

#[tokio::test]
async fn interrupt_cancels_a_pending_approval() {
    let f = fixture().await;
    let s = start(&f, f.workspace_id).await;
    send(&f, s.id, "[fake:pending]", false).await;
    let a = wait_pending(&f, s.id).await;
    f.client.call::<SessionsInterrupt>(SessionIdParams { session_id: s.id }).await.unwrap();
    wait_state(&f, s.id, SessionState::Interrupted).await;
    let settled = approvals(&f, s.id, None).await.into_iter().find(|x| x.id == a.id).unwrap();
    assert_eq!(settled.status, ApprovalStatus::Cancelled);
    assert_eq!(settled.decision_source, Some(DecisionSource::AgentCancelled));
    assert!(answers(&s.native_session_id.unwrap()).is_empty(), "nothing approved after the interrupt");
}

#[tokio::test]
async fn stop_and_resume_continue_the_same_thread() {
    let f = fixture().await;
    let s = start(&f, f.workspace_id).await;
    let thread = s.native_session_id.clone().unwrap();
    send(&f, s.id, "remember the word sapphire", false).await;
    wait_events(&f, s.id, "turn", |e| turns(e) == 1).await;
    let stopped = f.client.call::<SessionsStop>(SessionIdParams { session_id: s.id }).await.unwrap().session;
    assert_eq!(stopped.state, SessionState::Stopped);
    assert!(stopped.pid.is_none() && !stopped.attached);

    let resumed = f
        .client
        .call::<SessionsResume>(SessionsResumeParams { session_id: s.id, prompt: None })
        .await
        .unwrap()
        .session;
    assert_eq!(resumed.state, SessionState::Idle);
    assert_eq!(resumed.native_session_id.as_deref(), Some(thread.as_str()), "same Codex thread");
    assert_eq!(resumed.resume_count, 1);
    send(&f, s.id, "[fake:remember]", false).await;
    let evs = wait_events(&f, s.id, "resumed turn", |e| turns(e) == 2).await;
    let last = of_type(&evs, "agent.message").last().unwrap().payload["text"].as_str().unwrap().to_owned();
    assert!(last.contains("sapphire"), "the conversation continues: {last}");
    assert_eq!(of_type(&evs, "agent.session_resumed").len(), 1);
}

#[tokio::test]
async fn resume_after_an_interrupted_turn() {
    let f = fixture().await;
    let s = start(&f, f.workspace_id).await;
    send(&f, s.id, "[fake:slow]", false).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    f.client.call::<SessionsInterrupt>(SessionIdParams { session_id: s.id }).await.unwrap();
    wait_state(&f, s.id, SessionState::Interrupted).await;
    f.client.call::<SessionsStop>(SessionIdParams { session_id: s.id }).await.unwrap();
    let r = f
        .client
        .call::<SessionsResume>(SessionsResumeParams { session_id: s.id, prompt: None })
        .await
        .unwrap();
    assert_eq!(r.session.state, SessionState::Idle);
    send(&f, s.id, "[fake:remember]", false).await;
    let evs = wait_events(&f, s.id, "turn", |e| turns(e) == 2).await;
    assert!(
        of_type(&evs, "agent.message").last().unwrap().payload["text"]
            .as_str()
            .unwrap()
            .contains("[fake:slow]")
    );
}

#[tokio::test]
async fn a_missing_thread_is_never_replaced_by_a_new_one() {
    let f = fixture().await;
    let s = start(&f, f.workspace_id).await;
    let thread = s.native_session_id.clone().unwrap();
    f.client.call::<SessionsStop>(SessionIdParams { session_id: s.id }).await.unwrap();
    // Codex lost the thread (deleted rollout).
    std::fs::remove_file(
        std::env::temp_dir().join("switchyard-fake-codex").join(format!("thread-{thread}.json")),
    )
    .unwrap();
    let err = f.client.call::<SessionsResume>(SessionsResumeParams { session_id: s.id, prompt: None }).await;
    assert!(err.is_err(), "resume must fail, not start a new conversation");
    let after = wait_state(&f, s.id, SessionState::Failed).await;
    assert_eq!(after.native_session_id.as_deref(), Some(thread.as_str()), "identity unchanged");
    assert!(after.failure.as_deref().is_some_and(|m| m.contains("no rollout found")), "{:?}", after.failure);
    let evs = events(&f, s.id).await;
    assert_eq!(of_type(&evs, "agent.ready").len(), 1, "no second thread was opened");
    assert_eq!(of_type(&evs, "agent.failed").last().unwrap().payload["reason"], "adapter_failed");
}

#[tokio::test]
async fn unexpected_exit_fails_the_session_with_diagnostics() {
    let f = fixture().await;
    let s = start(&f, f.workspace_id).await;
    send(&f, s.id, "[fake:crash]", false).await;
    let after = wait_state(&f, s.id, SessionState::Failed).await;
    assert_eq!(after.exit_code, Some(3));
    assert!(after.failure.as_deref().is_some_and(|m| m.contains("simulated crash")), "{:?}", after.failure);
    assert!(!after.attached);
}

#[tokio::test]
async fn core_shutdown_stops_codex_and_cancels_pending_approvals() {
    let mut f = fixture().await;
    let s = start(&f, f.workspace_id).await;
    let pid = s.pid.unwrap();
    send(&f, s.id, "[fake:pending]", false).await;
    let a = wait_pending(&f, s.id).await;
    f.core
        .restart_with(|c| {
            c.codex_executable = Some(PathBuf::from(common::FAKE_CODEX));
        })
        .await;
    let (client, _rx) = f.core.connect().await;
    f.client = client;
    let after = session(&f, s.id).await;
    assert_eq!(after.state, SessionState::Stopped);
    assert!(!after.attached);
    let settled = approvals(&f, s.id, None).await.into_iter().find(|x| x.id == a.id).unwrap();
    assert_eq!(settled.decision_source, Some(DecisionSource::CoreShutdown));
    let alive = std::process::Command::new("kill").args(["-0", &pid.to_string()]).status().unwrap().success();
    assert!(!alive, "no orphaned Codex process");
    // Nothing restarts it automatically; an explicit resume does.
    let r = f
        .client
        .call::<SessionsResume>(SessionsResumeParams { session_id: s.id, prompt: None })
        .await
        .unwrap();
    assert_eq!(r.session.state, SessionState::Idle);
}

// --- workspace isolation and coexistence -------------------------------------

#[tokio::test]
async fn two_codex_sessions_in_two_workspaces_stay_isolated() {
    let f = fixture().await;
    let ws2 = create_workspace(&f.client, f.project_id).await;
    let ws2_path = PathBuf::from(&ws2.path);
    // A workspace rule in workspace 2 must not leak into workspace 1.
    f.client
        .call::<PoliciesCreate>(rule(
            f.project_id,
            Some(ws2.id),
            OperationCategory::Shell,
            PolicyDecision::Allow,
            Some("touch two.txt"),
            "Allow in workspace 2 only",
        ))
        .await
        .unwrap();
    let s1 = start(&f, f.workspace_id).await;
    let s2 = start(&f, ws2.id).await;
    assert_ne!(s1.native_session_id, s2.native_session_id);
    assert_eq!(canonical(Path::new(&s1.cwd)), canonical(&f.ws));
    assert_eq!(canonical(Path::new(&s2.cwd)), canonical(&ws2_path));

    // Run concurrently.
    send(&f, s1.id, "[fake:cmd:touch two.txt]", false).await;
    send(&f, s2.id, "[fake:cmd:touch two.txt]", false).await;
    // Workspace 2: allowed by its rule. Workspace 1: asks.
    let ans2 = wait_answers(&s2.native_session_id.clone().unwrap(), 1).await;
    assert_eq!(decision_of(&ans2[0]), "accept");
    let a1 = wait_pending(&f, s1.id).await;
    assert_eq!(a1.workspace_id, Some(f.workspace_id));
    decide(&f, a1.id, ApprovalDecision::Deny).await;
    wait_events(&f, s1.id, "s1 turn", |e| turns(e) == 1).await;
    let evs2 = wait_events(&f, s2.id, "s2 turn", |e| turns(e) == 1).await;

    assert!(ws2_path.join("two.txt").exists());
    assert!(!f.ws.join("two.txt").exists(), "workspace 1 never got the file");
    assert!(!f.repo.join("two.txt").exists(), "the project checkout is untouched");
    assert!(evs2.iter().all(|e| e.context.workspace_id == Some(ws2.id)));
    assert!(events(&f, s1.id).await.iter().all(|e| e.context.workspace_id == Some(f.workspace_id)));
}

#[tokio::test]
async fn claude_and_codex_run_side_by_side() {
    let f = fixture().await;
    let ws2 = create_workspace(&f.client, f.project_id).await;
    let codex = start(&f, f.workspace_id).await;
    let claude = f
        .client
        .call::<SessionsStart>(SessionsStartParams { provider: None, ..codex_params(ws2.id) })
        .await
        .unwrap()
        .session;
    assert_eq!((codex.provider.as_str(), claude.provider.as_str()), ("codex", "claude_code"));
    assert!(claude.capabilities.cost.is_supported() && !codex.capabilities.cost.is_supported());
    send(&f, codex.id, "hello codex", false).await;
    send(&f, claude.id, "hello claude", false).await;
    let ce = wait_events(&f, codex.id, "codex turn", |e| turns(e) == 1).await;
    let cl = wait_events(&f, claude.id, "claude turn", |e| turns(e) == 1).await;
    assert_eq!(of_type(&ce, "agent.message")[0].payload["text"], "Hello from fake codex.");
    assert!(of_type(&cl, "agent.message")[0].payload["text"].as_str().unwrap().contains("fake claude"));
    // A second session cannot take a workspace that one already owns.
    let err = f.client.call::<SessionsStart>(codex_params(ws2.id)).await.unwrap_err();
    assert!(
        matches!(err, switchyard_client::ClientError::Remote(ref e) if e.code == ErrorCode::WorkspaceOccupied)
    );
}
