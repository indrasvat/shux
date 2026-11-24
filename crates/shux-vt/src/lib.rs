//! shux-vt -- Virtual terminal grid and VT parser.
//!
//! Provides per-pane terminal emulation: a VecDeque-based grid that tracks
//! cell content, styles, cursor position, and scrollback. Driven by the
//! vte crate parsing raw PTY output bytes.

mod capture;
mod cast;
mod cell;
mod charset;
mod commands;
mod cursor;
mod diff;
mod graphics;
mod grid;
mod parser;
mod program_status;
mod screen;
mod settle;
mod sync;
mod tabstops;

pub use capture::{
    CapColor, CapStyle, CaptureError, CursorRepr, CursorShapeRepr, Defaults, FrameEnvelope,
    MaskRect, MaskSet, RowRepr, Run, RunContent, SCHEMA_VERSION, Size, UnderlineStyleRepr,
};
pub use cast::{CastWriter, cast_complete_prefix};
pub use cell::{
    Cell, CellFlags, CellMeta, CellStyle, Color, ExtendedAttrs, Rgb, TerminalDefaultColors,
    UnderlineStyle,
};
pub use charset::{CharsetSlot, TerminalCharset, TerminalCharsets};
pub use commands::{
    CommandLookup, CommandOutput, CommandRecord, EndedBy, KEEP_ENDED as COMMANDS_KEPT, LineEditor,
    NthCommand, OUTPUT_BUDGET as COMMAND_OUTPUT_BUDGET, OUTPUT_LIMIT as COMMAND_OUTPUT_LIMIT,
    ShellState, Submitted, eol_mark_len,
};
pub use cursor::{Cursor, CursorShape, SavedCursor};
pub use diff::{
    CellGridView, CellRef, CursorState, FrameDiff, FrameView, GridFrame, LensRowSpan, diff_frames,
};
pub use graphics::image::StoredImage;
pub use graphics::kitty::Format as ImageFormat;
use grid::ResizePoint;
pub use grid::{
    CellRect, ComposedPlacement, DirtyRegion, Grid, GridConfig, ImageGeometry, PixelRect,
    Placement, Row, visible_span,
};
pub use parser::{MouseMode, ScrollRegion, TerminalModes, VtHandler};
#[cfg(any(test, fuzzing))]
#[doc(hidden)]
pub use program_status::fuzzing;
pub use program_status::{StatusPublication, StatusRecords, as_of as program_status_as_of};
pub use settle::{FrameStability, frame_stability_hash};

/// Pixel size of one cell as DECLARED to pane children through
/// `ws_xpixel`/`ws_ypixel`. A client sizes its bitmap against this, so an
/// image's cell footprint is derived from the same box. `shux-pty` declares
/// it; `crates/shux` pins the two together.
pub const DECLARED_CELL_PIXELS: (u32, u32) = (9, 19);
pub use tabstops::TabStops;

use crate::parser::VtParser;
use std::sync::atomic::Ordering;

/// How long synchronized output (`CSI ?2026h`) may hold the presented frame
/// before shux shows the live one anyway.
///
/// `?2026h` is a promise to finish and send `?2026l`. A pane that breaks that
/// promise — an application killed mid-redraw, a script that emits the open
/// and nothing else — would otherwise leave its pane showing one frame for
/// ever, with a copy of that frame pinned in daemon memory for just as long.
/// So the promise has a deadline, as it does in every other terminal that
/// implements the mode: releasing early costs one torn frame, and not
/// releasing costs the pane.
///
/// 150 ms is the value the ecosystem settled on (it is Alacritty's
/// `SYNC_UPDATE_TIMEOUT`). It is an order of magnitude longer than a
/// full-screen redraw by `vim`, `lazygit` or `btop`, which is what the mode
/// exists to protect, and short enough that a broken pane looks like a glitch
/// rather than a hang.
pub const SYNC_UPDATE_TIMEOUT_MS: u64 = 150;

/// How much pane output one synchronized-output window may absorb before the
/// presented frame is released regardless of the clock.
///
/// A second guard on a different axis from the timeout, so that neither has to
/// be trusted alone: a clock can be read wrong, and a pane that produces
/// megabytes without finishing its redraw is not mid-redraw. 2 MiB is
/// Alacritty's `SYNC_BUFFER_SIZE`.
pub const SYNC_UPDATE_MAX_BYTES: u64 = 2 * 1024 * 1024;

use crate::parser::{DcsState, LastGraphic};

/// Per-pane virtual terminal.
///
/// Owns the grid, cursor, terminal modes, and the vte parser state machine.
/// Feed PTY output bytes via `process()` and read the resulting grid state
/// for rendering.
pub struct VirtualTerminal {
    /// Primary screen grid.
    grid: Grid,
    /// Alternate screen grid (for fullscreen apps like vim).
    alt_grid: Option<Grid>,
    /// The retired alternate-screen buffer, held for the next entry so that
    /// toggling the alternate screen stops costing a full grid allocation per
    /// eight-byte escape sequence (issue #106). At most one, ever: this is a
    /// slot, not a cache.
    alt_spare: Option<Grid>,
    /// Current cursor state.
    cursor: Cursor,
    /// Saved cursor for alternate screen.
    alt_cursor: Option<Cursor>,
    /// Terminal mode flags.
    modes: TerminalModes,
    /// Scroll region (top/bottom margins).
    scroll_region: ScrollRegion,
    /// Window title (set via OSC 0/2).
    title: Option<String>,
    /// Dynamic default foreground/background/cursor set via OSC 10/11/12.
    default_colors: TerminalDefaultColors,
    /// vte parser state machine.
    parser: VtParser,
    /// In-progress DCS payload, preserved across partial PTY chunks.
    dcs_state: Option<DcsState>,
    /// Locates kitty-graphics APC sequences, whose bytes vte consumes without
    /// calling back. Carries state across PTY reads.
    apc_scanner: graphics::apc::ApcScanner,
    /// False only in the differential oracle; see [`Self::set_apc_cut_slicing`].
    apc_cut_slicing: bool,
    /// Frames OSC 7501 / OSC 133 from raw bytes. Runs even while program
    /// status is disabled, so enabling mid-sequence cannot misparse.
    status_scanner: program_status::OscStatusScanner,
    commands: commands::CommandBoundaries,
    /// The absolute row the last prompt started on.
    prompt_row: Option<u64>,
    /// `None` while program status is disabled.
    program_status: Option<program_status::VtProgramStatus>,
    /// Generation of the last disable, or of construction.
    status_disabled_generation: u64,
    /// Everything the graphics path writes; see `dispatch_graphics`.
    graphics: graphics::GraphicsSink,
    /// Whether synchronized output (`CSI ?2026h`) is holding the presentation
    /// open. Shared by reference with every [`sync::Presented`] wrapper handed
    /// to the parser, which is why it is a cell rather than a plain `bool`;
    /// atomic only so `VirtualTerminal` stays `Sync` (issue #115).
    sync_armed: std::sync::atomic::AtomicBool,
    /// The presented frame, one component per slot, each filled lazily by the
    /// first write that would change it and empty while the presented value is
    /// still the live one. `None` everywhere is the normal state, including for
    /// most of a `?2026h`/`?2026l` window that draws nothing.
    ///
    /// Each component freezes independently and that is still coherent: a
    /// component is snapshotted on its FIRST change after `?2026h`, and it did
    /// not change before that, so every slot holds its `?2026h` value whether
    /// it was filled at `?2026h` or long after.
    frozen_grid: Option<sync::FrozenScreen>,
    frozen_cursor: Option<Cursor>,
    frozen_colors: Option<TerminalDefaultColors>,
    /// `Some(None)` is a real state: no window title at freeze time. The outer
    /// `Option` is "is it frozen", the inner one is "was there a title".
    frozen_title: Option<Option<String>>,
    /// Alt-screen flag at freeze time (codex P2 review blocker): presented
    /// readers (glance) must see grid/cursor/colors AND this flag from the
    /// same frozen frame — an alt toggle inside ?2026h must not leak a
    /// future flag against old pixels.
    frozen_alt: Option<bool>,
    /// Screen swaps, frozen alongside `frozen_alt`.
    swaps: screen::Swaps,
    /// Whether to take the whole snapshot at `?2026h` rather than at the first
    /// write. Always `false` outside the differential tests — see
    /// [`VirtualTerminal::set_eager_sync_freeze`].
    eager_sync_freeze: bool,
    /// Monotonic nanoseconds at which the open synchronized-output window was
    /// first seen to still be open at the end of a batch, or 0 when no window
    /// is open. Stamped once per window rather than at `?2026h`, so a pane
    /// toggling the mode millions of times a second does not buy a clock read
    /// per toggle.
    sync_opened_ns: u64,
    /// Pane output absorbed since the open window started.
    sync_bytes: u64,
    /// Visible cell currently accepting zero-width/joined grapheme scalars.
    active_grapheme_cell: Option<(usize, usize)>,
    /// The preceding character in the data stream, which is what REP (`CSI b`)
    /// repeats (issue #122). Unlike `active_grapheme_cell` this is NOT a
    /// position on the screen and survives everything that moves the cursor;
    /// only RIS clears it.
    last_graphic: Option<LastGraphic>,
    /// VT100 G0/G1 charset designations and active locking shift.
    charsets: TerminalCharsets,
    /// Mutable horizontal tab-stop state.
    tab_stops: TabStops,
    /// Number of visible rows.
    rows: usize,
    /// Number of columns.
    cols: usize,
    /// Lens ContentRevision (PRD §4, LENS-R-001): per-pane monotonic `u64`
    /// starting at 1, incremented by exactly 1 per Class-A mutation BATCH
    /// (one `process()`/`resize()` producing ≥1 Class-A event). Never wraps,
    /// never decreases, not persisted. Independent of `SessionGraph` version
    /// and `DirtyState` (LENS-R-005, §4.4).
    content_revision: u64,
    /// Monotonic-clock nanoseconds at the last Class-A batch (LENS-R-002),
    /// INITIALIZED to pane-creation time so a fresh pane never reports
    /// "settled" before `quiet_ms` of genuine silence.
    last_mutation_ns: u64,
    /// Class-A events occurred while synchronized output (CSI ?2026h) froze
    /// the presentation. The revision bump is DEFERRED to mode release as ONE
    /// batch (§4.2 adjudicated row): the counter tracks the PRESENTED frame,
    /// matching `grid()`/`cursor()`'s frozen view.
    sync_hidden_class_a: bool,
    /// Sticky: a valid OSC 4 palette override was applied at least once. shux-vt
    /// discards the override (Class-B limitation), so an indexed-colour capture
    /// taken afterwards cannot be trusted to render identically elsewhere. The
    /// lens gate reads [`VirtualTerminal::palette_overridden`] to emit the
    /// `palette_unportable` diagnostic (task 078, R1). Deliberately NOT part of
    /// the content-revision accounting.
    palette_overridden: bool,
    /// Whether retired screen buffers may be recycled. Always `true` outside
    /// the differential tests — see [`VirtualTerminal::set_retired_grid_reuse`].
    reuse_retired_grids: bool,
}

/// Process-monotonic nanoseconds since the first VT was created. Used for
/// `last_mutation_ns` (lens settle substrate). Clamped to ≥1: LENS-R-002 says
/// never 0, but the caller that INITIALIZES the epoch (the first
/// `VirtualTerminal::new()` in the process) would otherwise read elapsed == 0.
///
/// PUBLIC (P3, LENS-R-020): `pane.wait_settled` must read "now" on the SAME
/// clock that stamped `last_mutation_ns`. Both callers share the one `START`
/// epoch below, so `monotonic_now_ns() − last_mutation_ns` is a valid elapsed
/// duration — a mismatched clock here is the councils-caught bug class the
/// ns×1_000_000 settle math depends on avoiding.
pub fn monotonic_now_ns() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    let start = START.get_or_init(Instant::now);
    (start.elapsed().as_nanos() as u64).max(1)
}

/// The clock the synchronized-output deadline and the hidden-cursor stamp
/// read. Tests freeze and advance it with [`test_clock::Frozen`] so a deadline
/// test never races the scheduler.
fn sync_clock_ns() -> u64 {
    #[cfg(test)]
    if let Some(now) = test_clock::now() {
        return now;
    }
    monotonic_now_ns()
}

#[cfg(test)]
mod test_clock {
    use std::cell::Cell;

    thread_local! {
        static NOW: Cell<Option<u64>> = const { Cell::new(None) };
    }

    pub(super) fn now() -> Option<u64> {
        NOW.with(Cell::get)
    }

    /// Stops the deadline clock on this thread until dropped.
    pub(super) struct Frozen;

    impl Frozen {
        pub(super) fn new() -> Self {
            NOW.with(|now| now.set(Some(super::monotonic_now_ns())));
            Frozen
        }

        pub(super) fn advance_ms(&self, ms: u64) {
            NOW.with(|now| now.set(now.get().map(|t| t + ms * 1_000_000)));
        }
    }

    impl Drop for Frozen {
        fn drop(&mut self) {
            NOW.with(|now| now.set(None));
        }
    }
}

/// A terminal with no rows or no columns has no cell to put anything in, and
/// every addressing path in the parser assumes at least one of each: a bare
/// printable character on a 0-row grid indexed row 0 and panicked, and
/// DECSTBM's `rows - 1` clamp underflowed into a region of rows that did not
/// exist (issue #107).
///
/// Rather than teach several dozen parser paths to survive a terminal that
/// cannot display anything, a degenerate size is made unrepresentable at the
/// two places a size enters: construction and resize. Callers that hand shux a
/// 0 get the smallest real terminal instead of a panicking one. The daemon
/// separately refuses to *ask* for a degenerate pane; this is the backstop
/// under it, not a substitute for it.
fn clamp_dims(rows: usize, cols: usize) -> (usize, usize) {
    (rows.max(1), cols.max(1))
}

impl VirtualTerminal {
    /// Create a new virtual terminal with the given dimensions.
    pub fn new(rows: usize, cols: usize) -> Self {
        Self::with_config(rows, cols, GridConfig::default())
    }

    /// Create a new virtual terminal with custom grid configuration.
    pub fn with_config(rows: usize, cols: usize, config: GridConfig) -> Self {
        let (rows, cols) = clamp_dims(rows, cols);
        VirtualTerminal {
            grid: Grid::new(rows, cols, config),
            alt_grid: None,
            alt_spare: None,
            cursor: Cursor::new(),
            alt_cursor: None,
            modes: TerminalModes::default(),
            scroll_region: ScrollRegion {
                top: 0,
                bottom: rows.saturating_sub(1),
            },
            title: None,
            default_colors: TerminalDefaultColors::default(),
            parser: VtParser::new_with_size(),
            dcs_state: None,
            apc_scanner: graphics::apc::ApcScanner::default(),
            apc_cut_slicing: true,
            status_scanner: program_status::OscStatusScanner::default(),
            commands: commands::CommandBoundaries::default(),
            prompt_row: None,
            program_status: None,
            status_disabled_generation: program_status::disabled_generation(),
            graphics: graphics::GraphicsSink::default(),
            sync_armed: std::sync::atomic::AtomicBool::new(false),
            frozen_grid: None,
            frozen_cursor: None,
            frozen_colors: None,
            frozen_title: None,
            frozen_alt: None,
            swaps: screen::Swaps::default(),
            eager_sync_freeze: false,
            sync_opened_ns: 0,
            sync_bytes: 0,
            active_grapheme_cell: None,
            last_graphic: None,
            charsets: TerminalCharsets::default(),
            tab_stops: TabStops::new(cols),
            rows,
            cols,
            // LENS-R-001/002: revision starts at 1; last-mutation clock is
            // seeded at creation so settle can't fire before real silence.
            content_revision: 1,
            last_mutation_ns: monotonic_now_ns(),
            sync_hidden_class_a: false,
            palette_overridden: false,
            reuse_retired_grids: true,
        }
    }

    /// Turn APC cut-slicing off, so `process_with_responses` feeds vte one
    /// unbroken call exactly as it did before graphics existed.
    ///
    /// The only oracle with teeth is a terminal that does not slice.
    /// Chunking-invariance is not one: vte is itself chunk-sensitive (see
    /// `c1_controls_are_chunk_sensitive_in_vte`), so comparing two chunkings
    /// measures vte. Production never calls this.
    #[doc(hidden)]
    pub fn set_apc_cut_slicing(&mut self, enabled: bool) {
        self.apc_cut_slicing = enabled;
    }

    /// How many graphics commands this terminal has declined.
    ///
    /// shux answers nothing on the wire, so without this a refusal is
    /// indistinguishable from a command that was never located -- the gap that
    /// let a whole neutrality suite pass against a scanner gutted to find
    /// nothing.
    #[doc(hidden)]
    pub fn graphics_refusals(&self) -> u64 {
        self.graphics.refusals
    }

    /// Absolute index the LIVE grid is numbering its rows from.
    ///
    /// Exists so `tests/alt_screen_differential.rs` can compare a `pub(crate)`
    /// field. Live rather than presented: a frozen viewport clone counts from
    /// its own base (`Grid::evicted`), which is not the base `presented_row`
    /// resolves against, and the recycling defect this observes lands on the
    /// live grid at alternate-screen entry.
    #[doc(hidden)]
    pub fn eviction_base(&self) -> u64 {
        self.grid.evicted()
    }

    /// Turn retired-buffer recycling (issue #106) off or on.
    ///
    /// Recycling is required to be UNOBSERVABLE: a terminal that reuses its
    /// retired alternate-screen buffer must behave identically to one that
    /// allocates a fresh buffer every time. That is a property, not a hope, so
    /// the differential tests drive the same byte stream through two terminals
    /// — one with this off, one with it on — and compare every observable.
    /// This exists for that oracle. Production never calls it.
    #[doc(hidden)]
    pub fn set_retired_grid_reuse(&mut self, enabled: bool) {
        self.reuse_retired_grids = enabled;
        if !enabled {
            self.alt_spare = None;
        }
    }

    /// Take the synchronized-output snapshot at `CSI ?2026h` instead of at the
    /// first write that would change the presented frame (issue #115).
    ///
    /// Deferring the snapshot is required to be UNOBSERVABLE: a terminal that
    /// freezes lazily must behave identically to one that freezes the instant
    /// the mode opens. That is a property, not a hope, so the differential
    /// tests drive the same byte stream through two terminals — one eager, one
    /// lazy — and compare every observable after every step. This exists for
    /// that oracle. Production never calls it.
    #[doc(hidden)]
    pub fn set_eager_sync_freeze(&mut self, enabled: bool) {
        self.eager_sync_freeze = enabled;
        if enabled && self.sync_armed.load(std::sync::atomic::Ordering::Relaxed) {
            self.freeze_whole_presentation();
        }
    }

    /// Snapshot every still-unfrozen component of the presented frame.
    ///
    /// Called by the paths that mutate presented state from OUTSIDE the parser
    /// — `resize` and the direct alternate-screen API — where there is no
    /// [`sync::Presented`] wrapper to do it. Both are daemon-driven: a pane
    /// cannot emit bytes that reach them, so neither is an amplification
    /// route.
    fn freeze_whole_presentation(&mut self) {
        if !self.sync_armed.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        if self.frozen_grid.is_none() {
            self.frozen_grid = Some(sync::PresentedFrame::snapshot(&self.grid));
        }
        if self.frozen_cursor.is_none() {
            self.frozen_cursor = Some(self.cursor.clone());
        }
        if self.frozen_colors.is_none() {
            self.frozen_colors = Some(self.default_colors);
        }
        if self.frozen_title.is_none() {
            self.frozen_title = Some(self.title.clone());
        }
        if self.frozen_alt.is_none() {
            self.frozen_alt = Some(self.modes.alternate_screen);
            self.swaps.freeze();
        }
    }

    /// Whether releasing the window right now would reveal a **Class-A**
    /// change (PRD §4.2) — one that moves `ContentRevision`.
    ///
    /// The window title is deliberately NOT in this list. A title is Class B,
    /// and it stays Class B whether the pane closes its own window or the
    /// deadline closes it: otherwise the same `OSC 2` would move the revision
    /// or not depending on nothing but whether `?2026l` arrived, which is the
    /// `class_b_osc_title_no_bump` contract read two ways.
    fn sync_release_reveals_class_a(&self) -> bool {
        self.sync_hidden_class_a
            || self.frozen_grid.is_some()
            || self.frozen_cursor.is_some()
            || self.frozen_colors.is_some()
            || self.frozen_alt.is_some()
    }

    /// Drop the frozen presentation and show the live frame again, as
    /// `CSI ?2026l` would.
    ///
    /// Returns whether ANYTHING the pane had hidden is now visible — including
    /// a Class-B title, which a caller outside the parse loop still has to
    /// forward even though it moves no revision.
    fn force_release_sync(&mut self) -> bool {
        let revealed = self.sync_release_reveals_class_a() || self.frozen_title.is_some();
        self.sync_armed
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.frozen_grid = None;
        self.frozen_cursor = None;
        self.frozen_colors = None;
        self.frozen_title = None;
        self.frozen_alt = None;
        self.swaps.frozen = None;
        self.modes.synchronized_output = false;
        self.sync_opened_ns = 0;
        self.sync_bytes = 0;
        // The presentation jumps from the frozen frame to the live one, so
        // every cell on screen may differ and the renderer repaints in full.
        self.grid.mark_all_dirty();
        revealed
    }

    /// Whether an open synchronized-output window has outlived
    /// [`SYNC_UPDATE_TIMEOUT_MS`] or absorbed more than
    /// [`SYNC_UPDATE_MAX_BYTES`].
    fn sync_window_expired(&self) -> bool {
        if !self.sync_armed.load(std::sync::atomic::Ordering::Relaxed) || self.sync_opened_ns == 0 {
            return false;
        }
        self.sync_bytes >= SYNC_UPDATE_MAX_BYTES
            || sync_clock_ns().saturating_sub(self.sync_opened_ns)
                >= SYNC_UPDATE_TIMEOUT_MS * 1_000_000
    }

    /// Release a synchronized-output window that has outlived its deadline,
    /// reporting whether the presented frame moved.
    ///
    /// The only release on time; parsing releases only at [`SYNC_UPDATE_MAX_BYTES`],
    /// since a batch parsed late may have been sent in time. Call it only when
    /// none of the pane's output is pending.
    pub fn release_expired_sync(&mut self) -> bool {
        if !self.sync_window_expired() {
            return false;
        }
        let class_a = self.sync_release_reveals_class_a();
        let revealed = self.force_release_sync();
        self.sync_hidden_class_a = false;
        if class_a {
            self.record_class_a_batch();
        }
        revealed
    }

    /// Whether synchronized output is currently holding the presented frame.
    pub fn sync_output_active(&self) -> bool {
        self.sync_armed.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Process raw PTY output bytes through the VT parser.
    ///
    /// This is the main entry point for feeding terminal data.
    /// Each byte is parsed by vte, which calls back into our handler
    /// to mutate the grid and cursor.
    pub fn process(&mut self, bytes: &[u8]) {
        let _ = self.process_with_responses(bytes);
    }

    /// Feed one slice of the batch to vte.
    ///
    /// Split out of [`Self::process_with_responses`] so a batch can be cut at
    /// APC boundaries while vte still receives every byte.
    fn advance_slice(&mut self, bytes: &[u8], responses: &mut Vec<Vec<u8>>) {
        if bytes.is_empty() {
            return;
        }
        self.commands.capture(bytes);
        let mut handler = VtHandler {
            grid: sync::Presented::new(&mut self.grid, &mut self.frozen_grid, &self.sync_armed),
            cursor: sync::Presented::new(
                &mut self.cursor,
                &mut self.frozen_cursor,
                &self.sync_armed,
            ),
            modes: &mut self.modes,
            scroll_region: &mut self.scroll_region,
            title: sync::Presented::new(&mut self.title, &mut self.frozen_title, &self.sync_armed),
            default_colors: sync::Presented::new(
                &mut self.default_colors,
                &mut self.frozen_colors,
                &self.sync_armed,
            ),
            alt_grid: &mut self.alt_grid,
            alt_cursor: &mut self.alt_cursor,
            dcs_state: &mut self.dcs_state,
            sync_armed: &self.sync_armed,
            frozen_alt: &mut self.frozen_alt,
            swaps: &mut self.swaps,
            eager_sync_freeze: self.eager_sync_freeze,
            active_grapheme_cell: &mut self.active_grapheme_cell,
            last_graphic: &mut self.last_graphic,
            charsets: &mut self.charsets,
            tab_stops: &mut self.tab_stops,
            responses,
            palette_overridden: &mut self.palette_overridden,
            alt_spare: &mut self.alt_spare,
            reuse_retired_grids: self.reuse_retired_grids,
            program_status: &mut self.program_status,
            image_stores: &mut self.graphics.stores,
        };
        self.parser.advance(&mut handler, bytes);
    }

    /// `len`: the sequence's wire length, which a command it ends did not write.
    fn dispatch_program_status(
        &mut self,
        action: program_status::CutAction,
        len: usize,
        responses: &mut Vec<Vec<u8>>,
    ) {
        use program_status::CutAction;
        match action {
            CutAction::CommandStart => {
                let line = self.input_rows();
                self.commands.on_start(len, self.grid.cols(), line);
                self.prompt_row = None;
            }
            CutAction::CommandEnd { exit_code } => {
                self.commands.on_end(exit_code, len, self.grid.cols());
            }
            CutAction::PromptStart { editor } => {
                if self.commands.state() == ShellState::AtPrompt {
                    let rows = self.input_rows();
                    self.commands.on_input_rejected(rows);
                }
                self.commands.on_prompt(len, self.grid.cols(), editor);
                self.prompt_row = (!self.modes.alternate_screen).then(|| self.cursor_row_abs());
                if let Some(ps) = self.program_status.as_mut() {
                    ps.on_prompt_start();
                }
            }
            CutAction::Report { payload, bel } => {
                let Some(ps) = self.program_status.as_mut() else {
                    return;
                };
                let terminator: &[u8] = if bel { b"\x07" } else { b"\x1b\\" };
                if let Some(reply) = ps.on_report(&payload, terminator)
                    && responses.len() < parser::MAX_RESPONSES_PER_BATCH
                {
                    responses.push(reply);
                }
            }
        }
    }

    fn cursor_row_abs(&self) -> u64 {
        self.grid.evicted() + (self.grid.scrollback_len() + self.cursor.row) as u64
    }

    /// What the screen shows from the last prompt's row to the cursor's (its
    /// own row too unless the cursor is at its start): the prompt and the
    /// line typed at it, as the line editor finally drew them. One entry per
    /// line, soft-wrapped rows joined.
    fn input_rows(&self) -> Vec<String> {
        const MOST: u64 = 200;
        let (Some(from), false) = (self.prompt_row, self.modes.alternate_screen) else {
            return Vec::new();
        };
        let cursor = self.cursor_row_abs();
        let end = if self.cursor.col == 0 {
            cursor
        } else {
            cursor + 1
        };
        let from = from.max(end.saturating_sub(MOST)).max(self.grid.evicted());
        let mut lines = vec![String::new()];
        for abs in from..end {
            let Some(row) = usize::try_from(abs - self.grid.evicted())
                .ok()
                .and_then(|i| self.grid.row(i))
            else {
                break;
            };
            let mut text = String::new();
            for cell in row.cells.iter().filter(|c| c.width > 0) {
                cell.push_display_text(&mut text);
            }
            if let Some(line) = lines.last_mut() {
                line.push_str(text.trim_end_matches(' '));
            }
            if !row.wrapped {
                lines.push(String::new());
            }
        }
        lines.retain(|l| !l.is_empty());
        lines
    }

    /// Lines submitted to the shell above `floor`, in the order it read
    /// them: the commands they started (with what the screen showed for
    /// each), and the inputs it rejected. `None` when one is no longer kept.
    pub fn submitted_after(&self, floor: u64) -> Option<Vec<Submitted<'_>>> {
        self.commands.submitted_after(floor)
    }

    /// Whether this terminal has seen a prompt or a command start (`133;A`,
    /// `133;C`): its shell sends command boundaries. Never reset by RIS.
    pub fn commands_seen(&self) -> bool {
        self.commands.seen()
    }

    /// A command has started and not yet ended.
    pub fn command_running(&self) -> bool {
        self.commands.running()
    }

    /// The `n`th command (from 0) to start above `floor`.
    pub fn nth_command_after(&self, floor: u64, n: usize) -> NthCommand {
        self.commands.nth_after(floor, n)
    }

    /// The earliest ended command that started above `floor`.
    pub fn command_after(&self, floor: u64) -> CommandLookup {
        self.commands.first_after(floor)
    }

    /// What the ended command that started at `started` wrote, while kept.
    pub fn command_output(&self, started: u64) -> Option<CommandOutput<'_>> {
        self.commands.output(started)
    }

    /// What the running command has written so far, if it started above
    /// `floor`.
    pub fn running_command_output(&self, floor: u64) -> Option<CommandOutput<'_>> {
        self.commands.running_output(floor)
    }

    /// Where the shell stands, by the last boundary it reported.
    pub fn shell_state(&self) -> ShellState {
        self.commands.state()
    }

    /// Note input sent to the pane's program: whether it submitted a line (a
    /// CR or LF outside a bracketed paste), and whether keys that submitted
    /// nothing followed the last submission.
    pub fn note_input(&mut self, submits: bool, edits: bool) {
        self.commands.on_input(submits, edits);
    }

    /// Keys were sent to the shell's prompt since it was drawn and submitted
    /// nothing: its line editor may hold text, or be in another mode.
    pub fn input_present(&self) -> bool {
        self.commands.input_present()
    }

    /// The shell is at its prompt with a line submitted to it that has not
    /// started a command yet: a line typed now would run after it.
    pub fn line_pending(&self) -> bool {
        self.commands.line_pending()
    }

    /// The line editor the last prompt's mark named, if any.
    pub fn prompt_editor(&self) -> Option<LineEditor> {
        self.commands.editor()
    }

    /// How many prompts (`133;A`) this terminal has seen.
    pub fn prompts_seen(&self) -> u64 {
        self.commands.prompts()
    }

    /// Moves on every boundary that changed [`Self::shell_state`].
    pub fn boundary_marks(&self) -> u64 {
        self.commands.marks()
    }

    /// Turn OSC 7501 program status on or off. Off answers no probe and
    /// keeps no records — the terminal behaves as if the protocol did not
    /// exist. Turning it on starts empty.
    pub fn set_program_status_enabled(&mut self, enabled: bool) {
        match (enabled, self.program_status.is_some()) {
            (true, false) => self.program_status = Some(program_status::VtProgramStatus::new()),
            // Off (again or for the first time) always draws a fresh
            // generation: a terminal turned off before an await must still
            // outrank whatever its predecessor published meanwhile.
            (false, _) => {
                self.program_status = None;
                self.status_disabled_generation = program_status::disabled_generation();
            }
            (true, true) => {}
        }
    }

    pub fn program_status_enabled(&self) -> bool {
        self.program_status.is_some()
    }

    /// The text-free view the daemon forwards to the session graph. Every
    /// change, including enabling, disabling and replacing the terminal,
    /// carries a newer `generation` than anything before it in the process.
    pub fn program_status_publication(&self) -> StatusPublication {
        match &self.program_status {
            Some(ps) => ps.publication(),
            None => StatusPublication {
                generation: self.status_disabled_generation,
                summary: None,
                seen: false,
                enabled: false,
            },
        }
    }

    /// Every record, most urgent first. `None` while disabled.
    pub fn program_status_records(&self) -> Option<StatusRecords> {
        self.program_status.as_ref().map(|ps| ps.records())
    }

    /// The process attached to this terminal exited: `working`, `blocked`
    /// and `idle` records end; `done` and `error` stay.
    pub fn note_process_exit(&mut self) {
        if let Some(ps) = self.program_status.as_mut() {
            ps.on_process_exit();
        }
    }

    /// Act on one kitty graphics command. Non-`G` APCs are discarded, as vte
    /// discards every APC today.
    ///
    /// Placing and deleting a picture change the presented frame, so this
    /// takes `&mut self` -- and every write it makes goes through
    /// [`sync::Presented`], which freezes that frame on the first write inside
    /// a `CSI ?2026h` window. Reaching a raw field here would leak into a frame
    /// held still.
    fn dispatch_graphics(&mut self, body: &[u8], responses: &mut Vec<Vec<u8>>) {
        let Some(command) = body.strip_prefix(b"G") else {
            return;
        };
        #[cfg(test)]
        self.graphics.dispatched.push(command.to_vec());

        let (control, payload) = match command.iter().position(|b| *b == b';') {
            Some(i) => (&command[..i], &command[i + 1..]),
            None => (command, &b""[..]),
        };
        let Ok(cmd) = graphics::kitty::parse(control) else {
            self.graphics.refusals = self.graphics.refusals.saturating_add(1);
            self.graphics.assembler.abort();
            return;
        };

        use graphics::kitty::Action;
        match cmd.action {
            Action::Transmit | Action::TransmitAndPlace => {
                // "The existing image and all its placements must be deleted"
                // as the re-transmit begins, whether or not it completes or is
                // placed. An app that redraws under one id (terminal-browser
                // sends `i=1` every frame) would otherwise spend a slot per
                // frame.
                let alt = self.modes.alternate_screen;
                if !self.graphics.assembler.is_open() {
                    self.graphics.transfer_on_alt = alt;
                    if cmd.image_id != 0 {
                        self.store().retain(|k| k != cmd.image_id);
                        self.presented_grid().unplace_image(cmd.image_id, 0);
                    }
                }
                let Some((image, opening)) = self.graphics.assembler.feed(&cmd, payload) else {
                    return;
                };
                if self.graphics.transfer_on_alt != alt {
                    self.graphics.refusals = self.graphics.refusals.saturating_add(1);
                    return;
                }
                let image = std::sync::Arc::new(image);
                if opening.image_id != 0 {
                    self.store().insert(opening.image_id, image.clone());
                }
                if opening.action == Action::TransmitAndPlace {
                    self.place_image(image, &opening);
                }
            }
            // An image number names the newest image transmitted under it,
            // which only kitty's own id assignment can resolve.
            Action::Put => match self.store().get(cmd.image_id) {
                Some(image) => self.place_image(image, &cmd),
                None => self.graphics.refusals = self.graphics.refusals.saturating_add(1),
            },
            Action::Delete => {
                self.delete_images(&cmd);
                self.graphics.assembler.abort();
            }
            // Default `kitten icat` probes each transport and waits on a DA1
            // sentinel; with no reply before it, it declares the terminal
            // unsupported and sends no image bytes at all. Only a DIRECT probe
            // reaches here -- `parse` refuses the others first -- and silence
            // on those is what makes icat fall back to direct.
            Action::Query => {
                if cmd.image_id != 0 {
                    responses.push(format!("\x1b_Gi={};OK\x1b\\", cmd.image_id).into_bytes());
                }
            }
            Action::Animation => {}
        }
    }

    /// The live grid, through `Presented`: a placement added or dropped
    /// changes the presented frame, so inside a `CSI ?2026h` window it must
    /// freeze first or it tears the redraw it landed in (issue #115).
    fn presented_grid(&mut self) -> sync::Presented<'_, Grid> {
        sync::Presented::new(&mut self.grid, &mut self.frozen_grid, &self.sync_armed)
    }

    /// The store of the screen now showing.
    fn store(&mut self) -> &mut graphics::image::ImageStore {
        &mut self.graphics.stores[usize::from(self.modes.alternate_screen)]
    }

    /// `a=d`: drop the placements the target hits. Uppercase also frees the
    /// data of the images it removed, or named, once nothing shows them.
    /// Targets by image number or animation frame are refused: shux can
    /// resolve neither.
    fn delete_images(&mut self, cmd: &graphics::kitty::Command) {
        let target = if cmd.delete == 0 { b'a' } else { cmd.delete };
        let first = self.grid.evicted() + self.grid.scrollback_len() as u64;
        let rows = self.grid.rows() as u64;
        // `x,y` name a 1-based cell; a cell is (screen column, absolute row).
        let (x, y) = (cmd.src[0] as usize, cmd.src[1] as usize);
        // A 0 coordinate names no cell.
        let cell = (
            x.wrapping_sub(1),
            y.checked_sub(1).map_or(u64::MAX, |y| first + y as u64),
        );
        let cursor = (self.cursor.col, first + self.cursor.row as u64);
        let z = cmd.z;
        let covers = |p: &grid::Placement, col: Option<usize>, row: Option<u64>| {
            let (w, h) = p.geometry.cells();
            col.is_none_or(|c| (p.col..p.col + w).contains(&c))
                && row.is_none_or(|r| (p.abs_row..p.abs_row + h as u64).contains(&r))
        };
        let (id, pid) = (cmd.image_id, cmd.placement_id);
        let hit: Box<dyn Fn(&grid::Placement) -> bool> = match target.to_ascii_lowercase() {
            // Any part on screen; a placement wholly in scrollback stays.
            b'a' => Box::new(move |p| {
                p.abs_row < first + rows && p.abs_row + p.geometry.cells().1 as u64 > first
            }),
            b'i' if id != 0 => {
                Box::new(move |p| p.image_id == id && (pid == 0 || p.placement_id == pid))
            }
            b'r' if x <= y => Box::new(move |p| (x as u32..=y as u32).contains(&p.image_id)),
            b'c' => Box::new(move |p| covers(p, Some(cursor.0), Some(cursor.1))),
            b'p' => Box::new(move |p| covers(p, Some(cell.0), Some(cell.1))),
            b'q' => Box::new(move |p| p.z == z && covers(p, Some(cell.0), Some(cell.1))),
            b'x' => Box::new(move |p| covers(p, Some(cell.0), None)),
            b'y' => Box::new(move |p| covers(p, None, Some(cell.1))),
            b'z' => Box::new(move |p| p.z == z),
            _ => {
                self.graphics.refusals = self.graphics.refusals.saturating_add(1);
                return;
            }
        };
        let removed = self.presented_grid().unplace_where(hit);
        if target.is_ascii_uppercase() {
            // Named outright: an id with no placement given, or an id range.
            let named = |k: u32| match target {
                b'I' => pid == 0 && k == id,
                b'R' => (x as u64..=y as u64).contains(&u64::from(k)),
                _ => false,
            };
            let grid = &self.grid;
            let store = &mut self.graphics.stores[usize::from(self.modes.alternate_screen)];
            store.retain(|k| !(removed.contains(&k) || named(k)) || grid.shows_image(k));
        }
    }

    /// Anchor a picture at the cursor and advance past it: right by `cols`,
    /// down by `rows - 1`, wrapping hard at the pane width. The `- 1` is what
    /// kitty (`graphics.c:1315`) and Zellij (`grid.rs:3772`) do against a spec
    /// that says otherwise, and `kitten icat`'s single trailing newline only
    /// lands correctly if the terminal stopped short.
    fn place_image(
        &mut self,
        image: std::sync::Arc<graphics::image::StoredImage>,
        cmd: &graphics::kitty::Command,
    ) {
        let max = (self.grid.cols(), self.grid.rows());
        let Some(geometry) = grid::ImageGeometry::resolve(
            (image.width, image.height),
            cmd.src,
            (cmd.cols, cmd.rows),
            max,
        ) else {
            return;
        };
        // `s=`/`v=` are unvalidated wire numbers; unclamped, `v=4294967295`
        // is 226 million `scroll_up` calls inside the pane-IO lock.
        let (cols, rows) = geometry.cells();
        let (cols, rows) = (cols.min(max.0), rows.min(max.1));
        if cols == 0 || rows == 0 {
            return;
        }

        let abs_row =
            self.grid.evicted() + self.grid.scrollback_len() as u64 + self.cursor.row as u64;
        let col = self.cursor.col;
        // Disjoint from the cursor, which moves below while this is held.
        let mut grid =
            sync::Presented::new(&mut self.grid, &mut self.frozen_grid, &self.sync_armed);
        let placement = grid::Placement {
            image,
            abs_row,
            col,
            image_id: cmd.image_id,
            placement_id: cmd.placement_id,
            z: cmd.z,
            geometry,
        };
        if !grid.put(placement) {
            self.graphics.refusals = self.graphics.refusals.saturating_add(1);
            return;
        }
        if cmd.no_cursor_move {
            return;
        }

        let (grid_rows, grid_cols) = (grid.rows(), grid.cols());
        let mut cursor =
            sync::Presented::new(&mut self.cursor, &mut self.frozen_cursor, &self.sync_armed);
        let mut down = rows - 1;
        let target = cursor.col + cols;
        if target >= grid_cols {
            cursor.col = 0;
            down += 1;
        } else {
            cursor.col = target;
        }
        for _ in 0..down {
            if cursor.row + 1 >= grid_rows {
                grid.scroll_up(0, grid_rows - 1);
            } else {
                cursor.row += 1;
            }
        }
    }

    /// Process raw PTY output bytes and return terminal reply bytes.
    ///
    /// This is the request/response half of terminal emulation. Apps running
    /// under `TERM=xterm-256color` commonly emit DA/DSR/OSC/DCS probes and
    /// wait for the terminal to answer on stdin. Callers that own the PTY must
    /// write every returned response back to the child process.
    pub fn process_with_responses(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        // Lens ContentRevision (PRD §4): snapshot the Class-A signals BEFORE the
        // parser runs. `self.grid` is always the live writable grid (alt-screen
        // enter/leave swaps it), and its `mutations()` tally is value-independent
        // (identical repaints still advance it — §4.2 "MUST NOT diff to decide").
        // Cursor position/visibility, the alt-screen flag, and the OSC 10/11/12
        // dynamic default colors are compared as the table literally states
        // ("... change"); this is NOT cell-value diffing. Default colors are
        // Class A per the P2 re-adjudication (§4.2 OSC row): revision tracks
        // the PRESENTED frame, and a dynamic-default-color change alters every
        // rendered pixel that resolves Color::Default.
        //
        // The color compare uses the PRESENTED colors (the `default_colors()`
        // accessor, sync-aware), not the raw live field (codex P2 review
        // major): while ?2026h freezes the presentation, hidden color churn
        // must not accumulate a deferred bump — a set-then-restore inside
        // sync nets to NO presented change. The release batch compares the
        // frozen colors against the now-live value, so a real net change
        // still bumps exactly once at ?2026l, and a net-zero one never does.
        // A window that has absorbed too much output is released BEFORE this
        // batch lands. Time is not judged here: see `release_expired_sync`.
        if self.sync_armed.load(std::sync::atomic::Ordering::Relaxed)
            && self.sync_bytes >= SYNC_UPDATE_MAX_BYTES
        {
            self.force_release_sync();
        }

        let before_writes = self.grid.mutations();
        let before_cursor = (self.cursor.row, self.cursor.col, self.cursor.visible);
        let before_alt = self.modes.alternate_screen;
        let before_colors = self.default_colors();

        let mut responses = Vec::new();

        // Locate APCs but never remove them: vte gets every byte and only its
        // `advance` calls are cut. See the [`graphics::apc`] module docs for why
        // stripping cannot be made correct, and for why cutting is neutral --
        // which is a property of where the cuts land, NOT of vte being
        // chunk-insensitive, because it is not. Cutting at the boundary places each
        // command at its true stream position -- a placement anchors at the
        // cursor as it stands there, and replies queue in request order.
        let apc_cuts = if self.apc_cut_slicing {
            self.apc_scanner.scan(bytes)
        } else {
            Vec::new()
        };
        // Program-status cuts slice the same way, so a reply queues at the
        // query's stream position: before the DA1 that follows it.
        let mut status_cuts = self.status_scanner.scan(bytes);
        if self.program_status.is_none() {
            status_cuts.retain(|c| !matches!(c.action, program_status::CutAction::Report { .. }));
        }
        if apc_cuts.is_empty() && status_cuts.is_empty() {
            self.advance_slice(bytes, &mut responses);
        } else {
            let mut at = 0;
            let mut apc = apc_cuts.into_iter().peekable();
            let mut status = status_cuts.into_iter().peekable();
            loop {
                let take_apc = match (apc.peek(), status.peek()) {
                    (None, None) => break,
                    (Some(a), Some(s)) => a.end <= s.end,
                    (a, _) => a.is_some(),
                };
                if take_apc {
                    let Some(cut) = apc.next() else { break };
                    self.advance_slice(&bytes[at..cut.end], &mut responses);
                    self.dispatch_graphics(&cut.body, &mut responses);
                    at = cut.end;
                } else {
                    let Some(cut) = status.next() else { break };
                    self.advance_slice(&bytes[at..cut.end], &mut responses);
                    self.dispatch_program_status(cut.action, cut.len, &mut responses);
                    at = cut.end;
                }
            }
            self.advance_slice(&bytes[at..], &mut responses);
        }

        let after_alt = self.modes.alternate_screen;
        let after_cursor = (self.cursor.row, self.cursor.col, self.cursor.visible);
        // Alt toggle is Class-A on its own and also invalidates the write-tally
        // comparison (the tally belongs to whichever grid is now live), so only
        // compare tallies when the alt flag is unchanged. `default_colors` is a
        // single VT-level field (not swapped by alt screen), so its compare
        // needs no alt guard; the parser's own change-guards make a same-value
        // OSC set a net-zero batch (no bump), per the §4.2 batching rule.
        // PRESENTED colors on both sides: under sync this pair is frozen==
        // frozen (hidden churn never flags), and the release batch is
        // frozen-vs-live (net change bumps once, net-zero never).
        let presented_colors_changed = self.default_colors() != before_colors;
        let other_class_a = after_alt != before_alt
            || after_cursor != before_cursor
            || (before_alt == after_alt && self.grid.mutations() != before_writes);
        // §4.2 (adjudicated, PR #87 bot P1): while synchronized output
        // (CSI ?2026h) freezes the presentation, Class-A events must NOT bump
        // immediately — the counter tracks the PRESENTED frame, matching
        // grid()/cursor()'s frozen view. Defer: accumulate hidden events and
        // record exactly ONE batch when the mode releases (nothing hidden →
        // release records nothing).
        //
        // EXCEPTION (claude P2 review minor a): the presented-colors compare
        // is sync-aware on BOTH sides, so when it reports a change while sync
        // is active at batch end, the PRESENTATION itself changed within this
        // batch — e.g. `OSC 10` landing in the SAME batch that then opened
        // ?2026h froze the new color into the presentation. Deferring that
        // bump would lag the revision behind visibly-changed presented pixels
        // for the whole sync window. Bump immediately; only the OTHER Class-A
        // signals (whose compares read live state and cannot distinguish
        // pre-freeze from post-freeze changes within the batch) defer.
        if self.sync_armed.load(std::sync::atomic::Ordering::Relaxed) {
            if presented_colors_changed {
                self.record_class_a_batch();
            }
            if other_class_a {
                self.sync_hidden_class_a = true;
            }
        } else if std::mem::take(&mut self.sync_hidden_class_a)
            || presented_colors_changed
            || other_class_a
        {
            self.record_class_a_batch();
        }

        // Deadline bookkeeping. Stamped here rather than at `?2026h` so that a
        // pane toggling the mode does not buy a clock read per toggle: a window
        // that opens and closes inside one batch is never stamped at all, and
        // one that survives the batch is stamped exactly once.
        if self.sync_armed.load(std::sync::atomic::Ordering::Relaxed) {
            if self.sync_opened_ns == 0 {
                self.sync_opened_ns = sync_clock_ns();
            }
            self.sync_bytes = self.sync_bytes.saturating_add(bytes.len() as u64);
        } else {
            self.sync_opened_ns = 0;
            self.sync_bytes = 0;
        }
        self.stamp_hidden_cursor();
        responses
    }

    /// Give a streak this batch started its time: one clock read per batch,
    /// however often the batch toggles the cursor.
    fn stamp_hidden_cursor(&mut self) {
        let mut now = None;
        for cursor in std::iter::once(&mut self.cursor).chain(self.frozen_cursor.as_mut()) {
            if !cursor.visible && cursor.hidden_since_ns == 0 {
                cursor.hidden_since_ns = *now.get_or_insert_with(sync_clock_ns);
            }
        }
    }

    /// Advance ContentRevision by exactly one BATCH (§4.2 batching rule) and
    /// stamp the mutation clock. Called once per `process()`/`resize()` call
    /// that produced ≥1 Class-A event — inside the VT write path (single-writer
    /// task), BEFORE any watch publish (§4.4).
    fn record_class_a_batch(&mut self) {
        // "never wrapping" (LENS-R-001): saturating_add clamps instead of
        // wrapping; u64 exhaustion is unreachable in practice.
        self.content_revision = self.content_revision.saturating_add(1);
        self.last_mutation_ns = monotonic_now_ns();
    }

    /// Lens ContentRevision (PRD §4, LENS-R-001): monotonic per-pane frame
    /// counter, one per Class-A batch. Exposed on `session.snapshot` pane
    /// entries (LENS-R-006) and later by `pane.glance` (P2).
    pub fn content_revision(&self) -> u64 {
        self.content_revision
    }

    /// Monotonic-clock nanoseconds at the last Class-A batch (LENS-R-002),
    /// seeded at pane creation. Basis for `pane.wait_settled` (P3).
    pub fn last_mutation_ns(&self) -> u64 {
        self.last_mutation_ns
    }

    /// Access the current (active) grid.
    pub fn grid(&self) -> &Grid {
        self.frozen_grid
            .as_ref()
            .map(|frozen| &frozen.grid)
            .unwrap_or(&self.grid)
    }

    /// How many lines of history stand behind the presented frame right now.
    ///
    /// While a synchronized-output window is open this is the history the pane
    /// had when the window opened, less anything that has since fallen off the
    /// front of the scrollback — which is the truth: a line that has been
    /// evicted is gone from the pane, frozen frame or not.
    /// The grid the presented frame's history actually lives in.
    ///
    /// Entering the alternate screen inside a window swaps which grid is live,
    /// so a frame frozen on the primary screen leaves its history in the
    /// STASHED grid. Reading it from the live one hands back alternate-screen
    /// rows as "history" — content the frozen frame exists to hide — and holes
    /// where the alternate screen is shorter (it is built with no scrollback).
    ///
    /// The reverse case needs nothing: a frame frozen on the alternate screen
    /// has no history to read, for the same reason.
    fn presented_history_grid(&self) -> &Grid {
        if self
            .frozen_alt
            .is_some_and(|frozen_alt| frozen_alt != self.modes.alternate_screen)
        {
            self.alt_grid.as_ref().unwrap_or(&self.grid)
        } else {
            &self.grid
        }
    }

    fn presented_history_len(&self) -> usize {
        let Some(ref frozen) = self.frozen_grid else {
            return self.grid.scrollback_len();
        };
        let history_grid = self.presented_history_grid();
        let dropped = history_grid.evicted().saturating_sub(frozen.evicted);
        frozen
            .history_len
            .saturating_sub(usize::try_from(dropped).unwrap_or(usize::MAX))
    }

    /// Total lines the PRESENTED frame spans: history plus the frame itself.
    ///
    /// Copy mode's coordinate space. Distinct from `grid().total_lines()`,
    /// which while a window is open describes the frozen VIEWPORT alone —
    /// history is not part of the frame and is read live (issue #115).
    pub fn presented_total_lines(&self) -> usize {
        match self.frozen_grid {
            Some(ref frozen) => self.presented_history_len() + frozen.grid.total_lines(),
            None => self.grid.total_lines(),
        }
    }

    /// Lines evicted from the front of the presented history so far: never
    /// decreases, and presented line `i` names the same line as long as it
    /// does not change.
    pub fn presented_evicted(&self) -> u64 {
        self.presented_history_grid().evicted()
    }

    /// One line of the presented frame by absolute index, counting history
    /// from 0. The companion to [`VirtualTerminal::presented_total_lines`].
    pub fn presented_row(&self, abs: usize) -> Option<&Row> {
        let Some(ref frozen) = self.frozen_grid else {
            return self.grid.row(abs);
        };
        let history = self.presented_history_len();
        if abs < history {
            // History lives in the live grid, and eviction removes lines from
            // its FRONT — so the surviving lines slide DOWN to meet index 0
            // rather than the index needing to slide up to meet them. Adding
            // the eviction count here instead walks straight past the survivors
            // and into the live viewport, putting content written after the
            // freeze inside the frame that exists to hide it.
            self.presented_history_grid().row(abs)
        } else {
            frozen.grid.row(abs - history)
        }
    }

    /// Whether the currently presented viewport has changed since the last
    /// dirty drain.
    pub fn is_dirty(&self) -> bool {
        self.grid().is_dirty()
    }

    /// Consume and clear dirty regions for the currently presented viewport.
    ///
    /// This reports visible grid changes only. Cursor-only movement is outside
    /// this API because renderers draw cursor presentation as an overlay.
    pub fn take_dirty_regions(&mut self) -> Vec<DirtyRegion> {
        self.frozen_grid
            .as_mut()
            .map(|frozen| &mut frozen.grid)
            .unwrap_or(&mut self.grid)
            .take_dirty_regions()
    }

    /// Access the cursor state.
    pub fn cursor(&self) -> &Cursor {
        self.frozen_cursor.as_ref().unwrap_or(&self.cursor)
    }

    /// Access terminal modes.
    pub fn modes(&self) -> &TerminalModes {
        &self.modes
    }

    /// Get the window title (set by OSC 0/2).
    pub fn title(&self) -> Option<&str> {
        // The frozen slot wins even when it is `Some(None)` — a pane that had
        // no title when `?2026h` opened still has none until `?2026l`. Falling
        // through to the live title in that case leaked a title set inside the
        // synchronized window into the frozen frame.
        match self.frozen_title {
            Some(ref frozen) => frozen.as_deref(),
            None => self.title.as_deref(),
        }
    }

    /// Dynamic default foreground/background/cursor set by OSC 10/11/12.
    pub fn default_colors(&self) -> TerminalDefaultColors {
        self.frozen_colors.unwrap_or(self.default_colors)
    }

    /// Whether alternate screen is active in the PRESENTED frame.
    ///
    /// While synchronized output (?2026) freezes the presentation, this
    /// reads the flag captured at freeze time — the same source as
    /// `grid()`/`cursor()`/`default_colors()` — so a presented reader
    /// (glance) can never pair old pixels with a future alt flag (codex P2
    /// review blocker). Live mode state is available via `modes()`.
    pub fn is_alternate_screen(&self) -> bool {
        self.frozen_alt.unwrap_or(self.modes.alternate_screen)
    }

    /// Which screen the PRESENTED frame shows, as a count of swaps: two reads
    /// are equal only if no swap came between them, a round trip included.
    pub fn presented_screen(&self) -> u64 {
        self.swaps.presented()
    }

    /// Whether a valid OSC 4 palette override has been applied to this VT at
    /// least once. Sticky (an override persists for the VT's life). shux-vt
    /// discards the override colour (adjudicated Class-B limitation), so an
    /// indexed-colour capture taken after an override is NOT portable to another
    /// machine's palette — the lens gate flags such a capture `palette_unportable`
    /// (task 078, R1). Independent of content-revision accounting.
    pub fn palette_overridden(&self) -> bool {
        self.palette_overridden
    }

    /// Get the current scroll region.
    pub fn scroll_region(&self) -> &ScrollRegion {
        &self.scroll_region
    }

    /// Resize the virtual terminal.
    ///
    /// This resizes both primary and alternate grids, adjusts the scroll
    /// region, and clamps the cursor position.
    pub fn resize(&mut self, rows: usize, cols: usize) {
        let (rows, cols) = clamp_dims(rows, cols);
        // A resize RELEASES any open synchronized-output window.
        //
        // The frame an application asked shux to hold still is a frame drawn
        // for a geometry that no longer exists. Reflowing it instead is not
        // just extra work, it is wrong in two ways that adversarial review
        // found: the frozen frame holds no history to rewrap against, so a
        // widening resize cannot pull soft-wrapped lines back up the way the
        // live grid does; and on the ALTERNATE screen the live grid is
        // canvas-resized rather than reflowed, so reflowing the frozen copy
        // presented `vim` and `lazygit` content rewrapped in a way the
        // application never drew (that one predates the lazy freeze).
        //
        // Releasing costs one torn frame in a situation where the application
        // is about to repaint anyway — `SIGWINCH` is on its way — and it is
        // not an amplification route, because resizes come from the daemon and
        // no pane can emit one.
        if (rows != self.rows || cols != self.cols) && self.sync_armed.load(Ordering::Relaxed) {
            self.force_release_sync();
            std::mem::take(&mut self.sync_hidden_class_a);
        }
        // §4.2: "Pane resize" is Class-A. A resize to the SAME dimensions is not
        // a resize event (avoids spurious bumps when the daemon re-fans an
        // unchanged winsize) — compare dims, which is not cell-value diffing.
        let dims_changed = rows != self.rows || cols != self.cols;
        self.active_grapheme_cell = None;
        // A retired alternate-screen buffer is sized for the geometry it was
        // retired at. Rather than reason about a stale one surviving a reflow,
        // drop it: resizes come from the daemon, never from pane output, so
        // nothing a pane emits can turn this into churn.
        if dims_changed {
            self.alt_spare = None;
        }
        let auto_wrap = self.modes.auto_wrap;
        if self.modes.alternate_screen {
            self.grid.resize_canvas(rows, cols);
            let carried = self.cursor.saved.take();
            self.cursor.clamp(rows, cols);
            self.cursor.saved = carried;
            // The stashed primary grid is resized whether or not a saved
            // cursor came with it. DECSET 1047 enters the alternate screen
            // WITHOUT saving a cursor, so gating the whole branch on
            // `alt_cursor` left the primary grid at its pre-resize size: on
            // leaving 1047 the pane reported one geometry and rendered
            // another, and the next erase/insert indexed a row that was no
            // longer there (issue #107 adversarial review). 1049 was never
            // affected, which is why this survived.
            if let Some(primary) = &mut self.alt_grid {
                resize_primary(
                    primary,
                    rows,
                    cols,
                    auto_wrap,
                    self.alt_cursor.as_mut(),
                    self.cursor.saved.as_mut(),
                );
            }
        } else {
            let cursor = Some(&mut self.cursor);
            resize_primary(&mut self.grid, rows, cols, auto_wrap, cursor, None);
            if let Some(ref mut alt) = self.alt_grid {
                alt.resize_canvas(rows, cols);
            }
        }
        // No frozen frame can survive to here: a dimension change released it.
        debug_assert!(
            self.frozen_grid.is_none() || (rows == self.rows && cols == self.cols),
            "a synchronized-output window survived a resize"
        );
        self.rows = rows;
        self.cols = cols;
        self.tab_stops.resize(cols);
        self.scroll_region = ScrollRegion {
            top: 0,
            bottom: rows.saturating_sub(1),
        };
        if let Some(ref mut frozen_cursor) = self.frozen_cursor {
            frozen_cursor.clamp(rows, cols);
        }
        if dims_changed {
            self.record_class_a_batch();
        }
    }

    /// Borrow everything the alternate-screen swap needs. Shares one
    /// definition with the parser's `DECSET 1047/1049` and `RIS` paths, so a
    /// buffer retired through one route is reused by the other.
    fn screen_swap(&mut self) -> screen::ScreenSwap<'_> {
        screen::ScreenSwap {
            grid: &mut self.grid,
            cursor: &mut self.cursor,
            stashed_grid: &mut self.alt_grid,
            stashed_cursor: &mut self.alt_cursor,
            spare: &mut self.alt_spare,
            swaps: &mut self.swaps,
            reuse: self.reuse_retired_grids,
            alt_images: &mut self.graphics.stores[1],
        }
    }

    /// Switch to alternate screen buffer (DECSET 1049).
    pub fn enter_alternate_screen(&mut self) {
        self.freeze_whole_presentation();
        self.active_grapheme_cell = None;
        if !self.modes.alternate_screen {
            self.screen_swap().enter(true);
            self.modes.alternate_screen = true;
        }
    }

    /// Switch back to primary screen buffer (DECRST 1049).
    pub fn leave_alternate_screen(&mut self) {
        self.freeze_whole_presentation();
        self.active_grapheme_cell = None;
        if self.modes.alternate_screen {
            self.screen_swap().leave(true);
            self.modes.alternate_screen = false;
        }
        self.stamp_hidden_cursor();
    }

    /// Clear the scrollback buffer.
    pub fn clear_scrollback(&mut self) {
        self.grid.clear_scrollback();
    }

    /// Get the number of scrollback lines.
    pub fn scrollback_len(&self) -> usize {
        self.grid.scrollback_len()
    }

    /// Capture visible-viewport text, matching iTerm2 `get_screen_contents`.
    ///
    /// - `lines = None` → entire visible viewport, with trailing blank rows
    ///   trimmed (matches iTerm2's behaviour: get the whole screen, drop
    ///   the always-empty tail).
    /// - `lines = Some(N)` → up to N most-recent non-blank rows. Walks
    ///   back from the LAST non-blank row, picking that row and up to
    ///   N-1 preceding ones. Empty rows below the cursor are never
    ///   counted toward N.
    ///
    /// Returns `"\n"` when the viewport is entirely blank.
    pub fn capture_text(&self, lines: Option<usize>) -> String {
        let grid = self.grid();
        let total_rows = grid.rows();
        if total_rows == 0 {
            return String::new();
        }

        let last_content = (0..total_rows)
            .rev()
            .find(|&r| !grid.visible_row(r).is_blank())
            .unwrap_or(0);
        let end = last_content + 1;
        let start = match lines {
            Some(0) => return String::new(),
            Some(n) => end.saturating_sub(n),
            None => 0,
        };

        let mut output = String::new();
        for row_idx in start..end {
            let row = grid.visible_row(row_idx);
            let mut line = String::new();
            for cell in row.cells.iter() {
                if cell.is_wide_continuation() {
                    continue;
                }
                cell.push_display_text(&mut line);
            }
            output.push_str(line.trim_end());
            output.push('\n');
        }
        output
    }
}

/// Resize the primary `grid`, carrying through it `cursor` (the primary
/// cursor, live or parked) and every DECSC slot saved on the primary screen;
/// `carried` is a slot the alternate screen's cursor holds. A deferred wrap
/// survives only under DECAWM. Only the cursor holds its row on screen;
/// slots saved on the alternate screen are clamped.
fn resize_primary(
    grid: &mut Grid,
    rows: usize,
    cols: usize,
    auto_wrap: bool,
    cursor: Option<&mut Cursor>,
    carried: Option<&mut SavedCursor>,
) {
    let point = |row, col, pending: bool, holds_row, after_print: bool| ResizePoint {
        row,
        col,
        past_glyph: pending && auto_wrap,
        holds_row,
        after_print: pending || after_print,
    };
    let slot_point =
        |s: &SavedCursor| point(s.row, s.col, s.auto_wrap_pending, false, s.after_print);
    let primary_slot = |slot: &&SavedCursor| !slot.on_alternate;
    let mut points = Vec::with_capacity(3);
    if let Some(cursor) = &cursor {
        points.push(point(
            cursor.row,
            cursor.col,
            cursor.auto_wrap_pending,
            true,
            cursor.after_print,
        ));
        points.extend(cursor.saved.as_ref().filter(primary_slot).map(slot_point));
    }
    points.extend(carried.as_deref().filter(primary_slot).map(slot_point));
    grid.resize_with_points(rows, cols, &mut points);

    let mut points = points.into_iter().map(|mut p| {
        p.past_glyph &= auto_wrap;
        p
    });
    if let Some(cursor) = cursor
        && let Some(p) = points.next()
    {
        cursor.row = p.row.min(rows.saturating_sub(1));
        cursor.col = p.col.min(cols.saturating_sub(1));
        cursor.auto_wrap_pending = p.past_glyph;
        if let Some(slot) = &mut cursor.saved {
            place_slot(slot, &mut points, rows, cols);
        }
    }
    if let Some(slot) = carried {
        place_slot(slot, &mut points, rows, cols);
    }
}

fn place_slot(
    slot: &mut SavedCursor,
    points: &mut impl Iterator<Item = ResizePoint>,
    rows: usize,
    cols: usize,
) {
    if !slot.on_alternate
        && let Some(p) = points.next()
    {
        (slot.row, slot.col, slot.auto_wrap_pending) = (p.row, p.col, p.past_glyph);
    } else {
        slot.after_print = false;
        slot.row = slot.row.min(rows.saturating_sub(1));
        slot.col = slot.col.min(cols.saturating_sub(1));
        slot.auto_wrap_pending = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn compact_capture(vt: &VirtualTerminal) -> String {
        vt.capture_text(None).replace('\n', "")
    }

    const BLANK_BG_LINES: &[u8] =
        b"\x1b[44m          \x1b[0m\r\nX\x1b[48;2;10;20;30m      \x1b[0m\r\n\x1b[101m   \x1b[0m";

    fn blank_bg_counts(vt: &VirtualTerminal) -> [usize; 3] {
        let grid = vt.grid();
        let mut counts = [0; 3];
        for r in 0..grid.total_lines() {
            let row = grid.row(r).expect("row exists");
            for c in 0..row.len() {
                let cell = &row[c];
                if cell.ch != ' ' {
                    continue;
                }
                match cell.style.bg {
                    Color::Indexed(4) => counts[0] += 1,
                    Color::Rgb(10, 20, 30) => counts[1] += 1,
                    Color::Indexed(9) => counts[2] += 1,
                    _ => {}
                }
            }
        }
        counts
    }

    #[test]
    fn resize_keeps_background_of_blank_cells() {
        for (rows, cols) in [(33, 54), (30, 100), (10, 40), (12, 80), (24, 20)] {
            let mut vt = VirtualTerminal::new(24, 80);
            vt.process(BLANK_BG_LINES);
            vt.resize(rows, cols);
            assert_eq!(blank_bg_counts(&vt), [10, 6, 3], "80x24 -> {cols}x{rows}");
        }
    }

    #[test]
    fn resize_keeps_blank_bg_after_wide_combining_and_linked_text() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(
            "界e\u{301}\x1b]8;;https://x.test\x1b\\\x1b[44m   \x1b]8;;\x1b\\\x1b[0m".as_bytes(),
        );
        for (rows, cols) in [(24, 7), (24, 120), (10, 40)] {
            vt.resize(rows, cols);
            assert_grid_wide_invariants(vt.grid());
            let row = vt.grid().visible_row(0);
            assert!(row[0].is_wide() && row[1].is_wide_continuation(), "{cols}");
            assert_eq!(row[2].grapheme(), Some("e\u{301}"), "{cols}");
            for col in 3..6 {
                assert_eq!(row[col].style.bg, Color::Indexed(4), "{cols} col {col}");
                let link = row[col]
                    .extended
                    .as_ref()
                    .and_then(|e| e.hyperlink.as_deref());
                assert_eq!(link, Some("https://x.test"), "{cols} col {col}");
            }
        }
    }

    #[test]
    fn resize_keeps_cursor_column_after_trailing_blanks() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"$ ");
        vt.resize(24, 40);
        assert_eq!((vt.cursor().row, vt.cursor().col), (0, 2), "default blank");
        vt.process(b"\r\n\x1b[44m    ");
        vt.resize(24, 30);
        assert_eq!(
            (vt.cursor().row, vt.cursor().col),
            (1, 4),
            "coloured blanks"
        );
    }

    #[test]
    fn resize_keeps_background_of_blank_cells_in_scrollback() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(BLANK_BG_LINES);
        vt.process(&b"\r\n".repeat(40));
        assert!(vt.scrollback_len() >= 3);
        vt.resize(33, 54);
        assert_eq!(blank_bg_counts(&vt), [10, 6, 3]);
        let blue = vt.grid().scrollback_row(0).expect("scrollback row");
        assert_eq!(blue[9].style.bg, Color::Indexed(4));
    }

    #[test]
    fn resize_keeps_background_of_blank_cells_across_alt_screen() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(BLANK_BG_LINES);
        vt.process(b"\x1b[?1049h\x1b[H\x1b[44m    \x1b[0m");
        vt.resize(33, 54);
        assert_eq!(blank_bg_counts(&vt), [4, 0, 0], "alternate screen");
        vt.process(b"\x1b[?1049l");
        assert_eq!(blank_bg_counts(&vt), [10, 6, 3], "primary behind alt");
    }

    fn visible_text(vt: &VirtualTerminal, row: usize) -> String {
        row_text(vt, vt.scrollback_len() + row)
    }

    #[test]
    fn column_shrink_lets_erase_fill_below_the_cursor_yield() {
        let ed: &[u8] = b"\x1b[44m\x1b[2J\x1b[H\x1b[0mABCDEFGH";
        let el: &[u8] =
            b"\x1b[H\x1b[44m\x1b[K\n\x1b[K\n\x1b[K\n\x1b[K\n\x1b[K\n\x1b[K\x1b[H\x1b[0mABCDEFGH";
        for max_scrollback in [0, 100] {
            for (name, erase) in [("ED", ed), ("EL", el)] {
                let config = GridConfig {
                    max_scrollback,
                    ..GridConfig::default()
                };
                let mut vt = VirtualTerminal::with_config(6, 10, config);
                if max_scrollback > 0 {
                    vt.process(b"old1\r\nold2\r\nold3\r\nold4\r\nold5\r\nold6\r\nold7\r\n");
                }
                vt.process(erase);
                let sb = vt.scrollback_len();
                vt.resize(6, 4);
                let ctx = format!("{name} max_scrollback={max_scrollback}");
                assert_eq!(visible_text(&vt, 0), "ABCD", "{ctx}");
                assert_eq!(visible_text(&vt, 1), "EFGH", "{ctx}");
                assert_eq!(vt.scrollback_len(), sb, "{ctx}");
                assert_eq!(vt.cursor().row, 1, "{ctx}");
                for row in 2..6 {
                    let cell = &vt.grid().visible_row(row)[0];
                    assert_eq!(cell.style.bg, Color::Indexed(4), "{ctx} row {row}");
                }
            }
        }
    }

    #[test]
    fn hard_line_padding_keeps_what_fits_and_never_regains() {
        let mut vt = VirtualTerminal::new(4, 10);
        vt.process(b"ABCDEFG\x1b[44m   \x1b[0m");
        for (cols, blue) in [(12, 3), (8, 1), (12, 1), (10, 1)] {
            vt.resize(4, cols);
            assert_eq!(blank_bg_counts(&vt)[0], blue, "{cols} cols");
            assert_eq!(visible_text(&vt, 0), "ABCDEFG", "{cols} cols");
            assert_eq!(visible_text(&vt, 1), "", "{cols} cols");
        }
    }

    #[test]
    fn padding_is_counted_across_soft_wraps() {
        // ABC + 12 blue blanks: the blanks soft-wrap onto a second row.
        let mut vt = VirtualTerminal::new(4, 10);
        vt.process(b"ABC\x1b[44m            \x1b[0m");
        assert!(vt.grid().visible_row(0).wrapped);
        for (cols, blue) in [(9, 6), (10, 6), (11, 6), (20, 6)] {
            vt.resize(4, cols);
            assert_eq!(blank_bg_counts(&vt)[0], blue, "{cols} cols");
            assert!(!vt.grid().visible_row(0).wrapped, "{cols} cols");
        }
        let mut wide = VirtualTerminal::new(4, 10);
        wide.process(b"ABC\x1b[44m            \x1b[0m");
        wide.resize(4, 11);
        assert_eq!(blank_bg_counts(&wide)[0], 8);
    }

    #[test]
    fn linked_and_attributed_blanks_are_content() {
        let mut vt = VirtualTerminal::new(4, 10);
        vt.process(b"X\x1b]8;;https://x.test\x1b\\     \x1b]8;;\x1b\\\x1b[7m  \x1b[0m");
        vt.resize(4, 3);
        let grid = vt.grid();
        let mut linked = 0;
        let mut inverse = 0;
        for r in 0..grid.rows() {
            let row = grid.visible_row(r);
            for c in 0..row.len() {
                if row[c]
                    .extended
                    .as_ref()
                    .and_then(|e| e.hyperlink.as_deref())
                    .is_some()
                {
                    linked += 1;
                }
                if row[c].style.flags.contains(CellFlags::INVERSE) {
                    inverse += 1;
                }
            }
        }
        assert_eq!((linked, inverse), (5, 2));
        assert!(vt.grid().visible_row(1).wrapped);
    }

    #[test]
    fn resize_keeps_cursor_column_inside_untouched_padding() {
        let mut vt = VirtualTerminal::new(4, 80);
        vt.process(b"abc\x1b[1;21H");
        vt.resize(4, 40);
        assert_eq!((vt.cursor().row, vt.cursor().col), (0, 20));
        vt.resize(4, 10);
        assert_eq!((vt.cursor().row, vt.cursor().col), (0, 9));
    }

    #[test]
    fn exact_width_shrink_then_typing_wraps() {
        let mut vt = VirtualTerminal::new(4, 10);
        vt.process(b"ABCDE");
        vt.resize(4, 5);
        vt.process(b"F");
        assert_eq!(
            (visible_text(&vt, 0), visible_text(&vt, 1)),
            ("ABCDE".into(), "F".into())
        );
    }

    #[test]
    fn pending_wrap_survives_a_widen() {
        let mut vt = VirtualTerminal::new(4, 5);
        vt.process(b"ABCDE");
        assert!(vt.cursor().auto_wrap_pending);
        vt.resize(4, 10);
        vt.process(b"F");
        assert_eq!(visible_text(&vt, 0), "ABCDEF");
    }

    #[test]
    fn pending_wrap_without_decawm_is_not_an_offset() {
        let mut vt = VirtualTerminal::new(4, 5);
        vt.process(b"\x1b[?7lABCDEFG");
        vt.resize(4, 10);
        vt.process(b"Z");
        assert_eq!(visible_text(&vt, 0), "ABCDZ");
    }

    #[test]
    fn saved_cursor_is_remapped_by_reflow() {
        let mut vt = VirtualTerminal::new(4, 10);
        vt.process(b"ABCDEFGHIJKL\x1b7\x1b[4;1H");
        vt.resize(4, 20);
        vt.process(b"\x1b8");
        assert_eq!((vt.cursor().row, vt.cursor().col), (0, 12), "ESC 8");

        let mut alt = VirtualTerminal::new(4, 10);
        alt.process(b"ABCDEFGHIJKL\x1b[?1049h\x1b[3;3H");
        alt.resize(4, 20);
        alt.process(b"\x1b[?1049l");
        assert_eq!((alt.cursor().row, alt.cursor().col), (0, 12), "?1049l");
    }

    #[test]
    fn saved_cursor_is_remapped_on_the_screen_it_was_saved_on() {
        // Saved on the primary, carried into ?1047, restored after leaving.
        let mut vt = VirtualTerminal::new(4, 10);
        vt.process(b"ABCDEFGHIJKL\x1b7\x1b[?1047h\x1b[H");
        vt.resize(4, 20);
        vt.process(b"\x1b[?1047l\x1b8");
        assert_eq!((vt.cursor().row, vt.cursor().col), (0, 12), "primary slot");
        // Saved on the alternate screen: no primary reflow moves it.
        let mut alt = VirtualTerminal::new(4, 10);
        alt.process(b"ABCDEFGHIJKL\x1b[?1047h\x1b[2;3H\x1b7\x1b[H");
        alt.resize(4, 20);
        alt.process(b"\x1b8");
        assert_eq!(
            (alt.cursor().row, alt.cursor().col),
            (1, 2),
            "alternate slot"
        );
    }

    #[test]
    fn wide_glyph_wrap_gap_is_not_a_printed_space() {
        let mut reflowed = VirtualTerminal::new(4, 8);
        reflowed.process("ab界".as_bytes());
        reflowed.resize(4, 3);
        reflowed.resize(4, 8);
        assert_eq!(visible_text(&reflowed, 0), "ab界", "reflow gap");

        let mut printed = VirtualTerminal::new(4, 3);
        printed.process("ab界".as_bytes());
        printed.resize(4, 8);
        assert_eq!(visible_text(&printed, 0), "ab界", "print gap");

        // A printed space in the last cell of a wrapped row, with no gap.
        let mut space = VirtualTerminal::new(4, 3);
        space.process("ab 界".as_bytes());
        space.resize(4, 8);
        assert_eq!(visible_text(&space, 0), "ab 界", "printed space");
    }

    #[test]
    fn a_space_moved_into_a_wide_gap_is_kept() {
        // Printed into the gap.
        let mut printed = VirtualTerminal::new(4, 3);
        printed.process("ab界\x1b[1;3H ".as_bytes());
        printed.resize(4, 8);
        assert_eq!(visible_text(&printed, 0), "ab 界", "print");
        // Shifted into the gap by ICH.
        let mut inserted = VirtualTerminal::new(4, 3);
        inserted.process("a 界\x1b[1;1H\x1b[@".as_bytes());
        inserted.resize(4, 8);
        assert_eq!(visible_text(&inserted, 0), " a 界", "ICH");
    }

    #[test]
    fn row_shrink_keeps_the_cursor_on_its_row() {
        let mut vt = VirtualTerminal::new(6, 10);
        vt.process(b"a\r\nb\r\nc\r\nd\r\ne\r\n");
        vt.resize(3, 10);
        assert_eq!(vt.cursor().row, 2);
        assert_eq!(visible_text(&vt, 1), "e");
        assert_eq!(visible_text(&vt, 2), "");
    }

    fn no_scrollback(rows: usize, cols: usize) -> VirtualTerminal {
        let config = GridConfig {
            max_scrollback: 0,
            ..GridConfig::default()
        };
        VirtualTerminal::with_config(rows, cols, config)
    }

    #[test]
    fn a_saved_cursor_does_not_pin_blank_rows() {
        for fill in [&b""[..], b"\x1b[44m\x1b[2J\x1b[0m"] {
            let mut vt = no_scrollback(6, 10);
            vt.process(fill);
            vt.process(b"\x1b[6;1H\x1b7\x1b[HABCDEFGH");
            vt.resize(6, 4);
            assert_eq!(visible_text(&vt, 0), "ABCD", "fill {fill:?}");
            assert_eq!(visible_text(&vt, 1), "EFGH", "fill {fill:?}");
        }
    }

    #[test]
    fn reflow_creates_no_pending_wrap_without_decawm() {
        let mut vt = VirtualTerminal::new(4, 10);
        vt.process(b"\x1b[?7lABCDE");
        vt.resize(4, 5);
        vt.process(b"\x1b[?7hF");
        assert_eq!(visible_text(&vt, 0), "ABCDF");
        assert_eq!(visible_text(&vt, 1), "");
    }

    #[test]
    fn an_evicted_saved_cursor_keeps_no_pending_wrap() {
        let mut vt = no_scrollback(2, 10);
        vt.process(b"ABCDE\x1b7\r\nFGHIJKLM");
        vt.resize(2, 5);
        vt.process(b"\x1b8Z");
        assert_eq!(visible_text(&vt, 0), "FGHIZ");
        assert_eq!(visible_text(&vt, 1), "KLM");
    }

    #[test]
    fn a_styled_space_printed_into_a_wide_gap_is_kept() {
        let mut vt = VirtualTerminal::new(4, 3);
        vt.process("ab界\x1b[1;3H\x1b[7m \x1b[m".as_bytes());
        vt.resize(4, 8);
        assert_eq!(visible_text(&vt, 0), "ab 界");
        let row = vt.grid().visible_row(0);
        assert!(row[2].style.flags.contains(CellFlags::INVERSE));
    }

    #[test]
    fn a_blank_written_for_an_unwrapped_wide_glyph_is_not_a_gap() {
        let mut vt = VirtualTerminal::new(4, 3);
        vt.process("ab界\x1b[1;3H\x1b[?7l\x1b[7m界\x1b[m".as_bytes());
        vt.resize(4, 8);
        assert_eq!(visible_text(&vt, 0), "ab 界");
        let row = vt.grid().visible_row(0);
        assert!(row[2].style.flags.contains(CellFlags::INVERSE));
    }

    #[test]
    fn deleting_the_wide_gap_leaves_a_gap() {
        let mut vt = no_scrollback(2, 3);
        vt.process("ab界c\x1b[1;3H\x1b[1P".as_bytes());
        vt.resize(1, 5);
        assert_eq!(visible_text(&vt, 0), "ab界c");
    }

    #[test]
    fn a_blank_shifted_into_the_last_column_is_not_a_gap() {
        let mut vt = VirtualTerminal::new(2, 3);
        vt.process("ab界c\x1b[1;2H🇺🇸\x1b[1;1H\x1b[1P\x1b[1;3H".as_bytes());
        vt.resize(2, 8);
        vt.process(b"Z");
        let row = vt.grid().visible_row(0);
        let text: String = (2..row.len()).map(|c| row[c].ch).collect();
        assert_eq!(text.trim_end(), "Z界 c");
    }

    #[test]
    fn the_wide_gap_mark_is_invisible_to_equality_and_capture() {
        let mut gap = VirtualTerminal::new(2, 3);
        gap.process("ab界".as_bytes());
        let mut plain = VirtualTerminal::new(2, 3);
        plain.process("ab \r\n界".as_bytes());
        let (g, p) = (
            &gap.grid().visible_row(0)[2],
            &plain.grid().visible_row(0)[2],
        );
        assert_eq!(g, p);
        assert_eq!(gap.capture_text(None), plain.capture_text(None));
    }

    #[test]
    fn the_wide_gap_mark_is_not_an_extended_attribute() {
        let mut vt = VirtualTerminal::new(4, 3);
        vt.process("ab界".as_bytes());
        assert!(vt.grid().visible_row(0)[2].extended.is_none());
        vt.grid.visible_row_mut(0)[2].prune_default_extended();
        vt.resize(4, 8);
        assert_eq!(visible_text(&vt, 0), "ab界");
    }

    #[test]
    fn the_wide_gap_mark_costs_no_cell_space() {
        assert_eq!(std::mem::size_of::<Cell>(), 24);
    }

    #[test]
    fn deleting_before_a_wide_gap_adds_no_space() {
        let mut vt = VirtualTerminal::new(4, 3);
        vt.process("ab界\x1b[1;1H\x1b[1P".as_bytes());
        vt.resize(4, 8);
        assert_eq!(visible_text(&vt, 0), "b界");
    }

    #[test]
    fn a_cursor_after_a_printed_separator_keeps_its_insertion_point() {
        for text in ["ABCDE ", "ABCD ", "AB   "] {
            let mut vt = VirtualTerminal::new(4, 10);
            vt.process(text.as_bytes());
            vt.resize(4, 5);
            vt.process(b"X");
            assert_eq!(visible_text(&vt, 0), text.trim_end(), "{text:?}");
            assert_eq!(visible_text(&vt, 1), "X", "{text:?}");
        }
        let mut vt = VirtualTerminal::new(4, 10);
        vt.process(b"ABCDE ");
        vt.resize(4, 5);
        vt.resize(4, 10);
        vt.process(b"X");
        assert_eq!(visible_text(&vt, 0), "ABCDEX", "widened again");
    }

    #[test]
    fn a_cup_into_trailing_padding_stays_on_coordinates() {
        let mut vt = VirtualTerminal::new(4, 80);
        vt.process(b"abc\x1b[1;21H");
        vt.resize(4, 10);
        vt.process(b"X");
        assert_eq!(visible_text(&vt, 0), "abc      X");
        assert_eq!(visible_text(&vt, 1), "");
    }

    #[test]
    fn the_trim_floor_is_the_reflowed_history_boundary() {
        // Widen while shrinking: the history line unwraps to one row.
        let mut vt = VirtualTerminal::new(3, 5);
        vt.process(b"HHHHHHHHHH\r\nTOP\r\nmid\r\n\x1b[2;4H");
        assert_eq!(vt.scrollback_len(), 2);
        vt.resize(2, 10);
        assert_eq!(visible_text(&vt, 0), "TOP", "widen");
        assert_eq!(visible_text(&vt, 1), "mid", "widen");
    }

    #[test]
    fn the_trim_floor_follows_history_that_grows_on_a_narrow() {
        let mut vt = VirtualTerminal::new(3, 10);
        vt.process(b"HHHHHHHHHH\r\nTOP\r\nmid\r\n\x1b[2;4H");
        assert_eq!(vt.scrollback_len(), 1);
        vt.resize(3, 5);
        assert_eq!(visible_text(&vt, 0), "TOP", "narrow");
        assert_eq!(visible_text(&vt, 1), "mid", "narrow");
    }

    #[test]
    fn capped_history_is_evicted_not_pulled_into_view() {
        let config = GridConfig {
            max_scrollback: 1,
            ..GridConfig::default()
        };
        let mut vt = VirtualTerminal::with_config(3, 10, config);
        vt.process(b"HHHHHHHHHH\r\nTOP\r\nmid\r\n\x1b[2;4H");
        vt.resize(3, 5);
        assert_eq!(visible_text(&vt, 0), "TOP");
        assert_eq!(visible_text(&vt, 1), "mid");
    }

    #[test]
    fn a_line_left_with_only_gap_cells_survives_a_resize() {
        let mut vt = VirtualTerminal::new(2, 3);
        vt.process("ab界\x1b[T\x1b[2;1H\x1b[3P".as_bytes());
        vt.resize(2, 8);
        assert_eq!((vt.cursor().row, vt.cursor().col), (1, 0));
    }

    #[test]
    fn a_cup_one_past_the_new_edge_sets_no_deferred_wrap() {
        let mut vt = VirtualTerminal::new(4, 10);
        vt.process(b"abc\x1b[1;6H");
        vt.resize(4, 5);
        vt.process(b"F");
        assert_eq!(visible_text(&vt, 0), "abc F");
        assert_eq!(visible_text(&vt, 1), "");
    }

    #[test]
    fn ich_does_not_carry_stale_gap_marks_into_the_trailing_run() {
        let mut vt = VirtualTerminal::new(4, 4);
        vt.process("abc界\x1b[1;1H\x1b[2P\x1b[1;4HZ\x1b[1;2H\x1b[@".as_bytes());
        vt.resize(4, 8);
        assert_eq!(visible_text(&vt, 0), "c   界");
    }

    #[test]
    fn a_cursor_command_ends_print_intent_even_in_place() {
        for moves in [
            "\x1b[1;6H",
            "\x1b[C\x1b[D",
            "\x1b[6G",
            "\r\x1b[5C",
            "\x1b[1;6f",
        ] {
            let mut vt = VirtualTerminal::new(4, 10);
            vt.process(format!("ABCDE{moves}").as_bytes());
            assert_eq!(vt.cursor().col, 5, "{moves:?}");
            vt.resize(4, 5);
            vt.process(b"X");
            assert_eq!(visible_text(&vt, 0), "ABCDX", "{moves:?}");
            assert_eq!(visible_text(&vt, 1), "", "{moves:?}");
        }
    }

    #[test]
    fn a_saved_cursor_keeps_print_intent_across_reflow() {
        let mut vt = VirtualTerminal::new(4, 10);
        vt.process(b"ABCDE\x1b[?1049h");
        vt.resize(4, 5);
        vt.process(b"\x1b[?1049lF");
        assert_eq!(visible_text(&vt, 0), "ABCDE");
        assert_eq!(visible_text(&vt, 1), "F");
    }

    /// What `bash --norc` echoes and prints for the #301 smoke: the command
    /// line soft-wraps at 80 columns, then `clear` is `CSI H CSI J`.
    fn smoke_301(fill: usize) -> Vec<u8> {
        let echo = format!(
            "\x1b[?2004hbash-5.3$ clear; printf '\\033[48;2;30;60;160m%{fill}s\\033[0m\\n\
\\033[48;5;130m%{fill}s\\033[0m\\n\\033[42m%{fill}s\\033[0m\\n' '' '' ''; printf 'DONE%s\\n' -mark\r\n\
\x1b[?2004l\r\x1b[H\x1b[J"
        );
        let mut bytes = echo.into_bytes();
        for sgr in [&b"\x1b[48;2;30;60;160m"[..], b"\x1b[48;5;130m", b"\x1b[42m"] {
            bytes.extend_from_slice(sgr);
            bytes.extend(std::iter::repeat_n(b' ', fill));
            bytes.extend_from_slice(b"\x1b[0m\r\n");
        }
        bytes.extend_from_slice(b"DONE-mark\r\n\x1b[?2004hbash-5.3$ ");
        bytes
    }

    /// Row `r` shows `bg` on exactly its first `n` cells and the default after.
    fn assert_fill(vt: &VirtualTerminal, r: usize, bg: Color, n: usize, ctx: &str) {
        let row = vt.grid().visible_row(r);
        for c in 0..row.len() {
            let want = if c < n { bg } else { Color::Default };
            assert_eq!(row[c].style.bg, want, "{ctx} row {r} col {c}");
        }
    }

    #[test]
    fn a_clear_does_not_join_the_next_line_onto_a_wrapped_row() {
        let blue = Color::Rgb(30, 60, 160);
        let (brown, green) = (Color::Indexed(130), Color::Indexed(2));
        for (fill, steps) in [
            (40, &[(10, 40), (40, 120)][..]),
            (40, &[(10, 39)][..]),
            (40, &[(10, 41)][..]),
            (40, &[(24, 60)][..]),
            (60, &[(10, 40), (40, 120)][..]),
        ] {
            let mut vt = VirtualTerminal::new(24, 80);
            vt.process(&smoke_301(fill));
            // Padding clipped by a narrow is not regained by a later widen.
            let mut kept = fill;
            for &(rows, cols) in steps {
                vt.resize(rows, cols);
                kept = kept.min(cols);
                let ctx = format!("fill {fill} at {cols}x{rows}");
                for (r, bg) in [blue, brown, green].into_iter().enumerate() {
                    assert_fill(&vt, r, bg, kept, &ctx);
                }
                assert_eq!(
                    visible_text(&vt, 3),
                    "DONE-mark",
                    "fill {fill} at {cols}x{rows}"
                );
            }
        }
    }

    #[test]
    fn erasing_to_the_end_of_a_row_ends_its_soft_wrap() {
        for erase in [&b"\x1b[J"[..], b"\x1b[K", b"\x1b[2K"] {
            let mut vt = VirtualTerminal::new(4, 5);
            vt.process(b"ABCDEFG\x1b[H");
            assert!(vt.grid().visible_row(0).wrapped);
            vt.process(erase);
            assert!(!vt.grid().visible_row(0).wrapped, "{erase:?}");
        }
        let mut vt = VirtualTerminal::new(4, 5);
        vt.process(b"ABCDEFG\x1b[1;3H\x1b[1K");
        assert!(vt.grid().visible_row(0).wrapped, "EL 1 leaves the wrap");
        let mut vt = VirtualTerminal::new(4, 5);
        vt.process(b"ABCDEFG\x1b[1;2H\x1b[2X");
        assert!(
            vt.grid().visible_row(0).wrapped,
            "interior ECH leaves the wrap"
        );
    }

    #[test]
    fn a_suffix_erase_splits_the_line_for_a_widen() {
        let mut vt = VirtualTerminal::new(4, 5);
        vt.process(b"ABCDEFG\x1b[1;3H\x1b[K");
        vt.resize(4, 10);
        assert_eq!(visible_text(&vt, 0), "AB");
        assert_eq!(visible_text(&vt, 1), "FG");
    }

    #[test]
    fn an_erase_cancels_the_deferred_wrap_and_print_intent() {
        // xterm: ED 0, EL 0 and EL 2 all reset the deferred wrap; EL 0 at the
        // last column erases that column.
        let mut wrong = Vec::new();
        for (erase, row0) in [
            ("\x1b[J", "ABCDX"),
            ("\x1b[K", "ABCDX"),
            ("\x1b[2K", "    X"),
        ] {
            let mut vt = VirtualTerminal::new(4, 5);
            vt.process(format!("ABCDE{erase}X").as_bytes());
            let got = (visible_text(&vt, 0), visible_text(&vt, 1));
            if got != (row0.to_string(), String::new()) {
                wrong.push((erase, got));
            }
        }
        let mut vt = VirtualTerminal::new(4, 5);
        vt.process(b"ABCDE\x1b[J");
        vt.resize(4, 4);
        vt.process(b"X");
        let got = (visible_text(&vt, 0), visible_text(&vt, 1));
        if got != ("ABCX".to_string(), String::new()) {
            wrong.push(("ED 0 then 5->4", got));
        }
        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// History row 0 and visible row 0 after `ABCDEF CRLF`, `seq` and a widen.
    fn after_history_edit(seq: &str) -> (String, String) {
        let mut vt = VirtualTerminal::new(2, 5);
        vt.process(format!("ABCDEF\r\n{seq}").as_bytes());
        vt.resize(2, 10);
        (row_text(&vt, 0), visible_text(&vt, 0))
    }

    #[test]
    fn a_full_erase_of_the_first_visible_row_cuts_the_link_from_history() {
        let mut wrong = Vec::new();
        for seq in [
            "\x1b[H\x1b[JXYZ",
            "\x1b[H\x1b[KXYZ",
            "\x1b[H\x1b[2KXYZ",
            "\x1b[2J\x1b[HXYZ",
            "\x1b[2;1H\x1b[1J\x1b[HXYZ",
        ] {
            let got = after_history_edit(seq);
            if got != ("ABCDE".to_string(), "XYZ".to_string()) {
                wrong.push((seq, got));
            }
        }
        let got = after_history_edit("\x1b#8");
        if got.0 != "ABCDE" {
            wrong.push(("DECALN", got));
        }
        assert!(wrong.is_empty(), "{wrong:?}");
        // A suffix erase leaves the continuation's prefix: the link holds.
        let mut vt = VirtualTerminal::new(2, 5);
        vt.process(b"ABCDEFG\r\n\x1b[1;2H\x1b[K");
        vt.resize(2, 10);
        assert_eq!(visible_text(&vt, 0), "ABCDEF");
    }

    #[test]
    fn an_erase_marks_only_what_it_erased_dirty_and_counts_once() {
        let mut vt = VirtualTerminal::new(2, 8);
        vt.process(b"ABCDEFGH\x1b[1;5H");
        vt.grid.take_dirty_regions();
        let before = vt.grid().mutations();
        vt.process(b"\x1b[K");
        assert_eq!(vt.grid().mutations(), before + 1);
        let dirty = vt.grid.take_dirty_regions();
        assert_eq!(dirty, vec![DirtyRegion { row: 0, cols: 4..8 }]);
    }

    #[test]
    fn line_edits_cut_wrap_links_whose_rows_part() {
        let cases: [(&str, &[u8], &[&str]); 5] = [
            // IL between a wrapped row and its continuation.
            ("IL", b"ABCDEFG\x1b[2;1H\x1b[L", &["ABCDE", "", "FG"]),
            // DL of the continuation.
            ("DL", b"ABCDEFG\r\nHI\x1b[2;1H\x1b[M", &["ABCDE", "HI"]),
            // SD inside a region below the wrapped row.
            ("SD", b"ABCDEFG\x1b[2;4r\x1b[T", &["ABCDE", "", "FG"]),
            // Rows scrolling together inside a region keep their link.
            (
                "region scroll",
                b"\x1b[2;4r\x1b[3;1HABCDEFG\n",
                &["", "ABCDEFG"],
            ),
            // Autowrap at a region's bottom margin links into the new row.
            (
                "bottom-margin autowrap",
                b"\x1b[1;2r\x1b[2;1HABCDEFG",
                &["ABCDEFG"],
            ),
        ];
        let mut wrong = Vec::new();
        for (name, bytes, want) in cases {
            let mut vt = VirtualTerminal::new(4, 5);
            vt.process(bytes);
            vt.resize(4, 10);
            let got: Vec<_> = (0..want.len()).map(|r| visible_text(&vt, r)).collect();
            if got != want {
                wrong.push((name, got));
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
    }

    #[test]
    fn a_wrap_with_nowhere_to_go_sets_no_link() {
        // Cursor below a scroll region on the last row: autowrap cannot advance.
        let mut vt = VirtualTerminal::new(3, 5);
        vt.process(b"\x1b[1;2r\x1b[3;1HABCDEFG");
        assert!(!vt.grid().visible_row(2).wrapped);
    }

    #[test]
    fn a_one_row_screen_keeps_the_autowrap_link_into_history() {
        let mut vt = VirtualTerminal::new(1, 5);
        vt.process(b"ABCDEF");
        vt.resize(1, 10);
        assert_eq!(visible_text(&vt, 0), "ABCDEF");
    }

    #[test]
    fn any_erase_that_clears_the_first_visible_row_cuts_the_history_link() {
        let mut wrong = Vec::new();
        for seq in [
            "\x1b[1;5H\x1b[1J\x1b[HXYZ",
            "\x1b[1;5H\x1b[1K\x1b[HXYZ",
            "\x1b[H\x1b[5XXYZ",
        ] {
            let got = after_history_edit(seq);
            if got != ("ABCDE".to_string(), "XYZ".to_string()) {
                wrong.push((seq, got));
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
    }

    #[test]
    fn an_erase_widened_over_a_wide_glyph_to_the_whole_row_cuts_the_history_link() {
        let mut wrong = Vec::new();
        for bytes in [
            // EL 0 from the glyph's second cell widens to column 0.
            "ABCDE界\r\n\x1b[1;2H\x1b[K\x1b[HXYZ",
            // EL 1 and ECH ending on the glyph's first cell widen to the edge.
            "ABCDEabc界\r\n\x1b[1;4H\x1b[1K\x1b[HXYZ",
            "ABCDEabc界\r\n\x1b[H\x1b[4XXYZ",
        ] {
            let mut vt = VirtualTerminal::new(2, 5);
            vt.process(bytes.as_bytes());
            vt.resize(2, 10);
            let got = (row_text(&vt, 0), visible_text(&vt, 0));
            if got != ("ABCDE".to_string(), "XYZ".to_string()) {
                wrong.push((bytes, got));
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
    }

    #[test]
    fn row_shrink_without_a_parked_cursor_lets_erase_fill_yield() {
        let mut vt = VirtualTerminal::new(6, 10);
        vt.process(b"\x1b[44m\x1b[2J\x1b[0mAB\x1b7\x1b[?1047h");
        vt.resize(3, 10);
        vt.process(b"\x1b[?1047l");
        assert_eq!(visible_text(&vt, 0), "AB");
        assert_eq!(vt.scrollback_len(), 0);
    }

    #[test]
    fn row_shrink_without_a_parked_cursor_keeps_linked_blanks() {
        let mut vt = VirtualTerminal::new(2, 10);
        vt.process(b"A\x1b7\r\n\x1b]8;;https://x.test\x1b\\ \x1b]8;;\x1b\\\x1b[?1047h");
        vt.resize(1, 10);
        vt.process(b"\x1b[?1047l");
        let grid = vt.grid();
        let linked = (0..grid.total_lines())
            .filter_map(|r| grid.row(r))
            .any(|row| (0..row.len()).any(|c| row[c].extended.is_some()));
        assert!(linked, "the linked blank was dropped");
    }

    fn assert_grid_wide_invariants(grid: &Grid) {
        for row_idx in 0..grid.total_lines() {
            let row = grid.row(row_idx).expect("row exists");
            for col in 0..row.len() {
                let cell = &row[col];
                if cell.is_wide_continuation() {
                    assert_eq!(
                        cell.ch, ' ',
                        "continuation at row {row_idx} col {col} carries glyph"
                    );
                    assert!(col > 0, "orphan continuation at row {row_idx} col 0");
                    assert!(
                        row[col - 1].is_wide(),
                        "orphan continuation at row {row_idx} col {col}"
                    );
                }
                if cell.is_wide() {
                    assert!(
                        col + 1 < row.len(),
                        "wide head at row {row_idx} final col {col}"
                    );
                    assert!(
                        row[col + 1].is_wide_continuation(),
                        "wide head at row {row_idx} col {col} missing tail"
                    );
                }
            }
        }
    }

    #[test]
    fn capture_text_skips_trailing_blank_rows_below_content() {
        // Regression: pane.capture {lines:N} used to take the literal
        // bottom-N visible rows, returning blank output when the cursor
        // sat near the top of a 24-row viewport. capture_text now walks
        // back from the LAST non-blank row.
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"first line\r\nsecond line\r\n");
        // Cursor is now on row 2; rows 3..23 are blank.
        let text = vt.capture_text(Some(1));
        assert_eq!(text.trim_end(), "second line");

        let text = vt.capture_text(Some(5));
        // Should pick up the two content lines, not blanks 19..23.
        assert!(text.contains("first line"), "first line missing: {text:?}");
        assert!(
            text.contains("second line"),
            "second line missing: {text:?}"
        );
    }

    #[test]
    fn capture_text_empty_viewport_returns_single_newline() {
        let vt = VirtualTerminal::new(24, 80);
        // Fresh VT has only blank rows.
        let text = vt.capture_text(Some(5));
        assert_eq!(
            text, "\n",
            "expected single newline for blank pane, got {text:?}"
        );
    }

    #[test]
    fn test_process_plain_text() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"Hello, world!");
        assert_eq!(vt.grid().visible_row(0)[0].ch, 'H');
        assert_eq!(vt.grid().visible_row(0)[4].ch, 'o');
        assert_eq!(vt.cursor().col, 13);
    }

    #[test]
    fn test_process_newline() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"line1\r\nline2");
        assert_eq!(vt.grid().visible_row(0)[0].ch, 'l');
        assert_eq!(vt.grid().visible_row(1)[0].ch, 'l');
        assert_eq!(vt.cursor().row, 1);
    }

    #[test]
    fn test_cursor_movement() {
        let mut vt = VirtualTerminal::new(24, 80);
        // CSI 5;10H -- move cursor to row 5, col 10.
        vt.process(b"\x1b[5;10H");
        assert_eq!(vt.cursor().row, 4); // 0-indexed
        assert_eq!(vt.cursor().col, 9); // 0-indexed
    }

    /// A `RIS` earlier in the same read must not swallow an APC starting after
    /// it. The scanner consumes a whole read before vte sees any of it, so a
    /// reset at `ESC c` could only discard state for bytes AFTER the reset --
    /// an APC cannot span a `RIS`, since the introducing ESC terminates the
    /// string sequence first.
    #[test]
    fn a_ris_does_not_swallow_an_apc_that_starts_after_it_in_the_same_read() {
        let mut vt = VirtualTerminal::new(24, 80);
        // RIS, then the opening half of a graphics command, in ONE read.
        vt.process_with_responses(b"\x1bc\x1b_Ghalf");
        // Its continuation and terminator arrive in the next read.
        vt.process_with_responses(b"more\x1b\\");
        assert_eq!(
            vt.graphics.dispatched,
            vec![b"halfmore".to_vec()],
            "the APC opened after the RIS was dropped"
        );
    }

    /// The scanner and vte must agree on what ends an APC string, and they do:
    /// only `ESC \\` does. Neither the 8-bit ST `0x9C` nor `BEL` terminates one.
    ///
    /// Pinned rather than "fixed". Teaching the scanner to end a string at
    /// `0x9C` would CREATE a divergence: it would cut where vte keeps consuming,
    /// so the bytes after it would reach `dispatch_graphics` as a command while
    /// vte still treated them as string content. The 7-bit arm is a positive
    /// control -- without it a scanner that dispatched nothing at all would
    /// satisfy every other row here.
    #[test]
    fn only_the_7bit_st_terminates_an_apc_and_vte_agrees() {
        let observe = |bytes: &[u8]| {
            let mut vt = VirtualTerminal::new(24, 80);
            vt.process_with_responses(bytes);
            let text = vt.capture_text(None);
            (
                vt.graphics.dispatched.clone(),
                text.contains("AFTER"),
                text.contains("PAYLOAD"),
            )
        };

        // Control: the 7-bit ST both delivers the command and lets text resume.
        assert_eq!(
            observe(b"\x1b_Ga=T;PAYLOAD\x1b\\AFTER"),
            (vec![b"a=T;PAYLOAD".to_vec()], true, false),
        );

        // `0x9C` and BEL end nothing. Text does NOT resume after either, which
        // is what proves vte is still inside the string rather than past it --
        // "the payload never reached the grid" alone cannot tell those apart.
        for unterminated in [
            b"\x1b_Ga=T;PAYLOAD\x9cAFTER".as_slice(),
            b"\x1b_Ga=T;PAYLOAD\x07AFTER",
        ] {
            assert_eq!(observe(unterminated), (Vec::new(), false, false));
        }
    }

    #[test]
    fn process_with_responses_answers_da_and_dsr_queries() {
        let mut vt = VirtualTerminal::new(24, 80);
        let responses = vt.process_with_responses(b"\x1b[5;10H\x1b[6n\x1b[5n\x1b[c\x1b[>c");

        assert_eq!(
            responses,
            vec![
                b"\x1b[5;10R".to_vec(),
                b"\x1b[0n".to_vec(),
                b"\x1b[?62;1;2;6;9;15;22c".to_vec(),
                b"\x1b[>0;95;0c".to_vec(),
            ]
        );
    }

    #[test]
    fn process_with_responses_answers_private_dsr() {
        let mut vt = VirtualTerminal::new(24, 80);
        let responses = vt.process_with_responses(b"\x1b[2;3H\x1b[?6n\x1b[?15n\x1b[?25n");

        assert_eq!(
            responses,
            vec![
                b"\x1b[?2;3R".to_vec(),
                b"\x1b[?10n".to_vec(),
                b"\x1b[?20n".to_vec(),
            ]
        );
    }

    #[test]
    fn process_with_responses_reports_origin_relative_cursor_position() {
        let mut vt = VirtualTerminal::new(10, 20);
        let responses = vt.process_with_responses(b"\x1b[3;6r\x1b[?6h\x1b[2;4H\x1b[6n\x1b[?6n");

        assert_eq!(
            responses,
            vec![b"\x1b[2;4R".to_vec(), b"\x1b[?2;4R".to_vec()]
        );
        assert_eq!((vt.cursor().row, vt.cursor().col), (3, 3));
    }

    #[test]
    fn process_with_responses_answers_osc_color_queries() {
        let mut vt = VirtualTerminal::new(24, 80);
        let responses = vt.process_with_responses(
            b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b]12;?\x1b\\\x1b]4;1;?\x1b\\",
        );

        assert_eq!(
            responses,
            vec![
                b"\x1b]10;rgb:eeee/eeee/eeee\x1b\\".to_vec(),
                b"\x1b]11;rgb:0000/0000/0000\x1b\\".to_vec(),
                b"\x1b]12;rgb:eeee/eeee/eeee\x1b\\".to_vec(),
                b"\x1b]4;1;rgb:cdcd/0000/0000\x1b\\".to_vec(),
            ]
        );
    }

    #[test]
    fn osc_12_query_falls_back_to_dynamic_foreground() {
        let mut vt = VirtualTerminal::new(24, 80);
        let responses = vt.process_with_responses(b"\x1b]10;#ff0000\x1b\\\x1b]12;?\x1b\\");

        assert_eq!(
            responses,
            vec![b"\x1b]12;rgb:ffff/0000/0000\x1b\\".to_vec()]
        );
    }

    #[test]
    fn process_with_responses_preserves_osc_bel_query_terminator() {
        let mut vt = VirtualTerminal::new(24, 80);
        let responses = vt.process_with_responses(b"\x1b]10;?\x07\x1b]4;2;?\x07");

        assert_eq!(
            responses,
            vec![
                b"\x1b]10;rgb:eeee/eeee/eeee\x07".to_vec(),
                b"\x1b]4;2;rgb:0000/cdcd/0000\x07".to_vec(),
            ]
        );
    }

    #[test]
    fn process_with_responses_uses_dynamic_default_colors_in_osc_queries() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b]10;#123456\x1b\\\x1b]11;rgb:1/2/3\x1b\\\x1b]12;#00ff80\x1b\\");
        let responses = vt.process_with_responses(b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b]12;?\x1b\\");
        vt.process(b"\x1b]110\x1b\\\x1b]111\x1b\\\x1b]112\x1b\\");
        let reset_responses =
            vt.process_with_responses(b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b]12;?\x1b\\");

        assert_eq!(
            responses,
            vec![
                b"\x1b]10;rgb:1212/3434/5656\x1b\\".to_vec(),
                b"\x1b]11;rgb:1111/2222/3333\x1b\\".to_vec(),
                b"\x1b]12;rgb:0000/ffff/8080\x1b\\".to_vec(),
            ]
        );
        assert_eq!(
            reset_responses,
            vec![
                b"\x1b]10;rgb:eeee/eeee/eeee\x1b\\".to_vec(),
                b"\x1b]11;rgb:0000/0000/0000\x1b\\".to_vec(),
                b"\x1b]12;rgb:eeee/eeee/eeee\x1b\\".to_vec(),
            ]
        );
    }

    #[test]
    fn process_with_responses_answers_extended_osc_palette_queries() {
        let mut vt = VirtualTerminal::new(24, 80);
        let responses = vt.process_with_responses(b"\x1b]4;16;?;231;?;232;?;255;?\x1b\\");

        assert_eq!(
            responses,
            vec![
                b"\x1b]4;16;rgb:0000/0000/0000\x1b\\".to_vec(),
                b"\x1b]4;231;rgb:ffff/ffff/ffff\x1b\\".to_vec(),
                b"\x1b]4;232;rgb:0808/0808/0808\x1b\\".to_vec(),
                b"\x1b]4;255;rgb:eeee/eeee/eeee\x1b\\".to_vec(),
            ]
        );
    }

    #[test]
    fn process_with_responses_answers_xtgettcap() {
        let mut vt = VirtualTerminal::new(24, 80);
        // Hex for TN;Co;RGB.
        let responses = vt.process_with_responses(b"\x1bP+q544e;436f;524742\x1b\\");

        assert_eq!(
            responses,
            vec![
                b"\x1bP1+r544e3d787465726d2d323536636f6c6f72;436f3d323536;5247423d\x1b\\".to_vec()
            ]
        );
    }

    #[test]
    fn process_with_responses_answers_extended_xtgettcap() {
        let mut vt = VirtualTerminal::new(24, 80);
        // Hex for AX;Ms;Ss;Se;smcup;rmcup;smkx;rmkx;setaf;setab;setrgbf;setrgbb.
        let responses = vt.process_with_responses(
            b"\x1bP+q4158;4d73;5373;5365;736d637570;726d637570;736d6b78;726d6b78;7365746166;7365746162;73657472676266;73657472676262\x1b\\",
        );

        let response = String::from_utf8(responses.into_iter().next().unwrap()).unwrap();
        assert!(response.starts_with("\x1bP1+r"));
        assert!(response.contains("41583d")); // AX=
        assert!(response.contains("4d733d1b5d35323b25703125733b257032257307"));
        assert!(response.contains("53733d1b5b25703125642071"));
        assert!(response.contains("53653d1b5b2071"));
        assert!(response.contains("736d6375703d1b5b3f3130343968"));
        assert!(response.contains("726d6375703d1b5b3f313034396c"));
        assert!(response.contains("736d6b783d1b5b3f31681b3d"));
        assert!(response.contains("726d6b783d1b5b3f316c1b3e"));
        assert!(response.contains("73657461663d"));
        assert!(response.contains("73657461623d"));
        assert!(response.contains("736574726762663d1b5b33383b323b"));
        assert!(response.contains("736574726762623d1b5b34383b323b"));
    }

    #[test]
    fn process_with_responses_reports_failed_dcs_queries() {
        let mut vt = VirtualTerminal::new(24, 80);
        let responses = vt.process_with_responses(
            b"\x1bP+q556e6b6e6f776e\x1b\\\x1bP$qunknown\x1b\\\x1bP!qignored\x1b\\",
        );

        assert_eq!(
            responses,
            vec![b"\x1bP0+r\x1b\\".to_vec(), b"\x1bP0$r\x1b\\".to_vec()]
        );
    }

    #[test]
    fn process_with_responses_answers_decrqm_for_modes() {
        let mut vt = VirtualTerminal::new(24, 80);
        let initial = vt.process_with_responses(
            b"\x1b[4$p\x1b[20$p\x1b[?1$p\x1b[?6$p\x1b[?7$p\x1b[?25$p\x1b[?66$p\x1b[?1000$p\x1b[?1002$p\x1b[?1003$p\x1b[?1004$p\x1b[?1006$p\x1b[?1007$p\x1b[?1049$p\x1b[?2004$p\x1b[?2026$p\x1b[?9999$p",
        );

        assert_eq!(
            initial,
            vec![
                b"\x1b[4;2$y".to_vec(),
                b"\x1b[20;2$y".to_vec(),
                b"\x1b[?1;2$y".to_vec(),
                b"\x1b[?6;2$y".to_vec(),
                b"\x1b[?7;1$y".to_vec(),
                b"\x1b[?25;1$y".to_vec(),
                b"\x1b[?66;2$y".to_vec(),
                b"\x1b[?1000;2$y".to_vec(),
                b"\x1b[?1002;2$y".to_vec(),
                b"\x1b[?1003;2$y".to_vec(),
                b"\x1b[?1004;2$y".to_vec(),
                b"\x1b[?1006;2$y".to_vec(),
                // ?1007 (alternate scroll) defaults ON → reports set (;1).
                b"\x1b[?1007;1$y".to_vec(),
                b"\x1b[?1049;2$y".to_vec(),
                b"\x1b[?2004;2$y".to_vec(),
                b"\x1b[?2026;2$y".to_vec(),
                b"\x1b[?9999;0$y".to_vec(),
            ]
        );

        vt.process(
            b"\x1b[4h\x1b[20h\x1b[?1h\x1b[?6h\x1b[?25l\x1b=\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1004h\x1b[?1006h\x1b[?1049h\x1b[?2004h\x1b[?2026h\x1b[?2026h",
        );
        let enabled = vt.process_with_responses(
            b"\x1b[4$p\x1b[20$p\x1b[?1$p\x1b[?6$p\x1b[?25$p\x1b[?66$p\x1b[?1000$p\x1b[?1002$p\x1b[?1003$p\x1b[?1004$p\x1b[?1006$p\x1b[?1049$p\x1b[?2004$p\x1b[?2026$p",
        );

        assert_eq!(
            enabled,
            vec![
                b"\x1b[4;1$y".to_vec(),
                b"\x1b[20;1$y".to_vec(),
                b"\x1b[?1;1$y".to_vec(),
                b"\x1b[?6;1$y".to_vec(),
                b"\x1b[?25;2$y".to_vec(),
                b"\x1b[?66;1$y".to_vec(),
                b"\x1b[?1000;2$y".to_vec(),
                b"\x1b[?1002;2$y".to_vec(),
                b"\x1b[?1003;1$y".to_vec(),
                b"\x1b[?1004;1$y".to_vec(),
                b"\x1b[?1006;1$y".to_vec(),
                b"\x1b[?1049;1$y".to_vec(),
                b"\x1b[?2004;1$y".to_vec(),
                b"\x1b[?2026;1$y".to_vec(),
            ]
        );
    }

    #[test]
    fn process_with_responses_answers_xtversion() {
        let mut vt = VirtualTerminal::new(24, 80);
        let responses = vt.process_with_responses(b"\x1b[>0q");

        assert_eq!(
            responses,
            vec![format!("\x1bP>|shux {}\x1b\\", env!("CARGO_PKG_VERSION")).into_bytes()]
        );
    }

    #[test]
    fn synchronized_output_freezes_presented_frame_until_reset() {
        let mut vt = VirtualTerminal::new(3, 10);
        vt.process(b"old");

        vt.process(b"\x1b[?2026h\x1b[1;1Hnew");

        assert!(vt.modes().synchronized_output);
        assert_eq!(vt.grid().visible_row(0)[0].ch, 'o');
        assert_eq!(vt.grid().visible_row(0)[1].ch, 'l');
        assert_eq!(vt.grid().visible_row(0)[2].ch, 'd');
        assert_eq!(vt.capture_text(Some(1)).trim_end(), "old");

        vt.process(b"\x1b[?2026l");

        assert!(!vt.modes().synchronized_output);
        assert_eq!(vt.grid().visible_row(0)[0].ch, 'n');
        assert_eq!(vt.grid().visible_row(0)[1].ch, 'e');
        assert_eq!(vt.grid().visible_row(0)[2].ch, 'w');
        assert_eq!(vt.capture_text(Some(1)).trim_end(), "new");
    }

    #[test]
    fn synchronized_output_freezes_presented_colors_and_title() {
        let mut vt = VirtualTerminal::new(3, 10);
        vt.process(b"\x1b]10;#112233\x1b\\\x1b]11;#000000\x1b\\\x1b]12;#eeeeee\x1b\\");
        vt.process(b"\x1b]2;stable\x1b\\old");

        vt.process(
            b"\x1b[?2026h\x1b]10;#ff0000\x1b\\\x1b]11;#00ff00\x1b\\\
              \x1b]12;#0000ff\x1b\\\x1b]2;pending\x1b\\\x1b[1;1Hnew",
        );

        assert!(vt.modes().synchronized_output);
        assert_eq!(
            vt.default_colors(),
            TerminalDefaultColors {
                fg: Some([0x11, 0x22, 0x33]),
                bg: Some([0x00, 0x00, 0x00]),
                cursor: Some([0xee, 0xee, 0xee]),
            }
        );
        assert_eq!(vt.title(), Some("stable"));
        assert_eq!(vt.capture_text(Some(1)).trim_end(), "old");

        vt.process(b"\x1b[?2026l");

        assert!(!vt.modes().synchronized_output);
        assert_eq!(
            vt.default_colors(),
            TerminalDefaultColors {
                fg: Some([0xff, 0x00, 0x00]),
                bg: Some([0x00, 0xff, 0x00]),
                cursor: Some([0x00, 0x00, 0xff]),
            }
        );
        assert_eq!(vt.title(), Some("pending"));
        assert_eq!(vt.capture_text(Some(1)).trim_end(), "new");
    }

    /// History stays reachable while a window is open.
    ///
    /// Since issue #115 the frozen frame is the VIEWPORT alone — history is
    /// not part of the frame and is read live, through `presented_row`. This
    /// is the property that matters (copy mode must still work while `btop`
    /// redraws), and it is the one a viewport-only snapshot with no
    /// indirection would have broken.
    #[test]
    fn synchronized_output_keeps_presented_scrollback_reachable() {
        let mut vt = VirtualTerminal::new(2, 10);
        vt.process(b"first\r\nsecond\r\nthird");
        let presented_total = vt.presented_total_lines();

        vt.process(b"\x1b[?2026h\x1b[1;1Hpending\r\nwork  ");

        assert!(vt.modes().synchronized_output);
        assert_eq!(vt.presented_total_lines(), presented_total);
        let oldest: String = vt
            .presented_row(0)
            .expect("the oldest retained line must still be reachable")
            .cells
            .iter()
            .map(|cell| cell.ch)
            .collect();
        assert!(
            oldest.contains("first"),
            "oldest line read back as {oldest:?}"
        );
        // Every line the presented coordinate space claims must resolve.
        for line in 0..vt.presented_total_lines() {
            assert!(
                vt.presented_row(line).is_some(),
                "presented line {line} did not resolve"
            );
        }
        assert_eq!(vt.capture_text(Some(1)).trim_end(), "third");

        vt.process(b"\x1b[?2026l");

        assert!(!vt.modes().synchronized_output);
        assert_eq!(vt.capture_text(Some(1)).trim_end(), "work");
    }

    /// A resize RELEASES an open window (issue #115): the frame the pane asked
    /// shux to hold still was drawn for a geometry that no longer exists, and
    /// `SIGWINCH` means a repaint is on its way regardless.
    #[test]
    fn synchronized_output_resize_releases_the_window() {
        let mut vt = VirtualTerminal::new(3, 10);
        vt.process(b"stable\x1b[?2026h\x1b[1;1Hpending");
        assert_eq!(
            vt.grid().visible_row(0)[0].ch,
            's',
            "frozen before the resize"
        );

        vt.resize(5, 12);

        assert!(
            !vt.modes().synchronized_output,
            "the resize must release it"
        );
        assert!(!vt.sync_output_active());
        assert_eq!(vt.grid().rows(), 5);
        assert_eq!(vt.grid().cols(), 12);
        assert_eq!(
            vt.grid().visible_row(0)[0].ch,
            'p',
            "live frame is presented"
        );

        // And a later `?2026l` from the pane, which now arrives with no window
        // open, is simply inert.
        vt.process(b"\x1b[?2026l");
        assert_eq!(vt.grid().rows(), 5);
        assert_eq!(vt.grid().cols(), 12);
        assert_eq!(vt.grid().visible_row(0)[0].ch, 'p');
    }

    /// The frame presented after a resize is indistinguishable from a terminal
    /// that never opened a window — reflow included. Before, the frozen copy
    /// was reflowed on its own, which on a widening resize could not rewrap
    /// against history and on the alternate screen rewrapped a canvas that is
    /// never reflowed at all.
    #[test]
    fn synchronized_output_resize_presents_what_an_unsynced_terminal_would() {
        let script = b"ABCDEFGHIJK\x1b[?2026h\x1b[1;1Hpending";
        let mut reference = VirtualTerminal::new(4, 5);
        reference.process(b"ABCDEFGHIJK\x1b[1;1Hpending");
        reference.resize(5, 4);

        let mut vt = VirtualTerminal::new(4, 5);
        vt.process(script);
        assert_eq!(
            compact_capture(&vt),
            "ABCDEFGHIJK",
            "frozen before the resize"
        );

        vt.resize(5, 4);

        assert!(!vt.modes().synchronized_output);
        assert_eq!(compact_capture(&vt), compact_capture(&reference));
        assert!(compact_capture(&vt).starts_with("pending"));
    }

    #[test]
    fn synchronized_output_freezes_origin_mode_presented_frame() {
        let mut vt = VirtualTerminal::new(8, 20);
        vt.process(b"\x1b[3;6r\x1b[?6h\x1b[1;1HX");

        vt.process(b"\x1b[?2026h\x1b[1;1HY");

        assert!(vt.modes().synchronized_output);
        assert_eq!(vt.grid().visible_row(2)[0].ch, 'X');
        assert_eq!(vt.cursor().row, 2);
        assert_eq!(vt.cursor().col, 1);

        vt.process(b"\x1b[?2026l");

        assert_eq!(vt.grid().visible_row(2)[0].ch, 'Y');
        assert_eq!(vt.cursor().row, 2);
        assert_eq!(vt.cursor().col, 1);
    }

    // -----------------------------------------------------------------
    // Issue #115: the freeze is taken by the first write, not by `?2026h`
    // -----------------------------------------------------------------

    /// The presented frame must not move while a window is open, whether or
    /// not a copy has been taken yet — which is the whole claim the deferral
    /// rests on.
    #[test]
    fn sync_window_that_writes_nothing_still_presents_the_same_frame() {
        let mut vt = VirtualTerminal::new(3, 10);
        vt.process(b"before");
        let frame = compact_capture(&vt);

        vt.process(b"\x1b[?2026h");
        assert_eq!(compact_capture(&vt), frame);
        // Sequences that parse but change nothing presented must not disturb
        // it either, and must not cause a copy to be taken.
        vt.process(b"\x1b[6n\x1b[?1000h\x1b[?1000l\x1b[?9999h");
        assert_eq!(compact_capture(&vt), frame);

        vt.process(b"\x1b[?2026l");
        assert_eq!(compact_capture(&vt), frame);
    }

    /// The copy has to be of the frame as it stood at `?2026h`, not as it
    /// stands when the copy is finally taken. Several writes in a row inside
    /// one window must all be hidden, not just the ones after the first.
    #[test]
    fn every_write_in_a_sync_window_is_hidden_not_just_the_first() {
        let mut vt = VirtualTerminal::new(3, 10);
        vt.process(b"original");

        vt.process(b"\x1b[?2026h");
        for text in [
            "\x1b[1;1Hfirst   ",
            "\x1b[1;1Hsecond  ",
            "\x1b[1;1Hthird   ",
        ] {
            vt.process(text.as_bytes());
            assert_eq!(
                compact_capture(&vt),
                "original",
                "the presented frame moved mid-window"
            );
        }

        vt.process(b"\x1b[?2026l");
        assert_eq!(compact_capture(&vt), "third");
    }

    /// Splitting the sequence across two `process` calls is not exotic: a PTY
    /// read boundary can fall anywhere. Both the open and the close must
    /// survive it.
    #[test]
    fn sync_mode_survives_an_escape_split_across_reads() {
        let mut vt = VirtualTerminal::new(3, 10);
        vt.process(b"before");

        vt.process(b"\x1b[?20");
        vt.process(b"26h");
        assert!(vt.modes().synchronized_output);
        vt.process(b"\x1b[1;1Hafter   ");
        assert_eq!(compact_capture(&vt), "before");

        vt.process(b"\x1b[?2026");
        vt.process(b"l");
        assert!(!vt.modes().synchronized_output);
        assert_eq!(compact_capture(&vt), "after");
    }

    /// The mode arrives in a parameter LIST as often as alone — `?1049;2026h`
    /// is one sequence a full-screen application can reasonably emit. Arming
    /// on an exact byte match would miss it, and freezing on the sequence
    /// rather than on the write would freeze the wrong frame.
    #[test]
    fn sync_mode_arms_and_releases_from_a_combined_parameter_list() {
        let mut vt = VirtualTerminal::new(3, 10);
        vt.process(b"primary");

        // Alt screen first, then sync: the frozen frame is the alt screen.
        vt.process(b"\x1b[?1049;2026h");
        assert!(vt.modes().synchronized_output);
        assert!(vt.is_alternate_screen());
        vt.process(b"\x1b[1;1Halt     ");
        assert_eq!(compact_capture(&vt), "");

        // And the release also comes out of a list.
        vt.process(b"\x1b[?2026;1l");
        assert!(!vt.modes().synchronized_output);
        assert_eq!(compact_capture(&vt), "alt");
    }

    /// The other order: sync first, then the alt-screen switch happens INSIDE
    /// the window. The presented frame — pixels and the alt-screen flag alike
    /// — must stay on the primary screen until release.
    #[test]
    fn an_alt_screen_switch_inside_a_sync_window_stays_hidden() {
        let mut vt = VirtualTerminal::new(3, 10);
        vt.process(b"primary");

        vt.process(b"\x1b[?2026;1049h");
        assert!(
            !vt.is_alternate_screen(),
            "presented alt flag must stay on the frozen frame"
        );
        assert_eq!(compact_capture(&vt), "primary");
        vt.process(b"\x1b[1;1Halt     ");
        assert_eq!(compact_capture(&vt), "primary");

        vt.process(b"\x1b[?2026l");
        assert!(vt.is_alternate_screen());
        assert_eq!(compact_capture(&vt), "alt");
    }

    /// A window title set inside a window must stay hidden even when the pane
    /// had NO title at freeze time. The frozen slot has to win over the live
    /// one on its own account rather than only when it holds a value —
    /// otherwise "no title yet" reads as "not frozen" and the new title leaks
    /// straight through.
    #[test]
    fn a_title_set_inside_a_sync_window_does_not_leak_when_there_was_none() {
        let mut vt = VirtualTerminal::new(3, 10);
        assert_eq!(vt.title(), None);

        vt.process(b"\x1b[?2026h\x1b]2;pending\x07");
        assert_eq!(
            vt.title(),
            None,
            "a pane with no title at freeze time still has none until release"
        );

        vt.process(b"\x1b[?2026l");
        assert_eq!(vt.title(), Some("pending"));
    }

    /// Replacing a title inside a window is the same rule in the other
    /// direction. (`OSC 2` with an empty payload sets an empty title rather
    /// than removing one — that is shux's existing behaviour and not what
    /// this test is about; what matters is that the change stays hidden.)
    #[test]
    fn a_title_replaced_inside_a_sync_window_does_not_leak() {
        let mut vt = VirtualTerminal::new(3, 10);
        vt.process(b"\x1b]2;stable\x07");

        vt.process(b"\x1b[?2026h\x1b]2;\x07");
        assert_eq!(vt.title(), Some("stable"));

        vt.process(b"\x1b[?2026l");
        assert_eq!(vt.title(), Some(""));
    }

    /// RIS inside a window resets everything, including the mode itself.
    #[test]
    fn full_reset_inside_a_sync_window_releases_it() {
        let mut vt = VirtualTerminal::new(3, 10);
        vt.process(b"before");

        vt.process(b"\x1b[?2026h\x1b[1;1Hafter   \x1bc");

        assert!(!vt.modes().synchronized_output);
        assert!(!vt.sync_output_active());
        assert_eq!(compact_capture(&vt), "");
        vt.process(b"live");
        assert_eq!(compact_capture(&vt), "live");
    }

    // -----------------------------------------------------------------
    // Issue #115: a window cannot be held open for ever
    // -----------------------------------------------------------------

    /// A hidden cursor's streak starts at the batch that hid it. A show between
    /// two hides, even inside one batch, and a swap back to a hidden primary
    /// cursor start it again; a synchronized frame keeps the stamp it froze.
    #[test]
    fn a_hidden_cursor_is_stamped_by_the_batch_that_hid_it() {
        let clock = test_clock::Frozen::new();
        let t0 = sync_clock_ns();
        let ms = |n: u64| t0 + n * 1_000_000;
        let mut vt = VirtualTerminal::new(3, 10);
        assert_eq!(vt.cursor().hidden_for_ns(t0), None);
        vt.process(b"\x1b[?25l");
        assert_eq!(vt.cursor().hidden_since_ns, t0);
        clock.advance_ms(100);
        vt.process(b"\x1b[?25l");
        assert_eq!(vt.cursor().hidden_for_ns(ms(100)), Some(ms(100) - t0));
        vt.process(b"\x1b[?25h\x1b[?25l");
        assert_eq!(
            vt.cursor().hidden_since_ns,
            ms(100),
            "a show inside the batch"
        );

        clock.advance_ms(10);
        vt.process(b"\x1b[?2026h\x1b[?25h\x1b[?25l\x1b[2;2H");
        assert_eq!(
            vt.cursor().hidden_since_ns,
            ms(100),
            "the frozen frame's own"
        );
        vt.process(b"\x1b[?2026l");
        assert_eq!(
            vt.cursor().hidden_since_ns,
            ms(110),
            "the live one, stamped"
        );

        clock.advance_ms(10);
        vt.process(b"\x1b[?1049h");
        assert!(vt.cursor().visible);
        clock.advance_ms(10);
        vt.process(b"\x1b[?1049l");
        assert_eq!(
            vt.cursor().hidden_since_ns,
            ms(130),
            "restored from the parked screen"
        );
        clock.advance_ms(10);
        vt.enter_alternate_screen();
        vt.leave_alternate_screen();
        assert_eq!(vt.cursor().hidden_since_ns, ms(140), "the API swap too");
    }

    /// Parsed late is not sent late: a batch parsed past the deadline must not tear a
    /// frame the pane finished. Only the sweep releases a window on time.
    #[test]
    fn a_sync_window_is_released_once_it_outlives_its_deadline() {
        let clock = test_clock::Frozen::new();
        let mut vt = VirtualTerminal::new(3, 10);
        vt.process(b"before");
        vt.process(b"\x1b[?2026h\x1b[1;1Haf");
        clock.advance_ms(SYNC_UPDATE_TIMEOUT_MS * 2);

        vt.process(b"ter   ");
        assert!(vt.modes().synchronized_output);
        assert_eq!(
            compact_capture(&vt),
            "before",
            "a late batch must not show the half-drawn frame"
        );
        assert!(vt.release_expired_sync(), "the sweep releases it");
        assert!(compact_capture(&vt).starts_with("after"));
    }

    /// Parsing reads no clock, so two terminals fed the same bytes in turn agree
    /// however long passes between them; only a sweep past the deadline parts them.
    #[test]
    fn the_sync_deadline_is_what_two_terminals_fed_in_turn_can_disagree_on() {
        for swept in [true, false] {
            let clock = test_clock::Frozen::new();
            let (mut first, mut second) =
                (VirtualTerminal::new(3, 10), VirtualTerminal::new(3, 10));
            for vt in [&mut first, &mut second] {
                vt.process(b"before\x1b[?2026h\x1b[1;1Hafter   ");
            }
            first.process(b"!");
            clock.advance_ms(SYNC_UPDATE_TIMEOUT_MS);
            second.process(b"!");
            if swept {
                second.release_expired_sync();
            }
            assert_eq!(
                compact_capture(&first) != compact_capture(&second),
                swept,
                "swept {swept}: {:?} vs {:?}",
                compact_capture(&first),
                compact_capture(&second)
            );
        }
    }

    #[test]
    fn a_silent_pane_holding_a_sync_window_is_swept() {
        let clock = test_clock::Frozen::new();
        let mut vt = VirtualTerminal::new(3, 10);
        vt.process(b"before");
        vt.process(b"\x1b[?2026h\x1b[1;1Hafter   ");

        assert!(
            !vt.release_expired_sync(),
            "a window inside its deadline must not be swept"
        );
        assert_eq!(compact_capture(&vt), "before");

        clock.advance_ms(SYNC_UPDATE_TIMEOUT_MS - 1);
        assert!(
            !vt.release_expired_sync(),
            "a window 1 ms short of its deadline must not be swept"
        );
        clock.advance_ms(1);

        let revision_before = vt.content_revision();
        assert!(vt.release_expired_sync(), "an expired window must be swept");
        assert_eq!(compact_capture(&vt), "after");
        assert!(
            vt.content_revision() > revision_before,
            "revealing hidden writes moves the presented frame and must bump the revision"
        );
        assert!(
            !vt.release_expired_sync(),
            "sweeping twice must not keep reporting a release"
        );
    }

    /// A pane can also hold the window open while pouring out output, which
    /// the clock alone would let it do for as long as it liked if the clock
    /// were ever read wrong. The byte cap is the independent guard.
    #[test]
    fn a_sync_window_is_released_once_it_absorbs_too_much_output() {
        let _clock = test_clock::Frozen::new();
        let mut vt = VirtualTerminal::new(3, 4_000);
        vt.process(b"before");
        vt.process(b"\x1b[?2026h");

        // Deliberately well inside the time deadline: this must be the BYTE
        // cap firing, not the clock. Each chunk is one screen of plain text,
        // so no chunk is itself unusual.
        let chunk = vec![b'x'; 64 * 1024];
        let mut fed = 0u64;
        while vt.modes().synchronized_output && fed < SYNC_UPDATE_MAX_BYTES * 2 {
            vt.process(&chunk);
            fed += chunk.len() as u64;
        }

        assert!(
            !vt.modes().synchronized_output,
            "a window absorbed {fed} bytes without being released; the cap is {SYNC_UPDATE_MAX_BYTES}"
        );
        assert!(
            fed <= SYNC_UPDATE_MAX_BYTES + chunk.len() as u64,
            "the window absorbed {fed} bytes before releasing, over the {SYNC_UPDATE_MAX_BYTES} cap"
        );
    }

    /// The deadline must be per WINDOW, not per pane: a pane that opens and
    /// closes windows normally for longer than the timeout must never be
    /// force-released, or every long-lived TUI would tear.
    #[test]
    fn the_deadline_resets_with_every_window() {
        let clock = test_clock::Frozen::new();
        let mut vt = VirtualTerminal::new(3, 10);
        let step_ms = 10;
        for step in 0..(SYNC_UPDATE_TIMEOUT_MS * 2 / step_ms) {
            vt.process(b"\x1b[?2026h");
            vt.process(b"\x1b[1;1Hframe   ");
            assert!(!vt.release_expired_sync());
            assert!(
                vt.modes().synchronized_output,
                "a window was force-released {} ms into the pane's life",
                step * step_ms
            );
            vt.process(b"\x1b[?2026l");
            clock.advance_ms(step_ms);
        }
        assert_eq!(compact_capture(&vt), "frame");
    }

    /// Output inside an open window must not extend its deadline: a TUI that
    /// streams while holding the window is swept on time, not at the cap.
    #[test]
    fn output_inside_a_window_does_not_extend_its_deadline() {
        let clock = test_clock::Frozen::new();
        let mut vt = VirtualTerminal::new(3, 10);
        vt.process(b"\x1b[?2026h");
        let step_ms = 10;
        for elapsed in (step_ms..=SYNC_UPDATE_TIMEOUT_MS).step_by(step_ms as usize) {
            clock.advance_ms(step_ms);
            vt.process(b"x");
            vt.release_expired_sync();
            assert_eq!(
                vt.modes().synchronized_output,
                elapsed < SYNC_UPDATE_TIMEOUT_MS,
                "streamed into for {elapsed} ms; the deadline is {SYNC_UPDATE_TIMEOUT_MS} ms"
            );
        }
    }

    fn row_text(vt: &VirtualTerminal, abs: usize) -> String {
        vt.presented_row(abs)
            .map(|row| {
                (0..row.len())
                    .filter(|&c| !row[c].is_wide_continuation())
                    .map(|c| row[c].ch)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .unwrap_or_else(|| "<missing>".into())
    }

    /// A synchronized window that enters the ALTERNATE screen swaps which grid
    /// is live — so the history behind the frozen primary frame is no longer in
    /// `self.grid` at all, it is in the stashed one. Reading it from the live
    /// grid exposes alternate-screen rows as "history", or loses history
    /// entirely (the alternate screen is built with no scrollback).
    #[test]
    fn presented_history_comes_from_the_screen_the_frame_was_frozen_on() {
        let mut vt = VirtualTerminal::new(3, 16);
        for i in 0..12 {
            vt.process(format!("hist-{i:02}\r\n").as_bytes());
        }
        let history_before: Vec<String> = (0..vt.presented_total_lines())
            .map(|i| row_text(&vt, i))
            .collect();

        // Freeze on the PRIMARY screen, then enter the alternate screen and
        // draw on it — all inside the window.
        vt.process(b"\x1b[?2026h");
        vt.process(b"\x1b[1;1Hx"); // take the freeze on the primary screen
        vt.process(b"\x1b[?1049hALT-SECRET-ONE\x1b[2;1HALT-SECRET-TWO");

        assert!(
            !vt.is_alternate_screen(),
            "the presented frame is still the primary one"
        );
        let history_now: Vec<String> = (0..vt.presented_total_lines())
            .map(|i| row_text(&vt, i))
            .collect();
        assert!(
            !history_now.iter().any(|l| l.contains("ALT-SECRET")),
            "alternate-screen content leaked into the presented primary frame: {history_now:?}"
        );
        assert_eq!(
            history_now, history_before,
            "the presented frame moved while the window was open"
        );

        vt.process(b"\x1b[?2026l");
        assert!(vt.is_alternate_screen());
        assert!(compact_capture(&vt).contains("ALT-SECRET-ONE"));
    }

    /// History behind a frozen frame is read out of the LIVE grid, so its
    /// indices move as lines fall off the front of the scrollback.
    ///
    /// Getting the shift backwards does not just reorder history — it walks
    /// the index PAST the surviving lines and into the live viewport, so the
    /// content the frozen frame exists to hide appears above it. This pins the
    /// mapping with real content: the surviving lines, in order, and nothing
    /// written after the freeze.
    #[test]
    fn presented_history_survives_partial_eviction_without_leaking_live_content() {
        let cfg = GridConfig {
            max_scrollback: 8,
            track_dirty: true,
        };
        let mut vt = VirtualTerminal::with_config(2, 12, cfg);
        for i in 0..20 {
            vt.process(format!("line-{i:03}\r\n").as_bytes());
        }

        vt.process(b"\x1b[?2026h");
        vt.process(b"\x1b[1;1Hx"); // force the freeze

        // Enough scrolling to evict SOME of the history the freeze spans, not
        // all of it: partial eviction is where the index shift shows.
        for i in 0..4 {
            vt.process(format!("post-{i:03}\r\n").as_bytes());
        }

        let lines: Vec<String> = (0..vt.presented_total_lines())
            .map(|i| {
                vt.presented_row(i)
                    .map(|row| {
                        (0..row.len())
                            .map(|c| row[c].ch)
                            .collect::<String>()
                            .trim_end()
                            .to_string()
                    })
                    .unwrap_or_else(|| "<missing>".into())
            })
            .collect();

        assert!(
            !lines.iter().any(|l| l == "<missing>"),
            "presented span claims lines it cannot resolve: {lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l.starts_with("post-")),
            "content written after the freeze leaked into the presented frame: {lines:?}"
        );

        let numbers: Vec<usize> = lines
            .iter()
            .filter(|l| l.starts_with("line-"))
            .filter_map(|l| l.trim_start_matches("line-").parse().ok())
            .collect();
        assert!(
            numbers.len() >= 2,
            "expected surviving original history, got {lines:?}"
        );
        for pair in numbers.windows(2) {
            assert_eq!(
                pair[1],
                pair[0] + 1,
                "presented history skipped a line: {numbers:?} (full span {lines:?})"
            );
        }
    }

    #[test]
    fn process_with_responses_reports_active_sgr_in_decrqss() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b[1;2;3;4;5;7;8;9;38;2;1;2;3;48;5;196m");
        let responses = vt.process_with_responses(b"\x1bP$qm\x1b\\");

        assert_eq!(
            responses,
            vec![b"\x1bP1$r1;2;3;4;5;7;8;9;38;2;1;2;3;48;5;196m\x1b\\".to_vec()]
        );
    }

    #[test]
    fn process_with_responses_reports_advanced_underline_sgr_in_decrqss() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b[4:3;58:2::10:20:30m");
        let responses = vt.process_with_responses(b"\x1bP$qm\x1b\\");

        assert_eq!(
            responses,
            vec![b"\x1bP1$r4:3;58;2;10;20;30m\x1b\\".to_vec()]
        );
    }

    #[test]
    fn process_with_responses_reports_indexed_sgr_in_decrqss() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b[91;44m");
        let responses = vt.process_with_responses(b"\x1bP$qm\x1b\\");

        assert_eq!(responses, vec![b"\x1bP1$r91;44m\x1b\\".to_vec()]);
    }

    #[test]
    fn process_with_responses_answers_decrqss() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b[2;4r\x1b[5 q");
        let responses = vt.process_with_responses(b"\x1bP$qm\x1b\\\x1bP$qr\x1b\\\x1bP$q q\x1b\\");

        assert_eq!(
            responses,
            vec![
                b"\x1bP1$r0m\x1b\\".to_vec(),
                b"\x1bP1$r2;4r\x1b\\".to_vec(),
                b"\x1bP1$r5 q\x1b\\".to_vec(),
            ]
        );
    }

    #[test]
    fn test_cursor_up_down() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b[10;1H"); // Go to row 10.
        vt.process(b"\x1b[3A"); // Move up 3.
        assert_eq!(vt.cursor().row, 6);
        vt.process(b"\x1b[2B"); // Move down 2.
        assert_eq!(vt.cursor().row, 8);
    }

    #[test]
    fn test_cursor_forward_backward() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b[1;20H"); // Go to col 20.
        vt.process(b"\x1b[5C"); // Forward 5.
        assert_eq!(vt.cursor().col, 24);
        vt.process(b"\x1b[10D"); // Backward 10.
        assert_eq!(vt.cursor().col, 14);
    }

    #[test]
    fn test_sgr_colors() {
        let mut vt = VirtualTerminal::new(24, 80);
        // Set red foreground (SGR 31), then write a character.
        vt.process(b"\x1b[31mX");
        let cell = &vt.grid().visible_row(0)[0];
        assert_eq!(cell.ch, 'X');
        assert_eq!(cell.style.fg, Color::Indexed(1)); // red
    }

    #[test]
    fn test_sgr_24bit_color() {
        let mut vt = VirtualTerminal::new(24, 80);
        // Set 24-bit foreground: SGR 38;2;255;128;0.
        vt.process(b"\x1b[38;2;255;128;0mX");
        let cell = &vt.grid().visible_row(0)[0];
        assert_eq!(cell.style.fg, Color::Rgb(255, 128, 0));
    }

    #[test]
    fn truecolor_mid_row_sgr_preserves_unicode_box_cells() {
        let mut vt = VirtualTerminal::new(3, 40);
        vt.process(
            "\x1b[38;2;31;122;120m│\x1b[m \
             \x1b[38;2;240;190;101m2E 7W 8I\x1b[m \
             \x1b[38;2;31;122;120m│\x1b[m"
                .as_bytes(),
        );

        let row = vt.grid().visible_row(0);
        assert_eq!(row[0].ch, '│');
        assert_eq!(row[0].style.fg, Color::Rgb(31, 122, 120));
        assert_eq!(row[11].ch, '│');
        assert_eq!(row[11].style.fg, Color::Rgb(31, 122, 120));
    }

    #[test]
    fn erase_in_line_clears_shorter_diff_redraw_content() {
        let mut vt = VirtualTerminal::new(3, 40);
        vt.process(b"\x1b[?1049h");
        vt.process(b"\x1b[2;1Hhad 1. Fix blockers");
        vt.process(b"\x1b[2;1H\x1b[0K1. Fix blockers");

        let row = vt.grid().visible_row(1);
        let rendered: String = row.cells.iter().map(|cell| cell.ch).collect();
        assert!(rendered.starts_with("1. Fix blockers"));
        assert!(
            !rendered.contains("had "),
            "stale prior-frame content remained: {rendered:?}"
        );
    }

    #[test]
    fn csi_save_restore_keeps_diff_redraw_at_saved_cursor() {
        let mut vt = VirtualTerminal::new(3, 40);
        vt.process(b"\x1b[?1049h");
        vt.process(b"\x1b[2;1H\x1b[s");
        vt.process(b"had 1. Fix blockers");
        vt.process(b"\x1b[u\x1b[0K1. Fix blockers");

        let row = vt.grid().visible_row(1);
        let rendered: String = row.cells.iter().map(|cell| cell.ch).collect();
        assert!(rendered.starts_with("1. Fix blockers"));
        assert!(
            !rendered.contains("had "),
            "CSI u did not restore before erase/redraw: {rendered:?}"
        );
    }

    #[test]
    fn bubbletea_style_sync_redraw_preserves_box_and_clears_stale_text() {
        let mut vt = VirtualTerminal::new(4, 40);
        vt.process(b"\x1b[?1049h\x1b[?2026h");
        vt.process(
            "\x1b[2;1H\x1b[s\
             \x1b[38;2;31;122;120m│\x1b[m \
             \x1b[38;2;166;173;200mhad\x1b[m \
             \x1b[38;2;240;190;101m1.\x1b[m \
             \x1b[38;2;137;180;250mFix blockers\x1b[m \
             \x1b[38;2;31;122;120m│\x1b[m"
                .as_bytes(),
        );
        vt.process(
            "\x1b[u\x1b[0K\
             \x1b[38;2;31;122;120m│\x1b[m \
             \x1b[38;2;240;190;101m1.\x1b[m \
             \x1b[38;2;137;180;250mFix blockers\x1b[m \
             \x1b[38;2;31;122;120m│\x1b[m\
             \x1b[?2026l"
                .as_bytes(),
        );

        let row = vt.grid().visible_row(1);
        let rendered: String = row.cells.iter().map(|cell| cell.ch).collect();
        assert!(rendered.starts_with("│ 1. Fix blockers │"));
        assert!(
            !rendered.contains("had "),
            "stale prior-frame content remained: {rendered:?}"
        );
        assert_eq!(row[0].ch, '│');
        assert_eq!(row[18].ch, '│');
        assert_eq!(row[0].style.fg, Color::Rgb(31, 122, 120));
        assert_eq!(row[2].style.fg, Color::Rgb(240, 190, 101));
        assert_eq!(row[5].style.fg, Color::Rgb(137, 180, 250));
        assert_eq!(row[18].style.fg, Color::Rgb(31, 122, 120));
    }

    #[test]
    fn parameterized_csi_u_is_not_cursor_restore() {
        let mut vt = VirtualTerminal::new(3, 40);
        vt.process(b"\x1b[1;1H\x1b[s");
        vt.process(b"\x1b[2;2H\x1b[27;2u");

        assert_eq!((vt.cursor().row, vt.cursor().col), (1, 1));
    }

    #[test]
    fn private_mode_1048_saves_and_restores_cursor() {
        let mut vt = VirtualTerminal::new(3, 40);
        vt.process(b"\x1b[2;5H\x1b[?1048h");
        vt.process(b"\x1b[31m");
        vt.process(b"\x1b[3;20H");
        assert_eq!((vt.cursor().row, vt.cursor().col), (2, 19));

        vt.process(b"\x1b[?1048l");

        assert_eq!((vt.cursor().row, vt.cursor().col), (1, 4));
        assert_eq!(vt.cursor().style.fg, Color::Default);
    }

    #[test]
    fn repeated_alt_screen_enter_does_not_discard_primary_grid() {
        let mut vt = VirtualTerminal::new(3, 40);
        vt.process(b"primary");
        vt.process(b"\x1b[?1049h");
        vt.process(b"alt");
        vt.process(b"\x1b[?1049h");
        assert_eq!(
            vt.grid().visible_row(0)[0].ch,
            ' ',
            "re-entering 1049 should clear the active alternate grid"
        );
        vt.process(b"\x1b[?1049l");

        let row = vt.grid().visible_row(0);
        let rendered: String = row.cells.iter().map(|cell| cell.ch).collect();
        assert!(rendered.starts_with("primary"));
        assert!(!vt.is_alternate_screen());
    }

    #[test]
    fn mode_1047_does_not_restore_primary_cursor_on_leave() {
        let mut vt = VirtualTerminal::new(3, 40);
        vt.process(b"\x1b[2;5H");
        vt.process(b"\x1b[?1047h");
        vt.process(b"\x1b[3;20H");
        vt.process(b"\x1b[?1047l");

        assert_eq!((vt.cursor().row, vt.cursor().col), (2, 19));
        assert!(!vt.is_alternate_screen());
    }

    #[test]
    fn mode_1049_leave_restores_saved_cursor_even_when_primary_active() {
        let mut vt = VirtualTerminal::new(3, 40);
        vt.process(b"\x1b[2;5H\x1b[?1048h");
        vt.process(b"\x1b[3;20H");
        vt.process(b"\x1b[?1049l");

        assert_eq!((vt.cursor().row, vt.cursor().col), (1, 4));
        assert!(!vt.is_alternate_screen());
    }

    #[test]
    fn nested_1049_enter_preserves_saved_cursor_while_already_alt_screen() {
        let mut vt = VirtualTerminal::new(3, 40);
        vt.process(b"\x1b[?1047h\x1b[2;5H");
        vt.process(b"\x1b[?1049h");
        vt.process(b"\x1b[3;20Hnested");
        vt.process(b"\x1b[?1049l");

        assert_eq!((vt.cursor().row, vt.cursor().col), (1, 4));
    }

    #[test]
    fn repeat_preceding_space_clears_stale_prefix_before_short_redraw() {
        let mut vt = VirtualTerminal::new(3, 40);
        vt.process(b"\x1b[?1049h");
        vt.process(b"\x1b[2;1Hhad 1. Fix blockers");
        vt.process(b"\x1b[2;1H \x1b[3b1. Fix blockers");

        let row = vt.grid().visible_row(1);
        let rendered: String = row.cells.iter().map(|cell| cell.ch).collect();
        assert!(rendered.starts_with("    1. Fix blockers"));
        assert!(
            !rendered.contains("had "),
            "REP failed to clear stale prefix: {rendered:?}"
        );
    }

    /// REP's source is the data stream, not the screen (issue #122). Homing the
    /// cursor puts it at column 0, where the old screen-derived source had
    /// nothing to the left to read and dropped the repeat entirely.
    #[test]
    fn rep_repeats_across_a_cursor_move() {
        let mut vt = VirtualTerminal::new(2, 10);
        vt.process(b"X\x1b[1;1H\x1b[3b");

        assert_eq!(vt.capture_text(Some(1)).trim_end(), "XXX");
    }

    /// The character is not on the screen any more. It is still the preceding
    /// character in the stream.
    #[test]
    fn rep_repeats_a_character_that_has_been_erased() {
        let mut vt = VirtualTerminal::new(2, 10);
        vt.process(b"Y\x1b[2J\x1b[1;1H\x1b[3b");

        assert_eq!(vt.capture_text(Some(1)).trim_end(), "YYY");
    }

    /// Nothing has been printed, so there is no preceding character. REP must
    /// not fall back to whatever the cursor is parked next to.
    #[test]
    fn rep_without_a_preceding_character_writes_nothing() {
        let mut vt = VirtualTerminal::new(2, 10);
        let before = vt.grid().mutations();
        vt.process(b"\x1b[9b");

        assert_eq!(vt.grid().mutations(), before);
        assert_eq!(vt.capture_text(Some(1)).trim_end(), "");
    }

    /// RIS ends the stream: a terminal that has just been switched on has no
    /// preceding character.
    #[test]
    fn ris_clears_the_character_rep_would_repeat() {
        let mut vt = VirtualTerminal::new(2, 10);
        vt.process(b"Z\x1bc");
        let before = vt.grid().mutations();
        vt.process(b"\x1b[4b");

        assert_eq!(vt.grid().mutations(), before);
    }

    /// The pen belongs to the terminal, not to the remembered character, so the
    /// repeats take the colour that is current at the `CSI b`. Reading the
    /// source cell off the screen copied the original's colour with it.
    #[test]
    fn rep_paints_with_the_pen_current_at_the_sequence() {
        let mut vt = VirtualTerminal::new(2, 10);
        vt.process(b"\x1b[31mX\x1b[32m\x1b[2b");

        let row = vt.grid().visible_row(0);
        assert_eq!(row[0].style.fg, Color::Indexed(1));
        assert_eq!(row[1].style.fg, Color::Indexed(2));
        assert_eq!(row[2].style.fg, Color::Indexed(2));
    }

    #[test]
    fn rep_after_last_column_repeats_pending_wrap_character() {
        let mut vt = VirtualTerminal::new(3, 2);
        vt.process(b"\x1b[2GA\x1b[2b");

        assert_eq!(vt.grid().visible_row(0)[1].ch, 'A');
        assert_eq!(vt.grid().visible_row(1)[0].ch, 'A');
        assert_eq!(vt.grid().visible_row(1)[1].ch, 'A');
    }

    #[test]
    fn hpa_before_erase_clears_stale_scan_text_before_summary_redraw() {
        let mut vt = VirtualTerminal::new(3, 80);
        vt.process(b"\x1b[?1049h");
        vt.process(b"\x1b[2;1Hhadolint x");
        vt.process(b"\x1b[2;40H\x1b[1`\x1b[K    1. Fix blockers");

        let row = vt.grid().visible_row(1);
        let rendered: String = row.cells.iter().map(|cell| cell.ch).collect();
        assert!(rendered.starts_with("    1. Fix blockers"));
        assert!(
            !rendered.contains("hadolint"),
            "HPA before EL failed to clear stale scan text: {rendered:?}"
        );
    }

    #[test]
    fn renderer_cursor_tabulation_and_relative_moves_are_applied() {
        let mut vt = VirtualTerminal::new(4, 40);
        vt.process(b"A\x1b[2IC");
        assert_eq!(vt.cursor().col, 17);
        vt.process(b"\x1b[1ZX");
        assert_eq!(vt.grid().visible_row(0)[16].ch, 'X');
        vt.process(b"\x1b[2aY");
        assert_eq!(vt.grid().visible_row(0)[19].ch, 'Y');
        vt.process(b"\x1b[1eZ");
        assert_eq!(vt.grid().visible_row(1)[20].ch, 'Z');
        vt.process(b"\x1b[5`H");
        assert_eq!(vt.grid().visible_row(1)[4].ch, 'H');
    }

    #[test]
    fn tab_stops_resize_preserves_custom_and_extends_defaults() {
        let mut vt = VirtualTerminal::new(2, 16);
        vt.process(b"\x1b[13G\x1bH");
        vt.resize(2, 40);
        vt.process(b"\r\x1b[13G\x1b[IX");
        assert_eq!(vt.grid().visible_row(0)[16].ch, 'X');
    }

    #[test]
    fn tab_stops_clear_all_survives_resize_growth() {
        let mut vt = VirtualTerminal::new(2, 16);
        vt.process(b"\x1b[3g");
        vt.resize(2, 40);
        vt.process(b"\r\tX");
        assert_eq!(vt.grid().visible_row(0)[39].ch, 'X');
    }

    #[test]
    fn tab_stops_clear_all_then_hts_does_not_restore_resize_defaults() {
        let mut vt = VirtualTerminal::new(2, 16);
        vt.process(b"\x1b[3g\x1b[13G\x1bH");
        vt.resize(2, 40);
        vt.process(b"\r\x1b[13G\x1b[IX");
        assert_eq!(vt.grid().visible_row(0)[39].ch, 'X');
    }

    #[test]
    fn tab_stops_survive_alternate_screen_switch() {
        let mut vt = VirtualTerminal::new(4, 40);
        vt.process(b"\x1b[13G\x1bH\x1b[?1049h\x1b[?1049l\r\tA\tB");
        let row = vt.grid().visible_row(0);
        assert_eq!(row[8].ch, 'A');
        assert_eq!(row[12].ch, 'B');
    }

    #[test]
    fn tab_stops_ris_restores_defaults() {
        let mut vt = VirtualTerminal::new(2, 40);
        vt.process(b"\x1b[3g\x1bc\r\tX");
        assert_eq!(vt.grid().visible_row(0)[8].ch, 'X');
    }

    #[test]
    fn renderer_scroll_up_down_primitives_shift_scroll_region() {
        let mut vt = VirtualTerminal::new(5, 10);
        vt.process(b"\x1b[1;1HA\x1b[2;1HB\x1b[3;1HC\x1b[4;1HD\x1b[5;1HE");
        vt.process(b"\x1b[2;4r");
        vt.process(b"\x1b[1S");

        assert_eq!(vt.grid().visible_row(0)[0].ch, 'A');
        assert_eq!(vt.grid().visible_row(1)[0].ch, 'C');
        assert_eq!(vt.grid().visible_row(2)[0].ch, 'D');
        assert_eq!(vt.grid().visible_row(3)[0].ch, ' ');
        assert_eq!(vt.grid().visible_row(4)[0].ch, 'E');

        vt.process(b"\x1b[1T");
        assert_eq!(vt.grid().visible_row(0)[0].ch, 'A');
        assert_eq!(vt.grid().visible_row(1)[0].ch, ' ');
        assert_eq!(vt.grid().visible_row(2)[0].ch, 'C');
        assert_eq!(vt.grid().visible_row(3)[0].ch, 'D');
        assert_eq!(vt.grid().visible_row(4)[0].ch, 'E');
    }

    #[test]
    fn advanced_underline_sgr_sets_extended_cell_attributes() {
        let mut vt = VirtualTerminal::new(2, 40);
        vt.process(b"\x1b[4:3mC\x1b[58:2::10:20:30mU\x1b[59mN\x1b[24mP");

        let row = vt.grid().visible_row(0);
        let curly = row[0].extended.as_ref().expect("curly underline");
        assert_eq!(curly.underline_style, UnderlineStyle::Curly);
        assert!(row[0].style.flags.contains(CellFlags::UNDERLINE));

        let colored = row[1].extended.as_ref().expect("underline color");
        assert_eq!(colored.underline_style, UnderlineStyle::Curly);
        assert_eq!(colored.underline_color, Some(Color::Rgb(10, 20, 30)));

        let no_color = row[2].extended.as_ref().expect("underline style remains");
        assert_eq!(no_color.underline_style, UnderlineStyle::Curly);
        assert_eq!(no_color.underline_color, None);
        assert!(row[3].extended.is_none());
        assert!(!row[3].style.flags.contains(CellFlags::UNDERLINE));
    }

    #[test]
    fn osc8_hyperlink_applies_to_subsequent_cells_until_cleared() {
        let mut vt = VirtualTerminal::new(2, 40);
        vt.process(b"\x1b]8;;https://example.invalid/a;b\x07L\x1b]8;;\x07N");

        let linked = vt.grid().visible_row(0)[0]
            .extended
            .as_ref()
            .expect("hyperlink");
        assert_eq!(
            linked.hyperlink.as_deref(),
            Some("https://example.invalid/a;b")
        );
        assert!(vt.grid().visible_row(0)[1].extended.is_none());
    }

    /// A link ends at OSC 8 with an empty URI, not at an SGR reset: `ls
    /// --hyperlink`, compilers and shux's own renderer reset colours inside a
    /// link, and every real terminal keeps the link across that reset.
    #[test]
    fn osc8_hyperlink_survives_sgr_reset() {
        let mut vt = VirtualTerminal::new(2, 40);
        vt.process(b"\x1b]8;;https://example.invalid/x\x1b\\\x1b[1mA\x1b[0mB\x1b]8;;\x1b\\C");

        let row = vt.grid().visible_row(0);
        let link = |c: usize| {
            row[c]
                .extended
                .as_ref()
                .and_then(|e| e.hyperlink.as_deref())
        };
        assert_eq!(link(0), Some("https://example.invalid/x"));
        assert_eq!(
            link(1),
            Some("https://example.invalid/x"),
            "SGR 0 ended the link"
        );
        assert_eq!(link(2), None);
    }

    /// A link left open does not follow the program onto the other screen:
    /// switching buffers (47, 1047, 1049, either way) closes it.
    #[test]
    fn an_open_link_does_not_cross_a_screen_switch() {
        for mode in ["47", "1047", "1049"] {
            let mut vt = VirtualTerminal::new(2, 40);
            vt.process(b"\x1b]8;;https://example.invalid/x\x1b\\L");
            vt.process(format!("\x1b[?{mode}hA\x1b[H").as_bytes());
            let linked = |vt: &VirtualTerminal| {
                vt.grid().visible_row(0)[0]
                    .extended
                    .as_ref()
                    .and_then(|e| e.hyperlink.clone())
            };
            assert_eq!(
                linked(&vt),
                None,
                "?{mode}h carried the link onto the alternate screen"
            );
            vt.process(b"\x1b]8;;https://example.invalid/y\x1b\\B");
            vt.process(format!("\x1b[?{mode}l\x1b[HP").as_bytes());
            assert_eq!(linked(&vt), None, "?{mode}l carried the link back");
        }
    }

    #[test]
    fn grapheme_combining_mark_is_stored_and_captured() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process("e\u{0301}x".as_bytes());

        let row = vt.grid().visible_row(0);
        assert_eq!(row[0].ch, 'e');
        assert_eq!(row[0].grapheme(), Some("e\u{0301}"));
        assert_eq!(row[1].ch, 'x');
        assert!(row[1].grapheme().is_none());
        assert_eq!(vt.capture_text(Some(1)).trim_end(), "e\u{0301}x");
    }

    #[test]
    fn grapheme_variation_modifier_zwj_and_flag_payloads_are_preserved() {
        let mut vt = VirtualTerminal::new(3, 40);
        vt.process("A🛠\u{fe0f} B👍🏽 C👨\u{200d}💻 D🇺🇸".as_bytes());

        let captured = vt.capture_text(Some(1));
        assert!(
            captured.contains("🛠\u{fe0f}"),
            "VS16 payload missing: {captured:?}"
        );
        assert!(
            captured.contains("👍🏽"),
            "skin-tone payload missing: {captured:?}"
        );
        assert!(
            captured.contains("👨\u{200d}💻"),
            "ZWJ payload missing: {captured:?}"
        );
        assert!(
            captured.contains("🇺🇸"),
            "flag payload missing: {captured:?}"
        );
        assert!(
            vt.grid()
                .visible_row(0)
                .cells
                .iter()
                .any(|cell| cell.grapheme() == Some("👨\u{200d}💻"))
        );
        assert!(
            vt.grid()
                .visible_row(0)
                .cells
                .iter()
                .any(|cell| cell.grapheme() == Some("🇺🇸") && cell.width == 2)
        );
        assert_grid_wide_invariants(vt.grid());
    }

    #[test]
    fn ascii_cells_remain_on_compact_common_path() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process(b"plain ascii");

        for cell in vt.grid().visible_row(0).cells.iter().take(11) {
            assert!(cell.extended.is_none(), "ASCII cell used extended attrs");
            assert!(
                cell.grapheme().is_none(),
                "ASCII cell used grapheme payload"
            );
        }
    }

    #[test]
    fn grapheme_payload_is_cell_local_not_cursor_style() {
        let mut vt = VirtualTerminal::new(2, 40);
        vt.process(b"\x1b]8;;https://example.invalid/a;b\x07");
        vt.process("e\u{0301}x".as_bytes());

        let row = vt.grid().visible_row(0);
        let first = row[0].extended.as_ref().expect("first cell extended attrs");
        assert_eq!(first.grapheme.as_deref(), Some("e\u{0301}"));
        assert_eq!(
            first.hyperlink.as_deref(),
            Some("https://example.invalid/a;b")
        );

        let second = row[1].extended.as_ref().expect("second cell hyperlink");
        assert_eq!(
            second.hyperlink.as_deref(),
            Some("https://example.invalid/a;b")
        );
        assert_eq!(
            second.grapheme, None,
            "grapheme payload leaked through cursor extended attrs"
        );
    }

    #[test]
    fn styled_ascii_cells_share_cursor_extended_attrs() {
        let mut vt = VirtualTerminal::new(2, 40);
        vt.process(b"\x1b]8;;https://example.invalid/a;b\x07");
        vt.process(b"ab");
        vt.process("e\u{0301}".as_bytes());

        let row = vt.grid().visible_row(0);
        let first = row[0].extended.as_ref().expect("first hyperlink");
        let second = row[1].extended.as_ref().expect("second hyperlink");
        let grapheme = row[2].extended.as_ref().expect("grapheme hyperlink");

        assert!(
            Arc::ptr_eq(first, second),
            "plain styled run should share cursor extended attrs"
        );
        assert!(
            !Arc::ptr_eq(second, grapheme),
            "grapheme payload should copy-on-write cell attrs"
        );
        assert_eq!(first.grapheme, None);
        assert_eq!(second.grapheme, None);
        assert_eq!(grapheme.grapheme.as_deref(), Some("e\u{0301}"));
        assert_eq!(
            grapheme.hyperlink.as_deref(),
            Some("https://example.invalid/a;b")
        );
    }

    #[test]
    fn combining_after_cursor_motion_does_not_attach_to_stale_cell() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process("e\x1b[1;10H\u{0301}x".as_bytes());

        let row = vt.grid().visible_row(0);
        assert_eq!(row[0].ch, 'e');
        assert!(row[0].grapheme().is_none());
        assert_eq!(row[9].ch, 'x');
    }

    #[test]
    fn combining_after_esc_line_movement_does_not_attach_to_stale_cell() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process("e\x1bE\u{0301}x".as_bytes());

        let row0 = vt.grid().visible_row(0);
        let row1 = vt.grid().visible_row(1);
        assert_eq!(row0[0].ch, 'e');
        assert!(row0[0].grapheme().is_none());
        assert_eq!(row1[0].ch, 'x');
    }

    #[test]
    fn combining_after_final_column_preserves_pending_wrap() {
        let mut vt = VirtualTerminal::new(2, 3);
        vt.process("abe\u{0301}x".as_bytes());

        assert_eq!(vt.grid().visible_row(0)[2].grapheme(), Some("e\u{0301}"));
        assert_eq!(vt.grid().visible_row(1)[0].ch, 'x');
        assert_eq!(vt.capture_text(None), "abe\u{0301}\nx\n");
    }

    #[test]
    fn combining_after_cjk_attaches_to_wide_head() {
        let mut vt = VirtualTerminal::new(2, 10);
        vt.process("界\u{0301}x".as_bytes());

        let row = vt.grid().visible_row(0);
        assert_eq!(row[0].grapheme(), Some("界\u{0301}"));
        assert!(row[1].is_wide_continuation());
        assert_eq!(row[2].ch, 'x');
        assert_grid_wide_invariants(vt.grid());
    }

    #[test]
    fn zwj_width_expansion_preserves_wide_invariants() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process("a\u{200d}👨x".as_bytes());

        let row = vt.grid().visible_row(0);
        assert_eq!(row[0].grapheme(), Some("a\u{200d}👨"));
        assert!(row[0].is_wide());
        assert!(row[1].is_wide_continuation());
        assert_eq!(row[2].ch, 'x');
        assert_grid_wide_invariants(vt.grid());
    }

    #[test]
    fn zwj_and_flag_at_final_column_do_not_create_final_wide_head() {
        let mut zwj = VirtualTerminal::new(2, 5);
        zwj.process(b"\x1b[1;5H");
        zwj.process("a\u{200d}👨".as_bytes());
        assert!(
            !zwj.grid().visible_row(0)[4].is_wide(),
            "ZWJ sequence created a width-2 head in the final column"
        );
        assert!(
            zwj.capture_text(None).contains("a\u{200d}"),
            "final-column ZWJ marker was lost"
        );
        assert_grid_wide_invariants(zwj.grid());

        let mut flag = VirtualTerminal::new(2, 5);
        flag.process(b"\x1b[1;5H");
        flag.process("🇺🇸".as_bytes());
        assert!(
            !flag.grid().visible_row(0)[4].is_wide(),
            "flag pair created a width-2 head in the final column"
        );
        assert!(
            flag.capture_text(None).contains("🇺") && flag.capture_text(None).contains("🇸"),
            "final-column regional indicators were lost"
        );
        assert_grid_wide_invariants(flag.grid());
    }

    #[test]
    fn rep_repeats_full_grapheme_payload() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process("e\u{0301}\x1b[3b".as_bytes());

        assert_eq!(
            vt.capture_text(Some(1)).trim_end(),
            "e\u{0301}e\u{0301}e\u{0301}e\u{0301}"
        );
    }

    #[test]
    fn rep_preserves_width_expanded_grapheme_payload() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process("a\u{200d}👨\x1b[2bZ".as_bytes());

        let row = vt.grid().visible_row(0);
        for col in [0, 2, 4] {
            assert_eq!(row[col].grapheme(), Some("a\u{200d}👨"));
            assert!(row[col].is_wide(), "cell {col} lost width-2 head");
            assert!(
                row[col + 1].is_wide_continuation(),
                "cell {} lost continuation",
                col + 1
            );
        }
        assert_eq!(row[6].ch, 'Z');
        assert_eq!(
            vt.capture_text(Some(1)).trim_end(),
            "a\u{200d}👨a\u{200d}👨a\u{200d}👨Z"
        );
        assert_grid_wide_invariants(vt.grid());
    }

    #[test]
    fn dec_special_graphics_maps_g0_line_drawing() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process(b"\x1b(0lqkxmj\x1b(B ascii");

        assert_eq!(vt.capture_text(Some(1)).trim_end(), "┌─┐│└┘ ascii");
    }

    #[test]
    fn dec_special_graphics_maps_complete_standard_set() {
        let mut vt = VirtualTerminal::new(2, 80);
        vt.process(b"\x1b(0_`abcdefghijklmnopqrstuvwxyz{|}~");

        assert_eq!(
            vt.capture_text(Some(1)).trim_end(),
            " ◆▒␉␌␍␊°±␤␋┘┐┌└┼⎺⎻─⎼⎽├┤┴┬│≤≥π≠£·"
        );
    }

    #[test]
    fn dec_special_graphics_state_persists_across_process_chunks() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process(b"\x1b(0");
        vt.process(b"lqk");

        assert_eq!(vt.capture_text(Some(1)).trim_end(), "┌─┐");
    }

    #[test]
    fn dec_special_graphics_so_si_switches_g1_without_leaking() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process(b"\x1b)0A\x0elqk\x0fZ");

        assert_eq!(vt.capture_text(Some(1)).trim_end(), "A┌─┐Z");
    }

    #[test]
    fn dec_special_graphics_dynamic_redesignation_updates_active_slot() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process(b"\x1b)0\x0eq\x1b)Bq");

        assert_eq!(vt.capture_text(Some(1)).trim_end(), "─q");
    }

    #[test]
    fn dec_special_graphics_invalid_designation_falls_back_to_ascii() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process(b"\x1b(0q\x1b(Xq");

        assert_eq!(vt.capture_text(Some(1)).trim_end(), "─q");
    }

    #[test]
    fn dec_special_graphics_rep_repeats_translated_cell() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process(b"\x1b(0q\x1b[3b");

        assert_eq!(vt.capture_text(Some(1)).trim_end(), "────");
    }

    #[test]
    fn dec_special_graphics_ris_resets_charset_state() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process(b"\x1b(0q\x1bcq");

        assert_eq!(vt.capture_text(Some(1)).trim_end(), "q");
    }

    #[test]
    fn dec_special_graphics_decsc_decrc_restore_charset_state() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process(b"\x1b[3C\x1b(0\x1b7\x1b(Ba\x1b8q");

        let row = vt.grid().visible_row(0);
        assert_eq!(row[3].ch, '─');
        assert_eq!(vt.capture_text(Some(1)).trim_end(), "   ─");
    }

    #[test]
    fn dec_special_graphics_1049_restore_keeps_primary_saved_charset() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process(b"\x1b(0\x1b[?1049h\x1b(B\x1b7\x1b(0q\x1b8\x1b[?1049lq");

        assert_eq!(vt.capture_text(Some(1)).trim_end(), "─");
    }

    #[test]
    fn dec_special_graphics_does_not_translate_or_narrow_unicode_wide_cells() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process(" \x1b(0你q".as_bytes());

        let row = vt.grid().visible_row(0);
        assert_eq!(row[1].ch, '你');
        assert!(row[1].is_wide());
        assert!(row[2].is_wide_continuation());
        assert_eq!(row[3].ch, '─');
        assert_grid_wide_invariants(vt.grid());
    }

    #[test]
    fn test_sgr_256_color() {
        let mut vt = VirtualTerminal::new(24, 80);
        // Set 256-color foreground: SGR 38;5;196.
        vt.process(b"\x1b[38;5;196mX");
        let cell = &vt.grid().visible_row(0)[0];
        assert_eq!(cell.style.fg, Color::Indexed(196));
    }

    #[test]
    fn test_sgr_background_colors() {
        let mut vt = VirtualTerminal::new(24, 80);
        // 24-bit background.
        vt.process(b"\x1b[48;2;10;20;30mX");
        let cell = &vt.grid().visible_row(0)[0];
        assert_eq!(cell.style.bg, Color::Rgb(10, 20, 30));
    }

    #[test]
    fn test_sgr_bright_colors() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b[91mX"); // Bright red fg.
        let cell = &vt.grid().visible_row(0)[0];
        assert_eq!(cell.style.fg, Color::Indexed(9)); // 8 + 1 = bright red
    }

    #[test]
    fn test_sgr_multiple_attributes() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b[1;3;31mX"); // Bold + Italic + Red.
        let cell = &vt.grid().visible_row(0)[0];
        assert!(cell.style.flags.contains(CellFlags::BOLD));
        assert!(cell.style.flags.contains(CellFlags::ITALIC));
        assert_eq!(cell.style.fg, Color::Indexed(1));
    }

    #[test]
    fn test_erase_in_display() {
        let mut vt = VirtualTerminal::new(3, 10);
        vt.process(b"AAAAAAAAAA\r\nBBBBBBBBBB\r\nCCCCCCCCCC");
        // Move to row 2, col 1 and clear above (ED 1).
        vt.process(b"\x1b[2;1H\x1b[1J");
        // Row 0 should be cleared.
        assert_eq!(vt.grid().visible_row(0)[0].ch, ' ');
    }

    #[test]
    fn test_erase_in_line() {
        let mut vt = VirtualTerminal::new(1, 10);
        vt.process(b"ABCDEFGHIJ");
        vt.process(b"\x1b[1;6H"); // Move to col 5.
        vt.process(b"\x1b[0K"); // Erase from cursor to end.
        assert_eq!(vt.grid().visible_row(0)[4].ch, 'E');
        assert_eq!(vt.grid().visible_row(0)[5].ch, ' ');
        assert_eq!(vt.grid().visible_row(0)[9].ch, ' ');
    }

    #[test]
    fn test_scroll_region() {
        let mut vt = VirtualTerminal::new(5, 10);
        // Set scroll region to lines 2-4 (1-indexed: CSI 2;4r).
        vt.process(b"\x1b[2;4r");
        assert_eq!(vt.scroll_region().top, 1);
        assert_eq!(vt.scroll_region().bottom, 3);
    }

    #[test]
    fn origin_mode_cup_addresses_scroll_region_top() {
        let mut vt = VirtualTerminal::new(10, 20);
        vt.process(b"\x1b[3;6r\x1b[?6h\x1b[1;1H");

        assert_eq!((vt.cursor().row, vt.cursor().col), (2, 0));
    }

    #[test]
    fn origin_mode_cup_clamps_to_scroll_region_bottom() {
        let mut vt = VirtualTerminal::new(10, 20);
        vt.process(b"\x1b[3;6r\x1b[?6h\x1b[99;99H");

        assert_eq!((vt.cursor().row, vt.cursor().col), (5, 19));
    }

    #[test]
    fn cup_outside_origin_mode_remains_absolute() {
        let mut vt = VirtualTerminal::new(10, 20);
        vt.process(b"\x1b[3;6r\x1b[2;4H");

        assert_eq!((vt.cursor().row, vt.cursor().col), (1, 3));
    }

    #[test]
    fn origin_mode_vpa_addresses_scroll_region_top() {
        let mut vt = VirtualTerminal::new(10, 20);
        vt.process(b"\x1b[4;6r\x1b[?6h\x1b[2d");

        assert_eq!(vt.cursor().row, 4);
    }

    #[test]
    fn origin_mode_toggle_homes_to_current_origin() {
        let mut vt = VirtualTerminal::new(10, 20);
        vt.process(b"\x1b[3;6r\x1b[9;9H\x1b[?6h");
        assert_eq!((vt.cursor().row, vt.cursor().col), (2, 0));

        vt.process(b"\x1b[9;9H\x1b[?6l");
        assert_eq!((vt.cursor().row, vt.cursor().col), (0, 0));
    }

    #[test]
    fn scroll_region_set_homes_to_origin_top_when_origin_mode_is_set() {
        let mut vt = VirtualTerminal::new(10, 20);
        vt.process(b"\x1b[?6h\x1b[8;8H\x1b[4;7r");

        assert_eq!(vt.scroll_region().top, 3);
        assert_eq!(vt.scroll_region().bottom, 6);
        assert_eq!((vt.cursor().row, vt.cursor().col), (3, 0));

        vt.process(b"\x1b[r");
        assert_eq!(vt.scroll_region().top, 0);
        assert_eq!(vt.scroll_region().bottom, 9);
        assert_eq!((vt.cursor().row, vt.cursor().col), (0, 0));
    }

    #[test]
    fn invalid_scroll_region_does_not_home_or_change_region() {
        let mut vt = VirtualTerminal::new(10, 20);
        vt.process(b"\x1b[3;6r\x1b[5;5H");
        vt.process(b"\x1b[8;2r");

        assert_eq!(vt.scroll_region().top, 2);
        assert_eq!(vt.scroll_region().bottom, 5);
        assert_eq!((vt.cursor().row, vt.cursor().col), (4, 4));
    }

    #[test]
    fn save_restore_restores_origin_mode_and_grid_clamps_not_scroll_region() {
        let mut vt = VirtualTerminal::new(8, 20);
        vt.process(b"\x1b[2;4r\x1b[?6h\x1b[2;3H\x1b7");
        vt.process(b"\x1b[5;7r\x1b[3;3H\x1b[?6l\x1b8");

        assert!(vt.modes().origin_mode);
        assert_eq!(vt.scroll_region().top, 4);
        assert_eq!(vt.scroll_region().bottom, 6);
        assert_eq!((vt.cursor().row, vt.cursor().col), (2, 2));
    }

    #[test]
    fn relative_vertical_moves_clamp_within_scroll_region_when_started_inside() {
        let mut vt = VirtualTerminal::new(12, 20);
        vt.process(b"\x1b[4;6r");

        vt.process(b"\x1b[5;10H\x1b[99A");
        assert_eq!((vt.cursor().row, vt.cursor().col), (3, 9));

        vt.process(b"\x1b[5;10H\x1b[99B");
        assert_eq!((vt.cursor().row, vt.cursor().col), (5, 9));

        vt.process(b"\x1b[5;10H\x1b[99E");
        assert_eq!((vt.cursor().row, vt.cursor().col), (5, 0));

        vt.process(b"\x1b[5;10H\x1b[99e");
        assert_eq!((vt.cursor().row, vt.cursor().col), (5, 9));

        vt.process(b"\x1b[5;10H\x1b[99F");
        assert_eq!((vt.cursor().row, vt.cursor().col), (3, 0));
    }

    #[test]
    fn relative_vertical_moves_use_directional_scroll_region_bounds_when_started_outside() {
        let mut vt = VirtualTerminal::new(12, 20);
        vt.process(b"\x1b[4;6r");

        vt.process(b"\x1b[2;1H\x1b[8B");
        assert_eq!((vt.cursor().row, vt.cursor().col), (5, 0));

        vt.process(b"\x1b[10;1H\x1b[99A");
        assert_eq!((vt.cursor().row, vt.cursor().col), (3, 0));

        vt.process(b"\x1b[2;1H\x1b[99A");
        assert_eq!((vt.cursor().row, vt.cursor().col), (0, 0));

        vt.process(b"\x1b[10;1H\x1b[99B");
        assert_eq!((vt.cursor().row, vt.cursor().col), (11, 0));
    }

    #[test]
    fn test_auto_wrap() {
        let mut vt = VirtualTerminal::new(3, 5);
        vt.process(b"ABCDE"); // Fills the row exactly.
        // Cursor should be at col 4 with wrap pending.
        assert_eq!(vt.cursor().col, 4);
        // Next character should wrap.
        vt.process(b"F");
        assert_eq!(vt.cursor().row, 1);
        assert_eq!(vt.grid().visible_row(1)[0].ch, 'F');
    }

    #[test]
    fn test_auto_wrap_scrolls_at_bottom() {
        let mut vt = VirtualTerminal::new(3, 5);
        // Fill all 3 rows.
        vt.process(b"ABCDE");
        vt.process(b"FGHIJ");
        vt.process(b"KLMNO");
        // Now one more character should cause scroll.
        vt.process(b"P");
        assert_eq!(vt.grid().scrollback_len(), 1);
        assert_eq!(vt.grid().visible_row(2)[0].ch, 'P');
    }

    #[test]
    fn test_alternate_screen() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"primary content");
        assert!(!vt.is_alternate_screen());
        // DECSET 1049 -- enter alternate screen.
        vt.process(b"\x1b[?1049h");
        assert!(vt.is_alternate_screen());
        vt.process(b"alt content");
        // DECRST 1049 -- leave alternate screen.
        vt.process(b"\x1b[?1049l");
        assert!(!vt.is_alternate_screen());
        assert_eq!(vt.grid().visible_row(0)[0].ch, 'p'); // primary content restored
    }

    #[test]
    fn test_alternate_screen_via_api() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"primary");
        vt.enter_alternate_screen();
        assert!(vt.is_alternate_screen());
        vt.process(b"alt");
        assert_eq!(vt.grid().visible_row(0)[0].ch, 'a');
        vt.leave_alternate_screen();
        assert!(!vt.is_alternate_screen());
        assert_eq!(vt.grid().visible_row(0)[0].ch, 'p');
    }

    #[test]
    fn test_osc_title() {
        let mut vt = VirtualTerminal::new(24, 80);
        // OSC 2 -- set window title.
        vt.process(b"\x1b]2;my window title\x07");
        assert_eq!(vt.title(), Some("my window title"));
    }

    #[test]
    fn test_osc_title_icon_name() {
        let mut vt = VirtualTerminal::new(24, 80);
        // OSC 0 -- set icon name and window title.
        vt.process(b"\x1b]0;icon and title\x07");
        assert_eq!(vt.title(), Some("icon and title"));
    }

    #[test]
    fn test_wide_character() {
        let mut vt = VirtualTerminal::new(24, 80);
        // Write a wide character (CJK character, width 2).
        vt.process("\u{4f60}".as_bytes()); // Unicode for a CJK char
        assert_eq!(vt.grid().visible_row(0)[0].ch, '\u{4f60}');
        assert_eq!(vt.grid().visible_row(0)[0].width, 2);
        assert!(vt.grid().visible_row(0)[1].is_wide_continuation());
        assert_eq!(vt.cursor().col, 2);
    }

    #[test]
    fn wide_overwrite_on_continuation_clears_both_old_pairs() {
        let mut vt = VirtualTerminal::new(2, 8);
        vt.process("你你".as_bytes());
        vt.process("\x1b[1;2H好".as_bytes());

        let row = vt.grid().visible_row(0);
        assert_eq!(row[0].ch, ' ');
        assert_eq!(row[1].ch, '好');
        assert!(row[2].is_wide_continuation());
        assert_eq!(row[3].ch, ' ');
        assert_grid_wide_invariants(vt.grid());
        assert_eq!(compact_capture(&vt), " 好");
    }

    #[test]
    fn final_column_wide_char_wraps_before_writing_when_autowrap_enabled() {
        let mut vt = VirtualTerminal::new(2, 4);
        vt.process("\x1b[1;4H界".as_bytes());

        assert_eq!(vt.grid().visible_row(0)[3].ch, ' ');
        assert!(vt.grid().visible_row(0).wrapped);
        assert_eq!(vt.grid().visible_row(1)[0].ch, '界');
        assert!(vt.grid().visible_row(1)[1].is_wide_continuation());
        assert_grid_wide_invariants(vt.grid());
    }

    #[test]
    fn final_column_wide_char_degrades_to_space_when_autowrap_disabled() {
        let mut vt = VirtualTerminal::new(1, 4);
        vt.process("\x1b[?7l\x1b[1;4H界".as_bytes());

        let cell = &vt.grid().visible_row(0)[3];
        assert_eq!(cell.ch, ' ');
        assert_eq!(cell.width, 1);
        assert_eq!(vt.cursor().col, 3);
        assert!(!vt.cursor().auto_wrap_pending);
        assert_grid_wide_invariants(vt.grid());
    }

    #[test]
    fn zero_width_combining_character_does_not_create_fake_continuation() {
        let mut vt = VirtualTerminal::new(1, 4);
        vt.process("A\u{0301}B".as_bytes());

        assert_eq!(vt.grid().visible_row(0)[0].ch, 'A');
        assert_eq!(vt.grid().visible_row(0)[1].ch, 'B');
        assert_grid_wide_invariants(vt.grid());
    }

    #[test]
    fn rep_of_wide_character_preserves_wide_pairs() {
        let mut vt = VirtualTerminal::new(1, 8);
        vt.process("界\x1b[2b".as_bytes());

        assert_eq!(compact_capture(&vt), "界界界");
        assert_grid_wide_invariants(vt.grid());
    }

    #[test]
    fn test_resize() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"test");
        vt.resize(10, 40);
        assert_eq!(vt.grid().rows(), 10);
        assert_eq!(vt.grid().cols(), 40);
        // Content should be preserved.
        assert_eq!(vt.grid().visible_row(0)[0].ch, 't');
    }

    #[test]
    fn parser_marks_source_row_as_wrapped_before_advancing() {
        let mut vt = VirtualTerminal::new(3, 5);
        vt.process(b"abcdef");

        assert!(vt.grid().visible_row(0).wrapped);
        assert!(!vt.grid().visible_row(1).wrapped);
        assert_eq!(vt.grid().visible_row(0)[4].ch, 'e');
        assert_eq!(vt.grid().visible_row(1)[0].ch, 'f');
    }

    #[test]
    fn resize_reflow_preserves_wrapped_capture_text() {
        let text = "abcdefghijklmnopqrstuvwxyz";
        let mut vt = VirtualTerminal::new(5, 10);
        vt.process(text.as_bytes());

        vt.resize(7, 7);

        assert_eq!(compact_capture(&vt), text);

        vt.resize(7, 13);

        assert_eq!(compact_capture(&vt), text);
    }

    #[test]
    fn resize_keeps_alternate_screen_canvas_separate_from_primary_reflow() {
        let mut vt = VirtualTerminal::new(4, 5);
        vt.process(b"primary-wrap");
        vt.enter_alternate_screen();
        vt.process(b"ALT-CANVAS");

        vt.resize(4, 4);

        assert!(vt.is_alternate_screen());
        assert_eq!(vt.grid().visible_row(0)[0].ch, 'A');
        assert_eq!(vt.grid().visible_row(0)[3].ch, '-');

        vt.leave_alternate_screen();

        assert!(!vt.is_alternate_screen());
        assert_eq!(compact_capture(&vt), "primary-wrap");
    }

    #[test]
    fn resize_preserves_dynamic_default_colors() {
        let mut vt = VirtualTerminal::new(4, 5);
        vt.process(b"\x1b]10;#112233\x1b\\\x1b]11;#445566\x1b\\\x1b]12;#778899\x1b\\");
        vt.process(b"abcdef");

        vt.resize(5, 4);

        assert_eq!(
            vt.default_colors(),
            TerminalDefaultColors {
                fg: Some([0x11, 0x22, 0x33]),
                bg: Some([0x44, 0x55, 0x66]),
                cursor: Some([0x77, 0x88, 0x99]),
            }
        );
        assert_eq!(compact_capture(&vt), "abcdef");
    }

    #[test]
    fn test_resize_resets_scroll_region() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b[5;20r"); // Set custom scroll region.
        vt.resize(10, 40);
        assert_eq!(vt.scroll_region().top, 0);
        assert_eq!(vt.scroll_region().bottom, 9);
    }

    #[test]
    fn test_resize_clamps_cursor() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b[24;80H"); // Cursor at bottom-right.
        vt.resize(10, 40);
        assert!(vt.cursor().row <= 9);
        assert!(vt.cursor().col <= 39);
    }

    #[test]
    fn test_clear_scrollback() {
        let mut vt = VirtualTerminal::new(3, 5);
        // Generate scrollback.
        for _ in 0..10 {
            vt.process(b"\n");
        }
        assert!(vt.scrollback_len() > 0);
        vt.clear_scrollback();
        assert_eq!(vt.scrollback_len(), 0);
    }

    #[test]
    fn test_cursor_save_restore_via_esc() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b[5;10H"); // Move to 5,10.
        vt.process(b"\x1b7"); // Save cursor.
        vt.process(b"\x1b[1;1H"); // Move home.
        vt.process(b"\x1b8"); // Restore cursor.
        assert_eq!(vt.cursor().row, 4);
        assert_eq!(vt.cursor().col, 9);
    }

    #[test]
    fn test_insert_lines() {
        let mut vt = VirtualTerminal::new(5, 10);
        vt.process(b"\x1b[3;1HX"); // Write X at row 3 (0-indexed row 2).
        vt.process(b"\x1b[2;1H"); // Move to row 2 (0-indexed row 1).
        vt.process(b"\x1b[1L"); // Insert 1 line at row 1.
        // Row 1 should be the newly inserted blank line.
        assert_eq!(vt.grid().visible_row(1)[0].ch, ' ');
        // X was at row 2, shifted down to row 3.
        assert_eq!(vt.grid().visible_row(3)[0].ch, 'X');
    }

    #[test]
    fn test_delete_lines() {
        let mut vt = VirtualTerminal::new(5, 10);
        vt.process(b"A\r\nB\r\nC\r\nD\r\nE");
        vt.process(b"\x1b[2;1H"); // Move to row 2.
        vt.process(b"\x1b[1M"); // Delete 1 line.
        assert_eq!(vt.grid().visible_row(1)[0].ch, 'C'); // B removed, C shifted up.
    }

    #[test]
    fn test_insert_characters() {
        let mut vt = VirtualTerminal::new(1, 10);
        vt.process(b"ABCDE");
        vt.process(b"\x1b[1;3H"); // Move to col 3.
        vt.process(b"\x1b[2@"); // Insert 2 characters.
        assert_eq!(vt.grid().visible_row(0)[0].ch, 'A');
        assert_eq!(vt.grid().visible_row(0)[1].ch, 'B');
        assert_eq!(vt.grid().visible_row(0)[2].ch, ' ');
        assert_eq!(vt.grid().visible_row(0)[3].ch, ' ');
        assert_eq!(vt.grid().visible_row(0)[4].ch, 'C');
    }

    #[test]
    fn test_delete_characters() {
        let mut vt = VirtualTerminal::new(1, 10);
        vt.process(b"ABCDE");
        vt.process(b"\x1b[1;2H"); // Move to col 2.
        vt.process(b"\x1b[2P"); // Delete 2 characters.
        assert_eq!(vt.grid().visible_row(0)[0].ch, 'A');
        assert_eq!(vt.grid().visible_row(0)[1].ch, 'D');
        assert_eq!(vt.grid().visible_row(0)[2].ch, 'E');
    }

    #[test]
    fn test_erase_characters() {
        let mut vt = VirtualTerminal::new(1, 10);
        vt.process(b"ABCDE");
        vt.process(b"\x1b[1;2H"); // Move to col 2.
        vt.process(b"\x1b[3X"); // Erase 3 characters.
        assert_eq!(vt.grid().visible_row(0)[0].ch, 'A');
        assert_eq!(vt.grid().visible_row(0)[1].ch, ' ');
        assert_eq!(vt.grid().visible_row(0)[2].ch, ' ');
        assert_eq!(vt.grid().visible_row(0)[3].ch, ' ');
        assert_eq!(vt.grid().visible_row(0)[4].ch, 'E');
    }

    #[test]
    fn test_decawm_disable() {
        let mut vt = VirtualTerminal::new(3, 5);
        vt.process(b"\x1b[?7l"); // Disable auto-wrap.
        vt.process(b"ABCDEFGH"); // Should overwrite last col, not wrap.
        assert_eq!(vt.cursor().row, 0);
        assert_eq!(vt.grid().visible_row(0)[4].ch, 'H');
    }

    #[test]
    fn test_bracketed_paste_mode() {
        let mut vt = VirtualTerminal::new(24, 80);
        assert!(!vt.modes().bracketed_paste);
        vt.process(b"\x1b[?2004h");
        assert!(vt.modes().bracketed_paste);
        vt.process(b"\x1b[?2004l");
        assert!(!vt.modes().bracketed_paste);
    }

    #[test]
    fn test_alternate_scroll_mode() {
        let mut vt = VirtualTerminal::new(24, 80);
        // Defaults on (Debian-xterm / iTerm2 / kitty convention).
        assert!(vt.modes().alternate_scroll);
        vt.process(b"\x1b[?1007l");
        assert!(!vt.modes().alternate_scroll);
        vt.process(b"\x1b[?1007h");
        assert!(vt.modes().alternate_scroll);
    }

    #[test]
    fn test_application_keypad_mode() {
        let mut vt = VirtualTerminal::new(24, 80);
        assert!(!vt.modes().application_keypad);
        vt.process(b"\x1b=");
        assert!(vt.modes().application_keypad);
        vt.process(b"\x1b>");
        assert!(!vt.modes().application_keypad);
    }

    #[test]
    fn test_cursor_shape() {
        let mut vt = VirtualTerminal::new(24, 80);
        assert_eq!(vt.cursor().shape, CursorShape::Block);
        vt.process(b"\x1b[5 q"); // Bar cursor.
        assert_eq!(vt.cursor().shape, CursorShape::Bar);
        vt.process(b"\x1b[3 q"); // Underline cursor.
        assert_eq!(vt.cursor().shape, CursorShape::Underline);
        vt.process(b"\x1b[1 q"); // Block cursor.
        assert_eq!(vt.cursor().shape, CursorShape::Block);
    }

    #[test]
    fn test_scroll_up_generates_scrollback() {
        let mut vt = VirtualTerminal::new(3, 10);
        vt.process(b"AAA\r\nBBB\r\nCCC");
        // At bottom, linefeed should scroll. LF does NOT do CR.
        vt.process(b"\r\nDDD");
        assert_eq!(vt.scrollback_len(), 1);
        // After CR+LF at bottom: scroll up, cursor at row 2 col 0, then write DDD.
        assert_eq!(vt.grid().visible_row(2)[0].ch, 'D');
        // Scrollback should have the first line.
        assert_eq!(vt.grid().scrollback_row(0).unwrap()[0].ch, 'A');
    }

    #[test]
    fn test_ed_clear_entire_screen() {
        let mut vt = VirtualTerminal::new(3, 10);
        vt.process(b"AAA\r\nBBB\r\nCCC");
        vt.process(b"\x1b[2J"); // Clear entire screen.
        assert_eq!(vt.grid().visible_row(0)[0].ch, ' ');
        assert_eq!(vt.grid().visible_row(1)[0].ch, ' ');
        assert_eq!(vt.grid().visible_row(2)[0].ch, ' ');
    }

    #[test]
    fn test_ed_clear_screen_and_scrollback() {
        let mut vt = VirtualTerminal::new(3, 10);
        // Generate scrollback.
        for _ in 0..5 {
            vt.process(b"\n");
        }
        assert!(vt.scrollback_len() > 0);
        vt.process(b"\x1b[3J"); // Clear screen + scrollback.
        assert_eq!(vt.scrollback_len(), 0);
        assert_eq!(vt.grid().visible_row(0)[0].ch, ' ');
    }

    #[test]
    fn test_nel_next_line() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"ABC\x1bE"); // ESC E = Next Line.
        assert_eq!(vt.cursor().row, 1);
        assert_eq!(vt.cursor().col, 0);
    }

    #[test]
    fn test_ind_index() {
        let mut vt = VirtualTerminal::new(3, 10);
        vt.process(b"\x1b[3;1H"); // Move to last row.
        vt.process(b"\x1bD"); // ESC D = Index (linefeed without CR).
        // Should scroll since at bottom.
        assert_eq!(vt.scrollback_len(), 1);
    }

    #[test]
    fn test_vpa_vertical_position() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b[10d"); // VPA to row 10.
        assert_eq!(vt.cursor().row, 9); // 0-indexed
    }

    #[test]
    fn test_cha_cursor_character_absolute() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"ABCDEFG");
        vt.process(b"\x1b[4G"); // CHA to col 4.
        assert_eq!(vt.cursor().col, 3); // 0-indexed
    }

    #[test]
    fn test_cnl_cursor_next_line() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b[5;10H"); // Row 5, Col 10.
        vt.process(b"\x1b[2E"); // CNL 2 -- move down 2 lines, col 0.
        assert_eq!(vt.cursor().row, 6);
        assert_eq!(vt.cursor().col, 0);
    }

    #[test]
    fn test_cpl_cursor_previous_line() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b[5;10H"); // Row 5, Col 10.
        vt.process(b"\x1b[2F"); // CPL 2 -- move up 2 lines, col 0.
        assert_eq!(vt.cursor().row, 2);
        assert_eq!(vt.cursor().col, 0);
    }

    #[test]
    fn dirty_single_print_marks_written_row() {
        let mut vt = VirtualTerminal::new(2, 6);
        vt.process(b"A");

        assert_eq!(
            vt.take_dirty_regions(),
            vec![DirtyRegion { row: 0, cols: 0..6 }]
        );
        assert!(!vt.is_dirty());
    }

    #[test]
    fn dirty_cursor_only_movement_is_out_of_scope() {
        let mut vt = VirtualTerminal::new(2, 6);
        vt.process(b"\x1b[2;3H");

        assert!(vt.take_dirty_regions().is_empty());
    }

    #[test]
    fn dirty_vt_byte_fixture_reports_expected_sequence() {
        let mut vt = VirtualTerminal::new(3, 8);

        vt.process(b"abc");
        assert_eq!(
            vt.take_dirty_regions(),
            vec![DirtyRegion { row: 0, cols: 0..8 }]
        );

        vt.process(b"\x1b[1;2H\x1b[2P");
        assert_eq!(
            vt.take_dirty_regions(),
            vec![DirtyRegion { row: 0, cols: 0..8 }]
        );

        vt.process(b"\x1b[2J");
        assert_eq!(
            vt.take_dirty_regions(),
            vec![
                DirtyRegion { row: 0, cols: 0..8 },
                DirtyRegion { row: 1, cols: 0..8 },
                DirtyRegion { row: 2, cols: 0..8 },
            ]
        );
    }

    #[test]
    fn dirty_rep_and_grapheme_append_dirty_the_target_row() {
        let mut vt = VirtualTerminal::new(2, 8);
        vt.process("e\u{301}".as_bytes());
        assert_eq!(
            vt.take_dirty_regions(),
            vec![DirtyRegion { row: 0, cols: 0..8 }]
        );

        vt.process(b"\x1b[2b");
        assert_eq!(
            vt.take_dirty_regions(),
            vec![DirtyRegion { row: 0, cols: 0..8 }]
        );
    }

    #[test]
    fn dirty_wide_cell_neighbor_repair_dirties_row() {
        let mut vt = VirtualTerminal::new(2, 8);
        vt.process("A界B".as_bytes());
        vt.take_dirty_regions();

        vt.process(b"\x1b[1;3HX");
        assert_eq!(
            vt.take_dirty_regions(),
            vec![DirtyRegion { row: 0, cols: 0..8 }]
        );
        assert_eq!(vt.grid().visible_row(0)[1].ch, ' ');
        assert_eq!(vt.grid().visible_row(0)[2].ch, 'X');
    }

    #[test]
    fn dirty_sync_output_reports_presented_buffer_only_then_full_frame_on_leave() {
        let mut vt = VirtualTerminal::new(2, 6);
        vt.process(b"old");
        vt.take_dirty_regions();

        vt.process(b"\x1b[?2026h\x1b[1;1Hnew");
        assert_eq!(
            vt.take_dirty_regions(),
            vec![
                DirtyRegion { row: 0, cols: 0..6 },
                DirtyRegion { row: 1, cols: 0..6 },
            ]
        );
        assert_eq!(compact_capture(&vt), "old");

        vt.process(b"\x1b[?2026l");
        assert_eq!(
            vt.take_dirty_regions(),
            vec![
                DirtyRegion { row: 0, cols: 0..6 },
                DirtyRegion { row: 1, cols: 0..6 },
            ]
        );
        assert_eq!(compact_capture(&vt), "new");
    }

    #[test]
    fn dirty_alternate_screen_enter_and_leave_are_full_frame() {
        let mut vt = VirtualTerminal::new(2, 5);
        vt.process(b"main");
        vt.take_dirty_regions();

        vt.process(b"\x1b[?1049h");
        assert_eq!(
            vt.take_dirty_regions(),
            vec![
                DirtyRegion { row: 0, cols: 0..5 },
                DirtyRegion { row: 1, cols: 0..5 },
            ]
        );

        vt.process(b"alt");
        vt.take_dirty_regions();
        vt.process(b"\x1b[?1049l");
        assert_eq!(
            vt.take_dirty_regions(),
            vec![
                DirtyRegion { row: 0, cols: 0..5 },
                DirtyRegion { row: 1, cols: 0..5 },
            ]
        );
    }

    #[test]
    fn dirty_default_color_changes_are_full_frame() {
        let mut vt = VirtualTerminal::new(2, 4);
        vt.process(b"cell");
        vt.take_dirty_regions();

        vt.process(b"\x1b]10;#ff0000\x1b\\");
        assert_eq!(
            vt.take_dirty_regions(),
            vec![
                DirtyRegion { row: 0, cols: 0..4 },
                DirtyRegion { row: 1, cols: 0..4 },
            ]
        );

        vt.process(b"\x1b]110\x1b\\");
        assert_eq!(
            vt.take_dirty_regions(),
            vec![
                DirtyRegion { row: 0, cols: 0..4 },
                DirtyRegion { row: 1, cols: 0..4 },
            ]
        );
    }
}

/// Lens ContentRevision substrate — L0 unit coverage of the §4.2 mutation-class
/// table (PRD §4 SPEC-A). Every row of that table is exercised here by feeding
/// escape sequences to a `VirtualTerminal` and asserting a bump / no-bump, plus
/// the batching rule, the identical-repaint rule, and `last_mutation_ns`
/// behaviour (LENS-R-001/002). These L0 tests never diff cell values.
#[cfg(test)]
mod content_revision_tests {
    use super::*;

    fn vt() -> VirtualTerminal {
        VirtualTerminal::new(24, 80)
    }

    // LENS-R-001: revision starts at 1 for a fresh VT.
    #[test]
    fn initial_revision_is_one() {
        assert_eq!(vt().content_revision(), 1);
    }

    // §4.2 Class A — visible cell change (glyph).
    #[test]
    fn class_a_glyph_print_bumps() {
        let mut vt = vt();
        let r = vt.content_revision();
        vt.process(b"X");
        assert_eq!(vt.content_revision(), r + 1);
    }

    // §4.2 Class A — visible cell change (fg/bg/attrs). A style-only recolor of
    // an existing cell, SAME glyph, MUST bump exactly like a glyph change
    // (LENS-R-038 spirit; value-independent write tally).
    #[test]
    fn class_a_style_only_change_bumps() {
        let mut vt = vt();
        vt.process(b"\x1b[HX"); // write 'X' at (1,1)
        let r = vt.content_revision();
        vt.process(b"\x1b[H\x1b[42mX"); // same glyph 'X', green bg
        assert_eq!(vt.content_revision(), r + 1);
    }

    // §4.2 Class A — cursor position change (CUP only, no cell write).
    #[test]
    fn class_a_cursor_move_bumps() {
        let mut vt = vt();
        let r = vt.content_revision();
        vt.process(b"\x1b[5;10H");
        assert_eq!(vt.content_revision(), r + 1);
    }

    // §4.2 Class A — cursor visibility change (DECTCEM).
    #[test]
    fn class_a_cursor_visibility_bumps() {
        let mut vt = vt();
        let r = vt.content_revision();
        vt.process(b"\x1b[?25l"); // hide
        assert_eq!(vt.content_revision(), r + 1);
        let r2 = vt.content_revision();
        vt.process(b"\x1b[?25h"); // show
        assert_eq!(vt.content_revision(), r2 + 1);
    }

    // §4.2 Class A — viewport scroll. SU (CSI S) scrolls WITHOUT moving the
    // cursor, isolating the scroll path from cursor movement.
    #[test]
    fn class_a_scroll_bumps() {
        let mut vt = vt();
        let cursor_before = (vt.cursor().row, vt.cursor().col);
        let r = vt.content_revision();
        vt.process(b"\x1b[S");
        assert_eq!(vt.content_revision(), r + 1);
        assert_eq!(
            (vt.cursor().row, vt.cursor().col),
            cursor_before,
            "SU must not move the cursor — proves the scroll path alone bumped"
        );
    }

    // §4.2 Class A — alternate screen enter/leave (each a batch, each a bump).
    #[test]
    fn class_a_alt_screen_enter_leave_bumps() {
        let mut vt = vt();
        let r = vt.content_revision();
        vt.process(b"\x1b[?1049h");
        assert!(vt.is_alternate_screen());
        assert_eq!(vt.content_revision(), r + 1);
        let r2 = vt.content_revision();
        vt.process(b"\x1b[?1049l");
        assert!(!vt.is_alternate_screen());
        assert_eq!(vt.content_revision(), r2 + 1);
    }

    // §4.2 Class A — pane resize (a real dimension change).
    #[test]
    fn class_a_resize_bumps() {
        let mut vt = vt();
        let r = vt.content_revision();
        vt.resize(30, 100);
        assert_eq!(vt.content_revision(), r + 1);
    }

    // A resize to the SAME dimensions is not a resize event → no bump.
    #[test]
    fn resize_same_dims_no_bump() {
        let mut vt = vt();
        let r = vt.content_revision();
        vt.resize(24, 80);
        assert_eq!(vt.content_revision(), r);
    }

    // Convergence-round pin: resize while the ALTERNATE screen is live must
    // bump. The bump is the explicit dims-compare in VirtualTerminal::resize()
    // (record_class_a_batch on dims_changed) — independent of mark_all_dirty
    // and of which grid is active.
    #[test]
    fn alt_screen_resize_bumps() {
        let mut vt = vt();
        vt.process(b"\x1b[?1049h");
        assert!(vt.is_alternate_screen());
        let r = vt.content_revision();
        vt.resize(30, 100);
        assert_eq!(vt.content_revision(), r + 1);
    }

    // Convergence-round pin: a WIDTH change takes the column-reflow path
    // (rows rewrap; content itself unchanged) and must bump — exactly once,
    // because the VT-level dims-compare fires once per resize() call no
    // matter how many rows the reflow rewrites.
    #[test]
    fn column_reflow_bumps() {
        let mut vt = vt();
        // A line long enough to rewrap when the width halves.
        vt.process(b"the quick brown fox jumps over the lazy dog 0123456789");
        let r = vt.content_revision();
        vt.resize(24, 40); // cols 80 -> 40: reflow path
        assert_eq!(vt.content_revision(), r + 1);
    }

    // §4.2 Class A — "Full repaint with identical resulting cells". Writing the
    // same cell twice (cursor returned to origin each time so cursor state is
    // identical across batches) MUST bump per batch — proving the counter keys
    // off the write, not a cell-value delta (§4.2 "MUST NOT diff to decide").
    #[test]
    fn class_a_identical_repaint_bumps() {
        let mut vt = vt();
        vt.process(b"\x1b[HX\x1b[H"); // write X, cursor back to (0,0)
        let cursor = (vt.cursor().row, vt.cursor().col, vt.cursor().visible);
        let r = vt.content_revision();
        vt.process(b"\x1b[HX\x1b[H"); // identical bytes, identical end cursor
        assert_eq!(
            (vt.cursor().row, vt.cursor().col, vt.cursor().visible),
            cursor,
            "end cursor must be identical so the bump is proven to come from the write"
        );
        assert_eq!(vt.content_revision(), r + 1);
    }

    // §4.2 batching rule — one process() call with MANY Class-A events bumps by
    // exactly 1 (frames, not bytes).
    #[test]
    fn batching_one_bump_per_process_batch() {
        let mut vt = vt();
        let r = vt.content_revision();
        vt.process(b"line one\r\nline two\r\nline three\r\n");
        assert_eq!(vt.content_revision(), r + 1);
    }

    // Two separate process() batches → exactly two bumps (batch granularity).
    #[test]
    fn two_batches_two_bumps() {
        let mut vt = vt();
        let r = vt.content_revision();
        vt.process(b"aaa");
        vt.process(b"bbb");
        assert_eq!(vt.content_revision(), r + 2);
    }

    // §4.2 Class B — OSC title/icon change (no bump).
    #[test]
    fn class_b_osc_title_no_bump() {
        let mut vt = vt();
        let r = vt.content_revision();
        vt.process(b"\x1b]2;my title\x07");
        vt.process(b"\x1b]0;icon+title\x07");
        assert_eq!(vt.content_revision(), r);
    }

    // §4.2 Class B — a title change is Class B whether the pane closes its own
    // synchronized-output window or the deadline closes it. Releasing on the
    // timeout used to record a Class-A batch for a window that had only
    // retitled, so the same OSC moved `ContentRevision` or not depending on
    // nothing but whether `?2026l` arrived.
    #[test]
    fn class_b_osc_title_no_bump_when_a_window_times_out() {
        let clock = test_clock::Frozen::new();
        let mut vt = vt();
        vt.process(b"\x1b[?2026h");
        vt.process(b"\x1b]2;set inside the window\x07");
        let r = vt.content_revision();

        clock.advance_ms(SYNC_UPDATE_TIMEOUT_MS);
        assert!(vt.release_expired_sync(), "the window must expire");

        assert_eq!(vt.title(), Some("set inside the window"));
        assert_eq!(
            vt.content_revision(),
            r,
            "a title-only window must not bump the revision when it times out"
        );
    }

    // The companion: a window that hid a REAL Class-A change still bumps when
    // the deadline closes it, so the guard above cannot pass by doing nothing.
    #[test]
    fn a_timed_out_window_that_hid_a_write_does_bump() {
        let clock = test_clock::Frozen::new();
        let mut vt = vt();
        vt.process(b"\x1b[?2026h");
        vt.process(b"\x1b[1;1Hhidden write");
        let r = vt.content_revision();

        clock.advance_ms(SYNC_UPDATE_TIMEOUT_MS);
        assert!(vt.release_expired_sync());

        assert!(
            vt.content_revision() > r,
            "revealing a hidden write must bump the revision"
        );
    }

    // §4.2 Class B — Bell (no bump).
    #[test]
    fn class_b_bel_no_bump() {
        let mut vt = vt();
        let r = vt.content_revision();
        vt.process(b"\x07");
        assert_eq!(vt.content_revision(), r);
    }

    // §4.2 Class B — OSC 52 clipboard write (no bump).
    #[test]
    fn class_b_osc52_no_bump() {
        let mut vt = vt();
        let r = vt.content_revision();
        vt.process(b"\x1b]52;c;aGVsbG8=\x07");
        assert_eq!(vt.content_revision(), r);
    }

    // §4.2 Class A (RE-ADJUDICATED in P2 — supersedes the P1 Class-B ruling):
    // OSC 10/11/12 dynamic default fg/bg/cursor color changes alter the
    // PRESENTED frame (every rendered pixel resolving Color::Default), so
    // they bump ContentRevision — one bump per changing batch. A repeat set
    // to the SAME color is a net-zero batch (parser change-guards) → no bump.
    #[test]
    fn osc_10_11_12_bumps() {
        let mut vt = vt();
        let r = vt.content_revision();
        vt.process(b"\x1b]10;#ff8800\x07"); // default fg
        assert_eq!(vt.content_revision(), r + 1);
        vt.process(b"\x1b]11;rgb:00/2b/36\x07"); // default bg
        assert_eq!(vt.content_revision(), r + 2);
        vt.process(b"\x1b]12;#00ff00\x07"); // cursor color
        assert_eq!(vt.content_revision(), r + 3);
        // Same value again → net-zero batch → no bump (§4.2 batching rule).
        vt.process(b"\x1b]10;#ff8800\x07");
        assert_eq!(vt.content_revision(), r + 3);
    }

    // §4.2 Class A (P2 re-adjudication, both directions) — OSC 110/111/112
    // dynamic-color RESETS also change the presented default colors when one
    // was set, so they bump too. A reset with nothing set is a no-op batch.
    #[test]
    fn osc_110_111_112_bumps_when_set() {
        let mut vt = vt();
        // Reset with nothing set → nothing changes → no bump.
        let r0 = vt.content_revision();
        vt.process(b"\x1b]110\x07");
        assert_eq!(vt.content_revision(), r0);
        // Set all three, then each reset changes presented colors → bump.
        vt.process(b"\x1b]10;#ff8800\x07\x1b]11;#002b36\x07\x1b]12;#00ff00\x07");
        let r = vt.content_revision();
        vt.process(b"\x1b]110\x07");
        assert_eq!(vt.content_revision(), r + 1);
        vt.process(b"\x1b]111\x07");
        assert_eq!(vt.content_revision(), r + 2);
        vt.process(b"\x1b]112\x07");
        assert_eq!(vt.content_revision(), r + 3);
    }

    // §4.2 (P2 re-adjudication) — a dynamic-color change while synchronized
    // output is active must respect the sync-deferral: no bump while frozen,
    // exactly one deferred bump at mode release (presented-frame semantics).
    #[test]
    fn osc_dynamic_color_defers_under_sync() {
        let mut vt = vt();
        vt.process(b"\x1b[?2026h");
        let r = vt.content_revision();
        vt.process(b"\x1b]10;#ff8800\x07");
        assert_eq!(vt.content_revision(), r, "frozen: no bump during sync");
        vt.process(b"\x1b[?2026l");
        assert_eq!(vt.content_revision(), r + 1, "one deferred bump at release");
    }

    // codex P2 review major — presented-colors compare: a set-then-restore of
    // a dynamic default color INSIDE sync nets to NO presented change, so
    // release must NOT bump. (A raw live-field compare would have flagged
    // each hidden batch and false-bumped at ?2026l.)
    #[test]
    fn osc_color_net_zero_under_sync_no_bump() {
        let mut vt = vt();
        vt.process(b"\x1b]10;#112233\x07"); // baseline set (bumps, pre-sync)
        vt.process(b"\x1b[?2026h");
        let r = vt.content_revision();
        vt.process(b"\x1b]10;#ff0000\x07"); // hidden change
        vt.process(b"\x1b]10;#112233\x07"); // hidden restore — net zero
        vt.process(b"\x1b[?2026l");
        assert_eq!(
            vt.content_revision(),
            r,
            "net-zero hidden color churn must not bump at release"
        );
        // Control: the same churn WITHOUT the restore is a real presented
        // change and must bump exactly once at release.
        vt.process(b"\x1b[?2026h");
        let r2 = vt.content_revision();
        vt.process(b"\x1b]10;#ff0000\x07");
        vt.process(b"\x1b[?2026l");
        assert_eq!(vt.content_revision(), r2 + 1);
    }

    // claude P2 review minor (a) — a color change landing in the SAME batch
    // that opens ?2026h is frozen INTO the presentation (the presented frame
    // visibly changed this batch), so the bump must land NOW, not lag to
    // release; and the release itself (frozen == live) must add nothing.
    #[test]
    fn osc_color_set_then_sync_enter_same_batch_bumps_immediately() {
        let mut vt = vt();
        let r = vt.content_revision();
        vt.process(b"\x1b]10;#ff8800\x07\x1b[?2026h"); // one batch: set + freeze
        assert_eq!(
            vt.default_colors().fg,
            Some([0xff, 0x88, 0x00]),
            "the frozen presentation carries the new color"
        );
        assert_eq!(
            vt.content_revision(),
            r + 1,
            "presented change must bump in the same batch, not lag to release"
        );
        vt.process(b"\x1b[?2026l");
        assert_eq!(
            vt.content_revision(),
            r + 1,
            "release with no hidden changes adds nothing"
        );
    }

    // codex P2 review blocker — presented alt-screen consistency: an alt
    // toggle inside ?2026h must not leak the future flag against the frozen
    // frame. The presented reader surface (grid/cursor/colors/alt flag — what
    // pane.glance clones) stays the frozen primary frame until release.
    #[test]
    fn sync_alt_toggle_glance_consistency() {
        let mut vt = vt();
        vt.process(b"primary");
        vt.process(b"\x1b[?2026h");
        vt.process(b"\x1b[?1049h\x1b[2JALT-CONTENT");
        assert!(
            vt.modes().alternate_screen,
            "live mode flag flips immediately"
        );
        assert!(
            !vt.is_alternate_screen(),
            "presented alt flag stays frozen until release"
        );
        // Flag and pixels must come from the SAME frozen frame: the
        // presented grid still shows the primary content.
        assert!(
            vt.capture_text(None).contains("primary"),
            "presented grid is still the frozen primary frame"
        );
        vt.process(b"\x1b[?2026l");
        assert!(
            vt.is_alternate_screen(),
            "release presents the alt frame's flag"
        );
        assert!(
            vt.capture_text(None).contains("ALT-CONTENT"),
            "release presents the alt frame's pixels"
        );
    }

    // Guard for the mark_all_dirty decoupling: RIS (ESC c) is a real repaint
    // (clears every visible cell) and must STILL bump — via clear_visible's
    // own write tally, not via mark_all_dirty.
    #[test]
    fn ris_full_reset_still_bumps() {
        let mut vt = vt();
        vt.process(b"hello");
        let r = vt.content_revision();
        vt.process(b"\x1bc");
        assert_eq!(vt.content_revision(), r + 1);
    }

    // Council addendum — OSC 4 palette redefinition also fires mark_all_dirty
    // and REMAINS Class B (P2 re-adjudication kept it: known limitation —
    // palette redefinition can alter indexed-color pixels without a bump).
    // (OSC 104 palette reset is not handled by the parser, so only the set
    // path exists to test.)
    #[test]
    fn osc_4_palette_no_bump() {
        let mut vt = vt();
        let r = vt.content_revision();
        vt.process(b"\x1b]4;1;#ff0000\x07");
        vt.process(b"\x1b]4;4;rgb:24/72/c8\x07");
        assert_eq!(vt.content_revision(), r);
    }

    // Council addendum — DOCUMENTED semantics, not adjudicated correctness:
    // alt-screen enter+leave within ONE process() batch nets to zero (same
    // alt flag, same cursor, same primary-grid tally at the batch boundary)
    // → NO bump. Consistent with the adjudicated net-zero-batch rule: batch
    // boundaries compare end states; transient intra-batch states are not
    // events. Flagged in the P1 report; revisit only via spec change.
    #[test]
    fn alt_screen_double_toggle_one_batch_no_bump() {
        let mut vt = vt();
        let r = vt.content_revision();
        vt.process(b"\x1b[?1049h\x1b[?1049l");
        assert!(!vt.is_alternate_screen());
        assert_eq!(vt.content_revision(), r);
    }

    // LENS-R-002 — last_mutation_ns is never 0, even for the VT whose
    // construction initializes the monotonic epoch (clamped to >= 1).
    #[test]
    fn monotonic_ns_never_zero() {
        let vt = vt();
        assert!(vt.last_mutation_ns() >= 1);
    }

    // §4.2 (adjudicated) — writes under synchronized output (CSI ?2026h) are
    // hidden by the frozen presentation and must NOT bump; the bump is
    // deferred to mode release as exactly ONE batch (revision tracks the
    // PRESENTED frame, matching grid()/cursor()'s frozen view).
    #[test]
    fn sync_output_defers_bump() {
        let mut vt = vt();
        vt.process(b"\x1b[?2026h");
        let r = vt.content_revision();
        vt.process(b"hidden frame one");
        vt.process(b"hidden frame two");
        assert_eq!(
            vt.content_revision(),
            r,
            "writes during synchronized output must not bump"
        );
        vt.process(b"\x1b[?2026l");
        assert_eq!(
            vt.content_revision(),
            r + 1,
            "release must record the hidden writes as exactly ONE batch"
        );
    }

    // §4.2 (adjudicated) — a 2026h/2026l cycle with NOTHING hidden in between
    // presents the same frame it started with: no bump at all.
    #[test]
    fn sync_output_no_writes_no_bump() {
        let mut vt = vt();
        let r = vt.content_revision();
        vt.process(b"\x1b[?2026h");
        vt.process(b"\x1b[?2026l");
        assert_eq!(vt.content_revision(), r);
    }

    // Council major 3 — a combining mark whose target cell is BLANK commits
    // no write and must not bump: row ACCESS is not a write. (Before the
    // fix, append_zero_width_scalar took the tally-bumping mutable row and
    // then returned without writing.)
    #[test]
    fn combining_mark_on_blank_no_bump() {
        let mut vt = vt();
        vt.process(b"\x1b[1;6H"); // park cursor at col 6 (Class-A bump)
        let cursor = (vt.cursor().row, vt.cursor().col);
        let r = vt.content_revision();
        vt.process("\u{0301}".as_bytes()); // combining acute; preceding cell blank
        assert_eq!(
            (vt.cursor().row, vt.cursor().col),
            cursor,
            "zero-width scalar must not move the cursor"
        );
        assert_eq!(vt.content_revision(), r);
    }

    // Companion guard: the same combining mark ON A REAL GLYPH does commit a
    // write and must bump (the major-3 fix must not swallow real joins).
    #[test]
    fn combining_mark_on_glyph_bumps() {
        let mut vt = vt();
        vt.process(b"e");
        let r = vt.content_revision();
        vt.process("\u{0301}".as_bytes()); // e + combining acute → é
        assert_eq!(vt.content_revision(), r + 1);
    }

    // §4.2 Class B — cursor style/shape change DECSCUSR (no bump).
    #[test]
    fn class_b_decscusr_no_bump() {
        let mut vt = vt();
        let r = vt.content_revision();
        vt.process(b"\x1b[2 q"); // steady block
        vt.process(b"\x1b[5 q"); // blinking bar
        assert_eq!(vt.content_revision(), r);
    }

    // §4.2 Class B — mode toggles: mouse reporting, bracketed paste, focus
    // events, kitty keyboard (no bump).
    #[test]
    fn class_b_mode_toggles_no_bump() {
        let mut vt = vt();
        let r = vt.content_revision();
        vt.process(b"\x1b[?1000h"); // mouse: normal tracking
        vt.process(b"\x1b[?1002h"); // mouse: button-event tracking
        vt.process(b"\x1b[?1003h"); // mouse: any-event tracking
        vt.process(b"\x1b[?1006h"); // mouse: SGR encoding
        vt.process(b"\x1b[?2004h"); // bracketed paste
        vt.process(b"\x1b[?1004h"); // focus events
        vt.process(b"\x1b[>1u"); // kitty keyboard push
        assert_eq!(vt.content_revision(), r);
    }

    // SGR pen change ALONE (no cell written) is not a visible cell change → no
    // bump. The bump only lands when a cell is subsequently written.
    #[test]
    fn sgr_pen_only_no_bump() {
        let mut vt = vt();
        let r = vt.content_revision();
        vt.process(b"\x1b[31;1m"); // bold red pen, no glyph
        assert_eq!(vt.content_revision(), r);
    }

    // An empty process() batch produces no Class-A event → no bump.
    #[test]
    fn empty_process_no_bump() {
        let mut vt = vt();
        let r = vt.content_revision();
        vt.process(b"");
        assert_eq!(vt.content_revision(), r);
    }

    // LENS-R-002: last_mutation_ns is stamped on Class-A batches and left
    // untouched by Class-B batches (settle must not reset on metadata noise).
    #[test]
    fn last_mutation_ns_updates_on_class_a_not_class_b() {
        let mut vt = vt();
        let ns_init = vt.last_mutation_ns();
        vt.process(b"hello");
        let ns_after_a = vt.last_mutation_ns();
        assert!(
            ns_after_a >= ns_init,
            "monotonic clock must not go backwards ({ns_init} -> {ns_after_a})"
        );
        assert!(vt.content_revision() > 1, "Class-A batch must have bumped");
        // A Class-B batch must NOT move the clock.
        vt.process(b"\x1b]2;title\x07");
        assert_eq!(
            vt.last_mutation_ns(),
            ns_after_a,
            "Class-B metadata must not update last_mutation_ns"
        );
    }
}
