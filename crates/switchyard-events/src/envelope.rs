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
}

impl DomainEvent {
    /// The stable wire/journal name of this event type.
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::CoreStarted { .. } => "core.started",
            Self::CoreStopping { .. } => "core.stopping",
            Self::ProjectRegistered { .. } => "project.registered",
            Self::ProjectRemoved { .. } => "project.removed",
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
