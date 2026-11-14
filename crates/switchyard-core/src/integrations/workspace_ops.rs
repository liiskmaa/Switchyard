//! Changes to the source workspace, each on the operator's explicit request
//! (ADR 0029): commit the agent's work, merge the target into the workspace,
//! abort that merge. The target branch is never changed here. Each step
//! holds the project's repository lock and marks the workspace
//! `integrating`, so no agent session can attach meanwhile.

use std::path::Path;

use switchyard_events::{DomainEvent, now_ms};
use switchyard_git::WorktreeMerge;
use switchyard_integrations::{IntegrationStatus, MergeFact};
use switchyard_protocol::model::Integration;
use switchyard_protocol::{ErrorCode, ProtocolError};
use switchyard_storage::integrations::{self as store, IntegrationUpdate};
use uuid::Uuid;

use super::{IntegrationService, context, git_err, invalid_params, invalid_state, transition};
use crate::journal::{commit_checked, draft};

const MAX_MESSAGE: usize = 5000;

impl IntegrationService {
    async fn record_workspace_change(
        &self,
        id: Uuid,
        action: &str,
        commit: Option<String>,
        files: Vec<String>,
        by: String,
        to: Option<IntegrationStatus>,
    ) -> Result<(), ProtocolError> {
        let action = action.to_owned();
        commit_checked(&self.inner.storage, &self.inner.bus, move |tx| {
            let i = store::get_integration(tx, id)?;
            let mut drafts = vec![draft(
                DomainEvent::IntegrationWorkspaceChanged { integration_id: id, action, commit, files, by },
                &context(&i),
            )?];
            if let Some(to) = to.filter(|t| *t != i.status) {
                let u = IntegrationUpdate { status: Some(to), ..Default::default() };
                transition(tx, &i, u, now_ms(), &mut drafts)?;
            }
            Ok(Ok(((), drafts)))
        })
        .await
    }

    /// Commits everything in the workspace (the agent's uncommitted work,
    /// untracked non-ignored files included) as one commit on its branch.
    /// While resolving, this completes the merge, and refuses if conflict
    /// markers remain.
    pub async fn commit_workspace(
        &self,
        id: Uuid,
        message: String,
        actor: String,
    ) -> Result<Integration, ProtocolError> {
        let message = message.trim().to_owned();
        if message.is_empty() || message.len() > MAX_MESSAGE {
            return Err(invalid_params(format!("the commit message must be 1 to {MAX_MESSAGE} bytes")));
        }
        let _claim = self.claim(id)?;
        let i = self.record(id).await?;
        if !matches!(
            i.status,
            IntegrationStatus::Open
                | IntegrationStatus::Ready
                | IntegrationStatus::Failed
                | IntegrationStatus::Resolving
        ) {
            return Err(invalid_state(format!(
                "the integration is {}; the workspace cannot be committed now",
                i.status
            )));
        }
        let lock = self.inner.workspaces.repo_lock(i.project_id);
        let _repo = lock.lock().await;
        let w = self.inner.workspaces.begin_integration_op(i.workspace_id).await?;
        let result = self.inner.workspaces.git().commit_all(Path::new(&w.path), &message).await;
        self.inner.workspaces.end_integration_op(w.id).await;
        let commit = result.map_err(git_err)?;
        self.record_workspace_change(id, "committed", Some(commit), Vec::new(), actor, None).await?;
        drop(_repo);
        self.prepare(id).await?;
        self.wire(id).await
    }

    /// Merges the current target commit into the workspace (never the other
    /// way round). A clean merge creates a merge commit on the workspace
    /// branch; conflicts leave the merge in progress in the workspace for
    /// the operator, or an agent assigned there, to resolve.
    pub async fn update_from_target(&self, id: Uuid, actor: String) -> Result<Integration, ProtocolError> {
        let _claim = self.claim(id)?;
        let i = self.prepare(id).await?;
        if !matches!(
            i.status,
            IntegrationStatus::Open | IntegrationStatus::Conflicted | IntegrationStatus::Failed
        ) {
            return Err(invalid_state(format!("the integration is {}; nothing to update", i.status)));
        }
        let facts: switchyard_integrations::Facts = serde_json::from_value(i.preflight["facts"].clone())
            .map_err(|e| invalid_state(format!("cannot read the preparation: {e}")))?;
        if matches!(facts.merge, MergeFact::UpToDate) {
            return Err(invalid_state("the workspace already contains the target branch"));
        }
        if facts.uncommitted_changes {
            return Err(invalid_state("commit the workspace's changes before updating it from the target"));
        }
        let lock = self.inner.workspaces.repo_lock(i.project_id);
        let _repo = lock.lock().await;
        // The target as it is now (re-read under the lock).
        let root = self.inner.workspaces.project_root(i.project_id).await?;
        let git = self.inner.workspaces.git().clone();
        let target = git.branch_tip(&root, &i.target_branch).await.map_err(git_err)?.ok_or_else(|| {
            invalid_state(format!("the target branch {} no longer exists", i.target_branch))
        })?;
        let w = self.inner.workspaces.begin_integration_op(i.workspace_id).await?;
        // Re-checked now that no session can attach: nothing uncommitted, nothing in progress.
        let clean = async {
            let head = git.tree_of(Path::new(&w.path), "HEAD").await.ok()?;
            let content = self.inner.workspaces.content_tree(&w).await.ok()?;
            let idle = git.repo_state(Path::new(&w.path)).await.ok()?.is_idle();
            Some(head == content && idle)
        }
        .await;
        if clean != Some(true) {
            self.inner.workspaces.end_integration_op(w.id).await;
            return Err(invalid_state(
                "the workspace changed (uncommitted changes or a Git operation in progress); commit or finish it first",
            ));
        }
        let message = format!(
            "Merge {} into {} (Switchyard integration {})\n\nSwitchyard-Integration: {}\n",
            i.target_branch, i.source_branch, i.id, i.id
        );
        let result = git.merge_into_worktree(Path::new(&w.path), &target, &message).await;
        self.inner.workspaces.end_integration_op(w.id).await;
        match result.map_err(git_err)? {
            WorktreeMerge::Merged(c) => {
                self.record_workspace_change(id, "updated", Some(c), Vec::new(), actor, None).await?;
            }
            WorktreeMerge::UpToDate => {}
            WorktreeMerge::Conflicts(files) => {
                self.record_workspace_change(
                    id,
                    "conflicts",
                    None,
                    files,
                    actor,
                    Some(IntegrationStatus::Resolving),
                )
                .await?;
            }
        }
        drop(_repo);
        self.prepare(id).await?;
        self.wire(id).await
    }

    /// Abandons a merge from the target that is in progress in the
    /// workspace (`git merge --abort`), restoring the workspace to its last
    /// commit. Edits made while resolving are discarded: the operator asked
    /// for exactly that.
    pub async fn abort_resolution(&self, id: Uuid, actor: String) -> Result<Integration, ProtocolError> {
        let _claim = self.claim(id)?;
        let i = self.record(id).await?;
        if i.status != IntegrationStatus::Resolving {
            return Err(invalid_state("no conflict resolution is in progress"));
        }
        let lock = self.inner.workspaces.repo_lock(i.project_id);
        let _repo = lock.lock().await;
        let w = self.inner.workspaces.begin_integration_op(i.workspace_id).await.map_err(|e| {
            if e.code == ErrorCode::WorkspaceOccupied {
                invalid_state("an agent session is running in the workspace; stop it first")
            } else {
                e
            }
        })?;
        let result = self.inner.workspaces.git().merge_abort(Path::new(&w.path)).await;
        self.inner.workspaces.end_integration_op(w.id).await;
        result.map_err(git_err)?;
        self.record_workspace_change(id, "aborted", None, Vec::new(), actor, None).await?;
        drop(_repo);
        self.prepare(id).await?;
        self.wire(id).await
    }
}
