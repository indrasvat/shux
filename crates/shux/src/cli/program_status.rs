//! `shux pane status` and `shux pane wait-status`.

use super::{args::*, rpc::*};

fn print_json(result: &serde_json::Value) -> anyhow::Result<()> {
    let envelope = serde_json::json!({ "result": result });
    println!(
        "{}",
        crate::style::json_safe(&serde_json::to_string_pretty(&envelope)?)
    );
    Ok(())
}

fn print_json_error(message: &str) -> anyhow::Result<()> {
    let envelope = serde_json::json!({
        "error": { "code": -32603, "message": message }
    });
    println!(
        "{}",
        crate::style::json_safe(&serde_json::to_string_pretty(&envelope)?)
    );
    Ok(())
}

pub async fn handle_pane_status(
    stream: &mut tokio::net::UnixStream,
    pane: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let result = rpc_call(
        stream,
        "pane.status",
        serde_json::json!({ "pane_id": pane }),
    )
    .await?;
    match format {
        OutputFormat::Json => print_json(&result)?,
        OutputFormat::Text => crate::style::print_pane_status(&result),
        OutputFormat::Plain => crate::style::print_pane_status_plain(&result),
    }
    Ok(())
}

pub async fn handle_pane_wait_status(
    stream: &mut tokio::net::UnixStream,
    pane: &str,
    states: &[String],
    matching: &str,
    timeout_ms: u64,
    format: OutputFormat,
) -> anyhow::Result<()> {
    match wait_status_exit_code(stream, pane, states, matching, timeout_ms, format).await? {
        0 => Ok(()),
        code => std::process::exit(code),
    }
}

/// Exits 3: the daemon could not be reached.
pub fn wait_status_unreachable(err: anyhow::Error, format: OutputFormat) -> ! {
    match format {
        OutputFormat::Json => {
            if let Err(e) = print_json_error(&format!("{err:#}")) {
                crate::style::print_error(&e.to_string());
            }
        }
        OutputFormat::Text | OutputFormat::Plain => {
            crate::style::print_error(&format!("wait-status could not reach the daemon: {err:#}"))
        }
    }
    std::process::exit(3)
}

/// Exit code: 0 matched, 1 timeout, 2 bad arguments, 3 anything else. Only a
/// timeout may be 1: scripts branch on it.
pub(crate) async fn wait_status_exit_code(
    stream: &mut tokio::net::UnixStream,
    pane: &str,
    states: &[String],
    matching: &str,
    timeout_ms: u64,
    format: OutputFormat,
) -> anyhow::Result<i32> {
    let mut unique: Vec<&String> = Vec::new();
    for s in states {
        if !unique.contains(&s) {
            unique.push(s);
        }
    }
    let states: Vec<String> = unique.into_iter().cloned().collect();
    let states = states.as_slice();
    let params = serde_json::json!({
        "pane_id": pane,
        "state": states,
        "match": matching,
        "timeout_ms": timeout_ms,
    });
    let result = match rpc_call(stream, "pane.wait_status", params).await {
        Ok(result) => result,
        Err(RpcClientError::Rpc {
            code,
            message,
            data,
        }) => super::lens::lens_emit_error_and_exit(
            format,
            "wait-status",
            code,
            &message,
            data,
            super::lens::lens_checkpoint_exit_code(code),
        ),
        // The daemon went away or the stream broke.
        Err(other) => {
            match format {
                OutputFormat::Json => print_json_error(&other.to_string())?,
                OutputFormat::Text | OutputFormat::Plain => crate::style::print_error(&format!(
                    "wait-status lost the daemon connection ({other}); is the daemon still running?"
                )),
            }
            return Ok(3);
        }
    };
    match format {
        OutputFormat::Json => print_json(&result)?,
        OutputFormat::Text => crate::style::print_pane_wait_status(&result, states),
        OutputFormat::Plain => crate::style::print_pane_wait_status_plain(&result),
    }
    Ok(match result.get("outcome").and_then(|v| v.as_str()) {
        Some("matched") => 0,
        Some("timeout") => 1,
        _ => 3,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn wait_status_exits_3_not_1_when_the_daemon_hangs_up() {
        let (mut client, mut daemon) = tokio::net::UnixStream::pair().unwrap();
        tokio::spawn(async move {
            let mut len = [0u8; 4];
            daemon.read_exact(&mut len).await.unwrap();
            let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
            daemon.read_exact(&mut body).await.unwrap();
            // Hang up without answering.
        });
        let code = wait_status_exit_code(
            &mut client,
            "abcd",
            &["done".into()],
            "top",
            1_000,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        assert_eq!(code, 3, "a broken stream must not read as a timeout");
    }
}
