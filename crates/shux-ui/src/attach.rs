//! Client-side attach loop.
//!
//! `shux attach` enters here. The daemon does all rendering — the
//! client is a thin two-way pipe:
//!   * server → terminal: take base64-decoded ANSI bytes from each
//!     `Render` frame, dump them onto stdout. Detach/session_ended
//!     frames cause us to exit cleanly.
//!   * terminal → server: poll crossterm events, encode keys, send as
//!     `Input` frames; intercept Tier-1 keybindings and forward as
//!     `Action` frames; on resize emit a `Resize` frame.
//!
//! The TUI is wrapped in `TerminalGuard` so raw mode + alt screen +
//! mouse are restored on any exit (panic, error, detach, session end).

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use bytes::Bytes;
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton as CtMouseButton,
    MouseEventKind,
};
use futures::{SinkExt, StreamExt};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio_util::codec::Framed;

use shux_rpc::attach::{
    ATTACH_PROTOCOL_VERSION, ActionArgs, ActionKind, AttachClientFrame, AttachHello, AttachReady,
    AttachServerFrame, MouseButton, MouseKind,
};
use shux_rpc::create_codec;

use crate::client::{ClientConfig, ExitReason, encode_key_event};
use crate::terminal::{self, TerminalGuard};

/// Public entry point: connect to the daemon's attach socket, do the
/// handshake, and run the bidirectional loop until detach or session
/// end. Restores the terminal automatically.
pub async fn run_attach(socket_path: &Path, config: ClientConfig) -> Result<ExitReason> {
    terminal::install_panic_hook();

    let (cols, rows) = TerminalGuard::size().context("terminal size")?;

    let stream = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("connect to attach socket {}", socket_path.display()))?;
    let mut framed = Framed::new(stream, create_codec());

    // 1. Send the hello.
    let hello = AttachHello {
        protocol: ATTACH_PROTOCOL_VERSION,
        session_name: Some(config.session_name.clone()),
        cols,
        rows,
        client_version: env!("CARGO_PKG_VERSION").to_string(),
    };
    framed
        .send(Bytes::from(serde_json::to_vec(&hello)?))
        .await
        .context("send hello")?;

    // 2. Receive AttachReady.
    let first = framed
        .next()
        .await
        .ok_or_else(|| anyhow::anyhow!("daemon closed connection before AttachReady"))?
        .context("read ready")?;
    let ready: AttachReady = serde_json::from_slice(&first).context("parse ready")?;
    let (session_id, session_name) = match ready {
        AttachReady::Ok {
            session_id,
            session_name,
            ..
        } => (session_id, session_name),
        AttachReady::Error { code, message } => {
            return Ok(ExitReason::Error(format!(
                "attach denied: {code}: {message}"
            )));
        }
    };
    tracing::info!(session = %session_name, %session_id, "attach: ready");

    // 3. Enter raw mode. From here on we MUST go through `guard.leave()`
    //    on any exit path (the panic hook covers panics).
    let mut guard = TerminalGuard::enter().context("enter raw mode")?;

    let result = run_loop(&mut framed, &config).await;

    guard.leave().ok();
    result
}

async fn run_loop<S>(
    framed: &mut Framed<S, tokio_util::codec::LengthDelimitedCodec>,
    config: &ClientConfig,
) -> Result<ExitReason>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (mut sink, mut stream) = framed.split();
    let mut stdout = tokio::io::stdout();
    let mut prefix_active = false;
    let mut last_size = TerminalGuard::size().unwrap_or((80, 24));

    // Spawn input reader: poll crossterm events on a blocking thread,
    // forward via channel.
    let (key_tx, mut key_rx) = tokio::sync::mpsc::channel::<Event>(64);
    std::thread::spawn(move || {
        loop {
            match crossterm::event::poll(Duration::from_millis(50)) {
                Ok(true) => match crossterm::event::read() {
                    Ok(ev) => {
                        if key_tx.blocking_send(ev).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                },
                Ok(false) => continue,
                Err(_) => break,
            }
        }
    });

    let prefix_key = config.prefix_key;

    loop {
        tokio::select! {
            // Server -> terminal.
            frame = stream.next() => {
                let buf = match frame {
                    Some(Ok(b)) => b,
                    Some(Err(e)) => return Ok(ExitReason::Error(format!("framing error: {e}"))),
                    None => return Ok(ExitReason::ConnectionLost),
                };
                let parsed: AttachServerFrame = match serde_json::from_slice(&buf) {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::warn!(error = %e, "client: bad server frame");
                        continue;
                    }
                };
                match parsed {
                    AttachServerFrame::Render { data } => {
                        let bytes = match BASE64.decode(data.as_bytes()) {
                            Ok(b) => b,
                            Err(_) => continue,
                        };
                        stdout.write_all(&bytes).await.ok();
                        stdout.flush().await.ok();
                    }
                    AttachServerFrame::Bell => {
                        stdout.write_all(b"\x07").await.ok();
                    }
                    AttachServerFrame::Notice { text: _ } => {}
                    AttachServerFrame::DetachAck => return Ok(ExitReason::Detached),
                    AttachServerFrame::SessionEnded { .. } => return Ok(ExitReason::SessionEnded),
                    AttachServerFrame::Ping => {
                        let _ = sink
                            .send(Bytes::from(serde_json::to_vec(&AttachClientFrame::Pong)?))
                            .await;
                    }
                }
            }

            // Terminal -> server.
            ev = key_rx.recv() => {
                let event = match ev {
                    Some(e) => e,
                    None => return Ok(ExitReason::Error("input thread died".into())),
                };
                match event {
                    Event::Key(key) => {
                        // Ignore key release events — crossterm 0.29 emits Press AND
                        // Release on macOS, which would double every keystroke.
                        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                            continue;
                        }
                        if prefix_active {
                            prefix_active = false;
                            // Prefix-prefix: send the literal prefix key
                            // (e.g. Ctrl+Space → NUL byte) to the PTY so
                            // nested shells / vim / emacs can receive it.
                            if key.code == prefix_key.code
                                && key.modifiers == prefix_key.modifiers
                            {
                                let bytes = encode_key_event(key);
                                if !bytes.is_empty() {
                                    let frame = AttachClientFrame::Input {
                                        data: BASE64.encode(&bytes),
                                    };
                                    let payload = serde_json::to_vec(&frame)?;
                                    sink.send(Bytes::from(payload)).await.ok();
                                }
                                continue;
                            }
                            if let Some(action) = key_to_prefix_action(key) {
                                let frame = AttachClientFrame::Action {
                                    kind: action,
                                    args: ActionArgs::default(),
                                };
                                let bytes = serde_json::to_vec(&frame)?;
                                sink.send(Bytes::from(bytes)).await.ok();
                                continue;
                            } else if matches!(key.code, KeyCode::Char('d'))
                                && key.modifiers == KeyModifiers::NONE
                            {
                                let frame = AttachClientFrame::Detach;
                                let bytes = serde_json::to_vec(&frame)?;
                                sink.send(Bytes::from(bytes)).await.ok();
                                continue;
                            }
                            // Unbound prefix-key: fall through and forward
                            // as a normal PTY input so the user doesn't
                            // lose the keystroke. (e.g. Prefix → Ctrl+C
                            // sends Ctrl+C to the running process.)
                        } else if key.code == prefix_key.code
                            && key.modifiers == prefix_key.modifiers
                        {
                            // First tap of the prefix: arm and consume.
                            prefix_active = true;
                            continue;
                        }
                        // Bare-key Tier-1 actions (Alt+key etc.).
                        if let Some(action) = key_to_bare_action(key) {
                            let frame = AttachClientFrame::Action {
                                kind: action,
                                args: ActionArgs::default(),
                            };
                            let bytes = serde_json::to_vec(&frame)?;
                            sink.send(Bytes::from(bytes)).await.ok();
                            continue;
                        }
                        // Otherwise forward as PTY input bytes.
                        let bytes = encode_key_event(key);
                        if !bytes.is_empty() {
                            let frame = AttachClientFrame::Input {
                                data: BASE64.encode(&bytes),
                            };
                            let payload = serde_json::to_vec(&frame)?;
                            sink.send(Bytes::from(payload)).await.ok();
                        }
                    }
                    Event::Resize(c, r) => {
                        last_size = (c, r);
                        let frame = AttachClientFrame::Resize { cols: c, rows: r };
                        let bytes = serde_json::to_vec(&frame)?;
                        sink.send(Bytes::from(bytes)).await.ok();
                    }
                    Event::Mouse(m) => {
                        // Translate crossterm's MouseEvent into our protocol
                        // shape and forward. The daemon decides what each
                        // event means (click → focus pane, drag on border →
                        // resize, scroll → scrollback).
                        let (kind, button) = match m.kind {
                            MouseEventKind::Down(b) => (MouseKind::Down, ct_button(b)),
                            MouseEventKind::Up(b) => (MouseKind::Up, ct_button(b)),
                            MouseEventKind::Drag(b) => (MouseKind::Drag, ct_button(b)),
                            MouseEventKind::Moved => (MouseKind::Move, MouseButton::None),
                            MouseEventKind::ScrollUp => (MouseKind::ScrollUp, MouseButton::None),
                            MouseEventKind::ScrollDown => (MouseKind::ScrollDown, MouseButton::None),
                            // ScrollLeft / ScrollRight: ignore for now.
                            _ => continue,
                        };
                        let frame = AttachClientFrame::Mouse {
                            kind,
                            button,
                            col: m.column,
                            row: m.row,
                        };
                        let bytes = serde_json::to_vec(&frame)?;
                        sink.send(Bytes::from(bytes)).await.ok();
                    }
                    Event::Paste(_) | Event::FocusGained | Event::FocusLost => {
                        // Ignore for now.
                    }
                }
            }

            // Periodic resize check (in case the OS missed the resize event).
            _ = tokio::time::sleep(Duration::from_secs(5)) => {
                if let Ok((c, r)) = TerminalGuard::size() {
                    if (c, r) != last_size {
                        last_size = (c, r);
                        let frame = AttachClientFrame::Resize { cols: c, rows: r };
                        let bytes = serde_json::to_vec(&frame)?;
                        sink.send(Bytes::from(bytes)).await.ok();
                    }
                }
            }
        }
    }
}

fn ct_button(b: CtMouseButton) -> MouseButton {
    match b {
        CtMouseButton::Left => MouseButton::Left,
        CtMouseButton::Right => MouseButton::Right,
        CtMouseButton::Middle => MouseButton::Middle,
    }
}

/// Map a key pressed *after the prefix* into an action, if any.
///
/// Only fires for unmodified keys. Prefix + Ctrl+C must NOT trigger
/// `c → NewWindow` — the user is trying to send a literal SIGINT to
/// whatever was running before they hit prefix.
fn key_to_prefix_action(key: KeyEvent) -> Option<ActionKind> {
    if key.modifiers != KeyModifiers::NONE && key.modifiers != KeyModifiers::SHIFT {
        return None;
    }
    Some(match key.code {
        KeyCode::Char('|') | KeyCode::Char('v') => ActionKind::SplitVertical,
        KeyCode::Char('-') | KeyCode::Char('s') => ActionKind::SplitHorizontal,
        KeyCode::Char(' ') => ActionKind::SplitSmart,
        KeyCode::Char('h') => ActionKind::FocusLeft,
        KeyCode::Char('j') => ActionKind::FocusDown,
        KeyCode::Char('k') => ActionKind::FocusUp,
        KeyCode::Char('l') => ActionKind::FocusRight,
        KeyCode::Char('o') => ActionKind::FocusNext,
        KeyCode::Char('z') => ActionKind::ToggleZoom,
        KeyCode::Char('x') => ActionKind::KillPane,
        KeyCode::Char('c') => ActionKind::NewWindow,
        KeyCode::Char('n') => ActionKind::NextWindow,
        KeyCode::Char('p') => ActionKind::PrevWindow,
        KeyCode::Char('r') => ActionKind::Redraw,
        KeyCode::Left => ActionKind::ResizeLeft,
        KeyCode::Right => ActionKind::ResizeRight,
        KeyCode::Up => ActionKind::ResizeUp,
        KeyCode::Down => ActionKind::ResizeDown,
        KeyCode::Char('?') => ActionKind::ToggleHelp,
        KeyCode::Char('[') => ActionKind::EnterCopyMode,
        _ => return None,
    })
}

/// Map a *bare* (non-prefixed) key into an action.
///
/// We support Alt-prefixed shortcuts as well as the dedicated prefix
/// system. This makes the multiplexer feel modern (no need to "leader
/// then key" every action) while still respecting the legacy tmux
/// muscle memory of users who like Ctrl+Space first.
fn key_to_bare_action(key: KeyEvent) -> Option<ActionKind> {
    if !key.modifiers.contains(KeyModifiers::ALT) {
        return None;
    }
    Some(match key.code {
        KeyCode::Enter => ActionKind::SplitSmart,
        KeyCode::Char('|') | KeyCode::Char('\\') => ActionKind::SplitVertical,
        KeyCode::Char('-') => ActionKind::SplitHorizontal,
        KeyCode::Left => ActionKind::FocusLeft,
        KeyCode::Right => ActionKind::FocusRight,
        KeyCode::Up => ActionKind::FocusUp,
        KeyCode::Down => ActionKind::FocusDown,
        KeyCode::Char('z') => ActionKind::ToggleZoom,
        KeyCode::Char('x') => ActionKind::KillPane,
        KeyCode::Tab => ActionKind::FocusNext,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prefix_actions() {
        for (code, expected) in [
            (KeyCode::Char('|'), ActionKind::SplitVertical),
            (KeyCode::Char('-'), ActionKind::SplitHorizontal),
            (KeyCode::Char('z'), ActionKind::ToggleZoom),
            (KeyCode::Char('h'), ActionKind::FocusLeft),
            (KeyCode::Char('l'), ActionKind::FocusRight),
            (KeyCode::Char('c'), ActionKind::NewWindow),
            (KeyCode::Char('x'), ActionKind::KillPane),
        ] {
            let action = key_to_prefix_action(KeyEvent::new(code, KeyModifiers::NONE));
            assert_eq!(action, Some(expected), "{code:?}");
        }
    }

    #[test]
    fn test_bare_alt_actions() {
        let action = key_to_bare_action(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        assert_eq!(action, Some(ActionKind::SplitSmart));
        let action = key_to_bare_action(KeyEvent::new(KeyCode::Tab, KeyModifiers::ALT));
        assert_eq!(action, Some(ActionKind::FocusNext));
    }

    #[test]
    fn test_bare_without_alt_returns_none() {
        let action = key_to_bare_action(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        assert_eq!(action, None);
    }

    #[test]
    fn test_unknown_prefix_key_returns_none() {
        // Pick a character that is genuinely unbound today. `?` used to
        // be unbound but is now ToggleHelp (task 033 / PR 4).
        let action = key_to_prefix_action(KeyEvent::new(KeyCode::Char('Q'), KeyModifiers::NONE));
        assert_eq!(action, None);
    }

    #[test]
    fn test_question_mark_maps_to_toggle_help() {
        let action = key_to_prefix_action(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        assert_eq!(action, Some(ActionKind::ToggleHelp));
        // Same with explicit Shift modifier (some terminals send it).
        let action = key_to_prefix_action(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT));
        assert_eq!(action, Some(ActionKind::ToggleHelp));
    }
}
