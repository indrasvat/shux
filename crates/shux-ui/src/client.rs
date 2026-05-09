//! TUI client: message types, key encoding, and event loop skeleton.
//!
//! This module provides the client-side logic for connecting to the shux
//! daemon, encoding keyboard input as PTY byte sequences, and managing
//! the attach/detach lifecycle. The full event loop wiring to the daemon
//! is completed in tasks 011/012.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};

// -- Message types -----------------------------------------------------------

/// Messages the client can send to the daemon.
#[derive(Debug, Serialize)]
#[serde(tag = "method", content = "params")]
pub enum ClientRequest {
    #[serde(rename = "session.attach")]
    Attach {
        session_name: String,
        cols: u16,
        rows: u16,
    },

    #[serde(rename = "session.detach")]
    Detach,

    #[serde(rename = "pane.send_keys")]
    SendKeys { pane_id: String, data: Vec<u8> },

    #[serde(rename = "client.resize")]
    Resize { cols: u16, rows: u16 },
}

/// Messages the daemon sends to an attached client.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum DaemonMessage {
    /// Pane output data to render.
    #[serde(rename = "pane_output")]
    PaneOutput { pane_id: String, data: Vec<u8> },

    /// Attached successfully. Contains the initial state.
    #[serde(rename = "attached")]
    Attached {
        session_id: String,
        active_pane_id: String,
        initial_grid: Option<Vec<u8>>,
    },

    /// Detach confirmed.
    #[serde(rename = "detached")]
    Detached,

    /// Session was destroyed (e.g., last pane exited).
    #[serde(rename = "session_ended")]
    SessionEnded { session_id: String },

    /// Error from the daemon.
    #[serde(rename = "error")]
    Error { code: i32, message: String },
}

// -- Config / Exit -----------------------------------------------------------

/// Configuration for the TUI client.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Path to the daemon's Unix domain socket.
    pub socket_path: String,
    /// Session name to attach to.
    pub session_name: String,
    /// Prefix key for keybindings (default: Ctrl+Space).
    pub prefix_key: KeyEvent,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            socket_path: String::new(),
            session_name: String::from("default"),
            prefix_key: KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL),
        }
    }
}

/// The reason the client exited its main loop.
#[derive(Debug)]
pub enum ExitReason {
    /// User pressed the detach key sequence (Prefix + d).
    Detached,
    /// The session ended (last pane exited).
    SessionEnded,
    /// The daemon connection was lost.
    ConnectionLost,
    /// An error occurred.
    Error(String),
}

// -- Key encoding ------------------------------------------------------------

/// Encode a crossterm KeyEvent into bytes suitable for sending to a PTY.
/// This handles the mapping from crossterm's key representation to the
/// actual byte sequences expected by terminal applications.
pub fn encode_key_event(key: KeyEvent) -> Vec<u8> {
    match key.code {
        KeyCode::Char(c) => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                // Ctrl+A..Z maps to 0x01..0x1A
                if c.is_ascii_lowercase() {
                    vec![(c as u8) - b'a' + 1]
                } else if c.is_ascii_uppercase() {
                    vec![(c.to_ascii_lowercase() as u8) - b'a' + 1]
                } else if c == ' ' {
                    vec![0] // Ctrl+Space = NUL
                } else {
                    c.to_string().into_bytes()
                }
            } else if key.modifiers.contains(KeyModifiers::ALT) {
                // Alt+key = ESC followed by the key
                let mut bytes = vec![0x1b];
                bytes.extend(c.to_string().bytes());
                bytes
            } else {
                c.to_string().into_bytes()
            }
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::F(n) => encode_function_key(n),
        _ => Vec::new(),
    }
}

/// Encode function key F1-F12 as ANSI escape sequences.
fn encode_function_key(n: u8) -> Vec<u8> {
    match n {
        1 => b"\x1bOP".to_vec(),
        2 => b"\x1bOQ".to_vec(),
        3 => b"\x1bOR".to_vec(),
        4 => b"\x1bOS".to_vec(),
        5 => b"\x1b[15~".to_vec(),
        6 => b"\x1b[17~".to_vec(),
        7 => b"\x1b[18~".to_vec(),
        8 => b"\x1b[19~".to_vec(),
        9 => b"\x1b[20~".to_vec(),
        10 => b"\x1b[21~".to_vec(),
        11 => b"\x1b[23~".to_vec(),
        12 => b"\x1b[24~".to_vec(),
        _ => Vec::new(),
    }
}

/// Parse a resize event from the internal encoding.
///
/// Resize events are encoded as `\x1b[RESIZE:cols:rows` by the input
/// reader, so they can be passed through the same channel as key data.
pub fn parse_resize_event(data: &[u8]) -> Option<(u16, u16)> {
    let s = std::str::from_utf8(data).ok()?;
    let stripped = s.strip_prefix("\x1b[RESIZE:")?;
    let parts: Vec<&str> = stripped.split(':').collect();
    if parts.len() == 2 {
        let cols = parts[0].parse().ok()?;
        let rows = parts[1].parse().ok()?;
        Some((cols, rows))
    } else {
        None
    }
}

/// Attempt to reconstruct a KeyEvent from encoded bytes.
/// This is used for prefix key detection in the client event loop.
///
/// This is a simplified parser for the most common cases.
/// Full key parsing is handled by the input decoder (task 006).
pub fn parse_key_from_bytes(data: &[u8]) -> Option<KeyEvent> {
    if data.is_empty() {
        return None;
    }

    match data {
        // Specific byte values must come before the ctrl range catch-all,
        // since \r (0x0d=13) and \t (0x09=9) fall in the 1..=26 range.
        [0] => Some(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL)),
        [b'\r'] => Some(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        [b'\t'] => Some(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
        [0x7f] => Some(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
        [0x1b] => Some(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        &[b] if (1..=26).contains(&b) => Some(KeyEvent::new(
            KeyCode::Char((b'a' + b - 1) as char),
            KeyModifiers::CONTROL,
        )),
        &[0x1b, b] => Some(KeyEvent::new(KeyCode::Char(b as char), KeyModifiers::ALT)),
        _ => {
            if data.len() == 1 && data[0].is_ascii() {
                Some(KeyEvent::new(
                    KeyCode::Char(data[0] as char),
                    KeyModifiers::NONE,
                ))
            } else {
                None
            }
        }
    }
}

/// Run the TUI client. This is the main entry point for `shux attach`.
///
/// This function:
/// 1. Installs the panic hook for terminal safety
/// 2. Enters raw mode + alternate screen
/// 3. Connects to the daemon
/// 4. Runs the input/output event loop
/// 5. Restores the terminal on exit
///
/// Returns the reason the client exited.
///
/// **Note:** The full event loop is wired up in tasks 011/012 when the
/// daemon has session management. Currently this is a compilable skeleton.
pub async fn run_client(config: ClientConfig) -> Result<ExitReason, anyhow::Error> {
    use crate::terminal;

    // Install panic hook before entering raw mode
    terminal::install_panic_hook();

    // Enter TUI mode
    let mut guard = terminal::TerminalGuard::enter()?;
    let (cols, rows) = terminal::TerminalGuard::size()?;

    tracing::info!(cols, rows, "TUI client starting");

    // TODO(task-011): Connect to daemon via UDS
    //   let stream = UnixStream::connect(&config.socket_path).await?;
    //   let (reader, writer) = stream.into_split();

    // TODO(task-011): Send session.attach and wait for Attached response
    //   let active_pane_id = ...;

    // TODO(task-011): Create compositor
    //   let compositor_config = CompositorConfig::default();
    //   let mut compositor = RenderCompositor::new(cols, rows, ...);

    // State tracking
    let _prefix_active = false;
    let _prefix_key = config.prefix_key;

    // TODO(task-011): Main event loop with tokio::select!
    //   - Input reader (crossterm events -> encode_key_event -> daemon)
    //   - Daemon message handler (PaneOutput -> VT -> compositor)
    //   - Shutdown signal handler
    let exit_reason = ExitReason::Detached;

    // Cleanup: leave TUI mode
    guard.leave()?;

    match &exit_reason {
        ExitReason::Detached => {
            println!("[detached from session '{}']", config.session_name);
        }
        ExitReason::SessionEnded => {
            println!("[session '{}' ended]", config.session_name);
        }
        ExitReason::ConnectionLost => {
            eprintln!("[connection to daemon lost]");
        }
        ExitReason::Error(msg) => {
            eprintln!("[error: {msg}]");
        }
    }

    let _ = (cols, rows); // suppress unused warnings until wired
    Ok(exit_reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn test_encode_regular_char() {
        let key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(encode_key_event(key), b"a");
    }

    #[test]
    fn test_encode_uppercase_char() {
        let key = KeyEvent::new(KeyCode::Char('Z'), KeyModifiers::NONE);
        assert_eq!(encode_key_event(key), b"Z");
    }

    #[test]
    fn test_encode_ctrl_c() {
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(encode_key_event(key), vec![3]); // ETX
    }

    #[test]
    fn test_encode_ctrl_a() {
        let key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL);
        assert_eq!(encode_key_event(key), vec![1]); // SOH
    }

    #[test]
    fn test_encode_ctrl_z() {
        let key = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(encode_key_event(key), vec![26]); // SUB
    }

    #[test]
    fn test_encode_ctrl_uppercase() {
        let key = KeyEvent::new(KeyCode::Char('C'), KeyModifiers::CONTROL);
        assert_eq!(encode_key_event(key), vec![3]); // Same as ctrl+c
    }

    #[test]
    fn test_encode_ctrl_space() {
        let key = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL);
        assert_eq!(encode_key_event(key), vec![0]); // NUL
    }

    #[test]
    fn test_encode_alt_key() {
        let key = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::ALT);
        assert_eq!(encode_key_event(key), vec![0x1b, b'h']);
    }

    #[test]
    fn test_encode_alt_uppercase() {
        let key = KeyEvent::new(KeyCode::Char('H'), KeyModifiers::ALT);
        assert_eq!(encode_key_event(key), vec![0x1b, b'H']);
    }

    #[test]
    fn test_encode_enter() {
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(encode_key_event(key), vec![b'\r']);
    }

    #[test]
    fn test_encode_backspace() {
        let key = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(encode_key_event(key), vec![0x7f]);
    }

    #[test]
    fn test_encode_tab() {
        let key = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(encode_key_event(key), vec![b'\t']);
    }

    #[test]
    fn test_encode_escape() {
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(encode_key_event(esc), vec![0x1b]);
    }

    #[test]
    fn test_encode_arrow_keys() {
        let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(encode_key_event(up), b"\x1b[A");

        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(encode_key_event(down), b"\x1b[B");

        let right = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(encode_key_event(right), b"\x1b[C");

        let left = KeyEvent::new(KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(encode_key_event(left), b"\x1b[D");
    }

    #[test]
    fn test_encode_navigation_keys() {
        assert_eq!(
            encode_key_event(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE)),
            b"\x1b[H"
        );
        assert_eq!(
            encode_key_event(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)),
            b"\x1b[F"
        );
        assert_eq!(
            encode_key_event(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE)),
            b"\x1b[5~"
        );
        assert_eq!(
            encode_key_event(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE)),
            b"\x1b[6~"
        );
        assert_eq!(
            encode_key_event(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE)),
            b"\x1b[3~"
        );
        assert_eq!(
            encode_key_event(KeyEvent::new(KeyCode::Insert, KeyModifiers::NONE)),
            b"\x1b[2~"
        );
    }

    #[test]
    fn test_encode_function_keys() {
        let f1 = KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE);
        assert_eq!(encode_key_event(f1), b"\x1bOP");

        let f2 = KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE);
        assert_eq!(encode_key_event(f2), b"\x1bOQ");

        let f3 = KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE);
        assert_eq!(encode_key_event(f3), b"\x1bOR");

        let f4 = KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE);
        assert_eq!(encode_key_event(f4), b"\x1bOS");

        let f5 = KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE);
        assert_eq!(encode_key_event(f5), b"\x1b[15~");

        let f12 = KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE);
        assert_eq!(encode_key_event(f12), b"\x1b[24~");
    }

    #[test]
    fn test_encode_unknown_function_key() {
        let f13 = KeyEvent::new(KeyCode::F(13), KeyModifiers::NONE);
        assert!(encode_key_event(f13).is_empty());
    }

    #[test]
    fn test_parse_resize_event() {
        let data = b"\x1b[RESIZE:120:40";
        assert_eq!(parse_resize_event(data), Some((120, 40)));
    }

    #[test]
    fn test_parse_resize_event_small() {
        let data = b"\x1b[RESIZE:80:24";
        assert_eq!(parse_resize_event(data), Some((80, 24)));
    }

    #[test]
    fn test_parse_resize_event_invalid() {
        assert_eq!(parse_resize_event(b"not a resize"), None);
        assert_eq!(parse_resize_event(b"\x1b[RESIZE:abc:40"), None);
        assert_eq!(parse_resize_event(b"\x1b[RESIZE:120"), None);
        assert_eq!(parse_resize_event(b""), None);
    }

    #[test]
    fn test_parse_key_from_bytes_ctrl_space() {
        let bytes = vec![0u8];
        let key = parse_key_from_bytes(&bytes).unwrap();
        assert_eq!(key.code, KeyCode::Char(' '));
        assert!(key.modifiers.contains(KeyModifiers::CONTROL));
    }

    #[test]
    fn test_parse_key_from_bytes_ctrl_a() {
        let bytes = vec![1u8];
        let key = parse_key_from_bytes(&bytes).unwrap();
        assert_eq!(key.code, KeyCode::Char('a'));
        assert!(key.modifiers.contains(KeyModifiers::CONTROL));
    }

    #[test]
    fn test_parse_key_from_bytes_ctrl_z() {
        let bytes = vec![26u8];
        let key = parse_key_from_bytes(&bytes).unwrap();
        assert_eq!(key.code, KeyCode::Char('z'));
        assert!(key.modifiers.contains(KeyModifiers::CONTROL));
    }

    #[test]
    fn test_parse_key_from_bytes_alt() {
        let bytes = vec![0x1b, b'd'];
        let key = parse_key_from_bytes(&bytes).unwrap();
        assert_eq!(key.code, KeyCode::Char('d'));
        assert!(key.modifiers.contains(KeyModifiers::ALT));
    }

    #[test]
    fn test_parse_key_from_bytes_escape() {
        let bytes = vec![0x1b];
        let key = parse_key_from_bytes(&bytes).unwrap();
        assert_eq!(key.code, KeyCode::Esc);
        assert_eq!(key.modifiers, KeyModifiers::NONE);
    }

    #[test]
    fn test_parse_key_from_bytes_enter() {
        let bytes = vec![b'\r'];
        let key = parse_key_from_bytes(&bytes).unwrap();
        assert_eq!(key.code, KeyCode::Enter);
    }

    #[test]
    fn test_parse_key_from_bytes_backspace() {
        let bytes = vec![0x7f];
        let key = parse_key_from_bytes(&bytes).unwrap();
        assert_eq!(key.code, KeyCode::Backspace);
    }

    #[test]
    fn test_parse_key_from_bytes_regular_char() {
        let bytes = vec![b'x'];
        let key = parse_key_from_bytes(&bytes).unwrap();
        assert_eq!(key.code, KeyCode::Char('x'));
        assert_eq!(key.modifiers, KeyModifiers::NONE);
    }

    #[test]
    fn test_parse_key_from_bytes_empty() {
        assert!(parse_key_from_bytes(&[]).is_none());
    }

    #[test]
    fn test_parse_key_from_bytes_multibyte_none() {
        // Multi-byte sequences that aren't recognized return None
        assert!(parse_key_from_bytes(b"\x1b[A").is_none());
    }

    #[test]
    fn test_prefix_key_default_is_ctrl_space() {
        let config = ClientConfig::default();
        assert_eq!(config.prefix_key.code, KeyCode::Char(' '));
        assert!(config.prefix_key.modifiers.contains(KeyModifiers::CONTROL));
    }

    #[test]
    fn test_detach_sequence() {
        // Verify that the prefix key followed by 'd' would trigger detach.
        // We test the encoding: Ctrl+Space encodes to [0], then 'd' encodes to [b'd'].
        let prefix = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL);
        let prefix_bytes = encode_key_event(prefix);
        assert_eq!(prefix_bytes, vec![0]);

        let d_key = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE);
        let d_bytes = encode_key_event(d_key);
        assert_eq!(d_bytes, b"d");
    }

    #[test]
    fn test_exit_reason_debug() {
        let reasons = vec![
            ExitReason::Detached,
            ExitReason::SessionEnded,
            ExitReason::ConnectionLost,
            ExitReason::Error("test".to_string()),
        ];
        for reason in reasons {
            let _ = format!("{reason:?}");
        }
    }

    #[test]
    fn test_roundtrip_ctrl_keys() {
        // Encode then parse should roundtrip for ctrl keys, except:
        // - Ctrl+I (0x09) parses as Tab
        // - Ctrl+M (0x0d) parses as Enter
        // - Ctrl+Space (0x00) parses as Ctrl+Space (handled separately)
        for c in b'a'..=b'z' {
            let key = KeyEvent::new(KeyCode::Char(c as char), KeyModifiers::CONTROL);
            let bytes = encode_key_event(key);
            let parsed = parse_key_from_bytes(&bytes).unwrap();
            match c {
                b'i' => assert_eq!(parsed.code, KeyCode::Tab),
                b'm' => assert_eq!(parsed.code, KeyCode::Enter),
                _ => {
                    assert_eq!(parsed.code, KeyCode::Char(c as char));
                    assert!(parsed.modifiers.contains(KeyModifiers::CONTROL));
                }
            }
        }
    }

    #[test]
    fn test_roundtrip_regular_ascii() {
        // Regular printable ASCII should roundtrip
        for c in b' '..=b'~' {
            let key = KeyEvent::new(KeyCode::Char(c as char), KeyModifiers::NONE);
            let bytes = encode_key_event(key);
            let parsed = parse_key_from_bytes(&bytes);
            // Space through tilde should parse; special chars may not
            if bytes.len() == 1 && bytes[0].is_ascii() {
                let p = parsed.unwrap();
                assert_eq!(p.code, KeyCode::Char(c as char));
            }
        }
    }

    #[test]
    fn test_client_request_serialize_attach() {
        let req = ClientRequest::Attach {
            session_name: "test".to_string(),
            cols: 80,
            rows: 24,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("session.attach"));
        assert!(json.contains("\"cols\":80"));
    }

    #[test]
    fn test_client_request_serialize_detach() {
        let req = ClientRequest::Detach;
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("session.detach"));
    }

    #[test]
    fn test_daemon_message_deserialize() {
        let json = r#"{"type":"detached"}"#;
        let msg: DaemonMessage = serde_json::from_str(json).unwrap();
        assert!(matches!(msg, DaemonMessage::Detached));
    }

    #[test]
    fn test_daemon_message_error() {
        let json = r#"{"type":"error","code":-1,"message":"not found"}"#;
        let msg: DaemonMessage = serde_json::from_str(json).unwrap();
        match msg {
            DaemonMessage::Error { code, message } => {
                assert_eq!(code, -1);
                assert_eq!(message, "not found");
            }
            _ => panic!("expected Error variant"),
        }
    }
}
