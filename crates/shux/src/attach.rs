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
use shux_core::graph::GraphHandle;
use shux_core::layout::{NavDirection, Rect};
use shux_core::model::{PaneId, SessionId, WindowId};
use shux_pty::handle::PtySize;
use shux_rpc::attach::{
    ATTACH_PROTOCOL_VERSION, ActionKind, AttachClientFrame, AttachHello, AttachReady,
    AttachServerFrame, MouseButton as ProtoMouseButton, MouseKind,
};
use shux_rpc::create_codec;
use shux_ui::{
    BorderStyle, CompositorConfig, MultiPaneFrame, RenderCompositor, StatusBar, StatusSegment,
};

use crate::PaneIoState;

/// Client-screen dimensions (cols, rows) tracked per attached client.
/// Used as the authoritative source of size when computing per-pane rects
/// and PTY winsize — never inferred from the VT grid (which would create
/// a self-feeding shrink loop).
type ClientSize = Arc<Mutex<(u16, u16)>>;

/// Status-bar rows reserved at the bottom of the client screen.
const STATUS_BAR_ROWS: u16 = 1;

/// Total time the daemon will wait for the AttachHello frame before
/// dropping the connection. Prevents slowloris-style blocking.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// How often the daemon pings the client to detect dead peers.
const PING_INTERVAL: Duration = Duration::from_secs(15);

/// Run the attach UDS listener. Each accepted connection spawns an
/// independent attach session task. Runs until `cancel` fires.
pub async fn run_attach_server(
    socket_path: std::path::PathBuf,
    graph: GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    config: ConfigHandle,
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
                        let c = cancel.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_attach_connection(stream, g, io, cfg, c).await {
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
async fn handle_attach_connection(
    stream: UnixStream,
    graph: GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    config: ConfigHandle,
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
    let resolved = resolve_or_create_session(&graph, &hello.session_name).await;
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
                io_state.clone(),
                cancel.clone(),
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
    run_attach_loop(framed, graph, io_state, config, session, hello, cancel).await
}

#[derive(Debug, Clone)]
struct AttachedSession {
    session_id: SessionId,
    name: String,
    active_window_id: WindowId,
    active_pane_id: PaneId,
}

/// Find a session by name, or create it (with one window + one pane) if
/// missing. Mirrors `shux new -s <name>` semantics.
async fn resolve_or_create_session(
    graph: &GraphHandle,
    name: &Option<String>,
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
        });
    }

    drop(snap);
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/tmp"));
    let session_id = graph
        .create_session(target_name.clone(), cwd)
        .await
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
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
    // a channel send while still holding the PaneIoState mutex.
    let mut to_send: Vec<(mpsc::Sender<PtySize>, PtySize)> = Vec::new();

    if win.layout.is_zoomed() {
        // Zoomed: every pane in the tree reports the full content area
        // size so apps in the zoomed pane lay out correctly, while
        // others stay at the same nominal size (cheap, harmless).
        let state = io_state.lock().await;
        for pid in win.layout.tree.pane_ids() {
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
        let _ = tx.send(size).await;
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
async fn run_attach_loop(
    framed: Framed<UnixStream, tokio_util::codec::LengthDelimitedCodec>,
    graph: GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    config: ConfigHandle,
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
    let renderer = tokio::spawn(async move {
        run_render_loop(
            render_graph,
            render_io,
            render_config,
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
                    AttachClientFrame::Action { kind, .. } => {
                        if let Err(e) = handle_action(
                            kind,
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
                        detached = true;
                        let _ = out_tx.send(AttachServerFrame::DetachAck).await;
                    }
                    AttachClientFrame::Pong => {}
                }
            }
        }
        // Detect: did the active session vanish?
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
                    *rs = session.clone();
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
async fn run_render_loop(
    graph: GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    config: ConfigHandle,
    session: Arc<Mutex<AttachedSession>>,
    client_size: ClientSize,
    out_tx: mpsc::Sender<AttachServerFrame>,
    cancel: CancellationToken,
) {
    let (mut cols, mut rows) = *client_size.lock().await;
    let initial = config.current();
    let cfg = CompositorConfig {
        show_border: false,
        status_bar_height: STATUS_BAR_ROWS,
        border_style: BorderStyle::parse(&initial.appearance.border_style),
        ..Default::default()
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

    // The config-change notify gives us a fast path for hot-reloads:
    // when the user saves a new ~/.config/shux/config.toml, the watcher
    // task fires this Notify and we redraw immediately with the new
    // appearance / status bar settings.
    let cfg_notify = config.change_notify();
    let mut cfg_listener = Box::pin(cfg_notify.notified());
    let mut last_border_style = initial.appearance.border_style.clone();

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

        // Build a multi-pane frame snapshot.
        let snap = graph.snapshot();
        let attached = session.lock().await.clone();
        let win = match snap.windows.get(&attached.active_window_id) {
            Some(w) => w,
            None => continue,
        };

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
        // Status bar text.
        let bar = build_status_bar(&snap, &attached);

        let frame = MultiPaneFrame {
            layout: &win.layout.tree,
            zoom: win.layout.zoom.as_ref(),
            focused: attached.active_pane_id,
            vts: &vt_refs,
            status_bar: Some(&bar),
        };
        // Reset the buffer first so we only ship the new frame's bytes.
        compositor.inner_mut().clear();
        let _ = compositor.render_multi_pane(frame);

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

/// Build the hardcoded status bar for the current session.
fn build_status_bar(
    snap: &shux_core::graph::SessionGraphSnapshot,
    attached: &AttachedSession,
) -> StatusBar {
    use crossterm::style::Color;
    let mut bar = StatusBar::new();
    bar.bg = Some(Color::Rgb {
        r: 30,
        g: 32,
        b: 48,
    });

    bar.left.push(StatusSegment::styled(
        format!(" ◆ {} ", attached.name),
        Color::Rgb {
            r: 116,
            g: 199,
            b: 236,
        },
        true,
    ));

    if let Some(sess) = snap.sessions.get(&attached.session_id) {
        let win_count = sess.windows.len();
        let active_idx = sess
            .windows
            .iter()
            .position(|w| *w == sess.active_window)
            .unwrap_or(0);
        if let Some(win) = snap.windows.get(&sess.active_window) {
            let title = if win.title.is_empty() {
                "shell".to_string()
            } else {
                win.title.clone()
            };
            bar.center.push(StatusSegment::plain(format!(
                " [{}/{}] {} ",
                active_idx + 1,
                win_count,
                title
            )));
        }
    }

    let now = chrono::Local::now();
    bar.right.push(StatusSegment::plain(format!(
        " {} ",
        now.format("%H:%M:%S")
    )));
    bar
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
                        .resize_pane(state.target, state.direction, delta_ratio)
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
    )
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
    graph: &GraphHandle,
    io_state: &Arc<Mutex<PaneIoState>>,
    session: &Arc<Mutex<AttachedSession>>,
    client_size: &ClientSize,
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    use shux_core::layout::Direction;
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
        ActionKind::ResizeLeft => resize_pane(graph, &attached, Direction::Vertical, -0.05).await,
        ActionKind::ResizeRight => resize_pane(graph, &attached, Direction::Vertical, 0.05).await,
        ActionKind::ResizeUp => resize_pane(graph, &attached, Direction::Horizontal, -0.05).await,
        ActionKind::ResizeDown => resize_pane(graph, &attached, Direction::Horizontal, 0.05).await,
        ActionKind::Redraw => Ok(()),
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
        io_state.clone(),
        cancel.clone(),
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
        .zoom_pane(attached.active_pane_id)
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
    match graph.destroy_pane(pane_id).await {
        Ok(()) => {}
        Err(e) => {
            // Don't silently swallow LastPane / not-found / version
            // errors — the user wanted to kill a pane and it didn't
            // happen. Surface as a tracing warn (a future Notice frame
            // will surface it to the UI).
            warn!(error = %e, "kill_pane: destroy_pane failed");
            return Ok(());
        }
    }
    // Tear down the PTY task: dropping the writer Sender closes the mpsc,
    // which unblocks the PTY task's write_rx.recv() with None, but our
    // task's main exit is via PTY EOF / cancel. To make kill prompt and
    // free the child shell, drop the writer + resizer + vt right away.
    // The PTY task will get EOF on the next read (since the slave side
    // is dropped when PtyHandle drops) and exit. We rely on PtyHandle's
    // tokio::process::Child to reap.
    {
        let mut state = io_state.lock().await;
        state.writers.remove(&pane_id);
        state.resizers.remove(&pane_id);
        state.vts.remove(&pane_id);
        state.render_pulse.notify_one();
    }
    Ok(())
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
    crate::spawn_pane_pty(pane_id, cwd, Vec::new(), io_state.clone(), cancel.clone())
        .await
        .ok();

    // Focus the new window.
    let _ = graph.focus_window(window_id).await;

    let mut s = session.lock().await;
    s.active_window_id = window_id;
    s.active_pane_id = pane_id;
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
    let _ = graph.focus_window(target).await;

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
        .resize_pane(attached.active_pane_id, direction, delta)
        .await;
    Ok(())
}
