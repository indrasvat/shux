//! CLI integration tests that spin up a real RPC server with a SessionGraph
//! and test the full CLI→UDS→RPC→SessionGraph→response pipeline.

use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use tokio::net::UnixStream;
use tokio_util::codec::Framed;

/// Map GraphError to appropriate RPC error codes (duplicated from main.rs).
fn graph_error_to_rpc(e: shux_core::graph::GraphError) -> shux_rpc::RpcError {
    use shux_core::graph::GraphError;
    match e {
        GraphError::SessionNotFound(_) => shux_rpc::RpcError::not_found("session", &e.to_string()),
        GraphError::WindowNotFound(_) => shux_rpc::RpcError::not_found("window", &e.to_string()),
        GraphError::PaneNotFound(_) => shux_rpc::RpcError::not_found("pane", &e.to_string()),
        GraphError::SessionNameExists(ref name) => {
            shux_rpc::RpcError::name_conflict("session", name)
        }
        GraphError::WindowNameConflict(ref name) => {
            shux_rpc::RpcError::name_conflict("window", name)
        }
        GraphError::EmptySessionName
        | GraphError::SessionNameTooLong(_)
        | GraphError::InvalidSessionName(_) => shux_rpc::RpcError::invalid_params(&e.to_string()),
        GraphError::EmptyWindowName | GraphError::WindowIndexOutOfRange { .. } => {
            shux_rpc::RpcError::invalid_params(&e.to_string())
        }
        GraphError::LastWindow | GraphError::LastPane => {
            shux_rpc::RpcError::invalid_params(&e.to_string())
        }
        GraphError::PaneSwapSelf | GraphError::PaneCrossWindow | GraphError::NoNeighbor(_) => {
            shux_rpc::RpcError::invalid_params(&e.to_string())
        }
        GraphError::LayoutError(_) => shux_rpc::RpcError::internal(&e.to_string()),
        GraphError::VersionConflict {
            resource,
            ref id,
            expected,
            actual,
        } => shux_rpc::RpcError::version_conflict(resource, id, expected, actual),
        GraphError::Shutdown => shux_rpc::RpcError::internal(&e.to_string()),
    }
}

/// Build session info JSON from a Session.
fn session_to_json(
    s: &shux_core::model::Session,
    snap: &shux_core::graph::SessionGraphSnapshot,
) -> serde_json::Value {
    let first_window_id = s.windows.first().map(|w| w.to_string());
    let first_pane_id = s
        .windows
        .first()
        .and_then(|wid| snap.windows.get(wid).map(|w| w.active_pane.to_string()));
    serde_json::json!({
        "id": s.id.to_string(),
        "name": s.name,
        "windows": s.windows.iter().map(|w| w.to_string()).collect::<Vec<_>>(),
        "window_count": s.windows.len(),
        "active_window_id": s.active_window.to_string(),
        "window_id": first_window_id,
        "pane_id": first_pane_id,
        "created_at": s.created_at.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
    })
}

/// Register session CRUD methods backed by a real GraphHandle.
fn register_session_methods(
    builder: shux_rpc::RouterBuilder,
    graph: shux_core::graph::GraphHandle,
) -> shux_rpc::RouterBuilder {
    let g1 = graph.clone();
    let g2 = graph.clone();
    let g3 = graph.clone();
    let g4 = graph.clone();
    let g5 = graph.clone();

    builder
        .register("session.list", move |_params: Option<serde_json::Value>| {
            let gh = g1.clone();
            async move {
                let snap = gh.snapshot();
                let mut sessions: Vec<_> = snap.sessions.values().collect();
                sessions.sort_by_key(|s| s.created_at);
                let sessions: Vec<serde_json::Value> =
                    sessions.iter().map(|s| session_to_json(s, &snap)).collect();
                Ok(serde_json::json!({ "sessions": sessions }))
            }
        })
        .register(
            "session.create",
            move |params: Option<serde_json::Value>| {
                let gh = g2.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let name = params
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let name = match name {
                        Some(n) => n,
                        None => {
                            let snap = gh.snapshot();
                            let mut idx = snap.sessions.len();
                            loop {
                                let candidate = format!("session-{idx}");
                                if !snap.session_name_exists(&candidate) {
                                    break candidate;
                                }
                                idx += 1;
                            }
                        }
                    };
                    let cwd = PathBuf::from("/tmp");
                    match gh.create_session(name, cwd).await {
                        Ok(session_id) => {
                            let snap = gh.snapshot();
                            if let Some(s) = snap.sessions.get(&session_id) {
                                Ok(session_to_json(s, &snap))
                            } else {
                                Ok(serde_json::json!({ "id": session_id.to_string() }))
                            }
                        }
                        Err(e) => Err(graph_error_to_rpc(e)),
                    }
                }
            },
        )
        .register("session.kill", move |params: Option<serde_json::Value>| {
            let gh = g3.clone();
            async move {
                let params = params.unwrap_or_default();
                let session_id = if let Some(id_str) = params.get("id").and_then(|v| v.as_str()) {
                    let parsed: shux_core::model::SessionId = id_str.parse().map_err(|_| {
                        shux_rpc::RpcError::invalid_params("invalid session ID format")
                    })?;
                    let snap = gh.snapshot();
                    if !snap.sessions.contains_key(&parsed) {
                        return Err(shux_rpc::RpcError::not_found("session", id_str));
                    }
                    parsed
                } else if let Some(name) = params.get("name").and_then(|v| v.as_str()) {
                    let snap = gh.snapshot();
                    let session = snap
                        .find_session_by_name(name)
                        .ok_or_else(|| shux_rpc::RpcError::not_found("session", name))?;
                    session.id
                } else {
                    return Err(shux_rpc::RpcError::invalid_params(
                        "missing 'name' or 'id' parameter",
                    ));
                };
                let snap = gh.snapshot();
                let name = snap
                    .sessions
                    .get(&session_id)
                    .map(|s| s.name.clone())
                    .unwrap_or_default();
                gh.destroy_session(session_id, None)
                    .await
                    .map_err(graph_error_to_rpc)?;
                Ok(serde_json::json!({ "killed": name }))
            }
        })
        .register(
            "session.ensure",
            move |params: Option<serde_json::Value>| {
                let gh = g4.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let name = params
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("default")
                        .to_string();
                    let snap = gh.snapshot();
                    if let Some(s) = snap.find_session_by_name(&name) {
                        let mut json = session_to_json(s, &snap);
                        json["created"] = serde_json::Value::Bool(false);
                        return Ok(json);
                    }
                    let cwd = PathBuf::from("/tmp");
                    match gh.create_session(name, cwd).await {
                        Ok(session_id) => {
                            let snap = gh.snapshot();
                            if let Some(s) = snap.sessions.get(&session_id) {
                                let mut json = session_to_json(s, &snap);
                                json["created"] = serde_json::Value::Bool(true);
                                Ok(json)
                            } else {
                                Ok(serde_json::json!({
                                    "id": session_id.to_string(),
                                    "created": true,
                                }))
                            }
                        }
                        Err(e) => Err(graph_error_to_rpc(e)),
                    }
                }
            },
        )
        .register(
            "session.rename",
            move |params: Option<serde_json::Value>| {
                let gh = g5.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let new_name = params
                        .get("new_name")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'new_name' parameter")
                        })?
                        .to_string();
                    let session_id = if let Some(name) = params.get("name").and_then(|v| v.as_str())
                    {
                        let snap = gh.snapshot();
                        let session = snap
                            .find_session_by_name(name)
                            .ok_or_else(|| shux_rpc::RpcError::not_found("session", name))?;
                        session.id
                    } else if let Some(id_str) = params.get("id").and_then(|v| v.as_str()) {
                        id_str.parse().map_err(|_| {
                            shux_rpc::RpcError::invalid_params("invalid session ID format")
                        })?
                    } else {
                        return Err(shux_rpc::RpcError::invalid_params(
                            "missing 'name' or 'id' parameter",
                        ));
                    };
                    gh.rename_session(session_id, new_name, None)
                        .await
                        .map_err(graph_error_to_rpc)?;
                    let snap = gh.snapshot();
                    if let Some(s) = snap.sessions.get(&session_id) {
                        Ok(session_to_json(s, &snap))
                    } else {
                        Err(shux_rpc::RpcError::internal(
                            "session vanished after rename",
                        ))
                    }
                }
            },
        )
}

/// Start a real RPC server with a SessionGraph on an ephemeral UDS socket.
/// Returns (socket_path, cancel_token) — cancel to shut down.
async fn start_test_server(
    dir: &std::path::Path,
) -> (PathBuf, tokio_util::sync::CancellationToken) {
    let socket_path = dir.join("test.sock");
    let cancel = tokio_util::sync::CancellationToken::new();

    // Set up SessionGraph + graph loop
    let (graph, state) = shux_core::graph::SessionGraph::new();
    let (graph_tx, graph_rx) = tokio::sync::mpsc::channel(256);
    let graph_handle = shux_core::graph::GraphHandle::new(graph_tx, state);

    let graph_cancel = cancel.clone();
    tokio::spawn(async move {
        shux_core::graph::run_graph_loop(graph, graph_rx, graph_cancel).await;
    });

    // Build router: system builtins + session methods backed by GraphHandle
    let router = register_session_methods(
        shux_rpc::server::register_builtin_methods(shux_rpc::Router::builder()),
        graph_handle,
    )
    .build();

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
    // When piped, format auto-switches to Plain; empty list produces no output (Unix convention)
    assert!(
        stdout.trim().is_empty(),
        "expected empty output for no sessions in plain format: {stdout}"
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
    // PR 3b: `shux api` wraps responses in `{result: ...}` / `{error: ...}`.
    assert_eq!(parsed["result"]["status"], "ok");

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
    assert!(stdout.contains("window"));
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
fn test_cli_config_validate_clean() {
    use std::io::Write;
    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    writeln!(
        tmp,
        "[appearance]\nborder_style = \"rounded\"\n\n[keys]\nprefix = \"ctrl-space\"\n"
    )
    .unwrap();
    let output = shux_bin()
        .args([
            "config",
            "validate",
            "--config",
            tmp.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("Config valid"));
}

#[test]
fn test_cli_config_validate_outer_error() {
    use std::io::Write;
    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    // 42 is not a string — fails type check at line 2 col 16.
    writeln!(tmp, "[appearance]\nborder_style = 42").unwrap();
    let output = shux_bin()
        .args([
            "config",
            "validate",
            "--config",
            tmp.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "expected non-zero exit");
    assert!(
        stderr.contains(":2:"),
        "stderr should include line 2: {stderr}"
    );
    assert!(stderr.contains("error found"));
}

#[test]
fn test_cli_config_validate_inner_starship_error() {
    use std::io::Write;
    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    write!(
        tmp,
        r#"[[statusbar.segment]]
zone = "left"
command = ["echo", "x"]
starship_config = """
[character]
this is not valid toml
"""
"#,
    )
    .unwrap();
    let output = shux_bin()
        .args([
            "config",
            "validate",
            "--config",
            tmp.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("statusbar.segment[0].starship_config"),
        "stderr: {stderr}"
    );
}

#[test]
fn test_cli_config_validate_missing_file() {
    let output = shux_bin()
        .args([
            "config",
            "validate",
            "--config",
            "/tmp/nonexistent-shux-validate-xyz.toml",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("config file not found"));
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
