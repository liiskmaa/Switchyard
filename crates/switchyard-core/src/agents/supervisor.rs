//! Per-session supervision: turns adapter output into journal events, state
//! transitions and live output, and finalizes the session when the process
//! exits.

use std::collections::HashMap;

use switchyard_agents::claude::{ExitReport, SessionUpdate};
use switchyard_agents::{
    AdapterOutput, AgentEvent, ApprovalStatus, DecisionSource, SessionState, ToolOutcome,
};
use switchyard_events::{DomainEvent, EventContext, EventId};
use switchyard_storage::sessions::SessionChanges;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::{AgentService, DraftOrEvent, StopReason, draft};

/// Warnings journaled per session before further ones are only logged.
const MAX_WARNINGS: u32 = 25;

#[derive(Default)]
struct Tracking {
    /// tool_use_id → id of its `agent.tool_started` event (causation).
    tool_events: HashMap<String, EventId>,
    warnings: u32,
}

pub(super) async fn run(
    svc: AgentService,
    session_id: Uuid,
    ctx: EventContext,
    mut updates: mpsc::Receiver<SessionUpdate>,
) {
    let mut tracking = Tracking::default();
    let mut exit = None;
    while let Some(update) = updates.recv().await {
        match update {
            SessionUpdate::Output(AdapterOutput::Live(chunk)) => svc.live().publish(session_id, chunk),
            SessionUpdate::Output(AdapterOutput::Event(event)) => {
                svc.on_event(session_id, &ctx, event, &mut tracking).await;
            }
            SessionUpdate::Output(AdapterOutput::PermissionRequested(request)) => {
                svc.on_permission_requested(session_id, &ctx, request).await;
            }
            SessionUpdate::Output(AdapterOutput::PermissionCancelled { native_request_id }) => {
                svc.on_permission_cancelled(session_id, native_request_id).await;
            }
            SessionUpdate::Exited(report) => {
                exit = Some(report);
                break;
            }
        }
    }
    let report = exit.unwrap_or_else(|| ExitReport {
        exit: switchyard_agents::process::ExitInfo { code: None, signal: None },
        stop_requested: false,
        stderr_tail: "adapter task ended without an exit report".into(),
        malformed_lines: 0,
    });
    svc.on_exit(session_id, &ctx, report).await;
}

impl AgentService {
    async fn journal(&self, ctx: &EventContext, events: Vec<DraftOrEvent>) {
        if let Err(err) = self.record(ctx, events).await {
            tracing::error!(session_id = ?ctx.session_id, ?err, "cannot journal agent activity");
        }
    }

    async fn on_event(&self, session_id: Uuid, ctx: &EventContext, event: AgentEvent, t: &mut Tracking) {
        match event {
            AgentEvent::Ready { native_session_id, model, tools, .. } => {
                let state = self.attached().get(&session_id).map(|a| a.state);
                let Some(state) = state else { return };
                let changes = SessionChanges {
                    native_session_id: Some(native_session_id.clone()),
                    model: model.clone(),
                    ..Default::default()
                };
                let event = DomainEvent::AgentReady { native_session_id, model, tools };
                if let Err(err) = self.transition(session_id, state, changes, vec![event]).await {
                    tracing::error!(%session_id, ?err, "cannot record agent readiness");
                }
            }
            AgentEvent::Message { message_id, text, truncated } => {
                self.journal(
                    ctx,
                    vec![DraftOrEvent::Event(DomainEvent::AgentMessage { message_id, text, truncated })],
                )
                .await;
            }
            AgentEvent::ToolStarted { tool_use_id, tool_name, input, input_truncated } => {
                let event = DomainEvent::AgentToolStarted {
                    tool_use_id: tool_use_id.clone(),
                    tool_name,
                    input,
                    input_truncated,
                };
                match draft(event, ctx) {
                    Ok(d) => {
                        t.tool_events.insert(tool_use_id, d.id);
                        self.journal(ctx, vec![DraftOrEvent::Draft(d)]).await;
                    }
                    Err(err) => tracing::error!(?err, "cannot encode tool event"),
                }
            }
            AgentEvent::ToolFinished { tool_use_id, tool_name, outcome, output, output_truncated } => {
                let causation = t.tool_events.remove(&tool_use_id);
                let event = match outcome {
                    ToolOutcome::Succeeded => {
                        DomainEvent::AgentToolCompleted { tool_use_id, tool_name, output, output_truncated }
                    }
                    ToolOutcome::Failed | ToolOutcome::Rejected => DomainEvent::AgentToolFailed {
                        tool_use_id,
                        tool_name,
                        output,
                        output_truncated,
                        rejected: outcome == ToolOutcome::Rejected,
                    },
                };
                let ctx = EventContext { causation_id: causation, ..ctx.clone() };
                self.journal(&ctx, vec![DraftOrEvent::Event(event)]).await;
            }
            AgentEvent::TurnFinished {
                success,
                interrupted,
                stop_reason,
                reason,
                duration_ms,
                cost_usd,
                turn_cost_usd,
                result_text,
                ..
            } => {
                self.live().clear(session_id);
                let next = {
                    let mut map = self.attached();
                    let Some(a) = map.get_mut(&session_id) else { return };
                    if a.queued_turns > 0 {
                        a.queued_turns -= 1;
                        SessionState::Running
                    } else if interrupted {
                        SessionState::Interrupted
                    } else {
                        SessionState::Idle
                    }
                };
                let event = DomainEvent::AgentTurnCompleted {
                    success,
                    interrupted,
                    reason,
                    stop_reason,
                    duration_ms,
                    cost_usd,
                    turn_cost_usd,
                    result: result_text,
                };
                if let Err(err) =
                    self.transition(session_id, next, SessionChanges::default(), vec![event]).await
                {
                    tracing::error!(%session_id, ?err, "cannot record turn completion");
                }
            }
            AgentEvent::InterruptAcknowledged => {
                self.journal(ctx, vec![DraftOrEvent::Event(DomainEvent::AgentInterrupted {})]).await;
            }
            AgentEvent::Warning { message } => {
                t.warnings += 1;
                if t.warnings <= MAX_WARNINGS {
                    self.journal(ctx, vec![DraftOrEvent::Event(DomainEvent::AgentWarning { message })]).await;
                } else {
                    tracing::warn!(%session_id, %message, "agent warning (journal limit reached)");
                }
            }
        }
    }

    /// Finalizes a session whose process ended (for any reason).
    pub(super) async fn on_exit(&self, session_id: Uuid, ctx: &EventContext, report: ExitReport) {
        // The session stays in the attached map until its final state is
        // committed, so callers waiting for detachment (stop, shutdown)
        // observe the finished record.
        let (stopping, workspace_id) = match self.attached().get(&session_id) {
            Some(a) => (a.stopping, a.workspace_id),
            None => (None, ctx.workspace_id),
        };
        if let Some(ws) = workspace_id {
            self.inner.watchers.detach(ws);
        }
        self.live().clear(session_id);

        let source = if stopping == Some(StopReason::CoreShutdown) {
            DecisionSource::CoreShutdown
        } else {
            DecisionSource::AgentExited
        };
        if let Err(err) = self.settle_approvals(session_id, ctx, ApprovalStatus::Cancelled, source).await {
            tracing::error!(%session_id, ?err, "cannot settle approvals for exited session");
        }

        let ExitReport { exit, stop_requested, stderr_tail, malformed_lines } = report;
        let (state, failure) = match stopping {
            Some(StopReason::CoreShutdown) => (SessionState::Stopped, Some("core_shutdown".to_owned())),
            Some(StopReason::Operator) => (SessionState::Stopped, None),
            None if exit.code == Some(0) => (SessionState::Completed, None),
            None => {
                let (tail, _) = switchyard_agents::event::truncate(&stderr_tail, 1000);
                let mut detail = format!("process exited unexpectedly ({})", exit.describe());
                if !tail.is_empty() {
                    detail.push_str(&format!("; stderr: {tail}"));
                }
                if malformed_lines > 0 {
                    detail.push_str(&format!("; {malformed_lines} malformed output line(s)"));
                }
                (SessionState::Failed, Some(detail))
            }
        };
        let mut events = vec![DomainEvent::AgentExited {
            exit_code: exit.code,
            exit_signal: exit.signal,
            stop_requested: stop_requested || stopping.is_some(),
            final_state: state.as_str().into(),
        }];
        if state == SessionState::Failed {
            events
                .push(DomainEvent::AgentFailed { reason: "process_exited".into(), detail: failure.clone() });
        }
        let changes = SessionChanges {
            pid: Some(None),
            ended_at_ms: Some(Some(switchyard_events::now_ms())),
            exit_code: Some(exit.code),
            exit_signal: Some(exit.signal),
            failure: Some(failure),
            ..Default::default()
        };
        if let Err(err) = self.transition(session_id, state, changes, events).await {
            tracing::error!(%session_id, ?err, "cannot finalize session");
        }
        self.attached().remove(&session_id);
        tracing::info!(%session_id, state = %state, exit = %exit.describe(), "agent session ended");
        // The workspace outlives the session: it returns to ready/dirty and
        // is never deleted here.
        if let Some(ws) = workspace_id {
            self.inner.workspaces.release(ws).await;
        }
    }
}
