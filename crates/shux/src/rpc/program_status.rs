//! `pane.status` and `pane.wait_status`: OSC 7501 program status over RPC.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use shux_core::bus::{EventBus, SubscriptionEvent};
use shux_core::event::EventData;
use shux_core::graph::GraphHandle;
use shux_core::model::PaneId;
use shux_program_status::{State, StatusSummary};
use shux_rpc::RpcError;
use shux_rpc::policy::{Policy, Sensitivity};
use tokio::sync::Mutex;

use crate::pane_io::PaneIoState;
use crate::rpc::params::resolve_pane_id_from_params;

const WAIT_DEFAULT_MS: u64 = 30_000;
const WAIT_MAX_MS: u64 = 600_000;
/// Re-read even without an event: installing a VT that matches the graph's
/// defaults (a disabled pane) changes nothing the graph would announce.
const WAIT_REPOLL: Duration = Duration::from_millis(250);

pub(crate) fn register_program_status_methods(
    builder: shux_rpc::RouterBuilder,
    graph: GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    bus: EventBus,
) -> shux_rpc::RouterBuilder {
    let (g1, io1) = (graph.clone(), io_state);
    let (g2, io2) = (graph, io1.clone());
    builder
        .register_with_policy(
            "pane.status",
            Policy::fixed(Sensitivity::ContentRead),
            move |params: Option<Value>| {
                let (gh, io) = (g1.clone(), io1.clone());
                async move {
                    let params = params.unwrap_or_default();
                    let pane_id = resolve_pane_id_from_params(&gh, &params)?;
                    let state = io.lock().await;
                    let vt = state
                        .vts
                        .get(&pane_id)
                        .ok_or_else(|| RpcError::not_found("pane VT", &pane_id.to_string()))?;
                    // One lock hold: summary and records describe the same instant.
                    let publication = vt.program_status_publication();
                    let records = vt
                        .program_status_records()
                        .map(|r| r.records)
                        .unwrap_or_default();
                    Ok(json!({
                        "pane_id": pane_id.to_string(),
                        "enabled": publication.enabled,
                        "seen": publication.seen,
                        "generation": publication.generation,
                        "summary": publication.summary,
                        "records": records,
                    }))
                }
            },
        )
        .register_with_policy(
            "pane.wait_status",
            Policy::fixed(Sensitivity::ContentRead),
            move |params: Option<Value>| {
                let (gh, io, bus) = (g2.clone(), io2.clone(), bus.clone());
                async move {
                    let params = params.unwrap_or_default();
                    let wait = WaitParams::parse(&params)?;
                    let pane_id = resolve_pane_id_from_params(&gh, &params)?;
                    wait_status(&gh, &io, &bus, pane_id, &wait).await
                }
            },
        )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Match {
    /// The pane's most urgent record is in the set.
    Top,
    /// Any record is in the set.
    Any,
}

#[derive(Debug)]
struct WaitParams {
    states: Vec<State>,
    matching: Match,
    timeout: Duration,
}

impl WaitParams {
    fn parse(params: &Value) -> Result<Self, RpcError> {
        let states: Vec<State> = match params.get("state") {
            None | Some(Value::Null) => {
                return Err(RpcError::invalid_params(
                    "state is required: one or more of idle, working, done, blocked, error",
                ));
            }
            Some(Value::String(s)) => vec![parse_state(s)?],
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| {
                    v.as_str()
                        .ok_or_else(|| RpcError::invalid_params("state entries must be strings"))
                        .and_then(parse_state)
                })
                .collect::<Result<_, _>>()?,
            Some(_) => {
                return Err(RpcError::invalid_params(
                    "state must be a string or an array",
                ));
            }
        };
        if states.is_empty() {
            return Err(RpcError::invalid_params(
                "state must name at least one state",
            ));
        }
        let matching = match params.get("match") {
            None | Some(Value::Null) => Match::Top,
            Some(Value::String(m)) if m == "top" => Match::Top,
            Some(Value::String(m)) if m == "any" => Match::Any,
            Some(other) => {
                return Err(RpcError::invalid_params(&format!(
                    "match must be \"top\" or \"any\", not {other}"
                )));
            }
        };
        let timeout_ms = match params.get("timeout_ms") {
            None | Some(Value::Null) => WAIT_DEFAULT_MS,
            Some(v) => v.as_u64().ok_or_else(|| {
                RpcError::invalid_params("timeout_ms must be a non-negative integer")
            })?,
        };
        if timeout_ms > WAIT_MAX_MS {
            return Err(RpcError::invalid_params(&format!(
                "timeout_ms must be at most {WAIT_MAX_MS}"
            )));
        }
        Ok(Self {
            states,
            matching,
            timeout: Duration::from_millis(timeout_ms),
        })
    }

    fn matches(&self, summary: &StatusSummary) -> bool {
        match self.matching {
            Match::Top => self.states.contains(&summary.state),
            Match::Any => self.states.iter().any(|s| count(summary, *s) > 0),
        }
    }
}

fn parse_state(s: &str) -> Result<State, RpcError> {
    serde_json::from_value(Value::String(s.to_string())).map_err(|_| {
        RpcError::invalid_params(&format!(
            "unknown state {s:?}: expected idle, working, done, blocked or error"
        ))
    })
}

fn count(summary: &StatusSummary, state: State) -> u16 {
    let c = &summary.counts;
    match state {
        State::Idle => c.idle,
        State::Working => c.working,
        State::Done => c.done,
        State::Blocked => c.blocked,
        State::Error => c.error,
    }
}

/// Outcome, or `None` to keep waiting. Off comes from the terminal itself:
/// the graph's flag is only set once the first publication lands, and a pane
/// that has not published yet is not a disabled one.
fn decide(
    graph: &GraphHandle,
    terminal_enabled: Option<bool>,
    pane_id: PaneId,
    wait: &WaitParams,
) -> Option<Value> {
    let snap = graph.snapshot();
    let Some(pane) = snap.panes.get(&pane_id) else {
        return Some(json!({"outcome": "pane_gone", "pane_id": pane_id.to_string()}));
    };
    let matched = pane.program_status.as_ref().filter(|s| wait.matches(s)).map(|summary| {
        json!({"outcome": "matched", "pane_id": pane_id.to_string(), "summary": summary})
    });
    // An exited pane's last status was committed with its exit; that is final.
    if let Some(code) = pane.exit_status {
        return Some(matched.unwrap_or_else(
            || json!({"outcome": "pane_gone", "pane_id": pane_id.to_string(), "exit_status": code}),
        ));
    }
    if terminal_enabled == Some(false) {
        return Some(json!({"outcome": "disabled", "pane_id": pane_id.to_string()}));
    }
    matched
}

async fn wait_status(
    graph: &GraphHandle,
    io: &Mutex<PaneIoState>,
    bus: &EventBus,
    pane_id: PaneId,
    wait: &WaitParams,
) -> Result<Value, RpcError> {
    // Subscribe before the first read, so no change can fall between them.
    let mut events = bus.subscribe_filtered(vec![
        "pane.status_changed".to_string(),
        "pane.exited".to_string(),
    ]);
    let deadline = tokio::time::Instant::now() + wait.timeout;
    loop {
        let terminal_enabled = io
            .lock()
            .await
            .vts
            .get(&pane_id)
            .map(|vt| vt.program_status_enabled());
        if let Some(outcome) = decide(graph, terminal_enabled, pane_id, wait) {
            return Ok(outcome);
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(json!({"outcome": "timeout", "pane_id": pane_id.to_string()}));
        }
        // Wait for something about this pane, a gap, or the re-read instant,
        // then re-read. The instant is fixed per pass: other panes' events
        // must not push it out.
        let wake = (tokio::time::Instant::now() + WAIT_REPOLL).min(deadline);
        loop {
            match tokio::time::timeout_at(wake, events.recv()).await {
                Err(_) => break,
                Ok(None) => return Err(RpcError::internal("event bus closed")),
                Ok(Some(SubscriptionEvent::Lagged(_))) => break,
                Ok(Some(SubscriptionEvent::Event(e))) => match e.data {
                    EventData::PaneStatusChanged { pane_id: p, .. }
                    | EventData::PaneExited { pane_id: p, .. }
                        if p == pane_id =>
                    {
                        break;
                    }
                    // A stream of other panes' events must not outlast `wake`.
                    _ if tokio::time::Instant::now() >= wake => break,
                    _ => {}
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(v: Value) -> Result<WaitParams, RpcError> {
        WaitParams::parse(&v)
    }

    #[test]
    fn wait_params_need_a_known_state() {
        assert!(params(json!({})).is_err());
        assert!(params(json!({"state": []})).is_err());
        assert!(params(json!({"state": "sleeping"})).is_err());
        assert!(params(json!({"state": ["done", 3]})).is_err());
        assert!(params(json!({"state": "done", "match": "most"})).is_err());
        assert!(
            params(json!({"state": "done", "match": 5})).is_err(),
            "non-string match read as top"
        );
        assert!(params(json!({"state": "done", "match": ["any"]})).is_err());
        assert!(params(json!({"state": "done", "timeout_ms": -1})).is_err());
        assert!(params(json!({"state": "done", "timeout_ms": WAIT_MAX_MS + 1})).is_err());
        let p = params(json!({"state": ["blocked", "error"], "match": "any", "timeout_ms": 5}))
            .unwrap();
        assert_eq!(p.states, [State::Blocked, State::Error]);
        assert_eq!(p.matching, Match::Any);
        assert_eq!(p.timeout, Duration::from_millis(5));
    }

    #[test]
    fn top_matches_the_most_urgent_record_and_any_matches_counts() {
        let mut summary = StatusSummary {
            state: State::Blocked,
            kind: None,
            progress: None,
            app: None,
            id: None,
            counts: Default::default(),
        };
        summary.counts.blocked = 1;
        summary.counts.working = 2;
        let top = params(json!({"state": "working"})).unwrap();
        let any = params(json!({"state": "working", "match": "any"})).unwrap();
        assert!(!top.matches(&summary));
        assert!(any.matches(&summary));
        assert!(
            params(json!({"state": "blocked"}))
                .unwrap()
                .matches(&summary)
        );
    }
}
