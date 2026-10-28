//! `events.watch` and `events.history`.

use shux_rpc::{Policy, Sensitivity};

use crate::rpc::convert::event_to_json;
use crate::rpc::params::{required_str, resolve_pane_ref, resolve_session_ref, resolve_window_ref};

/// Self-namespaced filters are Public — a plugin can always watch its own
/// published events. Anything broader (firehose or other plugins'
/// namespaces) is ContentRead and needs an explicit grant.
fn watch_sensitivity(params: Option<&serde_json::Value>, plugin_id: &str) -> Sensitivity {
    let prefix = format!("plugin.{plugin_id}.");
    let filters = event_filters(params);
    if !filters.is_empty() && filters.iter().all(|f| f.starts_with(&prefix)) {
        Sensitivity::Public
    } else {
        Sensitivity::ContentRead
    }
}

/// Entity a caller named with `pane_id` / `window_id` / `session_id`. The
/// permission check authorizes a scoped grant against these ids, so the
/// handlers must return only events about them, never the whole bus.
struct EventScope {
    pane: Option<String>,
    window: Option<String>,
    session: Option<String>,
}

impl EventScope {
    fn from_params(
        gh: &shux_core::graph::GraphHandle,
        params: &serde_json::Value,
    ) -> Result<Self, shux_rpc::RpcError> {
        let id = |key: &str| params.get(key).and_then(|v| v.as_str());
        Ok(Self {
            pane: id("pane_id")
                .map(|s| resolve_pane_ref(gh, s).map(|p| p.to_string()))
                .transpose()?,
            window: id("window_id")
                .map(|s| resolve_window_ref(gh, s, "window_id").map(|w| w.to_string()))
                .transpose()?,
            session: id("session_id")
                .map(|s| resolve_session_ref(gh, s, "session_id").map(|x| x.to_string()))
                .transpose()?,
        })
    }

    fn is_all(&self) -> bool {
        self.pane.is_none() && self.window.is_none() && self.session.is_none()
    }

    fn admits(&self, e: &shux_core::event::Event) -> bool {
        if self.is_all() {
            return true;
        }
        let data = serde_json::to_value(&e.data).unwrap_or_default();
        let field = |key: &str| {
            data.pointer(&format!("/data/{key}"))
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        };
        [
            (&self.pane, "pane_id"),
            (&self.window, "window_id"),
            (&self.session, "session_id"),
        ]
        .into_iter()
        .all(|(want, key)| want.is_none() || field(key) == *want)
    }
}

/// How long a long poll's next receive may block: until the deadline while it
/// has nothing to return, not at all once it has something. `None` once the
/// deadline has passed.
fn next_wait(deadline: tokio::time::Instant, have_any: bool) -> Option<std::time::Duration> {
    let remaining = deadline.checked_duration_since(tokio::time::Instant::now())?;
    if remaining.is_zero() {
        return None;
    }
    Some(if have_any {
        std::time::Duration::ZERO
    } else {
        remaining
    })
}

/// The `filter` list both the policy and the handlers act on. One parser,
/// so the tier is always decided on what the subscription will deliver.
fn event_filters(params: Option<&serde_json::Value>) -> Vec<String> {
    params
        .and_then(|p| p.get("filter"))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

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
///      event arrives or the deadline lapses, then takes only what is queued.
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
    let graph_watch = graph.clone();
    let graph_hist = graph.clone();
    let graph_pane_output = graph;

    builder
        .register_with_policy(
            "events.watch",
            Policy::param_aware(watch_sensitivity),
            move |params: Option<serde_json::Value>| {
                let (bus, gh) = (bus_watch.clone(), graph_watch.clone());
                async move {
                    let params = params.unwrap_or_default();
                    let scope = EventScope::from_params(&gh, &params)?;

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
                    let filters = event_filters(Some(&params));

                    // 1. Subscribe FIRST so any publish during step 2 lands in the
                    //    receiver buffer, not the void.
                    let mut sub = bus.subscribe_filtered(filters.clone());
                    // Every event from here on reaches `sub`, so this is where an
                    // empty result resumes.
                    let head = bus.current_seq();

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

                    let mut collected: Vec<shux_core::event::Event> =
                        history.into_iter().filter(|e| scope.admits(e)).collect();
                    let mut lagged = false;

                    // 3. Tail: block until the first admitted event or the
                    //    deadline. Once anything is collected, take only what is
                    //    already queued and return; a caller that wants more
                    //    calls again with `next_seq`.
                    let deadline =
                        tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
                    while collected.len() < max_events {
                        let Some(wait) = next_wait(deadline, !collected.is_empty()) else {
                            break;
                        };
                        match tokio::time::timeout(wait, sub.recv()).await {
                            Ok(Some(shux_core::bus::SubscriptionEvent::Event(e))) => {
                                if scope.admits(&e) {
                                    collected.push(e)
                                }
                            }
                            Ok(Some(shux_core::bus::SubscriptionEvent::Lagged(_))) => {
                                // Subscriber fell behind broadcast capacity. Surface
                                // to the client so it knows the stream is degraded.
                                lagged = true;
                                break;
                            }
                            Ok(None) => break, // bus shut down
                            Err(_) => break,   // deadline reached, or nothing queued
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
                        .unwrap_or(head);

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
                let (bus, gh) = (bus_hist.clone(), graph_hist.clone());
                async move {
                    let params = params.unwrap_or_default();
                    let scope = EventScope::from_params(&gh, &params)?;
                    let count = params
                        .get("count")
                        .and_then(|v| v.as_u64())
                        .map(|n| n as usize)
                        .unwrap_or(50)
                        .min(1000);
                    let filters = event_filters(Some(&params));

                    // Scope first, so `count` counts only the scope's events.
                    let events: Vec<_> = if scope.is_all() {
                        bus.history_filtered(count, &filters)
                    } else {
                        let mut scoped: Vec<_> = bus
                            .history_filtered(usize::MAX, &filters)
                            .into_iter()
                            .filter(|e| scope.admits(e))
                            .collect();
                        scoped.drain(..scoped.len().saturating_sub(count));
                        scoped
                    };
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
                        let Some(wait) = next_wait(deadline, !collected.is_empty()) else {
                            break;
                        };
                        match tokio::time::timeout(wait, sub.recv()).await {
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
    /// The tier is decided from the same filters the subscription uses.
    /// Classifying one key and subscribing on another let a plugin pass a
    /// self-scoped `filters` and receive every event in the daemon.
    #[test]
    fn watch_is_classified_on_the_filter_it_subscribes_with() {
        use serde_json::json;
        use shux_rpc::Sensitivity;
        let tier = |p: serde_json::Value| super::watch_sensitivity(Some(&p), "p");
        assert_eq!(tier(json!({"filter": ["plugin.p."]})), Sensitivity::Public);
        assert_eq!(
            tier(json!({"filters": ["plugin.p."]})),
            Sensitivity::ContentRead,
            "an unknown key subscribes to the firehose"
        );
        assert_eq!(
            tier(json!({"filters": ["plugin.p."], "filter": ["pane.status_changed"]})),
            Sensitivity::ContentRead
        );
        assert_eq!(
            tier(json!({"filter": ["plugin.p.", "pane."]})),
            Sensitivity::ContentRead
        );
        assert_eq!(tier(json!({})), Sensitivity::ContentRead);
    }

    use crate::rpc::test_harness::{RpcHarness, dispatch_err, dispatch_ok};
    use std::time::Duration;

    /// A scoped grant authorizes events about its own pane only: naming a
    /// pane must narrow the result, never return every pane's status.
    #[tokio::test]
    async fn events_named_for_a_pane_return_only_that_panes_events() {
        let harness = RpcHarness::new();
        let (_s, _w, a) = harness.seed_session("scope-a").await;
        let (_s, _w, b) = harness.seed_session("scope-b").await;
        let snap = harness.graph.snapshot();
        for pane in [a, b] {
            let p = &snap.panes[&pane];
            harness
                .bus
                .publish(shux_core::event::EventData::PaneStatusChanged {
                    pane_id: pane,
                    window_id: p.window_id,
                    session_id: snap.windows[&p.window_id].session_id,
                    old: None,
                    new: None,
                    seen: true,
                    enabled: true,
                });
        }
        let panes = |v: &serde_json::Value| -> Vec<String> {
            v["events"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| {
                    e["data"]["data"]["pane_id"]
                        .as_str()
                        .unwrap_or("")
                        .to_string()
                })
                .collect()
        };
        let history = dispatch_ok(
            &harness.router,
            "events.history",
            serde_json::json!({"pane_id": a.to_string(), "filter": ["pane.status_changed"], "count": 50}),
        )
        .await;
        assert_eq!(
            panes(&history),
            [a.to_string()],
            "events.history leaked another pane"
        );
        let watched = dispatch_ok(
            &harness.router,
            "events.watch",
            serde_json::json!({"pane_id": a.to_string(), "filter": ["pane.status_changed"], "from_seq": 1, "timeout_ms": 50}),
        )
        .await;
        assert_eq!(
            panes(&watched),
            [a.to_string()],
            "events.watch leaked another pane"
        );
        harness.stop().await;
    }

    /// `count` is how many of the named pane's events come back, however many
    /// newer events other panes have.
    #[tokio::test]
    async fn a_scoped_history_counts_only_the_scoped_events() {
        let harness = RpcHarness::new();
        let (_s, _w, a) = harness.seed_session("count-a").await;
        let (_s, _w, b) = harness.seed_session("count-b").await;
        let snap = harness.graph.snapshot();
        for pane in [a, a, b, b, b, b, b] {
            let p = &snap.panes[&pane];
            harness
                .bus
                .publish(shux_core::event::EventData::PaneStatusChanged {
                    pane_id: pane,
                    window_id: p.window_id,
                    session_id: snap.windows[&p.window_id].session_id,
                    old: None,
                    new: None,
                    seen: true,
                    enabled: true,
                });
        }
        let history = dispatch_ok(
            &harness.router,
            "events.history",
            serde_json::json!({"pane_id": a.to_string(), "filter": ["pane.status_changed"], "count": 2}),
        )
        .await;
        assert_eq!(
            history["events"].as_array().unwrap().len(),
            2,
            "the other pane's newer events used up the count: {history}"
        );
        harness.stop().await;
    }

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

    fn plugin_tick(harness: &RpcHarness) -> u64 {
        harness
            .bus
            .publish(shux_core::event::EventData::PluginEvent {
                plugin_id: "t".to_string(),
                event_type: "tick".to_string(),
                data: serde_json::json!({}),
            })
    }

    async fn timed_watch(
        harness: &RpcHarness,
        params: serde_json::Value,
    ) -> (serde_json::Value, Duration) {
        let started = std::time::Instant::now();
        let result = dispatch_ok(&harness.router, "events.watch", params).await;
        (result, started.elapsed())
    }

    /// History already in hand is returned at once, not after `timeout_ms`.
    #[tokio::test]
    async fn watch_with_history_returns_without_waiting_for_the_deadline() {
        let harness = RpcHarness::new();
        let first = plugin_tick(&harness);
        plugin_tick(&harness);
        plugin_tick(&harness);
        let (watched, elapsed) = timed_watch(
            &harness,
            serde_json::json!({"from_seq": first, "filter": ["plugin.t."], "timeout_ms": 5_000}),
        )
        .await;
        assert_eq!(watched["events"].as_array().unwrap().len(), 3, "{watched}");
        assert_eq!(watched["next_seq"], first + 3);
        assert!(
            elapsed < Duration::from_millis(2_000),
            "waited {elapsed:?} with 3 events of history already collected"
        );
        harness.stop().await;
    }

    /// With nothing to return the call still long-polls for the full timeout.
    #[tokio::test]
    async fn watch_without_history_waits_for_the_deadline() {
        let harness = RpcHarness::new();
        let next = plugin_tick(&harness) + 1;
        let (watched, elapsed) = timed_watch(
            &harness,
            serde_json::json!({"from_seq": next, "filter": ["plugin.t."], "timeout_ms": 400}),
        )
        .await;
        assert_eq!(watched["events"].as_array().unwrap().len(), 0, "{watched}");
        assert_eq!(watched["next_seq"], next);
        assert!(
            elapsed >= Duration::from_millis(350),
            "returned after {elapsed:?}, before the 400 ms deadline, with nothing to return"
        );
        harness.stop().await;
    }

    /// The first live event ends the long poll.
    #[tokio::test]
    async fn watch_returns_the_first_live_event_without_waiting_for_the_deadline() {
        let harness = RpcHarness::new();
        let router = harness.router.clone();
        let watch = tokio::spawn(async move {
            let started = std::time::Instant::now();
            let result = router
                .dispatch(
                    "events.watch",
                    Some(serde_json::json!({"filter": ["plugin.t."], "timeout_ms": 5_000})),
                )
                .await
                .unwrap();
            (result, started.elapsed())
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        let seq = plugin_tick(&harness);
        let (watched, elapsed) = watch.await.unwrap();
        assert_eq!(watched["events"][0]["seq"], seq, "{watched}");
        assert!(
            elapsed < Duration::from_millis(2_000),
            "held a live event for {elapsed:?} waiting for more"
        );
        harness.stop().await;
    }

    /// `pane.output.watch` hands back the first chunk it gets rather than
    /// holding it for `limit` chunks or the deadline.
    #[tokio::test]
    async fn pane_output_watch_returns_the_first_chunk_without_waiting_for_the_deadline() {
        let harness = RpcHarness::new();
        let (session_id, window_id, pane_id) = harness.seed_session("chunks").await;
        let router = harness.router.clone();
        let pane = pane_id.to_string();
        let watch = tokio::spawn(async move {
            let started = std::time::Instant::now();
            let result = router
                .dispatch(
                    "pane.output.watch",
                    Some(serde_json::json!({"pane_id": pane, "timeout_ms": 5_000, "limit": 50})),
                )
                .await
                .unwrap();
            (result, started.elapsed())
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        let seq = harness.bus.publish_pane_output(
            pane_id,
            window_id,
            session_id,
            "aGk=".to_string(),
            false,
        );
        let (watched, elapsed) = watch.await.unwrap();
        assert_eq!(watched["chunks"][0]["seq"], seq, "{watched}");
        assert!(
            elapsed < Duration::from_millis(2_000),
            "held a chunk for {elapsed:?} waiting for more"
        );
        harness.stop().await;
    }

    /// Paging with `from_seq = next_seq` while two threads publish at once
    /// delivers every event exactly once and never one below `from_seq`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn paging_under_concurrent_publishers_loses_and_duplicates_nothing() {
        const PER: u64 = 2_500;
        for round in 0..3 {
            let harness = RpcHarness::new();
            let start = harness.bus.current_seq();
            let publishers: Vec<_> = (0..3)
                .map(|t| {
                    let bus = harness.bus.clone();
                    std::thread::spawn(move || {
                        for i in 0..PER {
                            bus.publish(shux_core::event::EventData::PluginEvent {
                                plugin_id: "t".to_string(),
                                event_type: "tick".to_string(),
                                data: serde_json::json!({ "t": t, "i": i }),
                            });
                        }
                    })
                })
                .collect();
            // Other watchers snapshot history concurrently, as real pagers do.
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let readers: Vec<_> = (0..2)
                .map(|_| {
                    let (bus, stop) = (harness.bus.clone(), stop.clone());
                    std::thread::spawn(move || {
                        while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                            std::hint::black_box(bus.events_from_seq(start));
                        }
                    })
                })
                .collect();
            let (mut seq, mut seen, mut below, mut idle) = (start, Vec::new(), Vec::new(), 0);
            loop {
                let r = dispatch_ok(
                    &harness.router,
                    "events.watch",
                    serde_json::json!({"from_seq": seq, "filter": ["plugin.t."], "timeout_ms": 50}),
                )
                .await;
                let events = r["events"].as_array().unwrap();
                for e in events {
                    let s = e["seq"].as_u64().unwrap();
                    if s < seq {
                        below.push((s, seq));
                    }
                    seen.push(s);
                }
                seq = r["next_seq"].as_u64().unwrap();
                if !events.is_empty() {
                    idle = 0;
                } else if publishers.iter().all(|p| p.is_finished()) {
                    idle += 1;
                    if idle >= 2 {
                        break;
                    }
                }
            }
            for p in publishers {
                p.join().unwrap();
            }
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            for r in readers {
                r.join().unwrap();
            }
            let mut unique = seen.clone();
            unique.sort_unstable();
            unique.dedup();
            let missing: Vec<u64> = (start..start + 3 * PER)
                .filter(|s| unique.binary_search(s).is_err())
                .collect();
            assert!(
                missing.is_empty() && unique.len() == seen.len() && below.is_empty(),
                "round {round}: lost {} {:?}, duplicated {}, below from_seq {:?}",
                missing.len(),
                &missing[..missing.len().min(8)],
                seen.len() - unique.len(),
                &below[..below.len().min(8)],
            );
            harness.stop().await;
        }
    }
}
