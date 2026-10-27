//! OSC 7501 program status: the per-pane publisher that carries each
//! terminal's status to the session graph.
//!
//! The PTY task writes the VT's [`StatusPublication`] into a per-pane watch
//! slot under the io lock; one publisher task per pane forwards the newest
//! value to the graph, paced and outside every lock. Generations are
//! process-global and monotonic, so an older publication can never replace a
//! newer one, here or in the graph.

use std::time::Duration;

use shux_core::graph::{GraphError, GraphHandle, PaneStatusUpdate};
use shux_core::model::PaneId;
use shux_vt::StatusPublication;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Minimum spacing between graph writes for one pane.
const PACING: Duration = Duration::from_millis(100);
/// One graph write; under [`FLUSH_BUDGET`] so a retry still fits.
const CALL_BUDGET: Duration = Duration::from_millis(250);
/// Delay before retrying a write that failed or timed out.
const RETRY_AFTER: Duration = Duration::from_millis(100);
/// How long a caller waits for a flush before carrying on.
pub(crate) const FLUSH_BUDGET: Duration = Duration::from_millis(500);

/// Ask the publisher to commit at least `min_generation`, then ack.
struct Flush {
    min_generation: u64,
    ack: oneshot::Sender<()>,
}

/// One pane's status slot and its publisher. Lives as long as the pane, not
/// its PTY: a respawn keeps the publisher and only publishes a newer value.
pub(crate) struct StatusChannel {
    tx: watch::Sender<StatusPublication>,
    flush_tx: mpsc::Sender<Flush>,
    lifetime: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

/// Handle for waiting on a flush without holding the io lock.
#[derive(Clone)]
pub(crate) struct Flusher(mpsc::Sender<Flush>);

impl StatusChannel {
    pub(crate) fn start(pane_id: PaneId, initial: StatusPublication, graph: GraphHandle) -> Self {
        let (tx, rx) = watch::channel(initial);
        let (flush_tx, flush_rx) = mpsc::channel(16);
        let lifetime = CancellationToken::new();
        let task = tokio::spawn(run(pane_id, rx, flush_rx, graph, lifetime.clone()));
        Self {
            tx,
            flush_tx,
            lifetime,
            task,
        }
    }

    /// Store `publication` unless the slot already holds a newer one.
    pub(crate) fn publish(&self, publication: StatusPublication) {
        self.tx.send_if_modified(|cur| {
            if publication.generation > cur.generation {
                *cur = publication;
                true
            } else {
                false
            }
        });
    }

    pub(crate) fn flusher(&self) -> Flusher {
        Flusher(self.flush_tx.clone())
    }

    /// Stop the publisher; the caller may await the handle.
    pub(crate) fn stop(self) -> tokio::task::JoinHandle<()> {
        self.lifetime.cancel();
        self.task
    }
}

impl Flusher {
    /// Wait, at most [`FLUSH_BUDGET`], until the graph holds `min_generation`
    /// or newer. Best-effort: a slow or gone publisher only costs the budget.
    pub(crate) async fn flush(&self, min_generation: u64) {
        let (ack, done) = oneshot::channel();
        let wait = async {
            if self
                .0
                .send(Flush {
                    min_generation,
                    ack,
                })
                .await
                .is_ok()
            {
                let _ = done.await;
            }
        };
        let _ = tokio::time::timeout(FLUSH_BUDGET, wait).await;
    }
}

/// Apply `[program_status] enabled` now, then again on every config reload.
/// Turning it off flushes every pane, so a waiter sees `enabled = false`
/// promptly instead of after the next unrelated publish.
pub(crate) async fn run_config_walker(
    config: shux_core::config::ConfigHandle,
    io: std::sync::Arc<tokio::sync::Mutex<crate::pane_io::PaneIoState>>,
    cancel: CancellationToken,
) {
    let notify = config.change_notify();
    loop {
        // Arm before reading, so a reload between the two is not missed.
        let changed = notify.notified();
        let enabled = config.current().program_status.enabled;
        let flushes = apply_enabled(&io, enabled).await;
        for (flusher, generation) in flushes {
            flusher.flush(generation).await;
        }
        tokio::select! {
            _ = cancel.cancelled() => return,
            () = changed => {}
        }
    }
}

/// Flip the daemon-wide flag and every live terminal under one lock hold.
/// Returns what to flush once the lock is gone (only when turning off).
pub(crate) async fn apply_enabled(
    io: &tokio::sync::Mutex<crate::pane_io::PaneIoState>,
    enabled: bool,
) -> Vec<(Flusher, u64)> {
    let mut guard = io.lock().await;
    let state = &mut *guard;
    if state.program_status_enabled == enabled {
        return Vec::new();
    }
    state.program_status_enabled = enabled;
    let mut flushes = Vec::new();
    for (pane_id, vt) in state.vts.iter_mut() {
        // An exited pane's final status went out with its exit; nothing may
        // follow `pane.exited`, and its terminal keeps agreeing with the graph.
        if state.status_sealed.contains(pane_id) {
            continue;
        }
        vt.set_program_status_enabled(enabled);
        let publication = vt.program_status_publication();
        if let Some(channel) = state.status.get(pane_id) {
            let generation = publication.generation;
            channel.publish(publication);
            if !enabled {
                flushes.push((channel.flusher(), generation));
            }
        }
    }
    flushes
}

pub(crate) fn update(p: &StatusPublication) -> PaneStatusUpdate {
    PaneStatusUpdate {
        generation: p.generation,
        enabled: p.enabled,
        summary: p.summary.clone(),
        seen: p.seen,
    }
}

async fn run(
    pane_id: PaneId,
    mut rx: watch::Receiver<StatusPublication>,
    mut flush_rx: mpsc::Receiver<Flush>,
    graph: GraphHandle,
    lifetime: CancellationToken,
) {
    let mut committed = 0u64;
    let mut pending: Vec<Flush> = Vec::new();
    let mut retry_at: Option<Instant> = None;
    let mut last_write: Option<Instant> = None;
    loop {
        // Always publish first: the initial value counts too.
        let latest = rx.borrow_and_update().clone();
        if latest.generation > committed {
            tokio::select! {
                biased;
                _ = lifetime.cancelled() => break,
                // A flush abandons the in-flight write: the loop goes round and
                // sends the slot's current value, which covers the flush.
                Some(f) = flush_rx.recv() => {
                    pending.push(f);
                    continue;
                }
                r = tokio::time::timeout(CALL_BUDGET, graph.set_pane_program_status(pane_id, update(&latest))) => {
                    last_write = Some(Instant::now());
                    match r {
                        Ok(Ok(())) => {
                            committed = latest.generation;
                            retry_at = None;
                        }
                        Ok(Err(GraphError::PaneNotFound(_) | GraphError::Shutdown)) => break,
                        Ok(Err(e)) => {
                            tracing::debug!(%pane_id, error = %e, "program status publish failed; retrying");
                            retry_at = Some(Instant::now() + RETRY_AFTER);
                        }
                        Err(_) => {
                            tracing::debug!(%pane_id, "program status publish timed out; retrying");
                            retry_at = Some(Instant::now() + RETRY_AFTER);
                        }
                    }
                }
            }
        }
        let (done, waiting): (Vec<_>, Vec<_>) = pending
            .drain(..)
            .partition(|f| f.min_generation <= committed);
        pending = waiting;
        for f in done {
            let _ = f.ack.send(());
        }

        tokio::select! {
            biased;
            _ = lifetime.cancelled() => break,
            r = rx.changed() => if r.is_err() { break },
            f = flush_rx.recv() => match f {
                Some(f) => pending.push(f),
                None => break,
            },
            _ = tokio::time::sleep_until(retry_at.unwrap_or_else(Instant::now)), if retry_at.is_some() => {}
        }
        // Pace writes; a flush cuts the wait short.
        if pending.is_empty()
            && let Some(at) = last_write
        {
            tokio::select! {
                biased;
                _ = lifetime.cancelled() => break,
                Some(f) = flush_rx.recv() => pending.push(f),
                _ = tokio::time::sleep_until(at + PACING) => {}
            }
        }
    }
    // Pending flush acks drop here; their callers see the channel close and go on.
}

#[cfg(test)]
mod tests {
    use super::*;
    use shux_program_status::{State, StatusSummary};

    fn publication(generation: u64, state: Option<State>) -> StatusPublication {
        StatusPublication {
            generation,
            summary: state.map(|state| StatusSummary {
                state,
                kind: None,
                progress: None,
                app: None,
                id: None,
                counts: Default::default(),
            }),
            seen: state.is_some(),
            enabled: true,
        }
    }

    type Snapshot = std::sync::Arc<arc_swap::ArcSwap<shux_core::graph::SessionGraphSnapshot>>;

    /// A running graph actor with one pane; the token stops it with the test.
    fn graph_with_pane() -> (GraphHandle, Snapshot, PaneId) {
        let (graph, state) = shux_core::graph::SessionGraph::new_with_event_bus(None);
        let sid = graph
            .create_session("work".into(), std::env::temp_dir())
            .unwrap();
        let snap = state.load();
        let pid = snap.windows[&snap.sessions[&sid].windows[0]].active_pane;
        let (cmd_tx, cmd_rx) = mpsc::channel(128);
        let cancel = CancellationToken::new();
        tokio::spawn(shux_core::graph::run_graph_loop(graph, cmd_rx, cancel));
        (GraphHandle::new(cmd_tx, state.clone()), state, pid)
    }

    fn state_of(state: &Snapshot, pid: PaneId) -> Option<State> {
        state.load().panes[&pid]
            .program_status
            .as_ref()
            .map(|s| s.state)
    }

    #[tokio::test]
    async fn the_initial_value_reaches_the_graph() {
        let (graph, state, pid) = graph_with_pane();
        let ch = StatusChannel::start(pid, publication(10, Some(State::Working)), graph);
        ch.flusher().flush(10).await;
        assert_eq!(state_of(&state, pid), Some(State::Working));
        let _ = ch.stop().await;
    }

    #[tokio::test]
    async fn the_newest_value_wins_and_an_older_one_is_ignored() {
        let (graph, state, pid) = graph_with_pane();
        let ch = StatusChannel::start(pid, publication(10, Some(State::Working)), graph);
        ch.publish(publication(12, Some(State::Blocked)));
        ch.publish(publication(11, Some(State::Done)));
        ch.flusher().flush(12).await;
        assert_eq!(state_of(&state, pid), Some(State::Blocked));
        let _ = ch.stop().await;
    }

    #[tokio::test]
    async fn a_flush_returns_within_its_budget_when_nothing_can_commit() {
        let (graph, _state, pid) = graph_with_pane();
        let ch = StatusChannel::start(pid, publication(10, None), graph);
        let start = std::time::Instant::now();
        ch.flusher().flush(u64::MAX).await;
        assert!(start.elapsed() < FLUSH_BUDGET + Duration::from_millis(200));
        let _ = ch.stop().await;
    }

    #[tokio::test]
    async fn the_publisher_exits_when_its_pane_is_gone() {
        let (graph, _state, _pid) = graph_with_pane();
        let ch = StatusChannel::start(PaneId::new(), publication(10, None), graph);
        tokio::time::timeout(Duration::from_secs(2), ch.task)
            .await
            .expect("publisher kept running for a pane the graph does not have")
            .unwrap();
    }

    #[tokio::test]
    async fn stopping_ends_the_publisher_promptly() {
        let (graph, _state, pid) = graph_with_pane();
        let ch = StatusChannel::start(pid, publication(10, None), graph);
        tokio::time::timeout(Duration::from_secs(1), ch.stop())
            .await
            .expect("publisher ignored its lifetime token")
            .unwrap();
    }

    // ── End to end: a real PTY, real sh, the RPCs and the graph ──────────
    // Named `a_pane_*` so .config/nextest.toml puts them in `daemon-pty`.

    use crate::rpc::test_harness::{RpcHarness, dispatch_ok};
    use serde_json::json;

    async fn spawn_sh_result(
        h: &RpcHarness,
        pane: PaneId,
        script: &str,
    ) -> Result<(), shux_pty::PtyError> {
        crate::pane_spawn::spawn_pane_pty(
            pane,
            "/tmp".into(),
            vec!["sh".into(), "-c".into(), script.into()],
            shux_pty::handle::PtySize::default(),
            Vec::new(),
            false,
            h.io.clone(),
            h.cancel.clone(),
            h.graph.clone(),
        )
        .await
    }

    async fn spawn_sh(h: &RpcHarness, pane: PaneId, script: &str) {
        crate::pane_spawn::spawn_pane_pty(
            pane,
            "/tmp".into(),
            vec!["sh".into(), "-c".into(), script.into()],
            shux_pty::handle::PtySize::default(),
            Vec::new(),
            false,
            h.io.clone(),
            h.cancel.clone(),
            h.graph.clone(),
        )
        .await
        .unwrap();
    }

    /// Stop every PTY and wait until its child is reaped, then the harness.
    async fn finish(h: RpcHarness) {
        let done: Vec<_> =
            h.io.lock()
                .await
                .pty_done
                .drain()
                .map(|(_, rx)| rx)
                .collect();
        h.cancel.cancel();
        for rx in done {
            let _ = tokio::time::timeout(Duration::from_secs(5), rx).await;
        }
        h.stop().await;
    }

    async fn wait_status(
        h: &RpcHarness,
        pane: PaneId,
        state: &str,
        timeout_ms: u64,
    ) -> serde_json::Value {
        dispatch_ok(
            &h.router,
            "pane.wait_status",
            json!({"pane_id": pane.to_string(), "state": state, "timeout_ms": timeout_ms}),
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_pane_reports_its_status_through_the_graph_and_the_rpcs() {
        let h = RpcHarness::new();
        let (_s, _w, pane) = h.seed_session("status-e2e").await;
        spawn_sh(
            &h,
            pane,
            r"printf '\033]7501;state=blocked:kind=auth:title=U2lnbiBpbg\033\\'; sleep 30",
        )
        .await;

        let waited = wait_status(&h, pane, "blocked", 5_000).await;
        assert_eq!(waited["outcome"], "matched", "{waited}");
        assert_eq!(waited["summary"]["state"], "blocked");
        assert_eq!(waited["summary"]["kind"], "auth");

        let status = dispatch_ok(
            &h.router,
            "pane.status",
            json!({"pane_id": pane.to_string()}),
        )
        .await;
        assert_eq!(status["enabled"], true);
        assert_eq!(status["seen"], true);
        assert_eq!(status["records"][0]["state"], "blocked");
        assert_eq!(
            status["records"][0]["title"], "Sign in",
            "pane.status carries the text"
        );
        assert_eq!(
            h.graph.snapshot().panes[&pane]
                .program_status
                .as_ref()
                .map(|s| s.state),
            Some(State::Blocked)
        );
        finish(h).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_pane_that_exits_sends_its_final_status_with_the_exit() {
        let h = RpcHarness::new();
        let (_s, _w, pane) = h.seed_session("status-exit").await;
        spawn_sh(
            &h,
            pane,
            r"printf '\033]7501;state=working:id=a\033\\\033]7501;state=done:id=b\033\\'; sleep 0.5",
        )
        .await;
        let mut gone = None;
        for _ in 0..100 {
            let snap = h.graph.snapshot();
            if snap.panes[&pane].exit_status.is_some() {
                gone = snap.panes[&pane].program_status.clone();
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let summary = gone.expect("the pane exited with a status");
        assert_eq!(summary.state, State::Done, "done survives the exit");
        assert_eq!(summary.counts.working, 0, "working ends with the process");

        let order: Vec<&str> = h
            .bus
            .history(200)
            .iter()
            .filter_map(|e| match e.data {
                shux_core::event::EventData::PaneStatusChanged { pane_id, .. }
                    if pane_id == pane =>
                {
                    Some("status")
                }
                shux_core::event::EventData::PaneExited { pane_id, .. } if pane_id == pane => {
                    Some("exited")
                }
                _ => None,
            })
            .collect();
        assert_eq!(order.last(), Some(&"exited"), "{order:?}");
        assert_eq!(order.iter().filter(|e| **e == "exited").count(), 1);
        finish(h).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_pane_wait_reports_timeout_and_disabled() {
        let h = RpcHarness::new();
        let (_s, _w, pane) = h.seed_session("status-wait").await;
        spawn_sh(&h, pane, "sleep 30").await;
        let waited = wait_status(&h, pane, "error", 200).await;
        assert_eq!(waited["outcome"], "timeout", "{waited}");

        let (_s, _w, off) = h.seed_session("status-off").await;
        h.io.lock().await.program_status_enabled = false;
        spawn_sh(&h, off, r"printf '\033]7501;state=error\033\\'; sleep 30").await;
        let mut waited = json!(null);
        for _ in 0..40 {
            waited = wait_status(&h, off, "error", 100).await;
            if waited["outcome"] == "disabled" {
                break;
            }
        }
        assert_eq!(waited["outcome"], "disabled", "{waited}");
        let status = dispatch_ok(
            &h.router,
            "pane.status",
            json!({"pane_id": off.to_string()}),
        )
        .await;
        assert_eq!(status["enabled"], false);
        assert_eq!(status["records"], json!([]), "disabled keeps no records");
        finish(h).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_pane_follows_the_config_on_and_off_without_a_restart() {
        let h = RpcHarness::new();
        let path = std::env::temp_dir().join(format!("shux-ps-{}.toml", uuid::Uuid::new_v4()));
        let config = shux_core::config::ConfigHandle::load_or_default(&path);
        let walker = tokio::spawn(run_config_walker(
            config.clone(),
            h.io.clone(),
            h.cancel.clone(),
        ));
        let (_s, _w, pane) = h.seed_session("status-config").await;
        spawn_sh(
            &h,
            pane,
            r"printf '\033]7501;state=blocked\033\\'; sleep 30",
        )
        .await;
        assert_eq!(
            wait_status(&h, pane, "blocked", 5_000).await["outcome"],
            "matched"
        );

        let mut off = shux_core::config::Config::default();
        off.program_status.enabled = false;
        config.replace(off);
        // The walker applies a reload asynchronously; wait for the terminal.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while h.io.lock().await.vts[&pane].program_status_enabled() {
            assert!(
                std::time::Instant::now() < deadline,
                "the walker never applied the reload"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let waited = wait_status(&h, pane, "blocked", 5_000).await;
        assert_eq!(waited["outcome"], "disabled", "{waited}");
        // The graph follows within the walker's flush budget.
        let deadline = std::time::Instant::now() + FLUSH_BUDGET + Duration::from_millis(500);
        while h.graph.snapshot().panes[&pane].program_status_enabled {
            assert!(
                std::time::Instant::now() < deadline,
                "the graph never saw the disable"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        config.replace(shux_core::config::Config::default());
        let mut status = json!(null);
        for _ in 0..50 {
            status = dispatch_ok(
                &h.router,
                "pane.status",
                json!({"pane_id": pane.to_string()}),
            )
            .await;
            if status["enabled"] == true {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(status["enabled"], true, "{status}");
        assert_eq!(status["records"], json!([]), "re-enabling starts empty");
        h.cancel.cancel();
        let _ = walker.await;
        finish(h).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_pane_wait_started_before_a_disabled_terminal_exists_sees_disabled() {
        let h = RpcHarness::new();
        h.io.lock().await.program_status_enabled = false;
        let (_s, _w, pane) = h.seed_session("status-late-off").await;
        let router = h.router.clone();
        let waiter = tokio::spawn(async move {
            dispatch_ok(
                &router,
                "pane.wait_status",
                json!({"pane_id": pane.to_string(), "state": "done", "timeout_ms": 5_000}),
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        spawn_sh(&h, pane, "sleep 30").await;
        let start = std::time::Instant::now();
        let waited = waiter.await.unwrap();
        assert_eq!(waited["outcome"], "disabled", "{waited}");
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "noticed only at the deadline"
        );
        finish(h).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_pane_that_exited_publishes_nothing_after_its_exit() {
        let h = RpcHarness::new();
        let (_s, _w, pane) = h.seed_session("status-after-exit").await;
        spawn_sh(&h, pane, r"printf '\033]7501;state=done\033\\'; sleep 0.3").await;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while h.graph.snapshot().panes[&pane].exit_status.is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "the pane never exited"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        for (flusher, generation) in apply_enabled(&h.io, false).await {
            flusher.flush(generation).await;
        }
        tokio::time::sleep(FLUSH_BUDGET).await;
        let last = h.bus.history(200).iter().rev().find_map(|e| match e.data {
            shux_core::event::EventData::PaneStatusChanged { pane_id, .. } if pane_id == pane => {
                Some("status")
            }
            shux_core::event::EventData::PaneExited { pane_id, .. } if pane_id == pane => {
                Some("exited")
            }
            _ => None,
        });
        assert_eq!(last, Some("exited"), "a status event followed pane.exited");
        finish(h).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_pane_publisher_is_stopped_by_daemon_shutdown_even_after_exit() {
        let h = RpcHarness::new();
        let (_s, _w, pane) = h.seed_session("status-shutdown").await;
        spawn_sh(&h, pane, "exit 0").await;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while h.graph.snapshot().panes[&pane].exit_status.is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "the pane never exited"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            h.io.lock().await.status.contains_key(&pane),
            "exit keeps the slot"
        );
        crate::daemon_boot::shutdown_all_pane_io(h.io.clone()).await;
        let state = h.io.lock().await;
        assert!(
            state.status.is_empty(),
            "shutdown left a status publisher running"
        );
        assert!(
            state.teardown_waiters.is_empty(),
            "shutdown left publisher joins unawaited"
        );
        drop(state);
        finish(h).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_pane_wait_times_out_on_time_while_another_pane_is_busy() {
        let h = RpcHarness::new();
        let (_s, _w, busy) = h.seed_session("status-busy").await;
        let (_s, _w, quiet) = h.seed_session("status-quiet").await;
        spawn_sh(
            &h,
            busy,
            r"i=0; while [ $i -lt 2000 ]; do printf '\033]7501;state=working:progress=%d\033\\' $((i % 100)); i=$((i+1)); sleep 0.02; done",
        )
        .await;
        spawn_sh(&h, quiet, "sleep 30").await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let start = std::time::Instant::now();
        let waited =
            tokio::time::timeout(Duration::from_secs(5), wait_status(&h, quiet, "error", 600))
                .await
                .expect("wait_status ignored its deadline while another pane was busy");
        assert_eq!(waited["outcome"], "timeout", "{waited}");
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "took {:?}",
            start.elapsed()
        );
        finish(h).await;
    }

    /// A raw-mode program that sends probes and never reads the replies must
    /// not wedge its pane: the daemon keeps reading its output, and the pane
    /// can still be torn down.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_pane_that_never_reads_its_replies_is_not_wedged() {
        let h = RpcHarness::new();
        let (_s, _w, pane) = h.seed_session("status-unread").await;
        spawn_sh(
            &h,
            pane,
            r#"python3 -c "import os,tty,time; tty.setraw(0); os.write(1, b'\x1b]7501;?\x1b\\\\'*300); os.write(1, b'MAR'+b'KER'); time.sleep(30)""#,
        )
        .await;
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        loop {
            let text = h.io.lock().await.vts[&pane].capture_text(None);
            if text.contains("MARKER") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the pane stopped reading output once its replies went unread"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        finish(h).await;
    }

    /// Two spawns racing for a pane that has none (create's background spawn
    /// and an attach) must run its command once, not twice.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_pane_spawned_twice_concurrently_runs_its_command_once() {
        let h = RpcHarness::new();
        let (_s, _w, pane) = h.seed_session("status-double").await;
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("starts");
        let script = format!("echo start >> '{}'; sleep 30", log.display());
        let (a, b) = tokio::join!(
            spawn_sh_result(&h, pane, &script),
            spawn_sh_result(&h, pane, &script)
        );
        a.unwrap();
        b.unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let starts = std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .count();
        assert_eq!(starts, 1, "the pane's command ran {starts} times");
        finish(h).await;
    }

    /// An exited pane keeps what it last reported, in the terminal and the
    /// graph alike, whatever the config does afterwards.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_pane_that_exited_keeps_its_final_records_across_a_disable() {
        let h = RpcHarness::new();
        let (_s, _w, pane) = h.seed_session("status-exited-off").await;
        spawn_sh(&h, pane, r"printf '\033]7501;state=done\033\\'; sleep 0.3").await;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while h.graph.snapshot().panes[&pane].exit_status.is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "the pane never exited"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let _ = apply_enabled(&h.io, false).await;
        let status = dispatch_ok(
            &h.router,
            "pane.status",
            json!({"pane_id": pane.to_string()}),
        )
        .await;
        assert_eq!(status["records"][0]["state"], "done", "{status}");
        assert_eq!(
            wait_status(&h, pane, "done", 1_000).await["outcome"],
            "matched"
        );
        finish(h).await;
    }

    /// A spawn whose future is dropped mid-flight (an RPC client hung up)
    /// must not leave the pane unspawnable.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_pane_whose_spawn_was_cancelled_can_still_spawn() {
        let h = RpcHarness::new();
        let (_s, _w, pane) = h.seed_session("status-cancelled").await;
        let guard = h.io.lock().await; // park the first spawn inside its claim/install
        let first = tokio::time::timeout(
            Duration::from_millis(200),
            spawn_sh_result(&h, pane, "sleep 30"),
        );
        let (_, guard) = tokio::join!(first, async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            guard
        });
        drop(guard);
        spawn_sh_result(&h, pane, "sleep 30").await.unwrap();
        assert!(
            h.io.lock().await.writers.contains_key(&pane),
            "the pane never spawned after a cancelled spawn"
        );
        finish(h).await;
    }

    /// When the spawn that got there first fails, the one that waited on it
    /// must fail too, not report success.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_pane_spawn_that_waited_on_a_failed_one_fails() {
        let h = RpcHarness::new();
        let ghost = PaneId::new(); // not in the graph: the claim fails
        let (a, b) = tokio::join!(
            spawn_sh_result(&h, ghost, "sleep 30"),
            spawn_sh_result(&h, ghost, "sleep 30")
        );
        assert!(a.is_err());
        assert!(b.is_err(), "the waiting spawn reported success");
        finish(h).await;
    }

    /// Typing into a pane whose program has stopped reading its input must
    /// not freeze the pane either: output keeps flowing and it tears down.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_pane_that_never_reads_its_input_survives_typing() {
        let h = RpcHarness::new();
        let (_s, _w, pane) = h.seed_session("status-typing").await;
        spawn_sh(
            &h,
            pane,
            r#"python3 -c "import os,tty,time; tty.setraw(0); os.write(1, b'\x1b]7501;?\x1b\\\\'*300)
for i in range(600): os.write(1, b'TICK%dX ' % i); time.sleep(0.05)""#,
        )
        .await;
        let ticked = |text: &str, n: usize| text.contains(&format!("TICK{n}X"));
        let wait_tick = |n: usize| {
            let io = h.io.clone();
            async move {
                let deadline = std::time::Instant::now() + Duration::from_secs(8);
                loop {
                    let text = io.lock().await.vts[&pane].capture_text(None);
                    if ticked(&text, n) {
                        return true;
                    }
                    if std::time::Instant::now() > deadline {
                        return false;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        };
        assert!(wait_tick(5).await, "the program never started ticking");
        let writer = h.io.lock().await.writers[&pane].clone();
        for _ in 0..64 {
            let _ = writer.try_send(vec![b'k'; 1024]); // a paste the program never reads
        }
        assert!(wait_tick(40).await, "the pane froze once typed into");
        finish(h).await;
    }

    /// Cancelling a command in a pane whose input is backed up must not hold
    /// the shared pane I/O lock while it waits to send Ctrl-C.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_pane_with_backed_up_input_does_not_wedge_command_cancel() {
        let h = RpcHarness::new();
        let (_s, _w, pane) = h.seed_session("status-cancel").await;
        spawn_sh(
            &h,
            pane,
            r#"python3 -c "import tty,time; tty.setraw(0); time.sleep(900)""#,
        )
        .await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let (command_id, _) = h.io.lock().await.cmd_engine.start_command(
            pane.0,
            "true",
            &[],
            Duration::from_secs(60),
            None,
        );
        let writer = h.io.lock().await.writers[&pane].clone();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while writer.try_send(vec![b'k'; 64 << 10]).is_ok() {
            assert!(
                std::time::Instant::now() < deadline,
                "input never backed up"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let cancel = tokio::spawn({
            let router = h.router.clone();
            async move {
                router
                    .dispatch(
                        "pane.command_cancel",
                        Some(json!({ "command_id": command_id.to_string() })),
                    )
                    .await
            }
        });
        let locked = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            drop(h.io.lock().await);
        })
        .await;
        assert!(
            locked.is_ok(),
            "command_cancel holds the pane I/O lock while input is backed up"
        );
        let cancelled = tokio::time::timeout(Duration::from_secs(3), cancel).await;
        assert!(cancelled.is_ok(), "command_cancel never returned");
        finish(h).await;
    }

    /// A program that reads its input gets every byte of a large paste, in
    /// order, as fast as it reads: input is never dropped to protect the loop.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_reading_program_gets_a_large_paste_whole_and_promptly() {
        use sha2::Digest;
        const TOTAL: usize = 2 << 20;
        let h = RpcHarness::new();
        let (_s, _w, pane) = h.seed_session("status-paste").await;
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("got");
        spawn_sh(
            &h,
            pane,
            &format!(
                r#"python3 -c "import os,tty,hashlib; tty.setraw(0); n=0; d=hashlib.sha256()
while n < {TOTAL}:
    b = os.read(0, 65536); n += len(b); d.update(b)
open('{}', 'w').write('%d %s' % (n, d.hexdigest()))""#,
                out.display()
            ),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let paste: Vec<u8> = (0..TOTAL).map(|i| b'a' + (i % 26) as u8).collect();
        let writer = h.io.lock().await.writers[&pane].clone();
        let started = std::time::Instant::now();
        for chunk in paste.chunks(256 << 10) {
            writer.send(chunk.to_vec()).await.unwrap();
        }
        let deadline = started + Duration::from_secs(15);
        let got = loop {
            if let Ok(got) = std::fs::read_to_string(&out) {
                break got;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the reader never got the whole paste in 15s"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        let digest: String = sha2::Sha256::digest(&paste)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let want = format!("{TOTAL} {digest}");
        assert_eq!(got, want, "the paste arrived cut, reordered or padded");
        finish(h).await;
    }
}
