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

use crate::pane_io::PaneIoState;
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

/// Bounds on the terminal size a client is allowed to declare.
///
/// The client picks these numbers and the daemon allocates against them, so
/// they are untrusted input: `cols` and `rows` are `u16`, and a 93-byte attach
/// handshake declaring 65535x65535 asks for ~4.3e9 cells — enough to OOM-kill
/// the daemon and every session with it. Measured before this ceiling existed:
/// 8000x8000 peaked at 8.9 GB, 65535x65535 killed the daemon in 2.5 s.
///
/// The ceiling matches the range `pane.set_size` has always validated
/// (`4..=1000` cols, `2..=1000` rows), so the two paths that can size a pane
/// now agree at BOTH ends. It also keeps every pane inside the pixel budget
/// `pane.snapshot` assumes — above it, snapshots failed permanently.
///
/// Clamped once, where the size enters, so the layout arithmetic, the PTY
/// winsize, the VT allocation and the render buffer all inherit the bound.
const MIN_PANE_ROWS: u16 = 2;
const MIN_PANE_COLS: u16 = 2;
const MAX_CLIENT_ROWS: u16 = 1000;
const MAX_CLIENT_COLS: u16 = 1000;

/// Clamp a client-declared terminal size into the supported range.
fn clamp_client_dims(cols: u16, rows: u16) -> (u16, u16) {
    (
        cols.clamp(MIN_PANE_COLS, MAX_CLIENT_COLS),
        rows.clamp(MIN_PANE_ROWS, MAX_CLIENT_ROWS),
    )
}

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

/// Has this pane's PTY simply not started yet, as opposed to started and
/// finished?
///
/// Attach spawns a PTY for the active pane when none is registered, to close a
/// race: a freshly created session can be attached to before its spawn task
/// has run. A pane whose command **exited** looks identical from the writer
/// table, and used to be swept up by the same branch — so a pane that ran
/// `make` and finished came back, on attach, as a fresh login shell in the
/// daemon's working directory. The default `RestartPolicy` is `Never` and
/// attaching is not a restart request (issue #125 follow-up).
///
/// `exit_status` is the discriminator: `None` until the child is reaped, `Some`
/// forever after.
fn pane_awaits_first_spawn(pane: &shux_core::model::Pane) -> bool {
    pane.exit_status.is_none()
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
    //
    // Two things this must NOT do, both of which it used to (issue #125
    // follow-up). It must not resurrect a pane that genuinely **exited** —
    // the default `RestartPolicy` is `Never`, and attaching is not a restart
    // request; a pane whose command ran and finished came back as a login
    // shell, silently replacing the program the operator asked for. And when
    // it does spawn, it must use the pane's OWN command and cwd rather than
    // an empty argv and the daemon's working directory, or the race it exists
    // to close is closed with the wrong process in the wrong place.
    {
        let writer_present = {
            let state = io_state.lock().await;
            state.writers.contains_key(&session.active_pane_id)
        };
        if !writer_present {
            let snap = graph.snapshot();
            if let Some(pane) = snap.panes.get(&session.active_pane_id)
                && pane_awaits_first_spawn(pane)
            {
                // Deliberately NOT rolled back, unlike the split and
                // new-window paths: this pane already existed before the
                // attach — destroying it because its PTY would not start
                // would throw away a pane the user is trying to get back to.
                // But it is not swallowed either. A failure here is why the
                // pane renders empty forever, and with `[shell].command`
                // (issue #132) the usual cause is a config typo that says so.
                if let Err(e) = crate::pane_spawn::spawn_pane_pty(
                    session.active_pane_id,
                    pane.cwd.clone(),
                    pane.command.clone(),
                    shux_pty::handle::PtySize::default(),
                    Vec::new(),
                    false,
                    io_state.clone(),
                    cancel.clone(),
                    graph.clone(),
                )
                .await
                {
                    warn!(
                        pane = %session.active_pane_id,
                        error = %crate::pane_spawn::spawn_failure_message(&e),
                        "attach: pane has no PTY and could not be started"
                    );
                }
            }
        }
    }
    // Resize every pane in the active window to its real layout rect, not
    // the full client size. Multi-pane TUIs (vim, htop, less) read TIOCGWINSZ
    // and will lay themselves out wrong if every pane PTY pretends to be
    // the whole screen.
    apply_resize_to_window(&graph, &io_state, &session, &config, hello.cols, hello.rows).await;

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

    let attached = |sess: &shux_core::model::Session| -> anyhow::Result<AttachedSession> {
        let win = snap
            .windows
            .get(&sess.active_window)
            .ok_or_else(|| anyhow::anyhow!("active window missing from snapshot"))?;
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
            // Whether the toast actually renders is decided at attach
            // time by reading the onboarding state file; this stays
            // true here so the render loop can flip it off after dwell.
            show_welcome_toast: true,
        })
    };

    // An exact NAME first — unchanged, and it still beats a partial id.
    if let Some(sess) = snap.find_session_by_name(&target_name) {
        return attached(sess);
    }

    // Not a name. Before creating anything, try it as an ID — the short form
    // every listing prints (issue #120). This branch matters more here than
    // anywhere else: `attach` CREATES on a miss, so an unresolved id does not
    // produce an error, it produces a blank session named after the id while
    // the session the user meant sits untouched.
    match snap.resolve_session_ref(&target_name) {
        Ok(id) => {
            if let Some(sess) = snap.sessions.get(&id) {
                return attached(sess);
            }
            // A syntactically valid uuid that names nothing. Creating a session
            // whose NAME is a uuid is almost certainly not what was meant.
            anyhow::bail!("session '{}' not found", target_name.escape_debug());
        }
        Err(e @ shux_core::idref::RefError::Ambiguous { .. }) => {
            anyhow::bail!("{e}");
        }
        // Malformed or unmatched prefix: it is just a name. Fall through and
        // create it, which is what `attach` has always done.
        Err(_) => {}
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
/// real winsize. Multi-pane TUIs read `TIOCGWINSZ` and lay themselves out from
/// it, so a pane whose PTY thinks it is a different size than the rect it is
/// drawn in renders into the wrong shape.
///
/// Goes through [`shux_ui::pane_viewport`], the compositor's own rule. This
/// used to inset for the outline unconditionally, so under
/// `appearance.border_style = "none"` every pane's VT grid came out two columns
/// and two rows smaller than the rect the compositor drew it into: the last
/// columns of each pane held nothing, and a mouse click on them named a cell
/// the app did not have.
async fn apply_resize_to_window(
    graph: &GraphHandle,
    io_state: &Arc<Mutex<PaneIoState>>,
    session: &AttachedSession,
    config: &ConfigHandle,
    cols: u16,
    rows: u16,
) {
    let (cols, rows) = clamp_client_dims(cols, rows);
    let snap = graph.snapshot();
    let win = match snap.windows.get(&session.active_window_id) {
        Some(w) => w,
        None => return,
    };
    let content_h = rows.saturating_sub(STATUS_BAR_ROWS);
    let content = Rect::new(0, 0, cols, content_h);
    let viewport = shux_ui::pane_viewport(
        content,
        BorderStyle::parse(&config.current().appearance.border_style),
        false,
    );

    // Drain the resizer senders out from under the lock so we never await
    // a channel send while still holding the PaneIoState mutex. Attach
    // fan-out is fire-and-forget (ack=None); the synchronous path is
    // `pane.set_size` RPC which constructs its own oneshot.
    let mut to_send: Vec<(mpsc::Sender<crate::pane_io::ResizeRequest>, PtySize)> = Vec::new();

    if win.layout.is_zoomed() {
        // Zoomed: every pane in the tree reports the full content area
        // size so apps in the zoomed pane lay out correctly, while
        // others stay at the same nominal size (cheap, harmless).
        //
        // The floor matters as much here as in the tiled branch below: the
        // client picks `rows`, and a zoomed window subtracts the status bar
        // from it with no layout arithmetic in between, so `rows <= 1` used to
        // reach the PTY and the VT as a pane of height 0 (issue #107).
        let state = io_state.lock().await;
        let pane_ids = win
            .layout
            .zoom
            .as_ref()
            .map(|zoom| zoom.saved_layout.pane_ids())
            .unwrap_or_else(|| win.layout.tree.pane_ids());
        for pid in pane_ids {
            if let Some(tx) = state.resizers.get(&pid) {
                to_send.push((
                    tx.clone(),
                    PtySize::new(
                        content.width.max(MIN_PANE_COLS),
                        content.height.max(MIN_PANE_ROWS),
                    ),
                ));
            }
        }
    } else {
        let rects = win.layout.tree.compute_rects(viewport);
        let state = io_state.lock().await;
        for (pid, rect) in rects {
            if let Some(tx) = state.resizers.get(&pid) {
                let r_cols = rect.width.max(MIN_PANE_COLS);
                let r_rows = rect.height.max(MIN_PANE_ROWS);
                to_send.push((tx.clone(), PtySize::new(r_cols, r_rows)));
            }
        }
    }

    for (tx, size) in to_send {
        let _ = tx
            .send(crate::pane_io::ResizeRequest { size, ack: None })
            .await;
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
    let client_size: ClientSize = Arc::new(Mutex::new(clamp_client_dims(hello.cols, hello.rows)));

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
                                    &config,
                                )
                                .await;
                                let action = {
                                    let state = io_state.lock().await;
                                    let vt = state.vts.get(&active_pane);
                                    // #108: anchor copy-mode scrolling to the region the
                                    // attach frame shows for an oversized pane (identical to
                                    // grid.total_lines() when the grid fits its rect).
                                    let total_lines = vt
                                        .map(|vt| shux_ui::copy_mode::effective_total_lines(vt, rows))
                                        .unwrap_or(rows as usize);
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
                        let (cols, rows) = clamp_client_dims(cols, rows);
                        {
                            let mut cs = client_size.lock().await;
                            *cs = (cols, rows);
                        }
                        let attached = render_session.lock().await.clone();
                        apply_resize_to_window(&graph, &io_state, &attached, &config, cols, rows).await;
                        let pulse = io_state.lock().await.render_pulse.clone();
                        pulse.notify_one();
                    }
                    AttachClientFrame::Action { kind, args } => {
                        // The user pressed prefix + key — onboarding hint
                        // can dismiss. Cheap idempotent write; first call
                        // persists, subsequent ones short-circuit.
                        onboarding.mark_prefix_discovered().await;

                        let action_result = handle_action(
                            kind,
                            args.clone(),
                            &graph,
                            &io_state,
                            &render_session,
                            &client_size,
                            &config,
                            &cancel,
                        )
                        .await;
                        if let Err(e) = &action_result {
                            warn!(?kind, error = %e, "attach: action failed");
                        }

                        // Transient command-feedback overlay in the status
                        // bar's center zone. Resolves the "did my keystroke
                        // do anything?" UX gap for actions whose effect
                        // isn't immediately obvious (kill, zoom, copy).
                        //
                        // Gated on success: an action that rolled back must
                        // not flash its own name as if it had happened. That
                        // is the "did my keystroke do anything?" question
                        // answered wrongly, which is worse than not answering.
                        if action_result.is_ok()
                            && let Some(label) = action_feedback_label(kind)
                        {
                            let mut s = render_session.lock().await;
                            s.last_action = Some((label.into(), std::time::Instant::now()));
                        }
                        // Layout-changing actions invalidate per-pane PTY
                        // sizes. Re-fan the winsizes so vim/htop/etc. inside
                        // each pane learn their new dimensions.
                        if action_changes_layout(kind) {
                            let attached = render_session.lock().await.clone();
                            let (cols, rows) = *client_size.lock().await;
                            apply_resize_to_window(&graph, &io_state, &attached, &config, cols, rows).await;
                        }
                        let pulse = io_state.lock().await.render_pulse.clone();
                        pulse.notify_one();
                    }
                    AttachClientFrame::Mouse {
                        kind,
                        button,
                        col,
                        row,
                        shift,
                        alt,
                        ctrl,
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
                            // An app mid-drag behind the overlay has to be
                            // told the button came up, or it stays stuck
                            // dragging for the rest of its life.
                            release_app_gesture(&io_state, &mut selection_drag).await;
                            selection_drag = SelectionDrag::None;
                            continue;
                        }
                        // Inside a pane whose app asked for the mouse, the
                        // mouse is the app's — ahead of shux's own selection,
                        // behind shux's modes. `handle_wheel` still owns every
                        // scroll tick.
                        match handle_app_mouse(
                            kind,
                            button,
                            col,
                            row,
                            shift,
                            alt,
                            ctrl,
                            &graph,
                            &io_state,
                            &render_session,
                            &client_size,
                            &config,
                            &mouse_drag,
                            &mut selection_drag,
                        )
                        .await?
                        {
                            AppMouse::Consumed { redraw } => {
                                if redraw {
                                    let pulse = io_state.lock().await.render_pulse.clone();
                                    pulse.notify_one();
                                }
                                continue;
                            }
                            AppMouse::NotHandled => {}
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
                            &config,
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
                            &config,
                            &out_tx,
                            &mut selection_drag,
                        )
                        .await?
                        {
                            let pulse = io_state.lock().await.render_pulse.clone();
                            pulse.notify_one();
                            continue;
                        }
                        // Scroll wheel (copy mode inactive): scroll scrollback,
                        // or forward to a mouse-aware / alt-screen app.
                        if handle_wheel(
                            kind,
                            col,
                            row,
                            &graph,
                            &io_state,
                            &render_session,
                            &client_size,
                            &config,
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
                            &config,
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
            if live && let Some(s) = snap.sessions.get(&session.session_id) {
                session.active_window_id = s.active_window;
                if let Some(w) = snap.windows.get(&s.active_window) {
                    session.active_pane_id = w.active_pane;
                }
                let mut rs = render_session.lock().await;
                rs.active_window_id = session.active_window_id;
                rs.active_pane_id = session.active_pane_id;
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

    // A gesture forwarded to a pane app outlives the attach: the PTY does not
    // go away when the client does. Detaching mid-drag without this leaves the
    // app believing a button is still held, and it stays that way for the rest
    // of the pane's life — visible next time anyone attaches.
    release_app_gesture(&io_state, &mut selection_drag).await;

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
            let viewport = current_viewport(&client_size, &config).await;
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
            pane_rect_for(&graph, &attached, &client_size, &config, selection.pane_id)
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
            if let Some(p) = snap.panes.get(&pid)
                && !p.title.is_empty()
            {
                pane_titles.insert(pid, p.title.clone());
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

/// Who owns the in-flight mouse gesture.
///
/// Decided once, at button-down, and honoured until the last button comes up.
/// Two independent latches -- one for shux's selection, one for the pane app --
/// could both be live at once, and then a mode change mid-drag split the
/// gesture between them: shux kept a selection highlight that no mouse action
/// could clear, and the app got a release with no matching press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionDrag {
    None,
    CopyMode,
    MouseSelection {
        pane_id: PaneId,
    },
    /// Forwarded to a mouse-aware app in `pane_id`.
    ///
    /// `buttons` is a bitmask of the buttons currently held, not a count: on a
    /// host without SGR mouse reporting every release decodes as
    /// `Up(MouseButton::Left)`, so a latch keyed to one button would never
    /// clear. `last` is the pane-local cell of the most recent forwarded
    /// report, used to synthesize a release if the gesture is abandoned.
    App {
        pane_id: PaneId,
        buttons: u8,
        last: (u16, u16),
    },
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

/// Resolve the pane under the pointer for wheel routing: the pane at
/// `(col, row)`, falling back to the active pane when the window is zoomed
/// (only one pane is visible) or the point lands on no pane (e.g. a border
/// cell). Mirrors the cursor hit-test `handle_wheel` performs, so both the
/// copy-mode and non-copy-mode wheel paths agree on which pane a scroll hits.
fn pane_under_pointer(
    graph: &GraphHandle,
    attached: &AttachedSession,
    viewport: Rect,
    col: u16,
    row: u16,
) -> PaneId {
    let snap = graph.snapshot();
    match snap.windows.get(&attached.active_window_id) {
        Some(win) if !win.layout.is_zoomed() => pane_at(&win.layout.tree, viewport, col, row)
            .map(|(pid, _)| pid)
            .unwrap_or(attached.active_pane_id),
        _ => attached.active_pane_id,
    }
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
    config: &ConfigHandle,
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
                            pane_rect_for(graph, &attached, client_size, config, selection.pane_id)
                                .await
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
            let viewport = current_viewport(client_size, config).await;
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
            let Some(rect) = pane_rect_for(graph, &attached, client_size, config, pane_id).await
            else {
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
            let Some(rect) = pane_rect_for(graph, &attached, client_size, config, pane_id).await
            else {
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
            let Some(rect) =
                pane_rect_for(graph, &attached, client_size, config, selection.pane_id).await
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
    config: &ConfigHandle,
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

    let Some(rect) = focused_pane_rect(graph, &attached, client_size, config).await else {
        *dragging = SelectionDrag::None;
        return Ok(true);
    };

    match kind {
        MouseKind::ScrollUp | MouseKind::ScrollDown => {
            // Wheel scroll targets the pane under the pointer. A *transient*,
            // wheel-opened scrollback must release the wheel when the pointer
            // moves to a different pane, so `handle_wheel` can route the scroll
            // to the pane the user is actually pointing at (scroll its
            // scrollback, or forward to its app) instead of the pane that
            // happened to open scrollback first. A deliberately-entered copy
            // mode keeps the wheel so a stray scroll over another pane never
            // discards an in-progress selection/search (copy mode is
            // session-global — only one is active at a time).
            let viewport = current_viewport(client_size, config).await;
            let cursor_pane = pane_under_pointer(graph, &attached, viewport, col, row);
            let transient = attached
                .copy_mode
                .as_ref()
                .is_some_and(|cm| cm.wheel_initiated);
            if transient && cursor_pane != attached.active_pane_id {
                session.lock().await.copy_mode = None;
                *dragging = SelectionDrag::None;
                return Ok(false);
            }
            let total_lines = {
                let state = io_state.lock().await;
                state
                    .vts
                    .get(&attached.active_pane_id)
                    .map(|vt| vt.presented_total_lines())
                    .unwrap_or(rect.height as usize)
            };
            let mut s = session.lock().await;
            // A wheel-initiated scrollback view hands the keyboard back the
            // moment the wheel brings it to the live bottom; a manually-entered
            // copy mode (`Prefix [` / API) stays so an in-progress selection or
            // search is never lost. `exit` is computed while `cm` is borrowed,
            // then applied after the borrow ends.
            let exit = if let Some(ref mut cm) = s.copy_mode {
                if matches!(kind, MouseKind::ScrollUp) {
                    shux_ui::copy_mode::scroll_up(cm, 3, total_lines, rect.height);
                    false
                } else {
                    shux_ui::copy_mode::scroll_down(cm, 3, total_lines, rect.height);
                    cm.wheel_initiated && cm.scroll_offset == 0
                }
            } else {
                false
            };
            if exit {
                s.copy_mode = None;
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
    config: &ConfigHandle,
    drag: &mut Option<DragState>,
) -> anyhow::Result<()> {
    let attached = session.lock().await.clone();
    let viewport = current_viewport(client_size, config).await;
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
        // Scroll wheel is consumed earlier by `handle_wheel`; it never
        // reaches the layout/focus handler.
        _ => {}
    }
    Ok(())
}

/// A button event, as an app's mouse report understands it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ButtonAction {
    Press,
    Release,
    /// Motion with a button held.
    Drag,
}

/// Which xterm modes report `action`.
///
/// A real terminal reports only what the app asked for: in mode 1000 it sends
/// press and release and nothing else, so forwarding a drag there tells the app
/// about an event it has no handler for. `MouseMode::None` reports nothing —
/// that pane keeps shux's own mouse handling.
///
/// Motion with no button (mode 1003's extra) is absent because shux never
/// receives it: the host mouse profile deliberately enables 1000+1002 and not
/// 1003 (`shux_ui::terminal`, pinned by a test there), so crossterm produces no
/// `Moved` events to forward. An `AnyEvent` pane therefore gets press, release
/// and drag but never hover. Making that work means enabling 1003 on the host,
/// which costs the host terminal's own selection — a separate trade, not this
/// change.
fn mode_reports(mode: shux_vt::MouseMode, action: ButtonAction) -> bool {
    use shux_vt::MouseMode;
    match mode {
        MouseMode::None => false,
        MouseMode::Normal => action != ButtonAction::Drag,
        MouseMode::ButtonEvent | MouseMode::AnyEvent => true,
    }
}

/// Whether shux can encode a coordinate the app in this pane will read back as
/// the cell the user actually clicked.
///
/// shux emits X10 or SGR (1006). An app that also asked for 1005, 1015 or 1016
/// decodes those bytes as something else — 1016 in particular reads cells as
/// pixels and collapses every click into the pane's top-left corner. Forwarding
/// under any of them is not "mostly working"; it is clicking the wrong thing.
/// Standing down leaves the pane exactly where it was before this feature
/// existed, which is the honest floor.
fn coords_are_encodable(modes: &shux_vt::TerminalModes) -> bool {
    !modes.utf8_mouse && !modes.urxvt_mouse && !modes.pixel_mouse
}

/// Where a button event goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppMouseRoute {
    /// shux keeps it: selection, copy mode, border drag, or a pane whose app
    /// never asked for the mouse.
    Shux,
    /// Forward an encoded report to this pane.
    Forward(PaneId),
    /// The app owns the mouse but does not report this event. Consume it
    /// silently rather than letting shux start a selection the user did not
    /// ask for underneath a running app.
    Swallow,
}

/// Route a button event, given facts the caller has already resolved.
///
/// Pure so the precedence can be tested exhaustively instead of inferred from
/// the order of early returns in the handler — the same reason `route_wheel`
/// exists next to `handle_wheel`.
///
/// Order matters and is the tmux/wezterm model with one addition at the top:
///
/// 1. a gesture already in flight keeps its owner, whatever the pointer is over
///    now and whatever the app's mode has become since
/// 2. copy mode / the copy menu / an existing selection are shux's modes
/// 3. a border drag in flight is shux's
/// 4. no pane under the pointer (border cell, outline, status bar) is shux's
/// 5. a pane whose app never asked for the mouse is shux's
/// 6. an app that cannot decode what shux would send keeps nothing — shux does
/// 7. everything else belongs to the app
#[allow(clippy::too_many_arguments)]
fn route_app_mouse(
    gesture: SelectionDrag,
    border_drag_active: bool,
    copy_active: bool,
    shift: bool,
    pane_hit: Option<PaneId>,
    mode: shux_vt::MouseMode,
    encodable: bool,
    action: ButtonAction,
) -> AppMouseRoute {
    // 1. An in-flight gesture is not re-decided. This is what makes a press and
    //    its release reach the same place even if the app toggles its mouse
    //    mode, the user presses or releases Shift, or the pointer leaves the
    //    pane mid-drag.
    if let SelectionDrag::App { pane_id, .. } = gesture {
        return if mode_reports(mode, action) && encodable {
            AppMouseRoute::Forward(pane_id)
        } else {
            AppMouseRoute::Swallow
        };
    }
    if gesture != SelectionDrag::None || border_drag_active || copy_active {
        return AppMouseRoute::Shux;
    }
    // Only a press opens a gesture. A drag or release with no gesture in flight
    // belongs to whatever shux started, or to nothing.
    if action != ButtonAction::Press {
        return AppMouseRoute::Shux;
    }
    // Shift is the user taking the mouse back. See `AttachClientFrame::Mouse`
    // for how rarely most host terminals let this through.
    if shift {
        return AppMouseRoute::Shux;
    }
    let Some(pane_id) = pane_hit else {
        return AppMouseRoute::Shux;
    };
    // `mode_reports` is false for every action under `MouseMode::None`, so a
    // pane whose app never asked for the mouse falls out here too.
    if !encodable || !mode_reports(mode, action) {
        return AppMouseRoute::Shux;
    }
    AppMouseRoute::Forward(pane_id)
}

/// The xterm button code for a button event, before encoding.
///
/// `None` is not a button. It reaches here on motion, which xterm reports as
/// `3` (no button) with the motion bit set; on a press or release it is
/// meaningless and must not be turned into a left click, which is what a
/// `_ => 0` arm would do.
fn button_cb(action: ButtonAction, button: ProtoMouseButton, alt: bool, ctrl: bool) -> Option<u16> {
    let base: u16 = match button {
        ProtoMouseButton::Left => 0,
        ProtoMouseButton::Middle => 1,
        ProtoMouseButton::Right => 2,
        ProtoMouseButton::None => match action {
            ButtonAction::Drag => 3,
            ButtonAction::Press | ButtonAction::Release => return None,
        },
    };
    let motion = if action == ButtonAction::Drag { 32 } else { 0 };
    // Shift is deliberately absent: it is reserved for shux, so an event
    // carrying it never reaches an app to begin with.
    let mods = if alt { 8 } else { 0 } | if ctrl { 16 } else { 0 };
    Some(base + motion + mods)
}

/// The bit `button` occupies in [`SelectionDrag::App`]'s held-button mask.
fn button_bit(button: ProtoMouseButton) -> u8 {
    match button {
        ProtoMouseButton::Left => 1,
        ProtoMouseButton::Middle => 2,
        ProtoMouseButton::Right => 4,
        ProtoMouseButton::None => 0,
    }
}

/// Tell the app every held button came up, then end the gesture.
///
/// Called wherever a forwarded gesture is abandoned rather than completed — the
/// help overlay opening, the client detaching, the session ending, the pane's
/// rect disappearing under a zoom or a window switch. Without it the app is
/// left believing a button is still down: in mode 1002 it reports nothing
/// further until the next click, and most TUIs sit visibly mid-drag.
async fn release_app_gesture(io_state: &Arc<Mutex<PaneIoState>>, gesture: &mut SelectionDrag) {
    let SelectionDrag::App {
        pane_id,
        buttons,
        last,
    } = *gesture
    else {
        return;
    };
    *gesture = SelectionDrag::None;
    let sgr = {
        let state = io_state.lock().await;
        state
            .vts
            .get(&pane_id)
            .is_some_and(|vt| vt.modes().sgr_mouse)
    };
    for (bit, button) in [
        (1u8, ProtoMouseButton::Left),
        (2, ProtoMouseButton::Middle),
        (4, ProtoMouseButton::Right),
    ] {
        if buttons & bit == 0 {
            continue;
        }
        let Some(cb) = button_cb(ButtonAction::Release, button, false, false) else {
            continue;
        };
        if let Some(bytes) = encode_mouse_report(cb, true, sgr, last.0, last.1) {
            forward_bytes_to_pane(io_state, pane_id, bytes).await;
        }
    }
}

/// What `handle_app_mouse` did with an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppMouse {
    /// Not ours — fall through to shux's own mouse handling.
    NotHandled,
    /// Consumed. `redraw` is true only when shux's own state changed, so a
    /// drag under mode 1002 (which reports continuously) does not force a full
    /// compositor frame per motion event.
    Consumed { redraw: bool },
}

/// Forward a press / drag / release to a mouse-aware app in the pane under the
/// pointer.
///
/// Sits ahead of shux's selection handling in the attach loop: inside a pane
/// whose app asked for the mouse, the mouse is the app's. `handle_wheel` stays
/// the sole authority on scroll ticks — this returns [`AppMouse::NotHandled`]
/// for them so the scrollback, alt-scroll and forward tiers keep working.
#[allow(clippy::too_many_arguments)]
async fn handle_app_mouse(
    kind: MouseKind,
    button: ProtoMouseButton,
    col: u16,
    row: u16,
    shift: bool,
    alt: bool,
    ctrl: bool,
    graph: &GraphHandle,
    io_state: &Arc<Mutex<PaneIoState>>,
    session: &Arc<Mutex<AttachedSession>>,
    client_size: &ClientSize,
    config: &ConfigHandle,
    border_drag: &Option<DragState>,
    gesture: &mut SelectionDrag,
) -> anyhow::Result<AppMouse> {
    let action = match kind {
        MouseKind::Down => ButtonAction::Press,
        MouseKind::Up => ButtonAction::Release,
        MouseKind::Drag => ButtonAction::Drag,
        // The wheel keeps its own routing; `Move` never arrives, because the
        // host profile does not enable any-motion tracking.
        MouseKind::ScrollUp | MouseKind::ScrollDown | MouseKind::Move => {
            return Ok(AppMouse::NotHandled);
        }
    };
    let attached = session.lock().await.clone();
    let copy_active = attached.copy_mode.is_some() || attached.copy_menu.is_some();
    let viewport = current_viewport(client_size, config).await;

    // Resolve the pane under the pointer. `pane_at`, deliberately not
    // `pane_under_pointer`: that helper falls back to the active pane whenever
    // the hit-test misses, which would forward clicks on borders, the outline
    // and the status bar into whatever pane happens to be focused.
    let (pane_hit, on_border) = {
        let snap = graph.snapshot();
        match snap.windows.get(&attached.active_window_id) {
            None => (None, false),
            Some(win) if win.layout.is_zoomed() => (Some(attached.active_pane_id), false),
            Some(win) => (
                pane_at(&win.layout.tree, viewport, col, row).map(|(pid, _)| pid),
                border_at(&win.layout.tree, viewport, col, row).is_some(),
            ),
        }
    };
    let pane_hit = if on_border { None } else { pane_hit };

    // Modes are read from the pane the gesture already owns when there is one,
    // so an app that turns tracking off mid-drag still gets its release.
    let mode_pane = match *gesture {
        SelectionDrag::App { pane_id, .. } => Some(pane_id),
        _ => pane_hit,
    };
    let (mode, sgr, encodable) = {
        let state = io_state.lock().await;
        match mode_pane.and_then(|pid| state.vts.get(&pid)) {
            Some(vt) => {
                let m = vt.modes();
                (m.mouse_tracking, m.sgr_mouse, coords_are_encodable(m))
            }
            None => (shux_vt::MouseMode::None, false, true),
        }
    };

    let route = route_app_mouse(
        *gesture,
        border_drag.is_some(),
        copy_active,
        shift,
        pane_hit,
        mode,
        encodable,
        action,
    );
    let pane_id = match route {
        AppMouseRoute::Shux => return Ok(AppMouse::NotHandled),
        // The app owns the mouse but did not subscribe to this event. Consume
        // it: a stray shux selection appearing under a running TUI is not a
        // better answer than nothing happening.
        AppMouseRoute::Swallow => {
            end_gesture_if_released(gesture, action, button);
            return Ok(AppMouse::Consumed { redraw: false });
        }
        AppMouseRoute::Forward(pane_id) => pane_id,
    };

    // The pane can lose its rect mid-gesture: a window switch or a zoom leaves
    // it off-screen. Let the app know the button came up rather than letting
    // the gesture evaporate with it still held.
    let Some(rect) = pane_rect_for(graph, &attached, client_size, config, pane_id).await else {
        release_app_gesture(io_state, gesture).await;
        return Ok(AppMouse::NotHandled);
    };
    // While zoomed the pane's rect is the whole content area, so a click on the
    // status bar would otherwise clamp into the pane's last row and be
    // forwarded as a click the user never made.
    let inside =
        col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height;
    if !inside && matches!(*gesture, SelectionDrag::None) {
        return Ok(AppMouse::NotHandled);
    }
    let local_col = col
        .saturating_sub(rect.x)
        .min(rect.width.saturating_sub(1))
        .saturating_add(1);
    let local_row = row
        .saturating_sub(rect.y)
        .min(rect.height.saturating_sub(1))
        .saturating_add(1);

    let Some(cb) = button_cb(action, button, alt, ctrl) else {
        return Ok(AppMouse::NotHandled);
    };
    let Some(bytes) = encode_mouse_report(
        cb,
        action == ButtonAction::Release,
        sgr,
        local_col,
        local_row,
    ) else {
        // Not encodable for this app's coordinate mode. Consume it: the pane is
        // still the app's, and a stray shux selection under a running TUI is
        // not a better answer than nothing happening.
        end_gesture_if_released(gesture, action, button);
        return Ok(AppMouse::Consumed { redraw: false });
    };

    let mut redraw = false;
    if action == ButtonAction::Press {
        // tmux's `MouseDown1Pane` is `select-pane` followed by `send-keys -M`:
        // the click both focuses the pane and reaches the app. (The wheel's
        // forward tier deliberately does NOT focus, which is why this cites
        // tmux rather than `handle_wheel`.)
        if pane_id != attached.active_pane_id {
            let _ = graph.focus_pane(pane_id).await;
            session.lock().await.active_pane_id = pane_id;
            redraw = true;
        }
        // A selection left over from before the app took the mouse can no
        // longer be cleared by clicking, so clear it here or it renders
        // forever.
        let mut s = session.lock().await;
        if s.mouse_selection.is_some() || s.copy_menu.is_some() {
            s.mouse_selection = None;
            s.copy_menu = None;
            redraw = true;
        }
    }

    if !forward_bytes_to_pane(io_state, pane_id, bytes).await {
        // Dropped on the floor. Opening a gesture now would hand the app a
        // release with no press behind it.
        return Ok(AppMouse::Consumed { redraw });
    }

    if action == ButtonAction::Press {
        let held = match *gesture {
            SelectionDrag::App { buttons, .. } => buttons,
            _ => 0,
        };
        *gesture = SelectionDrag::App {
            pane_id,
            buttons: held | button_bit(button),
            last: (local_col, local_row),
        };
    } else {
        // Where the pointer was when the gesture is abandoned, so
        // `release_app_gesture` can put the synthetic release there.
        if let SelectionDrag::App { last, .. } = gesture {
            *last = (local_col, local_row);
        }
        end_gesture_if_released(gesture, action, button);
    }
    Ok(AppMouse::Consumed { redraw })
}

/// Clear the released button from an app gesture, ending it once nothing is
/// held.
///
/// A release whose button is not in the mask clears the whole mask: without SGR
/// reporting the host cannot say which button came up, and a mask that can
/// never empty is a gesture that never ends.
fn end_gesture_if_released(
    gesture: &mut SelectionDrag,
    action: ButtonAction,
    button: ProtoMouseButton,
) {
    if action != ButtonAction::Release {
        return;
    }
    let SelectionDrag::App { buttons, .. } = gesture else {
        return;
    };
    let bit = button_bit(button);
    *buttons = if bit != 0 && *buttons & bit != 0 {
        *buttons & !bit
    } else {
        0
    };
    if *buttons == 0 {
        *gesture = SelectionDrag::None;
    }
}

/// How a wheel tick is routed, given the target pane's live VT state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WheelRouting {
    /// App requested mouse tracking — forward an encoded mouse report.
    ForwardMouse,
    /// Alternate-screen app without mouse (alt-scroll on) — send arrow keys.
    ArrowKeys,
    /// Primary screen — scroll shux's own scrollback via copy mode.
    Scrollback,
}

/// Route a wheel event by the target pane's mode state (tmux/wezterm model).
fn route_wheel(mouse_on: bool, alt_screen: bool, alt_scroll: bool) -> WheelRouting {
    if mouse_on {
        WheelRouting::ForwardMouse
    } else if alt_screen && alt_scroll {
        WheelRouting::ArrowKeys
    } else {
        WheelRouting::Scrollback
    }
}

/// The largest coordinate the legacy byte-packed encodings can carry.
///
/// X10 offsets each value by 32 and packs it into one byte, so 223 is the last
/// value that fits. xterm reports `0` past it; shux sends nothing at all --
/// see [`encode_mouse_report`].
const X10_MOUSE_LIMIT: u16 = 223;

/// Encode one mouse report for a pane's application.
///
/// The single encoder for every mouse event shux forwards: wheel ticks, button
/// presses, drags and releases. It was two, and the duplicate meant a fix to
/// the coordinate encoding had two homes and only ever found one.
///
/// `cb` is the xterm button code before any encoding-specific rewrite: 0/1/2
/// for left/middle/right, `+32` for motion, 3+32 for motion with no button
/// held, 64/65 for wheel up/down, plus the modifier bits (shift 4, alt 8,
/// ctrl 16). `col`/`row` are **1-based pane-local** cells.
///
/// SGR (mode 1006) ends a release with `m` and keeps the real button, so the
/// app learns which button came up. Legacy X10 has no way to say that: every
/// release is `Cb = 3`, and the modifier bits ride along on it.
///
/// Returns `None` when the report cannot be encoded truthfully, and callers
/// forward nothing:
///
/// - a legacy coordinate past [`X10_MOUSE_LIMIT`]. The old code clamped to 255,
///   which is not a refusal — it is a perfectly valid report for a cell the
///   user did not click, so the app acts at column 223. For a wheel tick that
///   is a mis-aimed scroll; for a click it is the wrong button pressed in
///   vim or lazygit, silently.
/// - a 1-based coordinate of `0`, which is not a cell.
fn encode_mouse_report(cb: u16, release: bool, sgr: bool, col: u16, row: u16) -> Option<Vec<u8>> {
    // No `debug_assert!` here: it would panic in exactly the builds the tests
    // run, making the graceful branch below dead code everywhere it is checked.
    if col == 0 || row == 0 {
        return None;
    }
    if sgr {
        let fin = if release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{cb};{col};{row}{fin}").into_bytes());
    }
    if col > X10_MOUSE_LIMIT || row > X10_MOUSE_LIMIT || cb > X10_MOUSE_LIMIT {
        return None;
    }
    // Legacy cannot name the released button; 3 is "some button came up".
    // Modifier bits survive, which is what xterm does.
    let cb = if release { 3 | (cb & !3) } else { cb };
    let enc = |v: u16| -> u8 { (v + 32) as u8 };
    Some(vec![0x1b, b'[', b'M', enc(cb), enc(col), enc(row)])
}

/// Encode a wheel tick. Button 64 = wheel up, 65 = wheel down.
fn encode_mouse_wheel(up: bool, sgr: bool, col: u16, row: u16) -> Option<Vec<u8>> {
    encode_mouse_report(if up { 64 } else { 65 }, false, sgr, col, row)
}

/// The arrow-key bytes one wheel tick maps to on the alternate screen when the
/// app hasn't requested mouse tracking. DECCKM chooses CSI (`ESC [ A`) vs SS3
/// (`ESC O A`) form.
fn wheel_arrow_seq(up: bool, app_cursor: bool) -> &'static [u8] {
    match (up, app_cursor) {
        (true, false) => b"\x1b[A",
        (false, false) => b"\x1b[B",
        (true, true) => b"\x1bOA",
        (false, true) => b"\x1bOB",
    }
}

/// Send bytes to a pane's PTY writer using the same non-blocking path as
/// keystrokes (drop on backpressure rather than freeze the attach loop).
///
/// Returns whether the bytes were actually queued. Button forwarding needs to
/// know: a press that was dropped must not open a gesture, or the release that
/// follows reaches the app with no matching press and leaves it button-held.
async fn forward_bytes_to_pane(
    io_state: &Arc<Mutex<PaneIoState>>,
    pane_id: PaneId,
    bytes: Vec<u8>,
) -> bool {
    let writer = {
        let state = io_state.lock().await;
        state.writers.get(&pane_id).cloned()
    };
    match writer {
        None => false,
        Some(tx) => match tx.try_send(bytes) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(error = %e, "mouse forward dropped (pane backpressured)");
                false
            }
        },
    }
}

/// Handle a scroll-wheel event while copy mode is NOT active. Returns `Ok(true)`
/// if the event was a wheel event (and thus consumed), `Ok(false)` otherwise so
/// the caller falls through to the layout/focus mouse handler.
#[allow(clippy::too_many_arguments)]
async fn handle_wheel(
    kind: MouseKind,
    col: u16,
    row: u16,
    graph: &GraphHandle,
    io_state: &Arc<Mutex<PaneIoState>>,
    session: &Arc<Mutex<AttachedSession>>,
    client_size: &ClientSize,
    config: &ConfigHandle,
) -> anyhow::Result<bool> {
    let up = match kind {
        MouseKind::ScrollUp => true,
        MouseKind::ScrollDown => false,
        _ => return Ok(false),
    };
    let attached = session.lock().await.clone();
    let viewport = current_viewport(client_size, config).await;

    // Resolve the pane under the cursor (fallback: the active pane).
    let pane_id = {
        let snap = graph.snapshot();
        match snap.windows.get(&attached.active_window_id) {
            None => return Ok(true),
            Some(win) => {
                if win.layout.is_zoomed() {
                    attached.active_pane_id
                } else {
                    pane_at(&win.layout.tree, viewport, col, row)
                        .map(|(pid, _)| pid)
                        .unwrap_or(attached.active_pane_id)
                }
            }
        }
    };
    let Some(rect) = pane_rect_for(graph, &attached, client_size, config, pane_id).await else {
        return Ok(true);
    };

    // Snapshot the target pane's live mode state + scrollback depth.
    let (mouse_on, sgr, alt, alt_scroll, app_cursor, total_lines) = {
        let state = io_state.lock().await;
        match state.vts.get(&pane_id) {
            Some(vt) => {
                let m = vt.modes();
                (
                    m.mouse_tracking != shux_vt::MouseMode::None,
                    m.sgr_mouse,
                    vt.is_alternate_screen(),
                    m.alternate_scroll,
                    m.application_cursor_keys,
                    vt.presented_total_lines(),
                )
            }
            None => (false, false, false, true, false, rect.height as usize),
        }
    };

    match route_wheel(mouse_on, alt, alt_scroll) {
        WheelRouting::ForwardMouse => {
            let local_col = col
                .saturating_sub(rect.x)
                .min(rect.width.saturating_sub(1))
                .saturating_add(1);
            let local_row = row
                .saturating_sub(rect.y)
                .min(rect.height.saturating_sub(1))
                .saturating_add(1);
            // `None` means the tick cannot be encoded truthfully for this
            // app's coordinate mode; sending nothing beats sending a tick
            // aimed at a cell the user never touched.
            if let Some(bytes) = encode_mouse_wheel(up, sgr, local_col, local_row) {
                forward_bytes_to_pane(io_state, pane_id, bytes).await;
            }
        }
        WheelRouting::ArrowKeys => {
            let seq = wheel_arrow_seq(up, app_cursor);
            let mut bytes = Vec::with_capacity(seq.len() * 3);
            for _ in 0..3 {
                bytes.extend_from_slice(seq);
            }
            forward_bytes_to_pane(io_state, pane_id, bytes).await;
        }
        WheelRouting::Scrollback => {
            // This tier is only reached while copy mode is INACTIVE (an active
            // copy mode consumes the wheel earlier, in handle_copy_mode_mouse).
            // So wheel-down here has nothing to scroll — only wheel-up opens a
            // transient, wheel-initiated scrollback view.
            if !up {
                return Ok(true);
            }
            // Nothing above the live view — don't pointlessly enter copy mode.
            if shux_ui::copy_mode::max_scroll_offset(total_lines, rect.height) == 0 {
                return Ok(true);
            }
            // Scrolling a background pane focuses it (the overlay renders for the
            // active pane), matching tmux.
            if pane_id != attached.active_pane_id {
                let _ = graph.focus_pane(pane_id).await;
                session.lock().await.active_pane_id = pane_id;
            }
            let mut s = session.lock().await;
            let cm = s.copy_mode.get_or_insert_with(|| {
                let mut st = shux_ui::CopyModeState::new();
                st.wheel_initiated = true;
                st
            });
            shux_ui::copy_mode::scroll_up(cm, 3, total_lines, rect.height);
        }
    }
    Ok(true)
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
    config: &ConfigHandle,
) -> (u16, u16) {
    if pane_id != attached.active_pane_id {
        return (0, 0);
    }
    focused_pane_rect(graph, attached, client_size, config)
        .await
        .map(|rect| (rect.width, rect.height))
        .unwrap_or((0, 0))
}

async fn focused_pane_rect(
    graph: &GraphHandle,
    attached: &AttachedSession,
    client_size: &ClientSize,
    config: &ConfigHandle,
) -> Option<Rect> {
    pane_rect_for(
        graph,
        attached,
        client_size,
        config,
        attached.active_pane_id,
    )
    .await
}

async fn pane_rect_for(
    graph: &GraphHandle,
    attached: &AttachedSession,
    client_size: &ClientSize,
    config: &ConfigHandle,
    pane_id: PaneId,
) -> Option<Rect> {
    let content = current_content_rect(client_size).await;
    let viewport = current_viewport(client_size, config).await;
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
/// split) and by every mouse hit-test, so the geometry they reason about
/// matches what the user sees — not a hardcoded 120x40 fiction.
///
/// Delegates to [`shux_ui::pane_viewport`], the same function the compositor
/// lays panes out with. It used to inset for the outline unconditionally while
/// the compositor inset only when the outline was drawn, which put every
/// hit-test one cell off under `appearance.border_style = "none"`.
///
/// The style is read from the live config on every call rather than cached on
/// [`AttachedSession`]. A cache here was wrong twice over: it went stale for the
/// common case (a user whose config says `none` and never edits it — the render
/// loop only republished ON CHANGE, so the value never left its default), and
/// seeding it separately from the render loop's own change detection left a
/// window where a reload between the two reads stranded it forever. A config
/// read is an ArcSwap load.
///
/// `zoomed: false` is not an oversight: while zoomed there is one pane filling
/// the content rect, and every caller here takes the zoomed branch before it
/// asks for a viewport.
async fn current_viewport(client_size: &ClientSize, config: &ConfigHandle) -> Rect {
    let border_style = BorderStyle::parse(&config.current().appearance.border_style);
    shux_ui::pane_viewport(current_content_rect(client_size).await, border_style, false)
}

/// Dispatch an Action keybinding from the client.
#[allow(clippy::too_many_arguments)]
async fn handle_action(
    kind: ActionKind,
    args: shux_rpc::attach::ActionArgs,
    graph: &GraphHandle,
    io_state: &Arc<Mutex<PaneIoState>>,
    session: &Arc<Mutex<AttachedSession>>,
    client_size: &ClientSize,
    config: &ConfigHandle,
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
    let viewport = current_viewport(client_size, config).await;

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

    // Splitting focuses the new pane, so an undo has to put focus back itself
    // or a failed split silently relocates the operator's cursor.
    let prior_active_pane = graph
        .snapshot()
        .windows
        .get(&attached.active_window_id)
        .map(|w| w.active_pane);

    let new_pane = graph
        .split_pane(attached.active_pane_id, direction, 0.5)
        .await
        .map_err(|e| anyhow::anyhow!("split failed: {e}"))?;

    // A PTY that never started is not a pane. Discarding this error left a
    // phantom in the graph — focused, drawn, and answering "pane VT not found"
    // to every later verb — while the keystroke looked like it worked. The RPC
    // `pane.split` has rolled this back since issue #125; the attach path
    // never did, and `[shell].command` (issue #132) makes a default-pane spawn
    // newly able to fail, so the latent case became a reachable one.
    if let Err(e) = crate::pane_spawn::spawn_pane_pty(
        new_pane,
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/tmp")),
        Vec::new(),
        shux_pty::handle::PtySize::default(),
        Vec::new(),
        false,
        io_state.clone(),
        cancel.clone(),
        graph.clone(),
    )
    .await
    {
        // Compare-and-restore, not restore: an operator who moved focus while
        // the PTY was starting keeps their choice (the lost-update lesson from
        // #125's own rollback).
        let focus_is_still_ours = graph
            .snapshot()
            .windows
            .get(&attached.active_window_id)
            .map(|w| w.active_pane)
            == Some(new_pane);

        let _ = graph.destroy_pane(new_pane, None).await;

        if focus_is_still_ours
            && let Some(prev) = prior_active_pane
            && graph
                .snapshot()
                .windows
                .get(&attached.active_window_id)
                .map(|w| w.active_pane)
                != Some(prev)
        {
            let _ = graph.focus_pane(prev).await;
        }
        return Err(anyhow::anyhow!(
            "{}",
            crate::pane_spawn::spawn_failure_message(&e)
        ));
    }
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
    if let Some(win) = snap.windows.get(&attached.active_window_id)
        && win.layout.is_zoomed()
    {
        return Ok(());
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
    // A window whose only pane never started is not a window. Discarding this
    // left one in the graph and then focused it, so the attach UI switched to
    // a window that could never render — same contract the `window.create` RPC
    // has enforced since issue #125.
    if let Err(e) = crate::pane_spawn::spawn_pane_pty(
        pane_id,
        cwd,
        Vec::new(),
        shux_pty::handle::PtySize::default(),
        Vec::new(),
        false,
        io_state.clone(),
        cancel.clone(),
        graph.clone(),
    )
    .await
    {
        let _ = graph.destroy_window(window_id, None).await;
        return Err(anyhow::anyhow!(
            "{}",
            crate::pane_spawn::spawn_failure_message(&e)
        ));
    }

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
    /// A `ConfigHandle` on a path that does not exist, i.e. the defaults. The
    /// mouse handlers take one because the pane viewport is derived from
    /// `appearance.border_style` rather than cached.
    fn test_config() -> ConfigHandle {
        ConfigHandle::load_or_default(std::path::Path::new(
            "/nonexistent/shux-attach-tests/config.toml",
        ))
    }

    use super::*;

    /// Attach must close the spawn race without resurrecting a pane that
    /// genuinely finished (issue #125 follow-up).
    #[test]
    fn attach_spawns_only_a_pane_that_never_ran() {
        use shux_core::model::{Pane, WindowId};

        let mut pane = Pane::with_command(WindowId::new(), "/work", vec!["make".into()]);
        assert!(
            pane_awaits_first_spawn(&pane),
            "a pane that has not started yet must be spawned"
        );

        for status in [0, 1, 127, -1] {
            pane.exit_status = Some(status);
            assert!(
                !pane_awaits_first_spawn(&pane),
                "a pane that exited ({status}) must not be respawned as a shell"
            );
        }
    }

    #[test]
    fn wheel_routing_follows_pane_mode_state() {
        // App requested the mouse → forward, regardless of screen/alt-scroll.
        assert_eq!(route_wheel(true, false, false), WheelRouting::ForwardMouse);
        assert_eq!(route_wheel(true, true, true), WheelRouting::ForwardMouse);
        // Alt screen, no mouse, alt-scroll on → arrow keys (less/man/vim).
        assert_eq!(route_wheel(false, true, true), WheelRouting::ArrowKeys);
        // Alt screen, no mouse, alt-scroll OFF → fall back to scrollback.
        assert_eq!(route_wheel(false, true, false), WheelRouting::Scrollback);
        // Primary screen, no mouse → shux scrollback (the bug's fix path).
        assert_eq!(route_wheel(false, false, true), WheelRouting::Scrollback);
        assert_eq!(route_wheel(false, false, false), WheelRouting::Scrollback);
    }

    #[test]
    fn encode_mouse_wheel_sgr_is_byte_exact() {
        // Wheel up/down at pane-local (col=10, row=5), SGR encoding.
        assert_eq!(
            encode_mouse_wheel(true, true, 10, 5).as_deref(),
            Some(&b"\x1b[<64;10;5M"[..])
        );
        assert_eq!(
            encode_mouse_wheel(false, true, 10, 5).as_deref(),
            Some(&b"\x1b[<65;10;5M"[..])
        );
    }

    #[test]
    fn encode_mouse_wheel_x10_offsets_by_32() {
        // X10: ESC [ M  Cb  Cx  Cy, each byte = value + 32.
        // up at (1,1): Cb=64+32=96, Cx=Cy=1+32=33.
        assert_eq!(
            encode_mouse_wheel(true, false, 1, 1),
            Some(vec![0x1b, b'[', b'M', 96, 33, 33])
        );
        // down at (1,1): Cb=65+32=97.
        assert_eq!(
            encode_mouse_wheel(false, false, 1, 1),
            Some(vec![0x1b, b'[', b'M', 97, 33, 33])
        );
    }

    /// Issue #174. This test previously asserted the opposite -- that a
    /// coordinate past the legacy limit is clamped to the 255 wire ceiling.
    /// Clamping is not a refusal: byte 255 is a perfectly valid report for
    /// column 223, so the app acts on a cell the user never touched. For a
    /// wheel tick that is a mis-aimed scroll; once clicks travel the same
    /// encoder it is the wrong button pressed, silently. Refusing to encode
    /// leaves the app where it was.
    #[test]
    fn encode_mouse_report_refuses_legacy_coordinates_it_cannot_carry() {
        // 223 is the last cell the byte-packed form can name: 223+32 = 255.
        assert_eq!(
            encode_mouse_wheel(true, false, 223, 1).map(|b| b[4]),
            Some(255)
        );
        assert_eq!(encode_mouse_wheel(true, false, 224, 1), None);
        assert_eq!(encode_mouse_wheel(true, false, 400, 1), None);
        assert_eq!(encode_mouse_wheel(true, false, 1, 224), None);
        // SGR has no such ceiling -- it is decimal text.
        assert_eq!(
            encode_mouse_wheel(true, true, 400, 1).as_deref(),
            Some(&b"\x1b[<64;400;1M"[..])
        );
        // 1-based coordinates: 0 is not a cell in either encoding.
        assert_eq!(encode_mouse_report(0, false, true, 0, 5), None);
        assert_eq!(encode_mouse_report(0, false, false, 5, 0), None);
    }

    #[test]
    fn wheel_arrow_seq_honors_application_cursor_keys() {
        assert_eq!(wheel_arrow_seq(true, false), b"\x1b[A");
        assert_eq!(wheel_arrow_seq(false, false), b"\x1b[B");
        assert_eq!(wheel_arrow_seq(true, true), b"\x1bOA");
        assert_eq!(wheel_arrow_seq(false, true), b"\x1bOB");
    }

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
        assert_eq!(
            current_viewport(&size, &test_config()).await,
            Rect::new(1, 1, 118, 37)
        );
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

    /// Issue #120 — `session attach` resolves a reference before it considers
    /// creating. It is the one verb where an unresolved id is not an error: it
    /// CREATES, so a short id used to produce a blank session named after the
    /// id while the session the operator meant sat untouched.
    ///
    /// Driven here rather than end-to-end because attaching needs a terminal,
    /// and how far the attach path gets without one is environment-dependent.
    #[tokio::test]
    async fn attach_resolves_an_id_before_it_creates() {
        let (graph_inner, state) = shux_core::graph::SessionGraph::new();
        let (cmd_tx, cmd_rx) = mpsc::channel(128);
        let cancel = CancellationToken::new();
        let task = {
            let cancel = cancel.clone();
            tokio::spawn(async move {
                shux_core::graph::run_graph_loop(graph_inner, cmd_rx, cancel).await
            })
        };
        let graph = GraphHandle::new(cmd_tx, state);
        let meta = crate::session_meta::SessionMetaCache::new();
        let cwd = std::env::temp_dir();

        let target = graph
            .create_session("work".to_string(), cwd.clone())
            .await
            .expect("create session");
        let short = target.to_string()[..8].to_string();
        let before = graph.snapshot().sessions.len();

        // The short id `session list` prints attaches to that session.
        let got = resolve_or_create_session(&graph, &Some(short.clone()), &meta)
            .await
            .expect("attach by short id");
        assert_eq!(
            got.session_id, target,
            "short id resolved to another session"
        );
        assert_eq!(got.name, "work");
        assert_eq!(
            graph.snapshot().sessions.len(),
            before,
            "attaching by short id created a session"
        );

        // …and so does the full uuid.
        let got = resolve_or_create_session(&graph, &Some(target.to_string()), &meta)
            .await
            .expect("attach by full uuid");
        assert_eq!(got.session_id, target);
        assert_eq!(graph.snapshot().sessions.len(), before);

        // A genuinely new NAME still creates — the behaviour the id branch
        // must not have swallowed.
        let got = resolve_or_create_session(&graph, &Some("brand-new".to_string()), &meta)
            .await
            .expect("attach by new name");
        assert_ne!(got.session_id, target);
        assert_eq!(got.name, "brand-new");
        assert_eq!(
            graph.snapshot().sessions.len(),
            before + 1,
            "attaching by a new name must still create that session"
        );

        // An exact NAME still beats a partial id, even when the name IS one.
        let impostor = graph
            .create_session(short.clone(), cwd.clone())
            .await
            .expect("create impostor");
        let got = resolve_or_create_session(&graph, &Some(short.clone()), &meta)
            .await
            .expect("attach by ambiguous-looking name");
        assert_eq!(
            got.session_id, impostor,
            "an exact name must beat an id prefix"
        );

        // A well-formed uuid naming nothing is refused, not turned into a
        // session called after itself.
        let orphan = "00000000-0000-4000-8000-000000000001";
        let err = resolve_or_create_session(&graph, &Some(orphan.to_string()), &meta)
            .await
            .expect_err("an unknown uuid must not create a session");
        assert!(err.to_string().contains(orphan), "{err}");

        cancel.cancel();
        task.abort();
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
        mpsc::Receiver<crate::pane_io::ResizeRequest>,
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
            &test_config(),
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
            &test_config(),
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

    /// The client picks the size it reports. A zoomed window subtracts the
    /// status bar from it with no layout arithmetic in between, so a tiny
    /// `rows` used to reach the PTY and the VT as a pane of height 0 — where a
    /// single printable byte of pane output panicked the pane I/O task and
    /// DECSTBM could grow the grid without bound (issue #107).
    ///
    /// Both branches must apply the same floor AND the same ceiling, tiled and
    /// zoomed. The ceiling matters as much: `cols`/`rows` are `u16`, the client
    /// picks them, and the daemon allocates a grid against them.
    #[tokio::test]
    async fn resize_fanout_never_reports_a_degenerate_pane_size() {
        let fixture = attach_fixture().await;
        let (_, mut first_resize_rx, _) =
            seed_io_for_pane(&fixture.io_state, fixture.first_pane).await;
        let (_, mut second_resize_rx, _) =
            seed_io_for_pane(&fixture.io_state, fixture.second_pane).await;

        for zoomed in [false, true] {
            if zoomed {
                fixture
                    .graph
                    .zoom_pane(fixture.first_pane, None)
                    .await
                    .expect("zoom active pane");
            }
            // rows=0/1 make content_h saturate to 0; cols=0/1 do the same
            // horizontally. Each must still yield a usable pane.
            for (cols, rows) in [
                (0u16, 0u16),
                (1, 1),
                (0, 1),
                (1, 0),
                (2, 2),
                (100, 1),
                // A 93-byte handshake declaring this asked for ~4.3e9 cells
                // and OOM-killed the daemon with every session on it.
                (u16::MAX, u16::MAX),
                (1200, 1200),
                (4000, 4000),
            ] {
                apply_resize_to_window(
                    &fixture.graph,
                    &fixture.io_state,
                    &fixture.attached,
                    &test_config(),
                    cols,
                    rows,
                )
                .await;

                for (label, rx) in [
                    ("first", &mut first_resize_rx),
                    ("second", &mut second_resize_rx),
                ] {
                    let req = tokio::time::timeout(Duration::from_secs(1), rx.recv())
                        .await
                        .unwrap_or_else(|_| panic!("{label} resize (zoomed={zoomed})"))
                        .unwrap_or_else(|| panic!("{label} resize request"));
                    assert!(
                        req.size.rows >= MIN_PANE_ROWS && req.size.cols >= MIN_PANE_COLS,
                        "zoomed={zoomed} client {cols}x{rows} produced pane size \
                         {}x{} for the {label} pane; floor is {MIN_PANE_COLS}x{MIN_PANE_ROWS}",
                        req.size.cols,
                        req.size.rows
                    );
                    assert!(
                        req.size.rows <= MAX_CLIENT_ROWS && req.size.cols <= MAX_CLIENT_COLS,
                        "zoomed={zoomed} client {cols}x{rows} produced pane size \
                         {}x{} for the {label} pane; ceiling is \
                         {MAX_CLIENT_COLS}x{MAX_CLIENT_ROWS}",
                        req.size.cols,
                        req.size.rows
                    );
                }
            }
        }

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
            &test_config(),
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
            &test_config(),
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
            &test_config(),
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
            &test_config(),
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
            &test_config(),
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
            &test_config(),
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
            &test_config(),
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
            &test_config(),
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
            &test_config(),
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
            &test_config(),
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
            &test_config(),
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
                &test_config(),
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
            &test_config(),
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
            &test_config(),
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
                &test_config(),
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
                &test_config(),
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
                &test_config(),
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
                &test_config(),
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
            &test_config(),
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
            &test_config(),
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
            &test_config(),
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
        let viewport = current_viewport(&client_size, &test_config()).await;
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
            &test_config(),
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
            &test_config(),
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
            &test_config(),
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
            &test_config(),
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
            &test_config(),
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

    /// Seed a pane whose VT has real scrollback (more lines than fit), plus any
    /// mode-enabling setup bytes (e.g. `?1000h`, `?1049h`). Returns the pane's
    /// PTY writer receiver so a test can assert what the app was sent.
    async fn seed_scrollback_pane(
        io_state: &Arc<Mutex<PaneIoState>>,
        pane_id: PaneId,
        setup: &[u8],
    ) -> mpsc::Receiver<Vec<u8>> {
        let (writer_tx, writer_rx) = mpsc::channel(64);
        let (resize_tx, _resize_rx) = mpsc::channel(8);
        let shutdown = CancellationToken::new();
        let mut vt = shux_vt::VirtualTerminal::new(28, 49);
        for i in 0..300 {
            vt.process(format!("scrollback line {i}\r\n").as_bytes());
        }
        vt.process(setup);
        let mut state = io_state.lock().await;
        state.writers.insert(pane_id, writer_tx);
        state.resizers.insert(pane_id, resize_tx);
        state.shutdowns.insert(pane_id, shutdown);
        state.vts.insert(pane_id, vt);
        writer_rx
    }

    async fn recv_bytes(rx: &mut mpsc::Receiver<Vec<u8>>) -> Vec<u8> {
        tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("pane write timeout")
            .expect("pane write channel closed")
    }

    // --- Issue #174: button events reach a mouse-aware app ---------------
    //
    // These drive the real `handle_app_mouse` and the real
    // `route_app_mouse` against a live graph + pane VT. Every one of them was
    // run against the tree before the fix: the encoder and routing tests fail
    // to compile there (the functions do not exist), and the behavioural ones
    // were re-checked against a neutered `handle_app_mouse` that returns
    // `NotHandled` unconditionally, which is what the old code did.

    /// Byte-exact reports, checked against what crossterm's own decoder reads
    /// back. The classic defects here are silent: swapping middle and right
    /// puts a paste where a context menu belongs, and emitting `M` for a
    /// release leaves the app button-held forever.
    #[test]
    fn encode_mouse_report_is_byte_exact_for_every_button_and_action() {
        let sgr = |cb, release| encode_mouse_report(cb, release, true, 10, 5).unwrap();
        let cb = |action, button| button_cb(action, button, false, false).unwrap();

        // Press: left 0, middle 1, right 2.
        assert_eq!(
            sgr(cb(ButtonAction::Press, ProtoMouseButton::Left), false),
            b"\x1b[<0;10;5M"
        );
        assert_eq!(
            sgr(cb(ButtonAction::Press, ProtoMouseButton::Middle), false),
            b"\x1b[<1;10;5M"
        );
        assert_eq!(
            sgr(cb(ButtonAction::Press, ProtoMouseButton::Right), false),
            b"\x1b[<2;10;5M"
        );
        // Release: lowercase `m`, and the REAL button survives.
        assert_eq!(
            sgr(cb(ButtonAction::Release, ProtoMouseButton::Left), true),
            b"\x1b[<0;10;5m"
        );
        assert_eq!(
            sgr(cb(ButtonAction::Release, ProtoMouseButton::Right), true),
            b"\x1b[<2;10;5m"
        );
        // Drag: motion bit 32, still `M`.
        assert_eq!(
            sgr(cb(ButtonAction::Drag, ProtoMouseButton::Left), false),
            b"\x1b[<32;10;5M"
        );
        assert_eq!(
            sgr(cb(ButtonAction::Drag, ProtoMouseButton::Middle), false),
            b"\x1b[<33;10;5M"
        );
        // Motion with no button held is 3+32, not a left click.
        assert_eq!(
            sgr(cb(ButtonAction::Drag, ProtoMouseButton::None), false),
            b"\x1b[<35;10;5M"
        );
    }

    /// `None` is not a button. Answering "left" for it is how a hover turns
    /// into a click nobody made.
    #[test]
    fn button_cb_refuses_to_invent_a_button() {
        assert_eq!(
            button_cb(ButtonAction::Press, ProtoMouseButton::None, false, false),
            None
        );
        assert_eq!(
            button_cb(ButtonAction::Release, ProtoMouseButton::None, false, false),
            None
        );
        assert_eq!(
            button_cb(ButtonAction::Drag, ProtoMouseButton::None, false, false),
            Some(35)
        );
    }

    /// Modifier bits are 4/8/16, not 1/2/4. Shift is absent by design: it is
    /// reserved for shux, so it never reaches an app to be encoded.
    #[test]
    fn button_cb_carries_alt_and_ctrl_but_never_shift() {
        let left = ProtoMouseButton::Left;
        assert_eq!(button_cb(ButtonAction::Press, left, false, true), Some(16));
        assert_eq!(button_cb(ButtonAction::Press, left, true, false), Some(8));
        assert_eq!(button_cb(ButtonAction::Press, left, true, true), Some(24));
        // ctrl + alt while dragging: 0 + 32 + 8 + 16.
        assert_eq!(button_cb(ButtonAction::Drag, left, true, true), Some(56));
    }

    /// Legacy X10 cannot name the released button, but the modifier bits ride
    /// along on the `3` exactly as xterm does.
    #[test]
    fn x10_release_is_button_three_and_keeps_modifiers() {
        let cb = button_cb(ButtonAction::Release, ProtoMouseButton::Right, false, true).unwrap();
        assert_eq!(cb, 18, "right + ctrl");
        assert_eq!(
            encode_mouse_report(cb, true, false, 1, 1),
            // 3 | (18 & !3) = 19, +32 = 51.
            Some(vec![0x1b, b'[', b'M', 51, 33, 33])
        );
        // SGR keeps the real button instead.
        assert_eq!(
            encode_mouse_report(cb, true, true, 1, 1).as_deref(),
            Some(&b"\x1b[<18;1;1m"[..])
        );
    }

    /// A real terminal reports only what the app subscribed to.
    #[test]
    fn mode_reports_matches_what_each_xterm_mode_subscribes_to() {
        use shux_vt::MouseMode;
        for action in [
            ButtonAction::Press,
            ButtonAction::Release,
            ButtonAction::Drag,
        ] {
            assert!(!mode_reports(MouseMode::None, action), "{action:?}");
            assert!(mode_reports(MouseMode::ButtonEvent, action), "{action:?}");
            assert!(mode_reports(MouseMode::AnyEvent, action), "{action:?}");
        }
        assert!(mode_reports(MouseMode::Normal, ButtonAction::Press));
        assert!(mode_reports(MouseMode::Normal, ButtonAction::Release));
        assert!(
            !mode_reports(MouseMode::Normal, ButtonAction::Drag),
            "mode 1000 never asked for motion"
        );
    }

    /// The three coordinate modes shux does not emit must stop forwarding.
    /// 1016 is the sharp one: apps set it alongside 1006, so `sgr_mouse` alone
    /// cannot tell them apart, and the app reads cells as pixels.
    #[test]
    fn coordinate_modes_shux_cannot_encode_stop_forwarding() {
        let probe = |setup: &[u8]| {
            let mut vt = shux_vt::VirtualTerminal::new(10, 40);
            vt.process(setup);
            coords_are_encodable(vt.modes())
        };
        assert!(probe(b"\x1b[?1000h\x1b[?1006h"), "plain SGR is encodable");
        assert!(!probe(b"\x1b[?1000h\x1b[?1006h\x1b[?1016h"), "1016 pixel");
        assert!(!probe(b"\x1b[?1000h\x1b[?1005h"), "1005 utf-8");
        assert!(!probe(b"\x1b[?1000h\x1b[?1015h"), "1015 urxvt");
        // …and turning it back off restores forwarding.
        assert!(probe(b"\x1b[?1000h\x1b[?1006h\x1b[?1016h\x1b[?1016l"));
    }

    /// The precedence table, exhaustively. Written as a pure function for
    /// exactly this: inferring it from the order of early returns in the
    /// handler is how the border-drag and copy-menu cases got missed.
    #[test]
    fn route_app_mouse_precedence() {
        use shux_vt::MouseMode;
        let pane = PaneId::new();
        let other = PaneId::new();
        let press = ButtonAction::Press;

        let route = |gesture, border, copy, shift, hit, mode, enc, action| {
            route_app_mouse(gesture, border, copy, shift, hit, mode, enc, action)
        };

        // Baseline: a mouse-aware pane under the pointer takes the press.
        assert_eq!(
            route(
                SelectionDrag::None,
                false,
                false,
                false,
                Some(pane),
                MouseMode::Normal,
                true,
                press
            ),
            AppMouseRoute::Forward(pane)
        );
        // An in-flight app gesture keeps the whole gesture, even over another
        // pane and even once the app turned tracking off.
        assert_eq!(
            route(
                SelectionDrag::App {
                    pane_id: pane,
                    buttons: 1,
                    last: (1, 1)
                },
                false,
                false,
                true,
                Some(other),
                MouseMode::None,
                true,
                ButtonAction::Release
            ),
            AppMouseRoute::Swallow,
            "a gesture the app can no longer decode is still not shux's"
        );
        assert_eq!(
            route(
                SelectionDrag::App {
                    pane_id: pane,
                    buttons: 1,
                    last: (1, 1)
                },
                false,
                false,
                true,
                None,
                MouseMode::ButtonEvent,
                true,
                ButtonAction::Drag
            ),
            AppMouseRoute::Forward(pane),
            "shift pressed mid-drag must not split the gesture"
        );
        // shux's own modes and gestures win.
        for (label, gesture, border, copy) in [
            (
                "shux selection in flight",
                SelectionDrag::MouseSelection { pane_id: pane },
                false,
                false,
            ),
            ("copy mode gesture", SelectionDrag::CopyMode, false, false),
            ("border resize in flight", SelectionDrag::None, true, false),
            ("copy mode or menu open", SelectionDrag::None, false, true),
        ] {
            assert_eq!(
                route(
                    gesture,
                    border,
                    copy,
                    false,
                    Some(pane),
                    MouseMode::ButtonEvent,
                    true,
                    press
                ),
                AppMouseRoute::Shux,
                "{label}"
            );
        }
        // Shift is the escape hatch.
        assert_eq!(
            route(
                SelectionDrag::None,
                false,
                false,
                true,
                Some(pane),
                MouseMode::ButtonEvent,
                true,
                press
            ),
            AppMouseRoute::Shux
        );
        // No pane under the pointer: a border cell, the outline, the status bar.
        assert_eq!(
            route(
                SelectionDrag::None,
                false,
                false,
                false,
                None,
                MouseMode::ButtonEvent,
                true,
                press
            ),
            AppMouseRoute::Shux
        );
        // An app that never asked for the mouse.
        assert_eq!(
            route(
                SelectionDrag::None,
                false,
                false,
                false,
                Some(pane),
                MouseMode::None,
                true,
                press
            ),
            AppMouseRoute::Shux
        );
        // An app shux cannot encode for keeps shux's handling, not silence.
        assert_eq!(
            route(
                SelectionDrag::None,
                false,
                false,
                false,
                Some(pane),
                MouseMode::ButtonEvent,
                false,
                press
            ),
            AppMouseRoute::Shux
        );
        // Only a press opens a gesture: a stray drag or release with nothing in
        // flight belongs to whatever shux started.
        for action in [ButtonAction::Drag, ButtonAction::Release] {
            assert_eq!(
                route(
                    SelectionDrag::None,
                    false,
                    false,
                    false,
                    Some(pane),
                    MouseMode::ButtonEvent,
                    true,
                    action
                ),
                AppMouseRoute::Shux,
                "{action:?}"
            );
        }
    }

    /// A press whose button mask can never empty is a gesture that never ends.
    #[test]
    fn app_gesture_ends_only_when_every_button_is_up() {
        let pane = PaneId::new();
        let mut g = SelectionDrag::App {
            pane_id: pane,
            buttons: 0b101, // left + right
            last: (3, 4),
        };
        end_gesture_if_released(&mut g, ButtonAction::Release, ProtoMouseButton::Right);
        assert_eq!(
            g,
            SelectionDrag::App {
                pane_id: pane,
                buttons: 0b001,
                last: (3, 4)
            },
            "releasing one of two buttons must not end the gesture"
        );
        end_gesture_if_released(&mut g, ButtonAction::Release, ProtoMouseButton::Left);
        assert_eq!(g, SelectionDrag::None);

        // A host without SGR reporting decodes every release as Up(Left).
        // A mask keyed strictly to the button would strand the gesture.
        let mut g = SelectionDrag::App {
            pane_id: pane,
            buttons: 0b100, // right only
            last: (3, 4),
        };
        end_gesture_if_released(&mut g, ButtonAction::Release, ProtoMouseButton::Left);
        assert_eq!(
            g,
            SelectionDrag::None,
            "an unattributable release must still end the gesture"
        );

        // A press or drag never ends one.
        let mut g = SelectionDrag::App {
            pane_id: pane,
            buttons: 1,
            last: (3, 4),
        };
        end_gesture_if_released(&mut g, ButtonAction::Drag, ProtoMouseButton::Left);
        assert!(matches!(g, SelectionDrag::App { .. }));
    }

    // --- Issue #174: `handle_app_mouse` against a live graph + pane VT ------

    /// Every argument `handle_app_mouse` takes but the event itself, so the
    /// tests below read as the scenario rather than as a call.
    struct AppMouseHarness {
        fixture: AttachFixture,
        session: Arc<Mutex<AttachedSession>>,
        client_size: ClientSize,
        border_drag: Option<DragState>,
        gesture: SelectionDrag,
    }

    impl AppMouseHarness {
        async fn new() -> Self {
            let fixture = attach_fixture().await;
            let session = Arc::new(Mutex::new(fixture.attached.clone()));
            Self {
                fixture,
                session,
                client_size: Arc::new(Mutex::new((100, 30))),
                border_drag: None,
                gesture: SelectionDrag::None,
            }
        }

        async fn send(
            &mut self,
            kind: MouseKind,
            button: ProtoMouseButton,
            col: u16,
            row: u16,
        ) -> AppMouse {
            self.send_mod(kind, button, col, row, false, false, false)
                .await
        }

        #[allow(clippy::too_many_arguments)]
        async fn send_mod(
            &mut self,
            kind: MouseKind,
            button: ProtoMouseButton,
            col: u16,
            row: u16,
            shift: bool,
            alt: bool,
            ctrl: bool,
        ) -> AppMouse {
            handle_app_mouse(
                kind,
                button,
                col,
                row,
                shift,
                alt,
                ctrl,
                &self.fixture.graph,
                &self.fixture.io_state,
                &self.session,
                &self.client_size,
                &test_config(),
                &self.border_drag,
                &mut self.gesture,
            )
            .await
            .expect("handle_app_mouse")
        }
    }

    /// Nothing arrived on the pane's writer. Distinguished from "arrived late"
    /// by draining with a real timeout rather than a `try_recv`.
    async fn assert_no_bytes(rx: &mut mpsc::Receiver<Vec<u8>>, why: &str) {
        match tokio::time::timeout(Duration::from_millis(150), rx.recv()).await {
            Err(_) => {}
            Ok(None) => {}
            Ok(Some(bytes)) => panic!("{why}: app received {:?}", String::from_utf8_lossy(&bytes)),
        }
    }

    async fn recv_text(rx: &mut mpsc::Receiver<Vec<u8>>) -> String {
        String::from_utf8_lossy(&recv_bytes(rx).await).into_owned()
    }

    /// The headline defect: four clicks produced zero reports.
    #[tokio::test]
    async fn a_click_reaches_a_mouse_aware_app_at_pane_local_coordinates() {
        let mut h = AppMouseHarness::new().await;
        let mut wr = seed_scrollback_pane(
            &h.fixture.io_state,
            h.fixture.first_pane,
            b"\x1b[?1002h\x1b[?1006h",
        )
        .await;

        // The active pane's rect starts at (1,1) with the default outline, so
        // screen (10,5) is pane-local cell (10,5) 1-based.
        assert_eq!(
            h.send(MouseKind::Down, ProtoMouseButton::Left, 10, 5).await,
            AppMouse::Consumed { redraw: false }
        );
        assert_eq!(recv_text(&mut wr).await, "\x1b[<0;10;5M");
        h.send(MouseKind::Drag, ProtoMouseButton::Left, 12, 5).await;
        assert_eq!(recv_text(&mut wr).await, "\x1b[<32;12;5M");
        h.send(MouseKind::Up, ProtoMouseButton::Left, 12, 5).await;
        assert_eq!(recv_text(&mut wr).await, "\x1b[<0;12;5m");
        assert_eq!(h.gesture, SelectionDrag::None, "gesture must have ended");
        h.fixture.stop();
    }

    /// Screen-global coordinates reaching the app is the defect this catches:
    /// the click would land in the wrong place in every pane but the first.
    #[tokio::test]
    async fn a_click_in_a_second_pane_is_local_to_that_pane_and_focuses_it() {
        let mut h = AppMouseHarness::new().await;
        let mut wr = seed_scrollback_pane(
            &h.fixture.io_state,
            h.fixture.second_pane,
            b"\x1b[?1002h\x1b[?1006h",
        )
        .await;
        let viewport = current_viewport(&h.client_size, &test_config()).await;
        let rect = {
            let snap = h.fixture.graph.snapshot();
            let win = snap.windows.get(&h.fixture.first_window).expect("window");
            win.layout
                .compute_rects(viewport)
                .into_iter()
                .find(|(pid, _)| *pid == h.fixture.second_pane)
                .expect("second pane rect")
                .1
        };
        assert!(rect.x > 0, "the second pane must not start at the origin");

        let outcome = h
            .send(
                MouseKind::Down,
                ProtoMouseButton::Left,
                rect.x + 3,
                rect.y + 2,
            )
            .await;
        assert_eq!(
            outcome,
            AppMouse::Consumed { redraw: true },
            "focusing a background pane is a visible change"
        );
        assert_eq!(
            recv_text(&mut wr).await,
            "\x1b[<0;4;3M",
            "coordinates must be local to the clicked pane, not the screen"
        );
        // tmux's MouseDown1Pane selects the pane AND forwards the click.
        assert_eq!(
            h.fixture.graph.snapshot().windows[&h.fixture.first_window].active_pane,
            h.fixture.second_pane
        );
        assert_eq!(h.session.lock().await.active_pane_id, h.fixture.second_pane);
        h.fixture.stop();
    }

    /// Mode fidelity: 1000 subscribes to press and release only. The drag is
    /// swallowed rather than falling through, so no stray shux selection
    /// appears under a running app.
    #[tokio::test]
    async fn a_drag_is_withheld_from_a_mode_1000_app_and_not_given_to_shux() {
        let mut h = AppMouseHarness::new().await;
        let mut wr = seed_scrollback_pane(
            &h.fixture.io_state,
            h.fixture.first_pane,
            b"\x1b[?1000h\x1b[?1006h",
        )
        .await;

        h.send(MouseKind::Down, ProtoMouseButton::Left, 10, 5).await;
        assert_eq!(recv_text(&mut wr).await, "\x1b[<0;10;5M");
        assert_eq!(
            h.send(MouseKind::Drag, ProtoMouseButton::Left, 12, 5).await,
            AppMouse::Consumed { redraw: false },
            "the app owns the mouse; the drag is not shux's to reinterpret"
        );
        assert_no_bytes(&mut wr, "mode 1000 never subscribed to motion").await;
        h.send(MouseKind::Up, ProtoMouseButton::Left, 12, 5).await;
        assert_eq!(recv_text(&mut wr).await, "\x1b[<0;12;5m");
        assert!(
            h.session.lock().await.mouse_selection.is_none(),
            "a swallowed drag must not start a shux selection"
        );
        h.fixture.stop();
    }

    /// The gesture is decided at press and honoured to the end. Without this,
    /// an app enabling tracking mid-drag stranded a shux selection that no
    /// mouse action could clear.
    #[tokio::test]
    async fn a_gesture_started_in_shux_is_not_stolen_when_the_app_takes_the_mouse() {
        let mut h = AppMouseHarness::new().await;
        let mut wr = seed_scrollback_pane(&h.fixture.io_state, h.fixture.first_pane, b"").await;

        // Press lands on a plain pane: shux's.
        assert_eq!(
            h.send(MouseKind::Down, ProtoMouseButton::Left, 10, 5).await,
            AppMouse::NotHandled
        );
        h.gesture = SelectionDrag::MouseSelection {
            pane_id: h.fixture.first_pane,
        };
        // The app now turns tracking on mid-gesture (vim `:set mouse=a`).
        {
            let mut st = h.fixture.io_state.lock().await;
            st.vts
                .get_mut(&h.fixture.first_pane)
                .expect("vt")
                .process(b"\x1b[?1002h\x1b[?1006h");
        }
        assert_eq!(
            h.send(MouseKind::Drag, ProtoMouseButton::Left, 12, 5).await,
            AppMouse::NotHandled
        );
        assert_eq!(
            h.send(MouseKind::Up, ProtoMouseButton::Left, 12, 5).await,
            AppMouse::NotHandled
        );
        assert_no_bytes(&mut wr, "a gesture shux started stays shux's").await;
        h.fixture.stop();
    }

    /// The mirror image: an app losing the mouse mid-gesture still gets its
    /// release, so it does not sit button-held forever.
    #[tokio::test]
    async fn an_app_that_drops_tracking_mid_gesture_still_gets_its_release() {
        let mut h = AppMouseHarness::new().await;
        let mut wr = seed_scrollback_pane(
            &h.fixture.io_state,
            h.fixture.first_pane,
            b"\x1b[?1002h\x1b[?1006h",
        )
        .await;
        h.send(MouseKind::Down, ProtoMouseButton::Left, 10, 5).await;
        assert_eq!(recv_text(&mut wr).await, "\x1b[<0;10;5M");
        {
            let mut st = h.fixture.io_state.lock().await;
            st.vts
                .get_mut(&h.fixture.first_pane)
                .expect("vt")
                .process(b"\x1b[?1002l");
        }
        h.send(MouseKind::Up, ProtoMouseButton::Left, 10, 5).await;
        assert_no_bytes(
            &mut wr,
            "tracking is off, so the release is swallowed, not forwarded",
        )
        .await;
        assert_eq!(h.gesture, SelectionDrag::None, "the gesture must still end");
        h.fixture.stop();
    }

    /// Abandoning a forwarded gesture tells the app the button came up. The
    /// attach loop calls this when the help overlay opens and when the client
    /// detaches; without it the app stays mid-drag for the pane's whole life.
    #[tokio::test]
    async fn abandoning_a_gesture_synthesizes_the_release_the_app_is_waiting_for() {
        let mut h = AppMouseHarness::new().await;
        let mut wr = seed_scrollback_pane(
            &h.fixture.io_state,
            h.fixture.first_pane,
            b"\x1b[?1002h\x1b[?1006h",
        )
        .await;
        h.send(MouseKind::Down, ProtoMouseButton::Left, 10, 5).await;
        assert_eq!(recv_text(&mut wr).await, "\x1b[<0;10;5M");
        h.send(MouseKind::Drag, ProtoMouseButton::Left, 14, 7).await;
        assert_eq!(recv_text(&mut wr).await, "\x1b[<32;14;7M");

        release_app_gesture(&h.fixture.io_state, &mut h.gesture).await;
        assert_eq!(
            recv_text(&mut wr).await,
            "\x1b[<0;14;7m",
            "the synthetic release must land where the drag left the pointer"
        );
        assert_eq!(h.gesture, SelectionDrag::None);
        // Idempotent: nothing left to release.
        release_app_gesture(&h.fixture.io_state, &mut h.gesture).await;
        assert_no_bytes(&mut wr, "a finished gesture releases once").await;
        h.fixture.stop();
    }

    /// Shift is the escape that keeps shux's selection reachable.
    #[tokio::test]
    async fn shift_hands_the_click_back_to_shux() {
        let mut h = AppMouseHarness::new().await;
        let mut wr = seed_scrollback_pane(
            &h.fixture.io_state,
            h.fixture.first_pane,
            b"\x1b[?1002h\x1b[?1006h",
        )
        .await;
        assert_eq!(
            h.send_mod(
                MouseKind::Down,
                ProtoMouseButton::Left,
                10,
                5,
                true,
                false,
                false
            )
            .await,
            AppMouse::NotHandled
        );
        assert_no_bytes(&mut wr, "shift reserves the mouse for shux").await;
        h.fixture.stop();
    }

    /// Ctrl and alt are the app's; nvim's `<C-LeftMouse>` is jump-to-tag, and
    /// arriving as a plain click turns that into a silent cursor move.
    #[tokio::test]
    async fn ctrl_and_alt_travel_with_the_click() {
        let mut h = AppMouseHarness::new().await;
        let mut wr = seed_scrollback_pane(
            &h.fixture.io_state,
            h.fixture.first_pane,
            b"\x1b[?1002h\x1b[?1006h",
        )
        .await;
        h.send_mod(
            MouseKind::Down,
            ProtoMouseButton::Left,
            10,
            5,
            false,
            false,
            true,
        )
        .await;
        assert_eq!(recv_text(&mut wr).await, "\x1b[<16;10;5M");
        h.fixture.stop();
    }

    /// The wheel keeps its own three-way routing (scrollback / arrows /
    /// forward). Stealing it here would bypass the transient wheel-initiated
    /// scrollback view entirely.
    #[tokio::test]
    async fn the_wheel_is_never_taken_from_handle_wheel() {
        let mut h = AppMouseHarness::new().await;
        let mut wr = seed_scrollback_pane(
            &h.fixture.io_state,
            h.fixture.first_pane,
            b"\x1b[?1002h\x1b[?1006h",
        )
        .await;
        for kind in [MouseKind::ScrollUp, MouseKind::ScrollDown, MouseKind::Move] {
            assert_eq!(
                h.send(kind, ProtoMouseButton::None, 10, 5).await,
                AppMouse::NotHandled,
                "{kind:?}"
            );
        }
        assert_no_bytes(&mut wr, "handle_app_mouse must not forward wheel ticks").await;

        // …and it still is not ours mid-gesture, which is the case the
        // press-only rule does not cover: a wheel tick during a drag would
        // otherwise be re-decided as motion and forwarded twice.
        h.send(MouseKind::Down, ProtoMouseButton::Left, 10, 5).await;
        assert_eq!(recv_text(&mut wr).await, "\x1b[<0;10;5M");
        for kind in [MouseKind::ScrollUp, MouseKind::ScrollDown] {
            assert_eq!(
                h.send(kind, ProtoMouseButton::None, 10, 5).await,
                AppMouse::NotHandled,
                "{kind:?} during a gesture"
            );
        }
        assert_no_bytes(&mut wr, "the wheel stays handle_wheel's, gesture or not").await;
        h.fixture.stop();
    }

    /// A border drag already in flight is shux's for the whole gesture, even
    /// once the pointer has moved off the border and over a mouse-aware pane.
    /// Otherwise the resize stalls after one cell and the app gets a phantom
    /// drag it never saw the press for.
    #[tokio::test]
    async fn a_border_resize_in_flight_is_not_hijacked_by_the_pane_it_drags_over() {
        let mut h = AppMouseHarness::new().await;
        let mut wr = seed_scrollback_pane(
            &h.fixture.io_state,
            h.fixture.first_pane,
            b"\x1b[?1002h\x1b[?1006h",
        )
        .await;
        h.border_drag = Some(DragState {
            target: h.fixture.first_pane,
            direction: shux_core::layout::Direction::Vertical,
            last_col: 40,
            last_row: 5,
        });
        for (kind, col) in [
            (MouseKind::Down, 40),
            (MouseKind::Drag, 12),
            (MouseKind::Up, 12),
        ] {
            assert_eq!(
                h.send(kind, ProtoMouseButton::Left, col, 5).await,
                AppMouse::NotHandled,
                "{kind:?}"
            );
        }
        assert_no_bytes(&mut wr, "a border resize keeps the whole gesture").await;
        h.fixture.stop();
    }

    /// Copy mode and the copy menu are shux's modes. The menu is the sharp
    /// case: it swallows every event until dismissed, so forwarding the click
    /// meant to dismiss it left it stuck on screen with no way out but a key.
    #[tokio::test]
    async fn copy_mode_and_the_copy_menu_keep_the_mouse() {
        for open_menu in [false, true] {
            let mut h = AppMouseHarness::new().await;
            let mut wr = seed_scrollback_pane(
                &h.fixture.io_state,
                h.fixture.first_pane,
                b"\x1b[?1002h\x1b[?1006h",
            )
            .await;
            {
                let mut s = h.session.lock().await;
                if open_menu {
                    s.copy_menu = Some(CopyContextMenu {
                        pane_id: h.fixture.first_pane,
                        col: 10,
                        row: 5,
                    });
                } else {
                    s.copy_mode = Some(shux_ui::CopyModeState::new());
                }
            }
            assert_eq!(
                h.send(MouseKind::Down, ProtoMouseButton::Left, 10, 5).await,
                AppMouse::NotHandled,
                "open_menu={open_menu}"
            );
            assert_no_bytes(&mut wr, "shux's own modes outrank the app").await;
            h.fixture.stop();
        }
    }

    /// A selection left over from before the app took the mouse can no longer
    /// be cleared by clicking, so the forward clears it — otherwise the
    /// highlight renders until the user happens to press a key.
    #[tokio::test]
    async fn forwarding_a_press_clears_a_stale_shux_selection() {
        let mut h = AppMouseHarness::new().await;
        let mut wr = seed_scrollback_pane(
            &h.fixture.io_state,
            h.fixture.first_pane,
            b"\x1b[?1002h\x1b[?1006h",
        )
        .await;
        {
            let mut s = h.session.lock().await;
            s.mouse_selection = Some(MouseSelection {
                pane_id: h.fixture.first_pane,
                state: shux_ui::CopyModeState::new(),
            });
        }
        assert_eq!(
            h.send(MouseKind::Down, ProtoMouseButton::Left, 10, 5).await,
            AppMouse::Consumed { redraw: true },
            "clearing the highlight is a visible change and must pulse a frame"
        );
        assert!(h.session.lock().await.mouse_selection.is_none());
        assert_eq!(recv_text(&mut wr).await, "\x1b[<0;10;5M");
        h.fixture.stop();
    }

    /// While zoomed the pane's rect is the whole content area, so without a
    /// containment check a click on the status bar clamps into the pane's last
    /// row and reaches the app as a click the user never made.
    #[tokio::test]
    async fn a_click_on_the_status_bar_never_reaches_a_zoomed_app() {
        let mut h = AppMouseHarness::new().await;
        let mut wr = seed_scrollback_pane(
            &h.fixture.io_state,
            h.fixture.first_pane,
            b"\x1b[?1002h\x1b[?1006h",
        )
        .await;
        assert!(
            h.fixture
                .graph
                .zoom_pane(h.fixture.first_pane, None)
                .await
                .expect("zoom"),
            "pane must actually be zoomed"
        );
        let (_, rows) = *h.client_size.lock().await;

        // Inside the zoomed pane: forwarded.
        h.send(MouseKind::Down, ProtoMouseButton::Left, 10, 5).await;
        assert_eq!(recv_text(&mut wr).await, "\x1b[<0;11;6M");
        h.send(MouseKind::Up, ProtoMouseButton::Left, 10, 5).await;
        assert_eq!(recv_text(&mut wr).await, "\x1b[<0;11;6m");

        // The status bar row is outside the content rect.
        assert_eq!(
            h.send(MouseKind::Down, ProtoMouseButton::Left, 10, rows - 1)
                .await,
            AppMouse::NotHandled
        );
        assert_no_bytes(&mut wr, "the status bar is not part of the pane").await;
        h.fixture.stop();
    }

    /// A press dropped by a backpressured pane must not open a gesture: the
    /// release that follows would reach the app with no press behind it.
    #[tokio::test]
    async fn a_dropped_press_does_not_open_a_gesture() {
        let mut h = AppMouseHarness::new().await;
        // A writer with no receiver: `try_send` fails exactly as it does when a
        // pane is backpressured.
        let (writer_tx, writer_rx) = mpsc::channel(1);
        drop(writer_rx);
        {
            let mut st = h.fixture.io_state.lock().await;
            let mut vt = shux_vt::VirtualTerminal::new(28, 49);
            vt.process(b"\x1b[?1002h\x1b[?1006h");
            st.vts.insert(h.fixture.first_pane, vt);
            st.writers.insert(h.fixture.first_pane, writer_tx);
        }
        h.send(MouseKind::Down, ProtoMouseButton::Left, 10, 5).await;
        assert_eq!(
            h.gesture,
            SelectionDrag::None,
            "a press the pane never received is not a gesture"
        );
        h.fixture.stop();
    }

    /// The three coordinate modes shux cannot encode stop forwarding outright
    /// rather than sending bytes the app decodes as another cell. 1016 is the
    /// one that matters: every click would collapse into the top-left corner.
    #[tokio::test]
    async fn an_app_in_a_coordinate_mode_shux_cannot_encode_gets_nothing() {
        for mode in [b"\x1b[?1016h".as_slice(), b"\x1b[?1005h", b"\x1b[?1015h"] {
            let mut h = AppMouseHarness::new().await;
            let mut setup = b"\x1b[?1002h\x1b[?1006h".to_vec();
            setup.extend_from_slice(mode);
            let mut wr =
                seed_scrollback_pane(&h.fixture.io_state, h.fixture.first_pane, &setup).await;
            assert_eq!(
                h.send(MouseKind::Down, ProtoMouseButton::Left, 10, 5).await,
                AppMouse::NotHandled,
                "{:?}",
                String::from_utf8_lossy(mode)
            );
            assert_no_bytes(&mut wr, "cell coordinates would be misread").await;
            h.fixture.stop();
        }
    }

    /// A pane whose app never asked for the mouse keeps shux's own handling —
    /// selection, click-to-focus, border drags all still work there.
    #[tokio::test]
    async fn a_pane_that_never_asked_for_the_mouse_is_untouched() {
        let mut h = AppMouseHarness::new().await;
        let mut wr = seed_scrollback_pane(&h.fixture.io_state, h.fixture.first_pane, b"").await;
        for kind in [MouseKind::Down, MouseKind::Drag, MouseKind::Up] {
            assert_eq!(
                h.send(kind, ProtoMouseButton::Left, 10, 5).await,
                AppMouse::NotHandled,
                "{kind:?}"
            );
        }
        assert_no_bytes(&mut wr, "no mouse tracking, no forwarding").await;
        h.fixture.stop();
    }

    /// A drag or release with no gesture in flight belongs to whatever shux
    /// started, or to nothing. Opening a gesture on one would hand the app a
    /// motion report with no press behind it — and shux would then swallow the
    /// release that belonged to its own selection.
    #[tokio::test]
    async fn a_drag_with_no_gesture_in_flight_is_not_the_apps() {
        let mut h = AppMouseHarness::new().await;
        let mut wr = seed_scrollback_pane(
            &h.fixture.io_state,
            h.fixture.first_pane,
            b"\x1b[?1002h\x1b[?1006h",
        )
        .await;
        for kind in [MouseKind::Drag, MouseKind::Up] {
            assert_eq!(
                h.send(kind, ProtoMouseButton::Left, 10, 5).await,
                AppMouse::NotHandled,
                "{kind:?} with nothing in flight"
            );
        }
        assert_no_bytes(&mut wr, "only a press opens a gesture").await;
        h.fixture.stop();
    }

    /// The hit-test viewport must be the one the compositor laid the panes out
    /// with. It used to inset for the outline unconditionally while the
    /// compositor inset only when the outline was drawn, so under
    /// `appearance.border_style = "none"` every click was one cell off in both
    /// axes and the last column and row could not be clicked at all.
    ///
    /// Driven from a real config FILE, not from a `BorderStyle` handed in: the
    /// defect that shipped was in the plumbing from config to hit-test, not in
    /// the inset arithmetic, and a test that passes the style in by hand cannot
    /// see it.
    #[tokio::test]
    async fn the_pane_hit_test_agrees_with_the_compositor_under_every_border_style() {
        let dir = tempfile::tempdir().expect("tempdir");
        let client_size: ClientSize = Arc::new(Mutex::new((100, 30)));
        let content = current_content_rect(&client_size).await;

        for (name, style) in [
            ("none", BorderStyle::None),
            ("thin", BorderStyle::Thin),
            ("thick", BorderStyle::Thick),
            ("double", BorderStyle::Double),
            ("rounded", BorderStyle::Rounded),
            ("ascii", BorderStyle::Ascii),
        ] {
            let path = dir.path().join(format!("{name}.toml"));
            std::fs::write(&path, format!("[appearance]\nborder_style = \"{name}\"\n"))
                .expect("write config");
            let config = ConfigHandle::load_or_default(&path);
            assert_eq!(
                current_viewport(&client_size, &config).await,
                shux_ui::pane_viewport(content, style, false),
                "border_style = {name:?}: the hit-test and the compositor must agree"
            );
        }

        // Concretely, and this is the pair that discriminates: with no outline
        // the pane starts at the origin and the last column and row are
        // reachable; with one it is inset by exactly a cell on every side.
        let none = dir.path().join("none.toml");
        assert_eq!(
            current_viewport(&client_size, &ConfigHandle::load_or_default(&none)).await,
            Rect::new(0, 0, 100, 29)
        );
        let rounded = dir.path().join("rounded.toml");
        assert_eq!(
            current_viewport(&client_size, &ConfigHandle::load_or_default(&rounded)).await,
            Rect::new(1, 1, 98, 27)
        );
    }

    /// Cross-path consistency: the places that decide where a pane's rect is
    /// must agree, for every border style.
    ///
    /// They were three independent copies of the same arithmetic and two were
    /// wrong: mouse hit-testing (`current_viewport`) and the PTY resize fan-out
    /// (`apply_resize_to_window`) both inset for the outline unconditionally,
    /// while the compositor and the snapshot composer inset only when the
    /// outline is drawn. Under `border_style = "none"` that made every click
    /// land a cell off AND gave each pane a VT grid two columns narrower than
    /// the rect it was drawn into, so a click on the last column named a cell
    /// the app did not have.
    ///
    /// Asserted through the real APIs on each path -- where `compose` actually
    /// lands a pane's first cell, what the resize fan-out actually sends -- not
    /// by re-deriving the arithmetic and comparing it to itself.
    #[tokio::test]
    async fn every_render_path_agrees_on_the_pane_viewport() {
        use std::collections::HashMap;

        let dir = tempfile::tempdir().expect("tempdir");
        let fixture = attach_fixture().await;
        let client_size: ClientSize = Arc::new(Mutex::new((100, 30)));
        let content = current_content_rect(&client_size).await;

        // Each pane's grid carries a distinct glyph in its own (0,0), so where
        // that glyph lands in the composed frame IS the pane's origin.
        let marks = [(fixture.first_pane, 'A'), (fixture.second_pane, 'B')];
        let mut vts: HashMap<PaneId, shux_vt::VirtualTerminal> = HashMap::new();
        for (pid, mark) in marks {
            let mut vt = shux_vt::VirtualTerminal::new(30, 100);
            vt.process(mark.to_string().repeat(200).as_bytes());
            vts.insert(pid, vt);
        }

        for name in ["none", "rounded", "thick", "ascii"] {
            let path = dir.path().join(format!("{name}.toml"));
            std::fs::write(&path, format!("[appearance]\nborder_style = \"{name}\"\n"))
                .expect("write config");
            let config = ConfigHandle::load_or_default(&path);
            let style = BorderStyle::parse(name);
            let expected = shux_ui::pane_viewport(content, style, false);

            // 1. The live-attach hit-test.
            assert_eq!(
                current_viewport(&client_size, &config).await,
                expected,
                "{name}: hit-test viewport"
            );

            // 2. The snapshot / web-preview composer.
            let snap = fixture.graph.snapshot();
            let win = snap.windows.get(&fixture.first_window).expect("window");
            let rects: HashMap<PaneId, Rect> = win
                .layout
                .tree
                .compute_rects(expected)
                .into_iter()
                .collect();
            let panes: HashMap<PaneId, (&shux_vt::Grid, &shux_vt::Cursor)> = vts
                .iter()
                .map(|(pid, vt)| (*pid, (vt.grid(), vt.cursor())))
                .collect();
            let composed = shux_ui::compose(
                &shux_ui::ComposeInputs {
                    layout: &win.layout.tree,
                    zoom: None,
                    focused: fixture.first_pane,
                    panes: &panes,
                    titles: None,
                    status_bar: None,
                },
                100,
                30,
                style,
                shux_ui::BorderColors::default(),
                STATUS_BAR_ROWS,
            );
            for (pid, mark) in marks {
                let rect = rects.get(&pid).expect("pane rect");
                let cell = composed
                    .grid
                    .visible_row(rect.y as usize)
                    .get(rect.x as usize)
                    .expect("composed cell");
                assert_eq!(
                    cell.ch, mark,
                    "{name}: pane {pid}'s first cell is not at the rect every \
                     other path uses ({}, {})",
                    rect.x, rect.y
                );
            }
            drop(snap);

            // 3. The PTY resize fan-out: each pane must be TOLD the size of the
            //    rect it is DRAWN in, or a click on its last column names a
            //    cell the app does not have.
            let mut receivers = Vec::new();
            {
                let mut state = fixture.io_state.lock().await;
                state.resizers.clear();
                for (pid, _) in marks {
                    let (tx, rx) = mpsc::channel(8);
                    state.resizers.insert(pid, tx);
                    receivers.push((pid, rx));
                }
            }
            apply_resize_to_window(
                &fixture.graph,
                &fixture.io_state,
                &fixture.attached,
                &config,
                100,
                30,
            )
            .await;
            for (pid, mut rx) in receivers {
                let req = tokio::time::timeout(Duration::from_secs(1), rx.recv())
                    .await
                    .expect("resize request")
                    .expect("resize request");
                let rect = rects.get(&pid).expect("pane rect");
                assert_eq!(
                    (req.size.cols, req.size.rows),
                    (rect.width, rect.height),
                    "{name}: pane {pid} was told a size that is not the rect it is drawn in"
                );
            }
        }
        fixture.stop();
    }

    // --- Task 086: mouse wheel behavioral regression tests ---
    // These drive the real `handle_wheel` against a live graph + pane VT. Each
    // asserts an outcome the pre-fix code could NOT produce (the old scroll arm
    // was `=> {}` — no scrollback move, no PTY write). Proven red by neutering
    // `handle_wheel` to a no-op, then green with the fix in place.

    #[tokio::test]
    async fn handle_wheel_enters_scrollback_on_primary_screen() {
        let fixture = attach_fixture().await;
        let _wr = seed_scrollback_pane(&fixture.io_state, fixture.first_pane, b"").await;
        let session = Arc::new(Mutex::new(fixture.attached.clone()));
        let client_size = Arc::new(Mutex::new((100, 30)));
        assert!(session.lock().await.copy_mode.is_none());

        let consumed = handle_wheel(
            MouseKind::ScrollUp,
            3,
            3,
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &test_config(),
        )
        .await
        .expect("wheel up");

        assert!(consumed, "wheel event must be consumed");
        let s = session.lock().await;
        let cm = s
            .copy_mode
            .as_ref()
            .expect("primary-screen wheel-up must enter scrollback");
        assert!(
            cm.scroll_offset > 0,
            "scroll_offset must advance, got {}",
            cm.scroll_offset
        );
        drop(s);
        fixture.stop();
    }

    #[tokio::test]
    async fn handle_wheel_forwards_encoded_report_to_mouse_aware_app() {
        let fixture = attach_fixture().await;
        // App turns on SGR mouse tracking (like vim :set mouse=a / htop).
        let mut wr = seed_scrollback_pane(
            &fixture.io_state,
            fixture.first_pane,
            b"\x1b[?1000h\x1b[?1006h",
        )
        .await;
        let session = Arc::new(Mutex::new(fixture.attached.clone()));
        let client_size = Arc::new(Mutex::new((100, 30)));

        handle_wheel(
            MouseKind::ScrollUp,
            3,
            3,
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &test_config(),
        )
        .await
        .expect("wheel");

        let bytes = recv_bytes(&mut wr).await;
        let s = String::from_utf8_lossy(&bytes);
        assert!(
            s.starts_with("\x1b[<64;"),
            "expected SGR wheel-up, got {s:?}"
        );
        assert!(s.ends_with('M'), "SGR wheel is press-only ('M'), got {s:?}");
        assert!(
            session.lock().await.copy_mode.is_none(),
            "forwarding to the app must NOT enter shux scrollback"
        );
        fixture.stop();
    }

    #[tokio::test]
    async fn handle_wheel_translates_to_arrows_on_alt_screen_without_mouse() {
        let fixture = attach_fixture().await;
        // Alt-screen app that did NOT request the mouse (like less / man).
        let mut wr =
            seed_scrollback_pane(&fixture.io_state, fixture.first_pane, b"\x1b[?1049h").await;
        let session = Arc::new(Mutex::new(fixture.attached.clone()));
        let client_size = Arc::new(Mutex::new((100, 30)));

        handle_wheel(
            MouseKind::ScrollDown,
            3,
            3,
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &test_config(),
        )
        .await
        .expect("wheel");

        let bytes = recv_bytes(&mut wr).await;
        assert_eq!(
            bytes, b"\x1b[B\x1b[B\x1b[B",
            "one wheel-down tick maps to three down-arrows on the alt screen"
        );
        assert!(session.lock().await.copy_mode.is_none());
        fixture.stop();
    }

    // Regression (found by adversarial agent "Mudrārākṣasa"): once the wheel
    // enters scrollback, copy mode is active, so every later wheel event is
    // consumed by `handle_copy_mode_mouse` — NOT `handle_wheel`. The exit-at-
    // bottom logic must therefore live in that path too, or wheel-down returns
    // to the live view but leaves the session stuck in copy mode (keyboard
    // hijacked until `q`). This drives the real two-handler integration path.
    #[tokio::test]
    async fn wheel_initiated_scrollback_exits_when_wheeled_back_to_bottom() {
        let fixture = attach_fixture().await;
        let _wr = seed_scrollback_pane(&fixture.io_state, fixture.first_pane, b"").await;
        let session = Arc::new(Mutex::new(fixture.attached.clone()));
        let client_size = Arc::new(Mutex::new((100, 30)));
        let (out_tx, _out_rx) = mpsc::channel(8);
        let mut drag = SelectionDrag::None;

        // Wheel-up opens a wheel-initiated scrollback view (via handle_wheel).
        handle_wheel(
            MouseKind::ScrollUp,
            3,
            3,
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &test_config(),
        )
        .await
        .expect("wheel up");
        {
            let s = session.lock().await;
            let cm = s.copy_mode.as_ref().expect("wheel-up enters scrollback");
            assert!(cm.wheel_initiated, "must be flagged wheel-initiated");
            assert!(cm.scroll_offset > 0);
        }

        // Copy mode is now active, so wheel-down flows through the OTHER handler.
        for _ in 0..40 {
            handle_copy_mode_mouse(
                MouseKind::ScrollDown,
                ProtoMouseButton::None,
                3,
                3,
                &fixture.graph,
                &fixture.io_state,
                &session,
                &client_size,
                &test_config(),
                &out_tx,
                &mut drag,
            )
            .await
            .expect("copy-mode wheel down");
        }

        assert!(
            session.lock().await.copy_mode.is_none(),
            "wheel-down to the live bottom must exit wheel-initiated scrollback \
             (else the keyboard stays hijacked)"
        );
        fixture.stop();
    }

    #[tokio::test]
    async fn manual_copy_mode_survives_wheel_back_to_bottom() {
        // The counterpart guard: a copy mode the user opened deliberately
        // (Prefix [ / API — NOT wheel-initiated) must NOT auto-exit on a wheel,
        // so a scroll never discards an in-progress selection/search.
        let fixture = attach_fixture().await;
        let _wr = seed_scrollback_pane(&fixture.io_state, fixture.first_pane, b"").await;
        let mut attached = fixture.attached.clone();
        attached.copy_mode = Some(shux_ui::CopyModeState::new()); // wheel_initiated = false
        let session = Arc::new(Mutex::new(attached));
        let client_size = Arc::new(Mutex::new((100, 30)));
        let (out_tx, _out_rx) = mpsc::channel(8);
        let mut drag = SelectionDrag::None;

        handle_copy_mode_mouse(
            MouseKind::ScrollUp,
            ProtoMouseButton::None,
            3,
            3,
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &test_config(),
            &out_tx,
            &mut drag,
        )
        .await
        .expect("copy-mode wheel up");
        for _ in 0..40 {
            handle_copy_mode_mouse(
                MouseKind::ScrollDown,
                ProtoMouseButton::None,
                3,
                3,
                &fixture.graph,
                &fixture.io_state,
                &session,
                &client_size,
                &test_config(),
                &out_tx,
                &mut drag,
            )
            .await
            .expect("copy-mode wheel down");
        }

        assert!(
            session.lock().await.copy_mode.is_some(),
            "manually-entered copy mode must survive a wheel scroll to the bottom"
        );
        fixture.stop();
    }

    // Regression (Greptile P1 on PR #101): wheel scroll must target the pane
    // under the pointer. A wheel-opened (transient) scrollback on pane A made
    // copy mode session-active, and `handle_copy_mode_mouse` — dispatched before
    // `handle_wheel` — used to consume EVERY later wheel against A's active
    // pane, ignoring `col`/`row`. So wheeling over pane B kept scrolling A while
    // B received nothing. The fix releases a transient scrollback when the
    // pointer moves to another pane, so the dispatch falls through to
    // `handle_wheel`, which scrolls the pane actually under the cursor.
    #[tokio::test]
    async fn transient_wheel_scrollback_releases_wheel_to_pane_under_pointer() {
        let fixture = attach_fixture().await;
        let _wr_a = seed_scrollback_pane(&fixture.io_state, fixture.first_pane, b"").await;
        let _wr_b = seed_scrollback_pane(&fixture.io_state, fixture.second_pane, b"").await;
        let session = Arc::new(Mutex::new(fixture.attached.clone()));
        let client_size = Arc::new(Mutex::new((100, 30)));
        let (out_tx, _out_rx) = mpsc::channel(8);
        let mut drag = SelectionDrag::None;
        let viewport = current_viewport(&client_size, &test_config()).await;
        let (first_point, second_point, _border) =
            find_pane_and_border_points(&fixture.graph, fixture.first_window, viewport);

        // Wheel-up over pane A opens a transient, wheel-initiated scrollback on A.
        handle_wheel(
            MouseKind::ScrollUp,
            first_point.0,
            first_point.1,
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &test_config(),
        )
        .await
        .expect("wheel up over pane A");
        {
            let s = session.lock().await;
            let cm = s.copy_mode.as_ref().expect("A enters scrollback");
            assert!(cm.wheel_initiated, "must be wheel-initiated (transient)");
            assert_eq!(s.active_pane_id, fixture.first_pane);
        }

        // Now the pointer is over pane B. The copy-mode handler runs first; it
        // must NOT consume the wheel (returns false) and must release A's
        // transient scrollback so the dispatch can route to B.
        let consumed = handle_copy_mode_mouse(
            MouseKind::ScrollUp,
            ProtoMouseButton::None,
            second_point.0,
            second_point.1,
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &test_config(),
            &out_tx,
            &mut drag,
        )
        .await
        .expect("copy-mode wheel over pane B");
        assert!(
            !consumed,
            "a transient scrollback must release the wheel when the pointer is over another pane"
        );
        assert!(
            session.lock().await.copy_mode.is_none(),
            "A's transient scrollback must be dismissed before routing to B"
        );

        // The real dispatch now falls through to `handle_wheel`, which scrolls
        // the pane under the cursor (B) and focuses it.
        handle_wheel(
            MouseKind::ScrollUp,
            second_point.0,
            second_point.1,
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &test_config(),
        )
        .await
        .expect("wheel up over pane B");
        let s = session.lock().await;
        assert_eq!(
            s.active_pane_id, fixture.second_pane,
            "the pane under the pointer must scroll and take focus"
        );
        assert!(
            s.copy_mode.as_ref().is_some_and(|cm| cm.scroll_offset > 0),
            "B must now be in its own scrollback view"
        );
        drop(s);
        fixture.stop();
    }

    // Counterpart guard: a DELIBERATE copy mode (Prefix [ / API — not
    // wheel-initiated) must keep the wheel even when the pointer is over another
    // pane, so a stray scroll elsewhere never discards an in-progress selection.
    #[tokio::test]
    async fn deliberate_copy_mode_keeps_wheel_when_pointer_over_other_pane() {
        let fixture = attach_fixture().await;
        let _wr_a = seed_scrollback_pane(&fixture.io_state, fixture.first_pane, b"").await;
        let _wr_b = seed_scrollback_pane(&fixture.io_state, fixture.second_pane, b"").await;
        let mut attached = fixture.attached.clone();
        attached.copy_mode = Some(shux_ui::CopyModeState::new()); // wheel_initiated = false
        let session = Arc::new(Mutex::new(attached));
        let client_size = Arc::new(Mutex::new((100, 30)));
        let (out_tx, _out_rx) = mpsc::channel(8);
        let mut drag = SelectionDrag::None;
        let viewport = current_viewport(&client_size, &test_config()).await;
        let (_first_point, second_point, _border) =
            find_pane_and_border_points(&fixture.graph, fixture.first_window, viewport);

        let consumed = handle_copy_mode_mouse(
            MouseKind::ScrollUp,
            ProtoMouseButton::None,
            second_point.0,
            second_point.1,
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &test_config(),
            &out_tx,
            &mut drag,
        )
        .await
        .expect("copy-mode wheel over pane B");

        assert!(
            consumed,
            "a deliberate copy mode keeps the wheel regardless of pointer pane"
        );
        let s = session.lock().await;
        assert!(
            s.copy_mode.is_some(),
            "a deliberate selection must not be discarded by scrolling elsewhere"
        );
        assert_eq!(
            s.active_pane_id, fixture.first_pane,
            "focus must not jump panes while a deliberate copy mode is open"
        );
        drop(s);
        fixture.stop();
    }
}
