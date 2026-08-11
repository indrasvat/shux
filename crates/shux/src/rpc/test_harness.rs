//! Test-only harness: a real `Router` over a real `SessionGraph`.
//!
//! The production tests in this module tree dispatch through the same router
//! `build_router` assembles, minus the plugin and builtin registrations they
//! do not exercise. Anything that needs a live pane, a scratch registry or an
//! audit log gets it here rather than reinventing it per test file.

use std::sync::Arc;
use std::time::Duration;

use shux_core::config::ConfigHandle;
use shux_core::graph::{GraphHandle, SessionGraph, run_graph_loop};
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use crate::pane_io::{PaneIoState, ResizeRequest};
use crate::{lens_scratch, onboarding, session_meta, statusbar_runner};

pub(crate) fn write_plugin_script(
    dir: &std::path::Path,
    name: &str,
    body: &str,
) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path
}

pub(crate) struct RpcHarness {
    pub(crate) router: shux_rpc::Router,
    pub(crate) graph: GraphHandle,
    pub(crate) io: Arc<Mutex<PaneIoState>>,
    pub(crate) bus: shux_core::bus::EventBus,
    pub(crate) cancel: CancellationToken,
    pub(crate) graph_task: tokio::task::JoinHandle<()>,
    pub(crate) scratch_registry: lens_scratch::ScratchRegistry,
    pub(crate) lens_audit: Arc<lens_scratch::LensAuditLog>,
    /// Keeps the isolated scratch/audit dir alive for the harness's
    /// lifetime (registry + lens-audit files live inside).
    pub(crate) _scratch_dir: tempfile::TempDir,
}

impl RpcHarness {
    pub(crate) fn new() -> Self {
        let bus = shux_core::bus::EventBus::new();
        let (graph, state) = SessionGraph::new_with_event_bus(Some(bus.clone()));
        let (cmd_tx, cmd_rx) = mpsc::channel(128);
        let cancel = CancellationToken::new();
        let graph_cancel = cancel.clone();
        let graph_task = tokio::spawn(async move {
            run_graph_loop(graph, cmd_rx, graph_cancel).await;
        });
        let graph = GraphHandle::new(cmd_tx, state);
        let io = Arc::new(Mutex::new(PaneIoState::new().with_event_bus(bus.clone())));
        let meta = session_meta::SessionMetaCache::new();
        let config_path =
            std::env::temp_dir().join(format!("shux-rpc-test-{}.toml", uuid::Uuid::new_v4()));
        let config = ConfigHandle::load_or_default(&config_path);
        let onboarding = onboarding::OnboardingHandle::from_state_for_test(Default::default());
        let segments = statusbar_runner::SegmentCache::new();
        let scratch_dir = tempfile::tempdir().expect("scratch dir");
        let lens_audit = lens_scratch::LensAuditLog::open(scratch_dir.path());
        let scratch_registry =
            lens_scratch::ScratchRegistry::new(scratch_dir.path(), lens_audit.clone());

        let builder = crate::rpc::session::register_session_methods(
            shux_rpc::Router::builder(),
            graph.clone(),
            io.clone(),
            cancel.clone(),
            meta.clone(),
            scratch_registry.clone(),
        );
        let builder = crate::rpc::window::register_window_methods(
            builder,
            graph.clone(),
            io.clone(),
            cancel.clone(),
        );
        let builder = crate::rpc::pane::register_pane_methods(
            builder,
            graph.clone(),
            io.clone(),
            cancel.clone(),
        );
        let builder = crate::rpc::state::register_state_methods(
            builder,
            graph.clone(),
            io.clone(),
            cancel.clone(),
        );
        let builder = crate::rpc::pane_io::register_pane_io_methods(
            builder,
            graph.clone(),
            io.clone(),
            cancel.clone(),
            config,
            meta,
            onboarding,
            segments,
            lens_audit.clone(),
        );
        let builder = lens_scratch::register_lens_run_method(
            builder,
            graph.clone(),
            io.clone(),
            cancel.clone(),
            bus.clone(),
            scratch_registry.clone(),
        );

        let router =
            crate::rpc::events::register_events_methods(builder, bus.clone(), graph.clone())
                .build();
        router.assert_every_route_has_policy();

        Self {
            router,
            graph,
            io,
            bus,
            cancel,
            graph_task,
            scratch_registry,
            lens_audit,
            _scratch_dir: scratch_dir,
        }
    }

    pub(crate) async fn stop(self) {
        self.cancel.cancel();
        self.graph_task.await.unwrap();
    }

    pub(crate) async fn seed_session(
        &self,
        name: &str,
    ) -> (
        shux_core::model::SessionId,
        shux_core::model::WindowId,
        shux_core::model::PaneId,
    ) {
        let session_id = self
            .graph
            .create_session_with_command(
                name.to_string(),
                std::path::PathBuf::from("/tmp"),
                vec!["bash".to_string()],
            )
            .await
            .unwrap();
        let snap = self.graph.snapshot();
        let session = snap.sessions.get(&session_id).unwrap();
        let window_id = session.active_window;
        let pane_id = snap.windows.get(&window_id).unwrap().active_pane;
        (session_id, window_id, pane_id)
    }

    pub(crate) async fn seed_io(
        &self,
        pane_id: shux_core::model::PaneId,
        text: &[u8],
    ) -> mpsc::Receiver<Vec<u8>> {
        let (write_tx, write_rx) = mpsc::channel(16);
        let (resize_tx, mut resize_rx) = mpsc::channel::<ResizeRequest>(8);
        let io = self.io.clone();
        tokio::spawn(async move {
            while let Some(req) = resize_rx.recv().await {
                let mut state = io.lock().await;
                if let Some(vt) = state.vts.get_mut(&pane_id) {
                    vt.resize(req.size.rows as usize, req.size.cols as usize);
                }
                if let Some(ack) = req.ack {
                    let _ = ack.send(());
                }
            }
        });

        let mut vt = shux_vt::VirtualTerminal::new(6, 40);
        if !text.is_empty() {
            vt.process(text);
        }
        let mut state = self.io.lock().await;
        state.writers.insert(pane_id, write_tx);
        state.resizers.insert(pane_id, resize_tx);
        state.shutdowns.insert(pane_id, self.cancel.child_token());
        state.vts.insert(pane_id, vt);
        write_rx
    }
}

pub(crate) async fn dispatch_ok(
    router: &shux_rpc::Router,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    router.dispatch(method, Some(params)).await.unwrap()
}

pub(crate) async fn dispatch_err(
    router: &shux_rpc::Router,
    method: &str,
    params: serde_json::Value,
) -> shux_rpc::RpcError {
    router.dispatch(method, Some(params)).await.unwrap_err()
}

/// Kill a lens.run scratch session through the production route and
/// wait for its registry slot to free (the explicit-kill reap confirms
/// group death before dropping the row).
pub(crate) async fn kill_scratch_and_wait(harness: &RpcHarness, session_id: &str) {
    let _ = dispatch_ok(
        &harness.router,
        "session.kill",
        serde_json::json!({"id": session_id}),
    )
    .await;
    let sid: shux_core::model::SessionId = session_id.parse().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while harness.scratch_registry.ids().contains(&sid) {
        assert!(
            std::time::Instant::now() < deadline,
            "scratch {session_id} not reaped within 5s of explicit kill"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Count live processes whose argv contains `needle` (unique per test
/// run, so co-tenant argv text cannot collide).
pub(crate) fn count_procs_containing(needle: &str) -> usize {
    std::process::Command::new("ps")
        .args(["-axo", "args="])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| l.contains(needle))
                .count()
        })
        .unwrap_or(0)
}

/// `__scratch-*` sessions currently in the GRAPH (what a leaked
/// phantom would show up as — an ordinary-looking session outside the
/// registry; claude round-2 detail).
pub(crate) fn graph_scratch_sessions(harness: &RpcHarness) -> Vec<shux_core::model::SessionId> {
    harness
        .graph
        .snapshot()
        .sessions
        .values()
        .filter(|s| s.name.starts_with("__scratch-"))
        .map(|s| s.id)
        .collect()
}
