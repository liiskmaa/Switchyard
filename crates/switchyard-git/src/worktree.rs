//! Worktree management for Core-owned workspaces.
//!
//! Every invocation passes arguments as an array (no shell), paths are
//! absolute (so they can never be read as options), and user-supplied
//! revisions go after `--end-of-options`. Removal never uses `--force`:
//! Git's own refusal to delete modified, untracked or locked worktrees is a
//! second safety net behind Core's checks. There is deliberately no prune
//! and no branch deletion.

use std::path::{Path, PathBuf};

use crate::command::Git;
use crate::error::GitError;

/// One entry of `git worktree list --porcelain`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    pub head: Option<String>,
    /// Full ref (`refs/heads/...`), `None` when detached or bare.
    pub branch: Option<String>,
    pub detached: bool,
    pub bare: bool,
    pub locked: bool,
    pub prunable: bool,
}

/// What a directory is, according to Git run inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeIdentity {
    /// Canonical top level of the working tree containing the directory.
    pub toplevel: PathBuf,
    /// Canonical shared `.git` directory (identical for all worktrees of one
    /// repository).
    pub common_dir: PathBuf,
    /// Full ref checked out (`refs/heads/...`), `None` when detached.
    pub head_ref: Option<String>,
    /// `None` on an unborn branch.
    pub head_oid: Option<String>,
}

fn not_absolute(path: &Path) -> Result<(), GitError> {
    if path.is_absolute() { Ok(()) } else { Err(GitError::NotAbsolute(path.to_path_buf())) }
}

fn utf8(path: &Path) -> Result<&str, GitError> {
    path.to_str().ok_or_else(|| GitError::UnexpectedOutput(format!("non-UTF-8 path {}", path.display())))
}

async fn canonical(path: &str) -> PathBuf {
    tokio::fs::canonicalize(path).await.unwrap_or_else(|_| PathBuf::from(path))
}

/// Parses `git worktree list --porcelain -z`.
pub fn parse_worktree_list(output: &str) -> Vec<WorktreeEntry> {
    let mut entries = Vec::new();
    let mut current: Option<WorktreeEntry> = None;
    for field in output.split('\0') {
        if field.is_empty() {
            entries.extend(current.take());
            continue;
        }
        let (key, value) = field.split_once(' ').unwrap_or((field, ""));
        if key == "worktree" {
            entries.extend(current.take());
            current = Some(WorktreeEntry { path: PathBuf::from(value), ..Default::default() });
            continue;
        }
        let Some(entry) = current.as_mut() else { continue };
        match key {
            "HEAD" => entry.head = Some(value.to_owned()),
            "branch" => entry.branch = Some(value.to_owned()),
            "detached" => entry.detached = true,
            "bare" => entry.bare = true,
            "locked" => entry.locked = true,
            "prunable" => entry.prunable = true,
            _ => {}
        }
    }
    entries.extend(current);
    entries
}

impl Git {
    /// Resolves `rev` to a commit id in `repo`.
    pub async fn resolve_commit(&self, repo: &Path, rev: &str) -> Result<String, GitError> {
        if rev.starts_with('-') {
            return Err(GitError::UnknownRevision(rev.to_owned()));
        }
        let spec = format!("{rev}^{{commit}}");
        let out = self.run(repo, &["rev-parse", "--verify", "--quiet", "--end-of-options", &spec]).await?;
        let oid = out.stdout.trim();
        if !out.success || oid.is_empty() {
            return Err(GitError::UnknownRevision(rev.to_owned()));
        }
        Ok(oid.to_owned())
    }

    /// The commit a local branch points to, `None` if it does not exist.
    pub async fn branch_tip(&self, repo: &Path, branch: &str) -> Result<Option<String>, GitError> {
        let full = format!("refs/heads/{branch}");
        let out = self.run(repo, &["rev-parse", "--verify", "--quiet", "--end-of-options", &full]).await?;
        let oid = out.stdout.trim();
        Ok((out.success && !oid.is_empty()).then(|| oid.to_owned()))
    }

    /// `git worktree add -b <branch> <path> <commit>`. Fails if the branch
    /// already exists (it is never reused or reset) or the path exists.
    pub async fn add_worktree(
        &self,
        repo: &Path,
        path: &Path,
        branch: &str,
        commit: &str,
    ) -> Result<(), GitError> {
        not_absolute(path)?;
        if tokio::fs::symlink_metadata(path).await.is_ok() {
            return Err(GitError::PathExists(path.to_path_buf()));
        }
        if self.branch_tip(repo, branch).await?.is_some() {
            return Err(GitError::BranchExists(branch.to_owned()));
        }
        let path_str = utf8(path)?;
        let args = ["worktree", "add", "--quiet", "--no-track", "-b", branch, path_str, commit];
        let out = self.run_slow(repo, &args).await?;
        if out.success {
            return Ok(());
        }
        if out.stderr.contains("already exists") {
            if out.stderr.contains("branch") {
                return Err(GitError::BranchExists(branch.to_owned()));
            }
            return Err(GitError::PathExists(path.to_path_buf()));
        }
        Err(Git::failed(&args, out))
    }

    /// All worktrees of the repository at `repo`. Paths are canonicalized
    /// when they exist.
    pub async fn list_worktrees(&self, repo: &Path) -> Result<Vec<WorktreeEntry>, GitError> {
        let args = ["worktree", "list", "--porcelain", "-z"];
        let out = self.run(repo, &args).await?;
        if !out.success {
            return Err(Git::failed(&args, out));
        }
        let mut entries = parse_worktree_list(&out.stdout);
        for e in &mut entries {
            if let Ok(p) = tokio::fs::canonicalize(&e.path).await {
                e.path = p;
            }
        }
        Ok(entries)
    }

    /// Identifies the working tree containing `dir`.
    pub async fn worktree_identity(&self, dir: &Path) -> Result<WorktreeIdentity, GitError> {
        let canonical_dir = crate::repository::accessible_directory(dir).await?;
        let args = ["rev-parse", "--path-format=absolute", "--show-toplevel", "--git-common-dir"];
        let out = self.run(&canonical_dir, &args).await?;
        if !out.success {
            if out.stderr.contains("not a git repository")
                || out.stderr.contains("must be run in a work tree")
            {
                return Err(GitError::NotARepository { path: canonical_dir, reason: out.stderr });
            }
            return Err(Git::failed(&args, out));
        }
        let mut lines = out.stdout.lines();
        let (Some(top), Some(common)) = (lines.next(), lines.next()) else {
            return Err(GitError::UnexpectedOutput(out.stdout.clone()));
        };
        let toplevel = canonical(top).await;
        let common_dir = canonical(common).await;
        let head = self.run(&canonical_dir, &["symbolic-ref", "--quiet", "HEAD"]).await?;
        let head_ref = head.success.then(|| head.stdout.trim().to_owned()).filter(|s| !s.is_empty());
        let oid = self.run(&canonical_dir, &["rev-parse", "--verify", "--quiet", "HEAD"]).await?;
        let head_oid = oid.success.then(|| oid.stdout.trim().to_owned()).filter(|s| !s.is_empty());
        Ok(WorktreeIdentity { toplevel, common_dir, head_ref, head_oid })
    }

    /// `git worktree remove <path>` (never forced). Git refuses to remove a
    /// worktree with modified or untracked files, or a locked one. Ignored
    /// files *are* deleted, so callers must check for them first.
    pub async fn remove_worktree(&self, repo: &Path, path: &Path) -> Result<(), GitError> {
        not_absolute(path)?;
        let path_str = utf8(path)?;
        let args = ["worktree", "remove", path_str];
        let out = self.run(repo, &args).await?;
        if out.success {
            return Ok(());
        }
        if out.stderr.contains("locked") {
            return Err(GitError::WorktreeLocked(path.to_path_buf()));
        }
        if out.stderr.contains("modified or untracked") {
            return Err(GitError::WorktreeNotClean(path.to_path_buf()));
        }
        Err(Git::failed(&args, out))
    }

    /// Whether `commit` is reachable from `of`.
    pub async fn is_ancestor(&self, repo: &Path, commit: &str, of: &str) -> Result<bool, GitError> {
        let args = ["merge-base", "--is-ancestor", commit, of];
        let out = self.run(repo, &args).await?;
        match (out.success, out.stderr.is_empty()) {
            (true, _) => Ok(true),
            (false, true) => Ok(false),
            (false, false) => Err(Git::failed(&args, out)),
        }
    }

    /// Number of commits reachable from `tip` but not from `base`.
    pub async fn count_commits(&self, repo: &Path, base: &str, tip: &str) -> Result<u32, GitError> {
        let range = format!("{base}..{tip}");
        let args = ["rev-list", "--count", &range];
        let out = self.run(repo, &args).await?;
        if !out.success {
            return Err(Git::failed(&args, out));
        }
        out.stdout.trim().parse().map_err(|_| GitError::UnexpectedOutput(out.stdout.clone()))
    }

    /// Git-ignored paths in the working tree at `dir` (directories are
    /// reported once, with a trailing `/`). Returns at most `limit` paths and
    /// the total count.
    pub async fn ignored_paths(&self, dir: &Path, limit: usize) -> Result<(Vec<String>, u32), GitError> {
        let args = ["status", "--porcelain=v2", "-z", "--ignored=matching", "--untracked-files=all"];
        let out = self.run(dir, &args).await?;
        if !out.success {
            return Err(Git::failed(&args, out));
        }
        let mut sample = Vec::new();
        let mut total = 0u32;
        for record in out.stdout.split('\0') {
            if let Some(path) = record.strip_prefix("! ") {
                total = total.saturating_add(1);
                if sample.len() < limit {
                    sample.push(path.to_owned());
                }
            }
        }
        Ok((sample, total))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_list() {
        let out = "worktree /r\0HEAD aaa\0branch refs/heads/main\0\0worktree /w 1 ä\0HEAD bbb\0branch \
                   refs/heads/switchyard/x\0locked\0\0worktree /gone\0HEAD ccc\0detached\0prunable gitdir file \
                   points to non-existent location\0\0";
        let list = parse_worktree_list(out);
        assert_eq!(list.len(), 3);
        assert_eq!(list[1].path, PathBuf::from("/w 1 ä"));
        assert_eq!(list[1].branch.as_deref(), Some("refs/heads/switchyard/x"));
        assert!(list[1].locked && !list[1].prunable);
        assert!(list[2].detached && list[2].prunable && list[2].branch.is_none());
    }
}
