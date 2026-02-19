# 011 — CLI Foundation (clap)

**Status:** Pending
**Depends On:** 001, 008
**Parallelizable With:** 010

---

## Problem

shux's design principle "CLI == API" (PRD section 4.3, invariant 2) means every `shux` subcommand is a thin wrapper over a JSON-RPC call to the daemon. This task builds the CLI foundation: clap-based argument parsing, subcommand dispatch, daemon auto-start (probe UDS, fork daemon if needed, retry with backoff), JSON/text output formatting, and the initial set of subcommands for M0. Without this, users have no way to create sessions, list them, or interact with shux from the command line.

## PRD Reference

- **section 4.1** — Single binary with subcommands: `shux`, `shux new`, `shux attach`, `shux ls`, `shux kill`, `shux api`
- **section 4.3, invariant 2** — "CLI == API: Every `shux` subcommand is a thin wrapper over a JSON-RPC call"
- **section 4.5** — Daemon auto-start: probe UDS, spawn daemon, retry with exponential backoff
- **section 8.6** — CLI-to-API mapping table
- **section 15.2** — clap 4.x with derive macro
- **section 8.1** — JSON-RPC over UDS with length-prefixed framing

---

## Files to Create

- `crates/shux/src/cli.rs` — clap argument definitions with derive macro

## Files to Modify

- `crates/shux/src/main.rs` — Replace stub with real CLI dispatch
- `crates/shux/Cargo.toml` — Ensure all needed dependencies are present

---

## Execution Steps

### Step 1: Define the CLI structure with clap derive

```rust
// crates/shux/src/cli.rs

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

/// shux — a modern, batteries-included terminal multiplexer
#[derive(Parser, Debug)]
#[command(
    name = "shux",
    version,
    about = "A modern terminal multiplexer",
    long_about = "shux is a modern, batteries-included terminal multiplexer built in Rust.\n\
                  Tiny core, powerful plugin system, first-class support for humans and AI agents.",
    after_help = "Run 'shux <command> --help' for more information on a specific command."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Output format (text for humans, json for piping/scripting)
    #[arg(long, global = true, default_value = "text")]
    pub format: OutputFormat,

    /// Path to the daemon's Unix domain socket.
    /// Default: $XDG_RUNTIME_DIR/shux/shux.sock or /tmp/shux-$UID/shux.sock
    #[arg(long, global = true, env = "SHUX_SOCKET")]
    pub socket: Option<PathBuf>,

    /// Authentication token for TCP connections.
    /// Not required for UDS (which uses filesystem permissions).
    #[arg(long, global = true, env = "SHUX_TOKEN")]
    pub token: Option<String>,

    /// Enable verbose logging (sets RUST_LOG=debug for this invocation)
    #[arg(short, long, global = true)]
    pub verbose: bool,
}

/// Output format for CLI commands.
#[derive(Debug, Clone, Copy, ValueEnum, Default)]
pub enum OutputFormat {
    /// Human-readable text output (default)
    #[default]
    Text,
    /// JSON output for scripting and piping
    Json,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Create a new session (and optionally attach)
    New {
        /// Session name (auto-generated if not provided)
        #[arg(short, long)]
        session: Option<String>,

        /// Create-if-missing semantics (maps to session.ensure)
        #[arg(long)]
        ensure: bool,

        /// Do not attach after creating the session
        #[arg(short = 'd', long)]
        detached: bool,

        /// Shell command to run in the initial pane
        #[arg(long)]
        cmd: Option<String>,
    },

    /// Attach to an existing session
    Attach {
        /// Session name (attaches to most recent if not provided)
        #[arg(short, long)]
        session: Option<String>,
    },

    /// List sessions
    #[command(alias = "list")]
    Ls,

    /// Kill a session
    Kill {
        /// Session name to kill
        #[arg(short, long)]
        session: String,
    },

    /// Send a raw JSON-RPC call to the daemon (for debugging)
    Api {
        /// JSON-RPC method name (e.g., "system.version", "session.list")
        method: String,

        /// JSON-RPC params as a JSON string. Example: '{"name": "work"}'
        #[arg(default_value = "{}")]
        params: String,
    },

    /// Print version information
    Version,
}

impl Cli {
    /// Determine the socket path to use. Priority:
    /// 1. Explicit --socket flag
    /// 2. SHUX_SOCKET environment variable (handled by clap env)
    /// 3. $XDG_RUNTIME_DIR/shux/shux.sock
    /// 4. /tmp/shux-$UID/shux.sock
    pub fn socket_path(&self) -> PathBuf {
        if let Some(ref path) = self.socket {
            return path.clone();
        }

        // Try XDG_RUNTIME_DIR first
        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            return PathBuf::from(runtime_dir).join("shux").join("shux.sock");
        }

        // Fallback: /tmp/shux-$UID/
        let uid = unsafe { libc::getuid() };
        PathBuf::from(format!("/tmp/shux-{uid}")).join("shux.sock")
    }
}
```

### Step 2: Implement daemon auto-start with exponential backoff

When the CLI needs to talk to the daemon, it first probes the UDS. If the daemon is not running, it forks the daemon process and retries with exponential backoff. This follows PRD section 4.5.

```rust
// crates/shux/src/cli.rs (continued — daemon connection helper)

use std::time::Duration;

use tokio::net::UnixStream;
use tracing::{debug, info, warn};

/// Errors that can occur when connecting to the daemon.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("daemon not running and failed to start: {0}")]
    DaemonStartFailed(String),

    #[error("failed to connect after {retries} retries: {source}")]
    ConnectionFailed {
        retries: u32,
        source: io::Error,
    },

    #[error("IO error: {0}")]
    Io(#[from] io::Error),
}

use std::io;

/// Connect to the daemon, auto-starting it if necessary.
///
/// The algorithm:
/// 1. Try to connect to the UDS
/// 2. If connection refused or socket not found, start the daemon
/// 3. Retry with exponential backoff: 50ms, 100ms, 200ms, 400ms, 800ms
/// 4. Give up after 5 retries
pub async fn connect_to_daemon(socket_path: &std::path::Path) -> Result<UnixStream, ConnectError> {
    // First attempt: try to connect directly
    match UnixStream::connect(socket_path).await {
        Ok(stream) => {
            debug!("Connected to existing daemon");
            return Ok(stream);
        }
        Err(e) => {
            debug!("Daemon not available: {e}. Starting daemon...");
        }
    }

    // Start the daemon
    start_daemon(socket_path)?;

    // Retry with exponential backoff
    let backoff_ms = [50, 100, 200, 400, 800];
    let max_retries = backoff_ms.len() as u32;

    for (i, delay) in backoff_ms.iter().enumerate() {
        tokio::time::sleep(Duration::from_millis(*delay)).await;

        match UnixStream::connect(socket_path).await {
            Ok(stream) => {
                info!("Connected to daemon after {} retries", i + 1);
                return Ok(stream);
            }
            Err(e) => {
                if i < backoff_ms.len() - 1 {
                    debug!(
                        "Retry {}/{}: connection failed ({e}), waiting {}ms",
                        i + 1,
                        max_retries,
                        backoff_ms.get(i + 1).unwrap_or(&0)
                    );
                } else {
                    return Err(ConnectError::ConnectionFailed {
                        retries: max_retries,
                        source: e,
                    });
                }
            }
        }
    }

    unreachable!()
}

/// Start the daemon process. Uses fork + exec to spawn the daemon in the
/// background. The daemon itself handles daemonization (double-fork, setsid)
/// per task 001.
fn start_daemon(socket_path: &std::path::Path) -> Result<(), ConnectError> {
    use std::process::Command as StdCommand;

    // Get the path to the current executable (which is shux itself)
    let exe = std::env::current_exe().map_err(|e| {
        ConnectError::DaemonStartFailed(format!("cannot find current executable: {e}"))
    })?;

    // Ensure the socket directory exists
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            ConnectError::DaemonStartFailed(format!(
                "cannot create socket directory {}: {e}",
                parent.display()
            ))
        })?;

        // Set directory permissions to 0700 for security (PRD section 6.1, Security)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o700);
            let _ = std::fs::set_permissions(parent, perms);
        }
    }

    // Spawn the daemon. The daemon command handles its own daemonization.
    // We use `shux api serve` or an internal `--daemon` flag.
    let child = StdCommand::new(&exe)
        .arg("api")
        .arg("serve")
        .arg("--socket")
        .arg(socket_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| {
            ConnectError::DaemonStartFailed(format!("failed to spawn daemon: {e}"))
        })?;

    info!(pid = child.id(), "Daemon process spawned");
    Ok(())
}
```

### Step 3: Implement JSON-RPC client helper

A thin helper that sends a JSON-RPC request over the UDS connection and reads the response. Uses the same length-prefixed framing as the server (task 008).

```rust
// crates/shux/src/cli.rs (continued — JSON-RPC client)

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A JSON-RPC 2.0 request.
#[derive(Debug, Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    id: String,
    method: String,
    params: Value,
}

/// A JSON-RPC 2.0 response.
#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Option<String>,
    result: Option<Value>,
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    data: Option<Value>,
}

/// Send a JSON-RPC request over the given stream and read the response.
/// Uses 4-byte big-endian length prefix framing (matching PRD section 8.1).
async fn rpc_call(
    stream: &mut UnixStream,
    method: &str,
    params: Value,
) -> Result<Value, anyhow::Error> {
    let request = JsonRpcRequest {
        jsonrpc: "2.0",
        id: uuid::Uuid::new_v4().to_string(),
        method: method.to_string(),
        params,
    };

    let payload = serde_json::to_vec(&request)?;

    // Write length prefix (4 bytes, big-endian)
    let len = payload.len() as u32;
    stream.write_all(&len.to_be_bytes()).await?;

    // Write payload
    stream.write_all(&payload).await?;
    stream.flush().await?;

    // Read response length prefix
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let resp_len = u32::from_be_bytes(len_buf) as usize;

    // Enforce max frame size (16 MB per PRD section 8.1)
    if resp_len > 16 * 1024 * 1024 {
        anyhow::bail!("Response frame too large: {resp_len} bytes (max 16 MB)");
    }

    // Read response payload
    let mut resp_buf = vec![0u8; resp_len];
    stream.read_exact(&mut resp_buf).await?;

    let response: JsonRpcResponse = serde_json::from_slice(&resp_buf)?;

    if let Some(error) = response.error {
        anyhow::bail!(
            "RPC error {}: {} {}",
            error.code,
            error.message,
            error
                .data
                .map(|d| format!("({})", d))
                .unwrap_or_default()
        );
    }

    Ok(response.result.unwrap_or(Value::Null))
}
```

### Step 4: Implement subcommand handlers

Each subcommand is a thin wrapper that constructs a JSON-RPC call, sends it, and formats the output.

```rust
// crates/shux/src/cli.rs (continued — subcommand handlers)

/// Handle the `shux ls` command.
pub async fn handle_ls(
    stream: &mut UnixStream,
    format: OutputFormat,
) -> Result<(), anyhow::Error> {
    let result = rpc_call(stream, "session.list", Value::Object(Default::default())).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text => {
            if let Some(sessions) = result.as_array() {
                if sessions.is_empty() {
                    println!("no sessions");
                } else {
                    for session in sessions {
                        let name = session
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("(unnamed)");
                        let id = session
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        let windows = session
                            .get("windows")
                            .and_then(|v| v.as_array())
                            .map(|a| a.len())
                            .unwrap_or(0);
                        let created = session
                            .get("created_at")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");

                        println!(
                            "{name}: {windows} window{} (created {created}) [{id}]",
                            if windows == 1 { "" } else { "s" }
                        );
                    }
                }
            } else {
                println!("{result}");
            }
        }
    }

    Ok(())
}

/// Handle the `shux new` command.
pub async fn handle_new(
    stream: &mut UnixStream,
    session_name: Option<String>,
    cmd: Option<String>,
    ensure: bool,
    format: OutputFormat,
) -> Result<Value, anyhow::Error> {
    let mut params = serde_json::Map::new();
    if let Some(name) = session_name {
        params.insert("name".to_string(), Value::String(name));
    }
    if let Some(command) = cmd {
        params.insert("command".to_string(), Value::String(command));
    }

    let method = if ensure { "session.ensure" } else { "session.create" };
    let result = rpc_call(stream, method, Value::Object(params)).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text => {
            let name = result
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("(unnamed)");
            let id = result
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            if ensure {
                println!("Ensured session '{name}' [{id}]");
            } else {
                println!("Created session '{name}' [{id}]");
            }
        }
    }

    Ok(result)
}

/// Handle the `shux kill` command.
pub async fn handle_kill(
    stream: &mut UnixStream,
    session_name: &str,
    format: OutputFormat,
) -> Result<(), anyhow::Error> {
    let mut params = serde_json::Map::new();
    params.insert("name".to_string(), Value::String(session_name.to_string()));

    let result = rpc_call(stream, "session.kill", Value::Object(params)).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text => {
            println!("Killed session '{session_name}'");
        }
    }

    Ok(())
}

/// Handle the `shux api <method> <params>` command (raw JSON-RPC for debugging).
pub async fn handle_api(
    stream: &mut UnixStream,
    method: &str,
    params_str: &str,
    format: OutputFormat,
) -> Result<(), anyhow::Error> {
    let params: Value = serde_json::from_str(params_str)
        .map_err(|e| anyhow::anyhow!("Invalid JSON params: {e}"))?;

    let result = rpc_call(stream, method, params).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text => {
            // For raw API calls, JSON is always the most useful format
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
    }

    Ok(())
}

/// Handle the `shux version` command.
pub async fn handle_version(
    stream: &mut UnixStream,
    format: OutputFormat,
) -> Result<(), anyhow::Error> {
    let result =
        rpc_call(stream, "system.version", Value::Object(Default::default())).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text => {
            let version = result
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            println!("shux {version}");
        }
    }

    Ok(())
}
```

### Step 5: Implement main.rs dispatch

Replace the stub `main.rs` with real argument parsing and dispatch.

```rust
// crates/shux/src/main.rs

mod cli;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use cli::{Cli, Command, OutputFormat};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Cli::parse();

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

    let socket_path = args.socket_path();

    match args.command {
        // No subcommand: attach to last session or create "default"
        None => {
            let mut stream = cli::connect_to_daemon(&socket_path).await?;

            // Try to list sessions and attach to the most recent one.
            // If no sessions exist, create "default" and attach.
            let sessions_result = cli::handle_api(
                &mut stream,
                "session.list",
                "{}",
                OutputFormat::Json,
            )
            .await;

            // For M0, just create a default session and attach.
            // Full "last session" logic comes in M1.
            drop(stream);
            let mut stream = cli::connect_to_daemon(&socket_path).await?;
            let session = cli::handle_new(
                &mut stream,
                Some("default".to_string()),
                None,
                false,
                OutputFormat::Text,
            )
            .await?;

            // Attach via TUI client (task 010)
            // shux_ui::client::run_client(...)
            println!("[TUI attach not yet implemented — see task 010]");
            Ok(())
        }

        Some(Command::New {
            session,
            ensure,
            detached,
            cmd,
        }) => {
            let mut stream = cli::connect_to_daemon(&socket_path).await?;
            let result = cli::handle_new(&mut stream, session, cmd, ensure, args.format).await?;

            if !detached {
                // Attach via TUI client
                // The session name/ID is in the result
                println!("[TUI attach not yet implemented — see task 010]");
            }

            Ok(())
        }

        Some(Command::Attach { session }) => {
            let _stream = cli::connect_to_daemon(&socket_path).await?;
            let session_name = session.unwrap_or_else(|| "default".to_string());

            // Attach via TUI client (task 010)
            // shux_ui::client::run_client(ClientConfig {
            //     socket_path: socket_path.to_string_lossy().to_string(),
            //     session_name,
            //     ..Default::default()
            // }).await?;
            println!(
                "[TUI attach to '{}' not yet implemented — see task 010]",
                session_name
            );
            Ok(())
        }

        Some(Command::Ls) => {
            let mut stream = cli::connect_to_daemon(&socket_path).await?;
            cli::handle_ls(&mut stream, args.format).await
        }

        Some(Command::Kill { session }) => {
            let mut stream = cli::connect_to_daemon(&socket_path).await?;
            cli::handle_kill(&mut stream, &session, args.format).await
        }

        Some(Command::Api { method, params }) => {
            let mut stream = cli::connect_to_daemon(&socket_path).await?;
            cli::handle_api(&mut stream, &method, &params, args.format).await
        }

        Some(Command::Version) => {
            // First try to get version from daemon
            match cli::connect_to_daemon(&socket_path).await {
                Ok(mut stream) => {
                    cli::handle_version(&mut stream, args.format).await
                }
                Err(_) => {
                    // Daemon not running; print local version
                    match args.format {
                        OutputFormat::Json => {
                            println!(
                                "{{\"version\": \"{}\"}}",
                                env!("CARGO_PKG_VERSION")
                            );
                        }
                        OutputFormat::Text => {
                            println!("shux {} (daemon not running)", env!("CARGO_PKG_VERSION"));
                        }
                    }
                    Ok(())
                }
            }
        }
    }
}
```

### Step 6: Update Cargo.toml for the shux binary crate

```toml
# crates/shux/Cargo.toml
[package]
name = "shux"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
authors.workspace = true
description = "A modern, batteries-included terminal multiplexer"
default-run = "shux"

[[bin]]
name = "shux"
path = "src/main.rs"

[dependencies]
shux-core = { path = "../shux-core" }
shux-pty = { path = "../shux-pty" }
shux-vt = { path = "../shux-vt" }
shux-rpc = { path = "../shux-rpc" }
shux-plugin = { path = "../shux-plugin" }
shux-ui = { path = "../shux-ui" }
clap.workspace = true
tokio.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
anyhow.workspace = true
serde.workspace = true
serde_json.workspace = true
uuid.workspace = true
thiserror.workspace = true

[target.'cfg(unix)'.dependencies]
libc = "0.2"

[dev-dependencies]
assert_cmd.workspace = true
predicates.workspace = true
tempfile.workspace = true
```

### Step 7: Write tests with assert_cmd

```rust
// tests/cli_tests.rs (in the shux binary crate, or in tests/ directory)
//
// NOTE: These tests use assert_cmd to test the CLI binary. They require
// the binary to be built first. Some tests need a running daemon; those
// are marked with #[ignore] and run in integration test suites.

#[cfg(test)]
mod tests {
    use assert_cmd::Command;
    use predicates::prelude::*;

    /// Helper to get the shux binary command.
    fn shux() -> Command {
        Command::cargo_bin("shux").expect("shux binary should exist")
    }

    #[test]
    fn test_version_flag() {
        shux()
            .arg("--version")
            .assert()
            .success()
            .stdout(predicate::str::contains("shux"));
    }

    #[test]
    fn test_help_flag() {
        shux()
            .arg("--help")
            .assert()
            .success()
            .stdout(predicate::str::contains("terminal multiplexer"))
            .stdout(predicate::str::contains("new"))
            .stdout(predicate::str::contains("attach"))
            .stdout(predicate::str::contains("ls"))
            .stdout(predicate::str::contains("kill"))
            .stdout(predicate::str::contains("api"))
            .stdout(predicate::str::contains("version"));
    }

    #[test]
    fn test_new_help() {
        shux()
            .args(["new", "--help"])
            .assert()
            .success()
            .stdout(predicate::str::contains("--session"))
            .stdout(predicate::str::contains("--detached"));
    }

    #[test]
    fn test_attach_help() {
        shux()
            .args(["attach", "--help"])
            .assert()
            .success()
            .stdout(predicate::str::contains("--session"));
    }

    #[test]
    fn test_kill_help() {
        shux()
            .args(["kill", "--help"])
            .assert()
            .success()
            .stdout(predicate::str::contains("--session"));
    }

    #[test]
    fn test_api_help() {
        shux()
            .args(["api", "--help"])
            .assert()
            .success()
            .stdout(predicate::str::contains("method"));
    }

    #[test]
    fn test_global_format_option() {
        // Verify --format is accepted
        shux()
            .args(["--format", "json", "--help"])
            .assert()
            .success();
    }

    #[test]
    fn test_global_socket_option() {
        // Verify --socket is accepted
        shux()
            .args(["--socket", "/tmp/test.sock", "--help"])
            .assert()
            .success();
    }

    #[test]
    fn test_version_without_daemon() {
        // `shux version` should work even without a running daemon
        // (falls back to local version)
        shux()
            .arg("version")
            .env("SHUX_SOCKET", "/tmp/nonexistent-shux-test.sock")
            .assert()
            .success()
            .stdout(predicate::str::contains("shux"))
            .stdout(predicate::str::contains("daemon not running"));
    }

    #[test]
    fn test_version_json_without_daemon() {
        shux()
            .args(["--format", "json", "version"])
            .env("SHUX_SOCKET", "/tmp/nonexistent-shux-test.sock")
            .assert()
            .success()
            .stdout(predicate::str::contains("version"));
    }

    #[test]
    fn test_ls_alias() {
        // "list" should work as an alias for "ls"
        shux()
            .args(["list", "--help"])
            .assert()
            .success();
    }

    #[test]
    fn test_invalid_subcommand() {
        shux()
            .arg("nonexistent")
            .assert()
            .failure()
            .stderr(predicate::str::contains("unrecognized subcommand"));
    }

    #[test]
    fn test_kill_requires_session() {
        shux()
            .arg("kill")
            .assert()
            .failure()
            .stderr(predicate::str::contains("--session"));
    }

    // --- Integration tests (require running daemon) ---

    #[test]
    #[ignore = "requires running daemon — run in integration suite"]
    fn test_ls_with_daemon() {
        shux()
            .arg("ls")
            .assert()
            .success();
    }

    #[test]
    #[ignore = "requires running daemon — run in integration suite"]
    fn test_new_and_kill() {
        // Create a session
        shux()
            .args(["new", "-s", "test-cli-001", "-d"])
            .assert()
            .success()
            .stdout(predicate::str::contains("test-cli-001"));

        // List sessions
        shux()
            .arg("ls")
            .assert()
            .success()
            .stdout(predicate::str::contains("test-cli-001"));

        // Kill the session
        shux()
            .args(["kill", "-s", "test-cli-001"])
            .assert()
            .success();
    }

    #[test]
    #[ignore = "requires running daemon — run in integration suite"]
    fn test_api_system_version() {
        shux()
            .args(["--format", "json", "api", "system.version"])
            .assert()
            .success()
            .stdout(predicate::str::contains("version"));
    }
}
```

### Step 8: Test the socket path resolution

```rust
// crates/shux/src/cli.rs — tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_socket_path_explicit() {
        let cli = Cli {
            command: None,
            format: OutputFormat::Text,
            socket: Some(PathBuf::from("/custom/path.sock")),
            token: None,
            verbose: false,
        };
        assert_eq!(cli.socket_path(), PathBuf::from("/custom/path.sock"));
    }

    #[test]
    fn test_socket_path_fallback() {
        let cli = Cli {
            command: None,
            format: OutputFormat::Text,
            socket: None,
            token: None,
            verbose: false,
        };
        let path = cli.socket_path();

        // Should end with shux.sock
        assert!(path.to_string_lossy().ends_with("shux.sock"));

        // Should be an absolute path
        assert!(path.is_absolute());
    }

    #[test]
    fn test_output_format_default() {
        let format = OutputFormat::default();
        assert!(matches!(format, OutputFormat::Text));
    }
}
```

---

## Verification

### Functional

```bash
# Build the shux binary
cargo build -p shux

# Verify CLI help works
cargo run -p shux -- --help
# Expected: shows all subcommands and global options

# Verify subcommand help
cargo run -p shux -- new --help
cargo run -p shux -- attach --help
cargo run -p shux -- kill --help
cargo run -p shux -- api --help

# Verify ensure flag is exposed
cargo run -p shux -- new --help | rg -- '--ensure'

# Verify version works without daemon
SHUX_SOCKET=/tmp/nonexistent.sock cargo run -p shux -- version
# Expected: "shux 0.1.0 (daemon not running)"

# Verify JSON output format
SHUX_SOCKET=/tmp/nonexistent.sock cargo run -p shux -- --format json version
# Expected: {"version": "0.1.0"}

# Verify clippy
cargo clippy -p shux -- -D warnings
```

### Tests

```bash
# Run CLI tests
cargo nextest run -p shux

# Run assert_cmd tests specifically
cargo nextest run -p shux -- cli

# Expected passing tests:
#   tests::test_version_flag
#   tests::test_help_flag
#   tests::test_new_help
#   tests::test_attach_help
#   tests::test_kill_help
#   tests::test_api_help
#   tests::test_global_format_option
#   tests::test_global_socket_option
#   tests::test_version_without_daemon
#   tests::test_version_json_without_daemon
#   tests::test_ls_alias
#   tests::test_invalid_subcommand
#   tests::test_kill_requires_session
#   cli::tests::test_socket_path_explicit
#   cli::tests::test_socket_path_fallback
#   cli::tests::test_output_format_default
```

---

## Completion Criteria

- [ ] `crates/shux/src/cli.rs` defines `Cli` struct with clap derive macro
- [ ] Subcommands implemented: `new`, `attach`, `ls` (with `list` alias), `kill`, `api`, `version`
- [ ] `shux new --ensure` maps to `session.ensure`; default `shux new` maps to `session.create`
- [ ] Global options: `--format json|text`, `--socket <path>`, `--token <token>`, `--verbose`
- [ ] No-argument `shux` invocation creates/attaches to "default" session (stub in M0)
- [ ] `shux version` works without a running daemon (prints local version)
- [ ] `shux api <method> <params>` sends raw JSON-RPC for debugging
- [ ] `connect_to_daemon()` implements exponential backoff: 50ms, 100ms, 200ms, 400ms, 800ms (5 retries max)
- [ ] Daemon auto-start: spawns `shux api serve` when UDS is not available
- [ ] Socket directory created with 0700 permissions
- [ ] Socket path resolution: `--socket` > `$SHUX_SOCKET` > `$XDG_RUNTIME_DIR/shux/` > `/tmp/shux-$UID/`
- [ ] JSON-RPC client uses 4-byte big-endian length-prefixed framing (matching server in task 008)
- [ ] JSON-RPC client enforces 16 MB max frame size
- [ ] Text output is human-readable; JSON output is valid JSON
- [ ] `--help` for every subcommand shows meaningful descriptions
- [ ] All assert_cmd tests pass
- [ ] All unit tests pass (socket path, output format)
- [ ] `cargo clippy -p shux -- -D warnings` passes

---

## Commit Message
```
feat(cli): add clap-based CLI with subcommands, auto-start, and JSON/text output

- clap 4.x derive: new, attach, ls, kill, api, version subcommands
- Global options: --format json|text, --socket, --token, --verbose
- Daemon auto-start with exponential backoff (50-800ms, 5 retries)
- JSON-RPC client over UDS with length-prefixed framing
- Socket path resolution: flag > env > XDG_RUNTIME_DIR > /tmp fallback
- assert_cmd tests for CLI help, version, argument validation
```

---

## Session Protocol

1. **Before starting:** Read task 001 (daemon skeleton) to understand the daemon's startup and socket path conventions. Read task 008 (JSON-RPC server) to understand the framing protocol and available methods. Verify the daemon can start and listen on a UDS before testing CLI-to-daemon communication.
2. **During:** Implement in order: `cli.rs` structs (Step 1) -> socket path resolution -> daemon auto-start (Step 2) -> JSON-RPC client (Step 3) -> subcommand handlers (Step 4) -> `main.rs` (Step 5) -> Cargo.toml (Step 6) -> tests (Steps 7-8). Run `cargo check -p shux` after each significant change. The assert_cmd tests can run without a daemon for help/version tests. Integration tests (marked `#[ignore]`) need a running daemon.
3. **Important:** The `libc` crate is needed for `getuid()` in socket path fallback. Use a `cfg(unix)` guard since this code is Unix-only. The daemon start mechanism in Step 2 is a simplified version -- task 001 handles the full double-fork daemonization. Here we just spawn the process and let it daemonize itself.
4. **After:** Run `make check`. Update `docs/PROGRESS.md` (mark 011 in-progress or done). Update `CLAUDE.md` Learnings with any clap derive gotchas (e.g., `global = true` placement, ValueEnum derive requirements, subcommand aliases).
