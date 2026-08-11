//! The RPC surface, pinned.
//!
//! Every method the daemon serves, with the sensitivity a plugin call resolves
//! to. `Router::assert_every_route_has_policy` already refuses to boot a daemon
//! whose method lost its classification; this catches the other three ways the
//! surface can move without anyone noticing — a method disappearing, a method
//! appearing, and a method quietly changing tier.
//!
//! Editing the list below is not a way to make this test pass. It is a claim
//! that the daemon's API changed, and it belongs in a change that says so.

use std::sync::Arc;

use shux_rpc::{Policy, Sensitivity};
use tokio::sync::Mutex;

use crate::pane_io::PaneIoState;

/// `method → policy`, one line each, sorted by method.
///
/// `Fixed` methods print their tier. `events.watch` is parameter-aware, so both
/// of its branches are resolved and printed — a closure that collapsed to one
/// tier would otherwise read as unchanged.
const EXPECTED_SURFACE: &[&str] = &[
    "events.history => ContentRead",
    "events.watch => ParamAware(broad=ContentRead, self_scoped=Public)",
    "lens.run => Grantable",
    "pane.capture => ContentRead",
    "pane.checkpoint => ContentRead",
    "pane.command_cancel => OwnedMutation",
    "pane.command_status => ContentRead",
    "pane.diff_since => ContentRead",
    "pane.focus => OwnedMutation",
    "pane.focus_direction => OwnedMutation",
    "pane.glance => ContentRead",
    "pane.kill => OwnedMutation",
    "pane.list => Public",
    "pane.output.watch => ContentRead",
    "pane.record.start => PluginsForbidden",
    "pane.record.stop => PluginsForbidden",
    "pane.resize => OwnedMutation",
    "pane.run_command => OwnedMutation",
    "pane.send_keys => OwnedMutation",
    "pane.set_size => OwnedMutation",
    "pane.set_title => OwnedMutation",
    "pane.snapshot => ContentRead",
    "pane.split => OwnedMutation",
    "pane.swap => OwnedMutation",
    "pane.wait_for => ContentRead",
    "pane.wait_settled => ContentRead",
    "pane.zoom => OwnedMutation",
    "plugin.audit => PluginsForbidden",
    "plugin.grant => PluginsForbidden",
    "plugin.grants => PluginsForbidden",
    "plugin.install => PluginsForbidden",
    "plugin.kill => PluginsForbidden",
    "plugin.list => Public",
    "plugin.reload => PluginsForbidden",
    "plugin.revoke => PluginsForbidden",
    "session.create => OwnedMutation",
    "session.ensure => OwnedMutation",
    "session.export_template => Public",
    "session.kill => OwnedMutation",
    "session.list => Public",
    "session.rename => OwnedMutation",
    "session.snapshot => ContentRead",
    "state.apply => Grantable",
    "system.health => Public",
    "system.version => Public",
    "window.create => OwnedMutation",
    "window.ensure => OwnedMutation",
    "window.focus => OwnedMutation",
    "window.kill => OwnedMutation",
    "window.list => Public",
    "window.rename => OwnedMutation",
    "window.reorder => OwnedMutation",
    "window.snapshot => ContentRead",
];

fn describe(policy: &Policy) -> String {
    match policy {
        Policy::Fixed(s) => format!("{s:?}"),
        Policy::ParamAware(_) => {
            let broad = policy.resolve(None, "p");
            let scoped =
                policy.resolve(Some(&serde_json::json!({ "filters": ["plugin.p."] })), "p");
            format!("ParamAware(broad={broad:?}, self_scoped={scoped:?})")
        }
    }
}

fn actual_surface() -> Vec<String> {
    let bus = shux_core::bus::EventBus::new();
    let (_graph, state) = shux_core::graph::SessionGraph::new_with_event_bus(Some(bus.clone()));
    // Registration only stores handles in closures; nothing is dispatched here,
    // so the graph loop and the receiving ends stay unspawned on purpose.
    let (graph_tx, _graph_rx) = tokio::sync::mpsc::channel(1);
    let graph = shux_core::graph::GraphHandle::new(graph_tx, state);
    let io_state = Arc::new(Mutex::new(PaneIoState::new().with_event_bus(bus.clone())));
    let cancel = tokio_util::sync::CancellationToken::new();
    let state_dir = tempfile::tempdir().expect("state dir");
    let lens_audit = crate::lens_scratch::LensAuditLog::open(state_dir.path());
    let scratch_registry =
        crate::lens_scratch::ScratchRegistry::new(state_dir.path(), lens_audit.clone());
    // A path that cannot exist, so `load_or_default` takes the defaults branch
    // rather than whatever the developer running this has in `~/.config`.
    let config = shux_core::config::ConfigHandle::load_or_default(
        &state_dir.path().join("no-such-config.toml"),
    );
    let onboarding = crate::onboarding::OnboardingHandle::from_state_for_test(Default::default());
    let segments = crate::statusbar_runner::SegmentCache::new();
    let plugins =
        shux_plugin::PluginManager::with_state_root(bus.clone(), state_dir.path().join("plugins"));

    let router = super::build_router(
        graph,
        io_state,
        cancel,
        crate::session_meta::SessionMetaCache::new(),
        scratch_registry,
        config,
        onboarding,
        segments,
        lens_audit,
        bus,
        plugins,
    );

    let mut lines: Vec<String> = router
        .methods()
        .into_iter()
        .map(|m| {
            let policy = router
                .policy(m)
                .expect("assert_every_route_has_policy covers this");
            format!("{m} => {}", describe(policy))
        })
        .collect();
    lines.sort();
    lines
}

#[tokio::test]
async fn the_rpc_surface_is_exactly_what_it_was() {
    let actual = actual_surface();
    let expected: Vec<String> = EXPECTED_SURFACE.iter().map(|s| s.to_string()).collect();

    let added: Vec<&String> = actual.iter().filter(|l| !expected.contains(l)).collect();
    let removed: Vec<&String> = expected.iter().filter(|l| !actual.contains(l)).collect();

    assert!(
        added.is_empty() && removed.is_empty(),
        "the RPC surface changed.\n  added:   {added:#?}\n  removed: {removed:#?}",
    );
}

#[tokio::test]
async fn every_registered_method_declares_a_policy() {
    // The same assertion the daemon makes at boot, run without booting one.
    let bus = shux_core::bus::EventBus::new();
    let (_graph, state) = shux_core::graph::SessionGraph::new_with_event_bus(Some(bus.clone()));
    let (graph_tx, _graph_rx) = tokio::sync::mpsc::channel(1);
    let state_dir = tempfile::tempdir().expect("state dir");
    let lens_audit = crate::lens_scratch::LensAuditLog::open(state_dir.path());
    let router = super::build_router(
        shux_core::graph::GraphHandle::new(graph_tx, state),
        Arc::new(Mutex::new(PaneIoState::new().with_event_bus(bus.clone()))),
        tokio_util::sync::CancellationToken::new(),
        crate::session_meta::SessionMetaCache::new(),
        crate::lens_scratch::ScratchRegistry::new(state_dir.path(), lens_audit.clone()),
        shux_core::config::ConfigHandle::load_or_default(
            &state_dir.path().join("no-such-config.toml"),
        ),
        crate::onboarding::OnboardingHandle::from_state_for_test(Default::default()),
        crate::statusbar_runner::SegmentCache::new(),
        lens_audit,
        bus.clone(),
        shux_plugin::PluginManager::with_state_root(bus, state_dir.path().join("plugins")),
    );
    router.assert_every_route_has_policy();
}

#[tokio::test]
async fn the_pin_notices_a_method_that_disappears() {
    // The guard's own failure mode, proved rather than assumed: drop one line
    // from the pin and the comparison must report it.
    let actual = actual_surface();
    let mut expected = actual.clone();
    let dropped = expected.pop().expect("the surface is not empty");
    let removed: Vec<&String> = actual.iter().filter(|l| !expected.contains(l)).collect();
    assert_eq!(removed, vec![&dropped]);
}

/// `Sensitivity` is `Copy` and compared by value; a tier rename or reorder
/// would change the pinned strings above rather than pass silently.
#[test]
fn sensitivity_tiers_render_distinctly() {
    let tiers = [
        Sensitivity::Public,
        Sensitivity::ContentRead,
        Sensitivity::OwnedMutation,
        Sensitivity::Grantable,
        Sensitivity::PluginsForbidden,
    ];
    let rendered: std::collections::HashSet<String> =
        tiers.iter().map(|s| describe(&Policy::fixed(*s))).collect();
    assert_eq!(rendered.len(), tiers.len());
}
