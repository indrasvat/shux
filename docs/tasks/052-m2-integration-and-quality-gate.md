# 052 — M2 Integration and Quality Gate

**Status:** Pending
**Depends On:** 035-051
**Parallelizable With:** ---

---

## Problem

M2 (API Completeness + Plugin System) is the most complex milestone in shux, introducing the complete API surface, plugin host, event streaming, and three bundled plugins. Before moving to M3 polish, every M2 feature must be verified end-to-end. This task runs comprehensive integration tests, agent scenario scripts, and plugin tests that prove the entire M2 surface works correctly. The PRD's "Done when" criteria for M2 are specific: every API method has a contract test, agent scenarios pass, bundled plugins work, plugin hot reload works, and `shux doctor` works. This quality gate catches integration issues that unit tests miss.

## PRD Reference

- **SS 17** M2 "Done when": "Every API method has a contract test. Agent scenarios pass. Bundled plugins work. Plugin hot reload works. `shux doctor` works."
- **SS 16.1** Testing pyramid: L3 (API contract), L5 (Agent scenarios), L6 (Dogfood)
- **SS 16.2** Layer details: L3 contract test specifics (permission matrix, protocol parity, frame limits, interceptors), L5 agent scenarios (3 scripts)
- **SS 18** Success metrics: API coverage, plugin API sufficiency, agent story, crash resilience
- **SS 8.4** Event stream: Resume/gap detection contract
- **SS 5.4** state.apply: Batch operations with back-references, client_request_id deduplication

---

## Files to Create

- `tests/integration/m2_api_contract.rs` — L3 API contract tests for every JSON-RPC method
- `tests/integration/m2_event_stream.rs` — Event stream resume, gap detection, filtering
- `tests/integration/m2_plugin_system.rs` — Plugin lifecycle, hot reload, GC, permissions
- `tests/integration/m2_bundled_plugins.rs` — Bundled plugin functional tests
- `tests/integration/m2_state_apply.rs` — Batch operations, back-references, idempotency
- `tests/integration/m2_security.rs` — Permission matrix, frame limits, auth
- `tests/agent_scenarios/scenario_01_setup_workspace.py` — Agent: create workspace
- `tests/agent_scenarios/scenario_02_monitor_processes.py` — Agent: watch events, react
- `tests/agent_scenarios/scenario_03_batch_operations.py` — Agent: state.apply with batch ops
- `tests/agent_scenarios/requirements.txt` — Python dependencies for scenarios
- `tests/agent_scenarios/conftest.py` — Shared test fixtures (daemon connection, cleanup)
- `scripts/run-m2-gate.sh` — Orchestrator script for the full quality gate

## Files to Modify

- `Cargo.toml` — Add integration test dependencies if needed
- `docs/PROGRESS.md` — Mark all M2 tasks complete, update milestone status

---

## Execution Steps

### Step 1: Create Test Infrastructure

Create `tests/integration/m2_api_contract.rs` with a shared test harness that starts an ephemeral daemon, connects via UDS, and provides helper functions:

```rust
//! M2 L3 API contract tests — every JSON-RPC method exercised.
//!
//! Tests run against a real daemon with an ephemeral socket.
//! Each test gets a fresh daemon to avoid cross-contamination.

use std::path::PathBuf;
use std::process::{Child, Command};
use tempfile::TempDir;

/// Test harness: starts a shux daemon on an ephemeral socket.
struct TestDaemon {
    _process: Child,
    socket_path: PathBuf,
    _tmp_dir: TempDir,
}

impl TestDaemon {
    async fn start() -> Self {
        let tmp_dir = TempDir::new().expect("Failed to create temp dir");
        let socket_path = tmp_dir.path().join("shux-test.sock");

        let process = Command::new(env!("CARGO_BIN_EXE_shux"))
            .arg("api")
            .arg("serve")
            .arg("--socket")
            .arg(&socket_path)
            .arg("--no-daemonize")
            .spawn()
            .expect("Failed to start test daemon");

        // Wait for socket to appear
        for _ in 0..50 {
            if socket_path.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(socket_path.exists(), "Daemon did not start");

        Self {
            _process: process,
            socket_path,
            _tmp_dir: tmp_dir,
        }
    }

    /// Send a JSON-RPC request and return the response.
    async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> serde_json::Value {
        // Connect to UDS, send length-prefixed JSON-RPC, read response
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": uuid::Uuid::new_v4().to_string(),
            "method": method,
            "params": params,
        });

        // Length-prefixed framing: 4-byte BE length + JSON
        let payload = serde_json::to_vec(&request).unwrap();
        let mut frame = Vec::with_capacity(4 + payload.len());
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(&payload);

        use tokio::net::UnixStream;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut stream = UnixStream::connect(&self.socket_path).await.unwrap();
        stream.write_all(&frame).await.unwrap();

        // Read response: 4-byte length + payload
        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).await.unwrap();
        let response_len = u32::from_be_bytes(len_buf) as usize;

        let mut response_buf = vec![0u8; response_len];
        stream.read_exact(&mut response_buf).await.unwrap();

        serde_json::from_slice(&response_buf).unwrap()
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        let _ = self._process.kill();
    }
}
```

### Step 2: API Contract Tests — Happy Paths

Every API method from SS8.2 must have a happy-path test:

```rust
#[tokio::test]
async fn test_system_version() {
    let daemon = TestDaemon::start().await;
    let resp = daemon.call("system.version", serde_json::json!({})).await;
    assert!(resp["result"]["version"].is_string());
}

#[tokio::test]
async fn test_system_health() {
    let daemon = TestDaemon::start().await;
    let resp = daemon.call("system.health", serde_json::json!({})).await;
    assert_eq!(resp["result"]["status"], "ok");
}

#[tokio::test]
async fn test_session_lifecycle() {
    let daemon = TestDaemon::start().await;

    // Create
    let resp = daemon.call("session.create", serde_json::json!({"name": "test"})).await;
    let session_id = resp["result"]["id"].as_str().unwrap().to_string();
    assert!(!session_id.is_empty());

    // List
    let resp = daemon.call("session.list", serde_json::json!({})).await;
    let sessions = resp["result"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);

    // Rename
    let resp = daemon.call("session.rename", serde_json::json!({
        "session_id": session_id,
        "new_name": "renamed",
    })).await;
    assert!(resp["error"].is_null());

    // Kill
    let resp = daemon.call("session.kill", serde_json::json!({"session_id": session_id})).await;
    assert!(resp["error"].is_null());
}

#[tokio::test]
async fn test_session_ensure_idempotent() {
    let daemon = TestDaemon::start().await;

    let resp1 = daemon.call("session.ensure", serde_json::json!({"name": "work"})).await;
    let id1 = resp1["result"]["id"].as_str().unwrap();

    let resp2 = daemon.call("session.ensure", serde_json::json!({"name": "work"})).await;
    let id2 = resp2["result"]["id"].as_str().unwrap();

    assert_eq!(id1, id2, "Ensure must return the same session");
}

// ... continue for all methods: window.*, pane.*, theme.*, config.*,
// plugin.*, events.*, copy.*, keybinding.*, admin.*, log.*, metrics.*, diagnose.*
```

### Step 3: API Contract Tests — Error Paths

```rust
#[tokio::test]
async fn test_version_conflict_rejected() {
    let daemon = TestDaemon::start().await;

    let resp = daemon.call("session.create", serde_json::json!({"name": "test"})).await;
    let session_id = resp["result"]["id"].as_str().unwrap();

    // Rename with stale version
    let resp = daemon.call("session.rename", serde_json::json!({
        "session_id": session_id,
        "new_name": "new",
        "version": 0,  // stale
    })).await;
    assert_eq!(resp["error"]["code"], -32001);
    assert_eq!(resp["error"]["message"], "version_conflict");
}

#[tokio::test]
async fn test_frame_size_limit_rejection() {
    let daemon = TestDaemon::start().await;

    // Send a payload exceeding 16 MB
    let huge_payload = "x".repeat(17 * 1024 * 1024);
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "too-big",
        "method": "pane.send_keys",
        "params": {"pane_id": "fake", "data": huge_payload},
    });

    // Connection should be rejected or closed
    let payload = serde_json::to_vec(&request).unwrap();
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&payload);

    use tokio::net::UnixStream;
    use tokio::io::AsyncWriteExt;
    let mut stream = UnixStream::connect(&daemon.socket_path).await.unwrap();
    let result = stream.write_all(&frame).await;
    // Either write fails or subsequent read returns error
    // The daemon must not crash
}

#[tokio::test]
async fn test_permission_denied_matrix() {
    let daemon = TestDaemon::start().await;

    // Load a plugin with no permissions, then try privileged operations
    // Each permission must be tested with both grant and deny:
    // - manage_panes: create_pane should fail
    // - send_keys: send_keys should fail
    // - manage_sessions: create_session should fail
    // - api_extensions: register_api_method should fail
    // - fs_read: read_file should fail
    // - fs_write: write_file should fail
    // - clipboard: get_clipboard should fail
    // - exec: exec should fail
}

#[tokio::test]
async fn test_client_request_id_deduplication() {
    let daemon = TestDaemon::start().await;

    let params = serde_json::json!({
        "name": "dedup-test",
        "client_request_id": "unique-id-001",
    });

    let resp1 = daemon.call("session.create", params.clone()).await;
    let resp2 = daemon.call("session.create", params.clone()).await;

    // Second call should be a no-op, returning cached result
    assert_eq!(resp1["result"]["id"], resp2["result"]["id"]);
}
```

### Step 4: Event Stream Tests

Create `tests/integration/m2_event_stream.rs`:

```rust
#[tokio::test]
async fn test_event_stream_resume_with_gap_detection() {
    let daemon = TestDaemon::start().await;

    // Create events to fill the ring buffer
    for i in 0..10 {
        daemon.call("session.create", serde_json::json!({"name": format!("s{}", i)})).await;
    }

    // Subscribe from an old sequence number
    // If from_seq is too old, should receive a gap notification
    let resp = daemon.call("events.watch", serde_json::json!({
        "filters": ["session.created"],
        "from_seq": 0,
    })).await;

    // Verify events are delivered with sequence numbers
    // Verify gap notification if ring buffer was exceeded
}

#[tokio::test]
async fn test_event_stream_filtering() {
    let daemon = TestDaemon::start().await;

    // Subscribe to only pane events
    // Create a session (should NOT generate matching event)
    // Split a pane (should generate matching event)
}

#[tokio::test]
async fn test_interceptor_timeout_fail_closed() {
    let daemon = TestDaemon::start().await;

    // Load a plugin that intercepts pane.input and sleeps > 100ms
    // Send input to a pane
    // Verify: event is BLOCKED (fail-closed), plugin.error event emitted
}
```

### Step 5: State Apply Tests

Create `tests/integration/m2_state_apply.rs`:

```rust
#[tokio::test]
async fn test_batch_apply_with_back_references() {
    let daemon = TestDaemon::start().await;

    let resp = daemon.call("state.apply", serde_json::json!({
        "client_request_id": "batch-001",
        "operations": [
            {"op": "session.create", "params": {"name": "work"}},
            {"op": "window.create", "params": {"session_id": "$0.id", "name": "editor"}},
            {"op": "pane.split", "params": {
                "pane_id": "$1.active_pane_id",
                "direction": "vertical",
                "command": ["nvim"]
            }},
        ]
    })).await;

    assert!(resp["error"].is_null(), "Batch apply should succeed");

    // Verify the session, window, and pane were all created
    let sessions = daemon.call("session.list", serde_json::json!({})).await;
    assert_eq!(sessions["result"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn test_batch_apply_atomic_rollback() {
    let daemon = TestDaemon::start().await;

    // Create a session first
    daemon.call("session.create", serde_json::json!({"name": "existing"})).await;

    // Batch that fails midway: second op references invalid session
    let resp = daemon.call("state.apply", serde_json::json!({
        "operations": [
            {"op": "session.create", "params": {"name": "new-session"}},
            {"op": "window.create", "params": {"session_id": "nonexistent", "name": "fail"}},
        ]
    })).await;

    // Should fail, and first operation should be rolled back
    assert!(resp["error"].is_some());

    // Only the original session should exist
    let sessions = daemon.call("session.list", serde_json::json!({})).await;
    assert_eq!(sessions["result"].as_array().unwrap().len(), 1);
}
```

### Step 6: Plugin System Tests

Create `tests/integration/m2_plugin_system.rs`:

```rust
#[tokio::test]
async fn test_plugin_hot_reload() {
    let daemon = TestDaemon::start().await;

    // Enable a bundled plugin
    daemon.call("plugin.enable", serde_json::json!({"plugin_id": "com.shux.theme-pack"})).await;

    // Verify it's running
    let resp = daemon.call("plugin.inspect", serde_json::json!({
        "plugin_id": "com.shux.theme-pack"
    })).await;
    assert_eq!(resp["result"]["state"], "running");

    // Reload
    daemon.call("plugin.reload", serde_json::json!({
        "plugin_id": "com.shux.theme-pack"
    })).await;

    // Should still be running after reload
    let resp = daemon.call("plugin.inspect", serde_json::json!({
        "plugin_id": "com.shux.theme-pack"
    })).await;
    assert_eq!(resp["result"]["state"], "running");
}

#[tokio::test]
async fn test_plugin_gc_idle_process_plugin() {
    // Enable a process plugin with gc=true
    // Wait for gc_timeout (30s in test, shortened)
    // Verify plugin was stopped
}

#[tokio::test]
async fn test_process_plugin_protocol_parity_with_wit() {
    // Load the same plugin as both Wasm and process
    // Execute the same operations on both
    // Verify identical results
}

#[tokio::test]
async fn test_all_bundled_plugins_load() {
    let daemon = TestDaemon::start().await;

    let resp = daemon.call("plugin.list", serde_json::json!({})).await;
    let plugins = resp["result"].as_array().unwrap();

    let plugin_ids: Vec<&str> = plugins.iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();

    assert!(plugin_ids.contains(&"com.shux.status-bar"));
    assert!(plugin_ids.contains(&"com.shux.theme-pack"));
    assert!(plugin_ids.contains(&"com.shux.diagnostics"));
}
```

### Step 7: Agent Scenario Scripts

Create `tests/agent_scenarios/scenario_01_setup_workspace.py`:

```python
#!/usr/bin/env python3
"""Agent Scenario 1: Setup a development workspace.

Demonstrates the agent read → plan → apply → verify loop.
Creates a session, windows, splits panes, sets themes.
"""

import json
import socket
import struct
import sys
import os

SOCKET_PATH = os.environ.get("SHUX_SOCKET", "/tmp/shux-test.sock")

def send_rpc(sock, method, params=None):
    """Send a JSON-RPC request and return the response."""
    request = {
        "jsonrpc": "2.0",
        "id": f"agent-{method}",
        "method": method,
        "params": params or {},
    }
    payload = json.dumps(request).encode("utf-8")
    frame = struct.pack(">I", len(payload)) + payload
    sock.sendall(frame)

    length_bytes = sock.recv(4)
    length = struct.unpack(">I", length_bytes)[0]
    response_bytes = b""
    while len(response_bytes) < length:
        chunk = sock.recv(length - len(response_bytes))
        if not chunk:
            raise ConnectionError("Connection closed")
        response_bytes += chunk

    return json.loads(response_bytes)

def main():
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(SOCKET_PATH)

    # Step 1: Ensure session exists (idempotent)
    resp = send_rpc(sock, "session.ensure", {"name": "dev-workspace"})
    assert "result" in resp, f"session.ensure failed: {resp}"
    session_id = resp["result"]["id"]
    print(f"Session: {session_id}")

    # Step 2: Batch apply — create windows and panes atomically
    resp = send_rpc(sock, "state.apply", {
        "client_request_id": "agent-setup-001",
        "operations": [
            {"op": "window.ensure", "params": {"session_id": session_id, "name": "editor"}},
            {"op": "window.ensure", "params": {"session_id": session_id, "name": "servers"}},
            {"op": "window.ensure", "params": {"session_id": session_id, "name": "logs"}},
        ],
    })
    assert "result" in resp, f"state.apply failed: {resp}"

    # Step 3: Split panes in servers window
    windows = send_rpc(sock, "window.list", {"session_id": session_id})
    servers_window = next(
        w for w in windows["result"] if w["title"] == "servers"
    )
    pane_id = servers_window["active_pane_id"]

    resp = send_rpc(sock, "pane.split", {
        "pane_id": pane_id,
        "direction": "vertical",
    })
    assert "result" in resp

    # Step 4: Set theme on prod-like pane
    resp = send_rpc(sock, "pane.set_theme", {
        "pane_id": pane_id,
        "theme": "prod",
    })
    assert "error" not in resp or resp["error"] is None

    # Step 5: Verify final state
    snapshot = send_rpc(sock, "state.snapshot", {})
    assert "result" in snapshot

    print("Scenario 1 PASSED: workspace setup complete")
    sock.close()
    return 0

if __name__ == "__main__":
    sys.exit(main())
```

Create `tests/agent_scenarios/scenario_02_monitor_processes.py`:

```python
#!/usr/bin/env python3
"""Agent Scenario 2: Monitor processes.

Subscribes to events, watches for pane.exited, reacts by logging.
Tests the event streaming API from an agent's perspective.
"""
# ... event subscription, pane.exited detection, reaction logic

def main():
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(SOCKET_PATH)

    # Create a pane running a short-lived command
    resp = send_rpc(sock, "session.ensure", {"name": "monitor-test"})
    session_id = resp["result"]["id"]

    # Subscribe to pane events
    resp = send_rpc(sock, "events.watch", {
        "filters": ["pane.exited"],
    })

    # Run a command that exits quickly
    windows = send_rpc(sock, "window.list", {"session_id": session_id})
    pane_id = windows["result"][0]["active_pane_id"]

    send_rpc(sock, "pane.run_command", {
        "pane_id": pane_id,
        "command": ["echo", "hello"],
    })

    # Verify we receive the pane.exited event
    # (Read streaming events from the watch connection)

    print("Scenario 2 PASSED: process monitoring works")
    sock.close()
    return 0
```

Create `tests/agent_scenarios/scenario_03_batch_operations.py`:

```python
#!/usr/bin/env python3
"""Agent Scenario 3: Batch operations with state.apply.

Tests complex batch operations with back-references,
idempotency keys, and verification.
"""

def main():
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(SOCKET_PATH)

    # Batch: create session + 3 windows + split each
    resp = send_rpc(sock, "state.apply", {
        "client_request_id": "batch-complex-001",
        "operations": [
            {"op": "session.create", "params": {"name": "batch-test"}},
            {"op": "window.create", "params": {"session_id": "$0.id", "name": "w1"}},
            {"op": "window.create", "params": {"session_id": "$0.id", "name": "w2"}},
            {"op": "window.create", "params": {"session_id": "$0.id", "name": "w3"}},
            {"op": "pane.split", "params": {"pane_id": "$1.active_pane_id", "direction": "vertical"}},
            {"op": "pane.split", "params": {"pane_id": "$2.active_pane_id", "direction": "horizontal"}},
        ],
    })
    assert "result" in resp, f"Batch apply failed: {resp}"

    # Replay the same batch (idempotent via client_request_id)
    resp2 = send_rpc(sock, "state.apply", {
        "client_request_id": "batch-complex-001",
        "operations": [
            {"op": "session.create", "params": {"name": "batch-test"}},
        ],
    })
    # Should return cached result, not create a duplicate
    assert resp["result"] == resp2["result"], "Replay should be idempotent"

    print("Scenario 3 PASSED: batch operations work")
    sock.close()
    return 0
```

### Step 8: Create Quality Gate Script

Create `scripts/run-m2-gate.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail

echo "╔══════════════════════════════════════╗"
echo "║   shux M2 Quality Gate               ║"
echo "╚══════════════════════════════════════╝"

FAILURES=0

# L1: Unit tests
echo "─── L1: Unit Tests ───"
if cargo nextest run --workspace --lib; then
    echo "L1 PASSED"
else
    echo "L1 FAILED"
    FAILURES=$((FAILURES + 1))
fi

# L2: PTY integration
echo "─── L2: PTY Integration ───"
if cargo nextest run --workspace -E 'test(pty)'; then
    echo "L2 PASSED"
else
    echo "L2 FAILED"
    FAILURES=$((FAILURES + 1))
fi

# L3: API contract tests
echo "─── L3: API Contract Tests ───"
if cargo nextest run --test 'm2_*'; then
    echo "L3 PASSED"
else
    echo "L3 FAILED"
    FAILURES=$((FAILURES + 1))
fi

# L5: Agent scenarios
echo "─── L5: Agent Scenarios ───"
SHUX_SOCKET="/tmp/shux-m2-gate-$$.sock"
export SHUX_SOCKET
cargo run -p shux -- api serve --socket "$SHUX_SOCKET" --no-daemonize &
DAEMON_PID=$!
sleep 2

SCENARIO_PASS=true
for script in tests/agent_scenarios/scenario_*.py; do
    echo "Running $script..."
    if python3 "$script"; then
        echo "  PASSED"
    else
        echo "  FAILED"
        SCENARIO_PASS=false
    fi
done

kill "$DAEMON_PID" 2>/dev/null || true
rm -f "$SHUX_SOCKET"

if $SCENARIO_PASS; then
    echo "L5 PASSED"
else
    echo "L5 FAILED"
    FAILURES=$((FAILURES + 1))
fi

# L6: Dogfood (run tests inside shux)
echo "─── L6: Dogfood ───"
echo "(Skipped in automated gate — manual verification)"

# Summary
echo ""
echo "══════════════════════════════════"
if [ "$FAILURES" -eq 0 ]; then
    echo "M2 QUALITY GATE: ALL PASSED"
    exit 0
else
    echo "M2 QUALITY GATE: $FAILURES LAYER(S) FAILED"
    exit 1
fi
```

### Step 9: Update PROGRESS.md

Mark all M2 tasks (035-052) as complete. Update the milestone status from "Not started" to "Complete". Add a session log entry documenting the M2 gate results.

---

## Verification

### Functional

```bash
# Run the complete M2 quality gate
./scripts/run-m2-gate.sh

# Run only the L3 contract tests
cargo nextest run --test 'm2_*'

# Run only agent scenarios
python3 tests/agent_scenarios/scenario_01_setup_workspace.py
python3 tests/agent_scenarios/scenario_02_monitor_processes.py
python3 tests/agent_scenarios/scenario_03_batch_operations.py

# Verify doctor command works
shux doctor
shux doctor --redact strict

# Verify bundled plugins
shux plugin ls
shux theme ls
```

### Tests

```bash
# Full test suite including M2 integration tests
cargo nextest run --workspace

# Count: verify every API method has at least one test
grep -c '#\[tokio::test\]' tests/integration/m2_api_contract.rs
# Expected: >= 40 (one per API method minimum)
```

---

## Completion Criteria

- [ ] L3 API contract tests exist for EVERY method in SS8.2 (happy path + at least one error)
- [ ] Event stream resume/gap detection tested
- [ ] Permission-denied matrix: every permission tested with both grant and deny
- [ ] Process plugin protocol parity verified against WIT
- [ ] Frame-size limit rejection tested (16 MB limit)
- [ ] Interceptor timeout/fail-closed behavior tested
- [ ] Idempotent ensure operations tested (session, window, pane)
- [ ] Batch state.apply with back-references ($N.field) tested
- [ ] client_request_id deduplication tested
- [ ] Agent Scenario 1 (setup workspace) passes end-to-end
- [ ] Agent Scenario 2 (monitor processes) passes end-to-end
- [ ] Agent Scenario 3 (batch operations) passes end-to-end
- [ ] Plugin hot reload tested: disable, modify, reload, verify state
- [ ] Plugin GC tested: idle process plugin is stopped
- [ ] All 3 bundled plugins load and function correctly
- [ ] `shux doctor` produces valid JSON bundle
- [ ] All L1-L5 test layers pass (L6 manual)
- [ ] PROGRESS.md updated: all M2 tasks marked complete, session log entry

---

## Commit Message

```
test: add M2 integration tests, agent scenarios, and quality gate

- L3 API contract tests for every JSON-RPC method (happy + error)
- Event stream resume/gap detection, filtering tests
- Permission matrix, frame limits, interceptor fail-closed tests
- Batch state.apply with back-references and idempotency
- 3 Python agent scenarios driving API end-to-end
- Plugin hot reload, GC, and bundled plugin verification
- Quality gate script running L1-L5 test layers
- Update PROGRESS.md for M2 completion
```

---

## Session Protocol

1. **Before starting:** Verify all tasks 035-051 are complete and their tests pass individually. Read the full API method list from SS8.2 — use it as a checklist. Read SS16.2 for the specific contract test requirements (gap detection, permission matrix, frame limits, etc.).
2. **During:** Start with test infrastructure (Step 1), then systematically write one test per API method. Keep a checklist of methods tested. Write agent scenarios last (they depend on a fully working API). Run each test as you write it.
3. **Coverage checklist:** Print the method list from SS8.2 and check off each method as its test is written. The gate fails if any method lacks a test.
4. **Agent scenarios:** These are Python scripts, not Rust tests. They must work with a real running daemon. Test the UDS framing protocol manually first.
5. **After:** Run `./scripts/run-m2-gate.sh` and verify all layers pass. Update `docs/PROGRESS.md` with all M2 tasks marked complete. Update `CLAUDE.md` Learnings (create from task 000 template if missing) with any integration issues discovered.
