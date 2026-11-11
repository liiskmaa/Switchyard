//! Request dispatch. Each handler takes typed params and returns a typed
//! result; this module does the JSON conversion and method routing.

mod errors;
mod events;
mod projects;
mod system;

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
