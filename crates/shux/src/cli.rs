//! CLI argument definitions and subcommand dispatch.
//!
//! Every `shux` subcommand is a thin wrapper over a JSON-RPC call to the daemon
//! (PRD §4.3 invariant 2: "CLI == API").

use std::path::PathBuf;

use clap::builder::styling::{AnsiColor, Effects, Styles};
use clap::{Parser, Subcommand, ValueEnum};

const CLAP_STYLES: Styles = Styles::styled()
    .header(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
    .usage(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
    .literal(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .placeholder(AnsiColor::Yellow.on_default())
    .valid(AnsiColor::Green.on_default())
    .invalid(AnsiColor::Red.on_default().effects(Effects::BOLD))
    .error(AnsiColor::Red.on_default().effects(Effects::BOLD));

/// shux — a modern, batteries-included terminal multiplexer
#[derive(Parser, Debug)]
#[command(
    name = "shux",
    version,
    about = "A modern terminal multiplexer",
    long_about = "A modern terminal multiplexer \u{2014} tiny core \u{2022} powerful plugins \u{2022} built for humans and AI agents",
    after_help = "Run 'shux <command> --help' for more information on a specific command.",
    styles = CLAP_STYLES,
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

    /// Internal: start the daemon (used by auto-start, not for users)
    #[command(name = "__daemon", hide = true)]
    #[allow(non_camel_case_types)]
    __daemon,
}

impl Cli {
    /// Determine the socket path to use. Priority:
    /// 1. Explicit --socket flag / SHUX_SOCKET env (handled by clap env)
    /// 2. $XDG_RUNTIME_DIR/shux/shux.sock
    /// 3. /tmp/shux-$UID/shux.sock
    pub fn socket_path(&self) -> PathBuf {
        if let Some(ref path) = self.socket {
            return path.clone();
        }
        crate::daemon::socket_path().unwrap_or_else(|_| PathBuf::from("/tmp/shux/shux.sock"))
    }
}

/// Errors that can occur during RPC communication.
#[derive(Debug, thiserror::Error)]
pub enum RpcClientError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("response frame too large: {0} bytes (max 16 MB)")]
    FrameTooLarge(usize),
    #[error("RPC error {code}: {message}")]
    Rpc {
        code: i64,
        message: String,
        data: Option<serde_json::Value>,
    },
}

/// Send a JSON-RPC request over a UDS and read the response.
/// Uses 4-byte big-endian length-prefix framing (matching server in task 008).
pub async fn rpc_call(
    stream: &mut tokio::net::UnixStream,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, RpcClientError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": uuid::Uuid::new_v4().to_string(),
        "method": method,
        "params": params,
    });

    let payload = serde_json::to_vec(&request)?;

    // Write length prefix (4 bytes, big-endian)
    let len = payload.len() as u32;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(&payload).await?;
    stream.flush().await?;

    // Read response length prefix
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let resp_len = u32::from_be_bytes(len_buf) as usize;

    // Enforce max frame size (16 MB per PRD §8.1)
    if resp_len > 16 * 1024 * 1024 {
        return Err(RpcClientError::FrameTooLarge(resp_len));
    }

    // Read response payload
    let mut resp_buf = vec![0u8; resp_len];
    stream.read_exact(&mut resp_buf).await?;

    let response: serde_json::Value = serde_json::from_slice(&resp_buf)?;

    if let Some(error) = response.get("error") {
        let code = error.get("code").and_then(|v| v.as_i64()).unwrap_or(0);
        let message = error
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error")
            .to_string();
        let data = error.get("data").cloned();
        return Err(RpcClientError::Rpc {
            code,
            message,
            data,
        });
    }

    Ok(response
        .get("result")
        .cloned()
        .unwrap_or(serde_json::Value::Null))
}

/// Handle the `shux ls` command.
pub async fn handle_ls(
    stream: &mut tokio::net::UnixStream,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let result = rpc_call(
        stream,
        "session.list",
        serde_json::Value::Object(Default::default()),
    )
    .await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text => {
            use crate::style;

            // session.list returns {"sessions": [...]}
            let sessions = result
                .get("sessions")
                .and_then(|v| v.as_array())
                .or_else(|| result.as_array());

            if let Some(sessions) = sessions {
                if sessions.is_empty() {
                    style::print_no_sessions();
                } else {
                    for session in sessions {
                        let name = session
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("(unnamed)");
                        let id = session.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                        let windows = session
                            .get("windows")
                            .and_then(|v| v.as_array())
                            .map(|a| a.len())
                            .unwrap_or(0);
                        let created = session
                            .get("created_at")
                            .and_then(|v| {
                                v.as_str().map(String::from).or_else(|| {
                                    v.as_u64().map(|ts| {
                                        let secs = ts;
                                        let dt = std::time::UNIX_EPOCH
                                            + std::time::Duration::from_secs(secs);
                                        let elapsed = dt.elapsed().unwrap_or_default();
                                        if elapsed.as_secs() < 60 {
                                            format!("{}s ago", elapsed.as_secs())
                                        } else if elapsed.as_secs() < 3600 {
                                            format!("{}m ago", elapsed.as_secs() / 60)
                                        } else {
                                            format!("{}h ago", elapsed.as_secs() / 3600)
                                        }
                                    })
                                })
                            })
                            .unwrap_or_else(|| "?".to_string());

                        style::print_session_entry(name, windows, &created, id);
                    }
                }
            } else {
                style::print_no_sessions();
            }
        }
    }

    Ok(())
}

/// Handle the `shux new` command.
pub async fn handle_new(
    stream: &mut tokio::net::UnixStream,
    session_name: Option<String>,
    cmd: Option<String>,
    ensure: bool,
    format: OutputFormat,
) -> anyhow::Result<serde_json::Value> {
    let mut params = serde_json::Map::new();
    if let Some(name) = session_name {
        params.insert("name".to_string(), serde_json::Value::String(name));
    }
    if let Some(command) = cmd {
        params.insert("command".to_string(), serde_json::Value::String(command));
    }

    let method = if ensure {
        "session.ensure"
    } else {
        "session.create"
    };
    let result = rpc_call(stream, method, serde_json::Value::Object(params)).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text => {
            use crate::style;

            let name = result
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("(unnamed)");
            let id = result.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            style::print_session_created(name, id, ensure);
        }
    }

    Ok(result)
}

/// Handle the `shux kill` command.
pub async fn handle_kill(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let mut params = serde_json::Map::new();
    params.insert(
        "name".to_string(),
        serde_json::Value::String(session_name.to_string()),
    );

    let result = rpc_call(stream, "session.kill", serde_json::Value::Object(params)).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text => {
            crate::style::print_session_killed(session_name);
        }
    }

    Ok(())
}

/// Handle the `shux api <method> <params>` command (raw JSON-RPC for debugging).
pub async fn handle_api(
    stream: &mut tokio::net::UnixStream,
    method: &str,
    params_str: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let params: serde_json::Value = serde_json::from_str(params_str)
        .map_err(|e| anyhow::anyhow!("Invalid JSON params: {e}"))?;

    let result = rpc_call(stream, method, params).await?;

    match format {
        OutputFormat::Json | OutputFormat::Text => {
            // For raw API calls, JSON is always the most useful format
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
    }

    Ok(())
}

/// Handle the `shux version` command.
pub async fn handle_version(
    stream: &mut tokio::net::UnixStream,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let result = rpc_call(
        stream,
        "system.version",
        serde_json::Value::Object(Default::default()),
    )
    .await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text => {
            let version = result
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            crate::style::print_version(version, None);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_socket_path_explicit() {
        let cli = Cli {
            command: None,
            format: OutputFormat::Text,
            socket: Some(PathBuf::from("/custom/path.sock")),
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
            verbose: false,
        };
        let path = cli.socket_path();

        // Should end with shux.sock
        assert!(
            path.to_string_lossy().ends_with("shux.sock"),
            "socket path should end with shux.sock, got: {}",
            path.display()
        );

        // Should be an absolute path
        assert!(path.is_absolute());
    }

    #[test]
    fn test_output_format_default() {
        let format = OutputFormat::default();
        assert!(matches!(format, OutputFormat::Text));
    }

    #[test]
    fn test_cli_parse_ls() {
        let cli = Cli::try_parse_from(["shux", "ls"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Ls)));
    }

    #[test]
    fn test_cli_parse_new_with_options() {
        let cli = Cli::try_parse_from(["shux", "new", "-s", "work", "-d", "--ensure"]).unwrap();
        match cli.command {
            Some(Command::New {
                session,
                ensure,
                detached,
                cmd,
            }) => {
                assert_eq!(session, Some("work".to_string()));
                assert!(ensure);
                assert!(detached);
                assert!(cmd.is_none());
            }
            _ => panic!("expected New command"),
        }
    }

    #[test]
    fn test_cli_parse_kill() {
        let cli = Cli::try_parse_from(["shux", "kill", "-s", "mytest"]).unwrap();
        match cli.command {
            Some(Command::Kill { session }) => {
                assert_eq!(session, "mytest");
            }
            _ => panic!("expected Kill command"),
        }
    }

    #[test]
    fn test_cli_parse_api() {
        let cli =
            Cli::try_parse_from(["shux", "api", "system.version", r#"{"key":"val"}"#]).unwrap();
        match cli.command {
            Some(Command::Api { method, params }) => {
                assert_eq!(method, "system.version");
                assert_eq!(params, r#"{"key":"val"}"#);
            }
            _ => panic!("expected Api command"),
        }
    }

    #[test]
    fn test_cli_parse_api_default_params() {
        let cli = Cli::try_parse_from(["shux", "api", "system.health"]).unwrap();
        match cli.command {
            Some(Command::Api { params, .. }) => {
                assert_eq!(params, "{}");
            }
            _ => panic!("expected Api command"),
        }
    }

    #[test]
    fn test_cli_parse_global_format() {
        let cli = Cli::try_parse_from(["shux", "--format", "json", "ls"]).unwrap();
        assert!(matches!(cli.format, OutputFormat::Json));
    }

    #[test]
    fn test_cli_parse_global_socket() {
        let cli = Cli::try_parse_from(["shux", "--socket", "/tmp/my.sock", "ls"]).unwrap();
        assert_eq!(cli.socket, Some(PathBuf::from("/tmp/my.sock")));
    }

    #[test]
    fn test_cli_parse_verbose() {
        let cli = Cli::try_parse_from(["shux", "-v", "ls"]).unwrap();
        assert!(cli.verbose);
    }

    #[test]
    fn test_cli_list_alias() {
        let cli = Cli::try_parse_from(["shux", "list"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Ls)));
    }

    #[test]
    fn test_cli_no_subcommand() {
        let cli = Cli::try_parse_from(["shux"]).unwrap();
        assert!(cli.command.is_none());
    }

    #[test]
    fn test_cli_attach_with_session() {
        let cli = Cli::try_parse_from(["shux", "attach", "-s", "dev"]).unwrap();
        match cli.command {
            Some(Command::Attach { session }) => {
                assert_eq!(session, Some("dev".to_string()));
            }
            _ => panic!("expected Attach command"),
        }
    }

    #[test]
    fn test_cli_version_subcommand() {
        let cli = Cli::try_parse_from(["shux", "version"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Version)));
    }

    #[test]
    fn test_cli_kill_requires_session() {
        let result = Cli::try_parse_from(["shux", "kill"]);
        assert!(result.is_err());
    }
}
