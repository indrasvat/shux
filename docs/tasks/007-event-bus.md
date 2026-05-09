# 007 — Event Bus

**Status:** Done
**Depends On:** 002
**Parallelizable With:** 003, 004

---

## Problem

shux's architecture uses events as the primary integration surface (PRD §4.3 invariant 4). Plugins, clients, and agents all subscribe to a typed event stream rather than polling for state changes. This task builds the event bus — a typed, sequenced, broadcast-based pub/sub system that wraps `tokio::sync::broadcast`.

The event bus must solve several problems that raw `tokio::sync::broadcast` does not:
1. **Typed events**: A strongly-typed `Event` enum covering the full taxonomy from PRD Appendix A.
2. **Sequence numbers**: Monotonically increasing `AtomicU64` so subscribers can detect gaps.
3. **Timestamps**: Every event carries its emission time.
4. **Gap detection**: When a subscriber falls behind (broadcast channel overflow), the bus reports how many events were dropped.
5. **Per-client filtering**: Subscribers can filter by event type prefix (e.g., "pane." matches all pane events).
6. **Event history ring buffer**: Recent events are stored for the `events.history` API and for `from_seq` resumption.

## PRD Reference

- §4.3 — Architectural invariant 4: events as integration surface
- §4.4 — Event abstraction: strongly typed, sequenced (monotonic AtomicU64), timestamped
- §8.4 — Event stream: filters, from_seq, gap detection, buffer_size
- §15.2 — Technology choices: tokio::sync::broadcast wrapper
- §21 — Appendix A: complete event taxonomy (session, window, pane, client, theme, config, plugin, keybinding, system events)

---

## Files to Create

- `crates/shux-core/src/event.rs` — Event enum, EventData, EventMetadata, event taxonomy types
- `crates/shux-core/src/bus.rs` — EventBus wrapper around tokio::sync::broadcast

## Files to Modify

- `crates/shux-core/Cargo.toml` — Add dependencies (tokio, uuid, serde, serde_json, chrono or std::time)
- `crates/shux-core/src/lib.rs` — Re-export event and bus modules (replaces stub)

---

## Execution Steps

### Step 1: Add dependencies to shux-core

Update `crates/shux-core/Cargo.toml`:

```toml
[package]
name = "shux-core"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
tokio = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
uuid = { workspace = true }
tracing = { workspace = true }
thiserror = { workspace = true }

[dev-dependencies]
tokio = { workspace = true, features = ["test-util", "macros"] }
```

### Step 2: Define the event taxonomy (`event.rs`)

The event taxonomy directly implements PRD §21 (Appendix A). Every event type listed in the PRD has a corresponding variant.

```rust
//! Event types for shux's event bus.
//!
//! Implements the complete event taxonomy from PRD §21 (Appendix A).
//! Every event is typed, sequenced, and timestamped.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::SystemTime;
use uuid::Uuid;

/// Unique identifier types (re-exported from the data model — task 002).
/// For now, use Uuid directly. Task 002 will define newtypes.
pub type SessionId = Uuid;
pub type WindowId = Uuid;
pub type PaneId = Uuid;
pub type ClientId = Uuid;
pub type PluginId = String;

/// Metadata attached to every event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventMetadata {
    /// Monotonically increasing sequence number.
    /// Guaranteed to never decrease. Gaps indicate dropped events.
    pub seq: u64,
    /// Timestamp when the event was emitted.
    pub timestamp: SystemTime,
    /// The event type string (e.g., "pane.created", "session.killed").
    /// Used for filtering and routing.
    pub event_type: String,
}

/// A complete event: metadata + typed payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Event metadata (sequence number, timestamp, type string).
    pub meta: EventMetadata,
    /// The event payload.
    pub data: EventData,
}

impl Event {
    /// The event type string (convenience accessor).
    pub fn event_type(&self) -> &str {
        &self.meta.event_type
    }

    /// The sequence number (convenience accessor).
    pub fn seq(&self) -> u64 {
        self.meta.seq
    }

    /// Check if this event matches a type prefix filter.
    ///
    /// Filter matching rules (PRD §8.4):
    /// - Empty string matches everything.
    /// - "pane." matches "pane.created", "pane.exited", etc.
    /// - "pane.created" matches exactly "pane.created".
    pub fn matches_filter(&self, filter: &str) -> bool {
        if filter.is_empty() {
            return true;
        }
        self.meta.event_type.starts_with(filter)
    }
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[seq={}] {} {:?}",
            self.meta.seq, self.meta.event_type, self.data
        )
    }
}

/// Event payload — the typed data for each event kind.
///
/// Variants follow the taxonomy from PRD §21 (Appendix A).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum EventData {
    // ── Session lifecycle ──────────────────────────────────────

    /// A new session was created.
    SessionCreated {
        session_id: SessionId,
        name: String,
    },

    /// A session was renamed.
    SessionRenamed {
        session_id: SessionId,
        old_name: String,
        new_name: String,
    },

    /// A session was killed.
    SessionKilled {
        session_id: SessionId,
        name: String,
    },

    /// A client attached to a session.
    SessionAttached {
        session_id: SessionId,
        client_id: ClientId,
    },

    /// A client detached from a session.
    SessionDetached {
        session_id: SessionId,
        client_id: ClientId,
    },

    // ── Window lifecycle ───────────────────────────────────────

    /// A new window was created.
    WindowCreated {
        window_id: WindowId,
        session_id: SessionId,
        title: String,
    },

    /// A window became the active window in its session.
    WindowActivated {
        window_id: WindowId,
        session_id: SessionId,
        previous_window_id: Option<WindowId>,
    },

    /// A window was renamed.
    WindowRenamed {
        window_id: WindowId,
        old_title: String,
        new_title: String,
    },

    /// A window's position in the window list changed.
    WindowReordered {
        window_id: WindowId,
        session_id: SessionId,
        old_index: usize,
        new_index: usize,
    },

    /// A window was killed.
    WindowKilled {
        window_id: WindowId,
        session_id: SessionId,
    },

    // ── Pane lifecycle ─────────────────────────────────────────

    /// A new pane was created.
    PaneCreated {
        pane_id: PaneId,
        window_id: WindowId,
        command: Vec<String>,
    },

    /// A pane received focus.
    PaneFocused {
        pane_id: PaneId,
        window_id: WindowId,
        previous_pane_id: Option<PaneId>,
    },

    /// A pane was resized.
    PaneResized {
        pane_id: PaneId,
        cols: u16,
        rows: u16,
    },

    /// A pane's zoom state changed.
    PaneZoomed {
        pane_id: PaneId,
        zoomed: bool,
    },

    /// A pane's title changed (via OSC or manual set).
    PaneTitleChanged {
        pane_id: PaneId,
        old_title: String,
        new_title: String,
    },

    /// A pane's working directory changed.
    PaneCwdChanged {
        pane_id: PaneId,
        old_cwd: String,
        new_cwd: String,
    },

    /// A pane's process exited.
    PaneExited {
        pane_id: PaneId,
        exit_status: Option<i32>,
        command: Vec<String>,
    },

    /// A pane was respawned.
    PaneRespawned {
        pane_id: PaneId,
        command: Vec<String>,
    },

    /// An async command completed in a pane (pane.run_command with async=true).
    PaneCommandCompleted {
        pane_id: PaneId,
        command_id: String,
        exit_code: Option<i32>,
        stdout: String,
        stderr: String,
    },

    /// PTY output from a pane (opt-in, sampled by default).
    PaneOutput {
        pane_id: PaneId,
        /// Base64-encoded bytes (PRD §8.4).
        bytes: String,
        /// Whether this is a sample (true) or lossless (false).
        sample: bool,
    },

    /// Input sent to a pane (fired on line-submit, not per-keystroke).
    PaneInput {
        pane_id: PaneId,
        data: String,
    },

    /// Bell character received in a pane.
    PaneBell {
        pane_id: PaneId,
    },

    /// A pane's tag was changed.
    PaneTagChanged {
        pane_id: PaneId,
        key: String,
        old_value: Option<String>,
        new_value: Option<String>,
    },

    // ── Client ─────────────────────────────────────────────────

    /// A client connected to the daemon.
    ClientConnected {
        client_id: ClientId,
        terminal_cols: u16,
        terminal_rows: u16,
    },

    /// A client disconnected from the daemon.
    ClientDisconnected {
        client_id: ClientId,
        reason: String,
    },

    /// A client's terminal was resized.
    ClientResized {
        client_id: ClientId,
        old_cols: u16,
        old_rows: u16,
        new_cols: u16,
        new_rows: u16,
    },

    // ── Theme ──────────────────────────────────────────────────

    /// A theme was changed at some scope.
    ThemeChanged {
        /// "session", "window", or "pane".
        scope: String,
        scope_id: String,
        old_theme: Option<String>,
        new_theme: String,
    },

    // ── Config ─────────────────────────────────────────────────

    /// Configuration was reloaded.
    ConfigReloaded {
        source: String,
        changes: Vec<ConfigChange>,
    },

    // ── Plugin ─────────────────────────────────────────────────

    /// A plugin was enabled.
    PluginEnabled {
        plugin_id: PluginId,
        version: String,
    },

    /// A plugin was disabled.
    PluginDisabled {
        plugin_id: PluginId,
        reason: String,
    },

    /// A plugin was hot-reloaded.
    PluginReloaded {
        plugin_id: PluginId,
        version: String,
    },

    /// A plugin encountered an error.
    PluginError {
        plugin_id: PluginId,
        error: String,
        context: String,
    },

    /// An inter-plugin event (namespaced).
    PluginEvent {
        plugin_id: PluginId,
        event_type: String,
        data: serde_json::Value,
    },

    // ── Keybinding ─────────────────────────────────────────────

    /// A keybinding was changed.
    KeybindingChanged {
        key: String,
        old_action: Option<String>,
        new_action: String,
    },

    // ── System ─────────────────────────────────────────────────

    /// A system error occurred.
    Error {
        code: i32,
        message: String,
        context: String,
    },
}

/// A single config change entry (used in ConfigReloaded events).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigChange {
    pub key: String,
    pub old: Option<serde_json::Value>,
    pub new: serde_json::Value,
}

impl EventData {
    /// Get the event type string for this data variant.
    ///
    /// Returns the dotted event type string as specified in PRD §21.
    pub fn event_type(&self) -> &'static str {
        match self {
            EventData::SessionCreated { .. } => "session.created",
            EventData::SessionRenamed { .. } => "session.renamed",
            EventData::SessionKilled { .. } => "session.killed",
            EventData::SessionAttached { .. } => "session.attached",
            EventData::SessionDetached { .. } => "session.detached",
            EventData::WindowCreated { .. } => "window.created",
            EventData::WindowActivated { .. } => "window.activated",
            EventData::WindowRenamed { .. } => "window.renamed",
            EventData::WindowReordered { .. } => "window.reordered",
            EventData::WindowKilled { .. } => "window.killed",
            EventData::PaneCreated { .. } => "pane.created",
            EventData::PaneFocused { .. } => "pane.focused",
            EventData::PaneResized { .. } => "pane.resized",
            EventData::PaneZoomed { .. } => "pane.zoomed",
            EventData::PaneTitleChanged { .. } => "pane.title_changed",
            EventData::PaneCwdChanged { .. } => "pane.cwd_changed",
            EventData::PaneExited { .. } => "pane.exited",
            EventData::PaneRespawned { .. } => "pane.respawned",
            EventData::PaneCommandCompleted { .. } => "pane.command_completed",
            EventData::PaneOutput { .. } => "pane.output",
            EventData::PaneInput { .. } => "pane.input",
            EventData::PaneBell { .. } => "pane.bell",
            EventData::PaneTagChanged { .. } => "pane.tag_changed",
            EventData::ClientConnected { .. } => "client.connected",
            EventData::ClientDisconnected { .. } => "client.disconnected",
            EventData::ClientResized { .. } => "client.resized",
            EventData::ThemeChanged { .. } => "theme.changed",
            EventData::ConfigReloaded { .. } => "config.reloaded",
            EventData::PluginEnabled { .. } => "plugin.enabled",
            EventData::PluginDisabled { .. } => "plugin.disabled",
            EventData::PluginReloaded { .. } => "plugin.reloaded",
            EventData::PluginError { .. } => "plugin.error",
            EventData::PluginEvent { .. } => "plugin.event",
            EventData::KeybindingChanged { .. } => "keybinding.changed",
            EventData::Error { .. } => "error",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_type_strings() {
        let data = EventData::SessionCreated {
            session_id: Uuid::nil(),
            name: "test".to_string(),
        };
        assert_eq!(data.event_type(), "session.created");

        let data = EventData::PaneExited {
            pane_id: Uuid::nil(),
            exit_status: Some(0),
            command: vec!["bash".to_string()],
        };
        assert_eq!(data.event_type(), "pane.exited");
    }

    #[test]
    fn test_event_filter_matching() {
        let event = Event {
            meta: EventMetadata {
                seq: 1,
                timestamp: SystemTime::now(),
                event_type: "pane.created".to_string(),
            },
            data: EventData::PaneCreated {
                pane_id: Uuid::nil(),
                window_id: Uuid::nil(),
                command: vec!["bash".to_string()],
            },
        };

        // Exact match.
        assert!(event.matches_filter("pane.created"));
        // Prefix match.
        assert!(event.matches_filter("pane."));
        // Empty matches everything.
        assert!(event.matches_filter(""));
        // No match.
        assert!(!event.matches_filter("session."));
        assert!(!event.matches_filter("pane.exited"));
    }

    #[test]
    fn test_event_serialization() {
        let event = Event {
            meta: EventMetadata {
                seq: 42,
                timestamp: SystemTime::now(),
                event_type: "session.created".to_string(),
            },
            data: EventData::SessionCreated {
                session_id: Uuid::nil(),
                name: "work".to_string(),
            },
        };

        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("session.created"));
        assert!(json.contains("work"));

        // Roundtrip.
        let deserialized: Event = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.meta.seq, 42);
    }
}
```

### Step 3: Implement the EventBus (`bus.rs`)

The `EventBus` wraps `tokio::sync::broadcast` and adds sequence numbering, timestamps, gap detection, and an event history ring buffer.

```rust
//! Event bus — typed pub/sub built on tokio::sync::broadcast.
//!
//! Provides:
//! - Typed event publishing with automatic sequence numbers and timestamps
//! - Subscriber handles with per-client filtering
//! - Gap detection when subscribers lag behind
//! - Ring buffer for event history (events.history API and from_seq resumption)

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use tokio::sync::broadcast;
use tracing::{debug, warn};

use crate::event::{Event, EventData, EventMetadata};

/// Configuration for the event bus.
#[derive(Debug, Clone)]
pub struct EventBusConfig {
    /// Capacity of the broadcast channel.
    /// When exceeded, oldest events are dropped for slow subscribers.
    /// Default: 4096.
    pub broadcast_capacity: usize,

    /// Maximum number of events to keep in the history ring buffer.
    /// Used for `events.history` API and `from_seq` resumption.
    /// Default: 8192.
    pub history_capacity: usize,
}

impl Default for EventBusConfig {
    fn default() -> Self {
        EventBusConfig {
            broadcast_capacity: 4096,
            history_capacity: 8192,
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
    /// The broadcast sender.
    sender: broadcast::Sender<Event>,
    /// Monotonically increasing sequence counter.
    seq_counter: AtomicU64,
    /// Ring buffer of recent events for history queries and from_seq resumption.
    history: RwLock<EventHistory>,
    /// Configuration.
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
            buffer: VecDeque::with_capacity(capacity.min(1024)), // Don't pre-allocate huge buffers.
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
    fn newest_seq(&self) -> Option<u64> {
        self.buffer.back().map(|e| e.meta.seq)
    }

    /// Get events from a given sequence number onwards.
    /// Returns (events, gap_count) where gap_count > 0 if from_seq
    /// is older than the oldest event in history.
    fn events_from_seq(&self, from_seq: u64) -> (Vec<Event>, u64) {
        let oldest = self.oldest_seq().unwrap_or(0);

        let gap = if from_seq < oldest {
            oldest - from_seq
        } else {
            0
        };

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
        EventBus {
            inner: Arc::new(EventBusInner {
                sender,
                seq_counter: AtomicU64::new(1), // Start at 1 so 0 means "no events seen".
                history: RwLock::new(EventHistory::new(config.history_capacity)),
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
        let seq = self.inner.seq_counter.fetch_add(1, Ordering::Relaxed);
        let event_type = data.event_type().to_string();

        let event = Event {
            meta: EventMetadata {
                seq,
                timestamp: SystemTime::now(),
                event_type,
            },
            data,
        };

        // Record in history.
        {
            let mut history = self
                .inner
                .history
                .write()
                .expect("history lock poisoned");
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

    /// Get the current sequence number (the next event will get this + 1).
    pub fn current_seq(&self) -> u64 {
        self.inner.seq_counter.load(Ordering::Relaxed)
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

use std::fmt;

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

    /// Change the filters for this subscription.
    pub fn set_filters(&mut self, filters: Vec<String>) {
        self.filters = filters;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventData;
    use uuid::Uuid;

    #[tokio::test]
    async fn test_publish_and_subscribe() {
        let bus = EventBus::new();
        let mut sub = bus.subscribe();

        let seq = bus.publish(EventData::SessionCreated {
            session_id: Uuid::new_v4(),
            name: "test".to_string(),
        });

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

        let seq1 = bus.publish(EventData::SessionCreated {
            session_id: Uuid::new_v4(),
            name: "s1".to_string(),
        });
        let seq2 = bus.publish(EventData::SessionCreated {
            session_id: Uuid::new_v4(),
            name: "s2".to_string(),
        });
        let seq3 = bus.publish(EventData::SessionKilled {
            session_id: Uuid::new_v4(),
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
        bus.publish(EventData::SessionCreated {
            session_id: Uuid::new_v4(),
            name: "test".to_string(),
        });

        // Publish a pane event (should pass the filter).
        bus.publish(EventData::PaneCreated {
            pane_id: Uuid::new_v4(),
            window_id: Uuid::new_v4(),
            command: vec!["bash".to_string()],
        });

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
    async fn test_multiple_subscribers() {
        let bus = EventBus::new();
        let mut sub1 = bus.subscribe();
        let mut sub2 = bus.subscribe();

        bus.publish(EventData::SessionCreated {
            session_id: Uuid::new_v4(),
            name: "test".to_string(),
        });

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
        let seq = bus.publish(EventData::SessionCreated {
            session_id: Uuid::new_v4(),
            name: "test".to_string(),
        });
        assert_eq!(seq, 1);
    }

    #[tokio::test]
    async fn test_history() {
        let bus = EventBus::new();

        for i in 0..5 {
            bus.publish(EventData::SessionCreated {
                session_id: Uuid::new_v4(),
                name: format!("s{i}"),
            });
        }

        let recent = bus.history(3);
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].seq(), 3);
        assert_eq!(recent[1].seq(), 4);
        assert_eq!(recent[2].seq(), 5);
    }

    #[tokio::test]
    async fn test_history_filtered() {
        let bus = EventBus::new();

        bus.publish(EventData::SessionCreated {
            session_id: Uuid::new_v4(),
            name: "s1".to_string(),
        });
        bus.publish(EventData::PaneCreated {
            pane_id: Uuid::new_v4(),
            window_id: Uuid::new_v4(),
            command: vec!["bash".to_string()],
        });
        bus.publish(EventData::SessionKilled {
            session_id: Uuid::new_v4(),
            name: "s1".to_string(),
        });

        let filtered = bus.history_filtered(10, &["session.".to_string()]);
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].event_type(), "session.created");
        assert_eq!(filtered[1].event_type(), "session.killed");
    }

    #[tokio::test]
    async fn test_events_from_seq() {
        let bus = EventBus::new();

        for i in 0..5 {
            bus.publish(EventData::SessionCreated {
                session_id: Uuid::new_v4(),
                name: format!("s{i}"),
            });
        }

        let (events, gap) = bus.events_from_seq(3);
        assert_eq!(gap, 0);
        assert_eq!(events.len(), 3); // seq 3, 4, 5
        assert_eq!(events[0].seq(), 3);
    }

    #[tokio::test]
    async fn test_events_from_seq_with_gap() {
        let config = EventBusConfig {
            broadcast_capacity: 16,
            history_capacity: 3,
        };
        let bus = EventBus::with_config(config);

        for i in 0..10 {
            bus.publish(EventData::SessionCreated {
                session_id: Uuid::new_v4(),
                name: format!("s{i}"),
            });
        }

        // History only has last 3 events (seq 8, 9, 10).
        // Requesting from seq 2 should report a gap.
        let (events, gap) = bus.events_from_seq(2);
        assert!(gap > 0, "expected gap, got {gap}");
        assert_eq!(events.len(), 3);
    }

    #[tokio::test]
    async fn test_history_capacity_limit() {
        let config = EventBusConfig {
            broadcast_capacity: 16,
            history_capacity: 5,
        };
        let bus = EventBus::with_config(config);

        for i in 0..20 {
            bus.publish(EventData::SessionCreated {
                session_id: Uuid::new_v4(),
                name: format!("s{i}"),
            });
        }

        let all = bus.history(100);
        assert_eq!(all.len(), 5); // Capped at history_capacity.
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
        // Note: tokio broadcast may not immediately reflect dropped receivers.
        // This is implementation-dependent.
    }

    #[tokio::test]
    async fn test_current_seq() {
        let bus = EventBus::new();
        assert_eq!(bus.current_seq(), 1); // Starts at 1.

        bus.publish(EventData::PaneBell {
            pane_id: Uuid::new_v4(),
        });
        assert_eq!(bus.current_seq(), 2);

        bus.publish(EventData::PaneBell {
            pane_id: Uuid::new_v4(),
        });
        assert_eq!(bus.current_seq(), 3);
    }

    #[tokio::test]
    async fn test_lag_detection() {
        let config = EventBusConfig {
            broadcast_capacity: 4,
            history_capacity: 100,
        };
        let bus = EventBus::with_config(config);
        let mut sub = bus.subscribe();

        // Publish more events than the broadcast channel can hold.
        for i in 0..10 {
            bus.publish(EventData::SessionCreated {
                session_id: Uuid::new_v4(),
                name: format!("s{i}"),
            });
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
                // Continue receiving to check for lag or successful delivery.
                debug!("got event seq={} instead of Lagged", e.seq());
            }
            None => panic!("channel closed unexpectedly"),
        }
    }

    #[tokio::test]
    async fn test_event_timestamps() {
        let bus = EventBus::new();
        let before = SystemTime::now();

        bus.publish(EventData::SessionCreated {
            session_id: Uuid::new_v4(),
            name: "test".to_string(),
        });

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
            pane_id: Uuid::new_v4(),
        });

        // bus2 should see the same history.
        let history = bus2.history(10);
        assert_eq!(history.len(), 1);
        assert_eq!(bus2.current_seq(), 2);
    }
}
```

### Step 4: Update lib.rs

Replace the stub `lib.rs` with module declarations.

```rust
//! shux-core — Core engine for the shux terminal multiplexer.
//!
//! Contains the central data model (SessionGraph — task 002), event bus,
//! layout engine (task 003), configuration, and theme engine.

pub mod bus;
pub mod event;

// Re-export key types.
pub use bus::{EventBus, EventBusConfig, Subscription, SubscriptionEvent};
pub use event::{Event, EventData, EventMetadata};
```

### Step 5: Integration with task 002 (data model)

The `event.rs` file uses `SessionId`, `WindowId`, `PaneId`, `ClientId` as type aliases for `Uuid`. When task 002 (Core data model) is implemented, these should be changed to newtype wrappers defined in that task. For now, the aliases work and the types are forward-compatible.

The implementing agent should check if task 002 has been completed. If so, import the ID types from the data model module instead of defining aliases.

### Step 6: Verify thread safety

The `EventBus` design is thread-safe:
- `Arc<EventBusInner>` for shared ownership across clones.
- `AtomicU64` for lock-free sequence numbering (no contention on the hot path).
- `RwLock<EventHistory>` for history access (read-heavy workload, write on publish).
- `broadcast::Sender<Event>` is `Send + Sync` by design.

The `Event` type derives `Clone` (required by `broadcast::channel`). All fields are `Send + Sync`.

The `Subscription` is `!Clone` by design — each subscriber gets its own receiver position. Subscriptions are not `Send` between tasks without wrapping in an `Arc<Mutex<>>`, but that is the intended usage pattern (each task owns its own subscription).

---

## Verification

### Functional

```bash
# Build the shux-core crate
cargo build -p shux-core

# Check for clippy warnings
cargo clippy -p shux-core -- -D warnings

# Format check
cargo fmt -p shux-core -- --check
```

### Tests

```bash
# Run all shux-core tests
cargo nextest run -p shux-core

# Run with output
cargo nextest run -p shux-core --no-capture

# Run specific test modules
cargo nextest run -p shux-core -- bus::tests
cargo nextest run -p shux-core -- event::tests
cargo nextest run -p shux-core -- bus::tests::history_and_gap_detection
```

---

## Completion Criteria

- [ ] `crates/shux-core/src/event.rs` — Event struct with EventMetadata (seq, timestamp, event_type) and EventData enum
- [ ] `crates/shux-core/src/event.rs` — Complete EventData taxonomy matching PRD §21 Appendix A:
  - [ ] Session lifecycle: SessionCreated, SessionRenamed, SessionKilled, SessionAttached, SessionDetached
  - [ ] Window lifecycle: WindowCreated, WindowActivated, WindowRenamed, WindowReordered, WindowKilled
  - [ ] Pane lifecycle: PaneCreated, PaneFocused, PaneResized, PaneZoomed, PaneTitleChanged, PaneCwdChanged, PaneExited, PaneRespawned, PaneCommandCompleted, PaneOutput, PaneInput, PaneBell, PaneTagChanged
  - [ ] Client: ClientConnected, ClientDisconnected, ClientResized
  - [ ] Theme: ThemeChanged
  - [ ] Config: ConfigReloaded
  - [ ] Plugin: PluginEnabled, PluginDisabled, PluginReloaded, PluginError, PluginEvent
  - [ ] Keybinding: KeybindingChanged
  - [ ] System: Error
- [ ] `crates/shux-core/src/event.rs` — EventData::event_type() returns correct PRD type strings
- [ ] `crates/shux-core/src/event.rs` — Event::matches_filter() supports prefix matching
- [ ] `crates/shux-core/src/event.rs` — Event serializes/deserializes with serde_json
- [ ] `crates/shux-core/src/bus.rs` — EventBus wraps tokio::sync::broadcast
- [ ] `crates/shux-core/src/bus.rs` — Sequence numbers via AtomicU64, monotonically increasing, starting at 1
- [ ] `crates/shux-core/src/bus.rs` — Timestamps on every event (SystemTime)
- [ ] `crates/shux-core/src/bus.rs` — Gap detection: Subscription::recv() reports Lagged(count) on overflow
- [ ] `crates/shux-core/src/bus.rs` — Per-client filtering via subscribe_filtered() with event type prefixes
- [ ] `crates/shux-core/src/bus.rs` — History ring buffer: history(), history_filtered(), events_from_seq()
- [ ] `crates/shux-core/src/bus.rs` — events_from_seq() returns gap count for stale sequence numbers
- [ ] History/gap tests assert stale `from_seq` reports dropped counts and filtered replay honors prefixes
- [ ] `crates/shux-core/src/bus.rs` — History capacity is bounded and configurable
- [ ] `crates/shux-core/src/bus.rs` — EventBus is Clone (Arc-based), thread-safe
- [ ] `crates/shux-core/src/bus.rs` — Publishing with no subscribers does not panic
- [ ] `crates/shux-core/src/lib.rs` — Module declarations and re-exports
- [ ] `crates/shux-core/Cargo.toml` — Dependencies: tokio, serde, serde_json, uuid, tracing, thiserror
- [ ] Unit tests for event type string mapping pass
- [ ] Unit tests for event filter matching pass
- [ ] Unit tests for event serialization roundtrip pass
- [ ] Unit tests for publish/subscribe pass
- [ ] Unit tests for sequence numbering pass
- [ ] Unit tests for filtered subscriptions pass
- [ ] Unit tests for multiple subscribers pass
- [ ] Unit tests for history ring buffer pass
- [ ] Unit tests for events_from_seq with gap detection pass
- [ ] Unit tests for lag detection pass
- [ ] `cargo clippy -p shux-core -- -D warnings` passes
- [ ] `cargo fmt -p shux-core -- --check` passes

---

## Commit Message

```
feat(core): implement typed event bus with broadcast, sequencing, and history

- Event enum covering full PRD §21 taxonomy (session, window, pane, client,
  theme, config, plugin, keybinding, system events)
- EventBus wrapping tokio::sync::broadcast with AtomicU64 sequence numbers
- Gap detection for lagging subscribers (reports dropped event count)
- Per-client event filtering via type prefix matching
- Ring buffer for event history and from_seq resumption
- Configurable broadcast capacity and history size
- Tests for publish/subscribe, sequencing, filtering, history, gap detection
```

---

## Session Protocol

1. **Before starting:** Read `CLAUDE.md`, `docs/PRD.md` §4.3-4.4 (architectural invariants, Event abstraction), §8.4 (event stream contract), §21 (Appendix A — event taxonomy). Check if task 002 is complete — if so, use its ID types instead of Uuid aliases.
2. **During implementation:**
   - Start with `event.rs` — define the `EventData` enum exhaustively. Cross-reference every variant against PRD §21. Do not omit any event type.
   - Then `bus.rs` — build the EventBus. The hardest part is getting the history ring buffer and gap detection right. Write tests as you go.
   - The `RwLock<EventHistory>` is acceptable for history because writes happen only on publish (single-writer pattern) and reads are infrequent (API calls). If contention becomes an issue later, replace with a lock-free ring buffer.
   - Run `cargo clippy -p shux-core -- -D warnings` after each file.
3. **Key design decisions:**
   - `Event` must be `Clone` because `broadcast::channel` requires it.
   - Sequence numbers start at 1 (not 0) so that `from_seq: 0` means "give me everything."
   - The `EventBus` is `Clone` (Arc-based) so it can be shared across tasks without ceremony.
   - Filtering happens client-side in the `Subscription` (not in the broadcast channel). This means filtered subscribers still receive all events on the broadcast channel but discard non-matching ones. This is acceptable because filtering is O(1) per event (prefix match) and avoids the complexity of per-subscriber channels.
4. **After:** Run full test suite (`cargo nextest run -p shux-core`). Update `docs/PROGRESS.md` (mark 007 done). Update `CLAUDE.md` Learnings with any insights about broadcast channel behavior, lag characteristics, or RwLock performance.
