use uuid::Uuid;

use crate::envelope::{EventDraft, EventEnvelope};

/// Selection criteria for reading the journal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventFilter {
    /// Only events with `seq > after_seq`.
    pub after_seq: Option<i64>,
    pub project_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    /// Exact event type names. Empty means all types.
    pub event_types: Vec<String>,
    /// Maximum number of events to return (ascending by `seq`).
    pub limit: u32,
}

impl EventFilter {
    /// True when `event` satisfies every criterion except `limit`.
    pub fn matches(&self, event: &EventEnvelope) -> bool {
        self.after_seq.is_none_or(|after| event.seq > after)
            && self.project_id.is_none_or(|p| event.context.project_id == Some(p))
            && self.session_id.is_none_or(|s| event.context.session_id == Some(s))
            && (self.event_types.is_empty() || self.event_types.contains(&event.event_type))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("event {0} already exists in the journal")]
    Duplicate(Uuid),
    #[error("journal storage failure: {0}")]
    Storage(String),
}

/// Durable, append-only event storage.
///
/// Implementations are synchronous; the bus calls them on a blocking thread.
pub trait EventJournal: Send + Sync + 'static {
    /// Durably stores `draft` and returns it with its assigned sequence.
    fn append(&self, draft: EventDraft) -> Result<EventEnvelope, JournalError>;

    /// Reads events in ascending `seq` order.
    fn read(&self, filter: &EventFilter) -> Result<Vec<EventEnvelope>, JournalError>;
}
