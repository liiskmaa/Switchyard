//! Milestone 7: change integration against real temporary repositories,
//! with the fake agents where a task is involved.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::json;
use switchyard_client::{ClientError, CoreClient};
use switchyard_protocol::ErrorCode;
use switchyard_protocol::methods::*;
use switchyard_protocol::model::{
    ApprovalStatus, Integration, IntegrationStatus, OperationCategory, PolicyConditions, PolicyDecision,
    Role, RunStatus, SessionState, StepStatus, TaskStatus,
};
use uuid::Uuid;

use common::{TestCore, git_repo};

fn git(dir: &Path, args: &[&str]) -> String {
    let out =
        Command::new("git").arg("-C").arg(dir).args(args).env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn commit(dir: &Path, msg: &str) -> String {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", msg]);
    git(dir, &["rev-parse", "HEAD"])
}

struct Fx {
    core: TestCore,
    c: CoreClient,
    repo: PathBuf,
    project_id: Uuid,
    workspace_id: Uuid,
    ws: PathBuf,
}

/// A project (main checked out in the project checkout) with one workspace.
/// The repository has its own identity: Core's commits never depend on the
/// machine's Git configuration.
async fn fixture() -> Fx {
    let core = TestCore::start_with(|_| {}).await;
    let (c, _rx) = core.connect().await;
    let repo = git_repo(core.dir.path(), "int-repo");
    git(&repo, &["config", "user.name", "Switchyard Test"]);
    git(&repo, &["config", "user.email", "test@example.invalid"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("shared.txt"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "target/\n").unwrap();
    commit(&repo, "shared");
    let project =
        c.call::<ProjectsAdd>(ProjectsAddParams { path: repo.display().to_string() }).await.unwrap().project;
    let w = c
        .call::<WorkspacesCreate>(WorkspacesCreateParams {
            project_id: project.id,
            name: None,
            base_ref: None,
        })
        .await
        .unwrap()
        .workspace;
    Fx { core, c, repo, project_id: project.id, workspace_id: w.id, ws: PathBuf::from(w.path) }
}

async fn no_review(f: &Fx) {
    f.c.call::<IntegrationsSetSettings>(IntegrationsSetSettingsParams {
        project_id: f.project_id,
        require_review: false,
    })
    .await
    .unwrap();
}

async fn create(f: &Fx, task_id: Option<Uuid>) -> Integration {
    f.c.call::<IntegrationsCreate>(IntegrationsCreateParams {
        task_id,
        workspace_id: if task_id.is_some() { None } else { Some(f.workspace_id) },
        target_branch: None,
    })
    .await
    .unwrap()
    .integration
}

async fn get(f: &Fx, id: Uuid) -> IntegrationDetail {
    f.c.call::<IntegrationsGet>(IntegrationIdParams { integration_id: id }).await.unwrap()
}

async fn refresh(f: &Fx, id: Uuid) -> Integration {
    f.c.call::<IntegrationsRefresh>(IntegrationIdParams { integration_id: id }).await.unwrap().integration
}

async fn commit_ws(f: &Fx, id: Uuid, msg: &str) -> Result<Integration, ClientError> {
    f.c.call::<IntegrationsCommitWorkspace>(IntegrationsCommitWorkspaceParams {
        integration_id: id,
        message: msg.into(),
    })
    .await
    .map(|r| r.integration)
}

async fn approve(f: &Fx, i: &Integration) -> Result<Integration, ClientError> {
    f.c.call::<IntegrationsApprove>(IntegrationsApproveParams {
        integration_id: i.id,
        source_commit: i.source_commit.clone(),
        target_commit: i.target_commit.clone(),
    })
    .await
    .map(|r| r.integration)
}

fn codes(i: &Integration) -> Vec<String> {
    i.blockers.iter().map(|b| b.code.clone()).collect()
}

fn code_of(e: &ClientError) -> Option<ErrorCode> {
    match e {
        ClientError::Remote(p) => Some(p.code),
        _ => None,
    }
}

async fn wait_run(f: &Fx, id: Uuid) -> IntegrationDetail {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let d = get(f, id).await;
        if d.integration.status != IntegrationStatus::Validating
            && d.runs.first().is_some_and(|r| r.status != RunStatus::Running)
        {
            return d;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "validation did not finish: {:?}",
            d.integration.status
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn add_command(f: &Fx, name: &str, argv: &[&str], timeout: u32) -> Uuid {
    let s =
        f.c.call::<IntegrationsSetCommand>(IntegrationsSetCommandParams {
            project_id: f.project_id,
            command_id: None,
            name: name.into(),
            argv: argv.iter().map(|s| s.to_string()).collect(),
            timeout_secs: timeout,
            enabled: true,
            position: None,
        })
        .await
        .unwrap();
    s.commands.iter().find(|c| c.name == name).unwrap().id
}

/// The operator's authorization of commands, as shown in the plan (exact command lines).
async fn auth(f: &Fx, ids: &[Uuid]) -> Vec<ValidationAuthorization> {
    let s =
        f.c.call::<IntegrationsSettings>(IntegrationsSettingsParams { project_id: f.project_id })
            .await
            .unwrap();
    ids.iter()
        .map(|id| {
            let c = s.commands.iter().find(|c| c.id == *id).unwrap();
            ValidationAuthorization { command_id: c.id, command_line: c.command_line.clone() }
        })
        .collect()
}

async fn shell_rule(f: &Fx, command: &str, decision: PolicyDecision) {
    f.c.call::<PoliciesCreate>(PoliciesCreateParams {
        project_id: f.project_id,
        workspace_id: None,
        category: OperationCategory::Shell,
        decision,
        conditions: PolicyConditions { path_prefix: None, command: Some(command.into()) },
        description: format!("{decision:?} {command}"),
        enabled: None,
    })
    .await
    .unwrap();
}

async fn events_of(f: &Fx, id: Uuid) -> Vec<String> {
    f.c.call::<EventsList>(EventsListParams {
        project_id: Some(f.project_id),
        after_seq: Some(0),
        limit: Some(1000),
        ..Default::default()
    })
    .await
    .unwrap()
    .events
    .into_iter()
    .filter(|e| e.event_type.starts_with("integration.") && e.payload["integration_id"] == json!(id))
    .map(|e| e.event_type)
    .collect()
}

/// Agent work arrives uncommitted; the operator commits it, validation runs
/// through the policy, and only an approval of the exact commits integrates.
#[tokio::test]
async fn reviewed_validated_work_is_integrated_into_the_checked_out_target() {
    let f = fixture().await;
    no_review(&f).await;
    let check = add_command(&f, "feature present", &["/bin/test", "-f", "feature.txt"], 30).await;
    std::fs::write(f.ws.join("feature.txt"), "new feature\n").unwrap();
    std::fs::write(f.ws.join("shared.txt"), "one\nTWO\nthree\n").unwrap();
    let target_before = git(&f.repo, &["rev-parse", "main"]);

    let i = create(&f, None).await;
    assert_eq!((i.target_branch.as_str(), i.target_commit.as_str()), ("main", target_before.as_str()));
    assert_eq!(i.status, IntegrationStatus::Open);
    assert!(codes(&i).contains(&"uncommitted_changes".to_owned()), "{:?}", codes(&i));
    assert!(codes(&i).contains(&"no_changes".to_owned()), "nothing committed yet");
    let ch =
        f.c.call::<IntegrationsChanges>(IntegrationsChangesParams { integration_id: i.id }).await.unwrap();
    assert!(
        ch.files.is_empty() && ch.uncommitted.iter().any(|x| x.path == "feature.txt" && x.kind == "added")
    );

    // An approval while blocked is refused.
    assert_eq!(code_of(&approve(&f, &i).await.unwrap_err()), Some(ErrorCode::InvalidState));

    let i = commit_ws(&f, i.id, "Add the feature").await.unwrap();
    assert_eq!(codes(&i), vec!["validation_required"], "{:?}", i.blockers);
    let source = i.source_commit.clone();
    assert_eq!(git(&f.ws, &["rev-parse", "HEAD"]), source);
    let ch =
        f.c.call::<IntegrationsChanges>(IntegrationsChangesParams { integration_id: i.id }).await.unwrap();
    assert_eq!((ch.commits_total, ch.files_total), (1, 2));
    assert!(!ch.diverged && ch.uncommitted.is_empty());
    let patch =
        f.c.call::<IntegrationsPatch>(IntegrationsPatchParams {
            integration_id: i.id,
            path: Some("shared.txt".into()),
            uncommitted: false,
        })
        .await
        .unwrap();
    assert!(patch.patch.contains("+TWO") && !patch.truncated);

    // The default policy asks about commands: nothing runs without authorization.
    let plan = get(&f, i.id).await.plan;
    assert_eq!(plan[0].policy_decision, "ask");
    let r = f
        .c
        .call::<IntegrationsValidate>(IntegrationsValidateParams { integration_id: i.id, authorize: vec![] })
        .await
        .unwrap();
    assert_eq!(r.run.status, RunStatus::Failed);
    assert_eq!(r.run.steps[0].status, StepStatus::Refused);
    let d = get(&f, i.id).await;
    assert!(codes(&d.integration).contains(&"validation_failed".to_owned()));
    // Authorized: it runs in the workspace for exactly the source commit.
    f.c.call::<IntegrationsValidate>(IntegrationsValidateParams {
        integration_id: i.id,
        authorize: auth(&f, &[check]).await,
    })
    .await
    .unwrap();
    let d = wait_run(&f, i.id).await;
    let run = &d.runs[0];
    assert_eq!((run.status, run.source_commit.as_str()), (RunStatus::Passed, source.as_str()));
    assert_eq!(run.steps[0].authorized_by.as_deref(), Some("integration-test"));
    assert_eq!(run.steps[0].cwd, f.ws.display().to_string());
    assert_eq!(d.integration.status, IntegrationStatus::Ready, "{:?}", d.integration.blockers);

    // Approving other commits than the bound ones is refused.
    let wrong =
        f.c.call::<IntegrationsApprove>(IntegrationsApproveParams {
            integration_id: i.id,
            source_commit: target_before.clone(),
            target_commit: target_before.clone(),
        })
        .await
        .unwrap_err();
    assert_eq!(code_of(&wrong), Some(ErrorCode::Conflict));
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), target_before, "nothing moved");

    let done = approve(&f, &d.integration).await.unwrap();
    assert_eq!(done.status, IntegrationStatus::Integrated);
    assert_eq!(
        done.method.as_deref(),
        Some("checkout_fast_forward"),
        "main is checked out in the project checkout"
    );
    let merge = done.merge_commit.clone().unwrap();
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), merge);
    assert_eq!(git(&f.repo, &["log", "-1", "--format=%P", "main"]), format!("{target_before} {source}"));
    let body = git(&f.repo, &["log", "-1", "--format=%B", "main"]);
    assert!(
        body.contains(&format!("Switchyard-Integration: {}", i.id))
            && body.contains(&format!("Switchyard-Source: {source}"))
    );
    assert_eq!(
        std::fs::read_to_string(f.repo.join("feature.txt")).unwrap(),
        "new feature\n",
        "the checkout was fast-forwarded"
    );
    assert_eq!(git(&f.repo, &["status", "--porcelain"]), "");
    assert_eq!(git(&f.ws, &["rev-parse", "HEAD"]), source, "the source workspace is untouched");
    // Repeating the approval is refused; nothing moves again.
    assert_eq!(code_of(&approve(&f, &done).await.unwrap_err()), Some(ErrorCode::Conflict));
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), merge);
    let ev = events_of(&f, i.id).await;
    for e in [
        "integration.created",
        "integration.workspace_changed",
        "integration.validation_finished",
        "integration.approved",
        "integration.integrated",
    ] {
        assert!(ev.contains(&e.to_owned()), "missing {e} in {ev:?}");
    }
}

/// Approval is bound to exact commits: any movement invalidates it.
#[tokio::test]
async fn approvals_are_bound_to_the_exact_source_and_target_commits() {
    let f = fixture().await;
    no_review(&f).await;
    std::fs::write(f.ws.join("a.txt"), "a\n").unwrap();
    let i = create(&f, None).await;
    let i = commit_ws(&f, i.id, "a").await.unwrap();
    assert_eq!(
        i.status,
        IntegrationStatus::Ready,
        "no validation configured, no review required: {:?}",
        i.blockers
    );

    // The target moves (someone commits on main).
    std::fs::write(f.repo.join("other.txt"), "o\n").unwrap();
    let moved = commit(&f.repo, "unrelated work on main");
    let err = approve(&f, &i).await.unwrap_err();
    assert_eq!(code_of(&err), Some(ErrorCode::Conflict), "{err:?}");
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), moved, "the target was not touched");
    let d = get(&f, i.id).await.integration;
    assert_eq!(d.status, IntegrationStatus::Open);
    assert_eq!(codes(&d), vec!["out_of_date"]);
    assert_eq!(d.target_commit, moved, "rebound to the new target commit");
    assert!(d.approved_by.is_none());

    // Update the workspace from the target (a merge inside the workspace).
    let u =
        f.c.call::<IntegrationsUpdateFromTarget>(IntegrationIdParams { integration_id: i.id })
            .await
            .unwrap()
            .integration;
    assert_eq!(u.status, IntegrationStatus::Ready, "{:?}", u.blockers);
    assert_ne!(u.source_commit, i.source_commit);
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), moved, "updating never touches the target");

    // The source moves after preparation.
    std::fs::write(f.ws.join("late.txt"), "l\n").unwrap();
    commit(&f.ws, "late change");
    let err = approve(&f, &u).await.unwrap_err();
    assert_eq!(code_of(&err), Some(ErrorCode::Conflict));
    let now = get(&f, i.id).await.integration;
    assert_ne!(now.source_commit, u.source_commit);
    let done = approve(&f, &now).await.unwrap();
    assert_eq!(done.status, IntegrationStatus::Integrated);
    assert!(f.repo.join("late.txt").exists() && f.repo.join("other.txt").exists());
}

/// Conflicts are resolved in the workspace; the target branch is never used.
#[tokio::test]
async fn conflicts_are_resolved_in_the_workspace_never_on_the_target() {
    let f = fixture().await;
    no_review(&f).await;
    std::fs::write(f.ws.join("shared.txt"), "one\nWORKSPACE\nthree\n").unwrap();
    std::fs::write(f.repo.join("shared.txt"), "one\nMAIN\nthree\n").unwrap();
    let main_tip = commit(&f.repo, "main edits shared");
    let i = create(&f, None).await;
    let i = commit_ws(&f, i.id, "ws edits shared").await.unwrap();
    assert_eq!(i.status, IntegrationStatus::Conflicted);
    assert!(i.blockers.iter().any(|b| b.code == "conflicts" && b.message.contains("shared.txt")));

    let r =
        f.c.call::<IntegrationsUpdateFromTarget>(IntegrationIdParams { integration_id: i.id })
            .await
            .unwrap()
            .integration;
    assert_eq!(r.status, IntegrationStatus::Resolving);
    assert!(std::fs::read_to_string(f.ws.join("shared.txt")).unwrap().contains("<<<<<<<"));
    // Conflict markers cannot be committed.
    let err = commit_ws(&f, i.id, "with markers").await.unwrap_err();
    assert_eq!(code_of(&err), Some(ErrorCode::InvalidState), "{err:?}");
    // Abort restores the workspace.
    let a =
        f.c.call::<IntegrationsAbortResolution>(IntegrationIdParams { integration_id: i.id })
            .await
            .unwrap()
            .integration;
    assert_eq!(a.status, IntegrationStatus::Conflicted);
    assert_eq!(std::fs::read_to_string(f.ws.join("shared.txt")).unwrap(), "one\nWORKSPACE\nthree\n");

    // Resolve for real, complete, integrate.
    f.c.call::<IntegrationsUpdateFromTarget>(IntegrationIdParams { integration_id: i.id }).await.unwrap();
    std::fs::write(f.ws.join("shared.txt"), "one\nMAIN and WORKSPACE\nthree\n").unwrap();
    let done = commit_ws(&f, i.id, "Resolve shared.txt").await.unwrap();
    assert_eq!(done.status, IntegrationStatus::Ready, "{:?}", done.blockers);
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), main_tip, "the target never moved while resolving");
    let integrated = approve(&f, &done).await.unwrap();
    assert_eq!(integrated.status, IntegrationStatus::Integrated);
    assert_eq!(
        std::fs::read_to_string(f.repo.join("shared.txt")).unwrap(),
        "one\nMAIN and WORKSPACE\nthree\n"
    );
}

/// Uncommitted work in the target checkout blocks integration and is never touched.
#[tokio::test]
async fn uncommitted_work_in_the_target_checkout_is_never_overwritten() {
    let f = fixture().await;
    no_review(&f).await;
    std::fs::write(f.ws.join("shared.txt"), "one\nchanged by ws\nthree\n").unwrap();
    let i = create(&f, None).await;
    commit_ws(&f, i.id, "ws").await.unwrap();
    std::fs::write(f.repo.join("shared.txt"), "operator's unsaved work\n").unwrap();
    let i = refresh(&f, i.id).await;
    assert_eq!(codes(&i), vec!["target_checkout_dirty"]);
    assert_eq!(code_of(&approve(&f, &i).await.unwrap_err()), Some(ErrorCode::InvalidState));
    assert_eq!(std::fs::read_to_string(f.repo.join("shared.txt")).unwrap(), "operator's unsaved work\n");

    // When the target is not checked out, the branch moves by compare-and-swap only.
    git(&f.repo, &["checkout", "--", "shared.txt"]);
    git(&f.repo, &["switch", "-q", "-c", "elsewhere"]);
    let i = refresh(&f, i.id).await;
    assert_eq!(i.status, IntegrationStatus::Ready, "{:?}", i.blockers);
    let done = approve(&f, &i).await.unwrap();
    assert_eq!(done.method.as_deref(), Some("ref_update"));
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), done.merge_commit.unwrap());
    assert_eq!(
        git(&f.repo, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "elsewhere",
        "the checkout was not switched"
    );
}

/// Validation: policy decisions, failure, timeout, cancellation, output limits,
/// changes made by commands, and staleness.
#[tokio::test]
async fn validation_respects_policy_and_reports_every_outcome() {
    let f = fixture().await;
    no_review(&f).await;
    std::fs::write(f.ws.join("a.txt"), "a\n").unwrap();
    let i = create(&f, None).await;
    let i = commit_ws(&f, i.id, "a").await.unwrap();
    let validate = |authorize: Vec<ValidationAuthorization>| {
        f.c.call::<IntegrationsValidate>(IntegrationsValidateParams { integration_id: i.id, authorize })
    };

    // A denied command never runs, even if "authorized".
    let destructive = add_command(&f, "reset", &["git", "reset", "--hard"], 10).await;
    shell_rule(&f, "git reset --hard", PolicyDecision::Deny).await;
    let r = validate(auth(&f, &[destructive]).await).await.unwrap();
    assert_eq!((r.run.status, r.run.steps[0].status), (RunStatus::Failed, StepStatus::Refused));
    assert!(r.run.steps[0].detail.as_deref().unwrap().contains("denied by policy"));
    f.c.call::<IntegrationsDeleteCommand>(IntegrationsDeleteCommandParams { command_id: destructive })
        .await
        .unwrap();

    // A failure is reported with its exit code and output; later commands are skipped.
    let fail = add_command(&f, "fails", &["/bin/sh", "-c", "echo out; echo err >&2; exit 3"], 10).await;
    let later = add_command(&f, "later", &["/usr/bin/true"], 10).await;
    shell_rule(&f, "/usr/bin/true", PolicyDecision::Allow).await;
    // Quoted arguments cannot be allowed by an exact rule (M4 grammar): the
    // operator authorizes this one; `/usr/bin/true` is allowed by its rule.
    validate(auth(&f, &[fail]).await).await.unwrap();
    let d = wait_run(&f, i.id).await;
    let s = &d.runs[0].steps;
    assert_eq!(
        (s[0].status, s[0].exit_code, s[0].authorized_by.as_deref()),
        (StepStatus::Failed, Some(3), Some("integration-test"))
    );
    assert_eq!(s[1].authorized_by.as_deref(), Some("policy"));
    assert_eq!((s[0].stdout.as_deref(), s[0].stderr.as_deref()), (Some("out\n"), Some("err\n")));
    assert_eq!(s[1].status, StepStatus::Skipped);
    assert!(codes(&d.integration).contains(&"validation_failed".to_owned()));
    f.c.call::<IntegrationsDeleteCommand>(IntegrationsDeleteCommandParams { command_id: fail })
        .await
        .unwrap();
    f.c.call::<IntegrationsDeleteCommand>(IntegrationsDeleteCommandParams { command_id: later })
        .await
        .unwrap();

    // Timeout, with the process group stopped.
    let slow = add_command(&f, "slow", &["/bin/sleep", "30"], 1).await;
    validate(auth(&f, &[slow]).await).await.unwrap();
    let d = wait_run(&f, i.id).await;
    assert_eq!(d.runs[0].steps[0].status, StepStatus::TimedOut);
    assert!(d.runs[0].steps[0].detail.as_deref().unwrap().contains("timeout"));

    // Cancellation; meanwhile the workspace accepts no session.
    let slower = add_command(&f, "slower", &["/bin/sleep", "30"], 60).await;
    f.c.call::<IntegrationsDeleteCommand>(IntegrationsDeleteCommandParams { command_id: slow })
        .await
        .unwrap();
    validate(auth(&f, &[slower]).await).await.unwrap();
    let busy =
        f.c.call::<SessionsStart>(SessionsStartParams {
            workspace_id: f.workspace_id,
            project_id: None,
            provider: None,
            prompt: None,
            model: None,
        })
        .await
        .unwrap_err();
    assert_eq!(code_of(&busy), Some(ErrorCode::InvalidWorkspaceState), "{busy:?}");
    f.c.call::<IntegrationsCancelValidation>(IntegrationIdParams { integration_id: i.id }).await.unwrap();
    let d = wait_run(&f, i.id).await;
    assert_eq!((d.runs[0].status, d.runs[0].steps[0].status), (RunStatus::Cancelled, StepStatus::Cancelled));
    f.c.call::<IntegrationsDeleteCommand>(IntegrationsDeleteCommandParams { command_id: slower })
        .await
        .unwrap();

    // Output is cut at the limit (the tail is kept).
    let loud =
        add_command(&f, "loud", &["/bin/sh", "-c", "head -c 200000 /dev/zero | tr '\\0' x; echo END"], 30)
            .await;
    validate(auth(&f, &[loud]).await).await.unwrap();
    let d = wait_run(&f, i.id).await;
    let st = &d.runs[0].steps[0];
    assert_eq!(st.status, StepStatus::Passed);
    assert!(st.stdout_truncated && st.stdout.as_deref().unwrap().ends_with("END\n"));
    assert!(st.stdout.as_deref().unwrap().len() <= 64 * 1024);
    assert_eq!(d.integration.status, IntegrationStatus::Ready, "{:?}", d.integration.blockers);
    f.c.call::<IntegrationsDeleteCommand>(IntegrationsDeleteCommandParams { command_id: loud })
        .await
        .unwrap();

    // A command that leaves non-ignored files does not count as passed.
    let messy = add_command(&f, "messy", &["/usr/bin/touch", "generated.txt"], 10).await;
    validate(auth(&f, &[messy]).await).await.unwrap();
    let d = wait_run(&f, i.id).await;
    assert_eq!(d.runs[0].status, RunStatus::Failed);
    assert!(d.integration.status_reason.as_deref().unwrap().contains("changed the workspace"));
    std::fs::remove_file(f.ws.join("generated.txt")).unwrap();
    f.c.call::<IntegrationsDeleteCommand>(IntegrationsDeleteCommandParams { command_id: messy })
        .await
        .unwrap();

    // A passing run becomes stale when the source changes.
    let ok = add_command(&f, "ok", &["/usr/bin/true"], 10).await;
    validate(auth(&f, &[ok]).await).await.unwrap();
    let d = wait_run(&f, i.id).await;
    assert_eq!(d.integration.status, IntegrationStatus::Ready);
    std::fs::write(f.ws.join("b.txt"), "b\n").unwrap();
    let after = commit_ws(&f, i.id, "b").await.unwrap();
    assert_eq!(codes(&after), vec!["validation_required"], "the passed run was for another commit");
}

/// Two approvals at once integrate once.
#[tokio::test]
async fn concurrent_approvals_integrate_once() {
    let f = fixture().await;
    no_review(&f).await;
    std::fs::write(f.ws.join("a.txt"), "a\n").unwrap();
    let i = create(&f, None).await;
    let i = commit_ws(&f, i.id, "a").await.unwrap();
    let (a, b) = tokio::join!(approve(&f, &i), approve(&f, &i));
    let oks = [&a, &b].iter().filter(|r| r.is_ok()).count();
    assert_eq!(oks, 1, "{a:?} / {b:?}");
    let merges = git(&f.repo, &["rev-list", "--merges", "--count", "main"]);
    assert_eq!(merges, "1");
}

/// After a restart, an interrupted integration is settled from the target branch.
#[tokio::test]
async fn an_interrupted_integration_is_settled_after_a_restart() {
    let mut f = fixture().await;
    no_review(&f).await;
    std::fs::write(f.ws.join("a.txt"), "a\n").unwrap();
    let i = create(&f, None).await;
    let i = commit_ws(&f, i.id, "a").await.unwrap();
    let target = i.target_commit.clone();
    // Build the merge commit Core would have created.
    let tree = git(&f.ws, &["rev-parse", "HEAD^{tree}"]);
    let merge = git(&f.repo, &["commit-tree", &tree, "-p", &target, "-p", &i.source_commit, "-m", "Merge"]);
    f.core.stop().await;
    let db = f.core.dir.path().join("data/switchyard.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    // Crash after the branch moved: the branch is at the merge commit.
    conn.execute(
        "UPDATE integrations SET status = 'integrating', merge_commit = ?1, method = 'ref_update' WHERE id = ?2",
        rusqlite::params![merge, i.id.to_string()],
    )
    .unwrap();
    drop(conn);
    git(&f.repo, &["switch", "-q", "-c", "parking"]);
    git(&f.repo, &["update-ref", "refs/heads/main", &merge, &target]);
    // Someone committed on top of the merge while Core was down: it still landed.
    let on_top = git(&f.repo, &["commit-tree", &tree, "-p", &merge, "-m", "later work"]);
    git(&f.repo, &["update-ref", "refs/heads/main", &on_top, &merge]);
    f.core.restart_with(|_| {}).await;
    let (c, _rx) = f.core.connect().await;
    f.c = c;
    let d = get(&f, i.id).await.integration;
    assert_eq!(d.status, IntegrationStatus::Integrated, "{:?}", d.status_reason);
    assert!(events_of(&f, i.id).await.contains(&"integration.integrated".to_owned()));

    // A second integration interrupted before the branch moved fails, and nothing changed.
    std::fs::write(f.ws.join("b.txt"), "b\n").unwrap();
    let j = create(&f, None).await;
    let j = commit_ws(&f, j.id, "b").await.unwrap();
    let before = git(&f.repo, &["rev-parse", "main"]);
    f.core.stop().await;
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute(
        "UPDATE integrations SET status = 'integrating', merge_commit = ?1, method = 'ref_update' WHERE id = ?2",
        rusqlite::params!["0".repeat(40), j.id.to_string()],
    )
    .unwrap();
    drop(conn);
    f.core.restart_with(|_| {}).await;
    let (c, _rx) = f.core.connect().await;
    f.c = c;
    let d = get(&f, j.id).await.integration;
    assert_eq!(d.status, IntegrationStatus::Failed);
    assert!(d.failure.as_deref().unwrap().contains("nothing changed"), "{:?}", d.failure);
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), before);
    // It can be prepared again.
    assert_eq!(refresh(&f, j.id).await.status, IntegrationStatus::Ready);

    // Interrupted while fast-forwarding a checkout: the branch did not move,
    // but the checkout's files may have, and the failure says so.
    f.core.stop().await;
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute(
        "UPDATE integrations SET status = 'integrating', merge_commit = ?1, method = 'checkout_fast_forward' \
         WHERE id = ?2",
        rusqlite::params!["0".repeat(40), j.id.to_string()],
    )
    .unwrap();
    drop(conn);
    f.core.restart_with(|_| {}).await;
    let (c, _rx) = f.core.connect().await;
    f.c = c;
    let d = get(&f, j.id).await.integration;
    assert_eq!(d.status, IntegrationStatus::Failed);
    assert!(d.failure.as_deref().unwrap().contains("git status"), "{:?}", d.failure);
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), before);
}

/// The full M6 → M7 path with the fake agents: an implementation reviewed by
/// another agent is integrated only after the operator approves; a review
/// verdict or a report never integrates anything, and the task's own status
/// is not changed by delivery.
#[tokio::test]
async fn a_reviewed_task_is_delivered_only_on_the_operators_approval() {
    let f = fixture().await;
    let spec = json!({
        "implement": { "tool": "write:feature.txt", "report": {
            "type": "implementation", "summary": "Added feature.txt.", "integrate": true, "approve_integration": true } },
        "review": { "report": { "type": "review", "summary": "Good.", "result": "approved" } }
    });
    let hex: String = spec.to_string().bytes().map(|b| format!("{b:02x}")).collect();
    let task =
        f.c.call::<TasksCreate>(TasksCreateParams {
            project_id: f.project_id,
            title: "Feature".into(),
            description: Some(format!("[fake:report:{hex}]")),
            kind: None,
            priority: None,
            acceptance_criteria: vec![],
            depends_on: vec![],
            parent_task_id: None,
            publish: true,
        })
        .await
        .unwrap()
        .task;
    let assign = |role| TasksAssignParams {
        task_id: task.id,
        role,
        provider: "claude_code".into(),
        workspace_id: f.workspace_id,
        start: true,
    };
    f.c.call::<TasksAssign>(assign(Role::Implement)).await.unwrap();
    // Approve the agent's file write (an M4 approval, unrelated to integration).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let pending =
            f.c.call::<ApprovalsList>(ApprovalsListParams {
                status: Some(ApprovalStatus::Pending),
                ..Default::default()
            })
            .await
            .unwrap()
            .approvals;
        if let Some(a) = pending.first() {
            f.c.call::<ApprovalsDecide>(ApprovalsDecideParams {
                approval_id: a.id,
                decision: ApprovalDecision::Approve,
                note: None,
            })
            .await
            .unwrap();
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (fr, tid) = (&f, task.id);
    let wait_task = move |status: TaskStatus| async move {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            let t = fr.c.call::<TasksGet>(TaskIdParams { task_id: tid }).await.unwrap();
            if t.task.status == status && t.task.open_assignment.is_none() {
                return t;
            }
            assert!(tokio::time::Instant::now() < deadline, "task is {}", t.task.status);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    wait_task(TaskStatus::AwaitingReview).await;
    // The report's "integrate" claims created and approved nothing.
    let none =
        f.c.call::<IntegrationsList>(IntegrationsListParams {
            project_id: Some(f.project_id),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(none.integrations.is_empty());

    // Integration before any review: blocked by the review requirement.
    let i = create(&f, Some(task.id)).await;
    assert!(codes(&i).contains(&"review_required".to_owned()), "{:?}", codes(&i));
    f.c.call::<IntegrationsCancel>(IntegrationsCancelParams { integration_id: i.id, reason: None })
        .await
        .unwrap();

    // The independent review (read-only session) approves; still nothing integrates by itself.
    f.c.call::<TasksAssign>(assign(Role::Review)).await.unwrap();
    let t = wait_task(TaskStatus::AwaitingReview).await;
    assert_eq!(t.task.status_reason.as_deref(), Some("review: approved"));
    assert_eq!(git(&f.repo, &["rev-list", "--count", "main"]), "2", "the target is untouched");
    // The reviewer's session has ended before Core changes the workspace.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while f
        .c
        .call::<SessionsList>(SessionsListParams {
            workspace_id: Some(f.workspace_id),
            live_only: true,
            ..Default::default()
        })
        .await
        .unwrap()
        .sessions
        .iter()
        .any(|s| s.state != SessionState::Stopped)
    {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let i = create(&f, Some(task.id)).await;
    assert_eq!(codes(&i), vec!["uncommitted_changes", "no_changes", "review_required"], "{:?}", i.blockers);
    assert!(i.blockers[2].message.contains("commit the workspace"), "{}", i.blockers[2].message);
    let i = commit_ws(&f, i.id, "Add feature.txt").await.unwrap();
    assert!(i.blockers.is_empty(), "the reviewed content is exactly the source commit: {:?}", i.blockers);
    assert_eq!(i.status, IntegrationStatus::Ready);
    let done = approve(&f, &i).await.unwrap();
    assert_eq!(done.status, IntegrationStatus::Integrated);
    assert!(f.repo.join("feature.txt").exists());
    // Delivery is not task completion.
    let t = f.c.call::<TasksGet>(TaskIdParams { task_id: task.id }).await.unwrap();
    assert_eq!(t.task.status, TaskStatus::AwaitingReview);
    let delivered =
        f.c.call::<IntegrationsList>(IntegrationsListParams {
            task_id: Some(task.id),
            include_closed: true,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(delivered.integrations.iter().any(|x| x.status == IntegrationStatus::Integrated));

    // A change after the review invalidates the review binding.
    std::fs::write(f.ws.join("after-review.txt"), "x\n").unwrap();
    let j = create(&f, Some(task.id)).await;
    let j = commit_ws(&f, j.id, "after review").await.unwrap();
    assert!(codes(&j).contains(&"review_required".to_owned()), "{:?}", j.blockers);
}

/// Cross-project and invalid requests are refused.
#[tokio::test]
async fn cross_project_and_invalid_requests_are_refused() {
    let f = fixture().await;
    let other = git_repo(f.core.dir.path(), "other-repo");
    let p2 =
        f.c.call::<ProjectsAdd>(ProjectsAddParams { path: other.display().to_string() })
            .await
            .unwrap()
            .project;
    let task =
        f.c.call::<TasksCreate>(TasksCreateParams {
            project_id: p2.id,
            title: "Elsewhere".into(),
            description: None,
            kind: None,
            priority: None,
            acceptance_criteria: vec![],
            depends_on: vec![],
            parent_task_id: None,
            publish: true,
        })
        .await
        .unwrap()
        .task;
    let err =
        f.c.call::<IntegrationsCreate>(IntegrationsCreateParams {
            task_id: Some(task.id),
            workspace_id: Some(f.workspace_id),
            target_branch: None,
        })
        .await
        .unwrap_err();
    assert_eq!(code_of(&err), Some(ErrorCode::InvalidParams), "{err:?}");
    for target in ["--force", "refs/heads/main", "switchyard/x", "main~1", "main^", "HEAD@{1}"] {
        let err =
            f.c.call::<IntegrationsCreate>(IntegrationsCreateParams {
                task_id: None,
                workspace_id: Some(f.workspace_id),
                target_branch: Some(target.into()),
            })
            .await
            .unwrap_err();
        assert_eq!(code_of(&err), Some(ErrorCode::InvalidParams), "{target}: {err:?}");
    }
    // One unfinished integration per workspace.
    create(&f, None).await;
    let dup =
        f.c.call::<IntegrationsCreate>(IntegrationsCreateParams {
            task_id: None,
            workspace_id: Some(f.workspace_id),
            target_branch: None,
        })
        .await
        .unwrap_err();
    assert_eq!(code_of(&dup), Some(ErrorCode::Conflict));
}

/// Editing a command invalidates both a passed run and an earlier authorization.
#[tokio::test]
async fn edited_commands_invalidate_passed_runs_and_authorizations() {
    let f = fixture().await;
    no_review(&f).await;
    std::fs::write(f.ws.join("a.txt"), "a\n").unwrap();
    let i = create(&f, None).await;
    let i = commit_ws(&f, i.id, "a").await.unwrap();
    let cmd = add_command(&f, "check", &["/bin/test", "-f", "a.txt"], 10).await;
    let before = auth(&f, &[cmd]).await;
    f.c.call::<IntegrationsValidate>(IntegrationsValidateParams {
        integration_id: i.id,
        authorize: before.clone(),
    })
    .await
    .unwrap();
    assert_eq!(wait_run(&f, i.id).await.integration.status, IntegrationStatus::Ready);
    // The operator edits the command: the passed run no longer counts.
    f.c.call::<IntegrationsSetCommand>(IntegrationsSetCommandParams {
        project_id: f.project_id,
        command_id: Some(cmd),
        name: "check".into(),
        argv: vec!["/bin/test".into(), "-f".into(), "missing.txt".into()],
        timeout_secs: 10,
        enabled: true,
        position: None,
    })
    .await
    .unwrap();
    assert_eq!(codes(&refresh(&f, i.id).await), vec!["validation_required"]);
    // The old authorization (for the old command line) does not authorize the new one.
    let r = f
        .c
        .call::<IntegrationsValidate>(IntegrationsValidateParams { integration_id: i.id, authorize: before })
        .await
        .unwrap();
    assert_eq!((r.run.status, r.run.steps[0].status), (RunStatus::Failed, StepStatus::Refused));
    // A new policy rule also changes what a run is valid for.
    let fresh = auth(&f, &[cmd]).await;
    f.c.call::<IntegrationsValidate>(IntegrationsValidateParams { integration_id: i.id, authorize: fresh })
        .await
        .unwrap();
    assert_eq!(
        wait_run(&f, i.id).await.runs[0].steps[0].status,
        StepStatus::Failed,
        "missing.txt does not exist"
    );
}

/// A command whose child ignores SIGTERM cannot keep a stopped run alive.
#[tokio::test]
async fn a_stopped_command_cannot_hang_the_validation() {
    let f = fixture().await;
    no_review(&f).await;
    std::fs::write(f.ws.join("a.txt"), "a\n").unwrap();
    let i = create(&f, None).await;
    let i = commit_ws(&f, i.id, "a").await.unwrap();
    let stubborn = add_command(
        &f,
        "stubborn",
        &["/bin/sh", "-c", "(trap '' TERM; exec /bin/sleep 25) & exec /bin/sleep 25"],
        1,
    )
    .await;
    let started = std::time::Instant::now();
    f.c.call::<IntegrationsValidate>(IntegrationsValidateParams {
        integration_id: i.id,
        authorize: auth(&f, &[stubborn]).await,
    })
    .await
    .unwrap();
    let d = wait_run(&f, i.id).await;
    assert_eq!(d.runs[0].steps[0].status, StepStatus::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(15), "the run ended promptly: {:?}", started.elapsed());
    // The workspace is usable again (no longer `integrating`).
    let w =
        f.c.call::<WorkspacesGet>(WorkspaceIdParams { workspace_id: f.workspace_id })
            .await
            .unwrap()
            .workspace;
    assert_ne!(w.status, switchyard_protocol::model::WorkspaceStatus::Integrating);
}

/// An integration step keeps the operator's `completed` mark on the workspace.
#[tokio::test]
async fn a_completed_workspace_stays_completed_after_an_integration_step() {
    let f = fixture().await;
    no_review(&f).await;
    std::fs::write(f.ws.join("a.txt"), "a\n").unwrap();
    f.c.call::<WorkspacesComplete>(WorkspaceIdParams { workspace_id: f.workspace_id }).await.unwrap();
    let i = create(&f, None).await;
    commit_ws(&f, i.id, "a").await.unwrap();
    let w =
        f.c.call::<WorkspacesGet>(WorkspaceIdParams { workspace_id: f.workspace_id })
            .await
            .unwrap()
            .workspace;
    assert_eq!(w.status, switchyard_protocol::model::WorkspaceStatus::Completed);
}
