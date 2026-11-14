//! Change integration (Milestone 7, ADRs 0028–0031).
//!
//! An integration delivers one workspace's committed changes into a target
//! branch. Core binds it to an exact source commit and an exact target
//! commit, checks it against facts read from Git ([`facts`]), validates the
//! source commit with the project's commands ([`validate`]), and, only when
//! the operator approves those exact commits, creates a merge commit and
//! moves the target branch in one compare-and-swap step ([`execute`]).
//!
//! Changes to the source workspace (committing agent work, merging the
//! target into it, resolving conflicts) happen only on the operator's
//! request ([`workspace_ops`]); the target branch is never used to resolve
//! anything.

mod execute;
mod facts;
mod inspect;
mod validate;
mod workspace_ops;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use switchyard_events::{DomainEvent, EventBus, EventContext, EventDraft, new_id, now_ms};
use switchyard_integrations::{
    Blocker, IntegrationStatus, MergeFact, ValidationCommand, command_line, validate_command,
};
use switchyard_protocol::methods::{IntegrationsCreateParams, IntegrationsSetCommandParams};
use switchyard_protocol::model::{Integration, IntegrationSettingsInfo, ValidationCommandConfig};
use switchyard_protocol::{ErrorCode, ProtocolError};
use switchyard_storage::integrations::{self as store, CommandRecord, IntegrationRecord, IntegrationUpdate};
use switchyard_storage::{
    Connection, Storage, StorageError, WorkspaceRecord, tasks as task_store, workspaces,
};
use switchyard_tasks::Role;
use tokio::sync::watch;
use uuid::Uuid;

use crate::handlers::errors;
use crate::journal::{commit_checked, draft};
use crate::workspaces::WorkspaceService;

pub(crate) use facts::Prepared;

fn invalid_state(m: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorCode::InvalidState, m)
}

fn invalid_params(m: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorCode::InvalidParams, m)
}

/// Maps Git errors from integration steps to protocol errors.
pub(crate) fn git_err(e: switchyard_git::GitError) -> ProtocolError {
    use switchyard_git::GitError as G;
    match e {
        e @ G::IdentityMissing(_) => invalid_state(e.to_string()),
        e @ G::WorktreeBusy(_) => invalid_state(e.to_string()),
        e @ G::RefChanged { .. } => ProtocolError::new(ErrorCode::Conflict, e.to_string()),
        e @ G::UnknownRevision(_) => invalid_params(e.to_string()),
        other => errors::git_registered(other),
    }
}

pub(crate) fn context(i: &IntegrationRecord) -> EventContext {
    EventContext {
        project_id: Some(i.project_id),
        workspace_id: Some(i.workspace_id),
        task_id: i.task_id,
        ..Default::default()
    }
}

/// The status preparation settles to, from what Git says.
fn status_for(p: &Prepared, merge_in_progress: bool) -> IntegrationStatus {
    if merge_in_progress {
        IntegrationStatus::Resolving
    } else if matches!(p.facts.merge, MergeFact::Conflicts { .. }) {
        IntegrationStatus::Conflicted
    } else if p.blockers.is_empty() {
        IntegrationStatus::Ready
    } else {
        IntegrationStatus::Open
    }
}

pub(crate) fn blockers_of(i: &IntegrationRecord) -> Vec<Blocker> {
    serde_json::from_value(i.preflight["blockers"].clone()).unwrap_or_default()
}

fn to_wire(i: IntegrationRecord, workspace_name: Option<String>) -> Integration {
    Integration {
        blockers: blockers_of(&i),
        id: i.id,
        project_id: i.project_id,
        task_id: i.task_id,
        workspace_id: i.workspace_id,
        workspace_name,
        source_branch: i.source_branch,
        source_commit: i.source_commit,
        target_branch: i.target_branch,
        target_commit: i.target_commit,
        base_commit: i.base_commit,
        status: i.status,
        status_reason: i.status_reason,
        preflight: i.preflight,
        prepared_at_ms: i.prepared_at_ms,
        approved_by: i.approved_by,
        approved_at_ms: i.approved_at_ms,
        merge_commit: i.merge_commit,
        method: i.method,
        failure: i.failure,
        revision: i.revision,
        created_at_ms: i.created_at_ms,
        updated_at_ms: i.updated_at_ms,
        completed_at_ms: i.completed_at_ms,
    }
}

fn wire_from_conn(c: &Connection, i: IntegrationRecord) -> Result<Integration, StorageError> {
    let name = workspaces::find(c, i.workspace_id)?.map(|w| w.name);
    Ok(to_wire(i, name))
}

/// Status change plus its journal event.
pub(crate) fn transition(
    tx: &Connection,
    i: &IntegrationRecord,
    u: IntegrationUpdate,
    now: i64,
    drafts: &mut Vec<EventDraft>,
) -> Result<IntegrationRecord, StorageError> {
    let (before, after) = store::update_integration(tx, i.id, i.status, &u, now)?;
    if before.status != after.status {
        drafts.push(draft(
            DomainEvent::IntegrationStatusChanged {
                integration_id: after.id,
                from: before.status.as_str().into(),
                to: after.status.as_str().into(),
                reason: after.status_reason.clone(),
            },
            &context(&after),
        )?);
    }
    Ok(after)
}

fn command_wire(c: CommandRecord) -> ValidationCommandConfig {
    ValidationCommandConfig {
        command_line: command_line(&c.argv),
        id: c.id,
        name: c.name,
        argv: c.argv,
        timeout_secs: c.timeout_secs,
        enabled: c.enabled,
        position: c.position,
    }
}

/// Removes an integration from the in-memory busy set when dropped.
pub(crate) struct Claim {
    set: Arc<Mutex<HashSet<Uuid>>>,
    id: Uuid,
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.set.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&self.id);
    }
}

#[derive(Clone)]
pub(crate) struct IntegrationService {
    inner: Arc<Inner>,
}

struct Inner {
    storage: Storage,
    bus: EventBus,
    workspaces: WorkspaceService,
    /// Integrations with an operation in progress in this Core.
    busy: Arc<Mutex<HashSet<Uuid>>>,
    /// Cancellation of running validation runs, by integration.
    cancels: Mutex<HashMap<Uuid, watch::Sender<bool>>>,
}

impl IntegrationService {
    pub fn new(storage: Storage, bus: EventBus, workspaces: WorkspaceService) -> Self {
        Self {
            inner: Arc::new(Inner {
                storage,
                bus,
                workspaces,
                busy: Arc::default(),
                cancels: Mutex::default(),
            }),
        }
    }

    /// One operation per integration at a time (approve, validate, workspace
    /// changes). The status checks in the database are the second guard.
    fn claim(&self, id: Uuid) -> Result<Claim, ProtocolError> {
        let mut set = self.inner.busy.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !set.insert(id) {
            return Err(ProtocolError::new(
                ErrorCode::Conflict,
                "another operation on this integration is in progress",
            ));
        }
        Ok(Claim { set: Arc::clone(&self.inner.busy), id })
    }

    async fn record(&self, id: Uuid) -> Result<IntegrationRecord, ProtocolError> {
        self.inner.storage.call(move |c| store::get_integration(c, id)).await.map_err(errors::storage)
    }

    pub async fn wire(&self, id: Uuid) -> Result<Integration, ProtocolError> {
        self.inner
            .storage
            .call(move |c| {
                let i = store::get_integration(c, id)?;
                wire_from_conn(c, i)
            })
            .await
            .map_err(errors::storage)
    }

    pub async fn list(
        &self,
        project_id: Option<Uuid>,
        task_id: Option<Uuid>,
        include_closed: bool,
        limit: u32,
    ) -> Result<Vec<Integration>, ProtocolError> {
        self.inner
            .storage
            .call(move |c| {
                store::list_integrations(c, project_id, task_id, include_closed, limit)?
                    .into_iter()
                    .map(|i| wire_from_conn(c, i))
                    .collect()
            })
            .await
            .map_err(errors::storage)
    }

    // --- create and prepare ----------------------------------------------------

    /// Proposes delivering a workspace (by default the task's implementation
    /// workspace) into a target branch, and prepares it.
    pub async fn create(
        &self,
        p: IntegrationsCreateParams,
        actor: String,
    ) -> Result<Integration, ProtocolError> {
        let (task_id, workspace_id) = (p.task_id, p.workspace_id);
        let workspace_id = match (workspace_id, task_id) {
            (Some(w), _) => w,
            (None, Some(t)) => self
                .inner
                .storage
                .call(move |c| {
                    // The workspace of the task's latest implementation (or investigation).
                    let mut a = task_store::assignments_for_task(c, t)?;
                    a.reverse();
                    Ok(a.iter()
                        .find(|x| x.role == Role::Implement)
                        .or_else(|| a.iter().find(|x| x.role.writes()))
                        .map(|x| x.workspace_id))
                })
                .await
                .map_err(errors::storage)?
                .ok_or_else(|| {
                    invalid_state("the task has no implementation workspace; pass workspace_id")
                })?,
            (None, None) => return Err(invalid_params("pass task_id or workspace_id")),
        };
        let w = self.inner.workspaces.record(workspace_id).await?;
        let Some(source_branch) =
            w.branch.clone().filter(|_| w.kind == switchyard_workspaces::WorkspaceKind::Worktree)
        else {
            return Err(invalid_params("only a Switchyard worktree workspace can be integrated"));
        };
        if let Some(t) = task_id {
            let task = self
                .inner
                .storage
                .call(move |c| task_store::get_task(c, t))
                .await
                .map_err(errors::storage)?;
            if task.project_id != w.project_id {
                return Err(invalid_params("the task and the workspace belong to different projects"));
            }
        }
        let root = self.inner.workspaces.project_root(w.project_id).await?;
        let target_branch =
            match p.target_branch.map(|b| b.trim().to_owned()).filter(|b| !b.is_empty()) {
                Some(b) => b,
                None => self.inner.workspaces.git().status(&root).await.map_err(git_err)?.branch.ok_or_else(
                    || invalid_params("the project checkout is detached; name a target_branch"),
                )?,
            };
        check_target(&target_branch, &source_branch)?;
        let prepared = self.gather(&w, &target_branch, task_id, None).await?;
        let id = new_id();
        let merge_in_progress = prepared.workspace_merge_in_progress;
        commit_checked(&self.inner.storage, &self.inner.bus, move |tx| {
            let now = now_ms();
            let record = IntegrationRecord {
                id,
                project_id: w.project_id,
                task_id,
                workspace_id: w.id,
                source_branch: source_branch.clone(),
                source_commit: prepared.source_commit.clone(),
                target_branch: target_branch.clone(),
                target_commit: prepared.target_commit.clone(),
                base_commit: prepared.base_commit.clone(),
                status: status_for(&prepared, merge_in_progress),
                status_reason: None,
                preflight: prepared.preflight.clone(),
                prepared_at_ms: now,
                approved_by: None,
                approved_at_ms: None,
                merge_commit: None,
                method: None,
                failure: None,
                revision: 1,
                created_at_ms: now,
                updated_at_ms: now,
                completed_at_ms: None,
            };
            match store::insert_integration(tx, &record) {
                Ok(()) => {}
                Err(StorageError::Conflict(m)) => return Ok(Err(ProtocolError::new(ErrorCode::Conflict, m))),
                Err(e) => return Err(e),
            }
            let ctx = context(&record);
            let drafts = vec![
                draft(
                    DomainEvent::IntegrationCreated {
                        integration_id: id,
                        source_branch,
                        target_branch,
                        created_by: actor,
                    },
                    &ctx,
                )?,
                draft(prepared.event(id), &ctx)?,
            ];
            Ok(Ok(((), drafts)))
        })
        .await?;
        self.wire(id).await
    }

    /// Re-reads everything from Git and rebinds the integration to the
    /// current source and target commits. Any change since the last
    /// preparation means earlier validation and inspection no longer apply.
    pub async fn refresh(&self, id: Uuid) -> Result<Integration, ProtocolError> {
        let _claim = self.claim(id)?;
        self.prepare(id).await?;
        self.wire(id).await
    }

    /// Prepares and stores the result (no claim taken: callers hold one).
    pub(crate) async fn prepare(&self, id: Uuid) -> Result<IntegrationRecord, ProtocolError> {
        self.prepare_with(id, None).await
    }

    /// [`Self::prepare`], recording `reason` instead of the first blocker.
    pub(crate) async fn prepare_with(
        &self,
        id: Uuid,
        reason: Option<String>,
    ) -> Result<IntegrationRecord, ProtocolError> {
        let i = self.record(id).await?;
        if !matches!(
            i.status,
            IntegrationStatus::Open
                | IntegrationStatus::Conflicted
                | IntegrationStatus::Ready
                | IntegrationStatus::Failed
                | IntegrationStatus::Resolving
                | IntegrationStatus::Validating
        ) {
            return Err(invalid_state(format!("the integration is {}", i.status)));
        }
        let w = self.inner.workspaces.record(i.workspace_id).await?;
        let prepared = self.gather(&w, &i.target_branch, i.task_id, Some(i.id)).await?;
        self.store_prepared(i, prepared, reason).await
    }

    async fn store_prepared(
        &self,
        i: IntegrationRecord,
        prepared: Prepared,
        reason: Option<String>,
    ) -> Result<IntegrationRecord, ProtocolError> {
        let to = status_for(&prepared, prepared.workspace_merge_in_progress);
        commit_checked(&self.inner.storage, &self.inner.bus, move |tx| {
            let now = now_ms();
            let mut drafts = Vec::new();
            let current = store::get_integration(tx, i.id)?;
            if current.status != i.status {
                return Ok(Err(ProtocolError::new(ErrorCode::Conflict, "the integration changed meanwhile")));
            }
            let reason = reason.or_else(|| prepared.blockers.first().map(|b| b.message.clone()));
            let u = IntegrationUpdate {
                status: Some(to),
                status_reason: Some(reason),
                source_commit: Some(prepared.source_commit.clone()),
                target_commit: Some(prepared.target_commit.clone()),
                base_commit: Some(prepared.base_commit.clone()),
                preflight: Some(prepared.preflight.clone()),
                prepared: true,
                // A new preparation is not approved by anyone.
                approved_by: Some(None),
                approved_at_ms: Some(None),
                failure: if to == IntegrationStatus::Failed { None } else { Some(None) },
                ..Default::default()
            };
            let after = transition(tx, &current, u, now, &mut drafts)?;
            drafts.push(draft(prepared.event(after.id), &context(&after))?);
            Ok(Ok((after, drafts)))
        })
        .await
    }

    /// The operator rejects or withdraws the integration. Nothing in Git changes.
    pub async fn cancel(
        &self,
        id: Uuid,
        reason: Option<String>,
        actor: String,
    ) -> Result<Integration, ProtocolError> {
        let _claim = self.claim(id)?;
        let reason =
            reason.filter(|r| !r.trim().is_empty()).unwrap_or_else(|| format!("cancelled by {actor}"));
        commit_checked(&self.inner.storage, &self.inner.bus, move |tx| {
            let i = store::get_integration(tx, id)?;
            if matches!(i.status, IntegrationStatus::Validating | IntegrationStatus::Integrating) {
                return Ok(Err(invalid_state(format!(
                    "the integration is {}; wait for it (or cancel the validation) first",
                    i.status
                ))));
            }
            if i.status.is_terminal() {
                return Ok(Err(invalid_state(format!("the integration is already {}", i.status))));
            }
            let mut drafts = Vec::new();
            let u = IntegrationUpdate {
                status: Some(IntegrationStatus::Cancelled),
                status_reason: Some(Some(reason)),
                completed: true,
                ..Default::default()
            };
            transition(tx, &i, u, now_ms(), &mut drafts)?;
            Ok(Ok(((), drafts)))
        })
        .await?;
        self.wire(id).await
    }

    // --- settings and validation commands --------------------------------------

    pub async fn settings(&self, project_id: Uuid) -> Result<IntegrationSettingsInfo, ProtocolError> {
        self.inner
            .storage
            .call(move |c| {
                switchyard_storage::projects::get(c, project_id)?;
                let s = store::settings(c, project_id)?;
                Ok(IntegrationSettingsInfo {
                    project_id,
                    require_review: s.require_review,
                    revision: s.revision,
                    commands: store::commands(c, project_id)?.into_iter().map(command_wire).collect(),
                })
            })
            .await
            .map_err(|e| match e {
                StorageError::NotFound(w) => {
                    ProtocolError::new(ErrorCode::ProjectNotFound, format!("{w} not found"))
                }
                other => errors::storage(other),
            })
    }

    pub async fn set_settings(
        &self,
        project_id: Uuid,
        require_review: bool,
    ) -> Result<IntegrationSettingsInfo, ProtocolError> {
        self.settings(project_id).await?;
        self.inner
            .storage
            .call(move |c| store::set_settings(c, project_id, require_review, now_ms()))
            .await
            .map_err(errors::storage)?;
        self.settings(project_id).await
    }

    pub async fn set_command(
        &self,
        p: IntegrationsSetCommandParams,
    ) -> Result<IntegrationSettingsInfo, ProtocolError> {
        let cmd = ValidationCommand {
            name: p.name.trim().to_owned(),
            argv: p.argv.clone(),
            timeout_secs: p.timeout_secs,
        };
        validate_command(&cmd).map_err(|e| invalid_params(e.to_string()))?;
        let project_id = p.project_id;
        self.settings(project_id).await?;
        self.inner
            .storage
            .call(move |c| {
                let now = now_ms();
                let existing = match p.command_id {
                    Some(id) => {
                        let e = store::get_command(c, id)?;
                        if e.project_id != project_id {
                            return Err(StorageError::NotFound(format!("validation command {id}")));
                        }
                        Some(e)
                    }
                    None => None,
                };
                let position = p.position.unwrap_or_else(|| {
                    existing.as_ref().map(|e| e.position).unwrap_or_else(|| {
                        store::commands(c, project_id).map(|v| v.len() as i64).unwrap_or(0)
                    })
                });
                store::upsert_command(
                    c,
                    &CommandRecord {
                        id: p.command_id.unwrap_or_else(new_id),
                        project_id,
                        position,
                        name: cmd.name,
                        argv: cmd.argv,
                        timeout_secs: cmd.timeout_secs,
                        enabled: p.enabled,
                        created_at_ms: existing.map(|e| e.created_at_ms).unwrap_or(now),
                        updated_at_ms: now,
                    },
                )
            })
            .await
            .map_err(errors::storage)?;
        self.settings(project_id).await
    }

    pub async fn delete_command(&self, command_id: Uuid) -> Result<IntegrationSettingsInfo, ProtocolError> {
        let project_id = self
            .inner
            .storage
            .call(move |c| {
                let cmd = store::get_command(c, command_id)?;
                store::delete_command(c, command_id)?;
                Ok(cmd.project_id)
            })
            .await
            .map_err(errors::storage)?;
        self.settings(project_id).await
    }

    /// The workspace of an integration (for UI and tests).
    pub(crate) async fn workspace_of(&self, i: &IntegrationRecord) -> Result<WorkspaceRecord, ProtocolError> {
        self.inner.workspaces.record(i.workspace_id).await
    }
}

/// The target must be an ordinary branch other than the source, and not a
/// Switchyard workspace branch.
fn check_target(target: &str, source: &str) -> Result<(), ProtocolError> {
    let bad_char = target.chars().any(|c| c.is_control() || " ~^:?*[\\".contains(c));
    if target.starts_with('-')
        || target.starts_with("refs/")
        || target.contains("..")
        || target.contains("@{")
        || target.contains("//")
        || target.ends_with('/')
        || target.ends_with(".lock")
        || bad_char
    {
        return Err(invalid_params(format!("{target:?} is not a branch name")));
    }
    if target == source {
        return Err(invalid_params("the target branch is the workspace's own branch"));
    }
    if target.starts_with("switchyard/") {
        return Err(invalid_params("integrating into another Switchyard workspace branch is not supported"));
    }
    Ok(())
}
