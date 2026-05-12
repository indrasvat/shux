//! Event types for shux's event bus.
//!
//! Implements the complete event taxonomy from PRD §21 (Appendix A).
//! Every event is typed, sequenced, and timestamped.

use std::fmt;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::{PaneId, SessionId, WindowId};

/// Client identifier. Not in the core data model (task 002) because clients
/// are transient connections, not persistent entities in the session graph.
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct ClientId(pub Uuid);

impl ClientId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ClientId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<Uuid> for ClientId {
    fn from(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

/// Plugin identifier — a human-readable string like "git-status" or "ai-agent".
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
    /// Optional correlation ID linking this event to a batch / transaction
    /// that produced it. Set by `state.apply` so subscribers can attribute
    /// an event burst to a specific apply call (vs. concurrent mutations).
    /// `None` for individually-fired mutations. Codex review of PR 3 plan
    /// flagged this as load-bearing for agent orchestration: without it,
    /// agents watching events cannot tell which burst belongs to their apply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
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

    /// Wire shape used by every external consumer (`events.watch`,
    /// `events.history`, and the plugin event channel). Lifts `seq`,
    /// `type`, `timestamp`, and `correlation_id` (when set) to the
    /// top level so handlers can route on them without recursing
    /// into the serde-tagged `EventData` envelope. Payload fields
    /// live under `data` (e.g. `data.session_id`).
    pub fn to_wire_json(&self) -> serde_json::Value {
        let mut out = serde_json::json!({
            "seq": self.meta.seq,
            "type": self.meta.event_type,
            "timestamp": self
                .meta
                .timestamp
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            "data": &self.data,
        });
        if let Some(cid) = &self.meta.correlation_id {
            out["correlation_id"] = serde_json::Value::String(cid.clone());
        }
        out
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
    SessionCreated { session_id: SessionId, name: String },

    /// A session was renamed.
    SessionRenamed {
        session_id: SessionId,
        old_name: String,
        new_name: String,
    },

    /// A session was killed.
    SessionKilled { session_id: SessionId, name: String },

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
        /// Position in the session's window list at creation time.
        /// Plugins use this to render `[index]` prefixes without
        /// having to call `window.list` for round-trip context.
        index: u32,
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
    //
    // PR 3a (codex review): every pane-scoped event carries `session_id` and
    // `window_id` so subscribers can route by session without dereferencing
    // the live graph (which is wrong for historical events — the pane may
    // have moved or died since the event fired).
    /// A new pane was created.
    PaneCreated {
        pane_id: PaneId,
        window_id: WindowId,
        session_id: SessionId,
        command: Vec<String>,
    },

    /// A pane received focus.
    PaneFocused {
        pane_id: PaneId,
        window_id: WindowId,
        session_id: SessionId,
        previous_pane_id: Option<PaneId>,
    },

    /// A pane was resized.
    PaneResized {
        pane_id: PaneId,
        window_id: WindowId,
        session_id: SessionId,
        cols: u16,
        rows: u16,
    },

    /// A pane's zoom state changed.
    PaneZoomed {
        pane_id: PaneId,
        window_id: WindowId,
        session_id: SessionId,
        zoomed: bool,
    },

    /// A pane's title changed (via OSC or manual set).
    PaneTitleChanged {
        pane_id: PaneId,
        window_id: WindowId,
        session_id: SessionId,
        old_title: String,
        new_title: String,
    },

    /// A pane's working directory changed.
    PaneCwdChanged {
        pane_id: PaneId,
        window_id: WindowId,
        session_id: SessionId,
        old_cwd: String,
        new_cwd: String,
    },

    /// A pane's process exited.
    PaneExited {
        pane_id: PaneId,
        window_id: WindowId,
        session_id: SessionId,
        exit_status: Option<i32>,
        command: Vec<String>,
    },

    /// A pane was respawned.
    PaneRespawned {
        pane_id: PaneId,
        window_id: WindowId,
        session_id: SessionId,
        command: Vec<String>,
    },

    /// An async command completed in a pane (pane.run_command with async=true).
    PaneCommandCompleted {
        pane_id: PaneId,
        window_id: WindowId,
        session_id: SessionId,
        command_id: String,
        exit_code: Option<i32>,
        stdout: String,
        stderr: String,
        runtime_ms: u64,
    },

    /// PTY output from a pane (opt-in, sampled by default).
    PaneOutput {
        pane_id: PaneId,
        window_id: WindowId,
        session_id: SessionId,
        /// Base64-encoded bytes (PRD §8.4).
        bytes: String,
        /// Whether this is a sample (true) or lossless (false).
        sample: bool,
    },

    /// Input sent to a pane (fired on line-submit, not per-keystroke).
    PaneInput {
        pane_id: PaneId,
        window_id: WindowId,
        session_id: SessionId,
        data: String,
    },

    /// Bell character received in a pane.
    PaneBell {
        pane_id: PaneId,
        window_id: WindowId,
        session_id: SessionId,
    },

    /// A pane's tag was changed.
    PaneTagChanged {
        pane_id: PaneId,
        window_id: WindowId,
        session_id: SessionId,
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
    ClientDisconnected { client_id: ClientId, reason: String },

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
    PluginDisabled { plugin_id: PluginId, reason: String },

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
    use crate::model::{PaneId, SessionId, WindowId};

    #[test]
    fn test_event_type_strings_session() {
        let sid = SessionId::new();
        let cid = ClientId::new();

        assert_eq!(
            EventData::SessionCreated {
                session_id: sid,
                name: "test".into(),
            }
            .event_type(),
            "session.created"
        );
        assert_eq!(
            EventData::SessionRenamed {
                session_id: sid,
                old_name: "a".into(),
                new_name: "b".into(),
            }
            .event_type(),
            "session.renamed"
        );
        assert_eq!(
            EventData::SessionKilled {
                session_id: sid,
                name: "test".into(),
            }
            .event_type(),
            "session.killed"
        );
        assert_eq!(
            EventData::SessionAttached {
                session_id: sid,
                client_id: cid,
            }
            .event_type(),
            "session.attached"
        );
        assert_eq!(
            EventData::SessionDetached {
                session_id: sid,
                client_id: cid,
            }
            .event_type(),
            "session.detached"
        );
    }

    #[test]
    fn test_event_type_strings_window() {
        let wid = WindowId::new();
        let sid = SessionId::new();

        assert_eq!(
            EventData::WindowCreated {
                window_id: wid,
                session_id: sid,
                title: "t".into(),
                index: 0,
            }
            .event_type(),
            "window.created"
        );
        assert_eq!(
            EventData::WindowActivated {
                window_id: wid,
                session_id: sid,
                previous_window_id: None,
            }
            .event_type(),
            "window.activated"
        );
        assert_eq!(
            EventData::WindowRenamed {
                window_id: wid,
                old_title: "a".into(),
                new_title: "b".into(),
            }
            .event_type(),
            "window.renamed"
        );
        assert_eq!(
            EventData::WindowReordered {
                window_id: wid,
                session_id: sid,
                old_index: 0,
                new_index: 1,
            }
            .event_type(),
            "window.reordered"
        );
        assert_eq!(
            EventData::WindowKilled {
                window_id: wid,
                session_id: sid,
            }
            .event_type(),
            "window.killed"
        );
    }

    #[test]
    fn test_event_type_strings_pane() {
        let pid = PaneId::new();
        let wid = WindowId::new();
        let sid = SessionId::new();

        assert_eq!(
            EventData::PaneCreated {
                pane_id: pid,
                window_id: wid,
                session_id: sid,
                command: vec!["bash".into()],
            }
            .event_type(),
            "pane.created"
        );
        assert_eq!(
            EventData::PaneFocused {
                pane_id: pid,
                window_id: wid,
                session_id: sid,
                previous_pane_id: None,
            }
            .event_type(),
            "pane.focused"
        );
        assert_eq!(
            EventData::PaneResized {
                pane_id: pid,
                window_id: wid,
                session_id: sid,
                cols: 80,
                rows: 24,
            }
            .event_type(),
            "pane.resized"
        );
        assert_eq!(
            EventData::PaneZoomed {
                pane_id: pid,
                window_id: wid,
                session_id: sid,
                zoomed: true,
            }
            .event_type(),
            "pane.zoomed"
        );
        assert_eq!(
            EventData::PaneTitleChanged {
                pane_id: pid,
                window_id: wid,
                session_id: sid,
                old_title: "a".into(),
                new_title: "b".into(),
            }
            .event_type(),
            "pane.title_changed"
        );
        assert_eq!(
            EventData::PaneCwdChanged {
                pane_id: pid,
                window_id: wid,
                session_id: sid,
                old_cwd: "/a".into(),
                new_cwd: "/b".into(),
            }
            .event_type(),
            "pane.cwd_changed"
        );
        assert_eq!(
            EventData::PaneExited {
                pane_id: pid,
                window_id: wid,
                session_id: sid,
                exit_status: Some(0),
                command: vec!["bash".into()],
            }
            .event_type(),
            "pane.exited"
        );
        assert_eq!(
            EventData::PaneRespawned {
                pane_id: pid,
                window_id: wid,
                session_id: sid,
                command: vec!["bash".into()],
            }
            .event_type(),
            "pane.respawned"
        );
        assert_eq!(
            EventData::PaneCommandCompleted {
                pane_id: pid,
                window_id: wid,
                session_id: sid,
                command_id: "cmd1".into(),
                exit_code: Some(0),
                stdout: "out".into(),
                stderr: String::new(),
                runtime_ms: 42,
            }
            .event_type(),
            "pane.command_completed"
        );
        assert_eq!(
            EventData::PaneOutput {
                pane_id: pid,
                window_id: wid,
                session_id: sid,
                bytes: "dGVzdA==".into(),
                sample: true,
            }
            .event_type(),
            "pane.output"
        );
        assert_eq!(
            EventData::PaneInput {
                pane_id: pid,
                window_id: wid,
                session_id: sid,
                data: "ls\n".into(),
            }
            .event_type(),
            "pane.input"
        );
        assert_eq!(
            EventData::PaneBell {
                pane_id: pid,
                window_id: wid,
                session_id: sid,
            }
            .event_type(),
            "pane.bell"
        );
        assert_eq!(
            EventData::PaneTagChanged {
                pane_id: pid,
                window_id: wid,
                session_id: sid,
                key: "role".into(),
                old_value: None,
                new_value: Some("editor".into()),
            }
            .event_type(),
            "pane.tag_changed"
        );
    }

    #[test]
    fn test_event_type_strings_client() {
        let cid = ClientId::new();

        assert_eq!(
            EventData::ClientConnected {
                client_id: cid,
                terminal_cols: 80,
                terminal_rows: 24,
            }
            .event_type(),
            "client.connected"
        );
        assert_eq!(
            EventData::ClientDisconnected {
                client_id: cid,
                reason: "quit".into(),
            }
            .event_type(),
            "client.disconnected"
        );
        assert_eq!(
            EventData::ClientResized {
                client_id: cid,
                old_cols: 80,
                old_rows: 24,
                new_cols: 120,
                new_rows: 40,
            }
            .event_type(),
            "client.resized"
        );
    }

    #[test]
    fn test_event_type_strings_theme_config_plugin_keybinding_error() {
        assert_eq!(
            EventData::ThemeChanged {
                scope: "session".into(),
                scope_id: "abc".into(),
                old_theme: None,
                new_theme: "dracula".into(),
            }
            .event_type(),
            "theme.changed"
        );
        assert_eq!(
            EventData::ConfigReloaded {
                source: "~/.config/shux/config.toml".into(),
                changes: vec![],
            }
            .event_type(),
            "config.reloaded"
        );
        assert_eq!(
            EventData::PluginEnabled {
                plugin_id: "git-status".into(),
                version: "1.0".into(),
            }
            .event_type(),
            "plugin.enabled"
        );
        assert_eq!(
            EventData::PluginDisabled {
                plugin_id: "git-status".into(),
                reason: "user".into(),
            }
            .event_type(),
            "plugin.disabled"
        );
        assert_eq!(
            EventData::PluginReloaded {
                plugin_id: "git-status".into(),
                version: "1.1".into(),
            }
            .event_type(),
            "plugin.reloaded"
        );
        assert_eq!(
            EventData::PluginError {
                plugin_id: "git-status".into(),
                error: "panic".into(),
                context: "on_pane_created".into(),
            }
            .event_type(),
            "plugin.error"
        );
        assert_eq!(
            EventData::PluginEvent {
                plugin_id: "git-status".into(),
                event_type: "branch.changed".into(),
                data: serde_json::json!({"branch": "main"}),
            }
            .event_type(),
            "plugin.event"
        );
        assert_eq!(
            EventData::KeybindingChanged {
                key: "ctrl+a".into(),
                old_action: None,
                new_action: "prefix".into(),
            }
            .event_type(),
            "keybinding.changed"
        );
        assert_eq!(
            EventData::Error {
                code: 500,
                message: "internal".into(),
                context: "graph".into(),
            }
            .event_type(),
            "error"
        );
    }

    #[test]
    fn test_event_filter_matching() {
        let event = Event {
            meta: EventMetadata {
                seq: 1,
                timestamp: SystemTime::now(),
                event_type: "pane.created".to_string(),
                correlation_id: None,
            },
            data: EventData::PaneCreated {
                pane_id: PaneId::new(),
                window_id: WindowId::new(),
                session_id: SessionId::new(),
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
    fn test_event_filter_category_prefix() {
        let event = Event {
            meta: EventMetadata {
                seq: 1,
                timestamp: SystemTime::now(),
                event_type: "window.activated".to_string(),
                correlation_id: None,
            },
            data: EventData::WindowActivated {
                window_id: WindowId::new(),
                session_id: SessionId::new(),
                previous_window_id: None,
            },
        };

        assert!(event.matches_filter("window."));
        assert!(event.matches_filter("window.activated"));
        assert!(!event.matches_filter("window.created"));
        assert!(!event.matches_filter("pane."));
    }

    #[test]
    fn test_event_serialization_roundtrip() {
        let event = Event {
            meta: EventMetadata {
                seq: 42,
                timestamp: SystemTime::now(),
                event_type: "session.created".to_string(),
                correlation_id: None,
            },
            data: EventData::SessionCreated {
                session_id: SessionId::new(),
                name: "work".to_string(),
            },
        };

        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("session.created"));
        assert!(json.contains("work"));

        // Roundtrip.
        let deserialized: Event = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.meta.seq, 42);
        assert_eq!(deserialized.event_type(), "session.created");
    }

    #[test]
    fn test_event_serialization_complex_variant() {
        let event = Event {
            meta: EventMetadata {
                seq: 7,
                timestamp: SystemTime::now(),
                event_type: "pane.command_completed".to_string(),
                correlation_id: None,
            },
            data: EventData::PaneCommandCompleted {
                pane_id: PaneId::new(),
                window_id: WindowId::new(),
                session_id: SessionId::new(),
                command_id: "cmd-123".to_string(),
                exit_code: Some(0),
                stdout: "hello world\n".to_string(),
                stderr: String::new(),
                runtime_ms: 150,
            },
        };

        let json = serde_json::to_string_pretty(&event).expect("serialize");
        let deserialized: Event = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.seq(), 7);
        assert_eq!(deserialized.event_type(), "pane.command_completed");
    }

    #[test]
    fn test_config_change_serialization() {
        let change = ConfigChange {
            key: "default-shell".to_string(),
            old: Some(serde_json::json!("/bin/bash")),
            new: serde_json::json!("/bin/zsh"),
        };

        let json = serde_json::to_string(&change).expect("serialize");
        let deserialized: ConfigChange = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.key, "default-shell");
    }

    #[test]
    fn test_event_display() {
        let event = Event {
            meta: EventMetadata {
                seq: 3,
                timestamp: SystemTime::now(),
                event_type: "pane.bell".to_string(),
                correlation_id: None,
            },
            data: EventData::PaneBell {
                pane_id: PaneId::new(),
                window_id: WindowId::new(),
                session_id: SessionId::new(),
            },
        };

        let display = format!("{event}");
        assert!(display.contains("[seq=3]"));
        assert!(display.contains("pane.bell"));
    }

    #[test]
    fn test_client_id_newtype() {
        let a = ClientId::new();
        let b = ClientId::new();
        assert_ne!(a, b);

        // Copy semantics.
        let c = a;
        assert_eq!(a, c);

        // Display.
        let s = a.to_string();
        assert_eq!(s.len(), 36); // UUID v4 format
    }
}
