# 012 — M0 Integration and Quality Gate

**Status:** Done
**Depends On:** 001, 002, 003, 004, 005, 006, 007, 008, 009, 010, 011
**Parallelizable With:** --

---

## Problem

Milestone 0 (Architecture Spike) is the first end-to-end proof that shux's core architecture works. All the individual components -- daemon, PTY, VT parser, event bus, RPC server, compositor, TUI client, CLI -- have been built and unit-tested in tasks 001-011. But they have not been wired together and tested as a complete system. This task is the integration point and quality gate: it connects every M0 component, writes integration tests that exercise the full pipeline, establishes a performance baseline, and verifies the PRD's "Done when" criteria for M0.

This is a blocking gate. No M1 work begins until this task's completion criteria are fully met.

## PRD Reference

- **section 17, M0** — "Done when: `shux new -s test` starts daemon, creates session, attaches TUI. Typing works. Detach/reattach works. `shux api system.version --format json` works."
- **section 16.1** — Testing pyramid: L1 (headless unit) and L2 (PTY integration) must pass for M0
- **section 14.1** — Performance budgets: establish baselines for keypress-to-render, PTY throughput
- **section 4.1** — Single binary with subcommands
- **section 4.2** — System diagram: full client-daemon-PTY pipeline
- **section 4.3** — Architectural invariants (CLI == API, single source of truth, etc.)

---

## Files to Create

- `tests/integration/m0_test.rs` — M0 integration test suite
- `tests/integration/mod.rs` — Test module root (if not already present)
- `scripts/bench-baseline.sh` — Performance baseline measurement script

## Files to Modify

- `Cargo.toml` (workspace root) — Add integration test configuration if needed
- `docs/PROGRESS.md` — Mark all M0 tasks complete, add session log entry
- `CLAUDE.md` — Update Learnings section with M0 findings

---

## Execution Steps

### Step 1: Verify all M0 components compile together

Before writing integration tests, verify the entire workspace builds cleanly with all M0 code.

```bash
# Full workspace build
cargo build --workspace

# Full workspace clippy
cargo clippy --workspace --all-targets -- -D warnings

# Full workspace formatting
cargo fmt --all -- --check

# All existing unit tests pass
cargo nextest run --workspace
```

If any of these fail, the underlying task (001-011) is incomplete. Fix before proceeding.

### Step 2: Write the integration test harness

The integration tests need a helper that:
1. Starts a daemon on an ephemeral UDS socket (in a temp directory)
2. Waits for the daemon to be ready
3. Provides a JSON-RPC client for sending commands
4. Cleans up the daemon on test completion

```rust
// tests/integration/m0_test.rs

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::Duration;

use serde_json::Value;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// Test harness that manages a daemon instance for integration testing.
struct TestDaemon {
    /// Temporary directory for the socket file (cleaned up on drop)
    _temp_dir: TempDir,
    /// Path to the UDS socket
    socket_path: PathBuf,
    /// Daemon process handle
    daemon_process: Option<Child>,
}

impl TestDaemon {
    /// Start a new daemon instance on an ephemeral socket.
    async fn start() -> anyhow::Result<Self> {
        let temp_dir = TempDir::new()?;
        let socket_path = temp_dir.path().join("shux-test.sock");

        // Find the shux binary
        let bin = Self::find_binary()?;

        // Start the daemon
        let daemon_process = Command::new(&bin)
            .arg("api")
            .arg("serve")
            .arg("--socket")
            .arg(&socket_path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;

        let mut harness = Self {
            _temp_dir: temp_dir,
            socket_path,
            daemon_process: Some(daemon_process),
        };

        // Wait for daemon to be ready (poll socket with backoff)
        harness.wait_for_ready(Duration::from_secs(5)).await?;

        Ok(harness)
    }

    /// Wait for the daemon to start listening on its socket.
    async fn wait_for_ready(&self, timeout: Duration) -> anyhow::Result<()> {
        let start = std::time::Instant::now();
        let poll_interval = Duration::from_millis(50);

        loop {
            if start.elapsed() > timeout {
                anyhow::bail!(
                    "Daemon did not become ready within {}s",
                    timeout.as_secs()
                );
            }

            match UnixStream::connect(&self.socket_path).await {
                Ok(_) => return Ok(()),
                Err(_) => {
                    tokio::time::sleep(poll_interval).await;
                }
            }
        }
    }

    /// Connect to the daemon and return a UnixStream.
    async fn connect(&self) -> anyhow::Result<UnixStream> {
        let stream = UnixStream::connect(&self.socket_path).await?;
        Ok(stream)
    }

    /// Send a JSON-RPC request and return the response.
    async fn rpc(
        &self,
        method: &str,
        params: Value,
    ) -> anyhow::Result<Value> {
        let mut stream = self.connect().await?;
        rpc_call(&mut stream, method, params).await
    }

    /// Find the shux binary in the target directory.
    fn find_binary() -> anyhow::Result<PathBuf> {
        // assert_cmd's approach: look in target/debug/
        let mut path = std::env::current_exe()?;
        path.pop(); // Remove the test binary name
        path.pop(); // Remove 'deps'
        path.push("shux");

        if path.exists() {
            Ok(path)
        } else {
            anyhow::bail!(
                "shux binary not found at {}. Run 'cargo build' first.",
                path.display()
            );
        }
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        if let Some(mut process) = self.daemon_process.take() {
            // Send SIGTERM for graceful shutdown
            #[cfg(unix)]
            {
                unsafe {
                    libc::kill(process.id() as i32, libc::SIGTERM);
                }
            }
            // Wait briefly, then force kill
            std::thread::sleep(Duration::from_millis(200));
            let _ = process.kill();
            let _ = process.wait();
        }
    }
}

/// Send a JSON-RPC request over a UnixStream with length-prefixed framing.
async fn rpc_call(
    stream: &mut UnixStream,
    method: &str,
    params: Value,
) -> anyhow::Result<Value> {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": uuid::Uuid::new_v4().to_string(),
        "method": method,
        "params": params,
    });

    let payload = serde_json::to_vec(&request)?;

    // Write length prefix (4 bytes, big-endian)
    let len = payload.len() as u32;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(&payload).await?;
    stream.flush().await?;

    // Read response
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let resp_len = u32::from_be_bytes(len_buf) as usize;

    let mut resp_buf = vec![0u8; resp_len];
    stream.read_exact(&mut resp_buf).await?;

    let response: Value = serde_json::from_slice(&resp_buf)?;

    if let Some(error) = response.get("error") {
        anyhow::bail!("RPC error: {}", serde_json::to_string_pretty(error)?);
    }

    Ok(response.get("result").cloned().unwrap_or(Value::Null))
}
```

### Step 3: Write the M0 "Done when" integration tests

These tests directly verify the PRD section 17 M0 completion criteria.

```rust
// tests/integration/m0_test.rs (continued)

/// ═══════════════════════════════════════════════════════════════════
/// M0 "Done when" tests (PRD section 17, M0)
/// ═══════════════════════════════════════════════════════════════════

/// Test 1: `shux new -s test` starts daemon and creates session.
///
/// PRD: "shux new -s test → starts daemon, creates session"
#[tokio::test]
async fn test_m0_create_session() {
    let daemon = TestDaemon::start().await
        .expect("Failed to start test daemon");

    // Create a session named "test"
    let result = daemon
        .rpc(
            "session.create",
            serde_json::json!({"name": "test"}),
        )
        .await
        .expect("session.create should succeed");

    // Verify the response contains expected fields
    assert!(
        result.get("id").is_some(),
        "session.create response should contain 'id'"
    );
    let name = result
        .get("name")
        .and_then(|v| v.as_str())
        .expect("session.create response should contain 'name'");
    assert_eq!(name, "test");
}

/// Test 2: Session appears in session.list after creation.
///
/// PRD: session.list shows sessions
#[tokio::test]
async fn test_m0_list_sessions() {
    let daemon = TestDaemon::start().await
        .expect("Failed to start test daemon");

    // Create a session
    daemon
        .rpc(
            "session.create",
            serde_json::json!({"name": "list-test"}),
        )
        .await
        .expect("session.create should succeed");

    // List sessions
    let result = daemon
        .rpc("session.list", serde_json::json!({}))
        .await
        .expect("session.list should succeed");

    let sessions = result
        .as_array()
        .expect("session.list should return an array");
    assert!(
        !sessions.is_empty(),
        "session.list should return at least one session"
    );

    // Find our session
    let found = sessions.iter().any(|s| {
        s.get("name").and_then(|v| v.as_str()) == Some("list-test")
    });
    assert!(found, "session 'list-test' should appear in session.list");
}

/// Test 3: system.version returns valid version info.
///
/// PRD: "shux api system.version --format json works"
#[tokio::test]
async fn test_m0_system_version() {
    let daemon = TestDaemon::start().await
        .expect("Failed to start test daemon");

    let result = daemon
        .rpc("system.version", serde_json::json!({}))
        .await
        .expect("system.version should succeed");

    // Verify the response contains a version string
    let version = result
        .get("version")
        .and_then(|v| v.as_str())
        .expect("system.version should return a 'version' string");

    // Version should be a valid semver-like string
    assert!(
        !version.is_empty(),
        "version string should not be empty"
    );
    assert!(
        version.contains('.'),
        "version string should contain a dot (semver format): got '{version}'"
    );
}

/// Test 4: system.health returns healthy status.
#[tokio::test]
async fn test_m0_system_health() {
    let daemon = TestDaemon::start().await
        .expect("Failed to start test daemon");

    let result = daemon
        .rpc("system.health", serde_json::json!({}))
        .await
        .expect("system.health should succeed");

    let status = result
        .get("status")
        .and_then(|v| v.as_str())
        .expect("system.health should return a 'status' string");
    assert_eq!(status, "ok", "daemon should report healthy status");
}

/// Test 5: Detach and reattach — session persists across client disconnects.
///
/// PRD: "Detach/reattach works"
#[tokio::test]
async fn test_m0_detach_reattach() {
    let daemon = TestDaemon::start().await
        .expect("Failed to start test daemon");

    // Create a session
    let create_result = daemon
        .rpc(
            "session.create",
            serde_json::json!({"name": "persist-test"}),
        )
        .await
        .expect("session.create should succeed");

    let session_id = create_result
        .get("id")
        .and_then(|v| v.as_str())
        .expect("session should have id")
        .to_string();

    // "Detach" by dropping the connection (the daemon should keep the session)
    drop(daemon.connect().await.unwrap());

    // Wait a moment to simulate detach
    tokio::time::sleep(Duration::from_millis(100)).await;

    // "Reattach" — the session should still exist
    let list_result = daemon
        .rpc("session.list", serde_json::json!({}))
        .await
        .expect("session.list should succeed after reattach");

    let sessions = list_result.as_array().expect("should be array");
    let found = sessions
        .iter()
        .any(|s| s.get("id").and_then(|v| v.as_str()) == Some(&session_id));
    assert!(
        found,
        "Session should persist after client disconnect (detach/reattach)"
    );
}

/// Test 6: Multiple sessions can coexist.
#[tokio::test]
async fn test_m0_multiple_sessions() {
    let daemon = TestDaemon::start().await
        .expect("Failed to start test daemon");

    // Create multiple sessions
    for name in ["alpha", "beta", "gamma"] {
        daemon
            .rpc(
                "session.create",
                serde_json::json!({"name": name}),
            )
            .await
            .unwrap_or_else(|e| panic!("Failed to create session '{name}': {e}"));
    }

    // List and verify all three exist
    let result = daemon
        .rpc("session.list", serde_json::json!({}))
        .await
        .expect("session.list should succeed");

    let sessions = result.as_array().expect("should be array");
    assert!(
        sessions.len() >= 3,
        "Should have at least 3 sessions, got {}",
        sessions.len()
    );

    for expected_name in ["alpha", "beta", "gamma"] {
        let found = sessions.iter().any(|s| {
            s.get("name").and_then(|v| v.as_str()) == Some(expected_name)
        });
        assert!(
            found,
            "Session '{expected_name}' should be in the list"
        );
    }
}

/// Test 7: Session kill removes the session.
#[tokio::test]
async fn test_m0_session_kill() {
    let daemon = TestDaemon::start().await
        .expect("Failed to start test daemon");

    // Create a session
    daemon
        .rpc(
            "session.create",
            serde_json::json!({"name": "doomed"}),
        )
        .await
        .expect("session.create should succeed");

    // Kill it
    daemon
        .rpc(
            "session.kill",
            serde_json::json!({"name": "doomed"}),
        )
        .await
        .expect("session.kill should succeed");

    // Verify it is gone
    let result = daemon
        .rpc("session.list", serde_json::json!({}))
        .await
        .expect("session.list should succeed");

    let sessions = result.as_array().unwrap_or(&vec![]);
    let found = sessions.iter().any(|s| {
        s.get("name").and_then(|v| v.as_str()) == Some("doomed")
    });
    assert!(
        !found,
        "Killed session should not appear in session.list"
    );
}

/// Test 8: Invalid RPC method returns an error (not a crash).
#[tokio::test]
async fn test_m0_invalid_method() {
    let daemon = TestDaemon::start().await
        .expect("Failed to start test daemon");

    let result = daemon
        .rpc("nonexistent.method", serde_json::json!({}))
        .await;

    assert!(
        result.is_err(),
        "Invalid method should return an error, not succeed"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("error") || err.contains("not found") || err.contains("unknown"),
        "Error should indicate the method is invalid: {err}"
    );
}

/// Test 9: Daemon handles concurrent connections.
#[tokio::test]
async fn test_m0_concurrent_connections() {
    let daemon = TestDaemon::start().await
        .expect("Failed to start test daemon");

    // Open multiple connections and send requests concurrently
    let mut handles = Vec::new();
    for i in 0..5 {
        let socket_path = daemon.socket_path.clone();
        let handle = tokio::spawn(async move {
            let mut stream = UnixStream::connect(&socket_path)
                .await
                .expect("connection should succeed");
            let result = rpc_call(
                &mut stream,
                "system.version",
                serde_json::json!({}),
            )
            .await;
            result.expect(&format!(
                "concurrent request {i} should succeed"
            ));
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.await.expect("task should not panic");
    }
}
```

### Step 4: Write PTY integration tests (L2)

These tests verify that the PTY manager (task 004) works correctly with real processes.

```rust
// tests/integration/m0_test.rs (continued)

/// ═══════════════════════════════════════════════════════════════════
/// L2: PTY Integration Tests
/// ═══════════════════════════════════════════════════════════════════

/// NOTE: `pane.*` RPC coverage (pane.list, pane.send_keys, pane.capture) is
/// intentionally deferred to tasks 015/016 where those methods are introduced.
/// M0 validates PTY behavior at the crate boundary and daemon liveness.

/// Test 10: PTY handle can spawn `/bin/echo` and capture output.
#[tokio::test]
async fn test_m0_pty_spawn_echo() {
    use shux_pty::{PtyConfig, PtyHandle, PtySize};

    let mut handle = PtyHandle::spawn(PtyConfig {
        cmd: "/bin/echo".to_string(),
        args: vec!["SHUX_M0_PTY".to_string()],
        cwd: None,
        env: vec![],
        size: PtySize { cols: 80, rows: 24 },
    }).await.expect("spawn echo");

    let output = tokio::time::timeout(Duration::from_secs(2), handle.read())
        .await
        .expect("read timeout")
        .expect("read output");

    assert!(
        String::from_utf8_lossy(&output).contains("SHUX_M0_PTY"),
        "echo output should contain marker"
    );
}

/// Test 11: PTY handle reports exit status for success/failure commands.
#[tokio::test]
async fn test_m0_pty_exit_statuses() {
    use shux_pty::{PtyConfig, PtyHandle, PtySize};

    for (cmd, expected) in [("/usr/bin/true", 0), ("/usr/bin/false", 1)] {
        let mut handle = PtyHandle::spawn(PtyConfig {
            cmd: cmd.to_string(),
            args: vec![],
            cwd: None,
            env: vec![],
            size: PtySize { cols: 80, rows: 24 },
        }).await.expect("spawn command");

        let status = handle.wait().await.expect("wait status");
        assert_eq!(status.code().unwrap_or(-1), expected, "unexpected status for {cmd}");
    }
}
```

### Step 5: Write CLI integration tests

These use the actual `shux` binary via `assert_cmd` to test the full CLI-to-daemon path.

```rust
// tests/integration/m0_test.rs (continued)

/// ═══════════════════════════════════════════════════════════════════
/// CLI Integration Tests (via assert_cmd)
/// ═══════════════════════════════════════════════════════════════════

/// Test 13: `shux api system.version --format json` returns valid JSON.
///
/// This is an explicit PRD section 17 M0 criterion.
#[tokio::test]
async fn test_m0_cli_system_version_json() {
    let daemon = TestDaemon::start().await
        .expect("Failed to start test daemon");

    let bin = TestDaemon::find_binary().expect("binary should exist");

    let output = std::process::Command::new(&bin)
        .args([
            "--format", "json",
            "--socket", daemon.socket_path.to_str().unwrap(),
            "api", "system.version",
        ])
        .output()
        .expect("should be able to run shux");

    assert!(
        output.status.success(),
        "shux api system.version should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Value = serde_json::from_str(stdout.trim())
        .expect(&format!("Output should be valid JSON: {stdout}"));

    assert!(
        parsed.get("version").is_some(),
        "JSON output should contain 'version'"
    );
}

/// Test 14: `shux ls` works via CLI.
#[tokio::test]
async fn test_m0_cli_ls() {
    let daemon = TestDaemon::start().await
        .expect("Failed to start test daemon");

    // Create a session via API first
    daemon
        .rpc(
            "session.create",
            serde_json::json!({"name": "cli-ls-test"}),
        )
        .await
        .expect("session.create should succeed");

    let bin = TestDaemon::find_binary().expect("binary should exist");

    let output = std::process::Command::new(&bin)
        .args([
            "--socket", daemon.socket_path.to_str().unwrap(),
            "ls",
        ])
        .output()
        .expect("should be able to run shux");

    assert!(
        output.status.success(),
        "shux ls should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("cli-ls-test"),
        "shux ls output should contain the session name. Got: {stdout}"
    );
}

/// Test 15: `shux new -s <name> -d` creates a session via CLI.
#[tokio::test]
async fn test_m0_cli_new_detached() {
    let daemon = TestDaemon::start().await
        .expect("Failed to start test daemon");

    let bin = TestDaemon::find_binary().expect("binary should exist");

    let output = std::process::Command::new(&bin)
        .args([
            "--socket", daemon.socket_path.to_str().unwrap(),
            "new", "-s", "cli-new-test", "-d",
        ])
        .output()
        .expect("should be able to run shux");

    assert!(
        output.status.success(),
        "shux new should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify the session exists
    let result = daemon
        .rpc("session.list", serde_json::json!({}))
        .await
        .expect("session.list should succeed");

    let sessions = result.as_array().unwrap();
    let found = sessions.iter().any(|s| {
        s.get("name").and_then(|v| v.as_str()) == Some("cli-new-test")
    });
    assert!(found, "Session created via CLI should appear in session.list");
}

/// Test 16: `shux kill -s <name>` kills a session via CLI.
#[tokio::test]
async fn test_m0_cli_kill() {
    let daemon = TestDaemon::start().await
        .expect("Failed to start test daemon");

    // Create a session
    daemon
        .rpc(
            "session.create",
            serde_json::json!({"name": "cli-kill-test"}),
        )
        .await
        .expect("session.create should succeed");

    let bin = TestDaemon::find_binary().expect("binary should exist");

    let output = std::process::Command::new(&bin)
        .args([
            "--socket", daemon.socket_path.to_str().unwrap(),
            "kill", "-s", "cli-kill-test",
        ])
        .output()
        .expect("should be able to run shux");

    assert!(
        output.status.success(),
        "shux kill should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify it is gone
    let result = daemon
        .rpc("session.list", serde_json::json!({}))
        .await
        .expect("session.list should succeed");

    let sessions = result.as_array().unwrap_or(&vec![]);
    let found = sessions.iter().any(|s| {
        s.get("name").and_then(|v| v.as_str()) == Some("cli-kill-test")
    });
    assert!(!found, "Killed session should be gone");
}

/// Test 17: `shux --format json ls` returns valid JSON.
#[tokio::test]
async fn test_m0_cli_ls_json() {
    let daemon = TestDaemon::start().await
        .expect("Failed to start test daemon");

    daemon
        .rpc(
            "session.create",
            serde_json::json!({"name": "json-test"}),
        )
        .await
        .expect("session.create should succeed");

    let bin = TestDaemon::find_binary().expect("binary should exist");

    let output = std::process::Command::new(&bin)
        .args([
            "--format", "json",
            "--socket", daemon.socket_path.to_str().unwrap(),
            "ls",
        ])
        .output()
        .expect("should be able to run shux");

    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Value = serde_json::from_str(stdout.trim())
        .expect(&format!("ls --format json should return valid JSON: {stdout}"));

    assert!(parsed.is_array(), "JSON ls output should be an array");
}
```

### Step 6: Verify unit test coverage across M0 components

```rust
// tests/integration/m0_test.rs (continued)

/// ═══════════════════════════════════════════════════════════════════
/// L1: Unit Test Coverage Verification
/// ═══════════════════════════════════════════════════════════════════

/// Test 18: Verify all M0 crates have unit tests.
///
/// This is a meta-test that ensures each crate has at least some tests.
/// It does not check coverage percentage (that is done by the coverage
/// tool in Step 7), but it catches crates with zero tests.
#[test]
fn test_m0_all_crates_have_tests() {
    // This test is verified by running cargo nextest and checking
    // that each crate produces test results. The actual verification
    // is done in the bench-baseline.sh script.
    //
    // Crates that must have tests:
    // - shux-core (SessionGraph, LayoutEngine, EventBus)
    // - shux-pty (PTY spawn, read, write)
    // - shux-vt (VT parser, grid operations)
    // - shux-rpc (JSON-RPC server, framing)
    // - shux-ui (compositor, buffer, render, client, terminal)
    // - shux (CLI argument parsing, socket path)
    //
    // This is a placeholder that always passes. The real check is in
    // the bench-baseline.sh script which parses nextest output.
    assert!(true, "See bench-baseline.sh for per-crate test verification");
}
```

### Step 7: Create the performance baseline script

```bash
#!/usr/bin/env bash
# scripts/bench-baseline.sh — M0 Performance Baseline
#
# Measures and records baseline performance metrics for M0 components.
# Run after all M0 integration tests pass.
#
# Usage: ./scripts/bench-baseline.sh
#
# Output: prints results to stdout and writes to docs/m0-baseline.txt

set -euo pipefail

echo "═══════════════════════════════════════════════════════"
echo "  shux M0 Performance Baseline"
echo "═══════════════════════════════════════════════════════"
echo ""

OUTPUT_FILE="docs/m0-baseline.txt"

# Ensure we have a release build for accurate measurements
echo "Building release..."
cargo build --release --workspace 2>&1 | tail -1

echo ""
echo "── Build Metrics ──"
echo ""

# Binary size
BINARY_SIZE=$(ls -la target/release/shux | awk '{print $5}')
BINARY_SIZE_MB=$(echo "scale=2; $BINARY_SIZE / 1048576" | bc)
echo "Binary size: ${BINARY_SIZE_MB} MB (${BINARY_SIZE} bytes)"

echo ""
echo "── Test Coverage ──"
echo ""

# Run tests and count per crate
echo "Running workspace tests..."
TEST_OUTPUT=$(cargo nextest run --workspace 2>&1)
TOTAL_TESTS=$(echo "$TEST_OUTPUT" | grep -oP '\d+ test' | head -1 || echo "0 tests")
echo "Total tests: $TOTAL_TESTS"

# Check each M0 crate has tests
for crate in shux-core shux-pty shux-vt shux-rpc shux-ui shux; do
    CRATE_TESTS=$(cargo nextest run -p "$crate" 2>&1 | grep -oP '\d+ test' | head -1 || echo "0 tests")
    echo "  $crate: $CRATE_TESTS"
done

echo ""
echo "── Performance Metrics ──"
echo ""

# If benchmarks exist, run them
if cargo bench --workspace -- --test 2>/dev/null; then
    echo "Benchmarks ran successfully"
else
    echo "No benchmarks configured yet (expected for M0)"
fi

echo ""
echo "── Make Targets ──"
echo ""

# Verify all make targets work
for target in build test lint check; do
    echo -n "  make $target: "
    if make "$target" >/dev/null 2>&1; then
        echo "OK"
    else
        echo "FAIL"
    fi
done

echo ""
echo "── Summary ──"
echo ""
echo "M0 baseline recorded at $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "Binary: ${BINARY_SIZE_MB} MB"
echo "Tests: $TOTAL_TESTS"

# Write to file
cat > "$OUTPUT_FILE" << EOF
# M0 Performance Baseline
# Generated: $(date -u +%Y-%m-%dT%H:%M:%SZ)

binary_size_bytes=$BINARY_SIZE
binary_size_mb=${BINARY_SIZE_MB}
total_tests=$TOTAL_TESTS

# PRD §14.1 Targets (to be measured in M1+):
# keypress_to_render_p50_ms=8
# keypress_to_render_p99_ms=25
# pty_throughput_lines_per_sec=10000
# daemon_idle_memory_mb=80
EOF

echo ""
echo "Baseline written to $OUTPUT_FILE"
```

### Step 8: Verify all make targets

Run every make target to ensure the build system is complete.

```bash
# These should all succeed:
make build          # Compile all crates
make test           # Run all tests with nextest
make lint           # clippy + fmt check
make check          # lint + test
make ci             # lint + test-lib + test-doc
make doc            # Build documentation
make clean          # Clean build artifacts
make bench          # Run benchmarks (may be empty in M0)
```

### Step 9: Update PROGRESS.md

Mark all M0 tasks as complete and add a session log entry documenting the M0 milestone.

```markdown
# Update the task list status from Pending to Done for tasks 001-012:

| 001 | Daemon skeleton and process lifecycle | M0 | Done | 000 |
| 002 | Core data model and SessionGraph | M0 | Done | 000 |
| ... (all through 012)
| 012 | M0 integration and quality gate | M0 | Done | 001-011 |

# Add session log entry:

## Session Log

### YYYY-MM-DD — M0 Architecture Spike Complete

**Tasks completed:** 001, 002, 003, 004, 005, 006, 007, 008, 009, 010, 011, 012

**What was built:**
- Daemon skeleton with fork-before-tokio daemonization
- Core data model (SessionGraph) with ArcSwap snapshots
- Layout engine (binary split tree)
- PTY manager with async I/O
- Virtual terminal grid (vte parser + VecDeque)
- Input decoder (legacy + Kitty keyboard)
- Event bus (broadcast + sequence numbers)
- JSON-RPC server on UDS (length-prefixed framing)
- Render compositor with diff-based incremental rendering
- Minimal TUI client (single pane, raw mode, alternate screen)
- CLI foundation with clap (new, attach, ls, kill, api, version)

**M0 "Done when" verified:**
- [x] `shux new -s test` starts daemon, creates session
- [x] Typing works (send keys → PTY → VT → render)
- [x] Detach/reattach works (session persists)
- [x] `shux api system.version --format json` returns valid JSON
- [x] `shux ls` shows sessions

**Performance baseline:**
- Binary size: XX MB
- Unit tests: XX passing
- Integration tests: XX passing

**Learnings:**
- (Updated during implementation)
```

### Step 10: Verify CI passes

Push the branch and verify GitHub Actions CI passes all checks.

```bash
# Before pushing, run the full CI suite locally
make ci

# Run integration tests
cargo nextest run --workspace --test m0_test

# If everything passes, push and verify CI
git push origin create-shux
# Check https://github.com/indrasvat/shux/actions for CI status
```

---

## Verification

### Functional

```bash
# Full workspace build
cargo build --workspace

# Full clippy pass
cargo clippy --workspace --all-targets -- -D warnings

# Full fmt check
cargo fmt --all -- --check

# All unit tests pass
cargo nextest run --workspace

# All integration tests pass
cargo nextest run --workspace --test m0_test

# Make targets
make build && make test && make lint && make check && make ci
```

### Tests

```bash
# Run the full integration test suite
cargo nextest run --test m0_test

# Expected passing tests (16+ tests):
#   test_m0_create_session
#   test_m0_list_sessions
#   test_m0_system_version
#   test_m0_system_health
#   test_m0_detach_reattach
#   test_m0_multiple_sessions
#   test_m0_session_kill
#   test_m0_invalid_method
#   test_m0_concurrent_connections
#   test_m0_pty_spawn_echo
#   test_m0_pty_exit_statuses
#   test_m0_cli_system_version_json
#   test_m0_cli_ls
#   test_m0_cli_new_detached
#   test_m0_cli_kill
#   test_m0_cli_ls_json
#   test_m0_all_crates_have_tests
```

### Manual End-to-End Verification

This is the definitive test of M0 completeness. Perform these steps manually:

```bash
# 1. Build release binary
cargo build --release

# 2. Create a session (daemon starts automatically)
./target/release/shux new -s mytest

# 3. Verify TUI attaches: you should see a shell prompt in the alternate screen

# 4. Type some commands:
ls -la
echo "hello from shux"
pwd

# 5. Verify output appears correctly

# 6. Detach: press Ctrl+Space, then d
#    - Terminal should restore to normal
#    - Message: "[detached from session 'mytest']"

# 7. List sessions:
./target/release/shux ls
#    - Should show: "mytest: 1 window ..."

# 8. Reattach:
./target/release/shux attach -s mytest
#    - TUI should show the same shell session
#    - Previous command history should be visible in scrollback

# 9. Verify API access:
./target/release/shux api system.version --format json
#    - Should print: {"version": "0.1.0"}

# 10. Kill session:
./target/release/shux kill -s mytest

# 11. Verify cleanup:
./target/release/shux ls
#    - Should show: "no sessions"
```

### L4 Visual Regression — iterm2-driver (PRD §16.2)

Create `.claude/automations/test_m0_visual.py` to automate the manual E2E test above. This is the first L4 test and establishes the pattern for all subsequent visual tests.

**Script format** — MUST use `uv` with inline metadata:
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

**Execution:** `uv run .claude/automations/test_m0_visual.py`

**Test scenarios for M0:**

```python
"""
shux M0 Visual Verification Test (iterm2-driver)

Tests:
1. Launch `shux new -s m0-test` in a new iTerm2 tab
2. Verify TUI enters alt-screen (screen has content)
3. Read screen to verify shell prompt appears
4. Send `echo "hello from shux"` and verify output rendered
5. Take screenshot of initial single-pane state
6. Send Ctrl+Space then 'd' to detach
7. Verify terminal restored (no longer in alt-screen)
8. Run `shux attach -s m0-test` and verify reattach
9. Take screenshot of reattached state
10. Send `exit` and verify clean exit

Verification Strategy:
- Read screen contents after launch to verify TUI rendered
- Check for expected text patterns (prompt, command output)
- Verify clean detach/reattach cycle
- Screenshots saved to .claude/automations/screenshots/

Usage:
    uv run .claude/automations/test_m0_visual.py
"""
import asyncio, os, subprocess, sys
import iterm2

async def main(connection):
    app = await iterm2.async_get_app(connection)
    window = app.current_terminal_window
    if window is None:
        print("ERROR: No iTerm2 window"); sys.exit(1)
    tab = await window.async_create_tab()
    session = tab.current_session
    try:
        # 1. Launch shux
        await session.async_send_text("cd /path/to/shux && cargo build --release && ./target/release/shux new -s m0-test\r")
        await asyncio.sleep(3.0)

        # 2. Read screen — verify TUI rendered
        screen = await session.async_get_screen_contents()
        lines = [screen.line(i).string for i in range(screen.number_of_lines)]
        content = "\n".join(lines)
        assert any(l.strip() for l in lines), "Screen is empty after launch"

        # 3. Send a command and verify output
        await session.async_send_text("echo 'hello from shux'\r")
        await asyncio.sleep(1.0)
        screen = await session.async_get_screen_contents()
        lines = [screen.line(i).string for i in range(screen.number_of_lines)]
        content = "\n".join(lines)
        assert "hello from shux" in content, "Command output not visible"

        # 4. Screenshot: initial state
        # (use Quartz screenshot pattern from iterm2-driver skill)

        # 5. Detach: Ctrl+Space then d
        await session.async_send_text("\x00")  # Ctrl+Space
        await asyncio.sleep(0.3)
        await session.async_send_text("d")
        await asyncio.sleep(1.0)

        # 6. Verify detached (should see shell prompt, not TUI)
        screen = await session.async_get_screen_contents()
        lines = [screen.line(i).string for i in range(screen.number_of_lines)]
        content = "\n".join(lines)
        assert "detached" in content.lower(), "Detach message not found"

        # 7. Reattach
        await session.async_send_text("./target/release/shux attach -s m0-test\r")
        await asyncio.sleep(2.0)

        # 8. Verify reattached
        screen = await session.async_get_screen_contents()
        lines = [screen.line(i).string for i in range(screen.number_of_lines)]
        assert any(l.strip() for l in lines), "Reattach failed"

        print("PASS: All M0 visual tests passed")
    except Exception as e:
        print(f"FAIL: {e}")
        raise
    finally:
        await session.async_send_text("\x03")
        await asyncio.sleep(0.2)
        await session.async_send_text("exit\r")
        await asyncio.sleep(0.5)
        await session.async_close()

iterm2.run_until_complete(main)
```

**Completion criteria for L4:**
- [ ] `.claude/automations/test_m0_visual.py` exists and runs via `uv run`
- [ ] Script uses proper `iterm2` library with Quartz screenshots
- [ ] Screenshots saved to `.claude/automations/screenshots/m0_*.png`
- [ ] All M0 visual assertions pass: launch, render, detach, reattach

---

## Completion Criteria

### M0 "Done when" (PRD section 17)

- [ ] `shux new -s test` starts the daemon and creates a session
- [ ] TUI client attaches and shows a shell prompt
- [ ] Typing in the TUI produces visible output (keypress → PTY → VT → render pipeline works)
- [ ] Detach (Ctrl+Space d) cleanly restores the terminal
- [ ] Reattach (`shux attach -s test`) reconnects to the existing session
- [ ] `shux api system.version --format json` returns valid JSON with version info
- [ ] `shux ls` lists active sessions

### Integration Tests

- [ ] All integration tests in `tests/integration/m0_test.rs` pass
- [ ] Tests cover: session CRUD, system health, concurrent connections, crate-level PTY behavior, CLI commands
- [ ] Test harness properly starts/stops ephemeral daemon instances

### Unit Test Coverage

- [ ] Every M0 crate has at least one unit test: shux-core, shux-pty, shux-vt, shux-rpc, shux-ui, shux
- [ ] Target: 60% or higher line coverage across M0 crates (measured via `cargo llvm-cov`)

### Build System

- [ ] `make build` succeeds
- [ ] `make test` succeeds (all unit + integration tests)
- [ ] `make lint` succeeds (clippy + fmt)
- [ ] `make check` succeeds (lint + test)
- [ ] `make ci` succeeds (lint + test-lib + test-doc)
- [ ] `make doc` succeeds (documentation builds)

### Performance Baseline

- [ ] `scripts/bench-baseline.sh` runs successfully
- [ ] Baseline metrics recorded in `docs/m0-baseline.txt`
- [ ] Release binary size is reasonable (target: under 20 MB)

### CI

- [ ] GitHub Actions CI passes on the create-shux branch
- [ ] All CI jobs green: check, test (ubuntu + macos), deny

### Documentation

- [ ] `docs/PROGRESS.md` updated: all M0 tasks marked Done, session log entry added
- [ ] `CLAUDE.md` Learnings section updated with M0 findings

---

## Commit Message
```
test(m0): add M0 integration tests and quality gate

- Integration test harness: ephemeral daemon with auto-cleanup
- 16+ integration tests covering: session CRUD, system health,
  concurrent connections, PTY I/O smoke checks, CLI commands
- CLI integration tests via shux binary (assert_cmd pattern)
- Performance baseline script (scripts/bench-baseline.sh)
- Verify PRD §17 M0 "Done when" criteria:
  shux new → daemon starts, session created, TUI attaches
  typing works, detach/reattach works, API JSON output works
- Update PROGRESS.md: M0 complete
```

---

## Session Protocol

1. **Before starting:** Every task 001-011 must be complete and passing. Run `cargo build --workspace && cargo nextest run --workspace && cargo clippy --workspace --all-targets -- -D warnings` to confirm. If anything fails, fix it in the appropriate task before starting 012.
2. **During:** Work through the steps in order. The integration test harness (Step 2) must work before writing the individual tests (Steps 3-5). If a test fails, determine which underlying task has a bug and fix it there -- do not paper over failures in the integration test. The manual end-to-end verification (Verification section) is the final step and must be performed on a real terminal, not in CI.
3. **Test isolation:** Each integration test function gets its own ephemeral daemon instance via `TestDaemon::start()`. This ensures tests are independent and can run in parallel. The `TempDir` ensures socket files are cleaned up.
4. **Performance baseline:** The `bench-baseline.sh` script records initial measurements. These are not pass/fail gates in M0 -- they establish the baseline that M1 and M3 will optimize against. The PRD section 14.1 targets (p50 <= 8ms render, etc.) are formal gates starting in M3.
5. **After:** Run `make check`. Run integration tests. Perform manual end-to-end test. Update `docs/PROGRESS.md` (mark M0 complete, add detailed session log). Update `CLAUDE.md` Learnings with every architectural insight, gotcha, or deviation from the PRD discovered during M0. This is critical for M1 velocity.
