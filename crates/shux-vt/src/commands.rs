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

/// The line editor drawing a shell's prompt, as the integration names it
/// on the prompt mark (`133;A;shux-editor=zsh`): what decides which
/// end-of-line mark, if any, it draws after a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEditor {
    /// zsh's ZLE: PROMPT_SP.
    Zsh,
    /// ble.sh: `prompt_eol_mark`.
    Ble,
    /// bash's readline: no mark.
    Readline,
    /// No line editor (`bash --noediting`): the prompt is read in the
    /// terminal's line mode.
    None,
}

impl LineEditor {
    /// The editor a mark's parameters (`;key=value...`) name, if any.
    pub(crate) fn named(params: &[u8]) -> Option<Self> {
        params.split(|&b| b == b';').find_map(|p| match p {
            b"shux-editor=zsh" => Some(LineEditor::Zsh),
            b"shux-editor=ble" => Some(LineEditor::Ble),
            b"shux-editor=readline" => Some(LineEditor::Readline),
            b"shux-editor=none" => Some(LineEditor::None),
            _ => None,
        })
    }
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

/// The `n`th command to start above a floor, counting from 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NthCommand {
    Ended(CommandRecord),
    /// It has started and not ended.
    Running,
    /// Not started yet.
    Pending,
    /// The commands after the floor include one no longer kept.
    Overrun,
}

/// A line the shell read at its prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Submitted<'a> {
    /// It started the `n`th command (from 0) above the floor; `line` is what
    /// the screen showed from the prompt to the start.
    Command { n: usize, line: &'a [String] },
    /// The shell drew its next prompt without starting a command: `rows` is
    /// what the screen showed from the prompt to the next one, the shell's
    /// reply included.
    Rejected { rows: &'a [String] },
}

/// Wire bytes kept from the end of the open command's output, for finding
/// the line editor's end-of-line mark however much output was dropped.
const TAIL_KEEP: usize = 16 << 10;

#[derive(Debug, Default)]
pub(crate) struct CommandBoundaries {
    /// The start stamp of the command running now.
    open: Option<u64>,
    open_output: Captured,
    /// Bytes the open command has seen, kept or not.
    open_wire: usize,
    /// The last of them, up to [`TAIL_KEEP`] (and up to twice that between
    /// trims).
    open_tail: Vec<u8>,
    ended: VecDeque<CommandRecord>,
    /// Ended commands' output by start stamp, oldest first.
    outputs: VecDeque<(u64, Captured)>,
    kept_bytes: usize,
    /// The start stamp of the newest record dropped from `ended`.
    dropped_through: u64,
    /// The stamp of the newest input dropped from `rejected`.
    rejected_through: u64,
    state: ShellState,
    /// A line typed into the shell and not yet started.
    line: Line,
    /// Keys sent to the prompt since it was drawn that submitted nothing:
    /// the line editor may hold text, or be in another mode.
    edited: bool,
    /// Such keys sent before the prompt was drawn.
    edited_before_prompt: bool,
    /// What the screen showed for each command's line, by start stamp,
    /// kept as long as its record.
    lines: VecDeque<(u64, Vec<String>)>,
    /// Inputs the shell drew a new prompt after without starting a command,
    /// by the stamp of that prompt.
    rejected: VecDeque<(u64, Vec<String>)>,
    /// The editor the last prompt named.
    editor: Option<LineEditor>,
    /// Prompts (`A`) seen.
    prompts: u64,
    /// Boundaries that changed the state; moves on every C, D and A that counts.
    marks: u64,
}

impl CommandBoundaries {
    /// `seq_len`: the wire length of the mark itself, which a command it ends
    /// did not write. `cols`: the terminal's width, which pins the line
    /// editor's end-of-line mark.
    /// `line`: what the screen showed from the prompt to this start.
    pub(crate) fn on_start(&mut self, seq_len: usize, cols: usize, line: Vec<String>) {
        self.close(None, EndedBy::NextCommand, seq_len, cols);
        let started = next_generation();
        self.open = Some(started);
        self.lines.push_back((started, line));
        self.state = ShellState::Running;
        self.line = Line::None;
        self.edited = false;
        self.edited_before_prompt = false;
        self.marks += 1;
    }

    /// A `D` with no command running (Enter at an empty prompt) is ignored.
    pub(crate) fn on_end(&mut self, exit_code: Option<i32>, seq_len: usize, cols: usize) {
        if self.open.is_some() {
            self.close(exit_code, EndedBy::End, seq_len, cols);
            self.state = ShellState::Between;
            self.marks += 1;
        }
    }

    pub(crate) fn on_prompt(&mut self, seq_len: usize, cols: usize, editor: Option<LineEditor>) {
        self.close(None, EndedBy::Prompt, seq_len, cols);
        self.editor = editor;
        self.state = ShellState::AtPrompt;
        self.line = match self.line {
            Line::BeforePrompt => Line::AtPrompt,
            _ => Line::None,
        };
        self.edited = std::mem::take(&mut self.edited_before_prompt);
        self.prompts += 1;
        self.marks += 1;
    }

    pub(crate) fn state(&self) -> ShellState {
        self.state
    }

    pub(crate) fn editor(&self) -> Option<LineEditor> {
        self.editor
    }

    /// The prompt is being redrawn with no command started since the last
    /// one: what the screen showed in between was rejected or abandoned.
    pub(crate) fn on_input_rejected(&mut self, rows: Vec<String>) {
        if rows.is_empty() {
            return;
        }
        self.rejected.push_back((next_generation(), rows));
        while self.rejected.len() > KEEP_ENDED {
            if let Some((stamp, _)) = self.rejected.pop_front() {
                self.rejected_through = stamp;
            }
        }
    }

    pub(crate) fn submitted_after(&self, floor: u64) -> Option<Vec<Submitted<'_>>> {
        if floor < self.dropped_through || floor < self.rejected_through {
            return None;
        }
        let commands = self
            .lines
            .iter()
            .filter(|(started, _)| *started > floor)
            .enumerate()
            .map(|(n, (started, line))| (*started, Submitted::Command { n, line }));
        let rejected = self
            .rejected
            .iter()
            .filter(|(stamp, _)| *stamp > floor)
            .map(|(stamp, rows)| (*stamp, Submitted::Rejected { rows }));
        let mut all: Vec<_> = commands.chain(rejected).collect();
        all.sort_by_key(|(stamp, _)| *stamp);
        Some(all.into_iter().map(|(_, s)| s).collect())
    }

    /// Input sent to the pane: whether it submitted a line (a CR or LF
    /// outside a bracketed paste), and whether keys that submitted nothing
    /// came after the last submission. While a command runs, input is that
    /// program's, so nothing is noted.
    pub(crate) fn on_input(&mut self, submits: bool, edits: bool) {
        match self.state {
            ShellState::AtPrompt => {
                if submits {
                    self.line = Line::AtPrompt;
                }
                self.edited = edits || (self.edited && !submits);
            }
            ShellState::Between => {
                if submits {
                    self.line = Line::BeforePrompt;
                }
                self.edited_before_prompt = edits || (self.edited_before_prompt && !submits);
            }
            ShellState::Unknown | ShellState::Running => {}
        }
    }

    /// The shell is at its prompt with a submitted line it has not started.
    pub(crate) fn line_pending(&self) -> bool {
        self.line == Line::AtPrompt && self.state == ShellState::AtPrompt
    }

    /// Keys were sent to the prompt since it was drawn and submitted nothing.
    pub(crate) fn input_present(&self) -> bool {
        self.edited && self.state == ShellState::AtPrompt
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
        self.open_tail.extend_from_slice(bytes);
        if self.open_tail.len() > 2 * TAIL_KEEP {
            self.open_tail.drain(..self.open_tail.len() - TAIL_KEEP);
        }
    }

    fn close(&mut self, exit_code: Option<i32>, ended_by: EndedBy, seq_len: usize, cols: usize) {
        let Some(started) = self.open.take() else {
            return;
        };
        // The ending sequence arrived as the command's last bytes, and the
        // line editor's end-of-line mark just before it.
        let tail = std::mem::take(&mut self.open_tail);
        let tail = &tail[..tail.len().saturating_sub(seq_len)];
        let payload = std::mem::take(&mut self.open_wire)
            .saturating_sub(seq_len)
            .saturating_sub(eol_mark_len(tail, cols, self.editor));
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
        while self
            .lines
            .front()
            .is_some_and(|(s, _)| *s <= self.dropped_through)
        {
            self.lines.pop_front();
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

    pub(crate) fn first_after(&self, floor: u64) -> CommandLookup {
        match self.nth_after(floor, 0) {
            NthCommand::Ended(c) => CommandLookup::Found(c),
            NthCommand::Overrun => CommandLookup::Overrun,
            NthCommand::Running | NthCommand::Pending => CommandLookup::Pending,
        }
    }

    pub(crate) fn nth_after(&self, floor: u64, n: usize) -> NthCommand {
        if floor < self.dropped_through {
            return NthCommand::Overrun;
        }
        let after = self.ended.iter().filter(|c| c.started > floor);
        if let Some(c) = after.clone().nth(n) {
            return NthCommand::Ended(*c);
        }
        let open_is_nth = self.open.is_some_and(|s| s > floor) && after.count() == n;
        if open_is_nth {
            NthCommand::Running
        } else {
            NthCommand::Pending
        }
    }
}

/// How many bytes at the end of `out` are the end-of-line mark `editor`
/// draws after a command and before its shell reports the end: shell
/// output, not the command's. 0 when there is none, or no editor was named.
/// Both shapes are pinned to the terminal's width, `cols`.
///
/// - zsh (PROMPT_SP, `preprompt()`): the mark (`PROMPT_EOL_MARK`, `%` by
///   default), `cols - w - !xn` spaces, CR, `w` spaces, CR, where `w` is the
///   mark's width.
/// - ble.sh (`prompt_eol_mark`, `.adjust-eol`): an optional `SGR0 [SI] DECSC
///   mark SGR0 [SI] DECRC`, then `CUF (cols - 2 | cols - 3)`, two spaces, CR,
///   and EL.
pub fn eol_mark_len(out: &[u8], cols: usize, editor: Option<LineEditor>) -> usize {
    match editor {
        Some(LineEditor::Zsh) => zsh_prompt_sp(out, cols),
        Some(LineEditor::Ble) => ble_eol_mark(out, cols),
        Some(LineEditor::Readline | LineEditor::None) | None => None,
    }
    .unwrap_or(0)
}

fn trailing(buf: &[u8], byte: u8) -> usize {
    buf.iter().rev().take_while(|&&b| b == byte).count()
}

fn zsh_prompt_sp(out: &[u8], cols: usize) -> Option<usize> {
    let body = out.strip_suffix(b"\r")?;
    let w = trailing(body, b' ');
    let body = body[..body.len() - w].strip_suffix(b"\r")?;
    let n = trailing(body, b' ');
    if n + w != cols && n + w + 1 != cols {
        return None;
    }
    let body = &body[..body.len() - n];
    Some(out.len() - mark_start(body, w)?)
}

/// Where a mark `width` columns wide, with escape sequences around it,
/// starts at the end of `body`.
fn mark_start(body: &[u8], width: usize) -> Option<usize> {
    let mut end = body.len();
    let mut seen = 0;
    loop {
        if let Some(csi) = trailing_csi(&body[..end]) {
            end -= csi;
            continue;
        }
        if seen == width {
            return Some(end);
        }
        let start = (0..end).rev().find(|&i| body[i] & 0xc0 != 0x80)?;
        let ch = std::str::from_utf8(&body[start..end])
            .ok()?
            .chars()
            .next()?;
        if ch.is_control() {
            return None;
        }
        seen += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if seen > width {
            return None;
        }
        end = start;
    }
}

/// The length of a CSI sequence (`ESC [ params final`) ending `buf`.
fn trailing_csi(buf: &[u8]) -> Option<usize> {
    let (&last, rest) = buf.split_last()?;
    if !(0x40..=0x7e).contains(&last) {
        return None;
    }
    let esc = rest.iter().rposition(|&b| b == 0x1b)?;
    let params = &rest[esc + 1..];
    let (&open, params) = params.split_first()?;
    (open == b'[' && params.iter().all(|b| (0x20..=0x3f).contains(b))).then(|| buf.len() - esc)
}

fn ble_eol_mark(out: &[u8], cols: usize) -> Option<usize> {
    let body = out.strip_suffix(b"\x1b[K").unwrap_or(out);
    let body = body.strip_suffix(b"  \r")?;
    let csi = trailing_csi(body)?;
    let cuf = &body[body.len() - csi..];
    let n: usize = std::str::from_utf8(cuf.get(2..cuf.len() - 1)?)
        .ok()?
        .parse()
        .ok()?;
    if !cuf.ends_with(b"C") || (n + 2 != cols && n + 3 != cols) {
        return None;
    }
    let mut body = &body[..body.len() - csi];
    if let Some(mark) = body.strip_suffix(b"\x1b8") {
        let saved = mark.windows(2).rposition(|w| w == b"\x1b7")?;
        body = &mark[..saved];
        body = body.strip_suffix(b"\x0f").unwrap_or(body);
        body = body.strip_suffix(b"\x1b[m").unwrap_or(body);
    }
    Some(out.len() - body.len())
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
        b.on_start(0, 80, Vec::new());
        assert!(b.seen() && b.running());
        assert_eq!(b.first_after(floor), CommandLookup::Pending);
        b.on_end(Some(7), 0, 80);
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
        b.on_start(0, 80, Vec::new());
        let floor = crate::program_status::as_of();
        b.on_end(Some(0), 0, 80);
        assert_eq!(b.first_after(floor), CommandLookup::Pending);
        b.on_start(0, 80, Vec::new());
        b.on_end(Some(3), 0, 80);
        assert_eq!(found(&b, floor).exit_code, Some(3));
    }

    /// Two commands back to back: each wait chains from the last one's start.
    #[test]
    fn chained_waits_see_every_command_in_order() {
        let mut b = CommandBoundaries::default();
        let floor = crate::program_status::as_of();
        for code in [1, 2] {
            b.on_start(0, 80, Vec::new());
            b.on_end(Some(code), 0, 80);
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
        b.on_start(0, 80, Vec::new());
        b.on_prompt(0, 80, None);
        let c = found(&b, floor);
        assert_eq!((c.exit_code, c.ended_by), (None, EndedBy::Prompt));
        b.on_start(0, 80, Vec::new());
        b.on_start(0, 80, Vec::new());
        let c = found(&b, c.started);
        assert_eq!((c.exit_code, c.ended_by), (None, EndedBy::NextCommand));
        b.on_prompt(0, 80, None);
        b.on_prompt(0, 80, None);
        assert!(!b.running());
    }

    #[test]
    fn a_floor_older_than_every_kept_command_is_an_overrun() {
        let mut b = CommandBoundaries::default();
        let floor = crate::program_status::as_of();
        for _ in 0..KEEP_ENDED + 1 {
            b.on_start(0, 80, Vec::new());
            b.on_end(Some(0), 0, 80);
        }
        assert_eq!(b.first_after(floor), CommandLookup::Overrun);
        let newest = b.ended.back().unwrap().started;
        assert_eq!(b.first_after(newest), CommandLookup::Pending);
    }
}
