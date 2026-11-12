#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Opt-in end-to-end tests against the locally installed Codex CLI, through
//! its real app-server. They make real model requests with your existing
//! Codex login, so they never run by default:
//!
//! ```sh
//! SWITCHYARD_REAL_CODEX=1 cargo test -p switchyard-core --test real_codex -- --ignored --nocapture
//! ```
//!
//! Every test works in throwaway repositories under a temporary directory.
//! Approvals are decided by the test as a person would: by category, never
//! with a blanket allow.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use switchyard_client::CoreClient;
use switchyard_protocol::EventEnvelope;
use switchyard_protocol::methods::*;
use switchyard_protocol::model::{
    Approval, ApprovalStatus, OperationCategory, PolicyConditions, PolicyDecision, Session, SessionState,
};
use uuid::Uuid;

use common::{TestCore, git_repo};

fn enabled() -> bool {
    if std::env::var("SWITCHYARD_REAL_CODEX").as_deref() == Ok("1") {
        return true;
    }
    eprintln!("skipped: set SWITCHYARD_REAL_CODEX=1");
    false
}

async fn real_core() -> TestCore {
    TestCore::start_with(|c| {
        c.codex_executable = None;
        c.agent_ready_timeout = Duration::from_secs(90);
    })
    .await
}

async fn events(c: &CoreClient, s: Uuid) -> Vec<EventEnvelope> {
    c.call::<EventsList>(EventsListParams {
        session_id: Some(s),
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

/// Decides every approval of the running turn with `decide` until `turns`
/// turns have completed. Returns the approvals it decided.
async fn drive(
    c: &CoreClient,
    s: Uuid,
    turns: usize,
    decide: impl Fn(&Approval) -> ApprovalDecision,
) -> (Vec<EventEnvelope>, Vec<Approval>) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
    let mut decided = Vec::new();
    loop {
        let pending = c
            .call::<ApprovalsList>(ApprovalsListParams {
                status: Some(ApprovalStatus::Pending),
                session_id: Some(s),
                ..Default::default()
            })
            .await
            .unwrap()
            .approvals;
        for a in pending {
            let d = decide(&a);
            println!(
                "approval {} {:?}: {} -> {d:?}",
                a.tool_name,
                a.policy.as_ref().map(|p| p.category),
                a.input
            );
            if let Ok(r) = c
                .call::<ApprovalsDecide>(ApprovalsDecideParams { approval_id: a.id, decision: d, note: None })
                .await
            {
                decided.push(r.approval);
            }
        }
        let evs = events(c, s).await;
        if count(&evs, "agent.turn_completed") >= turns {
            return (evs, decided);
        }
        assert!(tokio::time::Instant::now() < deadline, "turn {turns} did not complete");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn send(c: &CoreClient, s: Uuid, text: &str) {
    c.call::<SessionsSend>(SessionsSendParams { session_id: s, text: text.into(), queue_if_busy: false })
        .await
        .unwrap();
}

fn last_message(evs: &[EventEnvelope]) -> String {
    evs.iter()
        .rev()
        .find(|e| e.event_type == "agent.message")
        .map(|e| e.payload["text"].as_str().unwrap().to_owned())
        .unwrap_or_default()
}

/// A repository with a committed `notes.txt`, registered as a project.
async fn project_with_notes(c: &CoreClient, dir: &Path, name: &str) -> (PathBuf, Uuid) {
    let repo = git_repo(dir, name);
    std::fs::write(repo.join("notes.txt"), "The secret word is sapphire.\n").unwrap();
    for args in [
        vec!["add", "notes.txt"],
        vec!["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "notes"],
    ] {
        assert!(std::process::Command::new("git").args(&args).current_dir(&repo).status().unwrap().success());
    }
    let project =
        c.call::<ProjectsAdd>(ProjectsAddParams { path: repo.display().to_string() }).await.unwrap().project;
    (repo, project.id)
}

async fn workspace(c: &CoreClient, project_id: Uuid) -> (Uuid, PathBuf) {
    let w = c
        .call::<WorkspacesCreate>(WorkspacesCreateParams { project_id, name: None, base_ref: None })
        .await
        .unwrap()
        .workspace;
    (w.id, PathBuf::from(w.path))
}

async fn start_codex(c: &CoreClient, workspace_id: Uuid) -> Session {
    c.call::<SessionsStart>(SessionsStartParams {
        workspace_id,
        project_id: None,
        provider: Some("codex".into()),
        prompt: None,
        model: None,
    })
    .await
    .unwrap()
    .session
}

fn category(a: &Approval) -> Option<OperationCategory> {
    a.policy.as_ref().map(|p| p.category)
}

#[tokio::test]
#[ignore = "uses the real Codex CLI and makes model requests; set SWITCHYARD_REAL_CODEX=1"]
async fn real_codex_session_end_to_end() {
    if !enabled() {
        return;
    }
    let core = real_core().await;
    let (c, _rx) = core.connect().await;
    let agents = c.call::<AgentsList>(AgentsListParams { refresh: true }).await.unwrap().agents;
    let codex = agents.iter().find(|a| a.provider == "codex").unwrap();
    println!("detected: {} {:?} auth {:?}", codex.display_name, codex.version, codex.auth);
    assert!(codex.error.is_none(), "Codex not usable: {:?}", codex.error);

    let (repo, project_id) = project_with_notes(&c, core.dir.path(), "real-codex-repo").await;
    let (ws_id, ws) = workspace(&c, project_id).await;
    let s = start_codex(&c, ws_id).await;
    assert_eq!(s.state, SessionState::Idle);
    assert_eq!(s.cwd, ws.display().to_string());
    let thread = s.native_session_id.clone().expect("Codex thread id");
    println!("session {} thread {thread} pid {:?}", s.id, s.pid);

    // 1. A read is a shell command: it asks; approve reads only.
    send(&c, s.id, "Read the file notes.txt and tell me the secret word in one short sentence.").await;
    let (evs, decided) = drive(&c, s.id, 1, |a| {
        if category(a) == Some(OperationCategory::Shell)
            && a.input["command"].as_str().is_some_and(|x| x.contains("notes.txt"))
        {
            ApprovalDecision::Approve
        } else {
            ApprovalDecision::Deny
        }
    })
    .await;
    assert!(decided.iter().any(|a| a.status == ApprovalStatus::Approved), "the read went through Switchyard");
    assert!(last_message(&evs).to_lowercase().contains("sapphire"), "{}", last_message(&evs));
    let done = evs.iter().rev().find(|e| e.event_type == "agent.turn_completed").unwrap();
    assert!(done.payload["usage"]["total_tokens"].as_u64().unwrap_or(0) > 0, "{}", done.payload);
    assert!(done.payload["cost_usd"].is_null());

    // 2. Approved patch → file in the workspace only.
    send(&c, s.id, "Create a new file hello.txt containing exactly the line: hi from codex. Use apply_patch, not the shell.").await;
    let (_, decided) = drive(&c, s.id, 2, |a| match category(a) {
        Some(OperationCategory::FileWrite) => ApprovalDecision::Approve,
        _ => ApprovalDecision::Deny,
    })
    .await;
    assert!(decided.iter().any(|a| a.tool_name == "apply_patch" && a.status == ApprovalStatus::Approved));
    // The model may keep the prompt's trailing period.
    assert_eq!(
        std::fs::read_to_string(ws.join("hello.txt")).unwrap().trim().trim_end_matches('.'),
        "hi from codex"
    );
    assert!(!repo.join("hello.txt").exists(), "the project checkout is untouched");

    // 3. Denied command never runs.
    send(&c, s.id, "Run exactly this shell command: touch denied-by-switchyard.txt").await;
    let (evs, decided) = drive(&c, s.id, 3, |_| ApprovalDecision::Deny).await;
    assert!(decided.iter().any(|a| category(a) == Some(OperationCategory::Shell)));
    assert!(!ws.join("denied-by-switchyard.txt").exists());
    assert!(evs.iter().any(|e| e.event_type == "agent.tool_failed" && e.payload["rejected"] == true));

    // 4. Interrupt a long turn.
    send(&c, s.id, "Without using any tools, write a 2000-word essay about railway signal boxes.").await;
    tokio::time::sleep(Duration::from_secs(4)).await;
    c.call::<SessionsInterrupt>(SessionIdParams { session_id: s.id }).await.unwrap();
    let (evs, _) = drive(&c, s.id, 4, |_| ApprovalDecision::Deny).await;
    let last = evs.iter().rev().find(|e| e.event_type == "agent.turn_completed").unwrap();
    assert_eq!(last.payload["interrupted"], true, "{}", last.payload);
    assert_eq!(count(&evs, "agent.interrupted"), 1);

    // 5. Stop, resume in a new process, and continue the same conversation.
    let stopped = c.call::<SessionsStop>(SessionIdParams { session_id: s.id }).await.unwrap().session;
    assert_eq!(stopped.state, SessionState::Stopped);
    let old_pid = s.pid.unwrap();
    assert!(
        !std::process::Command::new("kill").args(["-0", &old_pid.to_string()]).status().unwrap().success()
    );
    let r = c
        .call::<SessionsResume>(SessionsResumeParams { session_id: s.id, prompt: None })
        .await
        .unwrap()
        .session;
    assert_eq!(r.state, SessionState::Idle);
    assert_eq!(r.native_session_id.as_deref(), Some(thread.as_str()));
    assert_ne!(r.pid, Some(old_pid));
    send(&c, s.id, "Without using any tools: what was the secret word you read earlier? One word.").await;
    let (evs, _) = drive(&c, s.id, 5, |_| ApprovalDecision::Deny).await;
    assert!(last_message(&evs).to_lowercase().contains("sapphire"), "{}", last_message(&evs));
    assert_eq!(count(&evs, "agent.ready"), 2, "one thread opened, one resumed");

    // Every approval has its durable evaluation.
    let evals = c
        .call::<PoliciesEvaluations>(PoliciesEvaluationsParams {
            session_id: Some(s.id),
            ..Default::default()
        })
        .await
        .unwrap()
        .evaluations;
    assert!(evals.iter().all(|e| e.adapter == "codex"));
    c.call::<SessionsStop>(SessionIdParams { session_id: s.id }).await.unwrap();
}

#[tokio::test]
#[ignore = "uses the real Codex CLI and makes model requests; set SWITCHYARD_REAL_CODEX=1"]
async fn real_codex_two_workspaces_concurrently() {
    if !enabled() {
        return;
    }
    let core = real_core().await;
    let (c, _rx) = core.connect().await;
    let (repo, project_id) = project_with_notes(&c, core.dir.path(), "real-codex-two").await;
    let (w1, p1) = workspace(&c, project_id).await;
    let (w2, p2) = workspace(&c, project_id).await;
    let s1 = start_codex(&c, w1).await;
    let s2 = start_codex(&c, w2).await;
    send(&c, s1.id, "Create a new file one.txt containing exactly: one. Use apply_patch, not the shell.")
        .await;
    send(&c, s2.id, "Create a new file two.txt containing exactly: two. Use apply_patch, not the shell.")
        .await;
    let only_writes = |a: &Approval| match category(a) {
        Some(OperationCategory::FileWrite) => ApprovalDecision::Approve,
        _ => ApprovalDecision::Deny,
    };
    let (e1, d2) = tokio::join!(drive(&c, s1.id, 1, only_writes), drive(&c, s2.id, 1, only_writes));
    let _ = d2;
    assert!(p1.join("one.txt").exists() && !p1.join("two.txt").exists());
    assert!(p2.join("two.txt").exists() && !p2.join("one.txt").exists());
    assert!(!repo.join("one.txt").exists() && !repo.join("two.txt").exists());
    assert!(e1.0.iter().all(|e| e.context.workspace_id == Some(w1)));
    let processes: Vec<_> = [s1.pid, s2.pid].into_iter().flatten().collect();
    assert_eq!(processes.len(), 2);
    assert_ne!(processes[0], processes[1]);
    for s in [s1.id, s2.id] {
        c.call::<SessionsStop>(SessionIdParams { session_id: s }).await.unwrap();
    }
}

#[tokio::test]
#[ignore = "uses the real Codex CLI and makes model requests; set SWITCHYARD_REAL_CODEX=1"]
async fn real_codex_policy_blocks_without_asking() {
    if !enabled() {
        return;
    }
    let core = real_core().await;
    let (c, _rx) = core.connect().await;
    let (_repo, project_id) = project_with_notes(&c, core.dir.path(), "real-codex-policy").await;
    let (ws_id, ws) = workspace(&c, project_id).await;
    c.call::<PoliciesCreate>(PoliciesCreateParams {
        project_id,
        workspace_id: Some(ws_id),
        category: OperationCategory::FileWrite,
        decision: PolicyDecision::Deny,
        conditions: PolicyConditions::default(),
        description: "No new files".into(),
        enabled: None,
    })
    .await
    .unwrap();
    let s = start_codex(&c, ws_id).await;
    send(&c, s.id, "Create a new file blocked.txt containing exactly: no. Use apply_patch only; do not use the shell. If it is refused, say so and stop.").await;
    let (_, decided) = drive(&c, s.id, 1, |_| ApprovalDecision::Deny).await;
    assert!(!ws.join("blocked.txt").exists());
    assert!(decided.iter().all(|a| a.tool_name != "apply_patch"), "the rule decided; no person was asked");
    let evals = c
        .call::<PoliciesEvaluations>(PoliciesEvaluationsParams {
            session_id: Some(s.id),
            ..Default::default()
        })
        .await
        .unwrap()
        .evaluations;
    let blocked =
        evals.iter().find(|e| e.category == OperationCategory::FileWrite).expect("the patch was evaluated");
    assert_eq!(blocked.decision, PolicyDecision::Deny);
    assert!(blocked.deciding_rule.is_some());
    c.call::<SessionsStop>(SessionIdParams { session_id: s.id }).await.unwrap();
}
