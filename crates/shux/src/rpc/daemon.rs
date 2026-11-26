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

/// Notes the build a call's `client` param names, if it is a newer release:
/// `true` then.
pub(crate) fn note_client(params: Option<&Value>) -> bool {
    let Some(version) = params
        .and_then(|p| p.get("client"))
        .and_then(|c| c.get("version"))
        .and_then(|v| v.as_str())
    else {
        return false;
    };
    let Some(seen) = crate::client::semver(version) else {
        return false;
    };
    // As this daemon says it is: clients compare against the same answer.
    let answer = shux_rpc::server::version_answer();
    let ours = answer
        .get("version")
        .and_then(|v| v.as_str())
        .and_then(crate::client::semver);
    if ours.is_none_or(|ours| seen <= ours) {
        return false;
    }
    let mut newest = SUPERSEDED_BY.lock().unwrap_or_else(|e| e.into_inner());
    let newer = newest
        .as_deref()
        .and_then(crate::client::semver)
        .is_none_or(|was| seen > was);
    if newer {
        // Only the numbers: what any caller sends is not shown verbatim.
        let (major, minor, patch) = seen;
        *newest = Some(format!("{major}.{minor}.{patch}"));
    }
    true
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
                // Only the daemon's own socket speaks for a CLI: not a plugin.
                let owner = shux_rpc::current_transport() == Some(shux_rpc::Transport::Uds);
                let newer = owner && note_client(params.as_ref());
                let graph = g_version.clone();
                async move {
                    // A newer CLI exists: switch to it once this build's sessions end.
                    if newer {
                        graph
                            .atomically(|g| g.retire_when_empty_if_busy())
                            .await
                            .map_err(graph_error_to_rpc)?;
                    }
                    let snap = graph.snapshot();
                    let mut answer = shux_rpc::server::version_answer();
                    if answer.get("version").is_some() {
                        let capabilities: serde_json::Map<String, Value> =
                            crate::rpc::accepts::ACCEPTS
                                .iter()
                                .map(|(m, keys)| ((*m).to_string(), json!(keys)))
                                .collect();
                        answer["capabilities"] = Value::Object(capabilities);
                        answer["superseded_by"] = json!(superseded_by());
                        answer["upgrade_pending"] = json!(snap.retire_pending && !snap.retired);
                        answer["build"] = crate::kept_build::advertised();
                    }
                    Ok(answer)
                }
            },
        )
        .register_to_completion(
            shux_rpc::UPGRADE,
            Policy::fixed(Sensitivity::PluginsForbidden),
            move |params: Option<Value>| {
                let gh = graph.clone();
                let cancel = cancel.clone();
                async move {
                    let newer = note_client(params.as_ref());
                    let when = params
                        .as_ref()
                        .and_then(|p| p.get("when"))
                        .and_then(|w| w.as_str())
                        .unwrap_or("now");
                    // A newer CLI arms it anyway, as `system.version` does.
                    let deferred = match when {
                        "now" => newer,
                        "idle" => true,
                        other => {
                            return Err(RpcError::invalid_params(&format!(
                                "'when' is \"now\" or \"idle\", not {other:?}"
                            )));
                        }
                    };
                    // One graph step: no session lands between the count and
                    // the refusal of every later one.
                    let (outcome, pending) = gh
                        .atomically(move |g| {
                            let outcome = if deferred {
                                g.retire_when_empty()
                            } else {
                                g.retire_if_empty()
                            };
                            (outcome, g.current().retire_pending)
                        })
                        .await
                        .map_err(graph_error_to_rpc)?;
                    Ok(match outcome {
                        Ok(()) => {
                            let _ =
                                tokio::task::spawn_blocking(crate::kept_build::mark_retiring).await;
                            cancel.cancel();
                            json!({ "retired": true, "pending": false, "sessions": 0 })
                        }
                        Err(n) => json!({ "retired": false, "pending": pending, "sessions": n }),
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
                    let _ = tokio::task::spawn_blocking(crate::kept_build::mark_retiring).await;
                    cancel.cancel();
                    return;
                }
            }
        }
    }
}
