//! One client connection: frame decoding, the initialize gate, concurrent
//! request execution, subscriptions and the outbound writer.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use switchyard_protocol::codec::{self, FrameError};
use switchyard_protocol::methods::{Initialize, InitializeParams, InitializeResult, Method};
use switchyard_protocol::{
    ClientFrame, ErrorCode, MIN_SUPPORTED_PROTOCOL_VERSION, PROTOCOL_VERSION, ProtocolError, RequestId,
    ServerFrame,
};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::{Semaphore, mpsc};
use tokio::task::{AbortHandle, JoinSet};
use uuid::Uuid;

use crate::handlers::{self, Reply};
use crate::state::{ClientGuard, CoreState, wait_closing};

/// Per-connection context handed to request handlers.
#[derive(Clone)]
pub(crate) struct Connection {
    pub id: Uuid,
    pub state: CoreState,
    pub outbound: mpsc::Sender<ServerFrame>,
    pub subscriptions: Arc<Mutex<HashMap<Uuid, AbortHandle>>>,
}

impl Connection {
    /// Queues a frame for this client. Waits when the client's queue is full
    /// (backpressure). Returns `false` if the connection is gone.
    pub async fn send(&self, frame: ServerFrame) -> bool {
        self.outbound.send(frame).await.is_ok()
    }

    /// Waits (bounded) for subscription tasks to flush and exit on their own.
    async fn wait_subscriptions_drained(&self, limit: Duration) {
        let deadline = tokio::time::Instant::now() + limit;
        while self.subscriptions.lock().is_ok_and(|s| !s.is_empty()) {
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!(connection = %self.id, "subscriptions did not drain before shutdown");
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn abort_subscriptions(&self) {
        if let Ok(mut subs) = self.subscriptions.lock() {
            for (_, handle) in subs.drain() {
                handle.abort();
            }
        }
    }
}

pub(crate) async fn serve(stream: UnixStream, state: CoreState) {
    let _client = ClientGuard::new(&state.connected_clients);
    let config = Arc::clone(&state.config);
    let (read, write) = stream.into_split();
    let (outbound, outbound_rx) = mpsc::channel(config.client_queue_capacity.max(1));
    let writer = tokio::spawn(write_loop(write, outbound_rx));

    let conn = Connection {
        id: switchyard_events::new_id(),
        state: state.clone(),
        outbound,
        subscriptions: Arc::default(),
    };
    tracing::info!(connection = %conn.id, clients = state.clients(), "client connected");

    let mut reader = BufReader::new(read);
    let mut closing = state.closing.subscribe();
    let in_flight = Arc::new(Semaphore::new(config.max_requests_per_client.max(1)));
    let mut requests = JoinSet::new();
    let mut initialized = false;

    loop {
        let frame = tokio::select! {
            () = wait_closing(&mut closing) => {
                conn.wait_subscriptions_drained(Duration::from_secs(1)).await;
                let reason = state.shutdown.borrow().clone().unwrap_or_default();
                conn.send(ServerFrame::Shutdown { reason }).await;
                break;
            }
            Some(_) = requests.join_next(), if !requests.is_empty() => continue,
            frame = codec::read_line(&mut reader, config.max_frame_bytes) => frame,
        };
        let line = match frame {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(FrameError::TooLarge { max }) => {
                // The stream cannot be resynchronised mid-frame; close it.
                let error =
                    ProtocolError::new(ErrorCode::FrameTooLarge, format!("frame exceeds {max} bytes"));
                conn.send(ServerFrame::Error { id: None, error }).await;
                break;
            }
            Err(err) => {
                tracing::debug!(connection = %conn.id, %err, "read failed");
                break;
            }
        };

        let (id, method, params) = match decode_request(&line) {
            Ok(request) => request,
            Err((id, error)) => {
                conn.send(ServerFrame::Error { id, error }).await;
                continue;
            }
        };

        if method == Initialize::NAME {
            let outcome = initialize(&state, initialized, params).and_then(Reply::encode);
            initialized |= outcome.is_ok();
            conn.send(response(id, outcome)).await;
            continue;
        }
        if !initialized {
            let error = ProtocolError::new(ErrorCode::NotInitialized, "send `initialize` first");
            conn.send(ServerFrame::Error { id: Some(id), error }).await;
            continue;
        }
        let Ok(permit) = Arc::clone(&in_flight).try_acquire_owned() else {
            let error =
                ProtocolError::new(ErrorCode::Overloaded, "too many concurrent requests on this connection");
            conn.send(ServerFrame::Error { id: Some(id), error }).await;
            continue;
        };

        let conn = conn.clone();
        requests.spawn(async move {
            let _permit = permit;
            let outcome = handlers::dispatch(&conn, &method, params).await;
            let after_send = outcome.as_ref().ok().and_then(|r| r.after_send.clone());
            if conn.send(response(id, outcome)).await
                && let Some(start) = after_send
            {
                start.notify_one();
            }
        });
    }

    conn.abort_subscriptions();
    requests.shutdown().await;
    drop(conn);
    // The writer exits once every sender (connection + its tasks) is gone and
    // the queue is flushed. Bound the wait in case the peer stopped reading.
    if tokio::time::timeout(Duration::from_secs(2), writer).await.is_err() {
        tracing::warn!("client writer did not finish; dropping connection");
    }
    tracing::info!(clients = state.clients().saturating_sub(1), "client disconnected");
}

fn response(id: RequestId, outcome: Result<Reply, ProtocolError>) -> ServerFrame {
    match outcome {
        Ok(reply) => ServerFrame::Response { id, result: reply.value },
        Err(error) => ServerFrame::Error { id: Some(id), error },
    }
}

/// Decodes a request line. On failure, returns the request id when it could
/// still be recovered so the client can correlate the error.
fn decode_request(line: &[u8]) -> Result<(RequestId, String, Value), (Option<RequestId>, ProtocolError)> {
    let value: Value = serde_json::from_slice(line)
        .map_err(|e| (None, ProtocolError::new(ErrorCode::ParseError, format!("invalid JSON: {e}"))))?;
    let id = value.get("id").and_then(Value::as_u64);
    match serde_json::from_value::<ClientFrame>(value) {
        Ok(ClientFrame::Request { id, method, params }) => Ok((id, method, params)),
        Err(e) => Err((id, ProtocolError::new(ErrorCode::ParseError, format!("invalid frame: {e}")))),
    }
}

fn initialize(state: &CoreState, already: bool, params: Value) -> Result<InitializeResult, ProtocolError> {
    if already {
        return Err(ProtocolError::new(ErrorCode::AlreadyInitialized, "connection is already initialized"));
    }
    let params: InitializeParams = serde_json::from_value(params)
        .map_err(|e| ProtocolError::new(ErrorCode::InvalidParams, e.to_string()))?;
    if params.protocol_version < MIN_SUPPORTED_PROTOCOL_VERSION {
        return Err(ProtocolError::new(
            ErrorCode::UnsupportedProtocolVersion,
            format!(
                "client speaks protocol {}, Core supports {MIN_SUPPORTED_PROTOCOL_VERSION}..={PROTOCOL_VERSION}",
                params.protocol_version
            ),
        )
        .with_data(serde_json::json!({
            "min_supported": MIN_SUPPORTED_PROTOCOL_VERSION,
            "max_supported": PROTOCOL_VERSION,
        })));
    }
    tracing::info!(client = %params.client.name, version = %params.client.version, "client initialized");
    Ok(InitializeResult {
        protocol_version: params.protocol_version.min(PROTOCOL_VERSION),
        core: (*state.info).clone(),
    })
}

async fn write_loop(mut write: OwnedWriteHalf, mut frames: mpsc::Receiver<ServerFrame>) {
    while let Some(frame) = frames.recv().await {
        if let Err(err) = codec::write_frame(&mut write, &frame).await {
            tracing::debug!(%err, "write to client failed");
            return;
        }
    }
    if let Err(err) = write.shutdown().await {
        tracing::debug!(%err, "socket shutdown failed");
    }
}
