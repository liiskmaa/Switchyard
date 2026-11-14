//! Change inspection for the operator: what the bound source commit
//! delivers, read from Git (never from an agent's description).

use std::path::Path;

use switchyard_git::{ChangedFile, DiffSummary};
use switchyard_protocol::ProtocolError;
use switchyard_protocol::model::{ChangedFileInfo, CommitSummary, IntegrationChanges};
use uuid::Uuid;

use super::{IntegrationService, git_err};

const MAX_COMMITS: u32 = 200;
const MAX_FILES: usize = 2000;
const MAX_PATCH_BYTES: usize = 1024 * 1024;

fn file_wire(f: ChangedFile) -> ChangedFileInfo {
    ChangedFileInfo {
        path: f.path,
        old_path: f.old_path,
        kind: f.kind.as_str().into(),
        additions: f.additions,
        deletions: f.deletions,
        binary: f.binary,
    }
}

impl IntegrationService {
    /// Commits and files the bound source commit delivers, measured from
    /// its common ancestor with the bound target commit, plus the
    /// workspace's uncommitted changes (which are *not* delivered).
    pub async fn changes(&self, id: Uuid) -> Result<IntegrationChanges, ProtocolError> {
        let i = self.record(id).await?;
        let git = self.inner.workspaces.git().clone();
        let root = self.inner.workspaces.project_root(i.project_id).await?;
        let (s, t) = (i.source_commit.clone(), i.target_commit.clone());
        let base = match &i.base_commit {
            Some(b) => Some(b.clone()),
            None => git.merge_base(&root, &t, &s).await.map_err(git_err)?,
        };
        let from = base.clone().unwrap_or_else(|| t.clone());
        let (commits, commits_total) =
            git.commits_between(&root, &t, &s, MAX_COMMITS).await.map_err(git_err)?;
        let DiffSummary { files, total_files, additions, deletions, truncated } =
            git.diff_summary(&root, &from, &s, MAX_FILES).await.map_err(git_err)?;
        let diverged = !git.is_ancestor(&root, &t, &s).await.map_err(git_err)?;
        let w = self.workspace_of(&i).await?;
        let uncommitted = match self.uncommitted_trees(&w).await {
            Some((head, content)) if head != content => git
                .diff_summary(Path::new(&w.path), &head, &content, MAX_FILES)
                .await
                .map(|d| d.files.into_iter().map(file_wire).collect())
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        Ok(IntegrationChanges {
            source_commit: s,
            target_commit: t,
            base_commit: base,
            diverged,
            commits: commits
                .into_iter()
                .map(|c| CommitSummary {
                    oid: c.oid,
                    parents: c.parents,
                    author_name: c.author_name,
                    author_email: c.author_email,
                    authored_at_ms: c.authored_at.saturating_mul(1000),
                    subject: c.subject,
                })
                .collect(),
            commits_total,
            files: files.into_iter().map(file_wire).collect(),
            files_total: total_files,
            additions,
            deletions,
            truncated,
            uncommitted,
        })
    }

    /// The patch of the delivered changes (or of the uncommitted ones),
    /// optionally for one path. Large patches are cut.
    pub async fn patch(
        &self,
        id: Uuid,
        path: Option<String>,
        uncommitted: bool,
    ) -> Result<(String, bool), ProtocolError> {
        let i = self.record(id).await?;
        let git = self.inner.workspaces.git().clone();
        let path = path.filter(|p| !p.is_empty());
        if uncommitted {
            let w = self.workspace_of(&i).await?;
            let Some((head, content)) = self.uncommitted_trees(&w).await else {
                return Ok((String::new(), false));
            };
            return git
                .diff_patch(Path::new(&w.path), &head, &content, path.as_deref(), MAX_PATCH_BYTES)
                .await
                .map_err(git_err);
        }
        let root = self.inner.workspaces.project_root(i.project_id).await?;
        let from = match &i.base_commit {
            Some(b) => b.clone(),
            None => i.target_commit.clone(),
        };
        git.diff_patch(&root, &from, &i.source_commit, path.as_deref(), MAX_PATCH_BYTES)
            .await
            .map_err(git_err)
    }

    /// (HEAD tree, content tree) of the workspace, if it can be read.
    async fn uncommitted_trees(&self, w: &switchyard_storage::WorkspaceRecord) -> Option<(String, String)> {
        let git = self.inner.workspaces.git();
        let head = git.tree_of(Path::new(&w.path), "HEAD").await.ok()?;
        let content = self.inner.workspaces.content_tree(w).await.ok()?;
        Some((head, content))
    }
}
