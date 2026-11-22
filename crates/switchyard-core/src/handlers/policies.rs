//! Policy rule methods (ADR 0016). Every rule change is followed by a
//! re-evaluation of the project's pending approvals, which can only deny
//! them (ADR 0018).

use switchyard_protocol::ProtocolError;
use switchyard_protocol::methods::{
    PoliciesArchiveParams, PoliciesCreateParams, PoliciesEffectiveParams, PoliciesEvaluationsParams,
    PoliciesEvaluationsResult, PoliciesGetResult, PoliciesListParams, PoliciesListResult,
    PoliciesSetAutoModeParams, PoliciesSetEnabledParams, PoliciesSettingsParams, PoliciesUpdateParams,
    PolicyRuleIdParams, PolicyRuleResult, PolicySettingsResult,
};
use switchyard_protocol::model::{EffectivePolicy, PolicyRule, PolicySettings};

use super::agents::limit;
use crate::connection::Connection;
use crate::state::CoreState;

fn changed_by(conn: &Connection) -> String {
    conn.client_name.get().cloned().unwrap_or_else(|| "unknown client".into())
}

async fn after_change(state: &CoreState, (rule, policy_revision): (PolicyRule, i64)) -> PolicyRuleResult {
    state.agents.reevaluate_pending(rule.project_id).await;
    PolicyRuleResult { rule, policy_revision }
}

pub(super) async fn list(
    state: &CoreState,
    p: PoliciesListParams,
) -> Result<PoliciesListResult, ProtocolError> {
    let (rules, policy_revision) = state.policies.list(p.project_id, p.include_archived).await?;
    Ok(PoliciesListResult { rules, policy_revision })
}

pub(super) async fn get(
    state: &CoreState,
    p: PolicyRuleIdParams,
) -> Result<PoliciesGetResult, ProtocolError> {
    let (rule, history) = state.policies.get(p.rule_id).await?;
    Ok(PoliciesGetResult { rule, history })
}

pub(super) async fn create(
    conn: &Connection,
    p: PoliciesCreateParams,
) -> Result<PolicyRuleResult, ProtocolError> {
    let changed = conn.state.policies.create(p, changed_by(conn)).await?;
    Ok(after_change(&conn.state, changed).await)
}

pub(super) async fn update(
    conn: &Connection,
    p: PoliciesUpdateParams,
) -> Result<PolicyRuleResult, ProtocolError> {
    let changed = conn.state.policies.update(p, changed_by(conn)).await?;
    Ok(after_change(&conn.state, changed).await)
}

pub(super) async fn set_enabled(
    conn: &Connection,
    p: PoliciesSetEnabledParams,
) -> Result<PolicyRuleResult, ProtocolError> {
    let changed =
        conn.state.policies.set_enabled(p.rule_id, p.expected_revision, p.enabled, changed_by(conn)).await?;
    Ok(after_change(&conn.state, changed).await)
}

pub(super) async fn archive(
    conn: &Connection,
    p: PoliciesArchiveParams,
) -> Result<PolicyRuleResult, ProtocolError> {
    let changed = conn.state.policies.archive(p.rule_id, p.expected_revision, changed_by(conn)).await?;
    Ok(after_change(&conn.state, changed).await)
}

pub(super) async fn effective(
    state: &CoreState,
    p: PoliciesEffectiveParams,
) -> Result<EffectivePolicy, ProtocolError> {
    state.policies.effective(p.project_id, p.workspace_id).await
}

pub(super) async fn evaluations(
    state: &CoreState,
    p: PoliciesEvaluationsParams,
) -> Result<PoliciesEvaluationsResult, ProtocolError> {
    let n = limit(p.limit)?;
    Ok(PoliciesEvaluationsResult { evaluations: state.policies.evaluations(p, n).await? })
}

pub(super) async fn settings(
    state: &CoreState,
    p: PoliciesSettingsParams,
) -> Result<PolicySettings, ProtocolError> {
    state.policies.settings(p.project_id).await
}

/// Auto mode never denies anything, so pending approvals are left to the
/// person they were asked of (ADR 0037).
pub(super) async fn set_auto_mode(
    conn: &Connection,
    p: PoliciesSetAutoModeParams,
) -> Result<PolicySettingsResult, ProtocolError> {
    let (settings, policy_revision) = conn
        .state
        .policies
        .set_auto_mode(p.project_id, p.enabled, p.expected_revision, changed_by(conn))
        .await?;
    Ok(PolicySettingsResult { settings, policy_revision })
}
