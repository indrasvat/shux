//! `client.list` and `client.detach`: who is attached, and ending one of them.

use std::sync::Arc;

use shux_core::idref::{self, ParsedRef, RefKind};
use shux_rpc::{Policy, Sensitivity};
use tokio::sync::Mutex;

use crate::pane_io::PaneIoState;

pub(crate) fn register_client_methods(
    builder: shux_rpc::RouterBuilder,
    graph: shux_core::graph::GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
) -> shux_rpc::RouterBuilder {
    let io_list = io_state.clone();
    let io_detach = io_state;
    builder
        .register_with_policy(
            "client.list",
            Policy::fixed(Sensitivity::Public),
            move |_params: Option<serde_json::Value>| {
                let io = io_list.clone();
                let gh = graph.clone();
                async move {
                    let clients = io.lock().await.clients.clone();
                    let snap = gh.snapshot();
                    let rows: Vec<serde_json::Value> = clients
                        .list()
                        .into_iter()
                        .map(|c| {
                            let attached_at = c
                                .attached_at
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0);
                            serde_json::json!({
                                "id": c.id.0.to_string(),
                                "session_id": c.session_id.to_string(),
                                "session": snap.sessions.get(&c.session_id).map(|s| s.name.clone()),
                                "cols": c.cols,
                                "rows": c.rows,
                                "attached_at": attached_at,
                            })
                        })
                        .collect();
                    Ok(serde_json::json!({ "clients": rows }))
                }
            },
        )
        .register_with_policy(
            "client.detach",
            Policy::fixed(Sensitivity::Grantable),
            move |params: Option<serde_json::Value>| {
                let io = io_detach.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let raw = match params.get("id") {
                        Some(serde_json::Value::String(s)) => s,
                        Some(_) => {
                            return Err(shux_rpc::RpcError::invalid_params(
                                "'id' must be a string",
                            ));
                        }
                        None => return Err(shux_rpc::RpcError::invalid_params("missing 'id'")),
                    };
                    let not_found = || {
                        shux_rpc::RpcError::with_data(
                            shux_rpc::ErrorCode::NotFound,
                            serde_json::json!({
                                "resource": "client",
                                "id": raw.trim(),
                                "hint": "`shux client list` shows the attached clients",
                            }),
                        )
                    };
                    let parsed = idref::parse_ref(RefKind::Client, raw)
                        .map_err(|e| super::params::ref_error_to_rpc(e, "id"))?;
                    let clients = io.lock().await.clients.clone();
                    let target = match parsed {
                        ParsedRef::Exact(id) => id,
                        ParsedRef::Prefix(prefix) => idref::match_prefix(
                            RefKind::Client,
                            raw,
                            &prefix,
                            clients.list().into_iter().map(|c| c.id.0),
                        )
                        .map_err(|e| match e {
                            idref::RefError::NotFound { .. } => not_found(),
                            _ => super::params::ref_error_to_rpc(e, "id"),
                        })?,
                    };
                    if !clients.remove(target.into()) {
                        return Err(not_found());
                    }
                    Ok(serde_json::json!({ "detached": target.to_string() }))
                }
            },
        )
}

#[cfg(test)]
mod tests {
    use crate::clients::ClientInfo;
    use crate::rpc::test_harness::{RpcHarness, dispatch_err, dispatch_ok};
    use shux_core::event::ClientId;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn list_names_each_client_and_detach_takes_a_unique_prefix() {
        let h = RpcHarness::new();
        let (session_id, _, _) = h.seed_session("watched").await;
        let clients = h.io.lock().await.clients.clone();
        let info = |id: uuid::Uuid| ClientInfo {
            id: ClientId::from(id),
            session_id,
            cols: 100,
            rows: 30,
            attached_at: std::time::SystemTime::now(),
            votes: true,
            last_input: None,
        };
        // Two ids sharing a prefix, so a short one is ambiguous.
        let a = uuid::Uuid::parse_str("aaaa1111-0000-4000-8000-000000000000").unwrap();
        let b = uuid::Uuid::parse_str("aaaa2222-0000-4000-8000-000000000000").unwrap();
        let (end_a, end_b) = (CancellationToken::new(), CancellationToken::new());
        let _ga = clients.register(info(a), end_a.clone());
        let _gb = clients.register(info(b), end_b.clone());

        let listed = dispatch_ok(&h.router, "client.list", serde_json::json!({})).await;
        let rows = listed["clients"].as_array().expect("clients array");
        assert_eq!(rows.len(), 2, "{listed}");
        assert!(
            rows.iter()
                .all(|r| r["session"] == "watched" && r["cols"] == 100)
        );

        let ambiguous = dispatch_err(
            &h.router,
            "client.detach",
            serde_json::json!({"id": "aaaa"}),
        )
        .await;
        assert!(
            ambiguous
                .data
                .as_ref()
                .and_then(|d| d["detail"].as_str())
                .is_some_and(|s| s.contains("is ambiguous")),
            "{ambiguous:?}"
        );
        let wrong_type =
            dispatch_err(&h.router, "client.detach", serde_json::json!({"id": 5})).await;
        assert!(
            format!("{wrong_type:?}").contains("must be a string"),
            "{wrong_type:?}"
        );
        let missing = dispatch_err(
            &h.router,
            "client.detach",
            serde_json::json!({"id": "bbbb"}),
        )
        .await;
        assert_eq!(
            missing.code,
            shux_rpc::ErrorCode::NotFound.code(),
            "{missing:?}"
        );
        assert!(
            !end_a.is_cancelled() && !end_b.is_cancelled(),
            "a failed detach ended a client"
        );

        let done = dispatch_ok(
            &h.router,
            "client.detach",
            serde_json::json!({"id": " AAAA1 "}),
        )
        .await;
        assert_eq!(done["detached"], a.to_string(), "ids match case-blind");
        assert!(end_a.is_cancelled());
        assert!(!end_b.is_cancelled(), "detaching one client ended another");

        // Gone from the list the moment detach returns, even while its
        // connection is still closing.
        let sessions = dispatch_ok(&h.router, "session.list", serde_json::json!({})).await;
        assert_eq!(sessions["sessions"][0]["attached"], 1);
        let again = dispatch_err(
            &h.router,
            "client.detach",
            serde_json::json!({"id": "aaaa1"}),
        )
        .await;
        assert_eq!(
            again.code,
            shux_rpc::ErrorCode::NotFound.code(),
            "{again:?}"
        );

        // An empty id is a prefix of every id; it names no one.
        let empty = dispatch_err(&h.router, "client.detach", serde_json::json!({"id": ""})).await;
        assert_eq!(
            empty.code,
            shux_rpc::ErrorCode::InvalidParams.code(),
            "{empty:?}"
        );
        assert!(!end_b.is_cancelled(), "an empty id detached a client");

        for raw in ["a", "zzzz"] {
            let err =
                dispatch_err(&h.router, "client.detach", serde_json::json!({"id": raw})).await;
            assert_eq!(err.code, shux_rpc::ErrorCode::InvalidParams.code());
            assert!(!end_b.is_cancelled());
        }
        let done = dispatch_ok(
            &h.router,
            "client.detach",
            serde_json::json!({"id": b.simple().to_string()}),
        )
        .await;
        assert_eq!(done["detached"], b.to_string());
        assert!(end_b.is_cancelled());
    }

    /// Issue #190: `pane.set_size` pins the pane only if the resize lands,
    /// and `unpin` hands it back to the layout.
    #[tokio::test]
    async fn a_failed_set_size_leaves_no_pin_and_unpin_releases_one() {
        let h = RpcHarness::new();
        let (_session, _window, pane) = h.seed_session("pinned").await;
        // A resizer nobody reads: the resize can never land.
        let (dead_tx, dead_rx) = tokio::sync::mpsc::channel(1);
        drop(dead_rx);
        h.io.lock().await.resizers.insert(pane, dead_tx);
        let clients = h.io.lock().await.clients.clone();

        let params = serde_json::json!({ "pane_id": pane.to_string(), "cols": 100, "rows": 30 });
        let _ = dispatch_err(&h.router, "pane.set_size", params).await;
        assert!(
            !clients.is_pinned(pane),
            "a resize that never landed left a pin"
        );

        clients.pin(pane);
        let unpinned = dispatch_ok(
            &h.router,
            "pane.set_size",
            serde_json::json!({ "pane_id": pane.to_string(), "unpin": true }),
        )
        .await;
        assert_eq!(unpinned["was_pinned"], true);
        assert!(!clients.is_pinned(pane));
        let both = dispatch_err(
            &h.router,
            "pane.set_size",
            serde_json::json!({ "pane_id": pane.to_string(), "unpin": true, "cols": 80 }),
        )
        .await;
        assert_eq!(both.code, shux_rpc::ErrorCode::InvalidParams.code());
    }

    /// Unpinning a pane that does not exist is an error, not a quiet success.
    #[tokio::test]
    async fn unpinning_a_missing_pane_fails() {
        let h = RpcHarness::new();
        let gone = shux_core::model::PaneId::new();
        let err = dispatch_err(
            &h.router,
            "pane.set_size",
            serde_json::json!({ "pane_id": gone.to_string(), "unpin": true }),
        )
        .await;
        assert_eq!(err.code, shux_rpc::ErrorCode::NotFound.code(), "{err:?}");
    }

    /// A pane named with a session it is not in is refused, not acted on.
    #[tokio::test]
    async fn a_pane_from_another_session_is_refused() {
        let h = RpcHarness::new();
        let (_a, _wa, pane_a) = h.seed_session("alpha").await;
        let (b, _wb, _pane_b) = h.seed_session("beta").await;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<crate::pane_io::ResizeRequest>(4);
        h.io.lock().await.resizers.insert(pane_a, tx);
        tokio::spawn(async move {
            while let Some(req) = rx.recv().await {
                if let Some(ack) = req.ack {
                    let _ = ack.send(());
                }
            }
        });
        let err = dispatch_err(
            &h.router,
            "pane.set_size",
            serde_json::json!({ "session_id": b.to_string(), "pane_id": pane_a.to_string(), "cols": 90, "rows": 30 }),
        )
        .await;
        assert_eq!(
            err.code,
            shux_rpc::ErrorCode::InvalidParams.code(),
            "{err:?}"
        );
        let clients = h.io.lock().await.clients.clone();
        assert!(
            !clients.is_pinned(pane_a),
            "the other session's pane was pinned"
        );
    }

    /// `-p` without `-w`: the CLI names the session's active window beside a
    /// pane of a background window. That pane is in the session, so it is
    /// accepted.
    #[tokio::test]
    async fn a_background_pane_named_with_the_active_window_is_accepted() {
        let h = RpcHarness::new();
        let (s, _w, background) = h.seed_session("gamma").await;
        let active = h
            .graph
            .create_window(s, "second".into(), "/tmp".into())
            .await
            .expect("second window");
        h.graph.focus_window(active, None).await.expect("focus it");
        let (tx, mut rx) = tokio::sync::mpsc::channel::<crate::pane_io::ResizeRequest>(4);
        h.io.lock().await.resizers.insert(background, tx);
        tokio::spawn(async move {
            while let Some(req) = rx.recv().await {
                if let Some(ack) = req.ack {
                    let _ = ack.send(());
                }
            }
        });
        dispatch_ok(
            &h.router,
            "pane.set_size",
            serde_json::json!({ "window_id": active.to_string(), "pane_id": background.to_string(), "cols": 90, "rows": 30 }),
        )
        .await;
    }

    /// A set_size cancelled inside its sizing turn leaves the queue and the
    /// recorded size agreeing, so the sizer never skips a needed correction.
    #[tokio::test]
    async fn a_set_size_cancelled_mid_turn_leaves_no_stale_size() {
        let h = RpcHarness::new();
        let (_s, _w, pane) = h.seed_session("midturn").await;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<crate::pane_io::ResizeRequest>(4);
        let probe = tx.clone();
        let clients = {
            let mut io = h.io.lock().await;
            io.resizers.insert(pane, tx);
            io.clients.clone()
        };
        // Stop the call at its sizing turn, after its slot is reserved; then
        // hold the I/O state so the turn stops wherever it next needs it.
        let turn = clients.sizing_turn().await;
        let router = h.router.clone();
        let id = pane.to_string();
        let call = tokio::spawn(async move {
            router
                .dispatch(
                    "pane.set_size",
                    Some(serde_json::json!({ "pane_id": id, "cols": 120, "rows": 30 })),
                )
                .await
        });
        shux_testkit::wait_until_async("the call to reserve its slot", || {
            let reserved = probe.capacity() < probe.max_capacity();
            async move { reserved }
        })
        .await;
        let held = h.io.lock().await;
        drop(turn);
        shux_testkit::wait_until_async("the call to take its sizing turn", || {
            let busy = clients.sizing_busy();
            async move { busy }
        })
        .await;
        // The turn is handed over before the call runs: let it run to its
        // next await, wherever that is.
        tokio::task::yield_now().await;
        call.abort();
        let _ = call.await;
        drop(held);
        let queued = rx.try_recv().ok().map(|r| (r.size.cols, r.size.rows));
        let recorded =
            h.io.lock()
                .await
                .requested_sizes
                .get(&pane)
                .map(|s| (s.cols, s.rows));
        assert_eq!(
            queued, recorded,
            "a size went out that the sizer does not know of"
        );
    }

    /// Two set_size calls both fail: neither leaves its pin behind.
    #[tokio::test]
    async fn two_failed_set_sizes_leave_no_pin() {
        let h = RpcHarness::new();
        let (_s, _w, pane) = h.seed_session("twofail").await;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<crate::pane_io::ResizeRequest>(4);
        h.io.lock().await.resizers.insert(pane, tx);
        tokio::spawn(async move {
            let first = rx.recv().await.expect("first resize");
            let second = rx.recv().await.expect("second resize");
            drop((first, second));
        });
        let set = |cols: u64| {
            let router = h.router.clone();
            let id = pane.to_string();
            tokio::spawn(async move {
                router
                    .dispatch(
                        "pane.set_size",
                        Some(serde_json::json!({ "pane_id": id, "cols": cols, "rows": 30 })),
                    )
                    .await
            })
        };
        let first = set(90);
        tokio::task::yield_now().await;
        let second = set(100);
        let (first, second) = (first.await.unwrap(), second.await.unwrap());
        assert!(first.is_err() && second.is_err(), "both were meant to fail");
        let clients = h.io.lock().await.clients.clone();
        assert!(!clients.is_pinned(pane), "two failed set_sizes left a pin");
    }

    /// A set_size whose caller goes away mid-resize leaves no pin behind.
    #[tokio::test]
    async fn a_cancelled_set_size_leaves_no_pin() {
        let h = RpcHarness::new();
        let (_s, _w, pane) = h.seed_session("cancelled").await;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<crate::pane_io::ResizeRequest>(4);
        h.io.lock().await.resizers.insert(pane, tx);
        let router = h.router.clone();
        let id = pane.to_string();
        let call = tokio::spawn(async move {
            router
                .dispatch(
                    "pane.set_size",
                    Some(serde_json::json!({ "pane_id": id, "cols": 90, "rows": 30 })),
                )
                .await
        });
        // Held, never acked: the call is still waiting when it is dropped.
        let held = rx.recv().await.expect("the resize request");
        call.abort();
        let _ = call.await;
        drop(held);
        let clients = h.io.lock().await.clients.clone();
        assert!(
            !clients.is_pinned(pane),
            "a cancelled set_size left its pin"
        );
    }

    /// Two set_size calls race: one fails after the other succeeded. The
    /// pane stays pinned, as the successful call said.
    #[tokio::test]
    async fn a_failed_set_size_does_not_undo_a_concurrent_pin() {
        let h = RpcHarness::new();
        let (_s, _w, pane) = h.seed_session("race").await;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<crate::pane_io::ResizeRequest>(4);
        h.io.lock().await.resizers.insert(pane, tx);
        // Holds both requests, then fails the first and lands the second.
        tokio::spawn(async move {
            let first = rx.recv().await.expect("first resize");
            let second = rx.recv().await.expect("second resize");
            drop(first);
            if let Some(ack) = second.ack {
                let _ = ack.send(());
            }
        });
        let set = |cols: u64| {
            let router = h.router.clone();
            let id = pane.to_string();
            tokio::spawn(async move {
                router
                    .dispatch(
                        "pane.set_size",
                        Some(serde_json::json!({ "pane_id": id, "cols": cols, "rows": 30 })),
                    )
                    .await
            })
        };
        let first = set(90);
        tokio::task::yield_now().await;
        let second = set(100);
        let (first, second) = (first.await.unwrap(), second.await.unwrap());
        assert!(first.is_err(), "the first resize was meant to fail");
        assert_eq!(second.expect("second lands")["pinned"], true);
        let clients = h.io.lock().await.clients.clone();
        assert!(
            clients.is_pinned(pane),
            "a failed set_size undid a pin another call had made"
        );
    }
}
