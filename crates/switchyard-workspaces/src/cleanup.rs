//! Cleanup safety evaluation.
//!
//! Core gathers facts about a workspace (from the database, the filesystem
//! and Git) into [`CleanupInputs`]; [`evaluate_cleanup`] turns them into a
//! [`CleanupReport`]. *Blockers* make cleanup impossible: nothing can
//! override them. *Warnings* describe what cleanup will do; the operator
//! accepts them by sending back the report's `confirmation`, which is bound
//! to the inspected state, so a workspace that changed after the operator
//! looked at it is re-inspected instead of removed.
//!
//! Cleanup removes the worktree directory and Git's registration of it. It
//! never deletes the branch, so committed work stays reachable.

use std::hash::{DefaultHasher, Hash, Hasher};

use serde::{Deserialize, Serialize};

use crate::model::{WorkspaceKind, WorkspaceStatus};

/// A complete scan of everything under the worktree directory (see
/// `switchyard-core`'s `fingerprint` module). Git cannot provide this: it
/// reports an ignored directory as one entry, whatever it contains.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct TreeSummary {
    /// Deterministic hash over every entry's relative path, type, size,
    /// mode, owner, inode, modification and change time, and symlink target.
    pub fingerprint: u64,
    pub entries: u64,
    pub bytes: u64,
    /// Reasons the tree cannot be removed safely (any one blocks cleanup).
    pub problems: Vec<TreeProblem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TreeProblem {
    /// `unreadable`, `foreign_filesystem`, `foreign_owner`, `not_removable`,
    /// `nested_repository`, `too_large`.
    pub code: String,
    /// Relative to the worktree root.
    pub path: String,
    pub detail: String,
}

/// Git's view of the worktree, from `git worktree list` in the project.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct RegisteredWorktree {
    /// Full ref (`refs/heads/...`), `None` when detached.
    pub branch: Option<String>,
    pub locked: bool,
    pub prunable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CleanupInputs {
    pub kind: WorkspaceKind,
    pub status: WorkspaceStatus,
    /// Short branch name recorded for the workspace (`switchyard/<id>`).
    pub branch: String,
    /// The recorded path equals the path derived from the ids under the
    /// Core-owned workspaces directory.
    pub path_matches_layout: bool,
    pub repository_available: bool,
    pub registered: Option<RegisteredWorktree>,
    pub directory_exists: bool,
    /// The directory is the top level of a worktree of the project's
    /// repository (same common Git directory). `None` if not checked.
    pub identity_ok: Option<bool>,
    /// Short branch checked out in the worktree, `None` when detached or
    /// unknown.
    pub head_branch: Option<String>,
    pub head_oid: Option<String>,
    pub staged: u32,
    pub unstaged: u32,
    pub untracked: u32,
    pub conflicted: u32,
    /// Git status could not be read although the directory exists.
    pub status_unknown: bool,
    /// Ignored paths (bounded sample) and their total count.
    pub ignored: Vec<String>,
    pub ignored_total: u32,
    pub branch_exists: bool,
    pub branch_tip: Option<String>,
    /// Commits on the branch not reachable from `merged_into`'s candidates.
    /// `None` when it could not be determined.
    pub unmerged_commits: Option<u32>,
    /// The reference the branch is fully contained in, if any.
    pub merged_into: Option<String>,
    /// The reference unmerged commits were counted against.
    pub compared_with: Option<String>,
    /// Filesystem scan of the directory; `None` if it was not scanned.
    pub tree: Option<TreeSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupFinding {
    /// Stable machine-readable code.
    pub code: String,
    pub message: String,
}

fn finding(code: &str, message: impl Into<String>) -> CleanupFinding {
    CleanupFinding { code: code.to_owned(), message: message.into() }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupReport {
    /// `true` when there are no blockers.
    pub allowed: bool,
    pub blockers: Vec<CleanupFinding>,
    pub warnings: Vec<CleanupFinding>,
    /// Cleanup would delete the worktree directory (otherwise it only
    /// removes a stale registration or marks the record removed).
    pub will_remove_directory: bool,
    pub branch: String,
    /// Always `true`: cleanup never deletes branches.
    pub branch_kept: bool,
    pub ignored_files: Vec<String>,
    pub ignored_total: u32,
    pub unmerged_commits: Option<u32>,
    pub merged_into: Option<String>,
    /// Entries and bytes that removal would delete (from the tree scan).
    pub entries_total: Option<u64>,
    pub bytes_total: Option<u64>,
    /// The scanned tree state this report was made from. Core re-scans
    /// immediately before removal and refuses if it differs.
    pub tree_fingerprint: Option<String>,
    /// Send back to confirm. Changes whenever the inspected state changes,
    /// including any change to any file under the worktree.
    pub confirmation: String,
}

pub fn evaluate_cleanup(i: &CleanupInputs) -> CleanupReport {
    let mut blockers = Vec::new();
    let mut warnings = Vec::new();

    if i.kind != WorkspaceKind::Worktree {
        blockers.push(finding(
            "legacy_checkout",
            "This record is the project's own checkout from before workspace isolation; Switchyard never \
             deletes it.",
        ));
    }
    if !matches!(i.status, WorkspaceStatus::Archived | WorkspaceStatus::CleanupPending) {
        blockers.push(finding(
            "not_archived",
            format!("The workspace is {}; archive it before requesting cleanup.", i.status),
        ));
    }
    if !i.path_matches_layout {
        blockers.push(finding(
            "path_outside_workspace_root",
            "The recorded path is not the Core-owned location derived from this workspace's id.",
        ));
    }
    if !i.repository_available {
        blockers.push(finding(
            "repository_unavailable",
            "The project repository is not accessible, so the worktree cannot be verified.",
        ));
    }

    let expected_ref = format!("refs/heads/{}", i.branch);
    if i.directory_exists {
        match &i.registered {
            None => blockers.push(finding(
                "not_a_registered_worktree",
                "The directory exists but Git does not list it as a worktree of this project. Inspect it \
                 and remove it manually if it is not needed.",
            )),
            Some(reg) => {
                if reg.locked {
                    blockers.push(finding(
                        "worktree_locked",
                        "The worktree is locked (git worktree lock). Unlock it first if removal is intended.",
                    ));
                }
                if reg.branch.as_deref() != Some(expected_ref.as_str()) {
                    blockers.push(finding(
                        "unexpected_branch",
                        format!("Git reports a different branch than {} for this worktree.", i.branch),
                    ));
                }
            }
        }
        if i.identity_ok == Some(false) {
            blockers.push(finding(
                "identity_mismatch",
                "The directory is not a worktree of this project's repository.",
            ));
        }
        if i.head_branch.as_deref() != Some(i.branch.as_str()) {
            blockers.push(finding(
                "unexpected_branch",
                format!(
                    "The worktree has {} checked out instead of {}.",
                    i.head_branch.as_deref().unwrap_or("a detached HEAD"),
                    i.branch
                ),
            ));
        }
        if i.status_unknown {
            blockers
                .push(finding("status_unknown", "Git status could not be read; cannot prove it is clean."));
        }
        let changed = i.staged + i.unstaged + i.conflicted;
        if changed > 0 {
            blockers.push(finding(
                "uncommitted_changes",
                format!(
                    "{changed} tracked path(s) have uncommitted changes. Commit or discard them yourself."
                ),
            ));
        }
        if i.untracked > 0 {
            blockers.push(finding(
                "untracked_files",
                format!(
                    "{} untracked path(s) would be lost. Commit, move or delete them yourself.",
                    i.untracked
                ),
            ));
        }
        match &i.tree {
            None => blockers.push(finding(
                "tree_not_scanned",
                "The worktree's files could not be scanned, so what cleanup would delete is unknown.",
            )),
            Some(tree) => {
                for (code, title) in [
                    ("unreadable", "Some entries could not be read; their contents are unknown"),
                    ("foreign_filesystem", "Another filesystem is mounted inside the worktree"),
                    ("foreign_owner", "Some entries belong to another user; removal could stop halfway"),
                    ("not_removable", "Some directories lack write permission; removal could stop halfway"),
                    (
                        "nested_repository",
                        "A nested Git repository (possibly with unpushed commits) would be deleted",
                    ),
                    ("too_large", "The worktree is too large to verify completely"),
                ] {
                    let paths: Vec<&str> =
                        tree.problems.iter().filter(|p| p.code == code).map(|p| p.path.as_str()).collect();
                    if !paths.is_empty() {
                        let sample = paths.iter().take(5).copied().collect::<Vec<_>>().join(", ");
                        let more = if paths.len() > 5 {
                            format!(" and {} more", paths.len() - 5)
                        } else {
                            String::new()
                        };
                        blockers.push(finding(code, format!("{title}: {sample}{more}.")));
                    }
                }
            }
        }
        if i.ignored_total > 0 {
            warnings.push(finding(
                "ignored_files_deleted",
                format!(
                    "{} Git-ignored path(s) (build output, dependencies, local config such as .env) will be \
                     deleted with the directory.",
                    i.ignored_total
                ),
            ));
        }
    } else if i.registered.is_some() {
        warnings.push(finding(
            "directory_missing",
            "The worktree directory is already gone; only Git's stale registration of it will be removed.",
        ));
    } else {
        warnings.push(finding(
            "nothing_to_remove",
            "Neither the directory nor a Git registration exists; the record will be marked removed.",
        ));
    }
    let mut seen = std::collections::HashSet::new();
    blockers.retain(|b: &CleanupFinding| seen.insert(b.code.clone()));

    if !i.branch_exists {
        warnings.push(finding("branch_missing", format!("Branch {} no longer exists.", i.branch)));
    } else {
        match (i.unmerged_commits, &i.merged_into) {
            (_, Some(target)) => warnings.push(finding(
                "branch_merged",
                format!("Branch {} is fully merged into {target}; it is kept.", i.branch),
            )),
            (Some(0), None) => {}
            (Some(n), None) => warnings.push(finding(
                "unmerged_commits",
                format!(
                    "Branch {} has {n} commit(s) not in {}. The branch is kept, so they stay reachable.",
                    i.branch,
                    i.compared_with.as_deref().unwrap_or("its base")
                ),
            )),
            (None, None) => warnings.push(finding(
                "merge_status_unknown",
                format!("Could not determine whether {} is merged. The branch is kept.", i.branch),
            )),
        }
    }

    let allowed = blockers.is_empty();
    CleanupReport {
        allowed,
        will_remove_directory: allowed && i.directory_exists,
        blockers,
        warnings,
        branch: i.branch.clone(),
        branch_kept: true,
        ignored_files: i.ignored.clone(),
        ignored_total: i.ignored_total,
        unmerged_commits: i.unmerged_commits,
        merged_into: i.merged_into.clone(),
        entries_total: i.tree.as_ref().map(|t| t.entries),
        bytes_total: i.tree.as_ref().map(|t| t.bytes),
        tree_fingerprint: i.tree.as_ref().map(|t| format!("t1-{:016x}", t.fingerprint)),
        confirmation: confirmation(i),
    }
}

/// Binds a confirmation to the inspected state (not a secret: it only
/// detects that something changed between inspection and confirmation).
fn confirmation(i: &CleanupInputs) -> String {
    let mut h = DefaultHasher::new();
    i.hash(&mut h);
    format!("c1-{:016x}", h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean() -> CleanupInputs {
        CleanupInputs {
            kind: WorkspaceKind::Worktree,
            status: WorkspaceStatus::Archived,
            branch: "switchyard/x".into(),
            path_matches_layout: true,
            repository_available: true,
            registered: Some(RegisteredWorktree {
                branch: Some("refs/heads/switchyard/x".into()),
                locked: false,
                prunable: false,
            }),
            directory_exists: true,
            identity_ok: Some(true),
            head_branch: Some("switchyard/x".into()),
            head_oid: Some("abc".into()),
            staged: 0,
            unstaged: 0,
            untracked: 0,
            conflicted: 0,
            status_unknown: false,
            ignored: Vec::new(),
            ignored_total: 0,
            branch_exists: true,
            branch_tip: Some("abc".into()),
            unmerged_commits: Some(0),
            merged_into: None,
            compared_with: Some("main".into()),
            tree: Some(TreeSummary { fingerprint: 1, entries: 3, bytes: 10, problems: Vec::new() }),
        }
    }

    fn codes(f: &[CleanupFinding]) -> Vec<&str> {
        f.iter().map(|f| f.code.as_str()).collect()
    }

    #[test]
    fn clean_archived_worktree_is_allowed_and_branch_kept() {
        let r = evaluate_cleanup(&clean());
        assert!(r.allowed && r.will_remove_directory && r.branch_kept);
        assert!(r.blockers.is_empty() && r.warnings.is_empty());
    }

    #[test]
    fn user_work_blocks_cleanup() {
        let mut i = clean();
        i.unstaged = 1;
        i.untracked = 2;
        let r = evaluate_cleanup(&i);
        assert!(!r.allowed && !r.will_remove_directory);
        assert_eq!(codes(&r.blockers), ["uncommitted_changes", "untracked_files"]);
    }

    #[test]
    fn identity_and_ownership_problems_block_cleanup() {
        for (mutate, code) in [
            (
                Box::new(|i: &mut CleanupInputs| i.kind = WorkspaceKind::LegacyCheckout)
                    as Box<dyn Fn(&mut _)>,
                "legacy_checkout",
            ),
            (Box::new(|i: &mut CleanupInputs| i.status = WorkspaceStatus::Ready), "not_archived"),
            (Box::new(|i: &mut CleanupInputs| i.path_matches_layout = false), "path_outside_workspace_root"),
            (Box::new(|i: &mut CleanupInputs| i.registered = None), "not_a_registered_worktree"),
            (Box::new(|i: &mut CleanupInputs| i.identity_ok = Some(false)), "identity_mismatch"),
            (Box::new(|i: &mut CleanupInputs| i.head_branch = Some("main".into())), "unexpected_branch"),
            (
                Box::new(|i: &mut CleanupInputs| {
                    if let Some(r) = &mut i.registered {
                        r.locked = true;
                    }
                }),
                "worktree_locked",
            ),
            (Box::new(|i: &mut CleanupInputs| i.status_unknown = true), "status_unknown"),
            (Box::new(|i: &mut CleanupInputs| i.repository_available = false), "repository_unavailable"),
        ] {
            let mut i = clean();
            mutate(&mut i);
            let r = evaluate_cleanup(&i);
            assert!(!r.allowed, "{code}");
            assert!(codes(&r.blockers).contains(&code), "{code}: {:?}", r.blockers);
        }
    }

    #[test]
    fn warnings_need_confirmation_bound_to_state() {
        let mut i = clean();
        i.ignored = vec![".env".into()];
        i.ignored_total = 1;
        i.unmerged_commits = Some(3);
        let r = evaluate_cleanup(&i);
        assert!(r.allowed);
        assert_eq!(codes(&r.warnings), ["ignored_files_deleted", "unmerged_commits"]);
        let before = r.confirmation;
        i.head_oid = Some("def".into());
        assert_ne!(evaluate_cleanup(&i).confirmation, before);
    }

    #[test]
    fn any_tree_change_invalidates_the_confirmation_and_tree_problems_block() {
        let i = clean();
        let before = evaluate_cleanup(&i).confirmation;
        let mut changed = i.clone();
        if let Some(t) = &mut changed.tree {
            t.fingerprint = 2;
        }
        assert_ne!(evaluate_cleanup(&changed).confirmation, before);

        let mut unscanned = clean();
        unscanned.tree = None;
        assert!(codes(&evaluate_cleanup(&unscanned).blockers).contains(&"tree_not_scanned"));

        for code in [
            "unreadable",
            "foreign_filesystem",
            "foreign_owner",
            "not_removable",
            "nested_repository",
            "too_large",
        ] {
            let mut bad = clean();
            if let Some(t) = &mut bad.tree {
                t.problems.push(TreeProblem {
                    code: code.into(),
                    path: "build/x".into(),
                    detail: String::new(),
                });
            }
            let r = evaluate_cleanup(&bad);
            assert!(!r.allowed && codes(&r.blockers).contains(&code), "{code}");
        }
    }

    #[test]
    fn missing_directory_only_unregisters() {
        let mut i = clean();
        i.directory_exists = false;
        i.head_branch = None;
        let r = evaluate_cleanup(&i);
        assert!(r.allowed && !r.will_remove_directory);
        assert_eq!(codes(&r.warnings), ["directory_missing"]);
    }
}
