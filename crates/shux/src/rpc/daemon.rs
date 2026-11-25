//! The daemon's own build: what it tells a client of another build, and how
//! it hands over to the build a newer client asks for.

use std::sync::Mutex;

use serde_json::{Value, json};
use shux_rpc::{Policy, RpcError, Sensitivity};

use crate::rpc::convert::graph_error_to_rpc;

/// The newest release of a CLI that has talked to this daemon, when it is a
/// newer release than the daemon: the status bar shows it.
static SUPERSEDED_BY: Mutex<Option<String>> = Mutex::new(None);

/// See [`SUPERSEDED_BY`].
pub(crate) fn superseded_by() -> Option<String> {
    SUPERSEDED_BY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Notes the build a call's `client` param names, if it is a newer release.
pub(crate) fn note_client(params: Option<&Value>) {
    let Some(version) = params
        .and_then(|p| p.get("client"))
        .and_then(|c| c.get("version"))
        .and_then(|v| v.as_str())
    else {
        return;
    };
    let Some(seen) = crate::client::semver(version) else {
        return;
    };
    if crate::client::semver(shux_rpc::VERSION).is_none_or(|ours| seen <= ours) {
        return;
    }
    let mut newest = SUPERSEDED_BY.lock().unwrap_or_else(|e| e.into_inner());
    let newer = newest
        .as_deref()
        .and_then(crate::client::semver)
        .is_none_or(|was| seen > was);
    if newer {
        *newest = Some(version.to_string());
    }
}

pub(crate) fn register_daemon_methods(
    builder: shux_rpc::RouterBuilder,
    graph: shux_core::graph::GraphHandle,
    cancel: tokio_util::sync::CancellationToken,
) -> shux_rpc::RouterBuilder {
    let g_version = graph.clone();
    builder
        // Replaces the builtin: a client of another build learns what this
        // build serves, and whether it is on its way out.
        .register_with_policy(
            "system.version",
            Policy::fixed(Sensitivity::Public),
            move |params: Option<Value>| {
                note_client(params.as_ref());
                let snap = g_version.snapshot();
                let mut answer = shux_rpc::server::version_answer();
                if answer.get("version").is_some() {
                    let capabilities: serde_json::Map<String, Value> = crate::rpc::accepts::ACCEPTS
                        .iter()
                        .map(|(m, keys)| ((*m).to_string(), json!(keys)))
                        .collect();
                    answer["capabilities"] = Value::Object(capabilities);
                    answer["superseded_by"] = json!(superseded_by());
                    answer["upgrade_pending"] = json!(snap.retire_pending && !snap.retired);
                    answer["build"] = crate::kept_build::advertised();
                }
                async move { Ok(answer) }
            },
        )
        .register_to_completion(
            shux_rpc::UPGRADE,
            Policy::fixed(Sensitivity::PluginsForbidden),
            move |params: Option<Value>| {
                let gh = graph.clone();
                let cancel = cancel.clone();
                async move {
                    note_client(params.as_ref());
                    let when = params
                        .as_ref()
                        .and_then(|p| p.get("when"))
                        .and_then(|w| w.as_str())
                        .unwrap_or("now");
                    let deferred = match when {
                        "now" => false,
                        "idle" => true,
                        other => {
                            return Err(RpcError::invalid_params(&format!(
                                "'when' is \"now\" or \"idle\", not {other:?}"
                            )));
                        }
                    };
                    // One graph step: no session lands between the count and
                    // the refusal of every later one.
                    let outcome = gh
                        .atomically(move |g| {
                            if deferred {
                                g.retire_when_empty()
                            } else {
                                g.retire_if_empty()
                            }
                        })
                        .await
                        .map_err(graph_error_to_rpc)?;
                    Ok(match outcome {
                        Ok(()) => {
                            cancel.cancel();
                            json!({ "retired": true, "pending": false, "sessions": 0 })
                        }
                        Err(n) => json!({ "retired": false, "pending": deferred, "sessions": n }),
                    })
                }
            },
        )
        .owner_local(shux_rpc::UPGRADE)
}

/// Ends the daemon once a pending upgrade has retired the graph: the step
/// that removed the last session set `retired`, and its event wakes this.
pub(crate) async fn retire_when_idle(
    graph: shux_core::graph::GraphHandle,
    bus: shux_core::bus::EventBus,
    cancel: tokio_util::sync::CancellationToken,
) {
    let mut events = bus.subscribe_filtered(vec!["session.killed".to_string()]);
    loop {
        tokio::select! {
            () = cancel.cancelled() => return,
            event = events.recv() => {
                if event.is_none() {
                    return;
                }
                // A lagged subscription is still a wake: the graph decides.
                if graph.snapshot().retired {
                    tracing::info!("last session ended with an upgrade pending; retiring");
                    cancel.cancel();
                    return;
                }
            }
        }
    }
}
