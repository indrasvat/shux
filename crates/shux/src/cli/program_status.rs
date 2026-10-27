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
        OutputFormat::Text | OutputFormat::Plain => crate::style::print_pane_status(&result),
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
    let params = serde_json::json!({
        "pane_id": pane,
        "state": states,
        "match": matching,
        "timeout_ms": timeout_ms,
    });
    // An error must not exit 1: that is the timeout code a script branches on.
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
        Err(other) => return Err(other.into()),
    };
    match format {
        OutputFormat::Json => print_json(&result)?,
        OutputFormat::Text | OutputFormat::Plain => {
            crate::style::print_pane_wait_status(&result, states)
        }
    }
    match result.get("outcome").and_then(|v| v.as_str()) {
        Some("matched") => Ok(()),
        Some("timeout") => std::process::exit(1),
        _ => std::process::exit(3),
    }
}
