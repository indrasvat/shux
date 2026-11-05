//! `shux pane status` and `shux pane wait-status`.

use super::{args::*, rpc::*};

pub(super) fn print_json(result: &serde_json::Value) -> anyhow::Result<()> {
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

/// What `pane wait-status` waits for.
pub struct StatusWait<'a> {
    pub states: &'a [String],
    /// Sent only when given, so the daemon's default applies otherwise.
    pub matching: Option<&'a str>,
    pub id: Option<&'a str>,
    pub after: Option<u64>,
    pub timeout_ms: u64,
}

pub async fn handle_pane_wait_status(
    stream: &mut tokio::net::UnixStream,
    pane: &str,
    wait: &StatusWait<'_>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    match wait_status_exit_code(stream, pane, wait, format).await? {
        0 => Ok(()),
        code => std::process::exit(code),
    }
}

/// Exits 3: the daemon could not be reached. For the verbs that keep 1 for a
/// timeout.
pub fn wait_unreachable(verb: &str, err: anyhow::Error, format: OutputFormat) -> ! {
    match format {
        OutputFormat::Json => {
            if let Err(e) = print_json_error(&format!("{err:#}")) {
                crate::style::print_error(&e.to_string());
            }
        }
        OutputFormat::Text | OutputFormat::Plain => {
            crate::style::print_error(&format!("{verb} could not reach the daemon: {err:#}"))
        }
    }
    std::process::exit(3)
}

fn unique(states: &[String]) -> Vec<&String> {
    let mut seen: Vec<&String> = Vec::new();
    for s in states {
        if !seen.contains(&s) {
            seen.push(s);
        }
    }
    seen
}

/// A wait RPC's result, or `Err` with the exit code once its failure is
/// reported: a refused request exits here, a lost daemon is 3. No such pane
/// is 2, as `wait-exit` says, so it cannot read as a pane that went away.
pub(super) async fn call_wait(
    stream: &mut tokio::net::UnixStream,
    method: &str,
    verb: &str,
    params: serde_json::Value,
    format: OutputFormat,
) -> anyhow::Result<Result<serde_json::Value, i32>> {
    match rpc_call(stream, method, params).await {
        Ok(result) => Ok(Ok(result)),
        Err(RpcClientError::Rpc {
            code,
            message,
            data,
        }) => super::lens::lens_emit_error_and_exit(
            format,
            verb,
            code,
            &message,
            data,
            match code {
                -32004 => 2,
                code => super::lens::lens_checkpoint_exit_code(code),
            },
        ),
        Err(other) => {
            match format {
                OutputFormat::Json => print_json_error(&other.to_string())?,
                OutputFormat::Text | OutputFormat::Plain => crate::style::print_error(&format!(
                    "{verb} lost the daemon connection ({other}); is the daemon still running?"
                )),
            }
            Ok(Err(3))
        }
    }
}

/// `pane.wait_status`'s params: `match`, `id` and `after` only when given.
fn wait_status_params(pane: &str, states: &[&String], wait: &StatusWait<'_>) -> serde_json::Value {
    let mut params = serde_json::json!({
        "pane_id": pane,
        "state": states,
        "timeout_ms": wait.timeout_ms,
    });
    if let Some(m) = wait.matching {
        params["match"] = m.into();
    }
    if let Some(id) = wait.id {
        params["id"] = id.into();
    }
    if let Some(after) = wait.after {
        params["after"] = after.into();
    }
    params
}

/// Exit code: 0 matched, 1 timeout, 2 bad arguments, 3 anything else. Only a
/// timeout may be 1: scripts branch on it.
pub(crate) async fn wait_status_exit_code(
    stream: &mut tokio::net::UnixStream,
    pane: &str,
    wait: &StatusWait<'_>,
    format: OutputFormat,
) -> anyhow::Result<i32> {
    let states = unique(wait.states);
    let params = wait_status_params(pane, &states, wait);
    let result = match call_wait(stream, "pane.wait_status", "wait-status", params, format).await? {
        Ok(result) => result,
        Err(code) => return Ok(code),
    };
    match format {
        OutputFormat::Json => print_json(&result)?,
        OutputFormat::Text => crate::style::print_pane_wait_status(&result, &states, wait.id),
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
        let states = ["done".to_string()];
        let wait = StatusWait {
            states: &states,
            matching: Some("top"),
            id: None,
            after: None,
            timeout_ms: 1_000,
        };
        let code = wait_status_exit_code(&mut client, "abcd", &wait, OutputFormat::Json)
            .await
            .unwrap();
        assert_eq!(code, 3, "a broken stream must not read as a timeout");
    }

    /// The daemon rejects `id` beside an explicit `match`, so the CLI must not
    /// send its own default: an `--id` wait would always be refused.
    #[test]
    fn match_is_sent_only_when_given() {
        let states = ["done".to_string(), "done".to_string()];
        let mut wait = StatusWait {
            states: &states,
            matching: None,
            id: Some("job"),
            after: Some(7),
            timeout_ms: 500,
        };
        let params = wait_status_params("p", &unique(wait.states), &wait);
        assert_eq!(
            params,
            serde_json::json!({"pane_id": "p", "state": ["done"], "timeout_ms": 500, "id": "job", "after": 7})
        );
        wait.matching = Some("any");
        wait.id = None;
        wait.after = None;
        assert_eq!(
            wait_status_params("p", &unique(wait.states), &wait)["match"],
            "any"
        );
    }
}
