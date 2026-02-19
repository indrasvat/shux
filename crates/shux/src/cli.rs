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

    /// Rename a session
    Rename {
        /// Current session name
        #[arg(short, long)]
        session: String,

        /// New name for the session
        #[arg(short, long)]
        name: String,
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

    /// Window management
    #[command(alias = "win")]
    Window {
        #[command(subcommand)]
        command: WindowCommand,
    },

    /// Pane management
    Pane {
        #[command(subcommand)]
        command: PaneCommand,
    },

    /// Internal: start the daemon (used by auto-start, not for users)
    #[command(name = "__daemon", hide = true)]
    #[allow(non_camel_case_types)]
    __daemon,
}

#[derive(Subcommand, Debug)]
pub enum WindowCommand {
    /// List windows in a session
    #[command(alias = "ls")]
    List {
        /// Session name
        #[arg(short, long)]
        session: String,
    },

    /// Create a new window in a session
    New {
        /// Session name
        #[arg(short, long)]
        session: String,

        /// Window name (auto-generated if not provided)
        #[arg(short, long)]
        name: Option<String>,

        /// Create-if-missing semantics (maps to window.ensure)
        #[arg(long)]
        ensure: bool,
    },

    /// Kill a window
    Kill {
        /// Session name
        #[arg(short, long)]
        session: String,

        /// Window name or index
        #[arg(short, long)]
        window: String,
    },

    /// Rename a window
    Rename {
        /// Session name
        #[arg(short, long)]
        session: String,

        /// Current window name or index
        #[arg(short, long)]
        window: String,

        /// New window name
        #[arg(short, long)]
        name: String,
    },

    /// Focus (select) a window
    Focus {
        /// Session name
        #[arg(short, long)]
        session: String,

        /// Window name or index
        #[arg(short, long)]
        window: String,
    },

    /// Reorder (move) a window to a new index
    Reorder {
        /// Session name
        #[arg(short, long)]
        session: String,

        /// Window name or index
        #[arg(short, long)]
        window: String,

        /// New index position
        #[arg(short, long)]
        index: usize,
    },
}

#[derive(Subcommand, Debug)]
pub enum PaneCommand {
    /// List panes in a window
    #[command(alias = "ls")]
    List {
        /// Session name
        #[arg(short, long)]
        session: String,

        /// Window name or index (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,
    },

    /// Split a pane
    Split {
        /// Session name
        #[arg(short, long)]
        session: String,

        /// Window name or index (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,

        /// Pane UUID to split (uses active pane if not provided)
        #[arg(short, long)]
        pane: Option<String>,

        /// Split direction: vertical, horizontal, or auto
        #[arg(short, long)]
        direction: Option<String>,

        /// Split ratio (0.0-1.0, default 0.5)
        #[arg(short, long)]
        ratio: Option<f64>,
    },

    /// Focus a specific pane by UUID
    Focus {
        /// Session name
        #[arg(short, long)]
        session: String,

        /// Window name or index (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,

        /// Pane UUID to focus
        #[arg(short, long)]
        pane: String,
    },

    /// Move focus in a direction (up/down/left/right)
    FocusDir {
        /// Session name
        #[arg(short, long)]
        session: String,

        /// Window name or index (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,

        /// Direction: up, down, left, right
        #[arg(short, long)]
        direction: String,
    },

    /// Resize a pane
    Resize {
        /// Session name
        #[arg(short, long)]
        session: String,

        /// Window name or index (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,

        /// Pane UUID to resize (uses active pane if not provided)
        #[arg(short, long)]
        pane: Option<String>,

        /// Resize direction: horizontal or vertical
        #[arg(short, long)]
        direction: String,

        /// Resize amount (0.0-1.0, default 0.1)
        #[arg(long)]
        delta: Option<f64>,
    },

    /// Toggle zoom on a pane
    Zoom {
        /// Session name
        #[arg(short, long)]
        session: String,

        /// Window name or index (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,

        /// Pane UUID to zoom (uses active pane if not provided)
        #[arg(short, long)]
        pane: Option<String>,
    },

    /// Swap two panes
    Swap {
        /// Session name
        #[arg(short, long)]
        session: String,

        /// Window name or index (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,

        /// First pane UUID
        #[arg(short, long)]
        pane: String,

        /// Second pane UUID (target to swap with)
        #[arg(short, long)]
        target: String,
    },

    /// Kill a pane
    Kill {
        /// Session name
        #[arg(short, long)]
        session: String,

        /// Window name or index (uses active window if not provided)
        #[arg(short, long)]
        window: Option<String>,

        /// Pane UUID to kill
        #[arg(short, long)]
        pane: String,
    },
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

/// Format an RPC error, including detail from data if available.
fn rpc_display(code: i64, message: &str, data: Option<&serde_json::Value>) -> String {
    if let Some(data) = data {
        // Try "detail" field (invalid_params, internal errors)
        if let Some(detail) = data.get("detail").and_then(|v| v.as_str()) {
            return detail.to_string();
        }
        // Try "name" field (name_conflict)
        if let Some(name) = data.get("name").and_then(|v| v.as_str()) {
            let resource = data
                .get("resource")
                .and_then(|v| v.as_str())
                .unwrap_or("resource");
            return format!("{resource} name '{name}' already exists");
        }
        // Try "id" field (not_found)
        if let Some(id) = data.get("id").and_then(|v| v.as_str()) {
            let resource = data
                .get("resource")
                .and_then(|v| v.as_str())
                .unwrap_or("resource");
            return format!("{resource} '{id}' not found");
        }
    }
    format!("RPC error {code}: {message}")
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
    #[error("{}", rpc_display(*.code, message, data.as_ref()))]
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

/// Handle the `shux rename` command.
pub async fn handle_rename(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    new_name: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let mut params = serde_json::Map::new();
    params.insert(
        "name".to_string(),
        serde_json::Value::String(session_name.to_string()),
    );
    params.insert(
        "new_name".to_string(),
        serde_json::Value::String(new_name.to_string()),
    );

    let result = rpc_call(stream, "session.rename", serde_json::Value::Object(params)).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text => {
            crate::style::print_session_renamed(session_name, new_name);
        }
    }

    Ok(())
}

/// Resolve a session name to its UUID by querying session.list.
async fn resolve_session_id(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
) -> Result<String, RpcClientError> {
    let result = rpc_call(stream, "session.list", serde_json::json!({})).await?;
    let sessions = result
        .get("sessions")
        .and_then(|v| v.as_array())
        .or_else(|| result.as_array());

    if let Some(sessions) = sessions {
        for s in sessions {
            if s.get("name").and_then(|v| v.as_str()) == Some(session_name) {
                if let Some(id) = s.get("id").and_then(|v| v.as_str()) {
                    return Ok(id.to_string());
                }
            }
        }
    }

    Err(RpcClientError::Rpc {
        code: -32004,
        message: format!("session '{session_name}' not found"),
        data: None,
    })
}

/// Resolve a window specifier (name or index) to (window_id, window_title).
async fn resolve_window_id(
    stream: &mut tokio::net::UnixStream,
    session_id: &str,
    window_spec: &str,
) -> Result<(String, String), RpcClientError> {
    let result = rpc_call(
        stream,
        "window.list",
        serde_json::json!({"session_id": session_id}),
    )
    .await?;
    let windows = result.as_array().ok_or_else(|| RpcClientError::Rpc {
        code: -32603,
        message: "unexpected response from window.list".to_string(),
        data: None,
    })?;

    // Try as numeric index first
    if let Ok(idx) = window_spec.parse::<usize>() {
        if let Some(w) = windows.get(idx) {
            let id = w.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            let title = w.get("title").and_then(|v| v.as_str()).unwrap_or("?");
            return Ok((id.to_string(), title.to_string()));
        }
    }

    // Try as window name
    for w in windows {
        if w.get("title").and_then(|v| v.as_str()) == Some(window_spec) {
            let id = w.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            let title = w.get("title").and_then(|v| v.as_str()).unwrap_or("?");
            return Ok((id.to_string(), title.to_string()));
        }
    }

    Err(RpcClientError::Rpc {
        code: -32004,
        message: format!("window '{window_spec}' not found in session"),
        data: None,
    })
}

/// Handle the `shux window list` command.
pub async fn handle_window_list(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let session_id = resolve_session_id(stream, session_name).await?;
    let result = rpc_call(
        stream,
        "window.list",
        serde_json::json!({"session_id": session_id}),
    )
    .await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text => {
            use crate::style;

            let windows = result.as_array();
            if let Some(windows) = windows {
                if windows.is_empty() {
                    println!("{}", style::muted("no windows"));
                } else {
                    for w in windows {
                        let index = w.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                        let title = w
                            .get("title")
                            .and_then(|v| v.as_str())
                            .unwrap_or("(untitled)");
                        let pane_count =
                            w.get("pane_count").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                        let is_active = w
                            .get("is_active")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        style::print_window_entry(index, title, pane_count, is_active);
                    }
                }
            } else {
                println!("{}", style::muted("no windows"));
            }
        }
    }

    Ok(())
}

/// Handle the `shux window new` command.
pub async fn handle_window_new(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_name: Option<String>,
    ensure: bool,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let session_id = resolve_session_id(stream, session_name).await?;

    let method = if ensure {
        "window.ensure"
    } else {
        "window.create"
    };
    let mut params = serde_json::Map::new();
    params.insert(
        "session_id".to_string(),
        serde_json::Value::String(session_id),
    );
    if let Some(name) = &window_name {
        params.insert("name".to_string(), serde_json::Value::String(name.clone()));
    }

    let result = rpc_call(stream, method, serde_json::Value::Object(params)).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text => {
            let title = result
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("(untitled)");
            let index = result.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
            crate::style::print_window_created(title, index);
        }
    }

    Ok(())
}

/// Handle the `shux window kill` command.
pub async fn handle_window_kill(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let session_id = resolve_session_id(stream, session_name).await?;
    let (window_id, window_title) = resolve_window_id(stream, &session_id, window_spec).await?;

    let result = rpc_call(stream, "window.kill", serde_json::json!({"id": window_id})).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text => {
            crate::style::print_window_killed(&window_title);
        }
    }

    Ok(())
}

/// Handle the `shux window rename` command.
pub async fn handle_window_rename(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: &str,
    new_name: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let session_id = resolve_session_id(stream, session_name).await?;
    let (window_id, old_title) = resolve_window_id(stream, &session_id, window_spec).await?;

    let result = rpc_call(
        stream,
        "window.rename",
        serde_json::json!({"id": window_id, "name": new_name}),
    )
    .await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text => {
            crate::style::print_window_renamed(&old_title, new_name);
        }
    }

    Ok(())
}

/// Handle the `shux window focus` command.
pub async fn handle_window_focus(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let session_id = resolve_session_id(stream, session_name).await?;
    let (window_id, window_title) = resolve_window_id(stream, &session_id, window_spec).await?;

    let result = rpc_call(stream, "window.focus", serde_json::json!({"id": window_id})).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text => {
            crate::style::print_window_focused(&window_title);
        }
    }

    Ok(())
}

/// Handle the `shux window reorder` command.
pub async fn handle_window_reorder(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: &str,
    new_index: usize,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let session_id = resolve_session_id(stream, session_name).await?;
    let (window_id, window_title) = resolve_window_id(stream, &session_id, window_spec).await?;

    let result = rpc_call(
        stream,
        "window.reorder",
        serde_json::json!({"id": window_id, "new_index": new_index}),
    )
    .await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text => {
            crate::style::print_window_reordered(&window_title, new_index);
        }
    }

    Ok(())
}

/// Resolve a pane-related window_id: either explicit window spec or session's active window.
async fn resolve_pane_window_id(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
) -> Result<(String, String), RpcClientError> {
    let session_id = resolve_session_id(stream, session_name).await?;
    match window_spec {
        Some(spec) => {
            let (wid, _title) = resolve_window_id(stream, &session_id, spec).await?;
            Ok((session_id, wid))
        }
        None => {
            // Get active window from session
            let result = rpc_call(stream, "session.list", serde_json::json!({})).await?;
            let sessions = result
                .get("sessions")
                .and_then(|v| v.as_array())
                .or_else(|| result.as_array());
            if let Some(sessions) = sessions {
                for s in sessions {
                    if s.get("id").and_then(|v| v.as_str()) == Some(&session_id) {
                        if let Some(aw) = s.get("active_window_id").and_then(|v| v.as_str()) {
                            return Ok((session_id, aw.to_string()));
                        }
                    }
                }
            }
            Err(RpcClientError::Rpc {
                code: -32004,
                message: "could not determine active window".to_string(),
                data: None,
            })
        }
    }
}

/// Handle the `shux pane list` command.
pub async fn handle_pane_list(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let (session_id, window_id) = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let result = rpc_call(
        stream,
        "pane.list",
        serde_json::json!({"session_id": session_id, "window_id": window_id}),
    )
    .await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text => {
            use crate::style;
            let panes = result.as_array();
            if let Some(panes) = panes {
                if panes.is_empty() {
                    println!("{}", style::muted("no panes"));
                } else {
                    for p in panes {
                        let id = p.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                        let title = p.get("title").and_then(|v| v.as_str()).unwrap_or("");
                        let is_focused = p
                            .get("is_focused")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let is_zoomed = p
                            .get("is_zoomed")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        style::print_pane_entry(id, title, is_focused, is_zoomed);
                    }
                }
            } else {
                println!("{}", style::muted("no panes"));
            }
        }
    }

    Ok(())
}

/// Handle the `shux pane split` command.
pub async fn handle_pane_split(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    pane_spec: Option<&str>,
    direction: Option<&str>,
    ratio: Option<f64>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let (session_id, window_id) = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let mut params = serde_json::json!({
        "session_id": session_id,
        "window_id": window_id,
    });

    if let Some(pid) = pane_spec {
        params["pane_id"] = serde_json::Value::String(pid.to_string());
    }
    if let Some(dir) = direction {
        params["direction"] = serde_json::Value::String(dir.to_string());
    }
    if let Some(r) = ratio {
        params["ratio"] = serde_json::json!(r);
    }

    let result = rpc_call(stream, "pane.split", params).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text => {
            let pane_id = result
                .get("pane")
                .and_then(|v| v.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let dir_label = direction.unwrap_or("vertical");
            crate::style::print_pane_split(pane_id, dir_label);
        }
    }

    Ok(())
}

/// Handle the `shux pane focus` command.
pub async fn handle_pane_focus(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    pane_id: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    // Resolve window for validation, but pane.focus only needs pane_id
    let _ = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let result = rpc_call(
        stream,
        "pane.focus",
        serde_json::json!({"pane_id": pane_id}),
    )
    .await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text => {
            crate::style::print_pane_focused(pane_id);
        }
    }

    Ok(())
}

/// Handle the `shux pane focus-dir` command.
pub async fn handle_pane_focus_dir(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    direction: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let (session_id, window_id) = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let result = rpc_call(
        stream,
        "pane.focus_direction",
        serde_json::json!({
            "session_id": session_id,
            "window_id": window_id,
            "direction": direction,
        }),
    )
    .await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text => {
            let pane_id = result
                .get("pane_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            crate::style::print_pane_focused(pane_id);
        }
    }

    Ok(())
}

/// Handle the `shux pane resize` command.
pub async fn handle_pane_resize(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    pane_spec: Option<&str>,
    direction: &str,
    delta: Option<f64>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let (session_id, window_id) = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let mut params = serde_json::json!({
        "session_id": session_id,
        "window_id": window_id,
        "direction": direction,
    });

    if let Some(pid) = pane_spec {
        params["pane_id"] = serde_json::Value::String(pid.to_string());
    }
    if let Some(d) = delta {
        params["delta"] = serde_json::json!(d);
    }

    let result = rpc_call(stream, "pane.resize", params).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text => {
            let pane_id = result
                .get("pane_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            crate::style::print_pane_resized(pane_id);
        }
    }

    Ok(())
}

/// Handle the `shux pane zoom` command.
pub async fn handle_pane_zoom(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    pane_spec: Option<&str>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let (session_id, window_id) = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let mut params = serde_json::json!({
        "session_id": session_id,
        "window_id": window_id,
    });

    if let Some(pid) = pane_spec {
        params["pane_id"] = serde_json::Value::String(pid.to_string());
    }

    let result = rpc_call(stream, "pane.zoom", params).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text => {
            let pane_id = result
                .get("pane_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let is_zoomed = result
                .get("is_zoomed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            crate::style::print_pane_zoomed(pane_id, is_zoomed);
        }
    }

    Ok(())
}

/// Handle the `shux pane swap` command.
pub async fn handle_pane_swap(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    pane_id: &str,
    target_id: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    // Resolve window for validation
    let _ = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let result = rpc_call(
        stream,
        "pane.swap",
        serde_json::json!({"pane_id": pane_id, "target_pane_id": target_id}),
    )
    .await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text => {
            crate::style::print_pane_swapped(pane_id, target_id);
        }
    }

    Ok(())
}

/// Handle the `shux pane kill` command.
pub async fn handle_pane_kill(
    stream: &mut tokio::net::UnixStream,
    session_name: &str,
    window_spec: Option<&str>,
    pane_id: &str,
    format: OutputFormat,
) -> anyhow::Result<()> {
    // Resolve window for validation
    let _ = resolve_pane_window_id(stream, session_name, window_spec).await?;

    let result = rpc_call(stream, "pane.kill", serde_json::json!({"pane_id": pane_id})).await?;

    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Text => {
            crate::style::print_pane_killed(pane_id);
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
            let git_sha = result.get("git_sha").and_then(|v| v.as_str());
            crate::style::print_version(version, git_sha, None);
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

    #[test]
    fn test_cli_parse_rename() {
        let cli = Cli::try_parse_from(["shux", "rename", "-s", "old", "-n", "new"]).unwrap();
        match cli.command {
            Some(Command::Rename { session, name }) => {
                assert_eq!(session, "old");
                assert_eq!(name, "new");
            }
            _ => panic!("expected Rename command"),
        }
    }

    #[test]
    fn test_cli_rename_requires_both_args() {
        let result = Cli::try_parse_from(["shux", "rename", "-s", "old"]);
        assert!(result.is_err());

        let result = Cli::try_parse_from(["shux", "rename", "-n", "new"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_cli_window_list() {
        let cli = Cli::try_parse_from(["shux", "window", "list", "-s", "work"]).unwrap();
        match cli.command {
            Some(Command::Window {
                command: WindowCommand::List { session },
            }) => {
                assert_eq!(session, "work");
            }
            _ => panic!("expected Window List command"),
        }
    }

    #[test]
    fn test_cli_window_list_alias() {
        let cli = Cli::try_parse_from(["shux", "window", "ls", "-s", "work"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Window {
                command: WindowCommand::List { .. }
            })
        ));
    }

    #[test]
    fn test_cli_window_alias() {
        let cli = Cli::try_parse_from(["shux", "win", "list", "-s", "work"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Window {
                command: WindowCommand::List { .. }
            })
        ));
    }

    #[test]
    fn test_cli_window_new() {
        let cli =
            Cli::try_parse_from(["shux", "window", "new", "-s", "work", "-n", "editor"]).unwrap();
        match cli.command {
            Some(Command::Window {
                command:
                    WindowCommand::New {
                        session,
                        name,
                        ensure,
                    },
            }) => {
                assert_eq!(session, "work");
                assert_eq!(name, Some("editor".to_string()));
                assert!(!ensure);
            }
            _ => panic!("expected Window New command"),
        }
    }

    #[test]
    fn test_cli_window_new_ensure() {
        let cli = Cli::try_parse_from(["shux", "window", "new", "-s", "work", "--ensure"]).unwrap();
        match cli.command {
            Some(Command::Window {
                command: WindowCommand::New { ensure, .. },
            }) => {
                assert!(ensure);
            }
            _ => panic!("expected Window New command"),
        }
    }

    #[test]
    fn test_cli_window_kill() {
        let cli =
            Cli::try_parse_from(["shux", "window", "kill", "-s", "work", "-w", "editor"]).unwrap();
        match cli.command {
            Some(Command::Window {
                command: WindowCommand::Kill { session, window },
            }) => {
                assert_eq!(session, "work");
                assert_eq!(window, "editor");
            }
            _ => panic!("expected Window Kill command"),
        }
    }

    #[test]
    fn test_cli_window_rename() {
        let cli = Cli::try_parse_from([
            "shux", "window", "rename", "-s", "work", "-w", "old", "-n", "new",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Window {
                command:
                    WindowCommand::Rename {
                        session,
                        window,
                        name,
                    },
            }) => {
                assert_eq!(session, "work");
                assert_eq!(window, "old");
                assert_eq!(name, "new");
            }
            _ => panic!("expected Window Rename command"),
        }
    }

    #[test]
    fn test_cli_window_focus() {
        let cli =
            Cli::try_parse_from(["shux", "window", "focus", "-s", "work", "-w", "0"]).unwrap();
        match cli.command {
            Some(Command::Window {
                command: WindowCommand::Focus { session, window },
            }) => {
                assert_eq!(session, "work");
                assert_eq!(window, "0");
            }
            _ => panic!("expected Window Focus command"),
        }
    }

    #[test]
    fn test_cli_window_reorder() {
        let cli = Cli::try_parse_from([
            "shux", "window", "reorder", "-s", "work", "-w", "editor", "-i", "2",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Window {
                command:
                    WindowCommand::Reorder {
                        session,
                        window,
                        index,
                    },
            }) => {
                assert_eq!(session, "work");
                assert_eq!(window, "editor");
                assert_eq!(index, 2);
            }
            _ => panic!("expected Window Reorder command"),
        }
    }
}
