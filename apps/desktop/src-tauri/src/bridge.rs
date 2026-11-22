//! The desktop's single connection to Core.
//!
//! [`supervise`] keeps the connection alive for the lifetime of the app:
//! connect → initialize → subscribe → forward events to the webview, and on
//! loss, back off and reconnect (starting Core if it is not running). Events
//! missed while disconnected are replayed from Core's journal using the last
//! sequence number seen, so the UI's activity feed has no gaps.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;
use switchyard_client::{ClientError, ClientOptions, CoreClient, Notification};
use switchyard_protocol::methods::{
    EventsSubscribe, EventsSubscribeParams, LiveSubscribe, LiveSubscribeParams,
};
use switchyard_protocol::model::CoreInfo;
use switchyard_protocol::paths::CorePaths;
use tauri::{AppHandle, Emitter};
use tokio::sync::RwLock;

use crate::launcher::Launcher;

pub const EVENT_CONNECTION: &str = "core://connection";
pub const EVENT_CORE_EVENT: &str = "core://event";
pub const EVENT_LIVE: &str = "core://live";

const BACKOFF_MIN: Duration = Duration::from_millis(250);
const BACKOFF_MAX: Duration = Duration::from_secs(5);
/// Minimum interval between auto-launch attempts.
const LAUNCH_COOLDOWN: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConnectionState {
    Connecting {
        attempt: u32,
    },
    Connected {
        core: CoreInfo,
        protocol_version: u32,
    },
    Disconnected {
        error: String,
        retry_in_ms: u64,
        launched_core: bool,
        /// The running Core speaks a protocol this app does not: restarting
        /// it starts the matching build (ADR 0015).
        incompatible: bool,
    },
    /// The operator stopped Core from the app: nothing restarts it until
    /// they start it again (a Core started elsewhere is still connected to).
    Stopped,
}

pub struct Bridge {
    pub paths: CorePaths,
    pub launcher: Launcher,
    client: RwLock<Option<CoreClient>>,
    state: Mutex<ConnectionState>,
    reconnect: tokio::sync::Notify,
    /// Set by "Stop Core": auto-launch is held until "Start".
    held: AtomicBool,
    /// When Core was last launched by the app (shared with the commands).
    last_launch: Mutex<Option<Instant>>,
}

impl Bridge {
    pub fn new(paths: CorePaths, launcher: Launcher) -> Self {
        Self {
            paths,
            launcher,
            client: RwLock::new(None),
            state: Mutex::new(ConnectionState::Connecting { attempt: 0 }),
            reconnect: tokio::sync::Notify::new(),
            held: AtomicBool::new(false),
            last_launch: Mutex::new(None),
        }
    }

    pub fn set_held(&self, held: bool) {
        self.held.store(held, Ordering::SeqCst);
    }

    pub fn held(&self) -> bool {
        self.held.load(Ordering::SeqCst)
    }

    /// Starts Core now (the operator's "Start"), whatever the auto-launch
    /// setting; records the launch so the supervisor does not start a second one.
    pub fn launch(&self) -> Result<u32, String> {
        if let Ok(mut l) = self.last_launch.lock() {
            *l = Some(Instant::now());
        }
        self.launcher.spawn()
    }

    fn may_auto_launch(&self) -> bool {
        if self.held() || !self.launcher.auto_launch {
            return false;
        }
        match self.last_launch.lock() {
            Ok(mut l) if l.is_none_or(|t| t.elapsed() >= LAUNCH_COOLDOWN) => {
                *l = Some(Instant::now());
                true
            }
            _ => false,
        }
    }

    pub fn state(&self) -> ConnectionState {
        self.state.lock().map(|s| s.clone()).unwrap_or(ConnectionState::Disconnected {
            error: "connection state unavailable".into(),
            retry_in_ms: 0,
            launched_core: false,
            incompatible: false,
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
        if bridge.held() {
            // Stopped by the operator: wait for "Start" (or poll for a Core started elsewhere).
            bridge.set_state(&app, ConnectionState::Stopped);
            tokio::select! {
                () = tokio::time::sleep(BACKOFF_MAX) => {}
                () = bridge.reconnect.notified() => {}
            }
            backoff = BACKOFF_MIN;
            continue;
        }
        if matches!(error, ClientError::Connect { .. }) && bridge.may_auto_launch() {
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
                incompatible: matches!(
                    error,
                    ClientError::IncompatibleCore { .. } | ClientError::IncompatibleProtocol(_)
                ),
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
    // Ephemeral agent output; anything streamed while disconnected is not
    // replayed (durable history comes from the journal).
    let live = match client.call::<LiveSubscribe>(LiveSubscribeParams::default()).await {
        Ok(sub) => sub.subscription_id,
        Err(err) => return err,
    };
    *bridge.client.write().await = Some(client.clone());
    tracing::info!(
        core = %info.core.version,
        instance = %info.core.instance_id,
        after_seq = ?*last_seq,
        "connected to Switchyard Core; events after `after_seq` are replayed"
    );
    bridge.set_state(
        app,
        ConnectionState::Connected { core: info.core.clone(), protocol_version: info.protocol_version },
    );

    let mut shutdown_reason = None;
    while let Some(notification) = notifications.recv().await {
        match notification {
            Notification::Event { subscription_id, event } if subscription_id == subscription => {
                tracing::debug!(seq = event.seq, event_type = %event.event_type, "forwarding event");
                *last_seq = Some(event.seq);
                if let Err(err) = app.emit(EVENT_CORE_EVENT, &event) {
                    tracing::warn!(%err, "cannot forward event to the webview");
                }
            }
            Notification::Event { .. } => {}
            Notification::Live { subscription_id, output } if subscription_id == live => {
                if let Err(err) = app.emit(EVENT_LIVE, &output) {
                    tracing::debug!(%err, "cannot forward live output");
                }
            }
            Notification::Live { .. } => {}
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
