//! Daemon-side attach session handler.
//!
//! Owns the streaming protocol that turns shux from a CLI tool into a
//! real interactive multiplexer. Each `shux attach` (or `shux` /
//! `shux new` without `--detached`) opens a UDS connection to
//! `${runtime_dir}/attach.sock`, sends an `AttachHello`, and starts
//! exchanging streaming frames with this handler.
//!
//! For each connection the daemon spawns:
//! - one **render** task that owns a `RenderCompositor`, watches
//!   `PaneIoState`, and ships ANSI bytes to the client whenever the VT
//!   for any visible pane changes;
//! - the connection task itself, which reads `AttachClientFrame`s from
//!   the client (input bytes, resizes, action keys, detach) and
//!   dispatches them.
//!
//! Per-connection state lives entirely on the stack — the daemon's
//! global state (graph + pane I/O) is borrowed via `Arc`s.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, mpsc};
use tokio_util::codec::Framed;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use shux_core::config::ConfigHandle;
use shux_core::graph::{GraphError, GraphHandle};
use shux_core::layout::{NavDirection, Rect};
use shux_core::model::{PaneId, SessionId, WindowId};
use shux_core::theme::Theme;
use shux_pty::handle::PtySize;
use shux_rpc::attach::{
    ATTACH_PROTOCOL_VERSION, ActionKind, AttachClientFrame, AttachHello, AttachReady,
    AttachServerFrame, MouseButton as ProtoMouseButton, MouseKind,
};
use shux_rpc::create_codec;
use shux_ui::{BorderStyle, CompositorConfig, MultiPaneFrame, RenderCompositor};

use crate::PaneIoState;
use crate::statusbar_runner::{SegmentCache, populate_bar};

/// Client-screen dimensions (cols, rows) tracked per attached client.
/// Used as the authoritative source of size when computing per-pane rects
/// and PTY winsize — never inferred from the VT grid (which would create
/// a self-feeding shrink loop).
type ClientSize = Arc<Mutex<(u16, u16)>>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct CopyOverlayStamp {
    kind: CopyOverlayKind,
    pane_id: PaneId,
    rect: Rect,
    state: shux_ui::CopyModeState,
    theme: Theme,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CopyOverlayKind {
    Modal,
    MouseSelection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MouseSelection {
    pane_id: PaneId,
    state: shux_ui::CopyModeState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CopyContextMenu {
    pane_id: PaneId,
    col: u16,
    row: u16,
}

fn copy_overlay_needs_base_redraw(
    last: Option<&CopyOverlayStamp>,
    next: Option<&CopyOverlayStamp>,
) -> bool {
    last != next
}

fn copy_overlay_needs_repaint(
    last: Option<&CopyOverlayStamp>,
    next: Option<&CopyOverlayStamp>,
    base_emitted: bool,
) -> bool {
    next.is_some() && (base_emitted || copy_overlay_needs_base_redraw(last, next))
}

/// Status-bar rows reserved at the bottom of the client screen.
const STATUS_BAR_ROWS: u16 = 1;

/// Total time the daemon will wait for the AttachHello frame before
/// dropping the connection. Prevents slowloris-style blocking.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// How often the daemon pings the client to detect dead peers.
const PING_INTERVAL: Duration = Duration::from_secs(15);

/// Run the attach UDS listener. Each accepted connection spawns an
/// independent attach session task. Runs until `cancel` fires.
#[allow(clippy::too_many_arguments)]
pub async fn run_attach_server(
    socket_path: std::path::PathBuf,
    graph: GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    config: ConfigHandle,
    segments: SegmentCache,
    meta_cache: crate::session_meta::SessionMetaCache,
    onboarding: crate::onboarding::OnboardingHandle,
    daemon_start: std::time::Instant,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    if socket_path.exists() {
        let _ = std::fs::remove_file(&socket_path);
    }
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(&socket_path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o700))?;
    }
    info!(path = %socket_path.display(), "attach UDS listener bound");

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("attach server shutting down");
                break;
            }
            res = listener.accept() => {
                match res {
                    Ok((stream, _)) => {
                        let g = graph.clone();
                        let io = io_state.clone();
                        let cfg = config.clone();
                        let segs = segments.clone();
                        let meta = meta_cache.clone();
                        let onb = onboarding.clone();
                        let c = cancel.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_attach_connection(
                                stream, g, io, cfg, segs, meta, onb, daemon_start, c,
                            )
                            .await
                            {
                                warn!(error = %e, "attach session ended with error");
                            }
                        });
                    }
                    Err(e) => warn!(error = %e, "attach accept failed"),
                }
            }
        }
    }

    let _ = std::fs::remove_file(&socket_path);
    Ok(())
}

/// Handle one attach connection: handshake, then run the streaming loop.
#[allow(clippy::too_many_arguments)]
async fn handle_attach_connection(
    stream: UnixStream,
    graph: GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    config: ConfigHandle,
    segments: SegmentCache,
    meta_cache: crate::session_meta::SessionMetaCache,
    onboarding: crate::onboarding::OnboardingHandle,
    daemon_start: std::time::Instant,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let mut framed = Framed::new(stream, create_codec());

    // Step 1: Receive the hello frame, bounded to HELLO_TIMEOUT so a
    // hung peer cannot tie up a worker forever.
    let first = match tokio::time::timeout(HELLO_TIMEOUT, framed.next()).await {
        Ok(Some(Ok(buf))) => buf,
        Ok(Some(Err(e))) => {
            warn!(error = %e, "attach: bad first frame");
            return Ok(());
        }
        Ok(None) => {
            debug!("attach: client disconnected before hello");
            return Ok(());
        }
        Err(_) => {
            warn!("attach: hello timeout — closing");
            return Ok(());
        }
    };
    let hello: AttachHello = match serde_json::from_slice(&first) {
        Ok(h) => h,
        Err(e) => {
            warn!(error = %e, "attach: hello parse failed");
            send_ready_error(&mut framed, "invalid_hello", &format!("{e}")).await?;
            return Ok(());
        }
    };

    if hello.protocol != ATTACH_PROTOCOL_VERSION {
        send_ready_error(
            &mut framed,
            "protocol_mismatch",
            &format!(
                "client protocol {} != server {}",
                hello.protocol, ATTACH_PROTOCOL_VERSION
            ),
        )
        .await?;
        return Ok(());
    }

    // Step 2: Resolve the target session.
    let resolved = resolve_or_create_session(&graph, &hello.session_name, &meta_cache).await;
    let session = match resolved {
        Ok(s) => s,
        Err(e) => {
            send_ready_error(&mut framed, "session_resolve", &e.to_string()).await?;
            return Ok(());
        }
    };

    // Spawn a PTY for the initial pane if it doesn't exist yet (newly
    // created sessions can race with the attach if the client hits us
    // before the spawn task finishes).
    {
        let writer_present = {
            let state = io_state.lock().await;
            state.writers.contains_key(&session.active_pane_id)
        };
        if !writer_present {
            crate::spawn_pane_pty(
                session.active_pane_id,
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/tmp")),
                Vec::new(),
                shux_pty::handle::PtySize::default(),
                Vec::new(),
                io_state.clone(),
                cancel.clone(),
                graph.clone(),
            )
            .await
            .ok();
        }
    }
    // Resize every pane in the active window to its real layout rect, not
    // the full client size. Multi-pane TUIs (vim, htop, less) read TIOCGWINSZ
    // and will lay themselves out wrong if every pane PTY pretends to be
    // the whole screen.
    apply_resize_to_window(&graph, &io_state, &session, hello.cols, hello.rows).await;

    // Step 3: Send AttachReady::Ok.
    let ready = AttachReady::Ok {
        session_id: session.session_id.to_string(),
        session_name: session.name.clone(),
        active_window_id: session.active_window_id.to_string(),
        active_pane_id: session.active_pane_id.to_string(),
        protocol: ATTACH_PROTOCOL_VERSION,
    };
    framed
        .send(Bytes::from(serde_json::to_vec(&ready)?))
        .await?;

    info!(session = %session.name, "attach session started");

    // Step 4: Run the main attach loop.
    run_attach_loop(
        framed,
        graph,
        io_state,
        config,
        segments,
        meta_cache,
        onboarding,
        daemon_start,
        session,
        hello,
        cancel,
    )
    .await
}

#[derive(Debug, Clone)]
struct AttachedSession {
    session_id: SessionId,
    name: String,
    active_window_id: WindowId,
    active_pane_id: PaneId,
    /// Whether the keybinding cheat-sheet overlay is currently visible.
    /// Toggled by `prefix + ?` (ActionKind::ToggleHelp); dismissed by
    /// any key while visible (Escape / q most natural). When true, the
    /// render loop draws the overlay and the input loop swallows raw
    /// Input frames so typing doesn't reach the focused PTY behind it.
    help_visible: bool,
    /// Active copy-mode session, if any. Entered via `prefix + [` →
    /// `ActionKind::EnterCopyMode`. While `Some(_)`, the input loop
    /// routes Input-frame bytes through `copy_mode::handle_key`
    /// instead of forwarding them to the focused PTY, the render
    /// loop overlays a cursor + selection on the focused pane, and
    /// `y` triggers an OSC 52 clipboard write before exiting.
    copy_mode: Option<shux_ui::CopyModeState>,
    /// Normal-mode, mouse-driven selection. Unlike `copy_mode`, this layer
    /// does not trap keyboard input; it is the everyday terminal-style
    /// selection model for visible pane text.
    mouse_selection: Option<MouseSelection>,
    /// Inline action menu opened by right-clicking an active mouse selection.
    copy_menu: Option<CopyContextMenu>,
    /// Most recent prefix-action label, with the wallclock instant it
    /// fired. The status bar renders `[<label>]` in the center zone for
    /// ~1.5s, then it auto-clears. Gives the user immediate "yes, that
    /// action took effect" feedback for ambiguous keystrokes (zoom,
    /// kill, copy). None at attach start. Cleared either by the render
    /// loop or by another action overwriting it.
    last_action: Option<(String, std::time::Instant)>,
    /// True until the welcome toast has been rendered for its full
    /// dwell (~3s). Render loop flips this to false; we then persist
    /// `welcome_toast_seen: true` via the OnboardingHandle so the next
    /// attach skips the toast.
    show_welcome_toast: bool,
}

/// Find a session by name, or create it (with one window + one pane) if
/// missing. Mirrors `shux new -s <name>` semantics. When a new session
/// is created here, kicks off the `SessionMetaCache` population that
/// the `session.create` / `session.ensure` RPC handlers would have
/// done — without this, bare `shux` on first run (which lands here
/// because there's no existing session) skips git/SSH decoration in
/// the OOTB status bar. Codex review P2 of PR #43.
async fn resolve_or_create_session(
    graph: &GraphHandle,
    name: &Option<String>,
    meta_cache: &crate::session_meta::SessionMetaCache,
) -> anyhow::Result<AttachedSession> {
    let snap = graph.snapshot();
    let target_name = name.clone().unwrap_or_else(|| "default".to_string());

    if let Some(sess) = snap.find_session_by_name(&target_name) {
        let win = snap
            .windows
            .get(&sess.active_window)
            .ok_or_else(|| anyhow::anyhow!("active window missing from snapshot"))?;
        return Ok(AttachedSession {
            session_id: sess.id,
            name: sess.name.clone(),
            active_window_id: win.id,
            active_pane_id: win.active_pane,
            help_visible: false,
            copy_mode: None,
            mouse_selection: None,
            copy_menu: None,
            last_action: None,
            // Whether the toast actually renders is decided at attach
            // time by reading the onboarding state file; this stays
            // true here so the render loop can flip it off after dwell.
            show_welcome_toast: true,
        });
    }

    drop(snap);
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/tmp"));
    let session_id = graph
        .create_session(target_name.clone(), cwd.clone())
        .await
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;

    // Populate the meta cache exactly like the session.create RPC
    // handler does (spawn_blocking so the synchronous git probe doesn't
    // stall the attach acceptor task on a slow filesystem / NFS).
    let cache_for_blocking = meta_cache.clone();
    let cwd_for_blocking = cwd.clone();
    tokio::task::spawn_blocking(move || {
        let branch = crate::session_meta::detect_git_branch(&cwd_for_blocking);
        let over_ssh = crate::session_meta::detect_over_ssh();
        let snapshot = crate::session_meta::SessionMeta {
            git_branch: branch,
            over_ssh,
        };
        tokio::runtime::Handle::current().block_on(async move {
            cache_for_blocking.set(session_id, snapshot).await;
        });
    });
    let snap = graph.snapshot();
    let sess = snap
        .sessions
        .get(&session_id)
        .ok_or_else(|| anyhow::anyhow!("session vanished after create"))?;
    let win = snap
        .windows
        .get(&sess.active_window)
        .ok_or_else(|| anyhow::anyhow!("active window missing after create"))?;
    Ok(AttachedSession {
        session_id: sess.id,
        name: sess.name.clone(),
        active_window_id: win.id,
        active_pane_id: win.active_pane,
        help_visible: false,
        copy_mode: None,
        mouse_selection: None,
        copy_menu: None,
        last_action: None,
        show_welcome_toast: true,
    })
}

/// Compute per-pane rects given the client size and dispatch each PTY its
/// real winsize. The compositor's pane viewport is inset by 1 cell on each
/// side for the border outline, and the bottom row is reserved for the
/// status bar — apply the same arithmetic here.
async fn apply_resize_to_window(
    graph: &GraphHandle,
    io_state: &Arc<Mutex<PaneIoState>>,
    session: &AttachedSession,
    cols: u16,
    rows: u16,
) {
    let snap = graph.snapshot();
    let win = match snap.windows.get(&session.active_window_id) {
        Some(w) => w,
        None => return,
    };
    let content_h = rows.saturating_sub(STATUS_BAR_ROWS);
    let content = Rect::new(0, 0, cols, content_h);
    let viewport = if cols >= 3 && content_h >= 3 {
        Rect::new(content.x + 1, content.y + 1, cols - 2, content_h - 2)
    } else {
        content
    };

    // Drain the resizer senders out from under the lock so we never await
    // a channel send while still holding the PaneIoState mutex. Attach
    // fan-out is fire-and-forget (ack=None); the synchronous path is
    // `pane.set_size` RPC which constructs its own oneshot.
    let mut to_send: Vec<(mpsc::Sender<crate::ResizeRequest>, PtySize)> = Vec::new();

    if win.layout.is_zoomed() {
        // Zoomed: every pane in the tree reports the full content area
        // size so apps in the zoomed pane lay out correctly, while
        // others stay at the same nominal size (cheap, harmless).
        let state = io_state.lock().await;
        let pane_ids = win
            .layout
            .zoom
            .as_ref()
            .map(|zoom| zoom.saved_layout.pane_ids())
            .unwrap_or_else(|| win.layout.tree.pane_ids());
        for pid in pane_ids {
            if let Some(tx) = state.resizers.get(&pid) {
                to_send.push((tx.clone(), PtySize::new(content.width, content.height)));
            }
        }
    } else {
        let rects = win.layout.tree.compute_rects(viewport);
        let state = io_state.lock().await;
        for (pid, rect) in rects {
            if let Some(tx) = state.resizers.get(&pid) {
                let r_cols = rect.width.max(2);
                let r_rows = rect.height.max(2);
                to_send.push((tx.clone(), PtySize::new(r_cols, r_rows)));
            }
        }
    }

    for (tx, size) in to_send {
        let _ = tx.send(crate::ResizeRequest { size, ack: None }).await;
    }
}

/// Send an AttachReady::Error and close.
async fn send_ready_error(
    framed: &mut Framed<UnixStream, tokio_util::codec::LengthDelimitedCodec>,
    code: &str,
    message: &str,
) -> anyhow::Result<()> {
    let err = AttachReady::Error {
        code: code.to_string(),
        message: message.to_string(),
    };
    framed.send(Bytes::from(serde_json::to_vec(&err)?)).await?;
    Ok(())
}

/// Main attach loop after handshake. Owns the render compositor and
/// dispatches all client frames.
#[allow(clippy::too_many_arguments)]
async fn run_attach_loop(
    framed: Framed<UnixStream, tokio_util::codec::LengthDelimitedCodec>,
    graph: GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    config: ConfigHandle,
    segments: SegmentCache,
    meta_cache: crate::session_meta::SessionMetaCache,
    onboarding: crate::onboarding::OnboardingHandle,
    daemon_start: std::time::Instant,
    mut session: AttachedSession,
    hello: AttachHello,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let (mut sink, mut stream) = framed.split();

    let (out_tx, mut out_rx) = mpsc::channel::<AttachServerFrame>(64);

    // Spawn the writer task: pulls from out_rx, frames + sends.
    let writer_cancel = cancel.clone();
    let writer = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = writer_cancel.cancelled() => break,
                Some(frame) = out_rx.recv() => {
                    let bytes = match serde_json::to_vec(&frame) {
                        Ok(b) => b,
                        Err(e) => {
                            warn!(error = %e, "attach: serialize failed");
                            continue;
                        }
                    };
                    if sink.send(Bytes::from(bytes)).await.is_err() {
                        debug!("attach: client closed (writer)");
                        break;
                    }
                }
                else => break,
            }
        }
    });

    // Authoritative client screen size. Updated only by Resize frames;
    // the renderer reads but never writes it.
    let client_size: ClientSize = Arc::new(Mutex::new((hello.cols, hello.rows)));

    // Spawn the renderer task.
    let render_cancel = cancel.child_token();
    let render_io = io_state.clone();
    let render_graph = graph.clone();
    let render_tx = out_tx.clone();
    let render_session = Arc::new(Mutex::new(session.clone()));
    let render_session_for_task = render_session.clone();
    let render_client_size = client_size.clone();
    let render_config = config.clone();
    let render_segments = segments.clone();
    let render_meta = meta_cache.clone();
    let render_onboarding = onboarding.clone();
    let renderer = tokio::spawn(async move {
        run_render_loop(
            render_graph,
            render_io,
            render_config,
            render_segments,
            render_meta,
            render_onboarding,
            daemon_start,
            render_session_for_task,
            render_client_size,
            render_tx,
            render_cancel,
        )
        .await;
    });

    // Periodic ping so a dead client is detected within ~PING_INTERVAL.
    let ping_tx = out_tx.clone();
    let ping_cancel = cancel.clone();
    let pinger = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(PING_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await; // skip first immediate tick
        loop {
            tokio::select! {
                _ = ping_cancel.cancelled() => break,
                _ = ticker.tick() => {
                    if ping_tx.send(AttachServerFrame::Ping).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    // Read client frames.
    let mut detached = false;
    // Mouse drag state: when a drag starts on a border cell, we remember
    // which boundary it's grabbing so subsequent Drag events can adjust
    // the layout split ratio.
    let mut mouse_drag: Option<DragState> = None;
    let mut selection_drag = SelectionDrag::None;
    while !detached {
        tokio::select! {
            _ = cancel.cancelled() => break,
            frame = stream.next() => {
                let buf = match frame {
                    Some(Ok(b)) => b,
                    Some(Err(e)) => {
                        warn!(error = %e, "attach: read error");
                        break;
                    }
                    None => {
                        debug!("attach: client disconnected");
                        break;
                    }
                };
                let parsed: AttachClientFrame = match serde_json::from_slice(&buf) {
                    Ok(f) => f,
                    Err(e) => {
                        warn!(error = %e, "attach: client frame parse error");
                        continue;
                    }
                };
                match parsed {
                    AttachClientFrame::Input { data } => {
                        let bytes = match BASE64.decode(data.as_bytes()) {
                            Ok(b) => b,
                            Err(_) => continue,
                        };
                        // Help-overlay capture: while the cheat sheet
                        // is on screen, every keystroke either dismisses
                        // (Esc 0x1b, 'q' 0x71) or is swallowed. We must
                        // not forward to the focused PTY — typing should
                        // not reach the shell behind the overlay.
                        {
                            let mut s = render_session.lock().await;
                            if s.help_visible {
                                let dismiss =
                                    bytes.iter().any(|&b| b == 0x1b || b == b'q' || b == b'Q');
                                if dismiss {
                                    s.help_visible = false;
                                    let pulse =
                                        io_state.lock().await.render_pulse.clone();
                                    pulse.notify_one();
                                }
                                continue;
                            }
                        }
                        let cleared_mouse_selection = {
                            let mut s = render_session.lock().await;
                            if s.copy_mode.is_none()
                                && (s.mouse_selection.is_some() || s.copy_menu.is_some())
                            {
                                s.mouse_selection = None;
                                s.copy_menu = None;
                                true
                            } else {
                                false
                            }
                        };
                        if cleared_mouse_selection {
                            let pulse = io_state.lock().await.render_pulse.clone();
                            pulse.notify_one();
                        }
                        // Copy-mode capture: route bytes through the
                        // copy-mode key handler instead of forwarding
                        // to the PTY. `y` triggers an OSC 52 yank that
                        // is shipped DIRECTLY to the client (not via
                        // the compositor) so it lands as a single
                        // self-contained terminal sequence — most
                        // terminals interpret it before any subsequent
                        // diff bytes overwrite the cursor position.
                        let copy_action = {
                            // Snapshot the bits we need under the lock,
                            // then drop it before computing the pane
                            // size (which itself takes locks).
                            let (active_pane, attached_clone, in_copy) = {
                                let s = render_session.lock().await;
                                (s.active_pane_id, s.clone(), s.copy_mode.is_some())
                            };
                            if in_copy {
                                let (cols, rows) = focused_pane_size(
                                    &graph,
                                    &io_state,
                                    active_pane,
                                    &attached_clone,
                                    &client_size,
                                )
                                .await;
                                let action = {
                                    let state = io_state.lock().await;
                                    let vt = state.vts.get(&active_pane);
                                    let total_lines =
                                        vt.map(|vt| vt.grid().total_lines()).unwrap_or(rows as usize);
                                    let mut s = render_session.lock().await;
                                    if let Some(ref mut cm) = s.copy_mode {
                                        shux_ui::copy_mode_key_with_vt(
                                            &bytes,
                                            cm,
                                            cols,
                                            rows,
                                            total_lines,
                                            vt,
                                        )
                                    } else {
                                        shux_ui::CopyKey::Ignored
                                    }
                                };
                                Some((action, active_pane, cols, rows))
                            } else {
                                None
                            }
                        };
                        if let Some((action, pane_id, cols, rows)) = copy_action {
                            match action {
                                shux_ui::CopyKey::Updated | shux_ui::CopyKey::Ignored => {
                                    let pulse = io_state.lock().await.render_pulse.clone();
                                    pulse.notify_one();
                                }
                                shux_ui::CopyKey::Exit => {
                                    let mut s = render_session.lock().await;
                                    s.copy_mode = None;
                                    drop(s);
                                    let pulse = io_state.lock().await.render_pulse.clone();
                                    pulse.notify_one();
                                }
                                shux_ui::CopyKey::Yank => {
                                    let text = {
                                        let s = render_session.lock().await;
                                        let cm = s.copy_mode.clone();
                                        drop(s);
                                        match cm {
                                            Some(cm) => {
                                                let state = io_state.lock().await;
                                                state
                                                    .vts
                                                    .get(&pane_id)
                                                    .map(|vt| {
                                                        shux_ui::copy_mode::extract_selection(
                                                            vt, &cm, cols, rows,
                                                        )
                                                    })
                                                    .unwrap_or_default()
                                            }
                                            None => String::new(),
                                        }
                                    };
                                    if !text.is_empty() {
                                        let osc = shux_ui::osc52_copy(&text);
                                        let frame = AttachServerFrame::Render {
                                            data: BASE64.encode(&osc),
                                        };
                                        let _ = out_tx.send(frame).await;
                                    }
                                    let mut s = render_session.lock().await;
                                    s.copy_mode = None;
                                    drop(s);
                                    let pulse = io_state.lock().await.render_pulse.clone();
                                    pulse.notify_one();
                                }
                            }
                            continue;
                        }
                        let target = render_session.lock().await.active_pane_id;
                        // Clone the writer Sender out of the map and drop the
                        // PaneIoState mutex BEFORE touching the channel.
                        let writer = {
                            let state = io_state.lock().await;
                            state.writers.get(&target).cloned()
                        };
                        if let Some(tx) = writer {
                            // Use try_send rather than send().await: if the
                            // pane's PTY writer is backpressured (e.g., the
                            // child stopped reading), blocking the whole
                            // attach loop would freeze the user out -- they
                            // wouldn't be able to detach or switch panes.
                            // Dropping the keystroke is the lesser evil.
                            if let Err(e) = tx.try_send(bytes) {
                                tracing::warn!(error = %e, "input dropped (pane backpressured)");
                            }
                        }
                    }
                    AttachClientFrame::Resize { cols, rows } => {
                        {
                            let mut cs = client_size.lock().await;
                            *cs = (cols, rows);
                        }
                        let attached = render_session.lock().await.clone();
                        apply_resize_to_window(&graph, &io_state, &attached, cols, rows).await;
                        let pulse = io_state.lock().await.render_pulse.clone();
                        pulse.notify_one();
                    }
                    AttachClientFrame::Action { kind, args } => {
                        // The user pressed prefix + key — onboarding hint
                        // can dismiss. Cheap idempotent write; first call
                        // persists, subsequent ones short-circuit.
                        onboarding.mark_prefix_discovered().await;

                        if let Err(e) = handle_action(
                            kind,
                            args.clone(),
                            &graph,
                            &io_state,
                            &render_session,
                            &client_size,
                            &cancel,
                        )
                        .await
                        {
                            warn!(?kind, error = %e, "attach: action failed");
                        }

                        // Transient command-feedback overlay in the status
                        // bar's center zone. Resolves the "did my keystroke
                        // do anything?" UX gap for actions whose effect
                        // isn't immediately obvious (kill, zoom, copy).
                        if let Some(label) = action_feedback_label(kind) {
                            let mut s = render_session.lock().await;
                            s.last_action = Some((label.into(), std::time::Instant::now()));
                        }
                        // Layout-changing actions invalidate per-pane PTY
                        // sizes. Re-fan the winsizes so vim/htop/etc. inside
                        // each pane learn their new dimensions.
                        if action_changes_layout(kind) {
                            let attached = render_session.lock().await.clone();
                            let (cols, rows) = *client_size.lock().await;
                            apply_resize_to_window(&graph, &io_state, &attached, cols, rows).await;
                        }
                        let pulse = io_state.lock().await.render_pulse.clone();
                        pulse.notify_one();
                    }
                    AttachClientFrame::Mouse {
                        kind,
                        button,
                        col,
                        row,
                    } => {
                        // Modal guard: swallow mouse events while the
                        // help overlay is visible. Otherwise a click or
                        // drag on the cheat sheet would leak through to
                        // handle_mouse and refocus / resize the pane
                        // behind it. Also clear any in-flight drag so a
                        // resize started just before the overlay opened
                        // doesn't keep ratcheting.
                        if render_session.lock().await.help_visible {
                            mouse_drag = None;
                            selection_drag = SelectionDrag::None;
                            continue;
                        }
                        if handle_mouse_selection(
                            kind,
                            button,
                            col,
                            row,
                            &graph,
                            &io_state,
                            &render_session,
                            &client_size,
                            &out_tx,
                            &mut selection_drag,
                        )
                        .await?
                        {
                            let pulse = io_state.lock().await.render_pulse.clone();
                            pulse.notify_one();
                            continue;
                        }
                        if handle_copy_mode_mouse(
                            kind,
                            button,
                            col,
                            row,
                            &graph,
                            &io_state,
                            &render_session,
                            &client_size,
                            &out_tx,
                            &mut selection_drag,
                        )
                        .await?
                        {
                            let pulse = io_state.lock().await.render_pulse.clone();
                            pulse.notify_one();
                            continue;
                        }
                        if let Err(e) = handle_mouse(
                            kind,
                            button,
                            col,
                            row,
                            &graph,
                            &io_state,
                            &render_session,
                            &client_size,
                            &mut mouse_drag,
                        )
                        .await
                        {
                            warn!(?kind, error = %e, "attach: mouse handle failed");
                        }
                        let pulse = io_state.lock().await.render_pulse.clone();
                        pulse.notify_one();
                    }
                    AttachClientFrame::Detach => {
                        // Detach implies the user found the prefix too.
                        onboarding.mark_prefix_discovered().await;
                        detached = true;
                        let _ = out_tx.send(AttachServerFrame::DetachAck).await;
                    }
                    AttachClientFrame::PrefixTapped => {
                        // Authoritative signal: user has discovered the
                        // prefix even if they bail without sending an
                        // Action (Ctrl+Space → Escape, etc). The OOTB
                        // hint dismisses forever.
                        onboarding.mark_prefix_discovered().await;
                    }
                    AttachClientFrame::Pong => {}
                }
            }
        }
        // Detect: did the active session vanish?
        //
        // We sync graph-derived fields (active_window_id, active_pane_id)
        // FROM the graph snapshot INTO the shared render_session, but
        // never overwrite the whole struct: UI state like
        // `help_visible` lives only in the shared mutex and would be
        // clobbered by a `*rs = session.clone()`. Keep the local
        // `session` in lockstep too so its `session_id` stays valid for
        // the next iteration's snapshot lookup.
        let still_alive = {
            let snap = graph.snapshot();
            let live = snap.sessions.contains_key(&session.session_id);
            if live {
                if let Some(s) = snap.sessions.get(&session.session_id) {
                    session.active_window_id = s.active_window;
                    if let Some(w) = snap.windows.get(&s.active_window) {
                        session.active_pane_id = w.active_pane;
                    }
                    let mut rs = render_session.lock().await;
                    rs.active_window_id = session.active_window_id;
                    rs.active_pane_id = session.active_pane_id;
                }
            }
            live
        };
        if !still_alive {
            let _ = out_tx
                .send(AttachServerFrame::SessionEnded {
                    reason: "session_destroyed".into(),
                })
                .await;
            break;
        }
    }

    drop(out_tx); // closes the writer cleanly
    let _ = writer.await;
    renderer.abort();
    pinger.abort();
    info!(session = %session.name, "attach session ended");
    Ok(())
}

/// Run the per-attach render loop.
///
/// The loop wakes on `render_pulse` notifications (PTY data, action
/// completion) and also fires a low-rate fallback tick (200ms) so cursor
/// blinks and clocks update without external input. After each wake-up
/// it grabs a fresh `SessionGraphSnapshot`, walks all panes in the
/// active window, runs the multi-pane compositor over a `Vec<u8>`
/// buffer, then ships the bytes as a `Render` frame.
#[allow(clippy::too_many_arguments)]
async fn run_render_loop(
    graph: GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    config: ConfigHandle,
    segments: SegmentCache,
    meta_cache: crate::session_meta::SessionMetaCache,
    onboarding: crate::onboarding::OnboardingHandle,
    daemon_start: std::time::Instant,
    session: Arc<Mutex<AttachedSession>>,
    client_size: ClientSize,
    out_tx: mpsc::Sender<AttachServerFrame>,
    cancel: CancellationToken,
) {
    let (mut cols, mut rows) = *client_size.lock().await;
    let initial = config.current();
    let initial_theme = shux_core::theme::Theme::resolve(&initial.theme);
    let cfg = CompositorConfig {
        show_border: false,
        status_bar_height: STATUS_BAR_ROWS,
        border_style: BorderStyle::parse(&initial.appearance.border_style),
        border_colors: shux_ui::BorderColors::from_theme(&initial_theme),
    };
    let buf: Vec<u8> = Vec::with_capacity(64 * 1024);
    let mut compositor: RenderCompositor<Vec<u8>> = RenderCompositor::new(cols, rows, buf, cfg);

    // Send a clear-screen ANSI prelude so the client terminal starts blank.
    let _ = out_tx
        .send(AttachServerFrame::Render {
            data: BASE64.encode(b"\x1b[2J\x1b[H"),
        })
        .await;

    // Fallback tick lets us update clocks etc. even when nothing else
    // happens. The pulse Notify covers the data-driven case.
    let mut tick = tokio::time::interval(Duration::from_millis(200));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Register notify *before* the first render so we never miss a wakeup.
    // `notify_one` enqueues a permit even if no listener exists yet, so
    // the next `notified().await` returns immediately — but we must
    // re-prime the listener after every wake.
    let pulse = io_state.lock().await.render_pulse.clone();
    let mut pulse_listener = Box::pin(pulse.notified());

    // Welcome toast: lazily-initialised first-render instant. Stays
    // None until the first render iteration where we actually start
    // drawing the toast; from there it ages out after WELCOME_TOAST_DWELL.
    let mut welcome_toast_started: Option<std::time::Instant> = None;

    // The config-change notify gives us a fast path for hot-reloads:
    // when the user saves a new ~/.config/shux/config.toml, the watcher
    // task fires this Notify and we redraw immediately with the new
    // appearance / status bar settings.
    let cfg_notify = config.change_notify();
    let mut cfg_listener = Box::pin(cfg_notify.notified());
    let mut last_border_style = initial.appearance.border_style.clone();
    let mut last_theme = initial_theme;
    let mut last_help_visible = false;
    let mut last_overlay_visible = false;
    let mut last_copy_overlay: Option<CopyOverlayStamp> = None;
    let mut last_copy_menu: Option<CopyContextMenu> = None;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = &mut pulse_listener => {
                pulse_listener = Box::pin(pulse.notified());
            }
            _ = &mut cfg_listener => {
                cfg_listener = Box::pin(cfg_notify.notified());
                // Force a full redraw so border-style changes etc.
                // visibly land on the very next frame.
                compositor.force_redraw();
            }
            _ = tick.tick() => {}
        }

        // Resize from authoritative client_size, NOT from VT grid (which
        // would create a self-feeding shrink loop in split mode).
        let (new_cols, new_rows) = *client_size.lock().await;
        if (new_cols, new_rows) != (cols, rows) {
            cols = new_cols;
            rows = new_rows;
            compositor.resize(cols, rows);
        }

        // Apply any config-driven appearance changes to the compositor.
        // We re-read here so live edits land without restart.
        let live_cfg = config.current();
        if live_cfg.appearance.border_style != last_border_style {
            last_border_style = live_cfg.appearance.border_style.clone();
            compositor.set_border_style(BorderStyle::parse(&last_border_style));
        }
        let live_theme = shux_core::theme::Theme::resolve(&live_cfg.theme);
        if live_theme != last_theme {
            last_theme = live_theme;
            compositor.set_border_colors(shux_ui::BorderColors::from_theme(&live_theme));
            last_copy_overlay = None;
        }

        // Build a multi-pane frame snapshot.
        let snap = graph.snapshot();
        let attached = session.lock().await.clone();

        // Toggling the help overlay needs a full redraw — the diffing
        // backend would otherwise leave overlay glyphs on screen after
        // dismiss (the underlying VT cells didn't change). Force a
        // redraw on EITHER edge of the toggle so both reveal and hide
        // produce clean frames.
        if attached.help_visible != last_help_visible {
            compositor.force_redraw();
            last_help_visible = attached.help_visible;
        }
        let overlay_visible_now = attached.copy_mode.is_some()
            || attached.mouse_selection.is_some()
            || attached.copy_menu.is_some();
        if overlay_visible_now != last_overlay_visible {
            compositor.force_redraw();
            last_overlay_visible = overlay_visible_now;
            last_copy_overlay = None;
            last_copy_menu = None;
        }

        let win = match snap.windows.get(&attached.active_window_id) {
            Some(w) => w,
            None => continue,
        };
        let copy_overlay = if let Some(ref cm) = attached.copy_mode {
            let content = current_content_rect(&client_size).await;
            let viewport = current_viewport(&client_size).await;
            let rect = if win.layout.is_zoomed() {
                Some(content)
            } else {
                win.layout
                    .compute_rects(viewport)
                    .into_iter()
                    .find(|(pid, _)| *pid == attached.active_pane_id)
                    .map(|(_, rect)| rect)
            };
            rect.map(|rect| CopyOverlayStamp {
                kind: CopyOverlayKind::Modal,
                pane_id: attached.active_pane_id,
                rect,
                state: cm.clone(),
                theme: live_theme,
            })
        } else if let Some(selection) = attached.mouse_selection.as_ref() {
            pane_rect_for(&graph, &attached, &client_size, selection.pane_id)
                .await
                .map(|rect| CopyOverlayStamp {
                    kind: CopyOverlayKind::MouseSelection,
                    pane_id: selection.pane_id,
                    rect,
                    state: selection.state.clone(),
                    theme: live_theme,
                })
        } else {
            None
        };
        let copy_overlay_changed =
            copy_overlay_needs_base_redraw(last_copy_overlay.as_ref(), copy_overlay.as_ref());
        let copy_menu_changed = attached.copy_menu != last_copy_menu;
        if copy_overlay_changed || copy_menu_changed {
            // The copy cursor/selection is drawn as an overlay after the normal
            // framebuffer diff, so changes to it are invisible to the
            // compositor. Redraw the base frame only when that overlay state
            // changes; doing this every tick makes the pane visibly flicker.
            compositor.force_redraw();
        }

        // Collect pane VT references while holding the io_state lock.
        // We render under the lock to avoid copying VT grids; the lock
        // is released as soon as the compositor finishes.
        let state = io_state.lock().await;
        let mut vt_refs: HashMap<PaneId, &shux_vt::VirtualTerminal> = HashMap::new();
        for pid in win.layout.tree.pane_ids() {
            if let Some(vt) = state.vts.get(&pid) {
                vt_refs.insert(pid, vt);
            }
        }
        // Per-pane titles for the border overlay (PR 4). Read from
        // the graph snapshot, NOT the VT — Pane.title is the
        // priority-resolved value (manual > osc > auto-derived),
        // and the VT only knows about the OSC layer. Skip empty
        // titles so panes without a title get the clean border.
        let mut pane_titles: HashMap<PaneId, String> = HashMap::new();
        for pid in win.layout.tree.pane_ids() {
            if let Some(p) = snap.panes.get(&pid) {
                if !p.title.is_empty() {
                    pane_titles.insert(pid, p.title.clone());
                }
            }
        }
        // Status bar text. Start from the built-in (always-good)
        // segments so OOTB looks the same even when no script segments
        // are configured. Then `populate_bar` appends any
        // `[[statusbar.segment]]` results from the runner cache.
        let live_cfg = config.current();
        let nerd_fonts = live_cfg.appearance.nerd_fonts;
        let prefix_label = prefix_display(&live_cfg.keys.prefix);
        let session_meta = meta_cache.get(attached.session_id).await;
        let onboarding_state = onboarding.current().await;
        let daemon_uptime = daemon_start.elapsed();
        let last_action_ref = attached.last_action.as_ref().map(|(s, i)| (s.as_str(), *i));
        let render_ctx = StatusBarCtx {
            session_id: attached.session_id,
            session_name: &attached.name,
            active_window_id: attached.active_window_id,
            active_pane_id: attached.active_pane_id,
            session_meta: &session_meta,
            onboarding: &onboarding_state,
            daemon_uptime,
            nerd_fonts,
            prefix_label: &prefix_label,
            client_cols: cols,
            copy_mode_active: attached.copy_mode.is_some(),
            last_action: last_action_ref,
        };
        let mut bar = build_status_bar_shared(&snap, &live_theme, &render_ctx);
        populate_bar(&mut bar, &config, &segments).await;

        // Welcome-toast lifecycle: if it's still showing and the
        // first-render-tick has passed, mark seen on the daemon side.
        // The renderer flips `show_welcome_toast = false` and persists
        // `welcome_toast_seen: true` ~3s after first attach via the
        // toast layer below.

        let frame = MultiPaneFrame {
            layout: &win.layout.tree,
            zoom: win.layout.zoom.as_ref(),
            focused: attached.active_pane_id,
            vts: &vt_refs,
            titles: Some(&pane_titles),
            status_bar: Some(&bar),
        };
        // Reset the buffer first so we only ship the new frame's bytes.
        compositor.inner_mut().clear();
        let _ = compositor.render_multi_pane(frame);

        // Copy-mode overlay layer: a cursor block + selection
        // highlight + status hint, scoped to the focused pane's
        // content rect. Drawn BEFORE the help overlay so the help
        // sheet wins z-order if both are somehow active.
        if let Some(ref overlay) = copy_overlay {
            let base_emitted = !compositor.inner().is_empty();
            if copy_overlay_needs_repaint(
                last_copy_overlay.as_ref(),
                copy_overlay.as_ref(),
                base_emitted,
            ) {
                if let Some(vt) = state.vts.get(&overlay.pane_id) {
                    if overlay.state.scroll_offset > 0 {
                        shux_ui::render_copy_view_into(
                            compositor.inner_mut(),
                            overlay.rect,
                            vt,
                            &overlay.state,
                        );
                    }
                    match overlay.kind {
                        CopyOverlayKind::Modal => {
                            shux_ui::render_copy_overlay_with_vt_into(
                                compositor.inner_mut(),
                                overlay.rect,
                                vt,
                                &overlay.state,
                                &overlay.theme,
                            );
                        }
                        CopyOverlayKind::MouseSelection => {
                            shux_ui::copy_mode::render_selection_overlay_with_vt_into(
                                compositor.inner_mut(),
                                overlay.rect,
                                vt,
                                &overlay.state,
                                &overlay.theme,
                            );
                        }
                    }
                } else {
                    match overlay.kind {
                        CopyOverlayKind::Modal => {
                            shux_ui::render_copy_overlay_into(
                                compositor.inner_mut(),
                                overlay.rect,
                                &overlay.state,
                                &overlay.theme,
                            );
                        }
                        CopyOverlayKind::MouseSelection => {
                            shux_ui::copy_mode::render_selection_overlay_into(
                                compositor.inner_mut(),
                                overlay.rect,
                                &overlay.state,
                                &overlay.theme,
                            );
                        }
                    }
                }
            }
        }
        last_copy_overlay = copy_overlay;

        if let Some(menu) = attached.copy_menu {
            shux_ui::copy_mode::render_copy_menu_into(
                compositor.inner_mut(),
                menu.col,
                menu.row,
                cols,
                rows,
                &live_theme,
            );
        }
        last_copy_menu = attached.copy_menu;

        // Help-overlay layer: drawn AFTER the diff'd multipane frame so
        // it covers the cells underneath. Toggling the overlay also
        // forces a full redraw on the next frame so the underlying
        // cells return when the overlay closes — otherwise the
        // compositor's diff would skip those positions because they
        // didn't change in the VT grid.
        if attached.help_visible {
            shux_ui::render_help_overlay_into(compositor.inner_mut(), cols, rows, &live_theme);
        }

        // Welcome toast (first-attach onboarding). Only fires when the
        // user has never seen it (per the onboarding state file).
        // Dwells WELCOME_TOAST_DWELL after first render, then auto-
        // dismisses and marks seen on disk so the next attach is clean.
        if !onboarding_state.welcome_toast_seen && attached.show_welcome_toast {
            let elapsed = welcome_toast_started
                .get_or_insert_with(std::time::Instant::now)
                .elapsed();
            if elapsed < WELCOME_TOAST_DWELL {
                render_welcome_toast(
                    compositor.inner_mut(),
                    cols,
                    rows,
                    &live_theme,
                    &prefix_label,
                    nerd_fonts,
                );
            } else {
                // One-shot persist + flag-flip.
                let onb = onboarding.clone();
                tokio::spawn(async move {
                    onb.mark_welcome_toast_seen().await;
                });
                {
                    let mut s = session.lock().await;
                    s.show_welcome_toast = false;
                }
                compositor.force_redraw();
            }
        }

        // Take the bytes out (drain) and send them.
        let bytes = std::mem::take(compositor.inner_mut());
        // Re-establish capacity for next frame.
        compositor.inner_mut().reserve(64 * 1024);
        drop(state);

        if !bytes.is_empty() {
            let frame = AttachServerFrame::Render {
                data: BASE64.encode(&bytes),
            };
            if out_tx.send(frame).await.is_err() {
                break;
            }
        }
    }
}

/// How long the first-attach welcome toast stays on screen before
/// auto-dismissing. Tuned for "long enough to read, short enough to
/// not get in the way".
const WELCOME_TOAST_DWELL: Duration = Duration::from_secs(3);

// build_status_bar + StatusBarCtx + helpers (action_feedback_label,
// prefix_display, format_uptime) all live in `crate::statusbar_build`
// so the snapshot path (window.snapshot / session.snapshot) can call
// the identical renderer and PNG output matches what an attached
// client sees.
use crate::statusbar_build::{
    StatusBarCtx, action_feedback_label, build as build_status_bar_shared, prefix_display,
};

/// Draw the first-attach welcome toast: a small centered box with
/// the prefix key and three core shortcuts. Renders into the
/// compositor's output buffer using direct ANSI so it sits ON TOP of
/// the multi-pane frame already composited there. Auto-dismisses
/// after `WELCOME_TOAST_DWELL` (see run_render_loop).
fn render_welcome_toast(
    out: &mut Vec<u8>,
    cols: u16,
    rows: u16,
    theme: &shux_core::theme::Theme,
    prefix_label: &str,
    nerd_fonts: bool,
) {
    use std::io::Write;
    if cols < 50 || rows < 8 {
        return; // not enough room
    }
    let icon = if nerd_fonts { "\u{f489}" } else { "◆" };
    let title = format!(" {icon} welcome to shux ");
    let lines: Vec<String> = vec![
        title.clone(),
        String::new(),
        format!("prefix is {prefix_label}"),
        String::new(),
        format!("{prefix_label} ?    open help (every shortcut)"),
        format!("{prefix_label} d    detach (session keeps running)"),
        format!("{prefix_label} |    split vertical"),
        String::new(),
        " press any key to dismiss ".to_string(),
    ];
    let box_w: u16 = (lines
        .iter()
        .map(|s| unicode_width::UnicodeWidthStr::width(s.as_str()))
        .max()
        .unwrap_or(0) as u16)
        + 4;
    let box_h: u16 = lines.len() as u16 + 2;
    let x = (cols.saturating_sub(box_w)) / 2;
    let y = (rows.saturating_sub(box_h)) / 2;

    // Catppuccin-anchored colors via the resolved theme.
    let accent = format!(
        "\x1b[38;2;{};{};{}m",
        theme.status_accent.r, theme.status_accent.g, theme.status_accent.b
    );
    let muted = format!(
        "\x1b[38;2;{};{};{}m",
        theme.status_muted.r, theme.status_muted.g, theme.status_muted.b
    );
    let bg = format!(
        "\x1b[48;2;{};{};{}m",
        theme.status_bg.r, theme.status_bg.g, theme.status_bg.b
    );
    let reset = "\x1b[0m";

    // Top border.
    let _ = write!(out, "\x1b[{};{}H{accent}{bg}╭", y + 1, x + 1);
    for _ in 0..(box_w.saturating_sub(2)) {
        let _ = write!(out, "─");
    }
    let _ = write!(out, "╮{reset}");

    // Body rows.
    for (i, line) in lines.iter().enumerate() {
        let row = y + 2 + i as u16;
        let w = unicode_width::UnicodeWidthStr::width(line.as_str()) as u16;
        let pad_right = box_w.saturating_sub(2).saturating_sub(w).saturating_sub(2); // leave 1-cell pad either side
        let color = if i == 0 { &accent } else { &muted };
        let style = if i == 0 { "\x1b[1m" } else { "" };
        let _ = write!(out, "\x1b[{};{}H{accent}{bg}│{reset}{bg} ", row, x + 1);
        let _ = write!(out, "{color}{style}{line}{reset}{bg}");
        for _ in 0..pad_right {
            let _ = write!(out, " ");
        }
        let _ = write!(out, " {accent}│{reset}");
    }

    // Bottom border.
    let bottom_row = y + box_h;
    let _ = write!(out, "\x1b[{};{}H{accent}{bg}╰", bottom_row, x + 1);
    for _ in 0..(box_w.saturating_sub(2)) {
        let _ = write!(out, "─");
    }
    let _ = write!(out, "╯{reset}");
}

/// State held during a left-button drag that started on a pane border.
/// We snapshot the dragged pane and direction at mouse-down; subsequent
/// Drag events translate the cursor delta into ResizePane calls.
#[derive(Debug, Clone, Copy)]
struct DragState {
    /// The pane whose border the user grabbed (we resize *this* pane).
    target: PaneId,
    /// Which axis the border was on. Vertical border → adjust horizontal
    /// split; horizontal border → adjust vertical split.
    direction: shux_core::layout::Direction,
    /// Last cursor position so we can compute deltas.
    last_col: u16,
    last_row: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionDrag {
    None,
    CopyMode,
    MouseSelection { pane_id: PaneId },
}

/// Look up which pane contains the cell at `(col, row)`. Returns the
/// pane and its rect, or None if the click landed on a border cell or
/// outside the content area.
fn pane_at(
    layout_tree: &shux_core::layout::LayoutNode,
    viewport: Rect,
    col: u16,
    row: u16,
) -> Option<(PaneId, Rect)> {
    layout_tree
        .compute_rects(viewport)
        .into_iter()
        .find(|(_, r)| col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height)
}

/// Detect that a click landed on a vertical or horizontal border cell
/// between two adjacent panes. Returns (the pane on the "earlier" side
/// of the border, axis along which to resize) so the caller can adjust
/// that pane's split ratio. Border cells are the 1-cell gaps between
/// rects that `compute_rects` reserves.
fn border_at(
    layout_tree: &shux_core::layout::LayoutNode,
    viewport: Rect,
    col: u16,
    row: u16,
) -> Option<(PaneId, shux_core::layout::Direction)> {
    use shux_core::layout::Direction;
    let rects = layout_tree.compute_rects(viewport);
    // Find a pane whose right edge is at col-1 and (row is inside its
    // vertical extent) — that's a vertical border between this pane and
    // the next.
    for (pid, r) in &rects {
        if col == r.x + r.width && row >= r.y && row < r.y + r.height {
            return Some((*pid, Direction::Vertical));
        }
        if row == r.y + r.height && col >= r.x && col < r.x + r.width {
            return Some((*pid, Direction::Horizontal));
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
async fn handle_mouse_selection(
    kind: MouseKind,
    button: ProtoMouseButton,
    col: u16,
    row: u16,
    graph: &GraphHandle,
    io_state: &Arc<Mutex<PaneIoState>>,
    session: &Arc<Mutex<AttachedSession>>,
    client_size: &ClientSize,
    out_tx: &mpsc::Sender<AttachServerFrame>,
    drag: &mut SelectionDrag,
) -> anyhow::Result<bool> {
    let attached = session.lock().await.clone();
    if attached.copy_mode.is_some() {
        return Ok(false);
    }

    if let Some(menu) = attached.copy_menu {
        if matches!(kind, MouseKind::Down) {
            let (cols, rows) = *client_size.lock().await;
            let (menu_col, menu_row) =
                shux_ui::copy_mode::copy_menu_origin(menu.col, menu.row, cols, rows);
            let action = shux_ui::copy_mode::copy_menu_action_at(menu_col, menu_row, col, row);
            match action {
                Some(shux_ui::copy_mode::CopyMenuAction::Copy) => {
                    let selection = attached
                        .mouse_selection
                        .as_ref()
                        .filter(|selection| selection.pane_id == menu.pane_id);
                    if let Some(selection) = selection {
                        if let Some(rect) =
                            pane_rect_for(graph, &attached, client_size, selection.pane_id).await
                        {
                            let copied = yank_selection(
                                selection.pane_id,
                                &selection.state,
                                rect,
                                io_state,
                                out_tx,
                            )
                            .await;
                            let mut s = session.lock().await;
                            s.copy_menu = None;
                            if copied {
                                s.last_action =
                                    Some(("copied selection".into(), std::time::Instant::now()));
                            }
                        }
                    } else {
                        session.lock().await.copy_menu = None;
                    }
                }
                Some(shux_ui::copy_mode::CopyMenuAction::Clear) => {
                    let mut s = session.lock().await;
                    s.mouse_selection = None;
                    s.copy_menu = None;
                }
                None => {
                    session.lock().await.copy_menu = None;
                }
            }
            *drag = SelectionDrag::None;
            return Ok(true);
        }
        return Ok(true);
    }

    match (kind, button) {
        (MouseKind::Down, ProtoMouseButton::Left) => {
            let viewport = current_viewport(client_size).await;
            let snap = graph.snapshot();
            let Some(win) = snap.windows.get(&attached.active_window_id) else {
                return Ok(false);
            };
            if !win.layout.is_zoomed() && border_at(&win.layout.tree, viewport, col, row).is_some()
            {
                return Ok(false);
            }
            let hit = if win.layout.is_zoomed() {
                Some((
                    attached.active_pane_id,
                    current_content_rect(client_size).await,
                ))
            } else {
                pane_at(&win.layout.tree, viewport, col, row)
            };
            let Some((pane_id, rect)) = hit else {
                return Ok(false);
            };
            drop(snap);

            if pane_id != attached.active_pane_id {
                let _ = graph.focus_pane(pane_id).await;
            }
            let pos = pane_local_point_clamped(rect, col, row);
            let mut state = shux_ui::CopyModeState::new();
            state.cursor = pos;
            state.anchor = Some(pos);
            let mut s = session.lock().await;
            s.active_pane_id = pane_id;
            s.mouse_selection = Some(MouseSelection { pane_id, state });
            s.copy_menu = None;
            *drag = SelectionDrag::MouseSelection { pane_id };
            Ok(true)
        }
        (MouseKind::Drag, ProtoMouseButton::Left) => {
            let SelectionDrag::MouseSelection { pane_id } = *drag else {
                return Ok(false);
            };
            let Some(rect) = pane_rect_for(graph, &attached, client_size, pane_id).await else {
                *drag = SelectionDrag::None;
                return Ok(true);
            };
            let pos = pane_local_point_clamped(rect, col, row);
            let mut s = session.lock().await;
            if let Some(selection) = s
                .mouse_selection
                .as_mut()
                .filter(|selection| selection.pane_id == pane_id)
            {
                selection.state.cursor = pos;
            }
            Ok(true)
        }
        (MouseKind::Up, ProtoMouseButton::Left) => {
            let SelectionDrag::MouseSelection { pane_id } = *drag else {
                return Ok(false);
            };
            let Some(rect) = pane_rect_for(graph, &attached, client_size, pane_id).await else {
                *drag = SelectionDrag::None;
                return Ok(true);
            };
            let pos = pane_local_point_clamped(rect, col, row);
            let selection = {
                let mut s = session.lock().await;
                if let Some(selection) = s
                    .mouse_selection
                    .as_mut()
                    .filter(|selection| selection.pane_id == pane_id)
                {
                    selection.state.cursor = pos;
                    Some(selection.clone())
                } else {
                    None
                }
            };
            if let Some(selection) = selection {
                let moved = selection
                    .state
                    .anchor
                    .is_some_and(|anchor| anchor != selection.state.cursor);
                if moved {
                    let copied =
                        yank_selection(selection.pane_id, &selection.state, rect, io_state, out_tx)
                            .await;
                    if copied {
                        let mut s = session.lock().await;
                        s.last_action =
                            Some(("copied selection".into(), std::time::Instant::now()));
                    }
                } else {
                    let mut s = session.lock().await;
                    s.mouse_selection = None;
                    s.copy_menu = None;
                }
            }
            *drag = SelectionDrag::None;
            Ok(true)
        }
        (MouseKind::Down, ProtoMouseButton::Right) => {
            let Some(selection) = attached.mouse_selection.as_ref() else {
                return Ok(false);
            };
            let Some(rect) = pane_rect_for(graph, &attached, client_size, selection.pane_id).await
            else {
                session.lock().await.mouse_selection = None;
                return Ok(true);
            };
            if selection_contains_screen_point(&selection.state, rect, col, row) {
                let mut s = session.lock().await;
                s.copy_menu = Some(CopyContextMenu {
                    pane_id: selection.pane_id,
                    col,
                    row,
                });
            } else {
                let mut s = session.lock().await;
                s.mouse_selection = None;
                s.copy_menu = None;
            }
            *drag = SelectionDrag::None;
            Ok(true)
        }
        (MouseKind::Up, _) => {
            if matches!(*drag, SelectionDrag::MouseSelection { .. }) {
                *drag = SelectionDrag::None;
                Ok(true)
            } else {
                Ok(false)
            }
        }
        _ => Ok(false),
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_copy_mode_mouse(
    kind: MouseKind,
    button: ProtoMouseButton,
    col: u16,
    row: u16,
    graph: &GraphHandle,
    io_state: &Arc<Mutex<PaneIoState>>,
    session: &Arc<Mutex<AttachedSession>>,
    client_size: &ClientSize,
    out_tx: &mpsc::Sender<AttachServerFrame>,
    dragging: &mut SelectionDrag,
) -> anyhow::Result<bool> {
    let attached = session.lock().await.clone();
    if attached.copy_mode.is_none() {
        if matches!(*dragging, SelectionDrag::CopyMode) {
            *dragging = SelectionDrag::None;
        }
        return Ok(false);
    }

    let Some(rect) = focused_pane_rect(graph, &attached, client_size).await else {
        *dragging = SelectionDrag::None;
        return Ok(true);
    };

    match kind {
        MouseKind::ScrollUp | MouseKind::ScrollDown => {
            let total_lines = {
                let state = io_state.lock().await;
                state
                    .vts
                    .get(&attached.active_pane_id)
                    .map(|vt| vt.grid().total_lines())
                    .unwrap_or(rect.height as usize)
            };
            let mut s = session.lock().await;
            if let Some(ref mut cm) = s.copy_mode {
                if matches!(kind, MouseKind::ScrollUp) {
                    shux_ui::copy_mode::scroll_up(cm, 3, total_lines, rect.height);
                } else {
                    shux_ui::copy_mode::scroll_down(cm, 3, total_lines, rect.height);
                }
            }
            *dragging = SelectionDrag::None;
        }
        MouseKind::Down => {
            if button != ProtoMouseButton::Left {
                return Ok(true);
            }
            if !point_in_rect(rect, col, row) {
                *dragging = SelectionDrag::None;
                return Ok(true);
            }
            let pos = pane_local_point_clamped(rect, col, row);
            let mut s = session.lock().await;
            if let Some(ref mut cm) = s.copy_mode {
                cm.cursor = pos;
                cm.anchor = Some(pos);
            }
            *dragging = SelectionDrag::CopyMode;
        }
        MouseKind::Drag if matches!(*dragging, SelectionDrag::CopyMode) => {
            if button != ProtoMouseButton::Left {
                return Ok(true);
            }
            let pos = pane_local_point_clamped(rect, col, row);
            let mut s = session.lock().await;
            if let Some(ref mut cm) = s.copy_mode {
                cm.cursor = pos;
            }
        }
        MouseKind::Up if matches!(*dragging, SelectionDrag::CopyMode) => {
            if button != ProtoMouseButton::Left {
                return Ok(true);
            }
            let pos = pane_local_point_clamped(rect, col, row);
            let cm = {
                let mut s = session.lock().await;
                if let Some(ref mut cm) = s.copy_mode {
                    cm.cursor = pos;
                }
                s.copy_mode.clone()
            };
            if let Some(cm) = cm {
                let moved = cm.anchor.is_some_and(|anchor| anchor != cm.cursor);
                if moved {
                    let text = {
                        let state = io_state.lock().await;
                        state
                            .vts
                            .get(&attached.active_pane_id)
                            .map(|vt| {
                                shux_ui::copy_mode::extract_selection(
                                    vt,
                                    &cm,
                                    rect.width,
                                    rect.height,
                                )
                            })
                            .unwrap_or_default()
                    };
                    if !text.is_empty() {
                        let osc = shux_ui::osc52_copy(&text);
                        let frame = AttachServerFrame::Render {
                            data: BASE64.encode(&osc),
                        };
                        let _ = out_tx.send(frame).await;
                    }
                    let mut s = session.lock().await;
                    s.copy_mode = None;
                } else {
                    let mut s = session.lock().await;
                    if let Some(ref mut cm) = s.copy_mode {
                        cm.anchor = None;
                    }
                }
            }
            *dragging = SelectionDrag::None;
        }
        MouseKind::Up => {
            *dragging = SelectionDrag::None;
        }
        _ => {}
    }

    Ok(true)
}

#[allow(clippy::too_many_arguments)]
async fn handle_mouse(
    kind: MouseKind,
    button: ProtoMouseButton,
    col: u16,
    row: u16,
    graph: &GraphHandle,
    _io_state: &Arc<Mutex<PaneIoState>>,
    session: &Arc<Mutex<AttachedSession>>,
    client_size: &ClientSize,
    drag: &mut Option<DragState>,
) -> anyhow::Result<()> {
    let viewport = current_viewport(client_size).await;
    let attached = session.lock().await.clone();
    let snap = graph.snapshot();
    let win = match snap.windows.get(&attached.active_window_id) {
        Some(w) => w,
        None => return Ok(()),
    };
    // Don't treat clicks while zoomed as layout edits — there are no
    // real borders to grab and only one pane to focus.
    if win.layout.is_zoomed() {
        return Ok(());
    }
    let tree = &win.layout.tree;

    match (kind, button) {
        // Left click → if it landed on a pane, focus that pane. If it
        // landed on a border, arm a drag.
        (MouseKind::Down, ProtoMouseButton::Left) => {
            if let Some((pid, dir)) = border_at(tree, viewport, col, row) {
                *drag = Some(DragState {
                    target: pid,
                    direction: dir,
                    last_col: col,
                    last_row: row,
                });
            } else if let Some((pid, _)) = pane_at(tree, viewport, col, row) {
                if pid != attached.active_pane_id {
                    let _ = graph.focus_pane(pid).await;
                    let mut s = session.lock().await;
                    s.active_pane_id = pid;
                }
                *drag = None;
            }
        }
        // Drag while a border-grab is armed → translate delta into a
        // resize. delta_ratio is approximate (works well enough for
        // interactive feel; rounding is bounded by clamp_ratio inside
        // the layout).
        (MouseKind::Drag, ProtoMouseButton::Left) => {
            if let Some(state) = *drag {
                let (delta_axis, span) = match state.direction {
                    shux_core::layout::Direction::Vertical => {
                        (col as i32 - state.last_col as i32, viewport.width as i32)
                    }
                    shux_core::layout::Direction::Horizontal => {
                        (row as i32 - state.last_row as i32, viewport.height as i32)
                    }
                };
                if delta_axis != 0 && span > 0 {
                    let delta_ratio = delta_axis as f32 / span as f32;
                    let _ = graph
                        .resize_pane(state.target, state.direction, delta_ratio, None)
                        .await;
                }
                *drag = Some(DragState {
                    target: state.target,
                    direction: state.direction,
                    last_col: col,
                    last_row: row,
                });
            }
        }
        (MouseKind::Up, ProtoMouseButton::Left) => {
            *drag = None;
        }
        // Scroll wheel: future scrollback navigation hook (task 021,
        // copy mode). For now: noop so the protocol still flows.
        (MouseKind::ScrollUp, _) | (MouseKind::ScrollDown, _) => {}
        _ => {}
    }
    Ok(())
}

fn point_in_rect(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x
        && col < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

fn pane_local_point_clamped(rect: Rect, col: u16, row: u16) -> (u16, u16) {
    let max_col = rect.width.saturating_sub(1);
    let max_row = rect.height.saturating_sub(1);
    let local_col = col.saturating_sub(rect.x).min(max_col);
    let local_row = row.saturating_sub(rect.y).min(max_row);
    (local_col, local_row)
}

/// True if an action mutates the pane layout in a way that changes the
/// rect size of one or more visible panes. Used to decide whether to
/// re-fan PTY winsize after dispatching the action.
fn action_changes_layout(kind: ActionKind) -> bool {
    matches!(
        kind,
        ActionKind::SplitSmart
            | ActionKind::SplitVertical
            | ActionKind::SplitHorizontal
            | ActionKind::ToggleZoom
            | ActionKind::KillPane
            | ActionKind::ResizeLeft
            | ActionKind::ResizeRight
            | ActionKind::ResizeUp
            | ActionKind::ResizeDown
            | ActionKind::NewWindow
            | ActionKind::NextWindow
            | ActionKind::PrevWindow
            | ActionKind::SwitchToWindow
    )
}

/// Compute the focused pane's content rect (cols, rows) — the size
/// copy mode uses to clamp cursor motion. Returns (0, 0) when the
/// pane is not in the active window's layout, which keeps `handle_key`
/// safely a no-op rather than panicking.
///
/// In a zoomed window the visible pane fills the full viewport, so we
/// use the viewport's dimensions instead of the saved split-layout
/// rectangle — otherwise cursor motion would clamp to an unzoomed
/// rect that no longer matches what's on screen.
async fn focused_pane_size(
    graph: &GraphHandle,
    _io_state: &Arc<Mutex<PaneIoState>>,
    pane_id: PaneId,
    attached: &AttachedSession,
    client_size: &ClientSize,
) -> (u16, u16) {
    if pane_id != attached.active_pane_id {
        return (0, 0);
    }
    focused_pane_rect(graph, attached, client_size)
        .await
        .map(|rect| (rect.width, rect.height))
        .unwrap_or((0, 0))
}

async fn focused_pane_rect(
    graph: &GraphHandle,
    attached: &AttachedSession,
    client_size: &ClientSize,
) -> Option<Rect> {
    pane_rect_for(graph, attached, client_size, attached.active_pane_id).await
}

async fn pane_rect_for(
    graph: &GraphHandle,
    attached: &AttachedSession,
    client_size: &ClientSize,
    pane_id: PaneId,
) -> Option<Rect> {
    let content = current_content_rect(client_size).await;
    let viewport = current_viewport(client_size).await;
    let snap = graph.snapshot();
    let win = snap.windows.get(&attached.active_window_id)?;
    if win.layout.is_zoomed() {
        if pane_id == attached.active_pane_id {
            return Some(content);
        }
        return None;
    }
    for (pid, rect) in win.layout.compute_rects(viewport) {
        if pid == pane_id {
            return Some(rect);
        }
    }
    None
}

async fn yank_selection(
    pane_id: PaneId,
    selection: &shux_ui::CopyModeState,
    rect: Rect,
    io_state: &Arc<Mutex<PaneIoState>>,
    out_tx: &mpsc::Sender<AttachServerFrame>,
) -> bool {
    let text = {
        let state = io_state.lock().await;
        state
            .vts
            .get(&pane_id)
            .map(|vt| shux_ui::copy_mode::extract_selection(vt, selection, rect.width, rect.height))
            .unwrap_or_default()
    };
    if text.is_empty() {
        return false;
    }
    let osc = shux_ui::osc52_copy(&text);
    let frame = AttachServerFrame::Render {
        data: BASE64.encode(&osc),
    };
    out_tx.send(frame).await.is_ok()
}

fn selection_contains_screen_point(
    state: &shux_ui::CopyModeState,
    rect: Rect,
    col: u16,
    row: u16,
) -> bool {
    if !point_in_rect(rect, col, row) {
        return false;
    }
    let Some(anchor) = state.anchor else {
        return false;
    };
    let point = pane_local_point_clamped(rect, col, row);
    selection_contains_local_point(anchor, state.cursor, point, rect.width)
}

fn selection_contains_local_point(
    anchor: (u16, u16),
    cursor: (u16, u16),
    point: (u16, u16),
    pane_width: u16,
) -> bool {
    let (start, end) = if anchor.1 < cursor.1 || (anchor.1 == cursor.1 && anchor.0 <= cursor.0) {
        (anchor, cursor)
    } else {
        (cursor, anchor)
    };
    if point.1 < start.1 || point.1 > end.1 {
        return false;
    }
    if start.1 == end.1 {
        return point.0 >= start.0 && point.0 <= end.0;
    }
    if point.1 == start.1 {
        return point.0 >= start.0 && point.0 < pane_width;
    }
    if point.1 == end.1 {
        return point.0 <= end.0;
    }
    true
}

async fn current_content_rect(client_size: &ClientSize) -> Rect {
    let (cols, rows) = *client_size.lock().await;
    Rect::new(0, 0, cols, rows.saturating_sub(STATUS_BAR_ROWS))
}

/// Compute the actual pane viewport (inset for outline + status bar) at
/// the current client size. Used by spatial actions (focus_dir, smart
/// split) so the geometry they reason about matches what the user sees
/// — not a hardcoded 120x40 fiction.
async fn current_viewport(client_size: &ClientSize) -> Rect {
    let (cols, rows) = *client_size.lock().await;
    let content_h = rows.saturating_sub(STATUS_BAR_ROWS);
    if cols >= 3 && content_h >= 3 {
        Rect::new(1, 1, cols - 2, content_h - 2)
    } else {
        Rect::new(0, 0, cols, content_h)
    }
}

/// Dispatch an Action keybinding from the client.
async fn handle_action(
    kind: ActionKind,
    args: shux_rpc::attach::ActionArgs,
    graph: &GraphHandle,
    io_state: &Arc<Mutex<PaneIoState>>,
    session: &Arc<Mutex<AttachedSession>>,
    client_size: &ClientSize,
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    use shux_core::layout::Direction;

    // ToggleHelp is handled before the snapshot — it's a UI-only flip
    // that doesn't touch the graph or PTYs, and needs to fire even
    // (especially) while the overlay is already on screen.
    if matches!(kind, ActionKind::ToggleHelp) {
        let mut s = session.lock().await;
        s.help_visible = !s.help_visible;
        tracing::info!(
            help_visible = s.help_visible,
            "attach: toggled help overlay"
        );
        return Ok(());
    }

    // EnterCopyMode is also a UI-only flip — start a fresh copy-mode
    // session on the currently focused pane. If one is already active
    // (the user pressed prefix+[ twice), reset it back to (0,0)
    // without an anchor, matching tmux's behavior.
    if matches!(kind, ActionKind::EnterCopyMode) {
        let mut s = session.lock().await;
        s.copy_mode = Some(shux_ui::CopyModeState::new());
        tracing::info!("attach: entered copy mode on focused pane");
        return Ok(());
    }

    // While the overlay is visible, swallow every other action — the
    // user is meant to read the cheat sheet, not navigate around behind
    // it. They dismiss with Esc / q (handled in the Input frame path).
    if session.lock().await.help_visible {
        return Ok(());
    }

    let attached = session.lock().await.clone();
    let viewport = current_viewport(client_size).await;

    match kind {
        ActionKind::SplitSmart => split(graph, &attached, None, viewport, io_state, cancel).await,
        ActionKind::SplitVertical => {
            split(
                graph,
                &attached,
                Some(Direction::Vertical),
                viewport,
                io_state,
                cancel,
            )
            .await
        }
        ActionKind::SplitHorizontal => {
            split(
                graph,
                &attached,
                Some(Direction::Horizontal),
                viewport,
                io_state,
                cancel,
            )
            .await
        }
        ActionKind::FocusUp => {
            focus_dir(graph, &attached, NavDirection::Up, viewport, session).await
        }
        ActionKind::FocusDown => {
            focus_dir(graph, &attached, NavDirection::Down, viewport, session).await
        }
        ActionKind::FocusLeft => {
            focus_dir(graph, &attached, NavDirection::Left, viewport, session).await
        }
        ActionKind::FocusRight => {
            focus_dir(graph, &attached, NavDirection::Right, viewport, session).await
        }
        ActionKind::FocusNext => focus_relative(graph, &attached, 1, session).await,
        ActionKind::FocusPrev => focus_relative(graph, &attached, -1, session).await,
        ActionKind::ToggleZoom => zoom(graph, &attached).await,
        ActionKind::KillPane => kill_pane(graph, &attached, io_state).await,
        ActionKind::NewWindow => new_window(graph, &attached, io_state, cancel, session).await,
        ActionKind::NextWindow => switch_window(graph, &attached, 1, session).await,
        ActionKind::PrevWindow => switch_window(graph, &attached, -1, session).await,
        ActionKind::SwitchToWindow => {
            // Codex P2 followup from PR #8 — bare Alt+1..9 lands here.
            // The window_index payload is 1-based; out-of-range
            // requests are silently dropped (matches tmux).
            if let Some(idx_1based) = args.window_index {
                switch_to_window_index(
                    graph,
                    &attached,
                    idx_1based.saturating_sub(1) as usize,
                    session,
                )
                .await
            } else {
                Ok(())
            }
        }
        ActionKind::ResizeLeft => resize_pane(graph, &attached, Direction::Vertical, -0.05).await,
        ActionKind::ResizeRight => resize_pane(graph, &attached, Direction::Vertical, 0.05).await,
        ActionKind::ResizeUp => resize_pane(graph, &attached, Direction::Horizontal, -0.05).await,
        ActionKind::ResizeDown => resize_pane(graph, &attached, Direction::Horizontal, 0.05).await,
        ActionKind::Redraw => Ok(()),
        // Handled above — the early-returns keep these branches
        // unreachable but the match arms are required so adding new
        // ActionKinds keeps failing the compile-time exhaustiveness
        // check.
        ActionKind::ToggleHelp => unreachable!("ToggleHelp short-circuited above"),
        ActionKind::EnterCopyMode => unreachable!("EnterCopyMode short-circuited above"),
    }
}

async fn split(
    graph: &GraphHandle,
    attached: &AttachedSession,
    dir: Option<shux_core::layout::Direction>,
    viewport: Rect,
    io_state: &Arc<Mutex<PaneIoState>>,
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    use shux_core::layout::Direction;

    // Smart split: pick direction based on the focused pane's *real*
    // current dimensions. Wider → vertical, taller → horizontal.
    let direction = match dir {
        Some(d) => d,
        None => {
            let snap = graph.snapshot();
            let win = snap
                .windows
                .get(&attached.active_window_id)
                .ok_or_else(|| anyhow::anyhow!("active window missing"))?;
            let rects = win.layout.compute_rects(viewport);
            let pane_rect = rects
                .iter()
                .find(|(p, _)| *p == attached.active_pane_id)
                .map(|(_, r)| *r)
                .unwrap_or(viewport);
            if pane_rect.width >= pane_rect.height {
                Direction::Vertical
            } else {
                Direction::Horizontal
            }
        }
    };

    let new_pane = graph
        .split_pane(attached.active_pane_id, direction, 0.5)
        .await
        .map_err(|e| anyhow::anyhow!("split failed: {e}"))?;

    crate::spawn_pane_pty(
        new_pane,
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/tmp")),
        Vec::new(),
        shux_pty::handle::PtySize::default(),
        Vec::new(),
        io_state.clone(),
        cancel.clone(),
        graph.clone(),
    )
    .await
    .ok();
    Ok(())
}

async fn focus_dir(
    graph: &GraphHandle,
    attached: &AttachedSession,
    nav: NavDirection,
    viewport: Rect,
    session: &Arc<Mutex<AttachedSession>>,
) -> anyhow::Result<()> {
    // Refuse to change focus while zoomed — the user wouldn't see the
    // change and would be typing into a hidden pane. Falls through as
    // a no-op (the renderer keeps showing the zoomed pane).
    let snap = graph.snapshot();
    if let Some(win) = snap.windows.get(&attached.active_window_id) {
        if win.layout.is_zoomed() {
            return Ok(());
        }
    }
    drop(snap);
    let new_id = graph
        .focus_pane_direction(attached.active_window_id, nav, viewport)
        .await
        .map_err(|e| anyhow::anyhow!("focus_dir failed: {e}"))?;
    if let Some(pid) = new_id {
        let mut s = session.lock().await;
        s.active_pane_id = pid;
    }
    Ok(())
}

async fn focus_relative(
    graph: &GraphHandle,
    attached: &AttachedSession,
    direction: i32,
    session: &Arc<Mutex<AttachedSession>>,
) -> anyhow::Result<()> {
    let snap = graph.snapshot();
    let win = snap
        .windows
        .get(&attached.active_window_id)
        .ok_or_else(|| anyhow::anyhow!("active window missing"))?;
    // Don't change focus while zoomed -- the user wouldn't see the new
    // pane and would be typing into a hidden one.
    if win.layout.is_zoomed() {
        return Ok(());
    }
    let panes = win.layout.tree.pane_ids();
    if panes.len() < 2 {
        return Ok(());
    }
    let cur_idx = panes
        .iter()
        .position(|p| *p == attached.active_pane_id)
        .unwrap_or(0);
    let next_idx = ((cur_idx as i32 + direction).rem_euclid(panes.len() as i32)) as usize;
    let target = panes[next_idx];
    let _ = graph.focus_pane(target).await;
    let mut s = session.lock().await;
    s.active_pane_id = target;
    Ok(())
}

async fn zoom(graph: &GraphHandle, attached: &AttachedSession) -> anyhow::Result<()> {
    let _ = graph
        .zoom_pane(attached.active_pane_id, None)
        .await
        .map_err(|e| anyhow::anyhow!("zoom failed: {e}"))?;
    Ok(())
}

async fn kill_pane(
    graph: &GraphHandle,
    attached: &AttachedSession,
    io_state: &Arc<Mutex<PaneIoState>>,
) -> anyhow::Result<()> {
    let pane_id = attached.active_pane_id;

    // Resolve the pane's window + session BEFORE we mutate anything, so
    // the cascade fallbacks have valid IDs even after destroy_pane bumps
    // the snapshot. Without this, a fresh `shux` session (single pane,
    // single window) silently no-op'd on Ctrl+Space x — destroy_pane
    // returned LastPane, the warn-log went nowhere, the user saw nothing.
    let snap = graph.snapshot();
    let (window_id, session_id) = match snap.panes.get(&pane_id) {
        Some(p) => {
            let sid = snap.windows.get(&p.window_id).map(|w| w.session_id);
            match sid {
                Some(s) => (p.window_id, s),
                None => {
                    warn!(%pane_id, "kill_pane: pane's window has no session");
                    return Ok(());
                }
            }
        }
        None => {
            warn!(%pane_id, "kill_pane: active pane not in snapshot");
            return Ok(());
        }
    };
    drop(snap);

    // tmux-style cascade: pane → window → session. The graph API stays
    // strict (LastPane/LastWindow are real errors for programmatic
    // clients that want pinned semantics); the human-interactive
    // Ctrl+Space x action cascades so the user can always kill what's
    // in front of them. When the cascade reaches destroy_session, the
    // attach render loop notices the session is gone on its next tick
    // and sends SessionEnded — the client detaches naturally.
    match graph.destroy_pane(pane_id, None).await {
        Ok(()) => {
            cleanup_pane_io(io_state, &[pane_id]).await;
            return Ok(());
        }
        Err(GraphError::LastPane) => {
            // Fall through to window kill.
        }
        Err(e) => {
            warn!(error = %e, "kill_pane: destroy_pane failed");
            return Ok(());
        }
    }

    let window_pane_ids: Vec<PaneId> = {
        let snap = graph.snapshot();
        snap.panes
            .values()
            .filter(|p| p.window_id == window_id)
            .map(|p| p.id)
            .collect()
    };

    match graph.destroy_window(window_id, None).await {
        Ok(()) => {
            cleanup_pane_io(io_state, &window_pane_ids).await;
            return Ok(());
        }
        Err(GraphError::LastWindow) => {
            // Fall through to session kill.
        }
        Err(e) => {
            warn!(error = %e, "kill_pane: destroy_window failed");
            return Ok(());
        }
    }

    let session_pane_ids: Vec<PaneId> = {
        let snap = graph.snapshot();
        let win_ids: std::collections::HashSet<WindowId> = snap
            .sessions
            .get(&session_id)
            .map(|s| s.windows.iter().copied().collect())
            .unwrap_or_default();
        snap.panes
            .values()
            .filter(|p| win_ids.contains(&p.window_id))
            .map(|p| p.id)
            .collect()
    };

    if let Err(e) = graph.destroy_session(session_id, None).await {
        warn!(error = %e, "kill_pane: destroy_session failed");
        return Ok(());
    }
    cleanup_pane_io(io_state, &session_pane_ids).await;
    Ok(())
}

/// Drop the PTY-bound writer + resizer entries for `pane_ids`, plus
/// their VTs, then poke the renderer so the disappearance shows up
/// promptly. Kept separate so all three cascade arms (pane / window /
/// session) share the exact same teardown semantics. VT eviction here
/// is explicit-destroy (intentional kill); contrast with the PTY
/// natural-exit path which now lets the VT linger so pane.capture
/// still works for a finished short-lived command.
async fn cleanup_pane_io(io_state: &Arc<Mutex<PaneIoState>>, pane_ids: &[PaneId]) {
    if pane_ids.is_empty() {
        return;
    }
    let mut state = io_state.lock().await;
    let pulse = state.teardown_panes(pane_ids, true);
    drop(state);
    pulse.notify_one();
}

async fn new_window(
    graph: &GraphHandle,
    attached: &AttachedSession,
    io_state: &Arc<Mutex<PaneIoState>>,
    cancel: &CancellationToken,
    session: &Arc<Mutex<AttachedSession>>,
) -> anyhow::Result<()> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/tmp"));
    let snap = graph.snapshot();
    let count = snap
        .sessions
        .get(&attached.session_id)
        .map(|s| s.windows.len())
        .unwrap_or(0);
    let title = format!("window-{}", count + 1);
    let window_id = graph
        .create_window(attached.session_id, title, cwd.clone())
        .await
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    // The graph's create_window also creates an initial pane.
    let snap = graph.snapshot();
    let win = snap
        .windows
        .get(&window_id)
        .ok_or_else(|| anyhow::anyhow!("window vanished after create"))?;
    let pane_id = win.active_pane;
    crate::spawn_pane_pty(
        pane_id,
        cwd,
        Vec::new(),
        shux_pty::handle::PtySize::default(),
        Vec::new(),
        io_state.clone(),
        cancel.clone(),
        graph.clone(),
    )
    .await
    .ok();

    // Focus the new window.
    let _ = graph.focus_window(window_id, None).await;

    let mut s = session.lock().await;
    s.active_window_id = window_id;
    s.active_pane_id = pane_id;
    Ok(())
}

/// Switch directly to the window at `index_0based` in the active
/// session. Out-of-range indices are silently ignored (matches tmux's
/// Alt+1..9 behavior — pressing Alt+5 when only 3 windows exist does
/// nothing rather than wrapping or beeping). Called from the bare
/// Alt+1..9 keybinding path (Codex P2 followup from PR #8).
async fn switch_to_window_index(
    graph: &GraphHandle,
    attached: &AttachedSession,
    index_0based: usize,
    session: &Arc<Mutex<AttachedSession>>,
) -> anyhow::Result<()> {
    let snap = graph.snapshot();
    let sess = snap
        .sessions
        .get(&attached.session_id)
        .ok_or_else(|| anyhow::anyhow!("session missing"))?;
    let target = match sess.windows.get(index_0based) {
        Some(&w) => w,
        None => return Ok(()),
    };
    if target == attached.active_window_id {
        return Ok(());
    }
    let _ = graph.focus_window(target, None).await;

    let new_pane = snap
        .windows
        .get(&target)
        .map(|w| w.active_pane)
        .unwrap_or(attached.active_pane_id);
    let mut s = session.lock().await;
    s.active_window_id = target;
    s.active_pane_id = new_pane;
    Ok(())
}

async fn switch_window(
    graph: &GraphHandle,
    attached: &AttachedSession,
    direction: i32,
    session: &Arc<Mutex<AttachedSession>>,
) -> anyhow::Result<()> {
    let snap = graph.snapshot();
    let sess = snap
        .sessions
        .get(&attached.session_id)
        .ok_or_else(|| anyhow::anyhow!("session missing"))?;
    if sess.windows.len() < 2 {
        return Ok(());
    }
    let cur_idx = sess
        .windows
        .iter()
        .position(|w| *w == attached.active_window_id)
        .unwrap_or(0);
    let next_idx = ((cur_idx as i32 + direction).rem_euclid(sess.windows.len() as i32)) as usize;
    let target = sess.windows[next_idx];
    let _ = graph.focus_window(target, None).await;

    let new_pane = snap
        .windows
        .get(&target)
        .map(|w| w.active_pane)
        .unwrap_or(attached.active_pane_id);
    let mut s = session.lock().await;
    s.active_window_id = target;
    s.active_pane_id = new_pane;
    Ok(())
}

async fn resize_pane(
    graph: &GraphHandle,
    attached: &AttachedSession,
    direction: shux_core::layout::Direction,
    delta: f32,
) -> anyhow::Result<()> {
    let _ = graph
        .resize_pane(attached.active_pane_id, direction, delta, None)
        .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlay_stamp(cursor: (u16, u16)) -> CopyOverlayStamp {
        let mut state = shux_ui::CopyModeState::new();
        state.cursor = cursor;
        CopyOverlayStamp {
            kind: CopyOverlayKind::Modal,
            pane_id: PaneId::new(),
            rect: Rect::new(1, 1, 80, 23),
            state,
            theme: Theme::DEFAULT,
        }
    }

    #[test]
    fn unchanged_copy_overlay_does_not_force_idle_redraw_or_repaint() {
        let stamp = overlay_stamp((0, 0));
        assert!(!copy_overlay_needs_base_redraw(Some(&stamp), Some(&stamp)));
        assert!(!copy_overlay_needs_repaint(
            Some(&stamp),
            Some(&stamp),
            false
        ));
    }

    #[test]
    fn changed_copy_overlay_forces_one_base_redraw_and_repaint() {
        let old = overlay_stamp((0, 0));
        let new = overlay_stamp((1, 0));
        assert!(copy_overlay_needs_base_redraw(Some(&old), Some(&new)));
        assert!(copy_overlay_needs_repaint(Some(&old), Some(&new), false));
    }

    #[test]
    fn unchanged_copy_overlay_repaints_after_underlying_bytes() {
        let stamp = overlay_stamp((0, 0));
        assert!(copy_overlay_needs_repaint(Some(&stamp), Some(&stamp), true));
    }

    #[test]
    fn point_in_rect_uses_half_open_bounds() {
        let rect = Rect::new(2, 3, 10, 5);
        assert!(point_in_rect(rect, 2, 3));
        assert!(point_in_rect(rect, 11, 7));
        assert!(!point_in_rect(rect, 12, 7));
        assert!(!point_in_rect(rect, 11, 8));
        assert!(!point_in_rect(rect, 1, 3));
        assert!(!point_in_rect(rect, 2, 2));
    }

    #[test]
    fn pane_local_point_clamps_to_content_rect() {
        let rect = Rect::new(2, 3, 10, 5);
        assert_eq!(pane_local_point_clamped(rect, 2, 3), (0, 0));
        assert_eq!(pane_local_point_clamped(rect, 11, 7), (9, 4));
        assert_eq!(pane_local_point_clamped(rect, 0, 0), (0, 0));
        assert_eq!(pane_local_point_clamped(rect, 99, 99), (9, 4));
    }

    #[test]
    fn pane_local_point_handles_empty_rect_without_underflow() {
        let rect = Rect::new(4, 5, 0, 0);
        assert_eq!(pane_local_point_clamped(rect, 10, 10), (0, 0));
    }

    #[test]
    fn selection_hit_test_handles_multiline_ranges() {
        let anchor = (3, 1);
        let cursor = (6, 3);
        assert!(selection_contains_local_point(anchor, cursor, (3, 1), 10));
        assert!(selection_contains_local_point(anchor, cursor, (9, 2), 10));
        assert!(selection_contains_local_point(anchor, cursor, (6, 3), 10));
        assert!(!selection_contains_local_point(anchor, cursor, (2, 1), 10));
        assert!(!selection_contains_local_point(anchor, cursor, (7, 3), 10));
        assert!(!selection_contains_local_point(anchor, cursor, (0, 4), 10));
    }

    #[test]
    fn selection_hit_test_handles_reverse_drag() {
        let anchor = (6, 3);
        let cursor = (3, 1);
        assert!(selection_contains_local_point(anchor, cursor, (4, 1), 10));
        assert!(selection_contains_local_point(anchor, cursor, (1, 2), 10));
        assert!(selection_contains_local_point(anchor, cursor, (6, 3), 10));
        assert!(!selection_contains_local_point(anchor, cursor, (2, 1), 10));
        assert!(!selection_contains_local_point(anchor, cursor, (7, 3), 10));
    }

    #[tokio::test]
    async fn current_content_rect_reserves_only_status_row() {
        let size = Arc::new(Mutex::new((120, 40)));
        assert_eq!(current_content_rect(&size).await, Rect::new(0, 0, 120, 39));
    }

    #[tokio::test]
    async fn current_viewport_insets_for_borders_and_status_row() {
        let size = Arc::new(Mutex::new((120, 40)));
        assert_eq!(current_viewport(&size).await, Rect::new(1, 1, 118, 37));
    }

    struct AttachFixture {
        graph: GraphHandle,
        io_state: Arc<Mutex<PaneIoState>>,
        cancel: CancellationToken,
        graph_task: tokio::task::JoinHandle<()>,
        attached: AttachedSession,
        session_id: SessionId,
        first_window: WindowId,
        first_pane: PaneId,
        second_pane: PaneId,
        second_window: WindowId,
        second_window_pane: PaneId,
    }

    impl AttachFixture {
        fn stop(self) {
            self.cancel.cancel();
            self.graph_task.abort();
        }
    }

    async fn attach_fixture() -> AttachFixture {
        let (graph_inner, state) = shux_core::graph::SessionGraph::new();
        let (cmd_tx, cmd_rx) = mpsc::channel(128);
        let cancel = CancellationToken::new();
        let graph_task = {
            let cancel = cancel.clone();
            tokio::spawn(async move {
                shux_core::graph::run_graph_loop(graph_inner, cmd_rx, cancel).await;
            })
        };
        let graph = GraphHandle::new(cmd_tx, state);
        let io_state = Arc::new(Mutex::new(PaneIoState::new()));
        let cwd = std::env::temp_dir();

        let session_id = graph
            .create_session_with_command(
                "attach-test".to_string(),
                cwd.clone(),
                vec!["bash".to_string()],
            )
            .await
            .expect("create session");
        let snap = graph.snapshot();
        let sess = snap.sessions.get(&session_id).expect("session");
        let first_window = sess.active_window;
        let first_pane = snap
            .windows
            .get(&first_window)
            .expect("first window")
            .active_pane;
        drop(snap);

        let second_pane = graph
            .split_pane(first_pane, shux_core::layout::Direction::Vertical, 0.5)
            .await
            .expect("split pane");
        graph
            .focus_pane(first_pane)
            .await
            .expect("focus first pane");

        let second_window = graph
            .create_window(session_id, "logs".to_string(), cwd)
            .await
            .expect("create second window");
        let second_window_pane = graph
            .snapshot()
            .windows
            .get(&second_window)
            .expect("second window")
            .active_pane;
        graph
            .focus_window(first_window, None)
            .await
            .expect("focus first window");
        graph
            .focus_pane(first_pane)
            .await
            .expect("focus first pane");

        let attached = AttachedSession {
            session_id,
            name: "attach-test".to_string(),
            active_window_id: first_window,
            active_pane_id: first_pane,
            help_visible: false,
            copy_mode: None,
            mouse_selection: None,
            copy_menu: None,
            last_action: None,
            show_welcome_toast: true,
        };

        AttachFixture {
            graph,
            io_state,
            cancel,
            graph_task,
            attached,
            session_id,
            first_window,
            first_pane,
            second_pane,
            second_window,
            second_window_pane,
        }
    }

    async fn seed_io_for_pane(
        io_state: &Arc<Mutex<PaneIoState>>,
        pane_id: PaneId,
    ) -> (
        mpsc::Receiver<Vec<u8>>,
        mpsc::Receiver<crate::ResizeRequest>,
        CancellationToken,
    ) {
        let (writer_tx, writer_rx) = mpsc::channel(8);
        let (resize_tx, resize_rx) = mpsc::channel(8);
        let shutdown = CancellationToken::new();
        let mut vt = shux_vt::VirtualTerminal::new(6, 40);
        vt.process(b"hello world\r\nsecond line\r\nthird\r\n");
        let mut state = io_state.lock().await;
        state.writers.insert(pane_id, writer_tx);
        state.resizers.insert(pane_id, resize_tx);
        state.shutdowns.insert(pane_id, shutdown.clone());
        state.vts.insert(pane_id, vt);
        (writer_rx, resize_rx, shutdown)
    }

    async fn recv_render_text(out_rx: &mut mpsc::Receiver<AttachServerFrame>) -> String {
        let frame = tokio::time::timeout(Duration::from_secs(1), out_rx.recv())
            .await
            .expect("render frame timeout")
            .expect("render frame");
        match frame {
            AttachServerFrame::Render { data } => {
                let decoded = BASE64.decode(data.as_bytes()).expect("render base64");
                String::from_utf8(decoded).expect("render utf8")
            }
            other => panic!("expected render frame, got {other:?}"),
        }
    }

    fn find_pane_and_border_points(
        graph: &GraphHandle,
        window_id: WindowId,
        viewport: Rect,
    ) -> ((u16, u16), (u16, u16), (u16, u16)) {
        let snap = graph.snapshot();
        let win = snap.windows.get(&window_id).expect("window");
        let rects = win.layout.compute_rects(viewport);
        assert!(rects.len() >= 2, "fixture should have split panes");
        let first = rects[0].1;
        let second = rects[1].1;
        let first_point = (first.x + 1, first.y + 1);
        let second_point = (second.x + 1, second.y + 1);

        for col in viewport.x..viewport.x + viewport.width {
            for row in viewport.y..viewport.y + viewport.height {
                if border_at(&win.layout.tree, viewport, col, row).is_some() {
                    return (first_point, second_point, (col, row));
                }
            }
        }
        panic!("split border not found");
    }

    #[tokio::test]
    async fn resize_fanout_uses_layout_rects_and_zoomed_content_size() {
        let fixture = attach_fixture().await;
        let (_, mut first_resize_rx, _) =
            seed_io_for_pane(&fixture.io_state, fixture.first_pane).await;
        let (_, mut second_resize_rx, _) =
            seed_io_for_pane(&fixture.io_state, fixture.second_pane).await;
        let (_, mut hidden_resize_rx, _) =
            seed_io_for_pane(&fixture.io_state, fixture.second_window_pane).await;

        apply_resize_to_window(
            &fixture.graph,
            &fixture.io_state,
            &fixture.attached,
            100,
            30,
        )
        .await;

        let first = tokio::time::timeout(Duration::from_secs(1), first_resize_rx.recv())
            .await
            .expect("first resize")
            .expect("first resize request");
        let second = tokio::time::timeout(Duration::from_secs(1), second_resize_rx.recv())
            .await
            .expect("second resize")
            .expect("second resize request");
        assert_eq!(first.size.rows, 27);
        assert_eq!(second.size.rows, 27);
        assert!(first.size.cols < 98, "split pane should not get full width");
        assert!(
            second.size.cols < 98,
            "split pane should not get full width"
        );
        assert!(hidden_resize_rx.try_recv().is_err());

        fixture
            .graph
            .zoom_pane(fixture.first_pane, None)
            .await
            .expect("zoom active pane");
        apply_resize_to_window(
            &fixture.graph,
            &fixture.io_state,
            &fixture.attached,
            100,
            30,
        )
        .await;

        let first_zoomed = first_resize_rx.recv().await.expect("first zoom resize");
        let second_zoomed = second_resize_rx.recv().await.expect("second zoom resize");
        assert_eq!((first_zoomed.size.cols, first_zoomed.size.rows), (100, 29));
        assert_eq!(
            (second_zoomed.size.cols, second_zoomed.size.rows),
            (100, 29)
        );

        fixture.stop();
    }

    #[tokio::test]
    async fn attach_action_state_machine_updates_ui_focus_zoom_resize_and_windows() {
        let fixture = attach_fixture().await;
        let session = Arc::new(Mutex::new(fixture.attached.clone()));
        let client_size = Arc::new(Mutex::new((100, 30)));

        handle_action(
            ActionKind::ToggleHelp,
            Default::default(),
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &fixture.cancel,
        )
        .await
        .expect("toggle help on");
        assert!(session.lock().await.help_visible);

        handle_action(
            ActionKind::FocusNext,
            Default::default(),
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &fixture.cancel,
        )
        .await
        .expect("focus swallowed by help");
        assert_eq!(session.lock().await.active_pane_id, fixture.first_pane);

        handle_action(
            ActionKind::ToggleHelp,
            Default::default(),
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &fixture.cancel,
        )
        .await
        .expect("toggle help off");
        handle_action(
            ActionKind::EnterCopyMode,
            Default::default(),
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &fixture.cancel,
        )
        .await
        .expect("enter copy mode");
        assert!(session.lock().await.copy_mode.is_some());

        handle_action(
            ActionKind::FocusNext,
            Default::default(),
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &fixture.cancel,
        )
        .await
        .expect("focus next");
        assert_eq!(session.lock().await.active_pane_id, fixture.second_pane);
        handle_action(
            ActionKind::FocusPrev,
            Default::default(),
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &fixture.cancel,
        )
        .await
        .expect("focus prev");
        assert_eq!(session.lock().await.active_pane_id, fixture.first_pane);

        handle_action(
            ActionKind::ToggleZoom,
            Default::default(),
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &fixture.cancel,
        )
        .await
        .expect("zoom");
        assert!(
            fixture
                .graph
                .snapshot()
                .windows
                .get(&fixture.first_window)
                .expect("first window")
                .layout
                .is_zoomed()
        );
        handle_action(
            ActionKind::ToggleZoom,
            Default::default(),
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &fixture.cancel,
        )
        .await
        .expect("unzoom");
        handle_action(
            ActionKind::ResizeRight,
            Default::default(),
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &fixture.cancel,
        )
        .await
        .expect("resize right");

        handle_action(
            ActionKind::SwitchToWindow,
            shux_rpc::attach::ActionArgs {
                window_index: Some(2),
                ..Default::default()
            },
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &fixture.cancel,
        )
        .await
        .expect("switch to second window");
        let switched = session.lock().await.clone();
        assert_eq!(switched.active_window_id, fixture.second_window);
        assert_eq!(switched.active_pane_id, fixture.second_window_pane);

        handle_action(
            ActionKind::NextWindow,
            Default::default(),
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &fixture.cancel,
        )
        .await
        .expect("wrap next window");
        assert_eq!(session.lock().await.active_window_id, fixture.first_window);

        fixture.stop();
    }

    #[tokio::test]
    async fn kill_pane_cleans_target_io_then_cascades_singleton_session() {
        let fixture = attach_fixture().await;
        let (_, _, first_shutdown) = seed_io_for_pane(&fixture.io_state, fixture.first_pane).await;
        let (_, _, second_shutdown) =
            seed_io_for_pane(&fixture.io_state, fixture.second_pane).await;

        let mut attached = fixture.attached.clone();
        attached.active_pane_id = fixture.second_pane;
        kill_pane(&fixture.graph, &attached, &fixture.io_state)
            .await
            .expect("kill split pane");
        {
            let state = fixture.io_state.lock().await;
            assert!(state.writers.contains_key(&fixture.first_pane));
            assert!(!state.writers.contains_key(&fixture.second_pane));
            assert!(state.vts.contains_key(&fixture.first_pane));
            assert!(!state.vts.contains_key(&fixture.second_pane));
        }
        assert!(!first_shutdown.is_cancelled());
        assert!(second_shutdown.is_cancelled());
        assert!(
            !fixture
                .graph
                .snapshot()
                .panes
                .contains_key(&fixture.second_pane)
        );
        fixture.stop();

        let singleton = attach_fixture().await;
        let (_, _, lone_shutdown) =
            seed_io_for_pane(&singleton.io_state, singleton.second_window_pane).await;
        let attached = AttachedSession {
            active_window_id: singleton.second_window,
            active_pane_id: singleton.second_window_pane,
            ..singleton.attached.clone()
        };
        kill_pane(&singleton.graph, &attached, &singleton.io_state)
            .await
            .expect("kill only pane in non-only window");
        assert!(lone_shutdown.is_cancelled());
        assert!(
            !singleton
                .graph
                .snapshot()
                .windows
                .contains_key(&singleton.second_window)
        );

        let last = attach_fixture().await;
        let (_, _, last_shutdown) = seed_io_for_pane(&last.io_state, last.first_pane).await;
        let attached = AttachedSession {
            active_window_id: last.first_window,
            active_pane_id: last.first_pane,
            ..last.attached.clone()
        };
        last.graph
            .destroy_pane(last.second_pane, None)
            .await
            .expect("remove split pane so session is singleton");
        last.graph
            .destroy_window(last.second_window, None)
            .await
            .expect("remove second window so session is singleton");
        kill_pane(&last.graph, &attached, &last.io_state)
            .await
            .expect("kill singleton session");
        assert!(last_shutdown.is_cancelled());
        assert!(
            !last
                .graph
                .snapshot()
                .sessions
                .contains_key(&last.session_id)
        );

        singleton.stop();
        last.stop();
    }

    #[tokio::test]
    async fn copy_helpers_render_toast_and_emit_osc52_selection() {
        let fixture = attach_fixture().await;
        seed_io_for_pane(&fixture.io_state, fixture.first_pane).await;

        let mut tiny = Vec::new();
        render_welcome_toast(&mut tiny, 20, 4, &Theme::DEFAULT, "C-Space", false);
        assert!(tiny.is_empty());

        let mut toast = Vec::new();
        render_welcome_toast(&mut toast, 80, 24, &Theme::DEFAULT, "C-Space", false);
        let toast = String::from_utf8(toast).expect("toast utf8");
        assert!(toast.contains("welcome to shux"));
        assert!(toast.contains("C-Space ?"));

        let rect = Rect::new(4, 3, 20, 6);
        let mut selection = shux_ui::CopyModeState::new();
        selection.anchor = Some((0, 0));
        selection.cursor = (4, 0);
        assert!(selection_contains_screen_point(&selection, rect, 6, 3));
        assert!(!selection_contains_screen_point(&selection, rect, 3, 3));

        let (out_tx, mut out_rx) = mpsc::channel(2);
        assert!(
            yank_selection(
                fixture.first_pane,
                &selection,
                rect,
                &fixture.io_state,
                &out_tx,
            )
            .await
        );
        let frame = out_rx.recv().await.expect("render frame");
        match frame {
            AttachServerFrame::Render { data } => {
                let decoded = BASE64.decode(data.as_bytes()).expect("osc52 base64");
                let text = String::from_utf8(decoded).expect("osc52 utf8");
                assert!(text.starts_with("\x1b]52;c;"));
                assert!(text.ends_with("\x07"));
            }
            other => panic!("expected render frame, got {other:?}"),
        }

        fixture.stop();
    }

    #[tokio::test]
    async fn mouse_selection_copies_opens_menu_and_clears_without_losing_focus() {
        let fixture = attach_fixture().await;
        seed_io_for_pane(&fixture.io_state, fixture.first_pane).await;
        let session = Arc::new(Mutex::new(fixture.attached.clone()));
        let client_size = Arc::new(Mutex::new((100, 30)));
        let (out_tx, mut out_rx) = mpsc::channel(4);
        let mut drag = SelectionDrag::None;

        assert!(
            handle_mouse_selection(
                MouseKind::Down,
                ProtoMouseButton::Left,
                2,
                2,
                &fixture.graph,
                &fixture.io_state,
                &session,
                &client_size,
                &out_tx,
                &mut drag,
            )
            .await
            .expect("selection down")
        );
        assert_eq!(
            drag,
            SelectionDrag::MouseSelection {
                pane_id: fixture.first_pane
            }
        );

        handle_mouse_selection(
            MouseKind::Drag,
            ProtoMouseButton::Left,
            6,
            2,
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &out_tx,
            &mut drag,
        )
        .await
        .expect("selection drag");
        handle_mouse_selection(
            MouseKind::Up,
            ProtoMouseButton::Left,
            6,
            2,
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &out_tx,
            &mut drag,
        )
        .await
        .expect("selection up");

        let copied = recv_render_text(&mut out_rx).await;
        assert!(copied.starts_with("\x1b]52;c;"));
        assert!(session.lock().await.last_action.is_some());

        assert!(
            handle_mouse_selection(
                MouseKind::Down,
                ProtoMouseButton::Right,
                3,
                2,
                &fixture.graph,
                &fixture.io_state,
                &session,
                &client_size,
                &out_tx,
                &mut drag,
            )
            .await
            .expect("open copy menu")
        );
        let menu = session.lock().await.copy_menu.expect("copy menu");
        let (menu_col, menu_row) =
            shux_ui::copy_mode::copy_menu_origin(menu.col, menu.row, 100, 30);
        assert!(
            handle_mouse_selection(
                MouseKind::Down,
                ProtoMouseButton::Left,
                menu_col + 1,
                menu_row + 1,
                &fixture.graph,
                &fixture.io_state,
                &session,
                &client_size,
                &out_tx,
                &mut drag,
            )
            .await
            .expect("clear menu action")
        );
        let cleared = session.lock().await;
        assert!(cleared.mouse_selection.is_none());
        assert!(cleared.copy_menu.is_none());

        fixture.stop();
    }

    #[tokio::test]
    async fn copy_mode_mouse_scrolls_drags_copies_and_handles_non_left_clicks() {
        let fixture = attach_fixture().await;
        seed_io_for_pane(&fixture.io_state, fixture.first_pane).await;
        let mut attached = fixture.attached.clone();
        attached.copy_mode = Some(shux_ui::CopyModeState::new());
        let session = Arc::new(Mutex::new(attached));
        let client_size = Arc::new(Mutex::new((100, 30)));
        let (out_tx, mut out_rx) = mpsc::channel(4);
        let mut drag = SelectionDrag::None;

        assert!(
            handle_copy_mode_mouse(
                MouseKind::ScrollUp,
                ProtoMouseButton::None,
                2,
                2,
                &fixture.graph,
                &fixture.io_state,
                &session,
                &client_size,
                &out_tx,
                &mut drag,
            )
            .await
            .expect("scroll up")
        );
        assert!(
            handle_copy_mode_mouse(
                MouseKind::Down,
                ProtoMouseButton::Right,
                2,
                2,
                &fixture.graph,
                &fixture.io_state,
                &session,
                &client_size,
                &out_tx,
                &mut drag,
            )
            .await
            .expect("ignore right down")
        );
        assert_eq!(drag, SelectionDrag::None);

        handle_copy_mode_mouse(
            MouseKind::Down,
            ProtoMouseButton::Left,
            2,
            2,
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &out_tx,
            &mut drag,
        )
        .await
        .expect("copy mode down");
        assert_eq!(drag, SelectionDrag::CopyMode);
        handle_copy_mode_mouse(
            MouseKind::Drag,
            ProtoMouseButton::Left,
            6,
            2,
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &out_tx,
            &mut drag,
        )
        .await
        .expect("copy mode drag");
        handle_copy_mode_mouse(
            MouseKind::Up,
            ProtoMouseButton::Left,
            6,
            2,
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &out_tx,
            &mut drag,
        )
        .await
        .expect("copy mode up");

        let copied = recv_render_text(&mut out_rx).await;
        assert!(copied.starts_with("\x1b]52;c;"));
        assert!(session.lock().await.copy_mode.is_none());
        assert_eq!(drag, SelectionDrag::None);

        fixture.stop();
    }

    #[tokio::test]
    async fn mouse_focus_border_drag_and_zoomed_noop_follow_layout_state() {
        let fixture = attach_fixture().await;
        let session = Arc::new(Mutex::new(fixture.attached.clone()));
        let client_size = Arc::new(Mutex::new((100, 30)));
        let viewport = current_viewport(&client_size).await;
        let (first_point, second_point, border_point) =
            find_pane_and_border_points(&fixture.graph, fixture.first_window, viewport);
        let mut drag = None;

        handle_mouse(
            MouseKind::Down,
            ProtoMouseButton::Left,
            second_point.0,
            second_point.1,
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &mut drag,
        )
        .await
        .expect("focus second pane");
        assert_eq!(session.lock().await.active_pane_id, fixture.second_pane);
        assert!(drag.is_none());

        handle_mouse(
            MouseKind::Down,
            ProtoMouseButton::Left,
            border_point.0,
            border_point.1,
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &mut drag,
        )
        .await
        .expect("arm border drag");
        let armed = drag.expect("drag armed");
        assert_eq!(armed.target, fixture.first_pane);

        handle_mouse(
            MouseKind::Drag,
            ProtoMouseButton::Left,
            border_point.0 + 4,
            border_point.1,
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &mut drag,
        )
        .await
        .expect("drag border");
        assert_eq!(drag.expect("updated drag").last_col, border_point.0 + 4);
        handle_mouse(
            MouseKind::Up,
            ProtoMouseButton::Left,
            border_point.0 + 4,
            border_point.1,
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &mut drag,
        )
        .await
        .expect("release border");
        assert!(drag.is_none());

        fixture
            .graph
            .zoom_pane(fixture.second_pane, None)
            .await
            .expect("zoom pane");
        let before = session.lock().await.active_pane_id;
        handle_mouse(
            MouseKind::Down,
            ProtoMouseButton::Left,
            first_point.0,
            first_point.1,
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &mut drag,
        )
        .await
        .expect("zoomed mouse noop");
        assert_eq!(session.lock().await.active_pane_id, before);

        fixture.stop();
    }

    #[tokio::test]
    async fn attach_connection_routes_handshake_resize_actions_input_and_detach() {
        let fixture = attach_fixture().await;
        let (mut writer_rx, mut resize_rx, _) =
            seed_io_for_pane(&fixture.io_state, fixture.first_pane).await;

        let temp = tempfile::tempdir().expect("tempdir");
        let config = ConfigHandle::load_or_default(&temp.path().join("missing.toml"));
        let segments = SegmentCache::new();
        let meta_cache = crate::session_meta::SessionMetaCache::new();
        let onboarding = crate::onboarding::OnboardingHandle::from_state_for_test(
            crate::onboarding::OnboardingState {
                prefix_discovered: false,
                welcome_toast_seen: true,
            },
        );
        let (server_stream, client_stream) = UnixStream::pair().expect("unix pair");
        let server_cancel = fixture.cancel.child_token();
        let server = {
            let graph = fixture.graph.clone();
            let io = fixture.io_state.clone();
            let config = config.clone();
            let segments = segments.clone();
            let meta = meta_cache.clone();
            let onboarding = onboarding.clone();
            let cancel = server_cancel.clone();
            tokio::spawn(async move {
                handle_attach_connection(
                    server_stream,
                    graph,
                    io,
                    config,
                    segments,
                    meta,
                    onboarding,
                    std::time::Instant::now(),
                    cancel,
                )
                .await
            })
        };

        let mut framed = Framed::new(client_stream, create_codec());
        let hello = AttachHello {
            protocol: ATTACH_PROTOCOL_VERSION,
            session_name: Some(fixture.attached.name.clone()),
            cols: 90,
            rows: 24,
            client_version: "test".to_string(),
        };
        framed
            .send(Bytes::from(serde_json::to_vec(&hello).expect("hello json")))
            .await
            .expect("send hello");
        let ready_buf = tokio::time::timeout(Duration::from_secs(1), framed.next())
            .await
            .expect("ready timeout")
            .expect("ready frame")
            .expect("ready bytes");
        let ready: AttachReady = serde_json::from_slice(&ready_buf).expect("ready json");
        match ready {
            AttachReady::Ok {
                session_name,
                active_pane_id,
                ..
            } => {
                assert_eq!(session_name, fixture.attached.name);
                assert_eq!(active_pane_id, fixture.first_pane.to_string());
            }
            other => panic!("expected ready ok, got {other:?}"),
        }

        let initial_resize = tokio::time::timeout(Duration::from_secs(1), resize_rx.recv())
            .await
            .expect("initial resize timeout")
            .expect("initial resize");
        assert_eq!(initial_resize.size.rows, 21);

        framed
            .send(Bytes::from(
                serde_json::to_vec(&AttachClientFrame::Action {
                    kind: ActionKind::ToggleHelp,
                    args: Default::default(),
                })
                .expect("action json"),
            ))
            .await
            .expect("send action");
        framed
            .send(Bytes::from(
                serde_json::to_vec(&AttachClientFrame::Input {
                    data: BASE64.encode(b"abc"),
                })
                .expect("input json"),
            ))
            .await
            .expect("send swallowed input");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(writer_rx.try_recv().is_err());

        framed
            .send(Bytes::from(
                serde_json::to_vec(&AttachClientFrame::Input {
                    data: BASE64.encode(b"q"),
                })
                .expect("input json"),
            ))
            .await
            .expect("dismiss help");
        framed
            .send(Bytes::from(
                serde_json::to_vec(&AttachClientFrame::Input {
                    data: BASE64.encode(b"ls\n"),
                })
                .expect("input json"),
            ))
            .await
            .expect("send input");
        let written = tokio::time::timeout(Duration::from_secs(1), writer_rx.recv())
            .await
            .expect("writer timeout")
            .expect("writer bytes");
        assert_eq!(written, b"ls\n");

        framed
            .send(Bytes::from(
                serde_json::to_vec(&AttachClientFrame::Resize {
                    cols: 100,
                    rows: 30,
                })
                .expect("resize json"),
            ))
            .await
            .expect("send resize");
        let resized = tokio::time::timeout(Duration::from_secs(1), resize_rx.recv())
            .await
            .expect("resize timeout")
            .expect("resize request");
        assert_eq!(resized.size.rows, 27);

        framed
            .send(Bytes::from(
                serde_json::to_vec(&AttachClientFrame::PrefixTapped).expect("prefix json"),
            ))
            .await
            .expect("send prefix tapped");
        framed
            .send(Bytes::from(
                serde_json::to_vec(&AttachClientFrame::Detach).expect("detach json"),
            ))
            .await
            .expect("send detach");

        let mut saw_detach = false;
        for _ in 0..8 {
            let Ok(next) = tokio::time::timeout(Duration::from_millis(100), framed.next()).await
            else {
                break;
            };
            let Some(frame) = next.transpose().expect("server frame bytes") else {
                break;
            };
            let parsed: AttachServerFrame =
                serde_json::from_slice(&frame).expect("server frame json");
            if matches!(parsed, AttachServerFrame::DetachAck) {
                saw_detach = true;
                break;
            }
        }
        assert!(onboarding.current().await.prefix_discovered);

        drop(framed);
        server_cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(1), server).await;
        assert!(saw_detach || writer_rx.try_recv().is_err());
        fixture.stop();
    }
}
