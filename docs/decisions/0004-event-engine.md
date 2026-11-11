# ADR 0004: Event engine with persist-then-publish and journal catch-up

- Status: accepted (2026-11-11)

## Decision

1. **Persist before publishing.** `EventBus::publish` appends to the journal first and broadcasts
   only if the append succeeded.
2. **Transactional outbox for domain changes.** `EventBus::publish_with(work)` runs a closure that
   writes the domain change and the event in one SQLite transaction. The state and the journal can
   never disagree, for example a project row without its `project.registered` event.
3. **Total order.** Append and broadcast happen under one async mutex, so delivery order equals
   journal `seq` order.
4. **Bounded transport with lossless recovery.** Fan-out uses `tokio::sync::broadcast` (bounded,
   1024). A subscriber that lags re-reads the gap from the journal in pages of 500 and resumes live,
   discarding duplicates by `seq`. Publishers never block on slow subscribers.
5. **Separation.** The `switchyard-events` crate defines the `EventJournal` trait, and
   `switchyard-storage` implements it. The bus does not know about SQLite.
6. **Typed in code, open on the wire.** Producers construct a typed `DomainEvent`. Stored envelopes
   keep `event_type: String` and `payload: Value`, so newer or unknown events can still be listed and
   forwarded. `EventEnvelope::decode()` gives the typed form.

## Consequences

- Every event costs one fsync. That is acceptable for lifecycle and domain events. High-frequency
  agent output deltas (future work) may need a separate, non-durable or batched channel. That will be
  decided when adapters exist.
- No external broker is needed. The journal doubles as the replay source for reconnecting clients.
