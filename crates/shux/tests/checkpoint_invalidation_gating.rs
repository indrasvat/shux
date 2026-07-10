//! P4 convergence round 1 — checkpoint-invalidation gating (task 077).
//!
//! Claude blocker: the PTY resize branch invalidated checkpoints
//! UNCONDITIONALLY, so no-op resizes (same-size `pane.set_size`, and the
//! attach render loop's `apply_resize_to_window` re-fan on attach / window
//! switch / zoom at an unchanged client size) destroyed every checkpoint on
//! panes whose dimensions never changed. Only an ACTUAL dimension change is
//! Class-A "pane resize" (§4.2) and only that may invalidate (LENS-R-032).
//!
//! These tests were written RED-FIRST against the unfixed code (both diffs
//! below then failed with RESIZE_INVALIDATED -32011) and flip green with the
//! dims-gated invalidation.
//!
//! Codex major (adjudicated, PRD §7.3 amended): the -32011 payload keeps
//! `{requested, invalidated_at, hint}` — `error_wire_shapes_pinned` pins the
//! exact field sets of both diff errors so the wire contract cannot drift.
//!
//! Claude minor: the window-switch test drives a REAL attach client — the
//! actual daemon-side attach handshake + streaming protocol over
//! `attach.sock` (thinnest headless client: framed AttachHello/AttachReady,
//! Action frames for window switching, Input frames for keystrokes, a drain
//! task for render frames) — not synthetic snapshot/glance readers. The diff
//! delta is asserted exact WHILE the client is attached.
//!
//! NOT part of the frozen lens red suite (§16.2 freezes `lens_*` files; this
//! is implementation-owned regression coverage). Reuses the frozen
//! `lens_common` harness READ-ONLY via `#[path]` — no frozen file is
//! modified.

#[path = "lens_common/mod.rs"]
mod lens_common;

use std::time::Duration;

use lens_common::{Harness, unique, wait_until};

fn checkpoint(h: &Harness, pane: &str, ctx: &str) -> u64 {
    h.rpc_raw("pane.checkpoint", serde_json::json!({ "pane_id": pane }))
        .expect_result(ctx)["revision"]
        .as_u64()
        .expect("checkpoint revision")
}

fn settle(h: &Harness, pane: &str, ctx: &str) {
    let env = h.rpc_raw(
        "pane.wait_settled",
        serde_json::json!({ "pane_id": pane, "quiet_ms": 300, "timeout_ms": 5_000 }),
    );
    assert_eq!(
        env.expect_result(ctx)["settled"],
        serde_json::Value::Bool(true),
        "{ctx}: expected settle"
    );
}

/// Glance-derived (cols, rows) of a pane.
fn pane_dims(h: &Harness, pane: &str) -> (u64, u64) {
    let g = h
        .rpc_raw(
            "pane.glance",
            serde_json::json!({ "pane_id": pane, "include_png": false }),
        )
        .expect_result("pane dims glance");
    (
        g["cols"].as_u64().expect("cols"),
        g["rows"].as_u64().expect("rows"),
    )
}

/// The glance-text character at grid (row, col) — full-width byte-stable
/// rows make this a positional read.
fn glance_char_at(h: &Harness, pane: &str, row: usize, col: usize) -> Option<char> {
    let g = h
        .rpc_raw(
            "pane.glance",
            serde_json::json!({ "pane_id": pane, "include_png": false }),
        )
        .expect_result("glance char probe");
    g["text"]
        .as_str()
        .and_then(|t| t.lines().nth(row))
        .and_then(|l| l.chars().nth(col))
}

// Claude blocker, repro (i): a SAME-SIZE `pane.set_size` (synchronous RPC —
// the ack guarantees the resize branch ran before it returns) must NOT
// invalidate checkpoints. Red on unfixed code: diff → -32011.
#[test]
fn same_size_set_size_preserves_checkpoints() {
    let h = Harness::new();
    let f = h.launch_fixture("f4_keys.sh", 80, 24, "LENS-F4-KEYS");

    let r = checkpoint(&h, &f.pane_id, "checkpoint before same-size set_size");

    // Same dims the fixture was launched at — a no-op resize.
    h.rpc_ok(
        "pane.set_size",
        serde_json::json!({ "pane_id": f.pane_id, "cols": 80, "rows": 24 }),
    );

    // The checkpoint must survive: zero-delta diff, not RESIZE_INVALIDATED.
    let env = h.rpc_raw(
        "pane.diff_since",
        serde_json::json!({ "pane_id": f.pane_id, "since_revision": r }),
    );
    let d = env.expect_result("diff after same-size set_size");
    assert_eq!(
        d["cells_changed"], 0,
        "no-op resize must not change content"
    );
    assert_eq!(d["from_revision"], r, "from_revision");

    // And the checkpoint still yields an exact delta for a real change.
    h.send_raw(&f.pane_id, "a");
    settle(&h, &f.pane_id, "settle after a");
    let d = h
        .rpc_raw(
            "pane.diff_since",
            serde_json::json!({ "pane_id": f.pane_id, "since_revision": r }),
        )
        .expect_result("diff after a");
    assert_eq!(d["cells_changed"], 10, "exact F4 delta after `a`");

    h.kill_session(&f.session_id);
}

// ── Thin REAL attach client (daemon-side handshake + streaming frames) ────

/// Commands the test thread sends to the attach-client thread.
enum AttachCmd {
    Action(shux_rpc::attach::ActionKind, Option<u16>),
    /// Raw input bytes, base64-encoded (the wire format of `Input.data`).
    Input(String),
    Detach,
}

/// A REAL attach client on a dedicated thread: performs the framed
/// AttachHello handshake against `attach.sock`, then continuously DRAINS
/// server frames (render output — keeping the daemon's writer from
/// backpressuring) while forwarding test-issued frames in order.
struct AttachClient {
    cmd_tx: tokio::sync::mpsc::Sender<AttachCmd>,
    joined: Option<std::thread::JoinHandle<()>>,
}

impl AttachClient {
    fn connect(socket: std::path::PathBuf, session_name: String) -> Self {
        let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel::<AttachCmd>(16);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();

        let joined = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("attach client runtime");
            rt.block_on(async move {
                use futures::{SinkExt, StreamExt};

                let stream = match tokio::net::UnixStream::connect(&socket).await {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = ready_tx.send(Err(format!("connect {}: {e}", socket.display())));
                        return;
                    }
                };
                let mut framed =
                    tokio_util::codec::Framed::new(stream, shux_rpc::codec::create_codec());

                // Handshake: Hello → Ready. 100×30 client terminal.
                let hello = shux_rpc::attach::AttachHello {
                    protocol: shux_rpc::attach::ATTACH_PROTOCOL_VERSION,
                    session_name: Some(session_name),
                    cols: 100,
                    rows: 30,
                    client_version: "p4-gating-test".into(),
                };
                let payload = serde_json::to_vec(&hello).expect("serialize hello");
                if let Err(e) = framed.send(bytes::Bytes::from(payload)).await {
                    let _ = ready_tx.send(Err(format!("send hello: {e}")));
                    return;
                }
                let first = match framed.next().await {
                    Some(Ok(b)) => b,
                    other => {
                        let _ = ready_tx.send(Err(format!("no ready frame: {other:?}")));
                        return;
                    }
                };
                match serde_json::from_slice::<shux_rpc::attach::AttachReady>(&first) {
                    Ok(shux_rpc::attach::AttachReady::Ok { .. }) => {
                        let _ = ready_tx.send(Ok(()));
                    }
                    other => {
                        let _ = ready_tx.send(Err(format!("handshake rejected: {other:?}")));
                        return;
                    }
                }

                // Streaming: drain server frames; forward commands in order.
                loop {
                    tokio::select! {
                        frame = framed.next() => {
                            match frame {
                                Some(Ok(_)) => {} // render/ping/etc. — discard
                                _ => break,       // connection closed
                            }
                        }
                        cmd = cmd_rx.recv() => {
                            let out = match cmd {
                                Some(AttachCmd::Action(kind, window_index)) => {
                                    shux_rpc::attach::AttachClientFrame::Action {
                                        kind,
                                        args: shux_rpc::attach::ActionArgs {
                                            name: None,
                                            window_index,
                                        },
                                    }
                                }
                                Some(AttachCmd::Input(data)) => {
                                    shux_rpc::attach::AttachClientFrame::Input { data }
                                }
                                Some(AttachCmd::Detach) => {
                                    let payload = serde_json::to_vec(
                                        &shux_rpc::attach::AttachClientFrame::Detach,
                                    )
                                    .expect("serialize detach");
                                    let _ = framed.send(bytes::Bytes::from(payload)).await;
                                    break;
                                }
                                None => break,
                            };
                            let payload = serde_json::to_vec(&out).expect("serialize frame");
                            if framed.send(bytes::Bytes::from(payload)).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        });

        ready_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("attach handshake result within 10s")
            .expect("attach handshake must succeed");

        Self {
            cmd_tx,
            joined: Some(joined),
        }
    }

    fn send(&self, cmd: AttachCmd) {
        self.cmd_tx
            .blocking_send(cmd)
            .expect("attach client thread alive");
    }

    fn detach(mut self) {
        let _ = self.cmd_tx.blocking_send(AttachCmd::Detach);
        if let Some(j) = self.joined.take() {
            let _ = j.join();
        }
    }
}

// Claude blocker, repro (ii) + claude minor folded in: a REAL attached
// client switching windows away and back re-fans the window's panes at the
// SAME client size (`apply_resize_to_window` fires on every layout action) —
// the checkpoint must survive and the diff must stay EXACT while the client
// is attached. Red on unfixed code: the same-size re-fan invalidated →
// diff -32011.
#[test]
fn real_attach_window_switch_preserves_checkpoints_and_diff_exact() {
    use shux_rpc::attach::ActionKind;

    let h = Harness::new();

    // Manual session (launch_fixture generates an opaque name; the attach
    // hello needs it): create → size → exec F4 → sentinel.
    let session_name = format!("attach-gate-{}", unique());
    let created = h.rpc_ok(
        "session.create",
        serde_json::json!({
            "name": session_name,
            "cwd": h.repo_root().display().to_string(),
        }),
    );
    let session_id = created["id"].as_str().expect("session id").to_string();
    let pane_id = created["pane_id"].as_str().expect("pane id").to_string();
    h.rpc_ok(
        "pane.set_size",
        serde_json::json!({ "pane_id": pane_id, "cols": 80, "rows": 24 }),
    );
    let abs = h.fixture_abs("f4_keys.sh");
    h.rpc_ok(
        "pane.send_keys",
        serde_json::json!({ "pane_id": pane_id, "text": format!("exec sh {abs}\n") }),
    );
    h.wait_for(&pane_id, "LENS-F4-KEYS", 10_000)
        .expect("F4 sentinel");

    // A second window to switch away to.
    h.rpc_ok(
        "window.create",
        serde_json::json!({ "session_id": session_id }),
    );

    // REAL attach (100×30 client). Make the F4 window active; its pane gets
    // re-fanned to the attach-computed rect — a REAL resize (legitimate
    // invalidation; nothing is checkpointed yet).
    let attach_sock = h.runtime_dir().join("shux").join("attach.sock");
    let client = AttachClient::connect(attach_sock, session_name);
    client.send(AttachCmd::Action(ActionKind::SwitchToWindow, Some(1)));
    assert!(
        wait_until(Duration::from_secs(10), || pane_dims(&h, &pane_id)
            != (80, 24)),
        "attach re-fan must resize the F4 pane away from 80x24"
    );
    settle(&h, &pane_id, "settle after attach re-fan");

    // Checkpoint the post-attach frame.
    let r = checkpoint(&h, &pane_id, "checkpoint while attached");

    // Switch away and back (each switch re-fans the newly-active window at
    // the SAME 100×30 client size → same pane dims → must NOT invalidate),
    // then Tab in-band. The attach loop handles client frames in order and
    // awaits apply_resize_to_window inline, so once Tab's 2-cell marker move
    // is visible the switch-back re-fan requests are already queued.
    client.send(AttachCmd::Action(ActionKind::SwitchToWindow, Some(2)));
    client.send(AttachCmd::Action(ActionKind::SwitchToWindow, Some(1)));
    client.send(AttachCmd::Input(base64_encode(b"\t")));
    assert!(
        wait_until(Duration::from_secs(10), || {
            glance_char_at(&h, &pane_id, 8, 25) == Some('▶')
        }),
        "Tab marker move (grid (8,25)) must land after the window switches"
    );

    // Flush the pane's resize channel: a synchronous same-size set_size is
    // queued BEHIND the switch-back re-fan on the same channel and its ack
    // only fires after the PTY task processed everything before it. After
    // this, the (buggy) invalidation would have landed — the diff below is
    // deterministic in both directions.
    let (cols, rows) = pane_dims(&h, &pane_id);
    h.rpc_ok(
        "pane.set_size",
        serde_json::json!({ "pane_id": pane_id, "cols": cols, "rows": rows }),
    );
    settle(&h, &pane_id, "settle after switches + Tab");

    // The checkpoint must survive the away/back re-fans: exact 2-cell Tab
    // delta (old marker cell cleared at (8,5), new marker drawn at (8,25)).
    let d = h
        .rpc_raw(
            "pane.diff_since",
            serde_json::json!({ "pane_id": pane_id, "since_revision": r }),
        )
        .expect_result("diff after window switches (while attached)");
    assert_eq!(
        d["cells_changed"], 2,
        "checkpoint survived the same-size re-fans; Tab moved exactly 2 cells"
    );
    assert_eq!(
        d["regions"],
        serde_json::json!([
            { "row": 8, "col_start": 5, "col_end": 6 },
            { "row": 8, "col_start": 25, "col_end": 26 },
        ]),
        "exact Tab regions while attached"
    );

    // Claude minor folded in: drive `a` through the REAL attach input path
    // and assert the accumulated delta stays exact while still attached.
    client.send(AttachCmd::Input(base64_encode(b"a")));
    h.wait_for(&pane_id, "A-PRESSED", 10_000)
        .expect("A-PRESSED after in-band `a`");
    settle(&h, &pane_id, "settle after in-band a");
    let d = h
        .rpc_raw(
            "pane.diff_since",
            serde_json::json!({ "pane_id": pane_id, "since_revision": r }),
        )
        .expect_result("diff after in-band a (while attached)");
    assert_eq!(
        d["cells_changed"], 12,
        "Tab (2 cells) + `a` (10 cells) — exact accumulated delta"
    );
    assert_eq!(
        d["regions"],
        serde_json::json!([
            { "row": 2, "col_start": 2, "col_end": 3 },
            { "row": 5, "col_start": 10, "col_end": 19 },
            { "row": 8, "col_start": 5, "col_end": 6 },
            { "row": 8, "col_start": 25, "col_end": 26 },
        ]),
        "exact accumulated regions while attached"
    );

    client.detach();
    h.kill_session(&session_id);
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

// Codex major (ADJUDICATED — PRD §7.3 amended): pin the exact wire shape of
// both diff error payloads so the agent-first contract cannot drift.
// -32011 carries EXACTLY {requested, invalidated_at, hint};
// -32010 carries EXACTLY {requested, available}.
#[test]
fn error_wire_shapes_pinned() {
    let h = Harness::new();
    let f = h.launch_fixture("f1_static.sh", 80, 24, "दृश्यते");

    // ── -32010 STALE_REVISION: {requested, available} exactly ──
    let c = checkpoint(&h, &f.pane_id, "wire-shape checkpoint");
    let err = h
        .rpc_raw(
            "pane.diff_since",
            serde_json::json!({ "pane_id": f.pane_id, "since_revision": c + 1 }),
        )
        .expect_error_code(-32010, "stale wire shape");
    let data = err.data.as_ref().expect("-32010 carries data");
    let mut keys: Vec<&str> = data
        .as_object()
        .expect("-32010 data is an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["available", "requested"],
        "-32010 data fields are EXACTLY {{requested, available}}"
    );
    assert_eq!(data["requested"], c + 1, "-32010 requested value");
    assert_eq!(
        data["available"],
        serde_json::json!([c]),
        "-32010 available value"
    );

    // ── -32011 RESIZE_INVALIDATED: {requested, invalidated_at, hint} ──
    // A REAL resize (dims change) invalidates the checkpoint.
    h.rpc_ok(
        "pane.set_size",
        serde_json::json!({ "pane_id": f.pane_id, "cols": 100, "rows": 30 }),
    );
    let err = h
        .rpc_raw(
            "pane.diff_since",
            serde_json::json!({ "pane_id": f.pane_id, "since_revision": c }),
        )
        .expect_error_code(-32011, "invalidated wire shape");
    let data = err.data.as_ref().expect("-32011 carries data");
    let mut keys: Vec<&str> = data
        .as_object()
        .expect("-32011 data is an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["hint", "invalidated_at", "requested"],
        "-32011 data fields are EXACTLY {{requested, invalidated_at, hint}}"
    );
    assert_eq!(data["requested"], c, "-32011 requested value");
    let invalidated_at = data["invalidated_at"]
        .as_u64()
        .expect("-32011 invalidated_at is u64");
    assert!(
        invalidated_at > c,
        "invalidated_at is the POST-mutation revision (> checkpoint {c}): {invalidated_at}"
    );
    assert!(
        data["hint"].as_str().is_some_and(|s| !s.is_empty()),
        "-32011 hint is a non-empty string"
    );

    h.kill_session(&f.session_id);
}
