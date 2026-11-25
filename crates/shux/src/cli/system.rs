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
    // This prints its answer, so inside a pane it never asks for a link,
    // whatever the params say of where it runs.
    let answer = if method == "share.create" && crate::cli::share::caller_pane().is_some() {
        Err(RpcClientError::Rpc {
            code: shux_rpc::ErrorCode::PermissionDenied.code(),
            message: "permission_denied".into(),
            data: Some(serde_json::json!({
                "reason": "in_pane",
                "detail": "inside a shux pane `shux rpc call share.create` would print the link; \
                           run `shux share`, which copies it to the clipboard",
            })),
        })
    } else {
        rpc_call(stream, method, params).await
    };

    // PR 3b: surface RPC errors as part of the JSON-RPC envelope on
    // stdout, not as a human-readable anyhow error on stderr. Callers
    // of `shux rpc call` are debug tools / agents that expect to parse the
    // raw `{result | error}` shape — including bounded `data` fields
    // like `expected_version` / `actual_version` for retry loops.
    match answer {
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
            let too_old = crate::client::DaemonTooOld::from_error(code, err_obj.get("data"));
            let envelope = serde_json::json!({"error": err_obj});
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&envelope)?)
            );
            // Non-zero exit so shell pipelines can branch, but the
            // structured error is still on stdout for parsers.
            if let Some(too_old) = too_old {
                crate::style::print_error(&too_old.to_string());
                std::process::exit(crate::client::DAEMON_TOO_OLD_EXIT);
            }
            std::process::exit(2);
        }
        Err(other) => Err(other.into()),
    }
}

/// Report a command refused because the daemon is another build that may hold
/// sessions, and exit [`crate::client::DAEMON_IN_USE_EXIT`]. With
/// `--format json` the refusal is the JSON document on stdout.
pub fn exit_daemon_in_use(refusal: &crate::client::DaemonInUse, format: OutputFormat) -> ! {
    use std::io::Write as _;
    match format {
        OutputFormat::Json => match serde_json::to_string_pretty(&refusal.to_json()) {
            Ok(s) => println!("{}", crate::style::json_safe(&s)),
            Err(e) => crate::style::print_error(&format!("{refusal} ({e})")),
        },
        OutputFormat::Text | OutputFormat::Plain => {
            crate::style::print_error(&refusal.to_string());
        }
    }
    let _ = std::io::stdout().flush();
    std::process::exit(crate::client::DAEMON_IN_USE_EXIT)
}

/// Report a call the running daemon, another build, cannot serve, and exit
/// [`crate::client::DAEMON_TOO_OLD_EXIT`]. With `--format json` the refusal is
/// the JSON document on stdout.
pub fn exit_daemon_too_old(refusal: &crate::client::DaemonTooOld, format: OutputFormat) -> ! {
    use std::io::Write as _;
    match format {
        OutputFormat::Json => match serde_json::to_string_pretty(&refusal.to_json()) {
            Ok(s) => println!("{}", crate::style::json_safe(&s)),
            Err(e) => crate::style::print_error(&format!("{refusal} ({e})")),
        },
        OutputFormat::Text | OutputFormat::Plain => {
            crate::style::print_error(&refusal.to_string());
        }
    }
    let _ = std::io::stdout().flush();
    std::process::exit(crate::client::DAEMON_TOO_OLD_EXIT)
}

/// Exits as the refusal `err` carries, if it carries one: a daemon of
/// another build left running, or one that cannot serve this call.
pub fn exit_if_build_refusal(err: &anyhow::Error, format: OutputFormat) {
    if let Some(refusal) = crate::client::DaemonInUse::find(err) {
        exit_daemon_in_use(refusal, format);
    }
    if let Some(refusal) = crate::client::DaemonTooOld::find(err) {
        exit_daemon_too_old(&refusal, format);
    }
}

/// Whether `err` carries a refusal [`exit_if_build_refusal`] exits on.
pub fn is_build_refusal(err: &anyhow::Error) -> bool {
    crate::client::DaemonInUse::find(err).is_some()
        || crate::client::DaemonTooOld::find(err).is_some()
}

/// `shux daemon upgrade`: replace the daemon serving `socket` with this build.
/// Exits 0 when it is done or will be done unasked, 1 when it was refused (the
/// daemon holds sessions, or is a newer release). Never starts a daemon.
pub async fn handle_daemon_upgrade(
    socket: &std::path::Path,
    when_idle: bool,
    force: bool,
    format: OutputFormat,
) -> anyhow::Result<()> {
    use crate::client::{Compat, DaemonAnswer};
    // A daemon of another wire generation is asked by its own build.
    if !force {
        crate::kept_build::hand_off(socket);
    }
    let this = format!("shux {} ({})", shux_rpc::VERSION, shux_rpc::GIT_SHA);
    let (answer, held) = crate::client::ask_daemon_version(socket).await;
    let dv = match answer {
        DaemonAnswer::Absent => {
            return upgrade_outcome(
                format,
                "absent",
                None,
                held,
                "no daemon is running",
                &format!("the next shux command starts {this}"),
            );
        }
        DaemonAnswer::Answered(dv) => Some(dv),
        DaemonAnswer::Unreadable if force => None,
        DaemonAnswer::Unreadable => anyhow::bail!(
            "the daemon serving this socket did not say which build it is; `daemon upgrade \
             --force` stops it"
        ),
        DaemonAnswer::Unresponsive => anyhow::bail!(
            "the daemon did not answer within {:?}; retry, or `daemon upgrade --force`",
            crate::patience::peer_patience()
        ),
        DaemonAnswer::Unreachable(e) => anyhow::bail!("could not reach the daemon: {e}"),
    };
    if let Some(dv) = dv.as_ref().filter(|dv| dv.is_current()) {
        return upgrade_outcome(
            format,
            "current",
            Some(dv),
            held,
            "the daemon is already this build",
            "nothing to do",
        );
    }
    if force {
        let Some(pid) = crate::daemon_boot::our_live_daemon(socket) else {
            anyhow::bail!("no shux daemon process serves this socket; stop whatever does");
        };
        if !crate::daemon_boot::stop_and_wait(pid, socket) {
            anyhow::bail!(crate::daemon_boot::not_gone(pid));
        }
        return upgrade_outcome(
            format,
            "stopped",
            dv.as_ref(),
            held,
            "stopped the daemon, ending its sessions",
            &format!("the next shux command starts {this}"),
        );
    }
    let Some(dv) = dv else {
        anyhow::bail!("the daemon did not say which build it is");
    };
    let sessions = crate::client::holding(held);
    if dv.is_newer_than_this_cli() {
        return upgrade_refused(
            format,
            "newer",
            &dv,
            held,
            "the daemon is a newer release than this CLI",
            &format!(
                "left as it is; `--force` ends its sessions and the next command starts this older {this}"
            ),
        );
    }
    match dv.compat() {
        Compat::Same | Compat::Wire => {
            let mut stream = crate::client::try_connect(socket).await?;
            let when = if when_idle { "idle" } else { "now" };
            let params = serde_json::json!({
                "when": when,
                "client": { "version": shux_rpc::VERSION, "git_sha": shux_rpc::GIT_SHA },
            });
            let r = rpc_call(&mut stream, shux_rpc::UPGRADE, params).await?;
            let flag = |k: &str| r.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
            if flag("retired") {
                upgrade_outcome(
                    format,
                    "retired",
                    Some(&dv),
                    held,
                    "the daemon held no sessions and is retiring",
                    &format!("the next shux command starts {this}"),
                )
            } else if flag("pending") {
                upgrade_outcome(
                    format,
                    "pending",
                    Some(&dv),
                    held,
                    &format!("the daemon retires when its last session ends ({sessions})"),
                    &format!("the first shux command after that starts {this}"),
                )
            } else {
                upgrade_refused(
                    format,
                    "busy",
                    &dv,
                    held,
                    &format!("the daemon is in use: {sessions}"),
                    "left running; `--when-idle` swaps it when its last session ends, `--force` ends them now",
                )
            }
        }
        Compat::Legacy064 | Compat::Legacy if when_idle || held == Some(0) => upgrade_outcome(
            format,
            "automatic",
            Some(&dv),
            held,
            &format!(
                "shux {} predates deferred upgrades, and is replaced on its own",
                dv.version
            ),
            &format!("the first shux command after its last session ends starts {this}"),
        ),
        Compat::Legacy064 | Compat::Legacy => upgrade_refused(
            format,
            "busy",
            &dv,
            held,
            &format!("the daemon is in use: {sessions}"),
            "left running; it is replaced by the first shux command after its last session ends, or `--force` ends them now",
        ),
        Compat::WireBreak => anyhow::bail!(
            "the daemon is shux {} ({}), which speaks another protocol, and no copy of its build \
             is left to ask it; `daemon upgrade --force` ends its sessions",
            dv.version,
            dv.git_sha
        ),
    }
}

fn compat_name(c: crate::client::Compat) -> &'static str {
    use crate::client::Compat;
    match c {
        Compat::Same => "same",
        Compat::Wire => "compatible",
        Compat::Legacy064 => "legacy_0_64",
        Compat::Legacy => "legacy",
        Compat::WireBreak => "wire_break",
    }
}

/// What the next command does with a daemon of another build, in words.
fn version_next(
    socket: &std::path::Path,
    found: Option<&crate::client::DaemonVersion>,
    held: Option<usize>,
) -> String {
    use crate::client::Compat;
    let holding = crate::client::holding(held);
    let upgrade = crate::client::upgrade_command(socket, "--when-idle");
    let Some(dv) = found else {
        return format!(
            "{holding}; shux commands refuse to run against it: `{}` ends them",
            crate::client::stop_command(socket)
        );
    };
    let pending = dv.answer.get("upgrade_pending").and_then(|p| p.as_bool()) == Some(true);
    match (dv.compat(), held) {
        (Compat::Same, _) => String::new(),
        (_, Some(0)) if !dv.is_newer_than_this_cli() => {
            "it holds no sessions now, so the next shux command replaces it with this build"
                .to_string()
        }
        (Compat::Wire, _) if pending => format!(
            "{holding}; commands run against it, and it hands over to this build when its \
             last session ends"
        ),
        (Compat::Wire, _) if dv.is_newer_than_this_cli() => format!(
            "a newer release; {holding}; commands run against it, and one it cannot serve says \
             so (exit 11)"
        ),
        (Compat::Wire, _) => format!(
            "{holding}; commands run against it, and one it cannot serve says so (exit 11): \
             `{upgrade}` swaps it for this build when its last session ends"
        ),
        (Compat::Legacy064, _) => format!(
            "{holding}; commands it can serve run against it, the rest say so (exit 11); it is \
             replaced by the first command after its last session ends"
        ),
        (Compat::WireBreak, _) => {
            format!("{holding}; it speaks another protocol, so commands are run by its own build")
        }
        (Compat::Legacy, _) => format!(
            "{holding}, so shux commands refuse to run against it (exit 10): it is replaced by \
             the first command after its last session ends, or `{}` ends them now",
            crate::client::stop_command(socket)
        ),
    }
}

fn upgrade_outcome(
    format: OutputFormat,
    outcome: &str,
    daemon: Option<&crate::client::DaemonVersion>,
    sessions: Option<usize>,
    headline: &str,
    next: &str,
) -> anyhow::Result<()> {
    print_upgrade(format, true, outcome, daemon, sessions, headline, next)
}

fn upgrade_refused(
    format: OutputFormat,
    outcome: &str,
    daemon: &crate::client::DaemonVersion,
    sessions: Option<usize>,
    headline: &str,
    next: &str,
) -> anyhow::Result<()> {
    print_upgrade(
        format,
        false,
        outcome,
        Some(daemon),
        sessions,
        headline,
        next,
    )?;
    std::process::exit(1)
}

fn print_upgrade(
    format: OutputFormat,
    done: bool,
    outcome: &str,
    daemon: Option<&crate::client::DaemonVersion>,
    sessions: Option<usize>,
    headline: &str,
    next: &str,
) -> anyhow::Result<()> {
    match format {
        OutputFormat::Json => {
            let doc = serde_json::json!({
                "outcome": outcome,
                "done": done,
                "daemon_version": daemon.map(|d| &d.version),
                "daemon_git_sha": daemon.map(|d| &d.git_sha),
                "client_version": shux_rpc::VERSION,
                "client_git_sha": shux_rpc::GIT_SHA,
                "sessions": sessions,
                "detail": format!("{headline}; {next}"),
            });
            println!(
                "{}",
                crate::style::json_safe(&serde_json::to_string_pretty(&doc)?)
            );
        }
        OutputFormat::Plain => println!("{outcome}\t{headline}\t{next}"),
        OutputFormat::Text => crate::style::print_upgrade(done, headline, next)?,
    }
    Ok(())
}

/// Handle the `shux version` command: this CLI's build, and the daemon's
/// beside it when one serves `socket_path`. Never starts a daemon.
pub async fn handle_version(
    socket_path: &std::path::Path,
    format: OutputFormat,
) -> anyhow::Result<()> {
    use crate::client::DaemonAnswer;

    let cli = (shux_rpc::VERSION, shux_rpc::GIT_SHA);
    // `daemon`: `None` when no daemon serves the socket, `Err` when one is
    // there but did not say which build. `mismatch`: whether it is not this
    // build, as far as it said. `held`: its sessions at the time of asking.
    let (answer, held) = crate::client::ask_daemon_version(socket_path).await;
    let (daemon, mismatch) = match answer {
        DaemonAnswer::Absent => (None, None),
        DaemonAnswer::Answered(dv) => {
            let stale = !dv.is_current();
            (Some(Ok(dv)), Some(stale))
        }
        DaemonAnswer::Unreadable => (
            Some(Err((
                "unreadable",
                "it did not say which build it is".to_string(),
            ))),
            Some(true),
        ),
        DaemonAnswer::Unresponsive => (
            Some(Err((
                "unresponsive",
                format!(
                    "it did not answer within {:?}; it may be busy, stopped or wedged",
                    crate::patience::peer_patience()
                ),
            ))),
            None,
        ),
        DaemonAnswer::Unreachable(e) => (Some(Err(("unreachable", e.to_string()))), None),
    };

    match format {
        OutputFormat::Json => {
            let daemon = match &daemon {
                None => serde_json::Value::Null,
                Some(Ok(dv)) => serde_json::json!({
                    "status": "running",
                    "version": dv.version,
                    "git_sha": dv.git_sha,
                    "sessions": held,
                    "wire": dv.wire,
                    "compat": compat_name(dv.compat()),
                    "upgrade_pending": dv.answer.get("upgrade_pending"),
                    "superseded_by": dv.answer.get("superseded_by"),
                    "methods": dv.answer.get("capabilities").and_then(|c| c.as_object()).map(|c| c.len()),
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
            crate::style::print_version_plain(cli, status, found, mismatch, held)?;
        }
        OutputFormat::Text => {
            crate::style::print_version(
                cli.0,
                Some(cli.1),
                daemon.is_none().then_some("daemon not running"),
            )?;
            // What the next command does, as `client::ensure_daemon_running_at`
            // decides it.
            let found = daemon.as_ref().and_then(|d| d.as_ref().ok());
            let next = if mismatch != Some(true) {
                String::new()
            } else if crate::daemon_boot::our_live_daemon(socket_path).is_none() {
                "no shux daemon process serves this socket, so shux commands \
                 refuse to run until whatever serves it is stopped"
                    .to_string()
            } else {
                version_next(socket_path, found, held)
            };
            match &daemon {
                None => {}
                Some(Ok(dv)) => {
                    let note =
                        (mismatch == Some(true)).then(|| format!("differs from this CLI; {next}"));
                    crate::style::print_daemon_version(
                        Some((&dv.version, &dv.git_sha)),
                        note.as_deref(),
                    )?;
                }
                Some(Err((_, error))) => {
                    let note = if mismatch == Some(true) {
                        format!("{error}; {next}")
                    } else {
                        error.clone()
                    };
                    crate::style::print_daemon_version(None, Some(&note))?;
                }
            }
        }
    }
    Ok(())
}
