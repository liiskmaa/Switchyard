#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Opt-in end-to-end acceptance test of milestone 7 with the locally
//! installed Claude Code and Codex CLIs. Claude investigates, Codex implements,
//! Claude reviews (Codex addresses findings if the review asks for changes),
//! then the operator integrates through Switchyard: commit the workspace,
//! validate through the policy, approve the exact commits, and verify Git
//! history. Real (paid) requests, so it never runs by default:
//!
//! ```sh
//! SWITCHYARD_REAL_CLAUDE=1 SWITCHYARD_REAL_CODEX=1 \
//!   cargo test -p switchyard-core --test real_integration -- --ignored --nocapture
//! ```
//!
//! It works in a throwaway repository. Approvals are decided as a careful
//! person would: file changes inside the workspace and read-only commands are
//! approved, everything else is denied.

mod common;

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use switchyard_client::CoreClient;
use switchyard_protocol::methods::*;
use switchyard_protocol::model::{
    Approval, ApprovalStatus, IntegrationStatus, MessageKind, OperationCategory, Role, RunStatus, TaskKind,
    TaskStatus,
};
use uuid::Uuid;

use common::{RealCli, TestCore, git_repo};

async fn real_core() -> Option<TestCore> {
    TestCore::start_real(RealCli { claude: true, codex: true }, |c| {
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
    "nl",
];

fn decide(a: &Approval) -> ApprovalDecision {
    let category = a.policy.as_ref().map(|p| p.category);
    let inside = a.policy.as_ref().and_then(|p| p.scope.as_ref()).is_some_and(|s| s.kind == "workspace");
    let command = a.input["command"].as_str().map(str::to_owned);
    let read_only = command.as_deref().is_some_and(|c| {
        let c = c.trim_start_matches("/bin/zsh -lc ").trim_matches(|ch| ch == '\'' || ch == '"').trim();
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
    })
    .await
    .unwrap();
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[tokio::test]
#[ignore = "real CLIs (paid): SWITCHYARD_REAL_CLAUDE=1 SWITCHYARD_REAL_CODEX=1"]
async fn agents_build_and_review_then_the_operator_integrates() {
    let Some(core) = real_core().await else { return };
    let (c, _rx) = core.connect().await;
    let repo = git_repo(core.dir.path(), "real-integration");
    git(&repo, &["config", "user.name", "Switchyard Acceptance"]);
    git(&repo, &["config", "user.email", "acceptance@example.invalid"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("greet.py"), "def greet(name):\n    return 'Hello ' + name\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "__pycache__/\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "base"]);
    let base = git(&repo, &["rev-parse", "main"]);
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
    let check = c
        .call::<IntegrationsSetCommand>(IntegrationsSetCommandParams {
            project_id: project.id,
            command_id: None,
            name: "greeting check".into(),
            argv: vec![
                "/usr/bin/python3".into(),
                "-B".into(),
                "-c".into(),
                "import greet; assert greet.greet('world') == 'Hello world!', greet.greet('world')".into(),
            ],
            timeout_secs: 60,
            enabled: true,
            position: None,
        })
        .await
        .unwrap()
        .commands[0]
        .id;
    let task = c
        .call::<TasksCreate>(TasksCreateParams {
            project_id: project.id,
            title: "Make greet() add an exclamation mark".into(),
            description: Some("greet('world') must return 'Hello world!'. Change only greet.py and keep it small. Do not run Python.".into()),
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

    // 1-4. Investigate, implement, review; fix and review again if changes are requested.
    assign(&c, task.id, Role::Investigate, "claude_code", ws.id).await;
    assert_eq!(drive(&c, task.id, TaskStatus::InProgress).await.task.status, TaskStatus::AwaitingReview);
    let mut fix_rounds = 0;
    loop {
        assign(&c, task.id, Role::Implement, "codex", ws.id).await;
        assert_eq!(drive(&c, task.id, TaskStatus::InProgress).await.task.status, TaskStatus::AwaitingReview);
        assign(&c, task.id, Role::Review, "claude_code", ws.id).await;
        let d = drive(&c, task.id, TaskStatus::InReview).await;
        println!("review: {:?}", d.task.status_reason);
        if d.task.status_reason.as_deref() == Some("review: approved") || fix_rounds == 1 {
            assert_eq!(
                d.task.status_reason.as_deref(),
                Some("review: approved"),
                "the review did not approve"
            );
            break;
        }
        fix_rounds += 1;
    }
    println!("fix rounds after review: {fix_rounds}");
    assert_eq!(git(&repo, &["rev-parse", "main"]), base, "agents never touched the target");
    // The reviewer's session stops after its report.
    tokio::time::sleep(Duration::from_secs(3)).await;

    // 5. The operator creates the integration.
    let i = c
        .call::<IntegrationsCreate>(IntegrationsCreateParams {
            task_id: Some(task.id),
            workspace_id: None,
            target_branch: None,
        })
        .await
        .unwrap()
        .integration;
    println!("prepared: {:?} {:?}", i.status, i.blockers);
    // 6. Commit the agent's work; inspect the actual diff and commit identities.
    let i = c
        .call::<IntegrationsCommitWorkspace>(IntegrationsCommitWorkspaceParams {
            integration_id: i.id,
            message: "Make greet() add an exclamation mark".into(),
        })
        .await
        .unwrap()
        .integration;
    let ch = c.call::<IntegrationsChanges>(IntegrationsChangesParams { integration_id: i.id }).await.unwrap();
    println!(
        "changes: {} commit(s), files {:?}",
        ch.commits_total,
        ch.files.iter().map(|f| (&f.path, &f.kind)).collect::<Vec<_>>()
    );
    assert!(ch.files.iter().any(|f| f.path == "greet.py"));
    assert_eq!(
        (ch.source_commit.as_str(), ch.target_commit.as_str()),
        (i.source_commit.as_str(), base.as_str())
    );
    let patch = c
        .call::<IntegrationsPatch>(IntegrationsPatchParams {
            integration_id: i.id,
            path: Some("greet.py".into()),
            uncommitted: false,
        })
        .await
        .unwrap();
    println!("patch:\n{}", patch.patch);
    // 7. Validation through the policy (it asks; the operator authorizes).
    let plan = c.call::<IntegrationsGet>(IntegrationIdParams { integration_id: i.id }).await.unwrap().plan;
    assert_eq!(plan[0].policy_decision, "ask");
    c.call::<IntegrationsValidate>(IntegrationsValidateParams {
        integration_id: i.id,
        authorize: plan
            .iter()
            .filter(|p| p.command_id == check)
            .map(|p| ValidationAuthorization {
                command_id: p.command_id,
                command_line: p.command_line.clone(),
            })
            .collect(),
    })
    .await
    .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    let d = loop {
        let d = c.call::<IntegrationsGet>(IntegrationIdParams { integration_id: i.id }).await.unwrap();
        if d.integration.status != IntegrationStatus::Validating
            && d.runs.first().is_some_and(|r| r.status != RunStatus::Running)
        {
            break d;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(300)).await;
    };
    println!(
        "validation: {:?} {:?}",
        d.runs[0].status,
        d.runs[0].steps.iter().map(|s| (&s.name, s.status, &s.stderr)).collect::<Vec<_>>()
    );
    assert_eq!(d.runs[0].status, RunStatus::Passed);
    // 8. Conflicts checked: ready means the source contains the target.
    assert_eq!(d.integration.status, IntegrationStatus::Ready, "{:?}", d.integration.blockers);
    // 9-10. The operator approves the exact commits; Core integrates.
    let done = c
        .call::<IntegrationsApprove>(IntegrationsApproveParams {
            integration_id: i.id,
            source_commit: d.integration.source_commit.clone(),
            target_commit: d.integration.target_commit.clone(),
        })
        .await
        .unwrap()
        .integration;
    assert_eq!(done.status, IntegrationStatus::Integrated, "{:?}", done.failure);
    // 11. The journal has the whole operation.
    let events: Vec<String> = c
        .call::<EventsList>(EventsListParams {
            project_id: Some(project.id),
            after_seq: Some(0),
            limit: Some(1000),
            ..Default::default()
        })
        .await
        .unwrap()
        .events
        .into_iter()
        .filter(|e| e.payload["integration_id"] == serde_json::json!(i.id))
        .map(|e| e.event_type)
        .collect();
    println!("integration events: {events:?}");
    for e in [
        "integration.created",
        "integration.workspace_changed",
        "integration.validation_finished",
        "integration.approved",
        "integration.integrated",
    ] {
        assert!(events.contains(&e.to_owned()), "{e}");
    }
    // 12. Git history and delivery status.
    let merge = done.merge_commit.unwrap();
    assert_eq!(git(&repo, &["rev-parse", "main"]), merge);
    println!("{}", git(&repo, &["log", "--graph", "--oneline", "-5", "main"]));
    println!("{}", git(&repo, &["log", "-1", "--format=%B", "main"]));
    assert_eq!(
        git(&repo, &["log", "-1", "--format=%P", "main"]),
        format!("{base} {}", d.integration.source_commit)
    );
    assert!(std::fs::read_to_string(repo.join("greet.py")).unwrap().contains('!'));
    let t = c.call::<TasksGet>(TaskIdParams { task_id: task.id }).await.unwrap();
    assert_eq!(t.task.status, TaskStatus::AwaitingReview, "delivery is not completion");
    let completed = c
        .call::<TasksComplete>(TaskDecisionParams { task_id: task.id, note: Some("integrated".into()) })
        .await
        .unwrap();
    assert_eq!(completed.task.status, TaskStatus::Completed);
}
