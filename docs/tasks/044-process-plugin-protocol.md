# 044 — Process Plugin Protocol

**Status:** Pending
**Depends On:** 041
**Parallelizable With:** 042, 043

---

## Problem

Wasm plugins cover most use cases, but some plugins need capabilities that Wasm cannot easily provide: native system access (SSH tunnels), AI SDK integration (agent orchestrator), high-throughput stream processing (session replay), or existing codebases in languages without mature Wasm tooling. Process plugins solve this by spawning plugin binaries as child processes and communicating via a length-prefixed framed JSON protocol over stdio.

This task implements the complete process plugin protocol: spawning, handshake, bidirectional framed messaging, flow control for high-volume streams, plugin garbage collection, cancellation, event subscription, and full parity with the WIT host interface. Every WIT function has a corresponding JSON message type, and the same permission enforcement applies.

Process plugins are disabled by default (`allow_process_plugins = false` in config) due to their reduced sandboxing compared to Wasm. When enabled, they are a first-class citizen with identical capabilities.

## PRD Reference

- **section 7.6** — Process plugin protocol: complete specification including frame format, handshake, message types, flow control, plugin GC, cancellation, and parity requirements
- **section 7.1** — Plugin design goals: "Language-agnostic: Process plugins work from any language with stdin/stdout"
- **section 7.4** — Plugin manifest: `kind = "process"` in `plugin.toml`
- **section 7.5** — WIT interface: every host function maps 1:1 to a JSON message type
- **section 13.1** — Security: "Process plugin isolation: Disabled by default; platform-dependent when enabled"
- **section 14.1** — Performance: plugin stall timeout (5s), bounded event buffer (256 events)

---

## Files to Create

- `crates/shux-plugin/src/process.rs` — Process plugin runtime: spawn, lifecycle management, message dispatch, GC timer, stall detection
- `crates/shux-plugin/src/protocol.rs` — Protocol types: all message type definitions (host->plugin, plugin->host), serialization/deserialization, version compatibility check
- `crates/shux-plugin/src/framing.rs` — Length-prefixed framing codec: 4-byte BE length + UTF-8 JSON, encode/decode, max frame size enforcement

## Files to Modify

- `crates/shux-plugin/src/lib.rs` — Add `pub mod process; pub mod protocol; pub mod framing;`
- `crates/shux-plugin/Cargo.toml` — Add dependencies: `tokio::process`, `tokio_util::codec`, `bytes`

---

## Execution Steps

### Step 1: Implement the framing codec in `crates/shux-plugin/src/framing.rs`

The framing layer handles length-prefixed encoding/decoding over raw byte streams. Frame format: 4-byte big-endian payload length + UTF-8 JSON payload.

```rust
use bytes::{Buf, BufMut, BytesMut};
use thiserror::Error;
use tokio_util::codec::{Decoder, Encoder};

/// Maximum frame payload size (16 MB, matching the API transport limit from PRD section 8.1).
const MAX_FRAME_SIZE: u32 = 16 * 1024 * 1024;

/// Length prefix size in bytes (4 bytes, big-endian u32).
const LENGTH_PREFIX_SIZE: usize = 4;

/// Errors from the framing codec.
#[derive(Debug, Error)]
pub enum FramingError {
    #[error("frame payload exceeds maximum size: {size} > {max}")]
    FrameTooLarge { size: u32, max: u32 },

    #[error("invalid UTF-8 in frame payload")]
    InvalidUtf8,

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Length-prefixed JSON framing codec.
///
/// Wire format:
/// ```text
/// +-------------------+---------------------------+
/// | length (4 bytes)  |  JSON payload (UTF-8)     |
/// | big-endian u32    |  `length` bytes           |
/// +-------------------+---------------------------+
/// ```
///
/// This is the same framing used by the JSON-RPC API transport (PRD section 8.1)
/// and inspired by nushell's plugin protocol.
#[derive(Debug, Default)]
pub struct LengthPrefixedCodec;

impl Decoder for LengthPrefixedCodec {
    type Item = String;
    type Error = FramingError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<String>, FramingError> {
        // Need at least the length prefix.
        if src.len() < LENGTH_PREFIX_SIZE {
            return Ok(None);
        }

        // Peek at the length without consuming.
        let length = u32::from_be_bytes([src[0], src[1], src[2], src[3]]);

        // Validate frame size.
        if length > MAX_FRAME_SIZE {
            return Err(FramingError::FrameTooLarge {
                size: length,
                max: MAX_FRAME_SIZE,
            });
        }

        let total_len = LENGTH_PREFIX_SIZE + length as usize;

        // Wait for the full frame.
        if src.len() < total_len {
            // Reserve space to avoid repeated allocations.
            src.reserve(total_len - src.len());
            return Ok(None);
        }

        // Consume the length prefix.
        src.advance(LENGTH_PREFIX_SIZE);

        // Consume the payload.
        let payload = src.split_to(length as usize);

        // Parse as UTF-8.
        let json = std::str::from_utf8(&payload)
            .map_err(|_| FramingError::InvalidUtf8)?
            .to_string();

        Ok(Some(json))
    }
}

impl Encoder<String> for LengthPrefixedCodec {
    type Error = FramingError;

    fn encode(&mut self, item: String, dst: &mut BytesMut) -> Result<(), FramingError> {
        let payload = item.as_bytes();
        let length = payload.len() as u32;

        if length > MAX_FRAME_SIZE {
            return Err(FramingError::FrameTooLarge {
                size: length,
                max: MAX_FRAME_SIZE,
            });
        }

        // Write length prefix (4 bytes, big-endian).
        dst.reserve(LENGTH_PREFIX_SIZE + payload.len());
        dst.put_u32(length);

        // Write payload.
        dst.extend_from_slice(payload);

        Ok(())
    }
}
```

### Step 2: Define protocol message types in `crates/shux-plugin/src/protocol.rs`

Define all message types for bidirectional communication between host and plugin.

```rust
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ══════════════════════════════════════════════
// Handshake
// ══════════════════════════════════════════════

/// Hello message (first message in both directions).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloMessage {
    #[serde(rename = "type")]
    pub msg_type: String, // "hello"
    pub protocol: String, // "shux-plugin"
    pub version: String,  // "1.0.0"
    pub plugin_id: Option<String>, // Set by host, absent from plugin response
    pub features: Vec<String>,
}

impl HelloMessage {
    pub fn host(plugin_id: &str, version: &str) -> Self {
        Self {
            msg_type: "hello".to_string(),
            protocol: "shux-plugin".to_string(),
            version: version.to_string(),
            plugin_id: Some(plugin_id.to_string()),
            features: Vec::new(),
        }
    }

    pub fn plugin(version: &str) -> Self {
        Self {
            msg_type: "hello".to_string(),
            protocol: "shux-plugin".to_string(),
            version: version.to_string(),
            plugin_id: None,
            features: Vec::new(),
        }
    }
}

/// Check version compatibility.
///
/// Rules (from PRD, matching nushell):
/// - `0.x.y` and `0.x'.y'` are incompatible if `x != x'`
/// - For `>=1.0.0`, major version must match
pub fn versions_compatible(host_version: &str, plugin_version: &str) -> bool {
    let parse = |v: &str| -> Option<(u64, u64, u64)> {
        let parts: Vec<&str> = v.split('.').collect();
        if parts.len() != 3 {
            return None;
        }
        Some((
            parts[0].parse().ok()?,
            parts[1].parse().ok()?,
            parts[2].parse().ok()?,
        ))
    };

    let (h_major, h_minor, _) = match parse(host_version) {
        Some(v) => v,
        None => return false,
    };
    let (p_major, p_minor, _) = match parse(plugin_version) {
        Some(v) => v,
        None => return false,
    };

    if h_major == 0 && p_major == 0 {
        // Pre-1.0: minor version must match
        h_minor == p_minor
    } else {
        // Post-1.0: major version must match
        h_major == p_major
    }
}

// ══════════════════════════════════════════════
// Host -> Plugin messages
// ══════════════════════════════════════════════

/// A message from the host to the plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum HostMessage {
    // --- Requests (have "id", expect response) ---

    /// Invoke a command on the plugin.
    #[serde(rename = "invoke")]
    Invoke {
        id: String,
        command: String,
        args: Vec<String>,
    },

    /// Request a status bar segment render.
    #[serde(rename = "render")]
    Render {
        id: String,
        segment: String,
        width: u16,
    },

    /// Request an overlay render.
    #[serde(rename = "render_overlay")]
    RenderOverlay {
        id: String,
        pane_id: String,
        width: u16,
        height: u16,
    },

    /// Request event interception.
    #[serde(rename = "intercept")]
    Intercept {
        id: String,
        event: Value,
    },

    /// Forward overlay input to the plugin.
    #[serde(rename = "overlay_input")]
    OverlayInput {
        id: String,
        pane_id: String,
        key_event: Value,
    },

    // --- Notifications (no "id", no response expected) ---

    /// Deliver an event to the plugin.
    #[serde(rename = "event")]
    Event {
        stream_id: Option<u64>,
        event: Value,
    },

    /// Signal the plugin (e.g., interrupt).
    #[serde(rename = "signal")]
    Signal {
        signal: String,
    },

    /// Request graceful shutdown.
    #[serde(rename = "shutdown")]
    Shutdown {},

    /// Cancel an in-flight request.
    #[serde(rename = "cancel")]
    Cancel {
        id: String,
    },
}

// ══════════════════════════════════════════════
// Plugin -> Host messages
// ══════════════════════════════════════════════

/// A message from the plugin to the host.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PluginMessage {
    // --- Responses ---

    /// Response to a host request.
    #[serde(rename = "result")]
    Result {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<PluginErrorPayload>,
    },

    // --- Registration ---

    /// Register extensions (sent once after hello).
    #[serde(rename = "register")]
    Register {
        #[serde(default)]
        commands: Vec<String>,
        #[serde(default)]
        segments: Vec<String>,
        #[serde(default)]
        themes: Vec<Value>,
        #[serde(default)]
        api_methods: Vec<ApiMethodRegistration>,
        #[serde(default)]
        command_overrides: Vec<String>,
        #[serde(default)]
        layouts: Vec<Value>,
    },

    // --- Display actions ---

    #[serde(rename = "set_status")]
    SetStatus {
        segment_id: String,
        content: StatusContent,
    },

    #[serde(rename = "set_badge")]
    SetBadge {
        pane_id: String,
        badge: String,
    },

    #[serde(rename = "clear_badge")]
    ClearBadge {
        pane_id: String,
    },

    #[serde(rename = "show_overlay")]
    ShowOverlay {
        pane_id: String,
    },

    #[serde(rename = "hide_overlay")]
    HideOverlay {
        pane_id: String,
    },

    #[serde(rename = "log")]
    Log {
        level: String,
        msg: String,
    },

    #[serde(rename = "emit_event")]
    EmitEvent {
        event_type: String,
        data: Value,
    },

    // --- Pane/session control ---

    #[serde(rename = "create_pane")]
    CreatePane {
        id: String,
        window_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        command: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default)]
        env: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },

    #[serde(rename = "split_pane")]
    SplitPane {
        id: String,
        target_pane_id: String,
        direction: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        size_percent: Option<u8>,
        #[serde(skip_serializing_if = "Option::is_none")]
        command: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },

    #[serde(rename = "close_pane")]
    ClosePanMsg { id: String, pane_id: String },

    #[serde(rename = "focus_pane")]
    FocusPane { id: String, pane_id: String },

    #[serde(rename = "send_keys")]
    SendKeys {
        id: String,
        pane_id: String,
        keys: String,
    },

    #[serde(rename = "read_pane_output")]
    ReadPaneOutput {
        id: String,
        pane_id: String,
        lines: u32,
    },

    // --- Session/window control ---

    #[serde(rename = "create_session")]
    CreateSession { id: String, name: String },

    #[serde(rename = "create_window")]
    CreateWindow {
        id: String,
        session_id: String,
        name: String,
    },

    #[serde(rename = "kill_session")]
    KillSession { id: String, session_id: String },

    #[serde(rename = "focus_window")]
    FocusWindow { id: String, window_id: String },

    // --- Queries ---

    #[serde(rename = "get_pane")]
    GetPane { id: String, pane_id: String },

    #[serde(rename = "list_panes")]
    ListPanes { id: String },

    #[serde(rename = "get_window")]
    GetWindow { id: String, window_id: String },

    #[serde(rename = "list_windows")]
    ListWindows { id: String },

    #[serde(rename = "get_session")]
    GetSession { id: String, session_id: String },

    #[serde(rename = "list_sessions")]
    ListSessions { id: String },

    #[serde(rename = "get_config")]
    GetConfig { id: String, key: String },

    // --- Flow control ---

    #[serde(rename = "ack")]
    Ack { stream_id: u64 },

    #[serde(rename = "drop")]
    Drop { stream_id: u64 },

    // --- Event subscription ---

    #[serde(rename = "subscribe")]
    Subscribe {
        event_type: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pane_id: Option<String>,
        #[serde(default)]
        exhaustive: bool,
    },

    // --- API extension ---

    #[serde(rename = "register_api_method")]
    RegisterApiMethod {
        method_name: String,
        description: String,
    },

    #[serde(rename = "register_command_override")]
    RegisterCommandOverride {
        command_name: String,
    },

    // --- Set pane tag ---

    #[serde(rename = "set_pane_tag")]
    SetPaneTag {
        pane_id: String,
        key: String,
        value: String,
    },

    // --- Layout ---

    #[serde(rename = "register_layout")]
    RegisterLayout {
        name: String,
        layout_json: String,
    },

    #[serde(rename = "apply_layout")]
    ApplyLayout {
        id: String,
        name: String,
    },

    // --- Clipboard ---

    #[serde(rename = "get_clipboard")]
    GetClipboard { id: String },

    #[serde(rename = "set_clipboard")]
    SetClipboard { id: String, content: String },
}

/// Status bar segment content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusContent {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bg: Option<String>,
}

/// API method registration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiMethodRegistration {
    pub method_name: String,
    pub description: String,
}

/// Plugin error payload in result messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginErrorPayload {
    pub code: i32,
    pub message: String,
}
```

### Step 3: Implement the process plugin runtime in `crates/shux-plugin/src/process.rs`

The runtime manages the child process lifecycle, handshake, message routing, and flow control.

```rust
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, RwLock};
use tokio::time::{sleep, timeout};
use tokio_util::codec::{FramedRead, FramedWrite};
use tracing::{debug, error, info, warn, instrument};

use crate::framing::LengthPrefixedCodec;
use crate::protocol::*;

/// Default idle timeout for process plugin garbage collection.
const DEFAULT_GC_TIMEOUT: Duration = Duration::from_secs(30);

/// Default stall timeout for exhaustive stream backpressure.
const DEFAULT_STALL_TIMEOUT: Duration = Duration::from_secs(5);

/// Default event buffer size per plugin.
const DEFAULT_EVENT_BUFFER_SIZE: usize = 256;

/// Configuration for a process plugin.
#[derive(Debug, Clone)]
pub struct ProcessPluginConfig {
    /// Path to the plugin binary.
    pub binary_path: PathBuf,

    /// Plugin ID from plugin.toml.
    pub plugin_id: String,

    /// Protocol version expected.
    pub protocol_version: String,

    /// Permissions from plugin.toml.
    pub permissions: PluginPermissions,

    /// Whether to opt out of GC (gc = false in plugin.toml).
    pub gc_exempt: bool,

    /// Custom GC timeout (overrides default).
    pub gc_timeout: Option<Duration>,

    /// Custom stall timeout (overrides default).
    pub stall_timeout: Option<Duration>,
}

/// Permissions for a process plugin (mirrors plugin.toml [permissions]).
#[derive(Debug, Clone, Default)]
pub struct PluginPermissions {
    pub events: Vec<String>,
    pub read_pane_output: bool,
    pub send_keys: bool,
    pub manage_panes: bool,
    pub manage_sessions: bool,
    pub api_extensions: bool,
    pub exec: bool,
    pub fs_read: Vec<String>,
    pub fs_write: Vec<String>,
    pub network: bool,
    pub clipboard: bool,
    pub intercept_events: Vec<String>,
    pub override_commands: Vec<String>,
}

/// State of a running process plugin.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum ProcessPluginState {
    /// Plugin is starting up (spawned, awaiting handshake).
    Starting,

    /// Handshake complete, awaiting registration.
    Handshaking,

    /// Plugin has registered its extensions and is ready.
    Ready,

    /// Plugin is processing events/requests.
    Active,

    /// Plugin is being shut down.
    ShuttingDown,

    /// Plugin has exited.
    Stopped,

    /// Plugin has been killed due to stall or error.
    Killed { reason: String },
}

/// A running process plugin.
pub struct ProcessPlugin {
    config: ProcessPluginConfig,
    state: ProcessPluginState,
    child: Option<Child>,

    /// Channel to send messages to the plugin (written to its stdin).
    outbound_tx: Option<mpsc::Sender<String>>,

    /// Pending request map: request ID -> response channel.
    pending_requests: HashMap<String, oneshot::Sender<PluginMessage>>,

    /// Next request ID counter.
    next_request_id: u64,

    /// Last activity timestamp (for GC).
    last_activity: Instant,

    /// Per-stream event buffers for flow control.
    /// Key: stream_id, Value: buffered event count.
    stream_buffers: HashMap<u64, StreamBuffer>,

    /// Next stream ID.
    next_stream_id: u64,

    /// Event subscriptions.
    subscriptions: Vec<EventSubscription>,
}

/// Tracks a flow-controlled event stream.
#[derive(Debug)]
struct StreamBuffer {
    /// Number of un-acked events in the buffer.
    pending_count: usize,

    /// Maximum buffer size before dropping/backpressure.
    max_buffer_size: usize,

    /// Whether this is an exhaustive stream (backpressure instead of drop).
    exhaustive: bool,

    /// Pane ID for exhaustive streams (to pause PTY reads).
    pane_id: Option<String>,
}

/// Event subscription from a plugin.
#[derive(Debug, Clone)]
struct EventSubscription {
    event_type: String,
    pane_id: Option<String>,
    exhaustive: bool,
    stream_id: u64,
}

impl ProcessPlugin {
    /// Spawn a new process plugin.
    #[instrument(skip(config), fields(plugin_id = %config.plugin_id))]
    pub async fn spawn(config: ProcessPluginConfig) -> Result<Self, ProcessPluginError> {
        info!(binary = %config.binary_path.display(), "Spawning process plugin");

        let mut child = Command::new(&config.binary_path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            // Scrub environment for security (PRD section 13.1).
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", std::env::var("HOME").unwrap_or_default())
            .env("TERM", std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".to_string()))
            .env("LANG", std::env::var("LANG").unwrap_or_else(|_| "en_US.UTF-8".to_string()))
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| ProcessPluginError::SpawnFailed(e.to_string()))?;

        // Set up framed I/O on stdin/stdout.
        let stdin = child.stdin.take()
            .ok_or(ProcessPluginError::StdioMissing("stdin"))?;
        let stdout = child.stdout.take()
            .ok_or(ProcessPluginError::StdioMissing("stdout"))?;

        // Spawn stderr reader that routes to shux log system.
        if let Some(stderr) = child.stderr.take() {
            let plugin_id = config.plugin_id.clone();
            tokio::spawn(async move {
                Self::stderr_reader(plugin_id, stderr).await;
            });
        }

        // Set up framed write channel.
        let (outbound_tx, outbound_rx) = mpsc::channel::<String>(64);
        tokio::spawn(Self::stdin_writer(stdin, outbound_rx));

        let mut plugin = Self {
            config,
            state: ProcessPluginState::Starting,
            child: Some(child),
            outbound_tx: Some(outbound_tx),
            pending_requests: HashMap::new(),
            next_request_id: 1,
            last_activity: Instant::now(),
            stream_buffers: HashMap::new(),
            next_stream_id: 1,
            subscriptions: Vec::new(),
        };

        // Start reading from stdout in a background task.
        // (In the full implementation, this task routes messages
        // back to the ProcessPlugin via another channel.)

        Ok(plugin)
    }

    /// Perform the hello handshake.
    pub async fn handshake(&mut self) -> Result<(), ProcessPluginError> {
        self.state = ProcessPluginState::Handshaking;

        // Send hello from host.
        let hello = HelloMessage::host(&self.config.plugin_id, &self.config.protocol_version);
        self.send_raw(&serde_json::to_string(&hello).unwrap()).await?;

        // Wait for hello response from plugin (with timeout).
        let response = timeout(Duration::from_secs(5), self.receive_hello())
            .await
            .map_err(|_| ProcessPluginError::HandshakeTimeout)?
            .map_err(|e| ProcessPluginError::HandshakeFailed(e.to_string()))?;

        // Check version compatibility.
        if !versions_compatible(&self.config.protocol_version, &response.version) {
            return Err(ProcessPluginError::IncompatibleVersion {
                host: self.config.protocol_version.clone(),
                plugin: response.version,
            });
        }

        self.state = ProcessPluginState::Ready;
        info!(
            plugin_id = %self.config.plugin_id,
            plugin_version = %response.version,
            "Process plugin handshake complete"
        );

        Ok(())
    }

    /// Send a request and wait for a response.
    pub async fn request(
        &mut self,
        message: HostMessage,
        timeout_duration: Duration,
    ) -> Result<PluginMessage, ProcessPluginError> {
        let request_id = self.next_request_id();
        // 1) Create a oneshot responder and store it in pending_requests[request_id].
        // 2) Wrap `message` with `id=request_id` and send it via `send_raw`.
        // 3) Await the oneshot with timeout; remove pending entry on both success/failure.
        // 4) Return the plugin response or timeout/channel error.
        self.last_activity = Instant::now();
        let response = self
            .send_request_wait_response(request_id, message, timeout_duration)
            .await?;
        Ok(response)
    }

    /// Send a notification (no response expected).
    pub async fn notify(&mut self, message: HostMessage) -> Result<(), ProcessPluginError> {
        let json = serde_json::to_string(&message)
            .map_err(|e| ProcessPluginError::SerializationError(e.to_string()))?;
        self.send_raw(&json).await?;
        self.last_activity = Instant::now();
        Ok(())
    }

    /// Deliver an event to the plugin (with flow control).
    pub async fn deliver_event(
        &mut self,
        event: serde_json::Value,
        stream_id: Option<u64>,
    ) -> Result<EventDeliveryResult, ProcessPluginError> {
        // Check flow control for the stream.
        if let Some(sid) = stream_id {
            if let Some(buf) = self.stream_buffers.get_mut(&sid) {
                if buf.pending_count >= buf.max_buffer_size {
                    if buf.exhaustive {
                        // Backpressure: pause PTY reads for this pane.
                        return Ok(EventDeliveryResult::Backpressure {
                            stream_id: sid,
                            pane_id: buf.pane_id.clone(),
                        });
                    } else {
                        // Drop: buffer full, discard event.
                        warn!(
                            plugin_id = %self.config.plugin_id,
                            stream_id = sid,
                            "Event buffer full — dropping event"
                        );
                        return Ok(EventDeliveryResult::Dropped);
                    }
                }
                buf.pending_count += 1;
            }
        }

        let msg = HostMessage::Event {
            stream_id,
            event,
        };
        self.notify(msg).await?;
        Ok(EventDeliveryResult::Delivered)
    }

    /// Handle an ACK from the plugin (flow control).
    pub fn handle_ack(&mut self, stream_id: u64) {
        if let Some(buf) = self.stream_buffers.get_mut(&stream_id) {
            buf.pending_count = buf.pending_count.saturating_sub(1);
        }
    }

    /// Handle a DROP from the plugin (unsubscribe from stream).
    pub fn handle_drop(&mut self, stream_id: u64) {
        self.stream_buffers.remove(&stream_id);
        self.subscriptions.retain(|s| s.stream_id != stream_id);
        info!(
            plugin_id = %self.config.plugin_id,
            stream_id = stream_id,
            "Plugin dropped event stream"
        );
    }

    /// Request graceful shutdown.
    pub async fn shutdown(&mut self) -> Result<(), ProcessPluginError> {
        self.state = ProcessPluginState::ShuttingDown;
        self.notify(HostMessage::Shutdown {}).await?;

        // Wait for process to exit with timeout.
        if let Some(ref mut child) = self.child {
            match timeout(Duration::from_secs(5), child.wait()).await {
                Ok(Ok(status)) => {
                    info!(
                        plugin_id = %self.config.plugin_id,
                        exit_code = status.code(),
                        "Process plugin exited cleanly"
                    );
                }
                Ok(Err(e)) => {
                    warn!(
                        plugin_id = %self.config.plugin_id,
                        error = %e,
                        "Process plugin exit error"
                    );
                }
                Err(_) => {
                    warn!(
                        plugin_id = %self.config.plugin_id,
                        "Process plugin did not exit within timeout — killing"
                    );
                    let _ = child.kill().await;
                }
            }
        }

        self.state = ProcessPluginState::Stopped;
        Ok(())
    }

    /// Check if the plugin should be garbage collected.
    pub fn should_gc(&self) -> bool {
        if self.config.gc_exempt {
            return false;
        }
        let gc_timeout = self.config.gc_timeout.unwrap_or(DEFAULT_GC_TIMEOUT);
        self.last_activity.elapsed() > gc_timeout
    }

    /// Check if the plugin is stalled (for exhaustive stream backpressure).
    pub fn is_stalled(&self) -> bool {
        let stall_timeout = self.config.stall_timeout.unwrap_or(DEFAULT_STALL_TIMEOUT);
        for buf in self.stream_buffers.values() {
            if buf.exhaustive && buf.pending_count >= buf.max_buffer_size {
                // Buffer has been full — check if we've been waiting too long.
                // (In practice, we'd track when the buffer became full.)
                return true;
            }
        }
        false
    }

    // --- Private helpers ---

    fn next_request_id(&mut self) -> String {
        let id = format!("req-{}", self.next_request_id);
        self.next_request_id += 1;
        id
    }

    async fn send_raw(&self, json: &str) -> Result<(), ProcessPluginError> {
        if let Some(ref tx) = self.outbound_tx {
            tx.send(json.to_string())
                .await
                .map_err(|_| ProcessPluginError::ChannelClosed)?;
            Ok(())
        } else {
            Err(ProcessPluginError::ChannelClosed)
        }
    }

    async fn receive_hello(&mut self) -> Result<HelloMessage, ProcessPluginError> {
        // Read exactly one framed message from stdout, parse as PluginMessage::Hello,
        // and reject any non-hello payload during handshake.
        let msg = self.read_next_plugin_message().await?;
        match msg {
            PluginMessage::Hello { plugin_id, version, capabilities } => {
                Ok(HelloMessage { plugin_id, version, capabilities })
            }
            other => Err(ProcessPluginError::HandshakeFailed(format!(
                "expected hello, got {other:?}"
            ))),
        }
    }

    async fn stdin_writer(
        stdin: tokio::process::ChildStdin,
        mut rx: mpsc::Receiver<String>,
    ) {
        use futures::SinkExt;
        let mut framed = FramedWrite::new(stdin, LengthPrefixedCodec);

        while let Some(msg) = rx.recv().await {
            if let Err(e) = framed.send(msg).await {
                error!("Failed to write to plugin stdin: {}", e);
                break;
            }
        }
    }

    async fn stderr_reader(
        plugin_id: String,
        stderr: tokio::process::ChildStderr,
    ) {
        use tokio::io::AsyncBufReadExt;
        let reader = tokio::io::BufReader::new(stderr);
        let mut lines = reader.lines();

        while let Ok(Some(line)) = lines.next_line().await {
            // Route to shux log system, tagged with plugin ID.
            info!(
                plugin_id = %plugin_id,
                source = "stderr",
                "{}",
                line
            );
        }
    }
}

/// Result of event delivery.
#[derive(Debug)]
pub enum EventDeliveryResult {
    /// Event was delivered to the plugin.
    Delivered,

    /// Event was dropped (buffer full, non-exhaustive stream).
    Dropped,

    /// Backpressure: need to pause PTY reads for this pane.
    Backpressure {
        stream_id: u64,
        pane_id: Option<String>,
    },
}

/// Errors from the process plugin runtime.
#[derive(Debug, thiserror::Error)]
pub enum ProcessPluginError {
    #[error("failed to spawn plugin binary: {0}")]
    SpawnFailed(String),

    #[error("plugin {0} missing")]
    StdioMissing(&'static str),

    #[error("handshake timed out")]
    HandshakeTimeout,

    #[error("handshake failed: {0}")]
    HandshakeFailed(String),

    #[error("incompatible version: host={host}, plugin={plugin}")]
    IncompatibleVersion { host: String, plugin: String },

    #[error("serialization error: {0}")]
    SerializationError(String),

    #[error("channel closed")]
    ChannelClosed,

    #[error("request timed out")]
    RequestTimeout,

    #[error("permission denied: {0}")]
    PermissionDenied(String),

    #[error("plugin exited unexpectedly: {0}")]
    UnexpectedExit(String),
}
```

### Step 4: Implement permission enforcement for process plugin messages

Every plugin->host message must be checked against the plugin's declared permissions.

```rust
// In crates/shux-plugin/src/process.rs

impl ProcessPlugin {
    /// Check if a plugin message is permitted by the plugin's permissions.
    pub fn check_permission(&self, message: &PluginMessage) -> Result<(), ProcessPluginError> {
        let perms = &self.config.permissions;

        match message {
            // Always allowed: queries, display actions, log, flow control
            PluginMessage::GetPane { .. }
            | PluginMessage::ListPanes { .. }
            | PluginMessage::GetWindow { .. }
            | PluginMessage::ListWindows { .. }
            | PluginMessage::GetSession { .. }
            | PluginMessage::ListSessions { .. }
            | PluginMessage::GetConfig { .. }
            | PluginMessage::SetStatus { .. }
            | PluginMessage::SetBadge { .. }
            | PluginMessage::ClearBadge { .. }
            | PluginMessage::ShowOverlay { .. }
            | PluginMessage::HideOverlay { .. }
            | PluginMessage::Log { .. }
            | PluginMessage::EmitEvent { .. }
            | PluginMessage::Ack { .. }
            | PluginMessage::Drop { .. }
            | PluginMessage::Subscribe { .. }
            | PluginMessage::Result { .. }
            | PluginMessage::Register { .. } => Ok(()),

            // Requires manage_panes
            PluginMessage::CreatePane { .. }
            | PluginMessage::SplitPane { .. }
            | PluginMessage::ClosePanMsg { .. }
            | PluginMessage::FocusPane { .. }
            | PluginMessage::RegisterLayout { .. }
            | PluginMessage::ApplyLayout { .. }
            | PluginMessage::SetPaneTag { .. } => {
                if perms.manage_panes {
                    Ok(())
                } else {
                    Err(ProcessPluginError::PermissionDenied("manage_panes".to_string()))
                }
            }

            // Requires send_keys
            PluginMessage::SendKeys { .. } => {
                if perms.send_keys {
                    Ok(())
                } else {
                    Err(ProcessPluginError::PermissionDenied("send_keys".to_string()))
                }
            }

            // Requires read_pane_output
            PluginMessage::ReadPaneOutput { .. } => {
                if perms.read_pane_output {
                    Ok(())
                } else {
                    Err(ProcessPluginError::PermissionDenied("read_pane_output".to_string()))
                }
            }

            // Requires manage_sessions
            PluginMessage::CreateSession { .. }
            | PluginMessage::CreateWindow { .. }
            | PluginMessage::KillSession { .. }
            | PluginMessage::FocusWindow { .. } => {
                if perms.manage_sessions {
                    Ok(())
                } else {
                    Err(ProcessPluginError::PermissionDenied("manage_sessions".to_string()))
                }
            }

            // Requires api_extensions
            PluginMessage::RegisterApiMethod { .. } => {
                if perms.api_extensions {
                    Ok(())
                } else {
                    Err(ProcessPluginError::PermissionDenied("api_extensions".to_string()))
                }
            }

            // Requires override_commands (checked per-command elsewhere)
            PluginMessage::RegisterCommandOverride { .. } => {
                if !perms.override_commands.is_empty() {
                    Ok(())
                } else {
                    Err(ProcessPluginError::PermissionDenied("override_commands".to_string()))
                }
            }

            // Requires clipboard
            PluginMessage::GetClipboard { .. }
            | PluginMessage::SetClipboard { .. } => {
                if perms.clipboard {
                    Ok(())
                } else {
                    Err(ProcessPluginError::PermissionDenied("clipboard".to_string()))
                }
            }
        }
    }
}
```

### Step 5: Write tests

```rust
#[cfg(test)]
mod framing_tests {
    use super::framing::*;
    use bytes::BytesMut;
    use tokio_util::codec::{Decoder, Encoder};

    #[test]
    fn test_encode_decode_roundtrip() {
        let mut codec = LengthPrefixedCodec;
        let mut buf = BytesMut::new();

        let message = r#"{"type":"hello","protocol":"shux-plugin","version":"1.0.0"}"#.to_string();
        codec.encode(message.clone(), &mut buf).unwrap();

        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded, message);
    }

    #[test]
    fn test_partial_frame() {
        let mut codec = LengthPrefixedCodec;
        let mut buf = BytesMut::new();

        // Encode a message.
        let message = r#"{"type":"hello"}"#.to_string();
        codec.encode(message.clone(), &mut buf).unwrap();

        // Split the buffer in half to simulate partial read.
        let full_len = buf.len();
        let mut partial = buf.split_to(full_len / 2);

        // First decode should return None (incomplete).
        assert!(codec.decode(&mut partial).unwrap().is_none());

        // Add the rest.
        partial.extend_from_slice(&buf);

        // Now it should decode.
        let decoded = codec.decode(&mut partial).unwrap().unwrap();
        assert_eq!(decoded, message);
    }

    #[test]
    fn test_frame_too_large() {
        let mut codec = LengthPrefixedCodec;
        let mut buf = BytesMut::new();

        // Write a length that exceeds the max.
        buf.extend_from_slice(&(MAX_FRAME_SIZE + 1).to_be_bytes());

        match codec.decode(&mut buf) {
            Err(FramingError::FrameTooLarge { .. }) => {} // expected
            other => panic!("Expected FrameTooLarge, got {:?}", other),
        }
    }

    #[test]
    fn test_multiple_frames() {
        let mut codec = LengthPrefixedCodec;
        let mut buf = BytesMut::new();

        let msg1 = r#"{"type":"hello"}"#.to_string();
        let msg2 = r#"{"type":"register"}"#.to_string();

        codec.encode(msg1.clone(), &mut buf).unwrap();
        codec.encode(msg2.clone(), &mut buf).unwrap();

        let decoded1 = codec.decode(&mut buf).unwrap().unwrap();
        let decoded2 = codec.decode(&mut buf).unwrap().unwrap();

        assert_eq!(decoded1, msg1);
        assert_eq!(decoded2, msg2);
    }
}

#[cfg(test)]
mod protocol_tests {
    use super::protocol::*;

    #[test]
    fn test_version_compatible_pre_1_0() {
        assert!(versions_compatible("0.1.0", "0.1.5")); // same minor
        assert!(!versions_compatible("0.1.0", "0.2.0")); // different minor
    }

    #[test]
    fn test_version_compatible_post_1_0() {
        assert!(versions_compatible("1.0.0", "1.5.3")); // same major
        assert!(!versions_compatible("1.0.0", "2.0.0")); // different major
    }

    #[test]
    fn test_hello_serialization() {
        let hello = HelloMessage::host("com.example.test", "1.0.0");
        let json = serde_json::to_string(&hello).unwrap();
        assert!(json.contains("shux-plugin"));
        assert!(json.contains("1.0.0"));
        assert!(json.contains("com.example.test"));
    }

    #[test]
    fn test_host_message_serialization() {
        let msg = HostMessage::Invoke {
            id: "req-1".to_string(),
            command: "my-plugin.refresh".to_string(),
            args: vec!["--force".to_string()],
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("invoke"));
        assert!(json.contains("req-1"));
    }

    #[test]
    fn test_plugin_message_deserialization() {
        let json = r#"{"type":"result","id":"req-1","data":{"status":"ok"}}"#;
        let msg: PluginMessage = serde_json::from_str(json).unwrap();
        match msg {
            PluginMessage::Result { id, data, error } => {
                assert_eq!(id, "req-1");
                assert!(data.is_some());
                assert!(error.is_none());
            }
            _ => panic!("Expected Result"),
        }
    }

    #[test]
    fn test_flow_control_messages() {
        let ack = PluginMessage::Ack { stream_id: 42 };
        let json = serde_json::to_string(&ack).unwrap();
        assert!(json.contains("ack"));
        assert!(json.contains("42"));

        let drop_msg = PluginMessage::Drop { stream_id: 42 };
        let json = serde_json::to_string(&drop_msg).unwrap();
        assert!(json.contains("drop"));
    }
}

#[cfg(test)]
mod permission_tests {
    use super::*;

    fn make_plugin_no_perms() -> ProcessPlugin {
        // Construct with in-memory channels (no subprocess spawn) and an empty
        // permission set so permission checks can be tested deterministically.
        ProcessPlugin::new_for_tests(ProcessPluginConfig::no_permissions())
    }

    // Permission tests would verify that:
    // - Queries (get_pane, list_panes, etc.) always pass
    // - Display actions (set_status, set_badge, etc.) always pass
    // - Control actions require manage_panes / manage_sessions
    // - send_keys requires send_keys permission
    // - read_pane_output requires read_pane_output permission
    // - API extensions require api_extensions permission
    // - Clipboard requires clipboard permission
}
```

---

## Verification

### Functional

```bash
# Build the process plugin modules
cargo build -p shux-plugin 2>&1 | tail -5

# Verify all protocol types compile and serialize correctly
cargo check -p shux-plugin

# Verify no clippy warnings
cargo clippy -p shux-plugin -- -D warnings
```

### Tests

```bash
# Run framing codec tests
cargo nextest run -p shux-plugin framing_tests

# Run protocol tests
cargo nextest run -p shux-plugin protocol_tests

# Run permission tests
cargo nextest run -p shux-plugin permission_tests

# Run all tests
cargo nextest run --workspace
```

---

## Completion Criteria

- [ ] `LengthPrefixedCodec` correctly encodes/decodes 4-byte BE length-prefixed JSON frames
- [ ] Max frame size (16 MB) enforced on both encode and decode
- [ ] Partial frame handling works correctly (returns None until complete)
- [ ] All host->plugin message types defined and serializable: invoke, render, render_overlay, intercept, overlay_input, event, signal, shutdown, cancel
- [ ] All plugin->host message types defined and deserializable: result, register, set_status, set_badge, log, emit_event, create_pane, send_keys, etc.
- [ ] Hello handshake with version compatibility check (0.x minor match, >=1.0 major match)
- [ ] Process plugin spawns with scrubbed environment (PATH, HOME, TERM, LANG only)
- [ ] stderr routed to shux log system tagged with plugin ID
- [ ] Flow control: 256-event bounded buffer per stream; non-exhaustive drops, exhaustive backpressures
- [ ] ACK/DROP handling: ack decrements pending count, drop removes subscription
- [ ] Plugin GC: idle 30s -> shutdown notification -> stop (unless gc_exempt)
- [ ] Stall detection: exhaustive stream stall > 5s -> kill plugin
- [ ] Permission enforcement: every plugin->host message checked against plugin.toml permissions
- [ ] Graceful shutdown: send shutdown message, wait 5s, kill if needed
- [ ] Process plugins disabled by default (require `allow_process_plugins` config flag)
- [ ] All tests pass
- [ ] No clippy warnings

---

## Commit Message

```
feat(plugin): implement process plugin protocol with framed JSON over stdio

- Length-prefixed framing codec (4-byte BE + UTF-8 JSON) matching API transport
- Complete protocol type definitions mirroring WIT 1:1 (45+ message types)
- Hello handshake with version compatibility (nushell rules: 0.x minor, >=1.0 major)
- Process spawn with scrubbed environment (PATH, HOME, TERM, LANG only)
- Flow control: 256-event bounded buffer, ack/drop, exhaustive backpressure
- Plugin GC: 30s idle timeout with shutdown notification
- Stall detection: kill plugin after 5s exhaustive stream stall
- Permission enforcement on all plugin->host messages
- Disabled by default (allow_process_plugins config flag)
```

---

## Session Protocol

1. **Before starting:** Read task 041 (plugin lifecycle) for integration points. Read PRD section 7.6 thoroughly -- it contains the complete process plugin specification. Study nushell's plugin protocol for inspiration (the PRD references it).
2. **During:** Implement in order: framing codec (Step 1) -> protocol types (Step 2) -> process runtime (Step 3) -> permission enforcement (Step 4) -> tests (Step 5). Run `cargo check` after each step. Run tests after Steps 1, 2, and 5. The framing codec is the most critical piece -- test it thoroughly with partial frames, oversized frames, and multi-frame sequences.
3. **After:** Run the full verification suite. Verify framing roundtrip, protocol serialization, version compatibility, and permission enforcement. Update `docs/PROGRESS.md` (mark 044 done). Update `CLAUDE.md` Learnings with insights about tokio process I/O and the framing codec pattern.
