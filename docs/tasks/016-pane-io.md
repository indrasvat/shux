# 016 — Pane I/O (send_keys, run_command, capture)

**Status:** Done
**Depends On:** 015, 004
**Parallelizable With:** 020

---

## Problem

Pane structural operations (task 015) let users and agents create, focus, and arrange panes. But there is no way to interact with the processes running inside those panes. Agents need to send keystrokes, run commands programmatically, poll running commands, cancel them, and capture output. Humans need the same operations exposed through the API for scripting.

The `pane.run_command` method is the single most important agent-facing operation in shux. It provides deterministic command execution: send a command, wait for it to complete, and get the exit code plus captured stdout/stderr. This is what replaces the fragile pattern of `tmux send-keys "make test" Enter` followed by `sleep 10; tmux capture-pane`.

The key design challenge is the **marker technique**: since we are writing to a PTY (not piping to a subprocess), we must detect when a command has completed by injecting a unique marker into the output stream and watching for it. This is how `pane.run_command` achieves synchronous semantics over an inherently asynchronous PTY.

## PRD Reference

- **PRD section 8.2 (pane.send_keys, pane.run_command, pane.capture, pane.command_status, pane.command_cancel)**: Full method signatures and semantics
- **PRD section 8.5 (Agent-safe patterns)**: "Prefer `pane.run_command` over `pane.send_keys`"
- **PRD section 7.5 (WIT host interface)**: send-keys (list<u8>), send-text (string), read-pane-output, read-pane-scrollback

---

## Files to Create

- `crates/shux-rpc/src/methods/pane_io.rs` — JSON-RPC handlers for pane I/O methods
- `crates/shux-pty/src/command.rs` — Command execution engine (marker technique, timeout, async/sync)
- `crates/shux-pty/src/capture.rs` — Scrollback capture and ANSI stripping
- `crates/shux-rpc/tests/pane_io_api.rs` — L3 API contract tests

## Files to Modify

- `crates/shux-pty/src/lib.rs` — Export command and capture modules
- `crates/shux-rpc/src/methods/mod.rs` — Register pane_io module
- `crates/shux-rpc/src/router.rs` — Register pane I/O method handlers
- `crates/shux-core/src/events.rs` — Add pane.command_completed event type
- `crates/shux-vt/src/lib.rs` — Add scrollback read and ANSI strip methods to VirtualTerminal

---

## Execution Steps

### Step 1: Implement pane.send_keys

The simplest I/O operation: write raw bytes to a pane's PTY input. This is a direct write to the PTY master file descriptor.

In `crates/shux-rpc/src/methods/pane_io.rs`:

```rust
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct SendKeysParams {
    pub pane_id: Uuid,
    /// Raw bytes to write to the PTY. Can be base64-encoded or a UTF-8 string.
    /// If `data` is present, write raw bytes. If `text` is present, write UTF-8.
    #[serde(default)]
    pub data: Option<String>, // base64-encoded bytes
    #[serde(default)]
    pub text: Option<String>, // UTF-8 text (convenience)
}

pub async fn handle_send_keys(
    state: &AppState,
    params: SendKeysParams,
) -> Result<serde_json::Value, RpcError> {
    // Resolve the PTY handle for this pane
    let snapshot = state.graph.load();
    let pane = snapshot.panes.get(&params.pane_id)
        .ok_or_else(|| RpcError::new(-32002, "pane not found"))?;

    let bytes = if let Some(data) = params.data {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.decode(&data)
            .map_err(|e| RpcError::invalid_params(format!("invalid base64: {}", e)))?
    } else if let Some(text) = params.text {
        text.into_bytes()
    } else {
        return Err(RpcError::invalid_params("either 'data' (base64) or 'text' required"));
    };

    state.pty_manager
        .write_to_pane(params.pane_id, &bytes)
        .await
        .map_err(|e| RpcError::internal(format!("PTY write failed: {}", e)))?;

    Ok(serde_json::json!({ "bytes_written": bytes.len() }))
}
```

### Step 2: Implement the command execution engine with marker technique

The marker technique works as follows:
1. Generate a unique marker string (UUID-based, unlikely to appear in normal output)
2. Send the command followed by `; echo "SHUX_MARKER_<uuid>_$?"` to the PTY
3. Monitor PTY output for the marker pattern
4. When the marker is found, extract the exit code and capture the output between the command start and the marker

In `crates/shux-pty/src/command.rs`:

```rust
use uuid::Uuid;
use std::time::Duration;
use tokio::sync::oneshot;

/// A running command tracked by the execution engine.
#[derive(Debug)]
pub struct TrackedCommand {
    pub id: Uuid,
    pub pane_id: Uuid,
    pub command: String,
    pub args: Vec<String>,
    pub marker: String,
    pub started_at: std::time::Instant,
    pub timeout: Duration,
    pub state: CommandState,
    /// Channel to notify the caller when the command completes (sync mode).
    pub completion_tx: Option<oneshot::Sender<CommandResult>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandState {
    Running,
    Completed,
    Failed,
    TimedOut,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResult {
    pub command_id: Uuid,
    pub state: String,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub runtime_ms: u64,
}

/// The command execution engine manages running commands in panes.
pub struct CommandEngine {
    /// Active tracked commands, indexed by command ID.
    commands: HashMap<Uuid, TrackedCommand>,
    /// Map from marker string to command ID for fast marker detection.
    marker_index: HashMap<String, Uuid>,
}

impl CommandEngine {
    pub fn new() -> Self {
        Self {
            commands: HashMap::new(),
            marker_index: HashMap::new(),
        }
    }

    /// Start tracking a new command.
    /// Returns the command ID and the full command string to send to the PTY.
    pub fn start_command(
        &mut self,
        pane_id: Uuid,
        command: &str,
        args: &[String],
        timeout: Duration,
        completion_tx: Option<oneshot::Sender<CommandResult>>,
    ) -> (Uuid, String) {
        let command_id = Uuid::new_v4();
        let marker = format!("__SHUX_CMD_{}__", command_id.as_simple());

        // Build the command string with marker injection.
        // We use a subshell to capture the exit code precisely.
        // The marker format is: SHUX_MARKER<marker>EXIT<exit_code>SHUX_END
        let full_args = if args.is_empty() {
            command.to_string()
        } else {
            format!("{} {}", command, shell_escape_args(args))
        };

        let pty_command = format!(
            "{cmd}; __shux_ec=$?; echo \"SHUX_MARKER{marker}EXIT${{__shux_ec}}SHUX_END\"; \
             unset __shux_ec\n",
            cmd = full_args,
            marker = marker,
        );

        let tracked = TrackedCommand {
            id: command_id,
            pane_id,
            command: command.to_string(),
            args: args.to_vec(),
            marker: marker.clone(),
            started_at: std::time::Instant::now(),
            timeout,
            state: CommandState::Running,
            completion_tx,
        };

        self.marker_index.insert(marker, command_id);
        self.commands.insert(command_id, tracked);

        (command_id, pty_command)
    }

    /// Called when PTY output is received. Scans for markers.
    /// Returns completed commands, if any.
    pub fn process_output(
        &mut self,
        pane_id: Uuid,
        output: &str,
    ) -> Vec<CommandResult> {
        let mut completed = Vec::new();

        // Scan for marker pattern: SHUX_MARKER<marker>EXIT<code>SHUX_END
        for (marker, cmd_id) in &self.marker_index {
            let pattern = format!("SHUX_MARKER{}EXIT", marker);
            if let Some(pos) = output.find(&pattern) {
                let after = &output[pos + pattern.len()..];
                if let Some(end_pos) = after.find("SHUX_END") {
                    let exit_code_str = &after[..end_pos];
                    let exit_code = exit_code_str.trim().parse::<i32>().ok();

                    if let Some(tracked) = self.commands.get_mut(cmd_id) {
                        if tracked.pane_id == pane_id && tracked.state == CommandState::Running {
                            tracked.state = CommandState::Completed;
                            let runtime = tracked.started_at.elapsed();

                            let result = CommandResult {
                                command_id: *cmd_id,
                                state: "completed".to_string(),
                                exit_code,
                                stdout: String::new(), // Filled from capture
                                stderr: String::new(), // Not separately capturable via PTY
                                runtime_ms: runtime.as_millis() as u64,
                            };

                            // Notify sync caller if present
                            if let Some(tx) = tracked.completion_tx.take() {
                                let _ = tx.send(result.clone());
                            }

                            completed.push(result);
                        }
                    }
                }
            }
        }

        // Clean up completed commands from index
        for result in &completed {
            if let Some(tracked) = self.commands.get(&result.command_id) {
                self.marker_index.remove(&tracked.marker);
            }
        }

        completed
    }

    /// Check for timed-out commands.
    pub fn check_timeouts(&mut self) -> Vec<CommandResult> {
        let mut timed_out = Vec::new();
        let now = std::time::Instant::now();

        for (id, tracked) in &mut self.commands {
            if tracked.state == CommandState::Running
                && now.duration_since(tracked.started_at) > tracked.timeout
            {
                tracked.state = CommandState::TimedOut;
                let result = CommandResult {
                    command_id: *id,
                    state: "timed_out".to_string(),
                    exit_code: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    runtime_ms: tracked.timeout.as_millis() as u64,
                };

                if let Some(tx) = tracked.completion_tx.take() {
                    let _ = tx.send(result.clone());
                }

                timed_out.push(result);
            }
        }

        timed_out
    }

    /// Cancel a running command by sending SIGTERM to the pane's foreground process.
    pub fn cancel_command(&mut self, command_id: Uuid) -> Option<Uuid> {
        if let Some(tracked) = self.commands.get_mut(&command_id) {
            if tracked.state == CommandState::Running {
                tracked.state = CommandState::Cancelled;
                self.marker_index.remove(&tracked.marker);

                if let Some(tx) = tracked.completion_tx.take() {
                    let _ = tx.send(CommandResult {
                        command_id,
                        state: "cancelled".to_string(),
                        exit_code: None,
                        stdout: String::new(),
                        stderr: String::new(),
                        runtime_ms: tracked.started_at.elapsed().as_millis() as u64,
                    });
                }

                return Some(tracked.pane_id);
            }
        }
        None
    }

    /// Get the status of a command.
    pub fn get_status(&self, command_id: Uuid) -> Option<CommandResult> {
        self.commands.get(&command_id).map(|tracked| {
            CommandResult {
                command_id,
                state: match tracked.state {
                    CommandState::Running => "running",
                    CommandState::Completed => "completed",
                    CommandState::Failed => "failed",
                    CommandState::TimedOut => "timed_out",
                    CommandState::Cancelled => "cancelled",
                }.to_string(),
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                runtime_ms: tracked.started_at.elapsed().as_millis() as u64,
            }
        })
    }
}

fn shell_escape_args(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            if a.contains(' ') || a.contains('"') || a.contains('\'')
                || a.contains('$') || a.contains('\\')
            {
                format!("'{}'", a.replace('\'', "'\\''"))
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
```

### Step 3: Implement scrollback capture with ANSI stripping

In `crates/shux-pty/src/capture.rs`:

```rust
use uuid::Uuid;

/// Capture visible lines from a pane's VirtualTerminal grid.
/// ANSI escape sequences are stripped, returning clean UTF-8 text.
pub fn capture_pane_output(
    vt: &shux_vt::VirtualTerminal,
    lines: usize,
) -> String {
    let grid = vt.grid();
    let total_lines = grid.visible_line_count();
    let start = total_lines.saturating_sub(lines);

    let mut output = String::new();
    for line_idx in start..total_lines {
        let line = grid.get_line(line_idx);
        let text = line.to_string(); // Rendered as plain text
        output.push_str(text.trim_end()); // Trim trailing whitespace
        output.push('\n');
    }

    // Remove trailing empty lines
    while output.ends_with("\n\n") {
        output.pop();
    }

    output
}

/// Strip ANSI escape sequences from a string.
/// Handles CSI, OSC, DCS, and single-character escapes.
pub fn strip_ansi(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // ESC sequence
            match chars.peek() {
                Some('[') => {
                    // CSI sequence: ESC [ ... final_byte
                    chars.next(); // consume '['
                    while let Some(&c) = chars.peek() {
                        if c.is_ascii_alphabetic() || c == '@' || c == '`'
                            || c == '{' || c == '|' || c == '}' || c == '~'
                        {
                            chars.next(); // consume final byte
                            break;
                        }
                        chars.next(); // consume parameter/intermediate byte
                    }
                }
                Some(']') => {
                    // OSC sequence: ESC ] ... ST (BEL or ESC \)
                    chars.next(); // consume ']'
                    while let Some(c) = chars.next() {
                        if c == '\x07' { break; } // BEL
                        if c == '\x1b' {
                            if chars.peek() == Some(&'\\') {
                                chars.next(); // consume '\'
                            }
                            break;
                        }
                    }
                }
                Some('(') | Some(')') | Some('*') | Some('+') => {
                    // Designate character set: ESC ( C
                    chars.next();
                    chars.next();
                }
                Some(_) => {
                    // Single-character escape (e.g., ESC M, ESC 7, ESC 8)
                    chars.next();
                }
                None => {}
            }
        } else if ch == '\x9b' {
            // CSI (8-bit form): skip like CSI above
            while let Some(&c) = chars.peek() {
                if c.is_ascii_alphabetic() || c == '@' {
                    chars.next();
                    break;
                }
                chars.next();
            }
        } else {
            output.push(ch);
        }
    }

    output
}
```

### Step 4: Implement JSON-RPC handlers for pane.run_command and friends

In `crates/shux-rpc/src/methods/pane_io.rs` (extending step 1):

```rust
#[derive(Debug, Deserialize)]
pub struct RunCommandParams {
    pub pane_id: Uuid,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Timeout in seconds (default: 300)
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    /// If true, return command_id immediately instead of waiting
    #[serde(default)]
    pub r#async: bool,
}

fn default_timeout() -> u64 { 300 }

#[derive(Debug, Deserialize)]
pub struct CommandStatusParams {
    pub command_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct CommandCancelParams {
    pub command_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct CaptureParams {
    pub pane_id: Uuid,
    #[serde(default = "default_capture_lines")]
    pub lines: usize,
}

fn default_capture_lines() -> usize { 50 }

/// Handle pane.run_command: execute a command in a pane and return the result.
pub async fn handle_run_command(
    state: &AppState,
    params: RunCommandParams,
) -> Result<serde_json::Value, RpcError> {
    // Verify pane exists
    let snapshot = state.graph.load();
    if !snapshot.panes.contains_key(&params.pane_id) {
        return Err(RpcError::new(-32002, "pane not found"));
    }

    let timeout = Duration::from_secs(params.timeout);

    if params.r#async {
        // Async mode: start command and return ID immediately
        let (command_id, pty_command) = state.command_engine.lock().await
            .start_command(
                params.pane_id,
                &params.command,
                &params.args,
                timeout,
                None, // No completion channel for async
            );

        // Send the command to the PTY
        state.pty_manager
            .write_to_pane(params.pane_id, pty_command.as_bytes())
            .await
            .map_err(|e| RpcError::internal(format!("PTY write failed: {}", e)))?;

        Ok(serde_json::json!({
            "command_id": command_id,
            "state": "running",
        }))
    } else {
        // Sync mode: wait for completion
        let (completion_tx, completion_rx) = oneshot::channel();

        let (command_id, pty_command) = state.command_engine.lock().await
            .start_command(
                params.pane_id,
                &params.command,
                &params.args,
                timeout,
                Some(completion_tx),
            );

        // Send the command to the PTY
        state.pty_manager
            .write_to_pane(params.pane_id, pty_command.as_bytes())
            .await
            .map_err(|e| RpcError::internal(format!("PTY write failed: {}", e)))?;

        // Wait for completion or timeout
        match tokio::time::timeout(timeout + Duration::from_secs(5), completion_rx).await {
            Ok(Ok(result)) => {
                // Capture output after command completes
                let captured = state.capture_pane(params.pane_id, 500);
                let mut result = result;
                result.stdout = captured;

                // Emit event
                let _ = state.event_tx.send(Event::pane(PaneEvent::CommandCompleted {
                    command_id,
                    pane_id: params.pane_id,
                    exit_code: result.exit_code,
                }));

                Ok(serde_json::to_value(result).unwrap())
            }
            Ok(Err(_)) => {
                Err(RpcError::internal("command tracking lost"))
            }
            Err(_) => {
                // Timeout exceeded even the grace period
                state.command_engine.lock().await.cancel_command(command_id);
                Err(RpcError::new(-32007, "command timed out"))
            }
        }
    }
}

/// Handle pane.command_status: check on a running async command.
pub async fn handle_command_status(
    state: &AppState,
    params: CommandStatusParams,
) -> Result<serde_json::Value, RpcError> {
    let engine = state.command_engine.lock().await;
    match engine.get_status(params.command_id) {
        Some(result) => Ok(serde_json::to_value(result).unwrap()),
        None => Err(RpcError::new(-32002, "command not found")),
    }
}

/// Handle pane.command_cancel: cancel a running async command.
pub async fn handle_command_cancel(
    state: &AppState,
    params: CommandCancelParams,
) -> Result<serde_json::Value, RpcError> {
    let pane_id = state.command_engine.lock().await
        .cancel_command(params.command_id);

    match pane_id {
        Some(pane_id) => {
            // Send SIGTERM to the pane's foreground process group
            state.pty_manager.signal_foreground(pane_id, nix::sys::signal::Signal::SIGTERM).await;

            // Schedule SIGKILL after 5 seconds if still running
            let pty = state.pty_manager.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(5)).await;
                pty.signal_foreground(pane_id, nix::sys::signal::Signal::SIGKILL).await;
            });

            Ok(serde_json::json!({
                "cancelled": true,
                "command_id": params.command_id,
            }))
        }
        None => Err(RpcError::new(-32002, "command not found or already finished")),
    }
}

/// Handle pane.capture: capture scrollback content from a pane.
pub async fn handle_capture(
    state: &AppState,
    params: CaptureParams,
) -> Result<serde_json::Value, RpcError> {
    let snapshot = state.graph.load();
    if !snapshot.panes.contains_key(&params.pane_id) {
        return Err(RpcError::new(-32002, "pane not found"));
    }

    let content = state.capture_pane(params.pane_id, params.lines);
    Ok(serde_json::json!({
        "pane_id": params.pane_id,
        "lines": params.lines,
        "content": content,
    }))
}
```

### Step 5: Add pane.command_completed event

In `crates/shux-core/src/events.rs`, add to PaneEvent:

```rust
#[serde(rename = "pane.command_completed")]
CommandCompleted {
    command_id: Uuid,
    pane_id: Uuid,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    runtime_ms: u64,
},
```

### Step 6: Integrate command engine with PTY output loop

The PTY manager's output reading loop must feed output through the command engine to detect markers. This integration happens in the daemon's main event loop.

```rust
// In the PTY output reading task (crates/shux-pty/src/manager.rs)
async fn pty_read_loop(
    pane_id: Uuid,
    pty_reader: impl AsyncRead + Unpin,
    vt_tx: mpsc::Sender<(Uuid, Vec<u8>)>,
    command_engine: Arc<Mutex<CommandEngine>>,
    event_tx: broadcast::Sender<Event>,
) {
    let mut buf = vec![0u8; 4096];

    loop {
        match pty_reader.read(&mut buf).await {
            Ok(0) => break, // EOF — PTY closed
            Ok(n) => {
                let data = &buf[..n];

                // Forward to VT parser
                let _ = vt_tx.send((pane_id, data.to_vec())).await;

                // Check for command completion markers
                if let Ok(output_str) = std::str::from_utf8(data) {
                    let completed = command_engine.lock().await
                        .process_output(pane_id, output_str);

                    for result in completed {
                        let _ = event_tx.send(Event::pane(PaneEvent::CommandCompleted {
                            command_id: result.command_id,
                            pane_id,
                            exit_code: result.exit_code,
                            stdout: result.stdout.clone(),
                            stderr: result.stderr.clone(),
                            runtime_ms: result.runtime_ms,
                        }));
                    }
                }
            }
            Err(e) => {
                tracing::warn!("PTY read error for pane {}: {}", pane_id, e);
                break;
            }
        }
    }
}
```

### Step 7: Implement timeout checker background task

```rust
// Spawned as a background task in the daemon
async fn command_timeout_checker(
    command_engine: Arc<Mutex<CommandEngine>>,
    pty_manager: Arc<PtyManager>,
    event_tx: broadcast::Sender<Event>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));

    loop {
        interval.tick().await;

        let timed_out = command_engine.lock().await.check_timeouts();

        for result in timed_out {
            // Kill the timed-out process
            if let Some(tracked) = command_engine.lock().await.commands.get(&result.command_id) {
                pty_manager.signal_foreground(
                    tracked.pane_id,
                    nix::sys::signal::Signal::SIGTERM,
                ).await;
            }

            let _ = event_tx.send(Event::pane(PaneEvent::CommandCompleted {
                command_id: result.command_id,
                pane_id: Uuid::nil(), // resolve from command
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                runtime_ms: result.runtime_ms,
            }));
        }
    }
}
```

### Step 8: Write L3 API contract tests

In `crates/shux-rpc/tests/pane_io_api.rs`:

```rust
#[tokio::test]
async fn test_send_keys_text() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let session = client.call("session.create", serde_json::json!({ "name": "test" })).await.unwrap();
    let pane_id = session["pane_id"].as_str().unwrap();

    let result = client.call("pane.send_keys", serde_json::json!({
        "pane_id": pane_id,
        "text": "echo hello\n",
    })).await.unwrap();

    assert!(result["bytes_written"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn test_send_keys_base64() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let session = client.call("session.create", serde_json::json!({ "name": "test" })).await.unwrap();
    let pane_id = session["pane_id"].as_str().unwrap();

    // Send Ctrl+C (0x03) as base64
    let result = client.call("pane.send_keys", serde_json::json!({
        "pane_id": pane_id,
        "data": "Aw==", // base64 of 0x03
    })).await.unwrap();

    assert_eq!(result["bytes_written"], 1);
}

#[tokio::test]
async fn test_run_command_sync() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let session = client.call("session.create", serde_json::json!({ "name": "test" })).await.unwrap();
    let pane_id = session["pane_id"].as_str().unwrap();

    // Wait for shell to be ready
    tokio::time::sleep(Duration::from_millis(500)).await;

    let result = client.call("pane.run_command", serde_json::json!({
        "pane_id": pane_id,
        "command": "echo",
        "args": ["hello"],
        "timeout": 10,
    })).await.unwrap();

    assert_eq!(result["state"], "completed");
    assert_eq!(result["exit_code"], 0);
    assert!(result["stdout"].as_str().unwrap().contains("hello"));
}

#[tokio::test]
async fn test_run_command_async() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let session = client.call("session.create", serde_json::json!({ "name": "test" })).await.unwrap();
    let pane_id = session["pane_id"].as_str().unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;

    let start_result = client.call("pane.run_command", serde_json::json!({
        "pane_id": pane_id,
        "command": "sleep",
        "args": ["1"],
        "timeout": 10,
        "async": true,
    })).await.unwrap();

    let command_id = start_result["command_id"].as_str().unwrap();
    assert_eq!(start_result["state"], "running");

    // Poll for completion
    tokio::time::sleep(Duration::from_secs(2)).await;

    let status = client.call("pane.command_status", serde_json::json!({
        "command_id": command_id,
    })).await.unwrap();

    assert_eq!(status["state"], "completed");
}

#[tokio::test]
async fn test_run_command_cancel() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let session = client.call("session.create", serde_json::json!({ "name": "test" })).await.unwrap();
    let pane_id = session["pane_id"].as_str().unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;

    let start = client.call("pane.run_command", serde_json::json!({
        "pane_id": pane_id,
        "command": "sleep",
        "args": ["60"],
        "timeout": 300,
        "async": true,
    })).await.unwrap();

    let command_id = start["command_id"].as_str().unwrap();

    let cancel = client.call("pane.command_cancel", serde_json::json!({
        "command_id": command_id,
    })).await.unwrap();

    assert_eq!(cancel["cancelled"], true);
}

#[tokio::test]
async fn test_capture_pane() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let session = client.call("session.create", serde_json::json!({ "name": "test" })).await.unwrap();
    let pane_id = session["pane_id"].as_str().unwrap();

    // Send some output
    client.call("pane.send_keys", serde_json::json!({
        "pane_id": pane_id,
        "text": "echo capture_test_output\n",
    })).await.unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;

    let result = client.call("pane.capture", serde_json::json!({
        "pane_id": pane_id,
        "lines": 20,
    })).await.unwrap();

    let content = result["content"].as_str().unwrap();
    assert!(content.contains("capture_test_output"));
}

#[tokio::test]
async fn test_send_keys_nonexistent_pane_fails() {
    let daemon = TestDaemon::start().await;
    let mut client = daemon.connect().await;

    let err = client.call("pane.send_keys", serde_json::json!({
        "pane_id": "00000000-0000-0000-0000-000000000000",
        "text": "hello",
    })).await.unwrap_err();

    assert_eq!(err.code, -32002);
}
```

### Step 9: Unit tests for ANSI stripping and command engine

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_ansi_removes_csi() {
        assert_eq!(strip_ansi("\x1b[31mhello\x1b[0m"), "hello");
    }

    #[test]
    fn test_strip_ansi_removes_osc() {
        assert_eq!(strip_ansi("\x1b]0;title\x07text"), "text");
    }

    #[test]
    fn test_strip_ansi_preserves_plain_text() {
        assert_eq!(strip_ansi("hello world"), "hello world");
    }

    #[test]
    fn test_strip_ansi_handles_multiline() {
        let input = "\x1b[32mline1\x1b[0m\nline2\n\x1b[1mline3\x1b[0m";
        assert_eq!(strip_ansi(input), "line1\nline2\nline3");
    }

    #[test]
    fn test_marker_detection() {
        let mut engine = CommandEngine::new();
        let pane_id = Uuid::new_v4();

        let (cmd_id, _) = engine.start_command(
            pane_id, "echo", &["hello".to_string()],
            Duration::from_secs(10), None,
        );

        let marker = engine.commands.get(&cmd_id).unwrap().marker.clone();
        let output = format!("hello\nSHUX_MARKER{}EXIT0SHUX_END\n", marker);

        let completed = engine.process_output(pane_id, &output);
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].exit_code, Some(0));
        assert_eq!(completed[0].state, "completed");
    }

    #[test]
    fn test_marker_nonzero_exit_code() {
        let mut engine = CommandEngine::new();
        let pane_id = Uuid::new_v4();

        let (cmd_id, _) = engine.start_command(
            pane_id, "false", &[],
            Duration::from_secs(10), None,
        );

        let marker = engine.commands.get(&cmd_id).unwrap().marker.clone();
        let output = format!("SHUX_MARKER{}EXIT1SHUX_END\n", marker);

        let completed = engine.process_output(pane_id, &output);
        assert_eq!(completed[0].exit_code, Some(1));
    }

    #[test]
    fn test_shell_escape() {
        assert_eq!(shell_escape_args(&["hello world".to_string()]), "'hello world'");
        assert_eq!(shell_escape_args(&["simple".to_string()]), "simple");
    }
}
```

---

## Verification

### Functional

```bash
# Test send_keys via CLI
shux new -s test --no-attach
echo '{"jsonrpc":"2.0","id":1,"method":"pane.send_keys","params":{"pane_id":"<id>","text":"echo hello\n"}}' | shux api call

# Test run_command sync
echo '{"jsonrpc":"2.0","id":2,"method":"pane.run_command","params":{"pane_id":"<id>","command":"echo","args":["hello"]}}' | shux api call
# Expected: {"exit_code":0,"stdout":"hello\n","state":"completed"}

# Test capture
echo '{"jsonrpc":"2.0","id":3,"method":"pane.capture","params":{"pane_id":"<id>","lines":10}}' | shux api call
```

### Tests

```bash
# Unit tests for ANSI stripping
cargo nextest run -p shux-pty --lib -- capture

# Unit tests for command engine
cargo nextest run -p shux-pty --lib -- command

# API contract tests
cargo nextest run -p shux-rpc --test pane_io_api

# All tests
cargo nextest run --workspace

# Clippy
cargo clippy --workspace --all-targets -- -D warnings
```

---

## Completion Criteria

- [ ] `pane.send_keys` writes raw bytes or UTF-8 text to a pane's PTY input
- [ ] `pane.send_keys` supports both `data` (base64) and `text` (UTF-8) parameters
- [ ] `pane.run_command` (sync mode) sends command, waits for marker, returns exit_code + stdout
- [ ] `pane.run_command` (async mode) returns command_id immediately
- [ ] `pane.command_status` returns current state of a tracked async command
- [ ] `pane.command_cancel` sends SIGTERM, then SIGKILL after 5s
- [ ] `pane.capture` returns ANSI-stripped UTF-8 content from pane scrollback
- [ ] Default timeout for run_command is 300s (configurable per call)
- [ ] Marker technique correctly detects command completion and extracts exit codes
- [ ] Command engine handles non-zero exit codes correctly
- [ ] Timed-out commands are detected and reported within ~1s of timeout
- [ ] Event emitted: pane.command_completed with command_id, exit_code, stdout, stderr
- [ ] ANSI stripping handles CSI, OSC, DCS, and character set designation sequences
- [ ] Unit tests pass for ANSI stripping, command engine marker detection, shell escaping
- [ ] L3 API contract tests pass: send_keys, run_command (sync + async), capture, cancel
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes
- [ ] `cargo nextest run --workspace` passes

---

## Commit Message

```
feat: implement pane I/O operations for agent-driven workflows

- Add pane.send_keys (raw bytes and UTF-8 text), pane.run_command
  (sync and async modes), pane.command_status, pane.command_cancel,
  and pane.capture JSON-RPC methods
- Command execution engine with marker technique for detecting
  completion through PTY output stream
- ANSI escape sequence stripping for clean text capture
- Async commands with polling and cancellation (SIGTERM + SIGKILL)
- Default 300s timeout with per-call override
- Emit pane.command_completed events on command completion
- Unit tests for marker detection, ANSI stripping, shell escaping
- L3 API contract tests for all I/O operations
```

---

## Session Protocol

1. **Before starting:** Verify tasks 015 and 004 are complete. Pane operations and PTY manager must work. Verify you can split a pane and that PTY I/O flows correctly (output appears in VT grid). Read `CLAUDE.md`.
2. **During:** Start with `pane.send_keys` (simplest). Then implement the ANSI stripping (standalone, easy to test). Then tackle the command engine (most complex). Finally wire everything into the RPC handlers. Run unit tests frequently.
3. **Key design decisions:**
   - The marker technique is not perfect. It assumes the shell echoes output synchronously. Some shells may reorder or buffer output. The marker pattern `SHUX_MARKER<uuid>EXIT<code>SHUX_END` is designed to be unique enough to never collide with real output.
   - `pane.run_command` captures stdout by reading from the VT grid after the marker is detected, not by intercepting PTY output directly. This means "stdout" is what was displayed, not raw subprocess output.
   - The command engine uses a `Mutex` because it is accessed from both the RPC handler (to start commands) and the PTY read loop (to detect markers). Contention is low since marker detection is fast.
4. **After:** Run full verification. Update `docs/PROGRESS.md`. This task is the foundation for agent integration testing.
