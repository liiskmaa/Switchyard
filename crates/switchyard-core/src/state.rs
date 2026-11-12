use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use switchyard_events::EventBus;
use switchyard_git::Git;
use switchyard_protocol::model::CoreInfo;
use switchyard_storage::Storage;
use tokio::sync::watch;

use crate::agents::AgentService;
use crate::config::CoreConfig;
use crate::workspaces::WorkspaceService;

/// Shared state for all connections. Cheap to clone.
#[derive(Clone)]
pub(crate) struct CoreState {
    pub info: Arc<CoreInfo>,
    pub started: Instant,
    pub config: Arc<CoreConfig>,
    pub storage: Storage,
    pub bus: EventBus,
    pub git: Git,
    pub agents: AgentService,
    pub workspaces: WorkspaceService,
    pub connected_clients: Arc<AtomicU64>,
    /// `Some(reason)` once shutdown has been requested.
    pub shutdown: watch::Sender<Option<String>>,
    /// Set by the server after `core.stopping` has been journaled and
    /// broadcast; tells connections to drain and close.
    pub closing: watch::Sender<bool>,
}

impl CoreState {
    pub fn request_shutdown(&self, reason: String) {
        self.shutdown.send_if_modified(|current| {
            if current.is_none() {
                tracing::info!(%reason, "shutdown requested");
                *current = Some(reason);
                true
            } else {
                false
            }
        });
    }

    pub fn is_shutting_down(&self) -> bool {
        self.shutdown.borrow().is_some()
    }

    pub fn clients(&self) -> u64 {
        self.connected_clients.load(Ordering::Relaxed)
    }
}

/// Decrements the connected-client count when dropped.
pub(crate) struct ClientGuard(Arc<AtomicU64>);

impl ClientGuard {
    pub fn new(counter: &Arc<AtomicU64>) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self(Arc::clone(counter))
    }
}

impl Drop for ClientGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Resolves once shutdown has been requested (or the signal source is gone).
pub(crate) async fn wait_shutdown(rx: &mut watch::Receiver<Option<String>>) {
    // The returned guard is not `Send`; drop it before yielding.
    let _ = rx.wait_for(Option::is_some).await.map(|_| ());
}

/// Resolves once connections have been told to close.
pub(crate) async fn wait_closing(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|closing| *closing).await.map(|_| ());
}
