//! `pane.wait_command`: block until a command typed into a pane's shell ends,
//! as the shell reports it over OSC 133, and return its exit code.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use shux_core::graph::GraphHandle;
use shux_core::model::PaneId;
use shux_rpc::RpcError;
use shux_rpc::policy::{Policy, Sensitivity};
use shux_vt::CommandLookup;
use tokio::sync::Mutex;

use crate::pane_io::PaneIoState;
use crate::rpc::params::resolve_pane_id_from_params;

pub(crate) fn register_wait_command_method(
    builder: shux_rpc::RouterBuilder,
    graph: GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
) -> shux_rpc::RouterBuilder {
    builder.register_with_policy(
        "pane.wait_command",
        Policy::fixed(Sensitivity::ContentRead),
        move |params: Option<Value>| {
            let (gh, io) = (graph.clone(), io_state.clone());
            async move {
                let params = params.unwrap_or_default();
                let after = crate::rpc::program_status::parse_after(&params)?;
                let timeout = crate::rpc::program_status::parse_wait_timeout(&params)?;
                let pane_id = resolve_pane_id_from_params(&gh, &params)?;
                // A full id resolves unchecked: one that names no pane is not
                // found, as `pane.wait_exit` says, not a pane that went away.
                if !gh.snapshot().panes.contains_key(&pane_id) {
                    return Err(RpcError::not_found("pane", &pane_id.to_string()));
                }
                wait_command(&gh, &io, pane_id, after, timeout).await
            }
        },
    )
}

async fn wait_command(
    graph: &GraphHandle,
    io: &Mutex<PaneIoState>,
    pane_id: PaneId,
    after: Option<u64>,
    timeout: Duration,
) -> Result<Value, RpcError> {
    let gone = |why: &str| {
        Ok(json!({"outcome": "pane_gone", "pane_id": pane_id.to_string(), "reason": why}))
    };
    let deadline = tokio::time::Instant::now() + timeout;
    // The floor and the incarnation are read in one hold, so the wait is
    // about this shell, and a later respawn ends it instead of answering it.
    let (floor, incarnation, mut wake) = {
        let state = io.lock().await;
        (
            after.unwrap_or_else(shux_vt::program_status_as_of),
            state.incarnations.get(&pane_id).copied(),
            state.commands_ended.subscribe(),
        )
    };
    loop {
        // Before the lookup: output is drained before an exit is recorded,
        // so a pane seen exited here has no command still to arrive.
        let ended = match graph.snapshot().panes.get(&pane_id) {
            None => Some("killed"),
            Some(p) if p.exit_status.is_some() => Some("exited"),
            Some(_) => None,
        };
        let (lookup, seen) = {
            let state = io.lock().await;
            // Torn down: the pane's program and terminal go together. A
            // different program in the pane is a respawn.
            let (Some(now), Some(vt)) = (state.incarnations.get(&pane_id), state.vts.get(&pane_id))
            else {
                return gone(if incarnation.is_some() {
                    "killed"
                } else {
                    "not_started"
                });
            };
            if Some(*now) != incarnation {
                return gone("respawned");
            }
            wake.borrow_and_update();
            (vt.command_after(floor), vt.commands_seen())
        };
        match lookup {
            CommandLookup::Found(c) => {
                return Ok(json!({
                    "outcome": "completed",
                    "pane_id": pane_id.to_string(),
                    "exit_code": c.exit_code,
                    "started": c.started,
                    "ended": c.ended,
                    "ended_by": c.ended_by.as_str(),
                }));
            }
            CommandLookup::Overrun => {
                return Ok(json!({
                    "outcome": "overrun",
                    "pane_id": pane_id.to_string(),
                    "kept": shux_vt::COMMANDS_KEPT,
                }));
            }
            CommandLookup::Pending => {}
        }
        if let Some(why) = ended {
            return gone(why);
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(json!({
                "outcome": "timeout",
                "pane_id": pane_id.to_string(),
                // Decided now, not up front: a fresh shell may not have read
                // its rc file when the wait began.
                "no_integration": !seen,
            }));
        }
        let wake_at =
            (tokio::time::Instant::now() + crate::rpc::program_status::WAIT_REPOLL).min(deadline);
        let _ = tokio::time::timeout_at(wake_at, wake.changed()).await;
    }
}
