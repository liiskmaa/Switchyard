//! Shared filesystem layout. Core and its clients must agree on where the
//! socket lives, so both resolve it through this module.

use std::path::{Path, PathBuf};

/// Overrides the data directory (absolute path).
pub const ENV_DATA_DIR: &str = "SWITCHYARD_DATA_DIR";
/// Overrides the socket path (absolute path).
pub const ENV_SOCKET: &str = "SWITCHYARD_SOCKET";

/// `sockaddr_un.sun_path` is 104 bytes on macOS (108 on Linux), including the
/// terminating NUL.
pub const MAX_SOCKET_PATH_BYTES: usize = 103;

#[derive(Debug, thiserror::Error)]
pub enum PathError {
    #[error("{var} must be an absolute path, got {value:?}")]
    NotAbsolute { var: &'static str, value: String },
    #[error("cannot determine the home directory: HOME is not set")]
    NoHome,
    #[error(
        "socket path {path:?} is {len} bytes; Unix sockets allow at most {MAX_SOCKET_PATH_BYTES}. \
         Set {ENV_SOCKET} to a shorter path"
    )]
    SocketPathTooLong { path: PathBuf, len: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorePaths {
    pub data_dir: PathBuf,
    pub socket: PathBuf,
    pub database: PathBuf,
    pub lock_file: PathBuf,
    pub log_dir: PathBuf,
    /// Core-owned Git worktrees: `<workspaces_dir>/<project-id>/<workspace-id>`.
    pub workspaces_dir: PathBuf,
}

impl CorePaths {
    /// Layout rooted at `data_dir`, with an optional explicit socket path.
    pub fn new(data_dir: PathBuf, socket: Option<PathBuf>) -> Result<Self, PathError> {
        let socket = socket.unwrap_or_else(|| data_dir.join("core.sock"));
        let len = socket.as_os_str().len();
        if len > MAX_SOCKET_PATH_BYTES {
            return Err(PathError::SocketPathTooLong { path: socket, len });
        }
        Ok(Self {
            database: data_dir.join("switchyard.db"),
            lock_file: data_dir.join("core.lock"),
            log_dir: data_dir.join("logs"),
            workspaces_dir: data_dir.join("workspaces"),
            socket,
            data_dir,
        })
    }

    /// Resolves the layout from the environment and platform defaults.
    pub fn from_env() -> Result<Self, PathError> {
        let data_dir = match env_path(ENV_DATA_DIR)? {
            Some(dir) => dir,
            None => default_data_dir()?,
        };
        Self::new(data_dir, env_path(ENV_SOCKET)?)
    }
}

fn env_path(var: &'static str) -> Result<Option<PathBuf>, PathError> {
    match std::env::var_os(var) {
        Some(value) if !value.is_empty() => {
            let path = PathBuf::from(&value);
            if path.is_absolute() {
                Ok(Some(path))
            } else {
                Err(PathError::NotAbsolute { var, value: value.to_string_lossy().into_owned() })
            }
        }
        _ => Ok(None),
    }
}

/// Platform default data directory.
///
/// * macOS: `~/Library/Application Support/Switchyard`
/// * other Unix: `$XDG_DATA_HOME/switchyard` or `~/.local/share/switchyard`
pub fn default_data_dir() -> Result<PathBuf, PathError> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty()).ok_or(PathError::NoHome)?;
    let home = Path::new(&home);
    if cfg!(target_os = "macos") {
        return Ok(home.join("Library/Application Support/Switchyard"));
    }
    match std::env::var_os("XDG_DATA_HOME").map(PathBuf::from) {
        Some(xdg) if xdg.is_absolute() => Ok(xdg.join("switchyard")),
        _ => Ok(home.join(".local/share/switchyard")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_rooted_in_data_dir() {
        let paths = CorePaths::new(PathBuf::from("/tmp/sy"), None).unwrap();
        assert_eq!(paths.socket, PathBuf::from("/tmp/sy/core.sock"));
        assert_eq!(paths.database, PathBuf::from("/tmp/sy/switchyard.db"));
    }

    #[test]
    fn overlong_socket_path_is_rejected() {
        let long = PathBuf::from(format!("/tmp/{}", "x".repeat(120)));
        assert!(matches!(CorePaths::new(long, None), Err(PathError::SocketPathTooLong { .. })));
    }
}
