# 010 — Minimal TUI Client

**Status:** Pending
**Depends On:** 004, 008, 009
**Parallelizable With:** 011

---

## Problem

The TUI client is the user-facing half of shux's client/server architecture. Without it, the daemon has no way to display pane output on the user's terminal or accept keyboard input. This task builds the minimal viable TUI client: connect to the daemon over Unix domain socket, enter raw mode, switch to the alternate screen, receive pane output and render it via the compositor (task 009), forward keyboard input to the daemon (which forwards it to the PTY), and support clean detach and graceful error recovery.

This task is single-pane only. Multi-pane rendering, layout-aware navigation, and status bar come in later tasks (017, 018, 026). The focus here is getting the basic attach/render/input/detach cycle working end-to-end.

## PRD Reference

- **section 4.1** — Single binary with subcommands; `shux attach -s <name>` attaches the TUI client
- **section 4.2** — System diagram: clients connect to daemon via UDS, receive PTY output, send input
- **section 4.4** — `RenderCompositor` composes VirtualTerminal grids into client output
- **section 4.4** — `ClientCaps` negotiated per attached client (basic version in this task)
- **section 9.2** — `Prefix + d` detaches (Ctrl+Space d)
- **section 14.1** — Keypress to visible update: p50 <= 8ms, p99 <= 25ms
- **section 15.2** — crossterm 0.29 for raw mode, alternate screen, keyboard events

---

## Files to Create

- `crates/shux-ui/src/client.rs` — TUI client: connect, event loop, attach/detach lifecycle
- `crates/shux-ui/src/terminal.rs` — Terminal state management: raw mode, alternate screen, cleanup

## Files to Modify

- `crates/shux-ui/Cargo.toml` — Add dependencies: tokio, shux-rpc, nix (for SIGWINCH)
- `crates/shux-ui/src/lib.rs` — Re-export client and terminal modules

---

## Execution Steps

### Step 1: Build the terminal state manager

The terminal module manages the host terminal's state transitions: entering raw mode, switching to the alternate screen, and restoring everything on exit (including crashes). This is critical -- a failure to restore the terminal leaves the user's shell in a broken state.

```rust
// crates/shux-ui/src/terminal.rs

use std::io::{self, Write};

use crossterm::{
    cursor,
    event::{
        DisableMouseCapture, EnableMouseCapture,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{
        self, disable_raw_mode, enable_raw_mode,
        EnterAlternateScreen, LeaveAlternateScreen,
    },
};

/// Tracks the state of the host terminal so we can restore it correctly
/// on exit, even after a panic.
pub struct TerminalGuard {
    raw_mode_enabled: bool,
    alternate_screen: bool,
    mouse_capture: bool,
    kitty_keyboard: bool,
}

impl TerminalGuard {
    /// Enter the TUI state: raw mode + alternate screen.
    /// Returns a guard that will restore the terminal on drop.
    pub fn enter() -> io::Result<Self> {
        let mut guard = Self {
            raw_mode_enabled: false,
            alternate_screen: false,
            mouse_capture: false,
            kitty_keyboard: false,
        };

        // Order matters: enable raw mode first so that escape sequences
        // for alternate screen are not echoed as text.
        enable_raw_mode()?;
        guard.raw_mode_enabled = true;

        // Switch to alternate screen (preserves the user's scrollback)
        execute!(io::stdout(), EnterAlternateScreen)?;
        guard.alternate_screen = true;

        // Enable mouse capture for click-to-focus (can be toggled later)
        execute!(io::stdout(), EnableMouseCapture)?;
        guard.mouse_capture = true;

        // Try to enable Kitty keyboard protocol for improved key detection.
        // This silently fails on terminals that do not support it.
        let result = execute!(
            io::stdout(),
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
            )
        );
        if result.is_ok() {
            guard.kitty_keyboard = true;
        }

        Ok(guard)
    }

    /// Restore the terminal to its original state. This is also called
    /// automatically by Drop, but calling it explicitly allows error handling.
    pub fn leave(&mut self) -> io::Result<()> {
        if self.kitty_keyboard {
            let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
            self.kitty_keyboard = false;
        }

        if self.mouse_capture {
            let _ = execute!(io::stdout(), DisableMouseCapture);
            self.mouse_capture = false;
        }

        if self.alternate_screen {
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            self.alternate_screen = false;
        }

        if self.raw_mode_enabled {
            let _ = disable_raw_mode();
            self.raw_mode_enabled = false;
        }

        // Show cursor in case it was hidden during rendering
        let _ = execute!(io::stdout(), cursor::Show);

        Ok(())
    }

    /// Query the current terminal size.
    pub fn size() -> io::Result<(u16, u16)> {
        terminal::size()
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Best-effort restore on drop. Errors are silently ignored because
        // we may be in a panic handler and cannot propagate errors.
        let _ = self.leave();
    }
}

/// Install a panic hook that restores the terminal before printing the
/// panic message. Without this, a panic leaves the terminal in raw mode
/// and the error message is invisible.
pub fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Best-effort terminal restore
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        let _ = execute!(io::stdout(), DisableMouseCapture);
        let _ = execute!(io::stdout(), cursor::Show);

        // Now print the panic info on the restored terminal
        default_hook(info);
    }));
}
```

### Step 2: Define the client-daemon message protocol

The client communicates with the daemon over the JSON-RPC connection established in task 008. Define the message types the client sends and receives. In M0, the protocol is simple:

- Client -> Daemon: `session.attach {session_id, cols, rows}` to attach
- Client -> Daemon: `pane.send_keys {pane_id, data}` to forward input
- Client -> Daemon: `session.detach {}` to detach
- Client -> Daemon: `client.resize {cols, rows}` to notify of terminal resize
- Daemon -> Client: pane output (streamed as events or direct frames)

```rust
// crates/shux-ui/src/client.rs — message types

use serde::{Deserialize, Serialize};

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
    SendKeys {
        pane_id: String,
        data: Vec<u8>,
    },

    #[serde(rename = "client.resize")]
    Resize {
        cols: u16,
        rows: u16,
    },
}

/// Messages the daemon sends to an attached client.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum DaemonMessage {
    /// Pane output data to render.
    #[serde(rename = "pane_output")]
    PaneOutput {
        pane_id: String,
        data: Vec<u8>,
    },

    /// Attached successfully. Contains the initial state.
    #[serde(rename = "attached")]
    Attached {
        session_id: String,
        active_pane_id: String,
        /// Serialized grid state for initial render
        initial_grid: Option<Vec<u8>>,
    },

    /// Detach confirmed.
    #[serde(rename = "detached")]
    Detached,

    /// Session was destroyed (e.g., last pane exited).
    #[serde(rename = "session_ended")]
    SessionEnded {
        session_id: String,
    },

    /// Error from the daemon.
    #[serde(rename = "error")]
    Error {
        code: i32,
        message: String,
    },
}
```

### Step 3: Build the TUI client event loop

The client runs two concurrent tasks:
1. **Input reader**: reads crossterm events (keyboard, mouse, resize) and sends them to the daemon
2. **Output renderer**: receives pane output from the daemon and renders via the compositor

These run in a tokio select loop with cancellation support.

```rust
// crates/shux-ui/src/client.rs

use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::buffer::RenderCell;
use crate::compositor::{CompositorConfig, RenderCompositor};
use crate::terminal::{self, TerminalGuard};

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
pub async fn run_client(config: ClientConfig) -> Result<ExitReason, anyhow::Error> {
    // Install panic hook before entering raw mode
    terminal::install_panic_hook();

    // Enter TUI mode
    let mut guard = TerminalGuard::enter()?;
    let (cols, rows) = TerminalGuard::size()?;

    info!(cols, rows, "TUI client starting");

    // Connect to daemon
    let stream = UnixStream::connect(&config.socket_path).await?;
    let (reader, writer) = stream.into_split();

    // Create channels for internal communication
    let (input_tx, mut input_rx) = mpsc::channel::<Vec<u8>>(256);
    let (output_tx, mut output_rx) = mpsc::channel::<DaemonMessage>(256);

    // Create the compositor
    let compositor_config = CompositorConfig::default();
    let stdout = io::stdout();
    let mut compositor = RenderCompositor::new(cols, rows, stdout.lock(), compositor_config);
    compositor.clear()?;

    // Register this client with the daemon before entering the main loop.
    // 1. Send `session.attach { session_id, cols, rows }`.
    // 2. Wait for an `Attached` response carrying `active_pane_id`.
    // 3. Store `active_pane_id` for later `pane.send_keys` calls.
    let mut active_pane_id = send_attach_and_wait_for_ack(
        &config.session_name,
        cols,
        rows,
        writer,
        &mut output_rx,
    ).await?;

    // State tracking
    let mut prefix_active = false;
    let prefix_key = config.prefix_key;

    // Main event loop
    let exit_reason = loop {
        tokio::select! {
            // Handle crossterm events (keyboard input, resize, mouse)
            _ = tokio::task::spawn_blocking({
                let input_tx = input_tx.clone();
                move || -> io::Result<()> {
                    // Poll for crossterm events with a 50ms timeout.
                    // This allows the select loop to check other channels.
                    if event::poll(Duration::from_millis(50))? {
                        if let Ok(evt) = event::read() {
                            match evt {
                                Event::Key(key) => {
                                    let _ = input_tx.blocking_send(
                                        encode_key_event(key)
                                    );
                                }
                                Event::Resize(cols, rows) => {
                                    // Encode resize as a special message
                                    let msg = format!("\x1b[RESIZE:{cols}:{rows}");
                                    let _ = input_tx.blocking_send(msg.into_bytes());
                                }
                                Event::Mouse(_) => {
                                    // Mouse support is basic in M0; expand in task 020
                                }
                                _ => {}
                            }
                        }
                    }
                    Ok(())
                }
            }) => {}

            // Handle input from the crossterm reader
            Some(data) = input_rx.recv() => {
                // Check for resize events (encoded as special sequence)
                if let Some(resize) = parse_resize_event(&data) {
                    let (new_cols, new_rows) = resize;
                    compositor.resize(new_cols, new_rows);
                    send_resize(&mut active_pane_id, new_cols, new_rows).await?;
                    debug!(new_cols, new_rows, "Terminal resized");
                    continue;
                }

                // Check for prefix key handling
                if let Some(key) = parse_key_from_bytes(&data) {
                    if prefix_active {
                        prefix_active = false;
                        match key.code {
                            KeyCode::Char('d') => {
                                // Detach
                                info!("Detach requested");
                                send_detach().await?;
                                break ExitReason::Detached;
                            }
                            _ => {
                                // Unknown prefix command; forward the key
                                send_keys(&active_pane_id, encode_key_for_pty(key)).await?;
                            }
                        }
                    } else if key == prefix_key {
                        prefix_active = true;
                        continue;
                    } else {
                        // Regular input: forward to daemon
                        send_keys(&active_pane_id, data).await?;
                    }
                }
            }

            // Handle messages from the daemon
            Some(msg) = output_rx.recv() => {
                match msg {
                    DaemonMessage::PaneOutput { data, .. } => {
                        // Feed bytes to VT, then render the new frame.
                        // 1. `vt.process(&data)` mutates grid/cursor state.
                        // 2. `compositor.render_frame(...)` draws diff.
                        // 3. If render fails, exit with Error to trigger terminal restore.
                        vt.process(&data);
                        compositor.render_frame(vt.render_view())?;
                    }
                    DaemonMessage::SessionEnded { .. } => {
                        break ExitReason::SessionEnded;
                    }
                    DaemonMessage::Detached => {
                        break ExitReason::Detached;
                    }
                    DaemonMessage::Error { message, .. } => {
                        error!(message, "Daemon error");
                        break ExitReason::Error(message);
                    }
                    _ => {}
                }
            }
        }
    };

    // Cleanup: leave TUI mode (guard.drop handles this, but explicit
    // for clarity and error handling)
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

    Ok(exit_reason)
}

/// Encode a crossterm KeyEvent into bytes suitable for sending to a PTY.
/// This handles the mapping from crossterm's key representation to the
/// actual byte sequences expected by terminal applications.
fn encode_key_event(key: KeyEvent) -> Vec<u8> {
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
fn parse_resize_event(data: &[u8]) -> Option<(u16, u16)> {
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
fn parse_key_from_bytes(data: &[u8]) -> Option<KeyEvent> {
    // This is a simplified parser for the most common cases.
    // Full key parsing is handled by the input decoder (task 006).
    if data.is_empty() {
        return None;
    }

    match data {
        [0] => Some(KeyEvent::new(
            KeyCode::Char(' '),
            KeyModifiers::CONTROL,
        )),
        [b] if *b >= 1 && *b <= 26 => Some(KeyEvent::new(
            KeyCode::Char((b'a' + b - 1) as char),
            KeyModifiers::CONTROL,
        )),
        [0x1b, b] => Some(KeyEvent::new(
            KeyCode::Char(*b as char),
            KeyModifiers::ALT,
        )),
        [0x1b] => Some(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        [b'\r'] => Some(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        [0x7f] => Some(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
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
```

### Step 4: Handle SIGWINCH (terminal resize)

Terminal resize is detected two ways:
1. **crossterm**: The `Event::Resize` event (which crossterm generates from SIGWINCH)
2. **Manual signal handler**: As a fallback

crossterm's event stream already handles SIGWINCH internally, so in the main event loop (Step 3), we handle `Event::Resize` by calling `compositor.resize()` and notifying the daemon. The daemon then resizes the PTY and VirtualTerminal.

```rust
// The resize flow is already implemented in the event loop above:
//
// 1. crossterm emits Event::Resize(cols, rows)
// 2. We encode it as a special internal message
// 3. The event loop calls compositor.resize(new_cols, new_rows)
// 4. The event loop sends a client.resize message to the daemon
// 5. The daemon calls pty.resize(new_cols, new_rows)
// 6. The daemon resizes the VirtualTerminal grid
// 7. The next pane output triggers a full re-render
```

### Step 5: Implement graceful cleanup

The `TerminalGuard` (Step 1) handles cleanup via RAII. But we also need to handle signals (SIGTERM, SIGINT) that might bypass normal cleanup.

```rust
// crates/shux-ui/src/terminal.rs (addition)

/// Set up signal handlers that trigger graceful shutdown.
/// Returns a future that resolves when a shutdown signal is received.
pub async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};

    let mut sigterm = signal(SignalKind::terminate())
        .expect("failed to install SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt())
        .expect("failed to install SIGINT handler");

    tokio::select! {
        _ = sigterm.recv() => {
            tracing::info!("Received SIGTERM");
        }
        _ = sigint.recv() => {
            tracing::info!("Received SIGINT");
        }
    }
}
```

### Step 6: Integrate shutdown signal into the client event loop

Add the shutdown signal as another branch in the `tokio::select!` loop:

```rust
// In the main event loop (run_client), add this branch:
//
//     _ = terminal::shutdown_signal() => {
//         info!("Shutdown signal received");
//         break ExitReason::Detached;
//     }
//
// This ensures SIGTERM/SIGINT trigger a clean detach with terminal restore.
```

### Step 7: Write the module structure and exports

```rust
// crates/shux-ui/src/lib.rs (updated)

//! shux-ui — TUI client: render compositor, terminal management, client connection

pub mod buffer;
pub mod client;
pub mod compositor;
pub mod render;
pub mod terminal;
```

### Step 8: Update Cargo.toml

```toml
# crates/shux-ui/Cargo.toml
[package]
name = "shux-ui"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
shux-vt = { path = "../shux-vt" }
shux-rpc = { path = "../shux-rpc" }
crossterm.workspace = true
tokio.workspace = true
serde.workspace = true
serde_json.workspace = true
tracing.workspace = true
anyhow.workspace = true
nix.workspace = true

[dev-dependencies]
tempfile.workspace = true
```

### Step 9: Write tests

The TUI client is inherently interactive and difficult to unit test (it requires a real terminal). Focus on testing the components that can be tested headlessly:

```rust
// crates/shux-ui/src/terminal.rs — tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_terminal_guard_is_send() {
        // Verify TerminalGuard can be sent across threads (needed for
        // tokio tasks)
        fn assert_send<T: Send>() {}
        assert_send::<TerminalGuard>();
    }

    #[test]
    fn test_terminal_size() {
        // This test may fail in CI without a real terminal, so we just
        // check it does not panic.
        let _ = TerminalGuard::size();
    }
}
```

```rust
// crates/shux-ui/src/client.rs — tests

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
    fn test_encode_ctrl_c() {
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(encode_key_event(key), vec![3]); // ETX
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
    fn test_encode_function_keys() {
        let f1 = KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE);
        assert_eq!(encode_key_event(f1), b"\x1bOP");

        let f5 = KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE);
        assert_eq!(encode_key_event(f5), b"\x1b[15~");

        let f12 = KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE);
        assert_eq!(encode_key_event(f12), b"\x1b[24~");
    }

    #[test]
    fn test_encode_escape() {
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(encode_key_event(esc), vec![0x1b]);
    }

    #[test]
    fn test_parse_resize_event() {
        let data = b"\x1b[RESIZE:120:40";
        assert_eq!(parse_resize_event(data), Some((120, 40)));
    }

    #[test]
    fn test_parse_resize_event_invalid() {
        assert_eq!(parse_resize_event(b"not a resize"), None);
        assert_eq!(parse_resize_event(b"\x1b[RESIZE:abc:40"), None);
        assert_eq!(parse_resize_event(b"\x1b[RESIZE:120"), None);
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
    fn test_parse_key_from_bytes_alt() {
        let bytes = vec![0x1b, b'd'];
        let key = parse_key_from_bytes(&bytes).unwrap();
        assert_eq!(key.code, KeyCode::Char('d'));
        assert!(key.modifiers.contains(KeyModifiers::ALT));
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
        // Just verify the enum variants exist and are Debug-printable
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
}
```

---

## Verification

### Functional

```bash
# Build the shux-ui crate (now includes client and terminal modules)
cargo build -p shux-ui

# Verify no clippy warnings
cargo clippy -p shux-ui -- -D warnings

# Verify formatting
cargo fmt -p shux-ui -- --check
```

### Tests

```bash
# Run all shux-ui tests
cargo nextest run -p shux-ui

# Expected passing tests:
#   terminal::tests::test_terminal_guard_is_send
#   terminal::tests::test_terminal_size
#   client::tests::test_encode_regular_char
#   client::tests::test_encode_ctrl_c
#   client::tests::test_encode_ctrl_space
#   client::tests::test_encode_alt_key
#   client::tests::test_encode_enter
#   client::tests::test_encode_backspace
#   client::tests::test_encode_arrow_keys
#   client::tests::test_encode_function_keys
#   client::tests::test_encode_escape
#   client::tests::test_parse_resize_event
#   client::tests::test_parse_resize_event_invalid
#   client::tests::test_parse_key_from_bytes_ctrl_space
#   client::tests::test_parse_key_from_bytes_ctrl_a
#   client::tests::test_parse_key_from_bytes_alt
#   client::tests::test_parse_key_from_bytes_regular_char
#   client::tests::test_parse_key_from_bytes_empty
#   client::tests::test_prefix_key_default_is_ctrl_space
#   client::tests::test_detach_sequence
#   client::tests::test_exit_reason_debug
#   (plus all tests from task 009: buffer, render, compositor)
```

### Manual Testing

After tasks 001, 004, 008, and 009 are also complete, perform an end-to-end manual test:

```bash
# 1. Start the daemon (or let it auto-start)
cargo run -p shux -- new -s test

# 2. The TUI client should attach automatically
# 3. Type commands (ls, echo hello, etc.) — output should appear
# 4. Resize the terminal window — content should reflow
# 5. Press Ctrl+Space then d — should detach cleanly
# 6. Verify the terminal is restored (not in raw mode, scrollback visible)

# 7. Reattach
cargo run -p shux -- attach -s test
# 8. Previous session should still be there
# 9. Press Ctrl+C in the shell — should send SIGINT to the running process, not exit shux
```

---

## Completion Criteria

- [ ] `crates/shux-ui/src/terminal.rs` implements `TerminalGuard` with RAII terminal state management
- [ ] `TerminalGuard::enter()` enables raw mode + alternate screen + mouse capture + Kitty keyboard (when available)
- [ ] `TerminalGuard::leave()` restores terminal state (also called automatically on drop)
- [ ] `install_panic_hook()` restores terminal before printing panic info
- [ ] `shutdown_signal()` returns a future that resolves on SIGTERM/SIGINT
- [ ] `crates/shux-ui/src/client.rs` implements the TUI client event loop
- [ ] Client sends `session.attach` at startup and records `active_pane_id` from the daemon response
- [ ] Client supports `Ctrl+Space d` detach sequence (prefix key + d)
- [ ] Client handles `Event::Resize` by resizing the compositor and notifying the daemon
- [ ] Pane output path is wired end-to-end (`pane.output` -> VT parser -> compositor render)
- [ ] `encode_key_event()` correctly maps crossterm KeyEvents to PTY byte sequences
- [ ] Key encoding covers: regular chars, Ctrl+A-Z, Ctrl+Space, Alt+key, Enter, Backspace, Tab, Esc, arrow keys, function keys F1-F12, Home, End, Page Up/Down, Insert, Delete
- [ ] `parse_key_from_bytes()` reconstructs KeyEvents for prefix detection
- [ ] Client prints status message on exit (detached, session ended, connection lost, error)
- [ ] All unit tests pass (key encoding, resize parsing, prefix detection)
- [ ] `cargo clippy -p shux-ui -- -D warnings` passes

---

## Commit Message
```
feat(ui): add minimal TUI client with terminal management and input handling

- TerminalGuard: RAII raw mode + alternate screen with crash-safe cleanup
- Panic hook that restores terminal before printing panic info
- TUI client event loop with tokio::select for input/output/signals
- Key encoding: crossterm KeyEvent to PTY byte sequences (Ctrl, Alt, arrows, F-keys)
- Prefix key system: Ctrl+Space as prefix, 'd' for detach
- Terminal resize handling via crossterm Event::Resize
- Graceful shutdown on SIGTERM/SIGINT
```

---

## Session Protocol

1. **Before starting:** Read task 009 (compositor) to understand the rendering API you will call. Read task 008 (JSON-RPC server) to understand the daemon connection protocol. Read task 004 (PTY manager) to understand how input reaches the PTY. Read task 006 (input decoder) to understand key encoding expectations.
2. **During:** Implement in order: `terminal.rs` (Step 1, 5) -> message types in `client.rs` (Step 2) -> event loop skeleton (Step 3) -> resize handling (Step 4) -> tests (Step 9). After each file, run `cargo check -p shux-ui`. The client cannot be fully integration-tested until the daemon (task 001) and RPC server (task 008) are working, so focus on unit-testable components.
3. **Key decision:** The key encoding in `encode_key_event()` must match what terminal applications expect. Reference `crossterm` documentation for the canonical byte sequences. The Kitty keyboard protocol (when enabled) changes the encoding -- handle that in task 028 (capability negotiation), not here.
4. **After:** Run `make check`. Update `docs/PROGRESS.md` (mark 010 in-progress or done). Update `CLAUDE.md` Learnings with any crossterm raw mode gotchas (e.g., `enable_raw_mode` is global, not per-thread; `spawn_blocking` for event polling to avoid blocking the async runtime).
