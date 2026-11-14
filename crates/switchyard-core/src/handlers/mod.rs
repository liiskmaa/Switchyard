//! Request dispatch. Each handler takes typed params and returns a typed
//! result; this module does the JSON conversion and method routing.

mod agents;
pub(crate) mod errors;
mod events;
mod policies;
mod projects;
mod system;
mod tasks;
mod workspaces;

use std::future::Future;
use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use switchyard_protocol::methods::{self, Method};
use switchyard_protocol::{ErrorCode, ProtocolError};
use tokio::sync::Notify;

use crate::connection::Connection;

/// A successful handler outcome.
pub(crate) struct Reply {
    pub value: Value,
    /// Notified once the response frame has been queued to the client. Used
    /// by `events.subscribe` so that no event frame precedes its response.
    pub after_send: Option<Arc<Notify>>,
}

impl Reply {
    pub fn encode<T: Serialize>(result: T) -> Result<Self, ProtocolError> {
        let value = serde_json::to_value(result)
            .map_err(|e| ProtocolError::new(ErrorCode::Internal, format!("cannot encode result: {e}")))?;
        Ok(Self { value, after_send: None })
    }
}

pub(crate) async fn dispatch(conn: &Connection, method: &str, params: Value) -> Result<Reply, ProtocolError> {
    if conn.state.is_shutting_down() {
        return Err(ProtocolError::new(ErrorCode::ShuttingDown, "Core is shutting down"));
    }
    let state = &conn.state;
    match method {
        methods::SystemHealth::NAME => {
            typed::<methods::SystemHealth, _, _>(params, |_| system::health(state)).await
        }
        methods::SystemShutdown::NAME => {
            typed::<methods::SystemShutdown, _, _>(params, |p| system::shutdown(state, p)).await
        }
        methods::ProjectsList::NAME => {
            typed::<methods::ProjectsList, _, _>(params, |_| projects::list(state)).await
        }
        methods::ProjectsGet::NAME => {
            typed::<methods::ProjectsGet, _, _>(params, |p| projects::get(state, p)).await
        }
        methods::ProjectsAdd::NAME => {
            typed::<methods::ProjectsAdd, _, _>(params, |p| projects::add(state, p)).await
        }
        methods::ProjectsRemove::NAME => {
            typed::<methods::ProjectsRemove, _, _>(params, |p| projects::remove(state, p)).await
        }
        methods::ProjectsGitStatus::NAME => {
            typed::<methods::ProjectsGitStatus, _, _>(params, |p| projects::git_status(state, p)).await
        }
        methods::EventsList::NAME => {
            typed::<methods::EventsList, _, _>(params, |p| events::list(state, p)).await
        }
        methods::EventsSubscribe::NAME => {
            events::subscribe(conn, parse::<methods::EventsSubscribe>(params)?).await
        }
        methods::EventsUnsubscribe::NAME => {
            typed::<methods::EventsUnsubscribe, _, _>(params, |p| events::unsubscribe(conn, p)).await
        }
        methods::AgentsList::NAME => {
            typed::<methods::AgentsList, _, _>(params, |p| agents::list(state, p)).await
        }
        methods::SessionsStart::NAME => {
            typed::<methods::SessionsStart, _, _>(params, |p| agents::start(state, p)).await
        }
        methods::SessionsList::NAME => {
            typed::<methods::SessionsList, _, _>(params, |p| agents::sessions(state, p)).await
        }
        methods::SessionsGet::NAME => {
            typed::<methods::SessionsGet, _, _>(params, |p| agents::get(state, p)).await
        }
        methods::SessionsSend::NAME => {
            typed::<methods::SessionsSend, _, _>(params, |p| agents::send(state, p)).await
        }
        methods::SessionsInterrupt::NAME => {
            typed::<methods::SessionsInterrupt, _, _>(params, |p| agents::interrupt(state, p)).await
        }
        methods::SessionsStop::NAME => {
            typed::<methods::SessionsStop, _, _>(params, |p| agents::stop(state, p)).await
        }
        methods::SessionsResume::NAME => {
            typed::<methods::SessionsResume, _, _>(params, |p| agents::resume(state, p)).await
        }
        methods::ApprovalsList::NAME => {
            typed::<methods::ApprovalsList, _, _>(params, |p| agents::approvals(state, p)).await
        }
        methods::ApprovalsDecide::NAME => {
            typed::<methods::ApprovalsDecide, _, _>(params, |p| agents::decide(conn, p)).await
        }
        methods::WorkspacesList::NAME => {
            typed::<methods::WorkspacesList, _, _>(params, |p| workspaces::list(state, p)).await
        }
        methods::WorkspacesGet::NAME => {
            typed::<methods::WorkspacesGet, _, _>(params, |p| workspaces::get(state, p)).await
        }
        methods::WorkspacesCreate::NAME => {
            typed::<methods::WorkspacesCreate, _, _>(params, |p| workspaces::create(state, p)).await
        }
        methods::WorkspacesGitStatus::NAME => {
            typed::<methods::WorkspacesGitStatus, _, _>(params, |p| workspaces::git_status(state, p)).await
        }
        methods::WorkspacesVerify::NAME => {
            typed::<methods::WorkspacesVerify, _, _>(params, |p| workspaces::verify(state, p)).await
        }
        methods::WorkspacesArchive::NAME => {
            typed::<methods::WorkspacesArchive, _, _>(params, |p| workspaces::archive(state, p)).await
        }
        methods::WorkspacesRestore::NAME => {
            typed::<methods::WorkspacesRestore, _, _>(params, |p| workspaces::restore(state, p)).await
        }
        methods::WorkspacesComplete::NAME => {
            typed::<methods::WorkspacesComplete, _, _>(params, |p| workspaces::complete(state, p)).await
        }
        methods::WorkspacesCleanupCheck::NAME => {
            typed::<methods::WorkspacesCleanupCheck, _, _>(params, |p| workspaces::cleanup_check(state, p))
                .await
        }
        methods::WorkspacesCleanup::NAME => {
            typed::<methods::WorkspacesCleanup, _, _>(params, |p| workspaces::cleanup(state, p)).await
        }
        methods::PoliciesList::NAME => {
            typed::<methods::PoliciesList, _, _>(params, |p| policies::list(state, p)).await
        }
        methods::PoliciesGet::NAME => {
            typed::<methods::PoliciesGet, _, _>(params, |p| policies::get(state, p)).await
        }
        methods::PoliciesCreate::NAME => {
            typed::<methods::PoliciesCreate, _, _>(params, |p| policies::create(conn, p)).await
        }
        methods::PoliciesUpdate::NAME => {
            typed::<methods::PoliciesUpdate, _, _>(params, |p| policies::update(conn, p)).await
        }
        methods::PoliciesSetEnabled::NAME => {
            typed::<methods::PoliciesSetEnabled, _, _>(params, |p| policies::set_enabled(conn, p)).await
        }
        methods::PoliciesArchive::NAME => {
            typed::<methods::PoliciesArchive, _, _>(params, |p| policies::archive(conn, p)).await
        }
        methods::PoliciesEffective::NAME => {
            typed::<methods::PoliciesEffective, _, _>(params, |p| policies::effective(state, p)).await
        }
        methods::PoliciesEvaluations::NAME => {
            typed::<methods::PoliciesEvaluations, _, _>(params, |p| policies::evaluations(state, p)).await
        }
        methods::TasksList::NAME => {
            typed::<methods::TasksList, _, _>(params, |p| tasks::list(state, p)).await
        }
        methods::TasksGet::NAME => typed::<methods::TasksGet, _, _>(params, |p| tasks::get(state, p)).await,
        methods::TasksCreate::NAME => {
            typed::<methods::TasksCreate, _, _>(params, |p| tasks::create(state, p)).await
        }
        methods::TasksUpdate::NAME => {
            typed::<methods::TasksUpdate, _, _>(params, |p| tasks::update(state, p)).await
        }
        methods::TasksPublish::NAME => {
            typed::<methods::TasksPublish, _, _>(params, |p| tasks::publish(state, p)).await
        }
        methods::TasksAddDependency::NAME => {
            typed::<methods::TasksAddDependency, _, _>(params, |p| tasks::add_dependency(state, p)).await
        }
        methods::TasksRemoveDependency::NAME => {
            typed::<methods::TasksRemoveDependency, _, _>(params, |p| tasks::remove_dependency(state, p))
                .await
        }
        methods::TasksAssign::NAME => {
            typed::<methods::TasksAssign, _, _>(params, |p| tasks::assign(conn, p)).await
        }
        methods::TasksStartAssignment::NAME => {
            typed::<methods::TasksStartAssignment, _, _>(params, |p| tasks::start_assignment(conn, p)).await
        }
        methods::TasksCancelAssignment::NAME => {
            typed::<methods::TasksCancelAssignment, _, _>(params, |p| tasks::cancel_assignment(conn, p)).await
        }
        methods::TasksNote::NAME => typed::<methods::TasksNote, _, _>(params, |p| tasks::note(conn, p)).await,
        methods::TasksComplete::NAME => {
            typed::<methods::TasksComplete, _, _>(params, |p| tasks::complete(conn, p)).await
        }
        methods::TasksFail::NAME => typed::<methods::TasksFail, _, _>(params, |p| tasks::fail(conn, p)).await,
        methods::TasksCancel::NAME => {
            typed::<methods::TasksCancel, _, _>(params, |p| tasks::cancel(conn, p)).await
        }
        methods::TasksRetry::NAME => {
            typed::<methods::TasksRetry, _, _>(params, |p| tasks::retry(conn, p)).await
        }
        methods::TasksDecideProposal::NAME => {
            typed::<methods::TasksDecideProposal, _, _>(params, |p| tasks::decide_proposal(conn, p)).await
        }
        methods::LiveSubscribe::NAME => {
            events::live_subscribe(conn, parse::<methods::LiveSubscribe>(params)?).await
        }
        methods::LiveUnsubscribe::NAME => {
            typed::<methods::LiveUnsubscribe, _, _>(params, |p| events::unsubscribe(conn, p)).await
        }
        other => Err(ProtocolError::new(ErrorCode::MethodNotFound, format!("unknown method {other:?}"))),
    }
}

fn parse<M: Method>(params: Value) -> Result<M::Params, ProtocolError> {
    // Absent params are equivalent to an empty object.
    let params = if params.is_null() { Value::Object(Default::default()) } else { params };
    serde_json::from_value(params)
        .map_err(|e| ProtocolError::new(ErrorCode::InvalidParams, format!("{}: {e}", M::NAME)))
}

async fn typed<M, F, Fut>(params: Value, handler: F) -> Result<Reply, ProtocolError>
where
    M: Method,
    F: FnOnce(M::Params) -> Fut,
    Fut: Future<Output = Result<M::Result, ProtocolError>>,
{
    Reply::encode(handler(parse::<M>(params)?).await?)
}
