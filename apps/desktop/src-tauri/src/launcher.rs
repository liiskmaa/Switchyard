//! Locating and starting the Core executable.

use std::fs::OpenOptions;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use switchyard_protocol::paths::CorePaths;

/// Overrides the Core executable used for auto-launch.
pub const ENV_CORE_BIN: &str = "SWITCHYARD_CORE_BIN";
/// Set to `0` to never start Core from the desktop app.
pub const ENV_AUTO_LAUNCH: &str = "SWITCHYARD_AUTO_LAUNCH_CORE";

#[derive(Debug, Clone)]
pub struct Launcher {
    pub binary: Option<PathBuf>,
    pub auto_launch: bool,
    log_file: PathBuf,
}

impl Launcher {
    /// Resolution order: `SWITCHYARD_CORE_BIN`, then `switchyard-core` next
    /// to the desktop executable. That covers `cargo` builds (shared target
    /// directory) and the bundled app (Tauri places `externalBin` sidecars
    /// in `Contents/MacOS`).
    pub fn detect(paths: &CorePaths) -> Self {
        let binary = std::env::var_os(ENV_CORE_BIN)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|exe| exe.parent().map(|dir| dir.join("switchyard-core")))
            })
            .filter(|p| p.is_file());
        let auto_launch = std::env::var(ENV_AUTO_LAUNCH).map(|v| v != "0").unwrap_or(true);
        Self { binary, auto_launch, log_file: paths.log_dir.join("core.log") }
    }

    /// Starts Core as an independent process.
    ///
    /// Core is placed in its own process group so terminal signals aimed at
    /// the desktop (e.g. Ctrl-C under `tauri dev`) do not reach it, and it is
    /// never waited on or killed by the desktop: it outlives the UI.
    pub fn spawn(&self) -> Result<u32, String> {
        let binary = self
            .binary
            .as_ref()
            .ok_or_else(|| format!("switchyard-core executable not found; build it or set {ENV_CORE_BIN}"))?;
        if let Some(dir) = self.log_file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_file)
            .map_err(|e| format!("cannot open {}: {e}", self.log_file.display()))?;
        let stderr = log.try_clone().map_err(|e| e.to_string())?;
        let mut child = Command::new(binary)
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(stderr)
            .process_group(0)
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", binary.display()))?;
        let pid = child.id();
        tracing::info!(pid, binary = %binary.display(), log = %self.log_file.display(), "started Switchyard Core");
        // Reap the child if it exits while the desktop is still running, so
        // it does not linger as a zombie. This never terminates Core.
        std::thread::spawn(move || match child.wait() {
            Ok(status) => tracing::info!(pid, %status, "Switchyard Core exited"),
            Err(err) => tracing::warn!(pid, %err, "cannot wait for Switchyard Core"),
        });
        Ok(pid)
    }

    pub fn log_file(&self) -> &PathBuf {
        &self.log_file
    }
}
