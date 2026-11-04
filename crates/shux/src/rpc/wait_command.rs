//! `pane.wait_command`: block until a command typed into a pane's shell ends,
//! as the shell reports it over OSC 133, and return its exit code.
//!
//! The caller reads `as_of` from `pane.status`, sends the command, and waits
//! with `after: as_of`: the earliest command that *started* above that floor
//! is the one returned, so a command already running at the read is never
//! mistaken for it. To wait for the next one, pass the returned `started`.

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

const WAIT_DEFAULT_MS: u64 = 30_000;
const WAIT_MAX_MS: u64 = 600_000;
/// Re-read even without a wake: a pane's exit is learned from the graph.
const WAIT_REPOLL: Duration = Duration::from_millis(250);

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
                let (after, timeout) = parse(&params)?;
                let pane_id = resolve_pane_id_from_params(&gh, &params)?;
                wait_command(&gh, &io, pane_id, after, timeout).await
            }
        },
    )
}

fn parse(params: &Value) -> Result<(Option<u64>, Duration), RpcError> {
    let after = match params.get("after") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_u64()
                .ok_or_else(|| RpcError::invalid_params("after must be a non-negative integer"))?,
        ),
    };
    if let Some(after) = after
        && after > shux_vt::program_status_as_of()
    {
        return Err(RpcError::invalid_params(&format!(
            "after {after} is above every stamp this daemon has issued: read as_of from pane.status"
        )));
    }
    let timeout_ms = match params.get("timeout_ms") {
        None | Some(Value::Null) => WAIT_DEFAULT_MS,
        Some(v) => v
            .as_u64()
            .ok_or_else(|| RpcError::invalid_params("timeout_ms must be a non-negative integer"))?,
    };
    if timeout_ms > WAIT_MAX_MS {
        return Err(RpcError::invalid_params(&format!(
            "timeout_ms must be at most {WAIT_MAX_MS}"
        )));
    }
    Ok((after, Duration::from_millis(timeout_ms)))
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
        if !state.vts.contains_key(&pane_id) {
            return gone("no_terminal");
        }
        (
            after.unwrap_or_else(shux_vt::program_status_as_of),
            state.incarnations.get(&pane_id).copied(),
            state.commands_ended.subscribe(),
        )
    };
    loop {
        let (lookup, seen) = {
            let state = io.lock().await;
            if state.incarnations.get(&pane_id).copied() != incarnation {
                return gone("respawned");
            }
            let Some(vt) = state.vts.get(&pane_id) else {
                return gone("no_terminal");
            };
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
        if graph
            .snapshot()
            .panes
            .get(&pane_id)
            .is_none_or(|p| p.exit_status.is_some())
        {
            return gone("exited");
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
        let wake_at = (tokio::time::Instant::now() + WAIT_REPOLL).min(deadline);
        let _ = tokio::time::timeout_at(wake_at, wake.changed()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_are_checked() {
        assert!(parse(&json!({"after": -1})).is_err());
        assert!(parse(&json!({"after": "3"})).is_err());
        assert!(
            parse(&json!({"after": u64::MAX})).is_err(),
            "a floor no stamp can pass"
        );
        assert!(parse(&json!({"timeout_ms": WAIT_MAX_MS + 1})).is_err());
        assert_eq!(
            parse(&json!({})).unwrap(),
            (None, Duration::from_millis(WAIT_DEFAULT_MS))
        );
        assert_eq!(
            parse(&json!({"after": 0, "timeout_ms": 5})).unwrap().0,
            Some(0)
        );
    }
}
