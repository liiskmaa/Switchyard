#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Project registration end to end through the IPC protocol.

mod common;

use switchyard_client::{ClientError, CoreClient};
use switchyard_protocol::ErrorCode;
use switchyard_protocol::methods::{
    Empty, ProjectIdParams, ProjectsAdd, ProjectsAddParams, ProjectsGitStatus, ProjectsList, ProjectsRemove,
    SystemHealth,
};

use common::{TestCore, git_repo};

async fn add(
    client: &CoreClient,
    path: &std::path::Path,
) -> Result<switchyard_protocol::model::Project, ClientError> {
    client
        .call::<ProjectsAdd>(ProjectsAddParams { path: path.display().to_string() })
        .await
        .map(|r| r.project)
}

fn code(err: ClientError) -> ErrorCode {
    match err {
        ClientError::Remote(e) => e.code,
        other => panic!("expected remote error, got {other:?}"),
    }
}

#[tokio::test]
async fn register_list_status_and_remove_without_touching_files() {
    let core = TestCore::start().await;
    let (client, _rx) = core.connect().await;
    let repo = git_repo(core.dir.path(), "alpha");

    let project = add(&client, &repo).await.unwrap();
    assert_eq!(project.name, "alpha");
    assert_eq!(project.root_path, repo.display().to_string());
    assert!(project.accessible);

    let listed = client.call::<ProjectsList>(Empty {}).await.unwrap().projects;
    assert_eq!(listed, vec![project.clone()]);
    let health = client.call::<SystemHealth>(Empty {}).await.unwrap();
    assert_eq!(health.projects_count, Some(1));

    let status = client.call::<ProjectsGitStatus>(ProjectIdParams { project_id: project.id }).await.unwrap();
    assert_eq!(status.branch.as_deref(), Some("main"));
    assert!(status.clean);
    std::fs::write(repo.join("new.txt"), "x").unwrap();
    std::fs::write(repo.join("README.md"), "changed").unwrap();
    let status = client.call::<ProjectsGitStatus>(ProjectIdParams { project_id: project.id }).await.unwrap();
    assert_eq!((status.untracked, status.unstaged, status.clean), (1, 1, false));

    let removed = client.call::<ProjectsRemove>(ProjectIdParams { project_id: project.id }).await.unwrap();
    assert_eq!(removed.project_id, project.id);
    assert!(client.call::<ProjectsList>(Empty {}).await.unwrap().projects.is_empty());
    assert!(repo.join(".git").is_dir() && repo.join("new.txt").is_file(), "files are untouched");

    let again = client.call::<ProjectsRemove>(ProjectIdParams { project_id: project.id }).await.unwrap_err();
    assert_eq!(code(again), ErrorCode::NotFound);
}

#[tokio::test]
async fn invalid_registrations_are_rejected_with_specific_codes() {
    let core = TestCore::start().await;
    let (client, _rx) = core.connect().await;
    let base = core.dir.path();
    let repo = git_repo(base, "beta");

    assert_eq!(
        code(add(&client, std::path::Path::new("relative")).await.unwrap_err()),
        ErrorCode::InvalidPath
    );
    assert_eq!(code(add(&client, &base.join("missing")).await.unwrap_err()), ErrorCode::InvalidPath);
    assert_eq!(code(add(&client, &repo.join("README.md")).await.unwrap_err()), ErrorCode::InvalidPath);
    let plain = base.join("plain");
    std::fs::create_dir(&plain).unwrap();
    assert_eq!(code(add(&client, &plain).await.unwrap_err()), ErrorCode::NotAGitRepository);
    let sub = repo.join("src");
    std::fs::create_dir(&sub).unwrap();
    assert_eq!(code(add(&client, &sub).await.unwrap_err()), ErrorCode::NotAGitRepository);

    add(&client, &repo).await.unwrap();
    assert_eq!(code(add(&client, &repo).await.unwrap_err()), ErrorCode::Conflict);
    // A different spelling of the same directory is still a duplicate.
    let dotted = repo.join("src/..");
    assert_eq!(code(add(&client, &dotted).await.unwrap_err()), ErrorCode::Conflict);

    let empty = client.call::<ProjectsAdd>(ProjectsAddParams { path: "  ".into() }).await.unwrap_err();
    assert_eq!(code(empty), ErrorCode::InvalidPath);
}

#[tokio::test]
async fn vanished_repository_is_reported_not_fatal() {
    let core = TestCore::start().await;
    let (client, _rx) = core.connect().await;
    let repo = git_repo(core.dir.path(), "gamma");
    let project = add(&client, &repo).await.unwrap();

    std::fs::remove_dir_all(&repo).unwrap();
    let listed = client.call::<ProjectsList>(Empty {}).await.unwrap().projects;
    assert!(!listed[0].accessible);
    let err = client.call::<ProjectsGitStatus>(ProjectIdParams { project_id: project.id }).await.unwrap_err();
    assert_eq!(code(err), ErrorCode::RepositoryUnavailable);
    // Registration can still be removed.
    client.call::<ProjectsRemove>(ProjectIdParams { project_id: project.id }).await.unwrap();
}

#[tokio::test]
async fn registrations_persist_across_core_restarts() {
    let mut core = TestCore::start().await;
    let repo = git_repo(core.dir.path(), "delta");
    let project = {
        let (client, _rx) = core.connect().await;
        add(&client, &repo).await.unwrap()
    };
    core.restart().await;
    let (client, _rx) = core.connect().await;
    let listed = client.call::<ProjectsList>(Empty {}).await.unwrap().projects;
    assert_eq!(listed, vec![project]);
}
