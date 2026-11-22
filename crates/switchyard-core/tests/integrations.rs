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
    /// The Git wrapper's hook directory, for fixtures made by [`fixture_hooked`].
    hooks: Option<tempfile::TempDir>,
}

/// A project (main checked out in the project checkout) with one workspace.
/// The repository has its own identity: Core's commits never depend on the
/// machine's Git configuration.
async fn fixture() -> Fx {
    fixture_with(|_| {}, None).await
}

/// A Git wrapper that runs the hooks in `dir` (files `hook-*`: first line a
/// pattern matched against the arguments, the rest a shell action) before
/// the real Git. A hook exiting with 99 makes that Git call fail (exit 128)
/// without running Git. Without hook files it is transparent.
fn git_wrapper(dir: &Path) -> PathBuf {
    let real = String::from_utf8(Command::new("sh").args(["-c", "command -v git"]).output().unwrap().stdout)
        .unwrap()
        .trim()
        .to_owned();
    let path = dir.join("git");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nfor f in '{d}'/hook-*; do\n  [ -f \"$f\" ] || continue\n  pat=$(head -n 1 \"$f\")\n  \
             case \"$*\" in *\"$pat\"*) sh \"$f.sh\"; [ $? -eq 99 ] && exit 128 ;; esac\ndone\nexec '{real}' \"$@\"\n",
            d = dir.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    path
}

/// [`fixture`] with Core's Git going through [`git_wrapper`], and `configure`.
async fn fixture_hooked(configure: impl FnOnce(&mut switchyard_core::CoreConfig)) -> Fx {
    let hooks = tempfile::Builder::new().prefix("syh").tempdir().unwrap();
    let wrapper = git_wrapper(hooks.path());
    fixture_with(
        move |c| {
            c.git.binary = wrapper;
            configure(c);
        },
        Some(hooks),
    )
    .await
}

impl Fx {
    /// Runs `action` (shell) before every Git call of Core whose arguments contain `pattern`.
    fn hook(&self, name: &str, pattern: &str, action: &str) {
        let dir = self.hooks.as_ref().unwrap().path();
        std::fs::write(dir.join(format!("hook-{name}")), format!("{pattern}\n")).unwrap();
        std::fs::write(dir.join(format!("hook-{name}.sh")), action).unwrap();
    }

    fn unhook(&self, name: &str) {
        let dir = self.hooks.as_ref().unwrap().path();
        let _ = std::fs::remove_file(dir.join(format!("hook-{name}")));
        let _ = std::fs::remove_file(dir.join(format!("hook-{name}.sh")));
    }

    fn db(&self) -> rusqlite::Connection {
        let c = rusqlite::Connection::open(self.core.dir.path().join("data/switchyard.db")).unwrap();
        c.busy_timeout(Duration::from_secs(10)).unwrap();
        c
    }
}

async fn fixture_with(
    configure: impl FnOnce(&mut switchyard_core::CoreConfig),
    hooks: Option<tempfile::TempDir>,
) -> Fx {
    let core = TestCore::start_with(configure).await;
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
    Fx { core, c, repo, project_id: project.id, workspace_id: w.id, ws: PathBuf::from(w.path), hooks }
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
    assert!(d.failure.as_deref().unwrap().contains("it was not moved"), "{:?}", d.failure);
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
        model: None,
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
/// With the project's auto mode on (ADR 0037), a validation command that
/// would ask runs without authorization; a block rule still refuses one.
#[tokio::test]
async fn auto_mode_runs_validation_commands_without_authorization() {
    let f = fixture().await;
    no_review(&f).await;
    std::fs::write(f.ws.join("a.txt"), "a\n").unwrap();
    let i = create(&f, None).await;
    let i = commit_ws(&f, i.id, "a").await.unwrap();
    f.c.call::<PoliciesSetAutoMode>(PoliciesSetAutoModeParams {
        project_id: f.project_id,
        enabled: true,
        expected_revision: 0,
    })
    .await
    .unwrap();
    // Quoted arguments always ask without auto mode (M4 grammar).
    let quoted = add_command(&f, "quoted", &["/bin/sh", "-c", "echo ok"], 10).await;
    f.c.call::<IntegrationsValidate>(IntegrationsValidateParams { integration_id: i.id, authorize: vec![] })
        .await
        .unwrap();
    let d = wait_run(&f, i.id).await;
    let step = &d.runs[0].steps[0];
    assert_eq!((step.status, step.policy_decision.as_str()), (StepStatus::Passed, "allow"));
    assert_eq!(step.authorized_by.as_deref(), Some("policy"));
    assert!(step.policy_explanation.contains("Auto mode"), "{}", step.policy_explanation);

    // A block rule still wins.
    f.c.call::<IntegrationsDeleteCommand>(IntegrationsDeleteCommandParams { command_id: quoted })
        .await
        .unwrap();
    add_command(&f, "reset", &["git", "reset", "--hard"], 10).await;
    shell_rule(&f, "git reset --hard", PolicyDecision::Deny).await;
    let r = f
        .c
        .call::<IntegrationsValidate>(IntegrationsValidateParams { integration_id: i.id, authorize: vec![] })
        .await
        .unwrap();
    assert_eq!(r.run.steps[0].status, StepStatus::Refused);
}

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

// --- Core-owned operations (ADR 0032) ---------------------------------------------

/// Polls the integration until `done` holds (30 s at most).
async fn wait_for(f: &Fx, id: Uuid, what: &str, done: impl Fn(&Integration) -> bool) -> Integration {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let i = get(f, id).await.integration;
        if done(&i) {
            return i;
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for {what}: {i:#?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn op_is(i: &Integration, kind: &str, status: &str) -> bool {
    i.operation.as_ref().is_some_and(|o| o.kind == kind && o.status == status)
}

async fn workspace_status(f: &Fx) -> switchyard_protocol::model::WorkspaceStatus {
    f.c.call::<WorkspacesGet>(WorkspaceIdParams { workspace_id: f.workspace_id })
        .await
        .unwrap()
        .workspace
        .status
}

/// A ready integration of one committed file, with `main` checked out in the project checkout.
async fn ready_integration(f: &Fx) -> Integration {
    no_review(f).await;
    std::fs::write(f.ws.join("a.txt"), "a\n").unwrap();
    let i = create(f, None).await;
    let i = commit_ws(f, i.id, "a").await.unwrap();
    assert_eq!(i.status, IntegrationStatus::Ready, "{:?}", i.blockers);
    i
}

/// Sends `approve` on a separate connection and drops that connection while
/// Core is still working on it.
async fn approve_and_disconnect(f: &Fx, i: &Integration) {
    let (c2, _rx) = f.core.connect().await;
    let p = IntegrationsApproveParams {
        integration_id: i.id,
        source_commit: i.source_commit.clone(),
        target_commit: i.target_commit.clone(),
    };
    let request = tokio::spawn(async move { c2.call::<IntegrationsApprove>(p).await });
    wait_for(f, i.id, "the approve operation to run", |x| op_is(x, "approve", "running")).await;
    request.abort();
    let _ = request.await;
}

#[tokio::test]
async fn an_approval_completes_after_its_client_disconnects() {
    let f = fixture_hooked(|_| {}).await;
    let i = ready_integration(&f).await;
    let target = i.target_commit.clone();
    f.hook("ff", "merge --ff-only", "sleep 3");
    approve_and_disconnect(&f, &i).await;

    // A client reconnecting meanwhile sees the operation running.
    let (c3, _rx) = f.core.connect().await;
    let now = c3.call::<IntegrationsGet>(IntegrationIdParams { integration_id: i.id }).await.unwrap();
    assert!(op_is(&now.integration, "approve", "running"), "{:?}", now.integration.operation);
    // A repeated approval while it runs is refused, not executed twice.
    let again = approve(&f, &i).await.unwrap_err();
    assert_eq!(code_of(&again), Some(ErrorCode::Conflict), "{again:?}");

    let done = wait_for(&f, i.id, "the integration to finish", |x| {
        x.status.is_terminal() || x.status == IntegrationStatus::Failed
    })
    .await;
    assert_eq!(done.status, IntegrationStatus::Integrated, "{:?}", done.status_reason);
    assert!(op_is(&done, "approve", "completed"));
    let merge = done.merge_commit.clone().unwrap();
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), merge);
    assert_eq!(git(&f.repo, &["rev-parse", "main^1"]), target);
    // After completion a retry is refused too; exactly one merge commit and one integrated event.
    let again = approve(&f, &i).await.unwrap_err();
    assert_eq!(code_of(&again), Some(ErrorCode::Conflict), "{again:?}");
    let merges = git(
        &f.repo,
        &["log", "--merges", "--format=%H", &format!("--grep=Switchyard-Integration: {}", i.id), "main"],
    );
    assert_eq!(merges.lines().count(), 1, "{merges}");
    let events = events_of(&f, i.id).await;
    assert_eq!(events.iter().filter(|e| *e == "integration.integrated").count(), 1);
    assert_eq!(events.iter().filter(|e| *e == "integration.operation_finished").count(), 2, "{events:?}");
    assert_ne!(workspace_status(&f).await, switchyard_protocol::model::WorkspaceStatus::Integrating);
}

#[tokio::test]
async fn a_workspace_update_completes_after_its_client_disconnects() {
    let f = fixture_hooked(|_| {}).await;
    no_review(&f).await;
    std::fs::write(f.ws.join("a.txt"), "a\n").unwrap();
    let i = create(&f, None).await;
    commit_ws(&f, i.id, "a").await.unwrap();
    std::fs::write(f.repo.join("main.txt"), "m\n").unwrap();
    let target = commit(&f.repo, "main moves");
    let i = refresh(&f, i.id).await;
    assert!(codes(&i).contains(&"out_of_date".to_owned()), "{:?}", codes(&i));
    f.hook("merge", "merge --no-ff", "sleep 3");
    let (c2, _rx) = f.core.connect().await;
    let request = tokio::spawn(async move {
        c2.call::<IntegrationsUpdateFromTarget>(IntegrationIdParams { integration_id: i.id }).await
    });
    wait_for(&f, i.id, "the update to run", |x| op_is(x, "update_from_target", "running")).await;
    // Disconnect once Core is inside the Git step (the workspace is locked).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while workspace_status(&f).await != switchyard_protocol::model::WorkspaceStatus::Integrating {
        assert!(tokio::time::Instant::now() < deadline, "the update never reached its Git step");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    request.abort();
    let _ = request.await;
    let done =
        wait_for(&f, i.id, "the update to finish", |x| op_is(x, "update_from_target", "completed")).await;
    assert_eq!(done.status, IntegrationStatus::Ready, "{:?}", done.blockers);
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(&f.ws)
            .args(["merge-base", "--is-ancestor", &target, "HEAD"])
            .status()
            .unwrap()
            .success()
    );
    assert_ne!(workspace_status(&f).await, switchyard_protocol::model::WorkspaceStatus::Integrating);
}

#[tokio::test]
async fn shutdown_waits_for_a_running_approval() {
    let mut f = fixture_hooked(|_| {}).await;
    let i = ready_integration(&f).await;
    f.hook("ff", "merge --ff-only", "sleep 2");
    approve_and_disconnect(&f, &i).await;
    f.core.stop().await;
    let hooks = f.hooks.as_ref().unwrap().path().to_path_buf();
    f.core.restart_with(|c| c.git.binary = hooks.join("git")).await;
    let (c, _rx) = f.core.connect().await;
    f.c = c;
    let d = get(&f, i.id).await.integration;
    assert_eq!(d.status, IntegrationStatus::Integrated, "{:?}", d.status_reason);
    assert!(op_is(&d, "approve", "completed"), "{:?}", d.operation);
}

#[tokio::test]
async fn an_operation_stopped_at_shutdown_is_settled_from_git_at_the_next_start() {
    let short = |c: &mut switchyard_core::CoreConfig| c.operation_shutdown_grace = Duration::from_secs(1);
    let mut f = fixture_hooked(short).await;
    let i = ready_integration(&f).await;
    let target = i.target_commit.clone();
    f.hook("ff", "merge --ff-only", "sleep 30");
    approve_and_disconnect(&f, &i).await;
    // Stop Core while Git is updating the checkout (after `integrating` was recorded).
    wait_for(&f, i.id, "the branch update to start", |x| x.status == IntegrationStatus::Integrating).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let started = std::time::Instant::now();
    f.core.stop().await;
    assert!(started.elapsed() < Duration::from_secs(15), "shutdown was bounded: {:?}", started.elapsed());
    f.unhook("ff");
    let hooks = f.hooks.as_ref().unwrap().path().to_path_buf();
    f.core.restart_with(|c| c.git.binary = hooks.join("git")).await;
    let (c, _rx) = f.core.connect().await;
    f.c = c;
    let d = get(&f, i.id).await.integration;
    assert_eq!(d.status, IntegrationStatus::Failed, "{:?}", d.status_reason);
    assert!(op_is(&d, "approve", "interrupted"), "{:?}", d.operation);
    let failure = d.failure.unwrap();
    assert!(failure.contains("nothing was changed") && failure.contains("Checked afterwards"), "{failure}");
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), target);
    // The supported path: refresh, approve again.
    let i = refresh(&f, i.id).await;
    assert_eq!(i.status, IntegrationStatus::Ready, "{:?}", i.blockers);
    assert_eq!(approve(&f, &i).await.unwrap().status, IntegrationStatus::Integrated);
}

#[tokio::test]
async fn a_timed_out_fast_forward_is_reported_from_what_git_shows() {
    let short_git = |c: &mut switchyard_core::CoreConfig| {
        c.git.timeout = Duration::from_secs(5);
        c.git.checkout_timeout = Duration::from_secs(5);
    };
    let f = fixture_hooked(short_git).await;
    let i = ready_integration(&f).await;
    let target = i.target_commit.clone();
    // Stopped before Git wrote anything: checked, and reported as unchanged.
    f.hook("ff", "merge --ff-only", "sleep 30");
    approve_and_disconnect(&f, &i).await;
    let d = wait_for(&f, i.id, "the approval to end", |x| op_is(x, "approve", "completed")).await;
    assert_eq!(d.status, IntegrationStatus::Failed);
    let failure = d.failure.unwrap();
    assert!(failure.contains("timeout") && failure.contains("nothing was changed"), "{failure}");
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), target);
    assert!(op_is(&get(&f, i.id).await.integration, "approve", "completed"));

    // Stopped after a file was written (simulated): may be partially updated.
    let i = refresh(&f, i.id).await;
    f.hook(
        "ff",
        "merge --ff-only",
        &format!("printf 'half\\n' > '{}/shared.txt'; sleep 30", f.repo.display()),
    );
    approve_and_disconnect(&f, &i).await;
    let d = wait_for(&f, i.id, "the approval to end", |x| {
        x.status == IntegrationStatus::Failed && op_is(x, "approve", "completed")
    })
    .await;
    let failure = d.failure.unwrap();
    assert!(failure.contains("may be partially updated") && failure.contains("git status"), "{failure}");
    assert!(!failure.contains("nothing was changed"), "{failure}");
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), target, "the branch did not move");
    assert_eq!(std::fs::read_to_string(f.repo.join("shared.txt")).unwrap(), "half\n", "nothing was repaired");
}

#[tokio::test]
async fn locks_are_released_after_failed_operations() {
    let f = fixture_hooked(|_| {}).await;
    let i = ready_integration(&f).await;
    // The fast-forward is refused inside the operation: an untracked file of
    // the user's is where the update would write `a.txt`.
    std::fs::write(f.repo.join("a.txt"), "user's file\n").unwrap();
    let d = approve(&f, &i).await.unwrap();
    assert_eq!(d.status, IntegrationStatus::Failed, "{:?}", d.status_reason);
    assert!(d.failure.as_deref().unwrap().contains("nothing was changed"), "{:?}", d.failure);
    assert_eq!(std::fs::read_to_string(f.repo.join("a.txt")).unwrap(), "user's file\n");
    std::fs::remove_file(f.repo.join("a.txt")).unwrap();
    // A failing workspace step (markers left in a file) also releases everything.
    std::fs::write(f.ws.join("m.txt"), "<<<<<<< HEAD\nx\n=======\ny\n>>>>>>> other\n").unwrap();
    let err = commit_ws(&f, i.id, "markers").await.unwrap_err();
    assert!(format!("{err:?}").contains("conflict markers remain in m.txt"), "{err:?}");
    assert_ne!(workspace_status(&f).await, switchyard_protocol::model::WorkspaceStatus::Integrating);
    // The next operations on the same project and workspace go ahead.
    std::fs::remove_file(f.ws.join("m.txt")).unwrap();
    let i = refresh(&f, i.id).await;
    assert_eq!(i.status, IntegrationStatus::Ready, "{:?}", i.blockers);
    assert_eq!(approve(&f, &i).await.unwrap().status, IntegrationStatus::Integrated);
}

#[tokio::test]
async fn a_target_checked_out_after_preparation_is_not_moved_under_its_checkout() {
    let f = fixture_hooked(|_| {}).await;
    no_review(&f).await;
    git(&f.repo, &["branch", "release"]);
    std::fs::write(f.ws.join("a.txt"), "a\n").unwrap();
    let i =
        f.c.call::<IntegrationsCreate>(IntegrationsCreateParams {
            task_id: None,
            workspace_id: Some(f.workspace_id),
            target_branch: Some("release".into()),
        })
        .await
        .unwrap()
        .integration;
    let i = commit_ws(&f, i.id, "a").await.unwrap();
    let release = git(&f.repo, &["rev-parse", "release"]);
    // Between preparation and the branch update, someone checks `release` out.
    let side = f.core.dir.path().join("release-checkout");
    f.hook(
        "checkout",
        "commit-tree",
        &format!("git -C '{}' worktree add -q '{}' release", f.repo.display(), side.display()),
    );
    let d = approve(&f, &i).await.unwrap();
    assert_eq!(d.status, IntegrationStatus::Failed, "{:?}", d.status_reason);
    assert!(d.failure.unwrap().contains("checked out at"));
    assert_eq!(git(&f.repo, &["rev-parse", "release"]), release, "nothing moved under the checkout");
}

#[tokio::test]
async fn validation_fails_closed_when_its_results_cannot_be_recorded() {
    let f = fixture().await;
    no_review(&f).await;
    std::fs::write(f.ws.join("a.txt"), "a\n").unwrap();
    let i = create(&f, None).await;
    let i = commit_ws(&f, i.id, "a").await.unwrap();
    let ok = add_command(&f, "ok", &["/usr/bin/true"], 10).await;
    // 1. A step's success cannot be recorded.
    f.db()
        .execute_batch(
            "CREATE TRIGGER fail_step BEFORE UPDATE OF status ON validation_steps WHEN NEW.status = 'passed' \
             BEGIN SELECT RAISE(ABORT, 'injected step failure'); END;",
        )
        .unwrap();
    f.c.call::<IntegrationsValidate>(IntegrationsValidateParams {
        integration_id: i.id,
        authorize: auth(&f, &[ok]).await,
    })
    .await
    .unwrap();
    let d = wait_run(&f, i.id).await;
    assert_eq!(d.runs[0].status, RunStatus::Failed);
    assert_ne!(d.integration.status, IntegrationStatus::Ready);
    f.db().execute_batch("DROP TRIGGER fail_step;").unwrap();
    // 2. The run's `passed` cannot be recorded: it is recorded failed instead.
    f.db()
        .execute_batch(
            "CREATE TRIGGER fail_run BEFORE UPDATE OF status ON validation_runs WHEN NEW.status = 'passed' \
             BEGIN SELECT RAISE(ABORT, 'injected run failure'); END;",
        )
        .unwrap();
    f.c.call::<IntegrationsValidate>(IntegrationsValidateParams {
        integration_id: i.id,
        authorize: auth(&f, &[ok]).await,
    })
    .await
    .unwrap();
    let d = wait_run(&f, i.id).await;
    assert_eq!(d.runs[0].status, RunStatus::Failed);
    assert_eq!(d.runs[0].steps[0].status, StepStatus::Passed, "the command itself passed");
    assert_ne!(d.integration.status, IntegrationStatus::Ready);
    f.db().execute_batch("DROP TRIGGER fail_run;").unwrap();
    let passed: i64 = f
        .db()
        .query_row("SELECT COUNT(*) FROM validation_runs WHERE status = 'passed'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(passed, 0, "no run was ever recorded as passed");
    assert_ne!(workspace_status(&f).await, switchyard_protocol::model::WorkspaceStatus::Integrating);
    // With recording working again, the same commands pass.
    f.c.call::<IntegrationsValidate>(IntegrationsValidateParams {
        integration_id: i.id,
        authorize: auth(&f, &[ok]).await,
    })
    .await
    .unwrap();
    assert_eq!(wait_run(&f, i.id).await.integration.status, IntegrationStatus::Ready);
}

#[tokio::test]
async fn a_validation_cannot_start_when_its_configuration_cannot_be_read() {
    let f = fixture().await;
    no_review(&f).await;
    std::fs::write(f.ws.join("a.txt"), "a\n").unwrap();
    let i = create(&f, None).await;
    let i = commit_ws(&f, i.id, "a").await.unwrap();
    let ok = add_command(&f, "ok", &["/usr/bin/true"], 10).await;
    let authorize = auth(&f, &[ok]).await;
    f.db().execute_batch("ALTER TABLE validation_commands RENAME TO validation_commands_hidden;").unwrap();
    let err =
        f.c.call::<IntegrationsValidate>(IntegrationsValidateParams {
            integration_id: i.id,
            authorize: authorize.clone(),
        })
        .await
        .unwrap_err();
    f.db().execute_batch("ALTER TABLE validation_commands_hidden RENAME TO validation_commands;").unwrap();
    assert!(code_of(&err).is_some(), "{err:?}");
    let d = get(&f, i.id).await;
    assert!(d.runs.is_empty(), "no run was recorded");
    assert_ne!(d.integration.status, IntegrationStatus::Validating);
    assert!(op_is(&d.integration, "validate", "failed"), "{:?}", d.integration.operation);
    assert_ne!(workspace_status(&f).await, switchyard_protocol::model::WorkspaceStatus::Integrating);
}

#[tokio::test]
async fn runs_record_exactly_the_configuration_they_executed() {
    let f = fixture().await;
    no_review(&f).await;
    std::fs::write(f.ws.join("a.txt"), "a\n").unwrap();
    let i = create(&f, None).await;
    let i = commit_ws(&f, i.id, "a").await.unwrap();
    let cmd = add_command(&f, "check", &["/bin/test", "-f", "a.txt"], 10).await;
    // Edits race with validation starts.
    let (editor, _rx) = f.core.connect().await;
    let project_id = f.project_id;
    let edits = tokio::spawn(async move {
        for n in 0..40u32 {
            let _ = editor
                .call::<IntegrationsSetCommand>(IntegrationsSetCommandParams {
                    project_id,
                    command_id: Some(cmd),
                    name: "check".into(),
                    argv: vec![
                        "/bin/test".into(),
                        "-f".into(),
                        if n % 2 == 0 { "a.txt" } else { "shared.txt" }.into(),
                    ],
                    timeout_secs: 10 + n % 3,
                    enabled: true,
                    position: None,
                })
                .await;
        }
    });
    for _ in 0..6 {
        let s = f.c.call::<IntegrationsSettings>(IntegrationsSettingsParams { project_id }).await.unwrap();
        let c = &s.commands[0];
        let a = vec![ValidationAuthorization { command_id: c.id, command_line: c.command_line.clone() }];
        let _ = f
            .c
            .call::<IntegrationsValidate>(IntegrationsValidateParams { integration_id: i.id, authorize: a })
            .await;
        let _ = wait_run(&f, i.id).await;
    }
    edits.await.unwrap();
    // Every run's recorded configuration is exactly what its steps executed.
    let db = f.db();
    let mut stmt = db.prepare("SELECT id, config FROM validation_runs").unwrap();
    let runs: Vec<(String, String)> =
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect();
    assert!(!runs.is_empty());
    for (run, config) in runs {
        let mut st = db
            .prepare("SELECT command_id, command_line, timeout_secs, policy_decision FROM validation_steps WHERE run_id = ?1 ORDER BY position")
            .unwrap();
        let lines: Vec<String> = st
            .query_map([&run], |r| {
                Ok(format!(
                    "{}\t{}\t{}\t{}",
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?
                ))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(config, lines.join("\n"), "run {run}");
    }
}

#[tokio::test]
async fn timeout_and_policy_changes_make_a_passed_run_stale() {
    let f = fixture().await;
    no_review(&f).await;
    std::fs::write(f.ws.join("a.txt"), "a\n").unwrap();
    let i = create(&f, None).await;
    let i = commit_ws(&f, i.id, "a").await.unwrap();
    let cmd = add_command(&f, "check", &["/bin/test", "-f", "a.txt"], 10).await;
    let validate = |a: Vec<ValidationAuthorization>| {
        f.c.call::<IntegrationsValidate>(IntegrationsValidateParams { integration_id: i.id, authorize: a })
    };
    validate(auth(&f, &[cmd]).await).await.unwrap();
    assert_eq!(wait_run(&f, i.id).await.integration.status, IntegrationStatus::Ready);
    // Only the timeout changes: the settings change re-checks the integration by itself.
    f.c.call::<IntegrationsSetCommand>(IntegrationsSetCommandParams {
        project_id: f.project_id,
        command_id: Some(cmd),
        name: "check".into(),
        argv: vec!["/bin/test".into(), "-f".into(), "a.txt".into()],
        timeout_secs: 11,
        enabled: true,
        position: None,
    })
    .await
    .unwrap();
    assert_eq!(codes(&get(&f, i.id).await.integration), vec!["validation_required"]);
    validate(auth(&f, &[cmd]).await).await.unwrap();
    assert_eq!(wait_run(&f, i.id).await.integration.status, IntegrationStatus::Ready);
    // A policy rule changes the decision (ask → allow): the run was for "ask".
    shell_rule(&f, "/bin/test -f a.txt", PolicyDecision::Allow).await;
    assert_eq!(codes(&refresh(&f, i.id).await), vec!["validation_required"]);
}

#[tokio::test]
async fn a_failed_preparation_after_validation_does_not_strand_the_integration() {
    let mut f = fixture().await;
    no_review(&f).await;
    git(&f.repo, &["branch", "release"]);
    std::fs::write(f.ws.join("a.txt"), "a\n").unwrap();
    let i =
        f.c.call::<IntegrationsCreate>(IntegrationsCreateParams {
            task_id: None,
            workspace_id: Some(f.workspace_id),
            target_branch: Some("release".into()),
        })
        .await
        .unwrap()
        .integration;
    let i = commit_ws(&f, i.id, "a").await.unwrap();
    let release = git(&f.repo, &["rev-parse", "release"]);
    // The command deletes the target branch, so preparing afterwards fails.
    let repo = f.repo.display().to_string();
    let drop_branch =
        add_command(&f, "drop", &["/usr/bin/git", "-C", &repo, "update-ref", "-d", "refs/heads/release"], 60)
            .await;
    f.c.call::<IntegrationsValidate>(IntegrationsValidateParams {
        integration_id: i.id,
        authorize: auth(&f, &[drop_branch]).await,
    })
    .await
    .unwrap();
    let d = wait_for(&f, i.id, "validation to end", |x| x.status != IntegrationStatus::Validating).await;
    assert_eq!(d.status, IntegrationStatus::Failed, "{:?} {:#?}", d.status_reason, get(&f, i.id).await.runs);
    assert!(d.failure.unwrap().contains("could not check the integration again"));
    assert_ne!(workspace_status(&f).await, switchyard_protocol::model::WorkspaceStatus::Integrating);
    // Once the branch is back, a refresh is the way on.
    git(&f.repo, &["branch", "release", &release]);
    f.c.call::<IntegrationsDeleteCommand>(IntegrationsDeleteCommandParams { command_id: drop_branch })
        .await
        .unwrap();
    assert_eq!(refresh(&f, i.id).await.status, IntegrationStatus::Ready);

    // The same at startup: an interrupted validation whose target vanished.
    f.core.stop().await;
    let db = f.db();
    db.execute(
        "INSERT INTO validation_runs (id, integration_id, project_id, source_commit, status, requested_by, config, started_at) \
         VALUES (?3, ?1, ?2, 'x', 'running', 'test', '', 1)",
        rusqlite::params![i.id.to_string(), f.project_id.to_string(), Uuid::now_v7().to_string()],
    )
    .unwrap();
    db.execute("UPDATE integrations SET status = 'validating' WHERE id = ?1", [i.id.to_string()]).unwrap();
    drop(db);
    git(&f.repo, &["update-ref", "-d", "refs/heads/release"]);
    f.core.restart_with(|_| {}).await;
    let (c, _rx) = f.core.connect().await;
    f.c = c;
    let d = get(&f, i.id).await;
    assert_eq!(d.integration.status, IntegrationStatus::Failed, "{:?}", d.integration.status_reason);
    assert!(d.runs.iter().any(|r| r.status == RunStatus::Interrupted));
    git(&f.repo, &["branch", "release", &release]);
    assert_eq!(refresh(&f, i.id).await.status, IntegrationStatus::Ready);
}

#[tokio::test]
async fn settings_and_command_changes_are_journaled_with_their_change() {
    let f = fixture().await;
    let cmd = add_command(&f, "check", &["/usr/bin/true"], 10).await;
    no_review(&f).await;
    f.c.call::<IntegrationsDeleteCommand>(IntegrationsDeleteCommandParams { command_id: cmd }).await.unwrap();
    let db = f.db();
    let mut st = db
        .prepare("SELECT event_type, payload FROM events WHERE event_type LIKE 'integration.%_changed' ORDER BY seq")
        .unwrap();
    let rows: Vec<(String, serde_json::Value)> = st
        .query_map([], |r| Ok((r.get(0)?, serde_json::from_str(&r.get::<_, String>(1)?).unwrap())))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let kinds: Vec<&str> = rows.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(
        kinds,
        [
            "integration.validation_command_changed",
            "integration.settings_changed",
            "integration.validation_command_changed"
        ]
    );
    assert_eq!(rows[0].1["action"], "created");
    assert_eq!(rows[0].1["after"]["command_line"], "/usr/bin/true");
    assert_eq!(
        (rows[1].1["require_review_before"].clone(), rows[1].1["require_review_after"].clone()),
        (json!(true), json!(false))
    );
    assert!(rows[1].1["changed_by"].as_str().is_some_and(|s| !s.is_empty()));
    assert_eq!((rows[2].1["action"].clone(), rows[2].1["after"].clone()), (json!("deleted"), json!(null)));
    assert_eq!(rows[2].1["before"]["timeout_secs"], 10);
    // Atomic: if the event cannot be written, the change is not made either.
    db.execute_batch(
        "CREATE TRIGGER no_settings_event BEFORE INSERT ON events WHEN NEW.event_type = 'integration.settings_changed' \
         BEGIN SELECT RAISE(ABORT, 'injected'); END;",
    )
    .unwrap();
    let err =
        f.c.call::<IntegrationsSetSettings>(IntegrationsSetSettingsParams {
            project_id: f.project_id,
            require_review: true,
        })
        .await;
    assert!(err.is_err());
    let s =
        f.c.call::<IntegrationsSettings>(IntegrationsSettingsParams { project_id: f.project_id })
            .await
            .unwrap();
    assert!(!s.require_review, "the setting did not change without its event");
}

/// Stops Core, makes `i` look interrupted after its merge commit `merge` was
/// recorded (method `checkout_fast_forward`), and restarts Core.
async fn interrupt_after_intent(f: &mut Fx, i: &Integration, merge: &str, restart: bool) {
    f.core.stop().await;
    let db = f.db();
    db.execute(
        "UPDATE integrations SET status = 'integrating', merge_commit = ?1, method = 'checkout_fast_forward' \
         WHERE id = ?2",
        rusqlite::params![merge, i.id.to_string()],
    )
    .unwrap();
    drop(db);
    if restart {
        let hooks = f.hooks.as_ref().unwrap().path().to_path_buf();
        f.core.restart_with(|c| c.git.binary = hooks.join("git")).await;
        let (c, _rx) = f.core.connect().await;
        f.c = c;
    }
}

/// A merge commit for `i` (target, source), created but not on any branch.
fn merge_of(f: &Fx, i: &Integration) -> String {
    let tree = git(&f.repo, &["rev-parse", &format!("{}^{{tree}}", i.source_commit)]);
    git(&f.repo, &["commit-tree", &tree, "-p", &i.target_commit, "-p", &i.source_commit, "-m", "merge"])
}

#[tokio::test]
async fn recovery_never_guesses_when_git_cannot_be_read() {
    let mut f = fixture_hooked(|_| {}).await;
    let i = ready_integration(&f).await;
    let merge = merge_of(&f, &i);
    // The merge landed and someone committed on top while Core was down.
    git(&f.repo, &["merge", "-q", "--ff-only", &merge]);
    std::fs::write(f.repo.join("later.txt"), "l\n").unwrap();
    commit(&f.repo, "later");
    // At the next start the ancestry check fails.
    f.hook("ancestry", "merge-base --is-ancestor", "exit 99");
    interrupt_after_intent(&mut f, &i, &merge, true).await;
    let d = get(&f, i.id).await.integration;
    assert_eq!(d.status, IntegrationStatus::Integrating, "undecided, not failed: {:?}", d.failure);
    // Once Git answers, a refresh settles it: it did land.
    f.unhook("ancestry");
    assert_eq!(refresh(&f, i.id).await.status, IntegrationStatus::Integrated);
}

#[tokio::test]
async fn recovery_waits_while_a_git_process_may_still_be_writing() {
    let mut f = fixture_hooked(|_| {}).await;
    let i = ready_integration(&f).await;
    let merge = merge_of(&f, &i);
    interrupt_after_intent(&mut f, &i, &merge, false).await;
    std::fs::write(f.repo.join(".git/index.lock"), "").unwrap();
    let hooks = f.hooks.as_ref().unwrap().path().to_path_buf();
    f.core.restart_with(|c| c.git.binary = hooks.join("git")).await;
    let (c, _rx) = f.core.connect().await;
    f.c = c;
    let d = get(&f, i.id).await.integration;
    assert_eq!(d.status, IntegrationStatus::Integrating);
    assert!(d.status_reason.as_deref().unwrap().contains("index.lock"), "{:?}", d.status_reason);
    // The lock is gone (the process finished, or the operator removed a stale one).
    std::fs::remove_file(f.repo.join(".git/index.lock")).unwrap();
    let d = refresh(&f, i.id).await;
    assert_eq!(d.status, IntegrationStatus::Failed);
    let failure = d.failure.unwrap();
    assert!(failure.contains("Checked afterwards") && failure.contains("nothing was changed"), "{failure}");
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), i.target_commit);
}

/// An integration's result and the end of the operation that produced it are
/// one transaction (ADR 0032): if the operation's end cannot be recorded,
/// the result is not recorded either. Proven with an injected SQLite
/// failure, not timing; recovery then settles both from Git.
#[tokio::test]
async fn an_operations_end_and_its_result_commit_together() {
    let f = fixture().await;
    let i = ready_integration(&f).await;
    let block = "CREATE TRIGGER no_completed_operation BEFORE UPDATE OF status ON integration_operations \
                 WHEN NEW.status = 'completed' BEGIN SELECT RAISE(ABORT, 'injected'); END;";

    // Approval: Git moves the branch, but `integrated` cannot commit without
    // the operation's `completed`; the integration stays `integrating`.
    f.db().execute_batch(block).unwrap();
    let _ = approve(&f, &i).await;
    let d = get(&f, i.id).await.integration;
    assert_eq!(d.status, IntegrationStatus::Integrating, "integrated was not recorded alone");
    assert!(op_is(&d, "approve", "failed"), "{:?}", d.operation);
    let merge = d.merge_commit.clone().unwrap();
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), merge, "Git did move the branch");
    let events = events_of(&f, i.id).await;
    assert!(!events.contains(&"integration.integrated".to_owned()), "{events:?}");
    f.db().execute_batch("DROP TRIGGER no_completed_operation;").unwrap();
    // Recovery semantics unchanged: a refresh settles it from the target branch.
    let d = refresh(&f, i.id).await;
    assert_eq!(d.status, IntegrationStatus::Integrated, "{:?}", d.status_reason);
    assert_eq!(events_of(&f, i.id).await.iter().filter(|e| *e == "integration.integrated").count(), 1);
}

#[tokio::test]
async fn leaving_validating_and_ending_the_validation_commit_together() {
    let f = fixture().await;
    no_review(&f).await;
    std::fs::write(f.ws.join("a.txt"), "a\n").unwrap();
    let i = create(&f, None).await;
    let i = commit_ws(&f, i.id, "a").await.unwrap();
    let ok = add_command(&f, "ok", &["/usr/bin/true"], 10).await;
    f.db()
        .execute_batch(
            "CREATE TRIGGER no_completed_operation BEFORE UPDATE OF status ON integration_operations \
             WHEN NEW.status = 'completed' BEGIN SELECT RAISE(ABORT, 'injected'); END;",
        )
        .unwrap();
    f.c.call::<IntegrationsValidate>(IntegrationsValidateParams {
        integration_id: i.id,
        authorize: auth(&f, &[ok]).await,
    })
    .await
    .unwrap();
    let d = wait_for(&f, i.id, "the validation to end", |x| x.status != IntegrationStatus::Validating).await;
    // It could not leave `validating` as `ready` (that commit included the
    // operation's `completed`); it left as `failed` together with the
    // operation's `failed`, never with the operation still running.
    assert_eq!(d.status, IntegrationStatus::Failed, "{:?}", d.status_reason);
    assert!(op_is(&d, "validate", "failed"), "{:?}", d.operation);
    f.db().execute_batch("DROP TRIGGER no_completed_operation;").unwrap();
    assert_eq!(refresh(&f, i.id).await.status, IntegrationStatus::Ready, "the passed run still counts");
}

/// Supplementary to the transactional tests above: an observer polling
/// throughout an approval never sees a final status with the approval
/// operation still running, or `integrating` without an approval operation.
#[tokio::test]
async fn an_observer_never_sees_a_result_with_its_operation_running() {
    let f = fixture_hooked(|_| {}).await;
    let i = ready_integration(&f).await;
    f.hook("ff", "merge --ff-only", "sleep 1");
    let (observer, _rx) = f.core.connect().await;
    let id = i.id;
    let watch = tokio::spawn(async move {
        let mut seen = Vec::new();
        loop {
            let x = observer
                .call::<IntegrationsGet>(IntegrationIdParams { integration_id: id })
                .await
                .unwrap()
                .integration;
            let op = x.operation.as_ref().map(|o| (o.kind.clone(), o.status.clone()));
            let done =
                x.status == IntegrationStatus::Integrated && op.as_ref().is_some_and(|(_, s)| s != "running");
            seen.push((x.status, op));
            if done || seen.len() > 20_000 {
                return seen;
            }
        }
    });
    approve(&f, &i).await.unwrap();
    let seen = watch.await.unwrap();
    let integrating_seen = seen.iter().filter(|(s, _)| *s == IntegrationStatus::Integrating).count();
    println!("{} observations, {integrating_seen} while integrating", seen.len());
    for (status, op) in &seen {
        let running = op.as_ref().is_some_and(|(k, s)| k == "approve" && s == "running");
        match status {
            IntegrationStatus::Integrated | IntegrationStatus::Failed => {
                assert!(!running, "{status:?} observed with the approval still running");
            }
            IntegrationStatus::Integrating => {
                assert!(running, "integrating observed without its operation: {op:?}")
            }
            _ => {}
        }
    }
    assert!(integrating_seen > 0, "the observer saw the integration in progress");
}
