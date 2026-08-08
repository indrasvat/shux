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

                    if let Some(tracked) = self.commands.get_mut(cmd_id)
                        && tracked.pane_id == pane_id
                        && tracked.state == CommandState::Running
                    {
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
        } else if let Some(buf) = self.pane_buffers.get_mut(&pane_id)
            && buf.len() > 4096
        {
            let start = buf.len() - 4096;
            *buf = buf[start..].to_string();
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
        if let Some(tracked) = self.commands.get_mut(&command_id)
            && tracked.state == CommandState::Running
        {
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

/// POSIX-shell-quote one argument so a shell re-reading the line recovers
/// **exactly** this string, as **one** word.
///
/// The rule is an allowlist, not a denylist. A denylist of metacharacters is
/// always one character short of the shell it is guarding — this function used
/// to name seven (space, `"`, `'`, `$`, `\`, `` ` ``, `!`) and let `;`, `|`,
/// `&`, `>`, `<`, `(`, `)`, `*`, `?`, `~`, `#`, a newline and a tab through
/// untouched. That mattered: [`CommandEngine::start_command`] writes the result
/// into a **live shell** on the pane's PTY, so an argument of `a;id` ran `id`.
///
/// Two consumers share this one implementation, deliberately:
///
/// * `pane.run` builds the line it injects, where a wrong answer executes.
/// * `pane list` renders a pane's argv for a human, where a wrong answer makes
///   `["sh", "-c", "a b"]` and `["sh", "-c", "a", "b"]` look identical
///   (issue #135).
///
/// They want the same thing — a faithful, unambiguous rendering of an argv —
/// and two implementations of that would be free to disagree about which
/// characters are safe.
///
/// The allowlist is ASCII-only. Every byte of a non-ASCII scalar is `>= 0x80`,
/// fails the test, and gets quoted: no shell treats those bytes as
/// metacharacters, so quoting them is unnecessary but never wrong, and it keeps
/// the rule free of Unicode edge cases.
///
/// `=` is allowed *inside* a word because `--flag=value` is ordinary, and an
/// assignment is only an assignment in a command's *first* word, which nothing
/// here ever is. A **leading** `=` is a different matter: zsh's `EQUALS` option
/// is on by default, so a bare `=ls` is rewritten to `/usr/bin/ls`, and a bare
/// `=nosuchprog` is a fatal error that aborts the whole line — taking the
/// completion marker [`CommandEngine::start_command`] appends with it, so the
/// caller waits out its full timeout instead of getting an answer. A leading
/// `=` is therefore always quoted.
///
/// `~` is deliberately NOT on the allowlist and must stay off it: bash expands
/// a tilde after an unquoted `=` or `:`, so allowing it would make `a=~` and
/// `PATH=/x:~` expand mid-word.
pub fn shell_quote_arg(arg: &str) -> String {
    // The empty string is the case a denylist never catches: it contains
    // nothing, so nothing triggers, and the argument DISAPPEARS from the line.
    if arg.is_empty() {
        return "''".to_string();
    }
    if arg.starts_with('=') {
        return format!("'{}'", arg.replace('\'', "'\\''"));
    }
    let unquoted_is_literal = arg.bytes().all(|b| {
        b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'_' | b'-' | b'.' | b'/' | b',' | b':' | b'=' | b'+' | b'@' | b'%'
            )
    });
    if unquoted_is_literal {
        return arg.to_string();
    }
    // Single quotes suspend every expansion a shell has, so the only character
    // that needs handling inside them is the closing quote itself.
    format!("'{}'", arg.replace('\'', "'\\''"))
}

/// Shell-quote an argument list into one space-separated line. See
/// [`shell_quote_arg`].
pub fn shell_escape_args(args: &[String]) -> String {
    args.iter()
        .map(|a| shell_quote_arg(a))
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

    /// The defect this function was carrying: it quoted on a DENYLIST of seven
    /// characters, so every other shell metacharacter reached the live shell
    /// `CommandEngine::start_command` writes into. Each of these is a second
    /// command, a redirect, a glob or a word break that the caller did not ask
    /// for.
    #[test]
    fn a_metacharacter_argument_is_one_word_not_a_second_command() {
        for hostile in [
            "a;id",     // command separator
            "a|id",     // pipeline
            "a&id",     // background + separator
            "a>out",    // redirect
            "a<in",     // redirect
            "$(id)",    // command substitution (denylist caught `$`, but only that)
            "a(b)",     // subshell
            "a*",       // glob
            "a?",       // glob
            "[ab]",     // glob
            "{a,b}",    // brace expansion
            "~root",    // tilde expansion
            "#comment", // the rest of the line disappears
            "a\nid",    // a newline IS a command separator
            "a\tb",     // field splitting
            "a b",      // the one the denylist did catch
        ] {
            let quoted = shell_quote_arg(hostile);
            assert_eq!(
                sh_word_split(&quoted),
                vec![hostile.to_string()],
                "`{hostile:?}` quoted as `{quoted}` did not come back as one literal word"
            );
        }
    }

    /// The empty string is the case a denylist structurally cannot catch: it
    /// contains none of the listed characters, so it was emitted as nothing and
    /// the argument vanished from the line.
    #[test]
    fn an_empty_argument_survives_instead_of_vanishing() {
        assert_eq!(shell_quote_arg(""), "''");
        assert_eq!(
            shell_escape_args(&["a".to_string(), String::new(), "b".to_string()]),
            "a '' b"
        );
        assert_eq!(
            sh_word_split(&shell_escape_args(&[
                "a".to_string(),
                String::new(),
                "b".to_string()
            ])),
            vec!["a".to_string(), String::new(), "b".to_string()],
        );
    }

    /// Ordinary arguments must stay readable — quoting everything would be safe
    /// and useless as a display format (issue #135 renders argv with this).
    #[test]
    fn ordinary_arguments_are_left_bare() {
        for plain in [
            "ls",
            "-la",
            "--color=always",
            "/usr/bin/env",
            "a.rs",
            "1,2",
            "host:port",
            "user@host",
            "50%",
            "a+b",
            "snake_case",
        ] {
            assert_eq!(
                shell_quote_arg(plain),
                plain,
                "{plain} should not be quoted"
            );
        }
    }

    /// Whatever the argument is, a shell re-reading the quoted form must hand
    /// back that exact string as one word. Round-tripped through a real
    /// `/bin/sh`, not through this crate's idea of one.
    #[test]
    fn every_shape_round_trips_through_a_real_shell() {
        for arg in [
            "it's",
            "it's a 'quoted' thing",
            "back\\slash",
            "double\"quote",
            "back`tick`",
            "bang!",
            "new\nline",
            "trailing ",
            " leading",
            "",
            "café",            // non-ASCII: quoted, and must survive it
            "変数",            // ditto, multi-byte
            "a'\\''b",         // already looks like an escape
            "'",               // a lone quote
            "''",              // and a pair
            &"x".repeat(4096), // long
        ] {
            let quoted = shell_quote_arg(arg);
            assert_eq!(
                sh_word_split(&quoted),
                vec![arg.to_string()],
                "`{arg:?}` quoted as `{quoted}` did not round-trip"
            );
        }
    }

    /// Ask the shell itself to split the line, so the assertion is about the
    /// shell's rules rather than a second implementation of them. Words come
    /// back NUL-separated because every other separator is a legal argument.
    fn word_split_with(shell: &str, line: &str) -> Vec<String> {
        let script = format!("for a in {line}; do printf '%s\\0' \"$a\"; done");
        let out = std::process::Command::new(shell)
            .arg("-c")
            .arg(&script)
            .output()
            .unwrap_or_else(|e| panic!("run {shell}: {e}"));
        assert!(
            out.status.success(),
            "{shell} rejected `{script}`:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.stderr.is_empty(),
            "{shell} wrote to stderr for `{script}`: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let mut words: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .split('\0')
            .map(String::from)
            .collect();
        // Trailing empty element after the final NUL, not a word.
        words.pop();
        words
    }

    fn sh_word_split(line: &str) -> Vec<String> {
        word_split_with("/bin/sh", line)
    }

    /// `/bin/sh` is one shell. A pane runs whatever `$SHELL` is, so the rule
    /// has to hold in the shells people actually set — and it is zsh that
    /// disagrees with the other two.
    #[test]
    fn the_quoting_holds_in_every_installed_shell() {
        let cases = [
            "=ls",         // zsh EQUALS: rewritten to /usr/bin/ls
            "=nosuchprog", // zsh EQUALS: FATAL, aborts the line
            "a=b",         // an interior `=` must stay unquoted and literal
            "--flag=value",
            "a;id",
            "{a,b}", // bash/zsh brace expansion — one word, not two
            "a b",
            "",
            "~root",
            "x=~",       // bash expands a tilde after an unquoted `=`
            "PATH=/x:~", // ...and after a `:`
            "!!",        // history expansion
            "a\nb",
            "café",
        ];
        let mut ran = 0;
        for shell in ["/bin/sh", "/usr/bin/bash", "/usr/bin/zsh"] {
            if !std::path::Path::new(shell).exists() {
                eprintln!("SKIP: {shell} is not installed");
                continue;
            }
            ran += 1;
            for arg in cases {
                let quoted = shell_quote_arg(arg);
                assert_eq!(
                    word_split_with(shell, &quoted),
                    vec![arg.to_string()],
                    "{shell}: `{arg:?}` quoted as `{quoted}` did not come back as itself"
                );
            }
        }
        assert!(ran > 0, "no shell to test against");
    }

    /// `start_command` skips the quoting entirely when there are no arguments,
    /// so the no-argument path needs its own pin.
    #[test]
    fn a_command_with_no_arguments_is_unchanged() {
        let mut engine = CommandEngine::new();
        let (_, line) =
            engine.start_command(Uuid::new_v4(), "echo hi", &[], Duration::from_secs(5), None);
        assert!(
            line.starts_with("echo hi; __shux_ec=$?;"),
            "the command string is a shell line and must be passed through: {line:?}"
        );
    }

    /// The line `start_command` actually injects. The argument is hostile; the
    /// shell must run `printf` once with it, and must NOT run `id`.
    #[test]
    fn the_injected_command_line_cannot_be_broken_out_of() {
        let mut engine = CommandEngine::new();
        // No space anywhere in the argument: the pre-fix denylist saw nothing
        // to quote and `id` became a command of its own.
        let (_, pty_line) = engine.start_command(
            Uuid::new_v4(),
            "echo",
            &["a;id".to_string()],
            Duration::from_secs(5),
            None,
        );
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(&pty_line)
            .output()
            .expect("run /bin/sh");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.starts_with("a;id\n"),
            "the argument was not delivered whole; got {stdout:?} from line {pty_line:?}"
        );
        assert!(
            !stdout.contains("uid="),
            "`id` ran — the argument escaped its word: {stdout:?}"
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
