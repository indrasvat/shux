//! Integration tests for the plugin permission/audit model.
//!
//! Exercises the full chain — install → grants file write → plugin
//! RPC frame → decision → audit log entry — with bash plugins driving
//! a real `PluginManager` and `Router`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use shux_core::bus::EventBus;
use shux_plugin::{PluginManager, PluginSource};
use shux_rpc::{Policy, Router, Sensitivity};

fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path
}

/// Build a router with a single test method classified at the given
/// sensitivity. Plugin host dispatches into this when the plugin
/// makes the matching RPC frame.
fn router_with(method: &str, sensitivity: Sensitivity) -> Router {
    use std::sync::Mutex;
    static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());
    Router::builder()
        .register_with_policy(
            method.to_string(),
            Policy::fixed(sensitivity),
            move |params: Option<Value>| async move {
                let _ = params; // unused
                SEEN.lock().unwrap().push("dispatched".into());
                Ok(json!({"ok": true}))
            },
        )
        .build()
}

/// Plugin that calls one RPC on startup, prints the response to
/// stderr (relayed to daemon log) AND to a sentinel file passed in
/// argv[1], then sits until plugin.shutdown.
fn call_one_method_plugin(method: &str, params_json: &str) -> String {
    format!(
        r#"#!/usr/bin/env bash
set -u
out_file="$1"
IFS= read -r _ || exit 1
printf '%s\n' '{{"jsonrpc":"2.0","id":"init","result":{{"name":"perm-tester","version":"0.1.0","subscribes":[],"provides":[],"capabilities":[]}}}}'

# Issue the call
printf '{{"jsonrpc":"2.0","method":"{method}","params":{params_json},"id":1}}\n'

# Read first response and dump to sentinel file
IFS= read -r resp
printf '%s' "$resp" > "$out_file"

while IFS= read -r line; do
  case "$line" in
    *'"plugin.shutdown"'*) exit 0 ;;
  esac
done
"#
    )
}

#[tokio::test]
async fn plugins_forbidden_method_is_denied_even_with_grant() {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = PluginManager::with_state_root(EventBus::new(), tmp.path().to_path_buf());
    let router = router_with("plugin.install", Sensitivity::PluginsForbidden);
    mgr.set_router(router);

    let sentinel = tmp.path().join("resp.json");
    let script = write_script(
        tmp.path(),
        "p.sh",
        &call_one_method_plugin("plugin.install", "{}"),
    );
    let mut source = PluginSource::from_path(&script);
    source.args = vec![sentinel.display().to_string()];
    let info = mgr.install(source).await.expect("install");

    // Even with an explicit (blanket) grant, plugins.install must be denied.
    mgr.grant("perm-tester", "plugin.install", None, false)
        .await
        .unwrap();

    // Wait for the plugin to issue + read the response
    let resp = wait_for_file(&sentinel, Duration::from_secs(5)).await;
    let v: Value = serde_json::from_str(&resp).unwrap();
    let err = v.get("error").expect("expected error response");
    assert_eq!(err["code"], -32004);
    assert_eq!(err["data"]["method"], "plugin.install");
    assert_eq!(err["data"]["reason"], "plugins_forbidden");

    mgr.kill(&info.name).await.unwrap();
}

#[tokio::test]
async fn unowned_owned_mutation_denied_then_allowed_via_grant() {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = PluginManager::with_state_root(EventBus::new(), tmp.path().to_path_buf());
    let router = router_with("pane.send_keys", Sensitivity::OwnedMutation);
    mgr.set_router(router);

    let sentinel = tmp.path().join("resp.json");
    let script = write_script(
        tmp.path(),
        "p.sh",
        &call_one_method_plugin(
            "pane.send_keys",
            r#"{"pane_id":"00000000-0000-0000-0000-000000000001","data":"x"}"#,
        ),
    );
    let mut source = PluginSource::from_path(&script);
    source.args = vec![sentinel.display().to_string()];
    let info = mgr.install(source).await.unwrap();

    // First call: no grant, no ownership → deny.
    let resp = wait_for_file(&sentinel, Duration::from_secs(5)).await;
    let v: Value = serde_json::from_str(&resp).unwrap();
    let err = v.get("error").expect("first call must be denied");
    assert_eq!(err["code"], -32004);
    assert_eq!(err["data"]["reason"], "no_grant_and_not_owned");

    mgr.kill(&info.name).await.unwrap();

    // Add per-target grant, re-install (different script for fresh sentinel)
    let sentinel2 = tmp.path().join("resp2.json");
    let script2 = write_script(
        tmp.path(),
        "p2.sh",
        &call_one_method_plugin(
            "pane.send_keys",
            r#"{"pane_id":"00000000-0000-0000-0000-000000000001","data":"x"}"#,
        ),
    );
    let mut source2 = PluginSource::from_path(&script2);
    source2.args = vec![sentinel2.display().to_string()];
    let info2 = mgr.install(source2).await.unwrap();

    mgr.grant(
        "perm-tester",
        "pane.send_keys",
        Some("00000000-0000-0000-0000-000000000001"),
        false,
    )
    .await
    .unwrap();

    let resp2 = wait_for_file(&sentinel2, Duration::from_secs(5)).await;
    let v2: Value = serde_json::from_str(&resp2).unwrap();
    assert!(
        v2.get("result").is_some(),
        "expected success after grant, got {v2:?}"
    );

    mgr.kill(&info2.name).await.unwrap();
}

#[tokio::test]
async fn audit_log_records_allow_and_deny() {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = PluginManager::with_state_root(EventBus::new(), tmp.path().to_path_buf());
    let router = router_with("pane.snapshot", Sensitivity::ContentRead);
    mgr.set_router(router);

    let sentinel = tmp.path().join("resp.json");
    let script = write_script(
        tmp.path(),
        "p.sh",
        &call_one_method_plugin(
            "pane.snapshot",
            r#"{"pane_id":"00000000-0000-0000-0000-000000000abc"}"#,
        ),
    );
    let mut source = PluginSource::from_path(&script);
    source.args = vec![sentinel.display().to_string()];
    let info = mgr.install(source).await.unwrap();

    // Wait for plugin to fire its RPC + record the audit entry.
    let _ = wait_for_file(&sentinel, Duration::from_secs(5)).await;
    tokio::time::sleep(Duration::from_millis(150)).await;

    let path = mgr.audit_path(&info.name).await.unwrap();
    let body = std::fs::read_to_string(&path).expect("audit log should exist");
    assert!(body.contains("\"pane.snapshot\""), "{body}");
    assert!(body.contains("\"deny\""), "{body}");
    assert!(body.contains("no_grant_and_not_owned"), "{body}");

    mgr.kill(&info.name).await.unwrap();
}

#[tokio::test]
async fn plugin_self_namespace_events_publish_audited_but_allowed() {
    // event.publish is a plugin-only intercept — it never hits the
    // router. The audit entry should still land, marked plugin_self_namespace.
    let tmp = tempfile::tempdir().unwrap();
    let mgr = PluginManager::with_state_root(EventBus::new(), tmp.path().to_path_buf());
    // Empty router is fine — the call doesn't reach it.
    mgr.set_router(Router::builder().build());

    let sentinel = tmp.path().join("resp.json");
    let script = write_script(
        tmp.path(),
        "p.sh",
        &call_one_method_plugin(
            "event.publish",
            r#"{"event_type":"thing_happened","data":{"k":"v"}}"#,
        ),
    );
    let mut source = PluginSource::from_path(&script);
    source.args = vec![sentinel.display().to_string()];
    let info = mgr.install(source).await.unwrap();

    let resp = wait_for_file(&sentinel, Duration::from_secs(5)).await;
    let v: Value = serde_json::from_str(&resp).unwrap();
    assert!(v.get("result").is_some(), "{v:?}");

    let audit = std::fs::read_to_string(mgr.audit_path(&info.name).await.unwrap()).unwrap();
    assert!(audit.contains("\"event.publish\""), "{audit}");
    assert!(audit.contains("plugin_self_namespace"), "{audit}");

    mgr.kill(&info.name).await.unwrap();
}

#[tokio::test]
async fn manifest_subscribes_locked_after_first_install() {
    // First install snapshots manifest.subscribes. A second install
    // with a NEW filter fails unless the user has granted it.
    let tmp = tempfile::tempdir().unwrap();
    let mgr = PluginManager::with_state_root(EventBus::new(), tmp.path().to_path_buf());

    let v1 = r#"#!/usr/bin/env bash
set -u
IFS= read -r _ || exit 1
printf '%s\n' '{"jsonrpc":"2.0","id":"init","result":{"name":"sub-tester","version":"0.1.0","subscribes":["pane.exited"],"provides":[],"capabilities":[]}}'
while IFS= read -r line; do
  case "$line" in
    *'"plugin.shutdown"'*) exit 0 ;;
  esac
done
"#;
    let p1 = write_script(tmp.path(), "v1.sh", v1);
    mgr.install(PluginSource::from_path(&p1)).await.unwrap();
    mgr.kill("sub-tester").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Re-install with a wider subscribes — should fail.
    let v2 = r#"#!/usr/bin/env bash
set -u
IFS= read -r _ || exit 1
printf '%s\n' '{"jsonrpc":"2.0","id":"init","result":{"name":"sub-tester","version":"0.2.0","subscribes":["pane.exited","pane.input.keystroke"],"provides":[],"capabilities":[]}}'
while IFS= read -r line; do
  case "$line" in
    *'"plugin.shutdown"'*) exit 0 ;;
  esac
done
"#;
    let p2 = write_script(tmp.path(), "v2.sh", v2);
    let err = mgr
        .install(PluginSource::from_path(&p2))
        .await
        .expect_err("should fail without grant");
    let msg = format!("{err}");
    assert!(
        msg.contains("pane.input.keystroke"),
        "expected error to name the new filter, got: {msg}"
    );

    // Grant the new filter, then re-install — succeeds.
    mgr.grant("sub-tester", "pane.input.keystroke", None, true)
        .await
        .unwrap();
    let info = mgr.install(PluginSource::from_path(&p2)).await.unwrap();
    assert_eq!(info.subscribes.len(), 2);
    mgr.kill(&info.name).await.unwrap();
}

#[tokio::test]
async fn plugin_id_survives_kill_and_reinstall() {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = PluginManager::with_state_root(EventBus::new(), tmp.path().to_path_buf());
    let body = r#"#!/usr/bin/env bash
set -u
IFS= read -r _ || exit 1
printf '%s\n' '{"jsonrpc":"2.0","id":"init","result":{"name":"stable-id","version":"0.1.0","subscribes":[],"provides":[],"capabilities":[]}}'
while IFS= read -r line; do
  case "$line" in
    *'"plugin.shutdown"'*) exit 0 ;;
  esac
done
"#;
    let script = write_script(tmp.path(), "stable.sh", body);

    let info1 = mgr.install(PluginSource::from_path(&script)).await.unwrap();
    let id1 = info1.plugin_id.expect("plugin_id populated");
    mgr.kill("stable-id").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let info2 = mgr.install(PluginSource::from_path(&script)).await.unwrap();
    let id2 = info2.plugin_id.expect("plugin_id populated on re-install");
    assert_eq!(id1, id2, "stable across uninstall");

    mgr.kill("stable-id").await.unwrap();
}

async fn wait_for_file(path: &Path, timeout: Duration) -> String {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if let Ok(s) = std::fs::read_to_string(path)
            && !s.is_empty()
        {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    panic!("sentinel never appeared at {}", path.display());
}
