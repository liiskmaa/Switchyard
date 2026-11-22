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
mod operations;
pub(crate) mod validate;
mod workspace_ops;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
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
pub(crate) use operations::{Ending, Op, OpKind};

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

fn to_wire(
    i: IntegrationRecord,
    workspace_name: Option<String>,
    operation: Option<store::OperationRecord>,
) -> Integration {
    Integration {
        operation: operation.map(operations::op_wire),
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
    let op = store::latest_operation(c, i.id)?;
    Ok(to_wire(i, name, op))
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

/// What an audit event records about a validation command.
fn command_snapshot(c: &CommandRecord) -> serde_json::Value {
    serde_json::json!({
        "name": c.name,
        "command_line": command_line(&c.argv),
        "timeout_secs": c.timeout_secs,
        "enabled": c.enabled,
        "position": c.position,
    })
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
    /// Set at shutdown: no new operation starts (ADR 0032).
    stopping: AtomicBool,
    /// Supervised operation tasks alive now.
    running: watch::Sender<usize>,
    /// The work of each running operation, stopped only at shutdown.
    bodies: Mutex<HashMap<Uuid, tokio::task::AbortHandle>>,
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
                stopping: AtomicBool::new(false),
                running: watch::channel(0).0,
                bodies: Mutex::default(),
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
    ///
    /// An integration left `integrating` or `validating` with no operation
    /// working on it (Core stopped, or a Git read failed during recovery) is
    /// settled from Git instead.
    pub async fn refresh(&self, id: Uuid) -> Result<Integration, ProtocolError> {
        let _claim = self.claim(id)?;
        let i = self.record(id).await?;
        if matches!(i.status, IntegrationStatus::Integrating | IntegrationStatus::Validating) {
            self.settle_stranded(id).await?;
        } else {
            self.prepare(id).await?;
        }
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
        self.prepare_ending(id, reason, None).await
    }

    /// [`Self::prepare_with`], ending `ending`'s operation in the same
    /// transaction that stores the preparation (ADR 0032).
    pub(crate) async fn prepare_ending(
        &self,
        id: Uuid,
        reason: Option<String>,
        ending: Option<Ending>,
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
        self.store_prepared(i, prepared, reason, ending).await
    }

    async fn store_prepared(
        &self,
        i: IntegrationRecord,
        prepared: Prepared,
        reason: Option<String>,
        ending: Option<Ending>,
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
            if let Some(ending) = &ending {
                drafts.extend(ending.record(tx, &after)?);
            }
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

    /// Changes the project's review requirement. The change and its
    /// `integration.settings_changed` event (before, after, who) are one
    /// transaction; open integrations are then checked again.
    pub async fn set_settings(
        &self,
        project_id: Uuid,
        require_review: bool,
        actor: String,
    ) -> Result<IntegrationSettingsInfo, ProtocolError> {
        self.settings(project_id).await?;
        commit_checked(&self.inner.storage, &self.inner.bus, move |tx| {
            let before = store::settings(tx, project_id)?;
            let after = store::set_settings(tx, project_id, require_review, now_ms())?;
            let ctx = EventContext { project_id: Some(project_id), ..Default::default() };
            let d = draft(
                DomainEvent::IntegrationSettingsChanged {
                    project_id,
                    require_review_before: before.require_review,
                    require_review_after: after.require_review,
                    revision: after.revision,
                    changed_by: actor,
                },
                &ctx,
            )?;
            Ok(Ok(((), vec![d])))
        })
        .await?;
        self.reprepare_project(project_id).await;
        self.settings(project_id).await
    }

    /// Creates or changes a validation command, with its
    /// `integration.validation_command_changed` event in the same
    /// transaction; open integrations are then checked again (a passed run
    /// counts only for the configuration it executed).
    pub async fn set_command(
        &self,
        p: IntegrationsSetCommandParams,
        actor: String,
    ) -> Result<IntegrationSettingsInfo, ProtocolError> {
        let cmd = ValidationCommand {
            name: p.name.trim().to_owned(),
            argv: p.argv.clone(),
            timeout_secs: p.timeout_secs,
        };
        validate_command(&cmd).map_err(|e| invalid_params(e.to_string()))?;
        let project_id = p.project_id;
        self.settings(project_id).await?;
        commit_checked(&self.inner.storage, &self.inner.bus, move |tx| {
            let now = now_ms();
            let existing = match p.command_id {
                Some(id) => match store::get_command(tx, id) {
                    Ok(e) if e.project_id == project_id => Some(e),
                    Ok(_) | Err(StorageError::NotFound(_)) => {
                        return Ok(Err(ProtocolError::new(
                            ErrorCode::NotFound,
                            format!("validation command {id} not found in this project"),
                        )));
                    }
                    Err(e) => return Err(e),
                },
                None => None,
            };
            let position = match (p.position, &existing) {
                (Some(pos), _) => pos,
                (None, Some(e)) => e.position,
                (None, None) => store::commands(tx, project_id)?.len() as i64,
            };
            let record = CommandRecord {
                id: p.command_id.unwrap_or_else(new_id),
                project_id,
                position,
                name: cmd.name,
                argv: cmd.argv,
                timeout_secs: cmd.timeout_secs,
                enabled: p.enabled,
                created_at_ms: existing.as_ref().map(|e| e.created_at_ms).unwrap_or(now),
                updated_at_ms: now,
            };
            store::upsert_command(tx, &record)?;
            let ctx = EventContext { project_id: Some(project_id), ..Default::default() };
            let d = draft(
                DomainEvent::IntegrationValidationCommandChanged {
                    project_id,
                    command_id: record.id,
                    action: if existing.is_some() { "updated" } else { "created" }.into(),
                    before: existing.as_ref().map(command_snapshot),
                    after: Some(command_snapshot(&record)),
                    changed_by: actor,
                },
                &ctx,
            )?;
            Ok(Ok(((), vec![d])))
        })
        .await?;
        self.reprepare_project(project_id).await;
        self.settings(project_id).await
    }

    /// Deletes a validation command, with its event in the same transaction.
    pub async fn delete_command(
        &self,
        command_id: Uuid,
        actor: String,
    ) -> Result<IntegrationSettingsInfo, ProtocolError> {
        let project_id = commit_checked(&self.inner.storage, &self.inner.bus, move |tx| {
            let cmd = match store::get_command(tx, command_id) {
                Ok(c) => c,
                Err(StorageError::NotFound(_)) => {
                    return Ok(Err(ProtocolError::new(
                        ErrorCode::NotFound,
                        format!("validation command {command_id} not found"),
                    )));
                }
                Err(e) => return Err(e),
            };
            store::delete_command(tx, command_id)?;
            let ctx = EventContext { project_id: Some(cmd.project_id), ..Default::default() };
            let d = draft(
                DomainEvent::IntegrationValidationCommandChanged {
                    project_id: cmd.project_id,
                    command_id,
                    action: "deleted".into(),
                    before: Some(command_snapshot(&cmd)),
                    after: None,
                    changed_by: actor,
                },
                &ctx,
            )?;
            Ok(Ok((cmd.project_id, vec![d])))
        })
        .await?;
        self.reprepare_project(project_id).await;
        self.settings(project_id).await
    }

    /// Checks the project's open integrations again after a settings change,
    /// so their blockers (and any approval readiness) reflect it. Ones with
    /// an operation running are skipped: the operation prepares them when it
    /// ends, and approval re-checks everything under the lock anyway.
    async fn reprepare_project(&self, project_id: Uuid) {
        let open = self
            .inner
            .storage
            .call(move |c| store::list_integrations(c, Some(project_id), None, false, 1000))
            .await;
        let Ok(open) = open else { return };
        for i in open {
            if !matches!(
                i.status,
                IntegrationStatus::Open
                    | IntegrationStatus::Conflicted
                    | IntegrationStatus::Ready
                    | IntegrationStatus::Failed
                    | IntegrationStatus::Resolving
            ) {
                continue;
            }
            let Ok(_claim) = self.claim(i.id) else { continue };
            if let Err(e) = self.prepare(i.id).await {
                tracing::warn!(integration = %i.id, ?e, "cannot check an integration again after a settings change");
            }
        }
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
