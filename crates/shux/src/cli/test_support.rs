//! Shared plumbing for the CLI handler tests: a scripted RPC server on a real
//! Unix socket, and the `session.list` / `window.list` payloads the resolution
//! tests replay through it.

use std::sync::{Arc, Mutex as StdMutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub fn spawn_rpc_script(
    responses: Vec<serde_json::Value>,
) -> (
    tokio::net::UnixStream,
    Arc<StdMutex<Vec<serde_json::Value>>>,
    tokio::task::JoinHandle<()>,
) {
    let (client, mut server) = tokio::net::UnixStream::pair().unwrap();
    let requests = Arc::new(StdMutex::new(Vec::new()));
    let captured = requests.clone();
    let task = tokio::spawn(async move {
        for scripted in responses {
            let mut len_buf = [0u8; 4];
            server.read_exact(&mut len_buf).await.unwrap();
            let len = u32::from_be_bytes(len_buf) as usize;
            let mut payload = vec![0u8; len];
            server.read_exact(&mut payload).await.unwrap();
            let request: serde_json::Value = serde_json::from_slice(&payload).unwrap();
            captured.lock().unwrap().push(request.clone());

            let response = if let Some(error) = scripted.get("error").filter(|e| !e.is_null()) {
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": request.get("id").cloned().unwrap_or(serde_json::Value::Null),
                    "error": error,
                })
            } else {
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": request.get("id").cloned().unwrap_or(serde_json::Value::Null),
                    "result": scripted,
                })
            };
            let bytes = serde_json::to_vec(&response).unwrap();
            server
                .write_all(&(bytes.len() as u32).to_be_bytes())
                .await
                .unwrap();
            server.write_all(&bytes).await.unwrap();
            server.flush().await.unwrap();
        }
    });
    (client, requests, task)
}

pub async fn finish_rpc_script(
    client: tokio::net::UnixStream,
    task: tokio::task::JoinHandle<()>,
    requests: Arc<StdMutex<Vec<serde_json::Value>>>,
) -> Vec<serde_json::Value> {
    drop(client);
    task.await.unwrap();
    Arc::try_unwrap(requests).unwrap().into_inner().unwrap()
}

pub fn session_list_response(session_id: &str, window_id: &str) -> serde_json::Value {
    serde_json::json!({
        "sessions": [{
            "id": session_id,
            "name": "dev",
            "active_window_id": window_id,
            "windows": [window_id],
            "window_count": 1,
            "created_at": 0
        }]
    })
}

pub fn window_list_response(window_id: &str, pane_id: &str) -> serde_json::Value {
    serde_json::json!([{
        "id": window_id,
        "title": "main",
        "index": 0,
        "pane_count": 1,
        "active_pane_id": pane_id,
        "is_active": true,
        "version": 7
    }])
}

// ── Entity id references (issue #120) ────────────────────────────────
//
// The daemon owns the authoritative resolver (`shux_core::idref`); these
// pin the CLI half, which resolves `-s` and `-w` locally against a listing
// before it ever sends a request.

pub fn multi_session_list(ids: &[(&str, &str)]) -> serde_json::Value {
    serde_json::json!({
        "sessions": ids.iter().map(|(id, name)| serde_json::json!({
            "id": id,
            "name": name,
            "active_window_id": "22222222-2222-4222-8222-222222222222",
            "windows": ["22222222-2222-4222-8222-222222222222"],
            "window_count": 1,
            "created_at": 0
        })).collect::<Vec<_>>()
    })
}

pub fn two_window_list() -> serde_json::Value {
    serde_json::json!([
        {"id": "aaaa1111-1111-4111-8111-111111111111", "title": "editor",
         "index": 0, "pane_count": 1, "is_active": true, "version": 1},
        {"id": "aaaa2222-2222-4222-8222-222222222222", "title": "logs",
         "index": 1, "pane_count": 1, "is_active": false, "version": 1},
    ])
}
