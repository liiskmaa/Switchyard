#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Worktree operations against real temporary repositories.

use std::path::{Path, PathBuf};
use std::process::Command;

use switchyard_git::{Git, GitError};

fn run_git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=Switchyard Test", "-c", "user.email=test@example.invalid"])
        .args(["-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"])
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A repository whose path contains spaces and non-ASCII characters.
fn repo() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("my repo ä");
    std::fs::create_dir(&root).unwrap();
    run_git(&root, &["init", "-q"]);
    std::fs::write(root.join("a.txt"), "a").unwrap();
    std::fs::write(root.join(".gitignore"), "build/\n").unwrap();
    run_git(&root, &["add", "."]);
    run_git(&root, &["commit", "-q", "-m", "init"]);
    let root = std::fs::canonicalize(root).unwrap();
    (dir, root)
}

#[tokio::test]
async fn creates_identifies_and_removes_a_worktree() {
    let (dir, root) = repo();
    let git = Git::default();
    let base = git.resolve_commit(&root, "HEAD").await.unwrap();
    let wt = std::fs::canonicalize(dir.path()).unwrap().join("work spaces ö").join("w1");
    std::fs::create_dir_all(wt.parent().unwrap()).unwrap();
    git.add_worktree(&root, &wt, "switchyard/w1", &base).await.unwrap();

    let id = git.worktree_identity(&wt).await.unwrap();
    let main = git.worktree_identity(&root).await.unwrap();
    assert_eq!(id.toplevel, wt);
    assert_eq!(id.common_dir, main.common_dir);
    assert_eq!(id.head_ref.as_deref(), Some("refs/heads/switchyard/w1"));
    assert_eq!(id.head_oid.as_deref(), Some(base.as_str()));
    assert_eq!(git.branch_tip(&root, "switchyard/w1").await.unwrap().as_deref(), Some(base.as_str()));

    let list = git.list_worktrees(&root).await.unwrap();
    let entry = list.iter().find(|e| e.path == wt).unwrap();
    assert_eq!(entry.branch.as_deref(), Some("refs/heads/switchyard/w1"));

    // The original checkout is untouched.
    assert_eq!(run_git(&root, &["branch", "--show-current"]), "main");

    // Ignored files are visible to the caller before removal.
    std::fs::create_dir(wt.join("build")).unwrap();
    std::fs::write(wt.join("build/out"), "x").unwrap();
    let (sample, total) = git.ignored_paths(&wt, 10).await.unwrap();
    assert_eq!((sample, total), (vec!["build/".to_owned()], 1));

    git.remove_worktree(&root, &wt).await.unwrap();
    assert!(!wt.exists());
    // The branch is kept.
    assert!(git.branch_tip(&root, "switchyard/w1").await.unwrap().is_some());
}

#[tokio::test]
async fn refuses_collisions_and_unsafe_removal() {
    let (dir, root) = repo();
    let git = Git::default();
    let base = git.resolve_commit(&root, "main").await.unwrap();
    let parent = std::fs::canonicalize(dir.path()).unwrap();
    let wt = parent.join("w");
    git.add_worktree(&root, &wt, "switchyard/a", &base).await.unwrap();

    // Existing branch is never reused.
    let err = git.add_worktree(&root, &parent.join("w2"), "switchyard/a", &base).await.unwrap_err();
    assert!(matches!(err, GitError::BranchExists(_)), "{err:?}");
    assert!(!parent.join("w2").exists());
    // Existing path is never overwritten.
    let err = git.add_worktree(&root, &wt, "switchyard/b", &base).await.unwrap_err();
    assert!(matches!(err, GitError::PathExists(_)), "{err:?}");
    assert!(git.branch_tip(&root, "switchyard/b").await.unwrap().is_none());

    // Option-like and unknown revisions are rejected.
    assert!(matches!(git.resolve_commit(&root, "--all").await, Err(GitError::UnknownRevision(_))));
    assert!(matches!(git.resolve_commit(&root, "nope").await, Err(GitError::UnknownRevision(_))));

    // Untracked files block removal (no --force).
    std::fs::write(wt.join("mine.txt"), "work").unwrap();
    let err = git.remove_worktree(&root, &wt).await.unwrap_err();
    assert!(matches!(err, GitError::WorktreeNotClean(_)), "{err:?}");
    assert!(wt.join("mine.txt").exists());
    std::fs::remove_file(wt.join("mine.txt")).unwrap();

    // Locked worktrees are refused.
    run_git(&root, &["worktree", "lock", wt.to_str().unwrap()]);
    let err = git.remove_worktree(&root, &wt).await.unwrap_err();
    assert!(matches!(err, GitError::WorktreeLocked(_)), "{err:?}");
    assert!(wt.exists());
}

#[tokio::test]
async fn merge_detection_and_missing_directories() {
    let (dir, root) = repo();
    let git = Git::default();
    let base = git.resolve_commit(&root, "HEAD").await.unwrap();
    let wt = std::fs::canonicalize(dir.path()).unwrap().join("w");
    git.add_worktree(&root, &wt, "switchyard/m", &base).await.unwrap();
    std::fs::write(wt.join("b.txt"), "b").unwrap();
    run_git(&wt, &["add", "b.txt"]);
    run_git(&wt, &["commit", "-q", "-m", "work"]);
    let tip = git.branch_tip(&root, "switchyard/m").await.unwrap().unwrap();
    assert_eq!(git.count_commits(&root, "main", &tip).await.unwrap(), 1);
    assert!(!git.is_ancestor(&root, &tip, "main").await.unwrap());
    run_git(&root, &["merge", "-q", "--ff-only", "switchyard/m"]);
    assert!(git.is_ancestor(&root, &tip, "main").await.unwrap());

    // A deleted directory is detectable and its registration removable
    // without touching other worktrees.
    let other = std::fs::canonicalize(dir.path()).unwrap().join("other");
    git.add_worktree(&root, &other, "user-branch", &base).await.unwrap();
    std::fs::remove_dir_all(&wt).unwrap();
    assert!(git.worktree_identity(&wt).await.is_err());
    let entry = git.list_worktrees(&root).await.unwrap().into_iter().find(|e| e.path == wt).unwrap();
    assert!(entry.prunable);
    git.remove_worktree(&root, &wt).await.unwrap();
    let list = git.list_worktrees(&root).await.unwrap();
    assert!(list.iter().all(|e| e.path != wt));
    assert!(list.iter().any(|e| e.path == other));
}

#[tokio::test]
async fn non_worktree_directory_is_not_identified_as_one() {
    let dir = tempfile::tempdir().unwrap();
    let git = Git::default();
    let err = git.worktree_identity(dir.path()).await.unwrap_err();
    assert!(matches!(err, GitError::NotARepository { .. }), "{err:?}");
}
