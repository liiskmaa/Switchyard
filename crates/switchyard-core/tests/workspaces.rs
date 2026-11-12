#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Workspace isolation end to end: real temporary Git repositories, real
//! worktrees, the fake Claude Code CLI, and Core restarts.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use switchyard_client::{ClientError, CoreClient};
use switchyard_protocol::methods::*;
use switchyard_protocol::model::{
    ApprovalStatus, CleanupReport, Session, SessionState, Workspace, WorkspaceKind, WorkspaceStatus,
};
use switchyard_protocol::{ErrorCode, EventEnvelope, ProtocolError};
use uuid::Uuid;

use common::{TestCore, run_git};

/// Worktrees go under a directory with a space and non-ASCII characters.
const WORKSPACES_DIR: &str = "work spaces ä";

fn configure(dir: &Path) -> impl FnOnce(&mut switchyard_core::CoreConfig) {
    let root = dir.join(WORKSPACES_DIR);
    move |c| c.paths.workspaces_dir = root
}

struct Fx {
    core: TestCore,
    client: CoreClient,
    repo: PathBuf,
    project_id: Uuid,
}

/// A repository whose path has spaces and non-ASCII characters, with an
/// ignore rule for `build/`.
fn repo(parent: &Path, name: &str) -> PathBuf {
    let root = parent.join(name);
    std::fs::create_dir_all(&root).unwrap();
    run_git(&root, &["init", "-q"]);
    std::fs::write(root.join("README.md"), "hello\n").unwrap();
    std::fs::write(root.join(".gitignore"), "build/\n").unwrap();
    run_git(&root, &["add", "."]);
    run_git(&root, &["commit", "-q", "-m", "initial"]);
    std::fs::canonicalize(root).unwrap()
}

async fn fx() -> Fx {
    fx_with(|_| {}).await
}

async fn fx_with(extra: impl FnOnce(&mut switchyard_core::CoreConfig)) -> Fx {
    let dir = common::data_dir();
    let mut config = common::agent_config(dir.path());
    configure(dir.path())(&mut config);
    extra(&mut config);
    let core = TestCore {
        core: Some(common::start_core(config, dir.path(), common::RealCli::default()).await),
        dir,
        real: common::RealCli::default(),
    };
    let (client, _rx) = core.connect().await;
    let repo = repo(core.dir.path(), "my repo ö");
    let project_id = add_project(&client, &repo).await;
    Fx { core, client, repo, project_id }
}

async fn add_project(client: &CoreClient, repo: &Path) -> Uuid {
    client
        .call::<ProjectsAdd>(ProjectsAddParams { path: repo.display().to_string() })
        .await
        .unwrap()
        .project
        .id
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn remote(err: ClientError) -> ProtocolError {
    match err {
        ClientError::Remote(e) => e,
        other => panic!("expected remote error, got {other:?}"),
    }
}

async fn create(f: &Fx, name: Option<&str>) -> Workspace {
    f.client
        .call::<WorkspacesCreate>(WorkspacesCreateParams {
            project_id: f.project_id,
            name: name.map(str::to_owned),
            base_ref: None,
        })
        .await
        .unwrap()
        .workspace
}

async fn get(f: &Fx, id: Uuid) -> Workspace {
    f.client.call::<WorkspacesGet>(WorkspaceIdParams { workspace_id: id }).await.unwrap().workspace
}

async fn start(f: &Fx, workspace_id: Uuid) -> Result<Session, ProtocolError> {
    f.client
        .call::<SessionsStart>(SessionsStartParams {
            workspace_id,
            project_id: Some(f.project_id),
            provider: None,
            prompt: None,
            model: None,
        })
        .await
        .map(|r| r.session)
        .map_err(remote)
}

async fn send(f: &Fx, session_id: Uuid, text: &str) {
    f.client
        .call::<SessionsSend>(SessionsSendParams { session_id, text: text.into(), queue_if_busy: false })
        .await
        .unwrap();
}

async fn stop(f: &Fx, session_id: Uuid) -> Session {
    f.client.call::<SessionsStop>(SessionIdParams { session_id }).await.unwrap().session
}

async fn approve_next(f: &Fx, session_id: Uuid) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let pending = f
            .client
            .call::<ApprovalsList>(ApprovalsListParams {
                status: Some(ApprovalStatus::Pending),
                session_id: Some(session_id),
                ..Default::default()
            })
            .await
            .unwrap()
            .approvals;
        if let Some(a) = pending.first() {
            f.client
                .call::<ApprovalsDecide>(ApprovalsDecideParams {
                    approval_id: a.id,
                    decision: ApprovalDecision::Approve,
                    note: None,
                })
                .await
                .unwrap();
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "no approval request");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

async fn events(f: &Fx, types: &[&str]) -> Vec<EventEnvelope> {
    f.client
        .call::<EventsList>(EventsListParams {
            project_id: Some(f.project_id),
            after_seq: Some(0),
            event_types: types.iter().map(|t| (*t).to_owned()).collect(),
            limit: Some(1000),
            ..Default::default()
        })
        .await
        .unwrap()
        .events
}

async fn wait_for(what: &str, mut cond: impl AsyncFnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !cond().await {
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

/// Runs SQL against the stopped Core's database (to simulate crashes at
/// precise points).
fn with_db(f: &Fx, sql: &str) {
    let path = common::config_for(f.core.dir.path()).paths.database;
    let storage = switchyard_storage::Storage::open(&path).unwrap();
    storage
        .with_conn(|c| {
            c.execute_batch(sql)?;
            Ok(())
        })
        .unwrap();
}

async fn restart(f: &mut Fx) {
    let dir = f.core.dir.path().to_path_buf();
    f.core.restart_with(configure(&dir)).await;
    f.client = f.core.connect().await.0;
}

// --- creation ------------------------------------------------------------------

#[tokio::test]
async fn creates_an_isolated_worktree_outside_the_repository() {
    let f = fx().await;
    let head = git_out(&f.repo, &["rev-parse", "HEAD"]);
    let w = create(&f, None).await;
    let root = std::fs::canonicalize(f.core.dir.path().join(WORKSPACES_DIR)).unwrap();
    let path = PathBuf::from(&w.path);

    assert_eq!(w.status, WorkspaceStatus::Ready);
    assert_eq!(w.kind, WorkspaceKind::Worktree);
    assert_eq!(w.name, "Workspace 1");
    assert_eq!(w.branch.as_deref(), Some(format!("switchyard/{}", w.id).as_str()));
    assert_eq!(w.base_commit.as_deref(), Some(head.as_str()));
    assert_eq!(w.base_ref.as_deref(), Some("HEAD"));
    assert_eq!(w.dirty, Some(false));
    assert!(w.path_exists && w.active_session_id.is_none());
    assert_eq!(path, root.join(f.project_id.to_string()).join(w.id.to_string()));
    assert!(!path.starts_with(&f.repo), "worktree is outside the repository");
    assert!(w.path.contains(' ') && w.path.contains('ä'));

    // Git agrees: a worktree of the project on the workspace branch.
    assert_eq!(git_out(&path, &["branch", "--show-current"]), format!("switchyard/{}", w.id));
    assert_eq!(
        std::fs::canonicalize(git_out(&path, &["rev-parse", "--path-format=absolute", "--git-common-dir"]))
            .unwrap(),
        f.repo.join(".git")
    );
    assert!(path.join("README.md").is_file());
    // The project checkout is untouched.
    assert_eq!(git_out(&f.repo, &["branch", "--show-current"]), "main");
    assert_eq!(git_out(&f.repo, &["status", "--porcelain"]), "");

    let status =
        f.client.call::<WorkspacesGitStatus>(WorkspaceIdParams { workspace_id: w.id }).await.unwrap();
    assert!(status.clean && status.on_expected_branch);
    assert_eq!(status.commits_since_base, Some(0));

    // A named workspace from another branch.
    run_git(&f.repo, &["branch", "feature-ü"]);
    let named = f
        .client
        .call::<WorkspacesCreate>(WorkspacesCreateParams {
            project_id: f.project_id,
            name: Some("  Fix ✓ login ".into()),
            base_ref: Some("feature-ü".into()),
        })
        .await
        .unwrap()
        .workspace;
    assert_eq!(named.name, "Fix ✓ login");
    assert_eq!(named.base_ref.as_deref(), Some("feature-ü"));

    let created = events(&f, &["workspace.created", "workspace.state_changed"]).await;
    let mine: Vec<_> = created.iter().filter(|e| e.context.workspace_id == Some(w.id)).collect();
    assert_eq!(mine[0].event_type, "workspace.created");
    assert_eq!(mine[0].payload["base_commit"], head.as_str());
    assert_eq!(
        (mine[1].payload["from"].as_str(), mine[1].payload["to"].as_str()),
        (Some("provisioning"), Some("ready"))
    );
}

#[tokio::test]
async fn invalid_creation_requests_are_rejected_with_typed_errors() {
    let f = fx().await;
    let bad_project = f
        .client
        .call::<WorkspacesCreate>(WorkspacesCreateParams {
            project_id: Uuid::nil(),
            name: None,
            base_ref: None,
        })
        .await
        .unwrap_err();
    assert_eq!(remote(bad_project).code, ErrorCode::ProjectNotFound);
    for base in ["--upload-pack=evil", "no-such-branch", "a b"] {
        let e = f
            .client
            .call::<WorkspacesCreate>(WorkspacesCreateParams {
                project_id: f.project_id,
                name: None,
                base_ref: Some(base.into()),
            })
            .await
            .unwrap_err();
        assert_eq!(remote(e).code, ErrorCode::InvalidParams, "{base}");
    }
    let e = f
        .client
        .call::<WorkspacesCreate>(WorkspacesCreateParams {
            project_id: f.project_id,
            name: Some("x\ny".into()),
            base_ref: None,
        })
        .await
        .unwrap_err();
    assert_eq!(remote(e).code, ErrorCode::InvalidParams);
    // Nothing was created by the rejected requests.
    let list = f
        .client
        .call::<WorkspacesList>(WorkspacesListParams { project_id: f.project_id, include_archived: true })
        .await
        .unwrap();
    assert!(list.workspaces.is_empty());
    assert_eq!(git_out(&f.repo, &["worktree", "list"]).lines().count(), 1);
}

#[tokio::test]
async fn concurrent_creation_yields_distinct_branches_and_paths() {
    let f = fx().await;
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..6 {
        let socket = f.core.socket();
        let project_id = f.project_id;
        tasks.spawn(async move {
            // Creation is serialized per repository (the repo lock: `git
            // worktree add` must not run concurrently on one repository), so
            // the last of six concurrent requests waits for the other five.
            // Measured: ~0.4 s per creation idle, 1.2 s+ while the rest of the
            // suite runs; the default 10 s test timeout then measured machine
            // load, not correctness. Use the product client's default (30 s).
            let mut options = switchyard_client::ClientOptions::new("integration-test", "0.0.0");
            options.request_timeout = std::time::Duration::from_secs(30);
            let (client, _rx) = switchyard_client::CoreClient::connect(&socket, options).await.unwrap();
            client
                .call::<WorkspacesCreate>(WorkspacesCreateParams { project_id, name: None, base_ref: None })
                .await
                .unwrap()
                .workspace
        });
    }
    let mut created = Vec::new();
    while let Some(w) = tasks.join_next().await {
        created.push(w.unwrap());
    }
    let ids: std::collections::HashSet<_> = created.iter().map(|w| w.id).collect();
    let paths: std::collections::HashSet<_> = created.iter().map(|w| w.path.clone()).collect();
    let branches: std::collections::HashSet<_> = created.iter().map(|w| w.branch.clone()).collect();
    assert_eq!((ids.len(), paths.len(), branches.len()), (6, 6, 6));
    assert!(created.iter().all(|w| w.status == WorkspaceStatus::Ready));
    assert_eq!(git_out(&f.repo, &["worktree", "list"]).lines().count(), 7);
    let names: std::collections::HashSet<_> = created.iter().map(|w| w.name.clone()).collect();
    assert_eq!(names.len(), 6, "default names are unique: {names:?}");
}

// --- concurrent sessions -----------------------------------------------------------

#[tokio::test]
async fn two_sessions_work_concurrently_in_separate_workspaces() {
    let f = fx().await;
    let (wa, wb) = (create(&f, Some("alpha")).await, create(&f, Some("beta")).await);
    let sa = start(&f, wa.id).await.unwrap();
    let sb = start(&f, wb.id).await.unwrap();
    // Both live at the same time, each in its own directory.
    assert!(sa.attached && sb.attached);
    assert_eq!((sa.cwd.as_str(), sb.cwd.as_str()), (wa.path.as_str(), wb.path.as_str()));
    assert_eq!((sa.workspace_id, sb.workspace_id), (Some(wa.id), Some(wb.id)));
    assert_eq!(sa.workspace_name.as_deref(), Some("alpha"));
    assert_eq!(get(&f, wa.id).await.status, WorkspaceStatus::Active);
    assert_eq!(get(&f, wa.id).await.active_session_id, Some(sa.id));
    let live = f
        .client
        .call::<SessionsList>(SessionsListParams {
            project_id: Some(f.project_id),
            live_only: true,
            ..Default::default()
        })
        .await
        .unwrap()
        .sessions;
    assert_eq!(live.len(), 2);

    send(&f, sa.id, "[fake:write:alpha.txt]").await;
    send(&f, sb.id, "[fake:write:beta.txt]").await;
    approve_next(&f, sa.id).await;
    approve_next(&f, sb.id).await;
    let (pa, pb) = (PathBuf::from(&wa.path), PathBuf::from(&wb.path));
    wait_for("both files", async || pa.join("alpha.txt").is_file() && pb.join("beta.txt").is_file()).await;
    assert!(!pa.join("beta.txt").exists() && !pb.join("alpha.txt").exists(), "no cross-workspace writes");
    assert!(!f.repo.join("alpha.txt").exists() && !f.repo.join("beta.txt").exists(), "project untouched");

    // File observations are attributed to the right workspace and session.
    wait_for("file events", async || {
        let evs = events(&f, &["files.changed"]).await;
        [(wa.id, sa.id, "alpha.txt"), (wb.id, sb.id, "beta.txt")].iter().all(|(w, s, file)| {
            evs.iter().any(|e| {
                e.context.workspace_id == Some(*w)
                    && e.context.session_id == Some(*s)
                    && e.payload["changes"].as_array().unwrap().iter().any(|c| c["path"] == *file)
            })
        })
    })
    .await;
    // The recorded Git observation follows file changes while the session
    // is still attached (found by the browser UI pass).
    let active = get(&f, wa.id).await;
    assert_eq!((active.status, active.dirty), (WorkspaceStatus::Active, Some(true)));
    let evs = events(&f, &["files.changed"]).await;
    for e in &evs {
        let paths: Vec<&str> =
            e.payload["changes"].as_array().unwrap().iter().map(|c| c["path"].as_str().unwrap()).collect();
        if e.context.workspace_id == Some(wa.id) {
            assert!(!paths.contains(&"beta.txt"), "{paths:?}");
        } else {
            assert!(!paths.contains(&"alpha.txt"), "{paths:?}");
        }
    }
    // Every agent event carries its own workspace.
    let agent = events(&f, &["agent.tool_completed", "agent.permission_requested", "agent.started"]).await;
    for e in agent {
        let expected = if e.context.session_id == Some(sa.id) { wa.id } else { wb.id };
        assert_eq!(e.context.workspace_id, Some(expected), "{}", e.event_type);
    }

    // Stopping a session keeps its workspace and its changes.
    let stopped = stop(&f, sa.id).await;
    assert_eq!(stopped.state, SessionState::Stopped);
    let after = get(&f, wa.id).await;
    assert_eq!(after.status, WorkspaceStatus::Dirty, "untracked alpha.txt");
    assert!(pa.join("alpha.txt").is_file());
    assert_eq!(get(&f, wb.id).await.status, WorkspaceStatus::Active, "the other session is unaffected");
    stop(&f, sb.id).await;
}

// --- validation -----------------------------------------------------------------

#[tokio::test]
async fn sessions_are_refused_for_invalid_workspaces() {
    let f = fx().await;
    // Unknown workspace.
    assert_eq!(start(&f, Uuid::nil()).await.unwrap_err().code, ErrorCode::WorkspaceNotFound);
    // A workspace of another project.
    let other_repo = repo(f.core.dir.path(), "other");
    let other_project = add_project(&f.client, &other_repo).await;
    let foreign = f
        .client
        .call::<WorkspacesCreate>(WorkspacesCreateParams {
            project_id: other_project,
            name: None,
            base_ref: None,
        })
        .await
        .unwrap()
        .workspace;
    assert_eq!(start(&f, foreign.id).await.unwrap_err().code, ErrorCode::WorkspaceProjectMismatch);

    // Branch changed externally.
    let w = create(&f, None).await;
    run_git(Path::new(&w.path), &["checkout", "-q", "-b", "elsewhere"]);
    let e = start(&f, w.id).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::WorktreeIdentityMismatch);
    let failed = get(&f, w.id).await;
    assert_eq!(failed.status, WorkspaceStatus::Failed);
    assert!(failed.status_reason.unwrap().starts_with("branch_mismatch"));
    // Explicit recovery after fixing it.
    run_git(Path::new(&w.path), &["checkout", "-q", w.branch.as_deref().unwrap()]);
    let verified =
        f.client.call::<WorkspacesVerify>(WorkspaceIdParams { workspace_id: w.id }).await.unwrap().workspace;
    assert_eq!(verified.status, WorkspaceStatus::Ready);
    assert!(start(&f, w.id).await.is_ok());

    // Two sessions can never share a workspace.
    assert_eq!(start(&f, w.id).await.unwrap_err().code, ErrorCode::WorkspaceOccupied);

    // A corrupted worktree (its .git link removed).
    let broken = create(&f, None).await;
    std::fs::remove_file(Path::new(&broken.path).join(".git")).unwrap();
    assert_eq!(start(&f, broken.id).await.unwrap_err().code, ErrorCode::WorktreeIdentityMismatch);
    assert!(get(&f, broken.id).await.status_reason.unwrap().starts_with("not_a_worktree"));
    assert!(Path::new(&broken.path).join("README.md").is_file(), "nothing was deleted");

    // A missing worktree.
    let gone = create(&f, None).await;
    std::fs::remove_dir_all(&gone.path).unwrap();
    assert_eq!(start(&f, gone.id).await.unwrap_err().code, ErrorCode::WorktreeMissing);
    assert!(get(&f, gone.id).await.status_reason.unwrap().starts_with("missing_worktree"));

    // An archived workspace.
    let archived = create(&f, None).await;
    f.client.call::<WorkspacesArchive>(WorkspaceIdParams { workspace_id: archived.id }).await.unwrap();
    assert_eq!(start(&f, archived.id).await.unwrap_err().code, ErrorCode::InvalidWorkspaceState);

    // No session ever fell back to the project checkout.
    let all = f
        .client
        .call::<SessionsList>(SessionsListParams { project_id: Some(f.project_id), ..Default::default() })
        .await
        .unwrap()
        .sessions;
    assert!(all.iter().all(|s| Path::new(&s.cwd) != f.repo));
}

// --- archive and cleanup -------------------------------------------------------------

async fn check_cleanup(f: &Fx, id: Uuid) -> CleanupReport {
    f.client.call::<WorkspacesCleanupCheck>(WorkspaceIdParams { workspace_id: id }).await.unwrap().report
}

async fn do_cleanup(f: &Fx, id: Uuid, confirmation: String) -> Result<WorkspaceCleanupResult, ClientError> {
    f.client.call::<WorkspacesCleanup>(WorkspacesCleanupParams { workspace_id: id, confirmation }).await
}

#[tokio::test]
async fn cleanup_preserves_user_work_and_requires_confirmation() {
    let f = fx().await;
    let w = create(&f, None).await;
    let keep = create(&f, Some("unrelated")).await;
    let path = PathBuf::from(&w.path);
    // A worktree the user made themselves, outside Switchyard.
    let user_wt = f.core.dir.path().join("user worktree");
    run_git(&f.repo, &["worktree", "add", "-q", "-b", "user-branch", user_wt.to_str().unwrap()]);

    // Only archived workspaces can be cleaned up.
    let r = check_cleanup(&f, w.id).await;
    assert!(!r.allowed && r.blockers.iter().any(|b| b.code == "not_archived"));

    std::fs::write(path.join("notes.txt"), "my work").unwrap();
    let archived =
        f.client.call::<WorkspacesArchive>(WorkspaceIdParams { workspace_id: w.id }).await.unwrap().workspace;
    assert_eq!(archived.status, WorkspaceStatus::Archived);
    assert!(path.join("notes.txt").is_file(), "archive deletes nothing");

    // Untracked files block cleanup.
    let r = check_cleanup(&f, w.id).await;
    assert!(r.blockers.iter().any(|b| b.code == "untracked_files"), "{r:?}");
    let e = remote(do_cleanup(&f, w.id, r.confirmation.clone()).await.unwrap_err());
    assert_eq!(e.code, ErrorCode::DirtyWorkspace);
    assert!(path.join("notes.txt").is_file());

    // Uncommitted changes block cleanup.
    std::fs::remove_file(path.join("notes.txt")).unwrap();
    std::fs::write(path.join("README.md"), "edited").unwrap();
    let r = check_cleanup(&f, w.id).await;
    assert!(r.blockers.iter().any(|b| b.code == "uncommitted_changes"), "{r:?}");
    assert_eq!(
        remote(do_cleanup(&f, w.id, r.confirmation.clone()).await.unwrap_err()).code,
        ErrorCode::DirtyWorkspace
    );

    // Commit the work on the workspace branch, and leave an ignored file.
    run_git(&path, &["commit", "-q", "-am", "work"]);
    std::fs::create_dir(path.join("build")).unwrap();
    std::fs::write(path.join("build/out.bin"), "x").unwrap();
    let r = check_cleanup(&f, w.id).await;
    assert!(r.allowed, "{r:?}");
    let warnings: Vec<_> = r.warnings.iter().map(|w| w.code.as_str()).collect();
    assert!(
        warnings.contains(&"ignored_files_deleted") && warnings.contains(&"unmerged_commits"),
        "{warnings:?}"
    );
    assert_eq!(r.ignored_files, vec!["build/".to_owned()]);
    assert_eq!(r.unmerged_commits, Some(1));
    assert!(r.branch_kept);

    // A stale or wrong confirmation is refused.
    assert_eq!(
        remote(do_cleanup(&f, w.id, "c1-0000".into()).await.unwrap_err()).code,
        ErrorCode::UnsafeCleanup
    );
    // A new commit after review invalidates the confirmation.
    std::fs::write(path.join("README.md"), "edited again").unwrap();
    run_git(&path, &["commit", "-q", "-am", "more work"]);
    assert_eq!(
        remote(do_cleanup(&f, w.id, r.confirmation.clone()).await.unwrap_err()).code,
        ErrorCode::UnsafeCleanup,
        "state changed after review"
    );
    assert!(path.exists());

    let r = check_cleanup(&f, w.id).await;
    let done = do_cleanup(&f, w.id, r.confirmation).await.unwrap();
    assert_eq!(done.workspace.status, WorkspaceStatus::Removed);
    assert!(!path.exists());
    // The branch and its commit are kept; nothing unrelated was touched.
    assert!(!git_out(&f.repo, &["branch", "--list", w.branch.as_deref().unwrap()]).is_empty());
    assert!(user_wt.join("README.md").is_file());
    assert!(git_out(&f.repo, &["worktree", "list"]).contains("user worktree"));
    assert!(Path::new(&keep.path).is_dir() && get(&f, keep.id).await.status == WorkspaceStatus::Ready);
    assert_eq!(git_out(&f.repo, &["branch", "--show-current"]), "main");

    let evs = events(&f, &["workspace.cleanup_requested", "workspace.removed"]).await;
    let mine: Vec<_> = evs.iter().filter(|e| e.context.workspace_id == Some(w.id)).collect();
    assert!(
        mine.iter().any(|e| e.event_type == "workspace.cleanup_requested" && e.payload["allowed"] == false)
    );
    let removed = mine.iter().find(|e| e.event_type == "workspace.removed").unwrap();
    assert_eq!(removed.payload["branch_kept"], true);
    // Removed is final.
    assert_eq!(
        remote(
            f.client.call::<WorkspacesRestore>(WorkspaceIdParams { workspace_id: w.id }).await.unwrap_err()
        )
        .code,
        ErrorCode::InvalidWorkspaceState
    );
}

#[tokio::test]
async fn unsafe_cleanups_are_refused_and_state_is_recoverable() {
    let f = fx().await;
    let w = create(&f, None).await;
    let path = PathBuf::from(&w.path);
    // An active workspace cannot be archived or cleaned up.
    let s = start(&f, w.id).await.unwrap();
    let e = f.client.call::<WorkspacesArchive>(WorkspaceIdParams { workspace_id: w.id }).await.unwrap_err();
    assert_eq!(remote(e).code, ErrorCode::InvalidWorkspaceState);
    stop(&f, s.id).await;

    // Locked worktrees are refused.
    f.client.call::<WorkspacesArchive>(WorkspaceIdParams { workspace_id: w.id }).await.unwrap();
    run_git(&f.repo, &["worktree", "lock", &w.path]);
    let r = f
        .client
        .call::<WorkspacesCleanupCheck>(WorkspaceIdParams { workspace_id: w.id })
        .await
        .unwrap()
        .report;
    assert!(r.blockers.iter().any(|b| b.code == "worktree_locked"));
    let e = f
        .client
        .call::<WorkspacesCleanup>(WorkspacesCleanupParams {
            workspace_id: w.id,
            confirmation: r.confirmation,
        })
        .await
        .unwrap_err();
    assert_eq!(remote(e).code, ErrorCode::UnsafeCleanup);
    assert!(path.is_dir());
    assert_eq!(get(&f, w.id).await.status, WorkspaceStatus::Archived);
    run_git(&f.repo, &["worktree", "unlock", &w.path]);

    // A directory Git does not know is never deleted.
    let stray = create(&f, None).await;
    f.client.call::<WorkspacesArchive>(WorkspaceIdParams { workspace_id: stray.id }).await.unwrap();
    run_git(&f.repo, &["worktree", "remove", &stray.path]);
    std::fs::create_dir_all(&stray.path).unwrap();
    std::fs::write(Path::new(&stray.path).join("precious"), "x").unwrap();
    let r = f
        .client
        .call::<WorkspacesCleanupCheck>(WorkspaceIdParams { workspace_id: stray.id })
        .await
        .unwrap()
        .report;
    assert!(r.blockers.iter().any(|b| b.code == "not_a_registered_worktree"), "{r:?}");
    assert!(Path::new(&stray.path).join("precious").is_file());

    // Restore returns an archived workspace to work.
    let restored =
        f.client.call::<WorkspacesRestore>(WorkspaceIdParams { workspace_id: w.id }).await.unwrap().workspace;
    assert_eq!(restored.status, WorkspaceStatus::Ready);

    // The project cannot be removed while workspaces may hold files.
    let e = f.client.call::<ProjectsRemove>(ProjectIdParams { project_id: f.project_id }).await.unwrap_err();
    assert_eq!(remote(e).code, ErrorCode::Conflict);
}

// --- recovery ---------------------------------------------------------------------

#[tokio::test]
async fn restart_reconciles_workspaces_without_deleting_anything() {
    let mut f = fx().await;
    let dirty = create(&f, Some("dirty")).await;
    std::fs::write(Path::new(&dirty.path).join("wip.txt"), "unsaved work").unwrap();
    let missing = create(&f, Some("missing")).await;
    let was_active = create(&f, Some("active")).await;
    let half = create(&f, Some("half-provisioned")).await;
    let cleaning = create(&f, Some("cleanup interrupted")).await;
    let cleaned = create(&f, Some("cleanup finished")).await;
    let completed = create(&f, Some("completed session")).await;
    let s = start(&f, completed.id).await.unwrap();
    send(&f, s.id, "[fake:exit]").await;
    wait_for("session completed", async || {
        f.client.call::<SessionsGet>(SessionIdParams { session_id: s.id }).await.unwrap().session.state
            == SessionState::Completed
    })
    .await;
    wait_for("workspace released", async || get(&f, completed.id).await.status == WorkspaceStatus::Ready)
        .await;
    assert!(Path::new(&completed.path).is_dir(), "a completed session never deletes its workspace");

    f.core.stop().await;
    std::fs::remove_dir_all(&missing.path).unwrap();
    run_git(&f.repo, &["worktree", "remove", &cleaned.path]);
    // Crash points: an agent was running; provisioning stopped after the
    // record was written; cleanup stopped before (and after) git ran.
    let never_created = Uuid::now_v7();
    let root = std::fs::canonicalize(f.core.dir.path().join(WORKSPACES_DIR)).unwrap();
    let half_path = root.join(f.project_id.to_string()).join(never_created.to_string());
    with_db(
        &f,
        &format!(
            "UPDATE workspaces SET status = 'active' WHERE id = '{a}';
             UPDATE workspaces SET status = 'provisioning' WHERE id = '{h}';
             INSERT INTO workspaces (id, project_id, kind, name, path, branch, base_ref, base_commit, status,
                 created_at, updated_at, last_activity_at)
                 VALUES ('{n}', '{p}', 'worktree', 'never created', '{np}', 'switchyard/{n}', 'HEAD', '{c}',
                 'provisioning', 1, 1, 1);
             UPDATE workspaces SET status = 'cleanup_pending', archived_at = 1 WHERE id IN ('{ci}', '{cd}');",
            a = was_active.id,
            h = half.id,
            n = never_created,
            p = f.project_id,
            np = half_path.display(),
            c = half.base_commit.clone().unwrap(),
            ci = cleaning.id,
            cd = cleaned.id,
        ),
    );
    restart(&mut f).await;

    let d = get(&f, dirty.id).await;
    assert_eq!(d.status, WorkspaceStatus::Dirty);
    assert_eq!(std::fs::read_to_string(Path::new(&dirty.path).join("wip.txt")).unwrap(), "unsaved work");
    let m = get(&f, missing.id).await;
    assert_eq!(m.status, WorkspaceStatus::Failed);
    assert!(m.status_reason.unwrap().starts_with("missing_worktree"));
    assert!(!m.path_exists);
    assert_eq!(get(&f, was_active.id).await.status, WorkspaceStatus::Ready, "no session survives; released");
    assert_eq!(
        get(&f, half.id).await.status,
        WorkspaceStatus::Ready,
        "complete worktree: provisioning finished"
    );
    let n = get(&f, never_created).await;
    assert_eq!(n.status, WorkspaceStatus::Failed);
    assert!(n.status_reason.unwrap().starts_with("provisioning_interrupted"));
    assert!(!half_path.exists(), "nothing was created behind the operator's back");
    let ci = get(&f, cleaning.id).await;
    assert_eq!(ci.status, WorkspaceStatus::Archived);
    assert!(ci.status_reason.unwrap().starts_with("cleanup_interrupted"));
    assert!(Path::new(&cleaning.path).is_dir(), "interrupted cleanup preserves the files");
    assert_eq!(get(&f, cleaned.id).await.status, WorkspaceStatus::Removed);
    assert!(Path::new(&completed.path).is_dir());
    // Nothing was started automatically.
    let live = f
        .client
        .call::<SessionsList>(SessionsListParams { live_only: true, ..Default::default() })
        .await
        .unwrap()
        .sessions;
    assert!(live.is_empty());

    // A failed workspace recovers only explicitly, once the cause is fixed.
    let still =
        f.client.call::<WorkspacesVerify>(WorkspaceIdParams { workspace_id: missing.id }).await.unwrap();
    assert_eq!(still.workspace.status, WorkspaceStatus::Failed);
}

#[tokio::test]
async fn core_restart_with_a_live_session_preserves_workspace_changes() {
    let mut f = fx().await;
    let w = create(&f, None).await;
    let s = start(&f, w.id).await.unwrap();
    send(&f, s.id, "[fake:write:kept.txt]").await;
    approve_next(&f, s.id).await;
    let file = Path::new(&w.path).join("kept.txt");
    wait_for("file", async || file.is_file()).await;
    restart(&mut f).await;
    let after = get(&f, w.id).await;
    assert_eq!(after.status, WorkspaceStatus::Dirty);
    assert!(file.is_file());
    let session = f.client.call::<SessionsGet>(SessionIdParams { session_id: s.id }).await.unwrap().session;
    assert!(session.state.is_terminal() && !session.attached);
    // The session can be resumed explicitly in the same workspace.
    let resumed = f
        .client
        .call::<SessionsResume>(SessionsResumeParams { session_id: s.id, prompt: None })
        .await
        .unwrap()
        .session;
    assert_eq!(resumed.cwd, w.path);
    assert_eq!(get(&f, w.id).await.status, WorkspaceStatus::Active);
    stop(&f, s.id).await;
}

#[tokio::test]
async fn legacy_sessions_are_never_resumed_in_the_project_checkout() {
    let mut f = fx().await;
    f.core.stop().await;
    let (legacy_ws, legacy_session, agent) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    with_db(
        &f,
        &format!(
            "INSERT INTO agents VALUES ('{agent}', 'claude_code', 'Claude Code', '/bin/claude', '2.1.322', 1, 1);
             INSERT INTO workspaces (id, project_id, kind, name, path, status, status_reason, created_at,
                 updated_at, last_activity_at, archived_at)
                 VALUES ('{legacy_ws}', '{p}', 'legacy_checkout', 'Project checkout (legacy)', '{root}',
                 'archived', 'legacy_checkout', 1, 1, 1, 1);
             INSERT INTO sessions (id, project_id, workspace_id, agent_id, provider, native_session_id, cwd,
                 state, started_at, last_activity_at)
                 VALUES ('{legacy_session}', '{p}', '{legacy_ws}', '{agent}', 'claude_code', 'native-1', '{root}',
                 'stopped', 1, 1);",
            p = f.project_id,
            root = f.repo.display(),
        ),
    );
    restart(&mut f).await;
    let e = f
        .client
        .call::<SessionsResume>(SessionsResumeParams { session_id: legacy_session, prompt: None })
        .await
        .unwrap_err();
    assert_eq!(remote(e).code, ErrorCode::LegacyWorkspace);
    assert_eq!(start(&f, legacy_ws).await.unwrap_err().code, ErrorCode::LegacyWorkspace);
    let r = f
        .client
        .call::<WorkspacesCleanupCheck>(WorkspaceIdParams { workspace_id: legacy_ws })
        .await
        .unwrap()
        .report;
    assert!(!r.allowed && r.blockers.iter().any(|b| b.code == "legacy_checkout"));
    // Still visible as history.
    let detail = f.client.call::<WorkspacesGet>(WorkspaceIdParams { workspace_id: legacy_ws }).await.unwrap();
    assert_eq!(detail.sessions.len(), 1);
    assert_eq!(detail.workspace.kind, WorkspaceKind::LegacyCheckout);
    assert!(f.repo.join("README.md").is_file());
}

// --- cleanup confirmation covers the whole tree (hardening) ----------------------

/// An archived workspace with an ignored `build/` directory, reviewed once.
async fn reviewed(f: &Fx) -> (Workspace, PathBuf, CleanupReport) {
    let w = create(f, None).await;
    let path = PathBuf::from(&w.path);
    std::fs::create_dir(path.join("build")).unwrap();
    std::fs::write(path.join("build/out.bin"), "aaaa").unwrap();
    f.client.call::<WorkspacesArchive>(WorkspaceIdParams { workspace_id: w.id }).await.unwrap();
    let r = check_cleanup(f, w.id).await;
    assert!(r.allowed, "{r:?}");
    assert_eq!(r.ignored_files, vec!["build/".to_owned()], "Git lists only the directory");
    (w, path, r)
}

async fn refused_and_preserved(f: &Fx, w: &Workspace, confirmation: String) {
    let e = remote(do_cleanup(f, w.id, confirmation).await.unwrap_err());
    assert_eq!(e.code, ErrorCode::UnsafeCleanup, "{e:?}");
    assert_eq!(get(f, w.id).await.status, WorkspaceStatus::Archived);
    assert!(Path::new(&w.path).join("build/out.bin").is_file(), "nothing was deleted");
}

/// The reported gap, reproduced: a file added inside an ignored directory
/// that the report already listed must invalidate the confirmation.
#[tokio::test]
async fn new_file_inside_listed_ignored_directory_invalidates_confirmation() {
    let f = fx().await;
    // 1. Review.
    let (w, path, first) = reviewed(&f).await;
    // 2. Add a file inside the already-listed ignored directory.
    std::fs::write(path.join("build/.env"), "API_KEY=keep-me").unwrap();
    let after = check_cleanup(&f, w.id).await;
    assert_eq!(after.ignored_files, first.ignored_files, "Git's view is unchanged…");
    assert_ne!(after.confirmation, first.confirmation, "…but the confirmation is not");
    // 3./4. The original confirmation is refused and nothing is deleted.
    refused_and_preserved(&f, &w, first.confirmation).await;
    assert_eq!(std::fs::read_to_string(path.join("build/.env")).unwrap(), "API_KEY=keep-me");
    // 5./6. A new review covers the new file and can be confirmed.
    let second = check_cleanup(&f, w.id).await;
    assert_eq!(second.entries_total, first.entries_total.map(|n| n + 1));
    assert!(second.warnings.iter().any(|w| w.code == "ignored_files_deleted"));
    let done = do_cleanup(&f, w.id, second.confirmation).await.unwrap();
    // 7. Documented behaviour: reviewed ignored content is deleted with the
    // directory; the branch is kept.
    assert_eq!(done.workspace.status, WorkspaceStatus::Removed);
    assert!(!path.exists());
    assert!(!git_out(&f.repo, &["branch", "--list", w.branch.as_deref().unwrap()]).is_empty());
}

#[tokio::test]
async fn modified_or_nested_ignored_content_invalidates_confirmation() {
    let f = fx().await;
    // Same-size modification of an ignored file.
    let (w, path, r) = reviewed(&f).await;
    std::fs::write(path.join("build/out.bin"), "bbbb").unwrap();
    refused_and_preserved(&f, &w, r.confirmation).await;
    // New nested ignored directories.
    let r = check_cleanup(&f, w.id).await;
    std::fs::create_dir_all(path.join("build/a/b/c")).unwrap();
    std::fs::write(path.join("build/a/b/c/data.db"), "x").unwrap();
    refused_and_preserved(&f, &w, r.confirmation).await;
    // A deletion is a change too.
    let r = check_cleanup(&f, w.id).await;
    std::fs::remove_file(path.join("build/a/b/c/data.db")).unwrap();
    refused_and_preserved(&f, &w, r.confirmation).await;
}

#[tokio::test]
async fn symlinks_are_not_followed_and_retargeting_invalidates() {
    let f = fx().await;
    let outside = f.core.dir.path().join("outside data");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("precious.txt"), "not in the workspace").unwrap();
    let w = create(&f, None).await;
    let path = PathBuf::from(&w.path);
    std::fs::create_dir(path.join("build")).unwrap();
    std::fs::write(path.join("build/out.bin"), "aaaa").unwrap();
    std::os::unix::fs::symlink(&outside, path.join("build/link")).unwrap();
    f.client.call::<WorkspacesArchive>(WorkspaceIdParams { workspace_id: w.id }).await.unwrap();
    let r = check_cleanup(&f, w.id).await;
    assert!(r.allowed, "{r:?}");
    // Retargeting the link after review is detected.
    std::fs::remove_file(path.join("build/link")).unwrap();
    std::os::unix::fs::symlink(f.core.dir.path(), path.join("build/link")).unwrap();
    refused_and_preserved(&f, &w, r.confirmation).await;
    let r = check_cleanup(&f, w.id).await;
    do_cleanup(&f, w.id, r.confirmation).await.unwrap();
    assert!(!path.exists());
    assert_eq!(
        std::fs::read_to_string(outside.join("precious.txt")).unwrap(),
        "not in the workspace",
        "the symlink's target outside the workspace is untouched"
    );
}

#[tokio::test]
async fn unreadable_content_and_nested_repositories_block_cleanup() {
    use std::os::unix::fs::PermissionsExt;
    let f = fx().await;
    let (w, path, _) = reviewed(&f).await;
    let locked = path.join("build/locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::write(locked.join("unknown.bin"), "x").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let r = check_cleanup(&f, w.id).await;
    let blocked = !r.allowed && r.blockers.iter().any(|b| b.code == "unreadable");
    let attempt = do_cleanup(&f, w.id, r.confirmation.clone()).await;
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(blocked, "{r:?}");
    assert_eq!(remote(attempt.unwrap_err()).code, ErrorCode::UnsafeCleanup);
    assert!(locked.join("unknown.bin").is_file());

    // A nested repository inside ignored content may hold the only copy of
    // some commits.
    std::fs::remove_dir_all(&locked).unwrap();
    std::fs::create_dir(path.join("build/vendored")).unwrap();
    run_git(&path.join("build/vendored"), &["init", "-q"]);
    let r = check_cleanup(&f, w.id).await;
    assert!(r.blockers.iter().any(|b| b.code == "nested_repository"), "{r:?}");
    assert_eq!(
        remote(do_cleanup(&f, w.id, r.confirmation).await.unwrap_err()).code,
        ErrorCode::UnsafeCleanup
    );
    assert!(path.join("build/vendored/.git").is_dir());
}

/// Changes made after the operator confirmed but before removal are caught
/// by the final check, which runs immediately before `git worktree remove`.
#[tokio::test]
async fn changes_between_confirmation_and_removal_abort_cleanup() {
    let f = fx_with(|c| c.cleanup_test_pause = Duration::from_millis(800)).await;
    let (w, path, r) = reviewed(&f).await;
    let socket = f.core.socket();
    let id = w.id;
    let confirmation = r.confirmation.clone();
    let pending = tokio::spawn(async move {
        let (client, _rx) = common::connect(&socket).await;
        client.call::<WorkspacesCleanup>(WorkspacesCleanupParams { workspace_id: id, confirmation }).await
    });
    wait_for("cleanup to start", async || get(&f, id).await.status == WorkspaceStatus::CleanupPending).await;
    std::fs::write(path.join("build/late.log"), "written during cleanup").unwrap();
    let e = remote(pending.await.unwrap().unwrap_err());
    assert_eq!(e.code, ErrorCode::UnsafeCleanup);
    let after = get(&f, id).await;
    assert_eq!(after.status, WorkspaceStatus::Archived);
    assert!(after.status_reason.unwrap().starts_with("cleanup_failed"));
    assert!(path.join("build/late.log").is_file() && path.join("build/out.bin").is_file());
    let failed = events(&f, &["workspace.cleanup_failed"]).await;
    assert!(
        failed
            .iter()
            .any(|e| e.context.workspace_id == Some(id) && e.payload["reason"] == "changed_before_removal")
    );
    // A fresh review of the unchanged state succeeds.
    let r = check_cleanup(&f, id).await;
    do_cleanup(&f, id, r.confirmation).await.unwrap();
    assert!(!path.exists());
}
