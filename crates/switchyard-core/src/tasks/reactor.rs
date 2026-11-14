//! Reactions to agent sessions that work on assignments.
//!
//! The reactor follows the journal through a lossless bus subscription that
//! starts at the lowest `last_event_seq` of the active assignments, so after a
//! Core restart it first replays what it has not processed yet. Each reaction
//! advances the assignment's `last_event_seq` in the same transaction as its
//! writes, so replaying an event never applies it twice.

use std::time::Duration;

use serde_json::{Value, json};
use switchyard_events::{DomainEvent, EventDraft, EventEnvelope, EventFilter, RecvError, now_ms};
use switchyard_protocol::ProtocolError;
use switchyard_storage::tasks::{self as store, AssignmentRecord, FindingRecord, ProposalRecord};
use switchyard_storage::{Connection, StorageError};
use switchyard_tasks::{
    AssignmentStatus, MessageKind, ProposalStatus, Report, ReportError, ReportKind, ReviewResult, Role,
    Severity, TaskStatus, parse_report,
};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::assign::end_assignment;
use super::{Checked, TaskService, check, new_message, record_message, set_status, task_context};
use crate::journal::{commit_checked, draft};
use crate::state::wait_shutdown;

const WATCHED: [&str; 3] = ["agent.turn_completed", "agent.exited", "agent.failed"];
/// Agent text kept in a malformed-report message for the operator.
const EXCERPT: usize = 600;

/// Starts the reactor; it ends when Core shuts down.
pub(crate) fn spawn_reactor(
    svc: TaskService,
    mut shutdown: watch::Receiver<Option<String>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let start = svc
            .storage
            .call(|c| {
                let lowest = store::active_assignments(c)?.iter().map(|a| a.last_event_seq).min();
                Ok(match lowest {
                    Some(seq) => seq,
                    None => switchyard_storage::events::last_seq(c)?.unwrap_or(0),
                })
            })
            .await;
        let start = match start {
            Ok(s) => s,
            Err(err) => {
                tracing::error!(
                    ?err,
                    "task reactor cannot read its starting point; tasks will not follow agents"
                );
                return;
            }
        };
        let filter = EventFilter { event_types: WATCHED.map(str::to_owned).to_vec(), ..Default::default() };
        let mut sub = svc.bus.subscribe(Some(start), filter);
        loop {
            tokio::select! {
                () = wait_shutdown(&mut shutdown) => break,
                next = sub.next() => match next {
                    Ok(event) => svc.react(&event).await,
                    Err(RecvError::Closed) => break,
                    Err(err) => {
                        tracing::error!(?err, "task reactor cannot read the journal; retrying");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                },
            }
        }
    })
}

/// What a turn's report means for its assignment.
enum Applied {
    /// Nothing to do (replayed, or the assignment ended meanwhile).
    Nothing,
    /// The assignment continues (a question, or no valid report yet).
    Continue,
    /// The assignment ended with a report; its session can be stopped.
    Ended(Uuid),
}

impl TaskService {
    async fn react(&self, event: &EventEnvelope) {
        let Some(session_id) = event.context.session_id else { return };
        let seq = event.seq;
        let found = self.storage.call(move |c| store::active_assignment_for_session(c, session_id)).await;
        let assignment = match found {
            Ok(Some(a)) if seq > a.last_event_seq => a,
            Ok(_) => return,
            Err(err) => {
                tracing::error!(%session_id, ?err, "task reactor cannot look up the assignment");
                return;
            }
        };
        let decoded = match event.decode() {
            Ok(d) => d,
            Err(err) => {
                tracing::warn!(?err, "task reactor skipped an undecodable event");
                return;
            }
        };
        if !matches!(
            decoded,
            DomainEvent::AgentTurnCompleted { .. }
                | DomainEvent::AgentExited { .. }
                | DomainEvent::AgentFailed { .. }
        ) {
            return;
        }
        // A reaction is idempotent (its transaction advances `last_event_seq`),
        // so a transient failure (journal read, workspace scan) is retried
        // instead of leaving the assignment stuck until the next restart.
        let mut attempt = 0;
        let outcome = loop {
            let outcome = match decoded.clone() {
                DomainEvent::AgentTurnCompleted { success, interrupted, reason, result, .. } => {
                    self.on_turn(&assignment, event, session_id, success, interrupted, reason, result).await
                }
                DomainEvent::AgentExited { final_state, .. } => {
                    let why =
                        format!("the agent session ended ({final_state}) before the assignment reported");
                    self.on_session_ended(&assignment, seq, why).await
                }
                DomainEvent::AgentFailed { reason, detail } => {
                    let why = match detail {
                        Some(d) => {
                            format!("the agent session failed ({reason}: {d}) before the assignment reported")
                        }
                        None => format!("the agent session failed ({reason}) before the assignment reported"),
                    };
                    self.on_session_ended(&assignment, seq, why).await
                }
                _ => return,
            };
            attempt += 1;
            match outcome {
                Err(err) if attempt < 4 => {
                    tracing::warn!(assignment = %assignment.id, ?err, attempt, "task reactor retrying an event");
                    tokio::time::sleep(Duration::from_millis(250 << attempt)).await;
                }
                other => break other,
            }
        };
        match outcome {
            Ok(Applied::Ended(session)) => {
                // Free the workspace for the next assignment (a reviewer).
                // `on_turn` registered the stop before committing, so a start
                // that sees the reported task also sees this stop and waits.
                let svc = self.clone();
                tokio::spawn(async move {
                    svc.stop_session(session).await;
                    svc.stopping().remove(&session);
                });
            }
            Ok(Applied::Continue | Applied::Nothing) => {}
            Err(err) => {
                tracing::error!(assignment = %assignment.id, ?err, "task reactor could not apply an event")
            }
        }
    }

    /// The text of the turn that just ended: the assistant messages journaled
    /// since the last processed event, then the turn's result text.
    async fn turn_text(
        &self,
        a: &AssignmentRecord,
        session_id: Uuid,
        until: i64,
        result: Option<String>,
    ) -> Result<(String, bool), ProtocolError> {
        let mut text = String::new();
        let mut truncated = false;
        let mut after = a.last_event_seq;
        loop {
            let page = self
                .bus
                .read(EventFilter {
                    after_seq: Some(after),
                    session_id: Some(session_id),
                    event_types: vec!["agent.message".into()],
                    limit: 500,
                    ..Default::default()
                })
                .await
                .map_err(|e| crate::handlers::errors::internal("journal read failed", e))?;
            let full = page.len() == 500;
            for e in page.iter().filter(|e| e.seq < until) {
                if let Ok(DomainEvent::AgentMessage { text: t, truncated: tr, .. }) = e.decode() {
                    text.push_str(&t);
                    text.push_str("\n\n");
                    truncated |= tr;
                }
            }
            match page.last() {
                Some(last) if full && last.seq < until => after = last.seq,
                _ => break,
            }
        }
        if let Some(r) = result {
            text.push_str(&r);
        }
        Ok((text, truncated))
    }

    #[allow(clippy::too_many_arguments)]
    async fn on_turn(
        &self,
        a: &AssignmentRecord,
        event: &EventEnvelope,
        session_id: Uuid,
        success: bool,
        interrupted: bool,
        reason: Option<String>,
        result: Option<String>,
    ) -> Result<Applied, ProtocolError> {
        let (text, truncated) = self.turn_text(a, session_id, event.seq, result).await?;
        let parsed = parse_report(&text, a.role);
        // A review is checked against the workspace as it was when it began.
        let workspace_changed = match (&parsed, a.role, &a.review_fingerprint) {
            // Every final report of a review (a verdict or a blocker) is checked.
            (Ok(r), Role::Review, Some(before)) if r.kind != ReportKind::Question => {
                let path = self.workspaces.record(a.workspace_id).await?.path;
                super::assign::workspace_fingerprint(&path).await? != *before
            }
            _ => false,
        };
        let (aid, seq, event_id) = (a.id, event.seq, event.id.to_string());
        let ends = matches!(&parsed, Ok(r) if r.kind != ReportKind::Question);
        if ends {
            self.stopping().insert(session_id, a.workspace_id);
        }
        let outcome = commit_checked(&self.storage, &self.bus, move |tx| {
            let now = now_ms();
            let mut drafts = Vec::new();
            if !store::advance_assignment_seq(tx, aid, seq)? {
                return Ok(Ok((Applied::Nothing, drafts)));
            }
            let a = store::get_assignment(tx, aid)?;
            if a.status != AssignmentStatus::Active {
                return Ok(Ok((Applied::Nothing, drafts)));
            }
            let applied = match parsed {
                Ok(report) => check!(apply_report(
                    tx,
                    &a,
                    session_id,
                    &event_id,
                    report,
                    workspace_changed,
                    now,
                    &mut drafts
                )?),
                Err(err) => {
                    record_unusable_turn(
                        tx,
                        &a,
                        session_id,
                        &event_id,
                        &err,
                        &text,
                        truncated,
                        success,
                        interrupted,
                        reason,
                        now,
                        &mut drafts,
                    )?;
                    Applied::Continue
                }
            };
            Ok(Ok((applied, drafts)))
        })
        .await;
        if ends && !matches!(outcome, Ok(Applied::Ended(_))) {
            self.stopping().remove(&session_id);
        }
        outcome
    }

    async fn on_session_ended(
        &self,
        a: &AssignmentRecord,
        seq: i64,
        why: String,
    ) -> Result<Applied, ProtocolError> {
        let aid = a.id;
        // A review that ends without a verdict is still checked against the
        // workspace as it was when it began.
        let workspace_changed = match (a.role, &a.review_fingerprint) {
            (Role::Review, Some(before)) => {
                let path = self.workspaces.record(a.workspace_id).await?.path;
                super::assign::workspace_fingerprint(&path).await? != *before
            }
            _ => false,
        };
        let why =
            if workspace_changed { format!("{why}; the workspace changed during the review") } else { why };
        commit_checked(&self.storage, &self.bus, move |tx| {
            let now = now_ms();
            let mut drafts = Vec::new();
            if !store::advance_assignment_seq(tx, aid, seq)? {
                return Ok(Ok((Applied::Nothing, drafts)));
            }
            let a = store::get_assignment(tx, aid)?;
            if a.status != AssignmentStatus::Active {
                return Ok(Ok((Applied::Nothing, drafts)));
            }
            let t = store::get_task(tx, a.task_id)?;
            let m = new_message(
                &t,
                Some(a.id),
                MessageKind::StatusUpdate,
                "core",
                a.session_id,
                "operator",
                json!({ "event": "session_ended", "reason": why, "workspace_changed": workspace_changed }),
                None,
                "recorded",
                now,
            );
            record_message(tx, &t, &m, Some(&why), &mut drafts)?;
            if workspace_changed {
                store::insert_finding(tx, &workspace_changed_finding(&t, m.id, a.id, now))?;
            }
            check!(super::assign::abandon_assignment(
                tx,
                a.id,
                AssignmentStatus::Failed,
                &why,
                true,
                now,
                &mut drafts
            )?);
            Ok(Ok((Applied::Nothing, drafts)))
        })
        .await
    }
}

fn message_kind(kind: ReportKind) -> MessageKind {
    match kind {
        ReportKind::Findings => MessageKind::Findings,
        ReportKind::Proposal => MessageKind::Proposal,
        ReportKind::Implementation => MessageKind::CompletionReport,
        ReportKind::Review => MessageKind::ReviewResult,
        ReportKind::Blocker => MessageKind::Blocker,
        ReportKind::Question => MessageKind::InfoRequest,
    }
}

/// Records a valid report and moves the assignment and task on. Nothing here
/// authorizes anything: findings and proposals wait for the operator, and a
/// review verdict never completes a task.
#[allow(clippy::too_many_arguments)]
fn apply_report(
    tx: &Connection,
    a: &AssignmentRecord,
    session_id: Uuid,
    event_id: &str,
    report: Report,
    workspace_changed: bool,
    now: i64,
    drafts: &mut Vec<EventDraft>,
) -> Checked<Applied> {
    let t = store::get_task(tx, a.task_id)?;
    let effective = match report.result {
        Some(_) if workspace_changed => Some(ReviewResult::Blocked),
        r => r,
    };
    let payload = json!({
        "report": report,
        "effective_result": effective,
        "workspace_changed": workspace_changed,
    });
    let m = new_message(
        &t,
        Some(a.id),
        message_kind(report.kind),
        "agent",
        Some(session_id),
        "operator",
        payload,
        Some(event_id.to_owned()),
        "recorded",
        now,
    );
    if !record_message(tx, &t, &m, Some(&report.summary), drafts)? {
        return Ok(Ok(Applied::Nothing));
    }
    if report.kind == ReportKind::Review {
        // A new review supersedes the findings of earlier ones.
        store::resolve_findings(tx, t.id, now)?;
    }
    let mut findings: Vec<FindingRecord> = report
        .findings
        .iter()
        .map(|f| FindingRecord {
            id: switchyard_events::new_id(),
            project_id: t.project_id,
            task_id: t.id,
            message_id: m.id,
            assignment_id: Some(a.id),
            severity: f.severity,
            title: f.title.clone(),
            explanation: f.explanation.clone(),
            suggestion: f.suggestion.clone(),
            path: f.path.clone(),
            line: f.line,
            status: "open".into(),
            created_at_ms: now,
            resolved_at_ms: None,
        })
        .collect();
    if workspace_changed {
        findings.push(workspace_changed_finding(&t, m.id, a.id, now));
    }
    for f in &findings {
        store::insert_finding(tx, f)?;
    }
    for (i, s) in report.proposed_subtasks.iter().enumerate() {
        store::insert_proposal(
            tx,
            &ProposalRecord {
                id: switchyard_events::new_id(),
                project_id: t.project_id,
                task_id: t.id,
                message_id: m.id,
                position: i as u32,
                proposal: serde_json::to_value(s).unwrap_or(Value::Null),
                status: ProposalStatus::Pending,
                decided_by: None,
                decision_note: None,
                created_task_id: None,
                created_at_ms: now,
                decided_at_ms: None,
            },
        )?;
    }
    if report.kind == ReportKind::Question {
        return Ok(Ok(Applied::Continue));
    }
    let (outcome, next, reason) = match (report.kind, a.role) {
        (ReportKind::Review, _) => {
            let result = effective.unwrap_or(ReviewResult::Blocked);
            drafts.push(draft(
                switchyard_events::DomainEvent::TaskReviewRecorded {
                    assignment_id: a.id,
                    reported_result: report.result.map(|r| r.as_str()).unwrap_or("none").into(),
                    result: result.as_str().into(),
                    workspace_changed,
                    findings: findings.len() as u32,
                },
                &task_context(&t),
            )?);
            (format!("review: {result}"), TaskStatus::AwaitingReview, format!("review: {result}"))
        }
        (ReportKind::Blocker, Role::Review) => {
            let changed = if workspace_changed { " (the workspace changed during the review)" } else { "" };
            (
                "blocker".to_owned(),
                TaskStatus::AwaitingReview,
                format!("review blocked{changed}: {}", report.summary),
            )
        }
        (ReportKind::Blocker, _) => {
            ("blocker".to_owned(), TaskStatus::Blocked, format!("agent: {}", report.summary))
        }
        (kind, _) => {
            let k = serde_json::to_value(kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default();
            (format!("reported: {k}"), TaskStatus::AwaitingReview, format!("{} reported: {k}", a.role))
        }
    };
    end_assignment(tx, &t, a, AssignmentStatus::Reported, &outcome, now, drafts)?;
    check!(set_status(tx, t.id, next, Some(super::bounded(&reason, 500)), None, None, now, drafts)?);
    Ok(Ok(Applied::Ended(session_id)))
}

/// A turn ended without a usable report: recorded for the operator; the
/// assignment stays active (the operator can send a note or cancel it).
#[allow(clippy::too_many_arguments)]
fn record_unusable_turn(
    tx: &Connection,
    a: &AssignmentRecord,
    session_id: Uuid,
    event_id: &str,
    err: &ReportError,
    text: &str,
    truncated: bool,
    success: bool,
    interrupted: bool,
    reason: Option<String>,
    now: i64,
    drafts: &mut Vec<EventDraft>,
) -> Result<(), StorageError> {
    let t = store::get_task(tx, a.task_id)?;
    let ended_normally = success && !interrupted;
    let (kind, summary) = match err {
        ReportError::Missing if !ended_normally => (
            MessageKind::StatusUpdate,
            format!(
                "the turn ended ({}) without a report",
                if interrupted { "interrupted".to_owned() } else { reason.clone().unwrap_or_else(|| "failed".into()) }
            ),
        ),
        ReportError::Missing if truncated => (
            MessageKind::MalformedReport,
            "no report found; the final message was longer than Switchyard records, so a report at its end was lost"
                .to_owned(),
        ),
        other => (MessageKind::MalformedReport, other.to_string()),
    };
    let start = text.len().saturating_sub(EXCERPT);
    let start = (start..text.len()).find(|i| text.is_char_boundary(*i)).unwrap_or(text.len());
    let m = new_message(
        &t,
        Some(a.id),
        kind,
        "core",
        Some(session_id),
        "operator",
        json!({
            "error": summary,
            "turn_success": success,
            "interrupted": interrupted,
            "message_truncated": truncated,
            "excerpt": &text[start..],
        }),
        Some(event_id.to_owned()),
        "recorded",
        now,
    );
    record_message(tx, &t, &m, Some(&summary), drafts)?;
    Ok(())
}

/// The finding Core adds when a review's before/after workspace scans differ.
fn workspace_changed_finding(
    t: &switchyard_storage::tasks::TaskRecord,
    message_id: Uuid,
    assignment_id: Uuid,
    now: i64,
) -> FindingRecord {
    FindingRecord {
        id: switchyard_events::new_id(),
        project_id: t.project_id,
        task_id: t.id,
        message_id,
        assignment_id: Some(assignment_id),
        severity: Severity::High,
        title: "The workspace changed during the review".into(),
        explanation: Some(
            "Switchyard scanned the workspace when the review started and again when it ended, and the two scans \
             differ. A verdict is recorded as blocked; inspect the workspace before relying on the review."
                .into(),
        ),
        suggestion: None,
        path: None,
        line: None,
        status: "open".into(),
        created_at_ms: now,
        resolved_at_ms: None,
    }
}
