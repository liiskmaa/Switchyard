//! Task methods (ADRs 0023-0027). Every decision here is the operator's: no
//! agent path reaches these handlers.

use switchyard_protocol::ProtocolError;
use switchyard_protocol::methods::{
    TaskAssignmentIdParams, TaskAssignmentResult, TaskDecisionParams, TaskDependencyParams, TaskDetail,
    TaskIdParams, TaskResult, TasksAssignParams, TasksCreateParams, TasksDecideProposalParams,
    TasksDecideProposalResult, TasksListParams, TasksListResult, TasksNoteParams, TasksNoteResult,
    TasksUpdateParams,
};

use super::agents::limit;
use crate::connection::Connection;
use crate::state::CoreState;

fn actor(conn: &Connection) -> String {
    conn.client_name.get().cloned().unwrap_or_else(|| "unknown client".into())
}

pub(super) async fn list(state: &CoreState, p: TasksListParams) -> Result<TasksListResult, ProtocolError> {
    Ok(TasksListResult { tasks: state.tasks.list(p.project_id, p.include_closed, limit(p.limit)?).await? })
}

pub(super) async fn get(state: &CoreState, p: TaskIdParams) -> Result<TaskDetail, ProtocolError> {
    state.tasks.get(p.task_id).await
}

pub(super) async fn create(state: &CoreState, p: TasksCreateParams) -> Result<TaskResult, ProtocolError> {
    Ok(TaskResult { task: state.tasks.create(p).await? })
}

pub(super) async fn update(state: &CoreState, p: TasksUpdateParams) -> Result<TaskResult, ProtocolError> {
    Ok(TaskResult { task: state.tasks.update(p).await? })
}

pub(super) async fn publish(state: &CoreState, p: TaskIdParams) -> Result<TaskResult, ProtocolError> {
    Ok(TaskResult { task: state.tasks.publish(p.task_id).await? })
}

pub(super) async fn add_dependency(
    state: &CoreState,
    p: TaskDependencyParams,
) -> Result<TaskResult, ProtocolError> {
    Ok(TaskResult { task: state.tasks.add_dependency(p.task_id, p.depends_on).await? })
}

pub(super) async fn remove_dependency(
    state: &CoreState,
    p: TaskDependencyParams,
) -> Result<TaskResult, ProtocolError> {
    Ok(TaskResult { task: state.tasks.remove_dependency(p.task_id, p.depends_on).await? })
}

pub(super) async fn assign(
    conn: &Connection,
    p: TasksAssignParams,
) -> Result<TaskAssignmentResult, ProtocolError> {
    conn.state.tasks.assign(p, actor(conn)).await
}

pub(super) async fn start_assignment(
    conn: &Connection,
    p: TaskAssignmentIdParams,
) -> Result<TaskAssignmentResult, ProtocolError> {
    conn.state.tasks.start_assignment(p.assignment_id).await
}

pub(super) async fn cancel_assignment(
    conn: &Connection,
    p: TaskAssignmentIdParams,
) -> Result<TaskAssignmentResult, ProtocolError> {
    conn.state.tasks.cancel_assignment(p.assignment_id, p.reason, actor(conn)).await
}

pub(super) async fn note(conn: &Connection, p: TasksNoteParams) -> Result<TasksNoteResult, ProtocolError> {
    conn.state.tasks.note(p.task_id, p.text, actor(conn)).await
}

pub(super) async fn complete(conn: &Connection, p: TaskDecisionParams) -> Result<TaskResult, ProtocolError> {
    Ok(TaskResult { task: conn.state.tasks.complete(p.task_id, p.note, actor(conn)).await? })
}

pub(super) async fn fail(conn: &Connection, p: TaskDecisionParams) -> Result<TaskResult, ProtocolError> {
    Ok(TaskResult { task: conn.state.tasks.fail(p.task_id, p.note, actor(conn)).await? })
}

pub(super) async fn cancel(conn: &Connection, p: TaskDecisionParams) -> Result<TaskResult, ProtocolError> {
    Ok(TaskResult { task: conn.state.tasks.cancel(p.task_id, p.note, actor(conn)).await? })
}

pub(super) async fn retry(conn: &Connection, p: TaskDecisionParams) -> Result<TaskResult, ProtocolError> {
    Ok(TaskResult { task: conn.state.tasks.retry(p.task_id, p.note, actor(conn)).await? })
}

pub(super) async fn decide_proposal(
    conn: &Connection,
    p: TasksDecideProposalParams,
) -> Result<TasksDecideProposalResult, ProtocolError> {
    conn.state.tasks.decide_proposal(p.proposal_id, p.approve, p.note, actor(conn)).await
}
