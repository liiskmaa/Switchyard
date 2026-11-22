//! Commands exposed to the webview. The webview never talks to Core, SQLite
//! or agent processes directly; it can only issue allow-listed Core requests
//! through this bridge.

use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use switchyard_client::ClientError;
use switchyard_protocol::Method;
use switchyard_protocol::methods::{
    AgentsList, ApprovalsDecide, ApprovalsList, EventsList, PoliciesArchive, PoliciesCreate,
    PoliciesEffective, PoliciesEvaluations, PoliciesGet, PoliciesList, PoliciesSetAutoMode,
    PoliciesSetEnabled, PoliciesSettings, PoliciesUpdate, ProjectsAdd, ProjectsGet, ProjectsGitStatus,
    ProjectsList, ProjectsRemove, SessionsGet, SessionsInterrupt, SessionsList, SessionsResume, SessionsSend,
    SessionsStart, SessionsStop, SystemHealth, WorkspacesArchive, WorkspacesCleanup, WorkspacesCleanupCheck,
    WorkspacesComplete, WorkspacesCreate, WorkspacesGet, WorkspacesGitStatus, WorkspacesList,
    WorkspacesRestore, WorkspacesVerify,
};
use switchyard_protocol::methods::{
    AgentsModels, SupervisorAsk, SupervisorGet, WatchdogList, WorkflowsCancel, WorkflowsGet, WorkflowsPause,
    WorkflowsPreviewNext, WorkflowsRequestCorrection, WorkflowsResume, WorkflowsSetSettings,
    WorkflowsSetStepThrough, WorkflowsSettings, WorkflowsStart,
};
use switchyard_protocol::methods::{
    IntegrationsAbortResolution, IntegrationsApprove, IntegrationsCancel, IntegrationsCancelValidation,
    IntegrationsChanges, IntegrationsCommitWorkspace, IntegrationsCreate, IntegrationsDeleteCommand,
    IntegrationsGet, IntegrationsList, IntegrationsPatch, IntegrationsRefresh, IntegrationsSetCommand,
    IntegrationsSetSettings, IntegrationsSettings, IntegrationsUpdateFromTarget, IntegrationsValidate,
};
use switchyard_protocol::methods::{
    OrganizationsAnswer, OrganizationsGet, OrganizationsSetSettings, OrganizationsSettings,
};
use switchyard_protocol::methods::{ShutdownParams, SystemShutdown};
use switchyard_protocol::methods::{
    TasksAddDependency, TasksAssign, TasksCancel, TasksCancelAssignment, TasksComplete, TasksCreate,
    TasksDecideProposal, TasksFail, TasksGet, TasksList, TasksNote, TasksPublish, TasksRemoveDependency,
    TasksRetry, TasksStartAssignment, TasksUpdate,
};
use tauri::State;

use crate::bridge::{Bridge, ConnectionState};

/// Core methods the UI may call. Connection-level methods (`initialize`,
/// `events.subscribe`, `live.subscribe`, `system.shutdown`) are reserved for
/// the bridge.
const ALLOWED_METHODS: &[&str] = &[
    SystemHealth::NAME,
    ProjectsList::NAME,
    ProjectsGet::NAME,
    ProjectsAdd::NAME,
    ProjectsRemove::NAME,
    ProjectsGitStatus::NAME,
    EventsList::NAME,
    AgentsList::NAME,
    WorkspacesList::NAME,
    WorkspacesGet::NAME,
    WorkspacesCreate::NAME,
    WorkspacesGitStatus::NAME,
    WorkspacesVerify::NAME,
    WorkspacesArchive::NAME,
    WorkspacesRestore::NAME,
    WorkspacesComplete::NAME,
    WorkspacesCleanupCheck::NAME,
    WorkspacesCleanup::NAME,
    SessionsStart::NAME,
    SessionsList::NAME,
    SessionsGet::NAME,
    SessionsSend::NAME,
    SessionsInterrupt::NAME,
    SessionsStop::NAME,
    SessionsResume::NAME,
    ApprovalsList::NAME,
    ApprovalsDecide::NAME,
    PoliciesList::NAME,
    PoliciesGet::NAME,
    PoliciesCreate::NAME,
    PoliciesUpdate::NAME,
    PoliciesSetEnabled::NAME,
    PoliciesArchive::NAME,
    PoliciesEffective::NAME,
    PoliciesEvaluations::NAME,
    PoliciesSettings::NAME,
    PoliciesSetAutoMode::NAME,
    TasksList::NAME,
    TasksGet::NAME,
    TasksCreate::NAME,
    TasksUpdate::NAME,
    TasksPublish::NAME,
    TasksAddDependency::NAME,
    TasksRemoveDependency::NAME,
    TasksAssign::NAME,
    TasksStartAssignment::NAME,
    TasksCancelAssignment::NAME,
    TasksNote::NAME,
    TasksComplete::NAME,
    TasksFail::NAME,
    TasksCancel::NAME,
    TasksRetry::NAME,
    TasksDecideProposal::NAME,
    IntegrationsList::NAME,
    IntegrationsGet::NAME,
    IntegrationsCreate::NAME,
    IntegrationsRefresh::NAME,
    IntegrationsChanges::NAME,
    IntegrationsPatch::NAME,
    IntegrationsCommitWorkspace::NAME,
    IntegrationsUpdateFromTarget::NAME,
    IntegrationsAbortResolution::NAME,
    IntegrationsValidate::NAME,
    IntegrationsCancelValidation::NAME,
    IntegrationsApprove::NAME,
    IntegrationsCancel::NAME,
    IntegrationsSettings::NAME,
    IntegrationsSetSettings::NAME,
    IntegrationsSetCommand::NAME,
    IntegrationsDeleteCommand::NAME,
    AgentsModels::NAME,
    WorkflowsSettings::NAME,
    WorkflowsSetSettings::NAME,
    WorkflowsStart::NAME,
    WorkflowsGet::NAME,
    WorkflowsPause::NAME,
    WorkflowsResume::NAME,
    SupervisorAsk::NAME,
    WatchdogList::NAME,
    WorkflowsSetStepThrough::NAME,
    WorkflowsPreviewNext::NAME,
    SupervisorGet::NAME,
    WorkflowsCancel::NAME,
    WorkflowsRequestCorrection::NAME,
    OrganizationsGet::NAME,
    OrganizationsAnswer::NAME,
    OrganizationsSettings::NAME,
    OrganizationsSetSettings::NAME,
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
    /// When the Core executable on disk was last built (Unix ms): newer than
    /// the running Core's start means a restart picks up a new build.
    pub core_binary_modified_ms: Option<i64>,
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
        core_binary_modified_ms: bridge
            .launcher
            .binary
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| i64::try_from(d.as_millis()).ok()),
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

/// How long a graceful shutdown may take (Core waits up to 60 s for running
/// integration steps before stopping them).
const STOP_WAIT: std::time::Duration = std::time::Duration::from_secs(75);

/// Stops Core gracefully (`system.shutdown`) and holds auto-launch until
/// [`core_start`]. Agents' sessions stop with it; they can be resumed later.
#[tauri::command]
pub async fn core_stop(bridge: State<'_, Arc<Bridge>>) -> Result<(), CommandError> {
    bridge.set_held(true);
    if let Some(client) = bridge.client().await {
        let params = ShutdownParams { reason: Some("stopped from the desktop app".into()) };
        // The connection may close before the answer arrives; that is a stop too.
        let _ = client.call::<SystemShutdown>(params).await;
    }
    let deadline = std::time::Instant::now() + STOP_WAIT;
    while tokio::net::UnixStream::connect(&bridge.paths.socket).await.is_ok() {
        if std::time::Instant::now() >= deadline {
            return Err(CommandError::new("core_still_running", "Core did not stop within 75 seconds"));
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    bridge.reconnect_now();
    Ok(())
}

/// Starts Core (releasing a stop) and connects to it.
#[tauri::command]
pub async fn core_start(bridge: State<'_, Arc<Bridge>>) -> Result<(), CommandError> {
    bridge.set_held(false);
    if tokio::net::UnixStream::connect(&bridge.paths.socket).await.is_err() {
        bridge.launch().map_err(|e| CommandError::new("core_start_failed", e))?;
    }
    bridge.reconnect_now();
    Ok(())
}

/// Stops Core gracefully, then starts the executable on disk again: the way
/// to pick up a new build.
#[tauri::command]
pub async fn core_restart(bridge: State<'_, Arc<Bridge>>) -> Result<(), CommandError> {
    core_stop(bridge.clone()).await?;
    core_start(bridge).await
}
