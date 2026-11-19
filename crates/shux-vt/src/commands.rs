//! OSC 133 command boundaries: when a command typed into the shell started
//! (`133;C`) and how it ended (`133;D;<exit>`).
//!
//! Each start and end takes a stamp from the same process-global counter as
//! program status, so `pane.status`'s `as_of` is a floor for both, and a
//! terminal replaced on respawn never reuses one. A command is matched by its
//! start stamp: one already running when the floor was read is never taken
//! for the next one, whenever it ends.
//!
//! Tracked whether or not OSC 7501 program status is on, and left alone by
//! RIS: `reset` run as a command emits RIS between its own C and D.
//!
//! Each command also keeps the raw bytes the pane wrote while it ran: what
//! followed its C, up to the sequence that ended it.

use std::collections::VecDeque;

use crate::program_status::next_generation;

/// Ended commands kept for [`CommandBoundaries::first_after`]; a floor older
/// than the oldest one reads as [`CommandLookup::Overrun`].
pub const KEEP_ENDED: usize = 32;
/// Output bytes kept per command; past it the rest is dropped and reported.
pub const OUTPUT_LIMIT: usize = 1 << 20;
/// Output bytes kept across ended commands; the oldest go first past it.
pub const OUTPUT_BUDGET: usize = 2 << 20;

/// How a command's end was learned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndedBy {
    /// Its own `133;D`.
    End,
    /// A prompt (`133;A`) with no `D` before it, as after `exec bash`.
    Prompt,
    /// Another `133;C` with no `D` before it.
    NextCommand,
}

impl EndedBy {
    pub fn as_str(self) -> &'static str {
        match self {
            EndedBy::End => "end",
            EndedBy::Prompt => "prompt",
            EndedBy::NextCommand => "next_command",
        }
    }
}

/// One command that started and ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandRecord {
    pub started: u64,
    pub ended: u64,
    /// `None` when the end carried no exit code, or was inferred.
    pub exit_code: Option<i32>,
    pub ended_by: EndedBy,
}

/// The earliest ended command that started after a floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandLookup {
    Found(CommandRecord),
    /// None yet.
    Pending,
    /// The commands after the floor include one no longer kept.
    Overrun,
}

/// Where the shell stands, by the last boundary it reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShellState {
    /// No boundary yet: the shell may not report them at all.
    #[default]
    Unknown,
    /// A prompt (`A`) and no command since: the shell is reading a line.
    AtPrompt,
    /// A command started (`C`) and has not ended.
    Running,
    /// A command ended (`D`) and the next prompt has not been drawn yet.
    Between,
}

impl ShellState {
    pub fn as_str(self) -> &'static str {
        match self {
            ShellState::Unknown => "unknown",
            ShellState::AtPrompt => "at_prompt",
            ShellState::Running => "running",
            ShellState::Between => "between",
        }
    }
}

/// What a command wrote, raw, as the pane received it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandOutput<'a> {
    pub bytes: &'a [u8],
    /// Output past [`OUTPUT_LIMIT`] was dropped.
    pub truncated: bool,
}

/// A line submitted to the shell that has not started a command yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Line {
    #[default]
    None,
    /// Submitted at the prompt: the shell is reading it.
    AtPrompt,
    /// Submitted before the prompt was drawn: the shell reads it there.
    BeforePrompt,
}

#[derive(Debug, Default)]
struct Captured {
    bytes: Vec<u8>,
    truncated: bool,
}

impl Captured {
    fn view(&self) -> CommandOutput<'_> {
        CommandOutput {
            bytes: &self.bytes,
            truncated: self.truncated,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct CommandBoundaries {
    /// The start stamp of the command running now.
    open: Option<u64>,
    open_output: Captured,
    /// Bytes the open command has seen, kept or not.
    open_wire: usize,
    ended: VecDeque<CommandRecord>,
    /// Ended commands' output by start stamp, oldest first.
    outputs: VecDeque<(u64, Captured)>,
    kept_bytes: usize,
    /// The start stamp of the newest record dropped from `ended`.
    dropped_through: u64,
    state: ShellState,
    /// A line typed into the shell and not yet started.
    line: Line,
    /// Prompts (`A`) seen.
    prompts: u64,
    /// Boundaries that changed the state; moves on every C, D and A that counts.
    marks: u64,
}

impl CommandBoundaries {
    /// `seq_len` on each mark: the wire length of the sequence itself, which
    /// a command it ends did not write.
    pub(crate) fn on_start(&mut self, seq_len: usize) {
        self.close(None, EndedBy::NextCommand, seq_len);
        self.open = Some(next_generation());
        self.state = ShellState::Running;
        self.line = Line::None;
        self.marks += 1;
    }

    /// A `D` with no command running (Enter at an empty prompt) is ignored.
    pub(crate) fn on_end(&mut self, exit_code: Option<i32>, seq_len: usize) {
        if self.open.is_some() {
            self.close(exit_code, EndedBy::End, seq_len);
            self.state = ShellState::Between;
            self.marks += 1;
        }
    }

    pub(crate) fn on_prompt(&mut self, seq_len: usize) {
        self.close(None, EndedBy::Prompt, seq_len);
        self.state = ShellState::AtPrompt;
        self.line = match self.line {
            Line::BeforePrompt => Line::AtPrompt,
            _ => Line::None,
        };
        self.prompts += 1;
        self.marks += 1;
    }

    pub(crate) fn state(&self) -> ShellState {
        self.state
    }

    /// A line was submitted (a CR or LF outside a bracketed paste). While a
    /// command runs it is that program's input, so nothing is noted.
    pub(crate) fn on_line_submitted(&mut self) {
        match self.state {
            ShellState::AtPrompt => self.line = Line::AtPrompt,
            ShellState::Between => self.line = Line::BeforePrompt,
            ShellState::Unknown | ShellState::Running => {}
        }
    }

    /// The shell is at its prompt with a submitted line it has not started.
    pub(crate) fn line_pending(&self) -> bool {
        self.line == Line::AtPrompt && self.state == ShellState::AtPrompt
    }

    pub(crate) fn prompts(&self) -> u64 {
        self.prompts
    }

    pub(crate) fn marks(&self) -> u64 {
        self.marks
    }

    /// Bytes the pane wrote, in stream order with the boundaries.
    pub(crate) fn capture(&mut self, bytes: &[u8]) {
        if self.open.is_none() {
            return;
        }
        self.open_wire = self.open_wire.saturating_add(bytes.len());
        let out = &mut self.open_output;
        let room = OUTPUT_LIMIT.saturating_sub(out.bytes.len());
        out.bytes.extend_from_slice(&bytes[..bytes.len().min(room)]);
        out.truncated = self.open_wire > OUTPUT_LIMIT;
    }

    fn close(&mut self, exit_code: Option<i32>, ended_by: EndedBy, seq_len: usize) {
        let Some(started) = self.open.take() else {
            return;
        };
        // The ending sequence arrived as the command's last bytes.
        let payload = std::mem::take(&mut self.open_wire).saturating_sub(seq_len);
        self.open_output.bytes.truncate(payload);
        self.open_output.truncated = payload > OUTPUT_LIMIT;
        self.ended.push_back(CommandRecord {
            started,
            ended: next_generation(),
            exit_code,
            ended_by,
        });
        let output = std::mem::take(&mut self.open_output);
        self.kept_bytes += output.bytes.len();
        self.outputs.push_back((started, output));
        while self.ended.len() > KEEP_ENDED {
            if let Some(old) = self.ended.pop_front() {
                self.dropped_through = old.started;
            }
        }
        while self.kept_bytes > OUTPUT_BUDGET
            || self
                .outputs
                .front()
                .is_some_and(|(s, _)| *s <= self.dropped_through)
        {
            let Some((_, old)) = self.outputs.pop_front() else {
                break;
            };
            self.kept_bytes -= old.bytes.len();
        }
    }

    /// The output of the ended command that started at `started`, while kept.
    pub(crate) fn output(&self, started: u64) -> Option<CommandOutput<'_>> {
        self.outputs
            .iter()
            .find(|(s, _)| *s == started)
            .map(|(_, c)| c.view())
    }

    /// The running command's output so far, if it started above `floor`.
    pub(crate) fn running_output(&self, floor: u64) -> Option<CommandOutput<'_>> {
        self.open
            .filter(|&s| s > floor)
            .map(|_| self.open_output.view())
    }

    /// A prompt or a command start has been seen in this terminal.
    pub(crate) fn seen(&self) -> bool {
        self.state != ShellState::Unknown
    }

    pub(crate) fn running(&self) -> bool {
        self.open.is_some()
    }

    /// The newest end stamp, or 0: it moves exactly when a command ends.
    pub(crate) fn last_end(&self) -> u64 {
        self.ended.back().map_or(0, |c| c.ended)
    }

    pub(crate) fn first_after(&self, floor: u64) -> CommandLookup {
        if floor < self.dropped_through {
            return CommandLookup::Overrun;
        }
        self.ended
            .iter()
            .find(|c| c.started > floor)
            .map_or(CommandLookup::Pending, |c| CommandLookup::Found(*c))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(b: &CommandBoundaries, floor: u64) -> CommandRecord {
        match b.first_after(floor) {
            CommandLookup::Found(c) => c,
            other => panic!("expected a command after {floor}: {other:?}"),
        }
    }

    #[test]
    fn a_command_is_its_start_and_its_end() {
        let mut b = CommandBoundaries::default();
        assert!(!b.seen());
        let floor = crate::program_status::as_of();
        b.on_start(0);
        assert!(b.seen() && b.running());
        assert_eq!(b.first_after(floor), CommandLookup::Pending);
        b.on_end(Some(7), 0);
        let c = found(&b, floor);
        assert_eq!((c.exit_code, c.ended_by), (Some(7), EndedBy::End));
        assert!(c.started > floor && c.ended > c.started);
        assert!(!b.running());
    }

    /// A command running when the floor is read ends above the floor; it is
    /// still not the next command.
    #[test]
    fn a_command_already_running_at_the_floor_is_not_the_next_one() {
        let mut b = CommandBoundaries::default();
        b.on_start(0);
        let floor = crate::program_status::as_of();
        b.on_end(Some(0), 0);
        assert_eq!(b.first_after(floor), CommandLookup::Pending);
        b.on_start(0);
        b.on_end(Some(3), 0);
        assert_eq!(found(&b, floor).exit_code, Some(3));
    }

    /// Two commands back to back: each wait chains from the last one's start.
    #[test]
    fn chained_waits_see_every_command_in_order() {
        let mut b = CommandBoundaries::default();
        let floor = crate::program_status::as_of();
        for code in [1, 2] {
            b.on_start(0);
            b.on_end(Some(code), 0);
        }
        let first = found(&b, floor);
        assert_eq!(first.exit_code, Some(1));
        assert_eq!(found(&b, first.started).exit_code, Some(2));
        assert_eq!(
            b.first_after(found(&b, first.started).started),
            CommandLookup::Pending
        );
    }

    /// `exec bash`: a C, no D, then the new shell's prompt.
    #[test]
    fn a_prompt_or_a_new_start_ends_a_command_that_never_reported() {
        let mut b = CommandBoundaries::default();
        let floor = crate::program_status::as_of();
        b.on_start(0);
        b.on_prompt(0);
        let c = found(&b, floor);
        assert_eq!((c.exit_code, c.ended_by), (None, EndedBy::Prompt));
        b.on_start(0);
        b.on_start(0);
        let c = found(&b, c.started);
        assert_eq!((c.exit_code, c.ended_by), (None, EndedBy::NextCommand));
        b.on_prompt(0);
        b.on_prompt(0);
        assert!(!b.running());
    }

    #[test]
    fn a_floor_older_than_every_kept_command_is_an_overrun() {
        let mut b = CommandBoundaries::default();
        let floor = crate::program_status::as_of();
        for _ in 0..KEEP_ENDED + 1 {
            b.on_start(0);
            b.on_end(Some(0), 0);
        }
        assert_eq!(b.first_after(floor), CommandLookup::Overrun);
        let newest = b.ended.back().unwrap().started;
        assert_eq!(b.first_after(newest), CommandLookup::Pending);
    }
}
