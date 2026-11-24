//! Workflow methods (M8.1, ADR 0033): role bindings, model choices and the
//! operator's control of a task's workflow. No agent path reaches these.

use switchyard_agents::AgentProvider;
use switchyard_protocol::methods::{
    AgentsModelsParams, AgentsModelsResult, ProjectIdParams, TaskIdParams, WorkflowResult,
    WorkflowsCancelParams, WorkflowsGetResult, WorkflowsRequestCorrectionParams, WorkflowsSetSettingsParams,
    WorkflowsStartParams,
};
use switchyard_protocol::model::WorkflowSettingsInfo;
use switchyard_protocol::{ErrorCode, ProtocolError};

use crate::connection::Connection;
use crate::state::CoreState;

fn actor(conn: &Connection) -> String {
    conn.client_name.get().cloned().unwrap_or_else(|| "unknown client".into())
}

pub(super) async fn models(
    state: &CoreState,
    p: AgentsModelsParams,
) -> Result<AgentsModelsResult, ProtocolError> {
    let provider = p.provider.parse::<AgentProvider>().map_err(|_| {
        ProtocolError::new(ErrorCode::InvalidParams, format!("unknown provider {:?}", p.provider))
    })?;
    Ok(AgentsModelsResult { catalog: state.agents.models(provider, p.refresh).await })
}

pub(super) async fn settings(
    state: &CoreState,
    p: ProjectIdParams,
) -> Result<WorkflowSettingsInfo, ProtocolError> {
    state.tasks.workflow_settings(p.project_id).await
}

pub(super) async fn set_settings(
    conn: &Connection,
    p: WorkflowsSetSettingsParams,
) -> Result<WorkflowSettingsInfo, ProtocolError> {
    conn.state.tasks.set_workflow_settings(p.project_id, p.bindings, actor(conn)).await
}

pub(super) async fn start(
    conn: &Connection,
    p: WorkflowsStartParams,
) -> Result<WorkflowResult, ProtocolError> {
    Ok(WorkflowResult { workflow: conn.state.tasks.start_workflow(p, actor(conn)).await? })
}

pub(super) async fn start_again(
    conn: &Connection,
    p: switchyard_protocol::methods::TasksStartAgainParams,
) -> Result<switchyard_protocol::methods::TasksStartAgainResult, ProtocolError> {
    conn.state.tasks.start_again(p, actor(conn)).await
}

pub(super) async fn get(state: &CoreState, p: TaskIdParams) -> Result<WorkflowsGetResult, ProtocolError> {
    Ok(WorkflowsGetResult { workflow: state.tasks.workflow(p.task_id).await? })
}

pub(super) async fn pause(conn: &Connection, p: TaskIdParams) -> Result<WorkflowResult, ProtocolError> {
    Ok(WorkflowResult { workflow: conn.state.tasks.pause_workflow(p.task_id, actor(conn)).await? })
}

pub(super) async fn set_step_through(
    conn: &Connection,
    p: switchyard_protocol::methods::WorkflowsSetStepThroughParams,
) -> Result<WorkflowResult, ProtocolError> {
    Ok(WorkflowResult { workflow: conn.state.tasks.set_step_through(p.task_id, p.on, actor(conn)).await? })
}

pub(super) async fn preview_next(
    conn: &Connection,
    p: TaskIdParams,
) -> Result<switchyard_protocol::methods::WorkflowsPreviewNextResult, ProtocolError> {
    conn.state.tasks.preview_next(p.task_id).await
}

pub(super) async fn resume(conn: &Connection, p: TaskIdParams) -> Result<WorkflowResult, ProtocolError> {
    Ok(WorkflowResult { workflow: conn.state.tasks.resume_workflow(p.task_id, actor(conn)).await? })
}

pub(super) async fn cancel(
    conn: &Connection,
    p: WorkflowsCancelParams,
) -> Result<WorkflowResult, ProtocolError> {
    Ok(WorkflowResult { workflow: conn.state.tasks.cancel_workflow(p.task_id, p.reason, actor(conn)).await? })
}

pub(super) async fn request_correction(
    conn: &Connection,
    p: WorkflowsRequestCorrectionParams,
) -> Result<WorkflowResult, ProtocolError> {
    Ok(WorkflowResult {
        workflow: conn.state.tasks.request_correction(p.task_id, p.note, actor(conn)).await?,
    })
}
