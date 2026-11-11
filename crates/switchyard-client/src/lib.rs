//! Async client for Switchyard Core.
//!
//! [`CoreClient::connect`] opens the Unix socket, performs the `initialize`
//! handshake and returns the client together with a receiver for
//! server-initiated frames (events, subscription closures, shutdown notices).
//!
//! A client represents one connection. When it drops (Core exited or
//! restarted), every in-flight call fails with [`ClientError::Disconnected`]
//! and the notification receiver ends; callers reconnect by calling
//! [`CoreClient::connect`] again. Reconnection policy is the caller's concern.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use switchyard_protocol::codec::{self, FrameError};
use switchyard_protocol::methods::{Initialize, InitializeParams, InitializeResult};
use switchyard_protocol::model::ClientInfo;
use switchyard_protocol::{
    ClientFrame, DEFAULT_MAX_FRAME_BYTES, EventEnvelope, Method, PROTOCOL_VERSION, ProtocolError, RequestId,
    ServerFrame,
};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{mpsc, oneshot, watch};
use uuid::Uuid;

/// Frames Core sends without a matching request.
#[derive(Debug, Clone, PartialEq)]
pub enum Notification {
    Event { subscription_id: Uuid, event: EventEnvelope },
    SubscriptionClosed { subscription_id: Uuid, error: ProtocolError },
    Shutdown { reason: String },
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("cannot connect to Switchyard Core at {path}: {source}")]
    Connect {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("Core returned an error: {0}")]
    Remote(#[from] ProtocolError),
    #[error("connection to Core lost")]
    Disconnected,
    #[error("request {method} timed out after {timeout_ms} ms")]
    Timeout { method: String, timeout_ms: u128 },
    #[error("framing error: {0}")]
    Frame(#[from] FrameError),
    #[error("cannot encode or decode {method}: {source}")]
    Codec {
        method: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("Core selected protocol version {0}, which this client does not support")]
    IncompatibleProtocol(u32),
}

impl ClientError {
    /// True when the error indicates Core is not reachable (as opposed to a
    /// request-level failure on a live connection).
    pub fn is_connection_error(&self) -> bool {
        matches!(self, Self::Connect { .. } | Self::Disconnected | Self::Frame(_))
    }
}

#[derive(Debug, Clone)]
pub struct ClientOptions {
    pub client: ClientInfo,
    pub request_timeout: Duration,
    pub max_frame_bytes: usize,
    /// Capacity of the notification channel. When the consumer falls behind,
    /// the client stops reading the socket, which pushes back on Core.
    pub notification_capacity: usize,
}

impl ClientOptions {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            client: ClientInfo { name: name.into(), version: version.into() },
            request_timeout: Duration::from_secs(30),
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
            notification_capacity: 1024,
        }
    }
}

type Pending = Arc<Mutex<HashMap<RequestId, oneshot::Sender<Result<Value, ProtocolError>>>>>;

/// A live, initialized connection to Core. Cheap to clone.
#[derive(Clone)]
pub struct CoreClient {
    writer: Arc<tokio::sync::Mutex<OwnedWriteHalf>>,
    pending: Pending,
    next_id: Arc<AtomicU64>,
    closed: watch::Receiver<bool>,
    request_timeout: Duration,
    info: Arc<InitializeResult>,
}

impl CoreClient {
    pub async fn connect(
        socket: &Path,
        options: ClientOptions,
    ) -> Result<(Self, mpsc::Receiver<Notification>), ClientError> {
        let stream = UnixStream::connect(socket)
            .await
            .map_err(|source| ClientError::Connect { path: socket.display().to_string(), source })?;
        let (read, write) = stream.into_split();
        let pending: Pending = Arc::default();
        let (closed_tx, closed_rx) = watch::channel(false);
        let (notify_tx, notify_rx) = mpsc::channel(options.notification_capacity.max(1));
        tokio::spawn(read_loop(read, options.max_frame_bytes, Arc::clone(&pending), notify_tx, closed_tx));

        let mut client = Self {
            writer: Arc::new(tokio::sync::Mutex::new(write)),
            pending,
            next_id: Arc::new(AtomicU64::new(1)),
            closed: closed_rx,
            request_timeout: options.request_timeout,
            info: Arc::new(InitializeResult {
                protocol_version: 0,
                core: switchyard_protocol::model::CoreInfo {
                    version: String::new(),
                    instance_id: Uuid::nil(),
                    started_at_ms: 0,
                    pid: 0,
                },
            }),
        };
        let init = client
            .call::<Initialize>(InitializeParams {
                protocol_version: PROTOCOL_VERSION,
                client: options.client,
            })
            .await?;
        if init.protocol_version > PROTOCOL_VERSION {
            client.close().await;
            return Err(ClientError::IncompatibleProtocol(init.protocol_version));
        }
        client.info = Arc::new(init);
        Ok((client, notify_rx))
    }

    /// Result of the `initialize` handshake for this connection.
    pub fn info(&self) -> &InitializeResult {
        &self.info
    }

    pub fn is_closed(&self) -> bool {
        *self.closed.borrow()
    }

    /// Resolves when the connection has been lost or closed.
    pub async fn closed(&self) {
        let mut rx = self.closed.clone();
        // An error means the reader task is gone, i.e. closed as well.
        let _ = rx.wait_for(|closed| *closed).await;
    }

    /// Sends a typed request and waits for its result.
    pub async fn call<M: Method>(&self, params: M::Params) -> Result<M::Result, ClientError> {
        let params = serde_json::to_value(params)
            .map_err(|source| ClientError::Codec { method: M::NAME.into(), source })?;
        let result = self.call_raw(M::NAME, params).await?;
        serde_json::from_value(result).map_err(|source| ClientError::Codec { method: M::NAME.into(), source })
    }

    /// Sends a request by method name with untyped params.
    pub async fn call_raw(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        if self.is_closed() {
            return Err(ClientError::Disconnected);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().map_err(|_| ClientError::Disconnected)?.insert(id, tx);

        let frame = ClientFrame::Request { id, method: method.to_owned(), params };
        let sent = {
            let mut writer = self.writer.lock().await;
            codec::write_frame(&mut *writer, &frame).await
        };
        if let Err(err) = sent {
            self.forget(id);
            return Err(err.into());
        }

        match tokio::time::timeout(self.request_timeout, rx).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(remote))) => Err(ClientError::Remote(remote)),
            Ok(Err(_)) => Err(ClientError::Disconnected),
            Err(_) => {
                self.forget(id);
                Err(ClientError::Timeout {
                    method: method.to_owned(),
                    timeout_ms: self.request_timeout.as_millis(),
                })
            }
        }
    }

    /// Half-closes the connection; Core then closes its side.
    pub async fn close(&self) {
        let mut writer = self.writer.lock().await;
        if let Err(err) = writer.shutdown().await {
            tracing::debug!(%err, "error while closing Core connection");
        }
    }

    fn forget(&self, id: RequestId) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&id);
        }
    }
}

async fn read_loop(
    read: OwnedReadHalf,
    max_frame_bytes: usize,
    pending: Pending,
    notify: mpsc::Sender<Notification>,
    closed: watch::Sender<bool>,
) {
    let mut reader = BufReader::new(read);
    loop {
        let frame = match codec::read_frame::<_, ServerFrame>(&mut reader, max_frame_bytes).await {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(err) => {
                tracing::warn!(%err, "Core connection read failed");
                break;
            }
        };
        let notification = match frame {
            ServerFrame::Response { id, result } => {
                resolve(&pending, id, Ok(result));
                continue;
            }
            ServerFrame::Error { id: Some(id), error } => {
                resolve(&pending, id, Err(error));
                continue;
            }
            ServerFrame::Error { id: None, error } => {
                tracing::warn!(?error, "Core reported a connection-level error");
                continue;
            }
            ServerFrame::Event { subscription_id, event } => Notification::Event { subscription_id, event },
            ServerFrame::SubscriptionClosed { subscription_id, error } => {
                Notification::SubscriptionClosed { subscription_id, error }
            }
            ServerFrame::Shutdown { reason } => Notification::Shutdown { reason },
        };
        // If the consumer dropped its receiver it no longer wants
        // notifications; responses must still be routed, so keep reading.
        let _ = notify.send(notification).await;
    }
    if let Ok(mut pending) = pending.lock() {
        // Dropping the senders wakes every waiter with Disconnected.
        pending.clear();
    }
    let _ = closed.send(true);
}

fn resolve(pending: &Pending, id: RequestId, result: Result<Value, ProtocolError>) {
    let waiter = pending.lock().ok().and_then(|mut p| p.remove(&id));
    match waiter {
        Some(tx) => {
            // The caller may have timed out and gone away.
            let _ = tx.send(result);
        }
        None => tracing::debug!(id, "response for unknown or expired request"),
    }
}
