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
    "client.detach => Grantable",
    "client.list => Public",
    "daemon.retire_if_idle => PluginsForbidden",
    "daemon.upgrade => PluginsForbidden, owner-local",
    "daemon.workspace => Public",
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
    "pane.record.wait => PluginsForbidden",
    "pane.resize => OwnedMutation",
    "pane.run_command => OwnedMutation",
    "pane.send_keys => OwnedMutation",
    "pane.set_size => OwnedMutation",
    "pane.set_title => OwnedMutation",
    "pane.snapshot => ContentRead",
    "pane.status => ContentRead",
    "pane.split => OwnedMutation",
    "pane.swap => OwnedMutation",
    "pane.wait_command => ContentRead",
    "pane.wait_exit => Public",
    "pane.wait_for => ContentRead",
    "pane.wait_settled => ContentRead",
    "pane.wait_status => ContentRead",
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
    "share.confirm => PluginsForbidden, owner-local",
    "share.create => PluginsForbidden, owner-local",
    "share.kick => PluginsForbidden, owner-local",
    "share.list => PluginsForbidden, owner-local",
    "share.revoke => PluginsForbidden, owner-local",
    "share.stop => PluginsForbidden, owner-local",
    "state.apply => Grantable",
    "system.health => Public",
    "system.version => Public",
    "window.create => OwnedMutation",
    "window.ensure => OwnedMutation",
    "window.focus => OwnedMutation",
    "window.kill => OwnedMutation",
    "window.layout => Public",
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
            let scoped = policy.resolve(Some(&serde_json::json!({ "filter": ["plugin.p."] })), "p");
            format!("ParamAware(broad={broad:?}, self_scoped={scoped:?})")
        }
    }
}

fn gateway(
    graph: &shux_core::graph::GraphHandle,
    io_state: &Arc<Mutex<PaneIoState>>,
    config: &shux_core::config::ConfigHandle,
    onboarding: &crate::onboarding::OnboardingHandle,
    segments: &crate::statusbar_runner::SegmentCache,
) -> crate::share::Gateway {
    crate::share::Gateway::new(
        crate::attach::AttachDeps {
            graph: graph.clone(),
            io_state: io_state.clone(),
            config: config.clone(),
            segments: segments.clone(),
            meta_cache: crate::session_meta::SessionMetaCache::new(),
            onboarding: onboarding.clone(),
            daemon_start: std::time::Instant::now(),
        },
        tokio_util::sync::CancellationToken::new(),
    )
}

fn actual_surface() -> Vec<String> {
    let router = daemon_router();
    let mut lines: Vec<String> = router
        .methods()
        .into_iter()
        .map(|m| {
            let policy = router
                .policy(m)
                .expect("assert_every_route_has_policy covers this");
            let local = if router.is_owner_local(m) {
                ", owner-local"
            } else {
                ""
            };
            format!("{m} => {}{local}", describe(policy))
        })
        .collect();
    lines.sort();
    lines
}

/// The daemon's router, built as the daemon builds it.
fn daemon_router() -> shux_rpc::Router {
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

    let share = gateway(&graph, &io_state, &config, &onboarding, &segments);
    super::build_router(
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
        share,
    )
}

/// Every method of the daemon refuses a key it does not take, before its
/// handler runs: the graph loop here is never spawned, so a handler that ran
/// would hang rather than answer.
#[tokio::test]
async fn every_method_refuses_a_parameter_it_does_not_take() {
    let router = daemon_router();
    router.assert_every_route_declares_params();
    let mut methods = router.methods();
    methods.sort_unstable();
    for method in methods {
        let call = router.dispatch(method, Some(serde_json::json!({ "zz_not_a_param": 1 })));
        let err = shux_rpc::with_transport(shux_rpc::Transport::Uds, call)
            .await
            .expect_err(method);
        let data = err.data.unwrap_or_default();
        assert_eq!(data["kind"], shux_rpc::UNKNOWN_PARAMS, "{method}: {data}");
        assert_eq!(
            data["params"],
            serde_json::json!(["zz_not_a_param"]),
            "{method}"
        );
    }
}

/// A field inside `state.apply` ops that this build does not know is refused
/// as `unknown_params` naming it, like a top-level key, at any depth.
#[tokio::test]
async fn an_unknown_op_field_is_refused_as_unknown_params() {
    let router = daemon_router();
    for (op, field) in [
        (
            serde_json::json!({"op": "create_session", "cwd": "/tmp", "zz_new": 1}),
            "ops.zz_new",
        ),
        (
            serde_json::json!({"op": "create_window", "session": {"ref_op": 0, "zz_ref": 1}, "title": "t"}),
            "ops.zz_ref",
        ),
    ] {
        let call = router.dispatch("state.apply", Some(serde_json::json!({ "ops": [op] })));
        let err = shux_rpc::with_transport(shux_rpc::Transport::Uds, call)
            .await
            .expect_err("refused");
        let data = err.data.unwrap_or_default();
        assert_eq!(data["kind"], shux_rpc::UNKNOWN_PARAMS, "{data}");
        assert_eq!(data["params"], serde_json::json!([field]), "{data}");
    }

    // Set to `null`, at any depth, a field asks for nothing, as at the top
    // level: the ops parse, and the batch goes on to the graph.
    let op = serde_json::json!({
        "op": "create_window", "session": {"ref_op": 5, "zz_ref": null},
        "title": "t", "cwd": null, "zz_new": null,
    });
    let call = router.dispatch("state.apply", Some(serde_json::json!({ "ops": [op] })));
    let err = shux_rpc::with_transport(shux_rpc::Transport::Uds, call)
        .await
        .expect_err("this router runs no graph");
    let said = format!("{err:?}");
    assert!(
        !said.contains(shux_rpc::UNKNOWN_PARAMS) && !said.contains("ops parse error"),
        "a null op field was refused: {said}"
    );
}

/// Only a caller on the daemon's own socket speaks for a newer CLI: a
/// plugin's `system.version` neither arms an upgrade nor sets the badge.
#[tokio::test]
async fn a_plugin_cannot_arm_an_upgrade_through_system_version() {
    let router = daemon_router();
    let newer = serde_json::json!({ "client": { "version": "999.0.0", "git_sha": "x" } });
    let answer = router
        .dispatch("system.version", Some(newer))
        .await
        .expect("answered");
    assert!(answer["superseded_by"].is_null(), "{answer}");
}

/// Generation 1 only grows: what `wire_1.rs` records is still served, and
/// what is served is recorded there, so a parameter added after 0.64.0 cannot
/// be removed again without a new generation.
#[test]
fn wire_generation_1_only_grows() {
    use std::collections::{BTreeSet, HashMap};
    assert_eq!(
        shux_rpc::WIRE_GENERATION,
        1,
        "a new generation starts a baseline of its own"
    );
    let served: HashMap<&str, &[&str]> = super::accepts::ACCEPTS.iter().copied().collect();
    let recorded: HashMap<&str, &[&str]> = super::wire_1::WIRE_1.iter().copied().collect();
    for (method, keys) in &recorded {
        let now = served
            .get(method)
            .unwrap_or_else(|| panic!("{method} was removed from generation 1"));
        for key in *keys {
            assert!(now.contains(key), "{method} no longer takes {key}");
        }
    }
    for (method, keys) in &served {
        let then = recorded
            .get(method)
            .unwrap_or_else(|| panic!("{method} is not recorded in wire_1.rs"));
        for key in *keys {
            assert!(
                then.contains(key),
                "{method} {key} is not recorded in wire_1.rs"
            );
        }
    }
    let ops: BTreeSet<&str> = super::wire_1::WIRE_1_OPS
        .iter()
        .flat_map(|(_, fields)| fields.iter().copied())
        .chain(super::wire_1::WIRE_1_BACK_REF.iter().copied())
        .collect();
    let now: BTreeSet<&str> = shux_core::apply::OP_FIELDS.iter().copied().collect();
    assert_eq!(ops, now, "state.apply op fields differ from wire_1.rs");
    // Each kind still takes each of its own fields.
    for (kind, fields) in super::wire_1::WIRE_1_OPS {
        for field in *fields {
            let op = serde_json::json!({ "op": kind, *field: 0 });
            if let Err(e) = serde_json::from_value::<shux_core::apply::Op>(op) {
                assert!(
                    !e.to_string().contains("unknown field"),
                    "{kind} no longer takes {field}: {e}"
                );
            }
        }
    }
}

/// The table names only methods the daemon serves: a stale entry would be
/// published as a capability the daemon does not have.
#[tokio::test]
async fn the_accepted_params_name_only_served_methods() {
    let router = daemon_router();
    let stale: Vec<&str> = super::accepts::ACCEPTS
        .iter()
        .map(|(m, _)| *m)
        .filter(|m| !router.has_method(m))
        .collect();
    assert!(stale.is_empty(), "declared but not served: {stale:?}");
}

/// Within a wire generation the surface only grows: every method and
/// parameter 0.64.0 served is still served. Removing one is a generation bump.
#[test]
fn no_method_or_parameter_of_an_earlier_release_is_gone() {
    assert_eq!(
        shux_rpc::WIRE_GENERATION,
        1,
        "a new generation starts a new baseline"
    );
    let now: std::collections::HashMap<&str, &[&str]> =
        super::accepts::ACCEPTS.iter().copied().collect();
    for (method, keys) in super::legacy_0_64::LEGACY_0_64 {
        let served = now
            .get(method)
            .unwrap_or_else(|| panic!("{method} was removed"));
        for key in *keys {
            assert!(served.contains(key), "{method} no longer takes {key}");
        }
    }
    let legacy_ops = super::legacy_0_64::LEGACY_0_64_OPS.iter();
    for field in legacy_ops
        .flat_map(|(_, fields)| fields.iter())
        .chain(super::legacy_0_64::LEGACY_0_64_BACK_REF)
    {
        assert!(
            shux_core::apply::OP_FIELDS.contains(field),
            "state.apply ops no longer take {field}"
        );
    }
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
    let graph = shux_core::graph::GraphHandle::new(graph_tx, state);
    let io_state = Arc::new(Mutex::new(PaneIoState::new().with_event_bus(bus.clone())));
    let config = shux_core::config::ConfigHandle::load_or_default(
        &state_dir.path().join("no-such-config.toml"),
    );
    let onboarding = crate::onboarding::OnboardingHandle::from_state_for_test(Default::default());
    let segments = crate::statusbar_runner::SegmentCache::new();
    let share = gateway(&graph, &io_state, &config, &onboarding, &segments);
    let router = super::build_router(
        graph,
        io_state,
        tokio_util::sync::CancellationToken::new(),
        crate::session_meta::SessionMetaCache::new(),
        crate::lens_scratch::ScratchRegistry::new(state_dir.path(), lens_audit.clone()),
        config,
        onboarding,
        segments,
        lens_audit,
        bus.clone(),
        shux_plugin::PluginManager::with_state_root(bus, state_dir.path().join("plugins")),
        share,
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
