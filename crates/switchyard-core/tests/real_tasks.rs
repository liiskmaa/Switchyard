#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Opt-in end-to-end task test with the locally installed Claude Code and
//! Codex CLIs: Claude investigates, Codex implements, Claude reviews. It makes
//! real (paid) model requests with your existing logins, so it never runs by
//! default and needs both opt-ins:
//!
//! ```sh
//! SWITCHYARD_REAL_CLAUDE=1 SWITCHYARD_REAL_CODEX=1 \
//!   cargo test -p switchyard-core --test real_tasks -- --ignored --nocapture
//! ```
//!
//! It works in a throwaway repository. Approvals are decided as a careful
//! person would: file changes inside the workspace and read-only commands are
//! approved, everything else is denied.

mod common;

use std::path::Path;
use std::time::Duration;

use switchyard_client::CoreClient;
use switchyard_protocol::methods::*;
use switchyard_protocol::model::{
    Approval, ApprovalStatus, AssignmentStatus, MessageKind, OperationCategory, Role, SessionAccess,
    TaskKind, TaskStatus,
};
use uuid::Uuid;

use common::{RealCli, TestCore, git_repo};

async fn real_core() -> Option<TestCore> {
    TestCore::start_real(RealCli { claude: true, codex: true, gemini: false }, |c| {
        c.agent_ready_timeout = Duration::from_secs(90);
    })
    .await
}

const READ_ONLY: &[&str] = &[
    "ls",
    "cat",
    "head",
    "tail",
    "wc",
    "grep",
    "rg",
    "find",
    "pwd",
    "git status",
    "git diff",
    "git log",
    "sed -n",
];

fn decide(a: &Approval) -> ApprovalDecision {
    let category = a.policy.as_ref().map(|p| p.category);
    let inside = a.policy.as_ref().and_then(|p| p.scope.as_ref()).is_some_and(|s| s.kind == "workspace");
    let command = a.input["command"].as_str().map(str::to_owned).or_else(|| {
        a.input["command"]
            .as_array()
            .map(|v| v.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" "))
    });
    let read_only = command.as_deref().is_some_and(|c| {
        let c = c.trim_start_matches("/bin/zsh -lc ").trim_matches('\'').trim();
        READ_ONLY.iter().any(|p| c == *p || c.starts_with(&format!("{p} ")))
            && !c.contains(['>', ';', '&', '|', '`', '$'])
    });
    match category {
        Some(OperationCategory::FileWrite | OperationCategory::FileEdit | OperationCategory::FileRead)
            if inside =>
        {
            ApprovalDecision::Approve
        }
        Some(OperationCategory::Shell) if read_only => ApprovalDecision::Approve,
        _ => ApprovalDecision::Deny,
    }
}

/// Answers approvals until the task leaves `working`.
async fn drive(c: &CoreClient, task: Uuid, working: TaskStatus) -> TaskDetail {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(600);
    loop {
        let d = c.call::<TasksGet>(TaskIdParams { task_id: task }).await.unwrap();
        if d.task.status != working && d.task.status != TaskStatus::Assigned {
            return d;
        }
        if let Some(s) = d.task.open_assignment.as_ref().and_then(|a| a.session_id) {
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
                let decision = decide(&a);
                println!(
                    "approval {} {:?} {} -> {decision:?}",
                    a.tool_name,
                    a.policy.as_ref().map(|p| p.category),
                    a.input
                );
                let _ = c
                    .call::<ApprovalsDecide>(ApprovalsDecideParams {
                        approval_id: a.id,
                        decision,
                        note: None,
                    })
                    .await;
            }
        }
        // An unusable report: ask once for a proper one.
        if let Some(m) = d.messages.last()
            && m.kind == MessageKind::MalformedReport
            && d.messages.iter().filter(|m| m.kind == MessageKind::OperatorNote).count() < 2
        {
            println!("unusable report: {}", m.payload);
            c.call::<TasksNote>(TasksNoteParams {
                task_id: task,
                text: "Please end your final message with the switchyard-report block described in the task context.".into(),
            })
            .await
            .unwrap();
        }
        assert!(tokio::time::Instant::now() < deadline, "the task stayed {working}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn assign(c: &CoreClient, task: Uuid, role: Role, provider: &str, ws: Uuid) {
    c.call::<TasksAssign>(TasksAssignParams {
        task_id: task,
        role,
        provider: provider.into(),
        workspace_id: ws,
        start: true,
        model: None,
    })
    .await
    .unwrap();
}

fn commit_all(repo: &Path) {
    for args in
        [vec!["add", "-A"], vec!["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "base"]]
    {
        assert!(std::process::Command::new("git").args(&args).current_dir(repo).status().unwrap().success());
    }
}

#[tokio::test]
#[ignore = "real CLIs (paid): SWITCHYARD_REAL_CLAUDE=1 SWITCHYARD_REAL_CODEX=1"]
async fn claude_investigates_codex_implements_claude_reviews() {
    let Some(core) = real_core().await else { return };
    let (c, _rx) = core.connect().await;
    let repo = git_repo(core.dir.path(), "real-tasks");
    std::fs::write(
        repo.join("greet.py"),
        "def greet(name):\n    return 'Hello ' + name\n\nif __name__ == '__main__':\n    print(greet('world'))\n",
    )
    .unwrap();
    commit_all(&repo);
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
    let task = c
        .call::<TasksCreate>(TasksCreateParams {
            project_id: project.id,
            title: "Make greet() add an exclamation mark".into(),
            description: Some(
                "greet('world') should return 'Hello world!'. Change only greet.py; keep it small. Do not run Python.".into(),
            ),
            kind: Some(TaskKind::Implementation),
            priority: None,
            acceptance_criteria: vec!["greet('world') returns 'Hello world!'".into(), "only greet.py changes".into()],
            depends_on: vec![],
            parent_task_id: None,
            publish: true,
        })
        .await
        .unwrap()
        .task;

    // 1. Claude investigates.
    assign(&c, task.id, Role::Investigate, "claude_code", ws.id).await;
    let d = drive(&c, task.id, TaskStatus::InProgress).await;
    println!("investigation: {:?} {:?}", d.task.status, d.messages.last().map(|m| &m.payload));
    assert_eq!(d.task.status, TaskStatus::AwaitingReview);

    // 2. Codex implements.
    assign(&c, task.id, Role::Implement, "codex", ws.id).await;
    let d = drive(&c, task.id, TaskStatus::InProgress).await;
    println!("implementation: {:?} {:?}", d.task.status, d.messages.last().map(|m| &m.payload));
    assert_eq!(d.task.status, TaskStatus::AwaitingReview);
    let code = std::fs::read_to_string(Path::new(&ws.path).join("greet.py")).unwrap();
    println!("greet.py now:\n{code}");
    assert!(code.contains('!'), "the implementation changed greet.py");

    // 3. Claude reviews in a read-only session.
    assign(&c, task.id, Role::Review, "claude_code", ws.id).await;
    let d = drive(&c, task.id, TaskStatus::InReview).await;
    let review = d.assignments.iter().rev().find(|a| a.role == Role::Review).unwrap();
    assert_eq!(review.status, AssignmentStatus::Reported, "{:?}", review.outcome);
    let s = c
        .call::<SessionsGet>(SessionIdParams { session_id: review.session_id.unwrap() })
        .await
        .unwrap()
        .session;
    assert_eq!(s.access, SessionAccess::Review);
    let verdict = d.messages.iter().rev().find(|m| m.kind == MessageKind::ReviewResult).unwrap();
    println!("review: {}", verdict.payload);
    assert_eq!(verdict.payload["workspace_changed"], false, "the reviewer changed nothing");
    assert_eq!(d.task.status, TaskStatus::AwaitingReview, "a review never completes a task");
}
