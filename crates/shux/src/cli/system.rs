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

/// `shux daemon upgrade`: replace the daemon serving `socket` with the
/// installed build. Exits 0 when it is done or will be done unasked, 1 when it
/// was refused (the daemon holds sessions, or is a newer release), 3 when the
/// daemon could not be asked. Never starts a daemon.
pub async fn handle_daemon_upgrade(
    socket: &std::path::Path,
    when_idle: bool,
    force: bool,
    format: OutputFormat,
) -> anyhow::Result<()> {
    use crate::client::{Compat, DaemonAnswer};
    let (version, sha) = crate::client::caller();
    let this = format!("shux {version} ({sha})");
    let starts = format!("the next shux command starts {this}");
    let mut report = Report {
        format,
        daemon: None,
        sessions: None,
    };
    if force {
        // Asked nothing first: a daemon that does not answer is ended too.
        let Some(pid) = crate::daemon_boot::our_live_daemon(socket) else {
            return report.say(0, "absent", "no shux daemon is running", &starts);
        };
        // A stopped process takes no SIGTERM until it runs again.
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGCONT,
        );
        if !crate::daemon_boot::stop_and_wait(pid, socket) {
            return report.say(3, "not_stopped", &crate::daemon_boot::not_gone(pid), "");
        }
        return report.say(
            0,
            "stopped",
            "stopped the daemon, ending its sessions",
            &starts,
        );
    }
    // A daemon of another wire generation is asked by its own build.
    crate::kept_build::hand_off(socket);
    let (answer, held, asked) = crate::client::ask_daemon_version(socket).await;
    report.sessions = held;
    let dv = match answer {
        DaemonAnswer::Absent => {
            return report.say(0, "absent", "no daemon is running", &starts);
        }
        DaemonAnswer::Answered(dv) => dv,
        DaemonAnswer::Unreadable => {
            return report.say(
                3,
                "unreadable",
                "the daemon serving this socket did not say which build it is",
                "`daemon upgrade --force` stops it",
            );
        }
        DaemonAnswer::Unresponsive => {
            let headline = format!(
                "the daemon did not answer within {:?}",
                crate::patience::peer_patience()
            );
            return report.say(
                3,
                "unresponsive",
                &headline,
                "retry, or `daemon upgrade --force`",
            );
        }
        DaemonAnswer::Unreachable(e) => {
            return report.say(
                3,
                "unreachable",
                &format!("could not reach the daemon: {e}"),
                "",
            );
        }
    };
    report.daemon = Some(dv.clone());
    if dv.is_current() && crate::kept_build::handed_to().is_none() {
        return report.say(
            0,
            "current",
            "the daemon is already this build",
            "nothing to do",
        );
    }
    let sessions = crate::client::holding(held);
    if dv.is_newer_than_this_cli() {
        return report.say(
            1,
            "newer",
            "the daemon is a newer release than this CLI",
            &format!("left as it is; `--force` ends its sessions and {starts}"),
        );
    }
    match dv.compat() {
        Compat::Same | Compat::Wire => {
            let Some(mut stream) = asked else {
                return report.say(
                    3,
                    "unresponsive",
                    "the daemon did not say how many sessions it holds",
                    "retry, or `daemon upgrade --force`",
                );
            };
            let mut params = crate::client::client_build();
            params["when"] = if when_idle { "idle" } else { "now" }.into();
            let r = rpc_call(&mut stream, shux_rpc::UPGRADE, params).await?;
            let flag = |k: &str| r.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
            if flag("retired") {
                report.say(0, "retired", "the daemon held no sessions and is retiring", &starts)
            } else if flag("pending") {
                report.say(
                    0,
                    "pending",
                    &format!("the daemon retires when its last session ends ({sessions})"),
                    &format!("the first shux command after that starts {this}"),
                )
            } else {
                report.say(
                    1,
                    "busy",
                    &format!("the daemon is in use: {sessions}"),
                    "it keeps running: `--when-idle` swaps it when its last session ends, `--force` ends them now",
                )
            }
        }
        Compat::Legacy064 | Compat::Legacy if held == Some(0) => report.say(
            0,
            "automatic",
            "the daemon holds no sessions",
            &format!("{starts}, replacing it"),
        ),
        Compat::Legacy064 | Compat::Legacy if when_idle => report.say(
            0,
            "automatic",
            &format!("shux {} predates deferred upgrades, and is replaced on its own", dv.version),
            &format!("the first shux command after its last session ends starts {this}"),
        ),
        Compat::Legacy064 | Compat::Legacy => report.say(
            1,
            "busy",
            &format!("the daemon is in use: {sessions}"),
            "it keeps running, and the first shux command after its last session ends replaces it; `--force` ends them now",
        ),
        Compat::WireBreak => report.say(
            3,
            "wire_break",
            &format!(
                "the daemon is shux {} ({}), which speaks another protocol, and no copy of its \
                 build is left to ask it",
                dv.version, dv.git_sha
            ),
            "`daemon upgrade --force` ends its sessions",
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

/// What `daemon upgrade` says, as one line or one JSON document.
struct Report {
    format: OutputFormat,
    daemon: Option<crate::client::DaemonVersion>,
    sessions: Option<usize>,
}

impl Report {
    /// Print the outcome, and exit `code` when it is not 0.
    fn say(&self, code: i32, outcome: &str, headline: &str, next: &str) -> anyhow::Result<()> {
        let daemon = self.daemon.as_ref();
        let detail = match next {
            "" => headline.to_string(),
            next => format!("{headline}; {next}"),
        };
        match self.format {
            OutputFormat::Json => {
                let doc = serde_json::json!({
                    "outcome": outcome,
                    "done": code == 0,
                    "exit_code": code,
                    "daemon_version": daemon.map(|d| &d.version),
                    "daemon_git_sha": daemon.map(|d| &d.git_sha),
                    "client_version": crate::client::caller().0,
                    "client_git_sha": crate::client::caller().1,
                    "sessions": self.sessions,
                    "detail": detail,
                });
                println!(
                    "{}",
                    crate::style::json_safe(&serde_json::to_string_pretty(&doc)?)
                );
            }
            OutputFormat::Plain => println!("{outcome}\t{headline}\t{next}"),
            OutputFormat::Text => crate::style::print_upgrade(code == 0, headline, next)?,
        }
        if code != 0 {
            use std::io::Write as _;
            let _ = std::io::stdout().flush();
            std::process::exit(code);
        }
        Ok(())
    }
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
    let (answer, held, _) = crate::client::ask_daemon_version(socket_path).await;
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
                    "build": dv.answer.get("build"),
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
