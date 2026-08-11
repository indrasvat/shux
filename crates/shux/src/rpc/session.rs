//! Session CRUD methods.

use std::path::PathBuf;
use std::sync::Arc;

use shux_rpc::{Policy, Sensitivity};
use tokio::sync::Mutex;

use crate::pane_io::PaneIoState;
use crate::pane_spawn::{spawn_failure, spawn_pane_pty};
use crate::rpc::convert::{graph_error_to_rpc, session_to_json};
use crate::rpc::params::{
    optional_ref_param, parse_expected_version, parse_initial_pane_title, resolve_session_ref,
    set_initial_pane_title,
};
use crate::{lens_scratch, pane_command, session_meta, session_persist};

/// Register session CRUD methods on the router builder.
///
/// These methods use a `GraphHandle` to interact with the SessionGraph.
/// They are registered here (in the binary crate) because shux-rpc
/// intentionally does not depend on shux-core.
pub(crate) fn register_session_methods(
    builder: shux_rpc::RouterBuilder,
    graph: shux_core::graph::GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    cancel: tokio_util::sync::CancellationToken,
    meta_cache: session_meta::SessionMetaCache,
    scratch_registry: lens_scratch::ScratchRegistry,
) -> shux_rpc::RouterBuilder {
    let g1 = graph.clone();
    let g2 = graph.clone();
    let g3 = graph.clone();
    let g4 = graph.clone();
    let g5 = graph.clone();
    let g6 = graph.clone();

    let io_create = io_state.clone();
    let io_kill = io_state.clone();
    let io_ensure = io_state;
    let cancel_create = cancel.clone();
    let cancel_ensure = cancel;

    let meta_create = meta_cache.clone();
    let meta_kill = meta_cache.clone();
    let meta_ensure = meta_cache;

    let scratch_list = scratch_registry.clone();
    let scratch_kill = scratch_registry;

    builder
        .register_with_policy(
            "session.list",
            Policy::fixed(Sensitivity::Public),
            move |params: Option<serde_json::Value>| {
                let gh = g1.clone();
                let registry = scratch_list.clone();
                async move {
                    // LENS-R-041: scratch sessions are excluded from the
                    // default listing; `include_scratch: true` reveals them
                    // flagged `scratch: true`. Visibility is not
                    // authorization — an id is always resolvable directly
                    // (session.kill/snapshot/etc. never consult this filter).
                    let include_scratch = params
                        .as_ref()
                        .and_then(|p| p.get("include_scratch"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let scratch_ids = registry.ids();

                    let snap = gh.snapshot();
                    let mut sessions: Vec<_> = snap.sessions.values().collect();
                    sessions.sort_by_key(|s| s.created_at);
                    let sessions: Vec<serde_json::Value> = sessions
                        .iter()
                        .filter(|s| include_scratch || !scratch_ids.contains(&s.id))
                        .map(|s| {
                            let mut json = session_to_json(s, &snap);
                            if include_scratch {
                                json["scratch"] =
                                    serde_json::Value::Bool(scratch_ids.contains(&s.id));
                            }
                            json
                        })
                        .collect();
                    Ok(serde_json::json!({ "sessions": sessions }))
                }
            },
        )
        .register_with_policy(
            "session.create",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g2.clone();
                let io = io_create.clone();
                let ct = cancel_create.clone();
                let meta = meta_create.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let name = params
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());

                    // Optional pane command — see `pane_command` for the
                    // contract every spawning RPC shares (issue #125).
                    let command = pane_command::parse_pane_command(&params)?;
                    let pane_title = parse_initial_pane_title(&params)?;

                    // Auto-generate name if not provided (None).
                    // Explicit empty string (Some("")) flows through to validation.
                    let name = match name {
                        Some(n) => n,
                        None => {
                            let snap = gh.snapshot();
                            let mut idx = snap.sessions.len();
                            loop {
                                let candidate = format!("session-{idx}");
                                if !snap.session_name_exists(&candidate) {
                                    break candidate;
                                }
                                idx += 1;
                            }
                        }
                    };

                    let cwd = params
                        .get("cwd")
                        .and_then(|v| v.as_str())
                        .map(PathBuf::from)
                        .unwrap_or_else(|| {
                            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"))
                        });

                    // PR followup (codex P2 #10): persist `command` onto
                    // the initial pane so subscribers + auto-title see
                    // the truth. Pre-followup this RPC stored an empty
                    // `Pane.command` and only the PTY layer knew about
                    // the user's --cmd arg.
                    match gh
                        .create_session_with_command(name, cwd.clone(), command.clone())
                        .await
                    {
                        Ok(session_id) => {
                            set_initial_pane_title(&gh, session_id, pane_title).await?;

                            // Populate session-meta cache: git branch from
                            // the spawn cwd, SSH context from the daemon
                            // env. spawn_blocking because detect_git_branch
                            // shells out to `git`; using async tokio for
                            // this would force the runtime to wait for git.
                            let meta_cache_clone = meta.clone();
                            let cwd_for_meta = cwd.clone();
                            tokio::task::spawn_blocking(move || {
                                let branch = session_meta::detect_git_branch(&cwd_for_meta);
                                let over_ssh = session_meta::detect_over_ssh();
                                let snapshot = session_meta::SessionMeta {
                                    git_branch: branch,
                                    over_ssh,
                                };
                                // Tiny tokio block to write the cache —
                                // SessionMetaCache.set is async because the
                                // inner RwLock is tokio::sync.
                                tokio::runtime::Handle::current().block_on(async move {
                                    meta_cache_clone.set(session_id, snapshot).await;
                                });
                            });

                            let snap = gh.snapshot();
                            // Spawn PTY for the initial pane. A session whose
                            // only pane never started is not a session — the
                            // CLI printed "✓ Created session" over a phantom
                            // that answered "pane VT not found" to everything
                            // afterwards (issue #125 follow-up).
                            if let Some(s) = snap.sessions.get(&session_id) {
                                if let Some(wid) = s.windows.first()
                                    && let Some(w) = snap.windows.get(wid)
                                    && let Err(e) = spawn_pane_pty(
                                        w.active_pane,
                                        cwd,
                                        command.clone(),
                                        shux_pty::handle::PtySize::default(),
                                        Vec::new(),
                                        false,
                                        io,
                                        ct,
                                        gh.clone(),
                                    )
                                    .await
                                {
                                    let _ = gh.destroy_session(session_id, None).await;
                                    meta.remove(session_id).await;
                                    return Err(spawn_failure(&e));
                                }
                                Ok(session_to_json(s, &snap))
                            } else {
                                Ok(serde_json::json!({
                                    "id": session_id.to_string(),
                                }))
                            }
                        }
                        Err(e) => Err(graph_error_to_rpc(e)),
                    }
                }
            },
        )
        .register_with_policy(
            "session.kill",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g3.clone();
                let io = io_kill.clone();
                let meta = meta_kill.clone();
                let registry = scratch_kill.clone();
                async move {
                    let params = params.unwrap_or_default();

                    // Accept name or id — try UUID parse first, fall back to name lookup
                    let session_id = if let Some(id_str) = optional_ref_param(&params, "id")? {
                        let parsed = resolve_session_ref(&gh, id_str, "id")?;
                        // Verify it exists
                        let snap = gh.snapshot();
                        if !snap.sessions.contains_key(&parsed) {
                            return Err(shux_rpc::RpcError::not_found("session", id_str));
                        }
                        parsed
                    } else if let Some(name) = optional_ref_param(&params, "name")? {
                        let snap = gh.snapshot();
                        let session = snap
                            .find_session_by_name(name)
                            .ok_or_else(|| shux_rpc::RpcError::not_found("session", name))?;
                        session.id
                    } else {
                        return Err(shux_rpc::RpcError::invalid_params(
                            "missing 'name' or 'id' parameter",
                        ));
                    };

                    // Snapshot the session BEFORE destroying it so we know
                    // which panes belong to it. After destroy_session the
                    // graph entries are gone; without this snapshot we'd
                    // have no way to find the orphaned PTY tasks to clean up.
                    let pre_snap = gh.snapshot();
                    let name = pre_snap
                        .sessions
                        .get(&session_id)
                        .map(|s| s.name.clone())
                        .unwrap_or_default();
                    let pane_ids: Vec<shux_core::model::PaneId> = pre_snap
                        .sessions
                        .get(&session_id)
                        .map(|s| {
                            s.windows
                                .iter()
                                .flat_map(|wid| {
                                    pre_snap
                                        .windows
                                        .get(wid)
                                        .map(|w| w.layout.tree.pane_ids())
                                        .unwrap_or_default()
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    drop(pre_snap);

                    let expected_version = parse_expected_version(&params)?;

                    // Mutate the graph FIRST. If destroy_session errors, we
                    // leave PTY/VT state untouched (otherwise we'd kill PTYs
                    // for a session that's still in the graph). Same applies
                    // to a stale `expected_version` — the check inside
                    // destroy_session rejects the request before IO teardown.
                    gh.destroy_session(session_id, expected_version)
                        .await
                        .map_err(graph_error_to_rpc)?;

                    // Tear down every pane that belonged to the session.
                    // The explicit per-pane shutdown token is the hard
                    // lifecycle contract; writer/resizer removal is only
                    // bookkeeping. The PTY task prioritizes cancellation
                    // and signals the pane's process group before reaping,
                    // so rich TUIs do not survive as unreachable children.
                    {
                        let mut state = io.lock().await;
                        let pulse = state.teardown_panes(&pane_ids, true);
                        drop(state);
                        pulse.notify_one();
                    }

                    meta.remove(session_id).await;

                    // LENS-R-042c: explicit session.kill reaps a scratch
                    // session IMMEDIATELY (no post_exit_ttl_ms wait) — this
                    // is a no-op for ordinary sessions (registry lookup
                    // misses). For scratch it enforces the LENS-R-042 kill
                    // sequence + death confirmation before the registry row
                    // is dropped (P5 round-1 codex B3).
                    lens_scratch::on_session_killed(&registry, &io, session_id).await;

                    Ok(serde_json::json!({ "killed": name }))
                }
            },
        )
        .register_with_policy(
            "session.ensure",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g4.clone();
                let io = io_ensure.clone();
                let ct = cancel_ensure.clone();
                let meta = meta_ensure.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let name = params
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("default")
                        .to_string();

                    // Optional pane command (same contract as session.create).
                    let command = pane_command::parse_pane_command(&params)?;
                    let pane_title = parse_initial_pane_title(&params)?;

                    // Check if session already exists
                    let snap = gh.snapshot();
                    if let Some(s) = snap.find_session_by_name(&name) {
                        let mut json = session_to_json(s, &snap);
                        json["created"] = serde_json::Value::Bool(false);
                        return Ok(json);
                    }

                    let cwd = params
                        .get("cwd")
                        .and_then(|v| v.as_str())
                        .map(PathBuf::from)
                        .unwrap_or_else(|| {
                            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"))
                        });

                    match gh
                        .create_session_with_command(name, cwd.clone(), command.clone())
                        .await
                    {
                        Ok(session_id) => {
                            set_initial_pane_title(&gh, session_id, pane_title).await?;

                            // Populate session-meta cache (git branch, SSH).
                            // Same pattern as session.create above.
                            let meta_cache_clone = meta.clone();
                            let cwd_for_meta = cwd.clone();
                            tokio::task::spawn_blocking(move || {
                                let branch = session_meta::detect_git_branch(&cwd_for_meta);
                                let over_ssh = session_meta::detect_over_ssh();
                                let snapshot = session_meta::SessionMeta {
                                    git_branch: branch,
                                    over_ssh,
                                };
                                tokio::runtime::Handle::current().block_on(async move {
                                    meta_cache_clone.set(session_id, snapshot).await;
                                });
                            });

                            let snap = gh.snapshot();
                            // Spawn PTY for the initial pane. Same contract as
                            // session.create: a failed spawn is an error, not a
                            // session (issue #125 follow-up).
                            if let Some(s) = snap.sessions.get(&session_id) {
                                if let Some(wid) = s.windows.first()
                                    && let Some(w) = snap.windows.get(wid)
                                    && let Err(e) = spawn_pane_pty(
                                        w.active_pane,
                                        cwd,
                                        command.clone(),
                                        shux_pty::handle::PtySize::default(),
                                        Vec::new(),
                                        false,
                                        io,
                                        ct,
                                        gh.clone(),
                                    )
                                    .await
                                {
                                    let _ = gh.destroy_session(session_id, None).await;
                                    meta.remove(session_id).await;
                                    return Err(spawn_failure(&e));
                                }
                                let mut json = session_to_json(s, &snap);
                                json["created"] = serde_json::Value::Bool(true);
                                Ok(json)
                            } else {
                                Ok(serde_json::json!({
                                    "id": session_id.to_string(),
                                    "created": true,
                                }))
                            }
                        }
                        Err(e) => Err(graph_error_to_rpc(e)),
                    }
                }
            },
        )
        .register_with_policy(
            "session.export_template",
            Policy::fixed(Sensitivity::Public),
            move |params: Option<serde_json::Value>| {
                let gh = g6.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let snap = gh.snapshot();
                    let session_id = if let Some(id) = optional_ref_param(&params, "id")? {
                        resolve_session_ref(&gh, id, "id")?
                    } else if let Some(name) = optional_ref_param(&params, "name")? {
                        snap.find_session_by_name(name)
                            .ok_or_else(|| shux_rpc::RpcError::not_found("session", name))?
                            .id
                    } else {
                        return Err(shux_rpc::RpcError::invalid_params(
                            "missing 'name' or 'id' parameter",
                        ));
                    };
                    let template = session_persist::export_session_template(&snap, session_id)
                        .map_err(|e| shux_rpc::RpcError::internal(&format!("{e}")))?;
                    Ok(serde_json::json!({ "template": template }))
                }
            },
        )
        .register_with_policy(
            "session.rename",
            Policy::fixed(Sensitivity::OwnedMutation),
            move |params: Option<serde_json::Value>| {
                let gh = g5.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let new_name = params
                        .get("new_name")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'new_name' parameter")
                        })?
                        .to_string();

                    // Resolve session by name or id
                    let session_id = if let Some(name) = optional_ref_param(&params, "name")? {
                        let snap = gh.snapshot();
                        let session = snap
                            .find_session_by_name(name)
                            .ok_or_else(|| shux_rpc::RpcError::not_found("session", name))?;
                        session.id
                    } else if let Some(id_str) = optional_ref_param(&params, "id")? {
                        resolve_session_ref(&gh, id_str, "id")?
                    } else {
                        return Err(shux_rpc::RpcError::invalid_params(
                            "missing 'name' or 'id' parameter",
                        ));
                    };

                    let expected_version = parse_expected_version(&params)?;

                    gh.rename_session(session_id, new_name, expected_version)
                        .await
                        .map_err(graph_error_to_rpc)?;

                    let snap = gh.snapshot();
                    if let Some(s) = snap.sessions.get(&session_id) {
                        Ok(session_to_json(s, &snap))
                    } else {
                        Err(shux_rpc::RpcError::internal(
                            "session vanished after rename",
                        ))
                    }
                }
            },
        )
}

#[cfg(test)]
mod tests {

    use crate::rpc::test_harness::{RpcHarness, dispatch_err, dispatch_ok};
    use shux_core::layout::Direction;

    #[tokio::test]
    async fn production_router_session_window_pane_routes_mutate_graph_and_cleanup_io() {
        let harness = RpcHarness::new();
        let (session_id, window_id, first_pane) = harness.seed_session("alpha").await;
        let second_pane = harness
            .graph
            .split_pane(first_pane, Direction::Vertical, 0.5)
            .await
            .unwrap();
        let second_window = harness
            .graph
            .create_window(
                session_id,
                "logs".to_string(),
                std::path::PathBuf::from("/tmp"),
            )
            .await
            .unwrap();
        let second_window_pane = {
            let snap = harness.graph.snapshot();
            snap.windows[&second_window].active_pane
        };
        let _first_rx = harness.seed_io(first_pane, b"alpha ready\n").await;
        let _second_rx = harness.seed_io(second_pane, b"beta ready\n").await;
        let _window_rx = harness.seed_io(second_window_pane, b"logs ready\n").await;

        let listed = dispatch_ok(&harness.router, "session.list", serde_json::json!({})).await;
        assert_eq!(listed["sessions"][0]["name"], "alpha");
        assert_eq!(listed["sessions"][0]["window_count"], 2);

        let renamed = dispatch_ok(
            &harness.router,
            "session.rename",
            serde_json::json!({"name": "alpha", "new_name": "beta"}),
        )
        .await;
        assert_eq!(renamed["name"], "beta");

        let _other = harness.seed_session("other").await;
        let conflict = dispatch_err(
            &harness.router,
            "session.rename",
            serde_json::json!({"id": session_id.to_string(), "new_name": "other"}),
        )
        .await;
        assert_eq!(conflict.code, shux_rpc::ErrorCode::NameConflict.code());

        let windows = dispatch_ok(
            &harness.router,
            "window.list",
            serde_json::json!({"session_id": session_id.to_string()}),
        )
        .await;
        assert_eq!(windows.as_array().unwrap().len(), 2);

        let renamed_window = dispatch_ok(
            &harness.router,
            "window.rename",
            serde_json::json!({"id": second_window.to_string(), "name": "ops"}),
        )
        .await;
        assert_eq!(renamed_window["title"], "ops");

        let refocused_first = dispatch_ok(
            &harness.router,
            "window.focus",
            serde_json::json!({"id": window_id.to_string()}),
        )
        .await;
        assert_eq!(
            refocused_first["previous_window_id"],
            second_window.to_string()
        );

        let focused_window = dispatch_ok(
            &harness.router,
            "window.focus",
            serde_json::json!({"id": second_window.to_string()}),
        )
        .await;
        assert_eq!(focused_window["previous_window_id"], window_id.to_string());

        let reordered = dispatch_ok(
            &harness.router,
            "window.reorder",
            serde_json::json!({"id": second_window.to_string(), "new_index": 0}),
        )
        .await;
        assert_eq!(reordered["index"], 0);

        let panes = dispatch_ok(
            &harness.router,
            "pane.list",
            serde_json::json!({"window_id": window_id.to_string()}),
        )
        .await;
        assert_eq!(panes.as_array().unwrap().len(), 2);

        let focused_pane = dispatch_ok(
            &harness.router,
            "pane.focus",
            serde_json::json!({"pane_id": second_pane.to_string()}),
        )
        .await;
        assert_eq!(focused_pane["pane_id"], second_pane.to_string());

        let titled = dispatch_ok(
            &harness.router,
            "pane.set_title",
            serde_json::json!({"pane_id": second_pane.to_string(), "title": "editor", "auto": false}),
        )
        .await;
        assert_eq!(titled["manual_title"], "editor");
        assert_eq!(titled["auto_title"], false);

        let zoomed = dispatch_ok(
            &harness.router,
            "pane.zoom",
            serde_json::json!({"pane_id": second_pane.to_string()}),
        )
        .await;
        assert_eq!(zoomed["is_zoomed"], true);
        let _ = dispatch_ok(
            &harness.router,
            "pane.zoom",
            serde_json::json!({"pane_id": second_pane.to_string()}),
        )
        .await;

        let swapped = dispatch_ok(
            &harness.router,
            "pane.swap",
            serde_json::json!({"pane_id": first_pane.to_string(), "target_pane_id": second_pane.to_string()}),
        )
        .await;
        assert_eq!(swapped["pane_a"], first_pane.to_string());

        let stale = dispatch_err(
            &harness.router,
            "pane.resize",
            serde_json::json!({"pane_id": second_pane.to_string(), "direction": "horizontal", "delta": 0.2, "expected_version": 999_999}),
        )
        .await;
        assert_eq!(stale.code, shux_rpc::ErrorCode::VersionConflict.code());
        assert!(
            harness.io.lock().await.vts.contains_key(&second_pane),
            "stale pane.resize must not tear down pane IO"
        );

        let killed_pane = dispatch_ok(
            &harness.router,
            "pane.kill",
            serde_json::json!({"pane_id": second_pane.to_string()}),
        )
        .await;
        assert_eq!(killed_pane["killed"], second_pane.to_string());
        assert!(!harness.io.lock().await.vts.contains_key(&second_pane));

        let killed_window = dispatch_ok(
            &harness.router,
            "window.kill",
            serde_json::json!({"id": second_window.to_string()}),
        )
        .await;
        assert_eq!(killed_window["killed"], second_window.to_string());
        assert!(
            !harness
                .io
                .lock()
                .await
                .vts
                .contains_key(&second_window_pane)
        );

        let killed_session = dispatch_ok(
            &harness.router,
            "session.kill",
            serde_json::json!({"id": session_id.to_string()}),
        )
        .await;
        assert_eq!(killed_session["killed"], "beta");
        assert!(!harness.io.lock().await.vts.contains_key(&first_pane));
        assert!(!harness.graph.snapshot().sessions.contains_key(&session_id));

        harness.stop().await;
    }
}
