use std::sync::Arc;

use clap::Parser;
use tokio::sync::{Notify, mpsc};
use tracing_subscriber::EnvFilter;

mod cli;
mod client;
mod daemon;
mod style;

use cli::{Cli, Command, OutputFormat};

fn main() -> anyhow::Result<()> {
    let args = Cli::parse();

    // Internal daemon subcommand — called by auto-start
    if matches!(args.command, Some(Command::__daemon)) {
        return run_daemon();
    }

    // Normal CLI client mode
    run_client(args)
}

/// Daemon entry point.
///
/// 1. Daemonize (double-fork) — BEFORE tokio runtime
/// 2. Create tokio runtime
/// 3. Set up CancellationToken tree
/// 4. Start signal handlers
/// 5. Bind UDS
/// 6. Run daemon state loop
fn run_daemon() -> anyhow::Result<()> {
    // Step 1: Daemonize BEFORE tokio
    if !daemon::daemonize()? {
        // We are the parent — exit cleanly
        return Ok(());
    }

    // Step 2: Now we are the daemon process — create tokio runtime
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        // Initialize tracing (to file, since stdio is /dev/null)
        // TODO: Set up file-based tracing subscriber
        tracing_subscriber::fmt()
            .with_env_filter("shux=info")
            .with_target(false)
            .init();

        let tokens = shux_core::daemon::ShutdownTokens::new();
        let config_reload_notify = Arc::new(Notify::new());
        let (cmd_tx, cmd_rx) = mpsc::channel(64);

        // Start signal handler
        daemon::spawn_signal_handler(cmd_tx.clone(), tokens.clone()).await?;

        // Ensure runtime dir and clean up stale socket
        daemon::ensure_runtime_dir()?;
        daemon::remove_socket_file()?;

        // TODO (task 012): Wire up full RPC server with SessionGraph
        // For now, bind UDS so client probe succeeds
        let sock_path = daemon::socket_path()?;
        let _listener = tokio::net::UnixListener::bind(&sock_path)?;
        tracing::info!(path = %sock_path.display(), "Daemon listening");

        // Run the daemon state loop (blocks until shutdown)
        shux_core::daemon::run_daemon_state_loop(cmd_rx, tokens.clone(), config_reload_notify)
            .await;

        // Cleanup
        daemon::remove_pid_file()?;
        daemon::remove_socket_file()?;
        tracing::info!("Daemon shut down cleanly");

        Ok::<(), anyhow::Error>(())
    })?;

    Ok(())
}

/// Client entry point — parse CLI args, ensure daemon is running, dispatch.
fn run_client(args: Cli) -> anyhow::Result<()> {
    // Set up logging
    let filter = if args.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::from_default_env()
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    let rt = tokio::runtime::Runtime::new()?;
    let result = rt.block_on(async { dispatch(args).await });
    if let Err(ref e) = result {
        style::print_error(&format!("{e:#}"));
    }
    result
}

/// Dispatch CLI subcommands.
async fn dispatch(args: Cli) -> anyhow::Result<()> {
    let socket_path = args.socket_path();

    match args.command {
        // No subcommand: attach to last session or create "default"
        None => {
            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;

            // For M0, create a default session and attach (stub).
            // Full "last session" logic comes in M1.
            let _result = cli::handle_new(
                &mut stream,
                Some("default".to_string()),
                None,
                false,
                OutputFormat::Text,
            )
            .await;

            // Attach via TUI client (wired in task 012)
            println!(
                "{}",
                style::muted("[TUI attach not yet wired — see task 012]")
            );
            Ok(())
        }

        Some(Command::New {
            session,
            ensure,
            detached,
            cmd,
        }) => {
            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
            let _result = cli::handle_new(&mut stream, session, cmd, ensure, args.format).await?;

            if !detached {
                // Attach via TUI client (wired in task 012)
                println!(
                    "{}",
                    style::muted("[TUI attach not yet wired — see task 012]")
                );
            }

            Ok(())
        }

        Some(Command::Attach { session }) => {
            let _stream = client::ensure_daemon_running_at(&socket_path).await?;
            let session_name = session.unwrap_or_else(|| "default".to_string());

            // Attach via TUI client (wired in task 012)
            println!(
                "{}",
                style::muted(format!(
                    "[TUI attach to '{session_name}' not yet wired — see task 012]"
                ))
            );
            Ok(())
        }

        Some(Command::Ls) => {
            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
            cli::handle_ls(&mut stream, args.format).await
        }

        Some(Command::Kill { session }) => {
            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
            cli::handle_kill(&mut stream, &session, args.format).await
        }

        Some(Command::Api { method, params }) => {
            let mut stream = client::ensure_daemon_running_at(&socket_path).await?;
            cli::handle_api(&mut stream, &method, &params, args.format).await
        }

        Some(Command::Version) => {
            // Try to get version from daemon; fall back to local version
            match client::ensure_daemon_running_at(&socket_path).await {
                Ok(mut stream) => cli::handle_version(&mut stream, args.format).await,
                Err(_) => {
                    match args.format {
                        OutputFormat::Json => {
                            println!("{{\"version\": \"{}\"}}", env!("CARGO_PKG_VERSION"));
                        }
                        OutputFormat::Text => {
                            style::print_version(
                                env!("CARGO_PKG_VERSION"),
                                Some("daemon not running"),
                            );
                        }
                    }
                    Ok(())
                }
            }
        }

        Some(Command::__daemon) => unreachable!("handled above"),
    }
}
