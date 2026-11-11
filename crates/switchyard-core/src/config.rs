use std::path::PathBuf;
use std::time::Duration;

use switchyard_git::GitConfig;
use switchyard_protocol::DEFAULT_MAX_FRAME_BYTES;
use switchyard_protocol::paths::{CorePaths, PathError};

/// Complete runtime configuration for Core. Built once at startup.
#[derive(Debug, Clone)]
pub struct CoreConfig {
    pub paths: CorePaths,
    pub git: GitConfig,
    /// Live event fan-out ring size (see `EventBusConfig`).
    pub event_channel_capacity: usize,
    /// Outbound frames buffered per client before senders wait.
    pub client_queue_capacity: usize,
    /// Concurrent in-flight requests per client before `overloaded`.
    pub max_requests_per_client: usize,
    pub max_subscriptions_per_client: usize,
    pub max_frame_bytes: usize,
    /// How long shutdown waits for connections to drain.
    pub shutdown_grace: Duration,
}

impl CoreConfig {
    pub fn with_paths(paths: CorePaths) -> Self {
        Self {
            paths,
            git: GitConfig::default(),
            event_channel_capacity: 1024,
            client_queue_capacity: 256,
            max_requests_per_client: 32,
            max_subscriptions_per_client: 16,
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
            shutdown_grace: Duration::from_secs(3),
        }
    }

    /// Configuration rooted at an explicit data directory.
    pub fn for_data_dir(data_dir: PathBuf, socket: Option<PathBuf>) -> Result<Self, PathError> {
        Ok(Self::with_paths(CorePaths::new(data_dir, socket)?))
    }

    /// Configuration from `SWITCHYARD_*` environment variables and defaults.
    pub fn from_env() -> Result<Self, PathError> {
        let mut config = Self::with_paths(CorePaths::from_env()?);
        if let Some(git) = std::env::var_os("SWITCHYARD_GIT").filter(|v| !v.is_empty()) {
            config.git.binary = PathBuf::from(git);
        }
        Ok(config)
    }
}
