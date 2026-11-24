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
use tokio::sync::{Mutex, mpsc, oneshot, watch};
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
use shux_ui::viewport::PaneViewport;
use shux_ui::{BorderStyle, CompositorConfig, MultiPaneFrame, RenderCompositor};

use crate::pane_input::{InputPermit, Lane, PaneWriter};
use crate::pane_io::{OpenPaste, PASTE_END, PASTE_START, PaneIoState};
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
    theme: Theme,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CopyOverlayKind {
    /// Copy mode as of this frame, and whether its view is drawn over the
    /// pane's own cells.
    Modal(shux_ui::CopyModeState, bool),
    /// Over the slice the selection was made on.
    MouseSelection(PaneViewport, shux_ui::Selection),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MouseSelection {
    pane_id: PaneId,
    state: shux_ui::Selection,
    /// The slice the frame showed when the selection started; it ends when
    /// the pane shows another.
    view: PaneViewport,
    /// Tells this selection from a later one over the same slice.
    generation: u64,
}

fn next_selection_generation() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
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

/// What a sheet drawn over the frame (help, welcome toast, copy menu) looked
/// like. The terminal keeps it until bytes land under it, so it is sent again
/// only when it changed or the frame beneath it sent something.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SheetStamp {
    cols: u16,
    rows: u16,
    theme: Theme,
    label: String,
}

fn sheet_due(last: &mut Option<SheetStamp>, next: SheetStamp, base_emitted: bool) -> bool {
    let due = base_emitted || last.as_ref() != Some(&next);
    *last = Some(next);
    due
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

/// How often the daemon pings the client to detect dead peers.
const PING_INTERVAL: Duration = Duration::from_secs(15);
/// How often a blocked write checks that the client took bytes; a whole
/// check with none and it is gone, 10–20 s after its last byte. A stopped
/// reader must not hold its connection, a slow one must not lose it.
const WRITE_STALL: Duration = Duration::from_secs(10);

/// How often a render loop redraws with no pulse, so clocks and status
/// segments move on an idle screen.
const FALLBACK_TICK: Duration = Duration::from_millis(200);
/// Overrides [`FALLBACK_TICK`], in milliseconds. A test that must see a frame
/// caused by a pulse, never by the tick, sets it far past its own budget.
pub const FALLBACK_TICK_ENV: &str = "SHUX_ATTACH_FALLBACK_TICK_MS";

fn fallback_tick() -> Duration {
    std::env::var(FALLBACK_TICK_ENV)
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&ms: &u64| ms > 0)
        .map_or(FALLBACK_TICK, Duration::from_millis)
}

const ESC: u8 = 0x1b;

/// A pane's open paste, as one piece of input sees it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Open {
    Closed,
    /// Open, and not the paste this piece belongs to.
    Other,
    /// Open, and this piece is its next.
    Continued,
}

/// Input as the pane should receive it, and whether a bracketed paste is open
/// once it has. `paste` is `Some(more)` for a piece of a paste, `more` set
/// while pieces follow. Anything but the next piece of an open paste ends it
/// first. A paste goes between the markers when the pane asked for them
/// (mode 2004) as it began. ESC is removed from bracketed text, so no part of it can end
/// the paste.
fn frame_input(
    bytes: Vec<u8>,
    paste: Option<bool>,
    open: Open,
    wants_brackets: bool,
) -> (Vec<u8>, bool) {
    let continues = open == Open::Continued;
    let mut out = Vec::with_capacity(bytes.len() + PASTE_START.len() + 2 * PASTE_END.len());
    if open != Open::Closed && !continues {
        out.extend_from_slice(PASTE_END);
    }
    let Some(more) = paste.filter(|_| continues || wants_brackets) else {
        out.extend(bytes);
        return (out, false);
    };
    if !continues {
        out.extend_from_slice(PASTE_START);
    }
    out.extend(bytes.into_iter().filter(|&b| b != ESC));
    if !more {
        out.extend_from_slice(PASTE_END);
    }
    (out, more)
}

/// Frames `bytes` against the pane's paste state as it stands under this
/// lock, sends them through `permit` and records the paste they leave open.
/// `paste` names the paste a piece belongs to.
fn send_input(
    state: &mut PaneIoState,
    pane: PaneId,
    permit: InputPermit,
    bytes: Vec<u8>,
    paste: Option<(bool, u64)>,
    client: u64,
) {
    if !state.is_current_writer(pane, &permit) {
        permit.send(bytes);
        return;
    }
    let (program, wants_brackets) = state.paste_target(pane);
    // The next piece of a paste continues it whatever mode the program shows
    // now; anything else ends it only for a program still in paste mode.
    let continued = state.open_pastes.get(&pane).is_some_and(|open| {
        open.program == program
            && paste.is_some_and(|(_, id)| (open.client, open.paste) == (client, id))
    });
    let open = match state.open_paste(pane) {
        _ if continued => Open::Continued,
        None => Open::Closed,
        Some(_) => Open::Other,
    };
    let (bytes, open_after) = frame_input(bytes, paste.map(|(more, _)| more), open, wants_brackets);
    permit.send(bytes);
    match paste {
        Some((_, paste)) if open_after => {
            let open = OpenPaste {
                program,
                client,
                paste,
            };
            state.open_pastes.insert(pane, open);
        }
        _ => {
            state.open_pastes.remove(&pane);
        }
    }
}

/// Ends every paste this client left open, as a held button is released: no
/// later key from this client will.
async fn close_open_pastes(io_state: &Arc<Mutex<PaneIoState>>, client: u64) {
    let mut state = io_state.lock().await;
    let mine: Vec<PaneId> = state
        .open_pastes
        .keys()
        .copied()
        .filter(|&pane| {
            state
                .open_paste(pane)
                .is_some_and(|open| open.client == client)
        })
        .collect();
    for pane in mine {
        let Some(tx) = state.writers.get(&pane).cloned() else {
            continue;
        };
        // Urgent input fails only once the pane is gone.
        if let Ok(permit) = tx.try_reserve_in(PASTE_END.len(), crate::pane_input::Lane::Urgent) {
            state.open_pastes.remove(&pane);
            permit.send(PASTE_END.to_vec());
        }
    }
}

fn byte_count(n: usize) -> String {
    match n {
        n if n < 1 << 10 => format!("{n} B"),
        n if n < 1 << 20 => format!("{:.1} KiB", n as f64 / f64::from(1u32 << 10)),
        n => format!("{:.1} MiB", n as f64 / f64::from(1u32 << 20)),
    }
}

/// How long, in total, input may wait on a pane that is not keeping up
/// before input for it is dropped. The total resets each time the pane has
/// room the moment input arrives, so one slow or stuck pane holds the attach
/// loop (and with it detach and pane switching) for at most this long at a
/// time.
const INPUT_STALL: Duration = Duration::from_secs(1);

/// Names each attach connection, for the pastes it leaves open.
static NEXT_CLIENT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Room for one chunk of input bound for a pane, and how long the attach
/// loop waited for it.
enum Room {
    Ready(InputPermit, Duration),
    /// The pane had no room within the wait it was allowed.
    Full(Duration),
    /// The pane is gone, or the attach is ending.
    Closed,
}

/// One pane's share of [`INPUT_STALL`] spent since it last kept up, and the
/// input dropped for it over the whole attach.
#[derive(Default)]
struct InputBacklog {
    waited: Duration,
    dropped: usize,
}

/// Room for `len` bytes in a pane's input channel, waiting at most `budget`
/// for it.
async fn reserve_input(
    tx: &PaneWriter,
    len: usize,
    budget: Duration,
    cancel: &CancellationToken,
) -> Room {
    match tx.try_reserve(len) {
        Ok(permit) => return Room::Ready(permit, Duration::ZERO),
        Err(mpsc::error::TrySendError::Closed(_)) => return Room::Closed,
        Err(mpsc::error::TrySendError::Full(_)) if budget.is_zero() => {
            return Room::Full(Duration::ZERO);
        }
        Err(mpsc::error::TrySendError::Full(_)) => {}
    }
    let started = tokio::time::Instant::now();
    tokio::select! {
        _ = cancel.cancelled() => Room::Closed,
        reserved = tokio::time::timeout(budget, tx.reserve(len)) => match reserved {
            Ok(Ok(permit)) => Room::Ready(permit, started.elapsed()),
            Ok(Err(_)) => Room::Closed,
            Err(_) => Room::Full(started.elapsed()),
        },
    }
}

/// Run the attach UDS listener. Each accepted connection spawns an
/// independent attach session task. Runs until `cancel` fires.
#[expect(clippy::too_many_arguments, reason = "independent inputs")]
pub async fn run_attach_server(
    socket_path: std::path::PathBuf,
    graph: GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    config: ConfigHandle,
    segments: SegmentCache,
    meta_cache: crate::session_meta::SessionMetaCache,
    onboarding: crate::onboarding::OnboardingHandle,
    daemon_start: std::time::Instant,
    rebind_gate: shux_rpc::uds::RebindGate,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let mut socket = shux_rpc::uds::OwnedSocket::bind(&socket_path)?;
    // Losing only the attach socket leaves the daemon serving everything else.
    let mut lost = false;
    let mut socket_check = tokio::time::interval(shux_rpc::uds::CHECK_INTERVAL);
    socket_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    info!(path = %socket_path.display(), "attach UDS listener bound");

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("attach server shutting down");
                break;
            }
            _ = socket_check.tick(), if !lost => {
                if let Err(e) = socket.check(Some(&rebind_gate)) {
                    warn!(error = %e, "attach socket lost");
                    lost = true;
                }
            }
            res = socket.listener().accept() => {
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

/// A client connection's bytes: the attach socket in the daemon, an
/// in-memory pipe in tests, where nothing in flight hides from the scheduler.
trait AttachStream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static {}

impl<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static> AttachStream for S {}

/// Handle one attach connection: handshake, then run the streaming loop.
#[expect(clippy::too_many_arguments, reason = "independent inputs")]
async fn handle_attach_connection(
    stream: impl AttachStream,
    graph: GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    config: ConfigHandle,
    segments: SegmentCache,
    meta_cache: crate::session_meta::SessionMetaCache,
    onboarding: crate::onboarding::OnboardingHandle,
    daemon_start: std::time::Instant,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let (stream, taken) = crate::write_progress::Metered::new(stream);
    let mut framed = Framed::new(stream, create_codec());

    // Step 1: Receive the hello frame, bounded so a hung peer cannot hold
    // the connection forever.
    let patience = crate::patience::peer_patience();
    let first = match tokio::time::timeout(patience, framed.next()).await {
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
    // Listed before it is told it is attached, so it can find itself.
    let removed = CancellationToken::new();
    let (cols, rows) = clamp_client_dims(hello.cols, hello.rows);
    let clients = io_state.lock().await.clients.clone();
    let registered = clients.register(
        crate::clients::ClientInfo {
            id: shux_core::event::ClientId::new(),
            session_id: session.session_id,
            cols,
            rows,
            attached_at: std::time::SystemTime::now(),
            votes: true,
            last_input: None,
        },
        removed.clone(),
    );
    // Panes take the driver's size: this client's only if it is the first.
    reconcile_session(&graph, &io_state, &config, session.session_id).await;

    // Step 3: Send AttachReady::Ok.
    let ready = AttachReady::Ok {
        session_id: session.session_id.to_string(),
        session_name: session.name.clone(),
        active_window_id: session.active_window_id.to_string(),
        active_pane_id: session.active_pane_id.to_string(),
        protocol: ATTACH_PROTOCOL_VERSION,
        plain: hello.plain,
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
        taken,
        (registered, removed),
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
    /// Why the last action failed, for the status bar. Cleared by the next
    /// action or keystroke, so it stays up until the user has moved on.
    action_error: Option<String>,
    /// The prefix the client reported with `AttachClientFrame::Prefix`.
    /// Hints name it; `None` means the daemon config's prefix.
    client_prefix: Option<String>,
    /// True until the welcome toast has been up for its dwell (~3s) or a
    /// key or action ends it, as the toast promises. Either way
    /// `welcome_toast_seen: true` is persisted so the next attach skips it.
    show_welcome_toast: bool,
    /// What each pane showed in the last few frames sent to this client:
    /// mouse input names cells of the frame the client had written when it
    /// sent the event, not of one drawn since.
    shown: ShownFrames,
    /// This attach's entry in the client registry, once registered.
    client: Option<shux_core::event::ClientId>,
    /// One of the client's cells in pixels, once it has said.
    cell_pixels: Option<(u16, u16)>,
}

/// Frames queued to one client before the render loop waits for it.
const OUTBOUND_FRAMES: usize = 64;

/// What one frame showed: each pane's slice, and copy mode's view when it
/// was drawing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Shown {
    views: HashMap<PaneId, PaneViewport>,
    /// Where each pane's lines stood: the rows a view names move with output.
    drawn: HashMap<PaneId, shux_ui::Drawn>,
    modal: Option<(PaneId, shux_ui::CopyView)>,
}

impl Shown {
    /// What this frame showed of `pane`, and where its lines stood.
    fn of(&self, pane: PaneId) -> Option<(&PaneViewport, shux_ui::Drawn)> {
        Some((self.views.get(&pane)?, *self.drawn.get(&pane)?))
    }
}

/// What the latest frames sent to one client showed, oldest first, each
/// under the number its `Render { frame }` carried.
#[derive(Debug, Clone, Default)]
struct ShownFrames(std::collections::VecDeque<(u64, Arc<Shown>)>);

impl ShownFrames {
    /// Frames a client can trail the daemon by and still be mapped: the
    /// outbound queue twice over. The socket's buffers hold bytes, not frames,
    /// so small frames can still outrun this; a stamp past it falls back to
    /// shux's own handling at the live layout (`handle_app_mouse`).
    const KEPT: usize = 2 * OUTBOUND_FRAMES;

    fn push(&mut self, frame: u64, shown: Shown) {
        if self.0.len() == Self::KEPT {
            self.0.pop_front();
        }
        self.0.push_back((frame, Arc::new(shown)));
    }

    /// What the client saw when it had written `frame`: the newest frame not
    /// after it. `None` (a client that predates frame numbers) is the latest
    /// frame sent. A stamp older than every frame kept names nothing: any
    /// kept frame is newer than what that client shows.
    fn shown(&self, frame: Option<u64>) -> Option<&Shown> {
        let entry = match frame {
            None => self.0.back(),
            Some(f) => self.0.iter().rev().find(|(n, _)| *n <= f),
        };
        entry.map(|(_, shown)| shown.as_ref())
    }

    fn views(&self, frame: Option<u64>) -> Option<&HashMap<PaneId, PaneViewport>> {
        self.shown(frame).map(|s| &s.views)
    }
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
            client_prefix: None,
            action_error: None,
            // Whether the toast actually renders is decided at attach
            // time by reading the onboarding state file; this stays
            // true here so the render loop can flip it off after dwell.
            show_welcome_toast: true,
            shown: ShownFrames::default(),
            client: None,
            cell_pixels: None,
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
        client_prefix: None,
        action_error: None,
        show_welcome_toast: true,
        shown: ShownFrames::default(),
        client: None,
        cell_pixels: None,
    })
}

/// Compute per-pane rects given the client size and dispatch each PTY its real
/// winsize. Multi-pane TUIs read `TIOCGWINSZ` and lay themselves out from it, so
/// a pane whose PTY believes a different size than the rect it is drawn in
/// renders into the wrong shape. Uses [`shux_ui::pane_viewport`], the
/// compositor's own rule, so the two cannot diverge.
async fn apply_resize_to_window(
    graph: &GraphHandle,
    io_state: &Arc<Mutex<PaneIoState>>,
    window_id: WindowId,
    config: &ConfigHandle,
    cols: u16,
    rows: u16,
) -> bool {
    let snap = graph.snapshot();
    let Some(win) = snap.windows.get(&window_id) else {
        return false;
    };
    let border = BorderStyle::parse(&config.current().appearance.border_style);
    let sizes = window_sizes(&win.layout, border, cols, rows);
    let mut state = io_state.lock().await;
    let mut resized = false;
    for (pid, size) in sizes {
        if !needs_size(&state, pid, size) {
            continue;
        }
        let Some(tx) = state.resizers.get(&pid).cloned() else {
            continue;
        };
        // Never wait on a pane's queue: a backpressured pane would stall
        // every session's sizing. A full queue retries once it has room.
        match tx.try_send(crate::pane_io::ResizeRequest { size, ack: None }) {
            Ok(()) => {
                state.requested_sizes.insert(pid, size);
                resized = true;
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                let clients = state.clients.clone();
                tokio::spawn(async move {
                    if tx.reserve().await.is_ok() {
                        clients.touch();
                    }
                });
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {}
        }
    }
    resized
}

/// Each shown pane's size in a window laid out for a `cols`x`rows` client.
/// Zoomed: the zoomed pane takes the full content area; the hidden ones keep
/// their sizes, so their programs see no SIGWINCH for a zoom they cannot see.
/// Sizes are floored: `rows <= 1` once reached a PTY as height 0 (issue #107).
fn window_sizes(
    layout: &shux_core::layout::WindowLayout,
    border: BorderStyle,
    cols: u16,
    rows: u16,
) -> Vec<(PaneId, PtySize)> {
    let (cols, rows) = clamp_client_dims(cols, rows);
    let content = Rect::new(0, 0, cols, rows.saturating_sub(STATUS_BAR_ROWS));
    let floor = |w: u16, h: u16| PtySize::new(w.max(MIN_PANE_COLS), h.max(MIN_PANE_ROWS));
    if layout.is_zoomed() {
        let size = floor(content.width, content.height);
        return layout.zoom.iter().map(|z| (z.zoomed_pane, size)).collect();
    }
    layout
        .tree
        .compute_rects(shux_ui::pane_viewport(content, border, false))
        .into_iter()
        .map(|(pid, rect)| (pid, floor(rect.width, rect.height)))
        .collect()
}

/// The size a pane about to spawn should start at: its rect for its session's
/// driver, as last sized. None when nothing sizes it (no driver yet, or pinned).
pub(crate) async fn spawn_size(
    graph: &GraphHandle,
    io_state: &Arc<Mutex<PaneIoState>>,
    pane: PaneId,
) -> Option<PtySize> {
    let snap = graph.snapshot();
    let window = snap.windows.get(&snap.panes.get(&pane)?.window_id)?;
    let state = io_state.lock().await;
    // The last sizing outlives its driver, for snapshots; only a client
    // still attached sizes a new pane.
    if state.clients.is_pinned(pane) {
        return None;
    }
    // The client that drives now, which may not have sized anything yet: one
    // that just reconnected must not start a pane at its predecessor's size.
    let driver = state.clients.driver(window.session_id)?;
    let border = crate::pane_spawn::daemon_border_style().or_else(|| {
        state
            .layout_basis
            .get(&window.session_id)
            .map(|b| b.2.clone())
    })?;
    window_sizes(
        &window.layout,
        BorderStyle::parse(&border),
        driver.cols,
        driver.rows,
    )
    .into_iter()
    .find_map(|(pid, size)| (pid == pane).then_some(size))
}

/// A pane takes the layout's size unless it is pinned or was last asked for
/// that size; a queued request counts, so a correction is never skipped.
fn needs_size(state: &PaneIoState, pane: PaneId, size: PtySize) -> bool {
    let want = (usize::from(size.cols), usize::from(size.rows));
    let have = match state.requested_sizes.get(&pane) {
        Some(s) => Some((usize::from(s.cols), usize::from(s.rows))),
        None => state
            .vts
            .get(&pane)
            .map(|vt| (vt.grid().cols(), vt.grid().rows())),
    };
    !state.clients.is_pinned(pane) && have != Some(want)
}

/// Size a session's active window by its driver, the client whose input is
/// most recent; with no voting client attached, sizes stay as they are.
pub(crate) async fn reconcile_session(
    graph: &GraphHandle,
    io_state: &Arc<Mutex<PaneIoState>>,
    config: &ConfigHandle,
    session: SessionId,
) -> bool {
    let clients = io_state.lock().await.clients.clone();
    let _turn = clients.sizing_turn().await;
    let Some(driver) = clients.driver(session) else {
        // Nobody sizes it now. The last sizing stays as the snapshot default
        // until the session goes.
        return false;
    };
    let Some(window) = graph
        .snapshot()
        .sessions
        .get(&session)
        .map(|s| s.active_window)
    else {
        return false;
    };
    let border = config.current().appearance.border_style.clone();
    io_state
        .lock()
        .await
        .layout_basis
        .insert(session, (driver.cols, driver.rows, border));
    apply_resize_to_window(graph, io_state, window, config, driver.cols, driver.rows).await
}

/// The daemon's one sizing loop for changes no attach connection sees: a pin,
/// a pane that just became resizable, a window focused or split over RPC.
/// Each pass sizes every session by its driver and wakes the render loops,
/// so an idle client draws the window its session now shows.
pub(crate) async fn run_size_reconciler(
    graph: GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    config: ConfigHandle,
    cancel: CancellationToken,
) {
    let (clients, bus) = {
        let io = io_state.lock().await;
        (io.clients.clone(), io.event_bus.clone())
    };
    let mut changed = clients.changed();
    let mut layout = bus.map(|bus| {
        bus.subscribe_filtered(vec![
            "pane.created".into(),
            "pane.exited".into(),
            "pane.zoomed".into(),
            "window.".into(),
            "session.killed".into(),
        ])
    });
    loop {
        let mut redraw = false;
        tokio::select! {
            () = cancel.cancelled() => return,
            r = changed.changed() => {
                if r.is_err() {
                    return;
                }
            }
            ev = async {
                match layout.as_mut() {
                    Some(sub) => sub.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                if ev.is_none() {
                    layout = None;
                }
                redraw = true;
            }
        }
        let sessions: Vec<SessionId> = graph.snapshot().sessions.keys().copied().collect();
        // A session however it went (killed, its last pane exited) takes its
        // last sizing with it.
        io_state
            .lock()
            .await
            .layout_basis
            .retain(|s, _| sessions.contains(s));
        for session in sessions {
            redraw |= reconcile_session(&graph, &io_state, &config, session).await;
        }
        if redraw {
            io_state.lock().await.render_pulse.clone().bump();
        }
    }
}

/// Send an AttachReady::Error and close.
async fn send_ready_error(
    framed: &mut Framed<impl AttachStream, tokio_util::codec::LengthDelimitedCodec>,
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
#[expect(clippy::too_many_arguments, reason = "independent inputs")]
async fn run_attach_loop(
    framed: Framed<impl AttachStream, tokio_util::codec::LengthDelimitedCodec>,
    graph: GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    config: ConfigHandle,
    segments: SegmentCache,
    meta_cache: crate::session_meta::SessionMetaCache,
    onboarding: crate::onboarding::OnboardingHandle,
    daemon_start: std::time::Instant,
    mut session: AttachedSession,
    hello: AttachHello,
    taken: Arc<std::sync::atomic::AtomicU64>,
    (registered, removed): (crate::clients::Registered, CancellationToken),
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let (mut sink, mut stream) = framed.split();

    let (out_tx, mut out_rx) = mpsc::channel::<AttachServerFrame>(OUTBOUND_FRAMES);

    // This connection's own lifetime: the writer ends it when the client
    // stops reading, so the read loop and teardown never wait on a dead peer.
    let conn = cancel.child_token();
    let _conn_guard = conn.clone().drop_guard();
    let (finish_tx, mut finish_rx) = oneshot::channel();
    let (client_activity, mut activity) = watch::channel(());

    let writer_cancel = conn.clone();
    // Removal is acted on here, not by the read loop, which may be stuck
    // queueing a frame behind the very backlog removal must drop.
    let writer_removed = removed.clone();
    let writer = tokio::spawn(async move {
        use crate::write_progress::{Sent, send_while_progressing};

        // A ping every interval; a client that sent nothing since the last
        // one is gone, however many screens it was sent meanwhile.
        let mut ping =
            tokio::time::interval_at(tokio::time::Instant::now() + PING_INTERVAL, PING_INTERVAL);
        ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut awaiting_reply = false;
        loop {
            let (frame, finishing) = tokio::select! {
                biased;
                _ = writer_cancel.cancelled() => break,
                () = writer_removed.cancelled() => {
                    (AttachServerFrame::SessionEnded { reason: "removed".into() }, true)
                }
                frame = &mut finish_rx => {
                    let Ok(frame) = frame else { break };
                    (frame, true)
                }
                frame = out_rx.recv() => {
                    let Some(frame) = frame else { break };
                    (frame, false)
                }
                _ = ping.tick() => {
                    if awaiting_reply && !activity.has_changed().unwrap_or(false) {
                        warn!("attach: client did not answer its ping; dropping it");
                        break;
                    }
                    activity.borrow_and_update();
                    awaiting_reply = true;
                    (AttachServerFrame::Ping, false)
                }
            };
            let bytes = match serde_json::to_vec(&frame) {
                Ok(bytes) => bytes,
                Err(error) => {
                    warn!(%error, "attach: serialize failed");
                    break;
                }
            };
            match send_while_progressing(
                &mut sink,
                Bytes::from(bytes),
                &taken,
                WRITE_STALL,
                &writer_cancel,
            )
            .await
            {
                Sent::Done if finishing => break,
                Sent::Done => {}
                Sent::Cancelled => break,
                Sent::Closed => {
                    debug!("attach: client closed (writer)");
                    break;
                }
                Sent::Stalled => {
                    warn!("attach: client took no bytes for {WRITE_STALL:?}; dropping it");
                    break;
                }
            }
        }
        writer_cancel.cancel();
    });

    // Authoritative client screen size. Updated only by Resize frames;
    // the renderer reads but never writes it.
    let client_size: ClientSize = Arc::new(Mutex::new(clamp_client_dims(hello.cols, hello.rows)));
    let registry_id = registered.id();
    let (clients, bus) = {
        let io = io_state.lock().await;
        (io.clients.clone(), io.event_bus.clone())
    };
    let (cols0, rows0) = *client_size.lock().await;
    if let Some(bus) = &bus {
        bus.publish(shux_core::event::EventData::ClientConnected {
            client_id: registry_id,
            terminal_cols: cols0,
            terminal_rows: rows0,
        });
        bus.publish(shux_core::event::EventData::SessionAttached {
            session_id: session.session_id,
            client_id: registry_id,
        });
    }
    let mut end_reason = "detached";
    io_state.lock().await.render_pulse.clone().bump();

    // Spawn the renderer task.
    let render_cancel = conn.child_token();
    let render_io = io_state.clone();
    let render_graph = graph.clone();
    let render_tx = out_tx.clone();
    let render_session = Arc::new(Mutex::new(AttachedSession {
        client: Some(registry_id),
        ..session.clone()
    }));
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
            hello.graphics,
            hello.plain,
            fallback_tick(),
            WELCOME_TOAST_DWELL,
            shux_vt::monotonic_now_ns,
        )
        .await;
    });

    // Read client frames.
    // Mouse drag state: when a drag starts on a border cell, we remember
    // which boundary it's grabbing so subsequent Drag events can adjust
    // the layout split ratio.
    let mut mouse_drag: Option<DragState> = None;
    let mut selection_drag = SelectionDrag::None;
    // A press an app got, not yet counted as input: its gesture is open.
    let mut gesture_input = false;
    // `appearance.border_style` moves every pane's rect, and a pane's PTY only
    // learns its size on attach, a client resize, or a layout-changing action.
    // Fire the fan-out on the EDGE of a style change so the two cannot drift.
    //
    // Arm before reading the style: `Notified` captures the `notify_waiters`
    // count when constructed, so a reload landing between these two statements
    // is only seen if the listener is the older of the pair.
    let cfg_changed = config.change_notify();
    let mut cfg_listener = Box::pin(cfg_changed.notified());
    let mut last_border_style = config.current().appearance.border_style.clone();
    // A session destroyed from outside this client (`shux session kill`, an
    // RPC) sends no frame here, so the liveness check below needs a clock of
    // its own; otherwise it waits on the next keystroke or Pong.
    let mut alive_tick = tokio::time::interval(Duration::from_millis(200));
    alive_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut final_frame = None;
    let mut input_backlogs: HashMap<PaneId, InputBacklog> = HashMap::new();
    let client_id = NEXT_CLIENT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // The paste whose pieces are arriving with its pane, and how many this
    // client has sent.
    let (mut pasting, mut pastes): (Option<(u64, PaneId)>, u64) = (None, 0);
    // Whether the paste arriving began in copy mode, which takes all of it.
    let mut paste_in_copy_mode: Option<bool> = None;
    loop {
        tokio::select! {
            _ = conn.cancelled() => {
                // The writer ends `conn` after sending a removal notice too.
                end_reason = if removed.is_cancelled() {
                    "removed"
                } else if cancel.is_cancelled() {
                    "daemon_stopped"
                } else {
                    "connection_lost"
                };
                break;
            }
            _ = removed.cancelled() => {
                end_reason = "removed";
                final_frame = Some(AttachServerFrame::SessionEnded { reason: "removed".into() });
                break;
            }
            _ = alive_tick.tick() => {}
            () = &mut cfg_listener => {
                cfg_listener = Box::pin(cfg_changed.notified());
                let live = config.current().appearance.border_style.clone();
                if live != last_border_style {
                    last_border_style = live;
                    reconcile_session(&graph, &io_state, &config, session.session_id).await;
                    let pulse = io_state.lock().await.render_pulse.clone();
                    pulse.bump();
                }
            }
            frame = stream.next() => {
                let buf = match frame {
                    Some(Ok(b)) => b,
                    Some(Err(e)) => {
                        warn!(error = %e, "attach: read error");
                        end_reason = "closed";
                        break;
                    }
                    None => {
                        debug!("attach: client disconnected");
                        end_reason = "closed";
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
                client_activity.send_replace(());
                match parsed {
                    frame @ (AttachClientFrame::Input { .. } | AttachClientFrame::Paste { .. }) => {
                        let (data, paste) = match frame {
                            AttachClientFrame::Paste { data, more } => (data, Some(more)),
                            AttachClientFrame::Input { data } => (data, None),
                            _ => continue,
                        };
                        let bytes = match BASE64.decode(data.as_bytes()) {
                            Ok(b) => b,
                            Err(_) => continue,
                        };
                        // A key, or a paste's first piece, goes to the pane
                        // focused now; a paste stays with the pane it began in.
                        if paste.is_none() || pasting.is_none() {
                            sync_focus(&graph, &mut session, &render_session).await;
                        }
                        // Help-overlay capture: while the cheat sheet
                        // is on screen, every keystroke either dismisses
                        // (Esc 0x1b, 'q' 0x71) or is swallowed. We must
                        // not forward to the focused PTY — typing should
                        // not reach the shell behind the overlay.
                        {
                            let mut s = render_session.lock().await;
                            let toast_dismissed = std::mem::take(&mut s.show_welcome_toast);
                            if s.action_error.take().is_some() || toast_dismissed {
                                io_state.lock().await.render_pulse.bump();
                            }
                            if toast_dismissed {
                                onboarding.mark_welcome_toast_seen().await;
                            }
                            if s.help_visible {
                                // A paste is text, not keys: swallowed whole.
                                let dismiss = paste.is_none()
                                    && bytes.iter().any(|&b| b == ESC || b == b'q' || b == b'Q');
                                if dismiss {
                                    s.help_visible = false;
                                    let pulse =
                                        io_state.lock().await.render_pulse.clone();
                                    pulse.bump();
                                }
                                continue;
                            }
                        }
                        // A paste is text, not keys: one that begins in a copy
                        // mode that still maps is swallowed whole, and one
                        // that does not is the pane's to the end.
                        match paste {
                            Some(more) => {
                                let swallow = match paste_in_copy_mode {
                                    Some(swallow) => swallow,
                                    None => {
                                        settle_copy_mode(&graph, &io_state, &render_session, &client_size, &config)
                                            .await
                                    }
                                };
                                paste_in_copy_mode = more.then_some(swallow);
                                if swallow {
                                    continue;
                                }
                            }
                            None => paste_in_copy_mode = None,
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
                            pulse.bump();
                        }
                        // Copy-mode capture: route bytes through the
                        // copy-mode key handler instead of forwarding
                        // to the PTY. `y` triggers an OSC 52 yank that
                        // is shipped DIRECTLY to the client (not via
                        // the compositor) so it lands as a single
                        // self-contained terminal sequence — most
                        // terminals interpret it before any subsequent
                        // diff bytes overwrite the cursor position.
                        let attached_now = render_session.lock().await.clone();
                        if paste.is_none()
                            && let Some(cm) = attached_now.copy_mode.as_ref()
                        {
                            let rect = pane_rect_for(&graph, &attached_now, &client_size, &config, cm.pane).await;
                            let routed = {
                                let state = io_state.lock().await;
                                let mut s = render_session.lock().await;
                                copy_mode_key_input(&bytes, &mut s, rect, &state)
                            };
                            // A key that found copy mode gone is the pane's.
                            if let Some(yank) = routed {
                                if let Some(text) = yank.filter(|t| !t.is_empty()) {
                                    let osc = shux_ui::osc52_copy(&text);
                                    let frame = AttachServerFrame::Render {
                                        data: BASE64.encode(&osc),
                                        frame: None,
                                    };
                                    let _ = out_tx.send(frame).await;
                                }
                                continue;
                            }
                        }
                        let active = render_session.lock().await.active_pane_id;
                        // Pieces of one paste share an id and go to the pane
                        // the paste began in, wherever focus has moved since.
                        let (paste, target) = match paste {
                            Some(more) => {
                                let (id, pane) = *pasting.get_or_insert_with(|| {
                                    pastes += 1;
                                    (pastes, active)
                                });
                                if !more {
                                    pasting = None;
                                }
                                (Some((more, id)), pane)
                            }
                            None => {
                                pasting = None;
                                (None, active)
                            }
                        };
                        // Room first, with the PaneIoState mutex released.
                        let writer = io_state.lock().await.writers.get(&target).cloned();
                        if let Some(tx) = writer {
                            let backlog = input_backlogs.entry(target).or_default();
                            let budget = INPUT_STALL.saturating_sub(backlog.waited);
                            match reserve_input(&tx, bytes.len(), budget, &conn).await {
                                // Room the moment it was asked for: caught up.
                                Room::Ready(permit, waited) => {
                                    backlog.waited = if waited.is_zero() {
                                        Duration::ZERO
                                    } else {
                                        backlog.waited + waited
                                    };
                                    {
                                        let mut state = io_state.lock().await;
                                        send_input(&mut state, target, permit, bytes, paste, client_id);
                                    }
                                    // Typing is what makes a client the driver.
                                    if clients.mark_input(registry_id, tokio::time::Instant::now()) {
                                        reconcile_session(&graph, &io_state, &config, session.session_id).await;
                                    }
                                }
                                Room::Closed => {
                                    input_backlogs.remove(&target);
                                }
                                Room::Full(waited) => {
                                    backlog.waited += waited;
                                    // The drop that used up the wait: once per stall.
                                    if !budget.is_zero() {
                                        warn!(pane = %target, "input dropped: pane not keeping up");
                                    }
                                    backlog.dropped += bytes.len();
                                    let dropped = backlog.dropped;
                                    // Refreshed on every drop, so the notice
                                    // stays up for as long as input is lost.
                                    render_session.lock().await.last_action = Some((
                                        format!("pane not keeping up: {} of input dropped", byte_count(dropped)),
                                        std::time::Instant::now(),
                                    ));
                                    let pulse = io_state.lock().await.render_pulse.clone();
                                    pulse.bump();
                                }
                            }
                        }
                    }
                    AttachClientFrame::Resize { cols, rows } => {
                        let (cols, rows) = clamp_client_dims(cols, rows);
                        let (old_cols, old_rows) =
                            std::mem::replace(&mut *client_size.lock().await, (cols, rows));
                        clients.set_size(registry_id, cols, rows);
                        if let Some(bus) = bus.as_ref().filter(|_| (old_cols, old_rows) != (cols, rows)) {
                            bus.publish(shux_core::event::EventData::ClientResized {
                                client_id: registry_id,
                                old_cols,
                                old_rows,
                                new_cols: cols,
                                new_rows: rows,
                            });
                        }
                        // Only the driver's size reaches the panes.
                        reconcile_session(&graph, &io_state, &config, session.session_id).await;
                        let pulse = io_state.lock().await.render_pulse.clone();
                        pulse.bump();
                    }
                    AttachClientFrame::Action { kind, args } => {
                        sync_focus(&graph, &mut session, &render_session).await;
                        // The user pressed prefix + key — onboarding hint
                        // can dismiss. Cheap idempotent write; first call
                        // persists, subsequent ones short-circuit.
                        onboarding.mark_prefix_discovered().await;
                        if std::mem::take(&mut render_session.lock().await.show_welcome_toast) {
                            onboarding.mark_welcome_toast_seen().await;
                        }

                        // Modal copy mode takes over any text drag: a credit it
                        // left pending must not be spent by a later click.
                        if kind == ActionKind::EnterCopyMode {
                            gesture_input = false;
                        }
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
                            warn!(?kind, error = %format!("{e:#}"), "attach: action failed");
                        }
                        render_session.lock().await.action_error = action_result
                            .as_ref()
                            .err()
                            .map(|e| action_failure_notice(kind, e));

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
                        // An action that did something is input, and may have
                        // changed the layout the panes are sized from.
                        let committed = action_result.is_ok()
                            && !matches!(
                                kind,
                                ActionKind::Redraw | ActionKind::ToggleHelp | ActionKind::EnterCopyMode
                            );
                        if committed {
                            clients.mark_input(registry_id, tokio::time::Instant::now());
                            reconcile_session(&graph, &io_state, &config, session.session_id).await;
                        }
                        let pulse = io_state.lock().await.render_pulse.clone();
                        pulse.bump();
                    }
                    AttachClientFrame::Mouse {
                        kind,
                        button,
                        col,
                        row,
                        shift,
                        alt,
                        ctrl,
                        frame,
                    } => {
                        // Another client may have moved focus since this
                        // one last heard: a click compares against it.
                        sync_focus(&graph, &mut session, &render_session).await;
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
                            release_app_gesture(&io_state, &mut selection_drag, None).await;
                            selection_drag = SelectionDrag::None;
                            // That gesture ended unfinished: its press is no
                            // input to count at some later click.
                            gesture_input = false;
                            continue;
                        }
                        // A copy mode that no longer maps is gone before the
                        // event is routed: it is the app's or a selection's.
                        settle_copy_mode(&graph, &io_state, &render_session, &client_size, &config)
                            .await;
                        let unhandled_mouse = |error| {
                            warn!(?kind, %error, "attach: mouse handle failed");
                            false
                        };
                        let gesture_pane = match selection_drag {
                            SelectionDrag::App { pane_id, .. } => Some(pane_id),
                            _ => None,
                        };
                        let focus_before = focused_pane(&graph, session.session_id);
                        let route = route_mouse(
                            kind,
                            button,
                            (col, row),
                            (shift, alt, ctrl),
                            frame,
                            &graph,
                            &io_state,
                            &render_session,
                            &client_size,
                            &config,
                            &out_tx,
                            &mouse_drag,
                            &mut selection_drag,
                        )
                        .await;
                        match route {
                            MouseRoute::App { redraw, input } => {
                                // A press or drag the app received is input. The
                                // driver's keeps its hold on the panes for the
                                // gesture; a viewer's takes them only when the
                                // gesture ends, and only once its release is
                                // written: taking them resizes the grid, and a
                                // release mapped or read after that names
                                // another cell than its press.
                                let driving = clients
                                    .session_view(session.session_id, Some(registry_id), &[])
                                    .role
                                    == Some(crate::clients::Role::Driving);
                                // The driver's release too: the hold it had while
                                // still lasts a settle window past letting go.
                                if input && driving && kind != MouseKind::Move {
                                    clients.mark_input(registry_id, tokio::time::Instant::now());
                                } else {
                                    gesture_input |= input
                                        && matches!(kind, MouseKind::Down | MouseKind::ScrollUp | MouseKind::ScrollDown);
                                }
                                if gesture_input && !matches!(selection_drag, SelectionDrag::App { .. }) {
                                    gesture_input = false;
                                    // A release not written in time drops the
                                    // takeover: the next input can earn it.
                                    let written = match gesture_pane {
                                        Some(pane) => release_written(&io_state, pane, registry_id).await,
                                        None => true,
                                    };
                                    if written && clients.mark_input(registry_id, tokio::time::Instant::now()) {
                                        reconcile_session(&graph, &io_state, &config, session.session_id).await;
                                    }
                                }
                                if redraw {
                                    let pulse = io_state.lock().await.render_pulse.clone();
                                    pulse.bump();
                                }
                                continue;
                            }
                            MouseRoute::Selection => {
                                // A press that focused another pane is input; a
                                // selection inside the focused one is not. It counts
                                // once the selection ends and is yanked: taking the
                                // panes resizes them, which ends the selection.
                                if kind == MouseKind::Down
                                    && focused_pane(&graph, session.session_id) != focus_before
                                {
                                    gesture_input = true;
                                }
                                if gesture_input
                                    && !matches!(selection_drag, SelectionDrag::MouseSelection { .. })
                                {
                                    gesture_input = false;
                                    if clients.mark_input(registry_id, tokio::time::Instant::now()) {
                                        reconcile_session(&graph, &io_state, &config, session.session_id).await;
                                    }
                                }
                                let pulse = io_state.lock().await.render_pulse.clone();
                                pulse.bump();
                                continue;
                            }
                            MouseRoute::CopyMode => {
                                // Copy mode ended the drag a focusing press began:
                                // that selection was never yanked, so its credit goes.
                                if kind == MouseKind::Up {
                                    gesture_input = false;
                                }
                                let pulse = io_state.lock().await.render_pulse.clone();
                                pulse.bump();
                                continue;
                            }
                            MouseRoute::Unrouted => {}
                        }
                        // Scroll wheel (copy mode inactive): scroll scrollback,
                        // or forward to a mouse-aware / alt-screen app.
                        if handle_wheel(
                            kind,
                            col,
                            row,
                            frame,
                            &graph,
                            &io_state,
                            &render_session,
                            &client_size,
                            &config,
                        )
                        .await.unwrap_or_else(unhandled_mouse)
                        {
                            let pulse = io_state.lock().await.render_pulse.clone();
                            pulse.bump();
                            continue;
                        }
                        let acted = match handle_mouse(
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
                            frame,
                        )
                        .await
                        {
                            Ok(acted) => acted,
                            Err(e) => {
                                warn!(?kind, error = %e, "attach: mouse handle failed");
                                false
                            }
                        };
                        // Click-to-focus and a border drag are input too; a
                        // click on nothing (a pane's surplus) is not.
                        if acted {
                            clients.mark_input(registry_id, tokio::time::Instant::now());
                            reconcile_session(&graph, &io_state, &config, session.session_id).await;
                        }
                        let pulse = io_state.lock().await.render_pulse.clone();
                        pulse.bump();
                    }
                    AttachClientFrame::Detach => {
                        // Detach implies the user found the prefix too.
                        onboarding.mark_prefix_discovered().await;
                        final_frame = Some(AttachServerFrame::DetachAck);
                        break;
                    }
                    AttachClientFrame::PrefixTapped => {
                        // Authoritative signal: user has discovered the
                        // prefix even if they bail without sending an
                        // Action (Ctrl+Space → Escape, etc). The OOTB
                        // hint dismisses forever.
                        onboarding.mark_prefix_discovered().await;
                    }
                    AttachClientFrame::Prefix { prefix } => {
                        render_session.lock().await.client_prefix = Some(prefix);
                        let pulse = io_state.lock().await.render_pulse.clone();
                        pulse.bump();
                    }
                    AttachClientFrame::CellPixels { width, height } => {
                        render_session.lock().await.cell_pixels = Some((width, height));
                        let pulse = io_state.lock().await.render_pulse.clone();
                        pulse.bump();
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
        let still_alive = sync_focus(&graph, &mut session, &render_session).await;
        if !still_alive {
            end_reason = "session_destroyed";
            final_frame = Some(AttachServerFrame::SessionEnded {
                reason: "session_destroyed".into(),
            });
            break;
        }
    }

    renderer.abort();
    // Gone before the client hears it is: a detach ack or a socket nobody
    // reads must not outrun the list, the events, or a held mouse button.
    drop(registered);
    if let Some(bus) = &bus {
        bus.publish(shux_core::event::EventData::ClientDisconnected {
            client_id: registry_id,
            reason: end_reason.into(),
        });
        bus.publish(shux_core::event::EventData::SessionDetached {
            session_id: session.session_id,
            client_id: registry_id,
        });
    }
    let _ = renderer.await;
    io_state
        .lock()
        .await
        .mouse_release_written
        .retain(|(_, client), _| *client != registry_id);
    // The next voter takes the panes the moment this one is gone.
    reconcile_session(&graph, &io_state, &config, session.session_id).await;
    io_state.lock().await.render_pulse.clone().bump();

    // A gesture forwarded to a pane app outlives the attach: the PTY does not
    // go away when the client does. Detaching mid-drag without this leaves the
    // app believing a button is still held, and it stays that way for the rest
    // of the pane's life — visible next time anyone attaches.
    close_open_pastes(&io_state, client_id).await;
    release_app_gesture(&io_state, &mut selection_drag, None).await;

    // The writer finishes the frame on the wire, sends the final one and
    // ends; a stalled socket ends it at the write stall.
    match final_frame {
        Some(frame) => {
            let _ = finish_tx.send(frame);
        }
        None => conn.cancel(),
    }
    let _ = writer.await;
    info!(session = %session.name, "attach session ended");
    Ok(())
}

/// Run the per-attach render loop.
///
/// The loop wakes on every `render_pulse` bump (PTY data, action
/// completion) and also fires a low-rate fallback tick (200ms) so cursor
/// blinks and clocks update without external input. After each wake-up
/// it grabs a fresh `SessionGraphSnapshot`, walks all panes in the
/// active window, runs the multi-pane compositor over a `Vec<u8>`
/// buffer, then ships the bytes as a `Render` frame.
#[expect(clippy::too_many_arguments, reason = "independent inputs")]
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
    graphics: bool,
    plain: bool,
    tick_period: Duration,
    toast_dwell: Duration,
    // The clock hidden-cursor stamps are read against: `monotonic_now_ns`.
    now: fn() -> u64,
) {
    // Before anything else: a receiver starts at the current generation.
    let (mut pulse, clients) = {
        let io = io_state.lock().await;
        (io.render_pulse.subscribe(), io.clients.clone())
    };
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
    compositor.set_graphics(graphics && !plain);
    compositor.set_plain(plain);

    // Send a clear-screen ANSI prelude so the client terminal starts blank.
    let _ = out_tx
        .send(AttachServerFrame::Render {
            data: BASE64.encode(b"\x1b[2J\x1b[H"),
            frame: None,
        })
        .await;

    // Fallback tick lets us update clocks etc. even when nothing else
    // happens. The pulse covers the data-driven case.
    let mut tick = tokio::time::interval(tick_period);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_frame: Option<tokio::time::Instant> = None;
    let mut frame_number: u64 = 0;

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
    let mut toast_drawn = false;
    let mut last_overlay_visible = false;
    let mut last_copy_overlay: Option<CopyOverlayStamp> = None;
    let mut last_copy_menu: Option<CopyContextMenu> = None;
    let mut menu_sheet: Option<SheetStamp> = None;
    let mut help_sheet: Option<SheetStamp> = None;
    let mut toast_sheet: Option<SheetStamp> = None;
    // A driver handoff at the same size moves no pane, but changes this
    // client's status bar.
    let mut registry = clients.changed();
    // When a pending hidden-cursor latch will move a pane: a frame then.
    let mut latch_at: Option<u64> = None;
    let mut last_shown: Option<Shown> = None;

    loop {
        // A bump landed while the last frame was being drawn: output is
        // arriving faster than this loop renders.
        let behind = pulse.has_changed().unwrap_or(false);
        let latch = async {
            match latch_at {
                Some(at) => {
                    let wait = Duration::from_nanos(at.saturating_sub(now()));
                    #[expect(
                        clippy::disallowed_methods,
                        reason = "a latch deadline the frame must meet; never synchronises a test"
                    )]
                    tokio::time::sleep(wait).await;
                }
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            _ = cancel.cancelled() => break,
            () = latch => {}
            changed = registry.changed() => {
                if changed.is_err() {
                    break;
                }
            }
            changed = pulse.changed() => {
                // Unreachable while `PaneIoState` lives; never spin on it.
                if changed.is_err() {
                    break;
                }
                // Every loop wakes on every pane's output, so a flood would
                // have each one composite under the `io_state` lock as fast
                // as it can. Bumps that land meanwhile coalesce.
                if behind
                    && let Some(next) = last_frame.map(|at| at + MIN_FRAME_INTERVAL)
                    && next > tokio::time::Instant::now()
                {
                    #[expect(clippy::disallowed_methods, reason = "paces frames; never synchronises a test")]
                    let pace = tokio::time::sleep_until(next);
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = pace => {}
                    }
                }
            }
            _ = &mut cfg_listener => {
                cfg_listener = Box::pin(cfg_notify.notified());
                // Force a full redraw so border-style changes etc.
                // visibly land on the very next frame.
                compositor.force_redraw();
            }
            _ = tick.tick() => {}
        }

        // For the wait after the frame that finds it: a frame not drawn (its
        // window gone) must not leave a past deadline waking this at once.
        latch_at = None;
        last_frame = Some(tokio::time::Instant::now());

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
        // Show the window and pane the session shows, though another client
        // or an RPC moved them and this one has sent nothing since.
        // Drawn from this snapshot only: the input loop owns the cached
        // focus, and a stale frame must not write an old window back.
        let attached = {
            let mut s = session.lock().await.clone();
            if let Some(sess) = snap.sessions.get(&s.session_id)
                && let Some(w) = snap.windows.get(&sess.active_window)
            {
                s.active_window_id = sess.active_window;
                s.active_pane_id = w.active_pane;
            }
            s
        };

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
        // Modal copy mode's stamp is made from the prepared frame, below.
        let mut copy_overlay = attached
            .mouse_selection
            .as_ref()
            .filter(|_| attached.copy_mode.is_none())
            .map(|selection| CopyOverlayStamp {
                kind: CopyOverlayKind::MouseSelection(selection.view, selection.state),
                pane_id: selection.pane_id,
                theme: live_theme,
            });

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
        let mut geometry = shux_ui::prepare_frame(&shux_ui::FrameInputs {
            layout: &win.layout.tree,
            zoom: win.layout.zoom.as_ref(),
            cols,
            rows,
            status_bar_height: STATUS_BAR_ROWS,
            border_style: BorderStyle::parse(&last_border_style),
            panes: &shux_ui::PresentedPane::of_vts(&vt_refs),
            now_ns: now(),
        });
        // Modal copy mode draws its pane through its own view, as resolved.
        let mut modal = None;
        let mut modal_replaced = None;
        if let Some(seen) = attached.copy_mode.as_ref() {
            let pane = seen.pane;
            let rect = geometry.views.get(&pane).map(|v| v.rect);
            let mut cm = seen.clone();
            let vt = resolve_copy_mode(&mut cm, attached.active_pane_id, rect, now(), &state);
            if vt.is_none() || cm != *seen {
                modal_replaced = Some((seen.clone(), vt.map(|_| cm.clone())));
            }
            if let Some(vt) = vt {
                let (view, over) = cm.view.frame_view(vt);
                geometry.views.insert(pane, view);
                modal = Some((pane, cm.view));
                copy_overlay = Some(CopyOverlayStamp {
                    pane_id: pane,
                    kind: CopyOverlayKind::Modal(cm, over),
                    theme: live_theme,
                });
            }
        }
        latch_at = geometry.deadline_ns;
        // A selection reads the slice it was made over; once the pane shows
        // another, it would highlight and yank cells no longer drawn there.
        let stale_selection = attached
            .mouse_selection
            .as_ref()
            .filter(|s| geometry.views.get(&s.pane_id) != Some(&s.view))
            .map(|s| s.generation)
            .filter(|_| attached.copy_mode.is_none());
        if stale_selection.is_some() {
            copy_overlay = None;
            compositor.force_redraw();
        }
        // Its menu goes with it, in this frame already.
        let copy_menu = attached.copy_menu.filter(|_| stale_selection.is_none());
        let copy_overlay_changed =
            copy_overlay_needs_base_redraw(last_copy_overlay.as_ref(), copy_overlay.as_ref());
        if copy_overlay_changed || copy_menu != last_copy_menu {
            // The copy cursor/selection is drawn as an overlay after the normal
            // framebuffer diff, so changes to it are invisible to the
            // compositor. Redraw the base frame only when that overlay state
            // changes; doing this every tick makes the pane visibly flicker.
            compositor.force_redraw();
        }
        let pane_ids = win.layout.tree.pane_ids();
        let sizing = clients.session_view(attached.session_id, attached.client, &pane_ids);
        // Status bar text. Start from the built-in (always-good)
        // segments so OOTB looks the same even when no script segments
        // are configured. Then `populate_bar` appends any
        // `[[statusbar.segment]]` results from the runner cache.
        let nerd_fonts = live_cfg.appearance.nerd_fonts;
        let mut pane_chrome = crate::pane_chrome::pane_chrome(
            &snap,
            pane_ids,
            &live_cfg.program_status,
            nerd_fonts,
            &live_theme,
        );
        let bar_label = crate::pane_chrome::size_labels(
            &mut pane_chrome,
            &geometry,
            &sizing.pinned,
            attached.active_pane_id,
        );
        let prefix_label = prefix_display(
            attached
                .client_prefix
                .as_deref()
                .unwrap_or(&live_cfg.keys.prefix),
        );
        let session_meta = meta_cache.get(attached.session_id).await;
        let onboarding_state = onboarding.current().await;
        let toast_up = !onboarding_state.welcome_toast_seen && attached.show_welcome_toast;
        // A key ends the toast between frames, here or on another client;
        // its cells changed in no VT grid, so only a full redraw clears them.
        if toast_drawn && !toast_up {
            compositor.force_redraw();
            toast_drawn = false;
        }
        let daemon_uptime = daemon_start.elapsed();
        let last_action_ref = attached.last_action.as_ref().map(|(s, i)| (s.as_str(), *i));
        let render_ctx = StatusBarCtx {
            session_id: attached.session_id,
            // The graph, not the name cached at attach: a rename shows at once.
            session_name: snap
                .sessions
                .get(&attached.session_id)
                .map_or(attached.name.as_str(), |s| s.name.as_str()),
            active_window_id: attached.active_window_id,
            active_pane_id: attached.active_pane_id,
            session_meta: &session_meta,
            onboarding: &onboarding_state,
            daemon_uptime,
            nerd_fonts,
            prefix_label: &prefix_label,
            client_cols: cols,
            // As resolved for this frame: the one that ends it drops the flag.
            copy_mode_active: modal.is_some(),
            last_action: last_action_ref,
            action_error: attached.action_error.as_deref(),
        };
        let has_notice = attached.action_error.is_some()
            || last_action_ref.is_some_and(|(_, at)| {
                at.elapsed() < crate::statusbar_build::ACTION_FEEDBACK_DWELL
            });
        let mut bar = build_status_bar_shared(&snap, &live_theme, &render_ctx);
        populate_bar(&mut bar, &live_cfg, &segments).await;
        crate::statusbar_build::append_waiting(
            &mut bar,
            crate::pane_chrome::waiting_count(&snap, attached.session_id, &live_cfg.program_status),
            &live_theme,
            nerd_fonts,
            cols,
        );
        crate::statusbar_build::append_watchers(
            &mut bar,
            sizing.attached,
            &live_theme,
            cols,
            has_notice,
        );
        crate::statusbar_build::append_viewing(
            &mut bar,
            bar_label.as_ref(),
            sizing.role,
            &live_theme,
            cols,
            plain,
        );
        if has_notice {
            crate::statusbar_build::make_room_for_notice(&mut bar, cols);
        }

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
            chrome: Some(&pane_chrome),
            status_bar: Some(&bar),
            geometry: Some(&geometry),
        };
        compositor.set_cursor_hidden(
            attached.help_visible
                || copy_overlay
                    .as_ref()
                    .is_some_and(|o| matches!(o.kind, CopyOverlayKind::Modal(..))),
        );
        compositor.set_cell_pixels(attached.cell_pixels);
        // Reset the buffer first so we only ship the new frame's bytes.
        compositor.inner_mut().clear();
        let _ = compositor.render_multi_pane(frame);
        let frame_end = compositor.inner().len();

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
                let buf = compositor.inner_mut();
                let theme = &overlay.theme;
                match (&overlay.kind, state.vts.get(&overlay.pane_id)) {
                    (CopyOverlayKind::Modal(cm, over), Some(vt)) => {
                        if *over {
                            shux_ui::render_copy_view_into(buf, vt, cm);
                        }
                        shux_ui::render_copy_overlay_with_vt_into(buf, vt, cm, theme);
                    }
                    (CopyOverlayKind::Modal(cm, _), None) => {
                        shux_ui::render_copy_overlay_into(buf, cm, theme);
                    }
                    (CopyOverlayKind::MouseSelection(view, sel), Some(vt)) => {
                        shux_ui::copy_mode::render_selection_overlay_with_vt_into(
                            buf, view, vt, sel, theme,
                        );
                    }
                    (CopyOverlayKind::MouseSelection(view, sel), None) => {
                        shux_ui::copy_mode::render_selection_overlay_into(buf, view, sel, theme);
                    }
                }
            }
        }
        last_copy_overlay = copy_overlay;

        let sheet = |label: String| SheetStamp {
            cols,
            rows,
            theme: live_theme,
            label,
        };
        if let Some(menu) = copy_menu {
            // A different menu forced a full redraw above.
            let base_emitted = !compositor.inner().is_empty();
            if sheet_due(&mut menu_sheet, sheet(String::new()), base_emitted) {
                shux_ui::copy_mode::render_copy_menu_into(
                    compositor.inner_mut(),
                    menu.col,
                    menu.row,
                    cols,
                    rows,
                    &live_theme,
                );
            }
        } else {
            menu_sheet = None;
        }
        last_copy_menu = copy_menu;

        // Help-overlay layer: drawn AFTER the diff'd multipane frame so
        // it covers the cells underneath. Toggling the overlay also
        // forces a full redraw on the next frame so the underlying
        // cells return when the overlay closes — otherwise the
        // compositor's diff would skip those positions because they
        // didn't change in the VT grid.
        if attached.help_visible {
            let base_emitted = !compositor.inner().is_empty();
            if sheet_due(&mut help_sheet, sheet(prefix_label.clone()), base_emitted) {
                shux_ui::render_help_overlay_into(
                    compositor.inner_mut(),
                    cols,
                    rows,
                    &live_theme,
                    &prefix_label,
                );
            }
        } else {
            help_sheet = None;
        }

        // Welcome toast (first-attach onboarding). Only fires when the
        // user has never seen it (per the onboarding state file).
        // Dwells `toast_dwell` after first render, then auto-
        // dismisses and marks seen on disk so the next attach is clean.
        let mut toast_expired = false;
        if toast_up {
            let elapsed = welcome_toast_started
                .get_or_insert_with(std::time::Instant::now)
                .elapsed();
            if elapsed < toast_dwell {
                let stamp = sheet(format!("{prefix_label} {nerd_fonts}"));
                if sheet_due(&mut toast_sheet, stamp, !compositor.inner().is_empty()) {
                    render_welcome_toast(
                        compositor.inner_mut(),
                        cols,
                        rows,
                        &live_theme,
                        &prefix_label,
                        nerd_fonts,
                    );
                }
                toast_drawn = true;
            } else {
                // One-shot persist + flag-flip.
                let onb = onboarding.clone();
                tokio::spawn(async move {
                    onb.mark_welcome_toast_seen().await;
                });
                toast_expired = true;
                compositor.force_redraw();
                toast_drawn = false;
                toast_sheet = None;
            }
        } else {
            toast_sheet = None;
        }

        if compositor.inner().len() > frame_end {
            let _ = compositor.restore_cursor();
        }

        // Take the bytes out (drain) and send them.
        let bytes = std::mem::take(compositor.inner_mut());
        let bytes = if plain {
            shux_ui::render::strip_hidden_controls(bytes)
        } else {
            bytes
        };
        // Re-establish capacity for next frame.
        compositor.inner_mut().reserve(64 * 1024);
        let drawn = vt_refs
            .iter()
            .map(|(pane, vt)| (*pane, shux_ui::Drawn::of(vt)))
            .collect();
        drop(state);
        // Only once `io_state` is released: the input task takes the session
        // before `io_state`, so holding both the other way round deadlocks.
        if toast_expired {
            session.lock().await.show_welcome_toast = false;
        }
        // Only the copy mode found unmappable: one opened since stays.
        if let Some((seen, replacement)) = modal_replaced {
            let mut s = session.lock().await;
            if s.copy_mode.as_ref() == Some(&seen) {
                s.copy_mode = replacement;
            }
        }
        // Only the selection found stale: one started since stays.
        if let Some(stale) = stale_selection {
            let mut s = session.lock().await;
            if s.mouse_selection
                .as_ref()
                .is_some_and(|m| m.generation == stale)
            {
                s.mouse_selection = None;
                s.copy_menu = None;
            }
        }

        // A frame that moved a slice, or copy mode's view, but no cell still
        // goes out, so the client's next event is stamped with what it shows.
        let shown = Shown {
            views: compositor.pane_views().clone(),
            drawn,
            modal,
        };
        if !bytes.is_empty() || last_shown.as_ref() != Some(&shown) {
            frame_number += 1;
            last_shown = Some(shown.clone());
            if !send_frame(&out_tx, &session, frame_number, shown, &bytes).await {
                break;
            }
        }
    }
}

/// Send one composited frame as `Render` parts, the last numbered `frame`.
/// Its views are recorded first, so an event the client stamps with that
/// number always finds them. False when the client is gone.
///
/// Split rather than sent whole: base64 inflates by 4/3 and the codec refuses
/// a frame over MAX_FRAME_SIZE, so one composited frame that crosses the cap
/// used to fail the send, break the writer task and leave the attach
/// permanently blank with no error. Splitting is invisible to the terminal:
/// this is a byte stream, and the compositor wraps each frame in synchronized
/// output, so the parts are held and applied together.
async fn send_frame(
    out_tx: &mpsc::Sender<AttachServerFrame>,
    session: &Arc<Mutex<AttachedSession>>,
    frame: u64,
    shown: Shown,
    bytes: &[u8],
) -> bool {
    session.lock().await.shown.push(frame, shown);
    if bytes.is_empty() {
        let render = AttachServerFrame::Render {
            data: String::new(),
            frame: Some(frame),
        };
        return out_tx.send(render).await.is_ok();
    }
    let mut parts = bytes.chunks(RENDER_PART_BYTES).peekable();
    while let Some(part) = parts.next() {
        let render = AttachServerFrame::Render {
            data: BASE64.encode(part),
            frame: parts.peek().is_none().then_some(frame),
        };
        if out_tx.send(render).await.is_err() {
            return false;
        }
    }
    true
}

/// Bytes of one composited frame per `Render` message. Base64 takes this to
/// ~10.7 MiB, comfortably inside the codec's 16 MiB frame cap with room for the
/// JSON envelope.
const RENDER_PART_BYTES: usize = 8 * 1024 * 1024;

/// The shortest gap between two frames of one client while output arrives
/// faster than it renders. Output after an idle wait draws at once.
const MIN_FRAME_INTERVAL: Duration = Duration::from_millis(16);

/// How long the first-attach welcome toast stays on screen before
/// auto-dismissing. Tuned for "long enough to read, short enough to
/// not get in the way".
const WELCOME_TOAST_DWELL: Duration = Duration::from_secs(3);

/// The status-bar line for a failed action: `"<verb> failed: <reason>"`.
fn action_failure_notice(kind: ActionKind, e: &anyhow::Error) -> String {
    let verb = match kind {
        ActionKind::SplitVertical | ActionKind::SplitHorizontal | ActionKind::SplitSmart => "split",
        ActionKind::NewWindow => "new window",
        ActionKind::ToggleZoom => "zoom",
        _ => "action",
    };
    format!("{verb} failed: {e}")
}

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

/// Who owns the in-flight mouse gesture. Decided once, at button-down, and
/// honoured until the last button comes up: a single latch, so a mode change
/// mid-drag cannot split a gesture between shux and the app.
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
    /// `anchor` is the screen point of the last forwarded event and `offsets`
    /// the (row, column) view offsets it mapped through. An event at that same
    /// point maps through them too, so an app that moves its cursor with the
    /// mouse (vim) does not shift a click or a drag's release onto another
    /// cell when the view follows; an event anywhere else names the cell shown
    /// under the pointer now.
    App {
        pane_id: PaneId,
        buttons: u8,
        last: (u16, u16),
        offsets: (usize, usize),
        anchor: (u16, u16),
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

#[expect(clippy::too_many_arguments, reason = "independent inputs")]
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
    frame: Option<u64>,
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
                        let copied =
                            yank_selection(selection, &attached.shown, io_state, out_tx).await;
                        let mut s = session.lock().await;
                        s.copy_menu = None;
                        if copied {
                            s.last_action =
                                Some(("copied selection".into(), std::time::Instant::now()));
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
            drop(snap);
            // A selection starts only on a cell the client was shown: the
            // stamped frame's, never surplus or a frame no longer kept.
            let hit = attached.shown.views(frame).and_then(|views| {
                views.iter().find_map(|(pid, view)| {
                    let pos = view_point(view, col, row, false)?;
                    Some((*pid, *view, pos))
                })
            });
            let Some((pane_id, view, pos)) = hit else {
                return Ok(false);
            };
            if !pane_is_shown(graph, &attached, pane_id) {
                return Ok(false);
            }

            if pane_id != attached.active_pane_id {
                let _ = graph.focus_pane(pane_id).await;
            }
            let state = shux_ui::Selection {
                cursor: pos,
                anchor: Some(pos),
            };
            let mut s = session.lock().await;
            s.active_pane_id = pane_id;
            s.mouse_selection = Some(MouseSelection {
                pane_id,
                state,
                view,
                generation: next_selection_generation(),
            });
            s.copy_menu = None;
            *drag = SelectionDrag::MouseSelection { pane_id };
            Ok(true)
        }
        (MouseKind::Drag, ProtoMouseButton::Left) => {
            let SelectionDrag::MouseSelection { pane_id } = *drag else {
                return Ok(false);
            };
            if !pane_is_shown(graph, &attached, pane_id) {
                *drag = SelectionDrag::None;
                return Ok(true);
            }
            let mut s = session.lock().await;
            if let Some(selection) = s
                .mouse_selection
                .as_mut()
                .filter(|selection| selection.pane_id == pane_id)
                && let Some(pos) = view_point(&selection.view, col, row, true)
            {
                selection.state.cursor = pos;
            }
            Ok(true)
        }
        (MouseKind::Up, ProtoMouseButton::Left) => {
            let SelectionDrag::MouseSelection { pane_id } = *drag else {
                return Ok(false);
            };
            if !pane_is_shown(graph, &attached, pane_id) {
                *drag = SelectionDrag::None;
                return Ok(true);
            }
            let selection = {
                let mut s = session.lock().await;
                if let Some(selection) = s
                    .mouse_selection
                    .as_mut()
                    .filter(|selection| selection.pane_id == pane_id)
                {
                    if let Some(pos) = view_point(&selection.view, col, row, true) {
                        selection.state.cursor = pos;
                    }
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
                    let shown = session.lock().await.shown.clone();
                    let copied = yank_selection(&selection, &shown, io_state, out_tx).await;
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
            if !pane_is_shown(graph, &attached, selection.pane_id) {
                session.lock().await.mouse_selection = None;
                return Ok(true);
            }
            // The click names the selection only through the frame it was made
            // over: a client a frame behind or ahead saw other cells there.
            let stamped = attached
                .shown
                .views(frame)
                .and_then(|v| v.get(&selection.pane_id));
            let at = view_point(&selection.view, col, row, false)
                .filter(|_| stamped == Some(&selection.view))
                .zip(selection.state.anchor);
            let inside = at.is_some_and(|(point, anchor)| {
                selection_contains_local_point(
                    anchor,
                    selection.state.cursor,
                    point,
                    selection.view.rect.width,
                )
            });
            if inside {
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

/// Copy mode on `pane` over the lines `shown`, a frame, drew; when an action
/// since has moved or resized the pane, over the slice the next frame draws in
/// `rect`, which copy mode shows before anything is selected. None once the
/// pane shows another screen than that frame did.
fn copy_mode_over(
    pane: PaneId,
    shown: Option<&Shown>,
    rect: Option<Rect>,
    state: &PaneIoState,
) -> Option<shux_ui::CopyModeState> {
    let vt = state.vts.get(&pane)?;
    let stamped = shown.and_then(|frame| frame.of(pane));
    if stamped.is_some_and(|(_, drawn)| !drawn.on_screen_of(vt)) {
        return None;
    }
    stamped
        .and_then(|(view, drawn)| shux_ui::CopyModeState::enter(pane, *view, drawn, vt))
        .or_else(|| {
            let live = PaneViewport::of(rect?, vt, shux_vt::monotonic_now_ns());
            shux_ui::CopyModeState::enter(pane, live, shux_ui::Drawn::of(vt), vt)
        })
}

/// The pane's VT, with `cm` counted against it, while copy mode maps: focus
/// on its pane, drawn in `rect`, the grid and screen its view began on. One
/// nothing was done in yet reopens over the slice `rect` shows now, so an
/// action's resize landing under it loses nothing; never over another
/// screen. `None` ends copy mode.
fn resolve_copy_mode<'s>(
    cm: &mut shux_ui::CopyModeState,
    active: PaneId,
    rect: Option<Rect>,
    now_ns: u64,
    state: &'s PaneIoState,
) -> Option<&'s shux_vt::VirtualTerminal> {
    let vt = state.vts.get(&cm.pane).filter(|_| cm.pane == active)?;
    let rect = rect?;
    if !cm.view.maps(rect, vt) {
        if !cm.is_untouched() || !cm.view.on_screen_of(vt) {
            return None;
        }
        let view = PaneViewport::of(rect, vt, now_ns);
        let wheel = cm.wheel_initiated;
        *cm = shux_ui::CopyModeState::enter(cm.pane, view, shux_ui::Drawn::of(vt), vt)?;
        cm.wheel_initiated = wheel;
    }
    cm.rebase(vt);
    Some(vt)
}

/// A key for the client's copy mode, if it has one that still maps: what it
/// yanked, if anything. `None` when the key is the pane's.
fn copy_mode_key_input(
    bytes: &[u8],
    s: &mut AttachedSession,
    rect: Option<Rect>,
    state: &PaneIoState,
) -> Option<Option<String>> {
    let active = s.active_pane_id;
    let (mut yank, mut mapped, mut evicted) = (None, false, false);
    let keep = s.copy_mode.as_mut().is_some_and(|cm| {
        let now = shux_vt::monotonic_now_ns();
        let Some(vt) = resolve_copy_mode(cm, active, rect, now, state) else {
            return false;
        };
        mapped = true;
        match shux_ui::copy_mode_key(bytes, cm, vt) {
            shux_ui::CopyKey::Updated | shux_ui::CopyKey::Ignored => true,
            shux_ui::CopyKey::SelectionEvicted => {
                evicted = true;
                true
            }
            shux_ui::CopyKey::Exit => false,
            shux_ui::CopyKey::Yank => {
                yank = Some(shux_ui::copy_mode::extract_selection(vt, cm));
                false
            }
        }
    });
    if !keep {
        s.copy_mode = None;
    }
    if evicted {
        s.last_action = Some((
            "selection left history: nothing copied".into(),
            std::time::Instant::now(),
        ));
    }
    state.render_pulse.bump();
    mapped.then_some(yank)
}

/// Whether the client's copy mode still maps, ending it when it does not:
/// what decides whether the input in hand is copy mode's or the pane's.
async fn settle_copy_mode(
    graph: &GraphHandle,
    io_state: &Arc<Mutex<PaneIoState>>,
    session: &Arc<Mutex<AttachedSession>>,
    client_size: &ClientSize,
    config: &ConfigHandle,
) -> bool {
    let attached = session.lock().await.clone();
    let Some(pane) = attached.copy_mode.as_ref().map(|cm| cm.pane) else {
        return false;
    };
    let rect = pane_rect_for(graph, &attached, client_size, config, pane).await;
    let state = io_state.lock().await;
    let mut s = session.lock().await;
    let active = s.active_pane_id;
    let Some(cm) = s.copy_mode.as_mut() else {
        return false;
    };
    let now = shux_vt::monotonic_now_ns();
    if resolve_copy_mode(cm, active, rect, now, &state).is_some() {
        return true;
    }
    s.copy_mode = None;
    state.render_pulse.bump();
    false
}

#[expect(clippy::too_many_arguments, reason = "independent inputs")]
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
    frame: Option<u64>,
) -> anyhow::Result<bool> {
    let attached = session.lock().await.clone();
    let Some(cm) = attached.copy_mode.as_ref() else {
        if matches!(*dragging, SelectionDrag::CopyMode) {
            *dragging = SelectionDrag::None;
        }
        return Ok(false);
    };
    let rect = pane_rect_for(graph, &attached, client_size, config, cm.pane).await;

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
            if cm.wheel_initiated && cursor_pane != cm.pane {
                session.lock().await.copy_mode = None;
                *dragging = SelectionDrag::None;
                return Ok(false);
            }
            let state = io_state.lock().await;
            let mut s = session.lock().await;
            let active = s.active_pane_id;
            // A wheel-opened view hands the keyboard back once the wheel
            // brings it down to where it began; a deliberate copy mode
            // (`Prefix [`, or a search since) stays, so an in-progress
            // selection or search is never lost.
            let mut mapped = false;
            let keep = s.copy_mode.as_mut().is_some_and(|cm| {
                let now = shux_vt::monotonic_now_ns();
                let Some(vt) = resolve_copy_mode(cm, active, rect, now, &state) else {
                    return false;
                };
                mapped = true;
                if kind == MouseKind::ScrollUp {
                    shux_ui::copy_mode::scroll_up(cm, 3);
                    true
                } else {
                    shux_ui::copy_mode::scroll_down(cm, vt, 3);
                    !(cm.wheel_initiated && cm.view.top >= cm.view.entry_top)
                }
            });
            if !keep {
                s.copy_mode = None;
            }
            *dragging = SelectionDrag::None;
            // Found gone since routing began: the tick is the app's.
            if !mapped {
                return Ok(false);
            }
        }
        MouseKind::Down | MouseKind::Drag | MouseKind::Up
            if button == ProtoMouseButton::Left
                && (kind == MouseKind::Down || matches!(*dragging, SelectionDrag::CopyMode)) =>
        {
            // The cell is named through the copy view the stamped frame drew.
            let drawn = attached
                .shown
                .shown(frame)
                .and_then(|shown| shown.modal)
                .filter(|(pane, _)| *pane == cm.pane);
            let state = io_state.lock().await;
            let mut s = session.lock().await;
            let active = s.active_pane_id;
            let mut yank = None;
            let mut mapped = false;
            let keep = s.copy_mode.as_mut().is_some_and(|cm| {
                let now = shux_vt::monotonic_now_ns();
                let Some(vt) = resolve_copy_mode(cm, active, rect, now, &state) else {
                    return false;
                };
                mapped = true;
                // A frame from before a reopen, a page, a pan or the eviction
                // of its lines drew another view: the gesture it names is
                // dropped.
                let Some(view) = drawn
                    .and_then(|(_, v)| v.rebased_intact(vt))
                    .filter(|v| *v == cm.view)
                else {
                    if matches!(*dragging, SelectionDrag::CopyMode) {
                        cm.anchor = None;
                    }
                    *dragging = SelectionDrag::None;
                    return true;
                };
                let r = view.rect();
                if kind == MouseKind::Down && !point_in_rect(r, col, row) {
                    *dragging = SelectionDrag::None;
                    return true;
                }
                cm.set_cursor(view.source(pane_local_point_clamped(r, col, row)));
                match kind {
                    MouseKind::Down => {
                        cm.anchor = Some(cm.cursor);
                        *dragging = SelectionDrag::CopyMode;
                        true
                    }
                    MouseKind::Up => {
                        *dragging = SelectionDrag::None;
                        if cm.anchor.is_some_and(|anchor| anchor != cm.cursor) {
                            yank = Some(shux_ui::copy_mode::extract_selection(vt, cm));
                            false
                        } else {
                            cm.anchor = None;
                            true
                        }
                    }
                    _ => true,
                }
            });
            if !keep {
                s.copy_mode = None;
            }
            drop(s);
            drop(state);
            if !mapped {
                *dragging = SelectionDrag::None;
                return Ok(false);
            }
            if let Some(text) = yank.filter(|t| !t.is_empty()) {
                let osc = shux_ui::osc52_copy(&text);
                let frame = AttachServerFrame::Render {
                    data: BASE64.encode(&osc),
                    frame: None,
                };
                let _ = out_tx.send(frame).await;
            }
        }
        MouseKind::Down | MouseKind::Drag => {}
        MouseKind::Up => {
            *dragging = SelectionDrag::None;
        }
        _ => {}
    }

    Ok(true)
}

/// Which of the app, a mouse selection or copy mode took a mouse event.
#[derive(Debug, PartialEq, Eq)]
enum MouseRoute {
    App {
        redraw: bool,
        input: bool,
    },
    Selection,
    CopyMode,
    /// None of them: the wheel, focus and border handling come next.
    Unrouted,
}

/// Offer a mouse event to the app, a mouse selection and copy mode, in that
/// order; once more when copy mode turns out to have stopped mapping since
/// the event was first offered, so it reaches whoever takes it without one.
#[expect(clippy::too_many_arguments, reason = "independent inputs")]
async fn route_mouse(
    kind: MouseKind,
    button: ProtoMouseButton,
    (col, row): (u16, u16),
    (shift, alt, ctrl): (bool, bool, bool),
    frame: Option<u64>,
    graph: &GraphHandle,
    io_state: &Arc<Mutex<PaneIoState>>,
    session: &Arc<Mutex<AttachedSession>>,
    client_size: &ClientSize,
    config: &ConfigHandle,
    out_tx: &mpsc::Sender<AttachServerFrame>,
    border_drag: &Option<DragState>,
    gesture: &mut SelectionDrag,
) -> MouseRoute {
    for _ in 0..2 {
        let had_copy_mode = session.lock().await.copy_mode.is_some();
        let route = route_mouse_once(
            kind,
            button,
            (col, row),
            (shift, alt, ctrl),
            frame,
            graph,
            io_state,
            session,
            client_size,
            config,
            out_tx,
            border_drag,
            gesture,
        )
        .await;
        let ended = had_copy_mode && session.lock().await.copy_mode.is_none();
        if route != MouseRoute::Unrouted || !ended {
            return route;
        }
    }
    MouseRoute::Unrouted
}

#[expect(clippy::too_many_arguments, reason = "independent inputs")]
async fn route_mouse_once(
    kind: MouseKind,
    button: ProtoMouseButton,
    (col, row): (u16, u16),
    (shift, alt, ctrl): (bool, bool, bool),
    frame: Option<u64>,
    graph: &GraphHandle,
    io_state: &Arc<Mutex<PaneIoState>>,
    session: &Arc<Mutex<AttachedSession>>,
    client_size: &ClientSize,
    config: &ConfigHandle,
    out_tx: &mpsc::Sender<AttachServerFrame>,
    border_drag: &Option<DragState>,
    gesture: &mut SelectionDrag,
) -> MouseRoute {
    let unhandled = |error| {
        warn!(?kind, %error, "attach: mouse handle failed");
        false
    };
    match handle_app_mouse(
        kind,
        button,
        col,
        row,
        shift,
        alt,
        ctrl,
        frame,
        graph,
        io_state,
        session,
        client_size,
        config,
        border_drag,
        gesture,
    )
    .await
    {
        Ok(AppMouse::Consumed { redraw, input }) => return MouseRoute::App { redraw, input },
        Ok(AppMouse::NotHandled) => {}
        Err(error) => {
            unhandled(error);
        }
    }
    let selection = handle_mouse_selection(
        kind,
        button,
        col,
        row,
        graph,
        io_state,
        session,
        client_size,
        config,
        out_tx,
        gesture,
        frame,
    );
    if selection.await.unwrap_or_else(unhandled) {
        return MouseRoute::Selection;
    }
    let copy_mode = handle_copy_mode_mouse(
        kind,
        button,
        col,
        row,
        graph,
        io_state,
        session,
        client_size,
        config,
        out_tx,
        gesture,
        frame,
    );
    if copy_mode.await.unwrap_or_else(unhandled) {
        return MouseRoute::CopyMode;
    }
    MouseRoute::Unrouted
}

#[expect(clippy::too_many_arguments, reason = "independent inputs")]
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
    frame: Option<u64>,
) -> anyhow::Result<bool> {
    let attached = session.lock().await.clone();
    let viewport = current_viewport(client_size, config).await;
    let snap = graph.snapshot();
    let win = match snap.windows.get(&attached.active_window_id) {
        Some(w) => w,
        None => return Ok(false),
    };
    // Don't treat clicks while zoomed as layout edits — there are no
    // real borders to grab and only one pane to focus.
    if win.layout.is_zoomed() {
        return Ok(false);
    }
    let tree = &win.layout.tree;

    let mut acted = false;
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
                acted = true;
            } else if let Some((pid, _)) = pane_at(tree, viewport, col, row) {
                if pid != attached.active_pane_id {
                    let _ = graph.focus_pane(pid).await;
                    let mut s = session.lock().await;
                    s.active_pane_id = pid;
                    // Focus moves either way; only a click on a cell the
                    // client was shown is input that may take the panes.
                    acted = attached.shown.views(frame).is_none_or(|views| {
                        views
                            .get(&pid)
                            .is_some_and(|view| view_point(view, col, row, false).is_some())
                    });
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
                acted = true;
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
    Ok(acted)
}

/// A button event, as an app's mouse report understands it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ButtonAction {
    Press,
    Release,
    /// Motion with a button held.
    Drag,
}

/// Which xterm modes report `action`. Mode 1000 (`Normal`) subscribes to press
/// and release only; forwarding a drag there reports an event the app never
/// asked for. `MouseMode::None` reports nothing, so such a pane keeps shux's own
/// mouse handling. Hover (1003's extra) is absent because the host profile does
/// not enable 1003, so crossterm never produces one.
fn mode_reports(mode: shux_vt::MouseMode, action: ButtonAction) -> bool {
    use shux_vt::MouseMode;
    match mode {
        MouseMode::None => false,
        MouseMode::Normal => action != ButtonAction::Drag,
        MouseMode::ButtonEvent | MouseMode::AnyEvent => true,
    }
}

/// Whether shux can encode a coordinate this pane's app will read back as the
/// cell the user clicked. shux emits X10 or SGR (1006); an app that also asked
/// for 1005, 1015 or 1016 decodes those bytes as something else — 1016 reads
/// cells as pixels and collapses every click into the top-left corner. A pane
/// that fails this keeps shux's own mouse handling.
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
#[expect(clippy::too_many_arguments, reason = "independent inputs")]
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
    // Only a press opens a gesture; a stray drag or release is shux's.
    if action != ButtonAction::Press {
        return AppMouseRoute::Shux;
    }
    // Shift is the user taking the mouse back.
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

/// Tell the app every held button came up, then end the gesture. Called wherever
/// a forwarded gesture is abandoned rather than completed — help overlay,
/// detach, session end, the pane's rect vanishing under a zoom or window switch
/// — so the app is never left believing a button is down.
async fn release_app_gesture(
    io_state: &Arc<Mutex<PaneIoState>>,
    gesture: &mut SelectionDrag,
    client: Option<shux_core::event::ClientId>,
) {
    let SelectionDrag::App {
        pane_id,
        buttons,
        last,
        ..
    } = *gesture
    else {
        return;
    };
    *gesture = SelectionDrag::None;
    // The grid may have shrunk since `last`: release inside what it has now.
    let (sgr, last) = {
        let state = io_state.lock().await;
        match state.vts.get(&pane_id) {
            Some(vt) => {
                let fit = |v: u16, n: usize| v.min(u16::try_from(n).unwrap_or(u16::MAX)).max(1);
                let grid = vt.grid();
                (
                    vt.modes().sgr_mouse,
                    (fit(last.0, grid.cols()), fit(last.1, grid.rows())),
                )
            }
            None => (false, last),
        }
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
            forward_bytes_to_pane(io_state, pane_id, bytes, Lane::Urgent, client).await;
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
    /// compositor frame per motion event. `input` is true when the event
    /// reached the app or moved focus: only that may make this client drive.
    Consumed { redraw: bool, input: bool },
}

fn holds(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height
}

/// Forward a press / drag / release to a mouse-aware app in the pane under the
/// pointer.
///
/// Sits ahead of shux's selection handling in the attach loop: inside a pane
/// whose app asked for the mouse, the mouse is the app's. `handle_wheel` stays
/// the sole authority on scroll ticks — this returns [`AppMouse::NotHandled`]
/// for them so the scrollback, alt-scroll and forward tiers keep working.
#[expect(clippy::too_many_arguments, reason = "independent inputs")]
async fn handle_app_mouse(
    kind: MouseKind,
    button: ProtoMouseButton,
    col: u16,
    row: u16,
    shift: bool,
    alt: bool,
    ctrl: bool,
    frame: Option<u64>,
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

    // Resolve the pane under the pointer in the frame the client was shown: a
    // layout not drawn yet must not move a click into another pane. Views are
    // content rects, so borders, the outline and the status bar hit none.
    // A frame no longer kept cannot be mapped into an app: a client that far
    // behind (small frames fill the socket's byte buffers) gets shux's own
    // handling at the live layout, as before frames were stamped. A gesture
    // an app holds still gets its release.
    let Some(views) = attached.shown.views(frame) else {
        if matches!(*gesture, SelectionDrag::App { .. }) {
            if action == ButtonAction::Release {
                release_app_gesture(io_state, gesture, attached.client).await;
            }
            return Ok(AppMouse::Consumed {
                redraw: false,
                input: false,
            });
        }
        // Over a pane whose app reports the mouse, the press is not shux's to
        // select with: it only focuses that pane.
        if matches!(*gesture, SelectionDrag::None) && border_drag.is_none() && !shift {
            let viewport = current_viewport(client_size, config).await;
            let live = {
                let snap = graph.snapshot();
                snap.windows
                    .get(&attached.active_window_id)
                    .and_then(|win| match &win.layout.zoom {
                        Some(zoom) => Some(zoom.zoomed_pane),
                        None => pane_at(&win.layout.tree, viewport, col, row).map(|(p, _)| p),
                    })
            };
            let tracked = match live {
                Some(pid) => io_state
                    .lock()
                    .await
                    .vts
                    .get(&pid)
                    .is_some_and(|vt| vt.modes().mouse_tracking != shux_vt::MouseMode::None),
                None => false,
            };
            if let (Some(pid), true) = (live, tracked) {
                let redraw = action == ButtonAction::Press
                    && select_pane_on_press(graph, session, attached.active_pane_id, pid).await;
                let input = action == ButtonAction::Press && pid != attached.active_pane_id;
                return Ok(AppMouse::Consumed { redraw, input });
            }
        }
        return Ok(AppMouse::NotHandled);
    };
    let pane_hit = views
        .iter()
        .find(|(_, view)| holds(view.rect, col, row))
        .map(|(pid, _)| *pid);
    // A pane closed, hidden by a zoom or in another window since the frame
    // was drawn takes nothing more: what shux would do there now is not what
    // the user aimed at.
    if let Some(pid) = pane_hit
        && matches!(*gesture, SelectionDrag::None)
        && border_drag.is_none()
        && !pane_is_shown(graph, &attached, pid)
    {
        return Ok(AppMouse::Consumed {
            redraw: false,
            input: false,
        });
    }

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
        // The app owns the mouse but did not subscribe to this event; consume
        // it rather than letting shux act underneath a running TUI. An app that
        // is STILL tracking (it only switched to a coordinate mode shux cannot
        // encode) is still waiting for the button: send the release anyway,
        // because a stuck button does not recover and a mis-placed release does.
        AppMouseRoute::Swallow => {
            if action == ButtonAction::Release && mode != shux_vt::MouseMode::None {
                release_app_gesture(io_state, gesture, attached.client).await;
            } else {
                end_gesture_if_released(gesture, action, button);
            }
            return Ok(AppMouse::Consumed {
                redraw: false,
                input: false,
            });
        }
        AppMouseRoute::Forward(pane_id) => pane_id,
    };

    // The pane can lose its rect mid-gesture: a window switch or a zoom leaves
    // it off-screen. Let the app know the button came up rather than letting
    // the gesture evaporate with it still held.
    if !pane_is_shown(graph, &attached, pane_id) {
        release_app_gesture(io_state, gesture, attached.client).await;
        return Ok(AppMouse::Consumed {
            redraw: false,
            input: false,
        });
    }
    let shown = views.get(&pane_id).copied();
    if !shown.is_some_and(|view| holds(view.rect, col, row))
        && matches!(*gesture, SelectionDrag::None)
    {
        return Ok(AppMouse::NotHandled);
    }
    // A press that opens a gesture names a cell or nothing. Once a gesture is
    // underway none of it is swallowed: every event is pinned to the pane as
    // drawn now (and below, within legacy encoding's reach), so the app sees
    // every press and every release names a button it holds.
    let pin = !matches!(*gesture, SelectionDrag::None);
    let view = match *gesture {
        SelectionDrag::App {
            offsets: (row_offset, col_offset),
            anchor,
            ..
        } if pin && anchor == (col, row) => {
            shown.map(|view| view.with_offsets(row_offset, col_offset))
        }
        _ => shown,
    };
    let moves_focus = action == ButtonAction::Press && pane_id != attached.active_pane_id;
    let cell = view.and_then(|view| {
        shux_ui::viewport::pane_grid_cell(&view, (col, row), pin).map(|cell| (view, cell))
    });
    let bit = button_bit(button);
    // A release the gesture cannot place, or of a button it does not hold (a
    // legacy host reports every release as left, or the chord's first press
    // named no cell and opened no gesture): release what the app holds. It may
    // come early; it never strands a button or sends an orphan.
    if action == ButtonAction::Release
        && matches!(*gesture, SelectionDrag::App { buttons, .. }
            if cell.is_none() || buttons & bit == 0)
    {
        if let (Some((_, at)), SelectionDrag::App { last, .. }) = (cell, &mut *gesture) {
            *last = at;
        }
        release_app_gesture(io_state, gesture, attached.client).await;
        return Ok(AppMouse::Consumed {
            redraw: false,
            input: false,
        });
    }
    let Some((view, (local_col, local_row))) = cell else {
        // No cell (surplus, or past the grid): a press only selects the pane,
        // and motion is dropped. It reached no app, so it is no input: taking
        // the panes for it would resize everyone's view for a click on nothing.
        let redraw = action == ButtonAction::Press
            && select_pane_on_press(graph, session, attached.active_pane_id, pane_id).await;
        return Ok(AppMouse::Consumed {
            redraw,
            input: false,
        });
    };

    let Some(cb) = button_cb(action, button, alt, ctrl) else {
        // No truthful report exists for a buttonless press or release, but a
        // release must still clear the latch or the app sits button-held for
        // the rest of the pane's life.
        end_gesture_if_released(gesture, action, button);
        return Ok(AppMouse::NotHandled);
    };
    // The stamped frame can predate a resize. A gesture underway stays on the
    // grid the app has now; a press off it names no cell the app has.
    let live = io_state
        .lock()
        .await
        .vts
        .get(&pane_id)
        .map(|vt| (vt.grid().cols(), vt.grid().rows()));
    let (local_col, local_row) = match live {
        Some((cols, rows)) => {
            let fit = |v: u16, n: usize| v.min(u16::try_from(n).unwrap_or(u16::MAX));
            let on_grid = usize::from(local_col) <= cols && usize::from(local_row) <= rows;
            if !on_grid && !pin {
                let redraw = action == ButtonAction::Press
                    && select_pane_on_press(graph, session, attached.active_pane_id, pane_id).await;
                return Ok(AppMouse::Consumed {
                    redraw,
                    input: false,
                });
            }
            (fit(local_col, cols), fit(local_row, rows))
        }
        None => (local_col, local_row),
    };
    let (local_col, local_row) = if pin && !sgr {
        (
            local_col.min(X10_MOUSE_LIMIT),
            local_row.min(X10_MOUSE_LIMIT),
        )
    } else {
        (local_col, local_row)
    };
    let release = action == ButtonAction::Release;
    let Some(bytes) = encode_mouse_report(cb, release, sgr, local_col, local_row) else {
        // Not encodable for this app's coordinate mode (only a press opening a
        // gesture can be). Consume it: the pane is still the app's, and a stray
        // shux selection under a running TUI is not a better answer than
        // nothing happening.
        let redraw = action == ButtonAction::Press
            && select_pane_on_press(graph, session, attached.active_pane_id, pane_id).await;
        return Ok(AppMouse::Consumed {
            redraw,
            input: moves_focus,
        });
    };

    let redraw = action == ButtonAction::Press
        && select_pane_on_press(graph, session, attached.active_pane_id, pane_id).await;

    // A release must reach an app that saw its press; motion is worth
    // dropping.
    let lane = match action {
        ButtonAction::Press => Lane::Control,
        ButtonAction::Release => Lane::Urgent,
        ButtonAction::Drag => Lane::Main,
    };
    if !forward_bytes_to_pane(io_state, pane_id, bytes, lane, attached.client).await {
        // Dropped on the floor. Opening a gesture now would hand the app a
        // release with no press behind it.
        return Ok(AppMouse::Consumed {
            redraw,
            input: moves_focus,
        });
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
            offsets: (view.row_offset, view.col_offset),
            anchor: (col, row),
        };
    } else {
        // Where the pointer was when the gesture is abandoned, so
        // `release_app_gesture` can put the synthetic release there.
        if let SelectionDrag::App {
            last,
            offsets,
            anchor,
            ..
        } = gesture
        {
            *last = (local_col, local_row);
            *offsets = (view.row_offset, view.col_offset);
            *anchor = (col, row);
        }
        end_gesture_if_released(gesture, action, button);
    }
    Ok(AppMouse::Consumed {
        redraw,
        input: true,
    })
}

/// What a press inside an app's pane does on shux's side; true when that is a
/// visible change.
///
/// tmux's `MouseDown1Pane` is `select-pane` followed by `send-keys -M`: the
/// click both focuses the pane and reaches the app. A selection left over from
/// before the app took the mouse can no longer be cleared by clicking, so it is
/// cleared here or it renders forever.
async fn select_pane_on_press(
    graph: &GraphHandle,
    session: &Arc<Mutex<AttachedSession>>,
    active_pane_id: PaneId,
    pane_id: PaneId,
) -> bool {
    let mut redraw = false;
    if pane_id != active_pane_id && graph.focus_pane(pane_id).await.is_ok() {
        session.lock().await.active_pane_id = pane_id;
        redraw = true;
    }
    let mut s = session.lock().await;
    if s.mouse_selection.is_some() || s.copy_menu.is_some() {
        s.mouse_selection = None;
        s.copy_menu = None;
        redraw = true;
    }
    redraw
}

/// The pane a session shows focused now, from the graph.
fn focused_pane(graph: &GraphHandle, session: SessionId) -> Option<PaneId> {
    let snap = graph.snapshot();
    let window = snap.sessions.get(&session)?.active_window;
    snap.windows.get(&window).map(|w| w.active_pane)
}

/// Copy the session's live focus into this client's cache. `false` once the
/// session is gone.
async fn sync_focus(
    graph: &GraphHandle,
    session: &mut AttachedSession,
    render_session: &Arc<Mutex<AttachedSession>>,
) -> bool {
    let snap = graph.snapshot();
    let Some(s) = snap.sessions.get(&session.session_id) else {
        return false;
    };
    session.active_window_id = s.active_window;
    if let Some(w) = snap.windows.get(&s.active_window) {
        session.active_pane_id = w.active_pane;
    }
    let mut rs = render_session.lock().await;
    rs.active_window_id = session.active_window_id;
    rs.active_pane_id = session.active_pane_id;
    true
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

/// Last cell the X10 byte-packed form can name: each value is offset by 32 and
/// packed into one byte, so 223 + 32 = 255.
const X10_MOUSE_LIMIT: u16 = 223;

/// Encode one mouse report for a pane's application: wheel, press, drag, release.
///
/// `cb` is the xterm button code before any encoding-specific rewrite: 0/1/2
/// left/middle/right, `+32` motion, 3+32 motion with no button held, 64/65
/// wheel up/down, plus modifier bits (shift 4, alt 8, ctrl 16). `col`/`row` are
/// **1-based pane-local** cells.
///
/// SGR (1006) ends a release with `m` and keeps the real button; legacy X10
/// cannot, so every X10 release is `Cb = 3` with the modifier bits kept.
///
/// `None` — do not forward — for a legacy coordinate past [`X10_MOUSE_LIMIT`]
/// or a 1-based coordinate of `0`. Clamping instead would name a cell the user
/// did not click. A legacy release is the exception: it is pinned within
/// reach, because an app waiting on a release recovers from one a few cells
/// off, never from one that is missing.
fn encode_mouse_report(cb: u16, release: bool, sgr: bool, col: u16, row: u16) -> Option<Vec<u8>> {
    let (col, row) = if release && !sgr {
        (col.min(X10_MOUSE_LIMIT), row.min(X10_MOUSE_LIMIT))
    } else {
        (col, row)
    };
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

/// Send bytes to a pane's PTY writer without waiting: dropped when `lane`
/// has no room. A mouse report is worth less than a held attach loop.
///
/// Returns whether the bytes were actually queued. Button forwarding needs to
/// know: a press that was dropped must not open a gesture, or the release that
/// follows reaches the app with no matching press and leaves it button-held.
async fn forward_bytes_to_pane(
    io_state: &Arc<Mutex<PaneIoState>>,
    pane_id: PaneId,
    bytes: Vec<u8>,
    lane: Lane,
    client: Option<shux_core::event::ClientId>,
) -> bool {
    let mut state = io_state.lock().await;
    let Some(writer) = state.writers.get(&pane_id) else {
        return false;
    };
    let mut permit = match writer.try_reserve_in(bytes.len(), lane) {
        Ok(permit) => permit,
        Err(e) => {
            tracing::warn!(error = %e, "mouse forward dropped (pane backpressured)");
            return false;
        }
    };
    // Urgent is a release: what a takeover resize must not overtake.
    if lane == Lane::Urgent
        && let Some(client) = client
    {
        let written = permit.watch();
        state
            .mouse_release_written
            .insert((pane_id, client), written);
    }
    state.send_outside_paste(pane_id, permit, bytes)
}

/// Whether the last mouse release `client` sent `pane` was written within a
/// bounded wait. The PTY applies a resize ahead of queued input, so a takeover
/// is not issued until it was: one not written by then is dropped.
async fn release_written(
    io_state: &Arc<Mutex<PaneIoState>>,
    pane: PaneId,
    client: shux_core::event::ClientId,
) -> bool {
    let waiting = io_state
        .lock()
        .await
        .mouse_release_written
        .remove(&(pane, client));
    match waiting {
        Some(written) => tokio::time::timeout(RELEASE_WRITE_BOUND, written)
            .await
            .is_ok(),
        None => true,
    }
}

/// The longest a takeover waits for a release to reach a program that is
/// not reading its input.
const RELEASE_WRITE_BOUND: Duration = Duration::from_millis(100);

/// Handle a scroll-wheel event while copy mode is NOT active. Returns `Ok(true)`
/// if the event was a wheel event (and thus consumed), `Ok(false)` otherwise so
/// the caller falls through to the layout/focus mouse handler.
#[expect(clippy::too_many_arguments, reason = "independent inputs")]
async fn handle_wheel(
    kind: MouseKind,
    col: u16,
    row: u16,
    frame: Option<u64>,
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

    // Resolve the pane under the cursor in the frame the client was shown,
    // or the live layout for a frame no longer kept (fallback: the active
    // pane). Only a shown frame can place a report in an app.
    let views = attached.shown.views(frame);
    let hit = match views {
        Some(views) => views
            .iter()
            .find(|(_, view)| holds(view.rect, col, row))
            .map(|(pid, _)| *pid),
        None => {
            let viewport = current_viewport(client_size, config).await;
            let snap = graph.snapshot();
            snap.windows
                .get(&attached.active_window_id)
                .and_then(|win| match &win.layout.zoom {
                    Some(zoom) => Some(zoom.zoomed_pane),
                    None => pane_at(&win.layout.tree, viewport, col, row).map(|(pid, _)| pid),
                })
        }
    };
    let pane_id = hit.unwrap_or(attached.active_pane_id);
    let Some(rect) = pane_rect_for(graph, &attached, client_size, config, pane_id).await else {
        return Ok(true);
    };

    // Snapshot the target pane's live mode state. A pane shux cannot encode
    // for is treated as not mouse-aware, so the wheel falls back to shux's
    // own scrollback. See `coords_are_encodable`.
    let (mouse_on, sgr, alt, alt_scroll, app_cursor) = {
        let state = io_state.lock().await;
        match state.vts.get(&pane_id) {
            Some(vt) => {
                let m = vt.modes();
                (
                    m.mouse_tracking != shux_vt::MouseMode::None && coords_are_encodable(m),
                    m.sgr_mouse,
                    vt.is_alternate_screen(),
                    m.alternate_scroll,
                    m.application_cursor_keys,
                )
            }
            None => (false, false, false, true, false),
        }
    };

    match route_wheel(mouse_on, alt, alt_scroll) {
        WheelRouting::ForwardMouse => {
            let cell = views
                .and_then(|views| views.get(&pane_id))
                .and_then(|view| shux_ui::viewport::pane_grid_cell(view, (col, row), false));
            // No grid cell under the pointer, or no truthful encoding for this
            // app's coordinate mode: sending nothing beats a tick aimed at a
            // cell the user never touched.
            if let Some(bytes) = cell.and_then(|(local_col, local_row)| {
                encode_mouse_wheel(up, sgr, local_col, local_row)
            }) {
                forward_bytes_to_pane(io_state, pane_id, bytes, Lane::Main, None).await;
            }
        }
        WheelRouting::ArrowKeys => {
            let seq = wheel_arrow_seq(up, app_cursor);
            let mut bytes = Vec::with_capacity(seq.len() * 3);
            for _ in 0..3 {
                bytes.extend_from_slice(seq);
            }
            forward_bytes_to_pane(io_state, pane_id, bytes, Lane::Main, None).await;
        }
        WheelRouting::Scrollback => {
            // This tier is only reached while copy mode is INACTIVE (an active
            // copy mode consumes the wheel earlier, in handle_copy_mode_mouse).
            // So wheel-down here has nothing to scroll — only wheel-up opens a
            // transient, wheel-initiated scrollback view.
            if !up {
                return Ok(true);
            }
            // A stamp older than every frame kept names nothing, as for `prefix [`.
            if frame.is_some() && attached.shown.shown(frame).is_none() {
                session.lock().await.action_error = Some(
                    "action failed: the screen changed before copy mode opened; scroll again"
                        .into(),
                );
                return Ok(true);
            }
            // Copy mode opens over the slice the pointer was over, taken
            // before focus moves; with nothing above it, not at all.
            let entered = {
                let state = io_state.lock().await;
                copy_mode_over(pane_id, attached.shown.shown(frame), Some(rect), &state)
                    .filter(|cm| cm.view.entry_top > 0)
            };
            let Some(mut cm) = entered else {
                return Ok(true);
            };
            cm.wheel_initiated = true;
            shux_ui::copy_mode::scroll_up(&mut cm, 3);
            // Scrolling a background pane focuses it (the overlay renders for the
            // active pane), matching tmux.
            if pane_id != attached.active_pane_id {
                let _ = graph.focus_pane(pane_id).await;
            }
            let mut s = session.lock().await;
            s.active_pane_id = pane_id;
            s.copy_mode = Some(cm);
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

/// The rect `pane_id` is drawn in at the client's size now: the whole
/// content area while it is zoomed, `None` while the window does not draw it.
async fn pane_rect_for(
    graph: &GraphHandle,
    attached: &AttachedSession,
    client_size: &ClientSize,
    config: &ConfigHandle,
    pane_id: PaneId,
) -> Option<Rect> {
    let content = current_content_rect(client_size).await;
    let viewport = current_viewport(client_size, config).await;
    pane_rect_in(graph, attached, content, viewport, pane_id)
}

/// `pane_rect_for` with the geometry already resolved, so a caller that has
/// already computed the viewport for this event hit-tests against that one
/// rather than re-reading the live config and getting a second answer.
/// Whether the client's active window draws `pane_id` now.
fn pane_is_shown(graph: &GraphHandle, attached: &AttachedSession, pane_id: PaneId) -> bool {
    let snap = graph.snapshot();
    snap.windows
        .get(&attached.active_window_id)
        .is_some_and(|win| match &win.layout.zoom {
            Some(zoom) => zoom.zoomed_pane == pane_id,
            None => win.layout.tree.contains_pane(pane_id),
        })
}

fn pane_rect_in(
    graph: &GraphHandle,
    attached: &AttachedSession,
    content: Rect,
    viewport: Rect,
    pane_id: PaneId,
) -> Option<Rect> {
    let snap = graph.snapshot();
    let win = snap.windows.get(&attached.active_window_id)?;
    // A zoom draws its zoomed pane, which this client's cached focus may not
    // have caught up with yet.
    if let Some(zoom) = &win.layout.zoom {
        return (pane_id == zoom.zoomed_pane).then_some(content);
    }
    for (pid, rect) in win.layout.compute_rects(viewport) {
        if pid == pane_id {
            return Some(rect);
        }
    }
    None
}

/// Copy a mouse selection's text to the client, read from the slice it was
/// made over. Nothing when the pane shows another slice now: those cells are
/// not what was highlighted.
async fn yank_selection(
    selection: &MouseSelection,
    shown: &ShownFrames,
    io_state: &Arc<Mutex<PaneIoState>>,
    out_tx: &mpsc::Sender<AttachServerFrame>,
) -> bool {
    let current = shown.views(None).and_then(|v| v.get(&selection.pane_id));
    if current != Some(&selection.view) {
        return false;
    }
    let text = {
        let state = io_state.lock().await;
        // The grid as it is now, not as the last frame drew it: a resize may
        // have landed since, and the slice the selection was made over is gone.
        let Some(vt) = state.vts.get(&selection.pane_id).filter(|vt| {
            PaneViewport::of(selection.view.rect, vt, shux_vt::monotonic_now_ns()) == selection.view
        }) else {
            return false;
        };
        shux_ui::copy_mode::extract_frame_selection(vt, &selection.state, &selection.view)
    };
    if text.is_empty() {
        return false;
    }
    let osc = shux_ui::osc52_copy(&text);
    let frame = AttachServerFrame::Render {
        data: BASE64.encode(&osc),
        frame: None,
    };
    out_tx.send(frame).await.is_ok()
}

/// The cell under screen `(col, row)` in `view`, local to the slice: `None`
/// off every shown cell, unless `clamp` pins it onto the nearest.
fn view_point(view: &PaneViewport, col: u16, row: u16, clamp: bool) -> Option<(u16, u16)> {
    let (c, r) = shux_ui::viewport::pane_grid_cell(view, (col, row), clamp)?;
    Some((
        c - 1 - view.col_offset as u16,
        r - 1 - view.row_offset as u16,
    ))
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

/// The pane viewport at the current client size: the content area inset for the
/// outline (when one is drawn) and the status bar. Used by spatial actions
/// (focus_dir, smart split) and by every mouse hit-test, so their geometry
/// matches what the compositor drew. Read live rather than cached — a config
/// read is an ArcSwap load, and a cache here went stale in both directions.
///
/// `zoomed: false`: `SplitSmart` is the one caller that reaches here while
/// zoomed, and it only compares `width >= height`, which the inset preserves.
async fn current_viewport(client_size: &ClientSize, config: &ConfigHandle) -> Rect {
    let border_style = BorderStyle::parse(&config.current().appearance.border_style);
    shux_ui::pane_viewport(current_content_rect(client_size).await, border_style, false)
}

/// Dispatch an Action keybinding from the client.
#[expect(clippy::too_many_arguments, reason = "independent inputs")]
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

    // EnterCopyMode is also a UI-only flip: copy mode on the focused pane,
    // over the slice the frame the client stamped showed. Pressed again
    // while open, it changes nothing.
    if matches!(kind, ActionKind::EnterCopyMode) {
        let attached = session.lock().await.clone();
        let pane = attached.active_pane_id;
        let rect = pane_rect_for(graph, &attached, client_size, config, pane).await;
        let state = io_state.lock().await;
        let mut s = session.lock().await;
        if s.copy_mode.is_some() {
            return Ok(());
        }
        // A stamp older than every frame kept names nothing.
        let expired = args.frame.is_some() && s.shown.shown(args.frame).is_none();
        let entered = (!expired)
            .then(|| copy_mode_over(pane, s.shown.shown(args.frame), rect, &state))
            .flatten();
        // Refused, the notice changes the status bar, so a frame the client
        // can stamp afresh goes out.
        let cm = anyhow::Context::context(
            entered,
            "the screen changed before copy mode opened; press it again",
        )?;
        s.copy_mode = Some(cm);
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

    let (new_pane, prior) = crate::pane_spawn::split_noting_undo(
        graph,
        attached.active_pane_id,
        direction,
        0.5,
        Vec::new(),
    )
    .await?;

    // A PTY that never started is not a pane: left in the graph it is focused,
    // drawn, and answers "pane VT not found" to every later verb. A default
    // pane can fail to spawn because `[shell].command` names the program.
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
        crate::pane_spawn::undo_split(graph, new_pane, prior).await;
        return Err(
            anyhow::anyhow!("{}", crate::pane_spawn::spawn_failure_message(&e))
                .context(crate::pane_spawn::spawn_failure_notice(&e)),
        );
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
    let _ = graph.zoom_pane(attached.active_pane_id, None).await?;
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
    // attach frame loop notices the session is gone on its next tick
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
    pulse.bump();
}

async fn new_window(
    graph: &GraphHandle,
    attached: &AttachedSession,
    io_state: &Arc<Mutex<PaneIoState>>,
    cancel: &CancellationToken,
    session: &Arc<Mutex<AttachedSession>>,
) -> anyhow::Result<()> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/tmp"));
    let created = crate::pane_spawn::create_window_noting_focus(
        graph,
        attached.session_id,
        None,
        cwd.clone(),
        Vec::new(),
    )
    .await
    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let (window_id, pane_id) = (created.window, created.pane);
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
        crate::pane_spawn::undo_window_create(graph, created).await;
        return Err(
            anyhow::anyhow!("{}", crate::pane_spawn::spawn_failure_message(&e))
                .context(crate::pane_spawn::spawn_failure_notice(&e)),
        );
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
    use tokio::net::UnixStream;

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

    /// Name frozen by `check-test-inventory`, which treats a test name as the
    /// identity of its coverage. The cap itself is pinned by
    /// `encode_mouse_report_refuses_legacy_coordinates_it_cannot_carry`.
    #[test]
    fn encode_mouse_wheel_x10_offsets_by_32_and_caps() {
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

    /// A coordinate the legacy form cannot carry is refused, not clamped: byte
    /// 255 is a valid report for column 223, so a clamp makes the app act on a
    /// cell the user never touched.
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

    /// What a frame with no copy mode showed.
    fn views_only<const N: usize>(views: [(PaneId, PaneViewport); N]) -> Shown {
        Shown {
            views: HashMap::from(views),
            drawn: HashMap::new(),
            modal: None,
        }
    }

    /// Copy mode on `pane` over `vt` as a frame drawing it in `rect` shows it.
    fn copy_mode_on(
        pane: PaneId,
        rect: Rect,
        vt: &shux_vt::VirtualTerminal,
    ) -> shux_ui::CopyModeState {
        let drawn = shux_ui::Drawn::of(vt);
        shux_ui::CopyModeState::enter(pane, PaneViewport::of(rect, vt, 0), drawn, vt)
            .expect("copy mode")
    }

    fn overlay_stamp(cursor: (u16, u16)) -> CopyOverlayStamp {
        let pane = PaneId::from_uuid(uuid::Uuid::from_u128(7));
        let vt = shux_vt::VirtualTerminal::new(23, 80);
        let mut state = copy_mode_on(pane, Rect::new(1, 1, 80, 23), &vt);
        state.set_cursor(state.view.source(cursor));
        CopyOverlayStamp {
            kind: CopyOverlayKind::Modal(state, false),
            pane_id: pane,
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

    /// An open help sheet over a frame that did not change sends nothing: the
    /// terminal still shows it, and redrawing it on every wake streams the
    /// whole sheet at the tick rate for as long as it is open.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_open_help_sheet_is_not_resent_over_an_unchanged_frame() {
        let fixture = attach_fixture().await;
        let onboarding = crate::onboarding::OnboardingHandle::from_state_for_test(
            crate::onboarding::OnboardingState {
                prefix_discovered: true,
                welcome_toast_seen: true,
            },
        );
        let pulse = fixture.io_state.lock().await.render_pulse.clone();
        let (tx, mut rx) = mpsc::channel(64);
        let cancel = fixture.cancel.child_token();
        let mut attached = fixture.attached.clone();
        attached.help_visible = true;
        // Past the seconds tier, so the uptime segment holds still.
        // why: a start instant handed in as data; time passing only moves it further past the tier
        // ast-grep-ignore: test-wall-clock-unit
        let daemon_start = std::time::Instant::now()
            .checked_sub(Duration::from_secs(61))
            .expect("monotonic clock past 61 s");
        let render = tokio::spawn(run_render_loop(
            fixture.graph.clone(),
            fixture.io_state.clone(),
            test_config(),
            SegmentCache::new(),
            crate::session_meta::SessionMetaCache::new(),
            onboarding.clone(),
            daemon_start,
            Arc::new(Mutex::new(attached)),
            Arc::new(Mutex::new((100, 40))),
            tx,
            cancel.clone(),
            false,
            false,
            Duration::from_secs(3600),
            WELCOME_TOAST_DWELL,
            shux_vt::monotonic_now_ns,
        ));

        let mut screen = shux_vt::VirtualTerminal::new(40, 100);
        let deadline = shux_testkit::Deadline::start();
        while !screen.capture_text(None).contains("keybindings") {
            match deadline.run(rx.recv()).await {
                Some(Some(AttachServerFrame::Render { data, .. })) => {
                    screen.process(&BASE64.decode(data).expect("render base64"));
                }
                Some(Some(_)) => {}
                Some(None) | None => panic!("the help sheet was never drawn"),
            }
        }

        // Stop the next frame after it holds `io_state`: it has read every
        // input it draws from, all unchanged.
        let held = onboarding.hold_for_test().await;
        pulse.bump();
        let io_state = fixture.io_state.clone();
        shux_testkit::wait_until_async("the idle frame to hold io_state", || {
            let io_state = io_state.clone();
            async move { io_state.try_lock().is_err() }
        })
        .await;
        while let Ok(frame) = rx.try_recv() {
            if let AttachServerFrame::Render { data, .. } = frame {
                screen.process(&BASE64.decode(data).expect("render base64"));
            }
        }
        // A change for the frame after it, so its bytes mark where the idle
        // frame's would have ended.
        fixture
            .graph
            .rename_window(fixture.first_window, format!("MA{}RK", ""), None)
            .await
            .expect("rename window");
        pulse.bump();
        drop(held);

        let mut idle_frames = 0;
        let drew = loop {
            match deadline.run(rx.recv()).await {
                Some(Some(AttachServerFrame::Render { data, .. })) => {
                    screen.process(&BASE64.decode(data).expect("render base64"));
                    if screen.capture_text(None).contains("MARK") {
                        break true;
                    }
                    idle_frames += 1;
                }
                Some(Some(_)) => {}
                Some(None) | None => break false,
            }
        };
        assert!(drew, "the change after the idle frame was never drawn");
        assert_eq!(
            idle_frames, 0,
            "a frame that changed nothing resent the help sheet"
        );

        cancel.cancel();
        render.await.expect("render loop");
        fixture.stop();
    }

    /// A bump that lands while a frame is being drawn is drawn next, even with
    /// no tick to fall back on: the loop's receiver outlives the iteration.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_bump_during_a_frame_is_drawn_by_the_next_one() {
        let fixture = attach_fixture().await;
        let onboarding = crate::onboarding::OnboardingHandle::from_state_for_test(
            crate::onboarding::OnboardingState {
                prefix_discovered: true,
                welcome_toast_seen: true,
            },
        );
        let held = onboarding.hold_for_test().await;
        let pulse = fixture.io_state.lock().await.render_pulse.clone();
        let (tx, mut rx) = mpsc::channel(64);
        let cancel = fixture.cancel.child_token();
        let render = tokio::spawn(run_render_loop(
            fixture.graph.clone(),
            fixture.io_state.clone(),
            test_config(),
            SegmentCache::new(),
            crate::session_meta::SessionMetaCache::new(),
            onboarding.clone(),
            std::time::Instant::now(),
            Arc::new(Mutex::new(fixture.attached.clone())),
            Arc::new(Mutex::new((100, 24))),
            tx,
            cancel.clone(),
            false,
            false,
            Duration::from_secs(3600),
            WELCOME_TOAST_DWELL,
            shux_vt::monotonic_now_ns,
        ));

        // The prelude follows the subscription, so from here the only
        // `io_state` holder is the first frame, stuck reading `onboarding`
        // after it took the graph snapshot.
        tokio::time::timeout(shux_testkit::BUDGET, rx.recv())
            .await
            .expect("prelude timeout")
            .expect("prelude");
        let io_state = fixture.io_state.clone();
        shux_testkit::wait_until_async("the first frame to hold io_state", || {
            let io_state = io_state.clone();
            async move { io_state.try_lock().is_err() }
        })
        .await;
        fixture
            .graph
            .rename_window(fixture.first_window, format!("MA{}RK", ""), None)
            .await
            .expect("rename window");
        pulse.bump();
        drop(held);

        let mut screen = shux_vt::VirtualTerminal::new(24, 100);
        let deadline = shux_testkit::Deadline::start();
        let drew = loop {
            if screen.capture_text(None).contains("MARK") {
                break true;
            }
            match deadline.run(rx.recv()).await {
                Some(Some(AttachServerFrame::Render { data, .. })) => {
                    screen.process(&BASE64.decode(data).expect("render base64"));
                }
                Some(Some(_)) => {}
                Some(None) | None => break false,
            }
        };
        assert!(drew, "a bump that landed mid-frame was never drawn");

        cancel.cancel();
        render.await.expect("render loop");
        fixture.stop();
    }

    /// The frame that ends the welcome toast writes the session only after it
    /// releases `io_state`. The input task takes the session and then
    /// `io_state` when a key dismisses the toast, so a frame that held
    /// `io_state` while waiting for the session froze every pane.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_frame_that_ends_the_welcome_toast_never_waits_for_the_session_holding_io_state() {
        let fixture = attach_fixture().await;
        let onboarding = crate::onboarding::OnboardingHandle::from_state_for_test(
            crate::onboarding::OnboardingState {
                prefix_discovered: true,
                welcome_toast_seen: false,
            },
        );
        let held = onboarding.hold_for_test().await;
        let (tx, mut rx) = mpsc::channel(64);
        let cancel = fixture.cancel.child_token();
        let mut attached = fixture.attached.clone();
        attached.show_welcome_toast = true;
        let session = Arc::new(Mutex::new(attached));
        let render = tokio::spawn(run_render_loop(
            fixture.graph.clone(),
            fixture.io_state.clone(),
            test_config(),
            SegmentCache::new(),
            crate::session_meta::SessionMetaCache::new(),
            onboarding.clone(),
            std::time::Instant::now(),
            session.clone(),
            Arc::new(Mutex::new((100, 24))),
            tx,
            cancel.clone(),
            false,
            false,
            Duration::from_secs(3600),
            Duration::ZERO,
            shux_vt::monotonic_now_ns,
        ));

        tokio::time::timeout(shux_testkit::BUDGET, rx.recv())
            .await
            .expect("prelude timeout")
            .expect("prelude");
        let io_state = fixture.io_state.clone();
        shux_testkit::wait_until_async("the first frame to hold io_state", || {
            let io_state = io_state.clone();
            async move { io_state.try_lock().is_err() }
        })
        .await;
        // Where a key's dismissal would be: holding the session, about to
        // want `io_state`.
        let guard = session
            .try_lock()
            .expect("the frame holds the session while it holds io_state");
        drop(held);
        shux_testkit::wait_until_async(
            "the toast-ending frame to release io_state while the session is held",
            || {
                let io_state = io_state.clone();
                async move { io_state.try_lock().is_ok() }
            },
        )
        .await;
        drop(guard);
        shux_testkit::wait_until_async("the toast to end in the session", || {
            let session = session.clone();
            async move { !session.lock().await.show_welcome_toast }
        })
        .await;

        cancel.cancel();
        render.await.expect("render loop");
        fixture.stop();
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
            client_prefix: None,
            action_error: None,
            show_welcome_toast: true,
            shown: ShownFrames::default(),
            client: None,
            cell_pixels: None,
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
        crate::pane_input::PaneInputRx,
        mpsc::Receiver<crate::pane_io::ResizeRequest>,
        CancellationToken,
    ) {
        let (writer_tx, writer_rx) = crate::pane_input::channel_with(8, crate::pane_input::BUDGET);
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
        let frame = tokio::time::timeout(shux_testkit::BUDGET, out_rx.recv())
            .await
            .expect("render frame timeout")
            .expect("render frame");
        match frame {
            AttachServerFrame::Render { data, .. } => {
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
            fixture.attached.active_window_id,
            &test_config(),
            100,
            30,
        )
        .await;

        let first = tokio::time::timeout(shux_testkit::BUDGET, first_resize_rx.recv())
            .await
            .expect("first resize")
            .expect("first resize request");
        let second = tokio::time::timeout(shux_testkit::BUDGET, second_resize_rx.recv())
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
            fixture.attached.active_window_id,
            &test_config(),
            100,
            30,
        )
        .await;

        let first_zoomed = first_resize_rx.recv().await.expect("first zoom resize");
        assert_eq!((first_zoomed.size.cols, first_zoomed.size.rows), (100, 29));
        assert!(
            second_resize_rx.try_recv().is_err(),
            "zoom resized the hidden pane"
        );

        fixture.stop();
    }

    /// A resize still queued counts: sizing back to what the terminal shows
    /// now must still go out, or the pane is left at the queued size.
    #[tokio::test]
    async fn a_correction_behind_a_queued_resize_is_sent() {
        let fixture = attach_fixture().await;
        let (_, mut rx, _) = seed_io_for_pane(&fixture.io_state, fixture.first_pane).await;
        fixture
            .graph
            .zoom_pane(fixture.first_pane, None)
            .await
            .expect("zoom");
        let window = fixture.attached.active_window_id;
        apply_resize_to_window(
            &fixture.graph,
            &fixture.io_state,
            window,
            &test_config(),
            100,
            30,
        )
        .await;
        // The seeded terminal is 40x6: this asks for exactly that, while
        // 100x29 is still queued.
        apply_resize_to_window(
            &fixture.graph,
            &fixture.io_state,
            window,
            &test_config(),
            40,
            7,
        )
        .await;
        let sizes: Vec<(u16, u16)> = std::iter::from_fn(|| rx.try_recv().ok())
            .map(|r| (r.size.cols, r.size.rows))
            .collect();
        assert_eq!(
            sizes,
            vec![(100, 29), (40, 6)],
            "the correction was skipped"
        );
        fixture.stop();
    }

    /// A pane whose resize queue is full holds up no other sizing: the call
    /// returns at once and the size goes out once the queue has room.
    #[tokio::test]
    async fn a_full_resize_queue_blocks_nothing() {
        let fixture = attach_fixture().await;
        let (_, mut rx, _) = seed_io_for_pane(&fixture.io_state, fixture.first_pane).await;
        let tx = fixture.io_state.lock().await.resizers[&fixture.first_pane].clone();
        while tx
            .try_send(crate::pane_io::ResizeRequest {
                size: PtySize::new(50, 10),
                ack: None,
            })
            .is_ok()
        {}
        let mut changed = fixture.io_state.lock().await.clients.changed();
        changed.borrow_and_update();
        tokio::time::timeout(
            shux_testkit::BUDGET,
            apply_resize_to_window(
                &fixture.graph,
                &fixture.io_state,
                fixture.attached.active_window_id,
                &test_config(),
                100,
                30,
            ),
        )
        .await
        .expect("sizing waited on a full queue");
        rx.recv().await.expect("a queued resize");
        tokio::time::timeout(shux_testkit::BUDGET, changed.changed())
            .await
            .expect("room in the queue never woke the sizer")
            .expect("registry alive");
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
                // Sizes that clamp alike would be skipped as already asked for.
                fixture.io_state.lock().await.requested_sizes.clear();
                apply_resize_to_window(
                    &fixture.graph,
                    &fixture.io_state,
                    fixture.attached.active_window_id,
                    &test_config(),
                    cols,
                    rows,
                )
                .await;

                // Zoomed, the hidden pane keeps its size and gets no resize.
                let panes = if zoomed {
                    vec![("first", &mut first_resize_rx)]
                } else {
                    vec![
                        ("first", &mut first_resize_rx),
                        ("second", &mut second_resize_rx),
                    ]
                };
                for (label, rx) in panes {
                    let req = tokio::time::timeout(shux_testkit::BUDGET, rx.recv())
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
    async fn a_failed_split_or_zoom_says_failed_once() {
        let fixture = attach_fixture().await;
        let mut gone = fixture.attached.clone();
        gone.active_pane_id = PaneId::new();
        let session = Arc::new(Mutex::new(gone));
        let client_size = Arc::new(Mutex::new((100, 30)));
        for kind in [ActionKind::SplitVertical, ActionKind::ToggleZoom] {
            let e = handle_action(
                kind,
                Default::default(),
                &fixture.graph,
                &fixture.io_state,
                &session,
                &client_size,
                &test_config(),
                &fixture.cancel,
            )
            .await
            .expect_err("acting on a pane that is gone");
            let notice = action_failure_notice(kind, &e);
            assert_eq!(notice.matches("failed").count(), 1, "{notice:?}");
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
        let _seeded = seed_io_for_pane(&fixture.io_state, fixture.first_pane).await;
        ship_frame(&fixture.graph, &fixture.io_state, &session, &client_size).await;
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

        let view = {
            let io = fixture.io_state.lock().await;
            PaneViewport::of(Rect::new(4, 3, 20, 6), &io.vts[&fixture.first_pane], 0)
        };
        let state = shux_ui::Selection {
            anchor: Some((0, 0)),
            cursor: (4, 0),
        };
        assert_eq!(view_point(&view, 6, 3, false), Some((2, 0)));
        assert_eq!(view_point(&view, 3, 3, false), None);
        let selection = MouseSelection {
            pane_id: fixture.first_pane,
            state,
            view,
            generation: 1,
        };

        let (out_tx, mut out_rx) = mpsc::channel(2);
        let mut shown = ShownFrames::default();
        assert!(
            !yank_selection(&selection, &shown, &fixture.io_state, &out_tx).await,
            "no frame showed this slice"
        );
        shown.push(1, views_only([(fixture.first_pane, view)]));
        assert!(yank_selection(&selection, &shown, &fixture.io_state, &out_tx).await);
        let frame = out_rx.recv().await.expect("render frame");
        match frame {
            AttachServerFrame::Render { data, .. } => {
                let decoded = BASE64.decode(data.as_bytes()).expect("osc52 base64");
                let text = String::from_utf8(decoded).expect("osc52 utf8");
                assert!(text.starts_with("\x1b]52;c;"));
                assert!(text.ends_with("\x07"));
            }
            other => panic!("expected render frame, got {other:?}"),
        }

        fixture.stop();
    }

    /// Review of #190: a resize that lands after the last frame shrinks the
    /// grid under a selection still stamped with that frame. Yanking it then
    /// copies nothing rather than reading past the grid, on either axis.
    #[tokio::test]
    async fn a_yank_after_the_grid_shrank_copies_nothing() {
        let fixture = attach_fixture().await;
        let pane = fixture.first_pane;
        seed_io_for_pane(&fixture.io_state, pane).await;
        for (rows, cols) in [(2, 40), (6, 8)] {
            let view = {
                let mut io = fixture.io_state.lock().await;
                let vt = io.vts.get_mut(&pane).expect("pane vt");
                vt.resize(6, 40);
                vt.process(b"\x1b[6;40H");
                PaneViewport::of(Rect::new(4, 3, 20, 4), vt, 0)
            };
            assert!(view.row_offset > 0 && view.col_offset > 0, "{view:?}");
            let state = shux_ui::Selection {
                anchor: Some((0, 0)),
                cursor: (19, 3),
            };
            let selection = MouseSelection {
                pane_id: pane,
                state,
                view,
                generation: 1,
            };
            let mut shown = ShownFrames::default();
            shown.push(1, views_only([(pane, view)]));
            fixture
                .io_state
                .lock()
                .await
                .vts
                .get_mut(&pane)
                .expect("pane vt")
                .resize(rows, cols);
            let (out_tx, _out_rx) = mpsc::channel(2);
            assert!(
                !yank_selection(&selection, &shown, &fixture.io_state, &out_tx).await,
                "{rows}x{cols}: a slice no longer on the grid was yanked"
            );
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
        ship_frame(&fixture.graph, &fixture.io_state, &session, &client_size).await;

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
                None,
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
            None,
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
            None,
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
                None,
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
                None,
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
        let session = Arc::new(Mutex::new(fixture.attached.clone()));
        let client_size = Arc::new(Mutex::new((100, 30)));
        let (out_tx, mut out_rx) = mpsc::channel(4);
        let mut drag = SelectionDrag::None;
        open_copy_mode(&fixture.graph, &fixture.io_state, &session, &client_size).await;

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
                None,
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
                None,
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
            None,
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
            None,
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
            None,
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
            None,
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
            None,
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
            None,
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
            None,
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
            None,
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
            graphics: false,
            plain: false,
        };
        framed
            .send(Bytes::from(serde_json::to_vec(&hello).expect("hello json")))
            .await
            .expect("send hello");
        let ready_buf = tokio::time::timeout(shux_testkit::BUDGET, framed.next())
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

        let initial_resize = tokio::time::timeout(shux_testkit::BUDGET, resize_rx.recv())
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
        // The connection handles frames in order, so the first write below
        // being `ls` proves `abc` was swallowed rather than late.

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
        let written = tokio::time::timeout(shux_testkit::BUDGET, writer_rx.recv())
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
        // Nothing applies the sizes here, so earlier ones may be re-sent first.
        let deadline = shux_testkit::Deadline::start();
        loop {
            let resized = deadline
                .run(resize_rx.recv())
                .await
                .expect("resize timeout")
                .expect("resize request");
            if resized.size.rows == 27 {
                break;
            }
        }

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

        // DetachAck follows PrefixTapped in frame order, so it also orders the
        // onboarding write before the read below.
        loop {
            let frame = tokio::time::timeout(shux_testkit::BUDGET, framed.next())
                .await
                .expect("no DetachAck")
                .expect("connection closed before DetachAck")
                .expect("server frame bytes");
            let parsed: AttachServerFrame =
                serde_json::from_slice(&frame).expect("server frame json");
            if matches!(parsed, AttachServerFrame::DetachAck) {
                break;
            }
        }
        assert!(onboarding.current().await.prefix_discovered);

        drop(framed);
        server_cancel.cancel();
        let _ = tokio::time::timeout(shux_testkit::BUDGET, server).await;
        assert!(writer_rx.try_recv().is_err());
        fixture.stop();
    }

    /// The welcome toast says any key dismisses it. A keystroke the pane
    /// receives and a prefix action both end it, and the dismissal is
    /// persisted so the next attach does not show it again.
    #[tokio::test]
    async fn a_keystroke_or_an_action_dismisses_the_welcome_toast() {
        for first in [
            AttachClientFrame::Input {
                data: BASE64.encode(b"ls\n"),
            },
            AttachClientFrame::Action {
                kind: ActionKind::Redraw,
                args: Default::default(),
            },
        ] {
            let fixture = attach_fixture().await;
            let (mut writer_rx, _resize_rx, _) =
                seed_io_for_pane(&fixture.io_state, fixture.first_pane).await;
            let onboarding = crate::onboarding::OnboardingHandle::from_state_for_test(
                crate::onboarding::OnboardingState {
                    prefix_discovered: true,
                    welcome_toast_seen: false,
                },
            );
            let mut client =
                Client::attach_onboarded(&fixture, 90, onboarding.clone(), false).await;

            client.send(&first).await;
            // Frames are handled in order, so the pane receiving this proves
            // the first one was handled.
            client.key(b"x").await;
            loop {
                let written = tokio::time::timeout(shux_testkit::BUDGET, writer_rx.recv())
                    .await
                    .expect("writer timeout")
                    .expect("writer bytes");
                if written.ends_with(b"x") {
                    break;
                }
            }
            assert!(
                onboarding.current().await.welcome_toast_seen,
                "{first:?} left the welcome toast up"
            );

            client.stop().await;
            fixture.stop();
        }
    }

    /// Seed a pane whose VT has real scrollback (more lines than fit), plus any
    /// mode-enabling setup bytes (e.g. `?1000h`, `?1049h`). Returns the pane's
    /// PTY writer receiver so a test can assert what the app was sent.
    async fn seed_scrollback_pane(
        io_state: &Arc<Mutex<PaneIoState>>,
        pane_id: PaneId,
        setup: &[u8],
    ) -> crate::pane_input::PaneInputRx {
        let (writer_tx, writer_rx) = crate::pane_input::channel_with(64, crate::pane_input::BUDGET);
        let (resize_tx, _resize_rx) = mpsc::channel(8);
        let shutdown = CancellationToken::new();
        // Fits the harness pane's 27-row rect, as an attach sizes a real PTY.
        let mut vt = shux_vt::VirtualTerminal::new(27, 49);
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

    async fn recv_bytes(rx: &mut crate::pane_input::PaneInputRx) -> Vec<u8> {
        tokio::time::timeout(shux_testkit::BUDGET, rx.recv())
            .await
            .expect("pane write timeout")
            .expect("pane write channel closed")
    }

    // --- Issue #174: button events reach a mouse-aware app ---------------
    // These drive the real `handle_app_mouse` and `route_app_mouse` against a
    // live graph + pane VT.

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
                    last: (1, 1),
                    offsets: (0, 0),
                    anchor: (0, 0),
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
                    last: (1, 1),
                    offsets: (0, 0),
                    anchor: (0, 0),
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

    /// The client can have a numbered frame the instant it is queued; its views
    /// must already be recorded then. The session is held busy throughout, so
    /// a sender that queued first and recorded after would be caught between.
    #[tokio::test]
    async fn a_numbered_frame_is_recorded_before_the_client_can_have_it() {
        let fixture = attach_fixture().await;
        let session = Arc::new(Mutex::new(fixture.attached.clone()));
        let (out_tx, mut out_rx) = mpsc::channel(1);
        let busy = session.lock().await;
        let sender = {
            let session = session.clone();
            tokio::spawn(
                async move { send_frame(&out_tx, &session, 7, Shown::default(), b"x").await },
            )
        };
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert!(
            out_rx.try_recv().is_err(),
            "a frame went out before its views were recorded"
        );
        assert!(busy.shown.views(Some(7)).is_none());
        drop(busy);
        let got = out_rx.recv().await;
        assert!(matches!(
            got,
            Some(AttachServerFrame::Render { frame: Some(7), .. })
        ));
        assert!(session.lock().await.shown.views(Some(7)).is_some());
        assert!(sender.await.expect("sender task"));
        fixture.stop();
    }

    /// Which frame's views a stamped event maps through.
    #[test]
    fn shown_frames_map_a_stamp_to_the_frame_the_client_had_written() {
        let pane = PaneId::new();
        let view = |row_offset| {
            views_only([(
                pane,
                PaneViewport {
                    rect: Rect::new(0, 0, 1, 1),
                    row_offset,
                    col_offset: 0,
                    grid_rows: 1,
                    grid_cols: 1,
                },
            )])
        };
        let offset =
            |views: Option<&HashMap<PaneId, PaneViewport>>| views.map(|v| v[&pane].row_offset);
        let mut shown = ShownFrames::default();
        assert_eq!(offset(shown.views(Some(1))), None, "nothing sent yet");
        let last = ShownFrames::KEPT as u64 + 4;
        for n in 1..=last {
            shown.push(n, view(n as usize));
        }
        assert_eq!(offset(shown.views(Some(12))), Some(12), "the stamped frame");
        assert_eq!(
            offset(shown.views(None)),
            Some(last as usize),
            "an unstamped event: the latest"
        );
        assert_eq!(
            offset(shown.views(Some(2))),
            None,
            "a stamp older than any kept (5..=last) names nothing newer than it"
        );
    }

    /// A press whose button mask can never empty is a gesture that never ends.
    #[test]
    fn app_gesture_ends_only_when_every_button_is_up() {
        let pane = PaneId::new();
        let mut g = SelectionDrag::App {
            pane_id: pane,
            buttons: 0b101, // left + right
            last: (3, 4),
            offsets: (0, 0),
            anchor: (0, 0),
        };
        end_gesture_if_released(&mut g, ButtonAction::Release, ProtoMouseButton::Right);
        assert_eq!(
            g,
            SelectionDrag::App {
                pane_id: pane,
                buttons: 0b001,
                last: (3, 4),
                offsets: (0, 0),
                anchor: (0, 0),
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
            offsets: (0, 0),
            anchor: (0, 0),
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
            offsets: (0, 0),
            anchor: (0, 0),
        };
        end_gesture_if_released(&mut g, ButtonAction::Drag, ProtoMouseButton::Left);
        assert!(matches!(g, SelectionDrag::App { .. }));
    }

    // --- Issue #174: `handle_app_mouse` against a live graph + pane VT ------

    /// Records in `session` what a frame sent now would show, as the render
    /// loop does after each frame, for tests that call the handlers directly.
    async fn ship_frame(
        graph: &GraphHandle,
        io_state: &Arc<Mutex<PaneIoState>>,
        session: &Arc<Mutex<AttachedSession>>,
        client_size: &ClientSize,
    ) {
        let attached = session.lock().await.clone();
        let content = current_content_rect(client_size).await;
        let viewport = current_viewport(client_size, &test_config()).await;
        let (views, drawn) = {
            let state = io_state.lock().await;
            let views = state
                .vts
                .iter()
                .filter_map(|(pid, vt)| {
                    pane_rect_in(graph, &attached, content, viewport, *pid).map(|rect| {
                        (
                            *pid,
                            PaneViewport::of(rect, vt, shux_vt::monotonic_now_ns()),
                        )
                    })
                })
                .collect();
            let drawn = state
                .vts
                .iter()
                .map(|(pid, vt)| (*pid, shux_ui::Drawn::of(vt)))
                .collect();
            (views, drawn)
        };
        let mut s = session.lock().await;
        let next = s.shown.0.back().map_or(1, |(n, _)| n + 1);
        let modal = s.copy_mode.as_ref().map(|cm| (cm.pane, cm.view));
        s.shown.push(
            next,
            Shown {
                views,
                drawn,
                modal,
            },
        );
    }

    /// Copy mode on the session's focused pane over the slice a frame just
    /// shipped shows, and a frame showing it.
    async fn open_copy_mode(
        graph: &GraphHandle,
        io_state: &Arc<Mutex<PaneIoState>>,
        session: &Arc<Mutex<AttachedSession>>,
        client_size: &ClientSize,
    ) {
        ship_frame(graph, io_state, session, client_size).await;
        {
            let state = io_state.lock().await;
            let mut s = session.lock().await;
            let pane = s.active_pane_id;
            let cm = copy_mode_over(pane, s.shown.shown(None), None, &state);
            s.copy_mode = Some(cm.expect("copy mode"));
        }
        ship_frame(graph, io_state, session, client_size).await;
    }

    /// A release whose frame is no longer kept still finishes a selection
    /// shux owns: rejecting a stale point must not strand the gesture.
    #[tokio::test]
    async fn an_expired_release_still_finishes_a_shux_selection() {
        let mut h = AppMouseHarness::new().await;
        let _wr = seed_scrollback_pane(&h.fixture.io_state, h.fixture.first_pane, b"").await;
        let (tx, _rx) = mpsc::channel(8);
        assert_eq!(
            h.send(MouseKind::Down, ProtoMouseButton::Left, 3, 3).await,
            AppMouse::NotHandled
        );
        h.select(MouseKind::Down, &tx).await;
        assert!(matches!(h.gesture, SelectionDrag::MouseSelection { .. }));
        for _ in 0..ShownFrames::KEPT {
            ship_frame(
                &h.fixture.graph,
                &h.fixture.io_state,
                &h.session,
                &h.client_size,
            )
            .await;
        }
        if h.send_stamped(MouseKind::Up, 3, 3, Some(1)).await == AppMouse::NotHandled {
            h.select(MouseKind::Up, &tx).await;
        }
        h.fixture.stop();
        assert_eq!(
            h.gesture,
            SelectionDrag::None,
            "the selection was never finished"
        );
    }

    /// A client as far behind as the outbound queue lets it fall still has
    /// its clicks mapped: a busy pane on a slow link must not cost it the mouse.
    #[tokio::test]
    async fn a_client_a_full_queue_behind_still_has_its_clicks_handled() {
        let mut h = AppMouseHarness::new().await;
        let _wr = seed_scrollback_pane(&h.fixture.io_state, h.fixture.first_pane, b"").await;
        ship_frame(
            &h.fixture.graph,
            &h.fixture.io_state,
            &h.session,
            &h.client_size,
        )
        .await;
        let shown = h.session.lock().await.shown.0.back().map(|(n, _)| *n);
        for _ in 0..OUTBOUND_FRAMES {
            ship_frame(
                &h.fixture.graph,
                &h.fixture.io_state,
                &h.session,
                &h.client_size,
            )
            .await;
        }
        let route = h.send_stamped(MouseKind::Down, 3, 3, shown).await;
        h.fixture.stop();
        assert_eq!(
            route,
            AppMouse::NotHandled,
            "the click was dropped, not handled"
        );
    }

    /// A client further behind than every kept frame (small frames fill the
    /// socket's byte buffers with far more than the queue's count) still has
    /// shux's own mouse handling, as on base: only app reports need the frame.
    #[tokio::test]
    async fn a_client_behind_every_kept_frame_still_has_shux_handling() {
        let mut h = AppMouseHarness::new().await;
        let _wr = seed_scrollback_pane(&h.fixture.io_state, h.fixture.first_pane, b"").await;
        ship_frame(
            &h.fixture.graph,
            &h.fixture.io_state,
            &h.session,
            &h.client_size,
        )
        .await;
        let shown = h.session.lock().await.shown.0.back().map(|(n, _)| *n);
        for _ in 0..ShownFrames::KEPT + 10 {
            ship_frame(
                &h.fixture.graph,
                &h.fixture.io_state,
                &h.session,
                &h.client_size,
            )
            .await;
        }
        let route = h.send_stamped(MouseKind::Down, 3, 3, shown).await;
        h.fixture.stop();
        assert_eq!(
            route,
            AppMouse::NotHandled,
            "the click was dropped, not handled"
        );
    }

    /// Past every kept frame, a press over a pane whose app reports the mouse
    /// is not shux's: it focuses that pane and goes no further, so a drag
    /// cannot open a selection under the app and copy it on release.
    #[tokio::test]
    async fn a_press_from_a_frame_no_longer_kept_over_a_mouse_app_opens_no_selection() {
        let mut h = AppMouseHarness::new().await;
        let tracking = b"\x1b[?1000h\x1b[?1006h";
        let _wr = seed_scrollback_pane(&h.fixture.io_state, h.fixture.first_pane, tracking).await;
        ship_frame(
            &h.fixture.graph,
            &h.fixture.io_state,
            &h.session,
            &h.client_size,
        )
        .await;
        let shown = h.session.lock().await.shown.0.back().map(|(n, _)| *n);
        for _ in 0..ShownFrames::KEPT + 10 {
            ship_frame(
                &h.fixture.graph,
                &h.fixture.io_state,
                &h.session,
                &h.client_size,
            )
            .await;
        }
        let route = h.send_stamped(MouseKind::Down, 3, 3, shown).await;
        h.fixture.stop();
        assert!(
            matches!(route, AppMouse::Consumed { .. }),
            "the press fell through to shux's selection: {route:?}"
        );
    }

    /// A pane closed since the frame was drawn takes the click: it must not
    /// fall through to shux's handling at today's layout.
    #[tokio::test]
    async fn a_click_on_a_shown_pane_closed_since_is_consumed() {
        let mut h = AppMouseHarness::new().await;
        let tracking = b"\x1b[?1000h\x1b[?1006h";
        let _a = seed_scrollback_pane(&h.fixture.io_state, h.fixture.first_pane, tracking).await;
        let _b = seed_scrollback_pane(&h.fixture.io_state, h.fixture.second_pane, tracking).await;
        ship_frame(
            &h.fixture.graph,
            &h.fixture.io_state,
            &h.session,
            &h.client_size,
        )
        .await;
        h.fixture
            .graph
            .destroy_pane(h.fixture.first_pane, None)
            .await
            .unwrap();
        h.fixture
            .io_state
            .lock()
            .await
            .vts
            .remove(&h.fixture.first_pane);
        h.session.lock().await.active_pane_id = h.fixture.second_pane;
        let route = h.send_stamped(MouseKind::Down, 3, 3, None).await;
        h.fixture.stop();
        assert_eq!(
            route,
            AppMouse::Consumed {
                redraw: false,
                input: false
            }
        );
    }

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

        #[expect(clippy::too_many_arguments, reason = "independent inputs")]
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
            ship_frame(
                &self.fixture.graph,
                &self.fixture.io_state,
                &self.session,
                &self.client_size,
            )
            .await;
            handle_app_mouse(
                kind,
                button,
                col,
                row,
                shift,
                alt,
                ctrl,
                None,
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

        /// shux's own selection handling of a left-button event at (3, 3).
        async fn select(&mut self, kind: MouseKind, tx: &mpsc::Sender<AttachServerFrame>) {
            handle_mouse_selection(
                kind,
                ProtoMouseButton::Left,
                3,
                3,
                &self.fixture.graph,
                &self.fixture.io_state,
                &self.session,
                &self.client_size,
                &test_config(),
                tx,
                &mut self.gesture,
                None,
            )
            .await
            .expect("handle_mouse_selection");
        }

        /// A left-button event stamped with `frame`, shipping no new frame:
        /// the client is looking at whatever it was last sent.
        async fn send_stamped(
            &mut self,
            kind: MouseKind,
            col: u16,
            row: u16,
            frame: Option<u64>,
        ) -> AppMouse {
            handle_app_mouse(
                kind,
                ProtoMouseButton::Left,
                col,
                row,
                false,
                false,
                false,
                frame,
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

    /// Nothing arrived on the pane's writer. `handle_app_mouse` writes with
    /// `try_send` before it returns, so nothing can arrive late.
    async fn assert_no_bytes(rx: &mut crate::pane_input::PaneInputRx, why: &str) {
        if let Ok(bytes) = rx.try_recv() {
            panic!("{why}: app received {:?}", String::from_utf8_lossy(&bytes));
        }
    }

    async fn recv_text(rx: &mut crate::pane_input::PaneInputRx) -> String {
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
            AppMouse::Consumed {
                redraw: false,
                input: true
            }
        );
        assert_eq!(recv_text(&mut wr).await, "\x1b[<0;10;5M");
        h.send(MouseKind::Drag, ProtoMouseButton::Left, 12, 5).await;
        assert_eq!(recv_text(&mut wr).await, "\x1b[<32;12;5M");
        h.send(MouseKind::Up, ProtoMouseButton::Left, 12, 5).await;
        assert_eq!(recv_text(&mut wr).await, "\x1b[<0;12;5m");
        assert_eq!(h.gesture, SelectionDrag::None, "gesture must have ended");
        h.fixture.stop();
    }

    /// An `Up` the client cannot attribute to a button still ends the gesture,
    /// releasing the buttons the app holds: `Mouse` is an untrusted wire frame,
    /// so this shape is reachable even though shux's own client never sends it.
    #[tokio::test]
    async fn an_unattributable_release_still_ends_the_app_gesture() {
        let mut h = AppMouseHarness::new().await;
        let mut wr = seed_scrollback_pane(
            &h.fixture.io_state,
            h.fixture.first_pane,
            b"\x1b[?1002h\x1b[?1006h",
        )
        .await;

        h.send(MouseKind::Down, ProtoMouseButton::Left, 10, 5).await;
        assert_eq!(recv_text(&mut wr).await, "\x1b[<0;10;5M");
        assert!(matches!(h.gesture, SelectionDrag::App { .. }));

        h.send(MouseKind::Up, ProtoMouseButton::None, 10, 5).await;
        assert_eq!(
            recv_text(&mut wr).await,
            "\x1b[<0;10;5m",
            "the held button is released"
        );
        assert_eq!(
            h.gesture,
            SelectionDrag::None,
            "the app is left believing the button is still down"
        );
        h.fixture.stop();
    }

    /// Installs a `rows` x `cols` VT with `setup` modes in `pane`, cursor on its
    /// last row, and returns the pane's writer and rect.
    async fn install_app(
        h: &AppMouseHarness,
        pane: PaneId,
        (rows, cols): (usize, usize),
        setup: &[u8],
    ) -> (crate::pane_input::PaneInputRx, Rect) {
        let wr = seed_scrollback_pane(&h.fixture.io_state, pane, b"").await;
        let mut vt = shux_vt::VirtualTerminal::new(rows, cols);
        vt.process(setup);
        vt.process(format!("\x1b[{rows};1H").as_bytes());
        h.fixture.io_state.lock().await.vts.insert(pane, vt);
        let attached = h.session.lock().await.clone();
        let rect = pane_rect_for(
            &h.fixture.graph,
            &attached,
            &h.client_size,
            &test_config(),
            pane,
        )
        .await
        .expect("pane rect");
        (wr, rect)
    }

    fn drain(rx: &mut crate::pane_input::PaneInputRx) -> String {
        let mut out = Vec::new();
        while let Ok(bytes) = rx.try_recv() {
            out.extend(bytes);
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    /// A pane cropped on both axes reports the grid cell under the pointer,
    /// `screen - rect + offset`, 1-based, on each axis.
    #[tokio::test]
    async fn app_mouse_names_the_grid_cell_on_both_axes() {
        let mut h = AppMouseHarness::new().await;
        let pane = h.fixture.first_pane;
        let (mut wr, r) = install_app(&h, pane, (60, 300), b"\x1b[?1002h\x1b[?1006h").await;
        // The cursor at the grid's far corner takes both offsets as far as they go.
        h.fixture
            .io_state
            .lock()
            .await
            .vts
            .get_mut(&pane)
            .expect("pane vt")
            .process(b"\x1b[60;300H");
        let (row_off, col_off) = (60 - usize::from(r.height), 300 - usize::from(r.width));
        assert_eq!(
            h.send(MouseKind::Down, ProtoMouseButton::Left, r.x + 2, r.y + 1)
                .await,
            AppMouse::Consumed {
                redraw: false,
                input: true
            }
        );
        assert_eq!(
            recv_text(&mut wr).await,
            format!("\x1b[<0;{};{}M", col_off + 3, row_off + 2)
        );
        h.fixture.stop();
    }

    /// A press on the surplus of the focused pane names no cell: it reaches no
    /// app and moves no focus, so it is no input and makes no client drive.
    #[tokio::test]
    async fn a_press_on_the_focused_panes_surplus_is_not_input() {
        let mut h = AppMouseHarness::new().await;
        let pane = h.fixture.first_pane;
        let (mut wr, r) = install_app(&h, pane, (4, 12), b"\x1b[?1002h\x1b[?1006h").await;
        assert_eq!(
            h.send(MouseKind::Down, ProtoMouseButton::Left, r.x + 20, r.y + 1)
                .await,
            AppMouse::Consumed {
                redraw: false,
                input: false
            }
        );
        assert_eq!(drain(&mut wr), "", "the app got a press off its grid");

        // Without mouse tracking the click falls through to shux's own
        // click-to-focus, which in the focused pane does nothing either.
        let attached = h.session.lock().await.clone();
        let mut border = None;
        let acted = handle_mouse(
            MouseKind::Down,
            ProtoMouseButton::Left,
            r.x + 20,
            r.y + 1,
            &h.fixture.graph,
            &h.fixture.io_state,
            &h.session,
            &h.client_size,
            &test_config(),
            &mut border,
            None,
        )
        .await
        .expect("click");
        assert!(!acted, "a click in the focused pane is not input");
        let other = pane_rect_for(
            &h.fixture.graph,
            &attached,
            &h.client_size,
            &test_config(),
            h.fixture.second_pane,
        )
        .await
        .expect("second pane rect");
        let _second = install_app(&h, h.fixture.second_pane, (60, 200), b"").await;
        ship_frame(
            &h.fixture.graph,
            &h.fixture.io_state,
            &h.session,
            &h.client_size,
        )
        .await;
        let acted = handle_mouse(
            MouseKind::Down,
            ProtoMouseButton::Left,
            other.x + 1,
            other.y + 1,
            &h.fixture.graph,
            &h.fixture.io_state,
            &h.session,
            &h.client_size,
            &test_config(),
            &mut border,
            None,
        )
        .await
        .expect("click");
        assert!(acted, "focusing another pane is input");
        h.fixture.stop();
    }

    static LATCH_CLOCK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn latch_clock() -> u64 {
        LATCH_CLOCK.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// With no output and no tick, the render loop wakes itself when a hidden
    /// cursor's latch comes due and anchors the oversized pane at its
    /// top-left. The slice moved but no cell did, so the frame is an empty
    /// numbered `Render`: the client's next event is stamped with it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_due_latch_sends_a_frame_that_moves_only_the_slice() {
        use std::sync::atomic::Ordering::SeqCst;
        let fixture = attach_fixture().await;
        let pane = fixture.first_pane;
        let hidden_since = {
            let mut vt = shux_vt::VirtualTerminal::new(200, 400);
            vt.process(b"\x1b[200;400H\x1b[?25l");
            let at = vt.cursor().hidden_since_ns;
            fixture.io_state.lock().await.vts.insert(pane, vt);
            at
        };
        LATCH_CLOCK.store(hidden_since + 1_000_000, SeqCst);
        let onboarding = crate::onboarding::OnboardingHandle::from_state_for_test(
            crate::onboarding::OnboardingState {
                prefix_discovered: true,
                welcome_toast_seen: true,
            },
        );
        // why: a start instant handed in as data; time passing only moves it further past the tier
        // ast-grep-ignore: test-wall-clock-unit
        let daemon_start = std::time::Instant::now()
            .checked_sub(Duration::from_secs(61))
            .expect("monotonic clock past 61 s");
        let session = Arc::new(Mutex::new(fixture.attached.clone()));
        let (tx, mut rx) = mpsc::channel(64);
        let cancel = fixture.cancel.child_token();
        let render = tokio::spawn(run_render_loop(
            fixture.graph.clone(),
            fixture.io_state.clone(),
            test_config(),
            SegmentCache::new(),
            crate::session_meta::SessionMetaCache::new(),
            onboarding,
            daemon_start,
            session.clone(),
            Arc::new(Mutex::new((100, 40))),
            tx,
            cancel.clone(),
            false,
            false,
            Duration::from_secs(3600),
            WELCOME_TOAST_DWELL,
            latch_clock,
        ));
        let offsets = |s: &AttachedSession| {
            let views = s.shown.views(None)?;
            views.get(&pane).map(|v| (v.row_offset, v.col_offset))
        };
        let deadline = shux_testkit::Deadline::start();
        async fn numbered(
            rx: &mut mpsc::Receiver<AttachServerFrame>,
            deadline: &shux_testkit::Deadline,
        ) -> String {
            loop {
                match deadline.run(rx.recv()).await {
                    Some(Some(AttachServerFrame::Render {
                        data,
                        frame: Some(_),
                    })) => return data,
                    Some(Some(_)) => {}
                    Some(None) | None => panic!("no numbered frame came"),
                }
            }
        }
        numbered(&mut rx, &deadline).await;
        let first = offsets(&*session.lock().await).expect("the first frame's view");
        assert!(
            first.0 > 0 && first.1 > 0,
            "hidden for 1 ms of the latch: {first:?}"
        );

        LATCH_CLOCK.store(
            hidden_since + shux_ui::viewport::HIDDEN_CURSOR_LATCH_NS,
            SeqCst,
        );
        let data = numbered(&mut rx, &deadline).await;
        assert_eq!(offsets(&*session.lock().await), Some((0, 0)));
        assert_eq!(data, "", "only the slice moved");

        cancel.cancel();
        render.await.expect("render loop");
        fixture.stop();
    }

    static SPIN_CLOCK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    static SPIN_READS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn spin_clock() -> u64 {
        SPIN_READS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        SPIN_CLOCK.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// A latch deadline lives for the wait after the frame that found it.
    /// Once its session is gone the loop draws nothing, and a deadline kept
    /// from before woke it at once, for ever. The paused clock moves only
    /// when every task waits, so `idle_for` returning is the loop at rest.
    #[tokio::test(start_paused = true)]
    async fn a_latch_deadline_does_not_outlive_the_window_it_was_for() {
        use std::sync::atomic::Ordering::SeqCst;
        let fixture = attach_fixture().await;
        let pane = fixture.first_pane;
        let hidden_since = {
            let mut vt = shux_vt::VirtualTerminal::new(200, 400);
            vt.process(b"\x1b[200;400H\x1b[?25l");
            let at = vt.cursor().hidden_since_ns;
            fixture.io_state.lock().await.vts.insert(pane, vt);
            at
        };
        SPIN_CLOCK.store(hidden_since + 1_000_000, SeqCst);
        let onboarding = crate::onboarding::OnboardingHandle::from_state_for_test(
            crate::onboarding::OnboardingState {
                prefix_discovered: true,
                welcome_toast_seen: true,
            },
        );
        let (tx, mut rx) = mpsc::channel(64);
        let cancel = fixture.cancel.child_token();
        let render = tokio::spawn(run_render_loop(
            fixture.graph.clone(),
            fixture.io_state.clone(),
            test_config(),
            SegmentCache::new(),
            crate::session_meta::SessionMetaCache::new(),
            onboarding,
            std::time::Instant::now(),
            Arc::new(Mutex::new(fixture.attached.clone())),
            Arc::new(Mutex::new((100, 40))),
            tx,
            cancel.clone(),
            false,
            false,
            Duration::from_secs(3600),
            WELCOME_TOAST_DWELL,
            spin_clock,
        ));
        let deadline = shux_testkit::Deadline::start();
        loop {
            match deadline.run(rx.recv()).await {
                Some(Some(AttachServerFrame::Render { frame: Some(_), .. })) => break,
                Some(Some(_)) => {}
                Some(None) | None => panic!("no first frame"),
            }
        }

        // The deadline is due and the window it was for is gone.
        SPIN_CLOCK.store(
            hidden_since + shux_ui::viewport::HIDDEN_CURSOR_LATCH_NS,
            SeqCst,
        );
        fixture
            .graph
            .destroy_session(fixture.session_id, None)
            .await
            .expect("destroy the session");
        idle_for(QUIET).await;
        let reads = SPIN_READS.load(SeqCst);
        idle_for(QUIET).await;
        assert_eq!(SPIN_READS.load(SeqCst), reads, "the loop kept waking");

        cancel.cancel();
        render.await.expect("render loop");
        fixture.stop();
    }

    /// Review of #190: a right-click opens the copy menu only through the frame
    /// the selection was made over. Stamped with a frame that shows the pane's
    /// grid elsewhere, the same screen point is not on the selection.
    #[tokio::test]
    async fn a_right_click_from_another_frame_opens_no_menu() {
        let mut h = AppMouseHarness::new().await;
        let pane = h.fixture.first_pane;
        let (_wr, r) = install_app(&h, pane, (60, 40), b"").await;
        let (out_tx, _out_rx) = mpsc::channel(8);
        let (from, to) = ((r.x + 1, r.y + 1), (r.x + 6, r.y + 1));
        for (kind, at) in [
            (MouseKind::Down, from),
            (MouseKind::Drag, to),
            (MouseKind::Up, to),
        ] {
            ship_frame(
                &h.fixture.graph,
                &h.fixture.io_state,
                &h.session,
                &h.client_size,
            )
            .await;
            handle_mouse_selection(
                kind,
                ProtoMouseButton::Left,
                at.0,
                at.1,
                &h.fixture.graph,
                &h.fixture.io_state,
                &h.session,
                &h.client_size,
                &test_config(),
                &out_tx,
                &mut h.gesture,
                None,
            )
            .await
            .expect("selection");
        }
        assert!(h.session.lock().await.mouse_selection.is_some());
        // The cursor goes home: the next frame shows the grid's top instead.
        h.fixture
            .io_state
            .lock()
            .await
            .vts
            .get_mut(&pane)
            .expect("pane vt")
            .process(b"\x1b[H");
        ship_frame(
            &h.fixture.graph,
            &h.fixture.io_state,
            &h.session,
            &h.client_size,
        )
        .await;
        let newest = h.session.lock().await.shown.0.back().map(|(n, _)| *n);
        handle_mouse_selection(
            MouseKind::Down,
            ProtoMouseButton::Right,
            from.0 + 2,
            from.1,
            &h.fixture.graph,
            &h.fixture.io_state,
            &h.session,
            &h.client_size,
            &test_config(),
            &out_tx,
            &mut h.gesture,
            newest,
        )
        .await
        .expect("right-click");
        assert_eq!(
            h.session.lock().await.copy_menu,
            None,
            "a menu opened over cells that frame did not show selected"
        );
        h.fixture.stop();
    }

    /// Review of #190: the wheel enters copy mode only with history above the
    /// view. A grid taller than its rect, cursor at its top, shows its top
    /// rows: nothing above them to scroll to.
    #[tokio::test]
    async fn the_wheel_finds_no_history_above_a_tall_grid_shown_from_its_top() {
        let h = AppMouseHarness::new().await;
        let pane = h.fixture.first_pane;
        let (_wr, r) = install_app(&h, pane, (60, 40), b"").await;
        h.fixture
            .io_state
            .lock()
            .await
            .vts
            .get_mut(&pane)
            .expect("pane vt")
            .process(b"\x1b[H");
        ship_frame(
            &h.fixture.graph,
            &h.fixture.io_state,
            &h.session,
            &h.client_size,
        )
        .await;
        handle_wheel(
            MouseKind::ScrollUp,
            r.x + 1,
            r.y + 1,
            None,
            &h.fixture.graph,
            &h.fixture.io_state,
            &h.session,
            &h.client_size,
            &test_config(),
        )
        .await
        .expect("wheel");
        assert!(
            h.session.lock().await.copy_mode.is_none(),
            "the wheel opened a scrollback view with nothing above it"
        );
        h.fixture.stop();
    }

    /// A resize under an open copy view names other cells than it showed: the
    /// next frame ends a copy mode with a selection and brings the pane back to
    /// its live view. One nothing was done in yet opens again over the new grid.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_open_copy_view_past_its_history_is_brought_back() {
        for selecting in [true, false] {
            copy_mode_across_a_resize(selecting).await;
        }
    }

    async fn copy_mode_across_a_resize(selecting: bool) {
        let fixture = attach_fixture().await;
        seed_io_for_pane(&fixture.io_state, fixture.first_pane).await;
        let _ = fixture.graph.focus_pane(fixture.first_pane).await;
        let mut attached = fixture.attached.clone();
        attached.active_pane_id = fixture.first_pane;
        let session = Arc::new(Mutex::new(attached));
        let client_size: ClientSize = Arc::new(Mutex::new((100, 30)));
        open_copy_mode(&fixture.graph, &fixture.io_state, &session, &client_size).await;
        if selecting {
            let mut s = session.lock().await;
            let cm = s.copy_mode.as_mut().expect("copy mode");
            cm.anchor = Some(cm.cursor);
        }
        fixture
            .io_state
            .lock()
            .await
            .vts
            .get_mut(&fixture.first_pane)
            .expect("vt")
            .resize(4, 30);
        let onboarding = crate::onboarding::OnboardingHandle::from_state_for_test(
            crate::onboarding::OnboardingState {
                prefix_discovered: true,
                welcome_toast_seen: true,
            },
        );
        let (tx, mut rx) = mpsc::channel(64);
        let cancel = fixture.cancel.child_token();
        let render = tokio::spawn(run_render_loop(
            fixture.graph.clone(),
            fixture.io_state.clone(),
            test_config(),
            SegmentCache::new(),
            crate::session_meta::SessionMetaCache::new(),
            onboarding,
            std::time::Instant::now(),
            session.clone(),
            Arc::new(Mutex::new((100, 30))),
            tx,
            cancel.clone(),
            false,
            false,
            Duration::from_secs(3600),
            WELCOME_TOAST_DWELL,
            shux_vt::monotonic_now_ns,
        ));
        let deadline = shux_testkit::Deadline::start();
        loop {
            match deadline.run(rx.recv()).await {
                Some(Some(AttachServerFrame::Render { frame: Some(_), .. })) => break,
                Some(Some(_)) => {}
                Some(None) | None => panic!("no frame"),
            }
        }
        let grid = session
            .lock()
            .await
            .copy_mode
            .as_ref()
            .map(|cm| (cm.view.entry.grid_rows, cm.view.entry.grid_cols));
        if selecting {
            assert_eq!(grid, None, "a selection outlived the grid it was made on");
        } else {
            assert_eq!(grid, Some((4, 30)), "an untouched copy mode did not reopen");
        }
        cancel.cancel();
        render.await.expect("render loop");
        fixture.stop();
    }

    /// Review of #190: the frame that finds a selection stale ends it and its
    /// menu, drawing neither; a selection started while that frame was drawn
    /// is a new one, and stays.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stale_selection_ends_alone_and_takes_its_menu_with_it() {
        let fixture = attach_fixture().await;
        let pane = fixture.first_pane;
        seed_io_for_pane(&fixture.io_state, pane).await;
        let stale_view = PaneViewport {
            rect: Rect::new(0, 0, 1, 1),
            grid_rows: 1,
            grid_cols: 1,
            row_offset: 0,
            col_offset: 0,
        };
        let selection = |generation| MouseSelection {
            pane_id: pane,
            state: shux_ui::Selection::default(),
            view: stale_view,
            generation,
        };
        let mut attached = fixture.attached.clone();
        attached.mouse_selection = Some(selection(1));
        attached.copy_menu = Some(CopyContextMenu {
            pane_id: pane,
            col: 5,
            row: 5,
        });
        let session = Arc::new(Mutex::new(attached));
        let onboarding = crate::onboarding::OnboardingHandle::from_state_for_test(
            crate::onboarding::OnboardingState {
                prefix_discovered: true,
                welcome_toast_seen: true,
            },
        );
        let held = onboarding.hold_for_test().await;
        let (tx, mut rx) = mpsc::channel(64);
        let cancel = fixture.cancel.child_token();
        let render = tokio::spawn(run_render_loop(
            fixture.graph.clone(),
            fixture.io_state.clone(),
            test_config(),
            SegmentCache::new(),
            crate::session_meta::SessionMetaCache::new(),
            onboarding.clone(),
            std::time::Instant::now(),
            session.clone(),
            Arc::new(Mutex::new((100, 30))),
            tx,
            cancel.clone(),
            false,
            false,
            Duration::from_secs(3600),
            WELCOME_TOAST_DWELL,
            shux_vt::monotonic_now_ns,
        ));
        // The frame has read the session and holds io_state, waiting on the
        // hold: a selection made now is after the one it found stale.
        let io_state = fixture.io_state.clone();
        shux_testkit::wait_until_async("the frame to hold io_state", || {
            let io_state = io_state.clone();
            async move { io_state.try_lock().is_err() }
        })
        .await;
        session.lock().await.mouse_selection = Some(selection(2));
        drop(held);
        let deadline = shux_testkit::Deadline::start();
        let drawn = loop {
            match deadline.run(rx.recv()).await {
                Some(Some(AttachServerFrame::Render {
                    data,
                    frame: Some(_),
                })) => {
                    break String::from_utf8_lossy(&BASE64.decode(data).expect("b64")).into_owned();
                }
                Some(Some(_)) => {}
                Some(None) | None => panic!("no frame"),
            }
        };
        assert!(
            !drawn.contains(" Copy"),
            "the stale selection's menu was drawn"
        );
        assert_eq!(
            session
                .lock()
                .await
                .mouse_selection
                .as_ref()
                .map(|s| s.generation),
            Some(2),
            "the cleanup ended the selection made since"
        );
        cancel.cancel();
        render.await.expect("render loop");
        fixture.stop();
    }

    /// A press that named no cell was never sent, so its release is not sent
    /// either; it releases what the app does hold instead of stranding it.
    #[tokio::test]
    async fn a_release_the_app_never_saw_pressed_is_not_sent() {
        let mut h = AppMouseHarness::new().await;
        let (mut wr, r) =
            install_app(&h, h.fixture.first_pane, (4, 12), b"\x1b[?1002h\x1b[?1006h").await;
        h.send(MouseKind::Down, ProtoMouseButton::Left, r.x + 20, r.y)
            .await;
        h.send(MouseKind::Down, ProtoMouseButton::Right, r.x, r.y)
            .await;
        h.send(MouseKind::Up, ProtoMouseButton::Left, r.x + 1, r.y)
            .await;
        h.send(MouseKind::Up, ProtoMouseButton::Right, r.x + 1, r.y)
            .await;
        assert_eq!(drain(&mut wr), "\x1b[<2;1;1M\x1b[<2;2;1m");
        assert_eq!(h.gesture, SelectionDrag::None);
        h.fixture.stop();
    }

    /// A host without SGR reporting decodes every release as `Up(Left)`; one
    /// for a right-button gesture must still end it and release the button.
    #[tokio::test]
    async fn a_legacy_hosts_release_ends_a_right_button_gesture() {
        let mut h = AppMouseHarness::new().await;
        let (mut wr, r) =
            install_app(&h, h.fixture.first_pane, (4, 12), b"\x1b[?1002h\x1b[?1006h").await;
        h.send(MouseKind::Down, ProtoMouseButton::Right, r.x, r.y)
            .await;
        h.send(MouseKind::Up, ProtoMouseButton::Left, r.x, r.y)
            .await;
        assert_eq!(drain(&mut wr), "\x1b[<2;1;1M\x1b[<2;1;1m");
        assert_eq!(h.gesture, SelectionDrag::None);
        h.fixture.stop();
    }

    /// A legacy host's release lands where the button came up, not where the
    /// gesture last moved.
    #[tokio::test]
    async fn a_legacy_hosts_release_lands_where_the_button_came_up() {
        let mut h = AppMouseHarness::new().await;
        let (mut wr, r) =
            install_app(&h, h.fixture.first_pane, (4, 12), b"\x1b[?1002h\x1b[?1006h").await;
        h.send(MouseKind::Down, ProtoMouseButton::Right, r.x, r.y)
            .await;
        h.send(MouseKind::Up, ProtoMouseButton::Left, r.x + 4, r.y + 1)
            .await;
        assert_eq!(drain(&mut wr), "\x1b[<2;1;1M\x1b[<2;5;2m");
        h.fixture.stop();
    }

    /// The pane moves mid-gesture (the client widened): the release names the
    /// cell under the pointer in the pane as it is drawn now.
    #[tokio::test]
    async fn a_gesture_follows_its_pane_when_the_layout_moves() {
        let mut h = AppMouseHarness::new().await;
        let pane = h.fixture.second_pane;
        let (mut wr, r) = install_app(&h, pane, (10, 20), b"\x1b[?1002h\x1b[?1006h").await;
        h.send(MouseKind::Down, ProtoMouseButton::Left, r.x + 2, r.y + 2)
            .await;
        *h.client_size.lock().await = (140, 30);
        let attached = h.session.lock().await.clone();
        let moved = pane_rect_for(
            &h.fixture.graph,
            &attached,
            &h.client_size,
            &test_config(),
            pane,
        )
        .await
        .expect("pane rect");
        assert_ne!(moved.x, r.x, "the layout did not move the pane");
        h.send(
            MouseKind::Up,
            ProtoMouseButton::Left,
            moved.x + 2,
            moved.y + 2,
        )
        .await;
        assert_eq!(drain(&mut wr), "\x1b[<0;3;3M\x1b[<0;3;3m");
        h.fixture.stop();
    }

    /// The grid shrinks mid-gesture: the release still names a cell it has.
    #[tokio::test]
    async fn a_gesture_releases_inside_a_grid_that_shrank_under_it() {
        let mut h = AppMouseHarness::new().await;
        let pane = h.fixture.first_pane;
        let (mut wr, r) = install_app(&h, pane, (60, 40), b"\x1b[?1002h\x1b[?1006h").await;
        h.send(MouseKind::Down, ProtoMouseButton::Left, r.x + 2, r.y + 21)
            .await;
        assert!(drain(&mut wr).ends_with(";3;55M"));
        h.fixture
            .io_state
            .lock()
            .await
            .vts
            .get_mut(&pane)
            .expect("vt")
            .resize(4, 3);
        h.send(MouseKind::Up, ProtoMouseButton::Left, r.x + 2, r.y + 21)
            .await;
        assert_eq!(drain(&mut wr), "\x1b[<0;3;4m");
        h.fixture.stop();
    }

    /// Once a gesture is underway none of it is swallowed: a second press off
    /// the grid is pinned like the rest, so the app sees every press and every
    /// release names a button it holds.
    #[tokio::test]
    async fn every_press_of_a_gesture_underway_reaches_the_app() {
        let mut h = AppMouseHarness::new().await;
        let (mut wr, r) =
            install_app(&h, h.fixture.first_pane, (4, 12), b"\x1b[?1002h\x1b[?1006h").await;
        h.send(MouseKind::Down, ProtoMouseButton::Left, r.x, r.y)
            .await;
        assert_eq!(drain(&mut wr), "\x1b[<0;1;1M");
        h.send(MouseKind::Down, ProtoMouseButton::Right, r.x + 20, r.y + 2)
            .await;
        assert_eq!(drain(&mut wr), "\x1b[<2;12;3M", "pinned, not swallowed");
        h.send(MouseKind::Up, ProtoMouseButton::Right, r.x + 20, r.y + 2)
            .await;
        assert_eq!(drain(&mut wr), "\x1b[<2;12;3m", "left is still held");
        assert!(matches!(h.gesture, SelectionDrag::App { buttons: 1, .. }));
        h.send(MouseKind::Up, ProtoMouseButton::Left, r.x + 1, r.y)
            .await;
        assert_eq!(drain(&mut wr), "\x1b[<0;2;1m");
        assert_eq!(h.gesture, SelectionDrag::None);
        h.fixture.stop();
    }

    /// A legacy press past X10's reach during a gesture is pinned within it.
    #[tokio::test]
    async fn a_legacy_press_mid_gesture_past_the_x10_limit_is_pinned() {
        let mut h = AppMouseHarness::new().await;
        let bytes = |rx: &mut crate::pane_input::PaneInputRx| {
            let mut out = Vec::new();
            while let Ok(b) = rx.try_recv() {
                out.extend(b);
            }
            out
        };
        let (mut wr, _) = install_app(&h, h.fixture.first_pane, (300, 49), b"\x1b[?1002h").await;
        // Cursor on the last row: offset 273, so screen row 1 is grid row 274.
        h.send(MouseKind::Down, ProtoMouseButton::Left, 3, 1).await;
        assert_eq!(
            bytes(&mut wr),
            b"",
            "a lone press past the limit names nothing"
        );
        h.fixture
            .io_state
            .lock()
            .await
            .vts
            .get_mut(&h.fixture.first_pane)
            .expect("vt")
            .process(b"\x1b[200;1H");
        // Offset 173: screen row 1 is grid row 174, screen row 27 grid row 200.
        h.send(MouseKind::Down, ProtoMouseButton::Left, 3, 1).await;
        assert_eq!(bytes(&mut wr), b"\x1b[M #\xce");
        h.fixture
            .io_state
            .lock()
            .await
            .vts
            .get_mut(&h.fixture.first_pane)
            .expect("vt")
            .process(b"\x1b[300;1H");
        h.send(MouseKind::Down, ProtoMouseButton::Right, 3, 27)
            .await;
        assert_eq!(bytes(&mut wr), b"\x1b[M\"#\xff", "pinned to row 223");
        h.fixture.stop();
    }

    /// A zoom shows one pane; its clicks and wheel ticks go to that pane even
    /// before this client's cached focus catches up with the graph.
    #[tokio::test]
    async fn mouse_on_a_zoomed_pane_reaches_the_pane_on_screen() {
        let mut h = AppMouseHarness::new().await;
        let setup = b"\x1b[?1002h\x1b[?1006h";
        let mut first =
            seed_scrollback_pane(&h.fixture.io_state, h.fixture.first_pane, setup).await;
        let mut second =
            seed_scrollback_pane(&h.fixture.io_state, h.fixture.second_pane, setup).await;
        h.fixture
            .graph
            .zoom_pane(h.fixture.second_pane, None)
            .await
            .expect("zoom");
        assert_eq!(h.session.lock().await.active_pane_id, h.fixture.first_pane);

        h.send(MouseKind::Down, ProtoMouseButton::Left, 4, 3).await;
        h.send(MouseKind::Up, ProtoMouseButton::Left, 4, 3).await;
        ship_frame(
            &h.fixture.graph,
            &h.fixture.io_state,
            &h.session,
            &h.client_size,
        )
        .await;
        handle_wheel(
            MouseKind::ScrollUp,
            4,
            3,
            None,
            &h.fixture.graph,
            &h.fixture.io_state,
            &h.session,
            &h.client_size,
            &test_config(),
        )
        .await
        .expect("wheel");
        assert_eq!(drain(&mut first), "", "the hidden pane got the mouse");
        assert_eq!(drain(&mut second), "\x1b[<0;5;4M\x1b[<0;5;4m\x1b[<64;5;4M");
        h.fixture.stop();
    }

    /// A press X10 cannot encode reaches no app, but it still selects the pane.
    #[tokio::test]
    async fn an_unencodable_press_still_focuses_the_pane() {
        let mut h = AppMouseHarness::new().await;
        let pane = h.fixture.second_pane;
        let (mut wr, r) = install_app(&h, pane, (300, 40), b"\x1b[?1002h").await;
        let outcome = h
            .send(MouseKind::Down, ProtoMouseButton::Left, r.x + 2, r.y + 5)
            .await;
        assert_eq!(
            outcome,
            AppMouse::Consumed {
                redraw: true,
                input: true
            }
        );
        assert_eq!(drain(&mut wr), "");
        assert_eq!(
            h.fixture.graph.snapshot().windows[&h.fixture.first_window].active_pane,
            pane
        );
        h.fixture.stop();
    }

    /// An abandoned gesture's release names a cell the grid still has.
    #[tokio::test]
    async fn an_abandoned_gesture_releases_inside_a_shrunken_grid() {
        let mut h = AppMouseHarness::new().await;
        let pane = h.fixture.first_pane;
        let (mut wr, r) = install_app(&h, pane, (60, 40), b"\x1b[?1002h\x1b[?1006h").await;
        h.send(MouseKind::Down, ProtoMouseButton::Left, r.x + 2, r.y + 21)
            .await;
        assert!(drain(&mut wr).ends_with(";55M"));
        h.fixture
            .io_state
            .lock()
            .await
            .vts
            .get_mut(&pane)
            .expect("vt")
            .resize(20, 40);
        release_app_gesture(&h.fixture.io_state, &mut h.gesture, None).await;
        assert_eq!(drain(&mut wr), "\x1b[<0;3;20m");
        h.fixture.stop();
    }

    /// With the view offset applied, a legacy-encoded gesture can end on a row
    /// past what X10 can name. The press went out, so the release must too.
    #[tokio::test]
    async fn a_legacy_release_past_the_x10_limit_still_reaches_the_app() {
        let mut h = AppMouseHarness::new().await;
        let mut wr = seed_scrollback_pane(&h.fixture.io_state, h.fixture.first_pane, b"").await;
        let mut vt = shux_vt::VirtualTerminal::new(300, 49);
        vt.process(b"\x1b[?1002h\x1b[224;1H");
        h.fixture
            .io_state
            .lock()
            .await
            .vts
            .insert(h.fixture.first_pane, vt);

        // Offset 197: the pane's top row (screen 1) is grid row 198, its
        // bottom row (screen 27) is grid row 224.
        h.send(MouseKind::Down, ProtoMouseButton::Left, 3, 1).await;
        assert_eq!(recv_bytes(&mut wr).await, b"\x1b[M #\xe6");
        h.send(MouseKind::Up, ProtoMouseButton::Left, 3, 27).await;
        let got = recv_bytes(&mut wr).await;
        assert!(
            got.starts_with(b"\x1b[M#"),
            "the app never saw its release: {got:?}"
        );
        assert_eq!(h.gesture, SelectionDrag::None);
        h.fixture.stop();
    }

    /// A release must reach an app that saw the press even when the pane's rect
    /// has since collapsed to nothing, where no cell can be named.
    #[tokio::test]
    async fn a_release_on_a_collapsed_pane_still_reaches_the_app() {
        let mut h = AppMouseHarness::new().await;
        let mut wr = seed_scrollback_pane(
            &h.fixture.io_state,
            h.fixture.first_pane,
            b"\x1b[?1002h\x1b[?1006h",
        )
        .await;
        h.send(MouseKind::Down, ProtoMouseButton::Left, 2, 2).await;
        assert_eq!(recv_text(&mut wr).await, "\x1b[<0;2;2M");

        let mut collapsed = None;
        for cols in 1..10 {
            *h.client_size.lock().await = (cols, 30);
            let viewport = current_viewport(&h.client_size, &test_config()).await;
            let rect = h.fixture.graph.snapshot().windows[&h.fixture.first_window]
                .layout
                .compute_rects(viewport)
                .into_iter()
                .find(|(pid, _)| *pid == h.fixture.first_pane)
                .map(|(_, r)| r);
            if rect.is_some_and(|r| r.width == 0 || r.height == 0) {
                collapsed = Some(cols);
                break;
            }
        }
        assert!(collapsed.is_some(), "no client width collapses the pane");
        h.send(MouseKind::Up, ProtoMouseButton::Left, 0, 2).await;
        let got = recv_text(&mut wr).await;
        assert!(got.ends_with('m'), "the app never saw its release: {got:?}");
        assert_eq!(h.gesture, SelectionDrag::None);
        h.fixture.stop();
    }

    #[tokio::test]
    async fn a_click_off_a_background_panes_grid_still_focuses_it() {
        let mut h = AppMouseHarness::new().await;
        let (mut wr, rect) =
            install_app(&h, h.fixture.second_pane, (3, 4), b"\x1b[?1002h\x1b[?1006h").await;

        let outcome = h
            .send(
                MouseKind::Down,
                ProtoMouseButton::Left,
                rect.x + 10,
                rect.y + 8,
            )
            .await;
        assert_eq!(
            outcome,
            AppMouse::Consumed {
                redraw: true,
                input: false
            },
            "it focuses, but a click on no cell takes no panes"
        );
        assert_no_bytes(&mut wr, "no grid cell under the click").await;
        assert_eq!(
            h.fixture.graph.snapshot().windows[&h.fixture.first_window].active_pane,
            h.fixture.second_pane,
            "a click inside a pane selects it, grid or not"
        );
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
            AppMouse::Consumed {
                redraw: true,
                input: true
            },
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
            AppMouse::Consumed {
                redraw: false,
                input: false
            },
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
        // The discriminating step. Drag and Up above are also refused by the
        // press-only rule, so they cannot tell the gesture guard apart from its
        // absence — a second PRESS while shux owns the gesture can, and it is a
        // real gesture (middle-click while text-selecting).
        assert_eq!(
            h.send(MouseKind::Down, ProtoMouseButton::Middle, 12, 5)
                .await,
            AppMouse::NotHandled,
            "a press during a shux selection must not open an app gesture"
        );
        assert_eq!(
            h.send(MouseKind::Up, ProtoMouseButton::Left, 12, 5).await,
            AppMouse::NotHandled
        );
        assert_no_bytes(&mut wr, "a gesture shux started stays shux's").await;
        h.fixture.stop();
    }

    /// An app that turns tracking OFF mid-gesture stops receiving reports, and
    /// the gesture still ends.
    ///
    /// No synthetic release here, deliberately: a real terminal stops reporting
    /// the moment the app clears the mode, and manufacturing a release it never
    /// subscribed to would be inventing input. The release IS synthesized where
    /// the app never asked to stop — see
    /// `abandoning_a_gesture_synthesizes_the_release_the_app_is_waiting_for`.
    /// What matters here is that the gesture does not stay latched.
    #[tokio::test]
    async fn an_app_that_drops_tracking_mid_gesture_stops_receiving_reports() {
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

        release_app_gesture(&h.fixture.io_state, &mut h.gesture, None).await;
        assert_eq!(
            recv_text(&mut wr).await,
            "\x1b[<0;14;7m",
            "the synthetic release must land where the drag left the pointer"
        );
        assert_eq!(h.gesture, SelectionDrag::None);
        // Idempotent: nothing left to release.
        release_app_gesture(&h.fixture.io_state, &mut h.gesture, None).await;
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
                    let state = h.fixture.io_state.lock().await;
                    let pane = h.fixture.first_pane;
                    let rect = Rect::new(0, 0, 49, 27);
                    s.copy_mode = Some(copy_mode_on(pane, rect, &state.vts[&pane]));
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
                state: shux_ui::Selection::default(),
                view: PaneViewport {
                    rect: Rect::new(0, 0, 1, 1),
                    grid_rows: 1,
                    grid_cols: 1,
                    row_offset: 0,
                    col_offset: 0,
                },
                generation: 1,
            });
        }
        assert_eq!(
            h.send(MouseKind::Down, ProtoMouseButton::Left, 10, 5).await,
            AppMouse::Consumed {
                redraw: true,
                input: true
            },
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
        let (writer_tx, writer_rx) = crate::pane_input::channel_with(1, crate::pane_input::BUDGET);
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
    /// with. Driven from a real config FILE, not from a `BorderStyle` handed in:
    /// the defect was in the plumbing from config to hit-test, not in the inset
    /// arithmetic, and a test passing the style in by hand cannot see it.
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

    /// Cross-path consistency: hit-test, compositor, snapshot composer and the
    /// PTY resize fan-out must all place a pane's rect identically, for every
    /// border style. Asserted through each path's real API — where `compose`
    /// lands a pane's first cell, what the fan-out actually sends — not by
    /// re-deriving the arithmetic and comparing it to itself.
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
                    chrome: None,
                    status_bar: None,
                    geometry: None,
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
                state.requested_sizes.clear();
                for (pid, _) in marks {
                    let (tx, rx) = mpsc::channel(8);
                    state.resizers.insert(pid, tx);
                    receivers.push((pid, rx));
                }
            }
            apply_resize_to_window(
                &fixture.graph,
                &fixture.io_state,
                fixture.attached.active_window_id,
                &config,
                100,
                30,
            )
            .await;
            for (pid, mut rx) in receivers {
                let req = tokio::time::timeout(shux_testkit::BUDGET, rx.recv())
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
        every_render_path_agrees_on_a_scrolled_slice(&fixture).await;
        fixture.stop();
    }

    /// The same claim for a slice scrolled on both axes, split and zoomed:
    /// the live attach bytes, the composed frame, the kitty placement and
    /// the raster pixels all show the same cells and the same cropped
    /// picture, and copy mode opened over that frame shows it unchanged.
    async fn every_render_path_agrees_on_a_scrolled_slice(fixture: &AttachFixture) {
        use base64::Engine as _;
        let pane = fixture.first_pane;
        let snap = fixture.graph.snapshot();
        let tree = snap.windows[&fixture.first_window].layout.tree.clone();
        drop(snap);
        let zoomed = shux_core::layout::ZoomState {
            zoomed_pane: pane,
            saved_layout: tree.clone(),
        };
        let green = [30u8, 200, 60];
        for zoom in [None, Some(&zoomed)] {
            let mut vt = lettered(40, 200);
            // Colour probes in the shown slice, then a 2x1-cell picture whose
            // first cell lies left of it.
            vt.process(b"\x1b[13;48H\x1b[38;2;0;200;90mT\x1b[38;5;208mI\x1b[34mB\x1b[0m");
            vt.process(b"\x1b[12;45H");
            let px: Vec<u8> = (0..18 * 19).flat_map(|_| green).collect();
            let b64 = base64::engine::general_purpose::STANDARD.encode(&px);
            vt.process(format!("\x1b_Ga=T,f=24,s=18,v=19,C=1;{b64}\x1b\\").as_bytes());
            let prepare = |vts: &HashMap<PaneId, &shux_vt::VirtualTerminal>| {
                shux_ui::prepare_frame(&shux_ui::FrameInputs {
                    layout: &tree,
                    zoom,
                    cols: 100,
                    rows: 30,
                    status_bar_height: STATUS_BAR_ROWS,
                    border_style: BorderStyle::parse("rounded"),
                    panes: &shux_ui::PresentedPane::of_vts(vts),
                    now_ns: 0,
                })
            };
            let rect = prepare(&HashMap::from([(pane, &vt)])).views[&pane].rect;
            vt.process(&follow_to(rect, 10, 45));
            let vts = HashMap::from([(pane, &vt)]);
            let geometry = prepare(&vts);
            let view = geometry.views[&pane];
            let what = if zoom.is_some() { "zoomed" } else { "split" };
            assert_eq!((view.row_offset, view.col_offset), (10, 45), "{what}");
            let expect = |r: usize, c: usize| char::from(b'a' + ((10 + r + 45 + c) % 26) as u8);

            // 1. Live attach bytes, replayed into a terminal.
            let cfg = CompositorConfig {
                border_style: BorderStyle::parse("rounded"),
                status_bar_height: STATUS_BAR_ROWS,
                ..Default::default()
            };
            let mut compositor = RenderCompositor::new(100, 30, Vec::new(), cfg);
            compositor.set_graphics(true);
            let frame = MultiPaneFrame {
                layout: &tree,
                zoom,
                focused: pane,
                vts: &vts,
                chrome: None,
                status_bar: None,
                geometry: Some(&geometry),
            };
            compositor.render_multi_pane(frame).expect("render");
            let live = std::mem::take(compositor.inner_mut());
            let mut screen = shux_vt::VirtualTerminal::new(30, 100);
            screen.process(&live);

            // 2. The composed frame snapshots draw.
            let panes = HashMap::from([(pane, (vt.grid(), vt.cursor()))]);
            let composed = shux_ui::compose(
                &shux_ui::ComposeInputs {
                    layout: &tree,
                    zoom,
                    focused: pane,
                    panes: &panes,
                    chrome: None,
                    status_bar: None,
                    geometry: Some(&geometry),
                },
                100,
                30,
                BorderStyle::parse("rounded"),
                shux_ui::BorderColors::default(),
                STATUS_BAR_ROWS,
            );
            let (x0, y0) = (usize::from(rect.x), usize::from(rect.y));
            for (r, c) in [
                (0, 0),
                (0, 1),
                (4, 7),
                (usize::from(rect.height) - 1, usize::from(rect.width) - 1),
            ] {
                let want = expect(r, c);
                assert_eq!(
                    screen.grid().visible_row(y0 + r)[x0 + c].ch,
                    want,
                    "{what}: live ({r},{c})"
                );
                assert_eq!(
                    composed.grid.visible_row(y0 + r)[x0 + c].ch,
                    want,
                    "{what}: composed ({r},{c})"
                );
            }
            // The probes, coloured on both paths: grid row 12, columns 47..50.
            for path in [screen.grid(), &composed.grid] {
                let probe = path.visible_row(y0 + 2);
                assert_eq!(
                    [
                        probe[x0 + 2].style.fg,
                        probe[x0 + 3].style.fg,
                        probe[x0 + 4].style.fg
                    ],
                    [
                        shux_vt::Color::Rgb(0, 200, 90),
                        shux_vt::Color::Indexed(208),
                        shux_vt::Color::Indexed(4)
                    ],
                    "{what}: colour probes"
                );
            }
            assert_eq!(
                view.cursor_cell(vt.cursor()),
                Some((rect.x + rect.width - 1, rect.y + rect.height - 1)),
                "{what}: cursor"
            );

            // 3. Kitty: the picture's second cell at the slice's first, its
            //    first nine source columns cropped off.
            let live = String::from_utf8_lossy(&live);
            let put = format!("\x1b[{};{}H\x1b_Ga=T", y0 + 2, x0 + 1);
            assert!(
                live.contains(&put),
                "{what}: kitty placement not at {put:?}"
            );
            assert!(
                live.contains("x=9,y=0,w=9,h=19,c=1,r=1"),
                "{what}: kitty crop"
            );

            // 4. Raster: that cell green, the border left of it not.
            let rast = shux_raster::Rasterizer::new(14.0).expect("rasterizer");
            let (cw, ch) = rast.cell_size();
            let mut img = rast.render_regions(
                &composed.grid,
                &shux_raster::RasterOptions::default(),
                &composed.regions,
            );
            rast.composite_composed(&mut img, &composed.placements);
            let at = |x: u32, y: u32| {
                let p = img.get_pixel(x, y).0;
                [p[0], p[1], p[2]]
            };
            let (px0, py) = (x0 as u32 * cw, (y0 as u32 + 1) * ch + ch / 2);
            assert_eq!(at(px0 + cw / 2, py), green, "{what}: raster crop");
            assert_ne!(
                at(px0 + cw + cw / 2, py),
                green,
                "{what}: raster ran past the picture"
            );
            assert!(
                (0..px0).all(|x| at(x, py) != green),
                "{what}: the cropped cell landed left of the pane"
            );

            // 5. Copy mode over that frame draws the same slice, its picture
            //    where the frame put it.
            let cm = shux_ui::CopyModeState::enter(pane, view, shux_ui::Drawn::of(&vt), &vt)
                .expect("copy mode");
            assert_eq!(cm.view.frame_view(&vt), (view, false), "{what}: copy view");
            let mut copy = Vec::new();
            shux_ui::render_copy_view_into(&mut copy, &vt, &cm);
            screen.process(&copy);
            for (r, c) in [(0, 0), (4, 7)] {
                assert_eq!(
                    screen.grid().visible_row(y0 + r)[x0 + c].ch,
                    expect(r, c),
                    "{what}: copy view"
                );
            }
        }
    }

    /// A live `border_style` edit moves every pane's rect, so the PTYs must be
    /// re-told their size or they are drawn at one geometry and believe another.
    /// Drives the real `ConfigHandle::replace` and the real fan-out.
    #[tokio::test]
    async fn a_border_style_reload_re_fans_every_pane_winsize() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[appearance]\nborder_style = \"rounded\"\n").expect("write");
        let config = ConfigHandle::load_or_default(&path);

        let fixture = attach_fixture().await;
        let client_size: ClientSize = Arc::new(Mutex::new((100, 30)));
        let mut receivers = Vec::new();
        {
            let mut state = fixture.io_state.lock().await;
            for pid in [fixture.first_pane, fixture.second_pane] {
                let (tx, rx) = mpsc::channel(8);
                state.resizers.insert(pid, tx);
                receivers.push((pid, rx));
            }
        }

        // Baseline: the outline is drawn, so the viewport is inset.
        let (cols, rows) = *client_size.lock().await;
        apply_resize_to_window(
            &fixture.graph,
            &fixture.io_state,
            fixture.attached.active_window_id,
            &config,
            cols,
            rows,
        )
        .await;
        let mut before = Vec::new();
        for (pid, rx) in receivers.iter_mut() {
            let req = tokio::time::timeout(shux_testkit::BUDGET, rx.recv())
                .await
                .expect("baseline resize")
                .expect("baseline resize");
            before.push((*pid, req.size.cols, req.size.rows));
        }

        // The edit the user makes. `replace` is what the config watcher calls.
        let mut reloaded = (*config.current()).clone();
        reloaded.appearance.border_style = "none".to_string();
        config.replace(reloaded);

        apply_resize_to_window(
            &fixture.graph,
            &fixture.io_state,
            fixture.attached.active_window_id,
            &config,
            cols,
            rows,
        )
        .await;
        for ((pid, was_cols, was_rows), (_, rx)) in before.iter().zip(receivers.iter_mut()) {
            let req = tokio::time::timeout(shux_testkit::BUDGET, rx.recv())
                .await
                .expect("post-reload resize")
                .expect("post-reload resize");
            assert!(
                req.size.cols > *was_cols || req.size.rows > *was_rows,
                "pane {pid} kept {was_cols}x{was_rows} after the outline was \
                 turned off; it is now drawn larger than it was told"
            );
        }
        fixture.stop();
    }

    /// The wheel stands down under a coordinate mode shux cannot encode, just as
    /// the button path does, and falls back to shux's own scrollback.
    #[tokio::test]
    async fn the_wheel_stands_down_under_a_coordinate_mode_shux_cannot_encode() {
        let fixture = attach_fixture().await;
        let mut wr = seed_scrollback_pane(
            &fixture.io_state,
            fixture.first_pane,
            b"\x1b[?1002h\x1b[?1006h\x1b[?1016h",
        )
        .await;
        let session = Arc::new(Mutex::new(fixture.attached.clone()));
        let client_size: ClientSize = Arc::new(Mutex::new((100, 30)));

        ship_frame(&fixture.graph, &fixture.io_state, &session, &client_size).await;
        handle_wheel(
            MouseKind::ScrollUp,
            3,
            3,
            None,
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &test_config(),
        )
        .await
        .expect("wheel");

        assert_no_bytes(&mut wr, "1016 makes the app read cells as pixels").await;
        assert!(
            session.lock().await.copy_mode.is_some(),
            "the wheel should have fallen back to shux's scrollback"
        );
        fixture.stop();
    }

    /// An app that switches to an unencodable coordinate mode mid-drag is still
    /// tracking and still waiting for the button: send the release anyway.
    /// Contrast `an_app_that_drops_tracking_mid_gesture_stops_receiving_reports`
    /// — an app that turned tracking OFF never subscribed to it.
    #[tokio::test]
    async fn an_app_that_switches_to_an_unencodable_mode_mid_gesture_still_gets_its_release() {
        let mut h = AppMouseHarness::new().await;
        let mut wr = seed_scrollback_pane(
            &h.fixture.io_state,
            h.fixture.first_pane,
            b"\x1b[?1002h\x1b[?1006h",
        )
        .await;
        h.send(MouseKind::Down, ProtoMouseButton::Left, 10, 5).await;
        assert_eq!(recv_text(&mut wr).await, "\x1b[<0;10;5M");

        // The app asks for SGR-pixel coordinates mid-gesture.
        {
            let mut st = h.fixture.io_state.lock().await;
            st.vts
                .get_mut(&h.fixture.first_pane)
                .expect("vt")
                .process(b"\x1b[?1016h");
        }
        h.send(MouseKind::Up, ProtoMouseButton::Left, 10, 5).await;

        assert_eq!(
            recv_text(&mut wr).await,
            "\x1b[<0;10;5m",
            "the app is still tracking and still waiting for this release"
        );
        assert_eq!(h.gesture, SelectionDrag::None, "the gesture must end");
        h.fixture.stop();
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

        ship_frame(&fixture.graph, &fixture.io_state, &session, &client_size).await;
        let consumed = handle_wheel(
            MouseKind::ScrollUp,
            3,
            3,
            None,
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
            cm.view.top < cm.view.entry_top,
            "the view must scroll up from where it opened: {:?}",
            cm.view
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

        ship_frame(&fixture.graph, &fixture.io_state, &session, &client_size).await;
        handle_wheel(
            MouseKind::ScrollUp,
            3,
            3,
            None,
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

        ship_frame(&fixture.graph, &fixture.io_state, &session, &client_size).await;
        handle_wheel(
            MouseKind::ScrollDown,
            3,
            3,
            None,
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
        ship_frame(&fixture.graph, &fixture.io_state, &session, &client_size).await;
        handle_wheel(
            MouseKind::ScrollUp,
            3,
            3,
            None,
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
            assert!(cm.view.top < cm.view.entry_top);
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
                None,
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

    /// A wheel tick that reaches the copy-mode handler after its pane changed
    /// screen (past the settle that routing starts with) ends copy mode and
    /// is left for the routing that follows, not swallowed.
    #[tokio::test]
    async fn a_wheel_the_copy_mode_handler_finds_unmapped_is_routed_on() {
        let fixture = attach_fixture().await;
        let _wr = seed_scrollback_pane(&fixture.io_state, fixture.first_pane, b"").await;
        let session = Arc::new(Mutex::new(fixture.attached.clone()));
        let client_size: ClientSize = Arc::new(Mutex::new((100, 30)));
        open_copy_mode(&fixture.graph, &fixture.io_state, &session, &client_size).await;
        fixture
            .io_state
            .lock()
            .await
            .vts
            .get_mut(&fixture.first_pane)
            .expect("vt")
            .process(b"\x1b[?1049h\x1b[?1000h\x1b[?1006h");
        let (out_tx, _out_rx) = mpsc::channel(8);
        for kind in [MouseKind::ScrollUp, MouseKind::Down] {
            let handled = handle_copy_mode_mouse(
                kind,
                ProtoMouseButton::Left,
                3,
                3,
                &fixture.graph,
                &fixture.io_state,
                &session,
                &client_size,
                &test_config(),
                &out_tx,
                &mut SelectionDrag::None,
                None,
            )
            .await
            .expect("copy-mode mouse");
            assert!(
                !handled,
                "{kind:?} swallowed by a copy mode that no longer maps"
            );
            assert!(session.lock().await.copy_mode.is_none());
            open_copy_mode(&fixture.graph, &fixture.io_state, &session, &client_size).await;
            fixture
                .io_state
                .lock()
                .await
                .vts
                .get_mut(&fixture.first_pane)
                .expect("vt")
                .process(b"\x1b[?1049l");
        }
        fixture.stop();
    }

    /// A click whose copy mode stops mapping after routing began goes on to
    /// the app that asked for the mouse, as one arriving after it would.
    #[tokio::test]
    async fn a_click_copy_mode_finds_unmapped_is_routed_to_the_app() {
        let fixture = attach_fixture().await;
        let _wr = seed_scrollback_pane(&fixture.io_state, fixture.first_pane, b"").await;
        let session = Arc::new(Mutex::new(fixture.attached.clone()));
        let client_size: ClientSize = Arc::new(Mutex::new((100, 30)));
        open_copy_mode(&fixture.graph, &fixture.io_state, &session, &client_size).await;
        fixture
            .io_state
            .lock()
            .await
            .vts
            .get_mut(&fixture.first_pane)
            .expect("vt")
            .process(b"\x1b[?1049h\x1b[?1000h\x1b[?1006h");
        let (out_tx, _out_rx) = mpsc::channel(8);
        let route = route_mouse(
            MouseKind::Down,
            ProtoMouseButton::Left,
            (3, 3),
            (false, false, false),
            None,
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &test_config(),
            &out_tx,
            &None,
            &mut SelectionDrag::None,
        )
        .await;
        assert!(
            matches!(route, MouseRoute::App { input: true, .. }),
            "the click went to {route:?}, not the app"
        );
        assert!(session.lock().await.copy_mode.is_none());
        fixture.stop();
    }

    /// A resize lands under a copy mode nothing was done in yet: the wheel
    /// reopens it over the new grid, as a key does, rather than ending it.
    #[tokio::test]
    async fn the_wheel_reopens_an_untouched_copy_mode_as_a_key_does() {
        let fixture = attach_fixture().await;
        let _wr = seed_scrollback_pane(&fixture.io_state, fixture.first_pane, b"").await;
        let session = Arc::new(Mutex::new(fixture.attached.clone()));
        let client_size: ClientSize = Arc::new(Mutex::new((100, 30)));
        open_copy_mode(&fixture.graph, &fixture.io_state, &session, &client_size).await;
        resize_vt(&fixture, fixture.first_pane, 20, 49).await;
        let (out_tx, _out_rx) = mpsc::channel(8);
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
            &mut SelectionDrag::None,
            None,
        )
        .await
        .expect("copy-mode wheel up");
        let rows = session
            .lock()
            .await
            .copy_mode
            .as_ref()
            .map(|cm| cm.view.entry.grid_rows);
        assert_eq!(
            rows,
            Some(20),
            "the wheel ended a copy mode a key would reopen"
        );
        fixture.stop();
    }

    /// A wheel-up that lands after a resize no frame has shown yet opens copy
    /// mode over the slice the next frame draws, as `prefix [` does.
    #[tokio::test]
    async fn a_wheel_after_a_resize_opens_copy_mode_over_the_next_frame() {
        let fixture = attach_fixture().await;
        let _wr = seed_scrollback_pane(&fixture.io_state, fixture.first_pane, b"").await;
        let session = Arc::new(Mutex::new(fixture.attached.clone()));
        let client_size: ClientSize = Arc::new(Mutex::new((100, 30)));
        ship_frame(&fixture.graph, &fixture.io_state, &session, &client_size).await;
        resize_vt(&fixture, fixture.first_pane, 20, 49).await;
        handle_wheel(
            MouseKind::ScrollUp,
            3,
            3,
            None,
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &test_config(),
        )
        .await
        .expect("wheel up");
        let rows = session
            .lock()
            .await
            .copy_mode
            .as_ref()
            .map(|cm| cm.view.entry.grid_rows);
        assert_eq!(rows, Some(20), "a wheel-up after a resize was swallowed");
        fixture.stop();
    }

    /// Output after the frame the wheel names scrolls the pane: copy mode
    /// opens on the lines that frame showed, then scrolls up from them.
    #[tokio::test]
    async fn a_wheel_opens_on_the_lines_its_frame_showed() {
        let fixture = attach_fixture().await;
        let pane = fixture.first_pane;
        let _wr = seed_scrollback_pane(&fixture.io_state, pane, b"").await;
        let session = Arc::new(Mutex::new(fixture.attached.clone()));
        let client_size: ClientSize = Arc::new(Mutex::new((100, 30)));
        ship_frame(&fixture.graph, &fixture.io_state, &session, &client_size).await;
        let shown_top = {
            let mut state = fixture.io_state.lock().await;
            let vt = state.vts.get_mut(&pane).expect("vt");
            let top = vt.presented_total_lines() - vt.grid().rows();
            vt.process(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\n");
            top
        };
        handle_wheel(
            MouseKind::ScrollUp,
            3,
            3,
            None,
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &test_config(),
        )
        .await
        .expect("wheel up");
        let entry = session
            .lock()
            .await
            .copy_mode
            .as_ref()
            .map(|cm| cm.view.entry_top);
        assert_eq!(
            entry,
            Some(shown_top),
            "copy mode opened below the frame's top line"
        );
        fixture.stop();
    }

    /// A wheel stamped with a frame no longer kept opens nothing, and says so
    /// as `prefix [` does.
    #[tokio::test]
    async fn a_wheel_with_an_expired_stamp_says_why_it_did_nothing() {
        let fixture = attach_fixture().await;
        let _wr = seed_scrollback_pane(&fixture.io_state, fixture.first_pane, b"").await;
        let session = Arc::new(Mutex::new(fixture.attached.clone()));
        let client_size: ClientSize = Arc::new(Mutex::new((100, 30)));
        ship_frame(&fixture.graph, &fixture.io_state, &session, &client_size).await;
        ship_frame(&fixture.graph, &fixture.io_state, &session, &client_size).await;
        session.lock().await.shown.0.pop_front();
        handle_wheel(
            MouseKind::ScrollUp,
            3,
            3,
            Some(1),
            &fixture.graph,
            &fixture.io_state,
            &session,
            &client_size,
            &test_config(),
        )
        .await
        .expect("wheel up");
        let s = session.lock().await;
        assert!(s.copy_mode.is_none());
        assert!(
            s.action_error
                .as_deref()
                .is_some_and(|e| e.contains("screen changed")),
            "an expired wheel said nothing: {:?}",
            s.action_error
        );
        drop(s);
        fixture.stop();
    }

    async fn resize_vt(fixture: &AttachFixture, pane: PaneId, rows: usize, cols: usize) {
        let mut state = fixture.io_state.lock().await;
        state.vts.get_mut(&pane).expect("vt").resize(rows, cols);
    }

    /// `prefix [` opens copy mode on the pane focused when it was pressed,
    /// though focus moves while the action waits for the client's size.
    #[tokio::test]
    async fn copy_mode_opens_on_the_pane_focused_when_its_key_was_read() {
        let fixture = attach_fixture().await;
        let _a = seed_scrollback_pane(&fixture.io_state, fixture.first_pane, b"").await;
        let _b = seed_scrollback_pane(&fixture.io_state, fixture.second_pane, b"").await;
        let session = Arc::new(Mutex::new(fixture.attached.clone()));
        let client_size: ClientSize = Arc::new(Mutex::new((100, 30)));
        ship_frame(&fixture.graph, &fixture.io_state, &session, &client_size).await;
        let held = client_size.lock().await;
        let action = tokio::spawn({
            let (graph, io, session, size) = (
                fixture.graph.clone(),
                fixture.io_state.clone(),
                session.clone(),
                client_size.clone(),
            );
            let cancel = fixture.cancel.clone();
            async move {
                handle_action(
                    ActionKind::EnterCopyMode,
                    Default::default(),
                    &graph,
                    &io,
                    &session,
                    &size,
                    &test_config(),
                    &cancel,
                )
                .await
            }
        });
        // One thread: the action runs until it waits on the client's size.
        tokio::task::yield_now().await;
        session.lock().await.active_pane_id = fixture.second_pane;
        drop(held);
        action.await.expect("join").expect("enter copy mode");
        let pane = session.lock().await.copy_mode.as_ref().map(|cm| cm.pane);
        assert_eq!(
            pane,
            Some(fixture.first_pane),
            "copy mode opened on a pane focused after its key"
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
        let session = Arc::new(Mutex::new(fixture.attached.clone()));
        let client_size = Arc::new(Mutex::new((100, 30)));
        // wheel_initiated = false
        open_copy_mode(&fixture.graph, &fixture.io_state, &session, &client_size).await;
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
            None,
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
                None,
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
        ship_frame(&fixture.graph, &fixture.io_state, &session, &client_size).await;
        handle_wheel(
            MouseKind::ScrollUp,
            first_point.0,
            first_point.1,
            None,
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
            None,
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
        ship_frame(&fixture.graph, &fixture.io_state, &session, &client_size).await;
        handle_wheel(
            MouseKind::ScrollUp,
            second_point.0,
            second_point.1,
            None,
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
            s.copy_mode.as_ref().is_some_and(|cm| cm.pane == fixture.second_pane
                && cm.view.top < cm.view.entry_top),
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
        let session = Arc::new(Mutex::new(fixture.attached.clone()));
        let client_size = Arc::new(Mutex::new((100, 30)));
        // wheel_initiated = false
        open_copy_mode(&fixture.graph, &fixture.io_state, &session, &client_size).await;
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
            None,
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

    /// The real attach connection, with a pane whose input channel the test
    /// owns: the test decides how fast the "program" reads.
    ///
    /// Every test that uses it runs `start_paused`. Its clients talk to the
    /// attach over an in-memory pipe, so input in flight is always a task
    /// that can run, and tokio moves the paused clock only once no task can:
    /// [`INPUT_STALL`] runs out exactly, in virtual time, and the clock moving
    /// at all means everything in flight has landed.
    struct InputHarness {
        fixture: AttachFixture,
        writer_rx: crate::pane_input::PaneInputRx,
        client: Client,
    }

    impl std::ops::Deref for InputHarness {
        type Target = Client;
        fn deref(&self) -> &Client {
            &self.client
        }
    }

    impl std::ops::DerefMut for InputHarness {
        fn deref_mut(&mut self) -> &mut Client {
            &mut self.client
        }
    }

    impl InputHarness {
        async fn new(capacity: usize) -> Self {
            Self::sized(capacity, 90).await
        }

        async fn sized(capacity: usize, cols: u16) -> Self {
            // Panics unless the clock is paused: only then are the waits below
            // fences rather than guesses.
            tokio::time::advance(Duration::ZERO).await;
            let fixture = attach_fixture().await;
            let _ = seed_io_for_pane(&fixture.io_state, fixture.first_pane).await;
            let (writer_tx, writer_rx) =
                crate::pane_input::channel_with(capacity, crate::pane_input::BUDGET);
            fixture
                .io_state
                .lock()
                .await
                .writers
                .insert(fixture.first_pane, writer_tx);
            let client = Client::attach(&fixture, cols).await;
            Self {
                fixture,
                writer_rx,
                client,
            }
        }

        /// A second client attached to the same pane.
        async fn peer(&self) -> Client {
            Client::attach(&self.fixture, 90).await
        }

        /// The next input the pane receives.
        async fn next_input(&mut self) -> Vec<u8> {
            tokio::time::timeout(shux_testkit::BUDGET, self.writer_rx.recv())
                .await
                .expect("no input reached the pane")
                .expect("pane input channel closed")
        }

        async fn stop(self) {
            self.client.stop().await;
            self.fixture.stop();
        }
    }

    /// One attached client: its end of the connection, and its screen so far.
    struct Client {
        framed: Framed<tokio::io::DuplexStream, tokio_util::codec::LengthDelimitedCodec>,
        /// Kept across waits: later render frames redraw only what changed.
        screen: shux_vt::VirtualTerminal,
        /// The last numbered frame applied to `screen`, as the real client
        /// stamps its mouse events.
        written_frame: Option<u64>,
        /// Every frame received, applied or not: what `receive_unapplied`
        /// reads to know a frame in transit shows what it waits for.
        shadow: shux_vt::VirtualTerminal,
        cancel: CancellationToken,
        server: tokio::task::JoinHandle<anyhow::Result<()>>,
    }

    impl Client {
        /// A client attached to the fixture's session, past the handshake.
        async fn attach(fixture: &AttachFixture, cols: u16) -> Self {
            let onboarding = crate::onboarding::OnboardingHandle::from_state_for_test(
                crate::onboarding::OnboardingState {
                    prefix_discovered: true,
                    welcome_toast_seen: true,
                },
            );
            Self::attach_onboarded(fixture, cols, onboarding, false).await
        }

        async fn attach_onboarded(
            fixture: &AttachFixture,
            cols: u16,
            onboarding: crate::onboarding::OnboardingHandle,
            plain: bool,
        ) -> Self {
            Self::attach_drawing(fixture, cols, onboarding, plain, false).await
        }

        /// A client whose terminal draws kitty graphics when `graphics`.
        async fn attach_drawing(
            fixture: &AttachFixture,
            cols: u16,
            onboarding: crate::onboarding::OnboardingHandle,
            plain: bool,
            graphics: bool,
        ) -> Self {
            let (server_stream, client_stream) = tokio::io::duplex(1 << 20);
            let cancel = fixture.cancel.child_token();
            let server = tokio::spawn(handle_attach_connection(
                server_stream,
                fixture.graph.clone(),
                fixture.io_state.clone(),
                test_config(),
                SegmentCache::new(),
                crate::session_meta::SessionMetaCache::new(),
                onboarding,
                std::time::Instant::now(),
                cancel.clone(),
            ));
            let mut framed = Framed::new(client_stream, create_codec());
            let hello = AttachHello {
                protocol: ATTACH_PROTOCOL_VERSION,
                session_name: Some(fixture.attached.name.clone()),
                cols,
                rows: 24,
                client_version: "input-test".to_string(),
                graphics,
                plain,
            };
            framed
                .send(Bytes::from(serde_json::to_vec(&hello).expect("hello json")))
                .await
                .expect("send hello");
            let ready = tokio::time::timeout(shux_testkit::BUDGET, framed.next())
                .await
                .expect("ready timeout")
                .expect("ready frame")
                .expect("ready bytes");
            assert!(matches!(
                serde_json::from_slice::<AttachReady>(&ready).expect("ready json"),
                AttachReady::Ok { plain: echoed, .. } if echoed == plain
            ));
            Self {
                framed,
                screen: shux_vt::VirtualTerminal::new(24, usize::from(cols)),
                written_frame: None,
                shadow: shux_vt::VirtualTerminal::new(24, usize::from(cols)),
                cancel,
                server,
            }
        }

        async fn send(&mut self, frame: &AttachClientFrame) {
            self.framed
                .send(Bytes::from(serde_json::to_vec(frame).expect("frame json")))
                .await
                .expect("send frame");
        }

        /// Applies every frame that has arrived, as a client keeping up does.
        async fn catch_up(&mut self) {
            while let Ok(Some(Ok(buf))) = tokio::time::timeout(QUIET, self.framed.next()).await {
                let frame: AttachServerFrame = serde_json::from_slice(&buf).expect("server frame");
                if let AttachServerFrame::Render { data, frame } = frame {
                    let bytes = BASE64.decode(data.as_bytes()).expect("render base64");
                    self.screen.process(&bytes);
                    self.shadow.process(&bytes);
                    if frame.is_some() {
                        self.written_frame = frame;
                    }
                }
            }
        }

        async fn key(&mut self, bytes: &[u8]) {
            self.send(&AttachClientFrame::Input {
                data: BASE64.encode(bytes),
            })
            .await;
        }

        async fn paste(&mut self, text: &[u8], more: bool) {
            self.send(&AttachClientFrame::Paste {
                data: BASE64.encode(text),
                more,
            })
            .await;
        }

        /// Server frames until `pred` matches one, or [`shux_testkit::BUDGET`]
        /// runs out. `pred` also sees the client's screen: render frames
        /// position every cell, so their raw bytes never hold a word
        /// contiguously.
        async fn frames_until(
            &mut self,
            mut pred: impl FnMut(&AttachServerFrame, &str) -> bool,
        ) -> bool {
            let deadline = shux_testkit::Deadline::start();
            while let Some(Some(Ok(buf))) = deadline.run(self.framed.next()).await {
                let frame: AttachServerFrame = serde_json::from_slice(&buf).expect("server frame");
                if let AttachServerFrame::Render { data, frame } = &frame {
                    let bytes = BASE64.decode(data.as_bytes()).expect("render base64");
                    self.screen.process(&bytes);
                    self.shadow.process(&bytes);
                    if frame.is_some() {
                        self.written_frame = *frame;
                    }
                }
                let grid = self.screen.grid();
                let text: String = (0..grid.rows())
                    .flat_map(|r| {
                        let row = grid.visible_row(r);
                        (0..row.len()).map(|c| row[c].ch).chain(['\n'])
                    })
                    .collect();
                if pred(&frame, &text) {
                    return true;
                }
            }
            false
        }

        /// Takes server frames off the wire, without applying them, until a
        /// numbered one shows `needle`: a client that has not caught up yet,
        /// whose screen and mouse-event stamp stay where they were.
        async fn receive_unapplied(&mut self, needle: &str) {
            let deadline = shux_testkit::Deadline::start();
            while let Some(Some(Ok(buf))) = deadline.run(self.framed.next()).await {
                let frame: AttachServerFrame = serde_json::from_slice(&buf).expect("server frame");
                let AttachServerFrame::Render { data, frame } = frame else {
                    continue;
                };
                self.shadow
                    .process(&BASE64.decode(data.as_bytes()).expect("render base64"));
                let grid = self.shadow.grid();
                let shows = (0..grid.rows()).any(|r| {
                    let row = grid.visible_row(r);
                    (0..row.len())
                        .map(|c| row[c].ch)
                        .collect::<String>()
                        .contains(needle)
                });
                if frame.is_some() && shows {
                    return;
                }
            }
            panic!("no frame in transit showed {needle:?}");
        }

        /// Waits for the status bar to say `amount` of this client's input
        /// was dropped.
        async fn told_dropped(&mut self, amount: &str) {
            let notice = format!("not keeping up: {amount} of input dropped");
            let mut bar = String::new();
            let told = self
                .frames_until(|_, text| {
                    bar = text.lines().last().unwrap_or_default().to_string();
                    text.contains(&notice)
                })
                .await;
            assert!(told, "never told {notice:?}; the status bar reads {bar:?}");
        }

        async fn detach(&mut self) {
            self.send(&AttachClientFrame::Detach).await;
            let detached = self
                .frames_until(|f, _| matches!(f, AttachServerFrame::DetachAck))
                .await;
            assert!(detached, "detach never landed");
        }

        async fn stop(self) {
            drop(self.framed);
            self.cancel.cancel();
            let _ = tokio::time::timeout(shux_testkit::BUDGET, self.server).await;
        }
    }

    /// Lets every task run until all of them wait, then moves the paused
    /// clock on by `by`, firing each timer due on the way.
    async fn idle_for(by: Duration) {
        #[expect(
            clippy::disallowed_methods,
            reason = "drives the paused clock: it moves only once every task waits"
        )]
        tokio::time::sleep(by).await;
    }

    /// How long, in virtual time, the attach must have had nothing left to do
    /// before the pane's input counts as complete.
    const QUIET: Duration = Duration::from_millis(200);

    /// Everything the pane receives until the attach has nothing left to do:
    /// the clock is paused, so [`QUIET`] passing with the pane's channel empty
    /// means no task has input on its way to it.
    async fn pane_input(h: &mut InputHarness) -> Vec<u8> {
        let mut got = Vec::new();
        while let Ok(Some(chunk)) = tokio::time::timeout(QUIET, h.writer_rx.recv()).await {
            got.extend_from_slice(&chunk);
        }
        got
    }

    /// Issue #197: a paste is one Input frame per key, far more frames than a
    /// pane's channel holds. A pane that is reading, only slower than the
    /// frames arrive, must receive every byte in order.
    #[tokio::test(start_paused = true)]
    async fn a_paste_into_a_pane_that_reads_slowly_arrives_whole() {
        let mut h = InputHarness::new(8).await;
        let paste: Vec<u8> = (0..2000u32).map(|i| b'a' + (i % 26) as u8).collect();

        let mut rx = std::mem::replace(&mut h.writer_rx, crate::pane_input::channel_with(1, 1).1);
        let want = paste.len();
        let reader = tokio::spawn(async move {
            let mut got = Vec::with_capacity(want);
            while got.len() < want {
                match tokio::time::timeout(shux_testkit::BUDGET, rx.recv()).await {
                    Ok(Some(chunk)) => got.extend_from_slice(&chunk),
                    _ => break,
                }
                // A program that reads a burst, then is busy drawing it while
                // the attach keeps the pane's channel full.
                if got.len() % 16 == 0 {
                    for _ in 0..64 {
                        tokio::task::yield_now().await;
                    }
                }
            }
            got
        });
        for &b in &paste {
            h.key(&[b]).await;
        }
        let got = reader.await.expect("reader");
        assert_eq!(
            got.len(),
            paste.len(),
            "{} of {} pasted bytes reached the pane",
            got.len(),
            paste.len()
        );
        assert!(got == paste, "the paste arrived reordered");
        h.stop().await;
    }

    /// The other half of #197: a pane that stops reading altogether must not
    /// freeze the attach. Input it cannot take is dropped only after a bounded
    /// wait, the user is told on the status bar, and detach still works.
    #[tokio::test(start_paused = true)]
    async fn a_pane_that_stops_reading_drops_input_visibly_and_detach_still_works() {
        // Narrow enough that the notice only fits with the right zone gone.
        let mut h = InputHarness::sized(1, 56).await;
        for _ in 0..50 {
            h.key(b"x").await;
        }
        let mut screen = String::new();
        let told = h
            .frames_until(|_, text| {
                if text.contains("[p") {
                    screen = text.lines().last().unwrap_or_default().to_string();
                }
                text.contains("[pane not keeping up: 49 B of input dropped]")
            })
            .await;
        assert!(told, "the user was not told input was dropped: {screen:?}");

        let started = tokio::time::Instant::now();
        h.send(&AttachClientFrame::Detach).await;
        let detached = h
            .frames_until(|f, _| matches!(f, AttachServerFrame::DetachAck))
            .await;
        assert!(
            detached,
            "a stuck pane froze the attach: detach never landed"
        );
        // Virtual time: any wait on the pane would show here in full.
        assert!(
            started.elapsed() < INPUT_STALL,
            "detach waited {:?} behind a pane that is not reading",
            started.elapsed()
        );
        // The one byte the stuck pane's channel had room for.
        assert_eq!(h.writer_rx.try_recv().ok(), Some(b"x".to_vec()));
        h.stop().await;
    }

    /// A pane that reads, but slower than one frame per [`INPUT_STALL`],
    /// never stalls outright. It must still not hold the attach for longer
    /// than one budget: the wait is a total, not a per-key allowance.
    #[tokio::test(start_paused = true)]
    async fn a_pane_that_reads_too_slowly_cannot_hold_detach_behind_a_paste() {
        let mut h = InputHarness::new(1).await;
        let mut rx = std::mem::replace(&mut h.writer_rx, crate::pane_input::channel_with(1, 1).1);
        let reader = tokio::spawn(async move {
            while rx.recv().await.is_some() {
                #[expect(
                    clippy::disallowed_methods,
                    reason = "the program under test: one frame per 0.9 INPUT_STALL of virtual time"
                )]
                tokio::time::sleep(INPUT_STALL * 9 / 10).await;
            }
        });
        for _ in 0..10 {
            h.key(b"x").await;
        }
        let started = tokio::time::Instant::now();
        h.detach().await;
        assert!(
            started.elapsed() <= INPUT_STALL,
            "detach waited {:?} behind a pane that keeps falling behind",
            started.elapsed()
        );
        reader.abort();
        h.stop().await;
    }

    /// Two stuck panes, typed into in turn: each costs one wait, not one per
    /// focus change.
    #[tokio::test(start_paused = true)]
    async fn two_stuck_panes_each_cost_one_wait() {
        let mut h = InputHarness::new(1).await;
        let (second_tx, _second_rx) = crate::pane_input::channel_with(1, crate::pane_input::BUDGET);
        h.fixture
            .io_state
            .lock()
            .await
            .writers
            .insert(h.fixture.second_pane, second_tx);
        let started = tokio::time::Instant::now();
        for _ in 0..6 {
            for _ in 0..2 {
                h.key(b"a").await;
            }
            h.send(&AttachClientFrame::Action {
                kind: ActionKind::FocusNext,
                args: Default::default(),
            })
            .await;
        }
        h.detach().await;
        assert!(
            started.elapsed() <= INPUT_STALL * 2,
            "six focus changes between two stuck panes took {:?}",
            started.elapsed()
        );
        h.stop().await;
    }

    /// The notice counts every byte lost to a pane, even when one gets
    /// through in between.
    #[tokio::test(start_paused = true)]
    async fn the_drop_notice_counts_every_lost_byte() {
        let mut h = InputHarness::new(1).await;
        for _ in 0..5 {
            h.key(b"a").await;
        }
        h.told_dropped("4 B").await;
        assert_eq!(h.writer_rx.try_recv().ok(), Some(b"a".to_vec()));
        for _ in 0..3 {
            h.key(b"b").await;
        }
        // Not restarted by the byte that got through.
        h.told_dropped("6 B").await;
        h.stop().await;
    }

    #[test]
    fn dropped_input_is_counted_in_units_a_reader_takes_in() {
        assert_eq!(byte_count(1023), "1023 B");
        assert_eq!(byte_count(1024), "1.0 KiB");
        assert_eq!(byte_count((1 << 20) - 1), "1024.0 KiB");
        assert_eq!(byte_count(3 << 19), "1.5 MiB");
    }

    #[test]
    fn a_paste_is_bracketed_once_only_when_asked_and_cannot_end_itself() {
        let v = |b: &[u8]| b.to_vec();
        // Not asked for: as typed.
        assert_eq!(
            frame_input(v(b"ls\r"), Some(false), Open::Closed, false),
            (v(b"ls\r"), false)
        );
        // One piece.
        assert_eq!(
            frame_input(v(b"ls\r"), Some(false), Open::Closed, true),
            (v(b"\x1b[200~ls\r\x1b[201~"), false)
        );
        // Pieces: markers once around the whole paste.
        assert_eq!(
            frame_input(v(b"a"), Some(true), Open::Closed, true),
            (v(b"\x1b[200~a"), true)
        );
        assert_eq!(
            frame_input(v(b"b"), Some(true), Open::Continued, true),
            (v(b"b"), true)
        );
        assert_eq!(
            frame_input(v(b"c"), Some(false), Open::Continued, true),
            (v(b"c\x1b[201~"), false)
        );
        // Framed as it began, with the program out of paste mode since.
        assert_eq!(
            frame_input(v(b"c"), Some(false), Open::Continued, false),
            (v(b"c\x1b[201~"), false)
        );
        // The end was dropped: the next key, or another paste, closes it first.
        assert_eq!(
            frame_input(v(b"x"), None, Open::Other, true),
            (v(b"\x1b[201~x"), false)
        );
        assert_eq!(
            frame_input(v(b"z"), Some(false), Open::Other, true),
            (v(b"\x1b[201~\x1b[200~z\x1b[201~"), false)
        );
        assert_eq!(
            frame_input(v(b"x"), None, Open::Closed, true),
            (v(b"x"), false)
        );
        // An end marker inside the text cannot end the paste.
        assert_eq!(
            frame_input(v(b"a\x1b[201~rm -rf ~\r"), Some(false), Open::Closed, true),
            (v(b"\x1b[200~a[201~rm -rf ~\r\x1b[201~"), false)
        );
    }

    fn end_markers(bytes: &[u8]) -> usize {
        bytes
            .windows(PASTE_END.len())
            .filter(|w| *w == PASTE_END)
            .count()
    }

    #[test]
    fn a_pasted_end_marker_cannot_be_rebuilt_from_what_is_left() {
        let v = |b: &[u8]| b.to_vec();
        let (out, _) = frame_input(
            v(b"a\x1b[20\x1b[201~1~rm -rf ~\r"),
            Some(false),
            Open::Closed,
            true,
        );
        assert_eq!(end_markers(&out), 1, "the paste ends itself: {out:?}");
        assert!(out.ends_with(PASTE_END));

        // The client splits a paste anywhere a character ends, marker or not.
        let (first, _) = frame_input(v(b"aa\x1b[20"), Some(true), Open::Closed, true);
        let (second, _) = frame_input(v(b"1~rm -rf ~\r"), Some(false), Open::Continued, true);
        let joined = [first, second].concat();
        assert_eq!(end_markers(&joined), 1, "the paste ends itself: {joined:?}");
        assert!(joined.ends_with(PASTE_END));
    }

    /// Feeds `bytes` to the pane's VT, as its program would print them.
    async fn set_modes(h: &InputHarness, bytes: &[u8]) {
        let pane = h.fixture.first_pane;
        if let Some(vt) = h.fixture.io_state.lock().await.vts.get_mut(&pane) {
            vt.process(bytes);
        }
    }

    /// Replaces the pane's VT with a mouse-tracking app `rows` x `cols` whose
    /// row `i` reads `grid-row {i+1}`, cursor parked on the last row, and
    /// returns once the client's screen shows it.
    async fn mouse_app_grid(h: &mut InputHarness, rows: usize, cols: usize) {
        let mut vt = shux_vt::VirtualTerminal::new(rows, cols);
        vt.process(b"\x1b[?1002h\x1b[?1006h");
        let lines: Vec<String> = (1..=rows).map(|i| format!("grid-row {i:02}")).collect();
        vt.process(lines.join("\r\n").as_bytes());
        let last = format!("grid-row {rows:02}");
        {
            let mut state = h.fixture.io_state.lock().await;
            state.vts.insert(h.fixture.first_pane, vt);
            state.render_pulse.bump();
        }
        let shown = h.frames_until(|_, text| text.contains(&last)).await;
        assert!(shown, "the client never showed {last:?}");
    }

    /// Where the client's screen shows `needle`, 0-based, as the attach
    /// protocol addresses cells.
    async fn on_screen(h: &mut InputHarness, needle: &str) -> (u16, u16) {
        let mut at = None;
        h.frames_until(|_, text| {
            at = text.lines().enumerate().find_map(|(y, l)| {
                l.split_once(needle)
                    .map(|(before, _)| (before.chars().count() as u16, y as u16))
            });
            at.is_some()
        })
        .await;
        at.unwrap_or_else(|| panic!("{needle:?} is not on the client's screen"))
    }

    async fn mouse(h: &mut InputHarness, kind: MouseKind, col: u16, row: u16) {
        let frame = h.written_frame;
        h.send(&AttachClientFrame::Mouse {
            kind,
            button: ProtoMouseButton::Left,
            col,
            row,
            shift: false,
            alt: false,
            ctrl: false,
            frame,
        })
        .await;
    }

    /// Issue #192: a grid taller than its rect is drawn from a row offset, and
    /// a click must reach the app as the grid row the user sees.
    #[tokio::test(start_paused = true)]
    async fn a_click_on_an_oversized_pane_reports_the_grid_row_it_shows() {
        let mut h = InputHarness::new(64).await;
        mouse_app_grid(&mut h, 60, 40).await;
        let (x, y) = on_screen(&mut h, "grid-row 55").await;
        mouse(&mut h, MouseKind::Down, x, y).await;
        mouse(&mut h, MouseKind::Up, x, y).await;
        let got = String::from_utf8_lossy(&pane_input(&mut h).await).into_owned();
        assert!(
            got.starts_with("\x1b[<0;") && got.contains(";55M") && got.ends_with(";55m"),
            "clicked the row showing grid-row 55; the app was told {got:?}"
        );
        h.stop().await;
    }

    /// Over a slow link the daemon has sent a frame the client has not applied
    /// yet; a click names the cell of the frame the client shows, not the one
    /// still in transit.
    #[tokio::test(start_paused = true)]
    async fn a_click_names_the_frame_the_client_applied_not_one_in_transit() {
        let mut h = InputHarness::new(64).await;
        mouse_app_grid(&mut h, 60, 40).await;
        let (x, y) = on_screen(&mut h, "grid-row 55").await;
        {
            let mut state = h.fixture.io_state.lock().await;
            state
                .vts
                .get_mut(&h.fixture.first_pane)
                .expect("vt")
                .process(b"\x1b[1;1H");
            state.render_pulse.bump();
        }
        h.receive_unapplied("grid-row 01").await;
        mouse(&mut h, MouseKind::Down, x, y).await;
        let got = String::from_utf8_lossy(&pane_input(&mut h).await).into_owned();
        assert!(
            got.ends_with(";55M"),
            "clicked grid-row 55 as shown; the app was told {got:?}"
        );
        h.stop().await;
    }

    /// A layout change the client has not drawn yet: the click lands in the
    /// pane the client shows under the pointer, not in whatever pane the new
    /// layout puts there.
    #[tokio::test(start_paused = true)]
    async fn a_click_before_a_new_layout_is_drawn_reaches_the_pane_shown() {
        let mut h = InputHarness::new(64).await;
        mouse_app_grid(&mut h, 60, 40).await;
        let (x, y) = on_screen(&mut h, "grid-row 55").await;
        h.fixture
            .graph
            .swap_panes(h.fixture.first_pane, h.fixture.second_pane, None)
            .await
            .expect("swap panes");
        mouse(&mut h, MouseKind::Down, x, y).await;
        mouse(&mut h, MouseKind::Up, x, y).await;
        let got = String::from_utf8_lossy(&pane_input(&mut h).await).into_owned();
        assert_eq!(
            got, "\x1b[<0;1;55M\x1b[<0;1;55m",
            "clicked grid-row 55 as shown, before the swap was drawn"
        );
        h.stop().await;
    }

    /// A press stamped with a frame no longer kept is shux's to handle at the
    /// live layout, as before frames were stamped: on a border it drags. Only
    /// an app report needs the frame the client drew.
    #[tokio::test(start_paused = true)]
    async fn a_press_from_a_frame_no_longer_kept_is_handled_by_shux() {
        let mut h = InputHarness::new(64).await;
        mouse_app_grid(&mut h, 60, 40).await;
        let (_, y) = on_screen(&mut h, "grid-row 55").await;
        let border = {
            let grid = h.screen.grid();
            let row = grid.visible_row(usize::from(y));
            let bars: Vec<usize> = (0..row.len()).filter(|&c| row[c].ch == '│').collect();
            // The outline's two sides bracket the separator between the panes.
            assert_eq!(bars.len(), 3, "outline, separator, outline: {bars:?}");
            bars[1] as u16
        };
        let layout = |h: &InputHarness| {
            let snap = h.fixture.graph.snapshot();
            format!("{:?}", snap.windows[&h.fixture.first_window].layout.tree)
        };
        let before = layout(&h);
        for (kind, col) in [
            (MouseKind::Down, border),
            (MouseKind::Drag, border + 10),
            (MouseKind::Up, border + 10),
        ] {
            h.send(&AttachClientFrame::Mouse {
                kind,
                button: ProtoMouseButton::Left,
                col,
                row: y,
                shift: false,
                alt: false,
                ctrl: false,
                frame: Some(0),
            })
            .await;
        }
        assert!(
            pane_input(&mut h).await.is_empty(),
            "the app was sent a report"
        );
        assert_ne!(
            layout(&h),
            before,
            "an expired press did not drag the border"
        );
        h.stop().await;
    }

    /// Another client moved focus away; this client's click on the pane it
    /// last had active must still focus it, not be taken as already focused.
    #[tokio::test(start_paused = true)]
    async fn a_click_refocuses_a_pane_another_client_moved_focus_from() {
        let mut h = InputHarness::new(64).await;
        mouse_app_grid(&mut h, 60, 40).await;
        let (x, y) = on_screen(&mut h, "grid-row 55").await;
        h.fixture
            .graph
            .focus_pane(h.fixture.second_pane)
            .await
            .expect("focus the other pane, as a second client would");
        mouse(&mut h, MouseKind::Down, x, y).await;
        mouse(&mut h, MouseKind::Up, x, y).await;
        let _ = pane_input(&mut h).await;
        let active = h.fixture.graph.snapshot().windows[&h.fixture.first_window].active_pane;
        assert_eq!(
            active, h.fixture.first_pane,
            "the click did not take focus back"
        );
        h.stop().await;
    }

    /// The wheel's half of the same: a tick before a swap is drawn scrolls the
    /// pane the client shows under the pointer.
    #[tokio::test(start_paused = true)]
    async fn a_wheel_tick_before_a_new_layout_is_drawn_reaches_the_pane_shown() {
        let mut h = InputHarness::new(64).await;
        mouse_app_grid(&mut h, 60, 40).await;
        let (x, y) = on_screen(&mut h, "grid-row 55").await;
        h.fixture
            .graph
            .swap_panes(h.fixture.first_pane, h.fixture.second_pane, None)
            .await
            .expect("swap panes");
        let frame = h.written_frame;
        h.send(&AttachClientFrame::Mouse {
            kind: MouseKind::ScrollUp,
            button: ProtoMouseButton::None,
            col: x,
            row: y,
            shift: false,
            alt: false,
            ctrl: false,
            frame,
        })
        .await;
        let got = String::from_utf8_lossy(&pane_input(&mut h).await).into_owned();
        assert_eq!(got, "\x1b[<64;1;55M", "wheel over grid-row 55 as shown");
        h.stop().await;
    }

    /// The client names a cell in the frame it was shown. A cursor that moved
    /// below the fold since must not shift the click onto a row not yet drawn.
    #[tokio::test(start_paused = true)]
    async fn a_click_names_the_row_of_the_frame_the_client_was_shown() {
        let mut h = InputHarness::new(64).await;
        mouse_app_grid(&mut h, 60, 40).await;
        {
            let mut state = h.fixture.io_state.lock().await;
            state
                .vts
                .get_mut(&h.fixture.first_pane)
                .expect("vt")
                .process(b"\x1b[1;1H");
            state.render_pulse.bump();
        }
        let (x, y) = on_screen(&mut h, "grid-row 10").await;
        // The app moves its cursor to the bottom; no frame has shown it yet.
        h.fixture
            .io_state
            .lock()
            .await
            .vts
            .get_mut(&h.fixture.first_pane)
            .expect("vt")
            .process(b"\x1b[60;1H");
        mouse(&mut h, MouseKind::Down, x, y).await;
        let got = String::from_utf8_lossy(&pane_input(&mut h).await).into_owned();
        assert!(
            got.ends_with(";10M"),
            "clicked the row showing grid-row 10; the app was told {got:?}"
        );
        h.stop().await;
    }

    /// An app that moves its cursor on the press (vim) shifts the view under a
    /// held button; the release still names the cell the gesture began in.
    #[tokio::test(start_paused = true)]
    async fn a_click_whose_press_moves_the_view_stays_a_click() {
        let mut h = InputHarness::new(64).await;
        mouse_app_grid(&mut h, 60, 40).await;
        let (x, y) = on_screen(&mut h, "grid-row 55").await;
        mouse(&mut h, MouseKind::Down, x, y).await;
        {
            let mut state = h.fixture.io_state.lock().await;
            state
                .vts
                .get_mut(&h.fixture.first_pane)
                .expect("vt")
                .process(b"\x1b[1;1H");
            state.render_pulse.bump();
        }
        on_screen(&mut h, "grid-row 01").await;
        mouse(&mut h, MouseKind::Up, x, y).await;
        let got = String::from_utf8_lossy(&pane_input(&mut h).await).into_owned();
        assert_eq!(got, "\x1b[<0;1;55M\x1b[<0;1;55m", "a click became a drag");
        h.stop().await;
    }

    /// Once the press has moved the view, a drag away from the press point
    /// names the row the client now shows under the pointer.
    #[tokio::test(start_paused = true)]
    async fn a_drag_after_the_press_moved_the_view_names_the_row_now_shown() {
        let mut h = InputHarness::new(64).await;
        mouse_app_grid(&mut h, 60, 40).await;
        let (x, y) = on_screen(&mut h, "grid-row 55").await;
        mouse(&mut h, MouseKind::Down, x, y).await;
        {
            let mut state = h.fixture.io_state.lock().await;
            state
                .vts
                .get_mut(&h.fixture.first_pane)
                .expect("vt")
                .process(b"\x1b[1;1H");
            state.render_pulse.bump();
        }
        on_screen(&mut h, "grid-row 01").await;
        let (dx, dy) = on_screen(&mut h, "grid-row 10").await;
        mouse(&mut h, MouseKind::Drag, dx, dy).await;
        mouse(&mut h, MouseKind::Up, dx, dy).await;
        let got = String::from_utf8_lossy(&pane_input(&mut h).await).into_owned();
        assert_eq!(got, "\x1b[<0;1;55M\x1b[<32;1;10M\x1b[<0;1;10m");
        h.stop().await;
    }

    /// Once the pointer has left the press point, coming back to it names the
    /// row shown there now, not the press's.
    #[tokio::test(start_paused = true)]
    async fn a_drag_back_to_the_press_point_names_the_row_shown_there_now() {
        let mut h = InputHarness::new(64).await;
        mouse_app_grid(&mut h, 60, 40).await;
        let (x, y) = on_screen(&mut h, "grid-row 55").await;
        mouse(&mut h, MouseKind::Down, x, y).await;
        {
            let mut state = h.fixture.io_state.lock().await;
            state
                .vts
                .get_mut(&h.fixture.first_pane)
                .expect("vt")
                .process(b"\x1b[1;1H");
            state.render_pulse.bump();
        }
        on_screen(&mut h, "grid-row 01").await;
        let (dx, dy) = on_screen(&mut h, "grid-row 10").await;
        mouse(&mut h, MouseKind::Drag, dx, dy).await;
        let (bx, by) = on_screen(&mut h, &format!("grid-row {:02}", y)).await;
        assert_eq!((bx, by), (x, y), "the press point now shows grid-row {y}");
        mouse(&mut h, MouseKind::Drag, x, y).await;
        mouse(&mut h, MouseKind::Up, x, y).await;
        let got = String::from_utf8_lossy(&pane_input(&mut h).await).into_owned();
        assert_eq!(
            got,
            format!("\x1b[<0;1;55M\x1b[<32;1;10M\x1b[<32;1;{y}M\x1b[<0;1;{y}m")
        );
        h.stop().await;
    }

    /// A motion that moves the view (vim follows the drag with its cursor)
    /// must not shift a release at the same point onto another row.
    #[tokio::test(start_paused = true)]
    async fn a_release_where_the_drag_stopped_names_the_dragged_row() {
        let mut h = InputHarness::new(64).await;
        mouse_app_grid(&mut h, 60, 40).await;
        let (x, y) = on_screen(&mut h, "grid-row 55").await;
        mouse(&mut h, MouseKind::Down, x, y).await;
        let (dx, dy) = on_screen(&mut h, "grid-row 45").await;
        mouse(&mut h, MouseKind::Drag, dx, dy).await;
        {
            let mut state = h.fixture.io_state.lock().await;
            state
                .vts
                .get_mut(&h.fixture.first_pane)
                .expect("vt")
                .process(b"\x1b[1;1H");
            state.render_pulse.bump();
        }
        on_screen(&mut h, "grid-row 01").await;
        mouse(&mut h, MouseKind::Up, dx, dy).await;
        let got = String::from_utf8_lossy(&pane_input(&mut h).await).into_owned();
        assert_eq!(got, "\x1b[<0;1;55M\x1b[<32;1;45M\x1b[<0;1;45m");
        h.stop().await;
    }

    /// A wheel tick names the row of the frame the client was shown, like a click.
    #[tokio::test(start_paused = true)]
    async fn a_wheel_tick_names_the_row_of_the_frame_the_client_was_shown() {
        let mut h = InputHarness::new(64).await;
        mouse_app_grid(&mut h, 60, 40).await;
        let (x, y) = on_screen(&mut h, "grid-row 55").await;
        // The cursor moves up; no frame has shown the view that follows it.
        h.fixture
            .io_state
            .lock()
            .await
            .vts
            .get_mut(&h.fixture.first_pane)
            .expect("vt")
            .process(b"\x1b[1;1H");
        mouse(&mut h, MouseKind::ScrollDown, x, y).await;
        let got = String::from_utf8_lossy(&pane_input(&mut h).await).into_owned();
        assert!(
            got.ends_with(";55M"),
            "wheeled over grid-row 55; the app was told {got:?}"
        );
        h.stop().await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_wheel_tick_on_an_oversized_pane_reports_the_grid_row_it_shows() {
        let mut h = InputHarness::new(64).await;
        mouse_app_grid(&mut h, 60, 40).await;
        let (x, y) = on_screen(&mut h, "grid-row 55").await;
        mouse(&mut h, MouseKind::ScrollDown, x, y).await;
        let got = String::from_utf8_lossy(&pane_input(&mut h).await).into_owned();
        assert!(
            got.starts_with("\x1b[<65;") && got.ends_with(";55M"),
            "wheeled over the row showing grid-row 55; the app was told {got:?}"
        );
        h.stop().await;
    }

    /// The rect is bigger than this grid: space below and right of it names no
    /// cell the app has, so neither a click nor a wheel tick there is reported.
    #[tokio::test(start_paused = true)]
    async fn mouse_events_outside_the_grid_are_not_reported() {
        let mut h = InputHarness::new(64).await;
        mouse_app_grid(&mut h, 4, 12).await;
        let (x, y) = on_screen(&mut h, "grid-row 01").await;
        for (col, row) in [(x + 20, y), (x, y + 6)] {
            mouse(&mut h, MouseKind::Down, col, row).await;
            mouse(&mut h, MouseKind::Up, col, row).await;
            mouse(&mut h, MouseKind::ScrollUp, col, row).await;
        }
        let got = pane_input(&mut h).await;
        assert!(
            got.is_empty(),
            "events off the grid reached the app as {:?}",
            String::from_utf8_lossy(&got)
        );
        // The same events on the grid still arrive.
        mouse(&mut h, MouseKind::Down, x, y).await;
        let got = String::from_utf8_lossy(&pane_input(&mut h).await).into_owned();
        assert_eq!(got, "\x1b[<0;1;1M");
        h.stop().await;
    }

    /// A gesture that leaves the grid still ends: its release is pinned onto
    /// the nearest shown grid cell, never a cell the grid does not have.
    #[tokio::test(start_paused = true)]
    async fn a_release_off_the_grid_is_pinned_to_its_edge() {
        let mut h = InputHarness::new(64).await;
        mouse_app_grid(&mut h, 4, 12).await;
        let (x, y) = on_screen(&mut h, "grid-row 01").await;
        mouse(&mut h, MouseKind::Down, x, y).await;
        mouse(&mut h, MouseKind::Up, x + 30, y + 8).await;
        let got = String::from_utf8_lossy(&pane_input(&mut h).await).into_owned();
        assert_eq!(got, "\x1b[<0;1;1M\x1b[<0;12;4m");
        h.stop().await;
    }

    /// Pasted text is not a run of keystrokes: a `q` in it must not close
    /// the help overlay or quit copy mode and let the rest of the paste
    /// through to the pane.
    #[tokio::test(start_paused = true)]
    async fn a_paste_does_not_drive_the_help_overlay_or_copy_mode() {
        for (enter, leave) in [
            (ActionKind::ToggleHelp, b"\x1b".as_slice()),
            (ActionKind::EnterCopyMode, b"q".as_slice()),
        ] {
            let mut h = InputHarness::new(64).await;
            h.send(&AttachClientFrame::Action {
                kind: enter,
                args: shux_rpc::attach::ActionArgs::default(),
            })
            .await;
            h.paste(b"q", true).await;
            h.paste(b"echo half a script\r", false).await;
            h.key(b"a").await;
            h.key(leave).await;
            h.key(b"b").await;
            let got = pane_input(&mut h).await;
            assert_eq!(
                String::from_utf8_lossy(&got),
                "b",
                "{enter:?}: a paste reached the pane past the overlay"
            );
            h.stop().await;
        }
    }

    /// The pane's program after a paste whose end was dropped: left paste
    /// mode, or replaced by a respawn.
    async fn leave_the_paste(h: &mut InputHarness, respawn: bool) {
        let pane = h.fixture.first_pane;
        let mut state = h.fixture.io_state.lock().await;
        if respawn {
            state.incarnations.insert(pane, u64::MAX);
        } else if let Some(vt) = state.vts.get_mut(&pane) {
            vt.process(b"\x1b[?2004l");
        }
    }

    /// A paste into a pane of capacity 1 that is not reading: its start
    /// fills the pane, and its end is dropped once it has waited out
    /// [`INPUT_STALL`], which spends the pane's wait.
    async fn paste_with_dropped_end(h: &mut InputHarness) {
        set_modes(h, b"\x1b[?2004h").await;
        h.paste(b"a", true).await;
        h.paste(b"b", false).await;
        h.told_dropped("1 B").await;
    }

    /// An open paste belongs to the program it was pasted into: a program
    /// that is no longer in it must not read its end as input.
    #[tokio::test(start_paused = true)]
    async fn a_dropped_paste_end_reaches_only_the_program_still_in_it() {
        for respawn in [false, true] {
            let mut h = InputHarness::new(1).await;
            paste_with_dropped_end(&mut h).await;
            leave_the_paste(&mut h, respawn).await;
            h.detach().await;
            let got = pane_input(&mut h).await;
            assert_eq!(
                String::from_utf8_lossy(&got),
                "\x1b[200~a",
                "respawn {respawn}: detach ended a paste the program had left"
            );
            h.stop().await;

            let mut h = InputHarness::new(1).await;
            paste_with_dropped_end(&mut h).await;
            assert_eq!(pane_input(&mut h).await, b"\x1b[200~a");
            leave_the_paste(&mut h, respawn).await;
            h.key(b"x").await;
            let got = pane_input(&mut h).await;
            assert_eq!(
                String::from_utf8_lossy(&got),
                "x",
                "respawn {respawn}: a key ended a paste the program had left"
            );
            h.stop().await;
        }
    }

    /// A paste is the pane's, not the client's: a key from another client
    /// attached to the same pane ends it before reaching the program.
    #[tokio::test(start_paused = true)]
    async fn another_clients_key_ends_a_paste_left_open() {
        let mut h = InputHarness::new(1).await;
        paste_with_dropped_end(&mut h).await;
        assert_eq!(pane_input(&mut h).await, b"\x1b[200~a");
        let mut other = h.peer().await;
        other.key(b"x").await;
        let got = pane_input(&mut h).await;
        assert_eq!(
            String::from_utf8_lossy(&got),
            "\x1b[201~x",
            "another client's key went into the paste"
        );
        other.stop().await;
        h.stop().await;
    }

    /// Two writers waiting on a full pane end its open paste once between
    /// them: the second one queued finds it already ended.
    #[tokio::test(start_paused = true)]
    async fn waiting_writers_end_an_open_paste_once() {
        let mut h = InputHarness::new(1).await;
        paste_with_dropped_end(&mut h).await;
        let mut b = h.peer().await;
        let mut c = h.peer().await;
        b.key(b"x").await;
        // `x` is waiting for room before `y` is sent, and both before the
        // pane reads.
        idle_for(Duration::from_millis(1)).await;
        c.key(b"y").await;
        idle_for(Duration::from_millis(1)).await;
        let got = pane_input(&mut h).await;
        assert_eq!(
            String::from_utf8_lossy(&got),
            "\x1b[200~a\x1b[201~xy",
            "the paste was ended twice"
        );
        b.stop().await;
        c.stop().await;
        h.stop().await;
    }

    /// A key that waits behind a paste still waiting on a full pane lands
    /// after the paste's start, so it ends the paste first.
    #[tokio::test(start_paused = true)]
    async fn a_key_queued_behind_a_paste_ends_it() {
        let mut h = InputHarness::new(1).await;
        set_modes(&h, b"\x1b[?2004h").await;
        h.key(b"k").await;
        h.paste(b"a", true).await;
        // The paste is waiting for room behind `k`...
        idle_for(Duration::from_millis(1)).await;
        let mut other = h.peer().await;
        other.key(b"y").await;
        // ...and `y` behind the paste, before the pane reads.
        idle_for(Duration::from_millis(1)).await;
        let got = pane_input(&mut h).await;
        assert_eq!(
            String::from_utf8_lossy(&got),
            "k\x1b[200~a\x1b[201~y",
            "a key landed inside the paste"
        );
        other.stop().await;
        h.stop().await;
    }

    /// A new paste is its own paste, never the tail of one whose end was
    /// dropped, whichever client sends it.
    #[tokio::test(start_paused = true)]
    async fn a_new_paste_does_not_join_one_left_open() {
        for other_client in [false, true] {
            let mut h = InputHarness::new(1).await;
            paste_with_dropped_end(&mut h).await;
            assert_eq!(pane_input(&mut h).await, b"\x1b[200~a");
            let mut other = h.peer().await;
            if other_client {
                other.paste(b"z", false).await;
            } else {
                h.paste(b"z", false).await;
            }
            let got = pane_input(&mut h).await;
            assert_eq!(
                String::from_utf8_lossy(&got),
                "\x1b[201~\x1b[200~z\x1b[201~",
                "other client {other_client}: the paste joined the open one"
            );
            other.stop().await;
            h.stop().await;
        }
    }

    /// A paste whose first piece was dropped is still a new paste: its later
    /// pieces do not continue the one an earlier paste left open.
    #[tokio::test(start_paused = true)]
    async fn a_paste_whose_start_was_dropped_does_not_join_one_left_open() {
        let mut h = InputHarness::new(1).await;
        paste_with_dropped_end(&mut h).await;
        h.paste(b"p1", true).await;
        // Dropped at once: the pane has spent its wait.
        h.told_dropped("3 B").await;
        assert_eq!(pane_input(&mut h).await, b"\x1b[200~a");
        h.paste(b"p2", false).await;
        let got = pane_input(&mut h).await;
        assert_eq!(
            String::from_utf8_lossy(&got),
            "\x1b[201~\x1b[200~p2\x1b[201~",
            "the paste joined the open one"
        );
        h.stop().await;
    }

    /// A paste is framed as the program asked when it began: a mode change
    /// seen while its pieces arrive may be from before the program read its
    /// start, so the rest stays between the markers of one paste.
    #[tokio::test(start_paused = true)]
    async fn a_paste_keeps_the_framing_it_began_with() {
        for change in [b"\x1b[?2004l".as_slice(), b"\x1b[?2004l\x1b[?2004h"] {
            let mut h = InputHarness::new(8).await;
            set_modes(&h, b"\x1b[?2004h").await;
            h.paste(b"p1", true).await;
            assert_eq!(pane_input(&mut h).await, b"\x1b[200~p1");
            set_modes(&h, change).await;
            h.paste(b"p2\r", false).await;
            let got = pane_input(&mut h).await;
            assert_eq!(
                String::from_utf8_lossy(&got),
                "p2\r\x1b[201~",
                "{}: the paste was split",
                String::from_utf8_lossy(change).escape_debug()
            );
            h.stop().await;
        }
    }

    /// The rest of a paste goes where the paste began, not to a pane focused
    /// while it was arriving, which would run it as typed lines.
    #[tokio::test(start_paused = true)]
    async fn a_paste_stays_with_the_pane_it_began_in() {
        let mut h = InputHarness::new(8).await;
        set_modes(&h, b"\x1b[?2004h").await;
        let (mut second_rx, _resize, _shutdown) =
            seed_io_for_pane(&h.fixture.io_state, h.fixture.second_pane).await;
        h.paste(b"p1", true).await;
        h.send(&AttachClientFrame::Action {
            kind: ActionKind::FocusNext,
            args: shux_rpc::attach::ActionArgs::default(),
        })
        .await;
        let (graph, window, second) = (
            h.fixture.graph.clone(),
            h.fixture.first_window,
            h.fixture.second_pane,
        );
        shux_testkit::wait_until_async("focus to move to the second pane", || {
            let focused = graph
                .snapshot()
                .windows
                .get(&window)
                .is_some_and(|w| w.active_pane == second);
            async move { focused }
        })
        .await;
        h.paste(b"p2\r", false).await;
        let got = pane_input(&mut h).await;
        assert_eq!(
            String::from_utf8_lossy(&got),
            "\x1b[200~p1p2\r\x1b[201~",
            "the paste did not finish where it began"
        );
        // Fenced by `pane_input`: the attach had nothing left to do.
        let stray = second_rx.try_recv();
        assert!(
            stray.is_err(),
            "the newly focused pane got part of the paste: {stray:?}"
        );
        h.stop().await;
    }

    /// Detaching ends only the paste this client left open: one another
    /// client opened since stays open until that client goes.
    #[tokio::test(start_paused = true)]
    async fn detaching_leaves_another_clients_open_paste() {
        let mut h = InputHarness::new(1).await;
        paste_with_dropped_end(&mut h).await;
        let mut other = h.peer().await;
        other.key(b"x").await;
        assert_eq!(h.next_input().await, b"\x1b[200~a");
        other.paste(b"p", true).await;
        assert_eq!(h.next_input().await, b"\x1b[201~x");
        // Its end is dropped behind the start, which the pane leaves unread.
        other.paste(b"q", false).await;
        other.told_dropped("1 B").await;
        h.detach().await;
        let got = pane_input(&mut h).await;
        assert_eq!(
            String::from_utf8_lossy(&got),
            "\x1b[200~p",
            "detach ended another client's paste"
        );
        other.stop().await;
        h.stop().await;
    }

    /// A mouse report forwarded to the program is not pasted text: an open
    /// paste ends before it.
    #[tokio::test(start_paused = true)]
    async fn a_forwarded_click_ends_a_paste_left_open() {
        let mut h = InputHarness::new(1).await;
        paste_with_dropped_end(&mut h).await;
        assert_eq!(pane_input(&mut h).await, b"\x1b[200~a");
        set_modes(&h, b"\x1b[?1002h\x1b[?1006h").await;
        mouse(&mut h, MouseKind::Down, 3, 2).await;
        let got = pane_input(&mut h).await;
        assert_eq!(
            String::from_utf8_lossy(&got),
            "\x1b[201~\x1b[<0;3;2M",
            "the click went into the paste"
        );
        h.stop().await;
    }

    /// A held button released on detach is a mouse report, not pasted text:
    /// the open paste ends before it, or the program reads it as text and
    /// keeps the button down.
    #[tokio::test(start_paused = true)]
    async fn detaching_ends_an_open_paste_before_releasing_a_held_button() {
        let mut h = InputHarness::new(2).await;
        set_modes(&h, b"\x1b[?2004h\x1b[?1002h\x1b[?1006h").await;
        mouse(&mut h, MouseKind::Down, 3, 2).await;
        let press = pane_input(&mut h).await;
        assert!(
            press.ends_with(b"M"),
            "the press never reached the app: {press:?}"
        );
        h.paste(b"a", true).await;
        h.paste(b"b", true).await;
        h.paste(b"c", false).await;
        // The end waits out INPUT_STALL behind two pieces the pane leaves
        // unread, and is dropped.
        h.told_dropped("1 B").await;
        assert_eq!(pane_input(&mut h).await, b"\x1b[200~ab");

        h.detach().await;
        let got = pane_input(&mut h).await;
        assert!(
            got.starts_with(PASTE_END) && got.ends_with(b"m"),
            "the release went into the paste: {:?}",
            String::from_utf8_lossy(&got)
        );
        h.stop().await;
    }

    /// A paste whose end was dropped leaves the pane's program in paste mode.
    /// Detaching must close it: no later key from this client will.
    #[tokio::test(start_paused = true)]
    async fn detaching_closes_a_paste_whose_end_was_dropped() {
        let mut h = InputHarness::new(1).await;
        set_modes(&h, b"\x1b[?2004h").await;
        h.paste(b"a", true).await;
        h.paste(b"b", false).await;
        h.detach().await;
        // The program reads again only long after the client has gone.
        idle_for(INPUT_STALL * 3).await;
        let got = pane_input(&mut h).await;
        assert_eq!(
            String::from_utf8_lossy(&got),
            "\x1b[200~a\x1b[201~",
            "the pane was left inside a paste"
        );
        h.stop().await;
    }

    /// Drives a press, a long drag and a release through `channel` into a
    /// pane whose app has stopped reading, and returns the mouse reports the
    /// app would read. With `full`, every budget is spent first: the main one
    /// before the press, the control lane after it.
    async fn drag_over_a_stalled_app(
        (writer_tx, mut wr): (PaneWriter, crate::pane_input::PaneInputRx),
        full: bool,
    ) -> Vec<String> {
        let mut h = AppMouseHarness::new().await;
        let mut size = 1 << 20;
        while full && size > 0 {
            if writer_tx.try_send(vec![b'x'; size]).is_err() {
                size /= 2;
            }
        }
        let mut vt = shux_vt::VirtualTerminal::new(28, 49);
        vt.process(b"\x1b[?1002h\x1b[?1006h");
        {
            let mut state = h.fixture.io_state.lock().await;
            state
                .writers
                .insert(h.fixture.first_pane, writer_tx.clone());
            state.vts.insert(h.fixture.first_pane, vt);
        }
        h.send(MouseKind::Down, ProtoMouseButton::Left, 10, 5).await;
        while full && let Ok(permit) = writer_tx.try_reserve_in(1, Lane::Control) {
            permit.send(b"x".to_vec());
        }
        for i in 0..3000u16 {
            h.send(MouseKind::Drag, ProtoMouseButton::Left, 10 + i % 30, 5)
                .await;
        }
        h.send(MouseKind::Up, ProtoMouseButton::Left, 12, 5).await;
        let mut got = Vec::new();
        while let Ok(bytes) = wr.try_recv() {
            if !bytes.starts_with(b"x") {
                got.push(String::from_utf8_lossy(&bytes).into_owned());
            }
        }
        h.fixture.stop();
        got
    }

    /// A drag held over an app that has stopped reading must not use up the
    /// room its release needs, whether the channel's slots or its budgets run
    /// out first: an app given the press and the drags but not the release
    /// sits button-held once it reads again.
    #[tokio::test]
    async fn a_long_drag_over_an_app_that_stopped_reading_still_delivers_its_release() {
        let spare_slots = || crate::pane_input::channel_with(8192, crate::pane_input::BUDGET);
        for (case, channel, full) in [
            ("slots to spare", spare_slots(), false),
            ("budgets spent", spare_slots(), true),
            ("production slots", crate::pane_input::channel(), false),
        ] {
            let got = drag_over_a_stalled_app(channel, full).await;
            assert_eq!(
                got.first().map(String::as_str),
                Some("\x1b[<0;10;5M"),
                "{case}: the press was dropped"
            );
            assert_eq!(
                got.last().map(String::as_str),
                Some("\x1b[<0;12;5m"),
                "{case}: the app got the press and {} reports but no release",
                got.len().saturating_sub(1)
            );
        }
    }

    /// A client that has ended is unlisted and reported gone at once, even
    /// while its last frames are still stuck on a socket nobody reads.
    #[tokio::test(start_paused = true)]
    async fn a_detached_client_is_unlisted_before_its_socket_drains() {
        use tokio::io::AsyncReadExt;

        let fixture = attach_fixture().await;
        let _io = seed_io_for_pane(&fixture.io_state, fixture.first_pane).await;
        let mut vt = shux_vt::VirtualTerminal::new(58, 198);
        for _ in 0..50 {
            vt.process(&[b'x'; 180]);
            vt.process(b"\r\n");
        }
        fixture
            .io_state
            .lock()
            .await
            .vts
            .insert(fixture.first_pane, vt);
        let bus = shux_core::bus::EventBus::new();
        let mut events = bus.subscribe_filtered(vec!["client.disconnected".into()]);
        fixture.io_state.lock().await.event_bus = Some(bus);
        let (server_stream, client_stream) = tokio::io::duplex(1024);
        let server = tokio::spawn(handle_attach_connection(
            server_stream,
            fixture.graph.clone(),
            fixture.io_state.clone(),
            test_config(),
            SegmentCache::new(),
            crate::session_meta::SessionMetaCache::new(),
            crate::onboarding::OnboardingHandle::from_state_for_test(
                crate::onboarding::OnboardingState {
                    prefix_discovered: true,
                    welcome_toast_seen: true,
                },
            ),
            std::time::Instant::now(),
            fixture.cancel.child_token(),
        ));
        let mut framed = Framed::new(client_stream, create_codec());
        let hello = AttachHello {
            protocol: ATTACH_PROTOCOL_VERSION,
            session_name: Some(fixture.attached.name.clone()),
            cols: 200,
            rows: 60,
            client_version: "detach-then-stop".to_string(),
            graphics: false,
            plain: false,
        };
        framed
            .send(Bytes::from(serde_json::to_vec(&hello).expect("hello json")))
            .await
            .expect("send hello");
        shux_testkit::Deadline::start()
            .run(framed.next())
            .await
            .expect("ready timeout")
            .expect("ready frame")
            .expect("ready bytes");
        shux_testkit::Deadline::start()
            .run(framed.next())
            .await
            .expect("prelude timeout")
            .expect("prelude frame")
            .expect("prelude bytes");
        let mut parts = framed.into_parts();
        let buffered = std::mem::take(&mut parts.read_buf);
        let buffered_len = buffered.len();
        let mut read = std::io::Cursor::new(buffered).chain(&mut parts.io);
        let render_len = shux_testkit::Deadline::start()
            .run(read.read_u32())
            .await
            .expect("render header timeout")
            .expect("render header") as usize;
        assert!(
            render_len > buffered_len + 1024,
            "render could fit in the socket"
        );
        let mut framed = Framed::from_parts(parts);
        // The render has started and cannot finish until the peer reads again.
        framed
            .send(Bytes::from(
                serde_json::to_vec(&AttachClientFrame::Detach).expect("detach json"),
            ))
            .await
            .expect("send detach");

        let gone = shux_testkit::Deadline::start().run(events.recv()).await;
        assert!(gone.is_some(), "no client.disconnected");
        assert!(
            !server.is_finished(),
            "the client was reported gone only once its socket gave up"
        );
        assert_eq!(
            fixture
                .io_state
                .lock()
                .await
                .clients
                .count_for(fixture.session_id),
            0,
            "a detached client is still listed"
        );
        drop(framed);
        shux_testkit::Deadline::start()
            .run(server)
            .await
            .expect("server stayed open")
            .expect("server task")
            .expect("attach connection");
        fixture.stop();
    }

    /// A removed client gets at most the frame already on the wire, then the
    /// notice: the screens queued behind it are dropped, not delivered.
    #[tokio::test(start_paused = true)]
    async fn a_removed_client_is_not_sent_its_backlog() {
        let fixture = attach_fixture().await;
        let _io = seed_io_for_pane(&fixture.io_state, fixture.first_pane).await;
        let (server_stream, client_stream) = tokio::io::duplex(1024);
        let server = tokio::spawn(handle_attach_connection(
            server_stream,
            fixture.graph.clone(),
            fixture.io_state.clone(),
            test_config(),
            SegmentCache::new(),
            crate::session_meta::SessionMetaCache::new(),
            crate::onboarding::OnboardingHandle::from_state_for_test(
                crate::onboarding::OnboardingState {
                    prefix_discovered: true,
                    welcome_toast_seen: true,
                },
            ),
            std::time::Instant::now(),
            fixture.cancel.child_token(),
        ));
        let mut framed = Framed::new(client_stream, create_codec());
        let hello = AttachHello {
            protocol: ATTACH_PROTOCOL_VERSION,
            session_name: Some(fixture.attached.name.clone()),
            cols: 200,
            rows: 60,
            client_version: "slow-then-removed".to_string(),
            graphics: false,
            plain: false,
        };
        framed
            .send(Bytes::from(serde_json::to_vec(&hello).expect("hello json")))
            .await
            .expect("send hello");
        shux_testkit::Deadline::start()
            .run(framed.next())
            .await
            .expect("ready timeout")
            .expect("ready frame")
            .expect("ready bytes");

        // Screens pile up while the client reads nothing.
        for i in 0..6 {
            let pulse = {
                let mut io = fixture.io_state.lock().await;
                io.vts
                    .get_mut(&fixture.first_pane)
                    .expect("seeded vt")
                    .process(format!("queued-{i}\r\n").as_bytes());
                io.render_pulse.clone()
            };
            pulse.bump();
            idle_for(Duration::from_millis(100)).await;
        }
        let clients = fixture.io_state.lock().await.clients.clone();
        let id = clients.list()[0].id;
        assert!(clients.remove(id));

        let mut renders_after = 0;
        let mut told = None;
        let deadline = shux_testkit::Deadline::start();
        while let Some(Some(Ok(buf))) = deadline.run(framed.next()).await {
            match serde_json::from_slice::<AttachServerFrame>(&buf).expect("server frame") {
                AttachServerFrame::Render { .. } => renders_after += 1,
                AttachServerFrame::SessionEnded { reason } => told = Some(reason),
                _ => {}
            }
        }
        assert_eq!(
            told.as_deref(),
            Some("removed"),
            "never told it was removed"
        );
        assert!(
            renders_after <= 2,
            "a removed client was sent {renders_after} queued screens"
        );
        let _ = shux_testkit::Deadline::start().run(server).await;
        fixture.stop();
    }

    #[tokio::test(start_paused = true)]
    async fn an_idle_client_is_released_when_it_stops_answering_pings() {
        let fixture = attach_fixture().await;
        let _io = seed_io_for_pane(&fixture.io_state, fixture.first_pane).await;
        let mut client = Client::attach(&fixture, 90).await;
        let is_ping = |frame: &AttachServerFrame, _: &str| matches!(frame, AttachServerFrame::Ping);
        assert!(client.frames_until(is_ping).await, "no idle ping");
        client.send(&AttachClientFrame::Pong).await;
        assert!(
            client.frames_until(is_ping).await,
            "pong did not keep the client attached"
        );
        shux_testkit::Deadline::start()
            .run(&mut client.server)
            .await
            .expect("an unresponsive idle client was still held")
            .expect("server task")
            .expect("attach connection");
        fixture.stop();
    }

    /// Screens that keep changing do not stand in for a reply: a client that
    /// reads but never answers a ping is let go, however busy the pane.
    #[tokio::test(start_paused = true)]
    async fn a_client_that_never_answers_is_dropped_while_screens_change() {
        let fixture = attach_fixture().await;
        let _io = seed_io_for_pane(&fixture.io_state, fixture.first_pane).await;
        let mut client = Client::attach(&fixture, 90).await;
        let mut ended = false;
        for i in 0..60 {
            let pulse = {
                let mut io = fixture.io_state.lock().await;
                io.vts
                    .get_mut(&fixture.first_pane)
                    .expect("seeded vt")
                    .process(format!("tick-{i}\r\n").as_bytes());
                io.render_pulse.clone()
            };
            pulse.bump();
            if !client
                .frames_until(|f, _| matches!(f, AttachServerFrame::Render { .. }))
                .await
            {
                ended = true;
                break;
            }
            idle_for(Duration::from_secs(1)).await;
        }
        assert!(
            ended && client.server.is_finished(),
            "a client that never answered a ping was held for a minute of redraws"
        );
        fixture.stop();
    }

    /// Removal reaches the writer even while the read loop is stuck queueing
    /// a copy behind a full backlog: the client gets the notice, not the queue.
    #[tokio::test(start_paused = true)]
    async fn a_client_removed_while_its_copy_waits_gets_no_backlog() {
        let fixture = attach_fixture().await;
        let _io = seed_io_for_pane(&fixture.io_state, fixture.first_pane).await;
        let bus = shux_core::bus::EventBus::new();
        let mut events = bus.subscribe_filtered(vec!["client.disconnected".into()]);
        fixture.io_state.lock().await.event_bus = Some(bus);
        let (server_stream, client_stream) = tokio::io::duplex(1024);
        let server = tokio::spawn(handle_attach_connection(
            server_stream,
            fixture.graph.clone(),
            fixture.io_state.clone(),
            test_config(),
            SegmentCache::new(),
            crate::session_meta::SessionMetaCache::new(),
            crate::onboarding::OnboardingHandle::from_state_for_test(
                crate::onboarding::OnboardingState {
                    prefix_discovered: true,
                    welcome_toast_seen: true,
                },
            ),
            std::time::Instant::now(),
            fixture.cancel.child_token(),
        ));
        let mut framed = Framed::new(client_stream, create_codec());
        let hello = AttachHello {
            protocol: ATTACH_PROTOCOL_VERSION,
            session_name: Some(fixture.attached.name.clone()),
            cols: 200,
            rows: 60,
            client_version: "copy-then-removed".to_string(),
            graphics: false,
            plain: false,
        };
        let send = |f: &AttachClientFrame| Bytes::from(serde_json::to_vec(f).expect("frame json"));
        framed
            .send(Bytes::from(serde_json::to_vec(&hello).expect("hello json")))
            .await
            .expect("send hello");
        shux_testkit::Deadline::start()
            .run(framed.next())
            .await
            .expect("ready timeout")
            .expect("ready frame")
            .expect("ready bytes");
        // More screens than the outbound queue holds, none of them read.
        for i in 0..(OUTBOUND_FRAMES + 8) {
            let pulse = {
                let mut io = fixture.io_state.lock().await;
                io.vts
                    .get_mut(&fixture.first_pane)
                    .expect("seeded vt")
                    .process(format!("queued-{i}\r\n").as_bytes());
                io.render_pulse.clone()
            };
            pulse.bump();
            idle_for(Duration::from_millis(50)).await;
        }
        // A yank now waits on the full queue.
        framed
            .send(send(&AttachClientFrame::Action {
                kind: ActionKind::EnterCopyMode,
                args: Default::default(),
            }))
            .await
            .expect("enter copy mode");
        for key in [&b"k"[..], b"v", b"lllll", b"y"] {
            framed
                .send(send(&AttachClientFrame::Input {
                    data: BASE64.encode(key),
                }))
                .await
                .expect("copy key");
        }
        idle_for(Duration::from_millis(200)).await;
        let clients = fixture.io_state.lock().await.clients.clone();
        assert!(clients.remove(clients.list()[0].id));

        let (mut screens, mut told) = (0, None);
        let deadline = shux_testkit::Deadline::start();
        while let Some(Some(Ok(buf))) = deadline.run(framed.next()).await {
            match serde_json::from_slice::<AttachServerFrame>(&buf).expect("server frame") {
                AttachServerFrame::Render { frame: Some(_), .. } => screens += 1,
                AttachServerFrame::SessionEnded { reason } => told = Some(reason),
                _ => {}
            }
        }
        assert_eq!(
            told.as_deref(),
            Some("removed"),
            "never told it was removed"
        );
        assert!(
            screens <= 2,
            "a removed client was sent {screens} queued screens"
        );
        let _ = shux_testkit::Deadline::start().run(server).await;
        let reason = match events.try_recv() {
            Some(shux_core::bus::SubscriptionEvent::Event(e)) => match e.data {
                shux_core::event::EventData::ClientDisconnected { reason, .. } => reason,
                other => panic!("unexpected event {other:?}"),
            },
            other => panic!("no client.disconnected: {other:?}"),
        };
        assert_eq!(reason, "removed", "a removal was published as {reason:?}");
        fixture.stop();
    }

    /// Issue #287: a client that stops reading is let go, instead of its
    /// writer waiting on the socket for as long as the daemon runs.
    #[tokio::test(start_paused = true)]
    async fn a_client_that_stops_reading_is_released() {
        let fixture = attach_fixture().await;
        let _io = seed_io_for_pane(&fixture.io_state, fixture.first_pane).await;
        // Far less room than one full-screen frame.
        let (server_stream, client_stream) = tokio::io::duplex(1024);
        let server = tokio::spawn(handle_attach_connection(
            server_stream,
            fixture.graph.clone(),
            fixture.io_state.clone(),
            test_config(),
            SegmentCache::new(),
            crate::session_meta::SessionMetaCache::new(),
            crate::onboarding::OnboardingHandle::from_state_for_test(
                crate::onboarding::OnboardingState {
                    prefix_discovered: true,
                    welcome_toast_seen: true,
                },
            ),
            std::time::Instant::now(),
            fixture.cancel.child_token(),
        ));
        let mut framed = Framed::new(client_stream, create_codec());
        let hello = AttachHello {
            protocol: ATTACH_PROTOCOL_VERSION,
            session_name: Some(fixture.attached.name.clone()),
            cols: 200,
            rows: 60,
            client_version: "stopped-reader".to_string(),
            graphics: false,
            plain: false,
        };
        framed
            .send(Bytes::from(serde_json::to_vec(&hello).expect("hello json")))
            .await
            .expect("send hello");
        let ready = shux_testkit::Deadline::start()
            .run(framed.next())
            .await
            .expect("ready timeout")
            .expect("ready frame")
            .expect("ready bytes");
        assert!(matches!(
            serde_json::from_slice::<AttachReady>(&ready).expect("ready json"),
            AttachReady::Ok { .. }
        ));

        // From here on the client reads nothing.
        let ended = shux_testkit::Deadline::start().run(server).await;
        assert!(
            ended.is_some(),
            "a client that stopped reading was still held"
        );
        drop(framed);
        fixture.stop();
    }

    /// A session renamed while a client watches it is shown by its new name.
    #[tokio::test(start_paused = true)]
    async fn an_attached_client_follows_a_session_rename() {
        let mut h = InputHarness::new(8).await;
        assert!(
            h.frames_until(|_, text| text.contains("attach-test")).await,
            "the old name was never shown"
        );
        h.fixture
            .graph
            .rename_session(h.fixture.session_id, "renamed-live".to_string(), None)
            .await
            .expect("rename");
        assert!(
            h.frames_until(|_, text| text.contains("renamed-live"))
                .await,
            "the bar kept the old name"
        );
        h.stop().await;
    }

    /// A pane split from an attached client belongs to the session, not to
    /// that connection: it keeps running after the client detaches.
    #[tokio::test]
    async fn a_pane_split_from_a_client_outlives_the_client() {
        let fixture = attach_fixture().await;
        let panes = |f: &AttachFixture| {
            f.graph.snapshot().windows[&f.first_window]
                .layout
                .tree
                .pane_ids()
        };
        let before = panes(&fixture);
        let mut c = Client::attach(&fixture, 120).await;
        c.send(&AttachClientFrame::Action {
            kind: ActionKind::SplitSmart,
            args: Default::default(),
        })
        .await;
        assert!(
            c.frames_until(|_, _| panes(&fixture).len() > before.len())
                .await,
            "the split never happened"
        );
        let new_pane = *panes(&fixture)
            .iter()
            .find(|p| !before.contains(p))
            .expect("a new pane");

        c.detach().await;
        let Client { framed, server, .. } = c;
        drop(framed);
        assert!(
            shux_testkit::Deadline::start().run(server).await.is_some(),
            "the connection never ended"
        );
        let shutdown = fixture
            .io_state
            .lock()
            .await
            .shutdowns
            .get(&new_pane)
            .cloned();
        assert!(
            shutdown.is_some_and(|t| !t.is_cancelled()),
            "the split pane was torn down with the client that made it"
        );
        fixture.stop();
    }

    /// Issue #287: every client is listed; removing one ends exactly that
    /// one, tells it why, and publishes who came and went.
    #[tokio::test(start_paused = true)]
    async fn removing_a_client_ends_only_it_and_says_why() {
        let bus = shux_core::bus::EventBus::new();
        let mut events = bus.subscribe_filtered(vec![
            "client.".into(),
            "session.attached".into(),
            "session.detached".into(),
        ]);
        let mut h = InputHarness::new(8).await;
        assert!(
            h.frames_until(|f, _| matches!(f, AttachServerFrame::Render { .. }))
                .await
        );
        h.fixture.io_state.lock().await.event_bus = Some(bus);
        let clients = h.fixture.io_state.lock().await.clients.clone();
        let first = clients.list();
        let mut a = h.peer().await;
        let a_id = clients
            .list()
            .iter()
            .map(|c| c.id)
            .find(|id| !first.iter().any(|c| c.id == *id))
            .expect("the peer is listed");
        let mut b = h.peer().await;
        assert!(
            b.frames_until(|f, _| matches!(f, AttachServerFrame::Render { .. }))
                .await
        );
        assert_eq!(clients.count_for(h.fixture.session_id), 3);

        assert!(clients.remove(a_id));
        assert!(
            a.frames_until(|f, _| matches!(f, AttachServerFrame::SessionEnded { reason } if reason == "removed"))
                .await,
            "the removed client was not told why"
        );
        // The others are still served.
        b.send(&AttachClientFrame::Action {
            kind: ActionKind::Redraw,
            args: Default::default(),
        })
        .await;
        assert!(
            b.frames_until(|f, _| matches!(f, AttachServerFrame::Render { .. }))
                .await,
            "removing one client stopped another"
        );
        h.key(b"still-here").await;
        assert_eq!(h.next_input().await, b"still-here");

        // Detached is the last thing a connection publishes.
        let mut seen = Vec::new();
        let deadline = shux_testkit::Deadline::start();
        while let Some(Some(shux_core::bus::SubscriptionEvent::Event(e))) =
            deadline.run(events.recv()).await
        {
            let last = matches!(e.data, shux_core::event::EventData::SessionDetached { .. });
            seen.push(e.data);
            if last {
                break;
            }
        }
        let disconnected: Vec<_> = seen
            .iter()
            .filter_map(|d| match d {
                shux_core::event::EventData::ClientDisconnected { client_id, reason } => {
                    Some((*client_id, reason.as_str()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(disconnected, vec![(a_id, "removed")], "events: {seen:?}");
        assert!(seen.iter().any(|d| matches!(
            d,
            shux_core::event::EventData::SessionDetached { client_id, session_id }
                if *client_id == a_id && *session_id == h.fixture.session_id
        )));
        assert_eq!(
            seen.iter()
                .filter(|d| matches!(d, shux_core::event::EventData::SessionAttached { .. }))
                .count(),
            2,
            "the bus was set after the first client attached: {seen:?}"
        );
        assert_eq!(clients.count_for(h.fixture.session_id), 2);
        assert!(
            !clients.list().iter().any(|c| c.id == a_id),
            "the removed client is still listed"
        );
        b.stop().await;
        a.stop().await;
        h.stop().await;
    }

    /// Issue #288: a plain viewer gets the pane's text and colour, and none
    /// of the effects a pane can set in a terminal; a normal viewer of the
    /// same pane still gets them.
    #[tokio::test]
    async fn a_plain_viewer_gets_text_and_colour_but_no_terminal_effects() {
        let fixture = attach_fixture().await;
        let _io = seed_io_for_pane(&fixture.io_state, fixture.first_pane).await;
        fixture
            .io_state
            .lock()
            .await
            .vts
            .get_mut(&fixture.first_pane)
            .expect("seeded vt")
            .process(
                b"\x1b[31m\x1b]8;;https://example.invalid/p\x1b\\LINKED\x1b]8;;\x1b\\\x1b[0m\x1b]12;#ff0000\x1b\\\x1b[6 q",
            );
        let onboarded = || {
            crate::onboarding::OnboardingHandle::from_state_for_test(
                crate::onboarding::OnboardingState {
                    prefix_discovered: true,
                    welcome_toast_seen: true,
                },
            )
        };
        let mut seen = Vec::new();
        for plain in [true, false] {
            let mut c = Client::attach_onboarded(&fixture, 90, onboarded(), plain).await;
            let mut raw = Vec::new();
            assert!(
                c.frames_until(|f, text| {
                    if let AttachServerFrame::Render { data, .. } = f {
                        raw.extend(BASE64.decode(data.as_bytes()).expect("render base64"));
                    }
                    text.contains("LINKED")
                })
                .await,
                "plain={plain}: the pane's text never arrived"
            );
            seen.push(String::from_utf8_lossy(&raw).into_owned());
            c.stop().await;
        }
        let (plain, normal) = (&seen[0], &seen[1]);
        assert!(plain.contains("\x1b[38;"), "plain lost colour: {plain:?}");
        for effect in ["\x1b]8;;https", "\x1b]12;", "\x1b[6 q"] {
            assert!(!plain.contains(effect), "plain got {effect:?}");
            assert!(
                normal.contains(effect),
                "normal lacks {effect:?}: {normal:?}"
            );
        }
        fixture.stop();
    }

    /// Every path that draws for a plain viewer, selection highlighting
    /// included, leaves out invisible direction controls the pane printed.
    #[tokio::test]
    async fn a_plain_viewer_selecting_text_gets_no_direction_controls() {
        let fixture = attach_fixture().await;
        let _io = seed_io_for_pane(&fixture.io_state, fixture.first_pane).await;
        fixture
            .io_state
            .lock()
            .await
            .vts
            .get_mut(&fixture.first_pane)
            .expect("seeded vt")
            .process("\x1b[2J\x1b[HSPOOF-A\u{202E}B\u{2066}C".as_bytes());
        let onboarded = crate::onboarding::OnboardingHandle::from_state_for_test(
            crate::onboarding::OnboardingState {
                prefix_discovered: true,
                welcome_toast_seen: true,
            },
        );
        let mut c = Client::attach_onboarded(&fixture, 90, onboarded, true).await;
        let mut raw = Vec::new();
        let mut at = None;
        assert!(
            c.frames_until(|f, text| {
                if let AttachServerFrame::Render { data, .. } = f {
                    raw.extend(BASE64.decode(data.as_bytes()).expect("render base64"));
                }
                at = text.lines().enumerate().find_map(|(y, l)| {
                    l.split_once("SPOOF")
                        .map(|(before, _)| (before.chars().count() as u16, y as u16))
                });
                at.is_some()
            })
            .await
        );
        let (x, y) = at.expect("SPOOF on screen");
        for (kind, col) in [(MouseKind::Down, x), (MouseKind::Drag, x + 9)] {
            let frame = c.written_frame;
            c.send(&AttachClientFrame::Mouse {
                kind,
                button: ProtoMouseButton::Left,
                col,
                row: y,
                shift: false,
                alt: false,
                ctrl: false,
                frame,
            })
            .await;
        }
        let _ = c
            .frames_until(|f, _| {
                if let AttachServerFrame::Render { data, .. } = f {
                    raw.extend(BASE64.decode(data.as_bytes()).expect("render base64"));
                }
                matches!(f, AttachServerFrame::Render { frame: Some(_), .. })
            })
            .await;
        let text = String::from_utf8_lossy(&raw);
        for hidden in ['\u{202E}', '\u{2066}'] {
            assert!(!text.contains(hidden), "a plain viewer was sent {hidden:?}");
        }
        c.stop().await;
        fixture.stop();
    }

    /// A pane task stand-in: applies each resize to the pane's VT, as the PTY
    /// task does, and logs the sizes it was handed.
    fn fake_pty(
        io_state: &Arc<Mutex<PaneIoState>>,
        pane: PaneId,
        mut rx: mpsc::Receiver<crate::pane_io::ResizeRequest>,
    ) -> Arc<std::sync::Mutex<Vec<(u16, u16)>>> {
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (io, seen) = (io_state.clone(), log.clone());
        tokio::spawn(async move {
            while let Some(req) = rx.recv().await {
                let (cols, rows) = (req.size.cols, req.size.rows);
                if let Some(vt) = io.lock().await.vts.get_mut(&pane) {
                    vt.resize(usize::from(rows), usize::from(cols));
                }
                seen.lock().unwrap().push((cols, rows));
                if let Some(ack) = req.ack {
                    let _ = ack.send(());
                }
            }
        });
        log
    }

    /// Issue #190: the panes take the size of the client with the most recent
    /// input, not of whichever client attached or resized last.
    #[tokio::test(start_paused = true)]
    async fn panes_follow_the_client_that_types_not_the_one_that_resizes() {
        let fixture = attach_fixture().await;
        let (_w, resize_rx, _s) = seed_io_for_pane(&fixture.io_state, fixture.first_pane).await;
        let log = fake_pty(&fixture.io_state, fixture.first_pane, resize_rx);
        let last = || {
            *log.lock()
                .unwrap()
                .last()
                .expect("the pane was never sized")
        };
        let settled = || idle_for(QUIET);

        let mut a = Client::attach(&fixture, 200).await;
        settled().await;
        let by_a = last();
        let mut b = Client::attach(&fixture, 80).await;
        settled().await;
        assert_eq!(last(), by_a, "a client that only attached took the panes");
        b.send(&AttachClientFrame::Resize { cols: 90, rows: 30 })
            .await;
        settled().await;
        assert_eq!(last(), by_a, "a client that only resized took the panes");

        b.key(b"x").await;
        settled().await;
        let by_b = last();
        assert_ne!(by_b, by_a, "the typing client never took the panes");

        // A's key inside the settle window does not take them back.
        a.key(b"y").await;
        settled().await;
        assert_eq!(last(), by_b, "alternating keys flipped the size");
        idle_for(crate::clients::SETTLE).await;
        a.key(b"z").await;
        settled().await;
        assert_eq!(
            last(),
            by_a,
            "a quiet driver kept the panes from a typing client"
        );

        // B leaves: nothing moves while A still drives. A leaves too: sizes stay.
        b.stop().await;
        settled().await;
        assert_eq!(last(), by_a);
        let sized = log.lock().unwrap().len();
        a.stop().await;
        settled().await;
        assert_eq!(
            log.lock().unwrap().len(),
            sized,
            "the last client leaving resized the panes"
        );
        fixture.stop();
    }

    /// A paste from a client that has sent nothing since another window was
    /// focused over RPC goes to the window the session now shows.
    #[tokio::test(start_paused = true)]
    async fn a_paste_after_an_rpc_window_switch_goes_to_the_shown_window() {
        let fixture = attach_fixture().await;
        let (mut first, _r1, _s1) = seed_io_for_pane(&fixture.io_state, fixture.first_pane).await;
        let (mut shown, _r2, _s2) =
            seed_io_for_pane(&fixture.io_state, fixture.second_window_pane).await;
        let mut client = Client::attach(&fixture, 100).await;
        idle_for(QUIET).await;
        let window = fixture.graph.snapshot().panes[&fixture.second_window_pane].window_id;
        fixture
            .graph
            .focus_window(window, None)
            .await
            .expect("focus the other window");
        client
            .send(&AttachClientFrame::Paste {
                data: BASE64.encode(b"pasted"),
                more: false,
            })
            .await;
        idle_for(QUIET).await;
        assert!(
            first.try_recv().is_err(),
            "the paste went to the window no longer shown"
        );
        assert_eq!(shown.try_recv().ok(), Some(b"pasted".to_vec()));
        fixture.stop();
    }

    /// A click that moves focus to another pane hands that client the panes,
    /// as a key would; a client that only attached does not.
    #[tokio::test(start_paused = true)]
    async fn a_click_that_focuses_another_pane_takes_the_panes() {
        let fixture = attach_fixture().await;
        let (_w1, rx1, _s1) = seed_io_for_pane(&fixture.io_state, fixture.first_pane).await;
        let (_w2, rx2, _s2) = seed_io_for_pane(&fixture.io_state, fixture.second_pane).await;
        let log = fake_pty(&fixture.io_state, fixture.first_pane, rx1);
        let _log2 = fake_pty(&fixture.io_state, fixture.second_pane, rx2);
        let last = || {
            *log.lock()
                .unwrap()
                .last()
                .expect("the pane was never sized")
        };
        let focused = || {
            let snap = fixture.graph.snapshot();
            snap.windows[&fixture.first_window].active_pane
        };

        let _a = Client::attach(&fixture, 200).await;
        idle_for(QUIET).await;
        let by_a = last();
        let mut b = Client::attach(&fixture, 80).await;
        idle_for(QUIET).await;
        assert_eq!(last(), by_a);

        let viewport = current_viewport(&Arc::new(Mutex::new((80, 24))), &test_config()).await;
        let (first_point, second_point, _) =
            find_pane_and_border_points(&fixture.graph, fixture.first_window, viewport);
        let before = focused();
        let target = if before == fixture.first_pane {
            second_point
        } else {
            first_point
        };
        for kind in [MouseKind::Down, MouseKind::Up] {
            b.send(&AttachClientFrame::Mouse {
                kind,
                button: ProtoMouseButton::Left,
                col: target.0,
                row: target.1,
                shift: false,
                alt: false,
                ctrl: false,
                frame: b.written_frame,
            })
            .await;
        }
        idle_for(QUIET).await;
        assert_ne!(focused(), before, "the click did not move focus");
        assert_ne!(
            last(),
            by_a,
            "a click that moved focus did not take the panes"
        );
        fixture.stop();
    }

    /// The cell under `client`'s corner of `pane`, as an 80-column client lays
    /// the fixture's window out: its last visible column and row.
    async fn corner_of(fixture: &AttachFixture, pane: PaneId) -> (u16, u16) {
        let viewport = current_viewport(&Arc::new(Mutex::new((80, 24))), &test_config()).await;
        let rect = fixture.graph.snapshot().windows[&fixture.first_window]
            .layout
            .compute_rects(viewport)
            .into_iter()
            .find_map(|(p, r)| (p == pane).then_some(r))
            .expect("the pane's rect");
        (rect.x + rect.width - 1, rect.y + rect.height - 1)
    }

    fn left_button(
        kind: MouseKind,
        (col, row): (u16, u16),
        frame: Option<u64>,
    ) -> AttachClientFrame {
        AttachClientFrame::Mouse {
            kind,
            button: ProtoMouseButton::Left,
            col,
            row,
            shift: false,
            alt: false,
            ctrl: false,
            frame,
        }
    }

    /// Review of #190: a click that makes its client drive still presses and
    /// releases one cell. The panes change hands when the gesture ends; a
    /// resize between its press and release moved the release to another cell.
    #[tokio::test(start_paused = true)]
    async fn a_takeover_click_presses_and_releases_the_same_cell() {
        let fixture = attach_fixture().await;
        let pane = fixture.first_pane;
        let (mut writes, rx, _s) = seed_io_for_pane(&fixture.io_state, pane).await;
        let (_w2, rx2, _s2) = seed_io_for_pane(&fixture.io_state, fixture.second_pane).await;
        let log = fake_pty(&fixture.io_state, pane, rx);
        let _log2 = fake_pty(&fixture.io_state, fixture.second_pane, rx2);
        let last = || {
            *log.lock()
                .unwrap()
                .last()
                .expect("the pane was never sized")
        };
        let _a = Client::attach(&fixture, 200).await;
        idle_for(QUIET).await;
        let by_a = last();
        // An SGR mouse app with its cursor on the grid's last cell: a smaller
        // client's view follows it to the bottom-right.
        fixture
            .io_state
            .lock()
            .await
            .vts
            .get_mut(&pane)
            .expect("pane vt")
            .process(format!("\x1b[?1002h\x1b[?1006h\x1b[{};{}H", by_a.1, by_a.0).as_bytes());
        let mut b = Client::attach(&fixture, 80).await;
        idle_for(QUIET).await;
        b.catch_up().await;

        let corner = corner_of(&fixture, pane).await;
        b.send(&left_button(MouseKind::Down, corner, b.written_frame))
            .await;
        idle_for(QUIET).await;
        b.catch_up().await;
        b.send(&left_button(MouseKind::Up, corner, b.written_frame))
            .await;
        // The program reads its input: the release is written, within the
        // bound the takeover waits for it.
        idle_for(Duration::from_millis(10)).await;
        let sent = drain(&mut writes);
        idle_for(QUIET).await;
        let reports: Vec<&str> = sent.split("\x1b[<0;").skip(1).collect();
        let at = |r: &str| r[..r.len() - 1].to_string();
        assert_eq!(reports.len(), 2, "{sent:?}");
        assert_eq!(
            at(reports[0]),
            format!("{};{}", by_a.0, by_a.1),
            "the press names the cell shown there"
        );
        assert_eq!(
            at(reports[1]),
            at(reports[0]),
            "the release moved: {sent:?}"
        );
        assert_ne!(last(), by_a, "the click did not hand the panes over");
        fixture.stop();
    }

    /// Review of #190: a press on another pane's surplus reaches no app; it
    /// is no input, so it does not resize everyone's panes to the clicker,
    /// whether that pane's app tracks the mouse or not.
    #[tokio::test(start_paused = true)]
    async fn a_press_on_another_panes_surplus_does_not_take_the_panes() {
        let fixture = attach_fixture().await;
        let active = fixture.graph.snapshot().windows[&fixture.first_window].active_pane;
        let other = if active == fixture.first_pane {
            fixture.second_pane
        } else {
            fixture.first_pane
        };
        let (_wa, rxa, _sa) = seed_io_for_pane(&fixture.io_state, active).await;
        let (mut writes, rxo, _so) = seed_io_for_pane(&fixture.io_state, other).await;
        let log = fake_pty(&fixture.io_state, active, rxa);
        let _log_other = fake_pty(&fixture.io_state, other, rxo);
        let last = || {
            *log.lock()
                .unwrap()
                .last()
                .expect("the pane was never sized")
        };
        let _a = Client::attach(&fixture, 200).await;
        idle_for(QUIET).await;
        let by_a = last();
        {
            let mut io = fixture.io_state.lock().await;
            io.clients.pin(other);
            let vt = io.vts.get_mut(&other).expect("pane vt");
            vt.resize(4, 12);
            vt.process(b"\x1b[?1002h\x1b[?1006h");
        }
        let mut b = Client::attach(&fixture, 80).await;
        idle_for(QUIET).await;
        b.catch_up().await;
        let viewport = current_viewport(&Arc::new(Mutex::new((80, 24))), &test_config()).await;
        let rect = fixture.graph.snapshot().windows[&fixture.first_window]
            .layout
            .compute_rects(viewport)
            .into_iter()
            .find_map(|(p, r)| (p == other).then_some(r))
            .expect("the pane's rect");

        for (tracking, point) in [
            (true, (rect.x + 13, rect.y + 1)),
            (false, (rect.x + 1, rect.y + 6)),
        ] {
            if !tracking {
                fixture
                    .io_state
                    .lock()
                    .await
                    .vts
                    .get_mut(&other)
                    .expect("pane vt")
                    .process(b"\x1b[?1002l\x1b[?1006l");
                let _ = fixture.graph.focus_pane(active).await;
                idle_for(QUIET).await;
                b.catch_up().await;
            }
            b.send(&left_button(MouseKind::Down, point, b.written_frame))
                .await;
            b.send(&left_button(MouseKind::Up, point, b.written_frame))
                .await;
            idle_for(QUIET).await;
            assert_eq!(
                drain(&mut writes),
                "",
                "tracking {tracking}: the app got a report"
            );
            assert_eq!(
                last(),
                by_a,
                "tracking {tracking}: the surplus press took the panes"
            );
        }
        fixture.stop();
    }

    /// Two clients over the fixture's window: A (200 columns) drives, B (80)
    /// views, and the first pane runs an SGR mouse app whose cursor sits on
    /// its grid's last cell. Returns the app's input, the pane's size log, the
    /// clients and the size A gave the pane.
    async fn driver_and_viewer_over_an_app(
        fixture: &AttachFixture,
    ) -> (
        crate::pane_input::PaneInputRx,
        Arc<std::sync::Mutex<Vec<(u16, u16)>>>,
        Client,
        Client,
        (u16, u16),
    ) {
        let pane = fixture.first_pane;
        let (writes, rx, _s) = seed_io_for_pane(&fixture.io_state, pane).await;
        let (_w2, rx2, _s2) = seed_io_for_pane(&fixture.io_state, fixture.second_pane).await;
        let log = fake_pty(&fixture.io_state, pane, rx);
        let _log2 = fake_pty(&fixture.io_state, fixture.second_pane, rx2);
        let _ = fixture.graph.focus_pane(pane).await;
        let mut a = Client::attach(fixture, 200).await;
        idle_for(QUIET).await;
        a.catch_up().await;
        let by_a = *log.lock().unwrap().last().expect("A sized the pane");
        fixture
            .io_state
            .lock()
            .await
            .vts
            .get_mut(&pane)
            .expect("pane vt")
            .process(format!("\x1b[?1002h\x1b[?1006h\x1b[{};{}H", by_a.1, by_a.0).as_bytes());
        let mut b = Client::attach(fixture, 80).await;
        idle_for(QUIET).await;
        a.catch_up().await;
        b.catch_up().await;
        (writes, log, a, b, by_a)
    }

    /// The cell at `(dx, dy)` inside `pane` as a client `cols` wide lays it out.
    async fn point_in(
        fixture: &AttachFixture,
        pane: PaneId,
        cols: u16,
        (dx, dy): (u16, u16),
    ) -> (u16, u16) {
        let viewport = current_viewport(&Arc::new(Mutex::new((cols, 24))), &test_config()).await;
        let rect = fixture.graph.snapshot().windows[&fixture.first_window]
            .layout
            .compute_rects(viewport)
            .into_iter()
            .find_map(|(p, r)| (p == pane).then_some(r))
            .expect("the pane's rect");
        (rect.x + dx, rect.y + dy)
    }

    /// Review of #190: a viewer's takeover waits for its release to be
    /// written. The PTY applies a resize ahead of queued input, so a takeover
    /// sent at once could reach the app before the button came up.
    #[tokio::test(start_paused = true)]
    async fn a_takeover_waits_for_its_release_to_be_written() {
        let fixture = attach_fixture().await;
        let (mut writes, log, _a, mut b, by_a) = driver_and_viewer_over_an_app(&fixture).await;
        let last = || *log.lock().unwrap().last().expect("sized");
        let at = point_in(&fixture, fixture.first_pane, 80, (2, 2)).await;
        b.send(&left_button(MouseKind::Down, at, b.written_frame))
            .await;
        idle_for(QUIET).await;
        let press = writes.try_recv().expect("the press reached the app");
        assert!(press.ends_with(b"M"), "{press:?}");
        b.send(&left_button(MouseKind::Up, at, b.written_frame))
            .await;
        idle_for(Duration::from_millis(10)).await;
        assert_eq!(
            last(),
            by_a,
            "the panes were taken with the release unwritten"
        );
        let release = writes.try_recv().expect("the release reached the app");
        assert!(release.ends_with(b"m"), "{release:?}");
        idle_for(QUIET).await;
        assert_ne!(last(), by_a, "the click did not hand the panes over");
        fixture.stop();
    }

    /// Review of #190: the driver's own press and drag are input that keeps
    /// its hold on the panes: another client typing during the gesture, even
    /// past the settle window from the press, does not take them.
    #[tokio::test(start_paused = true)]
    async fn the_drivers_gesture_keeps_the_panes_from_a_typing_viewer() {
        let fixture = attach_fixture().await;
        let (_writes, log, mut a, mut b, by_a) = driver_and_viewer_over_an_app(&fixture).await;
        let last = || *log.lock().unwrap().last().expect("sized");
        let at = point_in(&fixture, fixture.first_pane, 200, (2, 2)).await;
        a.send(&left_button(MouseKind::Down, at, a.written_frame))
            .await;
        idle_for(QUIET).await;
        b.key(b"x").await;
        idle_for(QUIET).await;
        assert_eq!(
            last(),
            by_a,
            "a key took the panes during the driver's first press"
        );
        for step in 1..=4u16 {
            idle_for(crate::clients::SETTLE / 2).await;
            a.send(&left_button(
                MouseKind::Drag,
                (at.0 + step, at.1),
                a.written_frame,
            ))
            .await;
        }
        idle_for(QUIET).await;
        b.key(b"y").await;
        idle_for(QUIET).await;
        assert_eq!(
            last(),
            by_a,
            "a key took the panes mid-drag, past SETTLE from the press"
        );
        fixture.stop();
    }

    /// Review of #190: a gesture held across a resize reports the cells the
    /// app's grid has now, not cells of the grid it had at the press.
    #[tokio::test]
    async fn a_held_gesture_stays_on_a_grid_that_shrank() {
        let mut h = AppMouseHarness::new().await;
        let pane = h.fixture.first_pane;
        let (mut wr, r) = install_app(&h, pane, (60, 300), b"\x1b[?1002h\x1b[?1006h").await;
        let point = (r.x + r.width - 1, r.y + r.height - 1);
        h.send(MouseKind::Down, ProtoMouseButton::Left, point.0, point.1)
            .await;
        let _press = recv_text(&mut wr).await;
        h.fixture
            .io_state
            .lock()
            .await
            .vts
            .get_mut(&pane)
            .expect("pane vt")
            .resize(10, 20);
        handle_app_mouse(
            MouseKind::Drag,
            ProtoMouseButton::Left,
            point.0,
            point.1,
            false,
            false,
            false,
            None,
            &h.fixture.graph,
            &h.fixture.io_state,
            &h.session,
            &h.client_size,
            &test_config(),
            &h.border_drag,
            &mut h.gesture,
        )
        .await
        .expect("drag");
        let drag = recv_text(&mut wr).await;
        let at: Vec<u16> = drag
            .trim_start_matches("\x1b[<32;")
            .trim_end_matches('M')
            .split(';')
            .map(|n| n.parse().expect("a coordinate"))
            .collect();
        assert!(
            at[0] <= 20 && at[1] <= 10,
            "reported {drag:?} off a 20x10 grid"
        );
        h.fixture.stop();
    }

    /// Review of #190: a text selection a viewer starts by clicking into
    /// another pane is not ended by the takeover that click earns: the panes
    /// change hands once it is released and yanked.
    #[tokio::test(start_paused = true)]
    async fn a_selection_that_focuses_a_pane_is_yanked_before_the_takeover() {
        let fixture = attach_fixture().await;
        let pane = fixture.first_pane;
        let (_w, rx, _s) = seed_io_for_pane(&fixture.io_state, pane).await;
        let (_w2, rx2, _s2) = seed_io_for_pane(&fixture.io_state, fixture.second_pane).await;
        let log = fake_pty(&fixture.io_state, pane, rx);
        let _log2 = fake_pty(&fixture.io_state, fixture.second_pane, rx2);
        let last = || *log.lock().unwrap().last().expect("sized");
        let _ = fixture.graph.focus_pane(fixture.second_pane).await;
        let mut a = Client::attach(&fixture, 200).await;
        idle_for(QUIET).await;
        a.catch_up().await;
        let by_a = last();
        let mut b = Client::attach(&fixture, 80).await;
        idle_for(QUIET).await;
        b.catch_up().await;
        let from = point_in(&fixture, pane, 80, (0, 0)).await;
        b.send(&left_button(MouseKind::Down, from, b.written_frame))
            .await;
        idle_for(QUIET).await;
        b.catch_up().await;
        b.send(&left_button(
            MouseKind::Drag,
            (from.0 + 4, from.1),
            b.written_frame,
        ))
        .await;
        idle_for(QUIET).await;
        b.catch_up().await;
        b.send(&left_button(
            MouseKind::Up,
            (from.0 + 4, from.1),
            b.written_frame,
        ))
        .await;
        let copied = b
            .frames_until(|f, _| {
                matches!(f, AttachServerFrame::Render { data, .. }
                    if BASE64.decode(data).is_ok_and(|d| d.starts_with(b"\x1b]52;")))
            })
            .await;
        assert!(copied, "the selection was ended before it was yanked");
        idle_for(QUIET).await;
        assert_ne!(
            last(),
            by_a,
            "the click into the pane did not take the panes"
        );
        fixture.stop();
    }

    /// Review of #190: a help sheet opened mid-gesture ends that gesture, and
    /// its press with it: a later click on nothing does not count it.
    #[tokio::test(start_paused = true)]
    async fn a_gesture_help_interrupted_is_no_input_later() {
        let fixture = attach_fixture().await;
        let (_writes, log, _a, mut b, by_a) = driver_and_viewer_over_an_app(&fixture).await;
        let last = || *log.lock().unwrap().last().expect("sized");
        let at = point_in(&fixture, fixture.first_pane, 80, (2, 2)).await;
        b.send(&left_button(MouseKind::Down, at, b.written_frame))
            .await;
        let help = AttachClientFrame::Action {
            kind: ActionKind::ToggleHelp,
            args: Default::default(),
        };
        b.send(&help).await;
        idle_for(QUIET).await;
        b.send(&left_button(MouseKind::Drag, at, b.written_frame))
            .await;
        b.send(&help).await;
        idle_for(QUIET).await;
        // A press on another pane's surplus: it reaches no app.
        {
            let mut io = fixture.io_state.lock().await;
            io.clients.pin(fixture.second_pane);
            let vt = io.vts.get_mut(&fixture.second_pane).expect("pane vt");
            vt.resize(4, 12);
            vt.process(b"\x1b[?1002h\x1b[?1006h");
        }
        idle_for(QUIET).await;
        b.catch_up().await;
        let surplus = point_in(&fixture, fixture.second_pane, 80, (1, 10)).await;
        b.send(&left_button(MouseKind::Down, surplus, b.written_frame))
            .await;
        b.send(&left_button(MouseKind::Up, surplus, b.written_frame))
            .await;
        idle_for(QUIET).await;
        assert_eq!(last(), by_a, "a press help ended was counted later");
        fixture.stop();
    }

    /// Review of #190: a viewer's release the program has not read within the
    /// bound is still queued behind a resize the PTY would apply first; that
    /// takeover is dropped, not taken late, and does not come back.
    #[tokio::test(start_paused = true)]
    async fn a_takeover_whose_release_is_not_written_in_time_is_dropped() {
        let fixture = attach_fixture().await;
        let (mut writes, log, _a, mut b, by_a) = driver_and_viewer_over_an_app(&fixture).await;
        let last = || *log.lock().unwrap().last().expect("sized");
        let at = point_in(&fixture, fixture.first_pane, 80, (2, 2)).await;
        b.send(&left_button(MouseKind::Down, at, b.written_frame))
            .await;
        idle_for(QUIET).await;
        let _press = writes.try_recv().expect("the press reached the app");
        b.send(&left_button(MouseKind::Up, at, b.written_frame))
            .await;
        idle_for(QUIET).await;
        assert_eq!(last(), by_a, "taken with the release still queued");
        let _release = writes.try_recv().expect("the release reached the app");
        idle_for(QUIET).await;
        assert_eq!(last(), by_a, "the dropped takeover was taken later");
        fixture.stop();
    }

    /// Review of #190: the driver's release keeps its hold for a settle window,
    /// as its keys do, though it held the button still for longer than one.
    #[tokio::test(start_paused = true)]
    async fn the_drivers_release_keeps_the_panes_for_a_settle_window() {
        let fixture = attach_fixture().await;
        let (_writes, log, mut a, mut b, by_a) = driver_and_viewer_over_an_app(&fixture).await;
        let last = || *log.lock().unwrap().last().expect("sized");
        let at = point_in(&fixture, fixture.first_pane, 200, (2, 2)).await;
        a.send(&left_button(MouseKind::Down, at, a.written_frame))
            .await;
        idle_for(crate::clients::SETTLE + QUIET).await;
        a.send(&left_button(MouseKind::Up, at, a.written_frame))
            .await;
        idle_for(QUIET).await;
        b.key(b"x").await;
        idle_for(QUIET).await;
        assert_eq!(
            last(),
            by_a,
            "a key took the panes the moment the driver let go"
        );
        fixture.stop();
    }

    /// Review of #190: a focusing press's pending credit goes with its text
    /// drag when modal copy mode takes the drag over; a later click on
    /// another pane's surplus does not spend it.
    #[tokio::test(start_paused = true)]
    async fn copy_mode_ends_a_focusing_presss_credit() {
        let fixture = attach_fixture().await;
        let (shell, other) = (fixture.first_pane, fixture.second_pane);
        let (_w, rx, _s) = seed_io_for_pane(&fixture.io_state, shell).await;
        let (_w2, rx2, _s2) = seed_io_for_pane(&fixture.io_state, other).await;
        let log = fake_pty(&fixture.io_state, shell, rx);
        let _log2 = fake_pty(&fixture.io_state, other, rx2);
        let last = || *log.lock().unwrap().last().expect("sized");
        let _ = fixture.graph.focus_pane(other).await;
        let mut a = Client::attach(&fixture, 200).await;
        idle_for(QUIET).await;
        a.catch_up().await;
        let by_a = last();
        {
            let mut io = fixture.io_state.lock().await;
            io.clients.pin(other);
            let vt = io.vts.get_mut(&other).expect("pane vt");
            vt.resize(4, 12);
            vt.process(b"\x1b[?1002h\x1b[?1006h");
        }
        let mut b = Client::attach(&fixture, 80).await;
        idle_for(QUIET).await;
        b.catch_up().await;
        let from = point_in(&fixture, shell, 80, (0, 0)).await;
        b.send(&left_button(MouseKind::Down, from, b.written_frame))
            .await;
        b.send(&AttachClientFrame::Action {
            kind: ActionKind::EnterCopyMode,
            args: Default::default(),
        })
        .await;
        b.send(&left_button(MouseKind::Up, from, b.written_frame))
            .await;
        b.key(b"q").await;
        idle_for(QUIET).await;
        b.catch_up().await;
        let surplus = point_in(&fixture, other, 80, (1, 10)).await;
        b.send(&left_button(MouseKind::Down, surplus, b.written_frame))
            .await;
        b.send(&left_button(MouseKind::Up, surplus, b.written_frame))
            .await;
        idle_for(QUIET).await;
        assert_eq!(last(), by_a, "a click on nothing spent a drag's old credit");
        fixture.stop();
    }

    /// A frame ends the copy mode it found unmappable, and only that one: a
    /// copy mode opened over another pane while the frame was drawn stays.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_copy_view_clamp_lands_only_on_the_view_it_was_for() {
        let fixture = attach_fixture().await;
        let (short, long) = (fixture.first_pane, fixture.second_pane);
        seed_io_for_pane(&fixture.io_state, short).await;
        let _wr = seed_scrollback_pane(&fixture.io_state, long, b"").await;
        let (stale, opened) = {
            let state = fixture.io_state.lock().await;
            (
                copy_mode_on(short, Rect::new(0, 0, 1, 1), &state.vts[&short]),
                copy_mode_on(long, Rect::new(0, 0, 3, 3), &state.vts[&long]),
            )
        };
        let mut attached = fixture.attached.clone();
        attached.active_pane_id = short;
        attached.copy_mode = Some(stale);
        let session = Arc::new(Mutex::new(attached));
        let onboarding = crate::onboarding::OnboardingHandle::from_state_for_test(
            crate::onboarding::OnboardingState {
                prefix_discovered: true,
                welcome_toast_seen: true,
            },
        );
        let held = onboarding.hold_for_test().await;
        let (tx, mut rx) = mpsc::channel(64);
        let cancel = fixture.cancel.child_token();
        let render = tokio::spawn(run_render_loop(
            fixture.graph.clone(),
            fixture.io_state.clone(),
            test_config(),
            SegmentCache::new(),
            crate::session_meta::SessionMetaCache::new(),
            onboarding.clone(),
            std::time::Instant::now(),
            session.clone(),
            Arc::new(Mutex::new((100, 30))),
            tx,
            cancel.clone(),
            false,
            false,
            Duration::from_secs(3600),
            WELCOME_TOAST_DWELL,
            shux_vt::monotonic_now_ns,
        ));
        let io_state = fixture.io_state.clone();
        shux_testkit::wait_until_async("the frame to hold io_state", || {
            let io_state = io_state.clone();
            async move { io_state.try_lock().is_err() }
        })
        .await;
        {
            let mut s = session.lock().await;
            s.copy_mode = Some(opened.clone());
            s.active_pane_id = long;
        }
        drop(held);
        let deadline = shux_testkit::Deadline::start();
        loop {
            match deadline.run(rx.recv()).await {
                Some(Some(AttachServerFrame::Render { frame: Some(_), .. })) => break,
                Some(Some(_)) => {}
                Some(None) | None => panic!("no frame"),
            }
        }
        assert_eq!(
            session.lock().await.copy_mode,
            Some(opened),
            "the frame ended a copy mode it never drew"
        );
        cancel.cancel();
        render.await.expect("render loop");
        fixture.stop();
    }

    /// A client that leaves takes its mouse-release waiters with it, so a
    /// long-lived pane collects none from clients that come and go.
    #[tokio::test(start_paused = true)]
    async fn a_departed_client_leaves_no_release_waiter() {
        let fixture = attach_fixture().await;
        let (_w, _r, _s) = seed_io_for_pane(&fixture.io_state, fixture.first_pane).await;
        let client = Client::attach(&fixture, 100).await;
        idle_for(QUIET).await;
        let id = {
            let io = fixture.io_state.lock().await;
            io.clients.list().first().expect("the attached client").id
        };
        let (_tx, rx) = tokio::sync::oneshot::channel();
        fixture
            .io_state
            .lock()
            .await
            .mouse_release_written
            .insert((fixture.first_pane, id), rx);
        client.stop().await;
        idle_for(QUIET).await;
        let left = fixture.io_state.lock().await.mouse_release_written.len();
        assert_eq!(left, 0, "a departed client left a release waiter behind");
        fixture.stop();
    }

    /// Issue #190: when the driver leaves, the next client takes the panes at
    /// once, and a pinned pane keeps its size through all of it.
    #[tokio::test(start_paused = true)]
    async fn the_next_client_drives_when_the_driver_leaves_and_pins_hold() {
        let fixture = attach_fixture().await;
        let (_w1, rx1, _s1) = seed_io_for_pane(&fixture.io_state, fixture.first_pane).await;
        let (_w2, rx2, _s2) = seed_io_for_pane(&fixture.io_state, fixture.second_pane).await;
        let first = fake_pty(&fixture.io_state, fixture.first_pane, rx1);
        let second = fake_pty(&fixture.io_state, fixture.second_pane, rx2);
        let clients = fixture.io_state.lock().await.clients.clone();
        clients.pin(fixture.second_pane);

        let a = Client::attach(&fixture, 200).await;
        idle_for(QUIET).await;
        let by_a = *first.lock().unwrap().last().expect("sized by A");
        let mut b = Client::attach(&fixture, 80).await;
        b.key(b"x").await;
        idle_for(QUIET).await;
        let by_b = *first.lock().unwrap().last().unwrap();
        assert_ne!(by_b, by_a);
        b.stop().await;
        idle_for(QUIET).await;
        assert_eq!(
            *first.lock().unwrap().last().unwrap(),
            by_a,
            "the driver left and its size stayed"
        );
        assert!(
            second.lock().unwrap().is_empty(),
            "a pinned pane was resized: {:?}",
            second.lock().unwrap()
        );
        a.stop().await;
        fixture.stop();
    }

    /// A `rows` x `cols` grid whose cell (r, c) holds letter `(r + c) % 26`,
    /// so any slice of it reads as a string the test can name.
    fn lettered(rows: usize, cols: usize) -> shux_vt::VirtualTerminal {
        let mut vt = shux_vt::VirtualTerminal::new(rows, cols);
        for r in 0..rows {
            let line: String = (0..cols)
                .map(|c| char::from(b'a' + ((r + c) % 26) as u8))
                .collect();
            vt.process(format!("\x1b[{};1H{line}", r + 1).as_bytes());
        }
        vt
    }

    /// The cursor position that makes cursor-follow show `rect` from grid row
    /// `row`, column `col`.
    fn follow_to(rect: Rect, row: usize, col: usize) -> Vec<u8> {
        let (h, w) = (usize::from(rect.height), usize::from(rect.width));
        format!("\x1b[{};{}H", row + h, col + w).into_bytes()
    }

    impl InputHarness {
        /// The rect the first pane is drawn in for this harness's client.
        async fn first_rect(&self) -> Rect {
            let size: ClientSize = Arc::new(Mutex::new((90, 24)));
            let pane = self.fixture.first_pane;
            pane_rect_for(
                &self.fixture.graph,
                &self.fixture.attached,
                &size,
                &test_config(),
                pane,
            )
            .await
            .expect("first pane rect")
        }

        /// Make `vt` the first pane's grid and wake every render loop.
        async fn show(&self, vt: shux_vt::VirtualTerminal) {
            let mut state = self.fixture.io_state.lock().await;
            state.vts.insert(self.fixture.first_pane, vt);
            state.render_pulse.bump();
        }

        /// What the first pane's application writes next.
        async fn app_writes(&self, bytes: &[u8]) {
            let mut state = self.fixture.io_state.lock().await;
            let pane = self.fixture.first_pane;
            state
                .vts
                .get_mut(&pane)
                .expect("first pane vt")
                .process(bytes);
            state.render_pulse.bump();
        }
    }

    impl Client {
        /// `prefix [`, stamped with `frame` as the real client stamps it.
        /// Built from JSON: the stamp is a field an older daemon ignores.
        async fn enter_copy_mode(&mut self, frame: Option<u64>) {
            let action = serde_json::json!({
                "type": "action",
                "kind": "enter_copy_mode",
                "args": { "frame": frame },
            });
            let action: AttachClientFrame = serde_json::from_value(action).expect("action json");
            self.send(&action).await;
        }

        /// A left-button event at screen `(col, row)`, stamped with the
        /// frame this client last applied.
        async fn left(&mut self, kind: MouseKind, col: u16, row: u16) {
            let frame = self.written_frame;
            self.send(&AttachClientFrame::Mouse {
                kind,
                button: ProtoMouseButton::Left,
                col,
                row,
                shift: false,
                alt: false,
                ctrl: false,
                frame,
            })
            .await;
        }

        /// The text of the next OSC 52 clipboard write, if one comes.
        async fn yanked(&mut self) -> Option<String> {
            let mut text = None;
            self.frames_until(|f, _| {
                let AttachServerFrame::Render { data, .. } = f else {
                    return false;
                };
                let bytes = BASE64.decode(data.as_bytes()).expect("render base64");
                let Some(rest) = bytes.strip_prefix(b"\x1b]52;c;") else {
                    return false;
                };
                let b64 = rest.strip_suffix(b"\x07").expect("OSC 52 ends in BEL");
                let raw = BASE64.decode(b64).expect("OSC 52 base64");
                text = Some(String::from_utf8(raw).expect("utf8 yank"));
                true
            })
            .await;
            text
        }
    }

    /// Copy mode opens over the slice the client was looking at when it
    /// pressed the key, though a frame with another slice was already on its
    /// way, and holds it while the application moves and hides its cursor.
    #[tokio::test(start_paused = true)]
    async fn copy_mode_yanks_from_the_frame_the_client_was_shown() {
        let mut h = InputHarness::new(64).await;
        let rect = h.first_rect().await;
        let mut vt = lettered(40, 120);
        vt.process(b"\x1b[40;101HNEWVIEW");
        vt.process(&follow_to(rect, 10, 40));
        h.show(vt).await;
        h.catch_up().await;
        let shown = h.written_frame;

        // A newer slice reaches the client's socket before its key does.
        h.app_writes(b"\x1b[40;120H").await;
        h.receive_unapplied("NEWVIEW").await;
        h.enter_copy_mode(shown).await;
        h.catch_up().await;
        h.app_writes(b"\x1b[?25l\x1b[1;1H").await;
        idle_for(QUIET).await;
        for key in [b"v", b"l", b"l", b"l", b"l", b"y"] {
            h.key(key).await;
        }
        // Row 10 from column 40: letters (10 + 40) % 26 onward.
        assert_eq!(h.yanked().await.as_deref(), Some("yzabc"));
        h.stop().await;
    }

    /// A drag in copy mode names cells of the frame the pointer was over, and
    /// its release copies them.
    #[tokio::test(start_paused = true)]
    async fn a_copy_mode_drag_yanks_what_its_frame_showed() {
        let mut h = InputHarness::new(64).await;
        let rect = h.first_rect().await;
        let mut vt = lettered(40, 120);
        vt.process(&follow_to(rect, 10, 40));
        h.show(vt).await;
        h.catch_up().await;
        let shown = h.written_frame;
        h.enter_copy_mode(shown).await;
        h.catch_up().await;
        h.app_writes(b"\x1b[40;120H").await;
        idle_for(QUIET).await;
        let (x, y) = (rect.x + 2, rect.y + 1);
        h.left(MouseKind::Down, x, y).await;
        h.left(MouseKind::Drag, x + 4, y).await;
        h.left(MouseKind::Up, x + 4, y).await;
        // Row 11 from column 42: letters (11 + 42) % 26 onward.
        assert_eq!(h.yanked().await.as_deref(), Some("bcdef"));
        h.stop().await;
    }

    /// A stamp that names no frame still kept opens nothing: the next key is
    /// the pane's. One whose frame's grid has been resized since opens over
    /// the slice the next frame draws, never the stale one.
    #[tokio::test(start_paused = true)]
    async fn copy_mode_refuses_an_expired_stamp_and_follows_a_resize() {
        let mut h = InputHarness::new(64).await;
        let rect = h.first_rect().await;
        let mut vt = lettered(40, 120);
        vt.process(&follow_to(rect, 10, 40));
        h.show(vt).await;
        h.catch_up().await;

        h.enter_copy_mode(Some(0)).await;
        h.key(b"x").await;
        assert_eq!(
            h.next_input().await,
            b"x",
            "an expired stamp opened copy mode"
        );

        h.catch_up().await;
        let shown = h.written_frame;
        let top_left = {
            let mut state = h.fixture.io_state.lock().await;
            let pane = h.fixture.first_pane;
            let vt = state.vts.get_mut(&pane).expect("vt");
            vt.resize(30, 100);
            let view = PaneViewport::of(rect, vt, shux_vt::monotonic_now_ns());
            vt.grid().visible_row(view.row_offset)[view.col_offset].ch
        };
        h.enter_copy_mode(shown).await;
        h.key(b"y").await;
        assert_eq!(
            h.yanked().await,
            Some(top_left.to_string()),
            "copy mode opened over the slice the stale frame showed"
        );
        h.stop().await;
    }

    /// Copy mode belongs to the pane it opened on: focus moving off it ends
    /// copy mode, and the pane's keys are its own again.
    #[tokio::test(start_paused = true)]
    async fn copy_mode_ends_when_focus_leaves_its_pane() {
        let mut h = InputHarness::new(64).await;
        h.show(lettered(10, 40)).await;
        h.catch_up().await;
        let shown = h.written_frame;
        h.enter_copy_mode(shown).await;
        assert!(h.frames_until(|_, text| text.contains("COPY")).await);
        h.fixture
            .graph
            .focus_pane(h.fixture.second_pane)
            .await
            .expect("focus second");
        h.fixture.io_state.lock().await.render_pulse.bump();
        assert!(
            h.frames_until(|_, text| !text.contains("COPY")).await,
            "copy mode followed focus to another pane"
        );
        h.fixture
            .graph
            .focus_pane(h.fixture.first_pane)
            .await
            .expect("focus first");
        h.key(b"q").await;
        assert_eq!(h.next_input().await, b"q");
        h.stop().await;
    }

    /// A selection whose lines scroll out of the pane's history is dropped,
    /// not stretched over whatever slid into their place: `y` then copies
    /// nothing, and says why.
    #[tokio::test(start_paused = true)]
    async fn a_copy_selection_whose_lines_are_evicted_is_dropped() {
        let mut h = InputHarness::new(64).await;
        let config = shux_vt::GridConfig {
            max_scrollback: 4,
            ..Default::default()
        };
        let mut vt = shux_vt::VirtualTerminal::with_config(6, 40, config);
        for i in 0..10 {
            vt.process(format!("e{i:02}\r\n").as_bytes());
        }
        h.show(vt).await;
        h.catch_up().await;
        let shown = h.written_frame;
        h.enter_copy_mode(shown).await;
        h.catch_up().await;
        for key in [b"v", b"l", b"l"] {
            h.key(key).await;
        }
        idle_for(QUIET).await;
        let flood: String = (10..40).map(|i| format!("e{i:02}\r\n")).collect();
        h.app_writes(flood.as_bytes()).await;
        idle_for(QUIET).await;
        h.key(b"y").await;
        let mut yanked = None;
        let told = h
            .frames_until(|f, text| {
                if let AttachServerFrame::Render { data, .. } = f
                    && BASE64
                        .decode(data.as_bytes())
                        .is_ok_and(|b| b.starts_with(b"\x1b]52;"))
                {
                    yanked = Some(());
                }
                text.contains("selection left history")
            })
            .await;
        assert_eq!(
            yanked, None,
            "yanked after the selection's lines were evicted"
        );
        assert!(told, "the status bar never said the selection is gone");
        h.stop().await;
    }

    /// Output the client has not drawn yet scrolls the pane: copy mode opens
    /// on the lines its stamped frame showed, not where they have moved since.
    #[tokio::test(start_paused = true)]
    async fn copy_mode_opens_on_the_lines_its_frame_showed() {
        let mut h = InputHarness::new(64).await;
        let mut vt = shux_vt::VirtualTerminal::new(10, 40);
        for i in 0..20 {
            vt.process(format!("S{i:02}\r\n").as_bytes());
        }
        vt.process(b"END");
        h.show(vt).await;
        h.catch_up().await;
        let shown = h.written_frame;
        h.app_writes(b"\r\nS20\r\nS21\r\nS22\r\nNEWEND").await;
        h.receive_unapplied("NEWEND").await;
        h.enter_copy_mode(shown).await;
        for key in [b"v", b"l", b"l", b"y"] {
            h.key(key).await;
        }
        // The frame showed S11..S19 and END.
        assert_eq!(h.yanked().await.as_deref(), Some("S11"));
        h.stop().await;
    }

    /// A frame of one screen names nothing on the other: `prefix [` stamped
    /// with it opens no copy mode, rather than one over the screen shown now.
    #[tokio::test(start_paused = true)]
    async fn a_stamp_from_another_screen_opens_no_copy_mode() {
        let mut h = InputHarness::new(64).await;
        h.show(lettered(10, 40)).await;
        h.catch_up().await;
        let shown = h.written_frame;
        h.app_writes(b"\x1b[?1049hALT").await;
        h.enter_copy_mode(shown).await;
        h.key(b"x").await;
        assert_eq!(
            h.next_input().await,
            b"x",
            "copy mode opened over the other screen"
        );
        h.stop().await;
    }

    /// Copy mode ends when its pane changes screen, even with nothing done in
    /// it yet: the next key is the pane's.
    #[tokio::test(start_paused = true)]
    async fn copy_mode_ends_when_its_pane_changes_screen() {
        let mut h = InputHarness::new(64).await;
        h.show(lettered(10, 40)).await;
        h.catch_up().await;
        let shown = h.written_frame;
        h.enter_copy_mode(shown).await;
        assert!(h.frames_until(|_, text| text.contains("COPY")).await);
        h.app_writes(b"\x1b[?1049hALT\x1b[?1049l").await;
        h.key(b"x").await;
        assert_eq!(
            h.next_input().await,
            b"x",
            "copy mode reopened over the screen shown after a switch"
        );
        h.stop().await;
    }

    /// The first key after a resize ends a copy mode with a selection is the
    /// pane's, though it arrives before any frame shows copy mode gone.
    #[tokio::test(start_paused = true)]
    async fn the_key_after_a_resize_ends_copy_mode_reaches_the_pane() {
        let mut h = InputHarness::new(64).await;
        h.show(lettered(10, 40)).await;
        h.catch_up().await;
        let shown = h.written_frame;
        h.enter_copy_mode(shown).await;
        h.key(b"v").await;
        let selecting = h
            .frames_until(|_, text| text.contains("COPY") && !text.contains("PgUp"))
            .await;
        assert!(selecting, "the selection never started");
        {
            let mut state = h.fixture.io_state.lock().await;
            let pane = h.fixture.first_pane;
            state.vts.get_mut(&pane).expect("vt").resize(8, 30);
        }
        h.key(b"x").await;
        assert_eq!(
            h.next_input().await,
            b"x",
            "the key that found copy mode gone was swallowed"
        );
        h.stop().await;
    }

    /// Copy mode with a selection, its pane then changed under it with no
    /// frame drawn since: what reaches the daemon next finds it gone.
    async fn copy_mode_gone_unseen(
        h: &mut InputHarness,
        change: impl FnOnce(&mut shux_vt::VirtualTerminal),
    ) {
        h.show(lettered(10, 40)).await;
        h.catch_up().await;
        let shown = h.written_frame;
        h.enter_copy_mode(shown).await;
        h.key(b"v").await;
        let selecting = h
            .frames_until(|_, text| text.contains("COPY") && !text.contains("PgUp"))
            .await;
        assert!(selecting, "the selection never started");
        h.catch_up().await;
        let mut state = h.fixture.io_state.lock().await;
        let pane = h.fixture.first_pane;
        change(state.vts.get_mut(&pane).expect("vt"));
    }

    /// A key whose copy mode the renderer ended between the input task
    /// seeing it and taking the lock is the pane's, not swallowed.
    #[tokio::test]
    async fn a_key_that_finds_copy_mode_ended_is_the_panes() {
        let fixture = attach_fixture().await;
        let mut s = fixture.attached.clone();
        s.copy_mode = None;
        let routed = {
            let state = fixture.io_state.lock().await;
            copy_mode_key_input(b"x", &mut s, None, &state)
        };
        assert_eq!(routed, None, "the key went to a copy mode already gone");
        fixture.stop();
    }

    /// A paste that finds copy mode gone is the pane's, all of it.
    #[tokio::test(start_paused = true)]
    async fn a_paste_after_copy_mode_stops_mapping_reaches_the_pane() {
        let mut h = InputHarness::new(64).await;
        copy_mode_gone_unseen(&mut h, |vt| vt.resize(8, 30)).await;
        h.paste(b"pasted", false).await;
        assert_eq!(
            h.next_input().await,
            b"pasted",
            "the paste that found copy mode gone was swallowed"
        );
        h.stop().await;
    }

    /// A paste that began in copy mode stays there to its last piece, though
    /// copy mode ends meanwhile: none of it reaches the pane.
    #[tokio::test(start_paused = true)]
    async fn a_paste_that_began_in_copy_mode_is_swallowed_to_its_end() {
        let mut h = InputHarness::new(64).await;
        h.show(lettered(10, 40)).await;
        h.catch_up().await;
        let shown = h.written_frame;
        h.enter_copy_mode(shown).await;
        h.key(b"v").await;
        let selecting = h
            .frames_until(|_, text| text.contains("COPY") && !text.contains("PgUp"))
            .await;
        assert!(selecting, "the selection never started");
        h.paste(b"head", true).await;
        // Handled after the piece: a new size moves the pane out from under
        // the selection.
        h.send(&AttachClientFrame::Resize { cols: 80, rows: 24 })
            .await;
        assert!(
            h.frames_until(|_, text| !text.contains("COPY")).await,
            "copy mode never ended"
        );
        h.paste(b"tail", false).await;
        h.key(b"k").await;
        assert_eq!(
            h.next_input().await,
            b"k",
            "part of the paste reached the pane"
        );
        h.stop().await;
    }

    /// A wheel stamped with an older frame the client was shown opens copy
    /// mode over that frame's lines, though a newer one has been drawn since.
    #[tokio::test(start_paused = true)]
    async fn the_wheel_opens_copy_mode_over_its_older_stamped_frame() {
        let mut h = InputHarness::new(64).await;
        let rect = h.first_rect().await;
        let mut vt = shux_vt::VirtualTerminal::new(10, 40);
        for i in 0..40 {
            vt.process(format!("W{i:02}\r\n").as_bytes());
        }
        h.show(vt).await;
        h.catch_up().await;
        let older = h.written_frame;
        h.app_writes(b"X00\r\nX01\r\nX02\r\nX03\r\nX04\r\n").await;
        assert!(h.frames_until(|_, text| text.contains("X04")).await);
        h.catch_up().await;
        assert_ne!(h.written_frame, older, "setup: no newer frame drawn");
        h.send(&AttachClientFrame::Mouse {
            kind: MouseKind::ScrollUp,
            button: ProtoMouseButton::None,
            col: rect.x + 1,
            row: rect.y + 1,
            shift: false,
            alt: false,
            ctrl: false,
            frame: older,
        })
        .await;
        for key in [b"v", b"l", b"l", b"y"] {
            h.key(key).await;
        }
        // The older frame's top line was W31; the wheel scrolls three up.
        assert_eq!(h.yanked().await.as_deref(), Some("W28"));
        h.stop().await;
    }

    /// The frame that ends copy mode, because it no longer maps, drops its
    /// status bar flag with it.
    #[tokio::test(start_paused = true)]
    async fn the_frame_that_ends_copy_mode_drops_its_flag() {
        let mut h = InputHarness::new(64).await;
        let bar = |c: &Client| {
            let grid = c.screen.grid();
            let row = grid.visible_row(grid.rows() - 1);
            (0..row.len()).map(|c| row[c].ch).collect::<String>()
        };
        let rect = h.first_rect().await;
        let mut vt = shux_vt::VirtualTerminal::new(10, 40);
        for i in 0..30 {
            vt.process(format!("W{i:02}\r\n").as_bytes());
        }
        h.show(vt).await;
        h.catch_up().await;
        // The wheel, not `prefix [`: its notice would cover the flag.
        let frame = h.written_frame;
        h.send(&AttachClientFrame::Mouse {
            kind: MouseKind::ScrollUp,
            button: ProtoMouseButton::None,
            col: rect.x + 1,
            row: rect.y + 1,
            shift: false,
            alt: false,
            ctrl: false,
            frame,
        })
        .await;
        h.key(b"v").await;
        let flagged = h
            .frames_until(|_, text| text.lines().last().is_some_and(|l| l.contains(" Y ")))
            .await;
        assert!(
            flagged,
            "setup: no copy-mode flag in the status bar: {:?}",
            bar(&h)
        );
        {
            let mut state = h.fixture.io_state.lock().await;
            let pane = h.fixture.first_pane;
            state.vts.get_mut(&pane).expect("vt").resize(8, 30);
            state.render_pulse.bump();
        }
        let cleared = h.frames_until(|_, text| !text.contains("COPY")).await;
        assert!(cleared, "copy mode never ended");
        assert!(
            !bar(&h).contains(" Y "),
            "the flag outlived copy mode: {:?}",
            bar(&h)
        );
        h.stop().await;
    }

    /// A click that finds copy mode gone is the application's.
    #[tokio::test(start_paused = true)]
    async fn a_click_after_copy_mode_stops_mapping_reaches_the_app() {
        let mut h = InputHarness::new(64).await;
        let rect = h.first_rect().await;
        copy_mode_gone_unseen(&mut h, |vt| {
            vt.process(b"\x1b[?1049h\x1b[?1000h\x1b[?1006h");
        })
        .await;
        h.left(MouseKind::Down, rect.x + 2, rect.y + 1).await;
        let input = h.next_input().await;
        assert!(
            input.starts_with(b"\x1b[<0;3;2M"),
            "the click that found copy mode gone was not the app's: {:?}",
            String::from_utf8_lossy(&input)
        );
        h.stop().await;
    }

    /// A drag stamped with a frame whose top line has since been evicted names
    /// a line no longer there, though the view, pinned at the oldest line,
    /// has not moved: it is dropped, not yanked from the line that replaced it.
    #[tokio::test(start_paused = true)]
    async fn a_copy_mode_drag_over_evicted_lines_is_dropped() {
        let mut h = InputHarness::new(64).await;
        let rect = h.first_rect().await;
        let config = shux_vt::GridConfig {
            max_scrollback: 4,
            ..Default::default()
        };
        let mut vt = shux_vt::VirtualTerminal::with_config(6, 40, config);
        for i in 0..10 {
            vt.process(format!("e{i:02}\r\n").as_bytes());
        }
        h.show(vt).await;
        h.catch_up().await;
        let shown = h.written_frame;
        h.enter_copy_mode(shown).await;
        h.key(b"g").await;
        h.key(b"g").await;
        let oldest = |c: &Client| {
            let grid = c.screen.grid();
            let row = grid.visible_row(usize::from(rect.y));
            let x = usize::from(rect.x);
            (x..x + 3).map(|c| row[c].ch).collect::<String>()
        };
        assert!(h.frames_until(|_, text| text.contains("e01")).await);
        h.catch_up().await;
        assert_eq!(oldest(&h), "e01", "setup: not at the oldest line");
        // One more line evicts that one; the client has not drawn since.
        h.app_writes(b"e10\r\n").await;
        let (x, y) = (rect.x, rect.y);
        h.left(MouseKind::Down, x, y).await;
        h.left(MouseKind::Drag, x + 2, y).await;
        h.left(MouseKind::Up, x + 2, y).await;
        h.key(b"y").await;
        // The cursor's cell alone: nothing was selected.
        assert_eq!(h.yanked().await.as_deref(), Some("e"));
        h.stop().await;
    }

    /// A drag stamped with a frame from before the copy view moved names cells
    /// that view showed, not the ones under the pointer now: it is dropped,
    /// and leaves copy mode as it was.
    #[tokio::test(start_paused = true)]
    async fn a_copy_mode_drag_over_a_page_since_turned_is_dropped() {
        let mut h = InputHarness::new(64).await;
        let rect = h.first_rect().await;
        let mut vt = shux_vt::VirtualTerminal::new(10, 40);
        for i in 0..40 {
            vt.process(format!("S{i:02}\r\n").as_bytes());
        }
        h.show(vt).await;
        h.catch_up().await;
        let shown = h.written_frame;
        h.enter_copy_mode(shown).await;
        assert!(h.frames_until(|_, text| text.contains("COPY")).await);
        h.catch_up().await;
        // A page up the client has not drawn yet.
        h.key(b"\x1b[5~").await;
        let (x, y) = (rect.x, rect.y + 1);
        h.left(MouseKind::Down, x, y).await;
        h.left(MouseKind::Drag, x + 2, y).await;
        h.left(MouseKind::Up, x + 2, y).await;
        h.key(b"y").await;
        // The cursor where the page left it, nothing selected.
        assert_eq!(h.yanked().await.as_deref(), Some("S"));
        h.stop().await;
    }

    /// Enter copy mode over the first pane's `vt` and yank what `keys` select.
    async fn search_yank(vt: shux_vt::VirtualTerminal, keys: &[&[u8]]) -> Option<String> {
        let mut h = InputHarness::new(64).await;
        h.show(vt).await;
        h.catch_up().await;
        let shown = h.written_frame;
        h.enter_copy_mode(shown).await;
        h.catch_up().await;
        // One key per frame, as a terminal reports typing.
        for key in keys
            .iter()
            .flat_map(|k| std::str::from_utf8(k).expect("utf8").chars())
        {
            h.key(key.to_string().as_bytes()).await;
        }
        let text = h.yanked().await;
        h.stop().await;
        text
    }

    const SELECT_SIX: [&[u8]; 7] = [b"v", b"l", b"l", b"l", b"l", b"l", b"y"];

    /// A search finds a match on grid rows below the slice and scrolls to it.
    /// (Panning right to one is `a_pan_keeps_the_selection_it_started_with`.)
    #[tokio::test(start_paused = true)]
    async fn a_search_reveals_a_match_right_of_or_below_the_slice() {
        let keys = [&[b"/needle\r" as &[u8]][..], &SELECT_SIX].concat();
        let mut tall = shux_vt::VirtualTerminal::new(60, 40);
        tall.process(b"\x1b[51;4Hneedle\x1b[1;1H");
        assert_eq!(search_yank(tall, &keys).await.as_deref(), Some("needle"));
    }

    /// Match columns are the cells that hold the matched text: a wide glyph
    /// is two, a combining sequence one.
    #[tokio::test(start_paused = true)]
    async fn a_search_maps_matched_text_to_its_cells() {
        let row = "\x1b[3;1H界界e\u{301}x needle\x1b[1;1H";
        let mut vt = shux_vt::VirtualTerminal::new(10, 40);
        vt.process(row.as_bytes());
        let keys = [&[b"/needle\r" as &[u8]][..], &SELECT_SIX].concat();
        assert_eq!(search_yank(vt, &keys).await.as_deref(), Some("needle"));

        let mut vt = shux_vt::VirtualTerminal::new(10, 40);
        vt.process(row.as_bytes());
        let keys: [&[u8]; 6] = [b"/", "界e".as_bytes(), b"\r", b"v", b"l", b"l"];
        let keys = [&keys[..], &[b"y" as &[u8]]].concat();
        assert_eq!(search_yank(vt, &keys).await.as_deref(), Some("界e\u{301}"));
    }

    /// A backward search finds the match the cursor sits inside, and a
    /// forward one wraps back to a match left of the cursor on its own row.
    #[tokio::test(start_paused = true)]
    async fn a_search_finds_matches_across_and_behind_the_cursor() {
        let mut vt = shux_vt::VirtualTerminal::new(10, 40);
        vt.process(b"\x1b[5;1Hxx needle yy\x1b[1;1H");
        let keys = [&[b"/dle\r" as &[u8], b"?needle\r"][..], &SELECT_SIX].concat();
        assert_eq!(search_yank(vt, &keys).await.as_deref(), Some("needle"));

        let mut vt = shux_vt::VirtualTerminal::new(10, 40);
        vt.process(b"\x1b[5;1Hxx needle yy\x1b[1;1H");
        let keys = [&[b"/yy\r" as &[u8], b"/needle\r"][..], &SELECT_SIX].concat();
        assert_eq!(search_yank(vt, &keys).await.as_deref(), Some("needle"));
    }

    /// A grid smaller than its rect pages through history in the rows and
    /// columns it was shown in; the surplus beside and below keeps its fill.
    #[tokio::test(start_paused = true)]
    async fn copy_mode_over_an_undersized_grid_leaves_its_surplus() {
        let mut h = InputHarness::new(64).await;
        let rect = h.first_rect().await;
        let mut vt = shux_vt::VirtualTerminal::new(5, 20);
        for i in 0..30 {
            vt.process(format!("\r\nh{i:02}").as_bytes());
        }
        h.show(vt).await;
        h.catch_up().await;
        let surplus = |c: &Client| {
            let (x, y) = (usize::from(rect.x) + 25, usize::from(rect.y) + 7);
            (
                c.screen.grid().visible_row(usize::from(rect.y))[x].style.bg,
                c.screen.grid().visible_row(y)[x].style.bg,
            )
        };
        let before = surplus(&h);
        assert_ne!(before.0, shux_vt::Color::Default, "setup: no surplus fill");
        let shown = h.written_frame;
        h.enter_copy_mode(shown).await;
        h.key(b"\x02").await;
        idle_for(QUIET).await;
        h.catch_up().await;
        assert_eq!(surplus(&h), before, "copy mode painted over the surplus");
        for key in [b"v", b"l", b"l", b"y"] {
            h.key(key).await;
        }
        // Five rows a page: the slice now starts five lines above h25.
        assert_eq!(h.yanked().await.as_deref(), Some("h20"));
        h.stop().await;
    }

    /// A client that says how big its cells are in pixels is told where
    /// inside a cell a cropped picture begins in those pixels.
    #[tokio::test(start_paused = true)]
    async fn a_mid_cell_crop_is_placed_in_the_clients_own_pixels() {
        use base64::Engine as _;
        let mut h = InputHarness::new(64).await;
        let rect = h.first_rect().await;
        let onboarding = crate::onboarding::OnboardingHandle::from_state_for_test(
            crate::onboarding::OnboardingState {
                prefix_discovered: true,
                welcome_toast_seen: true,
            },
        );
        let drawing = Client::attach_drawing(&h.fixture, 90, onboarding, false, true).await;
        std::mem::replace(&mut h.client, drawing).stop().await;
        h.send(&AttachClientFrame::CellPixels {
            width: 18,
            height: 38,
        })
        .await;
        // 4x1 px fitted to five columns at column 3; the slice starts at
        // column 4, a quarter of pixel 1 cut: 4.5 px of an 18 px cell.
        let mut vt = shux_vt::VirtualTerminal::new(10, 120);
        let px = base64::engine::general_purpose::STANDARD.encode([90u8; 12]);
        vt.process(format!("\x1b[1;4H\x1b_Ga=t,f=24,s=4,v=1,i=5;{px}\x1b\\").as_bytes());
        vt.process(b"\x1b_Ga=p,i=5,c=5,r=1,C=1\x1b\\");
        vt.process(&follow_to(rect, 0, 4));
        h.show(vt).await;
        let placed = h
            .frames_until(|f, _| {
                let AttachServerFrame::Render { data, .. } = f else {
                    return false;
                };
                let bytes = BASE64.decode(data.as_bytes()).unwrap_or_default();
                String::from_utf8_lossy(&bytes).contains("w=3,h=1,c=4,r=1,X=4")
            })
            .await;
        assert!(placed, "the crop was not placed in the client's own pixels");
        h.stop().await;
    }
}
