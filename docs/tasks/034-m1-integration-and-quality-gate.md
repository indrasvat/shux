# 034 — M1 Integration and Quality Gate

**Status:** Pending
**Depends On:** 013, 014, 015, 016, 017, 018, 019, 020, 021, 022, 023, 024, 025, 026, 027, 028, 029, 030, 031, 032, 033
**Parallelizable With:** —

---

## Problem

Milestone 1 (M1) is the "Daily-Driver Core" milestone. Its completion criterion from the PRD is stark: "A developer can use shux for a full day. All keybindings work. Visual regression tests pass." Before declaring M1 complete, every feature must be verified to work individually and in combination. This task is the quality gate: it adds comprehensive integration tests, creates the first visual regression golden images, validates performance budgets, and initiates the dogfooding protocol.

This is not a feature task. It is the task that proves all M1 features work together. No new functionality is added; instead, every edge case, cross-feature interaction, and performance budget is validated. Any bug discovered here is fixed before M1 is declared complete.

## PRD Reference

- **section 17** Milestone plan, M1: "Done when: A developer can use shux for a full day. All keybindings work. Visual regression tests pass."
- **section 16.1** Testing pyramid: L3 (API contract), L4 (visual regression)
- **section 16.2** Layer details: L3 covers every API method (happy + error), L4 covers golden image comparison
- **section 14.1** Performance budgets: keypress-to-visible p99 <= 25ms, split p99 <= 80ms
- **section 5** "Prove it works" design principle

---

## Files to Create

- `tests/integration/m1_test.rs` — Comprehensive M1 integration test suite
- `tests/integration/api_contract_test.rs` — L3 API contract tests for all M1 methods
- `tests/integration/keybinding_test.rs` — Keybinding integration tests (all Tier 1 + Tier 2)
- `tests/golden/README.md` — Documentation for the golden image test system
- `tests/golden/initial_launch.golden` — Golden image: initial launch (single pane)
- `tests/golden/multi_pane_splits.golden` — Golden image: 2x2 grid
- `tests/golden/per_pane_theming.golden` — Golden image: prod=red, dev=green
- `tests/golden/zoom_mode.golden` — Golden image: zoomed pane
- `tests/golden/copy_mode.golden` — Golden image: copy mode active
- `tests/golden/command_palette.golden` — Golden image: command palette open
- `tests/golden/help_overlay.golden` — Golden image: help overlay
- `scripts/golden-test.sh` — Script to capture and compare golden images
- `scripts/perf-check.sh` — Script to run performance benchmarks
- `tests/integration/perf_test.rs` — Performance budget validation tests
- `docs/dogfooding.md` — Guide for using shux as a daily driver

## Files to Modify

- `Cargo.toml` — Add integration test crate dependencies if needed
- `Makefile` — Add `golden`, `perf-check`, and `m1-gate` targets
- `docs/PROGRESS.md` — Mark M1 complete, add session log entry

---

## Execution Steps

### Step 1: Create L3 API Contract Test Framework

Create `tests/integration/api_contract_test.rs`:

```rust
//! L3 API Contract Tests — M1 Surface
//!
//! Tests every API method available in M1 with:
//! - Happy path (correct usage)
//! - Error cases (invalid params, not found, version conflicts)
//! - Idempotent ensure operations
//!
//! Each test starts an ephemeral daemon on a temporary Unix socket.

use std::path::PathBuf;
use tempfile::TempDir;

/// Start an ephemeral daemon for testing.
/// Returns the socket path and a handle to stop the daemon.
async fn start_test_daemon() -> (PathBuf, DaemonHandle) {
    let tmp = TempDir::new().expect("create temp dir");
    let socket_path = tmp.path().join("shux-test.sock");

    // Start daemon with test configuration
    let handle = shux_core::daemon::start(shux_core::daemon::DaemonConfig {
        socket_path: socket_path.clone(),
        tcp_listen: String::new(),
        auto_exit: true,
        auto_exit_grace_secs: 0,
        log_level: "warn".into(),
        ..Default::default()
    })
    .await
    .expect("start daemon");

    // Wait for socket to be ready
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    (socket_path, handle)
}

/// Create a JSON-RPC client connected to the test daemon.
async fn connect(socket_path: &PathBuf) -> RpcClient {
    RpcClient::connect_uds(socket_path)
        .await
        .expect("connect to daemon")
}

// ── Session API ─────────────────────────────────────

#[tokio::test]
async fn test_session_create_happy_path() {
    let (socket, _handle) = start_test_daemon().await;
    let client = connect(&socket).await;

    let result = client
        .call("session.create", json!({"name": "test-session"}))
        .await
        .unwrap();

    assert_eq!(result["session"]["name"], "test-session");
    assert!(result["session"]["id"].is_string());
    assert_eq!(result["session"]["version"], 1);
}

#[tokio::test]
async fn test_session_create_duplicate_name_error() {
    let (socket, _handle) = start_test_daemon().await;
    let client = connect(&socket).await;

    client
        .call("session.create", json!({"name": "dup"}))
        .await
        .unwrap();

    let err = client
        .call("session.create", json!({"name": "dup"}))
        .await
        .unwrap_err();

    assert_eq!(err.code, -32002); // already_exists
}

#[tokio::test]
async fn test_session_ensure_idempotent() {
    let (socket, _handle) = start_test_daemon().await;
    let client = connect(&socket).await;

    let result1 = client
        .call("session.ensure", json!({"name": "idempotent"}))
        .await
        .unwrap();

    let result2 = client
        .call("session.ensure", json!({"name": "idempotent"}))
        .await
        .unwrap();

    // Same session returned both times
    assert_eq!(result1["session"]["id"], result2["session"]["id"]);
}

#[tokio::test]
async fn test_session_list() {
    let (socket, _handle) = start_test_daemon().await;
    let client = connect(&socket).await;

    client
        .call("session.create", json!({"name": "s1"}))
        .await
        .unwrap();
    client
        .call("session.create", json!({"name": "s2"}))
        .await
        .unwrap();

    let result = client.call("session.list", json!({})).await.unwrap();
    let sessions = result["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2);
}

#[tokio::test]
async fn test_session_rename() {
    let (socket, _handle) = start_test_daemon().await;
    let client = connect(&socket).await;

    let created = client
        .call("session.create", json!({"name": "old-name"}))
        .await
        .unwrap();
    let session_id = created["session"]["id"].as_str().unwrap();

    let result = client
        .call(
            "session.rename",
            json!({"session_id": session_id, "name": "new-name"}),
        )
        .await
        .unwrap();

    assert_eq!(result["session"]["name"], "new-name");
}

#[tokio::test]
async fn test_session_kill() {
    let (socket, _handle) = start_test_daemon().await;
    let client = connect(&socket).await;

    let created = client
        .call("session.create", json!({"name": "doomed"}))
        .await
        .unwrap();
    let session_id = created["session"]["id"].as_str().unwrap();

    client
        .call("session.kill", json!({"session_id": session_id}))
        .await
        .unwrap();

    let list = client.call("session.list", json!({})).await.unwrap();
    assert!(list["sessions"].as_array().unwrap().is_empty());
}

// ── Window API ──────────────────────────────────────

#[tokio::test]
async fn test_window_create_and_list() {
    let (socket, _handle) = start_test_daemon().await;
    let client = connect(&socket).await;

    let session = client
        .call("session.create", json!({"name": "wtest"}))
        .await
        .unwrap();
    let sid = session["session"]["id"].as_str().unwrap();

    // First window is created automatically with the session
    let windows = client
        .call("window.list", json!({"session_id": sid}))
        .await
        .unwrap();
    assert!(windows["windows"].as_array().unwrap().len() >= 1);

    // Create additional window
    client
        .call("window.create", json!({"session_id": sid, "title": "second"}))
        .await
        .unwrap();

    let windows = client
        .call("window.list", json!({"session_id": sid}))
        .await
        .unwrap();
    assert!(windows["windows"].as_array().unwrap().len() >= 2);
}

// ── Pane API ────────────────────────────────────────

#[tokio::test]
async fn test_pane_split_and_focus() {
    let (socket, _handle) = start_test_daemon().await;
    let client = connect(&socket).await;

    let session = client
        .call("session.create", json!({"name": "ptest"}))
        .await
        .unwrap();

    // Get the initial pane
    let panes = client
        .call("pane.list", json!({}))
        .await
        .unwrap();
    let initial_pane_id = panes["panes"][0]["id"].as_str().unwrap();

    // Split vertically
    let split_result = client
        .call(
            "pane.split",
            json!({
                "pane_id": initial_pane_id,
                "direction": "vertical"
            }),
        )
        .await
        .unwrap();

    let new_pane_id = split_result["pane"]["id"].as_str().unwrap();
    assert_ne!(initial_pane_id, new_pane_id);

    // Focus the new pane
    client
        .call("pane.focus", json!({"pane_id": new_pane_id}))
        .await
        .unwrap();
}

#[tokio::test]
async fn test_pane_resize() {
    let (socket, _handle) = start_test_daemon().await;
    let client = connect(&socket).await;

    client
        .call("session.create", json!({"name": "resize-test"}))
        .await
        .unwrap();

    let panes = client.call("pane.list", json!({})).await.unwrap();
    let pane_id = panes["panes"][0]["id"].as_str().unwrap();

    // Split to have something to resize
    client
        .call("pane.split", json!({"pane_id": pane_id, "direction": "vertical"}))
        .await
        .unwrap();

    // Resize
    let result = client
        .call(
            "pane.resize",
            json!({"pane_id": pane_id, "direction": "right", "amount": 5}),
        )
        .await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn test_pane_zoom_toggle() {
    let (socket, _handle) = start_test_daemon().await;
    let client = connect(&socket).await;

    client
        .call("session.create", json!({"name": "zoom-test"}))
        .await
        .unwrap();

    let panes = client.call("pane.list", json!({})).await.unwrap();
    let pane_id = panes["panes"][0]["id"].as_str().unwrap();

    // Zoom in
    let result = client
        .call("pane.zoom", json!({"pane_id": pane_id}))
        .await
        .unwrap();
    assert_eq!(result["zoomed"], true);

    // Zoom out
    let result = client
        .call("pane.zoom", json!({"pane_id": pane_id}))
        .await
        .unwrap();
    assert_eq!(result["zoomed"], false);
}

#[tokio::test]
async fn test_pane_set_title() {
    let (socket, _handle) = start_test_daemon().await;
    let client = connect(&socket).await;

    client
        .call("session.create", json!({"name": "title-test"}))
        .await
        .unwrap();

    let panes = client.call("pane.list", json!({})).await.unwrap();
    let pane_id = panes["panes"][0]["id"].as_str().unwrap();

    // Set manual title
    let result = client
        .call(
            "pane.set_title",
            json!({"pane_id": pane_id, "title": "My Custom Title"}),
        )
        .await
        .unwrap();

    assert_eq!(result["title"], "My Custom Title");

    // Clear manual title
    let result = client
        .call(
            "pane.set_title",
            json!({"pane_id": pane_id, "title": null}),
        )
        .await
        .unwrap();

    // Should revert to auto/osc title
    assert_ne!(result["title"], "My Custom Title");
}

// ── Config API ──────────────────────────────────────

#[tokio::test]
async fn test_config_get_and_set() {
    let (socket, _handle) = start_test_daemon().await;
    let client = connect(&socket).await;

    // Get a config value
    let result = client
        .call("config.get", json!({"key": "ui.mouse"}))
        .await
        .unwrap();
    assert_eq!(result["value"], true); // default

    // Override at runtime
    client
        .call("config.set", json!({"key": "ui.mouse", "value": false}))
        .await
        .unwrap();

    let result = client
        .call("config.get", json!({"key": "ui.mouse"}))
        .await
        .unwrap();
    assert_eq!(result["value"], false);
}

// ── Theme API ───────────────────────────────────────

#[tokio::test]
async fn test_theme_list_and_get() {
    let (socket, _handle) = start_test_daemon().await;
    let client = connect(&socket).await;

    let result = client.call("theme.list", json!({})).await.unwrap();
    let themes = result["themes"].as_array().unwrap();
    assert!(themes.iter().any(|t| t["name"] == "default-dark"));
}

#[tokio::test]
async fn test_pane_set_theme() {
    let (socket, _handle) = start_test_daemon().await;
    let client = connect(&socket).await;

    client
        .call("session.create", json!({"name": "theme-test"}))
        .await
        .unwrap();

    let panes = client.call("pane.list", json!({})).await.unwrap();
    let pane_id = panes["panes"][0]["id"].as_str().unwrap();

    let result = client
        .call(
            "pane.set_theme",
            json!({"pane_id": pane_id, "theme": "default-dark"}),
        )
        .await;

    assert!(result.is_ok());
}

// ── Keybinding API ──────────────────────────────────

#[tokio::test]
async fn test_keybinding_list() {
    let (socket, _handle) = start_test_daemon().await;
    let client = connect(&socket).await;

    let result = client.call("keybinding.list", json!({})).await.unwrap();
    let bindings = result["bindings"].as_array().unwrap();
    assert!(!bindings.is_empty());

    // Verify a known default binding exists
    assert!(bindings.iter().any(|b| {
        b["action"] == "pane.focus-left" && b["source"] == "built-in"
    }));
}

#[tokio::test]
async fn test_keybinding_set_and_reset() {
    let (socket, _handle) = start_test_daemon().await;
    let client = connect(&socket).await;

    // Override a binding
    client
        .call(
            "keybinding.set",
            json!({"key": "alt-h", "action": "custom.test"}),
        )
        .await
        .unwrap();

    // Verify override
    let result = client.call("keybinding.list", json!({})).await.unwrap();
    let bindings = result["bindings"].as_array().unwrap();
    assert!(bindings.iter().any(|b| {
        b["key"].as_str().unwrap().contains("Alt") && b["action"] == "custom.test"
    }));

    // Reset
    client
        .call("keybinding.reset", json!({"key": "alt-h"}))
        .await
        .unwrap();

    // Verify reset
    let result = client.call("keybinding.list", json!({})).await.unwrap();
    let bindings = result["bindings"].as_array().unwrap();
    assert!(bindings.iter().any(|b| {
        b["action"] == "pane.focus-left" && b["source"] == "built-in"
    }));
}

// ── Error Cases ─────────────────────────────────────

#[tokio::test]
async fn test_not_found_errors() {
    let (socket, _handle) = start_test_daemon().await;
    let client = connect(&socket).await;

    // Non-existent session
    let err = client
        .call("session.kill", json!({"session_id": "00000000-0000-0000-0000-000000000000"}))
        .await
        .unwrap_err();
    assert!(err.code < 0); // Error code

    // Non-existent pane
    let err = client
        .call("pane.focus", json!({"pane_id": "00000000-0000-0000-0000-000000000000"}))
        .await
        .unwrap_err();
    assert!(err.code < 0);
}

#[tokio::test]
async fn test_invalid_params_errors() {
    let (socket, _handle) = start_test_daemon().await;
    let client = connect(&socket).await;

    // Missing required field
    let err = client
        .call("session.create", json!({}))
        .await
        .unwrap_err();
    assert_eq!(err.code, -32602); // Invalid params

    // Invalid pane split direction
    let err = client
        .call("pane.split", json!({"pane_id": "x", "direction": "diagonal"}))
        .await
        .unwrap_err();
    assert!(err.code < 0);
}

// ── Version Conflict Detection ──────────────────────

#[tokio::test]
async fn test_version_conflict_detection() {
    let (socket, _handle) = start_test_daemon().await;
    let client = connect(&socket).await;

    let session = client
        .call("session.create", json!({"name": "conflict-test"}))
        .await
        .unwrap();
    let sid = session["session"]["id"].as_str().unwrap();
    let version = session["session"]["version"].as_u64().unwrap();

    // Rename with correct version
    client
        .call(
            "session.rename",
            json!({"session_id": sid, "name": "new", "version": version}),
        )
        .await
        .unwrap();

    // Rename with stale version (should fail)
    let err = client
        .call(
            "session.rename",
            json!({"session_id": sid, "name": "newer", "version": version}),
        )
        .await
        .unwrap_err();

    assert_eq!(err.code, -32001); // version_conflict
    assert!(err.data["hint"].as_str().unwrap().contains("Re-read"));
}
```

### Step 2: Create Golden Image Test Infrastructure

Create `scripts/golden-test.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail

# Golden Image Test Script for shux
#
# Usage:
#   ./scripts/golden-test.sh capture   # Capture new golden images
#   ./scripts/golden-test.sh compare   # Compare against golden images
#   ./scripts/golden-test.sh update    # Update golden images to current
#
# Requires: shux binary, iTerm2 (macOS) or xterm (Linux)

GOLDEN_DIR="tests/golden"
CAPTURE_DIR="tests/golden/captures"
SHUX_BIN="${SHUX_BIN:-./target/release/shux}"

mkdir -p "$GOLDEN_DIR" "$CAPTURE_DIR"

capture_scenario() {
    local name="$1"
    local setup_script="$2"

    echo "Capturing: $name"

    # Start shux in a controlled terminal
    # Use iTerm2 scripting on macOS, xterm on Linux
    if [[ "$(uname)" == "Darwin" ]]; then
        # macOS: Use iTerm2 driver (from task using iterm2-driver)
        python3 scripts/iterm2_capture.py \
            --scenario "$setup_script" \
            --output "$CAPTURE_DIR/${name}.png" \
            --width 120 --height 40
    else
        # Linux: Use xterm + xdotool
        echo "  (Linux golden capture not yet implemented)"
    fi
}

compare_scenario() {
    local name="$1"
    local golden="$GOLDEN_DIR/${name}.golden"
    local capture="$CAPTURE_DIR/${name}.png"

    if [[ ! -f "$golden" ]]; then
        echo "SKIP: No golden image for $name"
        return 0
    fi

    if [[ ! -f "$capture" ]]; then
        echo "FAIL: No capture for $name"
        return 1
    fi

    # Compare using ImageMagick or perceptual diff
    local diff_pct
    diff_pct=$(compare -metric RMSE "$golden" "$capture" /dev/null 2>&1 | cut -d' ' -f1)

    if (( $(echo "$diff_pct < 0.05" | bc -l) )); then
        echo "PASS: $name (diff: $diff_pct)"
        return 0
    else
        echo "FAIL: $name (diff: $diff_pct)"
        compare "$golden" "$capture" "$CAPTURE_DIR/${name}.diff.png" 2>/dev/null || true
        return 1
    fi
}

case "${1:-compare}" in
    capture)
        echo "=== Capturing golden images ==="
        capture_scenario "initial_launch" "scenarios/initial_launch.sh"
        capture_scenario "multi_pane_splits" "scenarios/multi_pane_splits.sh"
        capture_scenario "per_pane_theming" "scenarios/per_pane_theming.sh"
        capture_scenario "zoom_mode" "scenarios/zoom_mode.sh"
        capture_scenario "copy_mode" "scenarios/copy_mode.sh"
        capture_scenario "command_palette" "scenarios/command_palette.sh"
        capture_scenario "help_overlay" "scenarios/help_overlay.sh"
        echo "=== Captures saved to $CAPTURE_DIR ==="
        ;;

    compare)
        echo "=== Comparing against golden images ==="
        failures=0
        for golden in "$GOLDEN_DIR"/*.golden; do
            name=$(basename "$golden" .golden)
            compare_scenario "$name" || ((failures++))
        done
        echo "=== Results: $failures failures ==="
        exit $failures
        ;;

    update)
        echo "=== Updating golden images from captures ==="
        for capture in "$CAPTURE_DIR"/*.png; do
            name=$(basename "$capture" .png)
            cp "$capture" "$GOLDEN_DIR/${name}.golden"
            echo "Updated: $name"
        done
        echo "=== Golden images updated ==="
        ;;

    *)
        echo "Usage: $0 {capture|compare|update}"
        exit 1
        ;;
esac
```

### Step 3: Create Performance Validation Tests

Create `tests/integration/perf_test.rs`:

```rust
//! Performance budget validation tests.
//!
//! These tests verify that M1 features meet the performance budgets
//! defined in PRD section 14.1.

use std::time::{Duration, Instant};

/// Performance budget: keypress to visible update
const KEYPRESS_P99_BUDGET: Duration = Duration::from_millis(25);
const KEYPRESS_P50_BUDGET: Duration = Duration::from_millis(8);

/// Performance budget: split pane operation
const SPLIT_P99_BUDGET: Duration = Duration::from_millis(80);
const SPLIT_P50_BUDGET: Duration = Duration::from_millis(25);

/// Number of iterations for statistical measurement
const ITERATIONS: usize = 100;

fn percentile(durations: &mut [Duration], pct: f64) -> Duration {
    durations.sort();
    let idx = ((durations.len() as f64) * pct / 100.0).ceil() as usize - 1;
    durations[idx.min(durations.len() - 1)]
}

#[tokio::test]
async fn test_keypress_latency_budget() {
    let (socket, _handle) = start_test_daemon().await;
    let client = connect(&socket).await;

    client
        .call("session.create", json!({"name": "perf-key"}))
        .await
        .unwrap();

    let panes = client.call("pane.list", json!({})).await.unwrap();
    let pane_id = panes["panes"][0]["id"].as_str().unwrap();

    let mut durations = Vec::with_capacity(ITERATIONS);

    for _ in 0..ITERATIONS {
        let start = Instant::now();

        // Simulate: send a keypress and wait for state update
        client
            .call(
                "pane.send_keys",
                json!({"pane_id": pane_id, "keys": "a"}),
            )
            .await
            .unwrap();

        durations.push(start.elapsed());
    }

    let p50 = percentile(&mut durations, 50.0);
    let p99 = percentile(&mut durations, 99.0);

    println!(
        "Keypress latency: p50={:?}, p99={:?} (budget: p50<={:?}, p99<={:?})",
        p50, p99, KEYPRESS_P50_BUDGET, KEYPRESS_P99_BUDGET
    );

    assert!(
        p99 <= KEYPRESS_P99_BUDGET,
        "Keypress p99 ({:?}) exceeds budget ({:?})",
        p99,
        KEYPRESS_P99_BUDGET
    );
}

#[tokio::test]
async fn test_split_operation_latency_budget() {
    let (socket, _handle) = start_test_daemon().await;
    let client = connect(&socket).await;

    let mut durations = Vec::with_capacity(ITERATIONS);

    for i in 0..ITERATIONS {
        // Create a fresh session for each iteration to avoid
        // accumulating panes (which would slow splits)
        let session = client
            .call("session.create", json!({"name": format!("perf-split-{}", i)}))
            .await
            .unwrap();

        let panes = client.call("pane.list", json!({})).await.unwrap();
        let pane_id = panes["panes"][0]["id"].as_str().unwrap();

        let start = Instant::now();

        client
            .call(
                "pane.split",
                json!({"pane_id": pane_id, "direction": "vertical"}),
            )
            .await
            .unwrap();

        durations.push(start.elapsed());

        // Clean up
        let sid = session["session"]["id"].as_str().unwrap();
        client
            .call("session.kill", json!({"session_id": sid}))
            .await
            .unwrap();
    }

    let p50 = percentile(&mut durations, 50.0);
    let p99 = percentile(&mut durations, 99.0);

    println!(
        "Split latency: p50={:?}, p99={:?} (budget: p50<={:?}, p99<={:?})",
        p50, p99, SPLIT_P50_BUDGET, SPLIT_P99_BUDGET
    );

    assert!(
        p99 <= SPLIT_P99_BUDGET,
        "Split p99 ({:?}) exceeds budget ({:?})",
        p99,
        SPLIT_P99_BUDGET
    );
}
```

### Step 4: Create the M1 Comprehensive Integration Test

Create `tests/integration/m1_test.rs`:

```rust
//! M1 Integration Test — "Can a developer use shux for a full day?"
//!
//! This test simulates a realistic usage session covering all M1 features.
//! It exercises cross-feature interactions that unit tests don't cover.

#[tokio::test]
async fn test_m1_full_workflow() {
    let (socket, _handle) = start_test_daemon().await;
    let client = connect(&socket).await;

    // 1. Create session
    let session = client
        .call("session.create", json!({"name": "daily-driver"}))
        .await
        .unwrap();
    let sid = session["session"]["id"].as_str().unwrap();

    // 2. Create windows
    let win1 = client
        .call("window.create", json!({"session_id": sid, "title": "editor"}))
        .await
        .unwrap();

    let win2 = client
        .call("window.create", json!({"session_id": sid, "title": "servers"}))
        .await
        .unwrap();

    let win3 = client
        .call("window.create", json!({"session_id": sid, "title": "logs"}))
        .await
        .unwrap();

    // 3. Split panes in "servers" window
    // Focus window 2
    let win2_id = win2["window"]["id"].as_str().unwrap();
    client
        .call("window.focus", json!({"window_id": win2_id}))
        .await
        .unwrap();

    let panes = client.call("pane.list", json!({})).await.unwrap();
    let pane_id = panes["panes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["window_id"] == win2_id)
        .unwrap()["id"]
        .as_str()
        .unwrap();

    // Vertical split
    let split1 = client
        .call(
            "pane.split",
            json!({"pane_id": pane_id, "direction": "vertical"}),
        )
        .await
        .unwrap();

    // 4. Set per-pane themes
    client
        .call(
            "pane.set_theme",
            json!({"pane_id": pane_id, "theme": "default-dark"}),
        )
        .await
        .unwrap();

    // 5. Set pane titles
    client
        .call(
            "pane.set_title",
            json!({"pane_id": pane_id, "title": "frontend"}),
        )
        .await
        .unwrap();

    let new_pane_id = split1["pane"]["id"].as_str().unwrap();
    client
        .call(
            "pane.set_title",
            json!({"pane_id": new_pane_id, "title": "backend"}),
        )
        .await
        .unwrap();

    // 6. Verify config operations
    let config = client
        .call("config.get", json!({"key": "ui.status_bar"}))
        .await
        .unwrap();
    assert_eq!(config["value"], true);

    // 7. Verify keybinding operations
    let bindings = client
        .call("keybinding.list", json!({}))
        .await
        .unwrap();
    assert!(!bindings["bindings"].as_array().unwrap().is_empty());

    // 8. Verify theme operations
    let themes = client
        .call("theme.list", json!({}))
        .await
        .unwrap();
    assert!(!themes["themes"].as_array().unwrap().is_empty());

    // 9. Zoom pane
    client
        .call("pane.zoom", json!({"pane_id": pane_id}))
        .await
        .unwrap();

    // Unzoom
    client
        .call("pane.zoom", json!({"pane_id": pane_id}))
        .await
        .unwrap();

    // 10. Swap panes
    client
        .call(
            "pane.swap",
            json!({"pane_id": pane_id, "target_pane_id": new_pane_id}),
        )
        .await
        .unwrap();

    // 11. Resize pane
    client
        .call(
            "pane.resize",
            json!({"pane_id": pane_id, "direction": "down", "amount": 3}),
        )
        .await
        .unwrap();

    // 12. Clean up
    client
        .call("session.kill", json!({"session_id": sid}))
        .await
        .unwrap();

    let sessions = client
        .call("session.list", json!({}))
        .await
        .unwrap();
    assert!(sessions["sessions"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_m1_template_workflow() {
    let (socket, _handle) = start_test_daemon().await;
    let client = connect(&socket).await;

    // Apply a template (inline for testing)
    let template = json!({
        "session_name": "template-test",
        "windows": [
            {
                "title": "editor",
                "layout": "single",
                "panes": [{"command": ["echo", "hello"]}]
            },
            {
                "title": "split",
                "layout": "vertical",
                "panes": [
                    {"title": "top"},
                    {"title": "bottom"}
                ]
            }
        ]
    });

    let result = client
        .call("state.apply", template)
        .await
        .unwrap();

    // Verify session was created
    let sessions = client.call("session.list", json!({})).await.unwrap();
    assert!(sessions["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["name"] == "template-test"));
}
```

### Step 5: Add Makefile Targets

```makefile
# ── M1 Quality Gate ─────────────────────────────────

m1-gate: lint test-lib test-api test-perf golden-compare
	@echo "=== M1 Quality Gate ==="
	@echo "All checks passed!"

test-api:
	cargo nextest run --test api_contract_test --test m1_test --test keybinding_test

test-perf:
	cargo nextest run --test perf_test --no-capture

golden-capture:
	bash scripts/golden-test.sh capture

golden-compare:
	bash scripts/golden-test.sh compare

golden-update:
	bash scripts/golden-test.sh update
```

### Step 6: Create Dogfooding Guide

Create `docs/dogfooding.md`:

```markdown
# shux Dogfooding Guide

> Use shux as your daily driver. Report every rough edge.

## Getting Started

1. Build release binary: `make release`
2. Install: `make install` (installs to ~/.local/bin/shux)
3. Start your first session: `shux`
4. Or apply a template: `shux apply web-project --var project_name=myapp --var project_dir=.`

## Daily Driver Checklist

These are the things to verify work reliably over a full day of use:

- [ ] Session survives sleep/wake cycles
- [ ] Detach/reattach works reliably
- [ ] Multiple sessions work independently
- [ ] Split panes render correctly after resize
- [ ] Copy mode: can select and copy to system clipboard
- [ ] Mouse: click to focus, drag to resize, scroll for scrollback
- [ ] Status bar: updates correctly (session, windows, clock)
- [ ] Pane titles: auto-update from running commands
- [ ] Config changes reload without restart
- [ ] Theme changes apply immediately
- [ ] Command palette lists all commands and executes them
- [ ] Help overlay shows all current keybindings
- [ ] Performance: no perceptible lag on keypress
- [ ] High output: `seq 1 1000000` does not freeze other panes

## Reporting Issues

When you hit a rough edge:

1. Note the exact steps to reproduce
2. Run `shux doctor` to capture system state
3. Create an issue with the reproduction steps and doctor output
4. Tag with `dogfood` label

## Known Limitations (M1)

- No plugin system yet (M2)
- No session persistence across daemon restart (M2 plugin)
- No floating panes (M2 plugin)
- No image passthrough (M3)
- gRPC API not available (M2)
```

### Step 7: Update PROGRESS.md

Mark M1 as complete in `docs/PROGRESS.md`:

```markdown
## Current Phase

**M1: Daily-Driver Core** — Complete

## Status

### Milestone Targets

- [x] **M0: Architecture Spike** (tasks 001-012) -- Complete
- [x] **M1: Daily-Driver Core** (tasks 013-034) -- Complete
  - [x] Full session/window/pane CRUD (API + CLI)
  - [x] Splits, directional focus, resize, zoom, swap
  - [x] Copy mode with clipboard
  - [x] Graded keybindings (Tier 1 + 2), command palette, help overlay
  - [x] TOML config with live reload
  - [x] Theme engine with per-pane theming
  - [x] Mouse support, pane titles, status bar
  - [x] Session templates
  - [x] L1-L4 tests passing
  - [x] Dogfooding begins
```

---

## Verification

### Functional

```bash
# Full M1 quality gate
make m1-gate

# Individual test suites
cargo nextest run --test api_contract_test
cargo nextest run --test m1_test
cargo nextest run --test keybinding_test
cargo nextest run --test perf_test --no-capture

# Golden image tests (requires macOS with iTerm2)
bash scripts/golden-test.sh compare

# Manual dogfooding test: use shux for 2+ hours
# covering all features in the dogfooding checklist
```

### L4 Visual Regression — iterm2-driver (PRD §16.2)

Create `.claude/automations/test_m1_visual.py` — comprehensive iterm2-driver visual regression
covering all 7 scenarios from PRD L4: initial launch, splits, per-pane theming, zoom,
command palette, copy mode, help overlay.

**Pattern reference:** See `~/code/github.com/indrasvat-nidhi/.claude/automations/comprehensive_tui_test.py`
for the canonical iterm2-driver test pattern (interaction-based verification, content-change assertions,
polling with `wait_for_content`, screenshot capture via Quartz).

**Script format** — MUST use `uv` inline metadata:
```python
# /// script
# requires-python = ">=3.14"
# dependencies = [
#   "iterm2",
#   "pyobjc",
#   "pyobjc-framework-Quartz",
# ]
# ///
```

**Execution:** `uv run .claude/automations/test_m1_visual.py`

**Test matrix (7 scenarios per PRD L4):**

```python
"""
shux M1 Comprehensive Visual Verification Test (iterm2-driver)

Tests:
  Scenario 1: Initial launch
    1. Launch `shux new -s visual-test`
    2. Verify TUI renders (alt-screen, shell prompt visible)
    3. Verify status bar present at bottom (session name, window list, clock)
    4. Take screenshot: m1_01_initial_launch.png

  Scenario 2: Multi-pane splits (2x2 grid)
    5. Split vertical: Ctrl+Space then |
    6. Verify two panes visible with border separator
    7. Split horizontal in left pane: Ctrl+Space then -
    8. Split horizontal in right pane: focus right, Ctrl+Space then -
    9. Verify 4 panes visible in grid layout
    10. Take screenshot: m1_02_splits_2x2.png

  Scenario 3: Per-pane theming
    11. Set pane 1 theme to "prod" (red accent) via API: shux api pane.set_theme
    12. Set pane 2 theme to default-light (green accent)
    13. Verify screen content changed (different colors rendered)
    14. Take screenshot: m1_03_per_pane_theming.png

  Scenario 4: Zoom mode
    15. Focus pane 1, press Alt+z (zoom)
    16. Verify single pane fills entire viewport (no borders)
    17. Take screenshot: m1_04_zoom.png
    18. Press Alt+z again to unzoom
    19. Verify 4 panes restored

  Scenario 5: Copy mode
    20. Enter copy mode: Ctrl+Space then [
    21. Verify cursor visible and mode indicator shown
    22. Press v to start selection, move with j/l
    23. Verify selection highlighted
    24. Take screenshot: m1_05_copy_mode.png
    25. Press Escape to exit copy mode

  Scenario 6: Command palette
    26. Press Ctrl+Space then :
    27. Verify centered overlay appears with command list
    28. Type "split" to filter
    29. Verify filtered results shown
    30. Take screenshot: m1_06_command_palette.png
    31. Press Escape to close

  Scenario 7: Help overlay
    32. Press Ctrl+Space then ?
    33. Verify full-screen overlay with keybinding categories
    34. Verify categories: Navigation, Panes, Windows, Session, Copy, Config, General
    35. Press / to search, type "zoom"
    36. Verify filtered results
    37. Take screenshot: m1_07_help_overlay.png
    38. Press Escape to close

  Layout verification (every scenario):
    - Check box-drawing character connectivity (corners connected to edges)
    - Verify status bar width matches terminal width
    - Check no overlapping content between panes

Verification Strategy:
  - Use polling (wait_for_content) instead of hardcoded sleeps
  - Assert screen CONTENT CHANGES after each interaction
  - Dump screen on failure for debugging
  - Track pass/fail with test reporting pattern

Screenshots:
  - Saved to .claude/automations/screenshots/m1_*.png

Usage:
    uv run .claude/automations/test_m1_visual.py
"""
```

**Implementation notes:**
- Each scenario function should be independent and callable in isolation
- Use `log_result(name, status, details)` pattern from nidhi tests
- Use Quartz `CGWindowListCopyWindowInfo` + `screencapture -l` for window-targeted screenshots
- Include box-drawing layout verification checks between scenarios
- Multi-level cleanup in `finally`: Ctrl+C → `exit` → `session.async_close()`
- Makefile target: `make golden-test` runs `uv run .claude/automations/test_m1_visual.py`

**Additional per-feature iterm2-driver scripts** (created in the respective task's implementation):
- `.claude/automations/test_splits_visual.py` — from task 017 (multi-pane rendering)
- `.claude/automations/test_copy_mode_visual.py` — from task 021 (copy mode)
- `.claude/automations/test_theming_visual.py` — from task 025 (per-pane theming)
- `.claude/automations/test_palette_visual.py` — from task 032 (command palette)
- `.claude/automations/test_help_visual.py` — from task 033 (help overlay)

### Tests

```bash
# Run ALL tests (L1 through L4)
cargo nextest run --workspace

# API contract tests specifically
cargo nextest run --test api_contract_test -v

# Performance tests (run with --no-capture to see timing output)
cargo nextest run --test perf_test --no-capture

# Verify all M1 test files pass
cargo nextest run --test m1_test --test api_contract_test --test keybinding_test --test perf_test

# L4 visual regression (requires macOS with iTerm2)
uv run .claude/automations/test_m1_visual.py
```

---

## Completion Criteria

- [ ] **L3 API contract tests pass for ALL M1 API methods:**
  - [ ] session.create, session.ensure, session.list, session.rename, session.kill
  - [ ] window.create, window.ensure, window.list, window.rename, window.focus, window.kill
  - [ ] pane.split, pane.focus, pane.resize, pane.zoom, pane.swap, pane.kill, pane.set_title, pane.set_theme
  - [ ] config.get, config.set, config.validate
  - [ ] theme.list, theme.get, theme.set
  - [ ] keybinding.list, keybinding.set, keybinding.reset
  - [ ] Error cases: not_found, invalid_params, version_conflict
  - [ ] Idempotent ensure operations (session.ensure, window.ensure)
- [ ] **L4 visual regression: first golden images captured for:**
  - [ ] Initial launch (single pane with status bar)
  - [ ] Multi-pane splits (2x2 grid)
  - [ ] Per-pane theming (prod=red, dev=green)
  - [ ] Zoom mode (single pane filling screen)
  - [ ] Copy mode (selection highlighted)
  - [ ] Command palette (centered overlay with entries)
  - [ ] Help overlay (full-screen keybinding reference)
- [ ] Golden images captured via iterm2-driver: `uv run .claude/automations/test_m1_visual.py`
- [ ] All 7 screenshots saved to `.claude/automations/screenshots/m1_*.png`
- [ ] Layout verification passes (box-drawing connectivity, status bar width, no content overlap)
- [ ] **Performance budgets met:**
  - [ ] Keypress to visible update: p99 <= 25ms
  - [ ] Split pane operation: p99 <= 80ms
- [ ] **Cross-feature integration tested:**
  - [ ] Full workflow: create session, windows, splits, themes, titles, zoom, swap, resize, kill
  - [ ] Template application creates complete workspace atomically
  - [ ] Config runtime override reflected in UI
  - [ ] Keybinding override works at runtime
- [ ] **Quality gate targets added to Makefile:** m1-gate, test-api, test-perf, golden-*
- [ ] **Dogfooding guide created:** docs/dogfooding.md
- [ ] **PROGRESS.md updated:** M1 marked complete with session log entry
- [ ] **All clippy and format checks pass:** `make lint` clean
- [ ] **All existing L1/L2 tests still pass:** No regressions

---

## Commit Message

```
test: add M1 integration tests, golden images, and quality gate

- L3 API contract tests for all M1 methods (session, window, pane,
  config, theme, keybinding) with happy paths, error cases, and
  idempotent ensure operations
- L4 visual regression: golden image capture and comparison infrastructure
  with 7 initial scenarios (launch, splits, theming, zoom, copy,
  command palette, help overlay)
- Performance budget validation: keypress p99 <= 25ms, split p99 <= 80ms
- Full M1 workflow integration test (cross-feature interactions)
- Makefile targets: m1-gate, test-api, test-perf, golden-*
- Dogfooding guide: docs/dogfooding.md
- Mark M1 milestone complete in PROGRESS.md
```

---

## Session Protocol

1. **Before starting:** Verify ALL tasks 013-033 are marked complete in PROGRESS.md. Run `cargo nextest run --workspace` to confirm all existing tests pass. Build the release binary (`make release`).
2. **During:** Work through steps in order. For each API contract test, run it immediately after writing (`cargo nextest run --test api_contract_test::test_<name>`). Fix any failures discovered during testing before moving to the next test. For golden images, capture first and then compare. For performance tests, run multiple times to get stable numbers.
3. **Bug fixing protocol:**
   - If a test reveals a bug in a previous task's implementation, fix it in the appropriate crate
   - Document the bug and fix in CLAUDE.md Learnings
   - Re-run all tests to confirm the fix does not introduce regressions
   - Do NOT skip a failing test — either fix the bug or document it as a known limitation
4. **After:** Run the full quality gate (`make m1-gate`). Use shux as your terminal for at least 30 minutes. Update PROGRESS.md with detailed M1 completion notes. Update CLAUDE.md Learnings with everything discovered. Celebrate — M1 is the point where shux becomes a real, usable tool.
