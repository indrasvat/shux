//! The CLI client: one `match` arm per subcommand, each a thin JSON-RPC call.
//!
//! `dispatch` is where "CLI == API" (PRD §4.3) is actually true — every arm
//! resolves its arguments and hands off to a `cli::handle_*` function.

use crate::cli::{Cli, Command, OutputFormat, PaneCommand, WindowCommand};
use tracing_subscriber::EnvFilter;

use crate::daemon_boot::handle_daemon_command;
use crate::{cli, client, daemon, features, gate, pane_command, style, template};

/// Client entry point — parse CLI args, ensure daemon is running, dispatch.
pub(crate) fn run_client(args: Cli) -> anyhow::Result<()> {
    // Set up logging
    let filter = if args.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::from_default_env()
    };
    // 085 F22: logs go to STDERR, never stdout. stdout is the DATA channel — `--format
    // json` and `lens gate --report -` promise it carries only the payload — and the old
    // default writer broke that contract exactly when someone reached for `-v` to debug,
    // emitting ANSI-coloured DEBUG lines ahead of the JSON.
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    let rt = tokio::runtime::Runtime::new()?;
    // Rendering belongs to `report_fatal`, which is the single place an error
    // reaches the operator. Printing here as well as returning the `Err` is
    // what produced the double-print in issue #133.
    rt.block_on(async { dispatch(args).await })
}

/// Decide which session `shux` (no args) should attach to.
///
/// Strategy: query the daemon for sessions; if any exist, pick the most
/// recently created. If none, fall through to "default" (which the
/// daemon-side attach handler will create on demand).
/// Choose the session a bare `shux` / `shux attach` lands on from a
/// `session.list` result. NEVER a scratch session (lens PRD LENS-R-041 —
/// P5 round-1 minor: scratch panes are agent working surfaces; a human
/// attaching blind must not land inside one). Defense in depth: the
/// default `session.list` already omits scratch, and this filter also
/// rejects `scratch: true` flags and the reserved `__scratch-` name prefix
/// in case a future caller feeds an `--include-scratch` listing through.
pub(crate) fn choose_attach_session(sessions: &[serde_json::Value]) -> Option<String> {
    sessions
        .iter()
        .filter(|s| s.get("scratch").and_then(|v| v.as_bool()) != Some(true))
        .filter_map(|s| s.get("name")?.as_str())
        .find(|name| !name.starts_with("__scratch-"))
        .map(str::to_string)
}

pub(crate) async fn pick_attach_target(socket_path: &std::path::Path) -> String {
    if let Ok(mut stream) = client::try_connect(socket_path).await
        && let Ok(value) = cli::rpc_call(&mut stream, "session.list", serde_json::json!({})).await
        && let Some(arr) = value.get("sessions").and_then(|v| v.as_array())
        && let Some(target) = choose_attach_session(arr)
    {
        return target;
    }
    "default".to_string()
}

pub(crate) fn default_session_name() -> String {
    "default".to_string()
}

/// Run the attach TUI client. Translates the daemon's `attach.sock` into
/// real keystrokes / ANSI bytes on the user's terminal. Restores the
/// terminal on every exit path via `TerminalGuard`'s Drop.
pub(crate) async fn run_attach(
    _jsonrpc_socket: &std::path::Path,
    session_name: String,
) -> anyhow::Result<()> {
    let attach_path = daemon::attach_socket_path()?;
    let cfg_snapshot =
        shux_core::config::ConfigHandle::load_or_default(&shux_core::config::default_config_path())
            .current();
    let cfg = shux_ui::ClientConfig {
        socket_path: attach_path.to_string_lossy().to_string(),
        session_name: session_name.clone(),
        prefix: cfg_snapshot.keys.prefix.clone(),
        prefix_key: crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(' '),
            crossterm::event::KeyModifiers::CONTROL,
        ),
        keybindings: cfg_snapshot.keybindings.clone(),
    };
    match shux_ui::attach::run_attach(&attach_path, cfg).await {
        Ok(reason) => {
            match reason {
                shux_ui::ExitReason::Detached => {
                    println!("[detached from session '{session_name}']");
                }
                shux_ui::ExitReason::SessionEnded => {
                    println!("[session '{session_name}' ended]");
                }
                shux_ui::ExitReason::ConnectionLost => {
                    eprintln!("[connection to daemon lost]");
                }
                shux_ui::ExitReason::Error(msg) => {
                    eprintln!("[attach error: {msg}]");
                }
            }
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Dispatch CLI subcommands.
pub(crate) async fn dispatch(args: Cli) -> anyhow::Result<()> {
    let socket_path = args.socket_path();

    match args.command {
        // No subcommand: attach to last session (TTY only). On a
        // non-TTY stdin OR stdout (piped, CI, redirected), don't
        // block — print structured help so scripts get a deterministic
        // response. Attach drives crossterm raw-mode keyboard input,
        // so `shux </dev/null` (stdout-tty + stdin-piped) would
        // hang on the input thread. Guard on BOTH. (Codex council
        // May 2026 + codex bot review of PR #24.)
        None => {
            use std::io::IsTerminal;
            if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
                let help = serde_json::json!({
                    "shux": env!("CARGO_PKG_VERSION"),
                    "help": "Run `shux --help` to see commands. \
                             `shux` with no args attaches to the last session — \
                             but only when BOTH stdin and stdout are a TTY. Try \
                             `shux session list` or `shux rpc call session.list`.",
                    "common_commands": [
                        "shux session create <NAME>",
                        "shux session list",
                        "shux session attach <NAME>",
                        "shux session kill <NAME>",
                        "shux window create -s <SESSION>",
                        "shux pane send-keys -s <SESSION> --text '...'",
                        "shux pane snapshot",
                        "shux plugin install <PATH>",
                        "shux state apply <template.toml>",
                        "shux rpc call <method> --params @file"
                    ]
                });
                println!(
                    "{}",
                    style::json_safe(&serde_json::to_string_pretty(&help)?)
                );
                return Ok(());
            }
            // Recursion guard. Every pane shux spawns gets `SHUX=1`
            // injected (mirrors tmux's `TMUX` env var, see
            // crates/shux-pty defaults). Without a guard here, bare
            // `shux` inside a pane attaches the current TTY to its own
            // daemon — instant render-loop hall-of-mirrors. Mirrors
            // tmux's terse refusal: one line, suggest the env unset.
            if std::env::var_os("SHUX").is_some() {
                eprintln!("sessions should be nested with care, unset $SHUX to force");
                std::process::exit(1);
            }
            let _ = client::ensure_daemon_running_at(&socket_path).await?;
            let session_name = pick_attach_target(&socket_path).await;
            run_attach(&socket_path, session_name).await
        }

        // `shux session <verb>` — canonical session lifecycle.
        // Mirrors `session.*` RPC namespace (`session.create` ↔
        // `shux session create`, etc.). Codex council May 2026
        // established this as the agent-first invariant: RPC dots
        // become CLI spaces, no top-level shortcut verbs.
        Some(Command::Session { command: sc }) => match sc {
            cli::SessionCommand::Create {
                name,
                session,
                ensure,
                detached,
                cwd,
                title,
                cmd,
                argv,
            } => {
                let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
                let resolved = name.or(session);
                let session_name = resolved.clone().unwrap_or_else(default_session_name);
                let _ = cli::handle_new(
                    &mut stream,
                    cli::SessionCreateOptions {
                        session_name: resolved,
                        cwd,
                        title,
                        cmd,
                        argv,
                        ensure,
                    },
                    args.format,
                )
                .await?;
                drop(stream);
                if !detached {
                    run_attach(&socket_path, session_name).await
                } else {
                    Ok(())
                }
            }
            cli::SessionCommand::List { include_scratch } => {
                let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
                cli::handle_ls(&mut stream, include_scratch, args.format).await
            }
            cli::SessionCommand::Kill {
                name_pos,
                session,
                expected_version,
            } => {
                let resolved = name_pos.or(session).ok_or_else(|| {
                    anyhow::anyhow!(
                        "missing session name: pass it as a positional or via -s/--session"
                    )
                })?;
                let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
                cli::handle_kill(&mut stream, &resolved, expected_version, args.format).await
            }
            cli::SessionCommand::Rename {
                session,
                name,
                expected_version,
            } => {
                let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
                cli::handle_rename(&mut stream, &session, &name, expected_version, args.format)
                    .await
            }
            cli::SessionCommand::Attach { name_pos, session } => {
                // Refuse before touching the terminal. `run_attach` drives
                // crossterm raw mode, and crossterm opens `/dev/tty` DIRECTLY —
                // it resolves through the process's controlling terminal and
                // ignores stdin/stdout. So piping a child's stdio does not stop
                // it: with a controlling terminal present it grabbed the tty and
                // sat in the interactive loop forever, and without one it died
                // on `enter raw mode: No such device or address` after already
                // emitting escape sequences and a 35-frame backtrace.
                //
                // The bare-`shux` path at the `None` arm above has had this
                // guard since a council + bot review of PR #24, for exactly this
                // hang. The subcommand was missed. Same test, same reason.
                use std::io::IsTerminal;
                if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
                    anyhow::bail!(
                        "`session attach` needs a terminal: it takes over the screen \
                         and reads keys, so it cannot run with stdin or stdout \
                         redirected. Run it from a terminal, or use \
                         `shux pane capture`/`shux pane snapshot` to read a session \
                         non-interactively."
                    );
                }
                let _ = client::ensure_daemon_running_at(&socket_path).await?;
                let session_name = name_pos
                    .or(session)
                    .unwrap_or_else(|| "default".to_string());
                run_attach(&socket_path, session_name).await
            }
            cli::SessionCommand::Snapshot {
                session,
                output,
                cols,
                rows,
            } => {
                let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
                // session.snapshot dispatch: (Some(session), None) → handle_snapshot
                // routes to session.snapshot RPC.
                cli::handle_snapshot(
                    &mut stream,
                    Some(&session),
                    None,
                    output,
                    cols,
                    rows,
                    args.format,
                )
                .await
            }
            cli::SessionCommand::Save { session, output } => {
                let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
                cli::handle_session_save(&mut stream, &session, output).await
            }
            cli::SessionCommand::Restore {
                template,
                dry_run,
                watch,
            } => {
                // Identical wording and identical funnel to `state apply` —
                // see the comment there (issue #137).
                let ops = template::load_and_lower(&template)
                    .map_err(|e| anyhow::Error::from(e).context("template error"))?;
                pane_command::validate_ops(&ops)
                    .map_err(|e| anyhow::anyhow!("template error: {}", rpc_error_detail(&e)))?;
                if dry_run {
                    // Same payload, same shape as `state apply --dry-run`:
                    // both lower through `template::load_and_lower` and both
                    // preview what goes on the wire to `state.apply`, so a
                    // consumer should not have to know which verb produced it
                    // (issue #137).
                    println!(
                        "{}",
                        style::json_safe(&serde_json::to_string_pretty(
                            &serde_json::json!({"ops": ops})
                        )?)
                    );
                    Ok(())
                } else {
                    let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
                    cli::handle_apply(&mut stream, ops, watch, &socket_path, args.format).await
                }
            }
        },

        Some(Command::Window { command }) => {
            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
            match command {
                WindowCommand::List { session } => {
                    cli::handle_window_list(&mut stream, &session, args.format).await
                }
                WindowCommand::Create {
                    session,
                    name,
                    cwd,
                    cmd,
                    ensure,
                    argv,
                } => {
                    cli::handle_window_new(
                        &mut stream,
                        &session,
                        name,
                        cwd,
                        cmd,
                        argv,
                        ensure,
                        args.format,
                    )
                    .await
                }
                WindowCommand::Kill {
                    session,
                    window,
                    expected_version,
                } => {
                    cli::handle_window_kill(
                        &mut stream,
                        &session,
                        &window,
                        expected_version,
                        args.format,
                    )
                    .await
                }
                WindowCommand::Rename {
                    session,
                    window,
                    name,
                    expected_version,
                } => {
                    cli::handle_window_rename(
                        &mut stream,
                        &session,
                        &window,
                        &name,
                        expected_version,
                        args.format,
                    )
                    .await
                }
                WindowCommand::Focus {
                    session,
                    window,
                    expected_version,
                } => {
                    cli::handle_window_focus(
                        &mut stream,
                        &session,
                        &window,
                        expected_version,
                        args.format,
                    )
                    .await
                }
                WindowCommand::Reorder {
                    session,
                    window,
                    index,
                    expected_version,
                } => {
                    cli::handle_window_reorder(
                        &mut stream,
                        &session,
                        &window,
                        index,
                        expected_version,
                        args.format,
                    )
                    .await
                }
                WindowCommand::Snapshot {
                    session,
                    window,
                    output,
                    cols,
                    rows,
                } => {
                    cli::handle_snapshot(
                        &mut stream,
                        session.as_deref(),
                        window.as_deref(),
                        output,
                        cols,
                        rows,
                        args.format,
                    )
                    .await
                }
            }
        }

        Some(Command::Pane { command }) => {
            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
            match command {
                PaneCommand::List { session, window } => {
                    cli::handle_pane_list(&mut stream, &session, window.as_deref(), args.format)
                        .await
                }
                PaneCommand::Split {
                    session,
                    window,
                    pane,
                    direction,
                    ratio,
                    cmd,
                    argv,
                } => {
                    cli::handle_pane_split(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        pane.as_deref(),
                        direction.as_deref(),
                        ratio,
                        cmd,
                        argv,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Focus {
                    session,
                    window,
                    pane,
                } => {
                    cli::handle_pane_focus(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        &pane,
                        args.format,
                    )
                    .await
                }
                PaneCommand::FocusDir {
                    session,
                    window,
                    direction,
                } => {
                    cli::handle_pane_focus_dir(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        &direction,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Resize {
                    session,
                    window,
                    pane,
                    direction,
                    delta,
                    expected_version,
                } => {
                    cli::handle_pane_resize(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        pane.as_deref(),
                        &direction,
                        delta,
                        expected_version,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Zoom {
                    session,
                    window,
                    pane,
                    expected_version,
                } => {
                    cli::handle_pane_zoom(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        pane.as_deref(),
                        expected_version,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Swap {
                    session,
                    window,
                    pane,
                    target,
                    expected_version,
                } => {
                    cli::handle_pane_swap(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        &pane,
                        &target,
                        expected_version,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Kill {
                    session,
                    window,
                    pane,
                    expected_version,
                } => {
                    cli::handle_pane_kill(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        &pane,
                        expected_version,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Title {
                    session,
                    window,
                    pane,
                    title,
                    clear,
                    auto,
                    no_auto,
                } => {
                    cli::handle_pane_title(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        pane.as_deref(),
                        title.as_deref(),
                        clear,
                        auto,
                        no_auto,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Watch {
                    session,
                    pane,
                    timeout_ms,
                    limit,
                } => {
                    cli::handle_pane_watch(
                        &mut stream,
                        &session,
                        &pane,
                        timeout_ms,
                        limit,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Record {
                    session,
                    pane,
                    to,
                    force,
                    duration_ms,
                } => {
                    cli::handle_pane_record(
                        &mut stream,
                        &session,
                        &pane,
                        &to,
                        force,
                        duration_ms,
                        args.format,
                    )
                    .await
                }
                PaneCommand::SendKeys {
                    session,
                    window,
                    pane,
                    text,
                    data,
                } => {
                    cli::handle_pane_send_keys(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        pane.as_deref(),
                        text.as_deref(),
                        data.as_deref(),
                        args.format,
                    )
                    .await
                }
                PaneCommand::Run {
                    session,
                    window,
                    pane,
                    command,
                    timeout,
                    is_async,
                } => {
                    cli::handle_pane_run(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        pane.as_deref(),
                        &command,
                        timeout,
                        is_async,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Capture {
                    session,
                    window,
                    pane,
                    lines,
                } => {
                    cli::handle_pane_capture(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        pane.as_deref(),
                        lines,
                        args.format,
                    )
                    .await
                }
                PaneCommand::WaitFor {
                    session,
                    window,
                    pane,
                    text,
                    regex,
                    absent,
                    lines,
                    timeout_ms,
                    poll_ms,
                } => {
                    cli::handle_wait_for(
                        &mut stream,
                        session.as_deref(),
                        window.as_deref(),
                        pane.as_deref(),
                        text.as_deref(),
                        regex.as_deref(),
                        absent,
                        lines,
                        timeout_ms,
                        poll_ms,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Snapshot {
                    session,
                    window,
                    pane,
                    output,
                } => {
                    cli::handle_pane_snapshot(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        pane.as_deref(),
                        output,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Glance {
                    pane,
                    png,
                    text_only,
                    no_cursor,
                    checkpoint,
                    cells,
                    cells_out,
                    masks,
                } => {
                    cli::handle_pane_glance(
                        &mut stream,
                        &pane,
                        png,
                        text_only,
                        no_cursor,
                        checkpoint,
                        cells || cells_out.is_some(),
                        cells_out,
                        masks,
                        args.format,
                    )
                    .await
                }
                PaneCommand::WaitSettled {
                    pane,
                    quiet,
                    timeout,
                    hold_ms,
                    stable_frames,
                } => {
                    cli::handle_pane_wait_settled(
                        &mut stream,
                        &pane,
                        quiet,
                        timeout,
                        hold_ms,
                        stable_frames,
                        args.format,
                    )
                    .await
                }
                PaneCommand::Checkpoint { pane } => {
                    cli::handle_pane_checkpoint(&mut stream, &pane, args.format).await
                }
                PaneCommand::Diff {
                    pane,
                    since,
                    heat,
                    no_row_text,
                } => {
                    cli::handle_pane_diff(&mut stream, &pane, since, heat, no_row_text, args.format)
                        .await
                }
                PaneCommand::SetSize {
                    session,
                    window,
                    pane,
                    cols,
                    rows,
                } => {
                    cli::handle_pane_set_size(
                        &mut stream,
                        &session,
                        window.as_deref(),
                        pane.as_deref(),
                        cols,
                        rows,
                        args.format,
                    )
                    .await
                }
            }
        }

        Some(Command::Lens {
            command:
                cli::LensCommand::Run {
                    size,
                    ttl,
                    max_runtime,
                    env,
                    cwd,
                    wait,
                    argv,
                },
        }) => {
            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
            cli::handle_lens_run(
                &mut stream,
                &argv,
                size,
                ttl,
                max_runtime,
                &env,
                cwd.as_deref(),
                wait,
                args.format,
            )
            .await
        }

        Some(Command::Lens {
            command:
                cli::LensCommand::Gate {
                    scenario,
                    golden_dir,
                    report,
                    on_missing,
                    update,
                    reason,
                    tol,
                    out,
                    retries,
                    cast,
                    trace,
                    sub,
                    argv,
                },
        }) => {
            let code = match sub {
                Some(cli::GateSubcommand::Review {
                    scenario,
                    golden_dir,
                    out,
                }) => gate::review::run_review(&socket_path, scenario, golden_dir, out).await?,
                Some(cli::GateSubcommand::Init { name, dir }) => {
                    gate::init::run_init(&socket_path, name, dir).await?
                }
                None => {
                    let scenario_path = scenario.ok_or_else(|| {
                        anyhow::anyhow!(
                            "lens gate: a SCENARIO is required (or use `review`/`init`)"
                        )
                    })?;
                    let opts = gate::driver::GateRunOptions {
                        scenario_path,
                        golden_dir,
                        report,
                        on_missing,
                        update,
                        reason,
                        tol,
                        out,
                        retries,
                        cast,
                        trace,
                        argv,
                        format: args.format,
                    };
                    gate::driver::run_gate(&socket_path, opts).await?
                }
            };
            std::process::exit(code);
        }

        Some(Command::Rpc {
            command: cli::RpcCommand::Call { method, params },
        }) => {
            // Resolve `--params` source: inline JSON, `@<file>`, or `-` (stdin).
            // Codex council May 2026: eliminate shell-escaping bait for JSON.
            let resolved = if params == "-" {
                let mut buf = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
                buf
            } else if let Some(path) = params.strip_prefix('@') {
                std::fs::read_to_string(path)?
            } else {
                params
            };
            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
            cli::handle_api(&mut stream, &method, &resolved, args.format).await
        }

        Some(Command::Daemon { command }) => handle_daemon_command(command, args.format),

        Some(Command::Version) => {
            // Quick probe — don't auto-start daemon just for version
            match client::try_connect(&socket_path).await {
                Ok(mut stream) => cli::handle_version(&mut stream, args.format).await,
                Err(_) => {
                    match args.format {
                        OutputFormat::Json => {
                            println!(
                                "{{\"version\": \"{}\", \"git_sha\": \"{}\"}}",
                                env!("CARGO_PKG_VERSION"),
                                env!("SHUX_GIT_SHA"),
                            );
                        }
                        OutputFormat::Text | OutputFormat::Plain => {
                            style::print_version(
                                env!("CARGO_PKG_VERSION"),
                                Some(env!("SHUX_GIT_SHA")),
                                Some("daemon not running"),
                            );
                        }
                    }
                    Ok(())
                }
            }
        }

        Some(Command::Config { command: cfg_cmd }) => match cfg_cmd {
            cli::ConfigCommand::Init { force } => cli::handle_config_init(force),
            cli::ConfigCommand::ResetHints => cli::handle_config_reset_hints(),
            cli::ConfigCommand::Path => cli::handle_config_path(),
            cli::ConfigCommand::Show => cli::handle_config_show(),
            cli::ConfigCommand::Validate { path, config } => {
                // Either positional or `--config` (mutually exclusive at the
                // clap layer); fold to a single Option for the handler.
                let code = cli::handle_config_validate(path.or(config))?;
                std::process::exit(code);
            }
        },

        Some(Command::Plugin { command: pl_cmd }) => {
            features::plugin::dispatch(pl_cmd, &socket_path, args.format).await
        }

        Some(Command::Events { command: ev_cmd }) => {
            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
            match ev_cmd {
                cli::EventsCommand::Watch {
                    filter,
                    from_seq,
                    timeout_ms,
                    limit,
                } => {
                    cli::handle_events_watch(&mut stream, filter, from_seq, timeout_ms, limit).await
                }
                cli::EventsCommand::History { filter, count } => {
                    cli::handle_events_history(&mut stream, filter, count).await
                }
            }
        }

        Some(Command::State {
            command:
                cli::StateCommand::Apply {
                    template,
                    dry_run,
                    watch,
                },
        }) => {
            // Lower the TOML template to apply ops first (no daemon needed
            // for parse / validate). If --dry-run, print the lowered ops as
            // pretty JSON and exit.
            // Both template verbs report a bad template the same way, through
            // the same funnel: `report_fatal` renders the anyhow chain once
            // and `style::print_error` sanitizes it, which matters because the
            // TOML diagnostic quotes the offending source line verbatim and
            // this runs before the daemon exists, so ingress sanitizing cannot
            // reach it (issue #104). Previously `state apply` prefixed
            // `template error:` here while `session restore` propagated the
            // bare error, so the same broken template read differently
            // depending on which verb you typed (issue #137).
            let ops = template::load_and_lower(&template)
                .map_err(|e| anyhow::Error::from(e).context("template error"))?;

            // Before printing OR sending: `--dry-run` exists to answer "will
            // this apply succeed?", and the argv rule used to live only in the
            // daemon, so dry-run said yes to templates the real run rejects.
            pane_command::validate_ops(&ops)
                .map_err(|e| anyhow::anyhow!("template error: {}", rpc_error_detail(&e)))?;

            if dry_run {
                // `--dry-run` prints the ops BEFORE the graph sanitizes
                // them — it is the one place a hostile title is meant to
                // be shown verbatim, so it must be shown inertly.
                println!(
                    "{}",
                    style::json_safe(&serde_json::to_string_pretty(
                        &serde_json::json!({"ops": ops})
                    )?)
                );
                return Ok(());
            }

            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
            cli::handle_apply(&mut stream, ops, watch, &socket_path, args.format).await
        }

        Some(Command::Init { dir }) => {
            let root = dir.unwrap_or_else(|| std::path::PathBuf::from("."));
            cli::handle_init(&root, args.format)
        }

        Some(Command::__daemon) => unreachable!("handled above"),
    }
}

/// The human-readable half of an `RpcError`, for CLI-side reporting.
///
/// `RpcError`'s `Display` is the JSON-RPC code name (`invalid_params`); the
/// sentence a person needs is in `data.detail`.
fn rpc_error_detail(e: &shux_rpc::RpcError) -> String {
    serde_json::to_value(e)
        .ok()
        .and_then(|v| {
            v.get("data")
                .and_then(|d| d.get("detail"))
                .and_then(|d| d.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bare `shux` / `shux attach` target choice never lands on a scratch
    /// session (P5 round-1 minor — attach guard), whether flagged
    /// `scratch: true` or recognizable by the reserved name prefix.
    #[test]
    fn choose_attach_session_never_picks_scratch() {
        // Scratch-only listing → None (fall through to "default").
        let scratch_only = vec![
            serde_json::json!({"name": "__scratch-abc", "scratch": true}),
            serde_json::json!({"name": "__scratch-def"}),
        ];
        assert_eq!(choose_attach_session(&scratch_only), None);

        // Mixed listing → first NON-scratch name, regardless of order.
        let mixed = vec![
            serde_json::json!({"name": "__scratch-abc", "scratch": true}),
            serde_json::json!({"name": "work"}),
            serde_json::json!({"name": "other"}),
        ];
        assert_eq!(choose_attach_session(&mixed), Some("work".to_string()));

        // Flag wins even when the name looks ordinary.
        let flagged = vec![
            serde_json::json!({"name": "sneaky", "scratch": true}),
            serde_json::json!({"name": "real"}),
        ];
        assert_eq!(choose_attach_session(&flagged), Some("real".to_string()));

        assert_eq!(choose_attach_session(&[]), None);
    }
}
