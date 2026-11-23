//! `shux client list` / `shux client detach`. Mirrors `client.*` RPC.

use crate::cli::{OutputFormat, rpc_call, to_style_format};
use crate::style;

pub async fn handle_client_list(
    stream: &mut tokio::net::UnixStream,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let result = rpc_call(stream, "client.list", serde_json::json!({})).await?;
    if matches!(format, OutputFormat::Json) {
        println!(
            "{}",
            style::json_safe(&serde_json::to_string_pretty(&result)?)
        );
        return Ok(());
    }
    let rows = result
        .get("clients")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    match style::TerminalContext::detect(to_style_format(format)).format {
        style::OutputFormat::Plain => style::print_clients_plain(&rows)?,
        _ => style::print_clients(&rows)?,
    }
    Ok(())
}

pub async fn handle_client_detach(
    stream: &mut tokio::net::UnixStream,
    id: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let result = rpc_call(stream, "client.detach", serde_json::json!({ "id": id })).await?;
    if matches!(format, OutputFormat::Json) {
        println!(
            "{}",
            style::json_safe(&serde_json::to_string_pretty(&result)?)
        );
        return Ok(());
    }
    let detached = result
        .get("detached")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("client.detach answered without the detached id"))?;
    match style::TerminalContext::detect(to_style_format(format)).format {
        style::OutputFormat::Plain => style::print_plain_line(detached)?,
        _ => style::print_success("detached client", style::short_id(detached), None)?,
    }
    Ok(())
}
