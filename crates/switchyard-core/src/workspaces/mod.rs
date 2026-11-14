//! Workspaces owned by Core: isolated Git worktrees of a project.
//!
//! [`WorkspaceService`] is the only place that creates, validates, changes
//! the status of, or removes workspaces. Every status change is validated
//! against the lifecycle in `switchyard_workspaces` and written together with
//! its journal events in one transaction. Git work happens outside database
//! transactions, serialized per repository.
//!
//! Isolation is of source trees only. An agent's approved commands run with
//! the user's privileges and can reach files outside their workspace.

mod cleanup;
pub(crate) mod fingerprint;
mod inspect;
mod reconcile;

use std::collections::HashMap;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::json;
use switchyard_events::{DomainEvent, EventBus, EventContext, EventDraft, new_id, now_ms};
use switchyard_git::{Git, GitError, GitStatus};
use switchyard_protocol::model::{Workspace, WorkspaceGitStatus};
use switchyard_protocol::{ErrorCode, ProtocolError};
use switchyard_storage::workspaces::{self, WorkspaceChanges};
use switchyard_storage::{Storage, StorageError, Transaction, WorkspaceRecord, projects};
use switchyard_workspaces::{WorkspaceKind, WorkspaceStatus, branch_name, validate_base_ref, validate_name};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

pub(crate) use inspect::Problem;
use inspect::real_dir;

use crate::handlers::errors;
use crate::journal::{self, draft};

#[derive(Clone)]
pub(crate) struct WorkspaceService {
    inner: Arc<Inner>,
}

struct Inner {
    storage: Storage,
    bus: EventBus,
    git: Git,
    /// Canonical Core-owned directory holding all worktrees.
    root: PathBuf,
    /// Serializes Git mutations per repository (project id).
    repo_locks: Mutex<HashMap<Uuid, Arc<AsyncMutex<()>>>>,
    /// The status a workspace had before an integration step (restored after it).
    before_integration: Mutex<HashMap<Uuid, WorkspaceStatus>>,
    /// Test hook: pause between confirming a cleanup and the final check.
    cleanup_pause: std::time::Duration,
}

pub(crate) fn err(code: ErrorCode, message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(code, message)
}

fn context(w: &WorkspaceRecord) -> EventContext {
    EventContext { project_id: Some(w.project_id), workspace_id: Some(w.id), ..Default::default() }
}

/// `workspace.state_changed` for a transition (none if the status is
/// unchanged).
pub(crate) fn state_change_drafts(
    before: &WorkspaceRecord,
    after: &WorkspaceRecord,
) -> Result<Vec<EventDraft>, StorageError> {
    if before.status == after.status {
        return Ok(Vec::new());
    }
    Ok(vec![draft(
        DomainEvent::WorkspaceStateChanged {
            from: before.status.as_str().into(),
            to: after.status.as_str().into(),
            reason: after.status_reason.clone(),
        },
        &context(after),
    )?])
}

/// Changes recording a fresh Git status observation.
fn observed(status: &GitStatus, reason: Option<String>) -> WorkspaceChanges {
    WorkspaceChanges {
        status_reason: Some(reason),
        dirty: Some(Some(!status.is_clean())),
        git_checked_at_ms: Some(Some(now_ms())),
        activity: false,
    }
}

fn settled(status: &GitStatus) -> WorkspaceStatus {
    if status.is_clean() { WorkspaceStatus::Ready } else { WorkspaceStatus::Dirty }
}

impl WorkspaceService {
    /// `root` must exist; it is canonicalized here.
    pub fn new(
        storage: Storage,
        bus: EventBus,
        git: Git,
        root: &Path,
        cleanup_pause: std::time::Duration,
    ) -> std::io::Result<Self> {
        let root = std::fs::canonicalize(root)?;
        Ok(Self {
            inner: Arc::new(Inner {
                storage,
                bus,
                git,
                root,
                repo_locks: Mutex::default(),
                before_integration: Mutex::default(),
                cleanup_pause,
            }),
        })
    }

    pub(crate) fn git(&self) -> &Git {
        &self.inner.git
    }

    /// The tree of everything in the workspace's worktree (tracked files as
    /// on disk plus untracked, non-ignored files), computed with a private
    /// index so neither the worktree nor its index changes.
    pub(crate) async fn content_tree(&self, w: &WorkspaceRecord) -> Result<String, switchyard_git::GitError> {
        let index = self.inner.root.join(format!(".switchyard-index-{}", switchyard_events::new_id()));
        self.inner.git.content_tree(Path::new(&w.path), &index).await
    }

    /// Marks a settled, session-free workspace `integrating` while Core runs
    /// an integration step in it. Fails if a session is attached.
    pub(crate) async fn begin_integration_op(&self, id: Uuid) -> Result<WorkspaceRecord, ProtocolError> {
        let now = now_ms();
        let (before, after) = self
            .commit_checked(move |tx| {
                let current = workspaces::get(tx, id)?;
                if current.status == WorkspaceStatus::Integrating {
                    return Ok(Err(err(
                        ErrorCode::InvalidWorkspaceState,
                        "another integration step is running in this workspace",
                    )));
                }
                if workspaces::live_session(tx, id)?.is_some() {
                    return Ok(Err(err(
                        ErrorCode::WorkspaceOccupied,
                        "a session is attached to this workspace",
                    )));
                }
                if !current.status.can_transition(WorkspaceStatus::Integrating) {
                    return Ok(Err(err(
                        ErrorCode::InvalidWorkspaceState,
                        format!("workspace is {}; Core cannot work in it", current.status),
                    )));
                }
                let (before, after) = workspaces::transition(
                    tx,
                    id,
                    WorkspaceStatus::Integrating,
                    &WorkspaceChanges::default(),
                    now,
                )?;
                let drafts = state_change_drafts(&before, &after)?;
                Ok(Ok(((before.status, after), drafts)))
            })
            .await?;
        self.inner
            .before_integration
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, before);
        Ok(after)
    }

    /// Returns an `integrating` workspace to `completed` if the operator had
    /// marked it so, otherwise to `ready`/`dirty` from Git status (`dirty`
    /// if the status cannot be read).
    pub(crate) async fn end_integration_op(&self, id: Uuid) {
        let before = self
            .inner
            .before_integration
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id);
        let result: Result<(), ProtocolError> = async {
            let w = self.record(id).await?;
            if w.status != WorkspaceStatus::Integrating {
                return Ok(());
            }
            let to = match (before, self.status_of(&w).await) {
                (Some(WorkspaceStatus::Completed), Some(st)) => {
                    (WorkspaceStatus::Completed, observed(&st, None))
                }
                (Some(WorkspaceStatus::Completed), None) => {
                    (WorkspaceStatus::Completed, WorkspaceChanges::default())
                }
                (_, Some(st)) => (settled(&st), observed(&st, None)),
                (_, None) => (WorkspaceStatus::Dirty, WorkspaceChanges::default()),
            };
            self.set_status(id, to.0, to.1, Vec::new()).await.map(|_| ())
        }
        .await;
        if let Err(e) = result {
            tracing::error!(workspace_id = %id, ?e, "cannot settle the workspace after an integration step");
        }
    }

    pub(crate) fn repo_lock(&self, project_id: Uuid) -> Arc<AsyncMutex<()>> {
        let mut locks = self.inner.repo_locks.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(locks.entry(project_id).or_default())
    }

    async fn commit<R, F>(&self, work: F) -> Result<R, ProtocolError>
    where
        F: FnOnce(&Transaction<'_>) -> Result<(R, Vec<EventDraft>), StorageError> + Send + 'static,
        R: Send + 'static,
    {
        journal::commit(&self.inner.storage, &self.inner.bus, work).await
    }

    async fn commit_checked<R, F>(&self, work: F) -> Result<R, ProtocolError>
    where
        F: FnOnce(&Transaction<'_>) -> Result<Result<(R, Vec<EventDraft>), ProtocolError>, StorageError>
            + Send
            + 'static,
        R: Send + 'static,
    {
        journal::commit_checked(&self.inner.storage, &self.inner.bus, work).await
    }

    // --- reads ---------------------------------------------------------------

    pub async fn record(&self, id: Uuid) -> Result<WorkspaceRecord, ProtocolError> {
        match self.inner.storage.call(move |c| workspaces::find(c, id)).await.map_err(errors::storage)? {
            Some(w) => Ok(w),
            None => Err(err(ErrorCode::WorkspaceNotFound, format!("workspace {id} not found"))),
        }
    }

    pub(crate) async fn project_root(&self, project_id: Uuid) -> Result<PathBuf, ProtocolError> {
        match self.inner.storage.call(move |c| projects::get(c, project_id)).await {
            Ok(p) => Ok(PathBuf::from(p.root_path)),
            Err(StorageError::NotFound(_)) => {
                Err(err(ErrorCode::ProjectNotFound, format!("project {project_id} not found")))
            }
            Err(e) => Err(errors::storage(e)),
        }
    }

    pub async fn to_wire(&self, w: WorkspaceRecord) -> Result<Workspace, ProtocolError> {
        let id = w.id;
        let active_session_id = self
            .inner
            .storage
            .call(move |c| workspaces::live_session(c, id))
            .await
            .map_err(errors::storage)?;
        let path_exists = real_dir(Path::new(&w.path)).await;
        Ok(Workspace {
            id: w.id,
            project_id: w.project_id,
            kind: w.kind,
            name: w.name,
            path: w.path,
            branch: w.branch,
            base_ref: w.base_ref,
            base_commit: w.base_commit,
            status: w.status,
            status_reason: w.status_reason,
            dirty: w.dirty,
            git_checked_at_ms: w.git_checked_at_ms,
            created_at_ms: w.created_at_ms,
            updated_at_ms: w.updated_at_ms,
            last_activity_at_ms: w.last_activity_at_ms,
            archived_at_ms: w.archived_at_ms,
            active_session_id,
            path_exists,
        })
    }

    pub async fn get(&self, id: Uuid) -> Result<Workspace, ProtocolError> {
        let w = self.record(id).await?;
        self.to_wire(w).await
    }

    pub async fn list(
        &self,
        project_id: Uuid,
        include_archived: bool,
    ) -> Result<Vec<Workspace>, ProtocolError> {
        self.project_root(project_id).await?;
        let records = self
            .inner
            .storage
            .call(move |c| workspaces::list(c, project_id, include_archived))
            .await
            .map_err(errors::storage)?;
        let mut out = Vec::with_capacity(records.len());
        for w in records {
            out.push(self.to_wire(w).await?);
        }
        Ok(out)
    }

    // --- status changes ------------------------------------------------------

    /// Validated transition plus its events, atomically. The status check
    /// runs inside the transaction, so concurrent operations cannot both
    /// succeed from the same starting state.
    pub(crate) async fn set_status(
        &self,
        id: Uuid,
        to: WorkspaceStatus,
        changes: WorkspaceChanges,
        extra: Vec<DomainEvent>,
    ) -> Result<WorkspaceRecord, ProtocolError> {
        self.set_status_with(id, to, changes, extra, false).await
    }

    /// [`Self::set_status`]; with `unoccupied`, also fails (without writing)
    /// if a live session is attached at commit time.
    async fn set_status_with(
        &self,
        id: Uuid,
        to: WorkspaceStatus,
        changes: WorkspaceChanges,
        extra: Vec<DomainEvent>,
        unoccupied: bool,
    ) -> Result<WorkspaceRecord, ProtocolError> {
        let now = now_ms();
        self.commit_checked(move |tx| {
            let current = workspaces::get(tx, id)?;
            if unoccupied && workspaces::live_session(tx, id)?.is_some() {
                return Ok(Err(err(ErrorCode::WorkspaceOccupied, "a session is attached to this workspace")));
            }
            if !current.status.can_transition(to) {
                return Ok(Err(err(
                    ErrorCode::InvalidWorkspaceState,
                    format!("workspace is {}; cannot become {to}", current.status),
                )
                .with_data(json!({ "workspace_id": id, "status": current.status }))));
            }
            let (before, after) = workspaces::transition(tx, id, to, &changes, now)?;
            let mut drafts = state_change_drafts(&before, &after)?;
            let ctx = context(&after);
            for e in extra {
                drafts.push(draft(e, &ctx)?);
            }
            Ok(Ok((after, drafts)))
        })
        .await
    }

    /// Marks a workspace failed with `problem` (no-op if it already is).
    async fn fail(&self, w: &WorkspaceRecord, problem: &Problem) -> Result<WorkspaceRecord, ProtocolError> {
        let reason = problem.reason();
        if w.status == WorkspaceStatus::Failed && w.status_reason.as_deref() == Some(reason.as_str()) {
            return Ok(w.clone());
        }
        tracing::warn!(workspace_id = %w.id, %reason, "workspace failed validation");
        self.set_status(
            w.id,
            WorkspaceStatus::Failed,
            WorkspaceChanges { status_reason: Some(Some(reason)), ..Default::default() },
            Vec::new(),
        )
        .await
    }

    fn reject_legacy(w: &WorkspaceRecord) -> Result<(), ProtocolError> {
        if w.kind == WorkspaceKind::LegacyCheckout {
            return Err(err(
                ErrorCode::LegacyWorkspace,
                "this record is the project checkout used before workspace isolation; it accepts no new \
                 sessions and is never modified or cleaned up by Switchyard",
            )
            .with_data(json!({ "workspace_id": w.id })));
        }
        Ok(())
    }

    // --- create --------------------------------------------------------------

    /// Creates a workspace: record (`provisioning`) → worktree on a new
    /// branch → identity verification → `ready`. Any failure leaves the
    /// record `failed` with a reason and the files untouched for inspection.
    pub async fn create(
        &self,
        project_id: Uuid,
        name: Option<String>,
        base_ref: Option<String>,
    ) -> Result<Workspace, ProtocolError> {
        let invalid = |e: switchyard_workspaces::NameError| err(ErrorCode::InvalidParams, e.to_string());
        let name = name.as_deref().map(validate_name).transpose().map_err(invalid)?;
        let base_ref =
            base_ref.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("HEAD").to_owned();
        validate_base_ref(&base_ref).map_err(invalid)?;
        let project_root = self.project_root(project_id).await?;

        let lock = self.repo_lock(project_id);
        let _guard = lock.lock().await;
        let git = &self.inner.git;
        let repo = git.resolve_repository_root(&project_root).await.map_err(errors::git_registered)?;
        if repo.path != project_root {
            return Err(err(
                ErrorCode::RepositoryUnavailable,
                format!("{} is no longer the repository root", project_root.display()),
            ));
        }
        let base_commit = match git.resolve_commit(&project_root, &base_ref).await {
            Ok(c) => c,
            Err(GitError::UnknownRevision(_)) if base_ref == "HEAD" => {
                return Err(err(ErrorCode::InvalidParams, "the repository has no commits to branch from"));
            }
            Err(GitError::UnknownRevision(r)) => {
                return Err(err(ErrorCode::InvalidParams, format!("{r:?} does not name a commit")));
            }
            Err(e) => return Err(errors::git_registered(e)),
        };

        let id = new_id();
        let path = switchyard_workspaces::worktree_path(&self.inner.root, project_id, id);
        if path.starts_with(&project_root) || project_root.starts_with(&self.inner.root) {
            return Err(err(
                ErrorCode::WorktreeCreationFailed,
                format!(
                    "the workspaces directory {} overlaps the repository {}; move the Switchyard data directory",
                    self.inner.root.display(),
                    project_root.display()
                ),
            ));
        }
        let path_str = path
            .to_str()
            .ok_or_else(|| err(ErrorCode::WorktreeCreationFailed, "workspace path is not valid UTF-8"))?
            .to_owned();
        let branch = branch_name(id);
        let now = now_ms();
        let record = {
            let (path_str, branch, base_ref, base_commit) =
                (path_str.clone(), branch.clone(), base_ref.clone(), base_commit.clone());
            self.commit(move |tx| {
                let name = match name {
                    Some(n) => n,
                    None => format!("Workspace {}", workspaces::count_for_project(tx, project_id)? + 1),
                };
                let record = WorkspaceRecord {
                    id,
                    project_id,
                    kind: WorkspaceKind::Worktree,
                    name: name.clone(),
                    path: path_str.clone(),
                    branch: Some(branch.clone()),
                    base_ref: Some(base_ref.clone()),
                    base_commit: Some(base_commit.clone()),
                    status: WorkspaceStatus::Provisioning,
                    status_reason: None,
                    dirty: None,
                    git_checked_at_ms: None,
                    created_at_ms: now,
                    updated_at_ms: now,
                    last_activity_at_ms: now,
                    archived_at_ms: None,
                };
                workspaces::insert(tx, &record)?;
                let event =
                    DomainEvent::WorkspaceCreated { name, path: path_str, branch, base_ref, base_commit };
                Ok((record.clone(), vec![draft(event, &context(&record))?]))
            })
            .await?
        };
        tracing::info!(workspace_id = %id, %project_id, path = %path.display(), "provisioning workspace");

        let created = self.provision(&record, &project_root).await;
        match created {
            Ok(w) => self.to_wire(w).await,
            Err((problem, error)) => {
                if let Err(e) = self.fail(&record, &problem).await {
                    tracing::error!(workspace_id = %id, ?e, "cannot record provisioning failure");
                }
                Err(error.with_data(json!({ "workspace_id": id, "reason": problem.code })))
            }
        }
    }

    async fn provision(
        &self,
        record: &WorkspaceRecord,
        project_root: &Path,
    ) -> Result<WorkspaceRecord, (Problem, ProtocolError)> {
        let fail = |code: ErrorCode, reason: &'static str, detail: String| {
            (Problem { error: code, code: reason, detail: detail.clone() }, err(code, detail))
        };
        let path = PathBuf::from(&record.path);
        let (branch, base) =
            (record.branch.clone().unwrap_or_default(), record.base_commit.clone().unwrap_or_default());
        if let Some(parent) = path.parent() {
            std::fs::DirBuilder::new().recursive(true).mode(0o700).create(parent).map_err(|e| {
                fail(
                    ErrorCode::WorktreeCreationFailed,
                    "worktree_add_failed",
                    format!("cannot create {}: {e}", parent.display()),
                )
            })?;
        }
        if let Err(e) = self.inner.git.add_worktree(project_root, &path, &branch, &base).await {
            return Err(match e {
                GitError::BranchExists(b) => fail(
                    ErrorCode::BranchCollision,
                    "branch_collision",
                    format!("branch {b} already exists in the repository; it is not reused"),
                ),
                GitError::PathExists(p) => fail(
                    ErrorCode::WorktreeCreationFailed,
                    "path_exists",
                    format!("{} already exists; it is not overwritten", p.display()),
                ),
                other => fail(ErrorCode::WorktreeCreationFailed, "worktree_add_failed", other.to_string()),
            });
        }
        let inspection = self.inspect(record, project_root).await;
        if let Some(p) = inspection.problem {
            return Err((p.clone(), err(p.error, p.detail)));
        }
        if inspection.identity.as_ref().and_then(|i| i.head_oid.as_deref()) != Some(base.as_str()) {
            return Err(fail(
                ErrorCode::WorktreeIdentityMismatch,
                "base_mismatch",
                "the new worktree is not at the recorded base commit".into(),
            ));
        }
        let status = self.status_of(record).await;
        let (to, changes) = match &status {
            Some(s) => (settled(s), observed(s, None)),
            None => (WorkspaceStatus::Ready, WorkspaceChanges::default()),
        };
        let w = self
            .set_status(record.id, to, changes, Vec::new())
            .await
            .map_err(|e| (Problem { error: e.code, code: "internal", detail: e.message.clone() }, e))?;
        tracing::info!(workspace_id = %w.id, status = %w.status, "workspace ready");
        Ok(w)
    }

    // --- inspection and operator actions --------------------------------------

    /// Git status of the workspace. A settled workspace follows it
    /// (`ready` ↔ `dirty`).
    pub async fn git_status(&self, id: Uuid) -> Result<WorkspaceGitStatus, ProtocolError> {
        let w = self.record(id).await?;
        Self::reject_legacy(&w)?;
        if !real_dir(Path::new(&w.path)).await {
            return Err(err(ErrorCode::WorktreeMissing, format!("{} does not exist", w.path)));
        }
        let status = self.inner.git.status(Path::new(&w.path)).await.map_err(errors::git_registered)?;
        let mut commits_since_base = None;
        if let (Some(base), Some(head)) = (&w.base_commit, &status.head_oid) {
            commits_since_base = self.inner.git.count_commits(Path::new(&w.path), base, head).await.ok();
        }
        // Record the observation; a settled workspace follows it.
        let to = if w.status.tracks_git_status() { settled(&status) } else { w.status };
        let changes = observed(&status, w.status_reason.clone());
        if let Err(e) = self.set_status(id, to, changes, Vec::new()).await {
            tracing::debug!(workspace_id = %id, ?e, "workspace changed while reading status");
        }
        Ok(WorkspaceGitStatus {
            workspace_id: id,
            on_expected_branch: status.branch.is_some() && status.branch == w.branch,
            expected_branch: w.branch,
            clean: status.is_clean(),
            branch: status.branch,
            head_oid: status.head_oid,
            detached: status.detached,
            base_commit: w.base_commit,
            commits_since_base,
            staged: status.staged,
            unstaged: status.unstaged,
            untracked: status.untracked,
            conflicted: status.conflicted,
            checked_at_ms: now_ms(),
        })
    }

    /// Refreshes the recorded Git observation (`dirty`) after files changed,
    /// e.g. while a session is active. A settled workspace follows it
    /// (`ready` ⇄ `dirty`); other statuses are kept. Best effort.
    pub async fn observe(&self, id: Uuid) {
        let Ok(w) = self.record(id).await else { return };
        let observable = matches!(
            w.status,
            WorkspaceStatus::Active
                | WorkspaceStatus::Ready
                | WorkspaceStatus::Dirty
                | WorkspaceStatus::Completed
        );
        if !observable || w.kind != WorkspaceKind::Worktree || !real_dir(Path::new(&w.path)).await {
            return;
        }
        let Some(status) = self.status_of(&w).await else { return };
        if w.dirty == Some(!status.is_clean()) {
            return;
        }
        let to = if w.status.tracks_git_status() { settled(&status) } else { w.status };
        if let Err(e) = self.set_status(id, to, observed(&status, w.status_reason.clone()), Vec::new()).await
        {
            tracing::debug!(workspace_id = %id, ?e, "workspace changed while observing git status");
        }
    }

    /// Re-validates the worktree. A failed workspace that validates again
    /// returns to `ready`/`dirty`; a settled one that no longer validates
    /// becomes `failed`. Archived and active workspaces keep their status.
    pub async fn verify(&self, id: Uuid) -> Result<Workspace, ProtocolError> {
        let w = self.record(id).await?;
        Self::reject_legacy(&w)?;
        let lock = self.repo_lock(w.project_id);
        let _guard = lock.lock().await;
        let root = self.project_root(w.project_id).await?;
        let inspection = self.inspect(&w, &root).await;
        let updated = match (&inspection.problem, w.status) {
            (Some(p), s)
                if s.tracks_git_status()
                    || s == WorkspaceStatus::Completed
                    || s == WorkspaceStatus::Failed =>
            {
                self.fail(&w, p).await?
            }
            (Some(_), _) => w,
            (None, s)
                if s.tracks_git_status()
                    || s == WorkspaceStatus::Failed
                    || s == WorkspaceStatus::Completed =>
            {
                match self.status_of(&w).await {
                    Some(st) => {
                        let to = if s == WorkspaceStatus::Completed { s } else { settled(&st) };
                        self.set_status(id, to, observed(&st, None), Vec::new()).await?
                    }
                    None => w,
                }
            }
            (None, _) => w,
        };
        self.to_wire(updated).await
    }

    pub async fn archive(&self, id: Uuid) -> Result<Workspace, ProtocolError> {
        let w = self.record(id).await?;
        Self::reject_legacy(&w)?;
        let changes = WorkspaceChanges { activity: true, ..Default::default() };
        let w = self.set_status(id, WorkspaceStatus::Archived, changes, Vec::new()).await?;
        self.to_wire(w).await
    }

    /// Returns an archived workspace to work after re-validating it.
    pub async fn restore(&self, id: Uuid) -> Result<Workspace, ProtocolError> {
        let w = self.record(id).await?;
        Self::reject_legacy(&w)?;
        if w.status != WorkspaceStatus::Archived {
            return Err(err(
                ErrorCode::InvalidWorkspaceState,
                format!("workspace is {}, not archived", w.status),
            ));
        }
        let root = self.project_root(w.project_id).await?;
        let inspection = self.inspect(&w, &root).await;
        if let Some(p) = inspection.problem {
            return Err(err(p.error, format!("cannot restore: {}", p.detail)));
        }
        let status =
            self.status_of(&w).await.ok_or_else(|| err(ErrorCode::GitFailed, "cannot read git status"))?;
        let mut changes = observed(&status, None);
        changes.activity = true;
        let w = self.set_status(id, settled(&status), changes, Vec::new()).await?;
        self.to_wire(w).await
    }

    pub async fn complete(&self, id: Uuid) -> Result<Workspace, ProtocolError> {
        let w = self.record(id).await?;
        Self::reject_legacy(&w)?;
        let changes = WorkspaceChanges { activity: true, ..Default::default() };
        let w = self.set_status(id, WorkspaceStatus::Completed, changes, Vec::new()).await?;
        self.to_wire(w).await
    }

    // --- sessions --------------------------------------------------------------

    /// Everything that must hold before an agent may run in a workspace:
    /// it exists, belongs to `project_id` (when given), is a worktree that
    /// accepts sessions, and validates on disk and in Git. Returns the
    /// record; the caller attaches it with [`attach_in_tx`] in the same
    /// transaction that creates or resumes the session.
    pub async fn validate_for_session(
        &self,
        id: Uuid,
        project_id: Option<Uuid>,
    ) -> Result<WorkspaceRecord, ProtocolError> {
        let w = self.record(id).await?;
        if let Some(p) = project_id
            && p != w.project_id
        {
            return Err(err(
                ErrorCode::WorkspaceProjectMismatch,
                format!("workspace {id} belongs to project {}, not {p}", w.project_id),
            )
            .with_data(json!({ "workspace_id": id, "project_id": w.project_id })));
        }
        Self::reject_legacy(&w)?;
        let occupied = self
            .inner
            .storage
            .call(move |c| workspaces::live_session(c, id))
            .await
            .map_err(errors::storage)?
            .is_some();
        check_accepts_session(&w, occupied)?;
        let root = self.project_root(w.project_id).await?;
        let inspection = self.inspect(&w, &root).await;
        if let Some(p) = inspection.problem {
            // Record what was found so the UI shows why; the session is refused.
            if (w.status.accepts_session() || w.status == WorkspaceStatus::Active)
                && let Err(e) = self.fail(&w, &p).await
            {
                tracing::error!(workspace_id = %id, ?e, "cannot record workspace failure");
            }
            return Err(err(p.error, format!("workspace cannot be used: {}", p.detail))
                .with_data(json!({ "workspace_id": id, "reason": p.code })));
        }
        Ok(w)
    }

    /// Returns a workspace to `ready`/`dirty` (or `failed`) after its
    /// session ended. Never deletes anything.
    pub async fn release(&self, id: Uuid) {
        let result: Result<(), ProtocolError> = async {
            let w = self.record(id).await?;
            if w.status != WorkspaceStatus::Active {
                return Ok(());
            }
            let live = self
                .inner
                .storage
                .call(move |c| workspaces::live_session(c, id))
                .await
                .map_err(errors::storage)?;
            if live.is_some() {
                return Ok(());
            }
            self.settle_after_session(&w).await.map(|_| ())
        }
        .await;
        if let Err(e) = result {
            tracing::error!(workspace_id = %id, ?e, "cannot release workspace after its session");
        }
    }

    /// The final transition re-checks, inside its transaction, that no new
    /// session attached meanwhile.
    async fn settle_after_session(&self, w: &WorkspaceRecord) -> Result<WorkspaceRecord, ProtocolError> {
        let root = self.project_root(w.project_id).await?;
        let inspection = self.inspect(w, &root).await;
        let (to, changes) = match (&inspection.problem, self.status_of(w).await) {
            (Some(p), _) => (
                WorkspaceStatus::Failed,
                WorkspaceChanges {
                    status_reason: Some(Some(p.reason())),
                    activity: true,
                    ..Default::default()
                },
            ),
            (None, Some(st)) => (settled(&st), WorkspaceChanges { activity: true, ..observed(&st, None) }),
            (None, None) => {
                (WorkspaceStatus::Ready, WorkspaceChanges { activity: true, ..Default::default() })
            }
        };
        if w.status == WorkspaceStatus::Failed && to == WorkspaceStatus::Failed {
            return Ok(w.clone());
        }
        self.set_status_with(w.id, to, changes, Vec::new(), true).await
    }
}

/// `occupied`: a live session is attached. An `active` workspace without
/// one is a session that just ended and has not been released yet; it
/// accepts the next session.
fn check_accepts_session(w: &WorkspaceRecord, occupied: bool) -> Result<(), ProtocolError> {
    match w.status {
        s if s.accepts_session() && !occupied => Ok(()),
        WorkspaceStatus::Active if !occupied => Ok(()),
        _ if occupied => Err(err(
            ErrorCode::WorkspaceOccupied,
            "another session is already running in this workspace; stop it or use another workspace",
        )
        .with_data(json!({ "workspace_id": w.id }))),
        s => Err(err(ErrorCode::InvalidWorkspaceState, format!("workspace is {s}; it cannot run a session"))
            .with_data(json!({ "workspace_id": w.id, "status": s, "reason": w.status_reason }))),
    }
}

/// Inside a session transaction: marks the workspace `active` for
/// `session_id`. Fails (without writing) if the workspace no longer accepts a
/// session — another start won the race, or it was archived meanwhile.
pub(crate) fn attach_in_tx(
    tx: &Transaction<'_>,
    workspace_id: Uuid,
    now: i64,
) -> Result<Result<Vec<EventDraft>, ProtocolError>, StorageError> {
    let current = workspaces::get(tx, workspace_id)?;
    let occupied = workspaces::live_session(tx, workspace_id)?.is_some();
    if let Err(e) = check_accepts_session(&current, occupied) {
        return Ok(Err(e));
    }
    let changes = WorkspaceChanges { activity: true, ..Default::default() };
    let (before, after) = workspaces::transition(tx, workspace_id, WorkspaceStatus::Active, &changes, now)?;
    Ok(Ok(state_change_drafts(&before, &after)?))
}
