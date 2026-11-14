//! Storage records → protocol types.

use switchyard_agents::AgentProvider;
use switchyard_protocol::model::{AgentAvailability, Approval, ApprovalPolicy, Session};
use switchyard_storage::{ApprovalRecord, SessionRecord};

pub(super) fn session(
    r: SessionRecord,
    attached: bool,
    pending_approvals: u32,
    workspace_name: Option<String>,
) -> Session {
    Session {
        id: r.id,
        project_id: r.project_id,
        workspace_id: r.workspace_id,
        workspace_name,
        agent_id: r.agent_id,
        provider: r.provider.as_str().into(),
        native_session_id: r.native_session_id,
        cwd: r.cwd,
        state: r.state,
        pid: r.pid,
        cli_version: r.cli_version,
        model: r.model,
        started_at_ms: r.started_at_ms,
        ended_at_ms: r.ended_at_ms,
        last_activity_at_ms: r.last_activity_at_ms,
        exit_code: r.exit_code,
        exit_signal: r.exit_signal,
        failure: r.failure,
        resume_count: r.resume_count,
        attached,
        pending_approvals,
        capabilities: switchyard_agents::capabilities(r.provider),
        access: r.access,
    }
}

pub(super) fn approval(
    r: ApprovalRecord,
    workspace: Option<(uuid::Uuid, String)>,
    policy: Option<ApprovalPolicy>,
    provider: AgentProvider,
) -> Approval {
    let (workspace_id, workspace_name) = workspace.map_or((None, None), |(id, name)| (Some(id), Some(name)));
    Approval {
        id: r.id,
        project_id: r.project_id,
        session_id: r.session_id,
        workspace_id,
        workspace_name,
        agent_id: r.agent_id,
        provider: provider.as_str().into(),
        tool_name: r.tool_name,
        tool_use_id: r.tool_use_id,
        input: r.request.get("input").cloned().unwrap_or_default(),
        description: r.request.get("description").and_then(|d| d.as_str()).map(str::to_owned),
        details: r.request.get("details").cloned().unwrap_or_default(),
        status: r.status,
        requested_at_ms: r.requested_at_ms,
        expires_at_ms: r.expires_at_ms,
        decided_at_ms: r.decided_at_ms,
        decision_source: r.decision_source,
        decided_by: r.decided_by,
        decision_note: r.decision_note,
        policy,
    }
}

pub(super) fn unavailable(provider: AgentProvider, error: &str) -> AgentAvailability {
    AgentAvailability {
        provider: provider.as_str().into(),
        display_name: switchyard_agents::display_name(provider).into(),
        installed: false,
        executable: None,
        version: None,
        tested_version: switchyard_agents::tested_version(provider).into(),
        error: Some(error.to_owned()),
        auth: None,
        capabilities: switchyard_agents::capabilities(provider),
        checked_at_ms: 0,
    }
}
