//! Integration methods (ADRs 0028-0031). Every decision here is the
//! operator's: approving names the exact source and target commits, and no
//! agent path reaches these handlers.

use switchyard_protocol::ProtocolError;
use switchyard_protocol::methods::{
    IntegrationDetail, IntegrationIdParams, IntegrationResult, IntegrationsApproveParams,
    IntegrationsCancelParams, IntegrationsChangesParams, IntegrationsCommitWorkspaceParams,
    IntegrationsCreateParams, IntegrationsDeleteCommandParams, IntegrationsListParams,
    IntegrationsListResult, IntegrationsPatchParams, IntegrationsPatchResult, IntegrationsSetCommandParams,
    IntegrationsSetSettingsParams, IntegrationsSettingsParams, IntegrationsValidateParams,
    IntegrationsValidateResult,
};
use switchyard_protocol::model::{IntegrationChanges, IntegrationSettingsInfo};

use super::agents::limit;
use crate::connection::Connection;
use crate::state::CoreState;

fn actor(conn: &Connection) -> String {
    conn.client_name.get().cloned().unwrap_or_else(|| "unknown client".into())
}

pub(super) async fn list(
    state: &CoreState,
    p: IntegrationsListParams,
) -> Result<IntegrationsListResult, ProtocolError> {
    let integrations =
        state.integrations.list(p.project_id, p.task_id, p.include_closed, limit(p.limit)?).await?;
    Ok(IntegrationsListResult { integrations })
}

pub(super) async fn get(
    state: &CoreState,
    p: IntegrationIdParams,
) -> Result<IntegrationDetail, ProtocolError> {
    state.integrations.detail(p.integration_id).await
}

pub(super) async fn create(
    conn: &Connection,
    p: IntegrationsCreateParams,
) -> Result<IntegrationResult, ProtocolError> {
    Ok(IntegrationResult { integration: conn.state.integrations.create(p, actor(conn)).await? })
}

pub(super) async fn refresh(
    state: &CoreState,
    p: IntegrationIdParams,
) -> Result<IntegrationResult, ProtocolError> {
    Ok(IntegrationResult { integration: state.integrations.refresh(p.integration_id).await? })
}

pub(super) async fn changes(
    state: &CoreState,
    p: IntegrationsChangesParams,
) -> Result<IntegrationChanges, ProtocolError> {
    state.integrations.changes(p.integration_id).await
}

pub(super) async fn patch(
    state: &CoreState,
    p: IntegrationsPatchParams,
) -> Result<IntegrationsPatchResult, ProtocolError> {
    let (patch, truncated) = state.integrations.patch(p.integration_id, p.path, p.uncommitted).await?;
    Ok(IntegrationsPatchResult { patch, truncated })
}

pub(super) async fn commit_workspace(
    conn: &Connection,
    p: IntegrationsCommitWorkspaceParams,
) -> Result<IntegrationResult, ProtocolError> {
    let integration =
        conn.state.integrations.commit_workspace(p.integration_id, p.message, actor(conn)).await?;
    Ok(IntegrationResult { integration })
}

pub(super) async fn update_from_target(
    conn: &Connection,
    p: IntegrationIdParams,
) -> Result<IntegrationResult, ProtocolError> {
    Ok(IntegrationResult {
        integration: conn.state.integrations.update_from_target(p.integration_id, actor(conn)).await?,
    })
}

pub(super) async fn abort_resolution(
    conn: &Connection,
    p: IntegrationIdParams,
) -> Result<IntegrationResult, ProtocolError> {
    Ok(IntegrationResult {
        integration: conn.state.integrations.abort_resolution(p.integration_id, actor(conn)).await?,
    })
}

pub(super) async fn validate(
    conn: &Connection,
    p: IntegrationsValidateParams,
) -> Result<IntegrationsValidateResult, ProtocolError> {
    let (integration, run) =
        conn.state.integrations.validate(p.integration_id, p.authorize, actor(conn)).await?;
    Ok(IntegrationsValidateResult { integration, run })
}

pub(super) async fn cancel_validation(
    state: &CoreState,
    p: IntegrationIdParams,
) -> Result<IntegrationResult, ProtocolError> {
    Ok(IntegrationResult { integration: state.integrations.cancel_validation(p.integration_id).await? })
}

pub(super) async fn approve(
    conn: &Connection,
    p: IntegrationsApproveParams,
) -> Result<IntegrationResult, ProtocolError> {
    let integration = conn
        .state
        .integrations
        .approve(p.integration_id, p.source_commit, p.target_commit, actor(conn))
        .await?;
    Ok(IntegrationResult { integration })
}

pub(super) async fn cancel(
    conn: &Connection,
    p: IntegrationsCancelParams,
) -> Result<IntegrationResult, ProtocolError> {
    Ok(IntegrationResult {
        integration: conn.state.integrations.cancel(p.integration_id, p.reason, actor(conn)).await?,
    })
}

pub(super) async fn settings(
    state: &CoreState,
    p: IntegrationsSettingsParams,
) -> Result<IntegrationSettingsInfo, ProtocolError> {
    state.integrations.settings(p.project_id).await
}

pub(super) async fn set_settings(
    conn: &Connection,
    p: IntegrationsSetSettingsParams,
) -> Result<IntegrationSettingsInfo, ProtocolError> {
    conn.state.integrations.set_settings(p.project_id, p.require_review, actor(conn)).await
}

pub(super) async fn set_command(
    conn: &Connection,
    p: IntegrationsSetCommandParams,
) -> Result<IntegrationSettingsInfo, ProtocolError> {
    conn.state.integrations.set_command(p, actor(conn)).await
}

pub(super) async fn delete_command(
    conn: &Connection,
    p: IntegrationsDeleteCommandParams,
) -> Result<IntegrationSettingsInfo, ProtocolError> {
    conn.state.integrations.delete_command(p.command_id, actor(conn)).await
}
