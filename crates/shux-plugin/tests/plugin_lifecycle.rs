//! Integration tests for the process plugin host.
//!
//! Each test writes a small bash script into a tempdir, installs it
//! as a plugin, exercises one slice of the protocol, then kills it.
//! Bash is the lowest-common-denominator way to demonstrate the
//! handshake without dragging in a runtime.

use std::path::{Path, PathBuf};
use std::time::Duration;

use shux_core::bus::EventBus;
use shux_plugin::{HANDSHAKE_TIMEOUT, PluginManager, PluginSource};
use shux_testkit::{BUDGET, gate, open_gate_async, wait_for_async, wait_until_async};

/// How many processes on this machine have `needle` in their argv.
///
/// `ps` is a machine-global view, so every caller MUST pass a needle that is
/// unique to its own run. A shared needle (`"sleep 30"` was the original)
/// silently reads other suites' processes: the assertion still passes, but it
/// is no longer about the thing under test — and once the suite runs in
/// parallel it is a coin flip.
fn count_procs_containing(needle: &str) -> usize {
    let out = std::process::Command::new("ps")
        .args(["-axo", "args="])
        .output()
        .expect("run ps");
    assert!(
        out.status.success(),
        "ps failed: a failed scan would count 0"
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains(needle))
        .count()
}

/// Wait for `count_procs_containing(needle)` to equal `want`; returns the
/// count it last saw, so a caller asserts on it.
///
/// Polls on the blocking pool, never on the runtime thread: these tests run on
/// a current-thread runtime, whose I/O tasks must keep running to do the
/// killing. A blocking task also holds tokio's paused clock still, so this is
/// safe under `start_paused` too.
async fn wait_for_procs(needle: &str, want: usize) -> usize {
    let needle = needle.to_owned();
    tokio::task::spawn_blocking(move || {
        shux_testkit::poll_until(BUDGET, || count_procs_containing(&needle) == want);
        count_procs_containing(&needle)
    })
    .await
    .expect("the ps poll panicked")
}

async fn wait_for_no_procs(needle: &str) -> usize {
    wait_for_procs(needle, 0).await
}

/// The text of `path` once it holds `lines` complete lines.
async fn wait_for_lines(path: &Path, lines: usize) -> String {
    let what = format!("{lines} complete line(s) in {}", path.display());
    wait_for_async(&what, || async {
        std::fs::read_to_string(path)
            .ok()
            .filter(|s| s.ends_with('\n') && s.lines().count() >= lines)
    })
    .await
}

/// Wait on the blocking pool for `path` to exist. Safe under a paused clock.
async fn wait_for_path(path: &Path) {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        shux_testkit::wait_until(&format!("{} to exist", path.display()), || path.exists());
    })
    .await
    .expect("the path wait panicked");
}

/// Keeps tokio's paused clock still until dropped.
///
/// While any blocking task runs, a paused runtime does not auto-advance its
/// clock, so virtual time moves only on `tokio::time::advance`. A handshake
/// budget then expires exactly when the test says so, however long the real
/// plugin takes to be scheduled.
struct ClockHold {
    _release: std::sync::mpsc::Sender<()>,
}

fn hold_clock() -> ClockHold {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    tokio::task::spawn_blocking(move || {
        let _ = rx.recv();
    });
    ClockHold { _release: tx }
}

/// Await `task` for at most [`BUDGET`] of REAL time, also under a held clock
/// where `tokio::time::timeout` would never fire.
async fn within_budget<T: Send + 'static>(task: tokio::task::JoinHandle<T>) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    tokio::spawn(async move {
        let _ = tx.send(task.await);
    });
    tokio::task::spawn_blocking(move || rx.recv_timeout(BUDGET))
        .await
        .expect("the budget wait panicked")
        .unwrap_or_else(|_| panic!("the task was still running after {BUDGET:?}"))
        .expect("the task panicked")
}

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

/// The handshake budget fires on a plugin that never answers, and the plugin
/// goes with it. On tokio's paused clock the budget expires when the test
/// advances it, not when a loaded machine gets round to it.
#[tokio::test(start_paused = true)]
async fn install_times_out_on_silent_plugin() {
    let tmp = tempfile::tempdir().unwrap();
    // A run-unique marker: `shux`'s own daemon-lifecycle unit test scans the
    // global process table for a bare `sleep 30`.
    let marker = unique_marker("silent");
    let silent = format!(
        r#"#!/usr/bin/env bash
IFS= read -r _ || exit 1
exec -a {marker} sleep 300
"#
    );
    let script = write_script(tmp.path(), "silent.sh", &silent);

    let _hold = hold_clock();
    let mgr = PluginManager::with_state_root(EventBus::new(), tmp.path().join("state"));
    let start = tokio::time::Instant::now();
    let install = tokio::spawn({
        let mgr = mgr.clone();
        async move { mgr.install(PluginSource::from_path(&script)).await }
    });
    // The plugin has read `plugin.init`, so its budget is running.
    assert_eq!(
        wait_for_procs(&marker, 1).await,
        1,
        "the silent plugin never started"
    );
    tokio::time::advance(HANDSHAKE_TIMEOUT).await;
    let err = within_budget(install).await.unwrap_err();

    assert!(
        matches!(&err, shux_plugin::PluginError::HandshakeFailed(m)
            if m.contains("manifest not received")),
        "want the handshake budget to fire, got: {err}"
    );
    assert_eq!(start.elapsed(), HANDSHAKE_TIMEOUT);
    assert_eq!(
        wait_for_no_procs(&marker).await,
        0,
        "the timed-out plugin's own process survived the kill"
    );
}

/// A marker string unique to this process and this call site, safe to grep
/// the global process table for.
fn unique_marker(tag: &str) -> String {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "shuxplug-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// Killing a plugin must kill everything the plugin started.
///
/// `Child::kill` signals the direct child and nothing else. Plugins are
/// scripts as often as not, and a script's children outlive it: before this
/// was fixed, a plugin that failed its handshake left its entire subtree
/// running, reparented to init, with no record anywhere that it had existed.
/// On a developer's machine that is a stray process per failed install; in a
/// long-lived daemon it accumulates.
///
/// Seen failing first: without the process-group kill this leaves the
/// `descendant` process alive for its full 45s and the assertion trips.
#[tokio::test]
async fn a_killed_plugin_takes_its_whole_process_tree_with_it() {
    let tmp = tempfile::tempdir().unwrap();
    let own = unique_marker("self");
    let child = unique_marker("descendant");

    // Handshakes correctly, then forks a grandchild and parks. Both the
    // plugin and its grandchild carry distinct greppable argv markers.
    let body = format!(
        r#"#!/usr/bin/env bash
set -u
IFS= read -r _ || exit 1
printf '%s\n' '{{"jsonrpc":"2.0","id":"init","result":{{"name":"forker","version":"0.1.0","subscribes":[],"provides":[],"capabilities":[]}}}}'
( exec -a {child} sleep 45 ) &
exec -a {own} sleep 45
"#
    );
    let script = write_script(tmp.path(), "forker.sh", &body);

    let mgr = PluginManager::new(EventBus::new());
    mgr.install(PluginSource::from_path(&script)).await.unwrap();

    // Both must actually be running, or the test proves nothing.
    assert_eq!(
        wait_for_procs(&child, 1).await,
        1,
        "the plugin never started its grandchild — the test would pass vacuously"
    );
    assert_eq!(
        wait_for_procs(&own, 1).await,
        1,
        "the plugin never became its marked self — the test would pass vacuously"
    );

    mgr.kill("forker").await.unwrap();

    assert_eq!(
        wait_for_no_procs(&own).await,
        0,
        "the plugin process itself survived `kill`"
    );
    assert_eq!(
        wait_for_no_procs(&child).await,
        0,
        "the plugin died but the process IT spawned was left running"
    );
}

#[tokio::test]
async fn install_rejects_garbage_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    // The plugin forks a grandchild before writing its garbage manifest.
    //
    // It used to be a leaf (`printf …; sleep 1`), which made the test blind to
    // the thing most likely to go wrong on this path: a rejected manifest
    // returns through `?`, and `?` drops the child, and dropping reaches only
    // the leader. A leaf plugin has nothing to leak, so the assertion passed
    // whether or not the subtree was being cleaned up.
    let child = unique_marker("garbage-descendant");
    let bad = format!(
        r#"#!/usr/bin/env bash
IFS= read -r _ || exit 1
( exec -a {child} sleep 45 ) &
until ps -o args= -p $! | grep -q '^{child} '; do :; done
printf 'not json at all\n'
IFS= read -r _
"#
    );
    let script = write_script(tmp.path(), "bad.sh", &bad);

    let mgr = PluginManager::new(EventBus::new());
    let err = mgr
        .install(PluginSource::from_path(&script))
        .await
        .unwrap_err();
    assert!(matches!(err, shux_plugin::PluginError::HandshakeFailed(_)));

    assert_eq!(
        wait_for_no_procs(&child).await,
        0,
        "the rejected plugin's grandchild outlived the failed install"
    );
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

    let captured = wait_for_lines(&out, 1).await;
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

    // `kill` returns before the plugin has read the frame.
    wait_until_async(
        "the plugin to receive plugin.shutdown before being killed",
        || async { marker.exists() },
    )
    .await;
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

    let ev = tokio::time::timeout(BUDGET, async {
        loop {
            match sub.recv().await {
                Some(shux_core::bus::SubscriptionEvent::Event(ev)) => break ev,
                Some(shux_core::bus::SubscriptionEvent::Lagged(_)) => continue,
                None => panic!("the bus closed before the plugin event landed"),
            }
        }
    })
    .await
    .expect("plugin event never landed on the bus");
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

    let body = wait_for_lines(&resp_capture, 3).await;
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
    // The response to the set is written only after the state file is.
    wait_for_lines(&resp_capture, 1).await;
    mgr.kill("reloader").await.unwrap();

    // Phase 2: flip the phase marker, re-install, plugin should
    // now READ the value back.
    std::fs::write(&phase_file, "2").unwrap();
    mgr.install(PluginSource::from_path(&script_path))
        .await
        .expect("install phase 2");
    let body = wait_for_lines(&resp_capture, 2).await;
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

    wait_for_lines(&resp_capture, 1).await;

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

    wait_for_lines(&resp_capture, 1).await;

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

    wait_for_lines(&resp_capture, 1).await;

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

/// A plugin that is ALIVE but has not been scheduled yet must still install.
///
/// This is issue #160. `HANDSHAKE_TIMEOUT` was a 5s wall-clock budget for a
/// freshly spawned process to emit one line, and on a loaded machine that is
/// not enough: 4 of 5 CI reps went red with
/// `HandshakeFailed("manifest not received within 5s")` once the uncapped
/// test pool got more expensive. Nothing was broken — the machine was busy.
///
/// A plugin held at a gate reproduces starvation exactly as the manager sees
/// it: child alive, stdout open, no line yet. On the paused clock it stays
/// silent for exactly 8s, past the 5s budget that failed CI in #160.
#[tokio::test(start_paused = true)]
async fn install_survives_a_plugin_that_is_slow_to_hand_shake() {
    let tmp = tempfile::tempdir().unwrap();
    let ready = tmp.path().join("ready");
    let held = gate(tmp.path(), "gate");
    let slow = format!(
        r#"#!/usr/bin/env bash
set -u
IFS= read -r _ || exit 1
: > {ready}
IFS= read -r _ < {held}
printf '%s\n' '{{"jsonrpc":"2.0","id":"init","result":{{"name":"slowpoke","version":"0.1.0","subscribes":[],"provides":[],"capabilities":[]}}}}'
while IFS= read -r line; do
  case "$line" in
    *'"plugin.shutdown"'*) exit 0 ;;
  esac
done
"#,
        ready = ready.display(),
        held = held.display()
    );
    let script = write_script(tmp.path(), "slow.sh", &slow);

    let hold = hold_clock();
    let mgr = PluginManager::with_state_root(EventBus::new(), tmp.path().join("state"));
    let install = tokio::spawn({
        let mgr = mgr.clone();
        async move { mgr.install(PluginSource::from_path(&script)).await }
    });
    // The plugin has read `plugin.init`, so its budget is running.
    wait_for_path(&ready).await;
    tokio::time::advance(Duration::from_secs(8)).await;
    open_gate_async(&held).await;
    let info = within_budget(install)
        .await
        .expect("a plugin that is merely slow must not be mistaken for a broken one");
    assert_eq!(info.name, "slowpoke");

    drop(hold);
    mgr.kill("slowpoke").await.unwrap();
    mgr.shutdown().await;
}

/// The other half of #160: raising the budget must NOT slow down detection of
/// a plugin that actually died. That case never depended on the timeout —
/// stdout closes and the read returns EOF — so it fails with the paused clock
/// never having moved.
#[tokio::test(start_paused = true)]
async fn install_still_fails_fast_when_the_plugin_exits_without_a_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let dies = r#"#!/usr/bin/env bash
IFS= read -r _ || exit 1
exit 3
"#;
    let script = write_script(tmp.path(), "dies.sh", dies);

    let _hold = hold_clock();
    let mgr = PluginManager::with_state_root(EventBus::new(), tmp.path().join("state"));
    let start = tokio::time::Instant::now();
    let err = within_budget(tokio::spawn(async move {
        mgr.install(PluginSource::from_path(&script)).await
    }))
    .await
    .expect_err("a plugin that exits without a manifest must fail");
    assert_eq!(
        start.elapsed(),
        Duration::ZERO,
        "a dead plugin must be detected by EOF, not by a timer; error was: {err}"
    );
}

/// Cancelling an install must take the plugin's whole process group with it.
///
/// Raised by review on #160. `install` is awaited inside an RPC handler, so a
/// client that disconnects mid-handshake drops the future. Drop only runs
/// tokio's `kill_on_drop`, which signals the plugin LEADER — descendants it
/// forked survive, exactly the leak `kill_plugin_tree` exists to prevent on
/// every other exit path. Widening the handshake budget widens that window, so
/// the guard has to be cancellation-safe, not timeout-safe.
#[tokio::test]
async fn cancelling_an_install_still_reaps_the_plugin_process_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = unique_marker("cancelled-install");
    // Forks a marked grandchild, then never hands back a manifest. The
    // grandchild is what `kill_on_drop` alone cannot reach.
    let forker = format!(
        r#"#!/usr/bin/env bash
IFS= read -r _ || exit 1
exec -a {marker} sleep 120 &
sleep 120
"#
    );
    let script = write_script(tmp.path(), "forker.sh", &forker);

    let mgr = PluginManager::with_state_root(EventBus::new(), tmp.path().join("state"));
    let install = tokio::spawn({
        let mgr = mgr.clone();
        async move { mgr.install(PluginSource::from_path(&script)).await }
    });
    assert_eq!(
        wait_for_procs(&marker, 1).await,
        1,
        "the plugin never forked its grandchild — the test would pass vacuously"
    );
    // Drop the install future mid-handshake, the way a disconnecting client does.
    install.abort();
    let outcome = install.await;
    assert!(
        outcome.as_ref().is_err_and(|e| e.is_cancelled()),
        "the install should not have completed: {outcome:?}"
    );

    assert_eq!(
        wait_for_no_procs(&marker).await,
        0,
        "a cancelled install leaked the plugin's forked child"
    );
}

/// Swap `path`'s contents by rename, as editors that save atomically do, so a
/// running bash never reads a half-rewritten script.
fn save_script(path: &std::path::Path, body: &str) {
    let tmp = path.with_extension("swp");
    let _ = write_script(
        path.parent().unwrap(),
        tmp.file_name().unwrap().to_str().unwrap(),
        body,
    );
    std::fs::rename(&tmp, path).unwrap();
}

/// A polite plugin named `name` that appends a line to `starts` each time it
/// hands shake.
fn counting_plugin(name: &str, starts: &std::path::Path) -> String {
    format!(
        r#"#!/usr/bin/env bash
set -u
IFS= read -r _ || exit 1
printf '%s\n' '{{"jsonrpc":"2.0","id":"init","result":{{"name":"{name}","version":"0.1.0","subscribes":[],"provides":[],"capabilities":[]}}}}'
echo start >> {starts}
while IFS= read -r line; do
  case "$line" in
    *'"plugin.shutdown"'*) exit 0 ;;
  esac
done
"#,
        starts = starts.display()
    )
}

fn line_count(path: &std::path::Path) -> usize {
    std::fs::read_to_string(path)
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

/// Wait for a save to unregister every plugin, as a reload does first.
async fn wait_unlisted(mgr: &PluginManager) -> bool {
    shux_testkit::poll_until_async(BUDGET, || async { mgr.list().await.is_empty() }).await
}

/// Wait until every reload watcher `mgr` started has stopped. After that no
/// save can reload or reinstall anything, so an absence asserted next is
/// ordered after every reaction a save could have had.
async fn wait_watchers_stopped(mgr: &PluginManager, what: &str) {
    wait_until_async(what, || async {
        let (started, stopped) = mgr.watcher_counts();
        started == stopped
    })
    .await;
}

/// Wait until at least `n` reload watchers have stopped.
async fn wait_watchers_stopped_at_least(mgr: &PluginManager, n: u64, what: &str) {
    wait_until_async(what, || async { mgr.watcher_counts().1 >= n }).await;
}

/// A watcher re-armed after a failed reload must stop when the plugin it
/// brought back is killed: otherwise the next save installs it again.
#[tokio::test]
async fn a_plugin_killed_after_recovering_from_a_bad_save_stays_killed() {
    let tmp = tempfile::tempdir().unwrap();
    let starts = tmp.path().join("starts.log");
    let good = counting_plugin("phoenix", &starts);
    let broken = "#!/usr/bin/env bash\nexit 1\n";
    let script = write_script(tmp.path(), "phoenix.sh", &good);
    let start_count = || line_count(&starts);

    let mgr = PluginManager::with_state_root(EventBus::new(), tmp.path().join("state"));
    mgr.install(PluginSource::from_path(&script).with_watch(true))
        .await
        .expect("install");
    wait_until_async("the plugin to start", || async { start_count() == 1 }).await;

    // A bad save: the reload's install fails and the watcher re-arms. The
    // first watcher stops only after that reload has returned, re-armed.
    save_script(&script, broken);
    assert!(
        wait_unlisted(&mgr).await,
        "the bad save never unregistered the plugin"
    );
    wait_watchers_stopped_at_least(&mgr, 1, "the failed reload to re-arm its watcher").await;

    // The fix: the re-armed watcher installs it again.
    save_script(&script, &good);
    wait_until_async("the fixed save to bring the plugin back", || async {
        !mgr.list().await.is_empty() && start_count() == 2
    })
    .await;

    mgr.kill("phoenix").await.expect("kill");
    save_script(&script, &good);
    wait_watchers_stopped(
        &mgr,
        "every watcher to stop after the kill (a live one would reinstall on the save)",
    )
    .await;
    let listed = mgr.list().await;
    let restarted = start_count();
    mgr.shutdown().await;
    assert!(
        listed.is_empty() && restarted == 2,
        "a killed plugin came back on the next save: listed {:?}, {restarted} starts",
        listed.iter().map(|p| &p.name).collect::<Vec<_>>()
    );
}

/// `install` reports `watching` only once a save would be seen: a save
/// straight after it returns must reload the plugin.
#[tokio::test]
async fn a_save_right_after_a_watched_install_reloads_the_plugin() {
    let tmp = tempfile::tempdir().unwrap();
    let starts = tmp.path().join("starts.log");
    let body = counting_plugin("eager", &starts);
    let script = write_script(tmp.path(), "eager.sh", &body);
    let start_count = || line_count(&starts);

    let mgr = PluginManager::with_state_root(EventBus::new(), tmp.path().join("state"));
    let info = mgr
        .install(PluginSource::from_path(&script).with_watch(true))
        .await
        .expect("install");
    assert!(info.watching);
    save_script(&script, &body);
    let reloaded = shux_testkit::poll_until_async(BUDGET, || async { start_count() == 2 }).await;
    let _ = mgr.kill("eager").await;
    mgr.shutdown().await;
    assert!(reloaded, "a save made as `install` returned was never seen");
}

/// A plugin that exits on `plugin.shutdown` ends its grace at once: the
/// leader it leaves unreaped is not a process still shutting down. The grace
/// here would never expire on its own, so `shutdown` returns only if the exit
/// ended it. Linux only: elsewhere the grace cannot tell that zombie from a
/// live process.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn shutdown_returns_as_soon_as_a_polite_plugin_has_exited() {
    let tmp = tempfile::tempdir().unwrap();
    let script = write_script(tmp.path(), "noop.sh", NOOP_PLUGIN);
    let mgr = PluginManager::with_state_root(EventBus::new(), tmp.path().join("state"))
        .with_shutdown_grace(Duration::from_secs(3_600));
    mgr.install(PluginSource::from_path(&script))
        .await
        .expect("install");

    assert!(
        tokio::time::timeout(BUDGET, mgr.shutdown()).await.is_ok(),
        "shutdown of a plugin that exits on request sat out its whole grace"
    );
}

/// A plugin that stops reading its stdin fills the pipe and parks the I/O
/// task mid-write. `kill` and `shutdown` must still reach it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plugin_that_stopped_reading_is_still_killed_and_shut_down() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = unique_marker("deaf");
    // Reads one byte of the first event frame, says so, and stops reading.
    // That frame alone outsizes any pipe buffer, so from that byte on the I/O
    // task is parked mid-write for good.
    let body = format!(
        r#"#!/usr/bin/env bash
set -u
IFS= read -r _ || exit 1
printf '%s\n' '{{"jsonrpc":"2.0","id":"init","result":{{"name":"deaf","version":"0.1.0","subscribes":["session.created"],"provides":[],"capabilities":[]}}}}'
IFS= read -r -n 1 _
: > "$1"
exec -a {marker} sleep 300
"#
    );
    let script = write_script(tmp.path(), "deaf.sh", &body);
    let bus = EventBus::new();
    let mgr = PluginManager::with_state_root(bus.clone(), tmp.path().join("state"));

    for round in ["kill", "shutdown"] {
        let parked = tmp.path().join(format!("parked-{round}"));
        let mut source = PluginSource::from_path(&script);
        source.args = vec![parked.display().to_string()];
        mgr.install(source).await.expect("install");
        bus.publish(shux_core::event::EventData::SessionCreated {
            session_id: shux_core::model::SessionId::new(),
            name: "x".repeat(1 << 20),
        });
        wait_until_async("the plugin's I/O task to park mid-frame", || async {
            parked.exists()
        })
        .await;
        if round == "kill" {
            mgr.kill("deaf").await.expect("kill");
        } else {
            let done = tokio::time::timeout(BUDGET, mgr.shutdown()).await;
            assert!(
                done.is_ok(),
                "shutdown never returned for a plugin that stopped reading"
            );
        }
        assert_eq!(
            wait_for_no_procs(&marker).await,
            0,
            "a plugin that stopped reading survived `{round}`"
        );
    }
}

/// `kill` during a reload's handshake wins over the reload, whether that
/// handshake then fails or succeeds: the next save does not bring it back.
#[tokio::test]
async fn a_kill_during_a_reload_handshake_is_not_undone_by_the_next_save() {
    for handshake in ["fails", "succeeds"] {
        let tmp = tempfile::tempdir().unwrap();
        let starts = tmp.path().join("starts.log");
        let ready = tmp.path().join("ready");
        let held = gate(tmp.path(), "gate");
        let good = counting_plugin("lazarus", &starts);
        // Mid-handshake until the test has killed it.
        let wait_at_gate = format!(
            ": > {}\nIFS= read -r _ < {}\n",
            ready.display(),
            held.display()
        );
        let slow = if handshake == "fails" {
            format!("#!/usr/bin/env bash\nIFS= read -r _\n{wait_at_gate}exit 1\n")
        } else {
            good.replacen("|| exit 1\n", &format!("|| exit 1\n{wait_at_gate}"), 1)
        };
        let script = write_script(tmp.path(), "lazarus.sh", &good);
        let mgr = PluginManager::with_state_root(EventBus::new(), tmp.path().join("state"));
        mgr.install(PluginSource::from_path(&script).with_watch(true))
            .await
            .expect("install");

        save_script(&script, &slow);
        assert!(wait_unlisted(&mgr).await, "the save never started a reload");
        wait_for_path(&ready).await;
        mgr.kill("lazarus")
            .await
            .expect("a plugin mid-reload is found");
        open_gate_async(&held).await;
        wait_watchers_stopped(
            &mgr,
            "every watcher to stop after the kill (a live one would bring the plugin back)",
        )
        .await;
        let listed = mgr.list().await;
        mgr.shutdown().await;
        assert!(
            listed.is_empty(),
            "a plugin killed mid-reload came back when its handshake {handshake}"
        );
    }
}

/// `shutdown` also waits out plugins `kill` stopped just before it: their
/// grace may still be running when the owner drops the runtime.
#[test]
fn shutdown_waits_for_a_plugin_killed_just_before_it() {
    let tmp = tempfile::tempdir().unwrap();
    let child = unique_marker("stubborn-child");
    let body = format!(
        r#"#!/usr/bin/env bash
trap '' TERM HUP
IFS= read -r _ || exit 1
printf '%s\n' '{{"jsonrpc":"2.0","id":"init","result":{{"name":"stubborn","version":"0.1.0","subscribes":[],"provides":[],"capabilities":[]}}}}'
( trap '' TERM HUP; exec -a {child} sleep 45 ) &
while IFS= read -r line; do :; done
sleep 45
"#
    );
    let script = write_script(tmp.path(), "stubborn.sh", &body);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mgr = PluginManager::with_state_root(EventBus::new(), tmp.path().join("state"));
        mgr.install(PluginSource::from_path(&script))
            .await
            .expect("install");
        assert_eq!(
            wait_for_procs(&child, 1).await,
            1,
            "the plugin never started its child"
        );
        mgr.kill("stubborn").await.expect("kill");
        mgr.shutdown().await;
    });
    drop(rt);
    // The child outlives the budget unless shutdown's SIGKILL reached it.
    shux_testkit::poll_until(BUDGET, || count_procs_containing(&child) == 0);
    assert_eq!(
        count_procs_containing(&child),
        0,
        "a killed plugin's child outlived shutdown and the runtime"
    );
}

/// `kill` while a re-armed watcher's install is mid-handshake wins: the
/// plugin does not register when that handshake completes.
#[tokio::test]
async fn a_kill_during_a_rearmed_watchers_install_sticks() {
    let tmp = tempfile::tempdir().unwrap();
    let readies = tmp.path().join("readies.log");
    let held = gate(tmp.path(), "gate");
    // Every start waits at the gate mid-handshake.
    let good = format!(
        r#"#!/usr/bin/env bash
IFS= read -r _ || exit 1
echo ready >> {readies}
IFS= read -r _ < {held}
printf '%s\n' '{{"jsonrpc":"2.0","id":"init","result":{{"name":"revenant","version":"0.1.0","subscribes":[],"provides":[],"capabilities":[]}}}}'
while IFS= read -r line; do
  case "$line" in
    *'"plugin.shutdown"'*) exit 0 ;;
  esac
done
"#,
        readies = readies.display(),
        held = held.display()
    );
    let script = write_script(tmp.path(), "revenant.sh", &good);
    let mgr = PluginManager::with_state_root(EventBus::new(), tmp.path().join("state"));
    let install = tokio::spawn({
        let mgr = mgr.clone();
        let source = PluginSource::from_path(&script).with_watch(true);
        async move { mgr.install(source).await }
    });
    open_gate_async(&held).await;
    install.await.unwrap().expect("install");

    save_script(&script, "#!/usr/bin/env bash\nexit 1\n");
    assert!(
        wait_unlisted(&mgr).await,
        "the bad save never unregistered the plugin"
    );
    wait_watchers_stopped_at_least(&mgr, 1, "the failed reload to re-arm its watcher").await;

    save_script(&script, &good);
    wait_until_async("the re-armed watcher to start the plugin", || async {
        line_count(&readies) == 2
    })
    .await;
    let killed = mgr.kill("revenant").await;
    open_gate_async(&held).await;
    wait_watchers_stopped(
        &mgr,
        "every watcher to stop after the kill (a live one would bring the plugin back)",
    )
    .await;
    let listed = mgr.list().await;
    mgr.shutdown().await;
    assert!(
        listed.is_empty(),
        "kill returned {killed:?}, yet the plugin registered afterwards"
    );
}

/// A kill that lands mid-frame must not glue `plugin.shutdown` onto the
/// half-written frame: a line-at-a-time reader still sees it as its own line.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_mid_frame_still_delivers_shutdown_on_its_own_line() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("stdin.log");
    let parked = tmp.path().join("parked");
    let held = gate(tmp.path(), "gate");
    // Reads one byte of a frame that outsizes any pipe buffer, so the I/O
    // task is parked mid-write, and drains stdin only once the test has
    // killed it. `<&0`: a background job's stdin is otherwise /dev/null.
    // Exiting once the shutdown line is in ends the grace.
    let body = format!(
        r#"#!/usr/bin/env bash
IFS= read -r _ || exit 1
printf '%s\n' '{{"jsonrpc":"2.0","id":"init","result":{{"name":"torn","version":"0.1.0","subscribes":["session.created"],"provides":[],"capabilities":[]}}}}'
IFS= read -r -n 1 _
: > {parked}
IFS= read -r _ < {held}
cat <&0 > {log} &
until grep -q '"method":"plugin.shutdown"' {log}; do sleep 0.05; done
kill $!
"#,
        parked = parked.display(),
        held = held.display(),
        log = log.display()
    );
    let script = write_script(tmp.path(), "torn.sh", &body);
    let bus = EventBus::new();
    let mgr = PluginManager::with_state_root(bus.clone(), tmp.path().join("state"))
        .with_shutdown_grace(Duration::from_secs(3_600));
    mgr.install(PluginSource::from_path(&script))
        .await
        .expect("install");
    bus.publish(shux_core::event::EventData::SessionCreated {
        session_id: shux_core::model::SessionId::new(),
        name: "x".repeat(1 << 20),
    });
    wait_for_path(&parked).await;
    // The kill is signalled before the plugin reads another byte, and the
    // frame write polls the kill first, so the frame is cut, not finished.
    mgr.kill("torn").await.expect("kill");
    open_gate_async(&held).await;
    assert!(
        tokio::time::timeout(BUDGET, mgr.shutdown()).await.is_ok(),
        "the plugin never exited after reading plugin.shutdown"
    );
    let got = std::fs::read_to_string(&log).unwrap_or_default();
    let shutdown = r#"{"jsonrpc":"2.0","method":"plugin.shutdown""#;
    assert!(
        got.contains("plugin.shutdown"),
        "plugin.shutdown never arrived"
    );
    assert!(
        got.lines().any(|l| l.starts_with(shutdown)),
        "plugin.shutdown arrived glued to a torn frame"
    );
}

/// `plugin.shutdown` taken off the queue just as the kill signal lands is
/// still written: a polite plugin always gets the chance to exit itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_kill_delivers_plugin_shutdown() {
    let tmp = tempfile::tempdir().unwrap();
    let seen = tmp.path().join("seen.log");
    let body = format!(
        r#"#!/usr/bin/env bash
IFS= read -r _ || exit 1
printf '%s\n' '{{"jsonrpc":"2.0","id":"init","result":{{"name":"heed","version":"0.1.0","subscribes":[],"provides":[],"capabilities":[]}}}}'
while IFS= read -r line; do
  case "$line" in *'"plugin.shutdown"'*) echo seen >> {seen}; exit 0 ;; esac
done
"#,
        seen = seen.display()
    );
    let script = write_script(tmp.path(), "heed.sh", &body);
    let mgr = PluginManager::with_state_root(EventBus::new(), tmp.path().join("state"));
    let rounds = 200;
    for _ in 0..rounds {
        mgr.install(PluginSource::from_path(&script))
            .await
            .expect("install");
        mgr.kill("heed").await.expect("kill");
    }
    mgr.shutdown().await;
    assert_eq!(
        line_count(&seen),
        rounds,
        "some kills never delivered plugin.shutdown"
    );
}
