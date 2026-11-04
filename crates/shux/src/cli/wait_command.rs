//! `shux pane wait-command` and `shux shell-integration`.

use super::args::*;
use super::program_status::{call_wait, print_json};

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
    let result =
        match call_wait(stream, "pane.wait_command", "wait-command", params, format).await? {
            Ok(result) => result,
            Err(code) => std::process::exit(code),
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
