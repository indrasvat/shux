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
use crate::keybinding::{BindingTarget, KeybindingRegistry};
use crate::terminal::{self, TerminalGuard};

/// Environment set by an outer multiplexer that owns this terminal.
///
/// Overlaps `shux_pty::OUTER_TERMINAL_IDENTITY_VARS`, which scrubs the same
/// names out of a pane's environment; that answers "what should a child
/// believe", this answers "who owns our stdout". Kept apart deliberately, and
/// short: only a program that rewrites the byte stream belongs here.
const OUTER_MULTIPLEXER_VARS: &[&str] = &["TMUX", "STY", "ZELLIJ"];

/// Overrides the automatic decision below. `1`/`true`/`yes`/`on` forces images
/// on, `0`/`false`/`no`/`off` forces them off; anything else, including unset,
/// leaves the decision automatic. Compared case-insensitively and trimmed.
const GRAPHICS_OVERRIDE_VAR: &str = "SHUX_GRAPHICS";

/// Whether this terminal can be sent kitty graphics: yes, unless an outer
/// multiplexer announces itself. The check reads the CLIENT's environment while
/// the corruption is a property of the byte stream; the two decouple across
/// `sudo`/`ssh`/`env -i`, which is what [`GRAPHICS_OVERRIDE_VAR`] is for.
/// `docs/configuration.md` is the user-facing copy.
fn terminal_can_draw_images() -> bool {
    if let Ok(raw) = std::env::var(GRAPHICS_OVERRIDE_VAR) {
        let value = raw.trim().to_ascii_lowercase();
        match value.as_str() {
            "1" | "true" | "on" | "yes" => return true,
            "0" | "false" | "off" | "no" => return false,
            "" => {}
            // stderr, not `warn!`: without `-v` the client's subscriber is
            // ERROR-only, so a warn here reaches nobody. This runs before the
            // alternate screen, so the line lands on the normal screen.
            _ => eprintln!(
                "shux: ignoring {GRAPHICS_OVERRIDE_VAR}={raw:?}: expected on/off; deciding from the environment"
            ),
        }
    }
    !OUTER_MULTIPLEXER_VARS
        .iter()
        .any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty()))
}

/// Public entry point: connect to the daemon's attach socket, do the
/// handshake, and run the bidirectional loop until detach or session
/// end. Restores the terminal automatically.
pub async fn run_attach(socket_path: &Path, config: ClientConfig) -> Result<ExitReason> {
    terminal::install_panic_hook();

    let (cols, rows) = TerminalGuard::size().context("terminal size")?;
    let graphics = terminal_can_draw_images();

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
        graphics,
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
    tracing::info!(session = %session_name, %session_id, graphics, "attach: ready");

    // From here on we MUST go through `guard.leave()` on any exit path (the
    // panic hook covers panics).
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
    // True once we've sent a PrefixTapped frame for this attach.
    // One-shot — no need to spam the wire on every prefix-arm.
    let mut prefix_announced = false;
    let mut last_size = TerminalGuard::size().unwrap_or((80, 24));
    let bindings = KeybindingRegistry::with_overrides(&config.prefix, &config.keybindings)
        .map_err(|e| anyhow::anyhow!("invalid keybinding config: {e:?}"))?;

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
                            if bindings.is_prefix_key(key) {
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
                            if let Some(target) = bindings.resolve_prefix(key) {
                                send_binding_target(&mut sink, target).await?;
                                continue;
                            }
                            // Unbound prefix-key: fall through and forward
                            // as a normal PTY input so the user doesn't
                            // lose the keystroke. (e.g. Prefix → Ctrl+C
                            // sends Ctrl+C to the running process.)
                        } else if bindings.is_prefix_key(key) {
                            // First tap of the prefix: arm and consume.
                            // Tell the daemon the first time we arm so
                            // the OOTB onboarding hint dismisses even
                            // if the user immediately hits Escape /
                            // unbound key (no Action frame would have
                            // fired in that case — see codex review of
                            // PR #43).
                            if !prefix_announced {
                                prefix_announced = true;
                                let frame = AttachClientFrame::PrefixTapped;
                                if let Ok(bytes) = serde_json::to_vec(&frame) {
                                    sink.send(Bytes::from(bytes)).await.ok();
                                }
                            }
                            prefix_active = true;
                            continue;
                        }
                        // Bare-key Tier-1 actions (Alt+key etc.).
                        if let Some(target) = bindings.resolve_root(key) {
                            send_binding_target(&mut sink, target).await?;
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
                        // Modifiers travel with the event: dropping them made
                        // ctrl-click and alt-click arrive at the app as plain
                        // clicks (in nvim, jump-to-tag became a cursor move).
                        let frame = AttachClientFrame::Mouse {
                            kind,
                            button,
                            col: m.column,
                            row: m.row,
                            shift: m.modifiers.contains(KeyModifiers::SHIFT),
                            alt: m.modifiers.contains(KeyModifiers::ALT),
                            ctrl: m.modifiers.contains(KeyModifiers::CONTROL),
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
                if let Ok((c, r)) = TerminalGuard::size()
                    && (c, r) != last_size {
                        last_size = (c, r);
                        let frame = AttachClientFrame::Resize { cols: c, rows: r };
                        let bytes = serde_json::to_vec(&frame)?;
                        sink.send(Bytes::from(bytes)).await.ok();
                    }
            }
        }
    }
}

async fn send_binding_target<SinkT>(sink: &mut SinkT, target: &BindingTarget) -> Result<()>
where
    SinkT: futures::Sink<Bytes> + Unpin,
    SinkT::Error: std::fmt::Display,
{
    let frame = match target {
        BindingTarget::Action(kind, args) => AttachClientFrame::Action {
            kind: *kind,
            args: args.clone(),
        },
        BindingTarget::Detach => AttachClientFrame::Detach,
    };
    let bytes = serde_json::to_vec(&frame)?;
    sink.send(Bytes::from(bytes))
        .await
        .map_err(|e| anyhow::anyhow!("send binding frame: {e}"))?;
    Ok(())
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
#[allow(dead_code)]
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

/// Map a *bare* (non-prefixed) key into an action and (optionally)
/// its arguments.
///
/// We support Alt-prefixed shortcuts as well as the dedicated prefix
/// system. This makes the multiplexer feel modern (no need to "leader
/// then key" every action) while still respecting the legacy tmux
/// muscle memory of users who like Ctrl+Space first.
///
/// Bare bindings (Codex P2 followup from PR #8):
/// - Alt+h/j/k/l: directional focus (mirrors the prefix-key bindings)
/// - Alt+n / Alt+p: cycle to next/prev window
/// - Alt+1..9: switch directly to the Nth window (1-indexed)
///
/// Closes the gap that demoted task 018 to Partial.
#[allow(dead_code)]
fn key_to_bare_action(key: KeyEvent) -> Option<(ActionKind, ActionArgs)> {
    if !key.modifiers.contains(KeyModifiers::ALT) {
        return None;
    }
    // Alt+1..9 — switch to window N. Handled before the catch-all so a
    // user-defined `KeyModifiers::ALT + KeyCode::Char('1')` doesn't fall
    // through to "no match".
    if let KeyCode::Char(c) = key.code
        && let Some(d) = c.to_digit(10)
        && (1..=9).contains(&d)
    {
        return Some((
            ActionKind::SwitchToWindow,
            ActionArgs {
                window_index: Some(d as u16),
                ..Default::default()
            },
        ));
    }
    let kind = match key.code {
        KeyCode::Enter => ActionKind::SplitSmart,
        KeyCode::Char('|') | KeyCode::Char('\\') => ActionKind::SplitVertical,
        KeyCode::Char('-') => ActionKind::SplitHorizontal,
        KeyCode::Left | KeyCode::Char('h') => ActionKind::FocusLeft,
        KeyCode::Right | KeyCode::Char('l') => ActionKind::FocusRight,
        KeyCode::Up | KeyCode::Char('k') => ActionKind::FocusUp,
        KeyCode::Down | KeyCode::Char('j') => ActionKind::FocusDown,
        KeyCode::Char('z') => ActionKind::ToggleZoom,
        KeyCode::Char('x') => ActionKind::KillPane,
        KeyCode::Tab => ActionKind::FocusNext,
        KeyCode::Char('n') => ActionKind::NextWindow,
        KeyCode::Char('p') => ActionKind::PrevWindow,
        _ => return None,
    };
    Some((kind, ActionArgs::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prefix_actions() {
        for (code, expected) in [
            (KeyCode::Char('|'), ActionKind::SplitVertical),
            (KeyCode::Char('v'), ActionKind::SplitVertical),
            (KeyCode::Char('-'), ActionKind::SplitHorizontal),
            (KeyCode::Char('s'), ActionKind::SplitHorizontal),
            (KeyCode::Char(' '), ActionKind::SplitSmart),
            (KeyCode::Char('z'), ActionKind::ToggleZoom),
            (KeyCode::Char('h'), ActionKind::FocusLeft),
            (KeyCode::Char('j'), ActionKind::FocusDown),
            (KeyCode::Char('k'), ActionKind::FocusUp),
            (KeyCode::Char('l'), ActionKind::FocusRight),
            (KeyCode::Char('o'), ActionKind::FocusNext),
            (KeyCode::Char('c'), ActionKind::NewWindow),
            (KeyCode::Char('n'), ActionKind::NextWindow),
            (KeyCode::Char('p'), ActionKind::PrevWindow),
            (KeyCode::Char('r'), ActionKind::Redraw),
            (KeyCode::Char('x'), ActionKind::KillPane),
            (KeyCode::Left, ActionKind::ResizeLeft),
            (KeyCode::Right, ActionKind::ResizeRight),
            (KeyCode::Up, ActionKind::ResizeUp),
            (KeyCode::Down, ActionKind::ResizeDown),
            (KeyCode::Char('['), ActionKind::EnterCopyMode),
        ] {
            let action = key_to_prefix_action(KeyEvent::new(code, KeyModifiers::NONE));
            assert_eq!(action, Some(expected), "{code:?}");
        }
    }

    #[test]
    fn test_bare_alt_actions() {
        let action = key_to_bare_action(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        assert_eq!(
            action,
            Some((ActionKind::SplitSmart, ActionArgs::default()))
        );
        let action = key_to_bare_action(KeyEvent::new(KeyCode::Tab, KeyModifiers::ALT));
        assert_eq!(action, Some((ActionKind::FocusNext, ActionArgs::default())));
    }

    #[test]
    fn test_bare_without_alt_returns_none() {
        let action = key_to_bare_action(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        assert_eq!(action, None);
    }

    // ── Codex P2 followup from PR #8 — bare Alt+h/j/k/l + Alt+n/p + Alt+1..9 ──
    //
    // Closes the gap that demoted task 018 to Partial. tmux + zellij
    // both ship these by default; without them shux feels broken to
    // anyone with muscle memory who expects Alt+1 to switch to window 1.

    #[test]
    fn test_bare_alt_hjkl_directional_focus() {
        for (code, expected) in [
            (KeyCode::Char('h'), ActionKind::FocusLeft),
            (KeyCode::Left, ActionKind::FocusLeft),
            (KeyCode::Char('j'), ActionKind::FocusDown),
            (KeyCode::Down, ActionKind::FocusDown),
            (KeyCode::Char('k'), ActionKind::FocusUp),
            (KeyCode::Up, ActionKind::FocusUp),
            (KeyCode::Char('l'), ActionKind::FocusRight),
            (KeyCode::Right, ActionKind::FocusRight),
        ] {
            let action = key_to_bare_action(KeyEvent::new(code, KeyModifiers::ALT));
            assert_eq!(action, Some((expected, ActionArgs::default())), "{code:?}",);
        }
    }

    #[test]
    fn test_bare_alt_split_zoom_kill_and_unbound_paths() {
        for (code, expected) in [
            (KeyCode::Char('|'), ActionKind::SplitVertical),
            (KeyCode::Char('\\'), ActionKind::SplitVertical),
            (KeyCode::Char('-'), ActionKind::SplitHorizontal),
            (KeyCode::Char('z'), ActionKind::ToggleZoom),
            (KeyCode::Char('x'), ActionKind::KillPane),
        ] {
            let action = key_to_bare_action(KeyEvent::new(code, KeyModifiers::ALT));
            assert_eq!(action, Some((expected, ActionArgs::default())), "{code:?}");
        }

        let action = key_to_bare_action(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::ALT));
        assert_eq!(action, None);
    }

    #[test]
    fn test_bare_alt_n_p_cycle_windows() {
        let action = key_to_bare_action(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::ALT));
        assert_eq!(
            action,
            Some((ActionKind::NextWindow, ActionArgs::default()))
        );
        let action = key_to_bare_action(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::ALT));
        assert_eq!(
            action,
            Some((ActionKind::PrevWindow, ActionArgs::default()))
        );
    }

    #[test]
    fn test_bare_alt_digits_switch_to_window_n() {
        for d in 1..=9u8 {
            let ch = char::from_digit(d as u32, 10).unwrap();
            let action = key_to_bare_action(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::ALT));
            let (kind, args) = action.expect("Alt+digit must map");
            assert_eq!(kind, ActionKind::SwitchToWindow, "Alt+{ch}");
            assert_eq!(args.window_index, Some(d as u16), "Alt+{ch} → window_index",);
        }
    }

    #[test]
    fn test_bare_alt_zero_unbound() {
        // 0 is intentionally not a switch shortcut (would conflict with
        // tmux's Alt+0 meaning the 10th window, which we don't have).
        let action = key_to_bare_action(KeyEvent::new(KeyCode::Char('0'), KeyModifiers::ALT));
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

    #[test]
    fn test_modified_prefix_keys_do_not_trigger_actions() {
        let action = key_to_prefix_action(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(action, None);
    }

    #[test]
    fn mouse_button_mapping_matches_protocol_values() {
        assert_eq!(ct_button(CtMouseButton::Left), MouseButton::Left);
        assert_eq!(ct_button(CtMouseButton::Right), MouseButton::Right);
        assert_eq!(ct_button(CtMouseButton::Middle), MouseButton::Middle);
    }

    #[tokio::test]
    async fn send_binding_target_serializes_action_and_detach_frames() {
        let (mut tx, mut rx) = futures::channel::mpsc::channel::<Bytes>(4);
        send_binding_target(
            &mut tx,
            &BindingTarget::Action(
                ActionKind::SwitchToWindow,
                ActionArgs {
                    window_index: Some(3),
                    ..Default::default()
                },
            ),
        )
        .await
        .expect("send action");
        send_binding_target(&mut tx, &BindingTarget::Detach)
            .await
            .expect("send detach");
        drop(tx);

        let action = rx.next().await.expect("action frame");
        let parsed: AttachClientFrame = serde_json::from_slice(&action).expect("action json");
        match parsed {
            AttachClientFrame::Action { kind, args } => {
                assert_eq!(kind, ActionKind::SwitchToWindow);
                assert_eq!(args.window_index, Some(3));
            }
            other => panic!("expected action frame, got {other:?}"),
        }

        let detach = rx.next().await.expect("detach frame");
        let parsed: AttachClientFrame = serde_json::from_slice(&detach).expect("detach json");
        assert!(matches!(parsed, AttachClientFrame::Detach));
    }

    /// These mutate the process environment, so they hold a lock: `cargo test`
    /// shares one process across test threads, and only `cargo nextest` gives
    /// each test its own.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    const ENV_VARS: &[&str] = &["TMUX", "STY", "ZELLIJ", "SHUX_GRAPHICS"];

    /// Restores on `Drop`, so a failing assertion cannot unwind past the
    /// restore and leave every later test in this process reading a polluted
    /// environment -- one real failure buried under a cascade of fake ones.
    struct IsolatedEnv {
        saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
        _held: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for IsolatedEnv {
        fn drop(&mut self) {
            for (k, v) in self.saved.drain(..) {
                match v {
                    Some(v) => unsafe { std::env::set_var(k, v) },
                    None => unsafe { std::env::remove_var(k) },
                }
            }
        }
    }

    fn isolated_env() -> IsolatedEnv {
        let held = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved: Vec<_> = ENV_VARS.iter().map(|k| (*k, std::env::var_os(k))).collect();
        for k in ENV_VARS {
            unsafe { std::env::remove_var(k) };
        }
        IsolatedEnv { saved, _held: held }
    }

    #[test]
    fn a_bare_terminal_may_be_drawn_on() {
        let _env = isolated_env();
        assert!(super::terminal_can_draw_images());
    }

    #[test]
    fn an_outer_multiplexer_is_never_drawn_on() {
        // tmux 3.4 adopts APC content as a pane title, so anything shux emits
        // lands there instead of on the screen -- measured, once per frame.
        for var in ["TMUX", "STY", "ZELLIJ"] {
            let _env = isolated_env();
            unsafe { std::env::set_var(var, "/tmp/whatever,123,0") };
            assert!(
                !super::terminal_can_draw_images(),
                "{var} was set, and shux would still have emitted"
            );
        }
    }

    #[test]
    fn an_empty_value_does_not_count_as_a_multiplexer() {
        let _env = isolated_env();
        unsafe { std::env::set_var("TMUX", "") };
        assert!(super::terminal_can_draw_images());
    }

    #[test]
    fn an_unrecognised_override_leaves_the_decision_automatic() {
        let _env = isolated_env();
        unsafe { std::env::set_var("SHUX_GRAPHICS", "maybe") };
        assert!(super::terminal_can_draw_images());
        unsafe { std::env::set_var("TMUX", "/tmp/x,1,0") };
        assert!(
            !super::terminal_can_draw_images(),
            "a junk value did not fall through to the automatic check"
        );
    }

    /// The hatch fails OPEN, so an ordinary spelling that misses turns a
    /// documented limitation with a working remedy back into silent
    /// corruption. `is_ci` shipped this exact defect once already.
    #[test]
    fn the_override_is_case_insensitive_and_trimmed() {
        for on in ["on", "ON", "On", "TRUE", "True", " yes ", "1"] {
            let _env = isolated_env();
            unsafe { std::env::set_var("TMUX", "/tmp/x,1,0") };
            unsafe { std::env::set_var("SHUX_GRAPHICS", on) };
            assert!(
                super::terminal_can_draw_images(),
                "{on:?} did not force images on"
            );
        }
        for off in ["off", "OFF", "Off", " off ", "FALSE", "False", "No", "0"] {
            let _env = isolated_env();
            unsafe { std::env::set_var("SHUX_GRAPHICS", off) };
            assert!(
                !super::terminal_can_draw_images(),
                "{off:?} did not force images off"
            );
        }
    }
}
