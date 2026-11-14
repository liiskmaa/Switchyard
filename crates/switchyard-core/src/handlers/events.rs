use std::sync::Arc;
use std::time::Duration;

use switchyard_events::{EventFilter, RecvError};
use switchyard_protocol::methods::{
    Empty, EventsListParams, EventsListResult, EventsSubscribeParams, EventsSubscribeResult,
    EventsUnsubscribeParams, MAX_EVENTS_PAGE,
};
use switchyard_protocol::{ErrorCode, ProtocolError, ServerFrame};
use tokio::sync::Notify;

use super::{Reply, errors};
use crate::connection::Connection;
use crate::state::{CoreState, wait_closing};

const DEFAULT_PAGE: u32 = 100;
/// How long a stopping subscription waits for further buffered events.
const DRAIN_POLL: Duration = Duration::from_millis(50);

fn validate_types(types: &[String]) -> Result<(), ProtocolError> {
    if types.iter().any(|t| t.trim().is_empty()) {
        return Err(ProtocolError::new(ErrorCode::InvalidParams, "event_types must not contain empty names"));
    }
    Ok(())
}

pub(crate) async fn list(
    state: &CoreState,
    params: EventsListParams,
) -> Result<EventsListResult, ProtocolError> {
    validate_types(&params.event_types)?;
    let limit = params.limit.unwrap_or(DEFAULT_PAGE);
    if limit == 0 || limit > MAX_EVENTS_PAGE {
        return Err(ProtocolError::new(
            ErrorCode::InvalidParams,
            format!("limit must be between 1 and {MAX_EVENTS_PAGE}"),
        ));
    }
    // Without `after_seq` the caller wants the most recent page.
    if params.after_seq.is_none()
        && params.project_id.is_none()
        && params.session_id.is_none()
        && params.task_id.is_none()
        && params.event_types.is_empty()
    {
        let events = state
            .storage
            .call(move |c| switchyard_storage::events::read_latest(c, limit))
            .await
            .map_err(errors::storage)?;
        return Ok(EventsListResult { events });
    }
    let filter = EventFilter {
        after_seq: params.after_seq,
        project_id: params.project_id,
        session_id: params.session_id,
        task_id: params.task_id,
        event_types: params.event_types,
        limit,
    };
    let events =
        state.bus.read(filter).await.map_err(|e| errors::internal("event journal read failed", e))?;
    Ok(EventsListResult { events })
}

pub(crate) async fn subscribe(
    conn: &Connection,
    params: EventsSubscribeParams,
) -> Result<Reply, ProtocolError> {
    validate_types(&params.event_types)?;
    let max = conn.state.config.max_subscriptions_per_client;
    let subscription_id = switchyard_events::new_id();
    let start = Arc::new(Notify::new());

    let mut subscription = conn.state.bus.subscribe(
        params.after_seq,
        EventFilter { project_id: params.project_id, event_types: params.event_types, ..Default::default() },
    );
    let task_conn = conn.clone();
    let task_start = Arc::clone(&start);
    let mut closing = conn.state.closing.subscribe();
    let task = async move {
        // Do not emit events before the client has the subscription id.
        task_start.notified().await;
        loop {
            let next = tokio::select! {
                biased;
                next = subscription.next() => next,
                () = wait_closing(&mut closing) => {
                    // Core is stopping: forward what is already buffered
                    // (e.g. `core.stopping`), then end.
                    while let Ok(Ok(event)) = tokio::time::timeout(DRAIN_POLL, subscription.next()).await {
                        let frame = ServerFrame::Event { subscription_id, event: (*event).clone() };
                        if !task_conn.send(frame).await {
                            break;
                        }
                    }
                    break;
                }
            };
            match next {
                Ok(event) => {
                    let frame = ServerFrame::Event { subscription_id, event: (*event).clone() };
                    if !task_conn.send(frame).await {
                        break;
                    }
                }
                Err(RecvError::Closed) => break,
                Err(err) => {
                    tracing::error!(%subscription_id, %err, "subscription failed");
                    let error =
                        ProtocolError::new(ErrorCode::Internal, format!("subscription failed: {err}"))
                            .with_data(serde_json::json!({ "last_seq": subscription.last_seq() }));
                    task_conn.send(ServerFrame::SubscriptionClosed { subscription_id, error }).await;
                    break;
                }
            }
        }
        if let Ok(mut subs) = task_conn.subscriptions.lock() {
            subs.remove(&subscription_id);
        }
    };

    {
        let mut subs = conn
            .subscriptions
            .lock()
            .map_err(|_| errors::internal("subscription registry", "mutex poisoned"))?;
        if subs.len() >= max {
            return Err(ProtocolError::new(
                ErrorCode::Overloaded,
                format!("at most {max} subscriptions per connection"),
            ));
        }
        let handle = tokio::spawn(task);
        subs.insert(subscription_id, handle.abort_handle());
    }

    let mut reply = Reply::encode(EventsSubscribeResult { subscription_id })?;
    reply.after_send = Some(start);
    Ok(reply)
}

pub(crate) async fn unsubscribe(
    conn: &Connection,
    params: EventsUnsubscribeParams,
) -> Result<Empty, ProtocolError> {
    let handle = conn
        .subscriptions
        .lock()
        .map_err(|_| errors::internal("subscription registry", "mutex poisoned"))?
        .remove(&params.subscription_id);
    match handle {
        Some(handle) => {
            handle.abort();
            Ok(Empty {})
        }
        None => Err(ProtocolError::new(
            ErrorCode::NotFound,
            format!("subscription {} not found", params.subscription_id),
        )),
    }
}

/// Ephemeral live output. Lossy by design: if this client falls behind, the
/// broadcast ring overwrites chunks and the client receives a `gap` marker;
/// the agent is never slowed down by a client.
pub(crate) async fn live_subscribe(
    conn: &Connection,
    params: switchyard_protocol::methods::LiveSubscribeParams,
) -> Result<Reply, ProtocolError> {
    use switchyard_protocol::model::{LiveKind, LiveOutput};
    use tokio::sync::broadcast::error::RecvError as LiveRecvError;

    let max = conn.state.config.max_subscriptions_per_client;
    let subscription_id = switchyard_events::new_id();
    let start = Arc::new(Notify::new());
    let mut rx = conn.state.agents.live().subscribe();
    let filter = params.session_id;
    let task_conn = conn.clone();
    let task_start = Arc::clone(&start);
    let mut closing = conn.state.closing.subscribe();
    let task = async move {
        task_start.notified().await;
        loop {
            let output = tokio::select! {
                output = rx.recv() => output,
                () = wait_closing(&mut closing) => break,
            };
            let frame_output = match output {
                Ok(o) if filter.is_none_or(|s| s == o.session_id) => (*o).clone(),
                Ok(_) => continue,
                Err(LiveRecvError::Lagged(missed)) => LiveOutput {
                    session_id: filter.unwrap_or_default(),
                    seq: 0,
                    kind: LiveKind::Gap,
                    text: String::new(),
                    missed: Some(missed),
                },
                Err(LiveRecvError::Closed) => break,
            };
            if !task_conn.send(ServerFrame::Live { subscription_id, output: frame_output }).await {
                break;
            }
        }
        if let Ok(mut subs) = task_conn.subscriptions.lock() {
            subs.remove(&subscription_id);
        }
    };
    {
        let mut subs = conn
            .subscriptions
            .lock()
            .map_err(|_| errors::internal("subscription registry", "mutex poisoned"))?;
        if subs.len() >= max {
            return Err(ProtocolError::new(
                ErrorCode::Overloaded,
                format!("at most {max} subscriptions per connection"),
            ));
        }
        let handle = tokio::spawn(task);
        subs.insert(subscription_id, handle.abort_handle());
    }
    let mut reply = Reply::encode(EventsSubscribeResult { subscription_id })?;
    reply.after_send = Some(start);
    Ok(reply)
}
