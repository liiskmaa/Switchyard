#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Opt-in end-to-end test against the locally installed Claude Code CLI.
//!
//! Makes real (small) model requests with your existing Claude login, so it
//! never runs by default:
//!
//! ```sh
//! SWITCHYARD_REAL_CLAUDE=1 cargo test -p switchyard-core --test real_claude -- --ignored --nocapture
//! ```
//!
//! `SWITCHYARD_REAL_CLAUDE_MODEL` selects the model (default `haiku`).

mod common;

use std::time::Duration;

use switchyard_client::Notification;
use switchyard_protocol::EventEnvelope;
use switchyard_protocol::methods::*;
use switchyard_protocol::model::{ApprovalStatus, LiveKind, SessionState};

use common::{RealCli, TestCore, git_repo};

async fn events(c: &switchyard_client::CoreClient, s: uuid::Uuid) -> Vec<EventEnvelope> {
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

async fn wait_turns(c: &switchyard_client::CoreClient, s: uuid::Uuid, n: usize) -> Vec<EventEnvelope> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    loop {
        let evs = events(c, s).await;
        if evs.iter().filter(|e| e.event_type == "agent.turn_completed").count() >= n {
            return evs;
        }
        assert!(tokio::time::Instant::now() < deadline, "turn {n} did not complete");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn approve_or_deny_next(
    c: &switchyard_client::CoreClient,
    s: uuid::Uuid,
    decision: ApprovalDecision,
) -> switchyard_protocol::model::Approval {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
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
        if let Some(a) = pending.into_iter().next() {
            println!("approval request: {} {}", a.tool_name, a.input);
            return c
                .call::<ApprovalsDecide>(ApprovalsDecideParams { approval_id: a.id, decision, note: None })
                .await
                .unwrap()
                .approval;
        }
        assert!(tokio::time::Instant::now() < deadline, "no permission request arrived");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test]
#[ignore = "uses the real Claude Code CLI and makes model requests; set SWITCHYARD_REAL_CLAUDE=1"]
async fn real_claude_session_end_to_end() {
    if std::env::var("SWITCHYARD_REAL_CLAUDE").as_deref() != Ok("1") {
        eprintln!("skipped: set SWITCHYARD_REAL_CLAUDE=1");
        return;
    }
    let model = std::env::var("SWITCHYARD_REAL_CLAUDE_MODEL").unwrap_or_else(|_| "haiku".into());
    let Some(core) = TestCore::start_real(RealCli { claude: true, codex: false }, |_| {}).await else {
        return;
    };
    let (client, mut rx) = core.connect().await;

    let agents = client.call::<AgentsList>(AgentsListParams { refresh: true }).await.unwrap().agents;
    println!("detected: {:?}", agents[0]);
    assert!(agents[0].error.is_none(), "Claude Code not usable: {:?}", agents[0].error);

    let repo = git_repo(core.dir.path(), "real-claude-repo");
    let project = client
        .call::<ProjectsAdd>(ProjectsAddParams { path: repo.display().to_string() })
        .await
        .unwrap()
        .project;
    let workspace = client
        .call::<WorkspacesCreate>(WorkspacesCreateParams {
            project_id: project.id,
            name: None,
            base_ref: None,
        })
        .await
        .unwrap()
        .workspace;
    let ws = std::path::PathBuf::from(&workspace.path);
    let session = client
        .call::<SessionsStart>(SessionsStartParams {
            workspace_id: workspace.id,
            project_id: Some(project.id),
            provider: None,
            prompt: None,
            model: Some(model),
        })
        .await
        .unwrap()
        .session;
    assert_eq!(session.cwd, workspace.path);
    println!("session {} native {:?} pid {:?}", session.id, session.native_session_id, session.pid);
    client.call::<LiveSubscribe>(LiveSubscribeParams { session_id: Some(session.id) }).await.unwrap();

    // 1. Approved write → real file.
    client
        .call::<SessionsSend>(SessionsSendParams {
            session_id: session.id,
            text: "Create a file named switchyard.txt containing exactly the word approved. Use the Write tool. Then reply DONE.".into(),
            queue_if_busy: false,
        })
        .await
        .unwrap();
    let a = approve_or_deny_next(&client, session.id, ApprovalDecision::Approve).await;
    assert_eq!(a.status, ApprovalStatus::Approved);
    let evs = wait_turns(&client, session.id, 1).await;
    assert_eq!(std::fs::read_to_string(ws.join("switchyard.txt")).unwrap().trim(), "approved");
    assert!(!repo.join("switchyard.txt").exists(), "the project checkout is untouched");
    let ready = evs.iter().find(|e| e.event_type == "agent.ready").unwrap();
    assert_eq!(ready.payload["native_session_id"].as_str(), session.native_session_id.as_deref());
    let mut live_chunks = 0;
    while let Ok(Some(n)) = tokio::time::timeout(Duration::from_millis(50), rx.recv()).await {
        if let Notification::Live { output, .. } = n
            && output.kind == LiveKind::Text
        {
            live_chunks += 1;
        }
    }
    println!("live text chunks received: {live_chunks}; durable events: {}", evs.len());
    assert!(live_chunks > 0);
    assert!(evs.iter().any(|e| e.event_type == "files.changed"
        && e.payload["changes"].as_array().unwrap().iter().any(|c| c["path"] == "switchyard.txt")));

    // 2. Denied shell command.
    client
        .call::<SessionsSend>(SessionsSendParams {
            session_id: session.id,
            text: "Run `touch denied.txt` with the Bash tool. If it is denied, reply DENIED and stop.".into(),
            queue_if_busy: false,
        })
        .await
        .unwrap();
    let d = approve_or_deny_next(&client, session.id, ApprovalDecision::Deny).await;
    assert_eq!((d.tool_name.as_str(), d.status), ("Bash", ApprovalStatus::Denied));
    let evs = wait_turns(&client, session.id, 2).await;
    assert!(!ws.join("denied.txt").exists() && !repo.join("denied.txt").exists());
    assert!(evs.iter().any(|e| e.event_type == "agent.tool_failed" && e.payload["rejected"] == true));

    // 3. Interrupt a long answer.
    client
        .call::<SessionsSend>(SessionsSendParams {
            session_id: session.id,
            text: "Write a 700-word essay about railway switchyards. Do not use tools.".into(),
            queue_if_busy: false,
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(4)).await;
    client.call::<SessionsInterrupt>(SessionIdParams { session_id: session.id }).await.unwrap();
    let evs = wait_turns(&client, session.id, 3).await;
    let last_turn = evs.iter().rev().find(|e| e.event_type == "agent.turn_completed").unwrap();
    assert_eq!(last_turn.payload["interrupted"], true, "{last_turn:?}");

    // 4. Stop, then resume the same native session in a new process.
    let stopped =
        client.call::<SessionsStop>(SessionIdParams { session_id: session.id }).await.unwrap().session;
    assert_eq!(stopped.state, SessionState::Stopped);
    let resumed = client
        .call::<SessionsResume>(SessionsResumeParams {
            session_id: session.id,
            prompt: Some(
                "Which file did you create earlier in this conversation? Reply with just the file name."
                    .into(),
            ),
        })
        .await
        .unwrap()
        .session;
    assert_eq!(resumed.native_session_id, session.native_session_id);
    let evs = wait_turns(&client, session.id, 4).await;
    let answer = evs.iter().rev().find(|e| e.event_type == "agent.message").unwrap();
    println!("resumed answer: {}", answer.payload["text"]);
    assert!(answer.payload["text"].as_str().unwrap().contains("switchyard.txt"));
    client.call::<SessionsStop>(SessionIdParams { session_id: session.id }).await.unwrap();
}

/// Two real Claude Code sessions on one project at the same time, each in
/// its own workspace.
#[tokio::test]
#[ignore = "uses the real Claude Code CLI and makes model requests; set SWITCHYARD_REAL_CLAUDE=1"]
async fn real_claude_two_workspaces_concurrently() {
    if std::env::var("SWITCHYARD_REAL_CLAUDE").as_deref() != Ok("1") {
        eprintln!("skipped: set SWITCHYARD_REAL_CLAUDE=1");
        return;
    }
    let model = std::env::var("SWITCHYARD_REAL_CLAUDE_MODEL").unwrap_or_else(|_| "haiku".into());
    let Some(core) = TestCore::start_real(RealCli { claude: true, codex: false }, |_| {}).await else {
        return;
    };
    let (client, _rx) = core.connect().await;
    let repo = git_repo(core.dir.path(), "real-claude-parallel");
    let project = client
        .call::<ProjectsAdd>(ProjectsAddParams { path: repo.display().to_string() })
        .await
        .unwrap()
        .project;
    let mut runs = Vec::new();
    for name in ["alpha", "beta"] {
        let w = client
            .call::<WorkspacesCreate>(WorkspacesCreateParams {
                project_id: project.id,
                name: Some(name.into()),
                base_ref: None,
            })
            .await
            .unwrap()
            .workspace;
        let s = client
            .call::<SessionsStart>(SessionsStartParams {
                workspace_id: w.id,
                project_id: None,
                provider: None,
                prompt: Some(format!(
                    "Create a file named {name}.txt containing exactly the word {name}. Use the Write tool. \
                     Then reply DONE."
                )),
                model: Some(model.clone()),
            })
            .await
            .unwrap()
            .session;
        runs.push((name, w, s));
    }
    for (_, _, s) in &runs {
        let a = approve_or_deny_next(&client, s.id, ApprovalDecision::Approve).await;
        assert_eq!(a.session_id, s.id);
    }
    for (name, w, s) in &runs {
        let evs = wait_turns(&client, s.id, 1).await;
        let dir = std::path::PathBuf::from(&w.path);
        assert_eq!(std::fs::read_to_string(dir.join(format!("{name}.txt"))).unwrap().trim(), *name);
        assert!(evs.iter().all(|e| e.context.workspace_id == Some(w.id)), "events attributed to {name}");
        println!("{name}: session {} in {}", s.id, w.path);
    }
    let (a, b) = (std::path::PathBuf::from(&runs[0].1.path), std::path::PathBuf::from(&runs[1].1.path));
    assert!(!a.join("beta.txt").exists() && !b.join("alpha.txt").exists());
    assert!(!repo.join("alpha.txt").exists() && !repo.join("beta.txt").exists());
    for (_, _, s) in &runs {
        client.call::<SessionsStop>(SessionIdParams { session_id: s.id }).await.unwrap();
    }
}

/// Milestone 4 against the real CLI:
/// - a Read inside the workspace reaches Switchyard (because of the `ask` rule for every tool) and
///   the built-in default allows it without an approval;
/// - a block rule denies a Read without an approval, and the agent receives the explanation;
/// - an exact-command rule lets that one Bash command run without asking.
#[tokio::test]
#[ignore = "uses the real Claude Code CLI and makes model requests; set SWITCHYARD_REAL_CLAUDE=1"]
async fn real_claude_policy_decisions() {
    if std::env::var("SWITCHYARD_REAL_CLAUDE").as_deref() != Ok("1") {
        eprintln!("skipped: set SWITCHYARD_REAL_CLAUDE=1");
        return;
    }
    use switchyard_protocol::model::{OperationCategory, PolicyConditions, PolicyDecision};
    let model = std::env::var("SWITCHYARD_REAL_CLAUDE_MODEL").unwrap_or_else(|_| "haiku".into());
    let Some(core) = TestCore::start_real(RealCli { claude: true, codex: false }, |_| {}).await else {
        return;
    };
    let (client, _rx) = core.connect().await;
    let repo = git_repo(core.dir.path(), "real-policy-repo");
    let project = client
        .call::<ProjectsAdd>(ProjectsAddParams { path: repo.display().to_string() })
        .await
        .unwrap()
        .project;
    let workspace = client
        .call::<WorkspacesCreate>(WorkspacesCreateParams {
            project_id: project.id,
            name: None,
            base_ref: None,
        })
        .await
        .unwrap()
        .workspace;
    let ws = std::path::PathBuf::from(&workspace.path);
    std::fs::write(ws.join("notes.txt"), "the codeword is sapphire\n").unwrap();
    std::fs::write(ws.join("secret.txt"), "the codeword is ruby\n").unwrap();
    let rule = |category, decision, path: Option<&str>, command: Option<&str>, description: &str| {
        PoliciesCreateParams {
            project_id: project.id,
            workspace_id: None,
            category,
            decision,
            conditions: PolicyConditions {
                path_prefix: path.map(Into::into),
                command: command.map(Into::into),
            },
            description: description.into(),
            enabled: None,
        }
    };
    client
        .call::<PoliciesCreate>(rule(
            OperationCategory::FileRead,
            PolicyDecision::Deny,
            Some("secret.txt"),
            None,
            "Secret stays secret",
        ))
        .await
        .unwrap();
    client
        .call::<PoliciesCreate>(rule(
            OperationCategory::Shell,
            PolicyDecision::Allow,
            None,
            Some("git status"),
            "git status is fine",
        ))
        .await
        .unwrap();
    let session = client
        .call::<SessionsStart>(SessionsStartParams {
            workspace_id: workspace.id,
            project_id: Some(project.id),
            provider: None,
            prompt: Some(
                "Do these steps with tools, one at a time: 1) Read notes.txt with the Read tool. 2) Read secret.txt \
                 with the Read tool. 3) Run exactly `git status` with the Bash tool. Then reply with the codeword from \
                 notes.txt and the word DONE."
                    .into(),
            ),
            model: Some(model),
        })
        .await
        .unwrap()
        .session;
    let evs = wait_turns(&client, session.id, 1).await;
    let evaluations = client
        .call::<PoliciesEvaluations>(PoliciesEvaluationsParams {
            session_id: Some(session.id),
            ..Default::default()
        })
        .await
        .unwrap()
        .evaluations;
    for e in evaluations.iter().rev() {
        println!("evaluation: {} {:?} {:?} {}", e.tool_name, e.decision, e.reason, e.explanation);
    }
    let approvals = client
        .call::<ApprovalsList>(ApprovalsListParams { session_id: Some(session.id), ..Default::default() })
        .await
        .unwrap()
        .approvals;
    assert!(approvals.is_empty(), "no person was asked: {approvals:?}");
    let decided = |tool: &str, d: PolicyDecision, needle: &str| {
        evaluations
            .iter()
            .any(|e| e.tool_name == tool && e.decision == d && e.input.to_string().contains(needle))
    };
    assert!(
        decided("Read", PolicyDecision::Allow, "notes.txt"),
        "in-workspace read reached Switchyard and was allowed"
    );
    assert!(decided("Read", PolicyDecision::Deny, "secret.txt"), "the block rule denied the secret read");
    assert!(decided("Bash", PolicyDecision::Allow, "git status"), "the exact command ran without asking");
    assert!(evs.iter().any(|e| e.event_type == "agent.tool_failed" && e.payload["rejected"] == true));
    let result = evs.iter().rev().find(|e| e.event_type == "agent.turn_completed").unwrap();
    let text = result.payload["result"].as_str().unwrap_or_default().to_lowercase();
    println!("result: {text}");
    assert!(text.contains("sapphire") && !text.contains("ruby"), "the blocked file was not read");
    client.call::<SessionsStop>(SessionIdParams { session_id: session.id }).await.unwrap();
}
