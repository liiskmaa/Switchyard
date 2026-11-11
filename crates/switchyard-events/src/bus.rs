use std::collections::VecDeque;
use std::sync::Arc;

use tokio::sync::{Mutex, broadcast};

use crate::envelope::{DomainEvent, EventContext, EventDraft, EventEnvelope};
use crate::journal::{EventFilter, EventJournal, JournalError};

/// Page size used when a subscriber replays or catches up from the journal.
const CATCH_UP_PAGE: u32 = 500;

#[derive(Debug, Clone, Copy)]
pub struct EventBusConfig {
    /// Capacity of the live broadcast ring. A subscriber that falls more than
    /// this many events behind is lagged and transparently catches up from the
    /// journal.
    pub channel_capacity: usize,
}

impl Default for EventBusConfig {
    fn default() -> Self {
        Self { channel_capacity: 1024 }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    #[error("failed to encode event payload: {0}")]
    Encode(#[from] serde_json::Error),
    #[error(transparent)]
    Journal(#[from] JournalError),
    #[error("journal task failed: {0}")]
    Task(String),
}

#[derive(Debug, thiserror::Error)]
pub enum RecvError {
    #[error("event bus closed")]
    Closed,
    #[error(transparent)]
    Journal(#[from] JournalError),
    #[error("journal task failed: {0}")]
    Task(String),
}

/// In-process event transport.
///
/// Persistence is delegated to an [`EventJournal`]; this type only orders,
/// stores-then-publishes and fans out. Publication is serialized so that the
/// order in which subscribers observe events equals journal `seq` order.
#[derive(Clone)]
pub struct EventBus {
    journal: Arc<dyn EventJournal>,
    sender: broadcast::Sender<Arc<EventEnvelope>>,
    publish_lock: Arc<Mutex<()>>,
}

impl EventBus {
    pub fn new(journal: Arc<dyn EventJournal>, config: EventBusConfig) -> Self {
        let (sender, _) = broadcast::channel(config.channel_capacity.max(1));
        Self { journal, sender, publish_lock: Arc::new(Mutex::new(())) }
    }

    /// Persists the event, then delivers it to live subscribers.
    ///
    /// If persistence fails the event is not delivered to anyone.
    pub async fn publish(
        &self,
        event: DomainEvent,
        context: EventContext,
    ) -> Result<Arc<EventEnvelope>, PublishError> {
        let draft = EventDraft::new(event, context)?;
        let _ordering = self.publish_lock.lock().await;
        let journal = Arc::clone(&self.journal);
        let stored = tokio::task::spawn_blocking(move || journal.append(draft))
            .await
            .map_err(|e| PublishError::Task(e.to_string()))??;
        let stored = Arc::new(stored);
        // `send` only fails when there are no live subscribers, which is a
        // normal condition: the event is already durable in the journal.
        if self.sender.send(Arc::clone(&stored)).is_err() {
            tracing::trace!(seq = stored.seq, "event stored; no live subscribers");
        }
        Ok(stored)
    }

    /// Runs `work` — which must durably store a domain change *and* its event
    /// in one transaction and return the stored envelope — then delivers the
    /// event to live subscribers.
    ///
    /// This is the transactional-outbox path: the domain state and the journal
    /// can never disagree. `work` runs on a blocking thread while the bus's
    /// ordering lock is held, so delivery order still equals `seq` order.
    /// If `work` fails nothing is delivered and its error is returned as-is.
    pub async fn publish_with<R, E, F>(
        &self,
        work: F,
    ) -> Result<Result<(R, Arc<EventEnvelope>), E>, PublishError>
    where
        F: FnOnce() -> Result<(R, EventEnvelope), E> + Send + 'static,
        R: Send + 'static,
        E: Send + 'static,
    {
        let _ordering = self.publish_lock.lock().await;
        let outcome =
            tokio::task::spawn_blocking(work).await.map_err(|e| PublishError::Task(e.to_string()))?;
        Ok(outcome.map(|(value, stored)| {
            let stored = Arc::new(stored);
            if self.sender.send(Arc::clone(&stored)).is_err() {
                tracing::trace!(seq = stored.seq, "event stored; no live subscribers");
            }
            (value, stored)
        }))
    }

    /// Like [`Self::publish_with`] for a transaction that stores any number
    /// of events (possibly none). They are delivered in the order returned,
    /// which must be ascending `seq`.
    pub async fn publish_batch_with<R, E, F>(
        &self,
        work: F,
    ) -> Result<Result<(R, Vec<Arc<EventEnvelope>>), E>, PublishError>
    where
        F: FnOnce() -> Result<(R, Vec<EventEnvelope>), E> + Send + 'static,
        R: Send + 'static,
        E: Send + 'static,
    {
        let _ordering = self.publish_lock.lock().await;
        let outcome =
            tokio::task::spawn_blocking(work).await.map_err(|e| PublishError::Task(e.to_string()))?;
        Ok(outcome.map(|(value, stored)| {
            let stored: Vec<_> = stored.into_iter().map(Arc::new).collect();
            for event in &stored {
                if self.sender.send(Arc::clone(event)).is_err() {
                    tracing::trace!(seq = event.seq, "event stored; no live subscribers");
                }
            }
            (value, stored)
        }))
    }

    /// Reads stored events from the journal.
    pub async fn read(&self, filter: EventFilter) -> Result<Vec<EventEnvelope>, RecvError> {
        let journal = Arc::clone(&self.journal);
        tokio::task::spawn_blocking(move || journal.read(&filter))
            .await
            .map_err(|e| RecvError::Task(e.to_string()))?
            .map_err(RecvError::from)
    }

    /// Opens a lossless, ordered subscription.
    ///
    /// * `after_seq = Some(n)` first replays stored events with `seq > n`,
    ///   then continues live without gaps or duplicates.
    /// * `after_seq = None` delivers only events published after this call.
    ///
    /// `filter.after_seq` and `filter.limit` are ignored; the other criteria
    /// select which events are delivered.
    pub fn subscribe(&self, after_seq: Option<i64>, filter: EventFilter) -> EventSubscription {
        // Subscribe to the live channel *before* any journal read so that no
        // event can fall between the replay and the live stream.
        let receiver = self.sender.subscribe();
        EventSubscription {
            journal: Arc::clone(&self.journal),
            receiver,
            filter: EventFilter { after_seq: None, limit: CATCH_UP_PAGE, ..filter },
            last_seq: after_seq.unwrap_or(0),
            backlog: VecDeque::new(),
            catching_up: after_seq.is_some(),
            lag_recoveries: 0,
        }
    }

    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

/// A single consumer's ordered view of the event stream.
pub struct EventSubscription {
    journal: Arc<dyn EventJournal>,
    receiver: broadcast::Receiver<Arc<EventEnvelope>>,
    filter: EventFilter,
    last_seq: i64,
    backlog: VecDeque<Arc<EventEnvelope>>,
    catching_up: bool,
    lag_recoveries: u64,
}

impl EventSubscription {
    /// Returns the next matching event in `seq` order.
    pub async fn next(&mut self) -> Result<Arc<EventEnvelope>, RecvError> {
        loop {
            if let Some(event) = self.backlog.pop_front() {
                self.last_seq = event.seq;
                return Ok(event);
            }
            if self.catching_up {
                self.load_page().await?;
                continue;
            }
            match self.receiver.recv().await {
                Ok(event) if event.seq <= self.last_seq => continue,
                Ok(event) => {
                    self.last_seq = event.seq;
                    if self.filter.matches(&event) {
                        return Ok(event);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    self.lag_recoveries += 1;
                    tracing::warn!(
                        missed,
                        last_seq = self.last_seq,
                        "event subscriber lagged; catching up from journal"
                    );
                    self.catching_up = true;
                }
                Err(broadcast::error::RecvError::Closed) => return Err(RecvError::Closed),
            }
        }
    }

    /// Sequence number of the last event this subscription has passed.
    pub fn last_seq(&self) -> i64 {
        self.last_seq
    }

    /// How many times this subscription fell behind the live channel and
    /// recovered from the journal.
    pub fn lag_recoveries(&self) -> u64 {
        self.lag_recoveries
    }

    async fn load_page(&mut self) -> Result<(), RecvError> {
        let journal = Arc::clone(&self.journal);
        let filter = EventFilter { after_seq: Some(self.last_seq), ..self.filter.clone() };
        let page = tokio::task::spawn_blocking(move || journal.read(&filter))
            .await
            .map_err(|e| RecvError::Task(e.to_string()))??;
        if page.len() < CATCH_UP_PAGE as usize {
            self.catching_up = false;
        }
        if let Some(last) = page.last() {
            // Advance even when the page is consumed lazily, so a filtered-out
            // tail is not re-read.
            self.last_seq = self.last_seq.max(last.seq);
        }
        self.backlog.extend(page.into_iter().map(Arc::new));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::*;
    use crate::ids::new_id;

    #[derive(Default)]
    struct MemoryJournal {
        events: StdMutex<Vec<EventEnvelope>>,
        fail: AtomicBool,
    }

    impl EventJournal for MemoryJournal {
        fn append(&self, draft: EventDraft) -> Result<EventEnvelope, JournalError> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(JournalError::Storage("disk full".into()));
            }
            let mut events = self.events.lock().map_err(|e| JournalError::Storage(e.to_string()))?;
            let seq = events.last().map_or(1, |e| e.seq + 1);
            let envelope = draft.into_envelope(seq);
            events.push(envelope.clone());
            Ok(envelope)
        }

        fn read(&self, filter: &EventFilter) -> Result<Vec<EventEnvelope>, JournalError> {
            let events = self.events.lock().map_err(|e| JournalError::Storage(e.to_string()))?;
            Ok(events.iter().filter(|e| filter.matches(e)).take(filter.limit as usize).cloned().collect())
        }
    }

    fn project_event(project_id: uuid::Uuid) -> (DomainEvent, EventContext) {
        (
            DomainEvent::ProjectRegistered { project_id, name: "p".into(), root_path: "/p".into() },
            EventContext { project_id: Some(project_id), ..Default::default() },
        )
    }

    async fn next(sub: &mut EventSubscription) -> Arc<EventEnvelope> {
        match tokio::time::timeout(Duration::from_secs(2), sub.next()).await {
            Ok(Ok(e)) => e,
            other => panic!("expected event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn event_is_durable_before_subscribers_see_it() {
        let journal = Arc::new(MemoryJournal::default());
        let bus = EventBus::new(journal.clone(), EventBusConfig::default());
        let mut sub = bus.subscribe(None, EventFilter::default());
        let (event, ctx) = project_event(new_id());
        let published = bus.publish(event, ctx).await.unwrap();
        let received = next(&mut sub).await;
        assert_eq!(received.id, published.id);
        let stored = journal.read(&EventFilter { limit: 10, ..Default::default() }).unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].id, received.id);
    }

    #[tokio::test]
    async fn journal_failure_is_reported_and_not_broadcast() {
        let journal = Arc::new(MemoryJournal::default());
        journal.fail.store(true, Ordering::SeqCst);
        let bus = EventBus::new(journal.clone(), EventBusConfig::default());
        let mut sub = bus.subscribe(None, EventFilter::default());
        let (event, ctx) = project_event(new_id());
        assert!(matches!(bus.publish(event, ctx).await, Err(PublishError::Journal(_))));
        assert!(tokio::time::timeout(Duration::from_millis(100), sub.next()).await.is_err());
    }

    #[tokio::test]
    async fn lagged_subscriber_catches_up_from_journal_without_gaps() {
        let journal = Arc::new(MemoryJournal::default());
        let bus = EventBus::new(journal, EventBusConfig { channel_capacity: 2 });
        let mut sub = bus.subscribe(None, EventFilter::default());
        for _ in 0..25 {
            let (event, ctx) = project_event(new_id());
            bus.publish(event, ctx).await.unwrap();
        }
        let seqs: Vec<i64> = {
            let mut v = Vec::new();
            for _ in 0..25 {
                v.push(next(&mut sub).await.seq);
            }
            v
        };
        assert_eq!(seqs, (1..=25).collect::<Vec<_>>());
        assert!(sub.lag_recoveries() >= 1);
    }

    #[tokio::test]
    async fn replay_then_live_has_no_duplicates_and_respects_filter() {
        let journal = Arc::new(MemoryJournal::default());
        let bus = EventBus::new(journal, EventBusConfig::default());
        let wanted = new_id();
        for i in 0..4 {
            let (event, ctx) = project_event(if i % 2 == 0 { wanted } else { new_id() });
            bus.publish(event, ctx).await.unwrap();
        }
        let mut sub = bus.subscribe(Some(0), EventFilter { project_id: Some(wanted), ..Default::default() });
        let (event, ctx) = project_event(wanted);
        bus.publish(event, ctx).await.unwrap();
        let seqs = vec![next(&mut sub).await.seq, next(&mut sub).await.seq, next(&mut sub).await.seq];
        assert_eq!(seqs, vec![1, 3, 5]);
        assert!(tokio::time::timeout(Duration::from_millis(100), sub.next()).await.is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_publishers_are_observed_in_seq_order() {
        let journal = Arc::new(MemoryJournal::default());
        let bus = EventBus::new(journal, EventBusConfig::default());
        let mut sub = bus.subscribe(None, EventFilter::default());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let bus = bus.clone();
            handles.push(tokio::spawn(async move {
                for _ in 0..10 {
                    let (event, ctx) = project_event(new_id());
                    bus.publish(event, ctx).await.unwrap();
                }
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        let mut previous = 0;
        for _ in 0..80 {
            let seq = next(&mut sub).await.seq;
            assert!(seq > previous);
            previous = seq;
        }
    }
}
