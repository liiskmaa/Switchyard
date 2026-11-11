use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::ids::{EventId, new_id, now_ms};

/// Version of the envelope + payload schema written by this build.
///
/// Bump when an existing event type's payload changes incompatibly. Readers
/// must keep accepting older versions found in the journal.
pub const EVENT_SCHEMA_VERSION: u32 = 1;

/// Typed catalogue of events emitted by Switchyard Core.
///
/// Serialized adjacently tagged (`{"type": "...", "payload": {...}}`) so the
/// type string and payload can be stored in separate journal columns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum DomainEvent {
    #[serde(rename = "core.started")]
    CoreStarted { core_version: String, instance_id: Uuid },
    #[serde(rename = "core.stopping")]
    CoreStopping { instance_id: Uuid, reason: String },
    #[serde(rename = "project.registered")]
    ProjectRegistered { project_id: Uuid, name: String, root_path: String },
    #[serde(rename = "project.removed")]
    ProjectRemoved { project_id: Uuid, root_path: String },

    // --- agent sessions (context carries project/workspace/session/agent) --
    /// A native agent process was launched for a session.
    #[serde(rename = "agent.started")]
    AgentStarted {
        provider: String,
        cli_version: String,
        pid: u32,
        cwd: String,
        native_session_id: Option<String>,
        resumed: bool,
    },
    /// The agent completed its handshake.
    #[serde(rename = "agent.ready")]
    AgentReady { native_session_id: String, model: Option<String>, tools: Vec<String> },
    #[serde(rename = "agent.state_changed")]
    AgentStateChanged { from: String, to: String },
    /// Switchyard delivered a user message. `queued` means a turn was still
    /// running, so the agent will process it as its next turn.
    #[serde(rename = "agent.turn_started")]
    AgentTurnStarted { prompt: String, prompt_truncated: bool, queued: bool },
    /// A complete assistant message (never individual tokens).
    #[serde(rename = "agent.message")]
    AgentMessage { message_id: Option<String>, text: String, truncated: bool },
    #[serde(rename = "agent.tool_started")]
    AgentToolStarted {
        tool_use_id: String,
        tool_name: String,
        input: serde_json::Value,
        input_truncated: bool,
    },
    #[serde(rename = "agent.tool_completed")]
    AgentToolCompleted {
        tool_use_id: String,
        tool_name: Option<String>,
        output: String,
        output_truncated: bool,
    },
    /// The tool failed, or did not run (`rejected`: permission denied or the
    /// turn was interrupted first).
    #[serde(rename = "agent.tool_failed")]
    AgentToolFailed {
        tool_use_id: String,
        tool_name: Option<String>,
        output: String,
        output_truncated: bool,
        rejected: bool,
    },
    #[serde(rename = "agent.turn_completed")]
    AgentTurnCompleted {
        success: bool,
        interrupted: bool,
        reason: Option<String>,
        stop_reason: Option<String>,
        duration_ms: Option<u64>,
        cost_usd: Option<f64>,
        result: Option<String>,
    },
    #[serde(rename = "agent.permission_requested")]
    AgentPermissionRequested {
        approval_id: Uuid,
        tool_name: String,
        tool_use_id: Option<String>,
        input: serde_json::Value,
        description: Option<String>,
        expires_at_ms: i64,
    },
    #[serde(rename = "agent.permission_resolved")]
    AgentPermissionResolved {
        approval_id: Uuid,
        tool_name: String,
        status: String,
        source: String,
        decided_by: Option<String>,
        note: Option<String>,
    },
    /// The agent acknowledged an interrupt request.
    #[serde(rename = "agent.interrupted")]
    AgentInterrupted {},
    #[serde(rename = "agent.session_resumed")]
    AgentSessionResumed { native_session_id: String, resume_count: u32 },
    #[serde(rename = "agent.warning")]
    AgentWarning { message: String },
    /// The agent process ended.
    #[serde(rename = "agent.exited")]
    AgentExited {
        exit_code: Option<i32>,
        exit_signal: Option<i32>,
        stop_requested: bool,
        final_state: String,
    },
    /// The session failed (start failure, crash, lost supervision).
    #[serde(rename = "agent.failed")]
    AgentFailed { reason: String, detail: Option<String> },

    // --- filesystem --------------------------------------------------------
    /// Actual changes observed on disk in a project (debounced batch). This
    /// records what the filesystem reported, independent of agent intent.
    #[serde(rename = "files.changed")]
    FilesChanged { changes: Vec<FileChange>, dropped: u32 },
}

/// One observed filesystem change, relative to the project root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    /// `created`, `modified`, `deleted` or `renamed`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

impl DomainEvent {
    /// The stable wire/journal name of this event type.
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::CoreStarted { .. } => "core.started",
            Self::CoreStopping { .. } => "core.stopping",
            Self::ProjectRegistered { .. } => "project.registered",
            Self::ProjectRemoved { .. } => "project.removed",
            Self::AgentStarted { .. } => "agent.started",
            Self::AgentReady { .. } => "agent.ready",
            Self::AgentStateChanged { .. } => "agent.state_changed",
            Self::AgentTurnStarted { .. } => "agent.turn_started",
            Self::AgentMessage { .. } => "agent.message",
            Self::AgentToolStarted { .. } => "agent.tool_started",
            Self::AgentToolCompleted { .. } => "agent.tool_completed",
            Self::AgentToolFailed { .. } => "agent.tool_failed",
            Self::AgentTurnCompleted { .. } => "agent.turn_completed",
            Self::AgentPermissionRequested { .. } => "agent.permission_requested",
            Self::AgentPermissionResolved { .. } => "agent.permission_resolved",
            Self::AgentInterrupted {} => "agent.interrupted",
            Self::AgentSessionResumed { .. } => "agent.session_resumed",
            Self::AgentWarning { .. } => "agent.warning",
            Self::AgentExited { .. } => "agent.exited",
            Self::AgentFailed { .. } => "agent.failed",
            Self::FilesChanged { .. } => "files.changed",
        }
    }

    fn into_payload(self) -> Result<Value, serde_json::Error> {
        let mut tagged = serde_json::to_value(self)?;
        Ok(tagged.as_object_mut().and_then(|o| o.remove("payload")).unwrap_or(Value::Null))
    }
}

/// Correlation fields attached to an event. All are optional because many
/// events (e.g. core lifecycle) are not scoped to a project or session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventContext {
    pub project_id: Option<Uuid>,
    pub workspace_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub agent_id: Option<Uuid>,
    /// The event that directly caused this one.
    pub causation_id: Option<EventId>,
}

/// An event that has been constructed but not yet stored. It has no sequence
/// number; the journal assigns one on append.
#[derive(Debug, Clone, PartialEq)]
pub struct EventDraft {
    pub id: EventId,
    pub schema_version: u32,
    pub event_type: String,
    pub occurred_at_ms: i64,
    pub context: EventContext,
    pub payload: Value,
}

impl EventDraft {
    pub fn new(event: DomainEvent, context: EventContext) -> Result<Self, serde_json::Error> {
        let event_type = event.event_type().to_owned();
        Ok(Self {
            id: new_id(),
            schema_version: EVENT_SCHEMA_VERSION,
            event_type,
            occurred_at_ms: now_ms(),
            context,
            payload: event.into_payload()?,
        })
    }

    /// Attaches the journal-assigned sequence number.
    pub fn into_envelope(self, seq: i64) -> EventEnvelope {
        EventEnvelope {
            seq,
            id: self.id,
            schema_version: self.schema_version,
            event_type: self.event_type,
            occurred_at_ms: self.occurred_at_ms,
            context: self.context,
            payload: self.payload,
        }
    }
}

/// A durably stored event, as read from the journal and delivered to
/// subscribers.
///
/// `event_type` and `payload` are kept untyped so that events written by a
/// newer schema version can still be read, listed and forwarded. Use
/// [`EventEnvelope::decode`] to obtain a typed [`DomainEvent`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// Monotonic journal position. Strictly increasing, may contain gaps.
    pub seq: i64,
    pub id: EventId,
    pub schema_version: u32,
    pub event_type: String,
    pub occurred_at_ms: i64,
    #[serde(flatten)]
    pub context: EventContext,
    pub payload: Value,
}

#[derive(Debug, thiserror::Error)]
#[error("cannot decode event {event_type} (schema v{schema_version}): {source}")]
pub struct EventDecodeError {
    pub event_type: String,
    pub schema_version: u32,
    #[source]
    pub source: serde_json::Error,
}

impl EventEnvelope {
    pub fn decode(&self) -> Result<DomainEvent, EventDecodeError> {
        let tagged = serde_json::json!({ "type": self.event_type, "payload": self.payload });
        serde_json::from_value(tagged).map_err(|source| EventDecodeError {
            event_type: self.event_type.clone(),
            schema_version: self.schema_version,
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_splits_type_and_payload_and_decodes_back() -> Result<(), Box<dyn std::error::Error>> {
        let project_id = new_id();
        let event =
            DomainEvent::ProjectRegistered { project_id, name: "demo".into(), root_path: "/tmp/demo".into() };
        let draft = EventDraft::new(
            event.clone(),
            EventContext { project_id: Some(project_id), ..Default::default() },
        )?;
        assert_eq!(draft.event_type, "project.registered");
        assert_eq!(draft.payload["name"], "demo");
        assert_eq!(draft.schema_version, EVENT_SCHEMA_VERSION);

        let envelope = draft.into_envelope(7);
        assert_eq!(envelope.seq, 7);
        assert_eq!(envelope.decode()?, event);
        Ok(())
    }

    #[test]
    fn every_variant_tag_matches_event_type() -> Result<(), Box<dyn std::error::Error>> {
        let samples = vec![
            DomainEvent::AgentInterrupted {},
            DomainEvent::AgentWarning { message: "m".into() },
            DomainEvent::AgentStateChanged { from: "idle".into(), to: "running".into() },
            DomainEvent::FilesChanged {
                changes: vec![FileChange { path: "a".into(), kind: "created".into(), from: None }],
                dropped: 0,
            },
        ];
        for event in samples {
            let tagged = serde_json::to_value(&event)?;
            assert_eq!(tagged["type"], event.event_type());
            let envelope = EventDraft::new(event.clone(), EventContext::default())?.into_envelope(1);
            assert_eq!(envelope.decode()?, event);
        }
        Ok(())
    }

    #[test]
    fn unknown_event_types_are_preserved_but_not_decodable() -> Result<(), Box<dyn std::error::Error>> {
        let json = serde_json::json!({
            "seq": 1, "id": new_id(), "schema_version": 9, "event_type": "future.thing",
            "occurred_at_ms": 1, "project_id": null, "workspace_id": null, "session_id": null,
            "task_id": null, "agent_id": null, "causation_id": null, "payload": {"x": 1}
        });
        let envelope: EventEnvelope = serde_json::from_value(json)?;
        assert_eq!(envelope.event_type, "future.thing");
        assert!(envelope.decode().is_err());
        Ok(())
    }
}
