//! Request methods. Each method is a zero-sized type implementing [`Method`],
//! binding its wire name to its params and result types.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use switchyard_events::EventEnvelope;
use uuid::Uuid;

use crate::model::{ClientInfo, CoreInfo, GitStatusReport, HealthReport, Project};

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
