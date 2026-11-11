//! Ephemeral live output. Bounded and lossy by design: publishers never wait
//! for subscribers. Each session keeps only the text of the assistant
//! message currently streaming, so a reconnecting client can catch up on
//! in-progress output; completed messages live in the durable journal.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use switchyard_agents::{LiveChunk, LiveKind as AdapterLiveKind};
use switchyard_protocol::model::{LiveKind, LiveOutput};
use tokio::sync::broadcast;
use uuid::Uuid;

/// Maximum in-progress text retained per session.
const MAX_SNAPSHOT_BYTES: usize = 64 * 1024;

#[derive(Default)]
struct Buffer {
    seq: u64,
    text: String,
    truncated: bool,
}

#[derive(Clone)]
pub(crate) struct LiveHub {
    sender: broadcast::Sender<Arc<LiveOutput>>,
    buffers: Arc<Mutex<HashMap<Uuid, Buffer>>>,
}

impl LiveHub {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity.max(1));
        Self { sender, buffers: Arc::default() }
    }

    pub fn publish(&self, session_id: Uuid, chunk: LiveChunk) {
        let kind = match chunk.kind {
            AdapterLiveKind::Text => LiveKind::Text,
            AdapterLiveKind::Thinking => LiveKind::Thinking,
            AdapterLiveKind::MessageStart => LiveKind::MessageStart,
            AdapterLiveKind::MessageStop => LiveKind::MessageStop,
        };
        let seq = {
            let Ok(mut buffers) = self.buffers.lock() else { return };
            let buffer = buffers.entry(session_id).or_default();
            buffer.seq += 1;
            match kind {
                LiveKind::MessageStart => {
                    buffer.text.clear();
                    buffer.truncated = false;
                }
                LiveKind::Text if buffer.text.len() + chunk.text.len() <= MAX_SNAPSHOT_BYTES => {
                    buffer.text.push_str(&chunk.text);
                }
                LiveKind::Text => buffer.truncated = true,
                _ => {}
            }
            buffer.seq
        };
        // No subscribers is fine: live output is not retained.
        let _ =
            self.sender.send(Arc::new(LiveOutput { session_id, seq, kind, text: chunk.text, missed: None }));
    }

    /// In-progress assistant text for `session_id` and its live sequence.
    pub fn snapshot(&self, session_id: Uuid) -> (String, u64) {
        self.buffers
            .lock()
            .ok()
            .and_then(|b| b.get(&session_id).map(|buf| (buf.text.clone(), buf.seq)))
            .unwrap_or_default()
    }

    /// Clears the in-progress text (turn finished or process exited).
    pub fn clear(&self, session_id: Uuid) {
        if let Ok(mut buffers) = self.buffers.lock()
            && let Some(buffer) = buffers.get_mut(&session_id)
        {
            buffer.text.clear();
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<LiveOutput>> {
        self.sender.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(kind: AdapterLiveKind, text: &str) -> LiveChunk {
        LiveChunk { kind, text: text.into() }
    }

    #[tokio::test]
    async fn snapshot_tracks_current_message_and_lagging_is_detected() {
        let hub = LiveHub::new(2);
        let s = Uuid::now_v7();
        let mut rx = hub.subscribe();
        hub.publish(s, chunk(AdapterLiveKind::MessageStart, ""));
        hub.publish(s, chunk(AdapterLiveKind::Text, "Hel"));
        hub.publish(s, chunk(AdapterLiveKind::Text, "lo"));
        assert_eq!(hub.snapshot(s), ("Hello".to_owned(), 3));
        // Capacity 2: the first chunk was overwritten for this slow receiver.
        assert!(matches!(rx.recv().await, Err(broadcast::error::RecvError::Lagged(1))));
        hub.publish(s, chunk(AdapterLiveKind::MessageStart, ""));
        assert_eq!(hub.snapshot(s).0, "");
    }
}
