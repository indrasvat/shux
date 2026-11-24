//! One viewer's stream: its WebSocket on one side, a private read-only
//! attach on the other, and `shux.view.v1` between them.

use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use shux_rpc::attach::{
    ATTACH_PROTOCOL_VERSION, AttachClientFrame, AttachHello, AttachReady, AttachServerFrame,
};
use tokio::time::Instant;
use tokio_util::codec::Framed;

use super::grants::{Admitted, EndReason};
use super::wire::{self, Credit, Inbound, Lag, Outbound, Queued};
use super::{Gateway, StreamGuard};

/// How long a viewer has to say its size, and the attach to answer.
const HANDSHAKE: Duration = Duration::from_secs(10);
/// A socket that takes no bytes for this long is dropped.
const WRITE_STALL: Duration = Duration::from_secs(10);
/// Resizes closer together than this are applied as their last.
const RESIZE_COALESCE: Duration = Duration::from_millis(50);
/// The most an ended viewer's farewell may hold up its cutoff.
const FAREWELL: Duration = Duration::from_millis(100);
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

/// Opens the private attach at `cols`x`rows` and checks it is the one asked
/// for: read-only and plain, to exactly this session.
async fn open_attach(
    gw: &Gateway,
    admitted: &Admitted,
    cols: u16,
    rows: u16,
) -> Option<(
    Attach,
    tokio::task::JoinHandle<()>,
    tokio_util::sync::CancellationToken,
)> {
    let (ours, theirs) = tokio::io::duplex(1 << 16);
    let cancel = gw.inner.shutdown.child_token();
    let task = tokio::spawn({
        let deps = gw.inner.deps.clone();
        let (session, cancel) = (admitted.session, cancel.clone());
        async move {
            if let Err(e) = crate::attach::serve_read_only(theirs, deps, session, cancel).await {
                tracing::debug!(error = %e, "share: attach ended");
            }
        }
    });
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
        Ok(Some(())) => Some((attach, task, cancel)),
        _ => {
            cancel.cancel();
            None
        }
    }
}

/// The host's layout, as the panes are drawn: its rows less the status bar.
fn host_size(clients: &crate::clients::ClientRegistry, admitted: &Admitted) -> Option<wire::Size> {
    clients.driver(admitted.session).map(|d| wire::Size {
        cols: d.cols,
        rows: d.rows.saturating_sub(1).max(1),
    })
}

pub(super) async fn run(mut ws: WebSocket, gw: Gateway, admitted: Admitted, guard: StreamGuard) {
    let exit = serve(&mut ws, &gw, &admitted).await;
    let farewell = match exit {
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
        // Best effort, and never a wait on a socket that has stopped taking
        // bytes: the cutoff is the socket's drop, below.
        let _ = tokio::time::timeout(FAREWELL, async {
            ws.send(Message::Text(text.into())).await?;
            ws.send(Message::Close(Some(close))).await
        })
        .await;
    }
    drop(ws);
    drop(guard);
}

async fn serve(ws: &mut WebSocket, gw: &Gateway, admitted: &Admitted) -> Exit {
    let stop = admitted.live.stop().clone();
    let ended =
        |live: &super::grants::Live| Exit::Ended(live.reason().unwrap_or(EndReason::Stopped));
    // The viewer's size, before anything else.
    let first = tokio::select! {
        biased;
        () = stop.cancelled() => return ended(&admitted.live),
        m = tokio::time::timeout(HANDSHAKE, ws.next()) => m,
    };
    let (cols, rows) = match first {
        Ok(Some(Ok(Message::Text(t)))) => match Inbound::parse(&t) {
            Some(Inbound::Hello { cols, rows }) => wire::clamp(cols, rows),
            _ => return Exit::Viewer("hello expected"),
        },
        _ => return Exit::Viewer("no hello"),
    };
    let Some((mut attach, task, attach_cancel)) = open_attach(gw, admitted, cols, rows).await
    else {
        return Exit::Attach;
    };
    let _attach_guard = attach_cancel.clone().drop_guard();
    let clients = gw.inner.deps.io_state.lock().await.clients.clone();
    let mut presence = clients.presence();
    let mut host = host_size(&clients, admitted);
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
        size: wire::Size { cols, rows },
        host: host.clone(),
        palette: wire::Palette::of(&shux_core::theme::Theme::resolve(&config.theme)),
        expires_at: super::rfc3339(admitted.expires_wall),
        expires_in_ms: admitted
            .expires
            .saturating_duration_since(Instant::now())
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX),
        chunk: wire::CHUNK,
        window: wire::WINDOW,
        max: wire::Size {
            cols: wire::MAX_COLS,
            rows: wire::MAX_ROWS,
        },
    };
    if ws.send(Message::Text(ready.text().into())).await.is_err() {
        return Exit::Viewer("closed");
    }

    let mut credit = Credit::default();
    let mut size = (cols, rows);
    let mut pending_resize: Option<(u16, u16)> = None;
    let mut resize_at: Option<Instant> = None;
    let mut last_progress = Instant::now();
    let exit = 'run: loop {
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
            tokio::select! {
                biased;
                () = stop.cancelled() => break 'run ended(&admitted.live),
                sent = tokio::time::timeout(WRITE_STALL, ws.send(message)) => match sent {
                    Ok(Ok(())) => gw.inner.count_sent(bytes),
                    Ok(Err(_)) => break 'run Exit::Viewer("closed"),
                    Err(_) => break 'run Exit::Ended(EndReason::Lagging),
                },
            }
        }
        let ack_due = (credit.in_flight() > 0).then(|| last_progress + wire::ACK_DEADLINE);
        tokio::select! {
            biased;
            () = stop.cancelled() => break ended(&admitted.live),
            () = at(Some(admitted.expires)) => {
                gw.sweep();
                break Exit::Ended(EndReason::Expired);
            }
            () = at(ack_due) => break Exit::Ended(EndReason::Lagging),
            () = at(resize_at) => {
                resize_at = None;
                if let Some(want) = pending_resize.take().filter(|&w| w != size) {
                    size = want;
                    let frame = AttachClientFrame::Resize { cols: want.0, rows: want.1 };
                    if !send_attach(&mut attach, &frame).await {
                        break Exit::Attach;
                    }
                }
            }
            changed = presence.changed() => {
                if changed.is_err() {
                    break Exit::Attach;
                }
                let now = host_size(&clients, admitted);
                if now != host {
                    host = now;
                    if let Some(h) = &host {
                        credit.push_text(&Outbound::Host { cols: h.cols, rows: h.rows });
                    }
                }
            }
            frame = attach.next() => {
                let Some(Ok(buf)) = frame else { break Exit::Attach };
                let Ok(frame) = serde_json::from_slice::<AttachServerFrame>(&buf) else { continue };
                match frame {
                    AttachServerFrame::Render { data, .. } => {
                        let Ok(bytes) = BASE64.decode(data.as_bytes()) else { continue };
                        if credit.push_bytes(&bytes) == Err(Lag::Backlog) {
                            break Exit::Ended(EndReason::Lagging);
                        }
                    }
                    AttachServerFrame::Resized { cols, rows } => {
                        credit.push_text(&Outbound::Resize { cols, rows, at: credit.position() });
                    }
                    AttachServerFrame::Ping => {
                        if !send_attach(&mut attach, &AttachClientFrame::Pong).await {
                            break Exit::Attach;
                        }
                    }
                    AttachServerFrame::SessionEnded { .. } => {
                        gw.prune();
                        break Exit::Ended(admitted.live.reason().unwrap_or(EndReason::SessionEnded));
                    }
                    AttachServerFrame::Bell
                    | AttachServerFrame::Notice { .. }
                    | AttachServerFrame::DetachAck => {}
                }
            }
            message = ws.next() => {
                let text = match message {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                    Some(Ok(Message::Binary(_))) => break Exit::Viewer("binary from the viewer"),
                    Some(Ok(Message::Close(_)) | Err(_)) | None => break Exit::Viewer("closed"),
                };
                match Inbound::parse(&text) {
                    Some(Inbound::Ack { offset }) => match credit.ack(offset) {
                        Ok(true) => last_progress = Instant::now(),
                        Ok(false) => {}
                        Err(_) => break Exit::Viewer("bad ack"),
                    },
                    Some(Inbound::Resize { cols, rows }) => {
                        pending_resize = Some(wire::clamp(cols, rows));
                        resize_at.get_or_insert_with(|| Instant::now() + RESIZE_COALESCE);
                    }
                    Some(Inbound::Hello { .. }) | None => break Exit::Viewer("unexpected message"),
                }
            }
        }
    };
    let _ = send_attach(&mut attach, &AttachClientFrame::Detach).await;
    attach_cancel.cancel();
    drop(attach);
    let _ = tokio::time::timeout(HANDSHAKE, task).await;
    exit
}
