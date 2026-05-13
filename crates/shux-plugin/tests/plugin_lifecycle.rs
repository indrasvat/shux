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

/// Race regression: two concurrent installs of plugins whose manifests
/// report the same name must result in exactly one success and one
/// NameConflict — never two `Running` entries, never a silently
/// orphaned child. (codex review of PR #23.)
#[tokio::test]
async fn concurrent_installs_with_same_name_dedup() {
    let tmp = tempfile::tempdir().unwrap();
    // Same NOOP manifest reported by two distinct executables: the
    // dedup must key on manifest.name, not the source path.
    let a = write_script(tmp.path(), "a.sh", NOOP_PLUGIN);
    let b = write_script(tmp.path(), "b.sh", NOOP_PLUGIN);

    let mgr = PluginManager::new(EventBus::new());
    let mgr_a = mgr.clone();
    let mgr_b = mgr.clone();
    let (res_a, res_b) = tokio::join!(
        tokio::spawn(async move { mgr_a.install(PluginSource::from_path(&a)).await }),
        tokio::spawn(async move { mgr_b.install(PluginSource::from_path(&b)).await }),
    );
    let res_a = res_a.unwrap();
    let res_b = res_b.unwrap();

    let oks = [&res_a, &res_b].iter().filter(|r| r.is_ok()).count();
    let conflicts = [&res_a, &res_b]
        .iter()
        .filter(|r| matches!(r, Err(shux_plugin::PluginError::NameConflict(_))))
        .count();
    assert_eq!(
        oks, 1,
        "expected exactly one install to win: {res_a:?} / {res_b:?}"
    );
    assert_eq!(conflicts, 1, "expected exactly one NameConflict");

    let listed = mgr.list().await;
    assert_eq!(listed.len(), 1, "exactly one plugin should be registered");
    mgr.kill("noop").await.unwrap();
}

/// Plugin host must deliver events in the same wire shape that
/// `events.watch` and `events.history` produce — top-level `type`,
/// `seq`, `timestamp` (not buried under `meta`). Flattening the
/// inner enum tag to remove the `data.data` re-wrap is a separate
/// ergonomics fix (and a breaking change for existing event
/// consumers); not in scope here. (codex review of PR #23.)
#[tokio::test]
async fn event_frames_use_canonical_wire_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("captured.jsonl");

    // Capture every non-handshake stdin frame to a file so the test
    // can inspect what the daemon actually sent.
    let script = format!(
        r#"#!/usr/bin/env bash
set -u
OUT={out}
IFS= read -r _ || exit 1
printf '%s\n' '{{"jsonrpc":"2.0","id":"init","result":{{"name":"recorder","version":"0.1.0","subscribes":["session.created"],"provides":[],"capabilities":[]}}}}'
while IFS= read -r line; do
  case "$line" in
    *'"plugin.shutdown"'*) exit 0 ;;
    *) printf '%s\n' "$line" >> "$OUT" ;;
  esac
done
"#,
        out = out.display()
    );
    let script_path = write_script(tmp.path(), "recorder.sh", &script);

    let bus = EventBus::new();
    let mgr = PluginManager::new(bus.clone());
    mgr.install(PluginSource::from_path(&script_path))
        .await
        .expect("install");

    let session_id = shux_core::model::SessionId::new();
    bus.publish(shux_core::event::EventData::SessionCreated {
        session_id,
        name: "alpha".into(),
    });

    // Give the plugin a beat to read + flush its capture line.
    for _ in 0..50 {
        if out.exists()
            && std::fs::metadata(&out)
                .map(|m| m.len() > 0)
                .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let captured = std::fs::read_to_string(&out).expect("captured.jsonl");
    let line = captured
        .lines()
        .next()
        .expect("at least one captured frame");
    let frame: serde_json::Value = serde_json::from_str(line).expect("frame is JSON");
    assert_eq!(frame["method"], "event");

    let params = &frame["params"];
    assert_eq!(params["type"], "session.created", "type at top level");
    assert!(params["seq"].is_number(), "seq at top level");
    assert!(params["timestamp"].is_number(), "timestamp at top level");
    assert!(
        params["meta"].is_null(),
        "raw Event.meta envelope must not leak: {params}"
    );
    // Payload lives under data.data today (the serde tag/content shape
    // shared with events.watch). Flattening further is a follow-up.
    // `params.type` (from EventMetadata) is what consumers should
    // filter on — `params.data.type` carries the Rust variant name
    // and is not part of the contract.
    assert_eq!(
        params["data"]["data"]["session_id"],
        serde_json::Value::String(session_id.to_string()),
        "payload reachable at data.data.* (events.watch parity)",
    );

    mgr.kill("recorder").await.unwrap();
}

/// kill() must deliver the `plugin.shutdown` frame to the plugin
/// before the grace window expires. The biased `select!` would
/// otherwise jump to the kill branch first and starve the inbox
/// write. (codex review of PR #23.)
#[tokio::test]
async fn kill_flushes_shutdown_frame_before_grace_window() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("got_shutdown");

    // Plugin writes a marker file the moment it sees `plugin.shutdown`,
    // then exits cleanly. If we force-kill before the frame lands the
    // marker won't exist.
    let script = format!(
        r#"#!/usr/bin/env bash
set -u
MARKER={marker}
IFS= read -r _ || exit 1
printf '%s\n' '{{"jsonrpc":"2.0","id":"init","result":{{"name":"graceful","version":"0.1.0","subscribes":[],"provides":[],"capabilities":[]}}}}'
while IFS= read -r line; do
  case "$line" in
    *'"plugin.shutdown"'*)
      : > "$MARKER"
      exit 0
      ;;
  esac
done
"#,
        marker = marker.display()
    );
    let script_path = write_script(tmp.path(), "graceful.sh", &script);

    let mgr = PluginManager::new(EventBus::new());
    mgr.install(PluginSource::from_path(&script_path))
        .await
        .expect("install");

    mgr.kill("graceful").await.expect("kill");

    // Wait briefly for the plugin to flush + exit (kill returns
    // before the child reaps).
    for _ in 0..50 {
        if marker.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        marker.exists(),
        "plugin never received plugin.shutdown before being killed"
    );
}

/// `event.publish` from a plugin lands on the bus as a
/// `PluginEvent` whose filterable type is `plugin.<id>.<event_type>`.
/// Other subscribers using a prefix filter pick it up exactly.
#[tokio::test]
async fn plugin_can_publish_namespaced_events() {
    let tmp = tempfile::tempdir().unwrap();

    // A plugin that emits one event on startup (no subscribes
    // needed — uses the existing inbox/RPC channel to publish).
    let script = r#"#!/usr/bin/env bash
set -u
IFS= read -r _ || exit 1
printf '%s\n' '{"jsonrpc":"2.0","id":"init","result":{"name":"emitter","version":"0.1.0","subscribes":[],"provides":[],"capabilities":[]}}'
printf '%s\n' '{"jsonrpc":"2.0","method":"event.publish","params":{"event_type":"hello","data":{"answer":42}},"id":1}'
# Block until shutdown so the manager keeps the io task alive long
# enough for the publish to flow.
while IFS= read -r line; do
  case "$line" in
    *'"plugin.shutdown"'*) exit 0 ;;
  esac
done
"#;
    let script_path = write_script(tmp.path(), "emitter.sh", script);

    let bus = EventBus::new();
    // Subscribe BEFORE installing so the publish from the plugin's
    // first read-loop iteration lands on a live receiver.
    let mut sub = bus.subscribe_filtered(vec!["plugin.emitter.".into()]);

    let mgr = PluginManager::new(bus.clone());
    mgr.install(PluginSource::from_path(&script_path))
        .await
        .expect("install");

    // Wait for the namespaced event with a bounded poll.
    let mut got = None;
    for _ in 0..100 {
        match tokio::time::timeout(Duration::from_millis(20), sub.recv()).await {
            Ok(Some(shux_core::bus::SubscriptionEvent::Event(ev))) => {
                got = Some(ev);
                break;
            }
            _ => continue,
        }
    }

    let ev = got.expect("plugin event never landed on the bus");
    assert_eq!(
        ev.meta.event_type, "plugin.emitter.hello",
        "namespaced filterable type"
    );

    match ev.data {
        shux_core::event::EventData::PluginEvent {
            ref plugin_id,
            ref event_type,
            ref data,
        } => {
            assert_eq!(plugin_id, "emitter");
            assert_eq!(event_type, "hello");
            assert_eq!(data["answer"], serde_json::json!(42));
        }
        other => panic!("expected PluginEvent, got {other:?}"),
    }

    mgr.kill("emitter").await.unwrap();
}

/// Install rejects a plugin whose manifest name contains a `.` — the
/// name is used verbatim in the `plugin.<name>.<type>` event
/// namespace, so a name like `git-status.evil` would let it publish
/// events under another plugin's filter prefix
/// (`plugin.git-status.`). (codex bot P1 review on PR #31.)
#[tokio::test]
async fn install_rejects_dotted_plugin_name() {
    let tmp = tempfile::tempdir().unwrap();
    let script = r#"#!/usr/bin/env bash
set -u
IFS= read -r _ || exit 1
printf '%s\n' '{"jsonrpc":"2.0","id":"init","result":{"name":"git-status.evil","version":"0.1.0","subscribes":[],"provides":[],"capabilities":[]}}'
while IFS= read -r _; do :; done
"#;
    let script_path = write_script(tmp.path(), "dotted.sh", script);
    let mgr = PluginManager::new(EventBus::new());
    let err = mgr
        .install(PluginSource::from_path(&script_path))
        .await
        .expect_err("install must reject dotted manifest name");
    let msg = format!("{err}");
    assert!(
        msg.contains("must not contain '.'"),
        "diagnostic must mention dot rule: {msg}"
    );
}

/// `plugin.state.set` followed by `plugin.state.get` round-trips a
/// value through the on-disk store. The first `get` (before any
/// `set`) returns `null` rather than an error.
#[tokio::test]
async fn plugin_state_round_trips_through_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let resp_capture = tmp.path().join("resp.jsonl");
    let state_root = tmp.path().join("plugins");

    // Plugin: read initial state (expect null) → write a value → read
    // it back. Capture all daemon responses to a file.
    let script = format!(
        r#"#!/usr/bin/env bash
set -u
OUT={out}
IFS= read -r _ || exit 1
printf '%s\n' '{{"jsonrpc":"2.0","id":"init","result":{{"name":"persist","version":"0.1.0","subscribes":[],"provides":[],"capabilities":[]}}}}'
printf '%s\n' '{{"jsonrpc":"2.0","method":"plugin.state.get","params":{{}},"id":1}}'
printf '%s\n' '{{"jsonrpc":"2.0","method":"plugin.state.set","params":{{"value":{{"hits":42,"branch":"main"}}}},"id":2}}'
printf '%s\n' '{{"jsonrpc":"2.0","method":"plugin.state.get","params":{{}},"id":3}}'
while IFS= read -r line; do
  case "$line" in
    *'"plugin.shutdown"'*) exit 0 ;;
    *'"id":1'*|*'"id":2'*|*'"id":3'*) printf '%s\n' "$line" >> "$OUT" ;;
  esac
done
"#,
        out = resp_capture.display()
    );
    let script_path = write_script(tmp.path(), "persist.sh", &script);

    let mgr = PluginManager::with_state_root(EventBus::new(), state_root.clone());
    mgr.install(PluginSource::from_path(&script_path))
        .await
        .expect("install");

    for _ in 0..80 {
        let count = std::fs::read_to_string(&resp_capture)
            .map(|s| s.lines().count())
            .unwrap_or(0);
        if count >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let body = std::fs::read_to_string(&resp_capture).expect("captured responses");
    let lines: Vec<&str> = body.lines().collect();
    assert!(
        lines.len() >= 3,
        "want 3 responses, got {}: {body}",
        lines.len()
    );
    let by_id: std::collections::HashMap<i64, serde_json::Value> = lines
        .iter()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .map(|v| (v["id"].as_i64().unwrap(), v))
        .collect();

    // id=1: get before set → null.
    assert_eq!(by_id[&1]["result"]["value"], serde_json::Value::Null);
    // id=2: set → bytes_written > 0.
    assert!(by_id[&2]["result"]["bytes_written"].as_u64().unwrap() > 0);
    // id=3: get after set → the value we wrote.
    assert_eq!(by_id[&3]["result"]["value"]["hits"], serde_json::json!(42));
    assert_eq!(
        by_id[&3]["result"]["value"]["branch"],
        serde_json::json!("main")
    );

    // The on-disk file actually exists at the expected path.
    let expected = state_root.join("persist").join("state.json");
    assert!(
        expected.exists(),
        "state.json must land at {}",
        expected.display()
    );

    mgr.kill("persist").await.unwrap();
}

/// State persists across a plugin's hot reload — the file lives
/// outside the plugin process so a respawned instance reads back
/// what its predecessor wrote.
#[tokio::test]
async fn plugin_state_survives_reload() {
    let tmp = tempfile::tempdir().unwrap();
    let state_root = tmp.path().join("plugins");
    let resp_capture = tmp.path().join("resp.jsonl");
    let phase_file = tmp.path().join("phase");
    std::fs::write(&phase_file, "1").unwrap();

    // The plugin reads PHASE: in phase 1 it writes state and exits;
    // in phase 2 it reads state and dumps the result.
    let script = format!(
        r#"#!/usr/bin/env bash
set -u
OUT={out}
PHASE_FILE={phase}
PHASE=$(cat "$PHASE_FILE")
IFS= read -r _ || exit 1
printf '%s\n' '{{"jsonrpc":"2.0","id":"init","result":{{"name":"reloader","version":"0.1.0","subscribes":[],"provides":[],"capabilities":[]}}}}'
if [ "$PHASE" = "1" ]; then
  printf '%s\n' '{{"jsonrpc":"2.0","method":"plugin.state.set","params":{{"value":{{"phase1":true}}}},"id":1}}'
else
  printf '%s\n' '{{"jsonrpc":"2.0","method":"plugin.state.get","params":{{}},"id":2}}'
fi
while IFS= read -r line; do
  case "$line" in
    *'"plugin.shutdown"'*) exit 0 ;;
    *'"id":1'*|*'"id":2'*) printf '%s\n' "$line" >> "$OUT" ;;
  esac
done
"#,
        out = resp_capture.display(),
        phase = phase_file.display()
    );
    let script_path = write_script(tmp.path(), "reloader.sh", &script);

    let mgr = PluginManager::with_state_root(EventBus::new(), state_root.clone());

    // Phase 1: install, let it write, kill.
    mgr.install(PluginSource::from_path(&script_path))
        .await
        .expect("install phase 1");
    for _ in 0..50 {
        if std::fs::read_to_string(&resp_capture)
            .map(|s| s.contains("\"id\":1"))
            .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    mgr.kill("reloader").await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Phase 2: flip the phase marker, re-install, plugin should
    // now READ the value back.
    std::fs::write(&phase_file, "2").unwrap();
    mgr.install(PluginSource::from_path(&script_path))
        .await
        .expect("install phase 2");
    for _ in 0..80 {
        if std::fs::read_to_string(&resp_capture)
            .map(|s| s.contains("\"id\":2"))
            .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let body = std::fs::read_to_string(&resp_capture).unwrap();
    let id2 = body
        .lines()
        .find_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["id"].as_i64() == Some(2))
        .or_else(|| {
            body.lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .find(|v| v["id"].as_i64() == Some(2))
        })
        .expect("phase 2 response captured");
    assert_eq!(
        id2["result"]["value"]["phase1"],
        serde_json::json!(true),
        "respawned plugin must read what its predecessor wrote: {id2}"
    );

    mgr.kill("reloader").await.unwrap();
}

/// Per-install `state_root` override: when `PluginSource.state_root`
/// is set, `plugin.state.set` writes under THAT path, not the
/// daemon-wide default. Mirrors the CLI's behaviour of pinning
/// state to the calling client's project cwd. (codex P2 review on
/// PR #32.)
#[tokio::test]
async fn plugin_state_honours_per_install_state_root_override() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon_root = tmp.path().join("daemon_default");
    let project_root = tmp.path().join("project_a");
    let resp_capture = tmp.path().join("resp.jsonl");

    let script = format!(
        r#"#!/usr/bin/env bash
set -u
OUT={out}
IFS= read -r _ || exit 1
printf '%s\n' '{{"jsonrpc":"2.0","id":"init","result":{{"name":"projscoped","version":"0.1.0","subscribes":[],"provides":[],"capabilities":[]}}}}'
printf '%s\n' '{{"jsonrpc":"2.0","method":"plugin.state.set","params":{{"value":{{"marker":"project_a"}}}},"id":1}}'
while IFS= read -r line; do
  case "$line" in
    *'"plugin.shutdown"'*) exit 0 ;;
    *'"id":1'*) printf '%s\n' "$line" >> "$OUT" ;;
  esac
done
"#,
        out = resp_capture.display()
    );
    let script_path = write_script(tmp.path(), "projscoped.sh", &script);

    let mgr = PluginManager::with_state_root(EventBus::new(), daemon_root.clone());

    // Install with an explicit per-source state_root override.
    let mut source = PluginSource::from_path(&script_path);
    source.state_root = Some(project_root.clone());
    mgr.install(source).await.expect("install");

    for _ in 0..50 {
        if resp_capture.exists()
            && std::fs::metadata(&resp_capture)
                .map(|m| m.len() > 0)
                .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // The file must live under PROJECT root, not the daemon default.
    let project_path = project_root.join("projscoped").join("state.json");
    let daemon_path = daemon_root.join("projscoped").join("state.json");
    assert!(
        project_path.exists(),
        "state.json must land under the per-install override: {}",
        project_path.display()
    );
    assert!(
        !daemon_path.exists(),
        "state.json must NOT land under the daemon default when override is set: {}",
        daemon_path.display()
    );

    mgr.kill("projscoped").await.unwrap();
}

/// `plugin.state.set` rejects payloads over the 256 KiB cap.
#[tokio::test]
async fn plugin_state_set_rejects_oversized_payload() {
    let tmp = tempfile::tempdir().unwrap();
    let resp_capture = tmp.path().join("resp.jsonl");
    let state_root = tmp.path().join("plugins");

    // Build a 300 KiB string literal in JSON (so total exceeds the
    // 256 KiB cap once serialized).
    let big = "a".repeat(300 * 1024);
    let script = format!(
        r#"#!/usr/bin/env bash
set -u
OUT={out}
IFS= read -r _ || exit 1
printf '%s\n' '{{"jsonrpc":"2.0","id":"init","result":{{"name":"big","version":"0.1.0","subscribes":[],"provides":[],"capabilities":[]}}}}'
printf '%s\n' '{{"jsonrpc":"2.0","method":"plugin.state.set","params":{{"value":"{big}"}},"id":99}}'
while IFS= read -r line; do
  case "$line" in
    *'"plugin.shutdown"'*) exit 0 ;;
    *'"id":99'*) printf '%s\n' "$line" >> "$OUT" ;;
  esac
done
"#,
        out = resp_capture.display(),
        big = big,
    );
    let script_path = write_script(tmp.path(), "big.sh", &script);

    let mgr = PluginManager::with_state_root(EventBus::new(), state_root);
    mgr.install(PluginSource::from_path(&script_path))
        .await
        .expect("install");

    for _ in 0..50 {
        if resp_capture.exists()
            && std::fs::metadata(&resp_capture)
                .map(|m| m.len() > 0)
                .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let body = std::fs::read_to_string(&resp_capture).expect("response captured");
    let resp: serde_json::Value = serde_json::from_str(body.lines().next().unwrap()).unwrap();
    assert_eq!(resp["id"], 99);
    assert_eq!(resp["error"]["code"].as_i64(), Some(-32602));
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("exceeds cap"),
        "diagnostic must mention size cap: {resp}"
    );

    mgr.kill("big").await.unwrap();
}

/// `event.publish` rejects an event_type with embedded dots, because
/// that would let a plugin synthesise an event under a sibling's
/// namespace (`plugin.emitter.other.evil`).
#[tokio::test]
async fn plugin_event_publish_rejects_dotted_type() {
    let tmp = tempfile::tempdir().unwrap();
    let resp_capture = tmp.path().join("resp.jsonl");

    let script = format!(
        r#"#!/usr/bin/env bash
set -u
OUT={out}
IFS= read -r _ || exit 1
printf '%s\n' '{{"jsonrpc":"2.0","id":"init","result":{{"name":"badnames","version":"0.1.0","subscribes":[],"provides":[],"capabilities":[]}}}}'
printf '%s\n' '{{"jsonrpc":"2.0","method":"event.publish","params":{{"event_type":"a.b","data":{{}}}},"id":42}}'
# capture the daemon's response on stdin and exit
while IFS= read -r line; do
  case "$line" in
    *'"plugin.shutdown"'*) exit 0 ;;
    *'"id":42'*) printf '%s\n' "$line" >> "$OUT" ;;
  esac
done
"#,
        out = resp_capture.display()
    );
    let script_path = write_script(tmp.path(), "badnames.sh", &script);

    let mgr = PluginManager::new(EventBus::new());
    mgr.install(PluginSource::from_path(&script_path))
        .await
        .expect("install");

    for _ in 0..50 {
        if resp_capture.exists()
            && std::fs::metadata(&resp_capture)
                .map(|m| m.len() > 0)
                .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let body = std::fs::read_to_string(&resp_capture).expect("response captured");
    let resp: serde_json::Value = serde_json::from_str(body.lines().next().unwrap()).unwrap();
    assert_eq!(resp["id"], 42);
    assert!(
        resp["error"]["code"].as_i64() == Some(-32602),
        "want invalid_params: {resp}"
    );
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("must not contain '.'"),
        "diagnostic must mention dot rule: {resp}"
    );

    mgr.kill("badnames").await.unwrap();
}
