//! `events.watch` and `events.history`.

use shux_rpc::{Policy, Sensitivity};

use crate::rpc::convert::event_to_json;
use crate::rpc::params::{required_str, resolve_pane_ref};

/// Register `events.watch` and `events.history` RPC methods.
///
/// `events.watch` is the agent-facing subscription: long-poll style, since
/// the JSON-RPC Handler trait is single-response. The handler:
///   1. Subscribes to the bus FIRST (so concurrent publishes can't slip
///      between history snapshot and subscription start — the race that
///      Codex and Gemini both flagged as the load-bearing correctness
///      requirement).
///   2. Snapshots history from `from_seq` SECOND.
///   3. Drains the subscription with `timeout_ms` until either a matching
///      event arrives or the deadline lapses.
///   4. Returns history + tail, deduped by `seq` (the overlap between the
///      two streams is real — an event published in step 2 might appear in
///      both the history snapshot and the subscription receiver buffer).
///
/// `events.history` is a simple bus.history_filtered() wrapper.
pub(crate) fn register_events_methods(
    builder: shux_rpc::RouterBuilder,
    bus: shux_core::bus::EventBus,
    // `pane.output.watch` takes a `pane_id`, and every id parameter has to
    // resolve short forms (issue #120) — which needs the graph.
    graph: shux_core::graph::GraphHandle,
) -> shux_rpc::RouterBuilder {
    let bus_watch = bus.clone();
    let bus_hist = bus.clone();
    let bus_pane_output = bus;
    let graph_pane_output = graph;

    builder
        .register_with_policy(
            "events.watch",
            Policy::param_aware(|params, plugin_id| {
                // Self-namespaced filters are Public — a plugin can
                // always watch its own published events. Anything broader
                // (firehose or other plugins' namespaces) is ContentRead
                // and needs an explicit grant.
                let prefix = format!("plugin.{plugin_id}.");
                let filters = params
                    .and_then(|p| p.get("filters"))
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|f| f.as_str())
                            .map(|s| s.to_string())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if !filters.is_empty() && filters.iter().all(|f| f.starts_with(&prefix)) {
                    Sensitivity::Public
                } else {
                    Sensitivity::ContentRead
                }
            }),
            move |params: Option<serde_json::Value>| {
                let bus = bus_watch.clone();
                async move {
                    let params = params.unwrap_or_default();

                    let from_seq = params.get("from_seq").and_then(|v| v.as_u64());
                    let max_events = params
                        .get("max_events")
                        .and_then(|v| v.as_u64())
                        .map(|n| n as usize)
                        .unwrap_or(100)
                        .min(1000);
                    let timeout_ms = params
                        .get("timeout_ms")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(5_000)
                        .min(30_000);
                    let filters: Vec<String> = params
                        .get("filter")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();

                    // 1. Subscribe FIRST so any publish during step 2 lands in the
                    //    receiver buffer, not the void.
                    let mut sub = bus.subscribe_filtered(filters.clone());

                    // 2. Snapshot history from from_seq.
                    let (history, gap) = match from_seq {
                        Some(s) => {
                            let (events, gap) = bus.events_from_seq(s);
                            let filtered: Vec<_> = if filters.is_empty() {
                                events
                            } else {
                                events
                                    .into_iter()
                                    .filter(|e| filters.iter().any(|f| e.matches_filter(f)))
                                    .collect()
                            };
                            (filtered, gap)
                        }
                        None => (Vec::new(), 0),
                    };

                    let mut collected: Vec<shux_core::event::Event> = history;
                    let mut lagged = false;

                    // 3. Tail: drain up to (max_events - history_len) events from
                    //    the subscription with timeout. If from_seq was None and
                    //    we have no history, block until at least one event or
                    //    timeout. If we already have history, just opportunistically
                    //    grab anything queued without blocking past the deadline.
                    let deadline =
                        tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
                    while collected.len() < max_events {
                        let now = tokio::time::Instant::now();
                        if now >= deadline {
                            break;
                        }
                        let remaining = deadline - now;
                        match tokio::time::timeout(remaining, sub.recv()).await {
                            Ok(Some(shux_core::bus::SubscriptionEvent::Event(e))) => {
                                collected.push(e)
                            }
                            Ok(Some(shux_core::bus::SubscriptionEvent::Lagged(_))) => {
                                // Subscriber fell behind broadcast capacity. Surface
                                // to the client so it knows the stream is degraded.
                                lagged = true;
                                break;
                            }
                            Ok(None) => break, // bus shut down
                            Err(_) => break,   // deadline reached
                        }
                    }

                    // 4. Dedup by seq. History + subscription tail can legitimately
                    //    overlap; the subscription started before history was
                    //    snapshotted, so any event published in between can land in
                    //    both streams.
                    collected.sort_by_key(|e| e.meta.seq);
                    collected.dedup_by_key(|e| e.meta.seq);
                    if collected.len() > max_events {
                        collected.truncate(max_events);
                    }

                    let next_seq = collected
                        .last()
                        .map(|e| e.meta.seq + 1)
                        .or(from_seq)
                        .unwrap_or_else(|| bus.current_seq());

                    let events: Vec<serde_json::Value> =
                        collected.iter().map(event_to_json).collect();

                    Ok(serde_json::json!({
                        "events": events,
                        "next_seq": next_seq,
                        "gap": gap,
                        "lagged": lagged,
                    }))
                }
            },
        )
        .register_with_policy(
            "events.history",
            Policy::fixed(Sensitivity::ContentRead),
            move |params: Option<serde_json::Value>| {
                let bus = bus_hist.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let count = params
                        .get("count")
                        .and_then(|v| v.as_u64())
                        .map(|n| n as usize)
                        .unwrap_or(50)
                        .min(1000);
                    let filters: Vec<String> = params
                        .get("filter")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();

                    let events = bus.history_filtered(count, &filters);
                    let json: Vec<serde_json::Value> = events.iter().map(event_to_json).collect();

                    Ok(serde_json::json!({
                        "events": json,
                        "current_seq": bus.current_seq(),
                    }))
                }
            },
        )
        .register_with_policy(
            "pane.output.watch",
            Policy::fixed(Sensitivity::ContentRead),
            // PR 2c — sampled pane.output data-plane watch.
            //
            // Long-polls the data-plane broadcast channel for chunks
            // matching the given `pane_id`. Unlike `events.watch`,
            // there is no history snapshot — the data plane is
            // intentionally lossy to prevent secret leak via stored
            // PTY bytes and to give control-plane subscribers
            // priority. See `docs/PR2c-DESIGN.md`.
            move |params: Option<serde_json::Value>| {
                let bus = bus_pane_output.clone();
                let gh = graph_pane_output.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let pane_id = resolve_pane_ref(&gh, required_str(&params, "pane_id")?)?;
                    let from_seq = params.get("from_seq").and_then(|v| v.as_u64());
                    let timeout_ms = params
                        .get("timeout_ms")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(5_000)
                        .clamp(100, 30_000);
                    let limit = params
                        .get("limit")
                        .and_then(|v| v.as_u64())
                        .map(|n| n as usize)
                        .unwrap_or(50)
                        .min(500);

                    // Subscribe BEFORE returning any chunks so a chunk
                    // published while we're parsing params doesn't get
                    // missed. The data plane has no history, so the
                    // subscribe-first invariant from events.watch
                    // applies even more strictly here.
                    let mut sub = bus.subscribe_pane_output();

                    let mut collected: Vec<shux_core::bus::PaneOutputEvent> = Vec::new();
                    let mut lagged = false;
                    let deadline =
                        tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);

                    while collected.len() < limit {
                        let now = tokio::time::Instant::now();
                        if now >= deadline {
                            break;
                        }
                        let remaining = deadline - now;
                        match tokio::time::timeout(remaining, sub.recv()).await {
                            Ok(Some(shux_core::bus::PaneOutputSubscriptionEvent::Chunk(c))) => {
                                if c.pane_id != pane_id {
                                    continue; // not for this subscriber
                                }
                                if let Some(s) = from_seq
                                    && c.seq < s
                                {
                                    continue;
                                }
                                collected.push(c);
                            }
                            Ok(Some(shux_core::bus::PaneOutputSubscriptionEvent::Lagged(_))) => {
                                lagged = true;
                                break;
                            }
                            Ok(None) => break,
                            Err(_) => break,
                        }
                    }

                    let next_seq = collected
                        .last()
                        .map(|c| c.seq + 1)
                        .or(from_seq)
                        .unwrap_or_else(|| bus.current_data_seq());

                    let chunks: Vec<serde_json::Value> = collected
                        .into_iter()
                        .map(|c| {
                            serde_json::json!({
                                "seq": c.seq,
                                "pane_id": c.pane_id.to_string(),
                                "window_id": c.window_id.to_string(),
                                "session_id": c.session_id.to_string(),
                                "timestamp": c
                                    .timestamp
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_millis() as u64)
                                    .unwrap_or(0),
                                "bytes": c.bytes,
                                "sampled": c.sampled,
                            })
                        })
                        .collect();

                    Ok(serde_json::json!({
                        "chunks": chunks,
                        "next_seq": next_seq,
                        "lagged": lagged,
                    }))
                }
            },
        )
}

#[cfg(test)]
mod tests {

    use crate::rpc::test_harness::{RpcHarness, dispatch_err, dispatch_ok};
    use std::time::Duration;

    #[tokio::test]
    async fn production_events_routes_filter_history_and_live_data_plane() {
        let harness = RpcHarness::new();
        let (session_id, window_id, pane_id) = harness.seed_session("events").await;

        let seq = harness
            .bus
            .publish(shux_core::event::EventData::PluginEvent {
                plugin_id: "mine".to_string(),
                event_type: "tick".to_string(),
                data: serde_json::json!({"ok": true}),
            });
        harness
            .bus
            .publish(shux_core::event::EventData::PluginEvent {
                plugin_id: "other".to_string(),
                event_type: "tick".to_string(),
                data: serde_json::json!({"ok": false}),
            });

        let history = dispatch_ok(
            &harness.router,
            "events.history",
            serde_json::json!({"filter": ["plugin.mine."], "count": 10}),
        )
        .await;
        let events = history["events"].as_array().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "plugin.mine.tick");

        let watched = dispatch_ok(
            &harness.router,
            "events.watch",
            serde_json::json!({"from_seq": seq, "filter": ["plugin.mine."], "max_events": 5, "timeout_ms": 25}),
        )
        .await;
        assert_eq!(watched["events"].as_array().unwrap().len(), 1);
        assert_eq!(watched["next_seq"], seq + 1);
        assert_eq!(watched["lagged"], false);

        let router = harness.router.clone();
        let pane_str = pane_id.to_string();
        let watch = tokio::spawn(async move {
            router
                .dispatch(
                    "pane.output.watch",
                    Some(serde_json::json!({
                        "pane_id": pane_str,
                        "timeout_ms": 500,
                        "limit": 2,
                    })),
                )
                .await
                .unwrap()
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        let output_seq = harness.bus.publish_pane_output(
            pane_id,
            window_id,
            session_id,
            "aGVsbG8=".to_string(),
            false,
        );
        let output = watch.await.unwrap();
        assert_eq!(output["chunks"][0]["seq"], output_seq);
        assert_eq!(output["chunks"][0]["bytes"], "aGVsbG8=");
        assert_eq!(output["chunks"][0]["sampled"], false);

        let missing_pane = dispatch_err(
            &harness.router,
            "pane.output.watch",
            serde_json::json!({"timeout_ms": 100}),
        )
        .await;
        assert_eq!(missing_pane.code, shux_rpc::ErrorCode::InvalidParams.code());

        harness.stop().await;
    }
}
