//! M0 Integration Test Suite
//!
//! Verifies PRD §17 M0 "Done when" criteria by testing the full
//! CLI→UDS→RPC→SessionGraph pipeline. Each test gets its own ephemeral
//! daemon (RPC server + SessionGraph) for isolation.

use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use tokio::net::UnixStream;
use tokio_util::codec::Framed;

// ══════════════════════════════════════════════════════════════
// Test harness
// ══════════════════════════════════════════════════════════════

/// Map GraphError to appropriate RPC error codes (duplicated from main.rs).
fn graph_error_to_rpc(e: shux_core::graph::GraphError) -> shux_rpc::RpcError {
    use shux_core::graph::GraphError;
    match e {
        GraphError::SessionNotFound(_)
        | GraphError::WindowNotFound(_)
        | GraphError::PaneNotFound(_) => shux_rpc::RpcError::not_found("session", &e.to_string()),
        GraphError::SessionNameExists(ref name) => {
            shux_rpc::RpcError::name_conflict("session", name)
        }
        GraphError::EmptySessionName
        | GraphError::SessionNameTooLong(_)
        | GraphError::InvalidSessionName(_) => shux_rpc::RpcError::invalid_params(&e.to_string()),
        GraphError::VersionConflict { expected, actual } => {
            shux_rpc::RpcError::version_conflict("session", "?", expected, actual)
        }
        _ => shux_rpc::RpcError::internal(&e.to_string()),
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
                sessions.sort_by(|a, b| a.created_at.cmp(&b.created_at));
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

/// Start a test server (RPC + SessionGraph) on an ephemeral UDS.
/// Returns (socket_path, cancel_token).
async fn start_test_server(
    dir: &std::path::Path,
) -> (PathBuf, tokio_util::sync::CancellationToken) {
    let socket_path = dir.join("m0-test.sock");
    let cancel = tokio_util::sync::CancellationToken::new();

    let (graph, state) = shux_core::graph::SessionGraph::new();
    let (graph_tx, graph_rx) = tokio::sync::mpsc::channel(256);
    let graph_handle = shux_core::graph::GraphHandle::new(graph_tx, state);

    let graph_cancel = cancel.clone();
    tokio::spawn(async move {
        shux_core::graph::run_graph_loop(graph, graph_rx, graph_cancel).await;
    });

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

    for _ in 0..20 {
        if UnixStream::connect(&socket_path).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    (socket_path, cancel)
}

/// Send a JSON-RPC request over a framed UDS connection and get the response.
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
// M0 "Done when" tests (PRD §17)
// ══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_m0_system_version() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let response = rpc_raw(&socket_path, "system.version", serde_json::json!({})).await;

    assert_eq!(response["jsonrpc"], "2.0");
    let version = response["result"]["version"].as_str().unwrap();
    assert!(!version.is_empty());
    assert!(version.contains('.'), "version should be semver: {version}");
    assert_eq!(response["result"]["name"], "shux");

    cancel.cancel();
}

#[tokio::test]
async fn test_m0_system_health() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let response = rpc_raw(&socket_path, "system.health", serde_json::json!({})).await;
    assert_eq!(response["result"]["status"], "ok");

    cancel.cancel();
}

#[tokio::test]
async fn test_m0_create_session() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let response = rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "test"}),
    )
    .await;

    assert!(
        response["result"]["id"].is_string(),
        "session.create should return id"
    );
    assert_eq!(response["result"]["name"], "test");

    cancel.cancel();
}

#[tokio::test]
async fn test_m0_list_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    // Create a session
    rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "list-test"}),
    )
    .await;

    // List sessions
    let response = rpc_raw(&socket_path, "session.list", serde_json::json!({})).await;
    let sessions = response["result"]["sessions"].as_array().unwrap();
    assert!(!sessions.is_empty(), "should have at least 1 session");

    let found = sessions
        .iter()
        .any(|s| s["name"].as_str() == Some("list-test"));
    assert!(found, "session 'list-test' should appear in list");

    cancel.cancel();
}

#[tokio::test]
async fn test_m0_session_kill() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "doomed"}),
    )
    .await;

    let kill_response = rpc_raw(
        &socket_path,
        "session.kill",
        serde_json::json!({"name": "doomed"}),
    )
    .await;
    assert!(
        kill_response["error"].is_null(),
        "kill should succeed: {kill_response}"
    );

    // Verify it is gone
    let list_response = rpc_raw(&socket_path, "session.list", serde_json::json!({})).await;
    let sessions = list_response["result"]["sessions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let found = sessions
        .iter()
        .any(|s| s["name"].as_str() == Some("doomed"));
    assert!(!found, "killed session should not appear in list");

    cancel.cancel();
}

#[tokio::test]
async fn test_m0_detach_reattach() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let create_resp = rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "persist-test"}),
    )
    .await;
    let session_id = create_resp["result"]["id"].as_str().unwrap().to_string();

    // "Detach" by dropping the connection
    {
        let _stream = UnixStream::connect(&socket_path).await.unwrap();
        // stream dropped here
    }

    tokio::time::sleep(Duration::from_millis(100)).await;

    // "Reattach" — the session should still exist
    let list_response = rpc_raw(&socket_path, "session.list", serde_json::json!({})).await;
    let sessions = list_response["result"]["sessions"].as_array().unwrap();
    let found = sessions
        .iter()
        .any(|s| s["id"].as_str() == Some(&session_id));
    assert!(found, "session should persist after client disconnect");

    cancel.cancel();
}

#[tokio::test]
async fn test_m0_multiple_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    for name in ["alpha", "beta", "gamma"] {
        rpc_raw(
            &socket_path,
            "session.create",
            serde_json::json!({"name": name}),
        )
        .await;
    }

    let response = rpc_raw(&socket_path, "session.list", serde_json::json!({})).await;
    let sessions = response["result"]["sessions"].as_array().unwrap();
    assert!(sessions.len() >= 3, "should have 3+ sessions");

    for name in ["alpha", "beta", "gamma"] {
        let found = sessions.iter().any(|s| s["name"].as_str() == Some(name));
        assert!(found, "session '{name}' should be in list");
    }

    cancel.cancel();
}

#[tokio::test]
async fn test_m0_invalid_method() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let response = rpc_raw(&socket_path, "nonexistent.method", serde_json::json!({})).await;
    assert!(response["error"].is_object(), "should return an error");
    assert_eq!(response["error"]["code"], -32601);

    cancel.cancel();
}

#[tokio::test]
async fn test_m0_concurrent_connections() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let mut handles = Vec::new();
    for i in 0..5 {
        let path = socket_path.clone();
        handles.push(tokio::spawn(async move {
            let response = rpc_raw(&path, "system.version", serde_json::json!({})).await;
            assert!(
                response["result"]["version"].is_string(),
                "concurrent request {i} should succeed"
            );
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    cancel.cancel();
}

#[tokio::test]
async fn test_m0_session_ensure() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    // First ensure: creates session
    let resp1 = rpc_raw(
        &socket_path,
        "session.ensure",
        serde_json::json!({"name": "ensure-test"}),
    )
    .await;
    assert_eq!(resp1["result"]["name"], "ensure-test");
    assert_eq!(resp1["result"]["created"], true);
    let id1 = resp1["result"]["id"].as_str().unwrap().to_string();

    // Second ensure: returns existing session
    let resp2 = rpc_raw(
        &socket_path,
        "session.ensure",
        serde_json::json!({"name": "ensure-test"}),
    )
    .await;
    assert_eq!(resp2["result"]["name"], "ensure-test");
    assert_eq!(resp2["result"]["created"], false);
    assert_eq!(resp2["result"]["id"].as_str().unwrap(), id1);

    cancel.cancel();
}

// ══════════════════════════════════════════════════════════════
// L2: PTY Integration Tests (crate-level)
// ══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_m0_pty_spawn_echo() {
    let config = shux_pty::PtyConfig::with_command(
        vec!["echo".into(), "SHUX_M0_PTY".into()],
        PathBuf::from("/tmp"),
    );

    let mut handle = shux_pty::PtyHandle::spawn(&config).unwrap();

    let mut output = Vec::new();
    let mut buf = [0u8; 4096];

    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match handle.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => output.extend_from_slice(&buf[..n]),
                Err(_) => break,
            }
        }
    })
    .await;

    assert!(
        String::from_utf8_lossy(&output).contains("SHUX_M0_PTY"),
        "echo output should contain marker"
    );
}

#[tokio::test]
async fn test_m0_pty_exit_status() {
    for (cmd, expected_success) in [("true", true), ("false", false)] {
        let config = shux_pty::PtyConfig::with_command(vec![cmd.into()], PathBuf::from("/tmp"));
        let mut handle = shux_pty::PtyHandle::spawn(&config).unwrap();
        let status = handle.wait().await.unwrap();
        assert_eq!(
            status.success(),
            expected_success,
            "unexpected exit status for {cmd}"
        );
    }
}

// ══════════════════════════════════════════════════════════════
// CLI binary tests (uses the compiled shux binary)
// ══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_m0_cli_version_json() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shux"))
        .args([
            "--format",
            "json",
            "--socket",
            socket_path.to_str().unwrap(),
            "api",
            "system.version",
        ])
        .output()
        .await
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "shux api system.version should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("output should be valid JSON: {stdout}"));
    assert!(parsed["version"].is_string());

    cancel.cancel();
}

#[tokio::test]
async fn test_m0_cli_ls() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    // Create a session via RPC first
    rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "cli-ls-test"}),
    )
    .await;

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
        stdout.contains("cli-ls-test"),
        "ls output should contain session name. Got: {stdout}"
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_m0_cli_new_detached() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shux"))
        .args([
            "--socket",
            socket_path.to_str().unwrap(),
            "new",
            "-s",
            "cli-new-test",
            "-d",
        ])
        .output()
        .await
        .unwrap();

    assert!(
        output.status.success(),
        "shux new should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify session exists via RPC
    let response = rpc_raw(&socket_path, "session.list", serde_json::json!({})).await;
    let sessions = response["result"]["sessions"].as_array().unwrap();
    let found = sessions
        .iter()
        .any(|s| s["name"].as_str() == Some("cli-new-test"));
    assert!(found, "session created via CLI should appear in list");

    cancel.cancel();
}

#[tokio::test]
async fn test_m0_cli_kill() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    // Create a session via RPC
    rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "cli-kill-test"}),
    )
    .await;

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shux"))
        .args([
            "--socket",
            socket_path.to_str().unwrap(),
            "kill",
            "-s",
            "cli-kill-test",
        ])
        .output()
        .await
        .unwrap();

    assert!(
        output.status.success(),
        "shux kill should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify session is gone
    let response = rpc_raw(&socket_path, "session.list", serde_json::json!({})).await;
    let sessions = response["result"]["sessions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let found = sessions
        .iter()
        .any(|s| s["name"].as_str() == Some("cli-kill-test"));
    assert!(!found, "killed session should be gone");

    cancel.cancel();
}

#[tokio::test]
async fn test_m0_cli_ls_json() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "json-test"}),
    )
    .await;

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

    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("ls --format json should return valid JSON: {stdout}"));
    assert!(
        parsed["sessions"].is_array(),
        "JSON ls output should contain sessions array"
    );

    cancel.cancel();
}

// ══════════════════════════════════════════════════════════════
// Task 013: Session CRUD tests
// ══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_013_session_rename() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let create_resp = rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "old-name"}),
    )
    .await;
    assert!(create_resp["error"].is_null(), "create should succeed");

    // Rename by name
    let rename_resp = rpc_raw(
        &socket_path,
        "session.rename",
        serde_json::json!({"name": "old-name", "new_name": "new-name"}),
    )
    .await;
    assert!(
        rename_resp["error"].is_null(),
        "rename should succeed: {rename_resp}"
    );
    assert_eq!(rename_resp["result"]["name"], "new-name");

    // Verify old name is gone
    let list_resp = rpc_raw(&socket_path, "session.list", serde_json::json!({})).await;
    let sessions = list_resp["result"]["sessions"].as_array().unwrap();
    let names: Vec<&str> = sessions.iter().filter_map(|s| s["name"].as_str()).collect();
    assert!(names.contains(&"new-name"));
    assert!(!names.contains(&"old-name"));

    cancel.cancel();
}

#[tokio::test]
async fn test_013_session_rename_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "first"}),
    )
    .await;
    rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "second"}),
    )
    .await;

    let rename_resp = rpc_raw(
        &socket_path,
        "session.rename",
        serde_json::json!({"name": "second", "new_name": "first"}),
    )
    .await;
    assert!(
        rename_resp["error"].is_object(),
        "rename to existing name should fail"
    );
    assert_eq!(
        rename_resp["error"]["code"],
        shux_rpc::ErrorCode::NameConflict.code()
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_013_session_name_validation_empty() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let resp = rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": ""}),
    )
    .await;
    // Explicit empty name should fail validation (auto-generate only when name is absent)
    assert!(
        resp["error"].is_object(),
        "empty name should fail validation: {resp}"
    );
    assert_eq!(
        resp["error"]["code"],
        shux_rpc::ErrorCode::InvalidParams.code()
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_013_session_name_validation_spaces() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let resp = rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "bad name"}),
    )
    .await;
    assert!(
        resp["error"].is_object(),
        "name with spaces should fail: {resp}"
    );
    assert_eq!(
        resp["error"]["code"],
        shux_rpc::ErrorCode::InvalidParams.code()
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_013_session_name_validation_too_long() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let long_name = "a".repeat(129);
    let resp = rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": long_name}),
    )
    .await;
    assert!(
        resp["error"].is_object(),
        "name too long should fail: {resp}"
    );
    assert_eq!(
        resp["error"]["code"],
        shux_rpc::ErrorCode::InvalidParams.code()
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_013_session_ensure_idempotency_triple() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let r1 = rpc_raw(
        &socket_path,
        "session.ensure",
        serde_json::json!({"name": "idem"}),
    )
    .await;
    let r2 = rpc_raw(
        &socket_path,
        "session.ensure",
        serde_json::json!({"name": "idem"}),
    )
    .await;
    let r3 = rpc_raw(
        &socket_path,
        "session.ensure",
        serde_json::json!({"name": "idem"}),
    )
    .await;

    assert_eq!(r1["result"]["id"], r2["result"]["id"]);
    assert_eq!(r2["result"]["id"], r3["result"]["id"]);
    assert_eq!(r1["result"]["created"], true);
    assert_eq!(r2["result"]["created"], false);
    assert_eq!(r3["result"]["created"], false);

    cancel.cancel();
}

#[tokio::test]
async fn test_013_session_kill_nonexistent() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let resp = rpc_raw(
        &socket_path,
        "session.kill",
        serde_json::json!({"name": "nonexistent"}),
    )
    .await;
    assert!(
        resp["error"].is_object(),
        "killing nonexistent session should fail"
    );
    assert_eq!(resp["error"]["code"], shux_rpc::ErrorCode::NotFound.code());

    cancel.cancel();
}

#[tokio::test]
async fn test_013_session_create_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "dupe"}),
    )
    .await;

    let resp = rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "dupe"}),
    )
    .await;
    assert!(resp["error"].is_object(), "duplicate name should fail");
    assert_eq!(
        resp["error"]["code"],
        shux_rpc::ErrorCode::NameConflict.code()
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_013_session_list_sorted_by_creation() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    for name in ["alpha", "beta", "gamma"] {
        rpc_raw(
            &socket_path,
            "session.create",
            serde_json::json!({"name": name}),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let resp = rpc_raw(&socket_path, "session.list", serde_json::json!({})).await;
    let sessions = resp["result"]["sessions"].as_array().unwrap();
    let names: Vec<&str> = sessions.iter().filter_map(|s| s["name"].as_str()).collect();
    assert_eq!(names, vec!["alpha", "beta", "gamma"]);

    cancel.cancel();
}

#[tokio::test]
async fn test_013_session_list_has_window_count() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "counted"}),
    )
    .await;

    let resp = rpc_raw(&socket_path, "session.list", serde_json::json!({})).await;
    let sessions = resp["result"]["sessions"].as_array().unwrap();
    assert_eq!(sessions[0]["window_count"], 1);
    assert!(sessions[0]["active_window_id"].is_string());

    cancel.cancel();
}

#[tokio::test]
async fn test_013_session_create_has_window_pane_ids() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let resp = rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "with-ids"}),
    )
    .await;

    assert!(
        resp["result"]["window_id"].is_string(),
        "create should return window_id"
    );
    assert!(
        resp["result"]["pane_id"].is_string(),
        "create should return pane_id"
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_013_session_auto_name() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    // Create with no name — should auto-generate
    let resp = rpc_raw(&socket_path, "session.create", serde_json::json!({})).await;
    assert!(resp["error"].is_null(), "auto-name create should succeed");
    let name = resp["result"]["name"].as_str().unwrap();
    assert!(
        name.starts_with("session-"),
        "auto-name should start with 'session-', got: {name}"
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_013_session_kill_by_id() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let create_resp = rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "kill-by-id"}),
    )
    .await;
    let session_id = create_resp["result"]["id"].as_str().unwrap().to_string();

    let kill_resp = rpc_raw(
        &socket_path,
        "session.kill",
        serde_json::json!({"id": session_id}),
    )
    .await;
    assert!(
        kill_resp["error"].is_null(),
        "kill by ID should succeed: {kill_resp}"
    );

    // Verify it is gone
    let list_resp = rpc_raw(&socket_path, "session.list", serde_json::json!({})).await;
    let sessions = list_resp["result"]["sessions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(sessions.is_empty());

    cancel.cancel();
}

#[tokio::test]
async fn test_013_cli_rename() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    // Create via RPC
    rpc_raw(
        &socket_path,
        "session.create",
        serde_json::json!({"name": "cli-rename-old"}),
    )
    .await;

    // Rename via CLI
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_shux"))
        .args([
            "--socket",
            socket_path.to_str().unwrap(),
            "rename",
            "-s",
            "cli-rename-old",
            "-n",
            "cli-rename-new",
        ])
        .output()
        .await
        .unwrap();

    assert!(
        output.status.success(),
        "shux rename should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Renamed") || stdout.contains("renamed"),
        "rename output should confirm: {stdout}"
    );

    // Verify via RPC
    let list_resp = rpc_raw(&socket_path, "session.list", serde_json::json!({})).await;
    let sessions = list_resp["result"]["sessions"].as_array().unwrap();
    let names: Vec<&str> = sessions.iter().filter_map(|s| s["name"].as_str()).collect();
    assert!(names.contains(&"cli-rename-new"));
    assert!(!names.contains(&"cli-rename-old"));

    cancel.cancel();
}
