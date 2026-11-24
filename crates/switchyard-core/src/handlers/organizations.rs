//! Agent organization methods (M8.3, ADRs 0035–0036): the organization of a
//! task, the user's answer to an escalation, and the project's organization
//! settings. Starting, pausing and cancelling go through `workflows.*`.

use switchyard_protocol::ProtocolError;
use switchyard_protocol::methods::{
    OrganizationsAnswerParams, OrganizationsGetResult, OrganizationsSetSettingsParams, ProjectIdParams,
    TaskIdParams,
};
use switchyard_protocol::model::OrgSettingsInfo;

use crate::state::CoreState;

pub(super) async fn get(state: &CoreState, p: TaskIdParams) -> Result<OrganizationsGetResult, ProtocolError> {
    Ok(OrganizationsGetResult { organization: state.tasks.organization(p.task_id).await? })
}

pub(super) async fn decide_scope(
    state: &CoreState,
    p: switchyard_protocol::methods::OrganizationsDecideScopeParams,
) -> Result<OrganizationsGetResult, ProtocolError> {
    Ok(OrganizationsGetResult { organization: state.tasks.decide_scope(p.task_id, p.key, p.approve).await? })
}

pub(super) async fn answer(
    state: &CoreState,
    p: OrganizationsAnswerParams,
) -> Result<OrganizationsGetResult, ProtocolError> {
    Ok(OrganizationsGetResult { organization: state.tasks.answer_organization(p.task_id, p.text).await? })
}

pub(super) async fn settings(
    state: &CoreState,
    p: ProjectIdParams,
) -> Result<OrgSettingsInfo, ProtocolError> {
    state.tasks.org_settings(p.project_id).await
}

pub(super) async fn set_settings(
    state: &CoreState,
    p: OrganizationsSetSettingsParams,
) -> Result<OrgSettingsInfo, ProtocolError> {
    state.tasks.set_org_settings(p.project_id, p.limits, p.estimates, p.overrides).await
}
