//! `shux client list` / `shux client detach`. Mirrors `client.*` RPC.

use crate::cli::{OutputFormat, rpc_call};
use crate::style;

pub async fn handle_client_list(
    stream: &mut tokio::net::UnixStream,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let result = rpc_call(stream, "client.list", serde_json::json!({})).await?;
    match format {
        OutputFormat::Json => println!(
            "{}",
            style::json_safe(&serde_json::to_string_pretty(&result)?)
        ),
        OutputFormat::Text | OutputFormat::Plain => {
            let rows = result
                .get("clients")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            style::print_clients(&rows)?;
        }
    }
    Ok(())
}

pub async fn handle_client_detach(
    stream: &mut tokio::net::UnixStream,
    id: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let result = rpc_call(stream, "client.detach", serde_json::json!({ "id": id })).await?;
    match format {
        OutputFormat::Json => println!(
            "{}",
            style::json_safe(&serde_json::to_string_pretty(&result)?)
        ),
        OutputFormat::Text | OutputFormat::Plain => {
            let detached = result
                .get("detached")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("client.detach answered without the detached id"))?;
            style::print_success("detached client", style::short_id(detached), None)?;
        }
    }
    Ok(())
}
