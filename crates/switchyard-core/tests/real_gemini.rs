#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Opt-in end-to-end test against a real Gemini CLI over ACP (ADR 0044). It
//! makes real model requests with your Gemini API key (`GEMINI_API_KEY`, or
//! `~/.gemini/.env`), so it never runs by default:
//!
//! ```sh
//! SWITCHYARD_REAL_GEMINI=1 [SWITCHYARD_GEMINI_BIN=/path/to/gemini] \
//!   cargo test -p switchyard-core --test real_gemini -- --ignored --nocapture
//! ```
//!
//! It works in a throwaway repository and uses `gemini-3.5-flash-lite` (the
//! free tier allows it). Approvals are decided by the test as a person would.

mod common;

use std::time::Duration;

use switchyard_client::CoreClient;
use switchyard_protocol::methods::*;
use switchyard_protocol::model::{ApprovalStatus, OperationCategory, Session, SessionState};
use uuid::Uuid;

use common::{RealCli, TestCore, git_repo};

const MODEL: &str = "gemini-3.5-flash-lite";

async fn turns(c: &CoreClient, s: Uuid) -> usize {
    c.call::<EventsList>(EventsListParams {
        session_id: Some(s),
        after_seq: Some(0),
        limit: Some(1000),
        event_types: vec!["agent.turn_completed".into()],
        ..Default::default()
    })
    .await
    .unwrap()
    .events
    .len()
}

async fn pending(c: &CoreClient, s: Uuid) -> Option<switchyard_protocol::model::Approval> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    loop {
        let list = c
            .call::<ApprovalsList>(ApprovalsListParams {
                status: Some(ApprovalStatus::Pending),
                session_id: Some(s),
                ..Default::default()
            })
            .await
            .unwrap()
            .approvals;
        if let Some(a) = list.into_iter().next() {
            return Some(a);
        }
        if tokio::time::Instant::now() > deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

async fn wait_turns(c: &CoreClient, s: Uuid, n: usize) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    while turns(c, s).await < n {
        assert!(tokio::time::Instant::now() < deadline, "turn {n} did not end");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[tokio::test]
#[ignore = "uses the real Gemini CLI and makes model requests; set SWITCHYARD_REAL_GEMINI=1"]
async fn every_action_asks_and_only_approved_ones_happen() {
    let Some(core) = TestCore::start_real(RealCli { claude: false, codex: false, gemini: true }, |c| {
        c.agent_ready_timeout = Duration::from_secs(90);
    })
    .await
    else {
        return;
    };
    // A real Gemini (Node.js start, login, session handshake) takes longer than the usual 10 s.
    let mut options = switchyard_client::ClientOptions::new("integration-test", "0.0.0");
    options.request_timeout = Duration::from_secs(120);
    let (c, _rx) = CoreClient::connect(&core.socket(), options).await.unwrap();
    let repo = git_repo(core.dir.path(), "real-gemini");
    let project =
        c.call::<ProjectsAdd>(ProjectsAddParams { path: repo.display().to_string() }).await.unwrap().project;
    let ws = c
        .call::<WorkspacesCreate>(WorkspacesCreateParams {
            project_id: project.id,
            name: None,
            base_ref: None,
        })
        .await
        .unwrap()
        .workspace;
    let s: Session = c
        .call::<SessionsStart>(SessionsStartParams {
            workspace_id: ws.id,
            project_id: None,
            provider: Some("gemini".into()),
            prompt: None,
            model: Some(MODEL.into()),
        })
        .await
        .unwrap()
        .session;

    // A "known safe" command still asks (the admin policy's argsPattern rule).
    let send = |text: &str| SessionsSendParams { session_id: s.id, text: text.into(), queue_if_busy: false };
    c.call::<SessionsSend>(send(
        "Use your shell tool to run exactly this one command: ls -la . Then say done.",
    ))
    .await
    .unwrap();
    let a = pending(&c, s.id).await.expect("`ls -la` must ask Switchyard");
    assert_eq!(a.tool_name, "execute");
    assert_eq!(a.policy.as_ref().map(|p| p.category), Some(OperationCategory::Shell));
    c.call::<ApprovalsDecide>(ApprovalsDecideParams {
        approval_id: a.id,
        decision: ApprovalDecision::Approve,
        note: None,
    })
    .await
    .unwrap();
    wait_turns(&c, s.id, 1).await;

    // A write that is denied never happens.
    c.call::<SessionsSend>(send(
        "Create a file named denied.txt containing the word no, using your file writing tool. If it is refused, do not retry; just say so.",
    ))
    .await
    .unwrap();
    let a = pending(&c, s.id).await.expect("the write must ask");
    assert_eq!(a.policy.as_ref().map(|p| p.category), Some(OperationCategory::FileWrite), "{a:?}");
    c.call::<ApprovalsDecide>(ApprovalsDecideParams {
        approval_id: a.id,
        decision: ApprovalDecision::Deny,
        note: None,
    })
    .await
    .unwrap();
    wait_turns(&c, s.id, 2).await;
    assert!(!std::path::Path::new(&ws.path).join("denied.txt").exists());
    let s = c.call::<SessionsGet>(SessionIdParams { session_id: s.id }).await.unwrap().session;
    assert_ne!(s.state, SessionState::Failed, "{:?}", s.failure);
}
