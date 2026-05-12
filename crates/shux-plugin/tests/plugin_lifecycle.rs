//! Integration tests for the process plugin host.
//!
//! Each test writes a small bash script into a tempdir, installs it
//! as a plugin, exercises one slice of the protocol, then kills it.
//! Bash is the lowest-common-denominator way to demonstrate the
//! handshake without dragging in a runtime.

use std::path::PathBuf;
use std::time::Duration;

use shux_core::bus::EventBus;
use shux_plugin::{PluginManager, PluginSource};

fn write_script(dir: &std::path::Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path
}

/// A no-op plugin: handshakes, then sits in a read loop until
/// plugin.shutdown arrives, then exits 0.
const NOOP_PLUGIN: &str = r#"#!/usr/bin/env bash
set -u
IFS= read -r _ || exit 1
printf '%s\n' '{"jsonrpc":"2.0","id":"init","result":{"name":"noop","version":"0.1.0","subscribes":[],"provides":[],"capabilities":[]}}'
while IFS= read -r line; do
  case "$line" in
    *'"plugin.shutdown"'*) exit 0 ;;
  esac
done
"#;

#[tokio::test]
async fn install_lists_and_kills_a_plugin() {
    let tmp = tempfile::tempdir().unwrap();
    let script = write_script(tmp.path(), "noop.sh", NOOP_PLUGIN);

    let mgr = PluginManager::new(EventBus::new());
    let info = mgr
        .install(PluginSource::from_path(&script))
        .await
        .expect("install");
    assert_eq!(info.name, "noop");
    assert_eq!(info.version, "0.1.0");
    assert_eq!(info.status, "running");
    assert!(info.pid.is_some());

    let listed = mgr.list().await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "noop");

    mgr.kill("noop").await.expect("kill");

    // The kill removes the entry synchronously even though the child
    // is reaped asynchronously.
    let listed = mgr.list().await;
    assert!(listed.is_empty());
}

#[tokio::test]
async fn install_rejects_duplicate_names() {
    let tmp = tempfile::tempdir().unwrap();
    let a = write_script(tmp.path(), "a.sh", NOOP_PLUGIN);
    let b = write_script(tmp.path(), "b.sh", NOOP_PLUGIN);

    let mgr = PluginManager::new(EventBus::new());
    mgr.install(PluginSource::from_path(&a)).await.unwrap();

    let err = mgr.install(PluginSource::from_path(&b)).await.unwrap_err();
    assert!(matches!(err, shux_plugin::PluginError::NameConflict(ref n) if n == "noop"));

    mgr.kill("noop").await.unwrap();
}

#[tokio::test]
async fn install_times_out_on_silent_plugin() {
    let tmp = tempfile::tempdir().unwrap();
    // Plugin reads init but never writes a manifest — should time
    // out after HANDSHAKE_TIMEOUT (5s).
    let silent = r#"#!/usr/bin/env bash
IFS= read -r _ || exit 1
sleep 30
"#;
    let script = write_script(tmp.path(), "silent.sh", silent);

    let mgr = PluginManager::new(EventBus::new());
    let start = std::time::Instant::now();
    let err = mgr
        .install(PluginSource::from_path(&script))
        .await
        .unwrap_err();
    let elapsed = start.elapsed();

    assert!(matches!(err, shux_plugin::PluginError::HandshakeFailed(_)));
    // Must hit within ~6s (5s budget + slack); must not return early.
    assert!(elapsed >= Duration::from_secs(4));
    assert!(elapsed < Duration::from_secs(7));
}

#[tokio::test]
async fn install_rejects_garbage_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let bad = r#"#!/usr/bin/env bash
IFS= read -r _ || exit 1
printf 'not json at all\n'
sleep 1
"#;
    let script = write_script(tmp.path(), "bad.sh", bad);

    let mgr = PluginManager::new(EventBus::new());
    let err = mgr
        .install(PluginSource::from_path(&script))
        .await
        .unwrap_err();
    assert!(matches!(err, shux_plugin::PluginError::HandshakeFailed(_)));
}

#[tokio::test]
async fn kill_unknown_plugin_returns_not_found() {
    let mgr = PluginManager::new(EventBus::new());
    let err = mgr.kill("ghost").await.unwrap_err();
    assert!(matches!(err, shux_plugin::PluginError::NotFound(ref n) if n == "ghost"));
}
