//! `state.apply` — the generic `Op` delta the templates and SDKs target.

use std::sync::Arc;

use shux_rpc::{Policy, Sensitivity};
use tokio::sync::Mutex;

use crate::pane_command;
use crate::pane_io::PaneIoState;
use crate::pane_spawn::{spawn_failure_message, spawn_pane_pty};
use crate::rpc::convert::graph_error_to_rpc;

/// Register `state.apply` RPC method.
///
/// Takes a generic `Op` delta (NOT a TOML template — codex P0 #2: keeping
/// the daemon API agnostic to template grammar means future SDKs / MCP
/// servers / agents can target the same primitive).
///
/// Atomicity: graph-level all-or-nothing. PTY spawns happen AFTER the
/// graph commits and per-pane spawn outcomes are reported in
/// `BatchResult::spawn_results`. Spawn failure does NOT roll back the
/// graph (codex P0 #1: rolling back PTY-spawned commands would mean
/// killing already-launched subprocesses, which has its own side effects;
/// honest reporting beats dishonest atomicity).
pub(crate) fn register_state_methods(
    builder: shux_rpc::RouterBuilder,
    graph: shux_core::graph::GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    cancel: tokio_util::sync::CancellationToken,
) -> shux_rpc::RouterBuilder {
    builder.register_with_policy(
        "state.apply",
        Policy::fixed(Sensitivity::Grantable),
        move |params: Option<serde_json::Value>| {
            let gh = graph.clone();
            let io = io_state.clone();
            let ct = cancel.clone();
            async move {
                // Parse `{ ops: [...] }`.
                let params = params.unwrap_or_default();
                let ops_value = params
                    .get("ops")
                    .cloned()
                    .ok_or_else(|| shux_rpc::RpcError::invalid_params("missing 'ops' array"))?;
                let ops: Vec<shux_core::apply::Op> =
                    serde_json::from_value(ops_value).map_err(|e| {
                        shux_rpc::RpcError::invalid_params(&format!("ops parse error: {e}"))
                    })?;

                // serde proves each command is a `Vec<String>`; it does not
                // prove the strings can reach `execve`. `[""]` and a NUL-bearing
                // argument used to commit the whole batch and then leave a pane
                // that never spawned — reported as one line of `spawn_results`
                // among many, with the session, window and dead pane kept
                // (issue #125 follow-up). Rejected up front, before anything is
                // committed. Same function the CLI's `--dry-run` calls, so the
                // two cannot give different answers.
                pane_command::validate_ops(&ops)?;

                // Run the staged transaction through the single-writer task.
                let mut result = gh.apply_batch(ops).await.map_err(batch_error_to_rpc)?;

                // Graph commit succeeded. Now spawn PTYs for each new pane.
                // Per codex P0 #1: spawn outcomes are reported per-pane in
                // `spawn_results` and do NOT roll back the graph.
                let snap = gh.snapshot();
                let mut spawn_results = Vec::new();
                for output in &result.outputs {
                    if let Some(pane_id) = output.pane_id
                        && let Some(pane) = snap.panes.get(&pane_id)
                    {
                        let cwd = pane.cwd.clone();
                        let command = pane.command.clone();
                        let spawn_io = io.clone();
                        let spawn_ct = ct.clone();
                        match spawn_pane_pty(
                            pane_id,
                            cwd,
                            command,
                            shux_pty::handle::PtySize::default(),
                            Vec::new(),
                            false,
                            spawn_io,
                            spawn_ct,
                            gh.clone(),
                        )
                        .await
                        {
                            Ok(()) => spawn_results.push(shux_core::apply::SpawnResult {
                                op_index: output.op_index,
                                pane_id,
                                spawned: true,
                                error: None,
                            }),
                            Err(e) => spawn_results.push(shux_core::apply::SpawnResult {
                                op_index: output.op_index,
                                pane_id,
                                spawned: false,
                                // Same diagnosis the five rollback RPCs give.
                                // `SpawnResult` carries one string, so the hint
                                // rides along in it rather than being dropped —
                                // this is the one path where an oversized argv
                                // can actually land.
                                error: Some(spawn_failure_message(&e)),
                            }),
                        }
                    }
                }
                // `state.apply` deliberately keeps a pane whose PTY never
                // started (codex P0 #1: no rollback), and the batch focuses the
                // last pane it created regardless. So a batch with one bad op
                // left the window focused on a corpse, and every `-p`-less verb
                // in that window answered "pane VT not found". Hand focus to a
                // pane that actually started (issue #125 follow-up).
                let dead: std::collections::HashSet<_> = spawn_results
                    .iter()
                    .filter(|r| !r.spawned)
                    .map(|r| r.pane_id)
                    .collect();
                if !dead.is_empty() {
                    // "Usable" means it has a VT, not "absent from THIS batch's
                    // failures" and not "has a live PTY".
                    //
                    // A corpse left by an earlier apply never spawned, so it has
                    // no VT — the first cut of this rescue focused one anyway,
                    // reproducing the exact symptom it was written to prevent.
                    //
                    // But a pane that *exited* is not a corpse. Its writer is
                    // gone by design while its grid and scrollback stay (see
                    // `reap_pane`), which is what lets `pane capture` answer for
                    // a short-lived command long after it finished. Keying on
                    // `writers` therefore skipped the most ordinary sibling of
                    // all — the build step that already succeeded — and left
                    // focus on the pane that never started.
                    let usable: std::collections::HashSet<_> = {
                        let state = io.lock().await;
                        state.vts.keys().copied().collect()
                    };
                    let snap = gh.snapshot();
                    let mut rescue = Vec::new();
                    for pane_id in &dead {
                        let Some(window_id) = snap.panes.get(pane_id).map(|p| p.window_id) else {
                            continue;
                        };
                        let Some(window) = snap.windows.get(&window_id) else {
                            continue;
                        };
                        if window.active_pane != *pane_id {
                            continue;
                        }
                        // Layout order, not `HashMap` order: iterating the pane
                        // map gave a different answer run to run, so a script
                        // that applied a template and then used the focused pane
                        // targeted a different one each time.
                        if let Some(answering) = window
                            .layout
                            .tree
                            .pane_ids()
                            .into_iter()
                            .find(|id| usable.contains(id))
                        {
                            rescue.push(answering);
                        }
                    }
                    drop(snap);
                    for pane in rescue {
                        let _ = gh.focus_pane(pane).await;
                    }
                }

                result.spawn_results = spawn_results;

                serde_json::to_value(&result).map_err(|e| {
                    shux_rpc::RpcError::internal(&format!("apply result serialize error: {e}"))
                })
            }
        },
    )
}

/// Map BatchError to an appropriate RPC error.
fn batch_error_to_rpc(e: shux_core::apply::BatchError) -> shux_rpc::RpcError {
    use shux_core::apply::BatchError;
    match e {
        BatchError::Empty => shux_rpc::RpcError::invalid_params("ops array is empty"),
        BatchError::BackRefOutOfRange { .. } | BatchError::BackRefWrongType { .. } => {
            shux_rpc::RpcError::invalid_params(&e.to_string())
        }
        BatchError::OpFailed { source, .. } => graph_error_to_rpc(source),
    }
}

#[cfg(test)]
mod tests {

    use crate::rpc::params::{resolve_pane_id_from_params, resolve_window_id_from_params};
    use crate::rpc::test_harness::{RpcHarness, dispatch_err, dispatch_ok};

    #[tokio::test]
    async fn production_state_apply_reports_validation_and_spawn_results() {
        let harness = RpcHarness::new();

        let missing_ops = dispatch_err(&harness.router, "state.apply", serde_json::json!({})).await;
        assert_eq!(missing_ops.code, shux_rpc::ErrorCode::InvalidParams.code());

        let malformed_ops = dispatch_err(
            &harness.router,
            "state.apply",
            serde_json::json!({"ops": "not-an-array"}),
        )
        .await;
        assert_eq!(
            malformed_ops.code,
            shux_rpc::ErrorCode::InvalidParams.code()
        );

        let empty_ops = dispatch_err(
            &harness.router,
            "state.apply",
            serde_json::json!({"ops": []}),
        )
        .await;
        assert_eq!(empty_ops.code, shux_rpc::ErrorCode::InvalidParams.code());

        let applied = dispatch_ok(
            &harness.router,
            "state.apply",
            serde_json::json!({
                "ops": [{
                    "op": "create_session",
                    "name": "applied",
                    "cwd": "/tmp",
                    "initial_command": ["true"],
                    "initial_window_title": "dev"
                }]
            }),
        )
        .await;
        assert_eq!(applied["outputs"].as_array().unwrap().len(), 1);
        assert_eq!(applied["spawn_results"].as_array().unwrap().len(), 1);
        assert!(
            applied["correlation_id"]
                .as_str()
                .unwrap()
                .starts_with("apply-")
        );
        let snap = harness.graph.snapshot();
        let session = snap
            .find_session_by_name("applied")
            .expect("applied session");
        let session_id = session.id;
        let window_id = session.active_window;
        let pane_id = snap.windows[&window_id].active_pane;
        drop(snap);
        assert_eq!(
            resolve_window_id_from_params(
                &harness.graph,
                &serde_json::json!({"session_id": session_id.to_string()})
            )
            .unwrap(),
            window_id
        );
        assert_eq!(
            resolve_pane_id_from_params(
                &harness.graph,
                &serde_json::json!({"pane_id": pane_id.to_string()})
            )
            .unwrap(),
            pane_id
        );
        assert_eq!(
            resolve_window_id_from_params(&harness.graph, &serde_json::json!({}))
                .unwrap_err()
                .code,
            shux_rpc::ErrorCode::InvalidParams.code()
        );

        harness.stop().await;
    }
}
