use std::path::PathBuf;
use std::sync::Arc;

use clap::{CommandFactory, FromArgMatches};
use tokio::sync::{Notify, mpsc};
use tracing_subscriber::EnvFilter;

mod cli;
mod client;
mod daemon;
mod style;

use cli::{Cli, Command, OutputFormat};

fn main() -> anyhow::Result<()> {
    let cmd = Cli::command().before_help(style::banner());
    let matches = cmd.get_matches();
    let args = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());

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

        // Set up SessionGraph + graph loop
        let sock_path = daemon::socket_path()?;
        let cancel = tokens.root.clone();
        run_rpc_server(sock_path, cancel.clone()).await?;

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

/// Start the RPC server with a SessionGraph backing session methods.
///
/// Spawns:
/// 1. The SessionGraph graph loop (single-writer task)
/// 2. The RPC Server accept loop
///
/// Both run until `cancel` is triggered.
async fn run_rpc_server(
    socket_path: PathBuf,
    cancel: tokio_util::sync::CancellationToken,
) -> anyhow::Result<()> {
    // Create SessionGraph + graph loop
    let (graph, state) = shux_core::graph::SessionGraph::new();
    let (graph_tx, graph_rx) = mpsc::channel(256);
    let graph_handle = shux_core::graph::GraphHandle::new(graph_tx, state);

    let graph_cancel = cancel.clone();
    tokio::spawn(async move {
        shux_core::graph::run_graph_loop(graph, graph_rx, graph_cancel).await;
    });

    // Build router: system builtins + session methods backed by GraphHandle
    let router = register_session_methods(
        shux_rpc::server::register_builtin_methods(shux_rpc::Router::builder()),
        graph_handle,
    )
    .build();

    let config = shux_rpc::ServerConfig {
        socket_path,
        tcp_addr: String::new(),
        auth_token: None,
    };

    let server = shux_rpc::Server::new(config, router, cancel);

    tokio::spawn(async move {
        if let Err(e) = server.run().await {
            tracing::error!(error = %e, "RPC server error");
        }
    });

    Ok(())
}

/// Register session CRUD methods on the router builder.
///
/// These methods use a `GraphHandle` to interact with the SessionGraph.
/// They are registered here (in the binary crate) because shux-rpc
/// intentionally does not depend on shux-core.
fn register_session_methods(
    builder: shux_rpc::RouterBuilder,
    graph: shux_core::graph::GraphHandle,
) -> shux_rpc::RouterBuilder {
    let g1 = graph.clone();
    let g2 = graph.clone();
    let g3 = graph.clone();
    let g4 = graph.clone();

    builder
        .register("session.list", move |_params: Option<serde_json::Value>| {
            let gh = g1.clone();
            async move {
                let snap = gh.snapshot();
                let sessions: Vec<serde_json::Value> = snap
                    .sessions
                    .values()
                    .map(|s| {
                        serde_json::json!({
                            "id": s.id.to_string(),
                            "name": s.name,
                            "windows": s.windows.iter().map(|w| w.to_string()).collect::<Vec<_>>(),
                            "created_at": format!("{:?}", s.created_at),
                        })
                    })
                    .collect();
                Ok(serde_json::json!({ "sessions": sessions }))
            }
        })
        .register(
            "session.create",
            move |params: Option<serde_json::Value>| {
                let gh = g2.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let name = params
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("default")
                        .to_string();

                    let cwd =
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));

                    match gh.create_session(name, cwd).await {
                        Ok(session_id) => {
                            let snap = gh.snapshot();
                            if let Some(s) = snap.sessions.get(&session_id) {
                                Ok(serde_json::json!({
                                    "id": s.id.to_string(),
                                    "name": s.name,
                                    "windows": s.windows.iter().map(|w| w.to_string()).collect::<Vec<_>>(),
                                    "created_at": format!("{:?}", s.created_at),
                                }))
                            } else {
                                Ok(serde_json::json!({
                                    "id": session_id.to_string(),
                                }))
                            }
                        }
                        Err(e) => Err(shux_rpc::RpcError::with_message(
                            shux_rpc::ErrorCode::InternalError,
                            e.to_string(),
                        )),
                    }
                }
            },
        )
        .register("session.kill", move |params: Option<serde_json::Value>| {
            let gh = g3.clone();
            async move {
                let params = params.unwrap_or_default();
                let name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        shux_rpc::RpcError::with_message(
                            shux_rpc::ErrorCode::InvalidParams,
                            "missing 'name' parameter".to_string(),
                        )
                    })?;

                let snap = gh.snapshot();
                let session = snap.find_session_by_name(name).ok_or_else(|| {
                    shux_rpc::RpcError::with_message(
                        shux_rpc::ErrorCode::InternalError,
                        format!("session not found: {name}"),
                    )
                })?;
                let session_id = session.id;

                gh.destroy_session(session_id, None).await.map_err(|e| {
                    shux_rpc::RpcError::with_message(
                        shux_rpc::ErrorCode::InternalError,
                        e.to_string(),
                    )
                })?;

                Ok(serde_json::json!({ "killed": name }))
            }
        })
        .register(
            "session.ensure",
            move |params: Option<serde_json::Value>| {
                let gh = g4.clone();
                async move {
                    let params = params.unwrap_or_default();
                    let name = params
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("default")
                        .to_string();

                    // Check if session already exists
                    let snap = gh.snapshot();
                    if let Some(s) = snap.find_session_by_name(&name) {
                        return Ok(serde_json::json!({
                            "id": s.id.to_string(),
                            "name": s.name,
                            "windows": s.windows.iter().map(|w| w.to_string()).collect::<Vec<_>>(),
                            "created_at": format!("{:?}", s.created_at),
                            "created": false,
                        }));
                    }

                    // Create new session
                    let cwd =
                        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));

                    match gh.create_session(name, cwd).await {
                        Ok(session_id) => {
                            let snap = gh.snapshot();
                            if let Some(s) = snap.sessions.get(&session_id) {
                                Ok(serde_json::json!({
                                    "id": s.id.to_string(),
                                    "name": s.name,
                                    "windows": s.windows.iter().map(|w| w.to_string()).collect::<Vec<_>>(),
                                    "created_at": format!("{:?}", s.created_at),
                                    "created": true,
                                }))
                            } else {
                                Ok(serde_json::json!({
                                    "id": session_id.to_string(),
                                    "created": true,
                                }))
                            }
                        }
                        Err(e) => Err(shux_rpc::RpcError::with_message(
                            shux_rpc::ErrorCode::InternalError,
                            e.to_string(),
                        )),
                    }
                }
            },
        )
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
            // Quick probe — don't auto-start daemon just for version
            match client::try_connect(&socket_path).await {
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
