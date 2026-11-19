//! Command execution engine: runs a command in a pane's live shell and returns
//! what it wrote and how it exited.
//!
//! The typed line frames the command between two markers the shell prints:
//!
//! ```text
//! __shux_ec=$?; command -p printf 'SHUX_BE''GIN%s\n' <id>; (exit "$__shux_ec") && :; CMD; __shux_ec=$?;
//! command -p printf 'SHUX_MAR''KER%sEXIT%sSHUX_END\n' <id> "$__shux_ec";
//! eval "unset __shux_ec; (exit $__shux_ec)" && :
//! ```
//!
//! The marker text never appears contiguously in the typed line, so the
//! terminal's echo of that line cannot match either marker. `stdout` is the
//! pane's output stream between the BEGIN line and the END marker: the
//! command's stdout and stderr as the terminal received them, ANSI-stripped.
//!
//! A shell that reports its own boundaries (OSC 133) needs no markers: the
//! line typed is the command itself ([`Boundaries::Osc133`]). The engine then
//! learns nothing from the bytes; the daemon reads the pane's terminal and
//! reports each step through the `osc133_*` methods.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Payload bytes kept per command; output past it is dropped and reported.
pub const CAPTURE_LIMIT: usize = 1 << 20;
/// Finished commands whose results stay queryable; older ones are forgotten.
const KEEP_FINISHED: usize = 64;

/// How a command's start and end are found in the pane's output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Boundaries {
    /// The shell's own OSC 133 marks; the line typed is the command.
    Osc133,
    /// Markers the typed line prints around the command.
    Markers,
}

/// The pane's state when an OSC 133 command's line was queued: its command is
/// the first to start above `floor`, and a prompt past `prompts` with no
/// start means the shell did not run the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Osc133Anchor {
    pub floor: u64,
    pub prompts: u64,
}

/// How an OSC 133 command's line ended, as the pane's terminal reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Osc133End {
    /// Its command ended. `output` is raw, as the pane wrote it.
    Ended {
        exit_code: Option<i32>,
        ended_by: &'static str,
        output: Vec<u8>,
        truncated: bool,
    },
    /// The shell drew its next prompt without starting a command: a syntax
    /// error, a comment, an incomplete line abandoned.
    NotRun,
    /// More commands ended after the line than the terminal keeps.
    Overrun,
}

/// A command framed for a pane but not yet tracked.
pub struct PreparedCommand {
    id: Uuid,
    command: String,
    marker: String,
    line: String,
    boundaries: Boundaries,
    anchor: Option<Osc133Anchor>,
}

impl PreparedCommand {
    /// The line to type into the pane.
    pub fn line(&self) -> &str {
        &self.line
    }

    pub fn boundaries(&self) -> Boundaries {
        self.boundaries
    }

    /// Where an [`Boundaries::Osc133`] command's answer is looked for; set
    /// under the same lock that queues its line.
    pub fn anchor(&mut self, anchor: Osc133Anchor) {
        self.anchor = Some(anchor);
    }
}

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
    boundaries: Boundaries,
    anchor: Option<Osc133Anchor>,
    ended_by: Option<&'static str>,
    /// Its command has started: BEGIN printed, or the shell's C seen.
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
    /// The shell drew its next prompt without starting the line.
    NotRun,
    /// More commands ended after the line than the terminal keeps.
    Overrun,
    /// Its command started and is reading hidden input (a password prompt).
    AwaitingInput,
}

impl CommandState {
    fn as_str(self) -> &'static str {
        match self {
            CommandState::Running => "running",
            CommandState::Completed => "completed",
            CommandState::TimedOut => "timed_out",
            CommandState::Cancelled => "cancelled",
            CommandState::PaneGone => "pane_gone",
            CommandState::NotRun => "not_run",
            CommandState::Overrun => "overrun",
            CommandState::AwaitingInput => "awaiting_input",
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
    pub truncated: bool,
    pub runtime_ms: u64,
    pub boundaries: Boundaries,
    /// How an [`Boundaries::Osc133`] command's end was learned: `end` (its
    /// own `D`, with the exit code), or `prompt` / `next_command`, inferred,
    /// with none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_by: Option<String>,
    /// While running: `awaiting_start` until its command starts, then
    /// `executing`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// For [`Boundaries::Osc133`]: the floor its line was typed above. Pass it
    /// to `pane.wait_command` as `after` to wait on for the command's own end
    /// after this result froze (`timed_out`, `awaiting_input`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of: Option<u64>,
}

/// The command execution engine manages running commands in panes.
pub struct CommandEngine {
    commands: HashMap<Uuid, TrackedCommand>,
    /// Each pane's one running command. A shell runs one line at a time, and a
    /// line typed while another runs is echoed into its output, so a pane
    /// takes a second command only after the first has finished.
    active: HashMap<Uuid, Uuid>,
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
            active: HashMap::new(),
            pending: HashMap::new(),
            finished: VecDeque::new(),
        }
    }

    /// Frame `command` for the pane: its ID and the line to type.
    pub fn prepare(command: &str, args: &[String]) -> PreparedCommand {
        let id = Uuid::new_v4();
        let marker = format!("__SHUX_CMD_{}__", id.as_simple());
        let command = if args.is_empty() {
            command.to_string()
        } else {
            format!("{} {}", command, shell_escape_args(args))
        };
        // `command -p` skips shell functions and searches the default PATH, not
        // the pane's: zsh's `command` never runs the builtin. The pane's `$?`
        // is restored before the command, and the command's own is left as
        // `$?` after the line: `eval` expands it before the unset runs. Each
        // `(exit N)` heads an `&&` list, so errexit, which spares all but a
        // list's last command, never ends the shell on it.
        let line = format!(
            "__shux_ec=$?; command -p printf 'SHUX_BE''GIN%s\\n' {marker}; \
             (exit \"$__shux_ec\") && :; {command}; __shux_ec=$?; \
             command -p printf 'SHUX_MAR''KER%sEXIT%sSHUX_END\\n' {marker} \"$__shux_ec\"; \
             eval \"unset __shux_ec; (exit $__shux_ec)\" && :\n",
        );
        PreparedCommand {
            id,
            command,
            marker,
            line,
            boundaries: Boundaries::Markers,
            anchor: None,
        }
    }

    /// Frame `command` for a shell that reports OSC 133: the line is the
    /// command itself, after C-u, which clears anything typed at the prompt.
    pub fn prepare_osc133(command: &str, args: &[String]) -> PreparedCommand {
        let command = if args.is_empty() {
            command.to_string()
        } else {
            format!("{} {}", command, shell_escape_args(args))
        };
        PreparedCommand {
            id: Uuid::new_v4(),
            line: format!("\x15{command}\n"),
            command,
            marker: String::new(),
            boundaries: Boundaries::Osc133,
            anchor: None,
        }
    }

    /// Track a prepared command as the one running in `pane_id`: its ID and
    /// the line to send, or `Err` with the ID of the command already running.
    pub fn register(
        &mut self,
        pane_id: Uuid,
        prepared: PreparedCommand,
        timeout: Duration,
        completion_tx: Option<tokio::sync::oneshot::Sender<CommandResult>>,
    ) -> Result<(Uuid, String), Uuid> {
        if let Some(&running) = self.active.get(&pane_id) {
            return Err(running);
        }
        let PreparedCommand {
            id,
            command,
            marker,
            line,
            boundaries,
            anchor,
        } = prepared;
        self.commands.insert(
            id,
            TrackedCommand {
                id,
                pane_id,
                command,
                marker,
                started_at: Instant::now(),
                timeout,
                state: CommandState::Running,
                completion_tx,
                boundaries,
                anchor,
                ended_by: None,
                begun: false,
                capture: Vec::new(),
                truncated: false,
                result: None,
            },
        );
        self.active.insert(pane_id, id);
        Ok((id, line))
    }

    /// [`Self::prepare`] then [`Self::register`].
    pub fn start_command(
        &mut self,
        pane_id: Uuid,
        command: &str,
        args: &[String],
        timeout: Duration,
        completion_tx: Option<tokio::sync::oneshot::Sender<CommandResult>>,
    ) -> Result<(Uuid, String), Uuid> {
        self.register(
            pane_id,
            Self::prepare(command, args),
            timeout,
            completion_tx,
        )
    }

    /// Feed a pane's PTY output. Returns the command it completed, if any.
    pub fn process_output(&mut self, pane_id: Uuid, output: &[u8]) -> Vec<CommandResult> {
        let mut completed = Vec::new();
        let Some(&id) = self.active.get(&pane_id) else {
            return completed;
        };
        let mut buf = self.pending.remove(&pane_id).unwrap_or_default();
        buf.extend_from_slice(output);
        let Some(cmd) = self.commands.get_mut(&id) else {
            return completed;
        };
        if cmd.boundaries == Boundaries::Osc133 {
            // Before its start: what the pane shows for the line, kept in case
            // the shell never runs it. After: the terminal keeps the output.
            if !cmd.begun {
                cmd.keep(&buf);
            }
            return completed;
        }
        if !cmd.begun {
            let begin = format!("SHUX_BEGIN{}", cmd.marker).into_bytes();
            let Some(at) = find(&buf, &begin) else {
                // Nothing before BEGIN is the command's; keep only what could
                // be the start of a split marker.
                let keep = buf.len().min(begin.len() - 1);
                buf.drain(..buf.len() - keep);
                self.pending.insert(pane_id, buf);
                return completed;
            };
            let Some(nl) = buf[at + begin.len()..].iter().position(|&b| b == b'\n') else {
                buf.drain(..at);
                self.pending.insert(pane_id, buf);
                return completed;
            };
            buf.drain(..at + begin.len() + nl + 1);
            cmd.begun = true;
        }
        let end = format!("SHUX_MARKER{}EXIT", cmd.marker).into_bytes();
        loop {
            let Some(at) = find(&buf, &end) else {
                let take = buf.len() - buf.len().min(end.len() - 1);
                cmd.keep(&buf[..take]);
                buf.drain(..take);
                break;
            };
            match end_marker(&buf[at + end.len()..]) {
                EndMarker::Incomplete => {
                    cmd.keep(&buf[..at]);
                    buf.drain(..at);
                    break;
                }
                EndMarker::Not => {
                    // Marker-shaped bytes the command wrote itself.
                    cmd.keep(&buf[..at + end.len()]);
                    buf.drain(..at + end.len());
                }
                EndMarker::Exit(code) => {
                    cmd.keep(&buf[..at]);
                    completed.extend(self.finish(id, CommandState::Completed, Some(code)));
                    self.release(id);
                    return completed;
                }
            }
        }
        self.pending.insert(pane_id, buf);
        completed
    }

    /// Finish a running command: freeze its result and answer its waiter. It
    /// keeps its pane until [`Self::release`]: a timed-out command's program
    /// may still be reading the pane, so only its END marker, a cancel, or the
    /// pane going releases it.
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
        let pane = cmd.pane_id;
        let held = self.pending.get(&pane).cloned().unwrap_or_default();
        // Ended some other way than its END marker: the bytes held back in
        // case they began a marker were the command's own output, up to any
        // END marker already begun.
        if state != CommandState::Completed && cmd.begun && cmd.boundaries == Boundaries::Markers {
            let end = format!("SHUX_MARKER{}EXIT", cmd.marker).into_bytes();
            cmd.keep(&held[..find(&held, &end).unwrap_or(held.len())]);
        }
        cmd.state = state;
        let capture = std::mem::take(&mut cmd.capture);
        let stdout = if cmd.boundaries == Boundaries::Osc133 && cmd.begun {
            boundary_output_text(&capture)
        } else {
            output_text(&capture)
        };
        let result = CommandResult {
            command_id: id,
            state: state.as_str().to_string(),
            exit_code,
            stdout,
            truncated: cmd.truncated,
            runtime_ms: cmd.started_at.elapsed().as_millis() as u64,
            boundaries: cmd.boundaries,
            ended_by: cmd.ended_by.map(str::to_string),
            phase: None,
            as_of: cmd.anchor.map(|a| a.floor),
        };
        cmd.result = Some(result.clone());
        if let Some(tx) = cmd.completion_tx.take() {
            let _ = tx.send(result.clone());
        }
        Some(result)
    }

    /// Free the pane `id` holds, and let the oldest finished results go.
    fn release(&mut self, id: Uuid) {
        let Some(pane) = self.commands.get(&id).map(|c| c.pane_id) else {
            return;
        };
        if self.active.get(&pane) != Some(&id) {
            return;
        }
        self.active.remove(&pane);
        self.pending.remove(&pane);
        self.finished.push_back(id);
        while self.finished.len() > KEEP_FINISHED {
            if let Some(old) = self.finished.pop_front() {
                self.commands.remove(&old);
            }
        }
    }

    /// Check for timed-out commands. Returns their results and pane IDs.
    pub fn check_timeouts(&mut self) -> Vec<(Uuid, CommandResult)> {
        let now = Instant::now();
        let ids: Vec<Uuid> = self
            .commands
            .values()
            .filter(|t| {
                t.state == CommandState::Running && now.duration_since(t.started_at) >= t.timeout
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

    /// Cancel the command holding its pane, timed out or not. Returns the
    /// pane_id if it held one.
    pub fn cancel_command(&mut self, command_id: Uuid) -> Option<Uuid> {
        let pane = self.commands.get(&command_id)?.pane_id;
        if self.active.get(&pane) != Some(&command_id) {
            return None;
        }
        self.finish(command_id, CommandState::Cancelled, None);
        self.release(command_id);
        Some(pane)
    }

    /// The pane is gone: finish its running command, so no caller waits out
    /// its timeout.
    pub fn forget_pane(&mut self, pane_id: Uuid) {
        if let Some(&id) = self.active.get(&pane_id) {
            self.finish(id, CommandState::PaneGone, None);
            self.release(id);
        }
        self.pending.remove(&pane_id);
    }

    /// Get the status of a command: its frozen result once it has finished.
    pub fn get_status(&self, command_id: Uuid) -> Option<CommandResult> {
        let tracked = self.commands.get(&command_id)?;
        Some(tracked.result.clone().unwrap_or_else(|| {
            CommandResult {
                command_id,
                state: CommandState::Running.as_str().to_string(),
                exit_code: None,
                stdout: String::new(),
                truncated: false,
                runtime_ms: tracked.started_at.elapsed().as_millis() as u64,
                boundaries: tracked.boundaries,
                ended_by: None,
                phase: Some(
                    if tracked.begun {
                        "executing"
                    } else {
                        "awaiting_start"
                    }
                    .to_string(),
                ),
                as_of: tracked.anchor.map(|a| a.floor),
            }
        }))
    }

    /// Running commands whose command has started, with their panes.
    pub fn started(&self) -> Vec<(Uuid, Uuid)> {
        self.commands
            .values()
            .filter(|t| t.state == CommandState::Running && t.begun)
            .map(|t| (t.id, t.pane_id))
            .collect()
    }

    /// Its command is reading hidden input: answer its waiter now. Like a
    /// timeout, it keeps the pane until it ends or is cancelled.
    pub fn awaiting_input(&mut self, id: Uuid) -> Option<CommandResult> {
        self.finish(id, CommandState::AwaitingInput, None)
    }

    /// The pane `command_id` was typed into.
    pub fn get_pane(&self, command_id: Uuid) -> Option<Uuid> {
        self.commands.get(&command_id).map(|c| c.pane_id)
    }

    /// The [`Boundaries::Osc133`] command holding `pane_id`, its anchor, and
    /// whether its command has started.
    pub fn osc133_holder(&self, pane_id: Uuid) -> Option<(Uuid, Osc133Anchor, bool)> {
        let id = *self.active.get(&pane_id)?;
        let cmd = self.commands.get(&id)?;
        Some((id, cmd.anchor?, cmd.begun))
    }

    /// Its command started: what the pane showed before it was the line's.
    pub fn osc133_started(&mut self, id: Uuid) {
        if let Some(cmd) = self.commands.get_mut(&id) {
            cmd.begun = true;
            cmd.capture.clear();
            cmd.truncated = false;
        }
    }

    /// Replace a started command's output with what it has written so far,
    /// read from the terminal before a timeout, cancel or teardown freezes it.
    pub fn osc133_output_so_far(&mut self, id: Uuid, raw: &[u8], truncated: bool) {
        if let Some(cmd) = self.commands.get_mut(&id)
            && cmd.begun
            && cmd.result.is_none()
        {
            cmd.capture.clear();
            cmd.capture
                .extend_from_slice(&raw[..raw.len().min(CAPTURE_LIMIT)]);
            cmd.truncated = truncated || raw.len() > CAPTURE_LIMIT;
        }
    }

    /// Running [`Boundaries::Osc133`] commands whose timeout is due.
    pub fn osc133_due(&self) -> Vec<Uuid> {
        let now = Instant::now();
        self.commands
            .values()
            .filter(|t| {
                t.boundaries == Boundaries::Osc133
                    && t.state == CommandState::Running
                    && now.duration_since(t.started_at) >= t.timeout
            })
            .map(|t| t.id)
            .collect()
    }

    /// The line ended: answer its waiter, unless a timeout already did, and
    /// free its pane either way.
    pub fn osc133_end(&mut self, id: Uuid, end: Osc133End) -> Option<CommandResult> {
        let cmd = self.commands.get_mut(&id)?;
        let running = cmd.state == CommandState::Running;
        let result = match end {
            Osc133End::Ended {
                exit_code,
                ended_by,
                output,
                truncated,
            } => {
                if running {
                    cmd.begun = true;
                    cmd.capture = output;
                    cmd.capture.truncate(CAPTURE_LIMIT);
                    cmd.truncated = truncated;
                    cmd.ended_by = Some(ended_by);
                }
                self.finish(id, CommandState::Completed, exit_code)
            }
            Osc133End::NotRun => {
                if running {
                    // Up to the prompt that ended it, not the prompt itself.
                    let at = find(&cmd.capture, b"\x1b]133;A").unwrap_or(cmd.capture.len());
                    cmd.capture.truncate(at);
                    cmd.begun = true;
                }
                self.finish(id, CommandState::NotRun, None)
            }
            Osc133End::Overrun => self.finish(id, CommandState::Overrun, None),
        };
        self.release(id);
        result
    }
}

impl TrackedCommand {
    /// Keep payload bytes up to [`CAPTURE_LIMIT`]; note any dropped.
    fn keep(&mut self, payload: &[u8]) {
        if self.result.is_some() {
            return;
        }
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
    /// The command's exit code.
    Exit(i32),
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
    // At most three ASCII digits: no overflow, no parse failure.
    let code = rest[..digits]
        .iter()
        .fold(0, |n, b| n * 10 + i32::from(b - b'0'));
    EndMarker::Exit(code)
}

/// A command's raw output as text: ANSI-stripped, `\r\n` as `\n`.
pub fn output_text(raw: &[u8]) -> String {
    crate::strip_ansi(&String::from_utf8_lossy(raw)).replace("\r\n", "\n")
}

/// [`output_text`] for output cut at the shell's own boundaries, without the
/// end-of-line mark the line editor draws after a command and before it
/// reports the end: shell output, not the command's.
///
/// - ble.sh (`prompt_eol_mark`): `SGR0 [SI] DECSC <mark> SGR0 [SI] DECRC
///   [CUF n] "  " CR [EL]`.
/// - zsh (PROMPT_SP): `<mark>`, spaces to the edge, `CR " " CR`. The default
///   mark is `%`, or `#` for root.
pub fn boundary_output_text(raw: &[u8]) -> String {
    let mut text = output_text(trim_ble_eol_mark(raw));
    if let Some(body) = text.strip_suffix("\r \r") {
        let body = body.trim_end_matches(' ');
        let body = body.strip_suffix(['%', '#']).unwrap_or(body);
        text.truncate(body.len());
    }
    text
}

fn trim_ble_eol_mark(raw: &[u8]) -> &[u8] {
    let body = raw.strip_suffix(b"\x1b[K").unwrap_or(raw);
    let Some(body) = body.strip_suffix(b"  \r") else {
        return raw;
    };
    let body = match body.iter().rposition(|&b| b == 0x1b) {
        // CUF n: ESC [ digits C
        Some(at)
            if body[at..].starts_with(b"\x1b[")
                && body.ends_with(b"C")
                && body[at + 2..body.len() - 1].iter().all(u8::is_ascii_digit) =>
        {
            &body[..at]
        }
        _ => body,
    };
    let Some(body) = body.strip_suffix(b"\x1b8") else {
        return raw;
    };
    let Some(save) = body.windows(2).rposition(|w| w == b"\x1b7") else {
        return raw;
    };
    let before = &body[..save];
    let before = before.strip_suffix(b"\x0f").unwrap_or(before);
    before.strip_suffix(b"\x1b[m").unwrap_or(before)
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
        let (id, line) = engine
            .start_command(pane, "cmd", &[], Duration::from_secs(10), None)
            .unwrap();
        let bytes = framed(&engine, id, &line, payload, code);
        let done = engine.process_output(pane, &bytes);
        assert_eq!(done.len(), 1, "the command did not complete");
        done.into_iter().next().unwrap()
    }

    /// An OSC 133 command anchored and tracked in `pane`.
    fn osc133(
        engine: &mut CommandEngine,
        pane: Uuid,
        cmd: &str,
        timeout: Duration,
    ) -> (Uuid, String) {
        let mut prepared = CommandEngine::prepare_osc133(cmd, &[]);
        prepared.anchor(Osc133Anchor {
            floor: 7,
            prompts: 1,
        });
        engine.register(pane, prepared, timeout, None).unwrap()
    }

    fn ended(output: &[u8], exit: Option<i32>) -> Osc133End {
        Osc133End::Ended {
            exit_code: exit,
            ended_by: if exit.is_some() { "end" } else { "prompt" },
            output: output.to_vec(),
            truncated: false,
        }
    }

    /// zsh writes its partial-line mark (PROMPT_SP) after a command and
    /// before the `precmd` that reports its end: shell output, not the
    /// command's, whether or not the command ended its last line.
    #[test]
    fn zshs_partial_line_mark_is_not_command_output() {
        let mark = |cols: usize| {
            format!(
                "\x1b[1m\x1b[7m%\x1b[27m\x1b[1m\x1b[0m{}\r \r",
                " ".repeat(cols - 1)
            )
        };
        assert_eq!(
            boundary_output_text(format!("a\r\n{}", mark(80)).as_bytes()),
            "a\n"
        );
        assert_eq!(
            boundary_output_text(format!("foo{}", mark(40)).as_bytes()),
            "foo"
        );
        assert_eq!(boundary_output_text(mark(80).as_bytes()), "");
        // Root's mark is `#`; spaces the command itself wrote stay.
        assert_eq!(
            boundary_output_text(format!("x  \r\n#{}\r \r", " ".repeat(79)).as_bytes()),
            "x  \n"
        );
        assert_eq!(boundary_output_text(b"keep  \r\n"), "keep  \n");
    }

    /// ble.sh draws its `[ble: EOF]` mark under save/restore and erases it,
    /// after the command and before POSTEXEC: recorded from ble.sh 0.4.
    #[test]
    fn ble_shs_end_of_line_mark_is_not_command_output() {
        let mark =
            b"\x1b[m\x0f\x1b7\x1b[0;94m[ble: EOF]\x1b[m\x0f\x1b[m\x0f\x1b8\x1b[78C  \r\x1b[K";
        let with = |out: &[u8]| [out, mark].concat();
        assert_eq!(boundary_output_text(&with(b"a\r\nb\r\n")), "a\nb\n");
        assert_eq!(boundary_output_text(mark), "");
        assert_eq!(
            boundary_output_text(&with(
                b"bash: syntax error near unexpected token `newline'\r\n"
            )),
            "bash: syntax error near unexpected token `newline'\n"
        );
        // Only that shape, at the very end, is trimmed.
        assert_eq!(boundary_output_text(b"x  \r"), "x  \r");
        assert!(boundary_output_text(&[mark as &[u8], b"after\r\n"].concat()).ends_with("after\n"));
    }

    /// The line is the command itself, cleared of anything typed before it.
    #[test]
    fn an_osc133_line_is_the_command_and_nothing_else() {
        let p = CommandEngine::prepare_osc133("ls", &["a b".to_string()]);
        assert_eq!(p.line(), "\x15ls 'a b'\n");
        assert_eq!(p.boundaries(), Boundaries::Osc133);
        assert!(!p.line().contains("SHUX"));
    }

    /// Its result comes from the terminal's record, not from the bytes the
    /// engine saw: the echo before the start is never output.
    #[test]
    fn an_osc133_command_answers_with_the_terminals_record() {
        let mut engine = CommandEngine::new();
        let pane = Uuid::new_v4();
        let (id, _) = osc133(&mut engine, pane, "echo hi", Duration::from_secs(10));
        assert_eq!(
            engine.get_status(id).unwrap().phase.as_deref(),
            Some("awaiting_start")
        );
        assert!(engine.process_output(pane, b"echo hi\r\n").is_empty());
        engine.osc133_started(id);
        assert_eq!(
            engine.get_status(id).unwrap().phase.as_deref(),
            Some("executing")
        );
        let r = engine
            .osc133_end(id, ended(b"hi\r\n\x1b[1m!\x1b[0m\r\n", Some(0)))
            .unwrap();
        assert_eq!(
            (r.state.as_str(), r.exit_code, r.stdout.as_str()),
            ("completed", Some(0), "hi\n!\n")
        );
        assert_eq!(
            (r.boundaries, r.ended_by.as_deref(), r.as_of),
            (Boundaries::Osc133, Some("end"), Some(7))
        );
        assert_eq!(
            engine.get_status(id).unwrap().ended_by.as_deref(),
            Some("end"),
            "status lost ended_by"
        );
        assert!(
            osc133(&mut engine, pane, "true", Duration::from_secs(1)).0 != id,
            "the pane was not freed"
        );
    }

    /// A line the shell never ran answers with what the shell said, up to
    /// the prompt that ended it.
    #[test]
    fn a_line_the_shell_never_ran_is_not_run() {
        let mut engine = CommandEngine::new();
        let pane = Uuid::new_v4();
        let (id, _) = osc133(&mut engine, pane, "echo (", Duration::from_secs(10));
        engine.process_output(
            pane,
            b"echo (\r\nbash: syntax error near `newline'\r\n\x1b]133;A\x07$ ",
        );
        let r = engine.osc133_end(id, Osc133End::NotRun).unwrap();
        assert_eq!(r.state, "not_run");
        assert_eq!(r.stdout, "echo (\nbash: syntax error near `newline'\n");
        assert_eq!(r.exit_code, None);
    }

    /// A timeout freezes the result with the output so far; the command's own
    /// end later frees the pane and leaves that result as it was.
    #[test]
    fn a_timed_out_osc133_command_keeps_its_result_and_is_freed_by_its_end() {
        let mut engine = CommandEngine::new();
        let pane = Uuid::new_v4();
        let (id, _) = osc133(&mut engine, pane, "sleep 9", Duration::ZERO);
        engine.osc133_started(id);
        assert_eq!(engine.osc133_due(), vec![id]);
        engine.osc133_output_so_far(id, b"early\r\n", false);
        let frozen = engine.check_timeouts();
        assert_eq!(frozen[0].1.stdout, "early\n");
        let second = CommandEngine::prepare_osc133("x", &[]);
        assert_eq!(
            engine
                .register(pane, second, Duration::ZERO, None)
                .map(|r| r.0),
            Err(id),
            "a line was typed into a held pane"
        );
        assert!(
            engine
                .osc133_end(id, ended(b"early\r\nlate\r\n", Some(0)))
                .is_none()
        );
        let status = engine.get_status(id).unwrap();
        assert_eq!(
            (status.state.as_str(), status.stdout.as_str()),
            ("timed_out", "early\n")
        );
        assert!(
            engine.osc133_holder(pane).is_none(),
            "its end did not free the pane"
        );
    }

    /// Hidden input answers at once, and holds the pane like a timeout.
    #[test]
    fn a_command_on_hidden_input_answers_and_holds_its_pane() {
        let mut engine = CommandEngine::new();
        let pane = Uuid::new_v4();
        let (id, _) = osc133(&mut engine, pane, "sudo true", Duration::from_secs(60));
        assert!(
            engine.started().is_empty(),
            "a command not yet started was offered as started"
        );
        engine.osc133_started(id);
        assert_eq!(engine.started(), vec![(id, pane)]);
        assert_eq!(engine.awaiting_input(id).unwrap().state, "awaiting_input");
        assert!(engine.osc133_holder(pane).is_some());
        assert!(engine.started().is_empty());
        engine.osc133_end(id, ended(b"", Some(0)));
        assert!(engine.osc133_holder(pane).is_none());
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
        let (id, line) = engine
            .start_command(pane, "echo", &[], Duration::from_secs(10), None)
            .unwrap();
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
        let (id, line) = probe
            .start_command(pane, "cmd", &[], Duration::from_secs(10), None)
            .unwrap();
        let bytes = framed(&probe, id, &line, b"hello\r\n", 0);
        for cut in 1..bytes.len() {
            let mut engine = CommandEngine::new();
            let (id2, _) = engine
                .start_command(pane, "cmd", &[], Duration::from_secs(10), None)
                .unwrap();
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
        let (id, line) = engine
            .start_command(pane, "cmd", &[], Duration::from_secs(10), None)
            .unwrap();
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
        let (id, line) = engine
            .start_command(pane, "cmd", &[], Duration::from_secs(10), None)
            .unwrap();
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
        let (a, la) = engine
            .start_command(pane, "a", &[], Duration::from_secs(10), None)
            .unwrap();
        let first = framed(&engine, a, &la, b"first\r\n", 0);
        engine.process_output(pane, &first);
        let (b, lb) = engine
            .start_command(pane, "b", &[], Duration::from_secs(10), None)
            .unwrap();
        let done = engine.process_output(pane, &framed(&engine, b, &lb, b"second\r\n", 0));
        assert_eq!(done[0].stdout, "second\n");
        assert_eq!(engine.get_status(a).unwrap().stdout, "first\n");
    }

    #[test]
    fn a_printf_function_cannot_swallow_the_markers() {
        let mut ran = 0;
        for shell in ["bash", "zsh", "dash", "sh"] {
            let Ok(out) = std::process::Command::new(shell)
                .arg("-c")
                .arg("true")
                .output()
            else {
                continue;
            };
            assert!(out.status.success(), "{shell} -c true failed");
            ran += 1;
            let mut engine = CommandEngine::new();
            let pane = Uuid::new_v4();
            let (_, line) = engine
                .start_command(
                    pane,
                    "PATH=/nowhere; printf() { :; }; echo ok",
                    &[],
                    Duration::from_secs(5),
                    None,
                )
                .unwrap();
            let out = std::process::Command::new(shell)
                .arg("-c")
                .arg(&line)
                .output()
                .expect("shell");
            let done = engine.process_output(pane, &out.stdout);
            assert_eq!(
                done.first().map(|r| r.stdout.as_str()),
                Some("ok\n"),
                "{shell} wrote {:?}, stderr {:?}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        assert!(ran > 0, "no shell found to run the markers in");
    }

    /// Framing does not change what the command sees: `$?` is still the
    /// status of whatever the pane ran last.
    #[test]
    fn the_command_sees_the_shells_previous_status() {
        let mut ran = 0;
        for shell in ["bash", "zsh", "dash", "sh"] {
            if std::process::Command::new(shell)
                .arg("-c")
                .arg("true")
                .output()
                .is_err()
            {
                continue;
            }
            ran += 1;
            let mut engine = CommandEngine::new();
            let pane = Uuid::new_v4();
            let (_, line) = engine
                .start_command(pane, "echo \"was:$?\"", &[], Duration::from_secs(5), None)
                .unwrap();
            let out = std::process::Command::new(shell)
                .arg("-c")
                .arg(format!("false; {line}"))
                .output()
                .expect("shell");
            let done = engine.process_output(pane, &out.stdout);
            assert_eq!(
                done.first().map(|r| r.stdout.as_str()),
                Some("was:1\n"),
                "{shell}: the framing reset $? before the command ran"
            );
        }
        assert!(ran > 0, "no shell found to run the markers in");
    }

    /// A shell running with errexit survives the framing: a non-zero status
    /// carried in or out is restored in an `&&` list, which errexit spares.
    #[test]
    fn the_framing_does_not_end_a_shell_running_with_errexit() {
        let mut ran = 0;
        for shell in ["bash", "zsh", "dash", "sh"] {
            if std::process::Command::new(shell)
                .arg("-c")
                .arg("true")
                .output()
                .is_err()
            {
                continue;
            }
            ran += 1;
            for (before, command) in [
                ("false && :", "echo in"),
                ("true", "[ -f /nonexistent ] && echo never"),
            ] {
                let mut engine = CommandEngine::new();
                let pane = Uuid::new_v4();
                let (_, line) = engine
                    .start_command(pane, command, &[], Duration::from_secs(5), None)
                    .unwrap();
                let out = std::process::Command::new(shell)
                    .arg("-c")
                    .arg(format!("set -eu; {before}; {line}echo AL''IVE"))
                    .output()
                    .expect("shell");
                let stdout = String::from_utf8_lossy(&out.stdout);
                assert!(
                    stdout.contains("ALIVE"),
                    "{shell}: `{before}` then `{command}` ended the shell: {stdout:?}"
                );
            }
        }
        assert!(ran > 0, "no shell found to run the markers in");
    }

    /// A timeout answers the caller but leaves the program running, reading
    /// the pane: a second command typed now would be its input. The pane stays
    /// busy until the program ends (its END marker) or is cancelled.
    #[test]
    fn a_timed_out_command_holds_its_pane_until_it_ends_or_is_cancelled() {
        let mut engine = CommandEngine::new();
        let pane = Uuid::new_v4();
        let timed_out = |engine: &mut CommandEngine| {
            let (id, line) = engine
                .start_command(pane, "read -r x", &[], Duration::ZERO, None)
                .unwrap();
            let mut begun = line.replace('\n', "\r\n").into_bytes();
            begun.extend_from_slice(
                format!("SHUX_BEGIN{}\r\n", engine.commands[&id].marker).as_bytes(),
            );
            engine.process_output(pane, &begun);
            assert_eq!(engine.check_timeouts().len(), 1);
            assert_eq!(engine.get_status(id).unwrap().state, "timed_out");
            assert_eq!(
                engine.start_command(pane, "echo hi", &[], Duration::from_secs(10), None),
                Err(id),
                "a second command was typed into the timed-out one"
            );
            id
        };

        let id = timed_out(&mut engine);
        let marker = engine.commands[&id].marker.clone();
        engine.process_output(
            pane,
            format!("SHUX_MARKER{marker}EXIT0SHUX_END\r\n").as_bytes(),
        );
        assert_eq!(engine.get_status(id).unwrap().state, "timed_out");
        assert!(
            engine
                .start_command(pane, "echo hi", &[], Duration::from_secs(10), None)
                .is_ok()
        );

        let mut engine = CommandEngine::new();
        let id = timed_out(&mut engine);
        assert_eq!(engine.cancel_command(id), Some(pane));
        assert!(
            engine
                .start_command(pane, "echo hi", &[], Duration::from_secs(10), None)
                .is_ok()
        );
    }

    /// A timeout that lands between two reads of the END line: the part of the
    /// marker already read is not the command's output.
    #[test]
    fn a_timeout_mid_marker_keeps_the_marker_out_of_stdout() {
        let mut engine = CommandEngine::new();
        let pane = Uuid::new_v4();
        let (id, _) = engine
            .start_command(pane, "cmd", &[], Duration::ZERO, None)
            .unwrap();
        let marker = engine.commands[&id].marker.clone();
        engine.process_output(
            pane,
            format!("SHUX_BEGIN{marker}\r\nout\r\nSHUX_MARKER{marker}EXIT0").as_bytes(),
        );
        assert_eq!(engine.check_timeouts().len(), 1);
        assert_eq!(engine.get_status(id).unwrap().stdout, "out\n");
    }

    #[test]
    fn a_busy_pane_refuses_a_second_command() {
        let mut engine = CommandEngine::new();
        let pane = Uuid::new_v4();
        let (first, line) = engine
            .start_command(pane, "a", &[], Duration::from_secs(10), None)
            .unwrap();
        assert_eq!(
            engine
                .start_command(pane, "b", &[], Duration::from_secs(10), None)
                .err(),
            Some(first)
        );
        engine.process_output(pane, &framed(&engine, first, &line, b"", 0));
        assert!(
            engine
                .start_command(pane, "b", &[], Duration::from_secs(10), None)
                .is_ok()
        );
    }

    /// A command whose line the shell rejected never prints BEGIN. It holds
    /// the pane until cancelled; the next command then runs cleanly.
    #[test]
    fn a_command_that_never_begins_holds_the_pane_until_cancelled() {
        let mut engine = CommandEngine::new();
        let pane = Uuid::new_v4();
        let (stuck, line) = engine
            .start_command(pane, "if", &[], Duration::from_secs(30), None)
            .unwrap();
        let mut bash = line.replace('\n', "\r\n").into_bytes();
        bash.extend_from_slice(b"bash: syntax error near unexpected token `;'\r\n$ ");
        assert!(engine.process_output(pane, &bash).is_empty());
        assert_eq!(
            engine
                .start_command(pane, "echo hi", &[], Duration::from_secs(30), None)
                .err(),
            Some(stuck)
        );
        engine.cancel_command(stuck);
        let (next, l2) = engine
            .start_command(pane, "echo hi", &[], Duration::from_secs(30), None)
            .unwrap();
        let done = engine.process_output(pane, &framed(&engine, next, &l2, b"hi\r\n", 0));
        assert_eq!(done[0].stdout, "hi\n");
    }

    /// A command that ends some other way than its END marker keeps what it
    /// wrote, including bytes held back in case they began a marker.
    #[test]
    fn output_survives_a_command_that_ends_abnormally() {
        let mut engine = CommandEngine::new();
        let pane = Uuid::new_v4();
        let (id, _) = engine
            .start_command(
                pane,
                "printf hello; exit",
                &[],
                Duration::from_secs(10),
                None,
            )
            .unwrap();
        let marker = engine.commands[&id].marker.clone();
        engine.process_output(pane, format!("SHUX_BEGIN{marker}\r\nhello").as_bytes());
        engine.forget_pane(pane);
        let r = engine.get_status(id).unwrap();
        assert_eq!(
            (r.state.as_str(), r.stdout.as_str()),
            ("pane_gone", "hello")
        );
    }

    #[test]
    fn status_returns_the_finished_result_unchanged() {
        let mut engine = CommandEngine::new();
        let pane = Uuid::new_v4();
        let (id, line) = engine
            .start_command(pane, "cmd", &[], Duration::from_secs(10), None)
            .unwrap();
        assert_eq!(engine.get_status(id).unwrap().state, "running");
        engine.process_output(pane, &framed(&engine, id, &line, b"out\r\n", 2));
        let once = engine.get_status(id).unwrap();
        assert_eq!(
            (once.state.as_str(), once.exit_code, once.stdout.as_str()),
            ("completed", Some(2), "out\n")
        );
        let (other, lo) = engine
            .start_command(pane, "x", &[], Duration::from_secs(10), None)
            .unwrap();
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
        let (first, l) = engine
            .start_command(pane, "c", &[], Duration::from_secs(10), None)
            .unwrap();
        engine.process_output(pane, &framed(&engine, first, &l, b"", 0));
        for _ in 0..KEEP_FINISHED {
            let (id, l) = engine
                .start_command(pane, "c", &[], Duration::from_secs(10), None)
                .unwrap();
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
        let (id, _) = engine
            .start_command(pane, "sleep 60", &[], Duration::from_secs(300), Some(tx))
            .unwrap();
        engine.forget_pane(pane);
        assert_eq!(
            rx.try_recv().expect("the waiter was answered").state,
            "pane_gone"
        );
        assert_eq!(engine.get_status(id).unwrap().state, "pane_gone");
        assert!(engine.pending.is_empty() && engine.active.is_empty());
    }

    #[test]
    fn test_cancel_command() {
        let mut engine = CommandEngine::new();
        let pane_id = Uuid::new_v4();

        let (cmd_id, _) = engine
            .start_command(
                pane_id,
                "sleep",
                &["60".to_string()],
                Duration::from_secs(300),
                None,
            )
            .unwrap();

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

        let (cmd_id, _) = engine
            .start_command(
                pane_id,
                "echo",
                &["hi".to_string()],
                Duration::from_secs(10),
                None,
            )
            .unwrap();

        let bytes = framed(&engine, cmd_id, "", b"hi\r\n", 0);
        engine.process_output(pane_id, &bytes);

        assert!(engine.cancel_command(cmd_id).is_none());
    }

    #[test]
    fn test_wrong_pane_ignores_marker() {
        let mut engine = CommandEngine::new();
        let pane_a = Uuid::new_v4();
        let pane_b = Uuid::new_v4();

        let (cmd_id, _) = engine
            .start_command(pane_a, "echo", &[], Duration::from_secs(10), None)
            .unwrap();

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
        let (_, line) = engine
            .start_command(Uuid::new_v4(), "echo hi", &[], Duration::from_secs(5), None)
            .unwrap();
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
        let (_, pty_line) = engine
            .start_command(
                pane,
                "echo",
                &["a;id".to_string()],
                Duration::from_secs(5),
                None,
            )
            .unwrap();
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

        let (cmd_id, _) = engine
            .start_command(
                pane_id,
                "sleep",
                &["10".to_string()],
                Duration::from_secs(300),
                None,
            )
            .unwrap();

        let status = engine.get_status(cmd_id).unwrap();
        assert_eq!(status.state, "running");
    }

    #[test]
    fn test_get_status_nonexistent() {
        let engine = CommandEngine::new();
        assert!(engine.get_status(Uuid::new_v4()).is_none());
    }
}
