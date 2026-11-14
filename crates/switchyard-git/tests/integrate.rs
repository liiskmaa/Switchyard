#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Change inspection and integration primitives against real temporary
//! repositories (Milestone 7).

use std::path::{Path, PathBuf};
use std::process::Command;

use std::time::Duration;

use switchyard_git::{ChangeKind, FastForwardError, Git, GitConfig, GitError, MergeOutcome, WorktreeMerge};

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
    assert!(git.fast_forward_checkout(&root, "main", &target, &merge).await.is_err());
    assert_eq!(std::fs::read_to_string(root.join("keep.txt")).unwrap(), "user's unsaved work\n");
    assert_eq!(git.resolve_commit(&root, "main").await.unwrap(), target, "nothing moved");
    // With the checkout clean it fast-forwards to exactly the prepared commit.
    run_git(&root, &["checkout", "--", "keep.txt"]);
    git.fast_forward_checkout(&root, "main", &target, &merge).await.unwrap();
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
    // Staging the file does not make its markers a resolution.
    run_git(&ws, &["add", "--", "keep.txt"]);
    let refused = git.commit_all(&ws, "with staged markers").await.unwrap_err();
    assert!(refused.to_string().contains("conflict markers remain in keep.txt"), "{refused}");
    assert!(git.repo_state(&ws).await.unwrap().merge_in_progress, "nothing was committed");
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
    assert!(git.fast_forward_checkout(&root, "main", &target, &merge).await.is_err(), "Git must refuse");
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

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap()
}

/// Ignores `pattern` in the workspace (committed there), so files matching it
/// are ignored, untracked files of the workspace.
fn ignore_in_ws(ws: &Path, pattern: &str) {
    let mut gi = read(&ws.join(".gitignore"));
    gi.push_str(pattern);
    gi.push('\n');
    std::fs::write(ws.join(".gitignore"), gi).unwrap();
    commit(ws, "ignore");
}

#[tokio::test]
async fn an_ignored_file_is_never_replaced_by_a_directory() {
    let (_d, root, ws) = setup();
    let git = Git::default();
    ignore_in_ws(&ws, "cfg");
    std::fs::write(ws.join("cfg"), "SECRET\n").unwrap();
    // The target adds `cfg/inner`: merging it would turn `cfg` into a directory.
    std::fs::create_dir(root.join("cfg")).unwrap();
    std::fs::write(root.join("cfg/inner"), "t\n").unwrap();
    run_git(&root, &["add", "-f", "cfg/inner"]);
    let target = commit(&root, "add cfg/inner");
    let head = git.resolve_commit(&ws, "HEAD").await.unwrap();
    let err = git.merge_into_worktree(&ws, &target, "update").await.unwrap_err();
    assert!(err.to_string().contains("cfg"), "{err}");
    assert_eq!(read(&ws.join("cfg")), "SECRET\n");
    assert_eq!(git.resolve_commit(&ws, "HEAD").await.unwrap(), head, "nothing was merged");
    assert!(git.repo_state(&ws).await.unwrap().is_idle());

    // The same clash in a checked-out target being fast-forwarded.
    std::fs::remove_dir_all(root.join("cfg")).unwrap();
    run_git(&root, &["switch", "-q", "--detach"]);
    let main = run_git(&root, &["rev-parse", "HEAD~1"]);
    run_git(&root, &["switch", "-q", "-C", "main", &main]);
    std::fs::write(root.join(".git/info/exclude"), "cfg\n").unwrap();
    std::fs::write(root.join("cfg"), "ROOT SECRET\n").unwrap();
    let err = git.fast_forward_checkout(&root, "main", &main, &target).await.unwrap_err();
    assert!(matches!(err, FastForwardError::Refused(_)), "{err}");
    assert_eq!(read(&root.join("cfg")), "ROOT SECRET\n");
    assert_eq!(git.resolve_commit(&root, "main").await.unwrap(), main);
}

#[tokio::test]
async fn a_directory_rename_cannot_overwrite_an_ignored_file() {
    let (_d, root, ws) = setup();
    let git = Git::default();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"), "a\n").unwrap();
    std::fs::write(root.join("src/b.rs"), "b\n").unwrap();
    commit(&root, "src");
    run_git(&ws, &["merge", "-q", "main"]);
    ignore_in_ws(&ws, "*.env");
    // The workspace renames src/ to lib/; the target adds src/new.env, which
    // directory-rename detection places at lib/new.env.
    run_git(&ws, &["mv", "src", "lib"]);
    commit(&ws, "rename src to lib");
    std::fs::write(root.join("src/new.env"), "NEW\n").unwrap();
    run_git(&root, &["add", "-f", "src/new.env"]);
    let target = commit(&root, "add src/new.env");
    std::fs::write(ws.join("lib/new.env"), "SECRET\n").unwrap();
    let err = git.merge_into_worktree(&ws, &target, "update").await.unwrap_err();
    assert!(err.to_string().contains("lib/new.env"), "{err}");
    assert_eq!(read(&ws.join("lib/new.env")), "SECRET\n");
    assert!(git.repo_state(&ws).await.unwrap().is_idle());
}

#[tokio::test]
async fn nested_ignored_files_under_a_replaced_directory_are_protected() {
    let (_d, root, ws) = setup();
    let git = Git::default();
    ignore_in_ws(&ws, "logs/");
    std::fs::create_dir_all(ws.join("logs/deep/er")).unwrap();
    std::fs::write(ws.join("logs/deep/er/run.log"), "LOG\n").unwrap();
    // The target adds a file `logs/deep` where the workspace has a directory.
    std::fs::create_dir(root.join("logs")).unwrap();
    std::fs::write(root.join("logs/deep"), "a file now\n").unwrap();
    run_git(&root, &["add", "-f", "logs/deep"]);
    let target = commit(&root, "logs/deep is a file");
    let err = git.merge_into_worktree(&ws, &target, "update").await.unwrap_err();
    assert!(err.to_string().contains("logs/deep/er/run.log"), "{err}");
    assert_eq!(read(&ws.join("logs/deep/er/run.log")), "LOG\n");
    // Different letter case is the same file on a case-insensitive file system.
    let at_risk = git.untracked_files_at_risk(&ws, &["LOGS/DEEP".to_owned()]).await.unwrap();
    assert_eq!(at_risk, vec!["logs/deep/er/run.log"]);
}

#[tokio::test]
async fn unrelated_ignored_files_do_not_block_a_merge() {
    let (_d, root, ws) = setup();
    let git = Git::default();
    ignore_in_ws(&ws, ".env");
    std::fs::create_dir(ws.join("build")).unwrap();
    std::fs::write(ws.join("build/out.bin"), [0u8, 1]).unwrap();
    std::fs::write(ws.join(".env"), "KEEP\n").unwrap();
    std::fs::write(root.join("keep.txt"), "one\nmain\nthree\n").unwrap();
    std::fs::write(root.join("added.txt"), "x\n").unwrap();
    let target = commit(&root, "main change");
    let WorktreeMerge::Merged(_) = git.merge_into_worktree(&ws, &target, "update").await.unwrap() else {
        panic!("expected a clean merge")
    };
    assert_eq!(read(&ws.join(".env")), "KEEP\n");
    assert_eq!(std::fs::read(ws.join("build/out.bin")).unwrap(), [0u8, 1]);
    assert_eq!(read(&ws.join("keep.txt")), "one\nmain\nthree\n");
}

#[tokio::test]
async fn a_plain_merge_without_ignored_files_is_clean() {
    let (_d, root, ws) = setup();
    let git = Git::default();
    std::fs::write(ws.join("ws.txt"), "w\n").unwrap();
    commit(&ws, "ws work");
    std::fs::write(root.join("main.txt"), "m\n").unwrap();
    let target = commit(&root, "main work");
    let WorktreeMerge::Merged(head) = git.merge_into_worktree(&ws, &target, "update").await.unwrap() else {
        panic!("expected a clean merge")
    };
    assert!(git.is_ancestor(&ws, &target, &head).await.unwrap());
    assert_eq!((read(&ws.join("ws.txt")), read(&ws.join("main.txt"))), ("w\n".into(), "m\n".into()));
}

#[tokio::test]
async fn marker_like_text_is_judged_by_complete_new_conflict_blocks() {
    let (_d, root, ws) = setup();
    let git = Git::default();
    // A heading underline and a quoted marker are not conflict blocks.
    std::fs::write(ws.join("doc.md"), "Title\n=======\n\nGit writes `<<<<<<<` lines.\n<<<<<<< only\n")
        .unwrap();
    // A fixture with a complete block, committed earlier, may keep it.
    std::fs::write(ws.join("fixture.txt"), "<<<<<<< a\nx\n=======\ny\n>>>>>>> b\n").unwrap();
    run_git(&ws, &["add", "-A"]);
    run_git(&ws, &["commit", "-q", "--no-verify", "-m", "fixture"]);
    std::fs::write(ws.join("fixture.txt"), "<<<<<<< a\nx2\n=======\ny\n>>>>>>> b\n").unwrap();
    std::fs::write(ws.join("doc.md"), "Title\n=======\n\nchanged\n").unwrap();
    git.commit_all(&ws, "edits").await.unwrap();
    // A new complete block outside any merge is refused too.
    std::fs::write(ws.join("new.txt"), "<<<<<<< HEAD\nx\n=======\ny\n>>>>>>> other\n").unwrap();
    let err = git.commit_all(&ws, "new block").await.unwrap_err();
    assert!(err.to_string().contains("new.txt"), "{err}");
    let _ = root;
}

/// A `git` wrapper inside the test's directory that runs `action` before the
/// real Git for invocations whose arguments contain `matching`.
fn wrapper(dir: &Path, matching: &str, action: &str) -> PathBuf {
    let real = String::from_utf8(Command::new("sh").args(["-c", "command -v git"]).output().unwrap().stdout)
        .unwrap()
        .trim()
        .to_owned();
    let path = dir.join("git-wrapper.sh");
    std::fs::write(
        &path,
        format!("#!/bin/sh\ncase \"$*\" in *\"{matching}\"*) {action} ;; esac\nexec \"{real}\" \"$@\"\n"),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    path
}

/// Source and merge commits for a fast-forward of `main` in `root`.
async fn prepared_merge(git: &Git, root: &Path, ws: &Path) -> (String, String) {
    let target = git.resolve_commit(root, "main").await.unwrap();
    std::fs::write(ws.join("keep.txt"), "one\nfrom ws\nthree\n").unwrap();
    std::fs::write(ws.join("more.txt"), "m\n").unwrap();
    let source = commit(ws, "feature");
    let MergeOutcome::Clean { tree } = git.merge_tree(root, &target, &source).await.unwrap() else {
        panic!()
    };
    let merge = git.commit_tree(root, &tree, &[&target, &source], "Merge").await.unwrap();
    (target, merge)
}

#[tokio::test]
async fn a_checkout_switched_during_the_fast_forward_is_detected() {
    let (d, root, ws) = setup();
    let (target, merge) = prepared_merge(&Git::default(), &root, &ws).await;
    run_git(&root, &["branch", "other", &target]);
    let wrap = wrapper(d.path(), "merge --ff-only", &format!("git -C '{}' switch -q other", root.display()));
    let git = Git::new(GitConfig { binary: wrap, ..GitConfig::default() });
    let err = git.fast_forward_checkout(&root, "main", &target, &merge).await.unwrap_err();
    let FastForwardError::Diverted { checked_out, branch_tip } = &err else { panic!("{err}") };
    assert_eq!((checked_out.as_str(), branch_tip.as_deref()), ("refs/heads/other", Some(target.as_str())));
    assert_eq!(run_git(&root, &["rev-parse", "main"]), target, "the target branch did not move");
}

#[tokio::test]
async fn a_timed_out_fast_forward_reports_what_it_finds() {
    let (d, root, ws) = setup();
    let (target, merge) = prepared_merge(&Git::default(), &root, &ws).await;
    let config = |binary| GitConfig {
        binary,
        timeout: Duration::from_secs(3),
        checkout_timeout: Duration::from_secs(3),
    };
    let paths = Git::default().changed_paths(&root, &target, &merge).await.unwrap();
    // Stopped before Git wrote anything: provably unchanged.
    let git = Git::new(config(wrapper(d.path(), "merge --ff-only", "sleep 20")));
    let err = git.fast_forward_checkout(&root, "main", &target, &merge).await.unwrap_err();
    assert!(matches!(err, FastForwardError::Git(GitError::Timeout { .. })), "{err}");
    assert!(Git::default().checkout_unchanged(&root, "main", &target, &paths).await.unwrap());
    // Stopped after Git wrote a file (simulated): not unchanged.
    let action = format!("printf 'half\\n' > '{}/keep.txt'; sleep 20", root.display());
    let git = Git::new(config(wrapper(d.path(), "merge --ff-only", &action)));
    let err = git.fast_forward_checkout(&root, "main", &target, &merge).await.unwrap_err();
    assert!(matches!(err, FastForwardError::Git(GitError::Timeout { .. })), "{err}");
    assert!(!Git::default().checkout_unchanged(&root, "main", &target, &paths).await.unwrap());
    // A new file written by the interrupted update also counts.
    run_git(&root, &["checkout", "--", "keep.txt"]);
    std::fs::write(root.join("more.txt"), "m\n").unwrap();
    assert!(!Git::default().checkout_unchanged(&root, "main", &target, &paths).await.unwrap());
}
