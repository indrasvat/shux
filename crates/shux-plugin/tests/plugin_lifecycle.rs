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
