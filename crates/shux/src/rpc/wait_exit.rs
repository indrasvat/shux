//! `pane.wait_exit`: block until a pane's program exits, and say how it ended.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use shux_core::bus::{EventBus, SubscriptionEvent};
use shux_core::event::EventData;
use shux_core::graph::GraphHandle;
use shux_core::model::{PaneId, ProgramExit};
use shux_rpc::RpcError;
use shux_rpc::policy::{Policy, Sensitivity};
use tokio::sync::Mutex;

use crate::pane_io::PaneIoState;
use crate::rpc::params::resolve_pane_id_from_params;

/// How often a wait on a pane that has not started looks again.
const START_REPOLL: Duration = Duration::from_millis(250);

pub(crate) fn register_wait_exit_method(
    builder: shux_rpc::RouterBuilder,
    graph: GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    bus: EventBus,
) -> shux_rpc::RouterBuilder {
    builder.register_with_policy(
        "pane.wait_exit",
        // `pane.list` already shows `exit_status`; this only waits for it.
        Policy::fixed(Sensitivity::Public),
        move |params: Option<Value>| {
            let (gh, io, bus) = (graph.clone(), io_state.clone(), bus.clone());
            async move {
                let params = params.unwrap_or_default();
                let timeout = parse_timeout(&params)?;
                // Before resolving: an exit between the two must still be seen.
                let events = bus.subscribe_filtered(vec!["pane.exited".to_string()]);
                let pane_id = resolve_pane_id_from_params(&gh, &params)?;
                if !gh.snapshot().panes.contains_key(&pane_id) {
                    // A full id resolves unchecked: gone is not-found unless
                    // the bus saw it leave.
                    return announced_exit(&bus, pane_id)
                        .ok_or_else(|| RpcError::not_found("pane", &pane_id.to_string()));
                }
                wait_exit(&gh, &io, &bus, events, pane_id, timeout).await
            }
        },
    )
}

/// Whether a program has run in the pane, and if not, whether its spawn
/// failed with none under way to retry it.
#[derive(Debug, PartialEq, Eq)]
enum Start {
    Ran,
    Pending,
    Failed,
}

async fn start(io: &Mutex<PaneIoState>, pane_id: PaneId) -> Start {
    // The io lock orders this against a spawn installing or recording failure.
    let state = io.lock().await;
    if state.incarnations.contains_key(&pane_id) {
        Start::Ran
    } else if state.failed_spawns.contains(&pane_id) && !crate::pane_spawn::spawn_in_flight(pane_id)
    {
        Start::Failed
    } else {
        Start::Pending
    }
}

/// `None` waits for as long as the program runs.
fn parse_timeout(params: &Value) -> Result<Option<Duration>, RpcError> {
    match params.get("timeout_ms") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .map(|ms| Some(Duration::from_millis(ms)))
            .ok_or_else(|| RpcError::invalid_params("timeout_ms must be a non-negative integer")),
    }
}

/// How the program ended. `exited` carries exactly one of `exit_code` and
/// `signal`; a child that was never reaped is `unknown`, never a code.
pub(crate) fn exit_payload(pane_id: PaneId, exit: ProgramExit) -> Value {
    let pane_id = pane_id.to_string();
    match (exit.status, exit.signal) {
        (_, Some(signal)) => {
            let mut v = json!({
                "outcome": "exited",
                "pane_id": pane_id,
                "signal": signal,
                "signal_name": signal_name(signal),
            });
            if exit.by_shux {
                v["by_shux"] = true.into();
            }
            v
        }
        (-1, None) => unknown(&pane_id, "not_reaped"),
        (code, None) => json!({"outcome": "exited", "pane_id": pane_id, "exit_code": code}),
    }
}

fn unknown(pane_id: &str, reason: &str) -> Value {
    json!({"outcome": "unknown", "pane_id": pane_id, "reason": reason})
}

fn signal_name(signal: i32) -> Value {
    nix::sys::signal::Signal::try_from(signal)
        .map(|s| Value::String(s.as_str().to_string()))
        .unwrap_or(Value::Null)
}

/// `pane_id`'s `pane.exited`: `Some(None)` is a removal with no exit.
fn exited(data: &EventData, pane_id: PaneId) -> Option<Option<ProgramExit>> {
    match *data {
        EventData::PaneExited {
            pane_id: p,
            exit_status,
            signal,
            by_shux,
            ..
        } if p == pane_id => Some(exit_status.map(|status| ProgramExit {
            status,
            signal,
            by_shux,
        })),
        _ => None,
    }
}

fn killed(pane_id: PaneId) -> Value {
    json!({"outcome": "killed", "pane_id": pane_id.to_string()})
}

/// How a pane already gone from the graph ended, from event history back to
/// the start of its program: the exit it recorded, killed if it was removed
/// first, `unknown` if that start has left the history, `None` if history
/// never saw the pane.
fn announced_exit(bus: &EventBus, pane_id: PaneId) -> Option<Value> {
    let filters = ["pane.exited", "pane.created", "pane.respawned"].map(String::from);
    let mut seen = false;
    for e in bus.history_filtered(usize::MAX, &filters).into_iter().rev() {
        match exited(&e.data, pane_id) {
            Some(Some(exit)) => return Some(exit_payload(pane_id, exit)),
            Some(None) => {
                seen = true;
                continue;
            }
            None => {}
        }
        match e.data {
            EventData::PaneCreated { pane_id: p, .. }
            | EventData::PaneRespawned { pane_id: p, .. }
                if p == pane_id =>
            {
                return Some(killed(pane_id));
            }
            _ => {}
        }
    }
    seen.then(|| unknown(&pane_id.to_string(), "events_lost"))
}

/// Wait on `events`, a `pane.exited` subscription taken before `pane_id` was
/// resolved (or spawned).
pub(crate) async fn wait_exit(
    graph: &GraphHandle,
    io: &Mutex<PaneIoState>,
    bus: &EventBus,
    mut events: shux_core::bus::Subscription,
    pane_id: PaneId,
    timeout: Option<Duration>,
) -> Result<Value, RpcError> {
    let deadline = match timeout {
        None => None,
        Some(t) => Some(
            tokio::time::Instant::now()
                .checked_add(t)
                .ok_or_else(|| RpcError::invalid_params("timeout_ms is too large"))?,
        ),
    };
    // Read the graph now and after any gap; between them, the events say it.
    // An exit is final: a pane runs one program.
    let mut reread = true;
    let mut ran = false;
    loop {
        // A spawn that fails fires no event: until a program runs, look again.
        if !ran {
            match start(io, pane_id).await {
                Start::Ran => ran = true,
                Start::Failed
                    if graph
                        .snapshot()
                        .panes
                        .get(&pane_id)
                        .is_some_and(|p| p.exit_status.is_none()) =>
                {
                    return Ok(json!({"outcome": "not_started", "pane_id": pane_id.to_string()}));
                }
                _ => reread = true,
            }
        }
        if reread {
            match graph.snapshot().panes.get(&pane_id) {
                None => {
                    return Ok(announced_exit(bus, pane_id)
                        .unwrap_or_else(|| unknown(&pane_id.to_string(), "events_lost")));
                }
                Some(pane) => {
                    if let Some(status) = pane.exit_status {
                        let exit = ProgramExit {
                            status,
                            signal: pane.exit_signal,
                            by_shux: pane.exit_by_shux,
                        };
                        return Ok(exit_payload(pane_id, exit));
                    }
                }
            }
        }
        let wake = match (deadline, ran) {
            (at, true) => at,
            (None, false) => Some(tokio::time::Instant::now() + START_REPOLL),
            (Some(at), false) => Some(at.min(tokio::time::Instant::now() + START_REPOLL)),
        };
        let next = match wake {
            Some(at) => match tokio::time::timeout_at(at, events.recv()).await {
                Ok(next) => next,
                Err(_) if deadline.is_some_and(|d| tokio::time::Instant::now() >= d) => {
                    return Ok(json!({"outcome": "timeout", "pane_id": pane_id.to_string()}));
                }
                Err(_) => continue,
            },
            None => events.recv().await,
        };
        reread = false;
        match next {
            None => return Err(RpcError::internal("event bus closed")),
            Some(SubscriptionEvent::Lagged(_)) => reread = true,
            Some(SubscriptionEvent::Event(e)) => {
                // The pane may already be gone: the event is the record.
                if let Some(exit) = exited(&e.data, pane_id) {
                    return Ok(exit.map_or_else(|| killed(pane_id), |x| exit_payload(pane_id, x)));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_is_optional_and_never_negative() {
        assert_eq!(parse_timeout(&json!({})).unwrap(), None);
        assert_eq!(parse_timeout(&json!({"timeout_ms": null})).unwrap(), None);
        assert_eq!(
            parse_timeout(&json!({"timeout_ms": 5})).unwrap(),
            Some(Duration::from_millis(5))
        );
        assert!(parse_timeout(&json!({"timeout_ms": -1})).is_err());
        assert!(parse_timeout(&json!({"timeout_ms": "5s"})).is_err());
    }

    #[test]
    fn a_signal_death_reports_the_signal_and_no_code() {
        let pane = PaneId::new();
        let code = exit_payload(pane, ProgramExit::code(7));
        assert_eq!(
            (code["outcome"].as_str(), code["exit_code"].as_i64()),
            (Some("exited"), Some(7))
        );
        assert!(code.get("signal").is_none());
        let signal = exit_payload(
            pane,
            ProgramExit {
                status: -1,
                signal: Some(15),
                by_shux: false,
            },
        );
        assert_eq!(signal["signal"], 15);
        assert_eq!(signal["signal_name"], "SIGTERM");
        assert!(signal.get("exit_code").is_none());
        let unreaped = exit_payload(pane, ProgramExit::code(-1));
        assert_eq!(
            unreaped["outcome"], "unknown",
            "the -1 sentinel is not an exit code"
        );
        assert!(unreaped.get("exit_code").is_none());
    }

    use crate::rpc::test_harness::{RpcHarness, dispatch_err, dispatch_ok};

    fn wait(h: &RpcHarness, pane: PaneId) -> tokio::task::JoinHandle<Value> {
        let router = h.router.clone();
        tokio::spawn(async move {
            dispatch_ok(
                &router,
                "pane.wait_exit",
                json!({"pane_id": pane.to_string()}),
            )
            .await
        })
    }

    /// A pane the harness made, marked as having run a program.
    async fn running(h: &RpcHarness, pane: PaneId) -> PaneId {
        h.io.lock().await.incarnations.insert(pane, 1);
        pane
    }

    async fn subscribed(h: &RpcHarness, before: usize) {
        shux_testkit::wait_until_async("pane.wait_exit to subscribe", || async {
            h.bus.subscriber_count() > before
        })
        .await;
    }

    async fn outcome(waiter: tokio::task::JoinHandle<Value>) -> Value {
        tokio::time::timeout(shux_testkit::BUDGET, waiter)
            .await
            .expect("the wait never returned")
            .unwrap()
    }

    #[tokio::test]
    async fn an_id_that_never_named_a_pane_is_not_found_not_killed() {
        let h = RpcHarness::new();
        let err = dispatch_err(
            &h.router,
            "pane.wait_exit",
            json!({"pane_id": PaneId::new().to_string(), "timeout_ms": 0}),
        )
        .await;
        assert_eq!(err.code, shux_rpc::ErrorCode::NotFound.code(), "{err:?}");
        h.stop().await;
    }

    #[tokio::test]
    async fn an_exit_recorded_before_the_pane_was_removed_is_the_answer() {
        let h = RpcHarness::new();
        let (session, _w, pane) = h.seed_session("exit-then-kill").await;
        h.graph.set_pane_exit_status(pane, 7).await.unwrap();
        h.graph.destroy_session(session, None).await.unwrap();
        let got = dispatch_ok(
            &h.router,
            "pane.wait_exit",
            json!({"pane_id": pane.to_string(), "timeout_ms": 0}),
        )
        .await;
        assert_eq!(got["outcome"], "exited", "{got}");
        assert_eq!(got["exit_code"], 7, "{got}");
        h.stop().await;
    }

    #[tokio::test]
    async fn an_exit_and_removal_the_wait_lagged_past_is_still_the_answer() {
        use crate::lens_scratch::tests::{flood, lagging_bus};
        let h = RpcHarness::with_bus(lagging_bus());
        let (session, _w, pane) = h.seed_session("lagged").await;
        let events = h.bus.subscribe_filtered(vec!["pane.exited".to_string()]);
        let got = {
            let mut waiting =
                std::pin::pin!(wait_exit(&h.graph, &h.io, &h.bus, events, pane, None));
            // One poll: it reads a running pane and parks on the subscription.
            tokio::select! {
                biased;
                got = &mut waiting => panic!("a running pane answered: {got:?}"),
                () = std::future::ready(()) => {}
            }
            // Unpolled, it misses all of this: the exit is buried before it looks.
            h.graph.set_pane_exit_status(pane, 7).await.unwrap();
            h.graph.destroy_session(session, None).await.unwrap();
            flood(&h.bus, pane);
            tokio::time::timeout(shux_testkit::BUDGET, waiting)
                .await
                .expect("a lagged-away exit was never seen")
                .unwrap()
        };
        assert_eq!(
            (&got["outcome"], &got["exit_code"]),
            (&"exited".into(), &7.into()),
            "{got}"
        );
        h.stop().await;
    }

    #[tokio::test]
    async fn removing_a_running_pane_at_any_scope_ends_the_wait_as_killed() {
        for scope in ["pane", "window", "session"] {
            let h = RpcHarness::new();
            let (session, _w, first) = h.seed_session(scope).await;
            // A window keeps its last pane, and a session its last window.
            let pane = h
                .graph
                .split_pane(first, shux_core::layout::Direction::Vertical, 0.5)
                .await
                .unwrap();
            let window = h
                .graph
                .create_window(session, "second".into(), "/tmp".into())
                .await
                .unwrap();
            let pane = if scope == "window" {
                h.graph.snapshot().windows[&window].active_pane
            } else {
                pane
            };
            let pane = running(&h, pane).await;
            let before = h.bus.subscriber_count();
            let waiter = wait(&h, pane);
            subscribed(&h, before).await;
            match scope {
                "pane" => h.graph.destroy_pane(pane, None).await.unwrap(),
                "window" => h.graph.destroy_window(window, None).await.unwrap(),
                _ => h.graph.destroy_session(session, None).await.unwrap(),
            }
            let got = outcome(waiter).await;
            assert_eq!(got["outcome"], "killed", "{scope}: {got}");
            h.stop().await;
        }
    }

    #[tokio::test]
    async fn a_zero_timeout_on_a_running_pane_answers_at_once() {
        let h = RpcHarness::new();
        let (_s, _w, pane) = h.seed_session("zero").await;
        let pane = running(&h, pane).await;
        let got = dispatch_ok(
            &h.router,
            "pane.wait_exit",
            json!({"pane_id": pane.to_string(), "timeout_ms": 0}),
        )
        .await;
        assert_eq!(got["outcome"], "timeout", "{got}");
        h.stop().await;
    }

    #[tokio::test]
    async fn a_pane_whose_program_never_started_says_so_at_once() {
        let h = RpcHarness::new();
        let applied = dispatch_ok(
            &h.router,
            "state.apply",
            json!({"ops": [{"op": "create_session", "name": "never",
                "cwd": "/nonexistent/never", "initial_command": ["/no/such/binary"]}]}),
        )
        .await;
        assert_eq!(applied["spawn_results"][0]["spawned"], false, "{applied}");
        let pane = applied["outputs"][0]["pane_id"]
            .as_str()
            .unwrap()
            .to_string();
        let got = tokio::time::timeout(
            shux_testkit::BUDGET,
            dispatch_ok(&h.router, "pane.wait_exit", json!({"pane_id": pane})),
        )
        .await
        .expect("waited on a program that never ran");
        assert_eq!(got["outcome"], "not_started", "{got}");
        h.stop().await;
    }

    #[tokio::test]
    async fn a_pane_awaiting_its_first_spawn_is_waited_on_until_the_spawn_fails() {
        let h = RpcHarness::new();
        let (_s, _w, pane) = h.seed_session("pending").await;
        let got = dispatch_ok(
            &h.router,
            "pane.wait_exit",
            json!({"pane_id": pane.to_string(), "timeout_ms": 0}),
        )
        .await;
        assert_eq!(got["outcome"], "timeout", "a spawn may still come: {got}");
        let waiter = wait(&h, pane);
        h.io.lock().await.failed_spawns.insert(pane);
        let got = outcome(waiter).await;
        assert_eq!(got["outcome"], "not_started", "{got}");
        h.stop().await;
    }

    #[tokio::test]
    async fn a_respawned_pane_removed_while_running_is_killed_not_its_old_exit() {
        let h = RpcHarness::new();
        let (_s, _w, first) = h.seed_session("respawn").await;
        let pane = h
            .graph
            .split_pane(first, shux_core::layout::Direction::Vertical, 0.5)
            .await
            .unwrap();
        h.graph.claim_pane_pty(pane, 1).await.unwrap();
        h.graph
            .set_pane_exit_status_owned(pane, ProgramExit::code(7), 1, None)
            .await
            .unwrap();
        assert!(h.graph.claim_pane_pty(pane, 2).await.unwrap(), "respawned");
        h.graph.destroy_pane(pane, None).await.unwrap();
        assert_eq!(announced_exit(&h.bus, pane), Some(killed(pane)));
        h.stop().await;
    }

    #[tokio::test]
    async fn an_exit_whose_start_left_the_history_is_unknown_not_killed() {
        let h = RpcHarness::with_bus(EventBus::with_config(shux_core::bus::EventBusConfig {
            history_capacity: 4,
            ..Default::default()
        }));
        let (session, _w, pane) = h.seed_session("evicted").await;
        h.graph.set_pane_exit_status(pane, 7).await.unwrap();
        crate::lens_scratch::tests::flood(&h.bus, pane);
        h.graph.destroy_session(session, None).await.unwrap();
        let got = announced_exit(&h.bus, pane).expect("its removal is in history");
        assert_eq!(got["outcome"], "unknown", "the exit was evicted: {got}");
        h.stop().await;
    }
}
