//! Pane I/O Integration Tests
//!
//! Verifies pane.send_keys, pane.run_command, pane.command_status,
//! pane.command_cancel, and pane.capture with real PTY processes.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::UnixStream;
use tokio::sync::{Mutex, mpsc};

// ══════════════════════════════════════════════════════════════
// Shared state (mirrors main.rs PaneIoState)
// ══════════════════════════════════════════════════════════════

struct PaneIoState {
    writers: HashMap<shux_core::model::PaneId, mpsc::Sender<Vec<u8>>>,
    vts: HashMap<shux_core::model::PaneId, shux_vt::VirtualTerminal>,
    cmd_engine: shux_pty::CommandEngine,
}

impl PaneIoState {
    fn new() -> Self {
        Self {
            writers: HashMap::new(),
            vts: HashMap::new(),
            cmd_engine: shux_pty::CommandEngine::new(),
        }
    }
}

// ══════════════════════════════════════════════════════════════
// PTY helpers (mirrors main.rs)
// ══════════════════════════════════════════════════════════════

async fn run_pane_pty_task(
    pane_id: shux_core::model::PaneId,
    mut handle: shux_pty::handle::PtyHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    mut write_rx: mpsc::Receiver<Vec<u8>>,
    shutdown: tokio_util::sync::CancellationToken,
) {
    let mut buf = vec![0u8; 8192];
    loop {
        tokio::select! {
            result = handle.read(&mut buf) => {
                match result {
                    Ok(0) => break,
                    Ok(n) => {
                        let data = &buf[..n];
                        let terminal_responses = {
                            let mut state = io_state.lock().await;
                            let terminal_responses = if let Some(vt) = state.vts.get_mut(&pane_id) {
                                vt.process_with_responses(data)
                            } else {
                                Vec::new()
                            };
                            let output = String::from_utf8_lossy(data);
                            let _completed = state.cmd_engine.process_output(pane_id.0, &output);
                            terminal_responses
                        };
                        for response in &terminal_responses {
                            if handle.write(response).await.is_err() { break; }
                        }
                        if !terminal_responses.is_empty() && handle.flush().await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            Some(data) = write_rx.recv() => {
                if handle.write(&data).await.is_err() { break; }
                if handle.flush().await.is_err() { break; }
            }
            _ = shutdown.cancelled() => break,
        }
    }
    // Mirrors main.rs: drop the PTY-bound writer, keep the VT around
    // so pane.capture / pane.snapshot still work after the pane process
    // exits. The VT is only purged via the explicit-destroy paths.
    let mut state = io_state.lock().await;
    state.writers.remove(&pane_id);
}

async fn spawn_pane_pty(
    pane_id: shux_core::model::PaneId,
    cwd: PathBuf,
    io_state: Arc<Mutex<PaneIoState>>,
    shutdown: tokio_util::sync::CancellationToken,
) -> Result<(), shux_rpc::RpcError> {
    let config = shux_pty::handle::PtyConfig::default_shell(cwd);
    let handle = shux_pty::handle::PtyHandle::spawn(&config)
        .map_err(|e| shux_rpc::RpcError::internal(&format!("PTY spawn failed: {e}")))?;

    let (write_tx, write_rx) = mpsc::channel::<Vec<u8>>(256);
    let vt = shux_vt::VirtualTerminal::new(24, 80);

    {
        let mut state = io_state.lock().await;
        state.writers.insert(pane_id, write_tx);
        state.vts.insert(pane_id, vt);
    }

    tokio::spawn(run_pane_pty_task(
        pane_id, handle, io_state, write_rx, shutdown,
    ));
    Ok(())
}

// ══════════════════════════════════════════════════════════════
// RPC helpers (duplicated from main.rs — binary crate not importable)
// ══════════════════════════════════════════════════════════════

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

fn session_to_json(
    s: &shux_core::model::Session,
    snap: &shux_core::graph::SessionGraphSnapshot,
) -> serde_json::Value {
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
        "pane_id": first_pane_id,
        "created_at": 0,
    })
}

fn resolve_pane_id_from_params(
    gh: &shux_core::graph::GraphHandle,
    params: &serde_json::Value,
) -> Result<shux_core::model::PaneId, shux_rpc::RpcError> {
    if let Some(pane_id_str) = params.get("pane_id").and_then(|v| v.as_str()) {
        return pane_id_str
            .parse()
            .map_err(|_| shux_rpc::RpcError::invalid_params("invalid pane_id format"));
    }
    let window_id = resolve_window_id_from_params(gh, params)?;
    let snap = gh.snapshot();
    let window = snap
        .windows
        .get(&window_id)
        .ok_or_else(|| shux_rpc::RpcError::not_found("window", &window_id.to_string()))?;
    Ok(window.active_pane)
}

fn resolve_window_id_from_params(
    gh: &shux_core::graph::GraphHandle,
    params: &serde_json::Value,
) -> Result<shux_core::model::WindowId, shux_rpc::RpcError> {
    if let Some(wid_str) = params.get("window_id").and_then(|v| v.as_str()) {
        return wid_str
            .parse()
            .map_err(|_| shux_rpc::RpcError::invalid_params("invalid window_id format"));
    }
    let session_id_str = params
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            shux_rpc::RpcError::invalid_params(
                "missing 'pane_id' or 'window_id' or 'session_id' parameter",
            )
        })?;
    let session_id: shux_core::model::SessionId = session_id_str
        .parse()
        .map_err(|_| shux_rpc::RpcError::invalid_params("invalid session_id format"))?;
    let snap = gh.snapshot();
    let session = snap
        .sessions
        .get(&session_id)
        .ok_or_else(|| shux_rpc::RpcError::not_found("session", session_id_str))?;
    Ok(session.active_window)
}

// ══════════════════════════════════════════════════════════════
// Method registration (session + pane I/O)
// ══════════════════════════════════════════════════════════════

fn register_session_methods(
    builder: shux_rpc::RouterBuilder,
    graph: shux_core::graph::GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    cancel: tokio_util::sync::CancellationToken,
) -> shux_rpc::RouterBuilder {
    let g1 = graph.clone();
    let io = io_state;
    let ct = cancel;

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
                let gh = graph.clone();
                let io = io.clone();
                let ct = ct.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let name = params
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("test")
                        .to_string();
                    let cwd = PathBuf::from("/tmp");
                    match gh.create_session(name, cwd.clone()).await {
                        Ok(session_id) => {
                            let snap = gh.snapshot();
                            if let Some(s) = snap.sessions.get(&session_id) {
                                if let Some(wid) = s.windows.first() {
                                    if let Some(w) = snap.windows.get(wid) {
                                        let _ = spawn_pane_pty(w.active_pane, cwd, io, ct).await;
                                    }
                                }
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
}

fn register_pane_io_methods(
    builder: shux_rpc::RouterBuilder,
    graph: shux_core::graph::GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
) -> shux_rpc::RouterBuilder {
    let g1 = graph.clone();
    let g2 = graph.clone();
    let g5 = graph;

    let io1 = io_state.clone();
    let io2 = io_state.clone();
    let io3 = io_state.clone();
    let io4 = io_state.clone();
    let io5 = io_state;

    builder
        .register(
            "pane.send_keys",
            move |params: Option<serde_json::Value>| {
                let gh = g1.clone();
                let io = io1.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let pane_id = resolve_pane_id_from_params(&gh, &params)?;

                    let bytes = if let Some(text) = params.get("text").and_then(|v| v.as_str()) {
                        text.as_bytes().to_vec()
                    } else if let Some(b64) = params.get("data").and_then(|v| v.as_str()) {
                        use base64::Engine;
                        base64::engine::general_purpose::STANDARD
                            .decode(b64)
                            .map_err(|e| {
                                shux_rpc::RpcError::invalid_params(&format!("invalid base64: {e}"))
                            })?
                    } else {
                        return Err(shux_rpc::RpcError::invalid_params(
                            "missing 'text' or 'data' parameter",
                        ));
                    };

                    let state = io.lock().await;
                    let writer = state
                        .writers
                        .get(&pane_id)
                        .ok_or_else(|| {
                            shux_rpc::RpcError::not_found("pane PTY", &pane_id.to_string())
                        })?
                        .clone();
                    drop(state);

                    let len = bytes.len();
                    writer
                        .send(bytes)
                        .await
                        .map_err(|_| shux_rpc::RpcError::internal("PTY write channel closed"))?;

                    Ok(serde_json::json!({
                        "pane_id": pane_id.to_string(),
                        "bytes_written": len,
                    }))
                }
            },
        )
        .register(
            "pane.run_command",
            move |params: Option<serde_json::Value>| {
                let gh = g2.clone();
                let io = io2.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let pane_id = resolve_pane_id_from_params(&gh, &params)?;

                    let command =
                        params
                            .get("command")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| {
                                shux_rpc::RpcError::invalid_params("missing 'command' parameter")
                            })?;

                    let args: Vec<String> = params
                        .get("args")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();

                    let timeout_secs = params.get("timeout").and_then(|v| v.as_u64()).unwrap_or(30);
                    let timeout = Duration::from_secs(timeout_secs);

                    let is_async = params
                        .get("async")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);

                    let (completion_tx, completion_rx) = if !is_async {
                        let (tx, rx) = tokio::sync::oneshot::channel();
                        (Some(tx), Some(rx))
                    } else {
                        (None, None)
                    };

                    let (command_id, pty_command) = {
                        let mut state = io.lock().await;
                        state.cmd_engine.start_command(
                            pane_id.0,
                            command,
                            &args,
                            timeout,
                            completion_tx,
                        )
                    };

                    {
                        let state = io.lock().await;
                        let writer = state
                            .writers
                            .get(&pane_id)
                            .ok_or_else(|| {
                                shux_rpc::RpcError::not_found("pane PTY", &pane_id.to_string())
                            })?
                            .clone();
                        drop(state);

                        writer.send(pty_command.into_bytes()).await.map_err(|_| {
                            shux_rpc::RpcError::internal("PTY write channel closed")
                        })?;
                    }

                    if is_async {
                        return Ok(serde_json::json!({
                            "command_id": command_id.to_string(),
                            "state": "running",
                        }));
                    }

                    let result = completion_rx.unwrap().await.map_err(|_| {
                        shux_rpc::RpcError::internal("command completion channel dropped")
                    })?;

                    let stdout = {
                        let state = io.lock().await;
                        state
                            .vts
                            .get(&pane_id)
                            .map(|vt| {
                                let text = vt.capture_text(Some(50));
                                shux_pty::strip_ansi(&text)
                            })
                            .unwrap_or_default()
                    };

                    Ok(serde_json::json!({
                        "command_id": result.command_id.to_string(),
                        "state": result.state,
                        "exit_code": result.exit_code,
                        "stdout": stdout,
                        "runtime_ms": result.runtime_ms,
                    }))
                }
            },
        )
        .register(
            "pane.command_status",
            move |params: Option<serde_json::Value>| {
                let io = io3.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let cmd_id_str = params
                        .get("command_id")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'command_id' parameter")
                        })?;

                    let command_id: uuid::Uuid = cmd_id_str.parse().map_err(|_| {
                        shux_rpc::RpcError::invalid_params("invalid command_id format")
                    })?;

                    let state = io.lock().await;
                    let result = state
                        .cmd_engine
                        .get_status(command_id)
                        .ok_or_else(|| shux_rpc::RpcError::not_found("command", cmd_id_str))?;

                    Ok(serde_json::json!({
                        "command_id": result.command_id.to_string(),
                        "state": result.state,
                        "runtime_ms": result.runtime_ms,
                    }))
                }
            },
        )
        .register(
            "pane.command_cancel",
            move |params: Option<serde_json::Value>| {
                let io = io4.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let cmd_id_str = params
                        .get("command_id")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            shux_rpc::RpcError::invalid_params("missing 'command_id' parameter")
                        })?;

                    let command_id: uuid::Uuid = cmd_id_str.parse().map_err(|_| {
                        shux_rpc::RpcError::invalid_params("invalid command_id format")
                    })?;

                    let mut state = io.lock().await;
                    let pane_uuid = state
                        .cmd_engine
                        .cancel_command(command_id)
                        .ok_or_else(|| shux_rpc::RpcError::not_found("command", cmd_id_str))?;

                    let pane_id = shux_core::model::PaneId::from_uuid(pane_uuid);
                    if let Some(writer) = state.writers.get(&pane_id) {
                        let _ = writer.send(vec![0x03]).await;
                    }

                    Ok(serde_json::json!({
                        "command_id": cmd_id_str,
                        "state": "cancelled",
                    }))
                }
            },
        )
        .register("pane.capture", move |params: Option<serde_json::Value>| {
            let gh = g5.clone();
            let io = io5.clone();
            async move {
                let params = params.unwrap_or_default();
                let pane_id = resolve_pane_id_from_params(&gh, &params)?;

                let lines = params.get("lines").and_then(|v| v.as_u64()).unwrap_or(50) as usize;

                let state = io.lock().await;
                let vt = state.vts.get(&pane_id).ok_or_else(|| {
                    shux_rpc::RpcError::not_found("pane VT", &pane_id.to_string())
                })?;

                let text = vt.capture_text(Some(lines));
                let clean = shux_pty::strip_ansi(&text);

                Ok(serde_json::json!({
                    "pane_id": pane_id.to_string(),
                    "text": clean,
                    "lines": lines,
                }))
            }
        })
}

// ══════════════════════════════════════════════════════════════
// Test harness
// ══════════════════════════════════════════════════════════════

async fn rpc_call(
    stream: &mut UnixStream,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": uuid::Uuid::new_v4().to_string(),
        "method": method,
        "params": params,
    });

    let payload = serde_json::to_vec(&request).unwrap();
    let len = payload.len() as u32;
    stream.write_all(&len.to_be_bytes()).await.unwrap();
    stream.write_all(&payload).await.unwrap();
    stream.flush().await.unwrap();

    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await.unwrap();
    let resp_len = u32::from_be_bytes(len_buf) as usize;

    let mut resp_buf = vec![0u8; resp_len];
    stream.read_exact(&mut resp_buf).await.unwrap();

    let response: serde_json::Value = serde_json::from_slice(&resp_buf).unwrap();

    if let Some(error) = response.get("error") {
        panic!("RPC error: {error}");
    }

    response
        .get("result")
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

/// Start a test server with pane I/O methods, returning (socket_path, cancel_token).
async fn start_test_server(
    dir: &std::path::Path,
) -> (PathBuf, tokio_util::sync::CancellationToken) {
    let socket_path = dir.join("pane-io-test.sock");
    let cancel = tokio_util::sync::CancellationToken::new();

    let (graph, state) = shux_core::graph::SessionGraph::new();
    let (graph_tx, graph_rx) = mpsc::channel(256);
    let graph_handle = shux_core::graph::GraphHandle::new(graph_tx, state);

    let graph_cancel = cancel.clone();
    tokio::spawn(async move {
        shux_core::graph::run_graph_loop(graph, graph_rx, graph_cancel).await;
    });

    let io_state = Arc::new(Mutex::new(PaneIoState::new()));

    // Timeout checker
    let timeout_io = io_state.clone();
    let timeout_cancel = cancel.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let mut state = timeout_io.lock().await;
                    let _timed_out = state.cmd_engine.check_timeouts();
                }
                _ = timeout_cancel.cancelled() => break,
            }
        }
    });

    let router = register_pane_io_methods(
        register_session_methods(
            shux_rpc::server::register_builtin_methods(shux_rpc::Router::builder()),
            graph_handle.clone(),
            io_state.clone(),
            cancel.clone(),
        ),
        graph_handle,
        io_state,
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

    // Wait for socket to become available
    for _ in 0..20 {
        if UnixStream::connect(&socket_path).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    (socket_path, cancel)
}

/// Create a session and return (session_id, pane_id).
async fn create_test_session(stream: &mut UnixStream) -> (String, String) {
    let result = rpc_call(
        stream,
        "session.create",
        serde_json::json!({"name": "test"}),
    )
    .await;
    let session_id = result["id"].as_str().unwrap().to_string();
    let pane_id = result["pane_id"].as_str().unwrap().to_string();
    // Give the shell a moment to start
    tokio::time::sleep(Duration::from_millis(500)).await;
    (session_id, pane_id)
}

// ══════════════════════════════════════════════════════════════
// Tests
// ══════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_send_keys_text() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let mut stream = UnixStream::connect(&socket_path).await.unwrap();
    let (session_id, _pane_id) = create_test_session(&mut stream).await;

    let result = rpc_call(
        &mut stream,
        "pane.send_keys",
        serde_json::json!({"session_id": session_id, "text": "echo hello\n"}),
    )
    .await;

    assert!(result.get("bytes_written").is_some());
    let bytes = result["bytes_written"].as_u64().unwrap();
    assert_eq!(bytes, 11); // "echo hello\n" = 11 bytes

    cancel.cancel();
}

#[tokio::test]
async fn test_send_keys_base64() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let mut stream = UnixStream::connect(&socket_path).await.unwrap();
    let (session_id, _pane_id) = create_test_session(&mut stream).await;

    // Send Ctrl-C (0x03) via base64
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode([0x03]);

    let result = rpc_call(
        &mut stream,
        "pane.send_keys",
        serde_json::json!({"session_id": session_id, "data": b64}),
    )
    .await;

    assert_eq!(result["bytes_written"].as_u64().unwrap(), 1);

    cancel.cancel();
}

#[tokio::test]
async fn test_send_keys_nonexistent_pane() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let mut stream = UnixStream::connect(&socket_path).await.unwrap();

    // Try to send keys to a non-existent pane
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": uuid::Uuid::new_v4().to_string(),
        "method": "pane.send_keys",
        "params": {
            "pane_id": "00000000-0000-0000-0000-000000000000",
            "text": "hello",
        },
    });

    let payload = serde_json::to_vec(&request).unwrap();
    let len = payload.len() as u32;
    stream.write_all(&len.to_be_bytes()).await.unwrap();
    stream.write_all(&payload).await.unwrap();
    stream.flush().await.unwrap();

    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await.unwrap();
    let resp_len = u32::from_be_bytes(len_buf) as usize;

    let mut resp_buf = vec![0u8; resp_len];
    stream.read_exact(&mut resp_buf).await.unwrap();

    let response: serde_json::Value = serde_json::from_slice(&resp_buf).unwrap();
    assert!(response.get("error").is_some());

    cancel.cancel();
}

#[tokio::test]
async fn test_capture_after_echo() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let mut stream = UnixStream::connect(&socket_path).await.unwrap();
    let (session_id, _pane_id) = create_test_session(&mut stream).await;

    // Send echo command
    rpc_call(
        &mut stream,
        "pane.send_keys",
        serde_json::json!({"session_id": session_id, "text": "echo SHUX_TEST_OUTPUT\n"}),
    )
    .await;

    // Wait for output to arrive and be processed
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Capture output
    let result = rpc_call(
        &mut stream,
        "pane.capture",
        serde_json::json!({"session_id": session_id}),
    )
    .await;

    let text = result["text"].as_str().unwrap();
    assert!(
        text.contains("SHUX_TEST_OUTPUT"),
        "captured text should contain 'SHUX_TEST_OUTPUT', got: {text}"
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_xterm_cursor_report_probe_gets_terminal_response() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let mut stream = UnixStream::connect(&socket_path).await.unwrap();
    let (session_id, _pane_id) = create_test_session(&mut stream).await;

    let probe = r#"python3 -c 'import os,select,sys,termios,tty; fd=0; old=termios.tcgetattr(fd); tty.setraw(fd); sys.stdout.write("\x1b[5;10H\x1b[6n"); sys.stdout.flush(); r,_,_=select.select([fd],[],[],1.0); data=os.read(fd,32) if r else b""; termios.tcsetattr(fd, termios.TCSADRAIN, old); print("\nSHUX_CPR="+repr(data))'"#;
    rpc_call(
        &mut stream,
        "pane.send_keys",
        serde_json::json!({"session_id": session_id, "text": format!("{probe}\n")}),
    )
    .await;

    tokio::time::sleep(Duration::from_secs(2)).await;

    let result = rpc_call(
        &mut stream,
        "pane.capture",
        serde_json::json!({"session_id": session_id, "lines": 80}),
    )
    .await;
    let text = result["text"].as_str().unwrap();
    assert!(
        text.contains("SHUX_CPR=b'\\x1b[5;10R'"),
        "xterm CPR probe should receive a terminal response, got: {text}"
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_run_command_sync_echo() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let mut stream = UnixStream::connect(&socket_path).await.unwrap();
    let (session_id, _pane_id) = create_test_session(&mut stream).await;

    let result = rpc_call(
        &mut stream,
        "pane.run_command",
        serde_json::json!({
            "session_id": session_id,
            "command": "echo",
            "args": ["hello_from_shux"],
            "timeout": 10,
        }),
    )
    .await;

    assert_eq!(result["state"].as_str().unwrap(), "completed");
    assert_eq!(result["exit_code"].as_i64().unwrap(), 0);
    let stdout = result["stdout"].as_str().unwrap();
    assert!(
        stdout.contains("hello_from_shux"),
        "stdout should contain 'hello_from_shux', got: {stdout}"
    );

    cancel.cancel();
}

#[tokio::test]
async fn test_run_command_sync_false() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let mut stream = UnixStream::connect(&socket_path).await.unwrap();
    let (session_id, _pane_id) = create_test_session(&mut stream).await;

    let result = rpc_call(
        &mut stream,
        "pane.run_command",
        serde_json::json!({
            "session_id": session_id,
            "command": "false",
            "timeout": 10,
        }),
    )
    .await;

    assert_eq!(result["state"].as_str().unwrap(), "completed");
    assert_eq!(result["exit_code"].as_i64().unwrap(), 1);

    cancel.cancel();
}

#[tokio::test]
async fn test_run_command_async_and_poll() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let mut stream = UnixStream::connect(&socket_path).await.unwrap();
    let (session_id, _pane_id) = create_test_session(&mut stream).await;

    // Start async command
    let result = rpc_call(
        &mut stream,
        "pane.run_command",
        serde_json::json!({
            "session_id": session_id,
            "command": "sleep",
            "args": ["0.5"],
            "timeout": 10,
            "async": true,
        }),
    )
    .await;

    assert_eq!(result["state"].as_str().unwrap(), "running");
    let command_id = result["command_id"].as_str().unwrap();

    // Poll status — should be running
    let status = rpc_call(
        &mut stream,
        "pane.command_status",
        serde_json::json!({"command_id": command_id}),
    )
    .await;
    assert_eq!(status["state"].as_str().unwrap(), "running");

    cancel.cancel();
}

#[tokio::test]
async fn test_run_command_cancel() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let mut stream = UnixStream::connect(&socket_path).await.unwrap();
    let (session_id, _pane_id) = create_test_session(&mut stream).await;

    // Start a long-running async command
    let result = rpc_call(
        &mut stream,
        "pane.run_command",
        serde_json::json!({
            "session_id": session_id,
            "command": "sleep",
            "args": ["60"],
            "timeout": 120,
            "async": true,
        }),
    )
    .await;

    let command_id = result["command_id"].as_str().unwrap();

    // Cancel it
    let cancel_result = rpc_call(
        &mut stream,
        "pane.command_cancel",
        serde_json::json!({"command_id": command_id}),
    )
    .await;

    assert_eq!(cancel_result["state"].as_str().unwrap(), "cancelled");

    cancel.cancel();
}

#[tokio::test]
async fn test_capture_lines_default() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let mut stream = UnixStream::connect(&socket_path).await.unwrap();
    let (session_id, _pane_id) = create_test_session(&mut stream).await;

    // Default capture (50 lines)
    let result = rpc_call(
        &mut stream,
        "pane.capture",
        serde_json::json!({"session_id": session_id}),
    )
    .await;

    assert!(result.get("text").is_some());
    assert_eq!(result["lines"].as_u64().unwrap(), 50);

    cancel.cancel();
}

// Codex hit this in May 2026: short-lived commands inside a shux pane
// exit before the agent can call pane.capture, and the VT used to be
// evicted from io_state the moment the PTY task observed EOF. The pane
// stays in the graph (with exit_status set), but capture returns "pane
// VT not found". tmux keeps screen+grid until the user explicitly kills
// the pane; we should too — exit_status already plays the "dead" flag.
#[tokio::test]
async fn test_capture_works_after_pane_process_exits() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, cancel) = start_test_server(dir.path()).await;

    let mut stream = UnixStream::connect(&socket_path).await.unwrap();
    let (session_id, _pane_id) = create_test_session(&mut stream).await;

    // Print a marker, then exit the shell. After this, the PTY task
    // sees EOF, reaps the child, and tears its loop down. The Pane
    // remains in the graph with exit_status=Some(0).
    rpc_call(
        &mut stream,
        "pane.send_keys",
        serde_json::json!({
            "session_id": session_id,
            "text": "echo SHUX_LIVES_AFTER_EXIT && exit 0\n",
        }),
    )
    .await;

    // Give the shell time to: render the echo, exit cleanly, and let
    // the PTY task drain. 2s is overkill but tests should be sturdy.
    tokio::time::sleep(Duration::from_secs(2)).await;

    let result = rpc_call(
        &mut stream,
        "pane.capture",
        serde_json::json!({"session_id": session_id}),
    )
    .await;

    let text = result["text"].as_str().expect(
        "pane.capture must keep returning text after the pane process exits — \
         the VT should linger until the pane is explicitly destroyed",
    );
    assert!(
        text.contains("SHUX_LIVES_AFTER_EXIT"),
        "captured text after exit should contain the marker, got: {text}"
    );

    cancel.cancel();
}
