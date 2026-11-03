//! `shux pane wait-exit`, and `--wait` on the verbs that spawn a program.
//! Exit codes are `PaneCommand::WaitExit`'s.

use super::program_status::print_json;
use super::{args::*, rpc::*};

pub(crate) async fn wait_exit_code(
    stream: &mut tokio::net::UnixStream,
    pane: &str,
    timeout_ms: Option<u64>,
    format: OutputFormat,
) -> anyhow::Result<i32> {
    match call_wait_exit(stream, pane, timeout_ms).await {
        Ok(result) => report(&result, format),
        Err(e) => Ok(report_failed(&e, "wait-exit", format, None)),
    }
}

pub async fn handle_pane_wait_exit(
    stream: &mut tokio::net::UnixStream,
    pane: &str,
    timeout_ms: Option<u64>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    match wait_exit_code(stream, pane, timeout_ms, format).await? {
        0 => Ok(()),
        code => std::process::exit(code),
    }
}

/// `--wait` on a verb that just created `created`, whose new pane is at
/// `pane_at`: that pane's exit, with the created ids, under `pane wait-exit`'s
/// exit codes.
pub(crate) async fn wait_after_create(
    stream: &mut tokio::net::UnixStream,
    created: serde_json::Value,
    pane_at: &str,
    timeout_ms: Option<u64>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let Some(pane) = created.pointer(pane_at).and_then(|v| v.as_str()) else {
        anyhow::bail!("the daemon's reply named no pane at {pane_at}");
    };
    let mut result = match call_wait_exit(stream, pane, timeout_ms).await {
        Ok(result) => result,
        Err(e) => std::process::exit(report_failed(&e, "wait", format, Some(&created))),
    };
    result["created"] = created;
    match report(&result, format)? {
        0 => Ok(()),
        code => std::process::exit(code),
    }
}

/// A `--wait` verb that failed before its wait began: 2 for a request the
/// daemon refused, 3 for one it could not answer. Never 1, the timeout code.
fn wait_create_failed(err: anyhow::Error, verb: &str, format: OutputFormat) -> ! {
    match err.downcast_ref::<RpcClientError>() {
        Some(e) => std::process::exit(report_failed(e, verb, format, None)),
        None => super::lens::lens_emit_error_and_exit(
            format,
            verb,
            -32603,
            &format!("{err:#}"),
            None,
            3,
        ),
    }
}

/// Under `--wait`, `result`'s failure exits by [`wait_create_failed`].
pub fn or_wait_failed<T>(
    result: anyhow::Result<T>,
    waiting: bool,
    verb: &str,
    format: OutputFormat,
) -> anyhow::Result<T> {
    match result {
        Err(e) if waiting => wait_create_failed(e, verb, format),
        result => result,
    }
}

/// Report a failed RPC; its exit code: 2 when the daemon refused the request,
/// 3 when it could not answer it or went away.
fn report_failed(
    err: &RpcClientError,
    verb: &str,
    format: OutputFormat,
    created: Option<&serde_json::Value>,
) -> i32 {
    let (code, message, mut data, exit) = match err {
        RpcClientError::Rpc {
            code,
            message,
            data,
        } => (
            *code,
            message.clone(),
            data.clone(),
            if *code == -32603 { 3 } else { 2 },
        ),
        other => (
            -32603,
            format!("{other}; is the daemon still running?"),
            None,
            3,
        ),
    };
    if let Some(created) = created {
        data.get_or_insert_with(|| serde_json::json!({}))["created"] = created.clone();
    }
    super::lens::emit_error(format, verb, code, &message, data);
    exit
}

async fn call_wait_exit(
    stream: &mut tokio::net::UnixStream,
    pane: &str,
    timeout_ms: Option<u64>,
) -> Result<serde_json::Value, RpcClientError> {
    let mut params = serde_json::json!({ "pane_id": pane });
    if let Some(ms) = timeout_ms {
        params["timeout_ms"] = ms.into();
    }
    rpc_call(stream, "pane.wait_exit", params).await
}

/// Print the outcome; its exit code.
fn report(result: &serde_json::Value, format: OutputFormat) -> anyhow::Result<i32> {
    match format {
        OutputFormat::Json => print_json(result)?,
        OutputFormat::Text => crate::style::print_pane_wait_exit(result),
        OutputFormat::Plain => crate::style::print_pane_wait_exit_plain(result),
    }
    Ok(match result.get("outcome").and_then(|v| v.as_str()) {
        Some("exited") => 0,
        Some("timeout") => 1,
        Some("killed") => 4,
        _ => 3,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn wait_exit_exits_3_not_1_when_the_daemon_hangs_up() {
        let (mut client, mut daemon) = tokio::net::UnixStream::pair().unwrap();
        tokio::spawn(async move {
            let mut len = [0u8; 4];
            daemon.read_exact(&mut len).await.unwrap();
            let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
            daemon.read_exact(&mut body).await.unwrap();
        });
        let code = wait_exit_code(&mut client, "abcd", Some(1_000), OutputFormat::Json)
            .await
            .unwrap();
        assert_eq!(code, 3, "a broken stream must not read as a timeout");
    }
}
