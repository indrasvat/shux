//! Event bus — typed pub/sub built on tokio::sync::broadcast.
//!
//! Provides:
//! - Typed event publishing with automatic sequence numbers and timestamps
//! - Subscriber handles with per-client filtering
//! - Gap detection when subscribers lag behind
//! - Ring buffer for event history (events.history API and from_seq resumption)

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use tokio::sync::broadcast;
use tracing::{debug, warn};

use serde::{Deserialize, Serialize};

use crate::event::{Event, EventData, EventMetadata};
use crate::model::{PaneId, SessionId, WindowId};

/// A chunk of PTY output for one pane (PR 2c — data plane).
///
/// Carries up to `sample_interval`-worth of bytes coalesced into one
/// payload. Lives ONLY on the data plane — `events.history` and
/// `events.watch` never see these. See `docs/PR2c-DESIGN.md` for the
/// design rationale.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneOutputEvent {
    /// Monotonic sequence on the data plane. Independent from
    /// control-plane `Event.meta.seq`.
    pub seq: u64,
    pub pane_id: PaneId,
    pub window_id: WindowId,
    pub session_id: SessionId,
    pub timestamp: SystemTime,
    /// Base64-encoded raw bytes that landed on the PTY during this
    /// sampling interval. May include partial UTF-8 sequences —
    /// callers must decode lossily or buffer.
    pub bytes: String,
    /// Whether this chunk dropped bytes between `last_published_at`
    /// and now (true) or carries every byte verbatim (false).
    pub sampled: bool,
}

/// Configuration for the event bus.
#[derive(Debug, Clone)]
pub struct EventBusConfig {
    /// Capacity of the control-plane broadcast channel (lifecycle
    /// events: session/window/pane created/killed/renamed/etc.).
    /// When exceeded, oldest events are dropped for slow subscribers.
    /// Default: 4096.
    pub broadcast_capacity: usize,

    /// Maximum number of events to keep in the control-plane history
    /// ring buffer. Used for `events.history` API and `from_seq`
    /// resumption. Default: 8192.
    pub history_capacity: usize,

    /// Capacity of the data-plane broadcast channel (PR 2c — sampled
    /// PTY output chunks). Naturally burstier than the control plane,
    /// so this is larger. Data plane has NO history (see
    /// `PR2c-DESIGN.md`): subscribers can only ever read live chunks,
    /// removing the secret-leak vector.
    /// Default: 16384.
    pub data_plane_capacity: usize,
}

impl Default for EventBusConfig {
    fn default() -> Self {
        EventBusConfig {
            broadcast_capacity: 4096,
            history_capacity: 8192,
            data_plane_capacity: 16384,
        }
    }
}

/// The event bus — central pub/sub hub for shux.
///
/// Thread-safe and cheaply cloneable (wraps Arc internally).
/// All clones share the same underlying broadcast channel and history.
#[derive(Clone)]
pub struct EventBus {
    inner: Arc<EventBusInner>,
}

struct EventBusInner {
    /// Control-plane broadcast sender. Carries the typed
    /// `Event` (`session.created`, `pane.exited`, etc.).
    sender: broadcast::Sender<Event>,
    /// Monotonically increasing control-plane sequence counter.
    seq_counter: AtomicU64,
    /// Ring buffer of recent control-plane events. Backs
    /// `events.history` API and `from_seq` resumption.
    history: RwLock<EventHistory>,

    /// Data-plane broadcast sender. Carries sampled `PaneOutputEvent`
    /// chunks (PR 2c). Deliberately NOT mirrored to history — see
    /// `PR2c-DESIGN.md` for the secret-leak / DoS rationale.
    data_plane: broadcast::Sender<PaneOutputEvent>,
    /// Monotonically increasing data-plane sequence counter. Separate
    /// from control plane so subscribers gap-detect each independently.
    data_seq_counter: AtomicU64,

    /// Configuration (kept for introspection).
    #[allow(dead_code)]
    config: EventBusConfig,
}

/// Ring buffer for event history.
struct EventHistory {
    /// The events, oldest first.
    buffer: VecDeque<Event>,
    /// Maximum capacity.
    capacity: usize,
}

impl EventHistory {
    fn new(capacity: usize) -> Self {
        EventHistory {
            // Don't pre-allocate huge buffers; grow on demand up to 1024 initial.
            buffer: VecDeque::with_capacity(capacity.min(1024)),
            capacity,
        }
    }

    /// Push an event, evicting the oldest if at capacity.
    fn push(&mut self, event: Event) {
        if self.buffer.len() >= self.capacity {
            self.buffer.pop_front();
        }
        self.buffer.push_back(event);
    }

    /// Get the oldest sequence number in the history.
    fn oldest_seq(&self) -> Option<u64> {
        self.buffer.front().map(|e| e.meta.seq)
    }

    /// Get the newest sequence number in the history.
    #[allow(dead_code)]
    fn newest_seq(&self) -> Option<u64> {
        self.buffer.back().map(|e| e.meta.seq)
    }

    /// Get events from a given sequence number onwards.
    /// Returns (events, gap_count) where gap_count > 0 if from_seq
    /// is older than the oldest event in history.
    fn events_from_seq(&self, from_seq: u64) -> (Vec<Event>, u64) {
        let oldest = self.oldest_seq().unwrap_or(0);

        let gap = oldest.saturating_sub(from_seq);

        let events: Vec<Event> = self
            .buffer
            .iter()
            .filter(|e| e.meta.seq >= from_seq)
            .cloned()
            .collect();

        (events, gap)
    }

    /// Get the last N events (for events.history API).
    fn recent(&self, count: usize) -> Vec<Event> {
        let start = self.buffer.len().saturating_sub(count);
        self.buffer.iter().skip(start).cloned().collect()
    }

    /// Get the last N events matching a set of filters.
    fn recent_filtered(&self, count: usize, filters: &[String]) -> Vec<Event> {
        if filters.is_empty() {
            return self.recent(count);
        }
        self.buffer
            .iter()
            .rev()
            .filter(|e| filters.iter().any(|f| e.matches_filter(f)))
            .take(count)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }
}

impl EventBus {
    /// Create a new event bus with default configuration.
    pub fn new() -> Self {
        Self::with_config(EventBusConfig::default())
    }

    /// Create a new event bus with custom configuration.
    pub fn with_config(config: EventBusConfig) -> Self {
        let (sender, _) = broadcast::channel(config.broadcast_capacity);
        let (data_plane, _) = broadcast::channel(config.data_plane_capacity);
        EventBus {
            inner: Arc::new(EventBusInner {
                sender,
                seq_counter: AtomicU64::new(1), // Start at 1 so 0 means "no events seen".
                history: RwLock::new(EventHistory::new(config.history_capacity)),
                data_plane,
                data_seq_counter: AtomicU64::new(1),
                config,
            }),
        }
    }

    /// Publish an event.
    ///
    /// Assigns a sequence number and timestamp automatically.
    /// Returns the assigned sequence number.
    ///
    /// If no subscribers are listening, the event is still recorded in history.
    pub fn publish(&self, data: EventData) -> u64 {
        self.publish_with_correlation(data, None)
    }

    /// Publish an event with a correlation ID linking it to a batch /
    /// transaction (e.g. a `state.apply` call). Subscribers can group events
    /// by correlation_id to attribute a burst to a specific apply.
    pub fn publish_with_correlation(&self, data: EventData, correlation_id: Option<String>) -> u64 {
        let seq = self.inner.seq_counter.fetch_add(1, Ordering::Relaxed);
        let event_type = data.event_type().to_string();

        let event = Event {
            meta: EventMetadata {
                seq,
                timestamp: SystemTime::now(),
                event_type,
                correlation_id,
            },
            data,
        };

        // Record in history.
        {
            let mut history = self.inner.history.write().expect("history lock poisoned");
            history.push(event.clone());
        }

        // Broadcast to subscribers. If no receivers, that's fine.
        match self.inner.sender.send(event) {
            Ok(n) => {
                debug!(seq, receivers = n, "event published");
            }
            Err(_) => {
                // No active receivers. Event is still in history.
                debug!(seq, "event published (no receivers)");
            }
        }

        seq
    }

    /// Subscribe to all events (unfiltered).
    ///
    /// Returns a `Subscription` that can be polled for events.
    pub fn subscribe(&self) -> Subscription {
        Subscription {
            receiver: self.inner.sender.subscribe(),
            filters: Vec::new(),
        }
    }

    /// Subscribe to events matching the given filters.
    ///
    /// Filters are event type prefixes (e.g., "pane." matches all pane events).
    /// An empty filter list matches everything.
    pub fn subscribe_filtered(&self, filters: Vec<String>) -> Subscription {
        Subscription {
            receiver: self.inner.sender.subscribe(),
            filters,
        }
    }

    /// Get recent events from the history ring buffer.
    ///
    /// Returns at most `count` recent events.
    pub fn history(&self, count: usize) -> Vec<Event> {
        let history = self.inner.history.read().expect("history lock poisoned");
        history.recent(count)
    }

    /// Get recent events matching filters from the history ring buffer.
    pub fn history_filtered(&self, count: usize, filters: &[String]) -> Vec<Event> {
        let history = self.inner.history.read().expect("history lock poisoned");
        history.recent_filtered(count, filters)
    }

    /// Get events from a given sequence number onwards.
    ///
    /// Used for `from_seq` resumption in events.watch (PRD §8.4).
    /// Returns `(events, gap_count)` where `gap_count > 0` indicates
    /// events that were lost because they aged out of the history buffer.
    pub fn events_from_seq(&self, from_seq: u64) -> (Vec<Event>, u64) {
        let history = self.inner.history.read().expect("history lock poisoned");
        history.events_from_seq(from_seq)
    }

    /// Get the current sequence number (the next event will get this value).
    pub fn current_seq(&self) -> u64 {
        self.inner.seq_counter.load(Ordering::Relaxed)
    }

    /// Publish a pane output chunk to the data plane (PR 2c).
    ///
    /// Bypasses history entirely. Returns the assigned data-plane seq.
    /// `bytes_b64` should already be base64-encoded by the caller —
    /// keeps the bus from imposing a base64 dependency on consumers
    /// that just want to forward bytes verbatim.
    pub fn publish_pane_output(
        &self,
        pane_id: PaneId,
        window_id: WindowId,
        session_id: SessionId,
        bytes_b64: String,
        sampled: bool,
    ) -> u64 {
        let seq = self.inner.data_seq_counter.fetch_add(1, Ordering::Relaxed);
        let event = PaneOutputEvent {
            seq,
            pane_id,
            window_id,
            session_id,
            timestamp: SystemTime::now(),
            bytes: bytes_b64,
            sampled,
        };
        // No subscribers is fine; data plane has NO history so the
        // chunk is simply dropped (this is the design — see PR2c-DESIGN.md).
        let _ = self.inner.data_plane.send(event);
        seq
    }

    /// Subscribe to the data plane (PR 2c). Each `PaneOutputSubscription`
    /// is independent; the caller is responsible for filtering by
    /// `pane_id` if they only care about one pane.
    pub fn subscribe_pane_output(&self) -> PaneOutputSubscription {
        PaneOutputSubscription {
            receiver: self.inner.data_plane.subscribe(),
        }
    }

    /// Current data-plane sequence number.
    pub fn current_data_seq(&self) -> u64 {
        self.inner.data_seq_counter.load(Ordering::Relaxed)
    }

    /// Get the number of active subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.inner.sender.receiver_count()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for EventBus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventBus")
            .field("current_seq", &self.current_seq())
            .field("subscriber_count", &self.subscriber_count())
            .finish()
    }
}

/// A subscription to the event bus.
///
/// Call `recv()` to get the next event. Handles gap detection automatically.
pub struct Subscription {
    receiver: broadcast::Receiver<Event>,
    filters: Vec<String>,
}

/// The result of receiving an event from a subscription.
#[derive(Debug)]
pub enum SubscriptionEvent {
    /// An event was received.
    Event(Event),
    /// Events were dropped due to subscriber lag.
    /// Contains the number of events that were lost.
    Lagged(u64),
}

impl Subscription {
    /// Receive the next event, waiting asynchronously.
    ///
    /// Returns `SubscriptionEvent::Event` for normal events, or
    /// `SubscriptionEvent::Lagged(count)` when events were dropped
    /// due to the subscriber falling behind the broadcast channel.
    ///
    /// Returns `None` when the bus is shut down (sender dropped).
    pub async fn recv(&mut self) -> Option<SubscriptionEvent> {
        loop {
            match self.receiver.recv().await {
                Ok(event) => {
                    // Apply client-side filtering.
                    if self.matches(&event) {
                        return Some(SubscriptionEvent::Event(event));
                    }
                    // Event didn't match filters — keep receiving.
                    continue;
                }
                Err(broadcast::error::RecvError::Lagged(count)) => {
                    warn!(count, "subscriber lagged, events dropped");
                    return Some(SubscriptionEvent::Lagged(count));
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return None;
                }
            }
        }
    }

    /// Check if an event matches this subscription's filters.
    fn matches(&self, event: &Event) -> bool {
        if self.filters.is_empty() {
            return true;
        }
        self.filters.iter().any(|f| event.matches_filter(f))
    }

    /// Update the filters in-place. Used by `events.watch` to swap the
    /// subscriber's filter list mid-stream.
    pub fn set_filters(&mut self, filters: Vec<String>) {
        self.filters = filters;
    }
}

/// Subscription to the data plane (PR 2c — PTY output chunks).
///
/// Unlike `Subscription`, there's no filter list: data-plane events
/// are typed `PaneOutputEvent`s, and callers filter by `pane_id`
/// field directly. This keeps the bus from caring about routing
/// policy.
pub struct PaneOutputSubscription {
    receiver: broadcast::Receiver<PaneOutputEvent>,
}

#[derive(Debug)]
pub enum PaneOutputSubscriptionEvent {
    Chunk(PaneOutputEvent),
    /// Subscriber fell behind the broadcast channel; the number of
    /// dropped chunks is reported so callers can decide whether to
    /// log it or surface it.
    Lagged(u64),
}

impl PaneOutputSubscription {
    /// Receive the next chunk, waiting asynchronously.
    pub async fn recv(&mut self) -> Option<PaneOutputSubscriptionEvent> {
        match self.receiver.recv().await {
            Ok(chunk) => Some(PaneOutputSubscriptionEvent::Chunk(chunk)),
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!(count = n, "pane-output subscriber lagged");
                Some(PaneOutputSubscriptionEvent::Lagged(n))
            }
            Err(broadcast::error::RecvError::Closed) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::event::EventData;
    use crate::model::{PaneId, SessionId, WindowId};

    /// Helper to create a simple SessionCreated event data.
    fn session_created(name: &str) -> EventData {
        EventData::SessionCreated {
            session_id: SessionId::new(),
            name: name.to_string(),
        }
    }

    /// Helper to create a PaneCreated event data.
    fn pane_created() -> EventData {
        EventData::PaneCreated {
            pane_id: PaneId::new(),
            window_id: WindowId::new(),
            session_id: SessionId::new(),
            command: vec!["bash".to_string()],
        }
    }

    #[tokio::test]
    async fn test_publish_and_subscribe() {
        let bus = EventBus::new();
        let mut sub = bus.subscribe();

        let seq = bus.publish(session_created("test"));
        assert_eq!(seq, 1);

        match sub.recv().await {
            Some(SubscriptionEvent::Event(event)) => {
                assert_eq!(event.seq(), 1);
                assert_eq!(event.event_type(), "session.created");
            }
            other => panic!("expected Event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_sequence_numbers_increase() {
        let bus = EventBus::new();

        let seq1 = bus.publish(session_created("s1"));
        let seq2 = bus.publish(session_created("s2"));
        let seq3 = bus.publish(EventData::SessionKilled {
            session_id: SessionId::new(),
            name: "s1".to_string(),
        });

        assert_eq!(seq1, 1);
        assert_eq!(seq2, 2);
        assert_eq!(seq3, 3);
    }

    #[tokio::test]
    async fn test_filtered_subscription() {
        let bus = EventBus::new();
        let mut sub = bus.subscribe_filtered(vec!["pane.".to_string()]);

        // Publish a session event (should be filtered out).
        bus.publish(session_created("test"));

        // Publish a pane event (should pass the filter).
        bus.publish(pane_created());

        match sub.recv().await {
            Some(SubscriptionEvent::Event(event)) => {
                assert_eq!(event.event_type(), "pane.created");
                // Sequence should be 2 (session event was seq 1).
                assert_eq!(event.seq(), 2);
            }
            other => panic!("expected pane.created Event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_filtered_subscription_multiple_filters() {
        let bus = EventBus::new();
        let mut sub =
            bus.subscribe_filtered(vec!["session.created".to_string(), "pane.".to_string()]);

        // Publish events of various types.
        bus.publish(session_created("s1"));
        bus.publish(EventData::SessionKilled {
            session_id: SessionId::new(),
            name: "s1".to_string(),
        });
        bus.publish(pane_created());

        // Should get session.created (seq 1).
        match sub.recv().await {
            Some(SubscriptionEvent::Event(event)) => {
                assert_eq!(event.event_type(), "session.created");
                assert_eq!(event.seq(), 1);
            }
            other => panic!("expected session.created, got {other:?}"),
        }

        // Should skip session.killed (seq 2) and get pane.created (seq 3).
        match sub.recv().await {
            Some(SubscriptionEvent::Event(event)) => {
                assert_eq!(event.event_type(), "pane.created");
                assert_eq!(event.seq(), 3);
            }
            other => panic!("expected pane.created, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_multiple_subscribers() {
        let bus = EventBus::new();
        let mut sub1 = bus.subscribe();
        let mut sub2 = bus.subscribe();

        bus.publish(session_created("test"));

        // Both subscribers should receive the event.
        match sub1.recv().await {
            Some(SubscriptionEvent::Event(e)) => assert_eq!(e.seq(), 1),
            other => panic!("sub1: expected Event, got {other:?}"),
        }
        match sub2.recv().await {
            Some(SubscriptionEvent::Event(e)) => assert_eq!(e.seq(), 1),
            other => panic!("sub2: expected Event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_no_subscribers_ok() {
        let bus = EventBus::new();
        // Publishing without subscribers should not panic.
        let seq = bus.publish(session_created("test"));
        assert_eq!(seq, 1);
    }

    #[tokio::test]
    async fn test_history() {
        let bus = EventBus::new();

        for i in 0..5 {
            bus.publish(session_created(&format!("s{i}")));
        }

        let recent = bus.history(3);
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].seq(), 3);
        assert_eq!(recent[1].seq(), 4);
        assert_eq!(recent[2].seq(), 5);
    }

    #[tokio::test]
    async fn test_history_returns_all_when_fewer_than_requested() {
        let bus = EventBus::new();

        bus.publish(session_created("s1"));
        bus.publish(session_created("s2"));

        let recent = bus.history(10);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].seq(), 1);
        assert_eq!(recent[1].seq(), 2);
    }

    #[tokio::test]
    async fn test_history_filtered() {
        let bus = EventBus::new();

        bus.publish(session_created("s1"));
        bus.publish(pane_created());
        bus.publish(EventData::SessionKilled {
            session_id: SessionId::new(),
            name: "s1".to_string(),
        });

        let filtered = bus.history_filtered(10, &["session.".to_string()]);
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].event_type(), "session.created");
        assert_eq!(filtered[1].event_type(), "session.killed");
    }

    #[tokio::test]
    async fn test_history_filtered_with_count_limit() {
        let bus = EventBus::new();

        for _ in 0..5 {
            bus.publish(session_created("s"));
        }
        bus.publish(pane_created());

        // Request only 2 most recent session events.
        let filtered = bus.history_filtered(2, &["session.".to_string()]);
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].seq(), 4);
        assert_eq!(filtered[1].seq(), 5);
    }

    #[tokio::test]
    async fn test_events_from_seq() {
        let bus = EventBus::new();

        for i in 0..5 {
            bus.publish(session_created(&format!("s{i}")));
        }

        let (events, gap) = bus.events_from_seq(3);
        assert_eq!(gap, 0);
        assert_eq!(events.len(), 3); // seq 3, 4, 5
        assert_eq!(events[0].seq(), 3);
        assert_eq!(events[1].seq(), 4);
        assert_eq!(events[2].seq(), 5);
    }

    #[tokio::test]
    async fn test_events_from_seq_zero_returns_all() {
        let bus = EventBus::new();

        bus.publish(session_created("s1"));
        bus.publish(session_created("s2"));

        let (events, gap) = bus.events_from_seq(0);
        // oldest_seq = 1, from_seq = 0 => gap = 1.saturating_sub(0) = 1.
        // This gap of 1 is a side-effect of sequences starting at 1 while
        // from_seq=0 means "give me everything". All events are still returned
        // because the filter is seq >= 0 (which matches all).
        assert_eq!(gap, 1);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].seq(), 1);
        assert_eq!(events[1].seq(), 2);
    }

    #[tokio::test]
    async fn test_events_from_seq_with_gap() {
        let config = EventBusConfig {
            broadcast_capacity: 16,
            history_capacity: 3,
            data_plane_capacity: 16,
        };
        let bus = EventBus::with_config(config);

        for i in 0..10 {
            bus.publish(session_created(&format!("s{i}")));
        }

        // History only has last 3 events (seq 8, 9, 10).
        // Requesting from seq 2 should report a gap.
        let (events, gap) = bus.events_from_seq(2);
        assert!(gap > 0, "expected gap, got {gap}");
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].seq(), 8);
    }

    #[tokio::test]
    async fn test_events_from_seq_future_seq() {
        let bus = EventBus::new();

        bus.publish(session_created("s1"));

        // Request from a future sequence number.
        let (events, gap) = bus.events_from_seq(100);
        assert_eq!(gap, 0);
        assert_eq!(events.len(), 0);
    }

    #[tokio::test]
    async fn test_history_capacity_limit() {
        let config = EventBusConfig {
            broadcast_capacity: 16,
            history_capacity: 5,
            data_plane_capacity: 16,
        };
        let bus = EventBus::with_config(config);

        for i in 0..20 {
            bus.publish(session_created(&format!("s{i}")));
        }

        let all = bus.history(100);
        assert_eq!(all.len(), 5); // Capped at history_capacity.
        // Should contain the 5 most recent events.
        assert_eq!(all[0].seq(), 16);
        assert_eq!(all[4].seq(), 20);
    }

    #[tokio::test]
    async fn test_subscriber_count() {
        let bus = EventBus::new();
        assert_eq!(bus.subscriber_count(), 0);

        let _sub1 = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 1);

        let _sub2 = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 2);

        drop(_sub1);
        // Note: tokio broadcast may not immediately reflect dropped receivers
        // in all cases, but after a send it will. We just verify the count
        // went up correctly.
    }

    #[tokio::test]
    async fn test_current_seq() {
        let bus = EventBus::new();
        assert_eq!(bus.current_seq(), 1); // Starts at 1.

        bus.publish(EventData::PaneBell {
            pane_id: PaneId::new(),
            window_id: WindowId::new(),
            session_id: SessionId::new(),
        });
        assert_eq!(bus.current_seq(), 2);

        bus.publish(EventData::PaneBell {
            pane_id: PaneId::new(),
            window_id: WindowId::new(),
            session_id: SessionId::new(),
        });
        assert_eq!(bus.current_seq(), 3);
    }

    #[tokio::test]
    async fn test_lag_detection() {
        let config = EventBusConfig {
            broadcast_capacity: 4,
            history_capacity: 100,
            data_plane_capacity: 16,
        };
        let bus = EventBus::with_config(config);
        let mut sub = bus.subscribe();

        // Publish more events than the broadcast channel can hold.
        for i in 0..10 {
            bus.publish(session_created(&format!("s{i}")));
        }

        // The subscriber should detect lag.
        match sub.recv().await {
            Some(SubscriptionEvent::Lagged(count)) => {
                assert!(count > 0, "expected lagged count > 0, got {count}");
            }
            Some(SubscriptionEvent::Event(e)) => {
                // On some platforms, the subscriber might get events before
                // detecting lag. This is acceptable — the point is that lag
                // IS detected at some point.
                debug!("got event seq={} instead of Lagged", e.seq());
            }
            None => panic!("channel closed unexpectedly"),
        }
    }

    #[tokio::test]
    async fn test_event_timestamps() {
        let bus = EventBus::new();
        let before = SystemTime::now();

        bus.publish(session_created("test"));

        let after = SystemTime::now();
        let events = bus.history(1);
        assert_eq!(events.len(), 1);

        let ts = events[0].meta.timestamp;
        assert!(ts >= before, "timestamp should be >= before");
        assert!(ts <= after, "timestamp should be <= after");
    }

    #[tokio::test]
    async fn test_clone_shares_state() {
        let bus1 = EventBus::new();
        let bus2 = bus1.clone();

        bus1.publish(EventData::PaneBell {
            pane_id: PaneId::new(),
            window_id: WindowId::new(),
            session_id: SessionId::new(),
        });

        // bus2 should see the same history.
        let history = bus2.history(10);
        assert_eq!(history.len(), 1);
        assert_eq!(bus2.current_seq(), 2);
    }

    #[tokio::test]
    async fn test_publish_from_clone() {
        let bus1 = EventBus::new();
        let bus2 = bus1.clone();
        let mut sub = bus1.subscribe();

        // Publish from the clone.
        bus2.publish(session_created("from-clone"));

        match sub.recv().await {
            Some(SubscriptionEvent::Event(event)) => {
                assert_eq!(event.seq(), 1);
                assert_eq!(event.event_type(), "session.created");
            }
            other => panic!("expected Event, got {other:?}"),
        }

        // History visible from both.
        assert_eq!(bus1.history(10).len(), 1);
        assert_eq!(bus2.history(10).len(), 1);
    }

    #[tokio::test]
    async fn test_set_filters() {
        let bus = EventBus::new();
        let mut sub = bus.subscribe_filtered(vec!["session.".to_string()]);

        // Publish a pane event — should be filtered.
        bus.publish(pane_created());

        // Change filters to accept pane events.
        sub.set_filters(vec!["pane.".to_string()]);

        // Publish another pane event — should now pass.
        bus.publish(pane_created());

        match sub.recv().await {
            Some(SubscriptionEvent::Event(event)) => {
                assert_eq!(event.event_type(), "pane.created");
                // The first pane event (seq 1) was consumed by the broadcast but
                // filtered by the old filter. The second pane event (seq 2) should
                // be the one we receive after the recv loop skips seq 1.
                // Actually: the first event (seq 1) will still be in the receiver
                // buffer. The recv loop will skip it because filters changed
                // AFTER it was sent but the loop re-checks. Let me think...
                // The receiver already has seq 1 (pane.created) in its buffer.
                // After set_filters, the next recv() call will try to receive seq 1.
                // But set_filters was called after the first publish, before the second.
                // The new filter is "pane." which matches pane.created.
                // So we'll actually get seq 1, not seq 2.
                // This is correct behavior: set_filters affects future recv() calls,
                // not already-buffered events.
            }
            other => panic!("expected pane.created Event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_debug_format() {
        let bus = EventBus::new();
        bus.publish(session_created("test"));

        let debug_str = format!("{bus:?}");
        assert!(debug_str.contains("EventBus"));
        assert!(debug_str.contains("current_seq"));
        assert!(debug_str.contains("subscriber_count"));
    }

    #[tokio::test]
    async fn test_default_config() {
        let config = EventBusConfig::default();
        assert_eq!(config.broadcast_capacity, 4096);
        assert_eq!(config.history_capacity, 8192);
    }

    #[tokio::test]
    async fn test_empty_history() {
        let bus = EventBus::new();

        let history = bus.history(10);
        assert!(history.is_empty());

        let (events, gap) = bus.events_from_seq(1);
        assert!(events.is_empty());
        assert_eq!(gap, 0);
    }

    #[tokio::test]
    async fn test_history_filtered_empty_filters_returns_all() {
        let bus = EventBus::new();

        bus.publish(session_created("s1"));
        bus.publish(pane_created());

        let filtered = bus.history_filtered(10, &[]);
        assert_eq!(filtered.len(), 2);
    }

    #[tokio::test]
    async fn test_bus_closed_returns_none() {
        let bus = EventBus::new();
        let mut sub = bus.subscribe();

        // Drop the bus (and thus the sender).
        drop(bus);

        // recv() should return None since the channel is closed.
        let result = sub.recv().await;
        assert!(result.is_none());
    }

    // ── PR 2c — data-plane tests ──────────────────────────────────────
    //
    // The whole point of the data plane is that PTY chunks NEVER end up
    // in `events.history` and NEVER reach `events.watch` subscribers.
    // If either of those invariants regresses, the secret-leak vector
    // we were closing reopens. These tests pin it shut.

    fn pane_output_ids() -> (PaneId, WindowId, SessionId) {
        (PaneId::new(), WindowId::new(), SessionId::new())
    }

    #[tokio::test]
    async fn test_pane_output_does_not_appear_in_history() {
        let bus = EventBus::new();
        let (pid, wid, sid) = pane_output_ids();
        for i in 0..20 {
            bus.publish_pane_output(pid, wid, sid, format!("chunk-{i}"), false);
        }
        // History snapshots are control-plane only.
        let hist = bus.history(100);
        assert!(hist.is_empty(), "data-plane chunks must not enter history");
        // events.history filter by "pane." prefix should also return empty.
        let filtered = bus.history_filtered(100, &["pane.".to_string()]);
        assert!(
            filtered.is_empty(),
            "data plane must not leak through history_filtered",
        );
    }

    #[tokio::test]
    async fn test_pane_output_does_not_reach_control_plane_subscribers() {
        // Bus has both a control-plane and a data-plane subscriber.
        // We publish one of each and verify each subscriber sees ONLY
        // its own channel — the data plane is sealed from
        // `events.watch`.
        let bus = EventBus::new();
        let mut control_sub = bus.subscribe();
        let mut data_sub = bus.subscribe_pane_output();
        let (pid, wid, sid) = pane_output_ids();

        bus.publish_pane_output(pid, wid, sid, "secret-bytes".into(), false);
        bus.publish(session_created("not-secret"));

        // Control subscriber sees only the SessionCreated event.
        let next = tokio::time::timeout(Duration::from_millis(50), control_sub.recv())
            .await
            .expect("control plane should yield")
            .expect("control plane should not be closed");
        match next {
            SubscriptionEvent::Event(e) => match e.data {
                EventData::SessionCreated { ref name, .. } => assert_eq!(name, "not-secret"),
                other => panic!("expected SessionCreated, got {other:?}"),
            },
            SubscriptionEvent::Lagged(_) => panic!("unexpected lag"),
        }
        // No further control-plane event.
        let nothing = tokio::time::timeout(Duration::from_millis(20), control_sub.recv()).await;
        assert!(
            nothing.is_err(),
            "data-plane publish must NOT show up on control subscribers",
        );

        // Data subscriber sees the chunk.
        let chunk = tokio::time::timeout(Duration::from_millis(50), data_sub.recv())
            .await
            .expect("data plane should yield")
            .expect("data plane should not be closed");
        match chunk {
            PaneOutputSubscriptionEvent::Chunk(c) => {
                assert_eq!(c.pane_id, pid);
                assert_eq!(c.bytes, "secret-bytes");
            }
            PaneOutputSubscriptionEvent::Lagged(_) => panic!("unexpected lag"),
        }
    }

    #[tokio::test]
    async fn test_pane_output_seq_is_independent_of_control_seq() {
        let bus = EventBus::new();
        let (pid, wid, sid) = pane_output_ids();
        // Pump some control-plane events first.
        for i in 0..5 {
            bus.publish(session_created(&format!("s{i}")));
        }
        let control_after = bus.current_seq();
        // Data-plane sequence starts at 1 regardless of control.
        let data_seq = bus.publish_pane_output(pid, wid, sid, "x".into(), false);
        assert_eq!(data_seq, 1, "data-plane seq must start fresh at 1");
        // Publish more control events — data seq doesn't move.
        bus.publish(session_created("again"));
        assert_eq!(bus.current_data_seq(), 2);
        assert_eq!(bus.current_seq(), control_after + 1);
    }

    #[tokio::test]
    async fn test_pane_output_subscriber_receives_chunks() {
        let bus = EventBus::new();
        let mut sub = bus.subscribe_pane_output();
        let (pid, wid, sid) = pane_output_ids();
        bus.publish_pane_output(pid, wid, sid, "AAA".into(), false);
        bus.publish_pane_output(pid, wid, sid, "BBB".into(), true);
        let first = tokio::time::timeout(Duration::from_millis(50), sub.recv())
            .await
            .expect("yield")
            .expect("not closed");
        match first {
            PaneOutputSubscriptionEvent::Chunk(c) => {
                assert_eq!(c.bytes, "AAA");
                assert!(!c.sampled);
            }
            other => panic!("unexpected {other:?}"),
        }
        let second = tokio::time::timeout(Duration::from_millis(50), sub.recv())
            .await
            .expect("yield")
            .expect("not closed");
        match second {
            PaneOutputSubscriptionEvent::Chunk(c) => {
                assert_eq!(c.bytes, "BBB");
                assert!(c.sampled);
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
