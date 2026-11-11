//! Startup and shutdown sequencing.
//!
//! Startup: data dir → instance lock → database → event bus → `core.started`
//! → socket. Shutdown: stop accepting → `core.stopping` → notify and drain
//! clients → remove socket → release lock.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Instant;

use switchyard_events::{DomainEvent, EventBus, EventBusConfig, EventContext, new_id, now_ms};
use switchyard_git::Git;
use switchyard_protocol::model::CoreInfo;
use switchyard_storage::Storage;
use tokio::net::UnixListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::config::CoreConfig;
use crate::error::CoreError;
use crate::server;
use crate::state::CoreState;

/// Handle to a started Core.
pub struct RunningCore {
    info: CoreInfo,
    state: CoreState,
    server: JoinHandle<()>,
    // Held for the lifetime of Core; dropping it releases the instance lock.
    _lock: File,
}

impl RunningCore {
    pub fn info(&self) -> &CoreInfo {
        &self.info
    }

    /// Requests a graceful shutdown. Idempotent.
    pub fn shutdown(&self, reason: impl Into<String>) {
        self.state.request_shutdown(reason.into());
    }

    /// Resolves when shutdown has been requested (by [`Self::shutdown`] or by
    /// a client's `system.shutdown`).
    pub async fn shutdown_requested(&self) {
        let mut rx = self.state.shutdown.subscribe();
        let _ = rx.wait_for(Option::is_some).await;
    }

    /// Waits for the server to finish shutting down and releases resources.
    pub async fn wait(self) -> Result<(), CoreError> {
        let result = self.server.await.map_err(|e| CoreError::Task(e.to_string()));
        remove_socket(&self.state.config.paths.socket);
        tracing::info!("Switchyard Core stopped");
        result
    }
}

/// Starts Core. Returns once the socket is accepting connections.
pub async fn start(config: CoreConfig) -> Result<RunningCore, CoreError> {
    let paths = &config.paths;
    create_private_dir(&paths.data_dir)?;
    let lock = acquire_lock(&paths.lock_file)?;

    let storage = {
        let path = paths.database.clone();
        tokio::task::spawn_blocking(move || Storage::open(&path))
            .await
            .map_err(|e| CoreError::Task(e.to_string()))??
    };
    let bus = EventBus::new(
        Arc::new(storage.clone()),
        EventBusConfig { channel_capacity: config.event_channel_capacity },
    );

    let info = CoreInfo {
        version: crate::CORE_VERSION.to_owned(),
        instance_id: new_id(),
        started_at_ms: now_ms(),
        pid: std::process::id(),
    };
    bus.publish(
        DomainEvent::CoreStarted { core_version: info.version.clone(), instance_id: info.instance_id },
        EventContext::default(),
    )
    .await?;

    let listener = bind_socket(&paths.socket)?;
    let owner_uid = std::fs::metadata(&paths.socket)
        .map(|m| std::os::unix::fs::MetadataExt::uid(&m))
        .map_err(|source| CoreError::Listen { path: paths.socket.clone(), source })?;

    let (shutdown, _) = watch::channel(None);
    let state = CoreState {
        info: Arc::new(info.clone()),
        started: Instant::now(),
        git: Git::new(config.git.clone()),
        config: Arc::new(config),
        storage,
        bus,
        connected_clients: Arc::new(AtomicU64::new(0)),
        shutdown,
        closing: watch::channel(false).0,
    };
    tracing::info!(
        version = %info.version,
        instance_id = %info.instance_id,
        pid = info.pid,
        socket = %state.config.paths.socket.display(),
        "Switchyard Core listening"
    );
    let server = tokio::spawn(server::run(listener, owner_uid, state.clone()));
    Ok(RunningCore { info, state, server, _lock: lock })
}

fn create_private_dir(dir: &Path) -> Result<(), CoreError> {
    let err = |source| CoreError::DataDir { path: dir.to_path_buf(), source };
    if !dir.exists() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).map_err(err)?;
    }
    if !std::fs::metadata(dir).map_err(err)?.is_dir() {
        return Err(err(std::io::Error::new(std::io::ErrorKind::NotADirectory, "not a directory")));
    }
    Ok(())
}

/// Takes an exclusive advisory lock so only one Core serves a data dir. The
/// lock is released by the OS if Core dies, so a crash never leaves it stale.
fn acquire_lock(path: &Path) -> Result<File, CoreError> {
    let err = |source| CoreError::Lock { path: path.to_path_buf(), source };
    let mut file =
        OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path).map_err(err)?;
    match file.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Err(CoreError::AlreadyRunning(path.to_path_buf())),
        Err(std::fs::TryLockError::Error(source)) => return Err(err(source)),
    }
    file.set_len(0).map_err(err)?;
    writeln!(file, "{}", std::process::id()).map_err(err)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600)).map_err(err)?;
    Ok(file)
}

/// Binds the socket, replacing a stale socket file left by a crashed Core.
/// Safe because the instance lock is held: no live Core owns that file.
fn bind_socket(path: &Path) -> Result<UnixListener, CoreError> {
    let listen_err = |source| CoreError::Listen { path: path.to_path_buf(), source };
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => {
            tracing::warn!(path = %path.display(), "removing stale socket from a previous run");
            std::fs::remove_file(path).map_err(listen_err)?;
        }
        Ok(_) => return Err(CoreError::SocketPathOccupied(path.to_path_buf())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(listen_err(e)),
    }
    if let Some(parent) = path.parent() {
        create_private_dir(parent)?;
    }
    let listener = UnixListener::bind(path).map_err(listen_err)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(listen_err)?;
    Ok(listener)
}

fn remove_socket(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(path = %path.display(), error = %e, "cannot remove socket"),
    }
}

/// Publishes `core.stopping`. Failure is logged: shutdown must proceed.
pub(crate) async fn announce_stopping(state: &CoreState, reason: &str) {
    let event = DomainEvent::CoreStopping { instance_id: state.info.instance_id, reason: reason.to_owned() };
    if let Err(err) = state.bus.publish(event, EventContext::default()).await {
        tracing::error!(%err, "cannot record core.stopping event");
    }
}
