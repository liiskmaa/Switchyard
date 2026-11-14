//! Storage records → wire types.

use switchyard_protocol::model::{Task, TaskAssignment, TaskFinding, TaskMessage, TaskProposal, TaskRef};
use switchyard_storage::tasks::{
    self as store, AssignmentRecord, FindingRecord, MessageRecord, ProposalRecord, TaskRecord,
};
use switchyard_storage::{Connection, StorageError};
use switchyard_tasks::ProposalStatus;

/// The wire assignment, with the model its session reported (if it started).
pub(crate) fn assignment(
    c: &switchyard_storage::Connection,
    a: AssignmentRecord,
) -> Result<TaskAssignment, switchyard_storage::StorageError> {
    let actual_model = match a.session_id {
        Some(s) => match switchyard_storage::sessions::get(c, s) {
            Ok(s) => s.model,
            Err(switchyard_storage::StorageError::NotFound(_)) => None,
            Err(e) => return Err(e),
        },
        None => None,
    };
    Ok(TaskAssignment {
        id: a.id,
        task_id: a.task_id,
        role: a.role,
        provider: a.provider,
        workspace_id: a.workspace_id,
        session_id: a.session_id,
        status: a.status,
        context: a.context,
        context_version: a.context_version,
        outcome: a.outcome,
        created_at_ms: a.created_at_ms,
        started_at_ms: a.started_at_ms,
        ended_at_ms: a.ended_at_ms,
        origin: a.origin,
        step_key: a.step_key,
        model: a.model,
        actual_model,
    })
}

pub(crate) fn message(m: MessageRecord) -> TaskMessage {
    TaskMessage {
        id: m.id,
        task_id: m.task_id,
        assignment_id: m.assignment_id,
        kind: m.kind,
        sender: m.sender,
        sender_session_id: m.sender_session_id,
        recipient: m.recipient,
        payload: m.payload,
        causation_id: m.causation_id,
        delivery: m.delivery,
        delivered_at_ms: m.delivered_at_ms,
        created_at_ms: m.created_at_ms,
    }
}

pub(crate) fn finding(f: FindingRecord) -> TaskFinding {
    TaskFinding {
        id: f.id,
        task_id: f.task_id,
        message_id: f.message_id,
        assignment_id: f.assignment_id,
        severity: f.severity,
        title: f.title,
        explanation: f.explanation,
        suggestion: f.suggestion,
        path: f.path,
        line: f.line,
        status: f.status,
        created_at_ms: f.created_at_ms,
        resolved_at_ms: f.resolved_at_ms,
    }
}

pub(crate) fn proposal(p: ProposalRecord) -> TaskProposal {
    TaskProposal {
        id: p.id,
        task_id: p.task_id,
        message_id: p.message_id,
        position: p.position,
        proposal: p.proposal,
        status: p.status,
        decided_by: p.decided_by,
        decision_note: p.decision_note,
        created_task_id: p.created_task_id,
        created_at_ms: p.created_at_ms,
        decided_at_ms: p.decided_at_ms,
    }
}

pub(crate) fn task_ref(t: &TaskRecord) -> TaskRef {
    TaskRef { id: t.id, title: t.title.clone(), status: t.status }
}

/// The wire task, with its derived fields.
pub(crate) fn task(conn: &Connection, t: TaskRecord) -> Result<Task, StorageError> {
    let open_assignment = store::open_assignment(conn, t.id)?.map(|a| assignment(conn, a)).transpose()?;
    let dependency_ids = store::dependencies_of(conn, t.id)?.into_iter().map(|(id, _)| id).collect();
    let pending_proposals =
        store::proposals_for_task(conn, t.id)?.iter().filter(|p| p.status == ProposalStatus::Pending).count()
            as u32;
    let open_findings =
        store::findings_for_task(conn, t.id)?.iter().filter(|f| f.status == "open").count() as u32;
    Ok(Task {
        id: t.id,
        project_id: t.project_id,
        parent_task_id: t.parent_task_id,
        title: t.title,
        description: t.description,
        kind: t.kind,
        priority: t.priority,
        acceptance_criteria: t.acceptance_criteria,
        status: t.status,
        status_reason: t.status_reason,
        result: t.result,
        failure: t.failure,
        origin: t.origin,
        origin_proposal_id: t.origin_proposal_id,
        revision: t.revision,
        created_at_ms: t.created_at_ms,
        updated_at_ms: t.updated_at_ms,
        completed_at_ms: t.completed_at_ms,
        open_assignment,
        dependency_ids,
        pending_proposals,
        open_findings,
    })
}
