//! Commands exposed to the webview. The webview never talks to Core, SQLite
//! or agent processes directly; it can only issue allow-listed Core requests
//! through this bridge.

use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use switchyard_client::ClientError;
use switchyard_protocol::Method;
use switchyard_protocol::methods::{
    EventsList, ProjectsAdd, ProjectsGet, ProjectsGitStatus, ProjectsList, ProjectsRemove, SystemHealth,
};
use tauri::State;

use crate::bridge::{Bridge, ConnectionState};

/// Core methods the UI may call. Connection-level methods (`initialize`,
/// `events.subscribe`, `system.shutdown`) are reserved for the bridge.
const ALLOWED_METHODS: &[&str] = &[
    SystemHealth::NAME,
    ProjectsList::NAME,
    ProjectsGet::NAME,
    ProjectsAdd::NAME,
    ProjectsRemove::NAME,
    ProjectsGitStatus::NAME,
    EventsList::NAME,
];

#[derive(Debug, Serialize)]
pub struct CommandError {
    /// A protocol error code, or `core_unavailable` / `method_not_allowed`.
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl CommandError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into(), data: None }
    }
}

impl From<ClientError> for CommandError {
    fn from(err: ClientError) -> Self {
        match err {
            ClientError::Remote(e) => Self {
                code: serde_json::to_value(e.code)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "internal".into()),
                message: e.message,
                data: e.data,
            },
            e if e.is_connection_error() => Self::new("core_unavailable", e.to_string()),
            e => Self::new("request_failed", e.to_string()),
        }
    }
}

#[tauri::command]
pub fn core_connection(bridge: State<'_, Arc<Bridge>>) -> ConnectionState {
    bridge.state()
}

#[derive(Debug, Serialize)]
pub struct CoreEnvironment {
    pub data_dir: String,
    pub socket_path: String,
    pub database_path: String,
    pub core_binary: Option<String>,
    pub core_log: String,
    pub auto_launch: bool,
    pub desktop_version: &'static str,
}

#[tauri::command]
pub fn core_environment(bridge: State<'_, Arc<Bridge>>) -> CoreEnvironment {
    CoreEnvironment {
        data_dir: bridge.paths.data_dir.display().to_string(),
        socket_path: bridge.paths.socket.display().to_string(),
        database_path: bridge.paths.database.display().to_string(),
        core_binary: bridge.launcher.binary.as_ref().map(|p| p.display().to_string()),
        core_log: bridge.launcher.log_file().display().to_string(),
        auto_launch: bridge.launcher.auto_launch,
        desktop_version: env!("CARGO_PKG_VERSION"),
    }
}

#[tauri::command]
pub async fn core_request(
    bridge: State<'_, Arc<Bridge>>,
    method: String,
    params: Option<Value>,
) -> Result<Value, CommandError> {
    if !ALLOWED_METHODS.contains(&method.as_str()) {
        return Err(CommandError::new("method_not_allowed", format!("{method} is not available to the UI")));
    }
    let client = bridge
        .client()
        .await
        .ok_or_else(|| CommandError::new("core_unavailable", "not connected to Switchyard Core"))?;
    Ok(client.call_raw(&method, params.unwrap_or(Value::Null)).await?)
}

#[tauri::command]
pub fn core_reconnect(bridge: State<'_, Arc<Bridge>>) {
    bridge.reconnect_now();
}
