//! Centralized approvals for agent permission requests.
//!
//! Invariants:
//! * every request that reaches Switchyard is recorded before anything else;
//! * a request is settled exactly once (SQL-guarded `pending → final`), by
//!   the operator, by timeout, by the agent cancelling it, by the process
//!   exiting, or by Core shutdown/restart;
//! * the decision is committed durably *before* it is delivered to the
//!   agent; a failed delivery is journaled as a warning;
//! * nothing is approved automatically; every non-operator path denies.

use serde_json::json;
use switchyard_agents::event::{MAX_DURABLE_INPUT, bound_json};
use switchyard_agents::{
    ApprovalStatus, DecisionSource, PermissionDecision, PermissionRequest, SessionState,
};
use switchyard_events::{DomainEvent, EventContext, new_id, now_ms};
use switchyard_protocol::methods::ApprovalDecision;
use switchyard_protocol::model::Approval;
use switchyard_protocol::{ErrorCode, ProtocolError};
use switchyard_storage::approvals as repo;
use switchyard_storage::sessions::{self, SessionChanges};
use switchyard_storage::{ApprovalRecord, StorageError};
use uuid::Uuid;

use super::{AgentService, DraftOrEvent, convert, draft};
use crate::handlers::errors;

/// Upper bound for the request stored in the approvals table.
const MAX_STORED_REQUEST: usize = 256 * 1024;

fn resolved(record: &ApprovalRecord) -> DomainEvent {
    DomainEvent::AgentPermissionResolved {
        approval_id: record.id,
        tool_name: record.tool_name.clone(),
        status: record.status.as_str().into(),
        source: record.decision_source.map(|s| s.as_str().to_owned()).unwrap_or_default(),
        decided_by: record.decided_by.clone(),
        note: record.decision_note.clone(),
    }
}

/// Settles one approval and, if no request of the session remains pending,
/// moves `waiting_for_approval` back to `running` — in one transaction.
fn settle_in_tx(
    tx: &switchyard_storage::Transaction<'_>,
    approval_id: Uuid,
    status: ApprovalStatus,
    source: DecisionSource,
    decided_by: Option<&str>,
    note: Option<&str>,
    now: i64,
) -> Result<(ApprovalRecord, Vec<switchyard_events::EventDraft>), StorageError> {
    let record = repo::decide(tx, approval_id, status, source, decided_by, note, now)?;
    let session = sessions::get(tx, record.session_id)?;
    let ctx = AgentService::context(&session);
    let mut drafts = vec![draft(resolved(&record), &ctx)?];
    if session.state == SessionState::WaitingForApproval && repo::count_pending(tx, record.session_id)? == 0 {
        sessions::transition(tx, session.id, SessionState::Running, &SessionChanges::default(), now)?;
        drafts.push(draft(
            DomainEvent::AgentStateChanged { from: session.state.as_str().into(), to: "running".into() },
            &ctx,
        )?);
    }
    Ok((record, drafts))
}

impl AgentService {
    /// Records a new permission request and starts its expiry timer.
    pub(super) async fn on_permission_requested(
        &self,
        session_id: Uuid,
        ctx: &EventContext,
        request: PermissionRequest,
    ) {
        let approval_id = new_id();
        let now = now_ms();
        let timeout = self.inner.config.approval_timeout;
        let expires = now + i64::try_from(timeout.as_millis()).unwrap_or(i64::MAX / 2);
        let (stored_request, _) = bound_json(
            &json!({ "input": request.input, "description": request.description, "details": request.details }),
            MAX_STORED_REQUEST,
        );
        let (event_input, _) = bound_json(&request.input, MAX_DURABLE_INPUT);
        let record = ApprovalRecord {
            id: approval_id,
            project_id: ctx.project_id.unwrap_or_default(),
            session_id,
            agent_id: ctx.agent_id,
            tool_name: request.tool_name.clone(),
            tool_use_id: request.tool_use_id.clone(),
            native_request_id: request.native_request_id.clone(),
            request: stored_request,
            status: ApprovalStatus::Pending,
            requested_at_ms: now,
            expires_at_ms: expires,
            decided_at_ms: None,
            decision_source: None,
            decided_by: None,
            decision_note: None,
        };
        let ctx2 = ctx.clone();
        let result = self
            .commit(move |tx| {
                repo::insert(tx, &record)?;
                let mut drafts = vec![draft(
                    DomainEvent::AgentPermissionRequested {
                        approval_id,
                        tool_name: record.tool_name.clone(),
                        tool_use_id: record.tool_use_id.clone(),
                        input: event_input,
                        description: request.description.clone(),
                        expires_at_ms: expires,
                    },
                    &ctx2,
                )?];
                let session = sessions::get(tx, session_id)?;
                if session.state == SessionState::Running {
                    sessions::transition(
                        tx,
                        session_id,
                        SessionState::WaitingForApproval,
                        &SessionChanges::default(),
                        now,
                    )?;
                    drafts.push(draft(
                        DomainEvent::AgentStateChanged {
                            from: "running".into(),
                            to: "waiting_for_approval".into(),
                        },
                        &ctx2,
                    )?);
                }
                Ok(((), drafts))
            })
            .await;
        match result {
            Ok(()) => {
                if let Some(a) = self.attached().get_mut(&session_id)
                    && a.state == SessionState::Running
                {
                    a.state = SessionState::WaitingForApproval;
                }
                let svc = self.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(timeout).await;
                    svc.expire(approval_id).await;
                });
            }
            Err(err) => {
                // Without a durable record there must be no way to approve:
                // deny immediately.
                tracing::error!(%session_id, ?err, "cannot record permission request; denying");
                let handle = self.attached().get(&session_id).map(|a| a.handle.clone());
                if let Some(handle) = handle {
                    let _ = handle
                        .decide(
                            request.native_request_id,
                            PermissionDecision::Deny {
                                message: "Switchyard could not record this permission request; denied."
                                    .into(),
                            },
                        )
                        .await;
                }
            }
        }
    }

    /// The agent withdrew a request (e.g. the turn was interrupted).
    pub(super) async fn on_permission_cancelled(&self, session_id: Uuid, native_request_id: String) {
        let lookup =
            self.inner.storage.call(move |c| repo::find_by_native(c, session_id, &native_request_id)).await;
        let Ok(Some(record)) = lookup else { return };
        if record.status != ApprovalStatus::Pending {
            return;
        }
        let id = record.id;
        let now = now_ms();
        match self
            .commit(move |tx| {
                settle_in_tx(
                    tx,
                    id,
                    ApprovalStatus::Cancelled,
                    DecisionSource::AgentCancelled,
                    None,
                    None,
                    now,
                )
            })
            .await
        {
            Ok(_) => self.refresh_cached_state(session_id).await,
            Err(err) if err.code == ErrorCode::Conflict => {}
            Err(err) => tracing::error!(%id, ?err, "cannot record cancelled permission request"),
        }
    }

    /// Operator decision from a client.
    pub async fn decide(
        &self,
        approval_id: Uuid,
        decision: ApprovalDecision,
        note: Option<String>,
        decided_by: String,
    ) -> Result<Approval, ProtocolError> {
        let current =
            self.inner.storage.call(move |c| repo::get(c, approval_id)).await.map_err(errors::storage)?;
        if current.status != ApprovalStatus::Pending {
            return Err(ProtocolError::new(
                ErrorCode::Conflict,
                format!("approval is already {}", current.status),
            )
            .with_data(json!({ "status": current.status })));
        }
        let Some(handle) = self.attached().get(&current.session_id).map(|a| a.handle.clone()) else {
            return Err(ProtocolError::new(
                ErrorCode::Conflict,
                "the requesting agent session is no longer running",
            ));
        };
        let (status, agent_decision) = match decision {
            ApprovalDecision::Approve => (ApprovalStatus::Approved, PermissionDecision::Allow),
            ApprovalDecision::Deny => (
                ApprovalStatus::Denied,
                PermissionDecision::Deny {
                    message: note.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| {
                        "The Switchyard operator denied this tool use. Do not retry it without new instructions."
                            .into()
                    }),
                },
            ),
        };
        let now = now_ms();
        let note2 = note.clone();
        let record = self
            .commit(move |tx| {
                settle_in_tx(
                    tx,
                    approval_id,
                    status,
                    DecisionSource::Operator,
                    Some(&decided_by),
                    note2.as_deref(),
                    now,
                )
            })
            .await?;
        self.refresh_cached_state(record.session_id).await;
        self.deliver(&record, handle, agent_decision).await;
        Ok(convert::approval(record))
    }

    /// Default-deny when nobody decided in time.
    pub(super) async fn expire(&self, approval_id: Uuid) {
        let now = now_ms();
        let record = match self
            .commit(move |tx| {
                settle_in_tx(
                    tx,
                    approval_id,
                    ApprovalStatus::Expired,
                    DecisionSource::Timeout,
                    None,
                    None,
                    now,
                )
            })
            .await
        {
            Ok(r) => r,
            Err(err) if err.code == ErrorCode::Conflict => return, // already settled
            Err(err) => {
                tracing::error!(%approval_id, ?err, "cannot expire approval");
                return;
            }
        };
        tracing::info!(%approval_id, tool = %record.tool_name, "permission request expired; denied");
        self.refresh_cached_state(record.session_id).await;
        let handle = self.attached().get(&record.session_id).map(|a| a.handle.clone());
        if let Some(handle) = handle {
            let message =
                "No decision was made in Switchyard before the request expired; the tool use was denied.";
            self.deliver(&record, handle, PermissionDecision::Deny { message: message.into() }).await;
        }
    }

    async fn deliver(
        &self,
        record: &ApprovalRecord,
        handle: switchyard_agents::claude::ClaudeSession,
        decision: PermissionDecision,
    ) {
        if let Err(err) = handle.decide(record.native_request_id.clone(), decision).await {
            let session_id = record.session_id;
            tracing::warn!(approval_id = %record.id, %err, "decision not delivered to agent");
            if let Ok(session) = self.inner.storage.call(move |c| sessions::get(c, session_id)).await {
                let event = DomainEvent::AgentWarning {
                    message: format!(
                        "decision for approval {} was recorded but not delivered: {err}",
                        record.id
                    ),
                };
                let _ = self.record(&Self::context(&session), vec![DraftOrEvent::Event(event)]).await;
            }
        }
    }

    /// Settles every pending approval of a session without delivering
    /// anything (the process is gone or going).
    pub(super) async fn settle_approvals(
        &self,
        session_id: Uuid,
        ctx: &EventContext,
        status: ApprovalStatus,
        source: DecisionSource,
    ) -> Result<Vec<ApprovalRecord>, ProtocolError> {
        let now = now_ms();
        let ctx = ctx.clone();
        self.commit(move |tx| {
            let settled = repo::settle_pending_for_session(tx, session_id, status, source, now)?;
            let drafts = settled.iter().map(|r| draft(resolved(r), &ctx)).collect::<Result<Vec<_>, _>>()?;
            Ok((settled, drafts))
        })
        .await
    }

    /// Core shutdown: cancel pending requests and tell the agent (deny) so
    /// its turn can end before the process is stopped.
    pub(super) async fn cancel_pending_for_shutdown(&self, session_id: Uuid) -> Result<(), ProtocolError> {
        let session =
            self.inner.storage.call(move |c| sessions::get(c, session_id)).await.map_err(errors::storage)?;
        let settled = self
            .settle_approvals(
                session_id,
                &Self::context(&session),
                ApprovalStatus::Cancelled,
                DecisionSource::CoreShutdown,
            )
            .await?;
        let handle = self.attached().get(&session_id).map(|a| a.handle.clone());
        if let Some(handle) = handle {
            for record in settled {
                let message = "Switchyard is shutting down; the tool use was not approved.";
                let _ = handle
                    .decide(record.native_request_id, PermissionDecision::Deny { message: message.into() })
                    .await;
            }
        }
        Ok(())
    }

    /// Re-reads the persisted state into the attached cache.
    async fn refresh_cached_state(&self, session_id: Uuid) {
        if let Ok(record) = self.inner.storage.call(move |c| sessions::get(c, session_id)).await
            && let Some(a) = self.attached().get_mut(&session_id)
        {
            a.state = record.state;
        }
    }

    pub async fn list_approvals(
        &self,
        status: Option<ApprovalStatus>,
        session_id: Option<Uuid>,
        project_id: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<Approval>, ProtocolError> {
        let records = self
            .inner
            .storage
            .call(move |c| repo::list(c, status, session_id, project_id, limit))
            .await
            .map_err(errors::storage)?;
        Ok(records.into_iter().map(convert::approval).collect())
    }
}
