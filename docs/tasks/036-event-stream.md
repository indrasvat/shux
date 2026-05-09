# 036 — Event Stream (events.watch)

**Status:** Pending
**Depends On:** 035, 007
**Parallelizable With:** 037

---

## Problem

The event stream is shux's real-time integration surface. Agents, plugins, and TUI clients all need to subscribe to typed, sequenced events to react to changes without polling. PRD section 4.3 (invariant 4) states: "Plugins, clients, and agents all subscribe to the same typed event stream via `tokio::sync::broadcast`." Without `events.watch`, agents are blind to state changes and must poll `state.snapshot` repeatedly -- which is wasteful, slow, and defeats the event-driven architecture.

The event stream has several non-trivial requirements:
- **Filter grammar**: Prefix-based event type matching (`"pane."` matches all pane events)
- **Resumability**: Clients can reconnect with `from_seq` to replay missed events from a ring buffer
- **Gap detection**: When `from_seq` is too old, the server sends gap notifications rather than silently skipping
- **Per-client buffering**: Each subscriber has a bounded send buffer; overflow drops oldest events with gap notification
- **Binary encoding**: `pane.output` events encode PTY output as base64 with a `sample` indicator
- **Long-lived connections**: The stream is held open over a UDS connection, sending JSON-RPC notifications indefinitely

This task also upgrades the core event bus (task 007) with a ring buffer for history replay and structured event sequencing.

## PRD Reference

- **section 8.4** — Event stream: `events.watch` request/response, filter grammar, `from_seq` resume, `buffer_size`, gap notifications, `pane.output` binary encoding
- **section 21** (Appendix A) — Complete event taxonomy (session, window, pane, client, theme, config, plugin, keybinding, system events)
- **section 4.3** invariant 4 — Events are the integration surface; typed, sequenced, via `tokio::sync::broadcast`
- **section 4.4** — `Event`: strongly typed, sequenced (monotonic `AtomicU64`), timestamped
- **section 8.6** — CLI mapping: `shux events watch [--filter pane.output]` outputs JSON lines

---

## Files to Create

- `crates/shux-rpc/src/stream.rs` — `events.watch` handler: filter parsing, client subscription management, JSON-RPC notification framing, gap detection
- `crates/shux-core/src/ring_buffer.rs` — Bounded ring buffer for event history replay (supports `from_seq` and `events.history`)
- `crates/shux-core/src/event_filter.rs` — Event filter grammar: prefix matching, wildcard support

## Files to Modify

- `crates/shux-core/src/bus.rs` — Add ring buffer storage, sequence assignment, structured `Event` type
- `crates/shux-core/src/lib.rs` — Export new modules
- `crates/shux-rpc/src/lib.rs` — Wire stream handler into the server connection loop
- `crates/shux-rpc/src/methods/events.rs` — `events.history` implementation using ring buffer

---

## Execution Steps

### Step 1: Define the canonical Event type

Every event in shux is typed, sequenced, and timestamped. Define the shared event structure.

```rust
// crates/shux-core/src/event.rs

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Global monotonic sequence counter for events.
static EVENT_SEQ: AtomicU64 = AtomicU64::new(1);

/// Generate the next sequence number. Guaranteed monotonically increasing.
pub fn next_seq() -> u64 {
    EVENT_SEQ.fetch_add(1, Ordering::SeqCst)
}

/// A strongly typed, sequenced, timestamped event.
///
/// This is the canonical event representation used throughout shux:
/// on the broadcast bus, in the ring buffer, and in JSON-RPC notifications.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Monotonically increasing sequence number. Never reused.
    pub seq: u64,
    /// ISO 8601 timestamp (e.g., "2026-02-18T10:30:00.123Z").
    pub ts: String,
    /// Event type using dot-separated taxonomy (e.g., "pane.created").
    #[serde(rename = "type")]
    pub event_type: String,
    /// Event-specific data. Structure depends on event type.
    pub data: Value,
}

impl Event {
    /// Create a new event with the next sequence number and current timestamp.
    pub fn new(event_type: impl Into<String>, data: Value) -> Self {
        Self {
            seq: next_seq(),
            ts: iso8601_now(),
            event_type: event_type.into(),
            data,
        }
    }

    /// Check if this event matches a prefix filter.
    ///
    /// Filter rules (PRD section 8.4):
    /// - `"pane."` matches all events starting with "pane." (pane.created, pane.exited, etc.)
    /// - `"pane.created"` matches only that exact type
    /// - `""` (empty) matches all events
    pub fn matches_filter(&self, filter: &str) -> bool {
        if filter.is_empty() {
            return true;
        }
        if filter.ends_with('.') {
            // Prefix match: "pane." matches "pane.created", "pane.exited", etc.
            self.event_type.starts_with(filter)
        } else {
            // Exact match
            self.event_type == filter
        }
    }

    /// Check if this event matches any of the given filters.
    /// Empty filter list means "all events".
    pub fn matches_any_filter(&self, filters: &[String]) -> bool {
        if filters.is_empty() {
            return true;
        }
        filters.iter().any(|f| self.matches_filter(f))
    }
}

fn iso8601_now() -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    // Format as ISO 8601 with millisecond precision
    let secs = now.as_secs();
    let millis = now.subsec_millis();
    // Use chrono or time crate for proper formatting; this is a
    // simplified version for bootstrapping
    format!(
        "{}.{:03}Z",
        chrono::DateTime::from_timestamp(secs as i64, 0)
            .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S").to_string())
            .unwrap_or_else(|| "1970-01-01T00:00:00".to_string()),
        millis
    )
}
```

### Step 2: Implement the ring buffer for event history

The ring buffer stores recent events for replay. It supports `from_seq` queries (replay events after a given sequence number) and bounded capacity with oldest-event eviction.

```rust
// crates/shux-core/src/ring_buffer.rs

use std::collections::VecDeque;
use std::sync::{Arc, RwLock};

use crate::event::Event;

/// Default ring buffer capacity (number of events).
const DEFAULT_CAPACITY: usize = 65_536;

/// A bounded ring buffer that stores recent events for replay.
///
/// Used by:
/// - `events.watch` with `from_seq` to replay missed events
/// - `events.history` to query recent events
///
/// Thread-safe via RwLock: writers (event bus) take write lock,
/// readers (client replay) take read lock.
pub struct EventRingBuffer {
    inner: RwLock<RingBufferInner>,
}

struct RingBufferInner {
    buffer: VecDeque<Event>,
    capacity: usize,
    /// The lowest sequence number currently in the buffer.
    /// Used for gap detection: if `from_seq < min_seq`, the client
    /// has missed events that have been evicted.
    min_seq: u64,
}

/// Result of querying events from the ring buffer.
#[derive(Debug)]
pub struct ReplayResult {
    /// Events matching the query, in sequence order.
    pub events: Vec<Event>,
    /// If the requested `from_seq` was too old (events were evicted),
    /// this contains the gap information.
    pub gap: Option<GapInfo>,
}

/// Information about missed events due to ring buffer eviction.
#[derive(Debug, Clone)]
pub struct GapInfo {
    /// First sequence number that was requested but missing.
    pub from: u64,
    /// First sequence number available in the buffer.
    pub to: u64,
    /// Number of events lost.
    pub lost: u64,
}

impl EventRingBuffer {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: RwLock::new(RingBufferInner {
                buffer: VecDeque::with_capacity(capacity),
                capacity,
                min_seq: 0,
            }),
        }
    }

    /// Push a new event into the ring buffer.
    /// If the buffer is full, the oldest event is evicted.
    pub fn push(&self, event: Event) {
        let mut inner = self.inner.write().expect("ring buffer write lock poisoned");
        if inner.buffer.len() >= inner.capacity {
            if let Some(evicted) = inner.buffer.pop_front() {
                // Update min_seq to the next event's sequence
                inner.min_seq = inner
                    .buffer
                    .front()
                    .map(|e| e.seq)
                    .unwrap_or(evicted.seq + 1);
            }
        }
        inner.buffer.push_back(event);
    }

    /// Query events starting from `from_seq` (exclusive).
    ///
    /// Returns events with `seq > from_seq` that match the given filters.
    /// If `from_seq` is too old (events have been evicted), a gap
    /// notification is included in the result.
    ///
    /// `limit` caps the number of returned events.
    pub fn replay(
        &self,
        from_seq: u64,
        filters: &[String],
        limit: usize,
    ) -> ReplayResult {
        let inner = self.inner.read().expect("ring buffer read lock poisoned");

        let gap = if from_seq > 0 && inner.min_seq > 0 && from_seq < inner.min_seq {
            Some(GapInfo {
                from: from_seq,
                to: inner.min_seq,
                lost: inner.min_seq - from_seq,
            })
        } else {
            None
        };

        let events: Vec<Event> = inner
            .buffer
            .iter()
            .filter(|e| e.seq > from_seq)
            .filter(|e| e.matches_any_filter(filters))
            .take(limit)
            .cloned()
            .collect();

        ReplayResult { events, gap }
    }

    /// Query recent events matching filters, up to `limit`.
    /// Used by `events.history`.
    pub fn recent(&self, filters: &[String], limit: usize) -> Vec<Event> {
        let inner = self.inner.read().expect("ring buffer read lock poisoned");

        inner
            .buffer
            .iter()
            .rev()
            .filter(|e| e.matches_any_filter(filters))
            .take(limit)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    /// Current number of events in the buffer.
    pub fn len(&self) -> usize {
        let inner = self.inner.read().expect("ring buffer read lock poisoned");
        inner.buffer.len()
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The current minimum and maximum sequence numbers in the buffer.
    pub fn seq_range(&self) -> Option<(u64, u64)> {
        let inner = self.inner.read().expect("ring buffer read lock poisoned");
        let min = inner.buffer.front().map(|e| e.seq)?;
        let max = inner.buffer.back().map(|e| e.seq)?;
        Some((min, max))
    }
}

impl Default for EventRingBuffer {
    fn default() -> Self {
        Self::new()
    }
}
```

### Step 3: Implement the event stream handler

This is the core of `events.watch`. It handles:
1. Initial handshake (parse filter and from_seq)
2. Replay from ring buffer if from_seq is provided
3. Subscribe to the broadcast channel for live events
4. Send filtered events as JSON-RPC notifications
5. Detect and report gaps from overflow

```rust
// crates/shux-rpc/src/stream.rs

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::sync::broadcast;

use shux_core::event::Event;
use shux_core::ring_buffer::{EventRingBuffer, GapInfo};

/// Default per-client send buffer size.
const DEFAULT_BUFFER_SIZE: usize = 1024;

/// Maximum chunk size for pane.output binary data (64 KB).
const MAX_OUTPUT_CHUNK: usize = 64 * 1024;

/// Parameters for the `events.watch` request.
#[derive(Debug, Deserialize)]
pub struct WatchParams {
    /// Event type prefix filters. Empty = all events.
    #[serde(default)]
    pub filters: Vec<String>,
    /// Resume from this sequence number (exclusive).
    /// Events with seq > from_seq are replayed from the ring buffer.
    #[serde(default)]
    pub from_seq: u64,
    /// Per-client send buffer size. Default 1024.
    #[serde(default = "default_buffer_size")]
    pub buffer_size: usize,
}

fn default_buffer_size() -> usize {
    DEFAULT_BUFFER_SIZE
}

/// A JSON-RPC notification sent on the event stream.
#[derive(Debug, Serialize)]
struct EventNotification {
    jsonrpc: &'static str,
    method: &'static str,
    params: Event,
}

/// A gap notification sent when events were dropped.
#[derive(Debug, Serialize)]
struct GapNotification {
    jsonrpc: &'static str,
    method: &'static str,
    params: GapNotificationParams,
}

#[derive(Debug, Serialize)]
struct GapNotificationParams {
    from: u64,
    to: u64,
    lost: u64,
}

/// Handle an `events.watch` request on a held connection.
///
/// This function takes ownership of the write half of the connection
/// and streams events indefinitely until the connection is closed or
/// the shutdown token is cancelled.
///
/// Flow:
/// 1. Parse watch params
/// 2. Replay from ring buffer (if from_seq > 0)
/// 3. Subscribe to broadcast channel
/// 4. Stream filtered events as JSON-RPC notifications
pub async fn handle_watch<W: AsyncWriteExt + Unpin>(
    params: WatchParams,
    ring_buffer: Arc<EventRingBuffer>,
    mut event_rx: broadcast::Receiver<Event>,
    mut writer: W,
    shutdown: tokio_util::sync::CancellationToken,
) -> Result<(), StreamError> {
    // Phase 1: Replay from ring buffer
    let replay = ring_buffer.replay(
        params.from_seq,
        &params.filters,
        params.buffer_size,
    );

    // Send gap notification if events were missed
    if let Some(gap) = replay.gap {
        send_gap_notification(&mut writer, &gap).await?;
    }

    // Send replayed events
    for event in &replay.events {
        send_event_notification(&mut writer, event).await?;
    }

    // Track the last sent sequence number for gap detection
    let mut last_sent_seq = replay
        .events
        .last()
        .map(|e| e.seq)
        .unwrap_or(params.from_seq);

    // Phase 2: Stream live events from broadcast channel
    loop {
        tokio::select! {
            result = event_rx.recv() => {
                match result {
                    Ok(event) => {
                        // Check for gaps (broadcast channel overflow)
                        if event.seq > last_sent_seq + 1 && last_sent_seq > 0 {
                            let gap = GapInfo {
                                from: last_sent_seq + 1,
                                to: event.seq,
                                lost: event.seq - last_sent_seq - 1,
                            };
                            send_gap_notification(&mut writer, &gap).await?;
                        }

                        // Apply filters
                        if event.matches_any_filter(&params.filters) {
                            send_event_notification(&mut writer, &event).await?;
                        }

                        last_sent_seq = event.seq;
                    }
                    Err(broadcast::error::RecvError::Lagged(count)) => {
                        // Broadcast buffer overflow — some events were dropped
                        let gap = GapInfo {
                            from: last_sent_seq + 1,
                            to: last_sent_seq + count + 1,
                            lost: count,
                        };
                        send_gap_notification(&mut writer, &gap).await?;
                        last_sent_seq += count;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        // Event bus shut down
                        break;
                    }
                }
            }
            _ = shutdown.cancelled() => {
                break;
            }
        }
    }

    Ok(())
}

/// Send a single event as a JSON-RPC notification.
async fn send_event_notification<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    event: &Event,
) -> Result<(), StreamError> {
    let notification = EventNotification {
        jsonrpc: "2.0",
        method: "event",
        params: event.clone(),
    };

    let json = serde_json::to_vec(&notification)
        .map_err(StreamError::Serialization)?;

    // Length-prefixed frame (4-byte big-endian + JSON payload)
    let len = json.len() as u32;
    writer.write_all(&len.to_be_bytes()).await.map_err(StreamError::Io)?;
    writer.write_all(&json).await.map_err(StreamError::Io)?;
    writer.flush().await.map_err(StreamError::Io)?;

    Ok(())
}

/// Send a gap notification.
async fn send_gap_notification<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    gap: &GapInfo,
) -> Result<(), StreamError> {
    let notification = GapNotification {
        jsonrpc: "2.0",
        method: "event.gap",
        params: GapNotificationParams {
            from: gap.from,
            to: gap.to,
            lost: gap.lost,
        },
    };

    let json = serde_json::to_vec(&notification)
        .map_err(StreamError::Serialization)?;

    let len = json.len() as u32;
    writer.write_all(&len.to_be_bytes()).await.map_err(StreamError::Io)?;
    writer.write_all(&json).await.map_err(StreamError::Io)?;
    writer.flush().await.map_err(StreamError::Io)?;

    Ok(())
}

/// Encode pane output for the `pane.output` event.
///
/// PRD section 8.4: data.bytes uses base64 encoding for raw PTY output.
/// data.sample indicates whether output was sampled/coalesced.
/// Max chunk size: 64 KB per event.
pub fn encode_pane_output(raw_bytes: &[u8], sampled: bool) -> Value {
    use base64::Engine;
    let encoder = base64::engine::general_purpose::STANDARD;

    let truncated = if raw_bytes.len() > MAX_OUTPUT_CHUNK {
        &raw_bytes[..MAX_OUTPUT_CHUNK]
    } else {
        raw_bytes
    };

    serde_json::json!({
        "bytes": encoder.encode(truncated),
        "sample": sampled,
        "truncated": raw_bytes.len() > MAX_OUTPUT_CHUNK
    })
}

#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    #[error("I/O error: {0}")]
    Io(std::io::Error),
    #[error("serialization error: {0}")]
    Serialization(serde_json::Error),
}
```

### Step 4: Implement `events.history` using the ring buffer

```rust
// crates/shux-rpc/src/methods/events.rs

use std::sync::Arc;
use serde::Deserialize;
use serde_json::Value;

use super::{MethodContext, MethodResult, MethodError};

/// `events.history` — Query recent events from the bounded ring buffer.
///
/// Params:
///   filters: Vec<String> — event type prefix filters (empty = all)
///   limit: Option<u32> — max events to return (default 100, max 10000)
///   from_seq: Option<u64> — return events after this sequence number
pub async fn handle_history(ctx: Arc<MethodContext>, params: Value) -> MethodResult {
    #[derive(Deserialize)]
    struct Params {
        #[serde(default)]
        filters: Vec<String>,
        #[serde(default = "default_limit")]
        limit: u32,
        #[serde(default)]
        from_seq: Option<u64>,
    }

    fn default_limit() -> u32 {
        100
    }

    let params: Params = serde_json::from_value(params)
        .map_err(|e| MethodError::invalid_params(e.to_string()))?;

    let limit = (params.limit as usize).min(10_000);

    // TODO: Query ring buffer from ctx.ring_buffer
    // let events = match params.from_seq {
    //     Some(seq) => {
    //         let replay = ctx.ring_buffer.replay(seq, &params.filters, limit);
    //         // Include gap info if applicable
    //         replay.events
    //     }
    //     None => ctx.ring_buffer.recent(&params.filters, limit),
    // };

    let events: Vec<Value> = vec![];

    Ok(serde_json::json!({
        "events": events,
        "count": events.len()
    }))
}
```

### Step 5: Modify the event bus to integrate ring buffer and sequencing

Update `crates/shux-core/src/bus.rs` to:
1. Assign sequence numbers to every event before broadcast
2. Store events in the ring buffer after broadcast
3. Expose the ring buffer for replay queries

```rust
// Modifications to crates/shux-core/src/bus.rs

use std::sync::Arc;
use tokio::sync::broadcast;

use crate::event::Event;
use crate::ring_buffer::EventRingBuffer;

/// The core event bus.
///
/// All events flow through here. The bus assigns sequence numbers,
/// broadcasts to subscribers, and stores events in the ring buffer
/// for replay.
pub struct EventBus {
    /// Broadcast sender for live event distribution.
    tx: broadcast::Sender<Event>,
    /// Ring buffer for event history and replay.
    ring_buffer: Arc<EventRingBuffer>,
}

impl EventBus {
    /// Create a new event bus with the given broadcast capacity.
    pub fn new(broadcast_capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(broadcast_capacity);
        Self {
            tx,
            ring_buffer: Arc::new(EventRingBuffer::new()),
        }
    }

    /// Publish an event. The event gets a sequence number assigned,
    /// is broadcast to all subscribers, and stored in the ring buffer.
    pub fn publish(&self, event_type: impl Into<String>, data: serde_json::Value) {
        let event = Event::new(event_type, data);

        // Store in ring buffer before broadcast (ensures replay
        // can include events even if no subscribers are active)
        self.ring_buffer.push(event.clone());

        // Broadcast to subscribers. If no subscribers, this is a no-op
        // (broadcast::Sender::send returns Err if no receivers, which
        // is fine -- events are still in the ring buffer).
        let _ = self.tx.send(event);
    }

    /// Subscribe to live events.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    /// Get a reference to the ring buffer for replay queries.
    pub fn ring_buffer(&self) -> Arc<EventRingBuffer> {
        Arc::clone(&self.ring_buffer)
    }
}
```

### Step 6: Add CLI `shux events watch` command

```rust
// In the shux binary crate CLI, add the events subcommand:

// shux events watch [--filter pane.output] → JSON lines output
// Each line is a JSON-RPC notification object

// The CLI connects to the daemon UDS, sends an events.watch request,
// then reads and prints notifications as JSON lines until interrupted.
```

### Step 7: Write unit tests

```rust
#[cfg(test)]
mod event_tests {
    use super::*;
    use crate::event::Event;

    #[test]
    fn test_event_matches_exact_filter() {
        let event = Event::new("pane.created", serde_json::json!({}));
        assert!(event.matches_filter("pane.created"));
        assert!(!event.matches_filter("pane.exited"));
    }

    #[test]
    fn test_event_matches_prefix_filter() {
        let event = Event::new("pane.created", serde_json::json!({}));
        assert!(event.matches_filter("pane."));
        assert!(!event.matches_filter("session."));
    }

    #[test]
    fn test_event_matches_empty_filter() {
        let event = Event::new("pane.created", serde_json::json!({}));
        assert!(event.matches_filter(""));
    }

    #[test]
    fn test_event_matches_any_filter_empty_list() {
        let event = Event::new("pane.created", serde_json::json!({}));
        assert!(event.matches_any_filter(&[]));
    }

    #[test]
    fn test_event_matches_any_filter_with_match() {
        let event = Event::new("pane.created", serde_json::json!({}));
        let filters = vec!["session.".to_string(), "pane.".to_string()];
        assert!(event.matches_any_filter(&filters));
    }

    #[test]
    fn test_event_matches_any_filter_no_match() {
        let event = Event::new("pane.created", serde_json::json!({}));
        let filters = vec!["session.".to_string(), "window.".to_string()];
        assert!(!event.matches_any_filter(&filters));
    }

    #[test]
    fn test_event_sequence_is_monotonic() {
        let e1 = Event::new("a", serde_json::json!({}));
        let e2 = Event::new("b", serde_json::json!({}));
        let e3 = Event::new("c", serde_json::json!({}));
        assert!(e2.seq > e1.seq);
        assert!(e3.seq > e2.seq);
    }
}

#[cfg(test)]
mod ring_buffer_tests {
    use crate::event::Event;
    use crate::ring_buffer::EventRingBuffer;

    #[test]
    fn test_push_and_len() {
        let buf = EventRingBuffer::with_capacity(10);
        assert!(buf.is_empty());

        buf.push(Event::new("test", serde_json::json!({})));
        assert_eq!(buf.len(), 1);
    }

    #[test]
    fn test_eviction_on_overflow() {
        let buf = EventRingBuffer::with_capacity(3);

        for i in 0..5 {
            buf.push(Event::new(format!("event.{}", i), serde_json::json!({})));
        }

        // Should have exactly 3 events (oldest 2 evicted)
        assert_eq!(buf.len(), 3);
    }

    #[test]
    fn test_replay_from_seq() {
        let buf = EventRingBuffer::with_capacity(100);

        let events: Vec<Event> = (0..5)
            .map(|i| Event::new(format!("event.{}", i), serde_json::json!({"i": i})))
            .collect();

        let seq_2 = events[1].seq;

        for e in events {
            buf.push(e);
        }

        let result = buf.replay(seq_2, &[], 100);
        assert!(result.gap.is_none());
        assert_eq!(result.events.len(), 3); // events after seq_2
    }

    #[test]
    fn test_replay_with_gap() {
        let buf = EventRingBuffer::with_capacity(3);

        for i in 0..5 {
            buf.push(Event::new(format!("event.{}", i), serde_json::json!({})));
        }

        // Try to replay from seq 1, which has been evicted
        let result = buf.replay(1, &[], 100);
        assert!(result.gap.is_some());
        let gap = result.gap.unwrap();
        assert!(gap.lost > 0);
    }

    #[test]
    fn test_replay_with_filters() {
        let buf = EventRingBuffer::with_capacity(100);

        buf.push(Event::new("pane.created", serde_json::json!({})));
        buf.push(Event::new("session.created", serde_json::json!({})));
        buf.push(Event::new("pane.exited", serde_json::json!({})));

        let filters = vec!["pane.".to_string()];
        let result = buf.replay(0, &filters, 100);
        assert_eq!(result.events.len(), 2); // only pane events
    }

    #[test]
    fn test_recent_events() {
        let buf = EventRingBuffer::with_capacity(100);

        for i in 0..10 {
            buf.push(Event::new(format!("event.{}", i), serde_json::json!({})));
        }

        let recent = buf.recent(&[], 5);
        assert_eq!(recent.len(), 5);
        // Should be the 5 most recent, in chronological order
        assert!(recent[0].seq < recent[4].seq);
    }

    #[test]
    fn test_seq_range() {
        let buf = EventRingBuffer::with_capacity(100);

        assert!(buf.seq_range().is_none());

        buf.push(Event::new("a", serde_json::json!({})));
        buf.push(Event::new("b", serde_json::json!({})));

        let (min, max) = buf.seq_range().unwrap();
        assert!(max > min);
    }
}

#[cfg(test)]
mod pane_output_tests {
    use crate::stream::encode_pane_output;

    #[test]
    fn test_encode_pane_output_basic() {
        let data = b"hello world";
        let encoded = encode_pane_output(data, false);
        assert_eq!(encoded["sample"], false);
        assert_eq!(encoded["truncated"], false);
        // Verify base64 encoding
        assert!(encoded["bytes"].is_string());
    }

    #[test]
    fn test_encode_pane_output_sampled() {
        let data = b"sampled data";
        let encoded = encode_pane_output(data, true);
        assert_eq!(encoded["sample"], true);
    }

    #[test]
    fn test_encode_pane_output_truncation() {
        let data = vec![0u8; 128 * 1024]; // 128 KB > 64 KB limit
        let encoded = encode_pane_output(&data, false);
        assert_eq!(encoded["truncated"], true);
    }
}
```

---

## Verification

### Functional

```bash
# Build the affected crates
cargo build -p shux-core -p shux-rpc

# Verify clippy passes
cargo clippy -p shux-core -p shux-rpc -- -D warnings

# Integration test: start daemon, subscribe to events, create a
# session, verify event is received (requires daemon to be wired up)
# shux events watch --filter session. &
# shux new -s test-events
# Expected: JSON line with session.created event
#
# Replay/gap check:
# shux events watch --filter session. --from-seq <very-old-seq>
# Expected: initial `event.gap` notification before replayed events
```

### Tests

```bash
# Run event and ring buffer tests
cargo nextest run -p shux-core -- event
cargo nextest run -p shux-core -- ring_buffer

# Run stream tests
cargo nextest run -p shux-rpc -- stream
cargo nextest run -p shux-rpc -- pane_output

# Full workspace
cargo nextest run --workspace
```

---

## Completion Criteria

- [ ] `Event` type with monotonic sequence numbers, ISO 8601 timestamps, typed event names, and JSON data
- [ ] Event filter grammar: prefix matching (`"pane."` matches all pane events), exact matching, empty = all
- [ ] `EventRingBuffer` with bounded capacity, FIFO eviction, and `from_seq` replay
- [ ] `replay()` returns gap info when `from_seq` is older than the oldest buffered event
- [ ] `events.watch` handler: parses filters, replays from ring buffer, streams live events
- [ ] Gap notifications sent as `{"method": "event.gap", "params": {"from": N, "to": M, "lost": K}}`
- [ ] Event notifications sent as `{"method": "event", "params": {"seq": N, "ts": "...", "type": "...", "data": {...}}}`
- [ ] `pane.output` binary encoding: base64 `data.bytes`, `data.sample` boolean, 64 KB max chunk
- [ ] `events.history` method queries ring buffer with filters and limit
- [ ] `EventBus` integrates ring buffer: events stored before broadcast, ring buffer accessible for replay
- [ ] CLI `shux events watch [--filter ...]` outputs JSON lines
- [ ] Held connection stays open until client disconnect or daemon shutdown
- [ ] Per-client send buffer with overflow detection and gap notification
- [ ] All unit tests pass (event filtering, ring buffer, replay, gap detection, base64 encoding)
- [ ] `cargo clippy --workspace -- -D warnings` passes

---

## Commit Message
```
feat(core,rpc): add event stream with ring buffer replay and gap detection

- Event type with monotonic sequence numbers and ISO 8601 timestamps
- Prefix-based event filter grammar (pane. matches all pane events)
- EventRingBuffer with bounded capacity and from_seq replay
- events.watch handler: replay + live streaming over held UDS connection
- Gap notifications for evicted events and broadcast overflow
- pane.output binary encoding (base64, 64KB max chunk)
- events.history method for ring buffer queries
- EventBus integration: ring buffer storage + broadcast
```

---

## Session Protocol

1. **Before starting:** Read `CLAUDE.md`. Read tasks 007 (event bus) and 035 (JSON-RPC methods). Read PRD sections 8.4 (event stream contract), 21 (event taxonomy), and 4.3/4.4 (event invariants). Verify tasks 007 and 035 are complete.
2. **During:** Implement in order: Event type (Step 1) -> ring buffer (Step 2) -> stream handler (Step 3) -> events.history (Step 4) -> bus integration (Step 5) -> CLI (Step 6) -> tests (Step 7). Run `cargo check` after each step. The ring buffer and event filter are independent and can be tested in isolation before wiring into the bus.
3. **Testing:** Test the ring buffer thoroughly -- it is the foundation for replay correctness. Edge cases: empty buffer, single event, full buffer, eviction, from_seq at boundary, from_seq at 0, from_seq beyond current max.
4. **After:** Run `make check`. Update `docs/PROGRESS.md`. Update `CLAUDE.md` Learnings, especially about `tokio::sync::broadcast` lagged receiver behavior and ring buffer concurrency considerations.
5. **Watch out for:**
   - `broadcast::Receiver::recv()` returns `RecvError::Lagged(n)` when events are dropped. This is expected and must be handled with gap notifications, not panics.
   - The ring buffer uses `RwLock` -- read contention is acceptable but write contention should be minimized. The event bus is the only writer.
   - ISO 8601 timestamp formatting requires `chrono` or the `time` crate. Add the dependency to `shux-core/Cargo.toml`.
   - The `events.watch` handler must not block the main server event loop. It should be spawned as a separate task per client connection.
   - The global `AtomicU64` for sequence numbers means sequences are unique across all event types but may have gaps within a single type (which is fine for this design).
