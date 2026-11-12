#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Adversarial Codex tests (M5.1 audit) through the real Core, policy engine,
//! approval manager and Git worktrees, with the fake app-server
//! (`switchyard-fake-codex`). The fake only changes files after an `accept`,
//! so a missing file proves that a denied or unanswered operation had no
//! effect. The guarantee that the *real* Codex honours `decline` is covered by
//! `real_codex.rs`.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use switchyard_client::CoreClient;
use switchyard_protocol::EventEnvelope;
use switchyard_protocol::methods::{
    AgentsList, AgentsListParams, ApprovalDecision, ApprovalsDecide, ApprovalsDecideParams, ApprovalsList,
    ApprovalsListParams, EventsList, EventsListParams, PoliciesCreate, PoliciesCreateParams,
    PoliciesEvaluations, PoliciesEvaluationsParams, ProjectsAdd, ProjectsAddParams, SessionIdParams,
    SessionsGet, SessionsResume, SessionsResumeParams, SessionsSend, SessionsSendParams, SessionsStart,
    SessionsStartParams, WorkspacesCreate, WorkspacesCreateParams,
};
use switchyard_protocol::model::{
    Approval, ApprovalStatus, DecisionSource, OperationCategory, PolicyConditions, PolicyDecision, Session,
    SessionState,
};
use uuid::Uuid;

use common::{TestCore, git_repo};

struct Fx {
    core: TestCore,
    client: CoreClient,
    repo: PathBuf,
    project_id: Uuid,
    workspace_id: Uuid,
    ws: PathBuf,
    /// A directory outside the workspace and the repository.
    outside: PathBuf,
}

async fn fx_with(configure: impl FnOnce(&mut switchyard_core::CoreConfig)) -> Fx {
    let core = TestCore::start_with(configure).await;
    let (client, _rx) = core.connect().await;
    let repo = git_repo(core.dir.path(), "adv-repo");
    let outside = core.dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "not for agents\n").unwrap();
    let project = client
        .call::<ProjectsAdd>(ProjectsAddParams { path: repo.display().to_string() })
        .await
        .unwrap()
        .project;
    let w = client
        .call::<WorkspacesCreate>(WorkspacesCreateParams {
            project_id: project.id,
            name: None,
            base_ref: None,
        })
        .await
        .unwrap()
        .workspace;
    Fx { ws: PathBuf::from(&w.path), core, client, repo, project_id: project.id, workspace_id: w.id, outside }
}

async fn fx() -> Fx {
    fx_with(|_| {}).await
}

async fn start(f: &Fx) -> Session {
    f.client
        .call::<SessionsStart>(SessionsStartParams {
            workspace_id: f.workspace_id,
            project_id: None,
            provider: Some("codex".into()),
            prompt: None,
            model: None,
        })
        .await
        .unwrap()
        .session
}

async fn send(f: &Fx, id: Uuid, text: &str) {
    f.client
        .call::<SessionsSend>(SessionsSendParams { session_id: id, text: text.into(), queue_if_busy: false })
        .await
        .unwrap();
}

async fn session(f: &Fx, id: Uuid) -> Session {
    f.client.call::<SessionsGet>(SessionIdParams { session_id: id }).await.unwrap().session
}

async fn events(f: &Fx, id: Uuid) -> Vec<EventEnvelope> {
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

fn count(evs: &[EventEnvelope], t: &str) -> usize {
    evs.iter().filter(|e| e.event_type == t).count()
}

async fn until<T>(what: &str, mut f: impl AsyncFnMut() -> Option<T>) -> T {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(v) = f().await {
            return v;
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

async fn turns(f: &Fx, id: Uuid, n: usize) -> Vec<EventEnvelope> {
    until("turn", async || {
        let e = events(f, id).await;
        (count(&e, "agent.turn_completed") >= n).then_some(e)
    })
    .await
}

async fn state(f: &Fx, id: Uuid, s: SessionState) -> Session {
    until("state", async || {
        let x = session(f, id).await;
        (x.state == s).then_some(x)
    })
    .await
}

async fn approvals(f: &Fx, id: Uuid) -> Vec<Approval> {
    f.client
        .call::<ApprovalsList>(ApprovalsListParams { session_id: Some(id), ..Default::default() })
        .await
        .unwrap()
        .approvals
}

async fn pending(f: &Fx, id: Uuid) -> Approval {
    until("pending approval", async || {
        approvals(f, id).await.into_iter().find(|a| a.status == ApprovalStatus::Pending)
    })
    .await
}

async fn decide(f: &Fx, a: Uuid, d: ApprovalDecision) -> Result<Approval, switchyard_client::ClientError> {
    f.client
        .call::<ApprovalsDecide>(ApprovalsDecideParams { approval_id: a, decision: d, note: None })
        .await
        .map(|r| r.approval)
}

fn answers(thread: &str) -> Vec<Value> {
    let p = std::env::temp_dir().join("switchyard-fake-codex").join(format!("answers-{thread}.jsonl"));
    std::fs::read_to_string(p).unwrap_or_default().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

async fn answers_n(thread: &str, n: usize) -> Vec<Value> {
    until("answers", async || {
        let a = answers(thread);
        (a.len() >= n).then_some(a)
    })
    .await
}

async fn allow_rule(f: &Fx, category: OperationCategory, path_prefix: Option<&str>, description: &str) {
    f.client
        .call::<PoliciesCreate>(PoliciesCreateParams {
            project_id: f.project_id,
            workspace_id: None,
            category,
            decision: PolicyDecision::Allow,
            conditions: PolicyConditions { path_prefix: path_prefix.map(Into::into), command: None },
            description: description.into(),
            enabled: None,
        })
        .await
        .unwrap();
}

async fn evaluations(f: &Fx, s: Uuid) -> Vec<switchyard_protocol::model::PolicyEvaluation> {
    f.client
        .call::<PoliciesEvaluations>(PoliciesEvaluationsParams { session_id: Some(s), ..Default::default() })
        .await
        .unwrap()
        .evaluations
}

fn tree(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

// --- filesystem scope -------------------------------------------------------

#[tokio::test]
async fn a_patch_outside_the_workspace_is_never_automatic_shows_its_scope_and_denial_writes_nothing() {
    let f = fx().await;
    allow_rule(&f, OperationCategory::FileWrite, None, "Allow new files").await;
    let s = start(&f).await;
    let thread = s.native_session_id.clone().unwrap();
    let target = f.outside.join("planted.txt");
    send(&f, s.id, &format!("[fake:write:{}]", target.display())).await;
    let a = pending(&f, s.id).await;
    let scope = a.policy.as_ref().unwrap().scope.clone().unwrap();
    assert_eq!(scope.kind, "outside_workspace", "an allow rule cannot cover a path outside: {scope:?}");
    assert_eq!(scope.outside_paths, vec![target.display().to_string()]);
    decide(&f, a.id, ApprovalDecision::Deny).await.unwrap();
    assert_eq!(answers_n(&thread, 1).await[0]["answer"]["result"]["decision"], "decline");
    turns(&f, s.id, 1).await;
    assert!(!target.exists());
    assert_eq!(tree(&f.outside), ["secret.txt"]);
}

#[tokio::test]
async fn a_symlink_out_of_the_workspace_is_resolved_and_never_automatic() {
    let f = fx().await;
    std::os::unix::fs::symlink(&f.outside, f.ws.join("link")).unwrap();
    allow_rule(&f, OperationCategory::FileWrite, Some("link"), "Allow writes under link/").await;
    let s = start(&f).await;
    let thread = s.native_session_id.clone().unwrap();
    send(&f, s.id, "[fake:write:link/evil.txt]").await;
    let a = pending(&f, s.id).await;
    let scope = a.policy.as_ref().unwrap().scope.clone().unwrap();
    assert_eq!(scope.kind, "outside_workspace", "{scope:?}");
    decide(&f, a.id, ApprovalDecision::Deny).await.unwrap();
    answers_n(&thread, 1).await;
    turns(&f, s.id, 1).await;
    assert_eq!(tree(&f.outside), ["secret.txt"], "nothing written through the link");
}

#[tokio::test]
async fn git_metadata_is_never_changed_automatically() {
    let f = fx().await;
    allow_rule(&f, OperationCategory::FileWrite, None, "Allow new files").await;
    let s = start(&f).await;
    send(&f, s.id, "[fake:write:.git/evil]").await;
    // The allow rule matches, but the system cap turns it into a question.
    let a = pending(&f, s.id).await;
    // (In a worktree `.git` is a file, so `.git/evil` cannot be resolved; that
    // cap applies before the Git-metadata one.)
    let explanation = a.policy.as_ref().unwrap().explanation.clone();
    assert!(explanation.contains("allows this. Approval is still required"), "{explanation}");
    assert_ne!(a.policy.as_ref().unwrap().scope.as_ref().unwrap().kind, "workspace");
    decide(&f, a.id, ApprovalDecision::Deny).await.unwrap();
    turns(&f, s.id, 1).await;
    // A worktree's .git is a file pointing into the repository; neither changed.
    assert!(f.ws.join(".git").is_file());
    assert!(!f.repo.join(".git/evil").exists());
}

#[tokio::test]
async fn a_file_move_is_denied_without_asking_and_changes_nothing() {
    let f = fx().await;
    let s = start(&f).await;
    let thread = s.native_session_id.clone().unwrap();
    let before = std::fs::read_to_string(f.ws.join("README.md")).unwrap();
    send(&f, s.id, "[fake:move]").await;
    assert_eq!(answers_n(&thread, 1).await[0]["answer"]["result"]["decision"], "decline");
    turns(&f, s.id, 1).await;
    assert!(approvals(&f, s.id).await.is_empty(), "no person was asked");
    assert_eq!(std::fs::read_to_string(f.ws.join("README.md")).unwrap(), before);
    assert!(!f.ws.join("moved.txt").exists());
    let e = evaluations(&f, s.id).await;
    assert_eq!(e[0].decision, PolicyDecision::Deny);
}

#[tokio::test]
async fn approval_scopes_say_what_is_reached() {
    let f = fx().await;
    let s = start(&f).await;
    send(&f, s.id, "[fake:write:inside.txt]").await;
    let a = pending(&f, s.id).await;
    assert_eq!(a.policy.as_ref().unwrap().scope.as_ref().unwrap().kind, "workspace");
    decide(&f, a.id, ApprovalDecision::Deny).await.unwrap();
    turns(&f, s.id, 1).await;
    send(&f, s.id, "[fake:cmd:touch x]").await;
    let a = pending(&f, s.id).await;
    let scope = a.policy.as_ref().unwrap().scope.clone().unwrap();
    assert_eq!(scope.kind, "unrestricted");
    assert!(scope.note.contains("any file you can") && scope.note.contains("Codex"), "{}", scope.note);
    decide(&f, a.id, ApprovalDecision::Deny).await.unwrap();
}

// --- protocol robustness ----------------------------------------------------

#[tokio::test]
async fn an_unknown_request_method_is_refused_and_audited() {
    let f = fx().await;
    let s = start(&f).await;
    let thread = s.native_session_id.clone().unwrap();
    send(&f, s.id, "[fake:newreq]").await;
    let a = answers_n(&thread, 1).await;
    assert!(
        a[0]["answer"].get("error").is_some(),
        "an unknown request gets an error, never a result: {}",
        a[0]
    );
    turns(&f, s.id, 1).await;
    assert!(approvals(&f, s.id).await.is_empty());
    let e = evaluations(&f, s.id).await;
    assert_eq!(
        (e.len(), e[0].decision, e[0].category),
        (1, PolicyDecision::Deny, OperationCategory::Unknown)
    );
}

#[tokio::test]
async fn malformed_truncated_and_unknown_output_causes_no_action() {
    let f = fx().await;
    let s = start(&f).await;
    let thread = s.native_session_id.clone().unwrap();
    send(&f, s.id, "[fake:garbage]").await;
    let evs = turns(&f, s.id, 1).await;
    assert!(evs.iter().any(|e| e.event_type == "agent.message" && e.payload["text"] == "Still here."));
    assert!(count(&evs, "agent.warning") >= 2, "malformed lines and unknown items are reported");
    assert!(approvals(&f, s.id).await.is_empty(), "a truncated request is never treated as a request");
    assert!(answers(&thread).is_empty(), "nothing was answered");
    assert_eq!(session(&f, s.id).await.state, SessionState::Idle);
}

#[tokio::test]
async fn a_reused_request_id_stops_the_session_without_answering() {
    let f = fx().await;
    let s = start(&f).await;
    let thread = s.native_session_id.clone().unwrap();
    send(&f, s.id, "[fake:dupid]").await;
    let after = state(&f, s.id, SessionState::Failed).await;
    assert!(after.failure.as_deref().is_some_and(|m| m.contains("reused")), "{:?}", after.failure);
    assert!(answers(&thread).is_empty());
    assert!(!f.ws.join("dup.txt").exists());
    for a in approvals(&f, s.id).await {
        assert_ne!(a.status, ApprovalStatus::Approved);
    }
}

#[tokio::test]
async fn a_sub_agent_is_interrupted_and_its_request_denied() {
    let f = fx().await;
    let s = start(&f).await;
    let thread = s.native_session_id.clone().unwrap();
    send(&f, s.id, "[fake:subagent]").await;
    let a = answers_n(&thread, 2).await;
    assert!(a.iter().any(|x| x.get("child_interrupt").is_some()), "the child's turn was interrupted: {a:?}");
    assert!(a.iter().any(|x| x["answer"]["result"]["decision"] == "decline"));
    let evs = turns(&f, s.id, 1).await;
    assert!(evs.iter().any(|e| e.event_type == "agent.warning"
        && e.payload["message"].as_str().unwrap().contains("does not support sub-agents")));
    assert!(!f.ws.join("from-subagent.txt").exists());
    assert!(approvals(&f, s.id).await.is_empty(), "never offered to a person");
}

// --- approval lifecycle -------------------------------------------------------

#[tokio::test]
async fn an_expired_approval_is_declined_and_cannot_be_approved_later() {
    let f = fx_with(|c| c.approval_timeout = Duration::from_secs(1)).await;
    let s = start(&f).await;
    let thread = s.native_session_id.clone().unwrap();
    send(&f, s.id, "[fake:cmd:touch expired.txt]").await;
    let a = pending(&f, s.id).await;
    assert_eq!(answers_n(&thread, 1).await[0]["answer"]["result"]["decision"], "decline");
    let settled = until("expiry", async || {
        approvals(&f, s.id).await.into_iter().find(|x| x.id == a.id && x.status == ApprovalStatus::Expired)
    })
    .await;
    assert_eq!(settled.decision_source, Some(DecisionSource::Timeout));
    assert!(decide(&f, a.id, ApprovalDecision::Approve).await.is_err());
    turns(&f, s.id, 1).await;
    assert!(!f.ws.join("expired.txt").exists());
    assert_eq!(answers(&thread).len(), 1);
}

#[tokio::test]
async fn concurrent_decisions_produce_exactly_one_answer() {
    let f = fx().await;
    let s = start(&f).await;
    let thread = s.native_session_id.clone().unwrap();
    send(&f, s.id, "[fake:cmd:touch once.txt]").await;
    let a = pending(&f, s.id).await;
    let (x, y) =
        tokio::join!(decide(&f, a.id, ApprovalDecision::Approve), decide(&f, a.id, ApprovalDecision::Deny));
    assert_eq!([x.is_ok(), y.is_ok()].iter().filter(|ok| **ok).count(), 1, "exactly one decision wins");
    turns(&f, s.id, 1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(answers(&thread).len(), 1, "exactly one answer reached Codex");
    let won = if x.is_ok() { "accept" } else { "decline" };
    assert_eq!(answers(&thread)[0]["answer"]["result"]["decision"], won);
    assert_eq!(f.ws.join("once.txt").exists(), won == "accept");
}

#[tokio::test]
async fn a_crash_with_a_pending_approval_cancels_it_and_a_resumed_process_cannot_receive_it() {
    let f = fx().await;
    let s = start(&f).await;
    let thread = s.native_session_id.clone().unwrap();
    send(&f, s.id, "[fake:crashpending]").await;
    let a = pending(&f, s.id).await;
    let failed = state(&f, s.id, SessionState::Failed).await;
    assert_eq!(failed.exit_code, Some(3));
    let settled = approvals(&f, s.id).await.into_iter().find(|x| x.id == a.id).unwrap();
    assert_eq!(
        (settled.status, settled.decision_source),
        (ApprovalStatus::Cancelled, Some(DecisionSource::AgentExited))
    );
    // Resume: the new process numbers its requests from 0 again.
    f.client.call::<SessionsResume>(SessionsResumeParams { session_id: s.id, prompt: None }).await.unwrap();
    send(&f, s.id, "[fake:cmd:touch after-resume.txt]").await;
    let b = until("new approval", async || {
        approvals(&f, s.id).await.into_iter().find(|x| x.status == ApprovalStatus::Pending && x.id != a.id)
    })
    .await;
    assert_ne!(a.tool_use_id, b.tool_use_id);
    assert!(decide(&f, a.id, ApprovalDecision::Approve).await.is_err(), "the old approval is settled");
    assert!(answers(&thread).is_empty(), "nothing reached the resumed process from the old approval");
    decide(&f, b.id, ApprovalDecision::Deny).await.unwrap();
    turns(&f, s.id, 1).await;
    assert!(!f.ws.join("crashed.txt").exists() && !f.ws.join("after-resume.txt").exists());
}

#[tokio::test]
async fn a_database_failure_declines_instead_of_allowing() {
    let f = fx().await;
    allow_rule(&f, OperationCategory::FileWrite, None, "Allow new files").await;
    let s = start(&f).await;
    let thread = s.native_session_id.clone().unwrap();
    let c = rusqlite::Connection::open(common::config_for(f.core.dir.path()).paths.database).unwrap();
    c.busy_timeout(Duration::from_secs(5)).unwrap();
    c.execute_batch(
        "CREATE TRIGGER test_break BEFORE INSERT ON policy_evaluations BEGIN SELECT RAISE(ABORT, 'injected'); END;",
    )
    .unwrap();
    send(&f, s.id, "[fake:write:would-be-allowed.txt]").await;
    assert_eq!(answers_n(&thread, 1).await[0]["answer"]["result"]["decision"], "decline");
    turns(&f, s.id, 1).await;
    assert!(!f.ws.join("would-be-allowed.txt").exists());
    c.execute_batch("DROP TRIGGER test_break;").unwrap();
}

// --- versions -------------------------------------------------------------------

/// A Codex that reports another version (a wrapper around the fake, inside the
/// test's own directory).
fn other_version(dir: &Path, version: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join("codex-other");
    std::fs::write(
        &p,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'codex-cli {version}'; exit 0; fi\nexec '{}' \"$@\"\n",
            common::FAKE_CODEX
        ),
    )
    .unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

#[tokio::test]
async fn an_untested_codex_version_is_refused_unless_explicitly_allowed() {
    for version in ["0.167.0", "0.165.9"] {
        let mut core = TestCore::start().await;
        // The wrapper lives in this test's own directory (allowed by the guard).
        let local = other_version(core.dir.path(), version);
        core.restart_with(|c| c.codex_executable = Some(local.clone())).await;
        let (client, _rx) = core.connect().await;
        let codex = client
            .call::<AgentsList>(AgentsListParams { refresh: true })
            .await
            .unwrap()
            .agents
            .pop()
            .unwrap();
        assert_eq!(codex.version.as_deref(), Some(version));
        let err = codex.error.clone().unwrap_or_default();
        assert!(err.contains("untested") && err.contains("0.166.0"), "{err}");

        let repo = git_repo(core.dir.path(), "v-repo");
        let p = client
            .call::<ProjectsAdd>(ProjectsAddParams { path: repo.display().to_string() })
            .await
            .unwrap()
            .project;
        let w = client
            .call::<WorkspacesCreate>(WorkspacesCreateParams { project_id: p.id, name: None, base_ref: None })
            .await
            .unwrap()
            .workspace;
        let params = SessionsStartParams {
            workspace_id: w.id,
            project_id: None,
            provider: Some("codex".into()),
            prompt: None,
            model: None,
        };
        let refused = client.call::<SessionsStart>(params.clone()).await;
        assert!(refused.is_err(), "an untested version is not started by default");

        // Explicit opt-in: allowed, and the journal says so.
        core.restart_with(|c| {
            c.codex_executable = Some(local.clone());
            c.allow_untested_codex = true;
        })
        .await;
        let (client, _rx) = core.connect().await;
        let s = client.call::<SessionsStart>(params).await.unwrap().session;
        let evs = client
            .call::<EventsList>(EventsListParams {
                session_id: Some(s.id),
                after_seq: Some(0),
                limit: Some(100),
                ..Default::default()
            })
            .await
            .unwrap()
            .events;
        assert!(evs.iter().any(|e| e.event_type == "agent.warning"
            && e.payload["message"].as_str().unwrap().contains(&format!("untested Codex {version}"))));
    }
}
