//! `shux api` and `shux version`.

use super::{args::*, rpc::*};

/// Handle the `shux rpc call <method> --params ...` command.
pub async fn handle_api(
    stream: &mut tokio::net::UnixStream,
    method: &str,
    params_str: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let params: serde_json::Value = serde_json::from_str(params_str)
        .map_err(|e| anyhow::anyhow!("Invalid JSON params: {e}"))?;

    // PR 3b: surface RPC errors as part of the JSON-RPC envelope on
    // stdout, not as a human-readable anyhow error on stderr. Callers
    // of `shux rpc call` are debug tools / agents that expect to parse the
    // raw `{result | error}` shape — including bounded `data` fields
    // like `expected_version` / `actual_version` for retry loops.
    match rpc_call(stream, method, params).await {
        Ok(result) => {
            let envelope = serde_json::json!({"result": result});
            match format {
                OutputFormat::Json | OutputFormat::Text | OutputFormat::Plain => {
                    println!(
                        "{}",
                        crate::style::json_safe(&serde_json::to_string_pretty(&envelope)?)
                    );
                }
            }
            Ok(())
        }
        Err(RpcClientError::Rpc {
            code,
            message,
            data,
        }) => {
            let mut err_obj = serde_json::json!({
                "code": code,
                "message": message,
            });
            if let Some(d) = data {
                err_obj["data"] = d;
            }
            let envelope = serde_json::json!({"error": err_obj});
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&envelope)?)
            );
            // Non-zero exit so shell pipelines can branch, but the
            // structured error is still on stdout for parsers.
            std::process::exit(2);
        }
        Err(other) => Err(other.into()),
    }
}

/// Handle the `shux version` command: this CLI's build, and the daemon's
/// beside it when one serves `socket_path`. Never starts a daemon.
pub async fn handle_version(
    socket_path: &std::path::Path,
    format: OutputFormat,
) -> anyhow::Result<()> {
    use crate::client::{DaemonAnswer, VERSION_CHECK_TIMEOUT};

    let cli = (env!("CARGO_PKG_VERSION"), env!("SHUX_GIT_SHA"));
    let answer = crate::client::ask_daemon_version(socket_path).await;
    // Whether the next command that needs the daemon replaces it.
    let mismatch = match &answer {
        DaemonAnswer::Answered(dv) => Some(!dv.is_current()),
        DaemonAnswer::Unreadable => Some(true),
        _ => None,
    };
    // `None`: no daemon. `Err`: one is there but did not say which build.
    let daemon = match &answer {
        DaemonAnswer::Absent => None,
        DaemonAnswer::Answered(dv) => Some(Ok(dv)),
        DaemonAnswer::Unreadable => Some(Err((
            "unreadable",
            "it answered without a version".to_string(),
        ))),
        DaemonAnswer::Unresponsive => Some(Err((
            "unresponsive",
            format!(
                "it did not answer within {}s",
                VERSION_CHECK_TIMEOUT.as_secs()
            ),
        ))),
        DaemonAnswer::Unreachable(e) => Some(Err(("unreachable", e.to_string()))),
    };

    match format {
        OutputFormat::Json => {
            let daemon = match &daemon {
                None => serde_json::Value::Null,
                Some(Ok(dv)) => serde_json::json!({
                    "status": "running",
                    "version": dv.version,
                    "git_sha": dv.git_sha,
                }),
                Some(Err((status, error))) => serde_json::json!({
                    "status": status,
                    "version": null,
                    "git_sha": null,
                    "error": error,
                }),
            };
            let envelope = serde_json::json!({
                "name": "shux",
                "version": cli.0,
                "git_sha": cli.1,
                "cli": {"version": cli.0, "git_sha": cli.1},
                "daemon": daemon,
                "mismatch": mismatch,
            });
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&envelope)?)
            );
        }
        OutputFormat::Plain => {
            let (status, found) = match &daemon {
                None => ("not-running", None),
                Some(Ok(dv)) => ("running", Some((dv.version.as_str(), dv.git_sha.as_str()))),
                Some(Err((status, _))) => (*status, None),
            };
            crate::style::print_version_plain(cli, status, found, mismatch);
        }
        OutputFormat::Text => {
            let replaced = |why: &str| {
                format!("{why}; the next shux command restarts it, ending its sessions")
            };
            match &daemon {
                None => {
                    crate::style::print_version(cli.0, Some(cli.1), Some("daemon not running"));
                }
                Some(Ok(dv)) => {
                    crate::style::print_version(cli.0, Some(cli.1), None);
                    let note = (mismatch == Some(true)).then(|| replaced("differs from this CLI"));
                    crate::style::print_daemon_version(
                        Some((&dv.version, &dv.git_sha)),
                        note.as_deref(),
                    );
                }
                Some(Err((_, error))) => {
                    crate::style::print_version(cli.0, Some(cli.1), None);
                    let note = if mismatch == Some(true) {
                        replaced(error)
                    } else {
                        error.clone()
                    };
                    crate::style::print_daemon_version(None, Some(&note));
                }
            }
        }
    }
    Ok(())
}
