//! CLI integration tests that spin up a real RPC server and test the full
//! CLI→UDS→RPC→response pipeline.

use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use tokio::net::UnixStream;
use tokio_util::codec::Framed;

/// Start a real RPC server on an ephemeral UDS socket.
/// Returns (socket_path, cancel_token) — cancel to shut down.
async fn start_test_server(
    dir: &std::path::Path,
) -> (PathBuf, tokio_util::sync::CancellationToken) {
    let socket_path = dir.join("test.sock");
    let cancel = tokio_util::sync::CancellationToken::new();

    let router = shux_rpc::server::register_builtin_methods(shux_rpc::Router::builder()).build();
    let config = shux_rpc::ServerConfig {
        socket_path: socket_path.clone(),
        tcp_addr: String::new(),
        auth_token: None,
    };

    let server = shux_rpc::Server::new(config, router, cancel.clone());

    tokio::spawn(async move {
        let _ = server.run().await;
    });

    // Wait for server to be ready
    for _ in 0..20 {
        if UnixStream::connect(&socket_path).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    (socket_path, cancel)
}

/// Helper: send a raw JSON-RPC request and get response via the framed codec.
async fn rpc_raw(
    socket_path: &std::path::Path,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    let stream = UnixStream::connect(socket_path).await.unwrap();
    let mut framed = Framed::new(stream, shux_rpc::create_codec());

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": uuid::Uuid::new_v4().to_string(),
        "method": method,
        "params": params,
    });
    let payload = serde_json::to_vec(&request).unwrap();
    framed.send(Bytes::from(payload)).await.unwrap();

    let response_frame = framed.next().await.unwrap().unwrap();
    serde_json::from_slice(&response_frame).unwrap()
}

// ══════════════════════════════════════════════════════════════
// In-process RPC tests (fast, single-threaded)
// ══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_rpc_system_version() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let response = rpc_raw(&socket_path, "system.version", serde_json::json!({})).await;

    assert_eq!(response["jsonrpc"], "2.0");
    assert!(response["result"]["version"].is_string());
    assert_eq!(response["result"]["name"], "shux");

    cancel.cancel();
}

#[tokio::test]
async fn test_rpc_system_health() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let response = rpc_raw(&socket_path, "system.health", serde_json::json!({})).await;

    assert_eq!(response["result"]["status"], "ok");

    cancel.cancel();
}

#[tokio::test]
async fn test_rpc_session_list() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let response = rpc_raw(&socket_path, "session.list", serde_json::json!({})).await;

    assert!(response["result"]["sessions"].is_array());

    cancel.cancel();
}

#[tokio::test]
async fn test_rpc_unknown_method_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let response = rpc_raw(&socket_path, "nonexistent.method", serde_json::json!({})).await;

    assert!(response["error"].is_object());
    assert_eq!(response["error"]["code"], -32601);

    cancel.cancel();
}

#[tokio::test]
async fn test_rpc_concurrent_connections() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let mut handles = Vec::new();
    for _ in 0..5 {
        let path = socket_path.clone();
        handles.push(tokio::spawn(async move {
            let response = rpc_raw(&path, "system.version", serde_json::json!({})).await;
            assert!(response["result"]["version"].is_string());
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    cancel.cancel();
}

// ══════════════════════════════════════════════════════════════
// CLI binary tests against a real RPC server.
//
// These use `tokio::process::Command` (async) so the server task
// can continue processing while we await the child process.
// ══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_cli_version_against_server() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shux"))
        .args(["--socket", socket_path.to_str().unwrap(), "version"])
        .output()
        .await
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "shux version failed. stdout={stdout}, stderr={stderr}"
    );
    assert!(
        stdout.contains("shux"),
        "expected 'shux' in output: {stdout}"
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_cli_version_json_against_server() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shux"))
        .args([
            "--format",
            "json",
            "--socket",
            socket_path.to_str().unwrap(),
            "version",
        ])
        .output()
        .await
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(parsed["version"].is_string());
    assert_eq!(parsed["name"], "shux");

    cancel.cancel();
}

#[tokio::test]
async fn test_cli_ls_against_server() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shux"))
        .args(["--socket", socket_path.to_str().unwrap(), "ls"])
        .output()
        .await
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("no sessions"),
        "expected 'no sessions' in output: {stdout}"
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_cli_ls_json_against_server() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shux"))
        .args([
            "--format",
            "json",
            "--socket",
            socket_path.to_str().unwrap(),
            "ls",
        ])
        .output()
        .await
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(
        parsed["sessions"].is_array(),
        "expected sessions array in JSON output: {stdout}"
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_cli_api_raw_against_server() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shux"))
        .args([
            "--socket",
            socket_path.to_str().unwrap(),
            "api",
            "system.health",
        ])
        .output()
        .await
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(parsed["status"], "ok");

    cancel.cancel();
}

// ══════════════════════════════════════════════════════════════
// Smoke tests (no daemon needed) — use binary path from env
// ══════════════════════════════════════════════════════════════

fn shux_bin() -> std::process::Command {
    std::process::Command::new(env!("CARGO_BIN_EXE_shux"))
}

#[test]
fn test_cli_help_no_daemon() {
    let output = shux_bin().arg("--help").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(stdout.contains("terminal multiplexer"));
    assert!(stdout.contains("new"));
    assert!(stdout.contains("attach"));
    assert!(stdout.contains("ls"));
    assert!(stdout.contains("kill"));
    assert!(stdout.contains("api"));
    assert!(stdout.contains("version"));
}

#[test]
fn test_cli_version_flag() {
    let output = shux_bin().arg("--version").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(stdout.contains("shux"));
}

#[test]
fn test_cli_invalid_subcommand() {
    let output = shux_bin().arg("nonexistent").output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("unrecognized subcommand"));
}

#[test]
fn test_cli_kill_requires_session() {
    let output = shux_bin().arg("kill").output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("--session"));
}

#[test]
fn test_cli_list_alias() {
    let output = shux_bin().args(["list", "--help"]).output().unwrap();
    assert!(output.status.success());
}

#[test]
fn test_cli_version_without_daemon() {
    let output = shux_bin()
        .arg("version")
        .env("SHUX_SOCKET", "/tmp/nonexistent-shux-cli-test.sock")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(stdout.contains("shux"));
    assert!(stdout.contains("daemon not running"));
}

#[test]
fn test_cli_version_json_without_daemon() {
    let output = shux_bin()
        .args(["--format", "json", "version"])
        .env("SHUX_SOCKET", "/tmp/nonexistent-shux-cli-test.sock")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(stdout.contains("version"));
}
