//! One viewer's stream: its WebSocket on one side, a private read-only
//! attach on the other, and `shux.view.v1` between them.
//!
//! Its whole life runs under its cutoff: a revoke, kick, stop, replacement or
//! expiry interrupts whatever it awaits, setup and blocked writes included.
//! A write cut off midway closes the socket without a farewell, since a
//! farewell would first flush the rest of the cut-off terminal bytes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use shux_core::model::SessionId;
use shux_rpc::attach::{
    ATTACH_PROTOCOL_VERSION, AttachClientFrame, AttachHello, AttachReady, AttachServerFrame,
};
use tokio::time::Instant;
use tokio_util::codec::Framed;
use tokio_util::sync::CancellationToken;

use super::grants::{Admitted, EndReason};
use super::wire::{self, Ack, Credit, Inbound, Outbound, Queued};
use super::{Gateway, StreamGuard, TEARDOWN};

/// How long a viewer has to say its size, and the attach to answer.
const HANDSHAKE: Duration = Duration::from_secs(10);
/// A socket that takes no bytes for this long is dropped.
const WRITE_STALL: Duration = Duration::from_secs(10);
/// How often an idle stream checks its socket still drains, and its link's
/// expiry on the wall clock, which the monotonic one misses across a sleep.
const CHECK: Duration = Duration::from_secs(10);
/// At most one resize is applied per this, as the last one asked for.
const RESIZE_COALESCE: Duration = Duration::from_millis(250);
/// Messages a viewer may send per second, pings included; an ack that
/// completes a chunk it was sent is not counted. Past it, the viewer is
/// dropped.
const INBOUND_PER_SEC: u32 = 256;
/// The most an ended viewer's farewell may hold up its cutoff.
const FAREWELL: Duration = Duration::from_millis(100);
/// How long an ended stream waits for its attach to finish, once its socket
/// is closed.
const ATTACH_JOIN: Duration = Duration::from_secs(2);
const _: () = assert!(ATTACH_JOIN.as_millis() + FAREWELL.as_millis() < TEARDOWN.as_millis());
/// Close codes: the stream ended for a reason `ended` named, or the viewer
/// broke the protocol.
const CLOSE_ENDED: u16 = 4000;
const CLOSE_PROTOCOL: u16 = 4400;

enum Exit {
    Ended(EndReason),
    /// The browser went away, or broke the protocol.
    Viewer(&'static str),
    /// The attach went away without saying why.
    Attach,
}

/// Resolves at `deadline`; never without one.
#[expect(
    clippy::disallowed_methods,
    reason = "a stream's own deadlines (expiry, ack, resize); never synchronises a test"
)]
async fn at(deadline: Option<Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending().await,
    }
}

type Attach = Framed<tokio::io::DuplexStream, tokio_util::codec::LengthDelimitedCodec>;

async fn send_attach(attach: &mut Attach, frame: &AttachClientFrame) -> bool {
    match serde_json::to_vec(frame) {
        Ok(bytes) => attach.send(Bytes::from(bytes)).await.is_ok(),
        Err(_) => false,
    }
}

/// A write to the viewer's socket, bounded by [`WRITE_STALL`].
async fn socket_write<E>(write: impl Future<Output = Result<(), E>>) -> Result<(), Exit> {
    match tokio::time::timeout(WRITE_STALL, write).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(Exit::Viewer("closed")),
        Err(_) => Err(Exit::Ended(EndReason::Lagging)),
    }
}

/// Opens the private attach at `cols`x`rows` and checks it is the one asked
/// for: read-only and plain, to exactly this session. An administrator
/// removing it from the client registry drops this viewer's credential.
async fn open_attach(
    gw: &Gateway,
    admitted: &Admitted,
    (cols, rows): (u16, u16),
    cancel: &CancellationToken,
    task: &mut Option<tokio::task::JoinHandle<()>>,
) -> Option<Attach> {
    let (ours, theirs) = tokio::io::duplex(1 << 16);
    let gateway = std::sync::Arc::downgrade(&gw.inner);
    let viewer = admitted.viewer.clone();
    let on_remove = Box::new(move || {
        if let Some(inner) = gateway.upgrade() {
            let _ = super::lock(&inner.store).kick(&viewer);
        }
    });
    *task = Some(tokio::spawn({
        let deps = gw.inner.deps.clone();
        let (session, cancel) = (admitted.session, cancel.clone());
        async move {
            if let Err(e) =
                crate::attach::serve_read_only(theirs, deps, session, cancel, on_remove).await
            {
                tracing::debug!(error = %e, "share: attach ended");
            }
        }
    }));
    let mut attach = Framed::new(ours, shux_rpc::create_codec());
    let hello = AttachHello {
        protocol: ATTACH_PROTOCOL_VERSION,
        session_name: None,
        cols,
        rows,
        client_version: concat!("shux-share/", env!("CARGO_PKG_VERSION")).into(),
        graphics: false,
        plain: true,
        read_only: true,
        no_color: false,
    };
    let opened = async {
        let hello = serde_json::to_vec(&hello).ok()?;
        attach.send(Bytes::from(hello)).await.ok()?;
        let reply = attach.next().await?.ok()?;
        match serde_json::from_slice(&reply).ok()? {
            AttachReady::Ok {
                session_id,
                read_only: true,
                plain: true,
                ..
            } if session_id == admitted.session.to_string() => Some(()),
            _ => None,
        }
    };
    match tokio::time::timeout(HANDSHAKE, opened).await {
        Ok(Some(())) => Some(attach),
        _ => None,
    }
}

/// The grid the session's panes are laid out for, as a browser draws them
/// (no status bar): its driver's, else its last driver's, else the one that
/// shows its active window's panes whole, as the attach draws it then.
async fn layout_size(gw: &Gateway, session: SessionId) -> Option<wire::Size> {
    let border =
        shux_ui::BorderStyle::parse(&gw.inner.deps.config.current().appearance.border_style);
    let snap = gw.inner.deps.graph.snapshot();
    let state = gw.inner.deps.io_state.lock().await;
    let (cols, rows) = match state.clients.driver(session) {
        Some(d) => (d.cols, d.rows.saturating_sub(1)),
        None => match state.layout_basis.get(&session) {
            Some(&(cols, rows, _)) => (cols, rows.saturating_sub(1)),
            None => crate::snapshot::hostless_frame(&snap, &state, session, border)?.1,
        },
    };
    let (cols, rows) = wire::clamp(cols, rows);
    Some(wire::Size { cols, rows })
}

/// What a viewer asked for, held to the browser's bounds and the layout.
fn fit((cols, rows): (u16, u16), layout: Option<&wire::Size>) -> (u16, u16) {
    let (cols, rows) = wire::clamp(cols, rows);
    match layout {
        Some(l) => (cols.min(l.cols), rows.min(l.rows)),
        None => (cols, rows),
    }
}

/// Runs `work` until it ends or the stream's cutoff comes, whichever is
/// first: the stream's end, its link's expiry by either clock, or its
/// session's deletion.
async fn while_live(gw: &Gateway, admitted: &Admitted, work: impl Future<Output = Exit>) -> Exit {
    let exit = tokio::select! {
        biased;
        () = admitted.live.stop().cancelled() => Exit::Ended(EndReason::Stopped),
        () = expired(gw, admitted) => Exit::Ended(EndReason::Expired),
        () = session_gone(gw, admitted.session) => Exit::Ended(EndReason::SessionEnded),
        exit = work => exit,
    };
    match admitted.live.reason() {
        Some(why) => Exit::Ended(why),
        None => exit,
    }
}

/// Resolves once the stream's link has expired: rechecked at the monotonic
/// expiry, every [`CHECK`], and whenever the gateway's clock is moved.
async fn expired(gw: &Gateway, admitted: &Admitted) {
    loop {
        let moved = gw.inner.clock_moved.notified();
        if admitted.expires.passed(gw.now()) {
            return;
        }
        tokio::select! {
            () = at(Some(admitted.expires.mono.min(Instant::now() + CHECK))) => {}
            () = moved => {}
        }
    }
}

/// Resolves once `session` is gone from the graph; never on a daemon
/// without an event bus.
async fn session_gone(gw: &Gateway, session: SessionId) {
    let bus = gw.inner.deps.io_state.lock().await.event_bus.clone();
    let Some(bus) = bus else {
        return std::future::pending().await;
    };
    // Watched before the first look, so a deletion between them still wakes it.
    let mut killed = bus.watch_filtered(vec!["session.killed".into()]);
    while gw
        .inner
        .deps
        .graph
        .snapshot()
        .sessions
        .contains_key(&session)
    {
        if killed.changed().await.is_err() {
            return std::future::pending().await;
        }
    }
}

pub(super) async fn run(mut ws: WebSocket, gw: Gateway, admitted: Admitted, guard: StreamGuard) {
    let attach_cancel = gw.inner.shutdown.child_token();
    let _attach_guard = attach_cancel.clone().drop_guard();
    let mut task = None;
    // Set while terminal bytes are on their way to the socket.
    let writing = AtomicBool::new(false);
    let exit = while_live(
        &gw,
        &admitted,
        serve(&mut ws, &gw, &admitted, &attach_cancel, &mut task, &writing),
    )
    .await;
    let farewell = match exit {
        // The cut-off write's remaining bytes would go out first.
        _ if writing.load(Ordering::Relaxed) => None,
        Exit::Ended(why) => Some((
            Outbound::Ended { reason: why.code() }.text(),
            CloseFrame {
                code: CLOSE_ENDED,
                reason: why.code().into(),
            },
        )),
        Exit::Viewer(why) => Some((
            Outbound::Ended { reason: "protocol" }.text(),
            CloseFrame {
                code: CLOSE_PROTOCOL,
                reason: why.into(),
            },
        )),
        Exit::Attach => None,
    };
    if let Some((text, close)) = farewell {
        // Best effort, and never a long wait on a socket that has stopped
        // taking bytes.
        let _ = tokio::time::timeout(FAREWELL, async {
            ws.send(Message::Text(text.into())).await?;
            ws.send(Message::Close(Some(close))).await
        })
        .await;
    }
    // The cutoff: the socket closes first, then the slot and credential go.
    drop(ws);
    drop(guard);
    if matches!(
        exit,
        Exit::Ended(EndReason::Expired | EndReason::SessionEnded)
    ) {
        gw.prune();
    }
    attach_cancel.cancel();
    if let Some(mut task) = task
        && tokio::time::timeout(ATTACH_JOIN, &mut task).await.is_err()
    {
        task.abort();
    }
}

async fn serve(
    ws: &mut WebSocket,
    gw: &Gateway,
    admitted: &Admitted,
    attach_cancel: &CancellationToken,
    task: &mut Option<tokio::task::JoinHandle<()>>,
    writing: &AtomicBool,
) -> Exit {
    // The viewer's size, before anything else.
    let asked = match tokio::time::timeout(HANDSHAKE, ws.next()).await {
        Ok(Some(Ok(Message::Text(t)))) => match Inbound::parse(&t) {
            Some(Inbound::Hello { cols, rows }) => (cols, rows),
            _ => return Exit::Viewer("hello expected"),
        },
        _ => return Exit::Viewer("no hello"),
    };
    // Subscribed before the layout is read, so no change between is missed.
    let mut presence = gw.inner.deps.io_state.lock().await.clients.presence();
    let mut host = layout_size(gw, admitted.session).await;
    let mut size = fit(asked, host.as_ref());
    let Some(mut attach) = open_attach(gw, admitted, size, attach_cancel, task).await else {
        return Exit::Attach;
    };
    let config = gw.inner.deps.config.current();
    let ready = Outbound::Ready {
        v: wire::PROTOCOL,
        session: gw
            .inner
            .deps
            .graph
            .snapshot()
            .sessions
            .get(&admitted.session)
            .map(|s| s.name.clone())
            .unwrap_or_default(),
        size: wire::Size {
            cols: size.0,
            rows: size.1,
        },
        host: host.clone(),
        palette: wire::Palette::of(&shux_core::theme::Theme::resolve(&config.theme)),
        expires_at: super::rfc3339(admitted.expires.wall),
        expires_in_ms: admitted.expires.remaining_ms(gw.now()),
        max: wire::Size {
            cols: wire::MAX_COLS,
            rows: wire::MAX_ROWS,
        },
    };
    if let Err(exit) = socket_write(ws.send(Message::Text(ready.text().into()))).await {
        return exit;
    }

    let mut credit = Credit::default();
    let mut pending_resize: Option<(u16, u16)> = None;
    let mut resize_at: Option<Instant> = None;
    let mut last_progress = Instant::now();
    let mut next_check = Instant::now() + CHECK;
    let (mut inbound_since, mut inbound) = (Instant::now(), 0u32);
    loop {
        // Everything the window allows goes out before the next event.
        loop {
            let idle = credit.in_flight() == 0;
            let Some(q) = credit.pop() else { break };
            // The deadline runs from the first byte awaiting an ack.
            if idle && credit.in_flight() > 0 {
                last_progress = Instant::now();
            }
            let (message, bytes) = match q {
                Queued::Bytes(b) => {
                    let n = b.len() as u64 - 8;
                    (Message::Binary(b.into()), n)
                }
                Queued::Text(t) => (Message::Text(t.into()), 0),
            };
            writing.store(bytes > 0, Ordering::Relaxed);
            if let Err(exit) = socket_write(ws.send(message)).await {
                return exit;
            }
            writing.store(false, Ordering::Relaxed);
            gw.inner.sent.fetch_add(bytes, Ordering::Relaxed);
        }
        let ack_due = (credit.in_flight() > 0).then(|| last_progress + wire::ACK_DEADLINE);
        tokio::select! {
            biased;
            () = at(Some(next_check)) => {
                // Whatever the socket holds unsent (a pong the viewer never
                // read included) must drain, or the viewer is lagging.
                if let Err(exit) = socket_write(ws.flush()).await {
                    return exit;
                }
                next_check = Instant::now() + CHECK;
            }
            () = at(ack_due) => return Exit::Ended(EndReason::Lagging),
            () = at(resize_at) => {
                resize_at = None;
                if let Some(want) = pending_resize.take().map(|w| fit(w, host.as_ref())).filter(|&w| w != size) {
                    size = want;
                    let frame = AttachClientFrame::Resize { cols: want.0, rows: want.1 };
                    if !send_attach(&mut attach, &frame).await {
                        return Exit::Attach;
                    }
                }
            }
            // The viewer before the attach: a busy pane cannot starve its
            // acks, nor delay seeing that it is gone.
            message = ws.next() => {
                let now = Instant::now();
                if now.duration_since(inbound_since) >= Duration::from_secs(1) {
                    (inbound_since, inbound) = (now, 0);
                }
                tokio::task::consume_budget().await;
                let counted = match message {
                    Some(Ok(Message::Text(text))) => match Inbound::parse(&text) {
                        Some(Inbound::Ack { offset }) => match credit.ack(offset) {
                            Some(Ack::Chunk) => {
                                last_progress = now;
                                false
                            }
                            Some(Ack::Partial) => {
                                last_progress = now;
                                true
                            }
                            Some(Ack::Duplicate) => true,
                            None => return Exit::Viewer("bad ack"),
                        },
                        Some(Inbound::Resize { cols, rows }) => {
                            pending_resize = Some((cols, rows));
                            resize_at.get_or_insert_with(|| Instant::now() + RESIZE_COALESCE);
                            true
                        }
                        Some(Inbound::Hello { .. }) | None => return Exit::Viewer("unexpected message"),
                    },
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => true,
                    Some(Ok(Message::Binary(_))) => return Exit::Viewer("binary from the viewer"),
                    Some(Ok(Message::Close(_)) | Err(_)) | None => return Exit::Viewer("closed"),
                };
                inbound += u32::from(counted);
                if inbound > INBOUND_PER_SEC {
                    return Exit::Viewer("too many messages");
                }
            }
            changed = presence.changed() => {
                if changed.is_err() {
                    return Exit::Attach;
                }
                let now = layout_size(gw, admitted.session).await;
                if now != host {
                    host = now;
                    if let Some(h) = &host {
                        credit.push_text(&Outbound::Host { cols: h.cols, rows: h.rows });
                        // A layout that shrank shrinks this grid with it.
                        if fit(size, host.as_ref()) != size {
                            pending_resize.get_or_insert(size);
                            resize_at.get_or_insert_with(|| Instant::now() + RESIZE_COALESCE);
                        }
                    }
                }
            }
            frame = attach.next() => {
                let Some(Ok(buf)) = frame else { return Exit::Attach };
                let Ok(frame) = serde_json::from_slice::<AttachServerFrame>(&buf) else { continue };
                match frame {
                    AttachServerFrame::Render { data, .. } => {
                        let Ok(bytes) = BASE64.decode(data.as_bytes()) else { continue };
                        if !credit.push_bytes(&bytes) {
                            return Exit::Ended(EndReason::Lagging);
                        }
                    }
                    AttachServerFrame::Resized { cols, rows } => {
                        credit.push_text(&Outbound::Resize { cols, rows, at: credit.position() });
                    }
                    AttachServerFrame::Ping => {
                        if !send_attach(&mut attach, &AttachClientFrame::Pong).await {
                            return Exit::Attach;
                        }
                    }
                    AttachServerFrame::SessionEnded { .. } => {
                        gw.prune();
                        return Exit::Ended(EndReason::SessionEnded);
                    }
                    AttachServerFrame::Bell
                    | AttachServerFrame::Notice { .. }
                    | AttachServerFrame::DetachAck => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::share::grants::{Admit, CreateOpts, Now};
    use std::sync::atomic::AtomicU64;
    use std::time::SystemTime;

    static WALL: AtomicU64 = AtomicU64::new(1 << 30);

    /// The real monotonic clock, and a wall clock the test moves.
    fn moved_wall() -> Now {
        Now {
            mono: Instant::now(),
            wall: SystemTime::UNIX_EPOCH + Duration::from_secs(WALL.load(Ordering::SeqCst)),
        }
    }

    fn admitted(gw: &Gateway) -> Admitted {
        let now = gw.now();
        let mut store = gw.store();
        let minted = store
            .create(CreateOpts::new(SessionId::new()), now)
            .unwrap();
        let cookie = store
            .redeem(&minted.id, &minted.secret, now)
            .unwrap()
            .cookie;
        let Admit::Ready(a) = store.admit(&minted.id, &cookie, now) else {
            panic!("not admitted");
        };
        a
    }

    /// Only the wall clock passes the link's end, as across a laptop's
    /// sleep: the stream ends the moment it is told the clock moved, with
    /// no monotonic time passing at all.
    #[tokio::test(start_paused = true)]
    async fn a_stream_ends_when_only_the_wall_clock_passes_its_link() {
        let gw = super::super::test_gateway(moved_wall);
        let admitted = admitted(&gw);
        let mut ending = Box::pin(while_live(&gw, &admitted, std::future::pending()));
        assert!(futures::poll!(&mut ending).is_pending());
        WALL.fetch_add(
            super::super::grants::DEFAULT_TTL.as_secs(),
            Ordering::SeqCst,
        );
        gw.inner.clock_moved.notify_waiters();
        assert!(matches!(
            futures::poll!(&mut ending),
            std::task::Poll::Ready(Exit::Ended(EndReason::Expired))
        ));
    }

    /// A viewer that pings and never reads: what it is owed stops growing at
    /// the socket's write-buffer bound (tungstenite drops pongs past it),
    /// and the idle check's flush of it ends the viewer as lagging. Neither
    /// waits on the inbound limit.
    #[tokio::test(start_paused = true)]
    async fn owed_pongs_stay_within_the_write_buffer_and_a_stuck_flush_ends_the_viewer() {
        use tokio::io::AsyncWriteExt as _;
        use tokio_tungstenite::tungstenite::protocol::Role;
        let (server_io, mut client_io) = tokio::io::duplex(256);
        let mut server = tokio_tungstenite::WebSocketStream::from_raw_socket(
            server_io,
            Role::Server,
            Some(super::super::http::ws_config()),
        )
        .await;
        let mut ping = vec![0x89, 0x80 | 125, 0, 0, 0, 0];
        ping.extend_from_slice(&[b'p'; 125]);
        // Far more pings than the pipe holds pongs: none of them read.
        let pinger = tokio::spawn(async move {
            for _ in 0..10_000 {
                if client_io.write_all(&ping).await.is_err() {
                    break;
                }
            }
            client_io
        });
        for _ in 0..10_000 {
            assert!(
                matches!(
                    server.next().await,
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Ping(_)))
                ),
                "owed pongs grew until reading failed"
            );
        }
        let mut client_io = pinger.await.expect("pinger");
        assert!(matches!(
            socket_write(server.flush()).await,
            Err(Exit::Ended(EndReason::Lagging))
        ));
        // The viewer reads at last: everything owed reaches it, and it is
        // the pipe's worth and one more, not a pong per ping.
        use tokio::io::AsyncReadExt as _;
        let reader = tokio::spawn(async move {
            let mut owed = Vec::new();
            let _ = client_io.read_to_end(&mut owed).await;
            owed
        });
        server.flush().await.expect("the owed pong drains");
        drop(server);
        let owed = reader.await.expect("reader");
        let pongs = owed.chunks(127).filter(|f| f[0] == 0x8a).count();
        assert_eq!(owed.len() % 127, 0, "pongs of 125 bytes, unmasked");
        let bound = super::super::http::MAX_WRITE_BUFFER + 256 + 127;
        assert!(
            owed.len() <= bound,
            "{pongs} pongs ({} bytes) were owed for 10000 pings; the bound is {bound}",
            owed.len()
        );
    }

    /// A write blocked on the socket yields to the cutoff at once.
    #[tokio::test(start_paused = true)]
    async fn a_blocked_write_yields_to_the_cutoff() {
        let gw = super::super::test_gateway(Now::current);
        let admitted = admitted(&gw);
        let blocked = async {
            match socket_write(std::future::pending::<Result<(), ()>>()).await {
                Err(exit) => exit,
                Ok(()) => Exit::Attach,
            }
        };
        let mut work = Box::pin(while_live(&gw, &admitted, blocked));
        assert!(futures::poll!(&mut work).is_pending());
        gw.store().kick(&admitted.viewer).unwrap();
        assert!(matches!(
            futures::poll!(&mut work),
            std::task::Poll::Ready(Exit::Ended(EndReason::Kicked))
        ));
    }
}
