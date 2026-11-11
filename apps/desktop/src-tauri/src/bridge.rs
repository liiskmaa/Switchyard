//! The desktop's single connection to Core.
//!
//! [`supervise`] keeps the connection alive for the lifetime of the app:
//! connect → initialize → subscribe → forward events to the webview, and on
//! loss, back off and reconnect (starting Core if it is not running). Events
//! missed while disconnected are replayed from Core's journal using the last
//! sequence number seen, so the UI's activity feed has no gaps.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use switchyard_client::{ClientError, ClientOptions, CoreClient, Notification};
use switchyard_protocol::methods::{EventsSubscribe, EventsSubscribeParams};
use switchyard_protocol::model::CoreInfo;
use switchyard_protocol::paths::CorePaths;
use tauri::{AppHandle, Emitter};
use tokio::sync::RwLock;

use crate::launcher::Launcher;

pub const EVENT_CONNECTION: &str = "core://connection";
pub const EVENT_CORE_EVENT: &str = "core://event";

const BACKOFF_MIN: Duration = Duration::from_millis(250);
const BACKOFF_MAX: Duration = Duration::from_secs(5);
/// Minimum interval between auto-launch attempts.
const LAUNCH_COOLDOWN: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConnectionState {
    Connecting { attempt: u32 },
    Connected { core: CoreInfo, protocol_version: u32 },
    Disconnected { error: String, retry_in_ms: u64, launched_core: bool },
}

pub struct Bridge {
    pub paths: CorePaths,
    pub launcher: Launcher,
    client: RwLock<Option<CoreClient>>,
    state: Mutex<ConnectionState>,
    reconnect: tokio::sync::Notify,
}

impl Bridge {
    pub fn new(paths: CorePaths, launcher: Launcher) -> Self {
        Self {
            paths,
            launcher,
            client: RwLock::new(None),
            state: Mutex::new(ConnectionState::Connecting { attempt: 0 }),
            reconnect: tokio::sync::Notify::new(),
        }
    }

    pub fn state(&self) -> ConnectionState {
        self.state.lock().map(|s| s.clone()).unwrap_or(ConnectionState::Disconnected {
            error: "connection state unavailable".into(),
            retry_in_ms: 0,
            launched_core: false,
        })
    }

    pub async fn client(&self) -> Option<CoreClient> {
        self.client.read().await.clone().filter(|c| !c.is_closed())
    }

    /// Skips the current back-off delay.
    pub fn reconnect_now(&self) {
        self.reconnect.notify_one();
    }

    fn set_state(&self, app: &AppHandle, state: ConnectionState) {
        if let Ok(mut current) = self.state.lock() {
            *current = state.clone();
        }
        if let Err(err) = app.emit(EVENT_CONNECTION, state) {
            tracing::warn!(%err, "cannot emit connection state");
        }
    }
}

pub async fn supervise(app: AppHandle, bridge: std::sync::Arc<Bridge>) {
    let mut last_seq: Option<i64> = None;
    let mut attempt: u32 = 0;
    let mut backoff = BACKOFF_MIN;
    let mut last_launch: Option<Instant> = None;

    loop {
        attempt += 1;
        bridge.set_state(&app, ConnectionState::Connecting { attempt });
        let options = ClientOptions::new("switchyard-desktop", env!("CARGO_PKG_VERSION"));

        let error = match CoreClient::connect(&bridge.paths.socket, options).await {
            Ok((client, notifications)) => {
                attempt = 0;
                backoff = BACKOFF_MIN;
                serve_connection(&app, &bridge, client, notifications, &mut last_seq).await
            }
            Err(err) => err,
        };

        let mut launched_core = false;
        if matches!(error, ClientError::Connect { .. })
            && bridge.launcher.auto_launch
            && last_launch.is_none_or(|t| t.elapsed() >= LAUNCH_COOLDOWN)
        {
            last_launch = Some(Instant::now());
            match bridge.launcher.spawn() {
                Ok(_) => {
                    launched_core = true;
                    backoff = BACKOFF_MIN;
                }
                Err(err) => tracing::error!(%err, "cannot start Switchyard Core"),
            }
        }

        tracing::info!(%error, retry_ms = backoff.as_millis(), "Core unavailable");
        bridge.set_state(
            &app,
            ConnectionState::Disconnected {
                error: error.to_string(),
                retry_in_ms: u64::try_from(backoff.as_millis()).unwrap_or(u64::MAX),
                launched_core,
            },
        );
        tokio::select! {
            () = tokio::time::sleep(backoff) => {}
            () = bridge.reconnect.notified() => {}
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// Runs one connection until it is lost. Returns the reason.
async fn serve_connection(
    app: &AppHandle,
    bridge: &Bridge,
    client: CoreClient,
    mut notifications: tokio::sync::mpsc::Receiver<Notification>,
    last_seq: &mut Option<i64>,
) -> ClientError {
    let info = client.info().clone();
    // Replay what was missed while disconnected; on first connect, only live
    // events (the UI loads history with `events.list`).
    let subscription = match client
        .call::<EventsSubscribe>(EventsSubscribeParams { after_seq: *last_seq, ..Default::default() })
        .await
    {
        Ok(sub) => sub.subscription_id,
        Err(err) => return err,
    };
    *bridge.client.write().await = Some(client.clone());
    tracing::info!(core = %info.core.version, instance = %info.core.instance_id, "connected to Switchyard Core");
    bridge.set_state(
        app,
        ConnectionState::Connected { core: info.core.clone(), protocol_version: info.protocol_version },
    );

    let mut shutdown_reason = None;
    while let Some(notification) = notifications.recv().await {
        match notification {
            Notification::Event { subscription_id, event } if subscription_id == subscription => {
                *last_seq = Some(event.seq);
                if let Err(err) = app.emit(EVENT_CORE_EVENT, &event) {
                    tracing::warn!(%err, "cannot forward event to the webview");
                }
            }
            Notification::Event { .. } => {}
            Notification::SubscriptionClosed { error, .. } => {
                tracing::warn!(?error, "Core closed the event subscription; reconnecting");
                client.close().await;
                break;
            }
            Notification::Shutdown { reason } => {
                tracing::info!(%reason, "Core is shutting down");
                shutdown_reason = Some(reason);
            }
        }
    }
    *bridge.client.write().await = None;
    match shutdown_reason {
        Some(reason) => ClientError::Remote(switchyard_protocol::ProtocolError::new(
            switchyard_protocol::ErrorCode::ShuttingDown,
            format!("Core shut down ({reason})"),
        )),
        None => ClientError::Disconnected,
    }
}
