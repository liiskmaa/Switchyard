//! Core-owned integration operations (ADR 0032).
//!
//! Every integration step that changes Git (approving, committing,
//! updating or aborting in the workspace, a validation run) runs as an
//! **operation** that belongs to Core, not to the IPC request that asked for
//! it:
//!
//! * it is recorded (`integration_operations`, `integration.operation_started`)
//!   before any work starts, and ended (`integration.operation_finished`)
//!   when the work ends, whatever the outcome;
//! * its work runs in a task Core supervises. The request only waits for the
//!   result: a client that disconnects (or whose request is aborted) does not
//!   stop it, and a reconnecting client finds it on the integration;
//! * at most one runs per integration (an in-memory claim plus a unique
//!   index), so a retried request cannot start the same work twice;
//! * **its end is atomic with its result**: the transaction that commits the
//!   operation's final change to the integration (approve: `integrated` or
//!   `failed`; validate: leaving `validating`; workspace steps: the
//!   preparation after the Git step) also ends the operation ([`Ending`]).
//!   No client can observe that final state with the operation `running`.
//!   The supervisor ends an operation itself only when its work stopped
//!   without such a commit (an error before any change, a panic);
//! * if its work panics, the workspace is released and the integration is
//!   settled from Git;
//! * Core shutdown first cancels validation runs (their process groups are
//!   stopped) and then waits, bounded, for the rest. Operations still
//!   running after that are stopped with Core and stay recorded `running`;
//!   the next start settles them from Git exactly like after a crash.
//!
//! Explicit cancellation exists only for validation runs
//! (`integrations.cancel_validation`). A Git step that was authorized is
//! never cancelled half-way on purpose.

use std::future::Future;
use std::sync::atomic::Ordering;
use std::time::Duration;

use switchyard_events::{DomainEvent, new_id, now_ms};
use switchyard_integrations::{IntegrationStatus, RunStatus, StepStatus};
use switchyard_protocol::model::IntegrationOperation;
use switchyard_protocol::{ErrorCode, ProtocolError};
use switchyard_storage::integrations::{self as store, IntegrationUpdate, OperationRecord, StepOutcome};
use switchyard_storage::{StorageError, WorkspaceRecord};
use switchyard_workspaces::WorkspaceStatus;
use tokio::sync::oneshot;
use uuid::Uuid;

use super::{IntegrationService, context, invalid_state, transition};
use crate::handlers::errors;
use crate::journal::{commit_checked, draft};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpKind {
    Approve,
    CommitWorkspace,
    UpdateFromTarget,
    AbortResolution,
    Validate,
}

impl OpKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::CommitWorkspace => "commit_workspace",
            Self::UpdateFromTarget => "update_from_target",
            Self::AbortResolution => "abort_resolution",
            Self::Validate => "validate",
        }
    }

    /// Whether the operation marks the source workspace `integrating`.
    fn uses_workspace(self) -> bool {
        self != Self::Approve
    }
}

/// Identity of a running operation, handed to its work.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Op {
    pub id: Uuid,
    pub kind: OpKind,
}

/// Ends an operation inside the transaction that commits its final change:
/// `completed`, or `failed` with `error`.
#[derive(Debug, Clone)]
pub(crate) struct Ending {
    pub op: Op,
    pub error: Option<String>,
}

impl Ending {
    pub fn completed(op: Op) -> Self {
        Self { op, error: None }
    }

    /// Records the end in `tx`; returns its journal event, or nothing if the
    /// operation had already ended.
    pub fn record(
        &self,
        tx: &switchyard_storage::Connection,
        i: &switchyard_storage::integrations::IntegrationRecord,
    ) -> Result<Option<switchyard_events::EventDraft>, StorageError> {
        let status = if self.error.is_some() { "failed" } else { "completed" };
        if !store::finish_operation(tx, self.op.id, status, self.error.as_deref(), now_ms())? {
            return Ok(None);
        }
        Ok(Some(draft(
            DomainEvent::IntegrationOperationFinished {
                integration_id: i.id,
                operation_id: self.op.id,
                kind: self.op.kind.as_str().into(),
                status: status.into(),
                error: self.error.clone(),
            },
            &context(i),
        )?))
    }
}

pub(crate) fn op_wire(o: OperationRecord) -> IntegrationOperation {
    IntegrationOperation {
        id: o.id,
        kind: o.kind,
        status: o.status,
        requested_by: o.requested_by,
        error: o.error,
        started_at_ms: o.started_at_ms,
        ended_at_ms: o.ended_at_ms,
    }
}

/// Counts a supervised task while it lives (also when it panics or is aborted).
struct Running(tokio::sync::watch::Sender<usize>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.send_modify(|n| *n = n.saturating_sub(1));
    }
}

impl IntegrationService {
    fn track(&self) -> Running {
        self.inner.running.send_modify(|n| *n += 1);
        Running(self.inner.running.clone())
    }

    /// Runs `work` as a Core-owned operation and waits for its result. If the
    /// caller stops waiting (its client disconnected), the work continues.
    pub(crate) async fn run_op<T, F, Fut>(
        &self,
        integration_id: Uuid,
        kind: OpKind,
        actor: String,
        work: F,
    ) -> Result<T, ProtocolError>
    where
        F: FnOnce(Op) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, ProtocolError>> + Send + 'static,
        T: Send + 'static,
    {
        let rx = self.spawn_op(integration_id, kind, actor, work).await?;
        rx.await.unwrap_or_else(|_| {
            Err(ProtocolError::new(
                ErrorCode::Internal,
                "the operation stopped before it reported a result; its status is on the integration",
            ))
        })
    }

    /// Starts `work` as a Core-owned operation; the receiver gets its result.
    pub(crate) async fn spawn_op<T, F, Fut>(
        &self,
        integration_id: Uuid,
        kind: OpKind,
        actor: String,
        work: F,
    ) -> Result<oneshot::Receiver<Result<T, ProtocolError>>, ProtocolError>
    where
        F: FnOnce(Op) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, ProtocolError>> + Send + 'static,
        T: Send + 'static,
    {
        if self.inner.stopping.load(Ordering::SeqCst) {
            return Err(ProtocolError::new(ErrorCode::ShuttingDown, "Core is shutting down"));
        }
        let claim = self.claim(integration_id)?;
        let running = self.track();
        let op = Op { id: new_id(), kind };
        commit_checked(&self.inner.storage, &self.inner.bus, move |tx| {
            let i = store::get_integration(tx, integration_id)?;
            let record = OperationRecord {
                id: op.id,
                integration_id,
                project_id: i.project_id,
                kind: kind.as_str().into(),
                status: "running".into(),
                requested_by: actor.clone(),
                workspace_before: None,
                error: None,
                started_at_ms: now_ms(),
                ended_at_ms: None,
            };
            match store::insert_operation(tx, &record) {
                Ok(()) => {}
                Err(StorageError::Conflict(m)) => return Ok(Err(ProtocolError::new(ErrorCode::Conflict, m))),
                Err(e) => return Err(e),
            }
            let d = draft(
                DomainEvent::IntegrationOperationStarted {
                    integration_id,
                    operation_id: op.id,
                    kind: kind.as_str().into(),
                    requested_by: actor,
                },
                &context(&i),
            )?;
            Ok(Ok(((), vec![d])))
        })
        .await?;

        let (tx, rx) = oneshot::channel();
        let body = tokio::spawn(work(op));
        self.inner
            .bodies
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(op.id, body.abort_handle());
        let svc = self.clone();
        tokio::spawn(async move {
            let _running = running;
            let _claim = claim;
            let joined = body.await;
            svc.inner.bodies.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&op.id);
            match joined {
                // Stopped by Core shutdown after its grace: left `running`, so the
                // next start settles it from Git like after a crash.
                Err(e) if e.is_cancelled() => {
                    tracing::warn!(operation = %op.id, "integration operation stopped by shutdown");
                }
                Err(_) => {
                    tracing::error!(operation = %op.id, kind = kind.as_str(), "integration operation panicked");
                    let message = "the operation stopped unexpectedly (internal error); Core re-read the state from Git";
                    svc.after_panic(integration_id, kind).await;
                    svc.finish_op(op.id, integration_id, kind, "failed", Some(message.into())).await;
                    let _ = tx.send(Err(ProtocolError::new(ErrorCode::Internal, message)));
                }
                Ok(result) => {
                    let (status, error) = match &result {
                        Ok(_) => ("completed", None),
                        Err(e) => ("failed", Some(e.message.clone())),
                    };
                    svc.finish_op(op.id, integration_id, kind, status, error).await;
                    let _ = tx.send(result);
                }
            }
        });
        Ok(rx)
    }

    async fn finish_op(
        &self,
        op: Uuid,
        integration_id: Uuid,
        kind: OpKind,
        status: &'static str,
        error: Option<String>,
    ) {
        let r = commit_checked(&self.inner.storage, &self.inner.bus, move |tx| {
            if !store::finish_operation(tx, op, status, error.as_deref(), now_ms())? {
                return Ok(Ok(((), Vec::new())));
            }
            let i = store::get_integration(tx, integration_id)?;
            let d = draft(
                DomainEvent::IntegrationOperationFinished {
                    integration_id,
                    operation_id: op,
                    kind: kind.as_str().into(),
                    status: status.into(),
                    error,
                },
                &context(&i),
            )?;
            Ok(Ok(((), vec![d])))
        })
        .await;
        if let Err(e) = r {
            // The row stays `running`; startup recovery (or a refresh) settles it.
            tracing::error!(operation = %op, ?e, "cannot record the end of an integration operation");
        }
    }

    async fn after_panic(&self, integration_id: Uuid, kind: OpKind) {
        if kind.uses_workspace()
            && let Ok(i) = self.record(integration_id).await
        {
            self.inner.workspaces.end_integration_op(i.workspace_id).await;
        }
        if let Err(e) = self.settle_stranded(integration_id).await {
            tracing::error!(integration = %integration_id, ?e, "cannot settle an integration after a failed operation");
        }
    }

    /// Marks the workspace `integrating` for `op` and records the status it
    /// had, so recovery can restore it.
    pub(crate) async fn begin_workspace_step(
        &self,
        op: Op,
        workspace_id: Uuid,
    ) -> Result<WorkspaceRecord, ProtocolError> {
        let (w, before) = self.inner.workspaces.begin_integration_op(workspace_id).await.map_err(|e| {
            if e.code == ErrorCode::WorkspaceOccupied {
                invalid_state("an agent session is running in the workspace; stop it first")
            } else {
                e
            }
        })?;
        let recorded = self
            .inner
            .storage
            .call(move |c| store::set_operation_workspace_before(c, op.id, before.as_str()))
            .await;
        if let Err(e) = recorded {
            self.inner.workspaces.end_integration_op(w.id).await;
            return Err(errors::storage(e));
        }
        Ok(w)
    }

    /// Stops accepting operations, cancels validation runs and waits up to
    /// `grace` for running operations; stops the rest (see the module docs).
    pub async fn shutdown(&self, grace: Duration) {
        self.inner.stopping.store(true, Ordering::SeqCst);
        for cancel in self.inner.cancels.lock().unwrap_or_else(std::sync::PoisonError::into_inner).values() {
            let _ = cancel.send(true);
        }
        if self.wait_idle(grace).await {
            return;
        }
        let stopped: Vec<_> = self
            .inner
            .bodies
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain()
            .map(|(_, h)| h)
            .collect();
        tracing::warn!(
            operations = stopped.len(),
            "integration operations did not finish within the shutdown grace; stopping them (settled at the next start)"
        );
        for h in stopped {
            h.abort();
        }
        if !self.wait_idle(Duration::from_secs(5)).await {
            tracing::error!("integration operations are still being stopped");
        }
    }

    async fn wait_idle(&self, within: Duration) -> bool {
        let mut rx = self.inner.running.subscribe();
        tokio::time::timeout(within, async {
            let _ = rx.wait_for(|n| *n == 0).await;
        })
        .await
        .is_ok()
    }

    /// Settles an integration that is `integrating` or `validating` while no
    /// operation of this Core works on it (after a crash, a stopped
    /// operation, or a panic): `integrating` from the target branch,
    /// `validating` by interrupting its run and preparing again (or
    /// `failed` if that is impossible). Callers hold the claim or run at
    /// startup.
    pub(crate) async fn settle_stranded(&self, id: Uuid) -> Result<(), ProtocolError> {
        let i = self.record(id).await?;
        match i.status {
            IntegrationStatus::Integrating => self.recover_integrating(&i).await,
            IntegrationStatus::Validating => {
                self.interrupt_runs(id).await?;
                let w = self.inner.workspaces.record(i.workspace_id).await?;
                if w.status == WorkspaceStatus::Integrating {
                    self.inner.workspaces.end_integration_op(w.id).await;
                }
                self.prepare_or_fail(id, Some("The validation was interrupted; run it again.".into()), None)
                    .await;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Marks the integration's running validation runs `interrupted`.
    async fn interrupt_runs(&self, id: Uuid) -> Result<(), ProtocolError> {
        commit_checked(&self.inner.storage, &self.inner.bus, move |tx| {
            let now = now_ms();
            let i = store::get_integration(tx, id)?;
            let mut drafts = Vec::new();
            for run in
                store::runs_for_integration(tx, id)?.into_iter().filter(|r| r.status == RunStatus::Running)
            {
                for s in store::steps_for_run(tx, run.id)? {
                    if !s.status.is_final() {
                        let o = StepOutcome {
                            detail: Some("Core stopped while this ran; the result is unknown".into()),
                            ..Default::default()
                        };
                        store::finish_step(tx, s.id, StepStatus::Cancelled, &o, now)?;
                    }
                }
                if store::finish_run(tx, run.id, RunStatus::Interrupted, now)? {
                    drafts.push(draft(
                        DomainEvent::IntegrationValidationFinished {
                            integration_id: id,
                            run_id: run.id,
                            source_commit: run.source_commit.clone(),
                            status: RunStatus::Interrupted.as_str().into(),
                        },
                        &context(&i),
                    )?);
                }
            }
            Ok(Ok(((), drafts)))
        })
        .await
    }

    /// Prepares the integration again; if that is impossible (a branch was
    /// deleted, Git fails), a `validating` integration becomes `failed` with
    /// the reason instead of staying stuck. It can be refreshed later. With
    /// `op`, the operation ends in the same transaction (`completed` after a
    /// preparation, `failed` with the reason otherwise).
    pub(crate) async fn prepare_or_fail(&self, id: Uuid, reason: Option<String>, op: Option<Op>) {
        let Err(e) = self.prepare_ending(id, reason, op.map(Ending::completed)).await else { return };
        tracing::warn!(integration = %id, ?e, "cannot prepare the integration again");
        let message =
            format!("Core could not check the integration again: {}. Refresh it to retry.", e.message);
        let ending = op.map(|op| Ending { op, error: Some(message.clone()) });
        let r = commit_checked(&self.inner.storage, &self.inner.bus, move |tx| {
            let i = store::get_integration(tx, id)?;
            let mut drafts = Vec::new();
            if i.status == IntegrationStatus::Validating {
                let u = IntegrationUpdate {
                    status: Some(IntegrationStatus::Failed),
                    status_reason: Some(Some(message.clone())),
                    failure: Some(Some(message)),
                    ..Default::default()
                };
                transition(tx, &i, u, now_ms(), &mut drafts)?;
            }
            if let Some(ending) = &ending {
                drafts.extend(ending.record(tx, &i)?);
            }
            Ok(Ok(((), drafts)))
        })
        .await;
        if let Err(e) = r {
            tracing::error!(integration = %id, ?e, "cannot record that the integration could not be prepared");
        }
    }

    /// At startup: operations a previous Core left `running` were
    /// interrupted. Records that, and restores a workspace the operator had
    /// marked `completed` (workspace reconciliation settled it from Git).
    pub(crate) async fn recover_operations(&self) -> Result<(), ProtocolError> {
        let ops = self.inner.storage.call(|c| store::running_operations(c)).await.map_err(errors::storage)?;
        for op in ops {
            let kind = match op.kind.as_str() {
                "approve" => OpKind::Approve,
                "commit_workspace" => OpKind::CommitWorkspace,
                "update_from_target" => OpKind::UpdateFromTarget,
                "abort_resolution" => OpKind::AbortResolution,
                _ => OpKind::Validate,
            };
            let message = format!(
                "Core stopped during this {} operation; the state was re-read from Git at the next start",
                kind.as_str()
            );
            self.finish_op(op.id, op.integration_id, kind, "interrupted", Some(message)).await;
            if op.workspace_before.as_deref() == Some(WorkspaceStatus::Completed.as_str())
                && let Ok(i) = self.record(op.integration_id).await
                && let Ok(w) = self.inner.workspaces.record(i.workspace_id).await
                && matches!(w.status, WorkspaceStatus::Ready | WorkspaceStatus::Dirty)
            {
                let r = self
                    .inner
                    .workspaces
                    .set_status(w.id, WorkspaceStatus::Completed, Default::default(), Vec::new())
                    .await;
                if let Err(e) = r {
                    tracing::warn!(workspace = %w.id, ?e, "cannot restore the completed mark after a restart");
                }
            }
        }
        Ok(())
    }
}
