//! Startup reconciliation of workspace records with the worktrees on disk.
//!
//! Conservative by construction: it only changes statuses and records
//! reasons. It never deletes, moves, checks out or overwrites anything, and
//! never starts agents. Runs after agent sessions from the previous run were
//! marked failed, so no session is attached to any workspace.

use std::path::PathBuf;

use switchyard_events::DomainEvent;
use switchyard_protocol::ProtocolError;
use switchyard_storage::workspaces::{self, WorkspaceChanges};
use switchyard_storage::{StorageError, WorkspaceRecord, projects};
use switchyard_workspaces::WorkspaceStatus;

use super::inspect::{Problem, real_dir};
use super::{WorkspaceService, observed, settled};
use crate::handlers::errors;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReconcileSummary {
    pub checked: usize,
    pub failed: usize,
    pub recovered: usize,
}

impl WorkspaceService {
    pub async fn reconcile_on_startup(&self) -> Result<ReconcileSummary, ProtocolError> {
        let records = self
            .inner
            .storage
            .call(|c| workspaces::list_unremoved_worktrees(c))
            .await
            .map_err(errors::storage)?;
        let mut summary = ReconcileSummary::default();
        for w in records {
            summary.checked += 1;
            match self.reconcile_one(&w).await {
                Ok(after) => {
                    if after.status == WorkspaceStatus::Failed && w.status != WorkspaceStatus::Failed {
                        summary.failed += 1;
                    }
                    if w.status == WorkspaceStatus::Provisioning && after.status != WorkspaceStatus::Failed {
                        summary.recovered += 1;
                    }
                }
                Err(e) => tracing::error!(workspace_id = %w.id, ?e, "cannot reconcile workspace"),
            }
        }
        if summary.checked > 0 {
            tracing::info!(?summary, "reconciled workspaces");
        }
        Ok(summary)
    }

    async fn reconcile_one(&self, w: &WorkspaceRecord) -> Result<WorkspaceRecord, ProtocolError> {
        let project_id = w.project_id;
        let root = match self.inner.storage.call(move |c| projects::get(c, project_id)).await {
            Ok(p) => PathBuf::from(p.root_path),
            Err(StorageError::NotFound(_)) => return Ok(w.clone()),
            Err(e) => return Err(errors::storage(e)),
        };
        match w.status {
            // Archived and failed workspaces wait for the operator.
            WorkspaceStatus::Archived | WorkspaceStatus::Failed | WorkspaceStatus::Removed => Ok(w.clone()),
            WorkspaceStatus::CleanupPending => self.reconcile_cleanup(w, &root).await,
            WorkspaceStatus::Provisioning => {
                // Finish only if the worktree is complete and exactly as
                // recorded; otherwise leave everything for inspection.
                let inspection = self.inspect(w, &root).await;
                match inspection.problem {
                    None => self.settle(w, None).await,
                    Some(p) => {
                        let p = Problem {
                            code: "provisioning_interrupted",
                            detail: format!("Core stopped while creating this workspace ({})", p.reason()),
                            ..p
                        };
                        self.fail(w, &p).await
                    }
                }
            }
            WorkspaceStatus::Active
            | WorkspaceStatus::Ready
            | WorkspaceStatus::Dirty
            | WorkspaceStatus::Completed
            | WorkspaceStatus::Integrating => {
                let inspection = self.inspect(w, &root).await;
                match &inspection.problem {
                    Some(p) => self.fail(w, p).await,
                    None => self.settle(w, w.status_reason.clone()).await,
                }
            }
        }
    }

    /// Updates the Git observation; `active` (no session survives a Core
    /// restart) and settled workspaces follow Git status.
    async fn settle(
        &self,
        w: &WorkspaceRecord,
        reason: Option<String>,
    ) -> Result<WorkspaceRecord, ProtocolError> {
        let Some(status) = self.status_of(w).await else { return Ok(w.clone()) };
        let to = match w.status {
            // No Core operation survives a restart: `integrating` settles.
            WorkspaceStatus::Completed => w.status,
            _ => settled(&status),
        };
        self.set_status(w.id, to, observed(&status, reason), Vec::new()).await
    }

    /// A cleanup was interrupted. If the worktree is fully gone the cleanup
    /// is recorded as finished; otherwise the workspace returns to
    /// `archived` with a reason and its files are left alone.
    async fn reconcile_cleanup(
        &self,
        w: &WorkspaceRecord,
        root: &std::path::Path,
    ) -> Result<WorkspaceRecord, ProtocolError> {
        let path = PathBuf::from(&w.path);
        let exists = real_dir(&path).await;
        let registered = match self.inner.git.list_worktrees(root).await {
            Ok(list) => Some(list.iter().any(|e| e.path == path)),
            Err(_) => None,
        };
        if !exists && registered == Some(false) {
            return self
                .set_status(
                    w.id,
                    WorkspaceStatus::Removed,
                    WorkspaceChanges::default(),
                    vec![DomainEvent::WorkspaceRemoved {
                        path: w.path.clone(),
                        branch: w.branch.clone().unwrap_or_default(),
                        directory_removed: true,
                        branch_kept: true,
                    }],
                )
                .await;
        }
        self.set_status(
            w.id,
            WorkspaceStatus::Archived,
            WorkspaceChanges {
                status_reason: Some(Some(
                    "cleanup_interrupted: Core stopped during cleanup; the workspace was preserved. Request \
                     cleanup again to finish it."
                        .into(),
                )),
                ..Default::default()
            },
            vec![DomainEvent::WorkspaceCleanupFailed { reason: "cleanup_interrupted".into(), detail: None }],
        )
        .await
    }
}
