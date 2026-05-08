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

use shux_core::graph::GraphHandle;
use shux_core::layout::NavDirection;
use shux_core::model::{PaneId, SessionId, WindowId};
use shux_pty::handle::PtySize;
use shux_rpc::attach::{
    ATTACH_PROTOCOL_VERSION, ActionKind, AttachClientFrame, AttachHello, AttachReady,
    AttachServerFrame,
};
use shux_rpc::create_codec;
use shux_ui::{
    BorderStyle, CompositorConfig, MultiPaneFrame, RenderCompositor, StatusBar, StatusSegment,
};

use crate::PaneIoState;

/// Run the attach UDS listener. Each accepted connection spawns an
/// independent attach session task. Runs until `cancel` fires.
pub async fn run_attach_server(
    socket_path: std::path::PathBuf,
    graph: GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
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
                        let c = cancel.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_attach_connection(stream, g, io, c).await {
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
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let mut framed = Framed::new(stream, create_codec());

    // Step 1: Receive the hello frame.
    let first = match framed.next().await {
        Some(Ok(buf)) => buf,
        Some(Err(e)) => {
            warn!(error = %e, "attach: bad first frame");
            return Ok(());
        }
        None => {
            debug!("attach: client disconnected before hello");
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
        let state = io_state.lock().await;
        if !state.writers.contains_key(&session.active_pane_id) {
            drop(state);
            crate::spawn_pane_pty(
                session.active_pane_id,
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/tmp")),
                io_state.clone(),
                cancel.clone(),
            )
            .await
            .ok();
        }
    }
    // Always resize all panes in the active window to match the client.
    // This handles both freshly-spawned panes (which start at 80x24) and
    // pre-existing panes from `shux new --detached` runs that used a
    // different terminal size.
    {
        let snap = graph.snapshot();
        if let Some(win) = snap.windows.get(&session.active_window_id) {
            let state = io_state.lock().await;
            for pid in win.layout.tree.pane_ids() {
                if let Some(tx) = state.resizers.get(&pid) {
                    let _ = tx
                        .send(PtySize::new(hello.cols, hello.rows.saturating_sub(1)))
                        .await;
                }
            }
        }
    }

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
    run_attach_loop(framed, graph, io_state, session, hello, cancel).await
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

    // Spawn the renderer task.
    let render_cancel = cancel.child_token();
    let render_io = io_state.clone();
    let render_graph = graph.clone();
    let render_tx = out_tx.clone();
    let initial_size = (hello.cols, hello.rows);
    let render_session = Arc::new(Mutex::new(session.clone()));
    let render_session_for_task = render_session.clone();
    let renderer = tokio::spawn(async move {
        run_render_loop(
            render_graph,
            render_io,
            render_session_for_task,
            initial_size,
            render_tx,
            render_cancel,
        )
        .await;
    });

    // Read client frames.
    let mut detached = false;
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
                        let state = io_state.lock().await;
                        if let Some(tx) = state.writers.get(&target) {
                            let _ = tx.send(bytes).await;
                        }
                    }
                    AttachClientFrame::Resize { cols, rows } => {
                        // Notify all live panes of new size. We use one less
                        // row than the host's height so the status bar fits.
                        let pty_rows = rows.saturating_sub(1).max(1);
                        let state = io_state.lock().await;
                        for tx in state.resizers.values() {
                            let _ = tx.send(PtySize::new(cols, pty_rows)).await;
                        }
                        state.render_pulse.notify_waiters();
                    }
                    AttachClientFrame::Action { kind, .. } => {
                        if let Err(e) =
                            handle_action(kind, &graph, &io_state, &render_session, &cancel).await
                        {
                            warn!(?kind, error = %e, "attach: action failed");
                        }
                        // Wake renderer immediately so the user sees the
                        // result of their action without delay.
                        let st = io_state.lock().await;
                        st.render_pulse.notify_waiters();
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
    session: Arc<Mutex<AttachedSession>>,
    initial_size: (u16, u16),
    out_tx: mpsc::Sender<AttachServerFrame>,
    cancel: CancellationToken,
) {
    let (mut cols, mut rows) = initial_size;
    let cfg = CompositorConfig {
        show_border: false,
        status_bar_height: 1,
        border_style: BorderStyle::Rounded,
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

    let pulse = io_state.lock().await.render_pulse.clone();
    let mut pulse_listener = Box::pin(pulse.notified());

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = &mut pulse_listener => {
                pulse_listener = Box::pin(pulse.notified());
            }
            _ = tick.tick() => {}
        }

        // Re-read terminal size: client could have resized between cycles.
        let want_size = current_size_for_session(&graph, &session, &io_state).await;
        if let Some((c, r)) = want_size {
            if (c, r) != (cols, rows) {
                cols = c;
                rows = r;
                compositor.resize(cols, rows);
            }
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

/// Best-effort current size — the client's most recent resize lives in
/// the resizer channels, but we don't have direct access to it. As a
/// proxy, look at any pane's PTY size by inspecting the VT grid; if the
/// VT was resized via a recent Resize frame, its grid will reflect the
/// new dimensions.
async fn current_size_for_session(
    graph: &GraphHandle,
    session: &Arc<Mutex<AttachedSession>>,
    io_state: &Arc<Mutex<PaneIoState>>,
) -> Option<(u16, u16)> {
    let snap = graph.snapshot();
    let attached = session.lock().await.clone();
    let win = snap.windows.get(&attached.active_window_id)?;
    let pane_id = win.layout.tree.pane_ids().into_iter().next()?;
    let state = io_state.lock().await;
    let vt = state.vts.get(&pane_id)?;
    let grid = vt.grid();
    Some((grid.cols() as u16, grid.rows() as u16 + 1)) // +1 for status bar
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

/// Dispatch an Action keybinding from the client.
async fn handle_action(
    kind: ActionKind,
    graph: &GraphHandle,
    io_state: &Arc<Mutex<PaneIoState>>,
    session: &Arc<Mutex<AttachedSession>>,
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    use shux_core::layout::Direction;
    let attached = session.lock().await.clone();

    match kind {
        ActionKind::SplitSmart => split(graph, &attached, None, io_state, cancel).await,
        ActionKind::SplitVertical => {
            split(
                graph,
                &attached,
                Some(Direction::Vertical),
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
                io_state,
                cancel,
            )
            .await
        }
        ActionKind::FocusUp => focus_dir(graph, &attached, NavDirection::Up, session).await,
        ActionKind::FocusDown => focus_dir(graph, &attached, NavDirection::Down, session).await,
        ActionKind::FocusLeft => focus_dir(graph, &attached, NavDirection::Left, session).await,
        ActionKind::FocusRight => focus_dir(graph, &attached, NavDirection::Right, session).await,
        ActionKind::FocusNext => focus_relative(graph, &attached, 1, session).await,
        ActionKind::FocusPrev => focus_relative(graph, &attached, -1, session).await,
        ActionKind::ToggleZoom => zoom(graph, &attached).await,
        ActionKind::KillPane => kill_pane(graph, &attached).await,
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
    io_state: &Arc<Mutex<PaneIoState>>,
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    use shux_core::layout::{Direction, Rect};

    // Smart split: pick direction based on the focused pane's current
    // dimensions. Wider → vertical, taller → horizontal. We compute this
    // from a fresh snapshot.
    let direction = match dir {
        Some(d) => d,
        None => {
            let snap = graph.snapshot();
            let win = snap
                .windows
                .get(&attached.active_window_id)
                .ok_or_else(|| anyhow::anyhow!("active window missing"))?;
            // Use a reasonable default viewport — exact rect computation
            // happens on the client. 120x40 keeps the heuristic stable.
            let rects = win.layout.compute_rects(Rect::new(0, 0, 120, 40));
            let pane_rect = rects
                .iter()
                .find(|(p, _)| *p == attached.active_pane_id)
                .map(|(_, r)| *r)
                .unwrap_or(Rect::new(0, 0, 120, 40));
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
    session: &Arc<Mutex<AttachedSession>>,
) -> anyhow::Result<()> {
    use shux_core::layout::Rect;
    let new_id = graph
        .focus_pane_direction(attached.active_window_id, nav, Rect::new(0, 0, 120, 40))
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

async fn kill_pane(graph: &GraphHandle, attached: &AttachedSession) -> anyhow::Result<()> {
    let _ = graph.destroy_pane(attached.active_pane_id).await;
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
    crate::spawn_pane_pty(pane_id, cwd, io_state.clone(), cancel.clone())
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
