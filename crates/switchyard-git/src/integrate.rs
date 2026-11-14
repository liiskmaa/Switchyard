//! Change inspection and integration primitives (Milestone 7, ADRs 0028–0031).
//!
//! Inspection reads Git only. The mutating operations are:
//!
//! * creating objects (`merge-tree --write-tree`, `commit-tree`, `add` into a
//!   private index): they never move a branch or touch a working tree;
//! * [`Git::update_branch`]: one compare-and-swap ref update (`update-ref
//!   <new> <old>`), which Git applies atomically under its ref lock;
//! * operations inside a Core-owned workspace worktree ([`Git::commit_all`],
//!   [`Git::merge_into_worktree`], [`Git::merge_abort`]);
//! * [`Git::fast_forward_checkout`]: `merge --ff-only` in a clean checkout of
//!   the target branch. Git refuses instead of overwriting local changes.
//!
//! All writes run with hooks disabled. Nothing here pushes, resets, rebases,
//! force-updates, deletes branches or discards changes (except
//! [`Git::merge_abort`], which only undoes a merge Core started in a workspace).

use std::path::Path;

use crate::command::Git;
use crate::error::GitError;

/// One commit, as `git log` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    pub oid: String,
    pub parents: Vec<String>,
    pub author_name: String,
    pub author_email: String,
    /// Unix seconds.
    pub authored_at: i64,
    pub subject: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Unmerged,
    Unknown,
}

impl ChangeKind {
    fn from_status(s: &str) -> Self {
        match s.chars().next() {
            Some('A') => Self::Added,
            Some('M') => Self::Modified,
            Some('D') => Self::Deleted,
            Some('R') => Self::Renamed,
            Some('C') => Self::Copied,
            Some('T') => Self::TypeChanged,
            Some('U') => Self::Unmerged,
            _ => Self::Unknown,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Modified => "modified",
            Self::Deleted => "deleted",
            Self::Renamed => "renamed",
            Self::Copied => "copied",
            Self::TypeChanged => "type_changed",
            Self::Unmerged => "unmerged",
            Self::Unknown => "unknown",
        }
    }
}

/// One changed path between two trees or commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    /// The previous path of a rename or copy.
    pub old_path: Option<String>,
    pub kind: ChangeKind,
    /// `None` for binary files.
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
    pub binary: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffSummary {
    pub files: Vec<ChangedFile>,
    /// Number of changed files (may exceed `files.len()` when truncated).
    pub total_files: u64,
    pub additions: u64,
    pub deletions: u64,
    pub truncated: bool,
}

/// The result of merging two commits without touching any working tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    Clean { tree: String },
    Conflicts { files: Vec<String>, messages: Vec<String> },
}

/// Operations in progress in a working tree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoState {
    pub merge_in_progress: bool,
    pub rebase_in_progress: bool,
    pub cherry_pick_in_progress: bool,
    pub revert_in_progress: bool,
    pub bisect_in_progress: bool,
    /// `index.lock` exists: another Git process is (or was) writing.
    pub index_locked: bool,
}

impl RepoState {
    pub fn is_idle(&self) -> bool {
        *self == Self::default()
    }

    /// Human-readable list of what is in progress.
    pub fn describe(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.merge_in_progress {
            v.push("a merge is in progress");
        }
        if self.rebase_in_progress {
            v.push("a rebase is in progress");
        }
        if self.cherry_pick_in_progress {
            v.push("a cherry-pick is in progress");
        }
        if self.revert_in_progress {
            v.push("a revert is in progress");
        }
        if self.bisect_in_progress {
            v.push("a bisect is in progress");
        }
        if self.index_locked {
            v.push("index.lock exists (another Git process is writing, or one crashed)");
        }
        v
    }
}

/// What merging a commit into a workspace did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeMerge {
    /// A merge commit was created; HEAD is this commit.
    Merged(String),
    /// The commit was already contained in HEAD; nothing changed.
    UpToDate,
    /// The merge stopped with conflicts; the working tree holds conflict
    /// markers and `MERGE_HEAD` until the merge is completed or aborted.
    Conflicts(Vec<String>),
}

/// Whether a file contains a conflict-marker line (`<<<<<<< `, `=======`,
/// `>>>>>>> ` at a line start). A missing file has none.
pub async fn has_conflict_markers(path: &Path) -> bool {
    let Ok(bytes) = tokio::fs::read(path).await else { return false };
    bytes.split(|b| *b == b'\n').any(|line| {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        line.starts_with(b"<<<<<<< ") || line.starts_with(b">>>>>>> ") || line == b"======="
    })
}

/// Whether a file looks binary (a NUL byte in its first 8000 bytes, as Git decides).
async fn is_binary(path: &Path) -> bool {
    let Ok(bytes) = tokio::fs::read(path).await else { return false };
    bytes.iter().take(8000).any(|b| *b == 0)
}

fn check_rev(rev: &str) -> Result<(), GitError> {
    if rev.is_empty() || rev.starts_with('-') || rev.contains("..") {
        return Err(GitError::UnknownRevision(rev.to_owned()));
    }
    Ok(())
}

fn check_branch(branch: &str) -> Result<(), GitError> {
    if branch.is_empty() || branch.starts_with('-') || branch.starts_with("refs/") {
        return Err(GitError::UnknownRevision(branch.to_owned()));
    }
    Ok(())
}

/// Parses `git diff --name-status -z` and `--numstat -z` (both with `-M`).
pub fn parse_diff(name_status: &str, numstat: &str) -> Vec<ChangedFile> {
    let mut files = Vec::new();
    let mut fields = name_status.split('\0').filter(|f| !f.is_empty());
    while let Some(status) = fields.next() {
        let kind = ChangeKind::from_status(status);
        let (old_path, path) = if matches!(kind, ChangeKind::Renamed | ChangeKind::Copied) {
            let old = fields.next().unwrap_or_default().to_owned();
            (Some(old), fields.next().unwrap_or_default().to_owned())
        } else {
            (None, fields.next().unwrap_or_default().to_owned())
        };
        files.push(ChangedFile { path, old_path, kind, additions: None, deletions: None, binary: false });
    }
    // numstat: "<add>\t<del>\t<path>\0" or, for renames, "<add>\t<del>\t\0<old>\0<new>\0".
    let mut stats = Vec::new();
    let mut parts = numstat.split('\0');
    while let Some(head) = parts.next() {
        if head.is_empty() {
            continue;
        }
        let mut cols = head.splitn(3, '\t');
        let (a, d, p) = (cols.next().unwrap_or(""), cols.next().unwrap_or(""), cols.next().unwrap_or(""));
        let path = if p.is_empty() {
            let _old = parts.next();
            parts.next().unwrap_or_default().to_owned()
        } else {
            p.to_owned()
        };
        stats.push((path, a.parse::<u64>().ok(), d.parse::<u64>().ok(), a == "-" && d == "-"));
    }
    for f in &mut files {
        if let Some((_, a, d, bin)) = stats.iter().find(|(p, ..)| *p == f.path) {
            f.additions = *a;
            f.deletions = *d;
            f.binary = *bin;
        }
    }
    files
}

/// Parses `git merge-tree --write-tree -z --name-only` output.
pub fn parse_merge_tree(stdout: &str, conflicted: bool) -> Result<MergeOutcome, GitError> {
    let mut fields = stdout.split('\0');
    let tree = fields.next().unwrap_or_default().trim().to_owned();
    if !conflicted {
        if tree.len() < 40 {
            return Err(GitError::UnexpectedOutput(stdout.to_owned()));
        }
        return Ok(MergeOutcome::Clean { tree });
    }
    let mut files = Vec::new();
    for f in fields.by_ref() {
        if f.is_empty() {
            break;
        }
        if !files.iter().any(|x: &String| x == f) {
            files.push(f.to_owned());
        }
    }
    // Informational messages: <count>\0<path>...\0<type>\0<message>\0 groups.
    let rest: Vec<&str> = fields.collect();
    let mut messages = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        let Ok(n) = rest[i].parse::<usize>() else { break };
        let msg_at = i + 1 + n + 1;
        if let Some(m) = rest.get(msg_at) {
            messages.push(m.trim().to_owned());
        }
        i = msg_at + 1;
    }
    Ok(MergeOutcome::Conflicts { files, messages })
}

impl Git {
    /// The best common ancestor of two commits, `None` if they share no history.
    pub async fn merge_base(&self, repo: &Path, a: &str, b: &str) -> Result<Option<String>, GitError> {
        check_rev(a)?;
        check_rev(b)?;
        let args = ["merge-base", a, b];
        let out = self.run(repo, &args).await?;
        if out.success {
            return Ok(Some(out.stdout.trim().to_owned()));
        }
        if out.stderr.is_empty() {
            return Ok(None);
        }
        Err(Git::failed(&args, out))
    }

    /// The tree of a commit.
    pub async fn tree_of(&self, repo: &Path, rev: &str) -> Result<String, GitError> {
        check_rev(rev)?;
        let spec = format!("{rev}^{{tree}}");
        let out = self.run(repo, &["rev-parse", "--verify", "--quiet", "--end-of-options", &spec]).await?;
        let oid = out.stdout.trim();
        if !out.success || oid.is_empty() {
            return Err(GitError::UnknownRevision(rev.to_owned()));
        }
        Ok(oid.to_owned())
    }

    /// Commits reachable from `tip` but not from `base`, newest first (at
    /// most `limit`), with the total count.
    pub async fn commits_between(
        &self,
        repo: &Path,
        base: &str,
        tip: &str,
        limit: u32,
    ) -> Result<(Vec<CommitInfo>, u32), GitError> {
        check_rev(base)?;
        check_rev(tip)?;
        let total = self.count_commits(repo, base, tip).await?;
        let range = format!("{base}..{tip}");
        let max = format!("--max-count={limit}");
        let args = ["log", "--no-color", &max, "--format=%H%x00%P%x00%an%x00%ae%x00%at%x00%s%x1e", &range];
        let out = self.run(repo, &args).await?;
        if !out.success {
            return Err(Git::failed(&args, out));
        }
        let mut commits = Vec::new();
        for record in out.stdout.split('\x1e') {
            let record = record.trim_start_matches('\n');
            if record.is_empty() {
                continue;
            }
            let f: Vec<&str> = record.splitn(6, '\0').collect();
            if f.len() != 6 {
                return Err(GitError::UnexpectedOutput(record.to_owned()));
            }
            commits.push(CommitInfo {
                oid: f[0].to_owned(),
                parents: f[1].split_whitespace().map(str::to_owned).collect(),
                author_name: f[2].to_owned(),
                author_email: f[3].to_owned(),
                authored_at: f[4].parse().unwrap_or(0),
                subject: f[5].to_owned(),
            });
        }
        Ok((commits, total))
    }

    /// Files changed between two commits or trees (renames detected), at
    /// most `limit` entries.
    pub async fn diff_summary(
        &self,
        repo: &Path,
        from: &str,
        to: &str,
        limit: usize,
    ) -> Result<DiffSummary, GitError> {
        check_rev(from)?;
        check_rev(to)?;
        let ns_args = ["diff", "--no-color", "--no-ext-diff", "-M", "--name-status", "-z", from, to];
        let ns = self.run(repo, &ns_args).await?;
        if !ns.success {
            return Err(Git::failed(&ns_args, ns));
        }
        let num_args = ["diff", "--no-color", "--no-ext-diff", "-M", "--numstat", "-z", from, to];
        let num = self.run(repo, &num_args).await?;
        if !num.success {
            return Err(Git::failed(&num_args, num));
        }
        let mut files = parse_diff(&ns.stdout, &num.stdout);
        let total_files = files.len() as u64;
        let additions = files.iter().filter_map(|f| f.additions).sum();
        let deletions = files.iter().filter_map(|f| f.deletions).sum();
        let truncated = files.len() > limit;
        files.truncate(limit);
        Ok(DiffSummary { files, total_files, additions, deletions, truncated })
    }

    /// The patch between two commits or trees, optionally for one path,
    /// cut at `max_bytes`. Returns the text and whether it was truncated.
    /// Binary files appear as "Binary files … differ".
    pub async fn diff_patch(
        &self,
        repo: &Path,
        from: &str,
        to: &str,
        path: Option<&str>,
        max_bytes: usize,
    ) -> Result<(String, bool), GitError> {
        check_rev(from)?;
        check_rev(to)?;
        let mut args = vec!["diff", "--no-color", "--no-ext-diff", "-M", "--patch", from, to];
        if let Some(p) = path {
            args.extend(["--", p]);
        }
        let out = self.run(repo, &args).await?;
        if !out.success {
            return Err(Git::failed(&args, out));
        }
        let mut text = out.stdout;
        if text.len() <= max_bytes {
            return Ok((text, false));
        }
        let mut end = max_bytes;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        Ok((text, true))
    }

    /// The tree of everything in the working tree at `dir`: tracked files as
    /// they are on disk plus untracked, non-ignored files. Uses the private
    /// index file `index_file` (created and removed here), so neither the
    /// worktree nor its index is changed. Blob objects are written to the
    /// object database (unreferenced objects are harmless).
    pub async fn content_tree(&self, dir: &Path, index_file: &Path) -> Result<String, GitError> {
        let idx =
            index_file.to_str().ok_or_else(|| GitError::UnexpectedOutput("non-UTF-8 index path".into()))?;
        let env = [("GIT_INDEX_FILE", idx)];
        let result = async {
            let head = self.run_env(dir, &["read-tree", "HEAD"], &env).await?;
            if !head.success {
                // An unborn branch has no HEAD: start from an empty index.
                let empty = self.run_env(dir, &["read-tree", "--empty"], &env).await?;
                if !empty.success {
                    return Err(Git::failed(&["read-tree", "--empty"], empty));
                }
            }
            let add = self.run_env(dir, &["add", "-A", "--", "."], &env).await?;
            if !add.success {
                return Err(Git::failed(&["add", "-A"], add));
            }
            let tree = self.run_env(dir, &["write-tree"], &env).await?;
            if !tree.success {
                return Err(Git::failed(&["write-tree"], tree));
            }
            Ok(tree.stdout.trim().to_owned())
        }
        .await;
        let _ = tokio::fs::remove_file(index_file).await;
        result
    }

    /// Merges `source` into `target` in memory (`merge-tree --write-tree`).
    /// Nothing outside the object database changes.
    pub async fn merge_tree(
        &self,
        repo: &Path,
        target: &str,
        source: &str,
    ) -> Result<MergeOutcome, GitError> {
        check_rev(target)?;
        check_rev(source)?;
        let args = ["merge-tree", "--write-tree", "-z", "--name-only", target, source];
        let out = self.run(repo, &args).await?;
        // Exit 0: clean; 1: conflicts; anything else: an error.
        match out.status.as_str() {
            s if out.success || s.ends_with("status: 0") => parse_merge_tree(&out.stdout, false),
            s if s.ends_with("status: 1") => parse_merge_tree(&out.stdout, true),
            _ => Err(Git::failed(&args, out)),
        }
    }

    /// The committer identity Git would use, or an error if none is configured.
    pub async fn committer_ident(&self, repo: &Path) -> Result<String, GitError> {
        let out = self.run(repo, &["var", "GIT_COMMITTER_IDENT"]).await?;
        if !out.success {
            return Err(GitError::IdentityMissing(out.stderr));
        }
        let ident = out.stdout.trim();
        // Drop the timestamp: "Name <email> 1700000000 +0200".
        Ok(ident.rsplitn(3, ' ').nth(2).unwrap_or(ident).to_owned())
    }

    /// Creates a commit object (no branch moves) with exactly `parents`.
    pub async fn commit_tree(
        &self,
        repo: &Path,
        tree: &str,
        parents: &[&str],
        message: &str,
    ) -> Result<String, GitError> {
        check_rev(tree)?;
        let mut args = vec!["commit-tree", tree];
        for p in parents {
            check_rev(p)?;
            args.extend(["-p", p]);
        }
        args.extend(["-F", "-"]);
        let out = self.run_with_input(repo, &args, Some(message.as_bytes().to_vec())).await?;
        if !out.success {
            if out.stderr.contains("tell me who you are") || out.stderr.contains("empty ident") {
                return Err(GitError::IdentityMissing(out.stderr));
            }
            return Err(Git::failed(&args, out));
        }
        Ok(out.stdout.trim().to_owned())
    }

    /// Moves `refs/heads/<branch>` from `expected_old` to `new`, atomically:
    /// Git takes the ref lock and refuses if the branch is not at
    /// `expected_old` ([`GitError::RefChanged`]).
    pub async fn update_branch(
        &self,
        repo: &Path,
        branch: &str,
        new: &str,
        expected_old: &str,
        reason: &str,
    ) -> Result<(), GitError> {
        check_branch(branch)?;
        check_rev(new)?;
        check_rev(expected_old)?;
        let full = format!("refs/heads/{branch}");
        let args = ["update-ref", "-m", reason, &full, new, expected_old];
        let out = self.run_write(repo, &args, false).await?;
        if out.success {
            return Ok(());
        }
        let actual = self.branch_tip(repo, branch).await?.unwrap_or_else(|| "(missing)".into());
        if actual != expected_old {
            return Err(GitError::RefChanged {
                branch: branch.to_owned(),
                expected: expected_old.to_owned(),
                actual,
            });
        }
        Err(Git::failed(&args, out))
    }

    /// Operations in progress in the working tree at `dir`.
    pub async fn repo_state(&self, dir: &Path) -> Result<RepoState, GitError> {
        let names = [
            "MERGE_HEAD",
            "rebase-merge",
            "rebase-apply",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "BISECT_LOG",
            "index.lock",
        ];
        let mut args = vec!["rev-parse"];
        for n in &names {
            args.extend(["--git-path", n]);
        }
        let out = self.run(dir, &args).await?;
        if !out.success {
            return Err(Git::failed(&args, out));
        }
        let paths: Vec<std::path::PathBuf> = out.stdout.lines().map(|l| dir.join(l)).collect();
        if paths.len() != names.len() {
            return Err(GitError::UnexpectedOutput(out.stdout));
        }
        let exists = |i: usize| paths[i].exists();
        Ok(RepoState {
            merge_in_progress: exists(0),
            rebase_in_progress: exists(1) || exists(2),
            cherry_pick_in_progress: exists(3),
            revert_in_progress: exists(4),
            bisect_in_progress: exists(5),
            index_locked: exists(6),
        })
    }

    /// Commits everything in the worktree (tracked changes and untracked,
    /// non-ignored files) on its current branch. Returns the new HEAD, or
    /// the current HEAD if there was nothing to commit. Completes an
    /// in-progress merge.
    pub async fn commit_all(&self, dir: &Path, message: &str) -> Result<String, GitError> {
        // Checked before staging: `git add` would mark any unmerged file as
        // resolved. A text conflict counts as resolved once its markers are
        // gone; conflicts without markers (binary files, modify/delete,
        // renames) must be resolved explicitly with `git add` in the workspace.
        let mut unresolved = Vec::new();
        for (path, stages) in self.unmerged_stages(dir).await? {
            let file = dir.join(&path);
            let reason = if !(stages.contains(&2) && stages.contains(&3)) {
                Some("one side deleted or renamed it")
            } else if is_binary(&file).await {
                Some("binary file")
            } else if has_conflict_markers(&file).await {
                Some("conflict markers remain")
            } else {
                None
            };
            if let Some(r) = reason {
                unresolved.push(format!("{path} ({r})"));
            }
        }
        if !unresolved.is_empty() {
            return Err(GitError::WorktreeBusy(format!(
                "unresolved conflicts: {}. Resolve them in the workspace; for files without conflict markers, \
                 mark your resolution with `git add <path>` there",
                unresolved.join(", ")
            )));
        }
        let add = self.run_write(dir, &["add", "-A", "--", "."], true).await?;
        if !add.success {
            return Err(Git::failed(&["add", "-A"], add));
        }
        let state = self.repo_state(dir).await?;
        let staged = self.run(dir, &["diff", "--cached", "--quiet"]).await?;
        if staged.success && !state.merge_in_progress {
            return self.resolve_commit(dir, "HEAD").await;
        }
        let args = ["commit", "--no-verify", "--no-edit", "--quiet", "-m", message];
        let out = self.run_write(dir, &args, true).await?;
        if !out.success {
            if out.stderr.contains("tell me who you are") || out.stderr.contains("empty ident") {
                return Err(GitError::IdentityMissing(out.stderr));
            }
            return Err(Git::failed(&args, out));
        }
        self.resolve_commit(dir, "HEAD").await
    }

    /// Unmerged index entries of `dir`: path → the stages present (1 base,
    /// 2 ours, 3 theirs).
    pub async fn unmerged_stages(&self, dir: &Path) -> Result<Vec<(String, Vec<u8>)>, GitError> {
        let args = ["ls-files", "--unmerged", "-z"];
        let out = self.run(dir, &args).await?;
        if !out.success {
            return Err(Git::failed(&args, out));
        }
        let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
        for record in out.stdout.split('\0').filter(|r| !r.is_empty()) {
            // "<mode> <oid> <stage>\t<path>"
            let Some((meta, path)) = record.split_once('\t') else { continue };
            let stage = meta.rsplit(' ').next().and_then(|s| s.parse::<u8>().ok()).unwrap_or(0);
            match entries.iter_mut().find(|(p, _)| p == path) {
                Some((_, stages)) => stages.push(stage),
                None => entries.push((path.to_owned(), vec![stage])),
            }
        }
        Ok(entries)
    }

    /// The branch a checkout is on (`refs/heads/...`, `None` if detached)
    /// and its HEAD commit.
    pub async fn checkout_head(&self, dir: &Path) -> Result<(Option<String>, Option<String>), GitError> {
        let sym = self.run(dir, &["symbolic-ref", "--quiet", "HEAD"]).await?;
        let branch = sym.success.then(|| sym.stdout.trim().to_owned()).filter(|s| !s.is_empty());
        let oid = self.run(dir, &["rev-parse", "--verify", "--quiet", "HEAD"]).await?;
        let head = oid.success.then(|| oid.stdout.trim().to_owned()).filter(|s| !s.is_empty());
        Ok((branch, head))
    }

    /// Ignored, untracked files in the worktree at `dir` that a change from
    /// `from` to `to` would write. Git's merge machinery does not reliably
    /// protect ignored files (the three-way strategy replaces them even with
    /// `--no-overwrite-ignore`), so callers refuse when this is not empty.
    pub async fn ignored_files_at_risk(
        &self,
        dir: &Path,
        from: &str,
        to: &str,
    ) -> Result<Vec<String>, GitError> {
        check_rev(from)?;
        check_rev(to)?;
        let args = ["diff", "--no-renames", "--name-only", "-z", from, to];
        let out = self.run(dir, &args).await?;
        if !out.success {
            return Err(Git::failed(&args, out));
        }
        let paths: Vec<&str> = out.stdout.split('\0').filter(|p| !p.is_empty()).collect();
        let mut at_risk = Vec::new();
        for chunk in paths.chunks(500) {
            let mut args = vec!["ls-files", "--others", "--ignored", "--exclude-standard", "-z", "--"];
            args.extend_from_slice(chunk);
            let out = self.run(dir, &args).await?;
            if !out.success {
                return Err(Git::failed(&args, out));
            }
            at_risk.extend(out.stdout.split('\0').filter(|p| !p.is_empty()).map(str::to_owned));
        }
        Ok(at_risk)
    }

    async fn refuse_ignored_overwrite(&self, dir: &Path, from: &str, to: &str) -> Result<(), GitError> {
        let at_risk = self.ignored_files_at_risk(dir, from, to).await?;
        if at_risk.is_empty() {
            return Ok(());
        }
        Err(GitError::WorktreeBusy(format!(
            "the change would overwrite ignored files in {}: {}. Move them aside first; Switchyard never replaces them",
            dir.display(),
            at_risk.join(", ")
        )))
    }

    /// Paths with unresolved conflicts in the index of `dir`.
    pub async fn unmerged_paths(&self, dir: &Path) -> Result<Vec<String>, GitError> {
        let args = ["diff", "--name-only", "--diff-filter=U", "-z"];
        let out = self.run(dir, &args).await?;
        if !out.success {
            return Err(Git::failed(&args, out));
        }
        let mut v: Vec<String> =
            out.stdout.split('\0').filter(|s| !s.is_empty()).map(str::to_owned).collect();
        v.dedup();
        Ok(v)
    }

    /// `git merge --no-ff <commit>` inside the worktree at `dir` (a
    /// Core-owned workspace). On conflicts the merge is left in progress.
    pub async fn merge_into_worktree(
        &self,
        dir: &Path,
        commit: &str,
        message: &str,
    ) -> Result<WorktreeMerge, GitError> {
        check_rev(commit)?;
        let head = self.resolve_commit(dir, "HEAD").await?;
        if self.is_ancestor(dir, commit, &head).await? {
            return Ok(WorktreeMerge::UpToDate);
        }
        // Git's three-way merge replaces ignored files even with
        // `--no-overwrite-ignore`: check the paths the merge can write first.
        let base = self.merge_base(dir, &head, commit).await?.unwrap_or_else(|| head.clone());
        self.refuse_ignored_overwrite(dir, &base, commit).await?;
        let args =
            ["merge", "--no-ff", "--no-edit", "--no-verify", "--no-overwrite-ignore", "-m", message, commit];
        let out = self.run_write(dir, &args, true).await?;
        if out.success {
            return Ok(WorktreeMerge::Merged(self.resolve_commit(dir, "HEAD").await?));
        }
        let unmerged = self.unmerged_paths(dir).await?;
        if !unmerged.is_empty() && self.repo_state(dir).await?.merge_in_progress {
            return Ok(WorktreeMerge::Conflicts(unmerged));
        }
        if out.stderr.contains("tell me who you are") || out.stderr.contains("empty ident") {
            return Err(GitError::IdentityMissing(out.stderr));
        }
        Err(Git::failed(&args, out))
    }

    /// Aborts a merge in progress in `dir` (`git merge --abort`).
    pub async fn merge_abort(&self, dir: &Path) -> Result<(), GitError> {
        let args = ["merge", "--abort"];
        let out = self.run_write(dir, &args, true).await?;
        if !out.success {
            return Err(Git::failed(&args, out));
        }
        Ok(())
    }

    /// `git merge --ff-only <commit>` in a checkout of the target branch.
    /// Git refuses (and changes nothing) if it is not a fast-forward or if
    /// local changes or untracked files would be overwritten.
    pub async fn fast_forward_checkout(&self, dir: &Path, commit: &str) -> Result<(), GitError> {
        check_rev(commit)?;
        // Ignored files (e.g. a local `.env`) are never replaced: checked here,
        // with `--no-overwrite-ignore` as a second guard.
        let head = self.resolve_commit(dir, "HEAD").await?;
        self.refuse_ignored_overwrite(dir, &head, commit).await?;
        let args = ["merge", "--ff-only", "--no-edit", "--no-verify", "--no-overwrite-ignore", commit];
        let out = self.run_write(dir, &args, true).await?;
        if !out.success {
            return Err(Git::failed(&args, out));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_renames_binaries_and_deletions() {
        let ns = "M\0src/a.rs\0R087\0old name.txt\0new name.txt\0D\0gone.txt\0A\0img.png\0";
        let num = "3\t1\tsrc/a.rs\0".to_owned()
            + "1\t0\t\0old name.txt\0new name.txt\0"
            + "0\t4\tgone.txt\0"
            + "-\t-\timg.png\0";
        let files = parse_diff(ns, &num);
        assert_eq!(files.len(), 4);
        assert_eq!(
            (files[0].kind, files[0].additions, files[0].deletions),
            (ChangeKind::Modified, Some(3), Some(1))
        );
        assert_eq!(files[1].kind, ChangeKind::Renamed);
        assert_eq!(
            (files[1].old_path.as_deref(), files[1].path.as_str()),
            (Some("old name.txt"), "new name.txt")
        );
        assert_eq!(files[1].additions, Some(1));
        assert_eq!(files[2].kind, ChangeKind::Deleted);
        assert!(files[3].binary && files[3].additions.is_none());
    }

    #[test]
    fn parses_merge_tree_conflicts() {
        let out = "abca2c1461310a527adb6a8108f7332dde620c12\0f.txt\0\x001\0f.txt\0Auto-merging\0Auto-merging f.txt\n\0\
                   1\0f.txt\0CONFLICT (contents)\0CONFLICT (content): Merge conflict in f.txt\n\0";
        let MergeOutcome::Conflicts { files, messages } = parse_merge_tree(out, true).unwrap() else {
            panic!("expected conflicts")
        };
        assert_eq!(files, vec!["f.txt"]);
        assert_eq!(messages, vec!["Auto-merging f.txt", "CONFLICT (content): Merge conflict in f.txt"]);
        assert!(matches!(
            parse_merge_tree("690dbeaccf24a9e1be47b47ec752a626486b1737\n", false).unwrap(),
            MergeOutcome::Clean { .. }
        ));
    }

    #[test]
    fn revisions_that_look_like_options_or_ranges_are_refused() {
        assert!(check_rev("-x").is_err() && check_rev("a..b").is_err() && check_rev("").is_err());
        assert!(check_branch("refs/heads/x").is_err() && check_branch("--force").is_err());
        assert!(check_rev("HEAD").is_ok() && check_branch("main").is_ok());
    }
}
