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
use shux_ui::viewport::PaneView;
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

/// Total time the daemon will wait for the AttachHello frame before
/// dropping the connection. Prevents slowloris-style blocking.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// How often the daemon pings the client to detect dead peers.
const PING_INTERVAL: Duration = Duration::from_secs(15);

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
#[allow(clippy::too_many_arguments)]
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
}

/// Pane views of the latest frames sent to one client, oldest first, each
/// under the number its `Render { frame }` carried.
#[derive(Debug, Clone, Default)]
struct ShownFrames(std::collections::VecDeque<(u64, Arc<HashMap<PaneId, PaneView>>)>);

impl ShownFrames {
    /// Frames a client can trail the daemon by and still be mapped.
    const KEPT: usize = 16;

    fn push(&mut self, frame: u64, views: HashMap<PaneId, PaneView>) {
        if self.0.len() == Self::KEPT {
            self.0.pop_front();
        }
        self.0.push_back((frame, Arc::new(views)));
    }

    /// The views the client saw when it had written `frame`: the newest frame
    /// not after it. `None` (a client that predates frame numbers) is the
    /// latest frame sent. A stamp older than every frame kept names nothing:
    /// any kept frame is newer than what that client shows.
    fn views(&self, frame: Option<u64>) -> Option<&HashMap<PaneId, PaneView>> {
        let entry = match frame {
            None => self.0.back(),
            Some(f) => self.0.iter().rev().find(|(n, _)| *n <= f),
        };
        entry.map(|(_, views)| views.as_ref())
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
#[allow(clippy::too_many_arguments)]
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
                // Matched inside the arm, not in its pattern: a pattern that
                // refuses `None` disables the arm and leaves `cancelled()`
                // pending forever, so the writer never ends on its own.
                frame = out_rx.recv() => {
                    let Some(frame) = frame else { break };
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
            hello.graphics,
            fallback_tick(),
            WELCOME_TOAST_DWELL,
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
    // Sent at teardown, once nothing else can queue a frame behind it.
    let mut final_frame = None;
    let mut input_backlogs: HashMap<PaneId, InputBacklog> = HashMap::new();
    let client_id = NEXT_CLIENT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // The paste whose pieces are arriving with its pane, and how many this
    // client has sent.
    let (mut pasting, mut pastes): (Option<(u64, PaneId)>, u64) = (None, 0);
    while !detached {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = alive_tick.tick() => {}
            () = &mut cfg_listener => {
                cfg_listener = Box::pin(cfg_changed.notified());
                let live = config.current().appearance.border_style.clone();
                if live != last_border_style {
                    last_border_style = live;
                    let attached = render_session.lock().await.clone();
                    let (cols, rows) = *client_size.lock().await;
                    apply_resize_to_window(
                        &graph, &io_state, &attached, &config, cols, rows,
                    )
                    .await;
                    let pulse = io_state.lock().await.render_pulse.clone();
                    pulse.bump();
                }
            }
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
                            if paste.is_some() && s.copy_mode.is_some() {
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
                                    pulse.bump();
                                }
                                shux_ui::CopyKey::Exit => {
                                    let mut s = render_session.lock().await;
                                    s.copy_mode = None;
                                    drop(s);
                                    let pulse = io_state.lock().await.render_pulse.clone();
                                    pulse.bump();
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
                                            frame: None,
                                        };
                                        let _ = out_tx.send(frame).await;
                                    }
                                    let mut s = render_session.lock().await;
                                    s.copy_mode = None;
                                    drop(s);
                                    let pulse = io_state.lock().await.render_pulse.clone();
                                    pulse.bump();
                                }
                            }
                            continue;
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
                            match reserve_input(&tx, bytes.len(), budget, &cancel).await {
                                // Room the moment it was asked for: caught up.
                                Room::Ready(permit, waited) => {
                                    backlog.waited = if waited.is_zero() {
                                        Duration::ZERO
                                    } else {
                                        backlog.waited + waited
                                    };
                                    let mut state = io_state.lock().await;
                                    send_input(&mut state, target, permit, bytes, paste, client_id);
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
                        {
                            let mut cs = client_size.lock().await;
                            *cs = (cols, rows);
                        }
                        let attached = render_session.lock().await.clone();
                        apply_resize_to_window(&graph, &io_state, &attached, &config, cols, rows).await;
                        let pulse = io_state.lock().await.render_pulse.clone();
                        pulse.bump();
                    }
                    AttachClientFrame::Action { kind, args } => {
                        // The user pressed prefix + key — onboarding hint
                        // can dismiss. Cheap idempotent write; first call
                        // persists, subsequent ones short-circuit.
                        onboarding.mark_prefix_discovered().await;
                        if std::mem::take(&mut render_session.lock().await.show_welcome_toast) {
                            onboarding.mark_welcome_toast_seen().await;
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
                        // Layout-changing actions invalidate per-pane PTY
                        // sizes. Re-fan the winsizes so vim/htop/etc. inside
                        // each pane learn their new dimensions.
                        if action_changes_layout(kind) {
                            let attached = render_session.lock().await.clone();
                            let (cols, rows) = *client_size.lock().await;
                            apply_resize_to_window(&graph, &io_state, &attached, &config, cols, rows).await;
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
                        match handle_app_mouse(
                            kind,
                            button,
                            col,
                            row,
                            shift,
                            alt,
                            ctrl,
                            frame,
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
                                    pulse.bump();
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
                            pulse.bump();
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
                            pulse.bump();
                            continue;
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
                        .await?
                        {
                            let pulse = io_state.lock().await.render_pulse.clone();
                            pulse.bump();
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
                        pulse.bump();
                    }
                    AttachClientFrame::Detach => {
                        // Detach implies the user found the prefix too.
                        onboarding.mark_prefix_discovered().await;
                        detached = true;
                        final_frame = Some(AttachServerFrame::DetachAck);
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
            final_frame.get_or_insert(AttachServerFrame::SessionEnded {
                reason: "session_destroyed".into(),
            });
            break;
        }
    }

    // A gesture forwarded to a pane app outlives the attach: the PTY does not
    // go away when the client does. Detaching mid-drag without this leaves the
    // app believing a button is still held, and it stays that way for the rest
    // of the pane's life — visible next time anyone attaches.
    close_open_pastes(&io_state, client_id).await;
    release_app_gesture(&io_state, &mut selection_drag).await;

    // Stop every other holder of `out_tx` first, so the final frame is the
    // last one queued: the writer drains it and ends only once the last
    // sender is gone, which is what hangs up on the client.
    renderer.abort();
    pinger.abort();
    let _ = renderer.await;
    let _ = pinger.await;
    if let Some(frame) = final_frame {
        let _ = out_tx.send(frame).await;
    }
    drop(out_tx);
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
    graphics: bool,
    tick_period: Duration,
    toast_dwell: Duration,
) {
    // Before anything else: a receiver starts at the current generation.
    let mut pulse = io_state.lock().await.render_pulse.subscribe();
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
    compositor.set_graphics(graphics);

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

    loop {
        // A bump landed while the last frame was being drawn: output is
        // arriving faster than this loop renders.
        let behind = pulse.has_changed().unwrap_or(false);
        tokio::select! {
            _ = cancel.cancelled() => break,
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
        // Status bar text. Start from the built-in (always-good)
        // segments so OOTB looks the same even when no script segments
        // are configured. Then `populate_bar` appends any
        // `[[statusbar.segment]]` results from the runner cache.
        let nerd_fonts = live_cfg.appearance.nerd_fonts;
        let pane_chrome = crate::pane_chrome::pane_chrome(
            &snap,
            win.layout.tree.pane_ids(),
            &live_cfg.program_status,
            nerd_fonts,
            &live_theme,
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
            action_error: attached.action_error.as_deref(),
        };
        let mut bar = build_status_bar_shared(&snap, &live_theme, &render_ctx);
        populate_bar(&mut bar, &live_cfg, &segments).await;
        crate::statusbar_build::append_waiting(
            &mut bar,
            crate::pane_chrome::waiting_count(&snap, attached.session_id, &live_cfg.program_status),
            &live_theme,
            nerd_fonts,
            cols,
        );
        if attached.action_error.is_some()
            || last_action_ref
                .is_some_and(|(_, at)| at.elapsed() < crate::statusbar_build::ACTION_FEEDBACK_DWELL)
        {
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
        };
        compositor.set_cursor_hidden(
            attached.help_visible
                || copy_overlay
                    .as_ref()
                    .is_some_and(|o| matches!(o.kind, CopyOverlayKind::Modal)),
        );
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

        let sheet = |label: String| SheetStamp {
            cols,
            rows,
            theme: live_theme,
            label,
        };
        if let Some(menu) = attached.copy_menu {
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
        last_copy_menu = attached.copy_menu;

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
        // Re-establish capacity for next frame.
        compositor.inner_mut().reserve(64 * 1024);
        drop(state);
        // Only once `io_state` is released: the input task takes the session
        // before `io_state`, so holding both the other way round deadlocks.
        if toast_expired {
            session.lock().await.show_welcome_toast = false;
        }

        if !bytes.is_empty() {
            frame_number += 1;
            let views = compositor.pane_views().clone();
            if !send_frame(&out_tx, &session, frame_number, views, &bytes).await {
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
    views: HashMap<PaneId, PaneView>,
    bytes: &[u8],
) -> bool {
    session.lock().await.shown.push(frame, views);
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
    /// `anchor` is the screen point of the last forwarded event and
    /// `row_offset` the view offset it mapped through. An event at that same
    /// point maps through it too, so an app that moves its cursor with the
    /// mouse (vim) does not shift a click or a drag's release onto another row
    /// when the view follows; an event anywhere else names the cell shown
    /// under the pointer now.
    App {
        pane_id: PaneId,
        buttons: u8,
        last: (u16, u16),
        row_offset: usize,
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
            let hit = if let Some(zoom) = &win.layout.zoom {
                Some((zoom.zoomed_pane, current_content_rect(client_size).await))
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
                            frame: None,
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
async fn release_app_gesture(io_state: &Arc<Mutex<PaneIoState>>, gesture: &mut SelectionDrag) {
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
            forward_bytes_to_pane(io_state, pane_id, bytes, Lane::Urgent).await;
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
#[allow(clippy::too_many_arguments)]
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
    let viewport = current_viewport(client_size, config).await;

    // Resolve the pane under the pointer in the frame the client was shown: a
    // layout not drawn yet must not move a click into another pane. Views are
    // content rects, so borders, the outline and the status bar hit none.
    // A frame no longer kept names nothing. A gesture shux owns still
    // finishes through shux; anything else must not fall through to a border
    // drag or a selection at today's geometry.
    let Some(views) = attached.shown.views(frame) else {
        return Ok(match *gesture {
            SelectionDrag::CopyMode | SelectionDrag::MouseSelection { .. } => AppMouse::NotHandled,
            SelectionDrag::None if border_drag.is_some() => AppMouse::NotHandled,
            SelectionDrag::App { .. } => {
                if action == ButtonAction::Release {
                    release_app_gesture(io_state, gesture).await;
                }
                AppMouse::Consumed { redraw: false }
            }
            SelectionDrag::None => AppMouse::Consumed { redraw: false },
        });
    };
    let pane_hit = views
        .iter()
        .find(|(_, view)| holds(view.rect, col, row))
        .map(|(pid, _)| *pid);
    let content = current_content_rect(client_size).await;
    // A pane closed, hidden by a zoom or in another window since the frame
    // was drawn takes nothing more: what shux would do there now is not what
    // the user aimed at.
    if let Some(pid) = pane_hit
        && matches!(*gesture, SelectionDrag::None)
        && border_drag.is_none()
        && pane_rect_in(graph, &attached, content, viewport, pid).is_none()
    {
        return Ok(AppMouse::Consumed { redraw: false });
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
                release_app_gesture(io_state, gesture).await;
            } else {
                end_gesture_if_released(gesture, action, button);
            }
            return Ok(AppMouse::Consumed { redraw: false });
        }
        AppMouseRoute::Forward(pane_id) => pane_id,
    };

    // The pane can lose its rect mid-gesture: a window switch or a zoom leaves
    // it off-screen. Let the app know the button came up rather than letting
    // the gesture evaporate with it still held.
    if pane_rect_in(graph, &attached, content, viewport, pane_id).is_none() {
        release_app_gesture(io_state, gesture).await;
        return Ok(AppMouse::Consumed { redraw: false });
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
            row_offset, anchor, ..
        } if pin && anchor == (col, row) => shown.map(|view| view.with_row_offset(row_offset)),
        _ => shown,
    };
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
        release_app_gesture(io_state, gesture).await;
        return Ok(AppMouse::Consumed { redraw: false });
    }
    let Some((view, (local_col, local_row))) = cell else {
        // No cell: a press only selects the pane, and motion is dropped.
        let redraw = action == ButtonAction::Press
            && select_pane_on_press(graph, session, attached.active_pane_id, pane_id).await;
        return Ok(AppMouse::Consumed { redraw });
    };

    let Some(cb) = button_cb(action, button, alt, ctrl) else {
        // No truthful report exists for a buttonless press or release, but a
        // release must still clear the latch or the app sits button-held for
        // the rest of the pane's life.
        end_gesture_if_released(gesture, action, button);
        return Ok(AppMouse::NotHandled);
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
        return Ok(AppMouse::Consumed { redraw });
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
    if !forward_bytes_to_pane(io_state, pane_id, bytes, lane).await {
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
            row_offset: view.row_offset,
            anchor: (col, row),
        };
    } else {
        // Where the pointer was when the gesture is abandoned, so
        // `release_app_gesture` can put the synthetic release there.
        if let SelectionDrag::App {
            last,
            row_offset,
            anchor,
            ..
        } = gesture
        {
            *last = (local_col, local_row);
            *row_offset = view.row_offset;
            *anchor = (col, row);
        }
        end_gesture_if_released(gesture, action, button);
    }
    Ok(AppMouse::Consumed { redraw })
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
) -> bool {
    match io_state
        .lock()
        .await
        .try_send_outside_paste(pane_id, bytes, lane)
    {
        None => false,
        Some(Ok(())) => true,
        Some(Err(e)) => {
            tracing::warn!(error = %e, "mouse forward dropped (pane backpressured)");
            false
        }
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

    // Resolve the pane under the cursor in the frame the client was shown
    // (fallback: the active pane). A frame no longer kept names nothing.
    let Some(views) = attached.shown.views(frame) else {
        return Ok(true);
    };
    let pane_id = views
        .iter()
        .find(|(_, view)| holds(view.rect, col, row))
        .map_or(attached.active_pane_id, |(pid, _)| *pid);
    let Some(rect) = pane_rect_for(graph, &attached, client_size, config, pane_id).await else {
        return Ok(true);
    };

    // Snapshot the target pane's live mode state + scrollback depth. A pane shux
    // cannot encode for is treated as not mouse-aware, so the wheel falls back
    // to shux's own scrollback. See `coords_are_encodable`.
    let (mouse_on, sgr, alt, alt_scroll, app_cursor, total_lines) = {
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
                    vt.presented_total_lines(),
                )
            }
            None => (false, false, false, true, false, rect.height as usize),
        }
    };

    match route_wheel(mouse_on, alt, alt_scroll) {
        WheelRouting::ForwardMouse => {
            let cell = views
                .get(&pane_id)
                .and_then(|view| shux_ui::viewport::pane_grid_cell(view, (col, row), false));
            // No grid cell under the pointer, or no truthful encoding for this
            // app's coordinate mode: sending nothing beats a tick aimed at a
            // cell the user never touched.
            if let Some(bytes) = cell.and_then(|(local_col, local_row)| {
                encode_mouse_wheel(up, sgr, local_col, local_row)
            }) {
                forward_bytes_to_pane(io_state, pane_id, bytes, Lane::Main).await;
            }
        }
        WheelRouting::ArrowKeys => {
            let seq = wheel_arrow_seq(up, app_cursor);
            let mut bytes = Vec::with_capacity(seq.len() * 3);
            for _ in 0..3 {
                bytes.extend_from_slice(seq);
            }
            forward_bytes_to_pane(io_state, pane_id, bytes, Lane::Main).await;
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
    pane_rect_in(graph, attached, content, viewport, pane_id)
}

/// `pane_rect_for` with the geometry already resolved, so a caller that has
/// already computed the viewport for this event hit-tests against that one
/// rather than re-reading the live config and getting a second answer.
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
        frame: None,
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
    let snap = graph.snapshot();
    let count = snap
        .sessions
        .get(&attached.session_id)
        .map(|s| s.windows.len())
        .unwrap_or(0);
    let title = format!("window-{}", count + 1);
    let created = crate::pane_spawn::create_window_noting_focus(
        graph,
        attached.session_id,
        Some(title),
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
            Duration::from_secs(3600),
            WELCOME_TOAST_DWELL,
        ));

        let mut screen = shux_vt::VirtualTerminal::new(40, 100);
        let deadline = tokio::time::Instant::now() + shux_testkit::BUDGET;
        while !screen.capture_text(None).contains("keybindings") {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(AttachServerFrame::Render { data, .. })) => {
                    screen.process(&BASE64.decode(data).expect("render base64"));
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => panic!("the help sheet was never drawn"),
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
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(AttachServerFrame::Render { data, .. })) => {
                    screen.process(&BASE64.decode(data).expect("render base64"));
                    if screen.capture_text(None).contains("MARK") {
                        break true;
                    }
                    idle_frames += 1;
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break false,
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
            Duration::from_secs(3600),
            WELCOME_TOAST_DWELL,
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
        let deadline = tokio::time::Instant::now() + shux_testkit::BUDGET;
        let drew = loop {
            if screen.capture_text(None).contains("MARK") {
                break true;
            }
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(AttachServerFrame::Render { data, .. })) => {
                    screen.process(&BASE64.decode(data).expect("render base64"));
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break false,
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
            Duration::from_secs(3600),
            Duration::ZERO,
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
            &fixture.attached,
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
            graphics: false,
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
        let resized = tokio::time::timeout(shux_testkit::BUDGET, resize_rx.recv())
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
            let mut client = Client::attach_onboarded(&fixture, 90, onboarding.clone()).await;

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
                    row_offset: 0,
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
                    row_offset: 0,
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
                async move { send_frame(&out_tx, &session, 7, HashMap::new(), b"x").await },
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
            HashMap::from([(
                pane,
                PaneView {
                    rect: Rect::new(0, 0, 1, 1),
                    row_offset,
                    grid_rows: 1,
                    grid_cols: 1,
                },
            )])
        };
        let offset = |views: Option<&HashMap<PaneId, PaneView>>| views.map(|v| v[&pane].row_offset);
        let mut shown = ShownFrames::default();
        assert_eq!(offset(shown.views(Some(1))), None, "nothing sent yet");
        for n in 1..=20 {
            shown.push(n, view(n as usize));
        }
        assert_eq!(offset(shown.views(Some(12))), Some(12), "the stamped frame");
        assert_eq!(
            offset(shown.views(None)),
            Some(20),
            "an unstamped event: the latest"
        );
        assert_eq!(
            offset(shown.views(Some(2))),
            None,
            "a stamp older than any kept (5..=20) names nothing newer than it"
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
            row_offset: 0,
            anchor: (0, 0),
        };
        end_gesture_if_released(&mut g, ButtonAction::Release, ProtoMouseButton::Right);
        assert_eq!(
            g,
            SelectionDrag::App {
                pane_id: pane,
                buttons: 0b001,
                last: (3, 4),
                row_offset: 0,
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
            row_offset: 0,
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
            row_offset: 0,
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
        let shown = {
            let state = io_state.lock().await;
            state
                .vts
                .iter()
                .filter_map(|(pid, vt)| {
                    pane_rect_in(graph, &attached, content, viewport, *pid)
                        .map(|rect| (*pid, shux_ui::viewport::PaneView::of(rect, vt)))
                })
                .collect()
        };
        let mut s = session.lock().await;
        let next = s.shown.0.back().map_or(1, |(n, _)| n + 1);
        s.shown.push(next, shown);
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
        for _ in 0..16 {
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
        assert_eq!(route, AppMouse::Consumed { redraw: false });
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
        assert_eq!(outcome, AppMouse::Consumed { redraw: true });
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
        release_app_gesture(&h.fixture.io_state, &mut h.gesture).await;
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
        assert_eq!(outcome, AppMouse::Consumed { redraw: true });
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
        fixture.stop();
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
            &fixture.attached,
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
            &fixture.attached,
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
            Self::attach_onboarded(fixture, cols, onboarding).await
        }

        async fn attach_onboarded(
            fixture: &AttachFixture,
            cols: u16,
            onboarding: crate::onboarding::OnboardingHandle,
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
                graphics: false,
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
                AttachReady::Ok { .. }
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
            let deadline = tokio::time::Instant::now() + shux_testkit::BUDGET;
            while let Ok(Some(Ok(buf))) =
                tokio::time::timeout_at(deadline, self.framed.next()).await
            {
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
            let deadline = tokio::time::Instant::now() + shux_testkit::BUDGET;
            while let Ok(Some(Ok(buf))) =
                tokio::time::timeout_at(deadline, self.framed.next()).await
            {
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

    /// A press stamped with a frame no longer kept names nothing. It must not
    /// fall through to today's layout, where the same point may be a border
    /// and the drag that follows would resize the split.
    #[tokio::test(start_paused = true)]
    async fn a_press_from_a_frame_no_longer_kept_starts_no_border_drag() {
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
        assert_eq!(layout(&h), before, "an expired press dragged the border");
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
}
