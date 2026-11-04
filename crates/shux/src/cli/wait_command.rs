//! `shux pane wait-command` and `shux shell-integration`.

use super::program_status::{print_json, print_json_error};
use super::{args::*, rpc::*};

/// Exit 0: the command ended, whatever its own code (read `exit_code`).
/// 1: timeout. 2: bad arguments. 3: anything else.
pub async fn handle_pane_wait_command(
    stream: &mut tokio::net::UnixStream,
    pane: &str,
    after: Option<u64>,
    timeout_ms: u64,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let mut params = serde_json::json!({ "pane_id": pane, "timeout_ms": timeout_ms });
    if let Some(after) = after {
        params["after"] = after.into();
    }
    let result = match rpc_call(stream, "pane.wait_command", params).await {
        Ok(result) => result,
        Err(RpcClientError::Rpc {
            code,
            message,
            data,
        }) => super::lens::lens_emit_error_and_exit(
            format,
            "wait-command",
            code,
            &message,
            data,
            super::lens::lens_checkpoint_exit_code(code),
        ),
        Err(other) => {
            match format {
                OutputFormat::Json => print_json_error(&other.to_string())?,
                OutputFormat::Text | OutputFormat::Plain => crate::style::print_error(&format!(
                    "wait-command lost the daemon connection ({other}); is the daemon still running?"
                )),
            }
            std::process::exit(3);
        }
    };
    match format {
        OutputFormat::Json => print_json(&result)?,
        OutputFormat::Text => crate::style::print_pane_wait_command(&result),
        OutputFormat::Plain => crate::style::print_pane_wait_command_plain(&result),
    }
    match result.get("outcome").and_then(|v| v.as_str()) {
        Some("completed") => Ok(()),
        Some("timeout") => std::process::exit(1),
        _ => std::process::exit(3),
    }
}

/// The hooks that make a shell report its command boundaries.
pub fn shell_integration_script(shell: &str) -> Option<&'static str> {
    match shell {
        "bash" => Some(include_str!("../shell_integration/bash.sh")),
        "zsh" => Some(include_str!("../shell_integration/zsh.zsh")),
        _ => None,
    }
}
