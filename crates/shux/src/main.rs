use std::sync::Arc;

use tokio::sync::{Notify, mpsc};

mod client;
mod daemon;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    // Internal daemon subcommand — called by auto-start
    if args.get(1).map(|s| s.as_str()) == Some("__daemon") {
        return run_daemon();
    }

    // Normal CLI client mode
    run_client()
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

        // TODO (task 008): Bind UDS and start JSON-RPC server
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

/// Client entry point — ensure daemon is running, then execute CLI command.
fn run_client() -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let _stream = client::ensure_daemon_running().await?;

        // TODO (task 011): Parse CLI args with clap, dispatch JSON-RPC calls
        println!("shux v{}", env!("CARGO_PKG_VERSION"));

        Ok::<(), anyhow::Error>(())
    })?;

    Ok(())
}
