//! Command execution engine: runs a command in a pane's live shell and returns
//! what it wrote and how it exited.
//!
//! The typed line frames the command between two markers the shell prints:
//!
//! ```text
//! command printf 'SHUX_BE''GIN%s\n' <id>; CMD; __shux_ec=$?;
//! command printf 'SHUX_MAR''KER%sEXIT%sSHUX_END\n' <id> "$__shux_ec"; unset __shux_ec
//! ```
//!
//! The marker text never appears contiguously in the typed line, so the
//! terminal's echo of that line cannot match either marker. `stdout` is the
//! pane's output stream between the BEGIN line and the END marker: the
//! command's stdout and stderr as the terminal received them, ANSI-stripped.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Payload bytes kept per command; output past it is dropped and reported.
pub const CAPTURE_LIMIT: usize = 1 << 20;
/// Finished commands whose results stay queryable; older ones are forgotten.
const KEEP_FINISHED: usize = 64;

/// A command tracked by the execution engine.
pub struct TrackedCommand {
    pub id: Uuid,
    pub pane_id: Uuid,
    pub command: String,
    pub marker: String,
    pub started_at: Instant,
    pub timeout: Duration,
    pub state: CommandState,
    /// Channel to notify the caller when the command finishes (sync mode).
    pub completion_tx: Option<tokio::sync::oneshot::Sender<CommandResult>>,
    begun: bool,
    capture: Vec<u8>,
    truncated: bool,
    result: Option<CommandResult>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandState {
    Running,
    Completed,
    TimedOut,
    Cancelled,
    /// The pane's program ended or the pane was torn down first.
    PaneGone,
}

impl CommandState {
    fn as_str(self) -> &'static str {
        match self {
            CommandState::Running => "running",
            CommandState::Completed => "completed",
            CommandState::TimedOut => "timed_out",
            CommandState::Cancelled => "cancelled",
            CommandState::PaneGone => "pane_gone",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResult {
    pub command_id: Uuid,
    pub state: String,
    pub exit_code: Option<i32>,
    pub stdout: String,
    /// Output past [`CAPTURE_LIMIT`] bytes was dropped.
    #[serde(default)]
    pub truncated: bool,
    pub runtime_ms: u64,
}

/// The command execution engine manages running commands in panes.
pub struct CommandEngine {
    commands: HashMap<Uuid, TrackedCommand>,
    /// Each pane's running commands in the order they were sent; output is
    /// framed for the front one only, as the shell runs them in turn.
    queues: HashMap<Uuid, VecDeque<Uuid>>,
    /// Each pane's bytes not yet classified, so a marker split across reads
    /// is seen whole and never leaks into a capture.
    pending: HashMap<Uuid, Vec<u8>>,
    finished: VecDeque<Uuid>,
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
            queues: HashMap::new(),
            pending: HashMap::new(),
            finished: VecDeque::new(),
        }
    }

    /// Start tracking a new command.
    /// Returns the command ID and the full line to send to the PTY.
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

        // `command printf` skips a shell function named printf. `$?` is read
        // straight after the command, before anything else can reset it.
        let pty_command = format!(
            "command printf 'SHUX_BE''GIN%s\\n' {marker}; {full_cmd}; __shux_ec=$?; \
             command printf 'SHUX_MAR''KER%sEXIT%sSHUX_END\\n' {marker} \"$__shux_ec\"; \
             unset __shux_ec\n",
        );

        self.commands.insert(
            command_id,
            TrackedCommand {
                id: command_id,
                pane_id,
                command: full_cmd,
                marker,
                started_at: Instant::now(),
                timeout,
                state: CommandState::Running,
                completion_tx,
                begun: false,
                capture: Vec::new(),
                truncated: false,
                result: None,
            },
        );
        self.queues
            .entry(pane_id)
            .or_default()
            .push_back(command_id);

        (command_id, pty_command)
    }

    /// Feed a pane's PTY output. Returns the commands it completed.
    pub fn process_output(&mut self, pane_id: Uuid, output: &[u8]) -> Vec<CommandResult> {
        let mut completed = Vec::new();
        let Some(queue) = self.queues.get(&pane_id) else {
            return completed;
        };
        if queue.is_empty() {
            return completed;
        }
        let mut buf = self.pending.remove(&pane_id).unwrap_or_default();
        buf.extend_from_slice(output);
        while let Some(&id) = self.queues.get(&pane_id).and_then(|q| q.front()) {
            let Some(cmd) = self.commands.get_mut(&id) else {
                break;
            };
            if !cmd.begun {
                let begin = format!("SHUX_BEGIN{}", cmd.marker).into_bytes();
                match find(&buf, &begin) {
                    Some(at) => {
                        let after = at + begin.len();
                        match buf[after..].iter().position(|&b| b == b'\n') {
                            Some(nl) => {
                                buf.drain(..after + nl + 1);
                                cmd.begun = true;
                            }
                            None => {
                                buf.drain(..at);
                                break;
                            }
                        }
                    }
                    None => {
                        // Nothing before BEGIN is the command's; keep only
                        // what could be the start of a split marker.
                        let keep = buf.len().min(begin.len() - 1);
                        buf.drain(..buf.len() - keep);
                        break;
                    }
                }
                continue;
            }
            let end = format!("SHUX_MARKER{}EXIT", cmd.marker).into_bytes();
            let Some(at) = find(&buf, &end) else {
                let keep = buf.len().min(end.len() - 1);
                let payload: Vec<u8> = buf.drain(..buf.len() - keep).collect();
                cmd.keep(&payload);
                break;
            };
            match end_marker(&buf[at + end.len()..]) {
                EndMarker::Incomplete => {
                    let payload: Vec<u8> = buf.drain(..at).collect();
                    cmd.keep(&payload);
                    break;
                }
                EndMarker::Not => {
                    // Marker-shaped bytes the command wrote itself.
                    let payload: Vec<u8> = buf.drain(..at + end.len()).collect();
                    cmd.keep(&payload);
                }
                EndMarker::Exit(code, len) => {
                    let payload: Vec<u8> = buf.drain(..at).collect();
                    cmd.keep(&payload);
                    buf.drain(..end.len() + len);
                    if buf.first() == Some(&b'\r') {
                        buf.remove(0);
                    }
                    if buf.first() == Some(&b'\n') {
                        buf.remove(0);
                    }
                    if let Some(result) = self.finish(id, CommandState::Completed, Some(code)) {
                        completed.push(result);
                    }
                }
            }
        }
        if self.queues.get(&pane_id).is_some_and(|q| !q.is_empty()) {
            self.pending.insert(pane_id, buf);
        }
        completed
    }

    /// Finish a running command: freeze its result, answer its waiter, and
    /// let the oldest finished results go.
    fn finish(
        &mut self,
        id: Uuid,
        state: CommandState,
        exit_code: Option<i32>,
    ) -> Option<CommandResult> {
        let cmd = self.commands.get_mut(&id)?;
        if cmd.state != CommandState::Running {
            return None;
        }
        cmd.state = state;
        let capture = std::mem::take(&mut cmd.capture);
        let stdout = crate::strip_ansi(&String::from_utf8_lossy(&capture)).replace("\r\n", "\n");
        let result = CommandResult {
            command_id: id,
            state: state.as_str().to_string(),
            exit_code,
            stdout,
            truncated: cmd.truncated,
            runtime_ms: cmd.started_at.elapsed().as_millis() as u64,
        };
        cmd.result = Some(result.clone());
        if let Some(tx) = cmd.completion_tx.take() {
            let _ = tx.send(result.clone());
        }
        let pane = cmd.pane_id;
        if let Some(q) = self.queues.get_mut(&pane) {
            q.retain(|c| *c != id);
            if q.is_empty() {
                self.queues.remove(&pane);
                self.pending.remove(&pane);
            }
        }
        self.finished.push_back(id);
        while self.finished.len() > KEEP_FINISHED {
            if let Some(old) = self.finished.pop_front() {
                self.commands.remove(&old);
            }
        }
        Some(result)
    }

    /// Check for timed-out commands. Returns their results and pane IDs.
    pub fn check_timeouts(&mut self) -> Vec<(Uuid, CommandResult)> {
        let now = Instant::now();
        let ids: Vec<Uuid> = self
            .commands
            .values()
            .filter(|t| {
                t.state == CommandState::Running && now.duration_since(t.started_at) > t.timeout
            })
            .map(|t| t.id)
            .collect();
        ids.into_iter()
            .filter_map(|id| {
                let pane = self.commands.get(&id)?.pane_id;
                Some((pane, self.finish(id, CommandState::TimedOut, None)?))
            })
            .collect()
    }

    /// Cancel a running command. Returns the pane_id if found and cancelled.
    pub fn cancel_command(&mut self, command_id: Uuid) -> Option<Uuid> {
        let pane = self.commands.get(&command_id)?.pane_id;
        self.finish(command_id, CommandState::Cancelled, None)
            .map(|_| pane)
    }

    /// The pane is gone: finish every command still running in it, so no
    /// caller waits out its timeout.
    pub fn forget_pane(&mut self, pane_id: Uuid) {
        let ids: Vec<Uuid> = self
            .queues
            .get(&pane_id)
            .map(|q| q.iter().copied().collect())
            .unwrap_or_default();
        for id in ids {
            self.finish(id, CommandState::PaneGone, None);
        }
        self.pending.remove(&pane_id);
    }

    /// Get the status of a command: its frozen result once it has finished.
    pub fn get_status(&self, command_id: Uuid) -> Option<CommandResult> {
        let tracked = self.commands.get(&command_id)?;
        Some(tracked.result.clone().unwrap_or_else(|| CommandResult {
            command_id,
            state: CommandState::Running.as_str().to_string(),
            exit_code: None,
            stdout: String::new(),
            truncated: false,
            runtime_ms: tracked.started_at.elapsed().as_millis() as u64,
        }))
    }
}

impl TrackedCommand {
    /// Keep payload bytes up to [`CAPTURE_LIMIT`]; note any dropped.
    fn keep(&mut self, payload: &[u8]) {
        let room = CAPTURE_LIMIT.saturating_sub(self.capture.len());
        if payload.len() > room {
            self.truncated = true;
        }
        self.capture
            .extend_from_slice(&payload[..payload.len().min(room)]);
    }
}

enum EndMarker {
    /// More bytes are needed to tell.
    Incomplete,
    /// Not the END marker after all.
    Not,
    /// The exit code, and how many bytes after the prefix the marker used.
    Exit(i32, usize),
}

/// Classify what follows an END prefix: `<digits>SHUX_END`.
fn end_marker(rest: &[u8]) -> EndMarker {
    const SUFFIX: &[u8] = b"SHUX_END";
    let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits > 3 {
        return EndMarker::Not;
    }
    let tail = &rest[digits..];
    if tail.len() < SUFFIX.len() {
        // Still arriving: digits so far, then a prefix of the suffix.
        return if tail.is_empty() || (digits > 0 && SUFFIX.starts_with(tail)) {
            EndMarker::Incomplete
        } else {
            EndMarker::Not
        };
    }
    if digits == 0 || !tail.starts_with(SUFFIX) {
        return EndMarker::Not;
    }
    match std::str::from_utf8(&rest[..digits])
        .ok()
        .and_then(|d| d.parse().ok())
    {
        Some(code) => EndMarker::Exit(code, digits + SUFFIX.len()),
        None => EndMarker::Not,
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
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

    /// What a shell sends back for one framed command: the echo of the
    /// typed line, the BEGIN line, the payload, the END marker, a prompt.
    fn framed(engine: &CommandEngine, id: Uuid, line: &str, payload: &[u8], code: i32) -> Vec<u8> {
        let marker = &engine.commands[&id].marker;
        let mut out = line.replace('\n', "\r\n").into_bytes();
        out.extend_from_slice(format!("SHUX_BEGIN{marker}\r\n").as_bytes());
        out.extend_from_slice(payload);
        out.extend_from_slice(format!("SHUX_MARKER{marker}EXIT{code}SHUX_END\r\n").as_bytes());
        out.extend_from_slice(b"\x1b[32muser@host\x1b[0m $ ");
        out
    }

    fn run_one(payload: &[u8], code: i32) -> CommandResult {
        let mut engine = CommandEngine::new();
        let pane = Uuid::new_v4();
        let (id, line) = engine.start_command(pane, "cmd", &[], Duration::from_secs(10), None);
        let bytes = framed(&engine, id, &line, payload, code);
        let done = engine.process_output(pane, &bytes);
        assert_eq!(done.len(), 1, "the command did not complete");
        done.into_iter().next().unwrap()
    }

    #[test]
    fn test_marker_detection_exit_0() {
        let r = run_one(b"hello\r\n", 0);
        assert_eq!((r.state.as_str(), r.exit_code), ("completed", Some(0)));
    }

    #[test]
    fn test_marker_detection_nonzero_exit() {
        assert_eq!(run_one(b"", 1).exit_code, Some(1));
    }

    #[test]
    fn test_marker_split_across_chunks() {
        let mut engine = CommandEngine::new();
        let pane = Uuid::new_v4();
        let (id, line) = engine.start_command(pane, "echo", &[], Duration::from_secs(10), None);
        let bytes = framed(&engine, id, &line, b"hello\r\n", 0);
        let end = bytes.windows(8).position(|w| w == b"SHUX_END").unwrap();
        let (a, b) = bytes.split_at(end + 3);
        assert!(engine.process_output(pane, a).is_empty());
        let done = engine.process_output(pane, b);
        assert_eq!(done[0].exit_code, Some(0));
    }

    #[test]
    fn stdout_is_exactly_what_the_command_wrote() {
        assert_eq!(run_one(b"", 0).stdout, "");
        assert_eq!(run_one(b"foo", 0).stdout, "foo");
        assert_eq!(run_one(b"foo\r\n", 0).stdout, "foo\n");
        let r = run_one(b"row 1\r\nrow 2\r\n", 3);
        assert_eq!(
            (r.stdout.as_str(), r.exit_code),
            ("row 1\nrow 2\n", Some(3))
        );
    }

    #[test]
    fn the_typed_line_and_the_prompt_never_reach_stdout() {
        let r = run_one(b"\x1b[31mred\x1b[0m\r\n", 0);
        assert_eq!(r.stdout, "red\n");
    }

    #[test]
    fn markers_split_at_any_byte_still_frame_the_output() {
        let mut probe = CommandEngine::new();
        let pane = Uuid::new_v4();
        let (id, line) = probe.start_command(pane, "cmd", &[], Duration::from_secs(10), None);
        let bytes = framed(&probe, id, &line, b"hello\r\n", 0);
        for cut in 1..bytes.len() {
            let mut engine = CommandEngine::new();
            let (id2, _) = engine.start_command(pane, "cmd", &[], Duration::from_secs(10), None);
            let marker = engine.commands[&id2].marker.clone();
            let bytes = String::from_utf8(bytes.clone())
                .unwrap()
                .replace(&probe.commands[&id].marker, &marker);
            let (a, b) = bytes.as_bytes().split_at(cut);
            let mut done = engine.process_output(pane, a);
            done.extend(engine.process_output(pane, b));
            assert_eq!(done.len(), 1, "cut at {cut}");
            assert_eq!(done[0].stdout, "hello\n", "cut at {cut}");
        }
    }

    #[test]
    fn a_character_split_across_reads_survives() {
        let mut engine = CommandEngine::new();
        let pane = Uuid::new_v4();
        let (id, line) = engine.start_command(pane, "cmd", &[], Duration::from_secs(10), None);
        let bytes = framed(&engine, id, &line, "héllo ✓\r\n".as_bytes(), 0);
        let cut = bytes.windows(3).position(|w| w == "✓".as_bytes()).unwrap() + 1;
        let mut done = engine.process_output(pane, &bytes[..cut]);
        done.extend(engine.process_output(pane, &bytes[cut..]));
        assert_eq!(done[0].stdout, "héllo ✓\n");
    }

    #[test]
    fn marker_shaped_output_is_kept_as_output() {
        let mut engine = CommandEngine::new();
        let pane = Uuid::new_v4();
        let (id, line) = engine.start_command(pane, "cmd", &[], Duration::from_secs(10), None);
        let marker = engine.commands[&id].marker.clone();
        let fake = format!("SHUX_MARKER{marker}EXITnotacode\r\n");
        let done = engine.process_output(pane, &framed(&engine, id, &line, fake.as_bytes(), 0));
        assert_eq!(done[0].stdout, format!("SHUX_MARKER{marker}EXITnotacode\n"));
        assert_eq!(done[0].exit_code, Some(0));
    }

    #[test]
    fn output_past_the_cap_is_dropped_and_said_so() {
        let exact = run_one(&vec![b'x'; CAPTURE_LIMIT], 0);
        assert_eq!(
            (exact.stdout.len(), exact.truncated),
            (CAPTURE_LIMIT, false)
        );
        let over = run_one(&vec![b'x'; CAPTURE_LIMIT + 1], 7);
        assert_eq!((over.stdout.len(), over.truncated), (CAPTURE_LIMIT, true));
        assert_eq!(
            over.exit_code,
            Some(7),
            "the END marker is still found past the cap"
        );
    }

    #[test]
    fn a_second_command_gets_only_its_own_output() {
        let mut engine = CommandEngine::new();
        let pane = Uuid::new_v4();
        let (a, la) = engine.start_command(pane, "a", &[], Duration::from_secs(10), None);
        let first = framed(&engine, a, &la, b"first\r\n", 0);
        engine.process_output(pane, &first);
        let (b, lb) = engine.start_command(pane, "b", &[], Duration::from_secs(10), None);
        let done = engine.process_output(pane, &framed(&engine, b, &lb, b"second\r\n", 0));
        assert_eq!(done[0].stdout, "second\n");
        assert_eq!(engine.get_status(a).unwrap().stdout, "first\n");
    }

    #[test]
    fn status_returns_the_finished_result_unchanged() {
        let mut engine = CommandEngine::new();
        let pane = Uuid::new_v4();
        let (id, line) = engine.start_command(pane, "cmd", &[], Duration::from_secs(10), None);
        assert_eq!(engine.get_status(id).unwrap().state, "running");
        engine.process_output(pane, &framed(&engine, id, &line, b"out\r\n", 2));
        let once = engine.get_status(id).unwrap();
        assert_eq!(
            (once.state.as_str(), once.exit_code, once.stdout.as_str()),
            ("completed", Some(2), "out\n")
        );
        let (other, lo) = engine.start_command(pane, "x", &[], Duration::from_secs(10), None);
        engine.process_output(pane, &framed(&engine, other, &lo, b"noise\r\n", 0));
        let twice = engine.get_status(id).unwrap();
        assert_eq!(
            (twice.stdout, twice.runtime_ms),
            (once.stdout, once.runtime_ms)
        );
    }

    #[test]
    fn finished_results_are_kept_only_up_to_the_bound() {
        let mut engine = CommandEngine::new();
        let pane = Uuid::new_v4();
        let (first, l) = engine.start_command(pane, "c", &[], Duration::from_secs(10), None);
        engine.process_output(pane, &framed(&engine, first, &l, b"", 0));
        for _ in 0..KEEP_FINISHED {
            let (id, l) = engine.start_command(pane, "c", &[], Duration::from_secs(10), None);
            engine.process_output(pane, &framed(&engine, id, &l, b"", 0));
        }
        assert!(
            engine.get_status(first).is_none(),
            "the oldest result was kept past the bound"
        );
        assert_eq!(engine.commands.len(), KEEP_FINISHED);
    }

    #[test]
    fn a_pane_that_goes_away_answers_its_waiters() {
        let mut engine = CommandEngine::new();
        let pane = Uuid::new_v4();
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        let (id, _) =
            engine.start_command(pane, "sleep 60", &[], Duration::from_secs(300), Some(tx));
        engine.forget_pane(pane);
        assert_eq!(
            rx.try_recv().expect("the waiter was answered").state,
            "pane_gone"
        );
        assert_eq!(engine.get_status(id).unwrap().state, "pane_gone");
        assert!(engine.pending.is_empty() && engine.queues.is_empty());
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

        let line = String::new();
        let bytes = framed(&engine, cmd_id, &line, b"hi\r\n", 0);
        engine.process_output(pane_id, &bytes);

        // Cancel should fail (already completed)
        assert!(engine.cancel_command(cmd_id).is_none());
    }

    #[test]
    fn test_wrong_pane_ignores_marker() {
        let mut engine = CommandEngine::new();
        let pane_a = Uuid::new_v4();
        let pane_b = Uuid::new_v4();

        let (cmd_id, _) = engine.start_command(pane_a, "echo", &[], Duration::from_secs(10), None);

        let output = framed(&engine, cmd_id, "", b"", 0);

        // Feed the markers to the wrong pane — should not complete
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
            line.contains("; echo hi; __shux_ec=$?;"),
            "the command string is a shell line and must be passed through: {line:?}"
        );
    }

    /// The line `start_command` actually injects. The argument is hostile; the
    /// shell must run `printf` once with it, and must NOT run `id`.
    #[test]
    fn the_injected_command_line_cannot_be_broken_out_of() {
        let mut engine = CommandEngine::new();
        let pane = Uuid::new_v4();
        // No space anywhere in the argument: the pre-fix denylist saw nothing
        // to quote and `id` became a command of its own.
        let (_, pty_line) = engine.start_command(
            pane,
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
        // A real shell's framing, through the engine: exactly the argument.
        let done = engine.process_output(pane, &out.stdout);
        assert_eq!(
            done.first().map(|r| r.stdout.as_str()),
            Some("a;id\n"),
            "the argument was not delivered whole from line {pty_line:?}"
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
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
