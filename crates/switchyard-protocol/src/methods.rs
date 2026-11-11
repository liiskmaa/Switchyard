//! Request methods. Each method is a zero-sized type implementing [`Method`],
//! binding its wire name to its params and result types.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use switchyard_events::EventEnvelope;
use uuid::Uuid;

use crate::model::{
    AgentAvailability, Approval, ApprovalStatus, ClientInfo, CoreInfo, GitStatusReport, HealthReport,
    Project, Session,
};

pub trait Method {
    const NAME: &'static str;
    type Params: Serialize + DeserializeOwned + Send + 'static;
    type Result: Serialize + DeserializeOwned + Send + 'static;
}

/// Params/result for methods that carry no data.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Empty {}

macro_rules! method {
    ($ty:ident, $name:literal, $params:ty, $result:ty) => {
        #[derive(Debug, Clone, Copy)]
        pub struct $ty;
        impl Method for $ty {
            const NAME: &'static str = $name;
            type Params = $params;
            type Result = $result;
        }
    };
}

// --- initialize -------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitializeParams {
    /// Highest protocol version the client speaks.
    pub protocol_version: u32,
    pub client: ClientInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitializeResult {
    /// Version selected for this connection.
    pub protocol_version: u32,
    pub core: CoreInfo,
}

method!(Initialize, "initialize", InitializeParams, InitializeResult);

// --- system -----------------------------------------------------------------

method!(SystemHealth, "system.health", Empty, HealthReport);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShutdownParams {
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShutdownResult {
    pub accepted: bool,
}

method!(SystemShutdown, "system.shutdown", ShutdownParams, ShutdownResult);

// --- projects ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectsListResult {
    pub projects: Vec<Project>,
}

method!(ProjectsList, "projects.list", Empty, ProjectsListResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectsAddParams {
    /// Absolute path to the root of an existing Git working tree.
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectResult {
    pub project: Project,
}

method!(ProjectsAdd, "projects.add", ProjectsAddParams, ProjectResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectIdParams {
    pub project_id: Uuid,
}

method!(ProjectsGet, "projects.get", ProjectIdParams, ProjectResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectsRemoveResult {
    pub project_id: Uuid,
}

method!(ProjectsRemove, "projects.remove", ProjectIdParams, ProjectsRemoveResult);
method!(ProjectsGitStatus, "projects.git_status", ProjectIdParams, GitStatusReport);

// --- events -----------------------------------------------------------------

/// Upper bound for `events.list` page size.
pub const MAX_EVENTS_PAGE: u32 = 1000;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventsListParams {
    #[serde(default)]
    pub after_seq: Option<i64>,
    #[serde(default)]
    pub project_id: Option<Uuid>,
    #[serde(default)]
    pub session_id: Option<Uuid>,
    #[serde(default)]
    pub event_types: Vec<String>,
    /// Defaults to 100, capped at [`MAX_EVENTS_PAGE`].
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventsListResult {
    pub events: Vec<EventEnvelope>,
}

method!(EventsList, "events.list", EventsListParams, EventsListResult);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventsSubscribeParams {
    /// Replay stored events with `seq > after_seq` before going live.
    /// Omit to receive only events published after subscribing.
    #[serde(default)]
    pub after_seq: Option<i64>,
    #[serde(default)]
    pub project_id: Option<Uuid>,
    #[serde(default)]
    pub event_types: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventsSubscribeResult {
    pub subscription_id: Uuid,
}

method!(EventsSubscribe, "events.subscribe", EventsSubscribeParams, EventsSubscribeResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventsUnsubscribeParams {
    pub subscription_id: Uuid,
}

method!(EventsUnsubscribe, "events.unsubscribe", EventsUnsubscribeParams, Empty);

// --- agents -----------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentsListParams {
    /// Re-run detection instead of returning the cached result.
    #[serde(default)]
    pub refresh: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsListResult {
    pub agents: Vec<AgentAvailability>,
}

method!(AgentsList, "agents.list", AgentsListParams, AgentsListResult);

// --- sessions ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionsStartParams {
    pub project_id: Uuid,
    /// Only `claude_code` is supported in this version.
    #[serde(default)]
    pub provider: Option<String>,
    /// First task; sent once the agent is ready.
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionResult {
    pub session: Session,
}

method!(SessionsStart, "sessions.start", SessionsStartParams, SessionResult);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionsListParams {
    #[serde(default)]
    pub project_id: Option<Uuid>,
    #[serde(default)]
    pub live_only: bool,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionsListResult {
    pub sessions: Vec<Session>,
}

method!(SessionsList, "sessions.list", SessionsListParams, SessionsListResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionIdParams {
    pub session_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionDetail {
    pub session: Session,
    /// Assistant text of the message currently being streamed (ephemeral;
    /// empty when nothing is streaming). Lets a reconnecting client catch up
    /// on in-progress output without the journal storing tokens.
    pub live_text: String,
    /// Live sequence number the snapshot corresponds to.
    pub live_seq: u64,
}

method!(SessionsGet, "sessions.get", SessionIdParams, SessionDetail);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionsSendParams {
    pub session_id: Uuid,
    pub text: String,
    /// When a turn is running: `true` queues the message as the next turn
    /// (if the agent supports it); `false` fails with `invalid_state`.
    #[serde(default)]
    pub queue_if_busy: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// The message started a new turn.
    StartedTurn,
    /// A turn was running; the agent will process the message next.
    QueuedForNextTurn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionsSendResult {
    pub delivery: Delivery,
}

method!(SessionsSend, "sessions.send", SessionsSendParams, SessionsSendResult);
method!(SessionsInterrupt, "sessions.interrupt", SessionIdParams, SessionResult);
method!(SessionsStop, "sessions.stop", SessionIdParams, SessionResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionsResumeParams {
    pub session_id: Uuid,
    #[serde(default)]
    pub prompt: Option<String>,
}

method!(SessionsResume, "sessions.resume", SessionsResumeParams, SessionResult);

// --- approvals --------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalsListParams {
    #[serde(default)]
    pub status: Option<ApprovalStatus>,
    #[serde(default)]
    pub session_id: Option<Uuid>,
    #[serde(default)]
    pub project_id: Option<Uuid>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalsListResult {
    pub approvals: Vec<Approval>,
}

method!(ApprovalsList, "approvals.list", ApprovalsListParams, ApprovalsListResult);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approve,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalsDecideParams {
    pub approval_id: Uuid,
    pub decision: ApprovalDecision,
    /// Shown to the agent on deny.
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalResult {
    pub approval: Approval,
}

method!(ApprovalsDecide, "approvals.decide", ApprovalsDecideParams, ApprovalResult);

// --- live output ------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveSubscribeParams {
    /// Only this session; omit for all sessions.
    #[serde(default)]
    pub session_id: Option<Uuid>,
}

method!(LiveSubscribe, "live.subscribe", LiveSubscribeParams, EventsSubscribeResult);
method!(LiveUnsubscribe, "live.unsubscribe", EventsUnsubscribeParams, Empty);
