//! Agent, session and approval methods.

use switchyard_agents::AgentProvider;
use switchyard_protocol::methods::{
    AgentsListParams, AgentsListResult, ApprovalResult, ApprovalsDecideParams, ApprovalsListParams,
    ApprovalsListResult, SessionDetail, SessionIdParams, SessionResult, SessionsListParams,
    SessionsListResult, SessionsResumeParams, SessionsSendParams, SessionsSendResult, SessionsStartParams,
};
use switchyard_protocol::{ErrorCode, ProtocolError};
use switchyard_storage::sessions::SessionFilter;

use crate::connection::Connection;
use crate::state::CoreState;

const DEFAULT_LIMIT: u32 = 100;
const MAX_LIMIT: u32 = 1000;

pub(super) fn limit(requested: Option<u32>) -> Result<u32, ProtocolError> {
    match requested.unwrap_or(DEFAULT_LIMIT) {
        n @ 1..=MAX_LIMIT => Ok(n),
        _ => Err(ProtocolError::new(
            ErrorCode::InvalidParams,
            format!("limit must be between 1 and {MAX_LIMIT}"),
        )),
    }
}

pub(crate) async fn list(state: &CoreState, p: AgentsListParams) -> Result<AgentsListResult, ProtocolError> {
    Ok(AgentsListResult { agents: state.agents.availability_all(p.refresh).await })
}

pub(crate) async fn start(state: &CoreState, p: SessionsStartParams) -> Result<SessionResult, ProtocolError> {
    // Omitted = Claude Code, as before Codex existed.
    let provider = match p.provider.as_deref() {
        None => AgentProvider::ClaudeCode,
        Some(v) => v.parse::<AgentProvider>().map_err(|_| {
            ProtocolError::new(
                ErrorCode::InvalidParams,
                format!("unknown provider {v:?}; expected \"claude_code\" or \"codex\""),
            )
        })?,
    };
    let session = state.agents.start(provider, p.workspace_id, p.project_id, p.prompt, p.model).await?;
    Ok(SessionResult { session })
}

pub(crate) async fn sessions(
    state: &CoreState,
    p: SessionsListParams,
) -> Result<SessionsListResult, ProtocolError> {
    let filter =
        SessionFilter { project_id: p.project_id, workspace_id: p.workspace_id, live_only: p.live_only };
    Ok(SessionsListResult { sessions: state.agents.list(filter, limit(p.limit)?).await? })
}

pub(crate) async fn get(state: &CoreState, p: SessionIdParams) -> Result<SessionDetail, ProtocolError> {
    let session = state.agents.session(p.session_id).await?;
    let (live_text, live_seq) = state.agents.live().snapshot(p.session_id);
    Ok(SessionDetail { session, live_text, live_seq })
}

pub(crate) async fn send(
    state: &CoreState,
    p: SessionsSendParams,
) -> Result<SessionsSendResult, ProtocolError> {
    let delivery = state.agents.send(p.session_id, p.text, p.queue_if_busy).await?;
    Ok(SessionsSendResult { delivery })
}

pub(crate) async fn interrupt(state: &CoreState, p: SessionIdParams) -> Result<SessionResult, ProtocolError> {
    Ok(SessionResult { session: state.agents.interrupt(p.session_id).await? })
}

pub(crate) async fn stop(state: &CoreState, p: SessionIdParams) -> Result<SessionResult, ProtocolError> {
    Ok(SessionResult { session: state.agents.stop(p.session_id).await? })
}

pub(crate) async fn resume(
    state: &CoreState,
    p: SessionsResumeParams,
) -> Result<SessionResult, ProtocolError> {
    Ok(SessionResult { session: state.agents.resume(p.session_id, p.prompt).await? })
}

pub(crate) async fn approvals(
    state: &CoreState,
    p: ApprovalsListParams,
) -> Result<ApprovalsListResult, ProtocolError> {
    let approvals =
        state.agents.list_approvals(p.status, p.session_id, p.project_id, limit(p.limit)?).await?;
    Ok(ApprovalsListResult { approvals })
}

pub(crate) async fn decide(
    conn: &Connection,
    p: ApprovalsDecideParams,
) -> Result<ApprovalResult, ProtocolError> {
    let by = conn.client_name.get().cloned().unwrap_or_else(|| "unknown client".into());
    let approval = conn.state.agents.decide(p.approval_id, p.decision, p.note, by).await?;
    Ok(ApprovalResult { approval })
}
