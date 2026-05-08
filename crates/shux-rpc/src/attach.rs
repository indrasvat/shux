//! Attach-session protocol shared by daemon and client.
//!
//! After the JSON-RPC handshake (`AttachHello` → `AttachReady`/`AttachError`),
//! the connection enters a streaming mode where both sides exchange
//! length-prefixed JSON frames using the same codec as the rest of the
//! RPC system. Messages are tagged enums.
//!
//! The protocol is intentionally JSON+base64 to keep wire debugging easy
//! — local UDS bandwidth is not a concern.
//!
//! ### Server → Client
//! - `Render { data }` — ANSI bytes to dump straight onto the client's terminal.
//! - `Bell` — beep, let the local terminal handle it.
//! - `SessionEnded` — session destroyed; client should exit cleanly.
//! - `DetachAck` — server acknowledges a client-initiated detach.
//! - `Notice { text }` — transient banner.
//!
//! ### Client → Server
//! - `Input { data }` — bytes to forward to the focused pane's PTY.
//! - `Resize { cols, rows }` — the host terminal was resized.
//! - `Action { kind, args }` — handled keybinding, dispatched server-side.
//! - `Detach` — client requested detach; server stops streaming and closes.

use serde::{Deserialize, Serialize};

pub const ATTACH_PROTOCOL_VERSION: u32 = 1;

/// Initial handshake from client. Sent as a single framed JSON object on
/// connection open.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachHello {
    pub protocol: u32,
    pub session_name: Option<String>,
    pub cols: u16,
    pub rows: u16,
    pub client_version: String,
}

/// Daemon's reply to the hello.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttachReady {
    Ok {
        session_id: String,
        session_name: String,
        active_window_id: String,
        active_pane_id: String,
        protocol: u32,
    },
    Error {
        code: String,
        message: String,
    },
}

/// Frames sent from daemon to client during a streaming attach session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AttachServerFrame {
    /// Raw ANSI bytes — base64-encoded — to write directly to the
    /// client's terminal. Used for the entire render output.
    Render { data: String },
    /// Beep — write `\x07` locally.
    Bell,
    /// Banner text to flash (e.g., notification overlay).
    Notice { text: String },
    /// Server confirms the client's detach request.
    DetachAck,
    /// Session no longer exists; client should exit.
    SessionEnded { reason: String },
    /// Heartbeat (no-op) — keeps the connection alive in case the user is
    /// idle and we want to detect a dead client.
    Ping,
}

/// Frames sent from client to daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AttachClientFrame {
    /// Raw key bytes (already encoded by the client) for the focused pane.
    Input { data: String },
    /// Host terminal was resized.
    Resize { cols: u16, rows: u16 },
    /// Client-handled keybinding.
    Action {
        kind: ActionKind,
        #[serde(default)]
        args: ActionArgs,
    },
    /// Client wants to detach.
    Detach,
    /// Heartbeat reply.
    Pong,
}

/// Handled actions the client forwards to the daemon. These map 1:1 to
/// existing RPC mutations but are routed in-band over the attach socket
/// so the daemon doesn't need to multiplex with the main RPC socket
/// during a render cycle.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    /// Split the focused pane (smart split — wider→vertical, taller→horizontal).
    SplitSmart,
    /// Split the focused pane vertically.
    SplitVertical,
    /// Split the focused pane horizontally.
    SplitHorizontal,
    /// Move focus in a direction.
    FocusUp,
    FocusDown,
    FocusLeft,
    FocusRight,
    /// Cycle to next/prev pane in tree order.
    FocusNext,
    FocusPrev,
    /// Toggle zoom of focused pane.
    ToggleZoom,
    /// Kill focused pane.
    KillPane,
    /// Create new window.
    NewWindow,
    /// Switch window.
    NextWindow,
    PrevWindow,
    /// Resize focused pane.
    ResizeLeft,
    ResizeRight,
    ResizeUp,
    ResizeDown,
    /// Force a full redraw (debug aid).
    Redraw,
}

/// Optional per-action arguments. Not all actions use all fields.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActionArgs {
    #[serde(default)]
    pub name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hello_roundtrip() {
        let hello = AttachHello {
            protocol: ATTACH_PROTOCOL_VERSION,
            session_name: Some("dev".into()),
            cols: 120,
            rows: 30,
            client_version: "0.1.0".into(),
        };
        let s = serde_json::to_string(&hello).unwrap();
        let back: AttachHello = serde_json::from_str(&s).unwrap();
        assert_eq!(back.cols, 120);
        assert_eq!(back.rows, 30);
        assert_eq!(back.session_name.as_deref(), Some("dev"));
    }

    #[test]
    fn test_ready_ok_and_error() {
        let ok = AttachReady::Ok {
            session_id: "1".into(),
            session_name: "dev".into(),
            active_window_id: "w".into(),
            active_pane_id: "p".into(),
            protocol: ATTACH_PROTOCOL_VERSION,
        };
        let s = serde_json::to_string(&ok).unwrap();
        assert!(s.contains("\"kind\":\"ok\""));
        let _: AttachReady = serde_json::from_str(&s).unwrap();

        let err = AttachReady::Error {
            code: "not_found".into(),
            message: "no session".into(),
        };
        let s = serde_json::to_string(&err).unwrap();
        assert!(s.contains("\"kind\":\"error\""));
    }

    #[test]
    fn test_server_frame_render_roundtrip() {
        let f = AttachServerFrame::Render {
            data: "AAAA".into(),
        };
        let s = serde_json::to_string(&f).unwrap();
        assert!(s.contains("\"type\":\"render\""));
        let back: AttachServerFrame = serde_json::from_str(&s).unwrap();
        match back {
            AttachServerFrame::Render { data } => assert_eq!(data, "AAAA"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_client_frame_action_roundtrip() {
        let f = AttachClientFrame::Action {
            kind: ActionKind::ToggleZoom,
            args: ActionArgs::default(),
        };
        let s = serde_json::to_string(&f).unwrap();
        let back: AttachClientFrame = serde_json::from_str(&s).unwrap();
        match back {
            AttachClientFrame::Action { kind, .. } => assert_eq!(kind, ActionKind::ToggleZoom),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_action_kind_serde_uses_snake_case() {
        let s = serde_json::to_string(&ActionKind::SplitSmart).unwrap();
        assert_eq!(s, "\"split_smart\"");
        let s = serde_json::to_string(&ActionKind::FocusLeft).unwrap();
        assert_eq!(s, "\"focus_left\"");
    }
}
