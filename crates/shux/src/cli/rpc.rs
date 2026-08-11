//! The client half of the JSON-RPC wire: framing, and how an error is shown.
//!
//! `rpc_display` is the single funnel for every RPC error the CLI prints,
//! which is why the terminal-escape guard (issue #104) lives there and not at
//! the dozen sites that compose one.

use super::args::*;

/// Format an RPC error for human display. Dispatch on the JSON-RPC
/// CODE first — `version_conflict` (-32002) carries the same
/// `id`+`resource` envelope as `not_found` (-32004), so a
/// presence-of-fields heuristic mis-reports concurrency conflicts as
/// "not found" (issue #25 §3).
///
/// EGRESS GUARD (issue #104). Everything this returns is printed to a
/// terminal, and most of it quotes something a caller supplied — a session
/// name, a window selector, an id fragment. This is the single funnel for
/// every RPC error the CLI prints, so the guard belongs here rather than at
/// each of the dozen sites that compose one: a message that reaches a
/// terminal with a live `ESC ]0;` in it can retitle the user's window.
pub fn rpc_display(code: i64, message: &str, data: Option<&serde_json::Value>) -> String {
    crate::style::safe_diagnostic(&rpc_display_raw(code, message, data))
}

pub fn rpc_display_raw(code: i64, message: &str, data: Option<&serde_json::Value>) -> String {
    let resource = data
        .and_then(|d| d.get("resource"))
        .and_then(|v| v.as_str())
        .unwrap_or("resource");
    let id_field = data.and_then(|d| d.get("id")).and_then(|v| v.as_str());

    match code {
        // not_found
        -32004 => match id_field {
            Some(id) => format!("{resource} '{id}' not found"),
            // No structured data at all means the CLI composed this error
            // itself — `resolve_session_id`, `resolve_window_id` and the
            // pane-membership check all do, and all of them say which name
            // was missing. Collapsing that to a contentless "resource not
            // found" was what a mistyped session name actually printed.
            None if data.is_none() && !message.is_empty() => message.to_string(),
            None => format!("{resource} not found"),
        },
        // version_conflict
        -32002 => {
            let expected = data
                .and_then(|d| d.get("expected_version"))
                .and_then(|v| v.as_u64());
            let actual = data
                .and_then(|d| d.get("actual_version"))
                .and_then(|v| v.as_u64());
            match (id_field, expected, actual) {
                (Some(id), Some(e), Some(a)) => format!(
                    "{resource} '{id}' version_conflict: expected {e}, actual {a} \
                     (re-read state and retry with the current version)"
                ),
                _ => format!("{resource} version_conflict — re-read state and retry"),
            }
        }
        // stale_revision — `data` already carries the revisions that ARE
        // diffable. Falling through to the generic arm printed the bare code
        // and threw the answer away.
        -32010 => {
            let requested = data
                .and_then(|d| d.get("requested"))
                .and_then(|v| v.as_u64());
            let available: Vec<String> = data
                .and_then(|d| d.get("available"))
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_u64())
                        .map(|v| v.to_string())
                        .collect()
                })
                .unwrap_or_default();
            match (requested, available.is_empty()) {
                (Some(r), false) => format!(
                    "revision {r} was not checkpointed (available: {})",
                    available.join(", ")
                ),
                (Some(r), true) => format!(
                    "revision {r} was not checkpointed, and this pane has no live checkpoints"
                ),
                (None, _) => "that revision was not checkpointed".to_string(),
            }
        }
        // name_conflict — `data.name` carries the colliding name.
        // -32007, not -32003: -32003 is auth_required. Keyed on the wrong
        // code, this arm never fired for a real duplicate name (which fell
        // through to the raw "RPC error -32007: name_conflict") and instead
        // rendered auth failures as name conflicts.
        -32007 => {
            if let Some(name) = data.and_then(|d| d.get("name")).and_then(|v| v.as_str()) {
                format!("{resource} name '{name}' already exists")
            } else {
                format!("{resource} name_conflict")
            }
        }
        // invalid_params / internal / spawn_failed — use `detail` when present,
        // and `hint` alongside it.
        //
        // The hint used to be dropped here. `RpcError::spawn_failed` has always
        // attached "check argv[0] resolves via PATH and cwd exists", and no
        // user has ever seen it: a failed `session create` printed the bare OS
        // error ("No such file or directory (os error 2)") and nothing about
        // what to look at. A diagnostic that is built, serialized, and then
        // discarded one layer from the terminal is worse than none — it reads
        // as covered. Found dogfooding a typo'd `[shell].command` (issue #132).
        _ => {
            let detail = data.and_then(|d| d.get("detail")).and_then(|v| v.as_str());
            let hint = data.and_then(|d| d.get("hint")).and_then(|v| v.as_str());
            match (detail, hint) {
                (Some(d), Some(h)) => format!("{d} — {h}"),
                (Some(d), None) => d.to_string(),
                (None, _) => format!("RPC error {code}: {message}"),
            }
        }
    }
}

/// Errors that can occur during RPC communication.
#[derive(Debug, thiserror::Error)]
pub enum RpcClientError {
    #[error("IO error")]
    Io(#[from] std::io::Error),
    #[error("JSON error")]
    Json(#[from] serde_json::Error),
    #[error("response frame too large: {0} bytes (max 16 MB)")]
    FrameTooLarge(usize),
    #[error("{}", rpc_display(*.code, message, data.as_ref()))]
    Rpc {
        code: i64,
        message: String,
        data: Option<serde_json::Value>,
    },
}

/// Send a JSON-RPC request over a UDS and read the response.
/// Uses 4-byte big-endian length-prefix framing (matching server in task 008).
pub async fn rpc_call(
    stream: &mut tokio::net::UnixStream,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, RpcClientError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

    // Read response length prefix
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let resp_len = u32::from_be_bytes(len_buf) as usize;

    // Enforce max frame size (16 MB per PRD §8.1)
    if resp_len > 16 * 1024 * 1024 {
        return Err(RpcClientError::FrameTooLarge(resp_len));
    }

    // Read response payload
    let mut resp_buf = vec![0u8; resp_len];
    stream.read_exact(&mut resp_buf).await?;

    let response: serde_json::Value = serde_json::from_slice(&resp_buf)?;

    if let Some(error) = response.get("error") {
        let code = error.get("code").and_then(|v| v.as_i64()).unwrap_or(0);
        let message = error
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error")
            .to_string();
        let data = error.get("data").cloned();
        return Err(RpcClientError::Rpc {
            code,
            message,
            data,
        });
    }

    Ok(response
        .get("result")
        .cloned()
        .unwrap_or(serde_json::Value::Null))
}

/// Convert CLI OutputFormat to style OutputFormat.
pub fn to_style_format(format: OutputFormat) -> crate::style::OutputFormat {
    match format {
        OutputFormat::Text => crate::style::OutputFormat::Text,
        OutputFormat::Json => crate::style::OutputFormat::Json,
        OutputFormat::Plain => crate::style::OutputFormat::Plain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::test_support::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn rpc_call_surfaces_structured_errors_and_frame_limits() {
        let (mut client, requests, task) = spawn_rpc_script(vec![serde_json::json!({
            "error": {
                "code": -32002,
                "message": "version_conflict",
                "data": {
                    "resource": "pane",
                    "id": "p1",
                    "expected_version": 1,
                    "actual_version": 2
                }
            }
        })]);

        let err = rpc_call(&mut client, "pane.kill", serde_json::json!({}))
            .await
            .unwrap_err();
        let rendered = err.to_string();
        assert!(rendered.contains("version_conflict"));
        assert!(rendered.contains("expected 1, actual 2"));
        let requests = finish_rpc_script(client, task, requests).await;
        assert_eq!(requests[0]["method"], "pane.kill");

        let (mut client, mut server) = tokio::net::UnixStream::pair().unwrap();
        let oversized = tokio::spawn(async move {
            let mut len_buf = [0u8; 4];
            server.read_exact(&mut len_buf).await.unwrap();
            let len = u32::from_be_bytes(len_buf) as usize;
            let mut payload = vec![0u8; len];
            server.read_exact(&mut payload).await.unwrap();
            let too_large = 16 * 1024 * 1024 + 1;
            server
                .write_all(&(too_large as u32).to_be_bytes())
                .await
                .unwrap();
        });
        let err = rpc_call(&mut client, "system.version", serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, RpcClientError::FrameTooLarge(_)));
        oversized.await.unwrap();
    }

    // ── Error rendering (found while fixing issue #120) ──────────────────

    /// A not-found the CLI composed itself carries its text in `message` and
    /// has no `data`. Rendering must not throw that away: "resource not
    /// found" tells a reader neither what was missing nor what they typed,
    /// and it is what a mistyped session name, window name, or short id
    /// produced.
    #[test]
    fn a_client_side_not_found_keeps_its_own_message() {
        let rendered = rpc_display(-32004, "session 'nosuch' not found", None);
        assert_eq!(rendered, "session 'nosuch' not found");

        let rendered = rpc_display(-32004, "window 'nope' not found in session", None);
        assert_eq!(rendered, "window 'nope' not found in session");

        let rendered = rpc_display(
            -32004,
            "pane b57c601b does not belong to session demo",
            None,
        );
        assert_eq!(rendered, "pane b57c601b does not belong to session demo");
    }

    /// The server's own not-found still renders from its structured data, so
    /// the fix above is additive.
    #[test]
    fn a_server_not_found_still_renders_from_its_data() {
        let data = serde_json::json!({"resource": "pane", "id": "abc-123"});
        assert_eq!(
            rpc_display(-32004, "not_found", Some(&data)),
            "pane 'abc-123' not found"
        );
        // Data present but id-less keeps the generic shape rather than
        // leaking the bare code name.
        let data = serde_json::json!({"resource": "session"});
        assert_eq!(
            rpc_display(-32004, "not_found", Some(&data)),
            "session not found"
        );
    }

    /// `name_conflict` is -32007; the renderer's arm was keyed on -32003,
    /// which is `auth_required`. So a duplicate session name printed the raw
    /// "RPC error -32007: name_conflict" while the name it collided with sat
    /// unused in `data`.
    #[test]
    fn a_name_conflict_renders_the_colliding_name() {
        let data = serde_json::json!({"resource": "session", "name": "dup"});
        assert_eq!(
            rpc_display(-32007, "name_conflict", Some(&data)),
            "session name 'dup' already exists"
        );
    }

    /// …and -32003 is `auth_required`, which must not masquerade as a name
    /// conflict.
    #[test]
    fn auth_required_does_not_render_as_a_name_conflict() {
        let rendered = rpc_display(-32003, "auth_required", None);
        assert!(
            !rendered.contains("name"),
            "-32003 is auth_required, not name_conflict: {rendered}"
        );
        assert!(rendered.contains("auth_required"), "{rendered}");
    }

    /// `data.hint` is the actionable half of a spawn failure and was dropped
    /// on the floor here: `RpcError::spawn_failed` has always attached one, and
    /// `session create` has always printed the bare OS error without it.
    #[test]
    fn a_hint_is_printed_alongside_the_detail() {
        let data = serde_json::json!({
            "detail": "failed to spawn child process: No such file or directory (os error 2)",
            "hint": "check argv[0] resolves via PATH and cwd exists",
        });
        let rendered = rpc_display(-32014, "spawn_failed", Some(&data));
        assert!(rendered.contains("No such file or directory"), "{rendered}");
        assert!(rendered.contains("check argv[0]"), "{rendered}");
    }

    /// A `detail` with no `hint` must render exactly as before — the vast
    /// majority of invalid_params errors carry only the detail.
    #[test]
    fn a_detail_without_a_hint_renders_unchanged() {
        let data = serde_json::json!({ "detail": "hold_ms 5 out of range" });
        assert_eq!(
            rpc_display(-32602, "invalid_params", Some(&data)),
            "hold_ms 5 out of range"
        );
    }

    /// Every RPC error the CLI prints funnels through `rpc_display`, and most
    /// of them quote something the caller typed. A live escape sequence in
    /// there would reach the terminal (issue #104).
    #[test]
    fn rendered_rpc_errors_are_inert() {
        let hostile = "\u{1b}]0;PWNED\u{7}nosuchwindow";

        // Client-composed not-found, which now echoes its own message.
        let rendered = rpc_display(-32004, &format!("window '{hostile}' not found"), None);
        assert!(!rendered.contains('\u{1b}'), "{rendered:?}");
        assert!(!rendered.contains('\u{7}'), "{rendered:?}");
        assert!(rendered.contains("nosuchwindow"), "{rendered:?}");

        // Server-composed, via `data`.
        let data = serde_json::json!({"resource": "window", "id": hostile});
        let rendered = rpc_display(-32004, "not_found", Some(&data));
        assert!(!rendered.contains('\u{1b}'), "{rendered:?}");

        // …and via `detail`, the invalid_params path.
        let data = serde_json::json!({"detail": hostile});
        let rendered = rpc_display(-32602, "invalid_params", Some(&data));
        assert!(!rendered.contains('\u{1b}'), "{rendered:?}");

        // …and a hostile session NAME in a name_conflict.
        let data = serde_json::json!({"resource": "session", "name": hostile});
        let rendered = rpc_display(-32007, "name_conflict", Some(&data));
        assert!(!rendered.contains('\u{1b}'), "{rendered:?}");
    }
}
