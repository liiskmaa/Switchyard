//! Event model and in-process event transport for Switchyard Core.
//!
//! * [`envelope`] defines the durable event envelope and the typed catalogue of
//!   domain events ([`DomainEvent`]).
//! * [`journal`] defines the persistence contract ([`EventJournal`]). This crate
//!   does not know how events are stored; `switchyard-storage` implements it.
//! * [`bus`] is the transport: it persists through the journal first and only
//!   then fans the stored event out to subscribers over a bounded channel.

pub mod bus;
pub mod envelope;
pub mod ids;
pub mod journal;

pub use bus::{EventBus, EventBusConfig, EventSubscription, PublishError, RecvError};
pub use envelope::{
    DomainEvent, EVENT_SCHEMA_VERSION, EventContext, EventDecodeError, EventDraft, EventEnvelope,
};
pub use ids::{EventId, new_id, now_ms};
pub use journal::{EventFilter, EventJournal, JournalError};
