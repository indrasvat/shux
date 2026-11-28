//! The daemon's JSON-RPC surface: one registration function per noun, and the
//! single place they are chained into a `Router`.
//!
//! `build_router` is the whole surface. Nothing else in the crate registers a
//! method, so "what can a client call, and what may a plugin do with it" is one
//! function and one test away.

use std::sync::Arc;

use tokio::sync::Mutex;

use crate::pane_io::PaneIoState;

pub(crate) mod accepts;
pub(crate) mod client;
pub(crate) mod convert;
pub(crate) mod daemon;
pub(crate) mod events;
pub(crate) mod legacy_0_64;
pub(crate) mod pane;
pub(crate) mod pane_io;
pub(crate) mod params;
pub(crate) mod plugin;
pub(crate) mod program_status;
pub(crate) mod session;
pub(crate) mod share;
pub(crate) mod state;
pub(crate) mod wait_command;
pub(crate) mod wait_exit;
pub(crate) mod window;
#[cfg(test)]
pub(crate) mod wire_1;

#[cfg(test)]
pub(crate) mod test_harness;

/// Chain every registration function into the daemon's router.
///
/// The order is the registration order and is preserved verbatim: builtins
/// first, then session → window → pane → pane I/O → lens.run → program status →
/// events → state → share → plugin. Later registrations of the same method name would win, so this is
/// part of the surface, not a formatting choice.
#[expect(clippy::too_many_arguments, reason = "independent inputs")]
pub(crate) fn build_router(
    graph: shux_core::graph::GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    cancel: tokio_util::sync::CancellationToken,
    session_meta: crate::session_meta::SessionMetaCache,
    scratch_registry: crate::lens_scratch::ScratchRegistry,
    config: shux_core::config::ConfigHandle,
    onboarding: crate::onboarding::OnboardingHandle,
    segments: crate::statusbar_runner::SegmentCache,
    lens_audit: Arc<crate::lens_scratch::LensAuditLog>,
    event_bus: shux_core::bus::EventBus,
    plugins: shux_plugin::PluginManager,
    share: crate::share::Gateway,
) -> shux_rpc::Router {
    let (share_graph, share_scratch) = (graph.clone(), scratch_registry.clone());
    crate::rpc::plugin::register_plugin_methods(
        crate::rpc::share::register_share_methods(
            crate::rpc::state::register_state_methods(
                crate::rpc::events::register_events_methods(
                    crate::rpc::program_status::register_program_status_methods(
                        crate::lens_scratch::register_lens_run_method(
                            crate::rpc::pane_io::register_pane_io_methods(
                                crate::rpc::pane::register_pane_methods(
                                    crate::rpc::window::register_window_methods(
                                        crate::rpc::client::register_client_methods(
                                            crate::rpc::session::register_session_methods(
                                                crate::rpc::daemon::register_daemon_methods(
                                                    shux_rpc::server::register_builtin_methods(
                                                        shux_rpc::Router::builder(),
                                                    ),
                                                    graph.clone(),
                                                    io_state.clone(),
                                                    cancel.clone(),
                                                ),
                                                graph.clone(),
                                                io_state.clone(),
                                                cancel.clone(),
                                                session_meta.clone(),
                                                scratch_registry.clone(),
                                            ),
                                            graph.clone(),
                                            io_state.clone(),
                                        ),
                                        graph.clone(),
                                        io_state.clone(),
                                        cancel.clone(),
                                    ),
                                    graph.clone(),
                                    io_state.clone(),
                                    cancel.clone(),
                                ),
                                graph.clone(),
                                io_state.clone(),
                                cancel.clone(),
                                config,
                                session_meta,
                                onboarding,
                                segments,
                                lens_audit,
                            ),
                            graph.clone(),
                            io_state.clone(),
                            cancel.clone(),
                            event_bus.clone(),
                            scratch_registry,
                        ),
                        graph.clone(),
                        io_state.clone(),
                        event_bus.clone(),
                    ),
                    event_bus,
                    graph.clone(),
                ),
                graph,
                io_state,
                cancel,
            ),
            share_graph,
            share_scratch,
            share,
        ),
        plugins,
    )
    .accepts(accepts::ACCEPTS)
    // `null` means absent for every parameter added since 0.64.0.
    .keeps_null(legacy_0_64::LEGACY_0_64)
    .build()
}

#[cfg(test)]
mod surface_tests;
