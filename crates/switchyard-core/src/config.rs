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
    /// Claude Code executable; `None` resolves `SWITCHYARD_CLAUDE_BIN`, then
    /// `PATH` and common install locations.
    pub claude_executable: Option<PathBuf>,
    /// Codex CLI to use; `None` = `SWITCHYARD_CODEX_BIN`, `PATH`, common locations.
    pub codex_executable: Option<PathBuf>,
    /// Gemini CLI to use; `None` = `SWITCHYARD_GEMINI_BIN`, `PATH`, common locations.
    pub gemini_executable: Option<PathBuf>,
    /// Run a Gemini CLI version other than the verified one (ADR 0044).
    /// `SWITCHYARD_ALLOW_UNTESTED_GEMINI=1`.
    pub allow_untested_gemini: bool,
    /// How often the stuck-work watchdog looks (ADR 0045).
    pub watchdog_interval: Duration,
    /// When the watchdog counts work as stuck.
    pub watchdog_limits: switchyard_tasks::watchdog::Limits,
    /// Run a Codex version other than the one the adapter was verified against.
    /// Off by default: the app-server protocol is experimental and changes per
    /// release (ADR 0021). `SWITCHYARD_ALLOW_UNTESTED_CODEX=1`.
    pub allow_untested_codex: bool,
    /// How long a Codex session may take to open its thread before start fails.
    pub agent_ready_timeout: Duration,
    /// Pending permission requests are denied (`expired`) after this long.
    pub approval_timeout: Duration,
    /// Agent stop: wait after closing stdin before SIGTERM.
    pub agent_stop_grace: Duration,
    /// Agent stop: wait after SIGTERM before SIGKILL.
    pub agent_term_grace: Duration,
    /// Live output fan-out ring size.
    pub live_capacity: usize,
    /// Debounce window for filesystem change batches.
    pub file_debounce: Duration,
    /// How long shutdown waits for running integration operations (a Git
    /// step that was authorized) before stopping them; stopped ones are
    /// settled from Git at the next start (ADR 0032).
    pub operation_shutdown_grace: Duration,
    /// Test hook: delay between persisting a confirmed cleanup and its final
    /// filesystem check, to exercise changes made in that window. Zero in
    /// production; not configurable from the environment.
    #[doc(hidden)]
    pub cleanup_test_pause: Duration,
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
            claude_executable: None,
            codex_executable: None,
            gemini_executable: None,
            allow_untested_gemini: false,
            watchdog_interval: Duration::from_secs(30),
            watchdog_limits: switchyard_tasks::watchdog::Limits::default(),
            allow_untested_codex: false,
            agent_ready_timeout: Duration::from_secs(60),
            approval_timeout: Duration::from_secs(15 * 60),
            agent_stop_grace: Duration::from_secs(5),
            agent_term_grace: Duration::from_secs(5),
            live_capacity: 1024,
            file_debounce: Duration::from_millis(500),
            operation_shutdown_grace: Duration::from_secs(60),
            cleanup_test_pause: Duration::ZERO,
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
        if let Some(secs) =
            std::env::var("SWITCHYARD_APPROVAL_TIMEOUT_SECS").ok().and_then(|v| v.parse::<u64>().ok())
        {
            config.approval_timeout = Duration::from_secs(secs.max(1));
        }
        // Exactly "1"; anything else keeps the safe default.
        config.allow_untested_codex = std::env::var("SWITCHYARD_ALLOW_UNTESTED_CODEX").as_deref() == Ok("1");
        config.allow_untested_gemini =
            std::env::var("SWITCHYARD_ALLOW_UNTESTED_GEMINI").as_deref() == Ok("1");
        Ok(config)
    }
}
