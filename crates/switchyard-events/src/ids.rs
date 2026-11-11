use std::time::{SystemTime, UNIX_EPOCH};

use uuid::Uuid;

/// Globally unique event identifier (UUIDv7, time ordered).
pub type EventId = Uuid;

/// Generates a new time-ordered identifier for any Switchyard entity.
pub fn new_id() -> Uuid {
    Uuid::now_v7()
}

/// Current wall-clock time as Unix epoch milliseconds.
///
/// A clock set before 1970 yields 0 rather than panicking.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
