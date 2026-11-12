//! Centralized approvals for agent permission requests.
//!
//! Invariants:
//! * every request that reaches Switchyard is recorded before anything else;
//! * a request is settled exactly once (SQL-guarded `pending → final`), by
//!   the operator, by timeout, by the agent cancelling it, by the process
//!   exiting, or by Core shutdown/restart;
//! * the decision is committed durably *before* it is delivered to the
//!   agent; a failed delivery is journaled as a warning;
//! * every request is evaluated against the policy first (ADR 0018):
//!   `allow` and `deny` are answered at once and leave no approval; only
//!   `ask` creates one;
//! * nothing pending is approved automatically; every non-operator path
//!   denies, and a policy change can only deny a pending request.

use serde_json::json;
use switchyard_agents::claude;
use switchyard_agents::event::{MAX_DURABLE_INPUT, bound_json};
use switchyard_agents::{
    ApprovalStatus, DecisionSource, PermissionDecision, PermissionRequest, SessionState,
};
use switchyard_events::{DomainEvent, EventContext, new_id, now_ms};
use switchyard_policy::{Decision as PolicyDecision, EvaluationInput, RequestScope};
use switchyard_protocol::methods::ApprovalDecision;
use switchyard_protocol::model::Approval;
use switchyard_protocol::{ErrorCode, ProtocolError};
use switchyard_storage::approvals as repo;
use switchyard_storage::policies;
use switchyard_storage::sessions::{self, SessionChanges};
use switchyard_storage::{ApprovalRecord, StorageError};
use uuid::Uuid;

use super::{AgentService, DraftOrEvent, convert, draft};
use crate::handlers::errors;
use crate::policy::{self, RequestOrigin};

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

fn wire_approval(c: &switchyard_storage::Connection, r: ApprovalRecord) -> Result<Approval, StorageError> {
    let ws = switchyard_storage::workspaces::for_session(c, r.session_id)?;
    let policy = policy::approval_policy(c, r.evaluation_id)?;
    Ok(convert::approval(r, ws, policy))
}

/// What happened to a new permission request.
enum Outcome {
    /// `ask`: a pending approval was created.
    Pending { approval_id: Uuid },
    /// `allow` or `deny`, decided by policy; no approval exists.
    Automatic { decision: PolicyDecision, explanation: String },
}

type Denial = ((ApprovalRecord, String), Vec<switchyard_events::EventDraft>);

/// Re-evaluates a pending approval under the current policy. If the policy
/// now blocks it, records the evaluation, settles the approval `denied`
/// (source `policy`) and returns it with the explanation. Otherwise writes
/// nothing: a pending approval never becomes more permissive.
fn policy_denial_in_tx(
    tx: &switchyard_storage::Transaction<'_>,
    approval_id: Uuid,
    evaluation_id: Uuid,
    trigger: &str,
    now: i64,
) -> Result<Option<Denial>, StorageError> {
    let approval = repo::get(tx, approval_id)?;
    if approval.status != ApprovalStatus::Pending {
        return Ok(None);
    }
    let Some(original) = policies::get_evaluation(tx, evaluation_id)? else {
        return Err(StorageError::Corrupt(format!(
            "evaluation {evaluation_id} of approval {approval_id} is missing"
        )));
    };
    let input = policy::stored_input(&original)?;
    let session = sessions::get(tx, approval.session_id)?;
    let origin = RequestOrigin {
        agent_id: approval.agent_id,
        native_request_id: approval.native_request_id.clone(),
        ctx: AgentService::context(&session),
    };
    // Evaluate without recording first: only a denial is recorded.
    let rules = policies::applicable_rules(tx, input.scope.project_id, input.scope.workspace_id)?;
    let revision = policies::current_revision(tx)?;
    if switchyard_policy::evaluate(&input, &rules, revision).decision != PolicyDecision::Deny {
        return Ok(None);
    }
    let recorded = policy::evaluate_in_tx(tx, &input, &origin, trigger, Some(approval_id), now)?;
    let explanation = recorded.evaluation.explanation.clone();
    let (record, mut drafts) = settle_in_tx(
        tx,
        approval_id,
        ApprovalStatus::Denied,
        DecisionSource::Policy,
        None,
        Some(&explanation),
        now,
    )?;
    let mut all = recorded.drafts;
    all.append(&mut drafts);
    Ok(Some(((record, explanation), all)))
}

impl AgentService {
    /// The single entry point for an agent permission request (ADR 0018):
    /// classify → path facts → evaluate and record in one transaction →
    /// allow, ask (pending approval) or deny. Every failure denies.
    pub(super) async fn on_permission_requested(
        &self,
        session_id: Uuid,
        ctx: &EventContext,
        request: PermissionRequest,
    ) {
        let native_request_id = request.native_request_id.clone();
        match self.evaluate_request(session_id, ctx, request).await {
            Ok(Outcome::Pending { approval_id }) => {
                if let Some(a) = self.attached().get_mut(&session_id)
                    && a.state == SessionState::Running
                {
                    a.state = SessionState::WaitingForApproval;
                }
                let svc = self.clone();
                let timeout = self.inner.config.approval_timeout;
                tokio::spawn(async move {
                    tokio::time::sleep(timeout).await;
                    svc.expire(approval_id).await;
                });
            }
            Ok(Outcome::Automatic { decision, explanation }) => {
                let decision = match decision {
                    PolicyDecision::Allow => PermissionDecision::Allow,
                    // `ask` never reaches here; treat anything else as deny.
                    PolicyDecision::Deny | PolicyDecision::Ask => PermissionDecision::Deny {
                        message: format!(
                            "{explanation} Do not retry this tool use without new instructions."
                        ),
                    },
                };
                self.deliver_native(session_id, native_request_id, decision).await;
            }
            Err(err) => {
                // Without a durable evaluation there must be no way to
                // proceed: deny immediately.
                tracing::error!(%session_id, ?err, "cannot evaluate or record permission request; denying");
                let message = "Switchyard could not evaluate or record this permission request; denied.";
                self.deliver_native(
                    session_id,
                    native_request_id,
                    PermissionDecision::Deny { message: message.into() },
                )
                .await;
            }
        }
    }

    async fn evaluate_request(
        &self,
        session_id: Uuid,
        ctx: &EventContext,
        request: PermissionRequest,
    ) -> Result<Outcome, ProtocolError> {
        let operation = claude::classify(&request);
        // Scope from Core's own records, never from the agent.
        let (session, workspace) = self
            .inner
            .storage
            .call(move |c| {
                let session = sessions::get(c, session_id)?;
                let workspace = match session.workspace_id {
                    Some(w) => Some(switchyard_storage::workspaces::get(c, w)?),
                    None => None,
                };
                Ok((session, workspace))
            })
            .await
            .map_err(errors::storage)?;
        let scope = RequestScope {
            project_id: session.project_id,
            session_id,
            workspace_id: workspace.as_ref().map(|w| w.id),
            workspace_project_id: workspace.as_ref().map(|w| w.project_id),
        };
        let paths = match &workspace {
            Some(ws) => {
                let root = std::path::PathBuf::from(&ws.path);
                let op = operation.clone();
                tokio::task::spawn_blocking(move || crate::policy::paths::facts(&root, &op))
                    .await
                    .map_err(|e| errors::internal("path resolution failed", e))?
            }
            None => Vec::new(),
        };
        let input = EvaluationInput { operation, paths, scope };
        let origin = RequestOrigin {
            agent_id: ctx.agent_id,
            native_request_id: request.native_request_id.clone(),
            ctx: ctx.clone(),
        };
        let now = now_ms();
        let timeout = self.inner.config.approval_timeout;
        let expires = now + i64::try_from(timeout.as_millis()).unwrap_or(i64::MAX / 2);
        let (stored_request, _) = bound_json(
            &json!({ "input": request.input, "description": request.description, "details": request.details }),
            MAX_STORED_REQUEST,
        );
        let (event_input, _) = bound_json(&request.input, MAX_DURABLE_INPUT);
        self.commit(move |tx| {
            let approval_id = new_id();
            let recorded = policy::evaluate_in_tx(tx, &input, &origin, "request", Some(approval_id), now)?;
            let mut drafts = recorded.drafts;
            let evaluation = recorded.evaluation;
            if evaluation.decision != PolicyDecision::Ask {
                return Ok((
                    Outcome::Automatic { decision: evaluation.decision, explanation: evaluation.explanation },
                    drafts,
                ));
            }
            let record = ApprovalRecord {
                id: approval_id,
                project_id: session.project_id,
                session_id,
                agent_id: origin.agent_id,
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
                evaluation_id: Some(recorded.evaluation_id),
            };
            repo::insert(tx, &record)?;
            drafts.push(draft(
                DomainEvent::AgentPermissionRequested {
                    approval_id,
                    tool_name: record.tool_name.clone(),
                    tool_use_id: record.tool_use_id.clone(),
                    input: event_input,
                    description: request.description.clone(),
                    expires_at_ms: expires,
                    evaluation_id: Some(recorded.evaluation_id),
                    explanation: Some(evaluation.explanation.clone()),
                },
                &origin.ctx,
            )?);
            let current = sessions::get(tx, session_id)?;
            if current.state == SessionState::Running {
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
                    &origin.ctx,
                )?);
            }
            Ok((Outcome::Pending { approval_id }, drafts))
        })
        .await
    }

    /// Delivers an automatic decision (no approval row exists).
    async fn deliver_native(
        &self,
        session_id: Uuid,
        native_request_id: String,
        decision: PermissionDecision,
    ) {
        let handle = self.attached().get(&session_id).map(|a| a.handle.clone());
        let Some(handle) = handle else { return };
        if let Err(err) = handle.decide(native_request_id.clone(), decision).await {
            tracing::warn!(%session_id, %err, "automatic decision not delivered to agent");
            if let Ok(session) = self.inner.storage.call(move |c| sessions::get(c, session_id)).await {
                let event = DomainEvent::AgentWarning {
                    message: format!(
                        "decision for permission request {native_request_id} was recorded but not delivered: {err}"
                    ),
                };
                let _ = self.record(&Self::context(&session), vec![DraftOrEvent::Event(event)]).await;
            }
        }
    }

    /// Re-evaluates every pending approval of `project_id` after a policy
    /// change. A request the policy now blocks is settled `denied`
    /// (source `policy`) and the agent is told; nothing is ever approved
    /// this way (ADR 0018). Returns the approvals denied.
    pub async fn reevaluate_pending(&self, project_id: Uuid) -> Vec<Uuid> {
        let pending = match self
            .inner
            .storage
            .call(move |c| repo::list(c, Some(ApprovalStatus::Pending), None, Some(project_id), u32::MAX))
            .await
        {
            Ok(p) => p,
            Err(err) => {
                tracing::error!(%project_id, ?err, "cannot list pending approvals for re-evaluation");
                return Vec::new();
            }
        };
        let mut denied = Vec::new();
        for approval in pending {
            let Some(evaluation_id) = approval.evaluation_id else { continue };
            let id = approval.id;
            let result = self
                .commit(move |tx| {
                    Ok(match policy_denial_in_tx(tx, id, evaluation_id, "reevaluation", now_ms())? {
                        Some((value, drafts)) => (Some(value), drafts),
                        None => (None, Vec::new()),
                    })
                })
                .await;
            match result {
                Ok(Some((record, explanation))) => {
                    denied.push(record.id);
                    self.refresh_cached_state(record.session_id).await;
                    let handle = self.attached().get(&record.session_id).map(|a| a.handle.clone());
                    if let Some(handle) = handle {
                        let message = format!(
                            "{explanation} The policy changed while this request was pending; it was denied."
                        );
                        self.deliver(&record, handle, PermissionDecision::Deny { message }).await;
                    }
                }
                Ok(None) => {}
                Err(err) if err.code == ErrorCode::Conflict => {} // settled meanwhile
                Err(err) => tracing::error!(%id, ?err, "cannot re-evaluate pending approval"),
            }
        }
        denied
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
        let check = (status == ApprovalStatus::Approved).then_some(current.evaluation_id).flatten();
        let (record, blocked) = self
            .commit(move |tx| {
                // A human approval cannot override a policy that blocks the
                // request now (ADR 0018): re-check inside the transaction.
                if let Some(evaluation_id) = check
                    && let Some(((record, explanation), drafts)) =
                        policy_denial_in_tx(tx, approval_id, evaluation_id, "decision_check", now)?
                {
                    return Ok(((record, Some(explanation)), drafts));
                }
                let (record, drafts) = settle_in_tx(
                    tx,
                    approval_id,
                    status,
                    DecisionSource::Operator,
                    Some(&decided_by),
                    note2.as_deref(),
                    now,
                )?;
                Ok(((record, None), drafts))
            })
            .await?;
        self.refresh_cached_state(record.session_id).await;
        if let Some(explanation) = blocked {
            let message = format!("{explanation} The operator's approval was refused by policy.");
            self.deliver(&record, handle, PermissionDecision::Deny { message }).await;
            return Err(ProtocolError::new(
                ErrorCode::PolicyDenied,
                format!("the current policy blocks this request, so it cannot be approved; it was denied. {explanation}"),
            )
            .with_data(json!({ "approval_id": record.id, "status": record.status })));
        }
        self.deliver(&record, handle, agent_decision).await;
        self.inner.storage.call(move |c| wire_approval(c, record)).await.map_err(errors::storage)
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
        self.inner
            .storage
            .call(move |c| {
                let records = repo::list(c, status, session_id, project_id, limit)?;
                records.into_iter().map(|r| wire_approval(c, r)).collect::<Result<Vec<_>, StorageError>>()
            })
            .await
            .map_err(errors::storage)
    }
}
