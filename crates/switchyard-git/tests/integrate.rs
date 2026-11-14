#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Change inspection and integration primitives against real temporary
//! repositories (Milestone 7).

use std::path::{Path, PathBuf};
use std::process::Command;

use switchyard_git::{ChangeKind, Git, GitError, MergeOutcome, WorktreeMerge};

fn run_git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
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

fn commit(dir: &Path, msg: &str) -> String {
    run_git(dir, &["add", "-A"]);
    run_git(dir, &["commit", "-q", "--allow-empty", "-m", msg]);
    run_git(dir, &["rev-parse", "HEAD"])
}

/// A repository with a local identity (Core's own Git calls read it) and a
/// workspace worktree on `switchyard/w`.
fn setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo ä");
    std::fs::create_dir(&root).unwrap();
    run_git(&root, &["init", "-q"]);
    run_git(&root, &["config", "user.name", "Switchyard Test"]);
    run_git(&root, &["config", "user.email", "test@example.invalid"]);
    run_git(&root, &["config", "commit.gpgsign", "false"]);
    std::fs::write(root.join("keep.txt"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(
        root.join("old name.txt"),
        "a fairly long line so rename detection has content\n".repeat(5),
    )
    .unwrap();
    std::fs::write(root.join("gone.txt"), "bye\n").unwrap();
    std::fs::write(root.join(".gitignore"), "build/\n").unwrap();
    commit(&root, "init");
    let root = std::fs::canonicalize(root).unwrap();
    let ws = std::fs::canonicalize(dir.path()).unwrap().join("ws");
    run_git(&root, &["worktree", "add", "-q", "-b", "switchyard/w", ws.to_str().unwrap(), "HEAD"]);
    (dir, root, ws)
}

#[tokio::test]
async fn inspection_reports_commits_renames_deletions_binaries_and_merges() {
    let (_d, root, ws) = setup();
    let git = Git::default();
    let base = git.resolve_commit(&root, "main").await.unwrap();
    std::fs::rename(ws.join("old name.txt"), ws.join("new name.txt")).unwrap();
    std::fs::remove_file(ws.join("gone.txt")).unwrap();
    std::fs::write(ws.join("img.bin"), [0u8, 159, 146, 150, 0, 1, 2]).unwrap();
    std::fs::write(ws.join("keep.txt"), "one\nTWO\nthree\nfour\n").unwrap();
    commit(&ws, "agent work");
    // A merge commit inside the source history.
    run_git(&ws, &["switch", "-q", "-c", "side"]);
    std::fs::write(ws.join("side.txt"), "s\n").unwrap();
    commit(&ws, "side");
    run_git(&ws, &["switch", "-q", "switchyard/w"]);
    run_git(&ws, &["merge", "-q", "--no-ff", "-m", "merge side", "side"]);
    let tip = git.resolve_commit(&ws, "HEAD").await.unwrap();

    let (commits, total) = git.commits_between(&root, &base, &tip, 10).await.unwrap();
    assert_eq!(total, 3);
    assert_eq!(commits[0].subject, "merge side");
    assert_eq!(commits[0].parents.len(), 2, "merge commits are reported with both parents");
    assert_eq!(git.merge_base(&root, &base, &tip).await.unwrap().as_deref(), Some(base.as_str()));

    let diff = git.diff_summary(&root, &base, &tip, 100).await.unwrap();
    let by = |p: &str| diff.files.iter().find(|f| f.path == p).unwrap().clone();
    assert_eq!(by("new name.txt").kind, ChangeKind::Renamed);
    assert_eq!(by("new name.txt").old_path.as_deref(), Some("old name.txt"));
    assert_eq!(by("gone.txt").kind, ChangeKind::Deleted);
    assert!(by("img.bin").binary && by("img.bin").additions.is_none());
    assert_eq!((by("keep.txt").additions, by("keep.txt").deletions), (Some(2), Some(1)));
    assert_eq!(diff.total_files, 5);

    let (patch, cut) = git.diff_patch(&root, &base, &tip, Some("keep.txt"), 10_000).await.unwrap();
    assert!(!cut && patch.contains("+TWO") && patch.contains("-two"));
    let (all, cut) = git.diff_patch(&root, &base, &tip, None, 64).await.unwrap();
    assert!(cut && all.len() <= 64, "large patches are cut");
    let (bin, _) = git.diff_patch(&root, &base, &tip, Some("img.bin"), 10_000).await.unwrap();
    assert!(bin.contains("Binary files"));

    // Empty change: a commit compared with itself.
    let empty = git.diff_summary(&root, &tip, &tip, 100).await.unwrap();
    assert!(empty.files.is_empty() && empty.total_files == 0);
}

#[tokio::test]
async fn the_content_tree_includes_untracked_files_without_touching_the_index() {
    let (d, root, ws) = setup();
    let git = Git::default();
    let head_tree = git.tree_of(&ws, "HEAD").await.unwrap();
    let idx = d.path().join("probe.index");
    assert_eq!(git.content_tree(&ws, &idx).await.unwrap(), head_tree, "a clean worktree is its HEAD tree");
    std::fs::write(ws.join("untracked.txt"), "u\n").unwrap();
    std::fs::create_dir_all(ws.join("build")).unwrap();
    std::fs::write(ws.join("build/ignored.o"), "x").unwrap();
    std::fs::write(ws.join("keep.txt"), "changed\n").unwrap();
    let tree = git.content_tree(&ws, &idx).await.unwrap();
    assert_ne!(tree, head_tree);
    assert!(!idx.exists(), "the private index is removed");
    let diff = git.diff_summary(&root, &head_tree, &tree, 100).await.unwrap();
    let paths: Vec<_> = diff.files.iter().map(|f| (f.path.as_str(), f.kind)).collect();
    assert!(paths.contains(&("untracked.txt", ChangeKind::Added)));
    assert!(paths.contains(&("keep.txt", ChangeKind::Modified)));
    assert!(!paths.iter().any(|(p, _)| p.starts_with("build/")), "ignored files are not content");
    assert_eq!(
        run_git(&ws, &["diff", "--cached", "--name-only"]),
        "",
        "nothing was staged in the real index"
    );
    assert!(run_git(&ws, &["status", "--porcelain"]).contains("?? untracked.txt"), "still untracked");
}

#[tokio::test]
async fn merge_tree_detects_conflicts_without_changing_anything() {
    let (_d, root, ws) = setup();
    let git = Git::default();
    std::fs::write(ws.join("keep.txt"), "one\nWS\nthree\n").unwrap();
    let source = commit(&ws, "ws change");
    std::fs::write(root.join("keep.txt"), "one\nMAIN\nthree\n").unwrap();
    let target = commit(&root, "main change");
    let before = run_git(&root, &["status", "--porcelain"]);
    match git.merge_tree(&root, &target, &source).await.unwrap() {
        MergeOutcome::Conflicts { files, messages } => {
            assert_eq!(files, vec!["keep.txt"]);
            assert!(messages.iter().any(|m| m.contains("CONFLICT")), "{messages:?}");
        }
        other => panic!("expected conflicts, got {other:?}"),
    }
    assert_eq!(run_git(&root, &["status", "--porcelain"]), before);
    assert_eq!(git.resolve_commit(&root, "main").await.unwrap(), target, "the target branch did not move");

    // A real three-way merge without conflicts (both sides changed different
    // files since the base) yields a tree containing both, still moving nothing.
    run_git(&root, &["switch", "-q", "-c", "side", "HEAD~1"]);
    std::fs::write(root.join("side-only.txt"), "s\n").unwrap();
    let side = commit(&root, "side change");
    run_git(&root, &["switch", "-q", "main"]);
    std::fs::write(ws.join("ws-only.txt"), "w\n").unwrap();
    let ws_tip = commit(&ws, "ws change on another file");
    assert!(
        !git.is_ancestor(&root, &side, &ws_tip).await.unwrap()
            && !git.is_ancestor(&root, &ws_tip, &side).await.unwrap()
    );
    let MergeOutcome::Clean { tree } = git.merge_tree(&root, &side, &ws_tip).await.unwrap() else {
        panic!("expected a clean three-way merge")
    };
    let files = run_git(&root, &["ls-tree", "--name-only", &tree]);
    assert!(files.contains("side-only.txt") && files.contains("ws-only.txt"), "{files}");
    assert_eq!(git.resolve_commit(&root, "side").await.unwrap(), side, "nothing moved");
}

#[tokio::test]
async fn the_branch_update_is_a_compare_and_swap() {
    let (_d, root, ws) = setup();
    let git = Git::default();
    run_git(&root, &["switch", "-q", "-c", "parking"]); // main is not checked out anywhere now
    let target = git.resolve_commit(&root, "main").await.unwrap();
    std::fs::write(ws.join("feature.txt"), "f\n").unwrap();
    let source = commit(&ws, "feature");
    let MergeOutcome::Clean { tree } = git.merge_tree(&root, &target, &source).await.unwrap() else {
        panic!()
    };
    let merge = git
        .commit_tree(&root, &tree, &[&target, &source], "Merge feature\n\nSwitchyard-Integration: x\n")
        .await
        .unwrap();
    // A stale expectation is refused and nothing moves.
    let stale = git.update_branch(&root, "main", &merge, &source, "test").await.unwrap_err();
    assert!(matches!(stale, GitError::RefChanged { .. }), "{stale:?}");
    assert_eq!(git.resolve_commit(&root, "main").await.unwrap(), target);
    git.update_branch(&root, "main", &merge, &target, "switchyard: integrate").await.unwrap();
    assert_eq!(git.resolve_commit(&root, "main").await.unwrap(), merge);
    let parents = run_git(&root, &["log", "-1", "--format=%P", "main"]);
    assert_eq!(parents, format!("{target} {source}"), "exactly the approved parents");
    assert_eq!(
        run_git(&root, &["log", "-1", "--format=%B", "main"]),
        "Merge feature\n\nSwitchyard-Integration: x"
    );
    // Repeating the same update is refused (the branch moved): idempotence is the caller's check.
    assert!(git.update_branch(&root, "main", &merge, &target, "again").await.is_err());
    // The workspace was not modified.
    assert_eq!(git.resolve_commit(&ws, "HEAD").await.unwrap(), source);
}

#[tokio::test]
async fn a_fast_forward_of_a_checkout_never_overwrites_local_changes() {
    let (_d, root, ws) = setup();
    let git = Git::default();
    let target = git.resolve_commit(&root, "main").await.unwrap();
    std::fs::write(ws.join("keep.txt"), "one\nchanged by ws\nthree\n").unwrap();
    let source = commit(&ws, "feature");
    let MergeOutcome::Clean { tree } = git.merge_tree(&root, &target, &source).await.unwrap() else {
        panic!()
    };
    let merge = git.commit_tree(&root, &tree, &[&target, &source], "Merge").await.unwrap();
    // The user's uncommitted edit of the same file blocks the fast-forward.
    std::fs::write(root.join("keep.txt"), "user's unsaved work\n").unwrap();
    assert!(git.fast_forward_checkout(&root, &merge).await.is_err());
    assert_eq!(std::fs::read_to_string(root.join("keep.txt")).unwrap(), "user's unsaved work\n");
    assert_eq!(git.resolve_commit(&root, "main").await.unwrap(), target, "nothing moved");
    // With the checkout clean it fast-forwards to exactly the prepared commit.
    run_git(&root, &["checkout", "--", "keep.txt"]);
    git.fast_forward_checkout(&root, &merge).await.unwrap();
    assert_eq!(git.resolve_commit(&root, "main").await.unwrap(), merge);
    assert_eq!(std::fs::read_to_string(root.join("keep.txt")).unwrap(), "one\nchanged by ws\nthree\n");
}

#[tokio::test]
async fn workspace_merges_commit_all_and_abort() {
    let (_d, root, ws) = setup();
    let git = Git::default();
    // Uncommitted agent work becomes one commit, untracked files included.
    std::fs::write(ws.join("new.txt"), "n\n").unwrap();
    std::fs::write(ws.join("keep.txt"), "one\nWS\nthree\n").unwrap();
    let before = git.resolve_commit(&ws, "HEAD").await.unwrap();
    let snap = git.commit_all(&ws, "Snapshot").await.unwrap();
    assert_ne!(snap, before);
    assert_eq!(run_git(&ws, &["status", "--porcelain"]), "");
    assert_eq!(git.commit_all(&ws, "nothing").await.unwrap(), snap, "nothing to commit: HEAD unchanged");

    // The target moves; updating the workspace conflicts and can be aborted.
    std::fs::write(root.join("keep.txt"), "one\nMAIN\nthree\n").unwrap();
    let target = commit(&root, "main change");
    let WorktreeMerge::Conflicts(files) =
        git.merge_into_worktree(&ws, &target, "Update from main").await.unwrap()
    else {
        panic!("expected conflicts")
    };
    assert_eq!(files, vec!["keep.txt"]);
    assert!(git.repo_state(&ws).await.unwrap().merge_in_progress);
    let refused = git.commit_all(&ws, "with markers").await.unwrap_err();
    assert!(matches!(refused, GitError::WorktreeBusy(_)), "{refused:?}");
    git.merge_abort(&ws).await.unwrap();
    assert!(git.repo_state(&ws).await.unwrap().is_idle());
    assert_eq!(git.resolve_commit(&ws, "HEAD").await.unwrap(), snap);

    // Resolve: merge again, edit, commit_all completes the merge.
    let _ = git.merge_into_worktree(&ws, &target, "Update from main").await.unwrap();
    std::fs::write(ws.join("keep.txt"), "one\nresolved\nthree\n").unwrap();
    let resolved = git.commit_all(&ws, "Resolve").await.unwrap();
    assert_eq!(
        run_git(&ws, &["log", "-1", "--format=%P"]).split(' ').count(),
        2,
        "the resolution is a merge commit"
    );
    assert!(git.is_ancestor(&ws, &target, &resolved).await.unwrap());
    assert_eq!(git.resolve_commit(&root, "main").await.unwrap(), target, "the target branch never moved");
    assert_eq!(git.merge_into_worktree(&ws, &target, "again").await.unwrap(), WorktreeMerge::UpToDate);
}

#[tokio::test]
async fn repository_state_reports_locks_and_identity_problems() {
    let (_d, root, _ws) = setup();
    let git = Git::default();
    assert!(git.repo_state(&root).await.unwrap().is_idle());
    let lock = root.join(".git/index.lock");
    std::fs::write(&lock, "").unwrap();
    let st = git.repo_state(&root).await.unwrap();
    assert!(st.index_locked && st.describe().iter().any(|d| d.contains("index.lock")));
    std::fs::remove_file(lock).unwrap();
    assert!(git.committer_ident(&root).await.unwrap().contains("Switchyard Test <test@example.invalid>"));
    run_git(&root, &["config", "user.name", ""]);
    let err = git.commit_tree(&root, &git.tree_of(&root, "HEAD").await.unwrap(), &[], "x").await.unwrap_err();
    assert!(matches!(err, GitError::IdentityMissing(_)), "{err:?}");
}

#[tokio::test]
async fn merges_never_overwrite_ignored_files() {
    let (_d, root, ws) = setup();
    let git = Git::default();
    // The user's ignored local file in the target checkout.
    std::fs::write(root.join(".gitignore"), "build/\nsecrets.env\n").unwrap();
    let target = commit(&root, "ignore secrets.env");
    std::fs::write(root.join("secrets.env"), "USER SECRET\n").unwrap();
    // The workspace un-ignores it and commits its own version.
    run_git(&ws, &["merge", "-q", "main"]);
    std::fs::write(ws.join(".gitignore"), "build/\n").unwrap();
    std::fs::write(ws.join("secrets.env"), "AGENT VERSION\n").unwrap();
    let source = commit(&ws, "un-ignore and add secrets.env");
    let MergeOutcome::Clean { tree } = git.merge_tree(&root, &target, &source).await.unwrap() else {
        panic!()
    };
    let merge = git.commit_tree(&root, &tree, &[&target, &source], "Merge").await.unwrap();
    assert!(git.fast_forward_checkout(&root, &merge).await.is_err(), "Git must refuse");
    assert_eq!(std::fs::read_to_string(root.join("secrets.env")).unwrap(), "USER SECRET\n");
    assert_eq!(git.resolve_commit(&root, "main").await.unwrap(), target, "nothing moved");

    // The same protection when merging the target into a workspace.
    run_git(&root, &["switch", "-q", "-c", "parking"]);
    std::fs::write(root.join("cfg.local"), "x\n").unwrap();
    std::fs::write(root.join(".gitignore"), "build/\n").unwrap();
    run_git(&root, &["add", "-A"]);
    run_git(&root, &["commit", "-q", "-m", "track cfg.local"]);
    let tracked = run_git(&root, &["rev-parse", "HEAD"]);
    std::fs::write(ws.join(".gitignore"), "build/\ncfg.local\n").unwrap();
    commit(&ws, "ignore cfg.local");
    std::fs::write(ws.join("cfg.local"), "WORKSPACE LOCAL\n").unwrap();
    assert!(git.merge_into_worktree(&ws, &tracked, "update").await.is_err(), "Git must refuse");
    assert_eq!(std::fs::read_to_string(ws.join("cfg.local")).unwrap(), "WORKSPACE LOCAL\n");
}

#[tokio::test]
async fn conflicts_without_markers_need_an_explicit_resolution() {
    let (_d, root, ws) = setup();
    let git = Git::default();
    // modify/delete: main changes gone.txt, the workspace deletes it.
    std::fs::write(root.join("gone.txt"), "changed on main\n").unwrap();
    // binary conflict: both change img.bin.
    std::fs::write(root.join("img.bin"), [0u8, 1, 2, 3]).unwrap();
    let target = commit(&root, "main side");
    std::fs::remove_file(ws.join("gone.txt")).unwrap();
    std::fs::write(ws.join("img.bin"), [0u8, 9, 9, 9]).unwrap();
    commit(&ws, "ws side");
    let WorktreeMerge::Conflicts(files) = git.merge_into_worktree(&ws, &target, "update").await.unwrap()
    else {
        panic!("expected conflicts")
    };
    assert_eq!(files.len(), 2, "{files:?}");
    let err = git.commit_all(&ws, "resolve").await.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("gone.txt (one side deleted or renamed it)") && msg.contains("img.bin"), "{msg}");
    // An explicit resolution with `git add` is accepted.
    run_git(&ws, &["add", "--", "img.bin"]);
    run_git(&ws, &["rm", "-q", "--", "gone.txt"]);
    let done = git.commit_all(&ws, "resolve").await.unwrap();
    assert_eq!(run_git(&ws, &["rev-parse", "HEAD"]), done);
}
