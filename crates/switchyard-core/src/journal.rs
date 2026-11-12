//! Transactional outbox shared by the domain services: domain writes and
//! their journal events commit in one SQLite transaction, then the events
//! are broadcast in order.

use switchyard_events::{DomainEvent, EventBus, EventContext, EventDraft};
use switchyard_protocol::ProtocolError;
use switchyard_storage::{Storage, StorageError, Transaction};

use crate::handlers::errors;

pub(crate) fn draft(event: DomainEvent, ctx: &EventContext) -> Result<EventDraft, StorageError> {
    EventDraft::new(event, ctx.clone())
        .map_err(|e| StorageError::Corrupt(format!("cannot encode event: {e}")))
}

/// Runs `work` in one transaction, appends the drafts it returns, commits
/// and publishes them in order.
pub(crate) async fn commit<R, F>(storage: &Storage, bus: &EventBus, work: F) -> Result<R, ProtocolError>
where
    F: FnOnce(&Transaction<'_>) -> Result<(R, Vec<EventDraft>), StorageError> + Send + 'static,
    R: Send + 'static,
{
    let storage = storage.clone();
    bus.publish_batch_with(move || {
        storage.with_conn(|conn| {
            let tx = conn.transaction()?;
            let (value, drafts) = work(&tx)?;
            let mut stored = Vec::with_capacity(drafts.len());
            for d in drafts {
                stored.push(switchyard_storage::events::append(&tx, d)?);
            }
            tx.commit()?;
            Ok((value, stored))
        })
    })
    .await
    .map_err(errors::publish)?
    .map(|(value, _)| value)
    .map_err(errors::storage)
}

/// Like [`commit`], for work that can fail with a domain error decided
/// inside the transaction (e.g. a status check). On a domain error nothing
/// is written.
pub(crate) async fn commit_checked<R, F>(
    storage: &Storage,
    bus: &EventBus,
    work: F,
) -> Result<R, ProtocolError>
where
    F: FnOnce(&Transaction<'_>) -> Result<Result<(R, Vec<EventDraft>), ProtocolError>, StorageError>
        + Send
        + 'static,
    R: Send + 'static,
{
    commit(storage, bus, move |tx| {
        Ok(match work(tx)? {
            Ok((value, drafts)) => (Ok(value), drafts),
            Err(e) => (Err(e), Vec::new()),
        })
    })
    .await?
}
