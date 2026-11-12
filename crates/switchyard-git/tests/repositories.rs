#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Exercises the git CLI against real temporary repositories.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use switchyard_git::{Git, GitConfig, GitError};

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.name=Switchyard Test", "-c", "user.email=test@example.invalid"])
        .args(["-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"])
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(status.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&status.stderr));
}

fn repo_with_commit() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("demo-repo");
    std::fs::create_dir(&root).unwrap();
    run_git(&root, &["init", "-q"]);
    std::fs::write(root.join("a.txt"), "a").unwrap();
    run_git(&root, &["add", "a.txt"]);
    run_git(&root, &["commit", "-q", "-m", "init"]);
    (dir, root)
}

#[tokio::test]
async fn resolves_repository_root_and_reports_status() {
    let (_dir, root) = repo_with_commit();
    let git = Git::default();
    let repo = git.resolve_repository_root(&root).await.unwrap();
    assert_eq!(repo.path, std::fs::canonicalize(&root).unwrap());
    assert_eq!(repo.name, "demo-repo");

    let clean = git.status(&repo.path).await.unwrap();
    assert!(clean.is_clean());
    assert_eq!(clean.branch.as_deref(), Some("main"));
    assert!(clean.head_oid.is_some());

    std::fs::write(root.join("a.txt"), "changed").unwrap();
    std::fs::write(root.join("new.txt"), "n").unwrap();
    std::fs::write(root.join("staged.txt"), "s").unwrap();
    run_git(&root, &["add", "staged.txt"]);
    let dirty = git.status(&repo.path).await.unwrap();
    assert_eq!((dirty.staged, dirty.unstaged, dirty.untracked), (1, 1, 1));
}

#[tokio::test]
async fn unborn_repository_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    run_git(dir.path(), &["init", "-q"]);
    let git = Git::default();
    let repo = git.resolve_repository_root(dir.path()).await.unwrap();
    let status = git.status(&repo.path).await.unwrap();
    assert!(status.head_oid.is_none());
}

#[tokio::test]
async fn rejects_invalid_paths_with_specific_errors() {
    let (dir, root) = repo_with_commit();
    let git = Git::default();

    let r = git.resolve_repository_root(Path::new("relative/path")).await;
    assert!(matches!(r, Err(GitError::NotAbsolute(_))), "{r:?}");

    let r = git.resolve_repository_root(&dir.path().join("missing")).await;
    assert!(matches!(r, Err(GitError::NotFound(_))), "{r:?}");

    let r = git.resolve_repository_root(&root.join("a.txt")).await;
    assert!(matches!(r, Err(GitError::NotADirectory(_))), "{r:?}");

    let plain = dir.path().join("plain");
    std::fs::create_dir(&plain).unwrap();
    let r = git.resolve_repository_root(&plain).await;
    assert!(matches!(r, Err(GitError::NotARepository { .. })), "{r:?}");

    let sub = root.join("sub");
    std::fs::create_dir(&sub).unwrap();
    let r = git.resolve_repository_root(&sub).await;
    assert!(matches!(r, Err(GitError::NotRepositoryRoot { .. })), "{r:?}");

    let r = git.resolve_repository_root(&root.join(".git")).await;
    assert!(matches!(r, Err(GitError::NotARepository { .. })), "{r:?}");

    let bare = dir.path().join("bare.git");
    run_git(dir.path(), &["init", "-q", "--bare", bare.to_str().unwrap()]);
    let r = git.resolve_repository_root(&bare).await;
    assert!(matches!(r, Err(GitError::NotARepository { .. })), "{r:?}");
}

#[tokio::test]
async fn missing_git_binary_is_reported() {
    let (_dir, root) = repo_with_commit();
    let git = Git::new(GitConfig {
        binary: PathBuf::from("/nonexistent/switchyard-git-binary"),
        timeout: Duration::from_secs(5),
        ..GitConfig::default()
    });
    assert!(matches!(git.status(&root).await, Err(GitError::Unavailable { .. })));
}

#[tokio::test]
async fn reports_ignored_paths() {
    let (_dir, root) = repo_with_commit();
    std::fs::write(root.join(".gitignore"), "target/\n*.log\n").unwrap();
    let git = Git::default();
    let paths = vec!["target/debug/x".to_owned(), "src/main.rs".to_owned(), "a.log".to_owned()];
    let ignored = git.ignored(&root, &paths).await.unwrap();
    assert_eq!(ignored, ["target/debug/x".to_owned(), "a.log".to_owned()].into_iter().collect());
    assert!(git.ignored(&root, &["src/main.rs".to_owned()]).await.unwrap().is_empty());
}
