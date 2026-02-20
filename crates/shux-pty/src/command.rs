//! Command execution engine with marker technique for detecting command completion.
//!
//! The marker technique works as follows:
//! 1. Generate a unique marker string (UUID-based)
//! 2. Send the command followed by an echo of `SHUX_MARKER<marker>EXIT<$?>SHUX_END`
//! 3. Monitor PTY output for the marker pattern
//! 4. When the marker is found, extract the exit code

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A running command tracked by the execution engine.
pub struct TrackedCommand {
    pub id: Uuid,
    pub pane_id: Uuid,
    pub command: String,
    pub marker: String,
    pub started_at: Instant,
    pub timeout: Duration,
    pub state: CommandState,
    /// Channel to notify the caller when the command completes (sync mode).
    pub completion_tx: Option<tokio::sync::oneshot::Sender<CommandResult>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandState {
    Running,
    Completed,
    TimedOut,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResult {
    pub command_id: Uuid,
    pub state: String,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub runtime_ms: u64,
}

/// The command execution engine manages running commands in panes.
pub struct CommandEngine {
    /// Active tracked commands, indexed by command ID.
    commands: HashMap<Uuid, TrackedCommand>,
    /// Map from marker string to command ID for fast marker detection.
    marker_index: HashMap<String, Uuid>,
    /// Per-pane output buffer for handling markers split across PTY chunks.
    pane_buffers: HashMap<Uuid, String>,
}

impl Default for CommandEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandEngine {
    pub fn new() -> Self {
        Self {
            commands: HashMap::new(),
            marker_index: HashMap::new(),
            pane_buffers: HashMap::new(),
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
        completion_tx: Option<tokio::sync::oneshot::Sender<CommandResult>>,
    ) -> (Uuid, String) {
        let command_id = Uuid::new_v4();
        let marker = format!("__SHUX_CMD_{}__", command_id.as_simple());

        let full_cmd = if args.is_empty() {
            command.to_string()
        } else {
            format!("{} {}", command, shell_escape_args(args))
        };

        // Build the PTY command: run the user command, capture exit code,
        // echo the marker with the exit code, then unset the variable.
        // Split the echo string ("SHUX_MAR""KER...") so the terminal's input echo
        // doesn't contain the literal marker pattern — only the actual echo output does.
        let pty_command = format!(
            "{cmd}; __shux_ec=$?; echo \"SHUX_MAR\"\"KER{marker}EXIT${{__shux_ec}}SHUX_END\"; unset __shux_ec\n",
            cmd = full_cmd,
            marker = marker,
        );

        let tracked = TrackedCommand {
            id: command_id,
            pane_id,
            command: full_cmd,
            marker: marker.clone(),
            started_at: Instant::now(),
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
    pub fn process_output(&mut self, pane_id: Uuid, output: &str) -> Vec<CommandResult> {
        // Append to per-pane buffer (handles markers split across chunks)
        let buf = self.pane_buffers.entry(pane_id).or_default();
        buf.push_str(output);

        let mut completed = Vec::new();
        let mut found_markers = Vec::new();

        // Scan for marker patterns in the buffer
        for (marker, cmd_id) in &self.marker_index {
            let pattern = format!("SHUX_MARKER{}EXIT", marker);
            if let Some(pos) = buf.find(&pattern) {
                let after = &buf[pos + pattern.len()..];
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
                                stdout: String::new(), // Filled from VT capture
                                runtime_ms: runtime.as_millis() as u64,
                            };

                            // Notify sync caller if present
                            if let Some(tx) = tracked.completion_tx.take() {
                                let _ = tx.send(result.clone());
                            }

                            completed.push(result);
                            found_markers.push(marker.clone());
                        }
                    }

                    // Trim the buffer up to and including the marker
                    let trim_to = pos + pattern.len() + end_pos + "SHUX_END".len();
                    if trim_to <= buf.len() {
                        *buf = buf[trim_to..].to_string();
                    }
                }
            }
        }

        // Clean up completed markers from index
        for marker in &found_markers {
            self.marker_index.remove(marker);
        }

        // Prevent unbounded buffer growth — keep only last 4K if no active markers for this pane
        let has_active = self
            .commands
            .values()
            .any(|t| t.pane_id == pane_id && t.state == CommandState::Running);
        if !has_active {
            if let Some(buf) = self.pane_buffers.get_mut(&pane_id) {
                buf.clear();
            }
        } else if let Some(buf) = self.pane_buffers.get_mut(&pane_id) {
            if buf.len() > 4096 {
                let start = buf.len() - 4096;
                *buf = buf[start..].to_string();
            }
        }

        completed
    }

    /// Check for timed-out commands. Returns timed-out command results and their pane IDs.
    pub fn check_timeouts(&mut self) -> Vec<(Uuid, CommandResult)> {
        let mut timed_out = Vec::new();
        let now = Instant::now();

        let ids: Vec<Uuid> = self
            .commands
            .iter()
            .filter(|(_, t)| {
                t.state == CommandState::Running && now.duration_since(t.started_at) > t.timeout
            })
            .map(|(id, _)| *id)
            .collect();

        for id in ids {
            if let Some(tracked) = self.commands.get_mut(&id) {
                tracked.state = CommandState::TimedOut;
                let pane_id = tracked.pane_id;
                let result = CommandResult {
                    command_id: id,
                    state: "timed_out".to_string(),
                    exit_code: None,
                    stdout: String::new(),
                    runtime_ms: tracked.timeout.as_millis() as u64,
                };

                if let Some(tx) = tracked.completion_tx.take() {
                    let _ = tx.send(result.clone());
                }

                self.marker_index.remove(&tracked.marker);
                timed_out.push((pane_id, result));
            }
        }

        timed_out
    }

    /// Cancel a running command. Returns the pane_id if found and cancelled.
    pub fn cancel_command(&mut self, command_id: Uuid) -> Option<Uuid> {
        if let Some(tracked) = self.commands.get_mut(&command_id) {
            if tracked.state == CommandState::Running {
                tracked.state = CommandState::Cancelled;
                let pane_id = tracked.pane_id;
                self.marker_index.remove(&tracked.marker);

                if let Some(tx) = tracked.completion_tx.take() {
                    let _ = tx.send(CommandResult {
                        command_id,
                        state: "cancelled".to_string(),
                        exit_code: None,
                        stdout: String::new(),
                        runtime_ms: tracked.started_at.elapsed().as_millis() as u64,
                    });
                }

                return Some(pane_id);
            }
        }
        None
    }

    /// Get the status of a command.
    pub fn get_status(&self, command_id: Uuid) -> Option<CommandResult> {
        self.commands.get(&command_id).map(|tracked| CommandResult {
            command_id,
            state: match tracked.state {
                CommandState::Running => "running",
                CommandState::Completed => "completed",
                CommandState::TimedOut => "timed_out",
                CommandState::Cancelled => "cancelled",
            }
            .to_string(),
            exit_code: None,
            stdout: String::new(),
            runtime_ms: tracked.started_at.elapsed().as_millis() as u64,
        })
    }
}

/// Shell-escape an argument list for safe PTY injection.
pub fn shell_escape_args(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            if a.contains(' ')
                || a.contains('"')
                || a.contains('\'')
                || a.contains('$')
                || a.contains('\\')
                || a.contains('`')
                || a.contains('!')
            {
                format!("'{}'", a.replace('\'', "'\\''"))
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_marker_detection_exit_0() {
        let mut engine = CommandEngine::new();
        let pane_id = Uuid::new_v4();

        let (cmd_id, _pty_cmd) = engine.start_command(
            pane_id,
            "echo",
            &["hello".to_string()],
            Duration::from_secs(10),
            None,
        );

        let marker = engine.commands.get(&cmd_id).unwrap().marker.clone();
        let output = format!("hello\r\nSHUX_MARKER{}EXIT0SHUX_END\r\n", marker);

        let completed = engine.process_output(pane_id, &output);
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].exit_code, Some(0));
        assert_eq!(completed[0].state, "completed");
        assert_eq!(completed[0].command_id, cmd_id);
    }

    #[test]
    fn test_marker_detection_nonzero_exit() {
        let mut engine = CommandEngine::new();
        let pane_id = Uuid::new_v4();

        let (cmd_id, _) =
            engine.start_command(pane_id, "false", &[], Duration::from_secs(10), None);

        let marker = engine.commands.get(&cmd_id).unwrap().marker.clone();
        let output = format!("SHUX_MARKER{}EXIT1SHUX_END\n", marker);

        let completed = engine.process_output(pane_id, &output);
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].exit_code, Some(1));
    }

    #[test]
    fn test_marker_split_across_chunks() {
        let mut engine = CommandEngine::new();
        let pane_id = Uuid::new_v4();

        let (cmd_id, _) = engine.start_command(
            pane_id,
            "echo",
            &["hello".to_string()],
            Duration::from_secs(10),
            None,
        );

        let marker = engine.commands.get(&cmd_id).unwrap().marker.clone();
        let full = format!("SHUX_MARKER{}EXIT0SHUX_END\n", marker);

        // Split the marker across two chunks
        let mid = full.len() / 2;
        let chunk1 = &full[..mid];
        let chunk2 = &full[mid..];

        let completed1 = engine.process_output(pane_id, chunk1);
        assert!(completed1.is_empty());

        let completed2 = engine.process_output(pane_id, chunk2);
        assert_eq!(completed2.len(), 1);
        assert_eq!(completed2[0].exit_code, Some(0));
    }

    #[test]
    fn test_cancel_command() {
        let mut engine = CommandEngine::new();
        let pane_id = Uuid::new_v4();

        let (cmd_id, _) = engine.start_command(
            pane_id,
            "sleep",
            &["60".to_string()],
            Duration::from_secs(300),
            None,
        );

        let result = engine.cancel_command(cmd_id);
        assert_eq!(result, Some(pane_id));

        let status = engine.get_status(cmd_id).unwrap();
        assert_eq!(status.state, "cancelled");
    }

    #[test]
    fn test_cancel_nonexistent() {
        let mut engine = CommandEngine::new();
        assert!(engine.cancel_command(Uuid::new_v4()).is_none());
    }

    #[test]
    fn test_cancel_already_completed() {
        let mut engine = CommandEngine::new();
        let pane_id = Uuid::new_v4();

        let (cmd_id, _) = engine.start_command(
            pane_id,
            "echo",
            &["hi".to_string()],
            Duration::from_secs(10),
            None,
        );

        // Complete it
        let marker = engine.commands.get(&cmd_id).unwrap().marker.clone();
        engine.process_output(pane_id, &format!("SHUX_MARKER{}EXIT0SHUX_END\n", marker));

        // Cancel should fail (already completed)
        assert!(engine.cancel_command(cmd_id).is_none());
    }

    #[test]
    fn test_wrong_pane_ignores_marker() {
        let mut engine = CommandEngine::new();
        let pane_a = Uuid::new_v4();
        let pane_b = Uuid::new_v4();

        let (cmd_id, _) = engine.start_command(pane_a, "echo", &[], Duration::from_secs(10), None);

        let marker = engine.commands.get(&cmd_id).unwrap().marker.clone();
        let output = format!("SHUX_MARKER{}EXIT0SHUX_END\n", marker);

        // Feed the marker to the wrong pane — should not complete
        let completed = engine.process_output(pane_b, &output);
        assert!(completed.is_empty());
    }

    #[test]
    fn test_shell_escape_simple() {
        assert_eq!(shell_escape_args(&["simple".to_string()]), "simple");
    }

    #[test]
    fn test_shell_escape_spaces() {
        assert_eq!(
            shell_escape_args(&["hello world".to_string()]),
            "'hello world'"
        );
    }

    #[test]
    fn test_shell_escape_quotes() {
        assert_eq!(shell_escape_args(&["it's".to_string()]), "'it'\\''s'");
    }

    #[test]
    fn test_shell_escape_multiple() {
        assert_eq!(
            shell_escape_args(&["a".to_string(), "b c".to_string(), "d".to_string()]),
            "a 'b c' d"
        );
    }

    #[test]
    fn test_get_status_running() {
        let mut engine = CommandEngine::new();
        let pane_id = Uuid::new_v4();

        let (cmd_id, _) = engine.start_command(
            pane_id,
            "sleep",
            &["10".to_string()],
            Duration::from_secs(300),
            None,
        );

        let status = engine.get_status(cmd_id).unwrap();
        assert_eq!(status.state, "running");
    }

    #[test]
    fn test_get_status_nonexistent() {
        let engine = CommandEngine::new();
        assert!(engine.get_status(Uuid::new_v4()).is_none());
    }
}
