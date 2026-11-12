//! Workspace methods.

use switchyard_protocol::methods::{
    WorkspaceCleanupResult, WorkspaceDetail, WorkspaceIdParams, WorkspaceResult, WorkspacesCleanupParams,
    WorkspacesCreateParams, WorkspacesListParams, WorkspacesListResult,
};
use switchyard_protocol::model::WorkspaceGitStatus;
use switchyard_protocol::{ErrorCode, ProtocolError};
use switchyard_storage::sessions::SessionFilter;

use crate::state::CoreState;

/// Sessions returned by `workspaces.get`.
const SESSIONS_PER_WORKSPACE: u32 = 50;

pub(crate) async fn list(
    state: &CoreState,
    p: WorkspacesListParams,
) -> Result<WorkspacesListResult, ProtocolError> {
    Ok(WorkspacesListResult { workspaces: state.workspaces.list(p.project_id, p.include_archived).await? })
}

pub(crate) async fn get(state: &CoreState, p: WorkspaceIdParams) -> Result<WorkspaceDetail, ProtocolError> {
    let workspace = state.workspaces.get(p.workspace_id).await?;
    let filter = SessionFilter { workspace_id: Some(workspace.id), ..Default::default() };
    let sessions = state.agents.list(filter, SESSIONS_PER_WORKSPACE).await?;
    Ok(WorkspaceDetail { workspace, sessions })
}

pub(crate) async fn create(
    state: &CoreState,
    p: WorkspacesCreateParams,
) -> Result<WorkspaceResult, ProtocolError> {
    Ok(WorkspaceResult { workspace: state.workspaces.create(p.project_id, p.name, p.base_ref).await? })
}

pub(crate) async fn git_status(
    state: &CoreState,
    p: WorkspaceIdParams,
) -> Result<WorkspaceGitStatus, ProtocolError> {
    state.workspaces.git_status(p.workspace_id).await
}

pub(crate) async fn verify(
    state: &CoreState,
    p: WorkspaceIdParams,
) -> Result<WorkspaceResult, ProtocolError> {
    Ok(WorkspaceResult { workspace: state.workspaces.verify(p.workspace_id).await? })
}

pub(crate) async fn archive(
    state: &CoreState,
    p: WorkspaceIdParams,
) -> Result<WorkspaceResult, ProtocolError> {
    Ok(WorkspaceResult { workspace: state.workspaces.archive(p.workspace_id).await? })
}

pub(crate) async fn restore(
    state: &CoreState,
    p: WorkspaceIdParams,
) -> Result<WorkspaceResult, ProtocolError> {
    Ok(WorkspaceResult { workspace: state.workspaces.restore(p.workspace_id).await? })
}

pub(crate) async fn complete(
    state: &CoreState,
    p: WorkspaceIdParams,
) -> Result<WorkspaceResult, ProtocolError> {
    Ok(WorkspaceResult { workspace: state.workspaces.complete(p.workspace_id).await? })
}

pub(crate) async fn cleanup_check(
    state: &CoreState,
    p: WorkspaceIdParams,
) -> Result<WorkspaceCleanupResult, ProtocolError> {
    let (workspace, report) = state.workspaces.cleanup_check(p.workspace_id).await?;
    Ok(WorkspaceCleanupResult { workspace, report })
}

pub(crate) async fn cleanup(
    state: &CoreState,
    p: WorkspacesCleanupParams,
) -> Result<WorkspaceCleanupResult, ProtocolError> {
    if p.confirmation.trim().is_empty() {
        return Err(ProtocolError::new(
            ErrorCode::InvalidParams,
            "confirmation is required: send report.confirmation from workspaces.cleanup_check",
        ));
    }
    let (workspace, report) = state.workspaces.cleanup(p.workspace_id, &p.confirmation).await?;
    Ok(WorkspaceCleanupResult { workspace, report })
}
