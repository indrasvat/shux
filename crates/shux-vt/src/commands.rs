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

use std::collections::VecDeque;

use crate::program_status::next_generation;

/// Ended commands kept for [`CommandBoundaries::first_after`]; a floor older
/// than the oldest one reads as [`CommandLookup::Overrun`].
pub const KEEP_ENDED: usize = 32;

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

#[derive(Debug, Default)]
pub(crate) struct CommandBoundaries {
    /// The start stamp of the command running now.
    open: Option<u64>,
    ended: VecDeque<CommandRecord>,
    /// The start stamp of the newest record dropped from `ended`.
    dropped_through: u64,
}

impl CommandBoundaries {
    pub(crate) fn on_start(&mut self) {
        self.close(None, EndedBy::NextCommand);
        self.open = Some(next_generation());
    }

    /// A `D` with no command running (Enter at an empty prompt) is ignored.
    pub(crate) fn on_end(&mut self, exit_code: Option<i32>) {
        self.close(exit_code, EndedBy::End);
    }

    pub(crate) fn on_prompt(&mut self) {
        self.close(None, EndedBy::Prompt);
    }

    fn close(&mut self, exit_code: Option<i32>, ended_by: EndedBy) {
        let Some(started) = self.open.take() else {
            return;
        };
        self.ended.push_back(CommandRecord {
            started,
            ended: next_generation(),
            exit_code,
            ended_by,
        });
        while self.ended.len() > KEEP_ENDED {
            if let Some(old) = self.ended.pop_front() {
                self.dropped_through = old.started;
            }
        }
    }

    /// A `133;C` has been seen in this terminal.
    pub(crate) fn seen(&self) -> bool {
        self.open.is_some() || !self.ended.is_empty()
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
        b.on_start();
        assert!(b.seen() && b.running());
        assert_eq!(b.first_after(floor), CommandLookup::Pending);
        b.on_end(Some(7));
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
        b.on_start();
        let floor = crate::program_status::as_of();
        b.on_end(Some(0));
        assert_eq!(b.first_after(floor), CommandLookup::Pending);
        b.on_start();
        b.on_end(Some(3));
        assert_eq!(found(&b, floor).exit_code, Some(3));
    }

    /// Two commands back to back: each wait chains from the last one's start.
    #[test]
    fn chained_waits_see_every_command_in_order() {
        let mut b = CommandBoundaries::default();
        let floor = crate::program_status::as_of();
        for code in [1, 2] {
            b.on_start();
            b.on_end(Some(code));
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
        b.on_start();
        b.on_prompt();
        let c = found(&b, floor);
        assert_eq!((c.exit_code, c.ended_by), (None, EndedBy::Prompt));
        b.on_start();
        b.on_start();
        let c = found(&b, c.started);
        assert_eq!((c.exit_code, c.ended_by), (None, EndedBy::NextCommand));
        b.on_prompt();
        b.on_prompt();
        assert!(!b.running());
    }

    #[test]
    fn a_floor_older_than_every_kept_command_is_an_overrun() {
        let mut b = CommandBoundaries::default();
        let floor = crate::program_status::as_of();
        for _ in 0..KEEP_ENDED + 1 {
            b.on_start();
            b.on_end(Some(0));
        }
        assert_eq!(b.first_after(floor), CommandLookup::Overrun);
        let newest = b.ended.back().unwrap().started;
        assert_eq!(b.first_after(newest), CommandLookup::Pending);
    }
}
