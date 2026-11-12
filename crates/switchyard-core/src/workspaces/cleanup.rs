//! Explicit, confirmed workspace cleanup.
//!
//! `cleanup_check` gathers facts and evaluates them
//! (`switchyard_workspaces::evaluate_cleanup`); `cleanup` gathers them again
//! under the repository lock, refuses on any blocker or if the result's
//! confirmation differs from the one the operator reviewed, persists the
//! request, removes the worktree with `git worktree remove` (never forced)
//! and records the outcome. The branch is never deleted. Nothing here runs
//! automatically.

use std::path::{Path, PathBuf};

use serde_json::json;
use switchyard_events::DomainEvent;
use switchyard_protocol::model::{CleanupReport, Workspace};
use switchyard_protocol::{ErrorCode, ProtocolError};
use switchyard_storage::WorkspaceChanges;
use switchyard_storage::WorkspaceRecord;
use switchyard_workspaces::{
    CleanupInputs, RegisteredWorktree, WorkspaceKind, WorkspaceStatus, evaluate_cleanup,
};

use super::inspect::real_dir;
use super::{WorkspaceService, err};

async fn scan_tree(path: &std::path::Path) -> switchyard_workspaces::TreeSummary {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || super::fingerprint::scan(&path, super::fingerprint::MAX_ENTRIES))
        .await
        .unwrap_or_else(|e| switchyard_workspaces::TreeSummary {
            problems: vec![switchyard_workspaces::TreeProblem {
                code: "unreadable".into(),
                path: ".".into(),
                detail: format!("scan failed: {e}"),
            }],
            ..Default::default()
        })
}

/// Ignored paths listed in a report (the total is always reported).
const IGNORED_SAMPLE: usize = 50;

impl WorkspaceService {
    async fn cleanup_inputs(&self, w: &WorkspaceRecord) -> Result<CleanupInputs, ProtocolError> {
        let git = &self.inner.git;
        let root = self.project_root(w.project_id).await?;
        let path = PathBuf::from(&w.path);
        let branch = w.branch.clone().unwrap_or_default();
        let inspection =
            if w.kind == WorkspaceKind::Worktree { Some(self.inspect(w, &root).await) } else { None };
        let repository_available = inspection.as_ref().is_some_and(|i| i.repository_available);
        let directory_exists = real_dir(&path).await;

        let mut inputs = CleanupInputs {
            kind: w.kind,
            status: w.status,
            branch: branch.clone(),
            path_matches_layout: w.kind == WorkspaceKind::Worktree && path == self.expected_path(w),
            repository_available,
            registered: None,
            directory_exists,
            identity_ok: None,
            head_branch: None,
            head_oid: None,
            staged: 0,
            unstaged: 0,
            untracked: 0,
            conflicted: 0,
            status_unknown: false,
            ignored: Vec::new(),
            ignored_total: 0,
            branch_exists: false,
            branch_tip: None,
            unmerged_commits: None,
            merged_into: None,
            compared_with: None,
            tree: None,
        };
        if !repository_available || w.kind != WorkspaceKind::Worktree {
            return Ok(inputs);
        }
        let inspection = inspection.as_ref();
        if let Ok(list) = git.list_worktrees(&root).await {
            inputs.registered = list.into_iter().find(|e| e.path == path).map(|e| RegisteredWorktree {
                branch: e.branch,
                locked: e.locked,
                prunable: e.prunable,
            });
        } else {
            // Unknown registration is treated as "not registered", which
            // blocks removal of an existing directory.
            tracing::warn!(workspace_id = %w.id, "cannot list worktrees");
        }
        if directory_exists {
            inputs.tree = Some(scan_tree(&path).await);
            let identity = inspection.and_then(|i| i.identity.as_ref());
            let common = inspection.and_then(|i| i.common_dir.as_ref());
            inputs.identity_ok =
                Some(identity.is_some_and(|id| id.toplevel == path && Some(&id.common_dir) == common));
            inputs.head_branch = identity
                .and_then(|id| id.head_ref.as_deref())
                .and_then(|r| r.strip_prefix("refs/heads/"))
                .map(str::to_owned);
            inputs.head_oid = identity.and_then(|id| id.head_oid.clone());
            match git.status(&path).await {
                Ok(s) => {
                    inputs.staged = s.staged;
                    inputs.unstaged = s.unstaged;
                    inputs.untracked = s.untracked;
                    inputs.conflicted = s.conflicted;
                }
                Err(_) => inputs.status_unknown = true,
            }
            match git.ignored_paths(&path, IGNORED_SAMPLE).await {
                Ok((sample, total)) => {
                    inputs.ignored = sample;
                    inputs.ignored_total = total;
                }
                Err(_) => inputs.status_unknown = true,
            }
        }
        if let Ok(Some(tip)) = git.branch_tip(&root, &branch).await {
            inputs.branch_exists = true;
            self.merge_facts(&root, w, &tip, &mut inputs).await;
            inputs.branch_tip = Some(tip);
        }
        Ok(inputs)
    }

    /// Whether the branch's commits are contained in its base reference or
    /// the project's current HEAD.
    async fn merge_facts(&self, root: &Path, w: &WorkspaceRecord, tip: &str, inputs: &mut CleanupInputs) {
        let git = &self.inner.git;
        if w.base_commit.as_deref() == Some(tip) {
            inputs.unmerged_commits = Some(0);
            return;
        }
        let mut candidates = vec!["HEAD".to_owned()];
        if let Some(base) = w.base_ref.as_deref().filter(|b| *b != "HEAD") {
            candidates.insert(0, base.to_owned());
        }
        for candidate in &candidates {
            let Ok(target) = git.resolve_commit(root, candidate).await else { continue };
            if git.is_ancestor(root, tip, &target).await.unwrap_or(false) {
                inputs.merged_into = Some(candidate.clone());
                inputs.unmerged_commits = Some(0);
                return;
            }
        }
        let compared = candidates[0].clone();
        if let Ok(target) = git.resolve_commit(root, &compared).await {
            inputs.unmerged_commits = git.count_commits(root, &target, tip).await.ok();
        }
        inputs.compared_with = Some(compared);
    }

    pub async fn cleanup_check(&self, id: uuid::Uuid) -> Result<(Workspace, CleanupReport), ProtocolError> {
        let w = self.record(id).await?;
        let report = evaluate_cleanup(&self.cleanup_inputs(&w).await?);
        Ok((self.to_wire(w).await?, report))
    }

    pub async fn cleanup(
        &self,
        id: uuid::Uuid,
        confirmation: &str,
    ) -> Result<(Workspace, CleanupReport), ProtocolError> {
        let w = self.record(id).await?;
        let lock = self.repo_lock(w.project_id);
        let _guard = lock.lock().await;
        let w = self.record(id).await?;
        let report = evaluate_cleanup(&self.cleanup_inputs(&w).await?);
        let codes = |f: &[switchyard_workspaces::CleanupFinding]| f.iter().map(|x| x.code.clone()).collect();
        let request = DomainEvent::WorkspaceCleanupRequested {
            allowed: report.allowed,
            blockers: codes(&report.blockers),
            warnings: codes(&report.warnings),
            will_remove_directory: report.will_remove_directory,
            tree_fingerprint: report.tree_fingerprint.clone(),
            entries: report.entries_total,
        };
        let data = json!({ "workspace_id": id, "report": report });

        if !report.allowed {
            self.record_events(&w, vec![request]).await;
            let dirty = report
                .blockers
                .iter()
                .any(|b| b.code == "uncommitted_changes" || b.code == "untracked_files");
            let summary: Vec<&str> = report.blockers.iter().map(|b| b.message.as_str()).collect();
            let code = if dirty { ErrorCode::DirtyWorkspace } else { ErrorCode::UnsafeCleanup };
            return Err(err(
                code,
                format!("cleanup refused; the workspace is preserved. {}", summary.join(" ")),
            )
            .with_data(data));
        }
        if confirmation != report.confirmation {
            return Err(err(
                ErrorCode::UnsafeCleanup,
                "the workspace changed since cleanup was reviewed; review the new report and confirm again",
            )
            .with_data(data));
        }

        // Persist the request before touching the filesystem.
        let pending = self
            .set_status(
                id,
                WorkspaceStatus::CleanupPending,
                WorkspaceChanges { status_reason: Some(None), activity: true, ..Default::default() },
                vec![request],
            )
            .await?;
        tracing::info!(workspace_id = %id, path = %pending.path, "cleaning up workspace");
        let root = self.project_root(w.project_id).await?;
        let path = PathBuf::from(&pending.path);
        let had_directory = real_dir(&path).await;
        if !self.inner.cleanup_pause.is_zero() {
            // Test hook: widens the window between confirmation and removal.
            tokio::time::sleep(self.inner.cleanup_pause).await;
        }
        // Final check, immediately before anything is deleted: the tree must
        // be exactly what the operator reviewed.
        if had_directory {
            let now = scan_tree(&path).await;
            let current = format!("t1-{:016x}", now.fingerprint);
            if !now.problems.is_empty() || Some(&current) != report.tree_fingerprint.as_ref() {
                let detail =
                    "files in the worktree changed after the cleanup was reviewed; nothing was deleted. \
                              Review the workspace again before cleaning it up."
                        .to_owned();
                self.set_status(
                    id,
                    WorkspaceStatus::Archived,
                    WorkspaceChanges {
                        status_reason: Some(Some(format!("cleanup_failed: {detail}"))),
                        ..Default::default()
                    },
                    vec![DomainEvent::WorkspaceCleanupFailed {
                        reason: "changed_before_removal".into(),
                        detail: Some(detail.clone()),
                    }],
                )
                .await?;
                return Err(err(ErrorCode::UnsafeCleanup, detail).with_data(json!({ "workspace_id": id })));
            }
        }
        let registered = self
            .inner
            .git
            .list_worktrees(&root)
            .await
            .map(|l| l.iter().any(|e| e.path == path))
            .unwrap_or(true);
        if registered && let Err(e) = self.inner.git.remove_worktree(&root, &path).await {
            let detail = e.to_string();
            tracing::error!(workspace_id = %id, %detail, "workspace cleanup failed");
            let preserved = self
                .set_status(
                    id,
                    WorkspaceStatus::Archived,
                    WorkspaceChanges {
                        status_reason: Some(Some(format!("cleanup_failed: {detail}"))),
                        ..Default::default()
                    },
                    vec![DomainEvent::WorkspaceCleanupFailed {
                        reason: "git_failed".into(),
                        detail: Some(detail.clone()),
                    }],
                )
                .await?;
            return Err(err(
                ErrorCode::UnsafeCleanup,
                format!("cleanup failed and the workspace was preserved: {detail}"),
            )
            .with_data(json!({ "workspace_id": preserved.id, "report": report })));
        }
        if real_dir(&path).await {
            let detail = "the worktree directory still exists after git worktree remove".to_owned();
            self.set_status(
                id,
                WorkspaceStatus::Archived,
                WorkspaceChanges {
                    status_reason: Some(Some(format!("cleanup_failed: {detail}"))),
                    ..Default::default()
                },
                vec![DomainEvent::WorkspaceCleanupFailed {
                    reason: "directory_remains".into(),
                    detail: Some(detail.clone()),
                }],
            )
            .await?;
            return Err(err(ErrorCode::UnsafeCleanup, detail));
        }
        // The per-project directory is removed only if empty.
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir(parent);
        }
        let removed = self
            .set_status(
                id,
                WorkspaceStatus::Removed,
                WorkspaceChanges::default(),
                vec![DomainEvent::WorkspaceRemoved {
                    path: pending.path.clone(),
                    branch: report.branch.clone(),
                    directory_removed: had_directory,
                    branch_kept: true,
                }],
            )
            .await?;
        Ok((self.to_wire(removed).await?, report))
    }

    async fn record_events(&self, w: &WorkspaceRecord, events: Vec<DomainEvent>) {
        let ctx = super::context(w);
        let id = w.id;
        let now = switchyard_events::now_ms();
        let result = self
            .commit(move |tx| {
                switchyard_storage::workspaces::touch(tx, id, now)?;
                let drafts = events
                    .into_iter()
                    .map(|e| crate::journal::draft(e, &ctx))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(((), drafts))
            })
            .await;
        if let Err(e) = result {
            tracing::error!(workspace_id = %id, ?e, "cannot journal workspace event");
        }
    }
}
