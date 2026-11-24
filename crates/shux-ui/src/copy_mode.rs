//! Copy mode — vim-style cursor + selection on one pane, yanking via OSC 52
//! (terminal-driven system clipboard).
//!
//! Activated by `prefix + [` (tmux convention) → `ActionKind::EnterCopyMode`,
//! or by the wheel over a pane with history. Modal copy mode freezes the
//! slice of the pane the client was shown when it began ([`CopyView`]) and
//! reads the presented buffer through it while the application keeps
//! writing: its text, highlight, cursor, hit-testing and yank all go through
//! that one view. Cursor and anchor are SOURCE coordinates (column, presented
//! line), so moving the view keeps a selection.
//!
//! A mouse selection outside copy mode is a [`Selection`] over the slice one
//! frame showed.

use std::io::Write as _;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

use shux_core::layout::Rect;
use shux_core::model::PaneId;
use shux_core::theme::{Rgb, Theme};
use std::borrow::Cow;

use shux_vt::{Cell, CellFlags, Color, Row, VirtualTerminal};

use crate::viewport::{PaneViewport, blank_like};

/// A mouse selection, its ends local to the slice it was made over.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Selection {
    pub cursor: (u16, u16),
    pub anchor: Option<(u16, u16)>,
}

/// The slice of one pane modal copy mode reads, frozen when it began.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyView {
    /// The pane's viewport in the frame copy mode began from.
    pub entry: PaneViewport,
    /// Presented line (history counted from 0) at the top when it began.
    pub entry_top: usize,
    /// Presented line at the top now.
    pub top: usize,
    /// First source column shown.
    pub col: usize,
    /// The presented history's eviction count both lines are counted against.
    evicted: u64,
    alt_screen: bool,
}

/// Where a pane's presented lines stood when a frame drew it: output since
/// moves its lines further into history, and eviction moves them back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Drawn {
    pub history: usize,
    pub evicted: u64,
}

impl Drawn {
    pub fn of(vt: &VirtualTerminal) -> Self {
        Self {
            history: history_len(vt),
            evicted: vt.presented_evicted(),
        }
    }
}

impl CopyView {
    /// `entry`, drawn when `vt`'s lines stood at `drawn`, over `vt` as it
    /// presents now; `None` once `vt`'s grid is not the one `entry` showed.
    pub fn new(entry: PaneViewport, drawn: Drawn, vt: &VirtualTerminal) -> Option<Self> {
        let grid = vt.grid();
        if (grid.rows(), grid.cols()) != (entry.grid_rows, entry.grid_cols) {
            return None;
        }
        let top = drawn.history + entry.row_offset;
        let view = Self {
            entry,
            entry_top: top,
            top,
            col: entry.col_offset,
            evicted: drawn.evicted,
            alt_screen: vt.is_alternate_screen(),
        };
        Some(view.rebased(vt))
    }

    pub fn rect(&self) -> Rect {
        self.entry.rect
    }

    /// Lines shown: as many as the frame it began from showed.
    pub fn rows(&self) -> usize {
        self.entry.shown_rows()
    }

    pub fn cols(&self) -> usize {
        self.entry.shown_cols()
    }

    /// Whether this view still names the cells it did in a frame drawing the
    /// pane in `rect`: the same rect, grid size and screen.
    pub fn maps(&self, rect: Rect, vt: &VirtualTerminal) -> bool {
        let grid = vt.grid();
        rect == self.entry.rect
            && (grid.rows(), grid.cols()) == (self.entry.grid_rows, self.entry.grid_cols)
            && vt.is_alternate_screen() == self.alt_screen
    }

    fn evicted_since(&self, vt: &VirtualTerminal) -> usize {
        let gone = vt.presented_evicted().saturating_sub(self.evicted);
        usize::try_from(gone).unwrap_or(usize::MAX)
    }

    /// This view with its lines counted against `vt`'s history now.
    pub fn rebased(self, vt: &VirtualTerminal) -> Self {
        let gone = self.evicted_since(vt);
        Self {
            top: self.top.saturating_sub(gone),
            entry_top: self.entry_top.saturating_sub(gone),
            evicted: vt.presented_evicted(),
            ..self
        }
    }

    /// The last line readable at the bottom, above the hint bar.
    fn max_top(&self, vt: &VirtualTerminal) -> usize {
        vt.presented_total_lines().saturating_sub(self.readable())
    }

    /// Lines a page moves: the hint bar covers the last row of a slice as
    /// tall as its rect.
    fn readable(&self) -> usize {
        let rows = self.rows();
        if rows >= usize::from(self.entry.rect.height) {
            rows.saturating_sub(1).max(1)
        } else {
            rows.max(1)
        }
    }

    /// Source `(column, line)` of the shown cell at pane-local `(x, y)`,
    /// pinned onto the slice's readable rows.
    pub fn source(&self, (x, y): (u16, u16)) -> (usize, usize) {
        (
            self.col + usize::from(x).min(self.cols().saturating_sub(1)),
            self.top + usize::from(y).min(self.readable() - 1),
        )
    }

    /// Pane-local position of source `(column, line)`, if the slice shows it.
    pub fn local(&self, (col, line): (usize, usize)) -> Option<(u16, u16)> {
        let x = col.checked_sub(self.col).filter(|&x| x < self.cols())?;
        let y = line.checked_sub(self.top).filter(|&y| y < self.rows())?;
        Some((x as u16, y as u16))
    }

    /// The viewport the frame draws the pane with, and whether the copy view
    /// is drawn over it: a slice reaching into history, or past the grid's
    /// own columns, shows lines the grid does not. Pictures follow it, so
    /// they keep the copy view's column and, under history, the frozen rows.
    pub fn frame_view(&self, vt: &VirtualTerminal) -> (PaneViewport, bool) {
        let history = history_len(vt);
        let within = self.top >= history;
        let row_offset = if within {
            self.top - history
        } else {
            self.entry.row_offset
        };
        let max_col = self
            .entry
            .grid_cols
            .saturating_sub(self.entry.rect.width.into());
        let view = PaneViewport {
            row_offset,
            col_offset: self.col,
            ..self.entry
        };
        (view, !within || self.col > max_col)
    }

    /// The cell drawn at local column `x` of presented `line`: a wide glyph
    /// cut by either edge of the slice is its blank half.
    fn cell<'v>(&self, vt: &'v VirtualTerminal, x: usize, line: usize) -> Option<Cow<'v, Cell>> {
        if x >= self.cols() {
            return None;
        }
        let row = vt.presented_row(line)?;
        let c = self.col + x;
        let cell = row.get(c)?;
        Some(if x == 0 && c > 0 && cell.is_wide_continuation() {
            Cow::Owned(blank_like(&row[c - 1]))
        } else if x + 1 == self.cols() && cell.width == 2 {
            Cow::Owned(blank_like(cell))
        } else {
            Cow::Borrowed(cell)
        })
    }
}

fn history_len(vt: &VirtualTerminal) -> usize {
    vt.presented_total_lines().saturating_sub(vt.grid().rows())
}

/// One client's modal copy mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyModeState {
    /// The pane it opened on; it ends when focus leaves it.
    pub pane: PaneId,
    pub view: CopyView,
    /// Source `(column, line)` of the copy cursor, always inside the slice.
    pub cursor: (usize, usize),
    /// Source `(column, line)` the selection started at.
    pub anchor: Option<(usize, usize)>,
    /// Active `/` or `?` prompt while the user is typing a search.
    pub search: Option<SearchState>,
    /// Last accepted search, used by `n` / `N`.
    pub last_search: Option<SearchMatch>,
    /// Tracks the first `g` for the vim-style `gg` top-of-history motion.
    pending_g: bool,
    /// Opened by the wheel rather than `prefix [`: it ends, handing the
    /// keyboard back, once the wheel brings the view back down to where it
    /// began. Starting a search makes it deliberate.
    pub wheel_initiated: bool,
    /// A selection was dropped, its lines evicted, and none started since.
    selection_evicted: bool,
}

impl CopyModeState {
    /// Copy mode on `pane` over the slice `entry` showed of `vt` when its
    /// lines stood at `drawn`, its cursor at the slice's top-left. `None`
    /// when `vt` no longer has that grid.
    pub fn enter(
        pane: PaneId,
        entry: PaneViewport,
        drawn: Drawn,
        vt: &VirtualTerminal,
    ) -> Option<Self> {
        let view = CopyView::new(entry, drawn, vt)?;
        Some(Self {
            pane,
            view,
            cursor: (view.col, view.top),
            anchor: None,
            search: None,
            last_search: None,
            pending_g: false,
            wheel_initiated: false,
            selection_evicted: false,
        })
    }

    /// This state counted against `vt`'s history now. A selection with
    /// either end on a line since evicted is dropped.
    pub fn rebase(&mut self, vt: &VirtualTerminal) {
        let gone = self.view.evicted_since(vt);
        if gone == 0 {
            return;
        }
        self.view = self.view.rebased(vt);
        let cursor_kept = self.cursor.1 >= gone;
        self.cursor.1 = self.cursor.1.saturating_sub(gone);
        if let Some((col, line)) = self.anchor {
            self.anchor = line
                .checked_sub(gone)
                .filter(|_| cursor_kept)
                .map(|line| (col, line));
            self.selection_evicted = self.anchor.is_none();
        }
        self.keep_cursor_in_view();
    }

    /// The cursor's position in the slice.
    pub fn cursor_local(&self) -> (u16, u16) {
        self.view.local(self.cursor).unwrap_or((0, 0))
    }

    /// Nothing done in it yet: no selection, search or move off where it
    /// opened, so opening it again elsewhere loses nothing.
    pub fn is_untouched(&self) -> bool {
        let v = &self.view;
        self.anchor.is_none()
            && self.search.is_none()
            && self.last_search.is_none()
            && (v.top, v.col) == (v.entry_top, v.entry.col_offset)
            && self.cursor == (v.col, v.top)
    }

    /// Put the cursor on source `(column, line)`, pinned onto the slice.
    pub fn set_cursor(&mut self, at: (usize, usize)) {
        self.cursor = at;
        self.keep_cursor_in_view();
    }

    fn keep_cursor_in_view(&mut self) {
        let v = self.view;
        let (col, line) = self.cursor;
        self.cursor = (
            col.clamp(v.col, v.col + v.cols().saturating_sub(1)),
            line.clamp(v.top, v.top + v.readable() - 1),
        );
    }

    /// Move the view's top to `top`, carrying the cursor with it.
    fn scroll_to(&mut self, top: usize) -> bool {
        let old = self.view.top;
        if top == old {
            return false;
        }
        self.view.top = top;
        self.cursor.1 = (self.cursor.1 + top).saturating_sub(old);
        self.keep_cursor_in_view();
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchState {
    pub direction: SearchDirection,
    pub query: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchDirection {
    Forward,
    Backward,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMatch {
    pub direction: SearchDirection,
    pub query: String,
}

/// What the input handler decided to do with a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyKey {
    /// Cursor / selection updated; redraw needed.
    Updated,
    /// User pressed `y` — caller should extract the selection text
    /// and emit OSC 52, then exit copy mode.
    Yank,
    /// User pressed `q` / `Esc` — caller should exit copy mode.
    Exit,
    /// `y` with nothing to yank: the selection's lines left history. Copy
    /// mode stays, and the caller says why nothing was copied.
    SelectionEvicted,
    /// Key not bound; ignored.
    Ignored,
}

/// Decode raw bytes from a single Input frame and update `state`, reading
/// `vt` through its view. The caller has checked the view still maps `vt`.
///
/// Only the first byte that maps to a known action is consumed; everything
/// else is dropped. This keeps key-repeat behavior predictable when terminals
/// send escape sequences for arrow keys.
pub fn handle_key(bytes: &[u8], state: &mut CopyModeState, vt: &VirtualTerminal) -> CopyKey {
    if bytes.is_empty() {
        return CopyKey::Ignored;
    }
    state.rebase(vt);
    if state.search.is_some() {
        return handle_search_key(bytes, state, vt);
    }
    let page = state.view.readable();

    // Arrow keys arrive as the 3-byte sequence ESC [ A/B/C/D. Match
    // the prefix first so users can navigate with arrows AND hjkl.
    if bytes.len() >= 3 && bytes[0] == 0x1b && bytes[1] == b'[' {
        match bytes[2] {
            b'A' => return move_by(state, 0, -1).then_updated(),
            b'B' => return move_by(state, 0, 1).then_updated(),
            b'C' => return move_by(state, 1, 0).then_updated(),
            b'D' => return move_by(state, -1, 0).then_updated(),
            _ => {}
        }
    }
    // PageUp/PageDown arrive as ESC [ 5 ~ / ESC [ 6 ~.
    if bytes.len() >= 4 && bytes[0] == 0x1b && bytes[1] == b'[' && bytes[3] == b'~' {
        match bytes[2] {
            b'5' => return scroll_up(state, page).then_updated(),
            b'6' => return scroll_down(state, vt, page).then_updated(),
            _ => {}
        }
    }

    match bytes[0] {
        b'h' => move_by(state, -1, 0).then_updated(),
        b'j' => move_by(state, 0, 1).then_updated(),
        b'k' => move_by(state, 0, -1).then_updated(),
        b'l' => move_by(state, 1, 0).then_updated(),
        b'/' | b'?' => {
            state.pending_g = false;
            state.wheel_initiated = false;
            let direction = if bytes[0] == b'/' {
                SearchDirection::Forward
            } else {
                SearchDirection::Backward
            };
            state.search = Some(SearchState {
                direction,
                query: String::new(),
            });
            CopyKey::Updated
        }
        b'n' => repeat_search(state, vt, false).then_updated(),
        b'N' => repeat_search(state, vt, true).then_updated(),
        // Ctrl-b / Ctrl-f: full-page up/down. Ctrl-u / Ctrl-d: half-page.
        0x02 => scroll_up(state, page).then_updated(),
        0x06 => scroll_down(state, vt, page).then_updated(),
        0x15 => scroll_up(state, page.div_ceil(2)).then_updated(),
        0x04 => scroll_down(state, vt, page.div_ceil(2)).then_updated(),
        b'g' => {
            if state.pending_g {
                state.pending_g = false;
                state.scroll_to(0);
                state.cursor = (state.view.col, 0);
                CopyKey::Updated
            } else {
                state.pending_g = true;
                CopyKey::Ignored
            }
        }
        b'G' => {
            state.pending_g = false;
            state.scroll_to(state.view.max_top(vt));
            let v = state.view;
            state.cursor = (v.col, v.top + v.readable() - 1);
            CopyKey::Updated
        }
        b'v' => {
            state.pending_g = false;
            state.selection_evicted = false;
            state.anchor = match state.anchor {
                Some(_) => None,
                None => Some(state.cursor),
            };
            CopyKey::Updated
        }
        b'y' => {
            state.pending_g = false;
            if state.selection_evicted {
                CopyKey::SelectionEvicted
            } else {
                CopyKey::Yank
            }
        }
        b'q' | 0x1b => {
            state.pending_g = false;
            CopyKey::Exit
        }
        _ => {
            state.pending_g = false;
            CopyKey::Ignored
        }
    }
}

fn handle_search_key(bytes: &[u8], state: &mut CopyModeState, vt: &VirtualTerminal) -> CopyKey {
    let Some(mut search) = state.search.take() else {
        return CopyKey::Ignored;
    };
    match bytes[0] {
        b'\r' | b'\n' => {
            if search.query.is_empty() {
                return CopyKey::Ignored;
            }
            let matched = find_and_focus(state, vt, &search.query, search.direction);
            state.last_search = Some(SearchMatch {
                direction: search.direction,
                query: search.query,
            });
            matched.then_updated()
        }
        0x1b => CopyKey::Updated,
        0x7f | 0x08 => {
            search.query.pop();
            state.search = Some(search);
            CopyKey::Updated
        }
        byte if byte >= b' ' => {
            let typed = std::str::from_utf8(bytes)
                .ok()
                .filter(|text| !text.chars().any(char::is_control));
            let key = match typed {
                Some(text) => {
                    search.query.push_str(text);
                    CopyKey::Updated
                }
                None => CopyKey::Ignored,
            };
            state.search = Some(search);
            key
        }
        _ => {
            state.search = Some(search);
            CopyKey::Ignored
        }
    }
}

fn repeat_search(state: &mut CopyModeState, vt: &VirtualTerminal, reverse: bool) -> bool {
    let Some(last) = state.last_search.clone() else {
        return false;
    };
    let direction = match (last.direction, reverse) {
        (d, false) => d,
        (SearchDirection::Forward, true) => SearchDirection::Backward,
        (SearchDirection::Backward, true) => SearchDirection::Forward,
    };
    find_and_focus(state, vt, &last.query, direction)
}

/// A match: its presented line and the first and last cells it covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Hit {
    line: usize,
    start: usize,
    end: usize,
}

fn find_and_focus(
    state: &mut CopyModeState,
    vt: &VirtualTerminal,
    query: &str,
    direction: SearchDirection,
) -> bool {
    let total = vt.presented_total_lines();
    if query.is_empty() || total == 0 {
        return false;
    }
    let (col, line) = state.cursor;
    let line = line.min(total - 1);
    let forward = direction == SearchDirection::Forward;
    // Every line once from the cursor's, the way the search reads, then
    // back onto the cursor's own: matches are told apart by first column.
    let hit = (0..=total).find_map(|step| {
        let at = if forward {
            (line + step) % total
        } else {
            (line + total - step % total) % total
        };
        let matches = row_matches(vt.presented_row(at)?, query);
        let (start, end) = match (forward, step) {
            (true, 0) => matches.iter().find(|m| m.0 > col),
            (true, s) if s == total => matches.iter().find(|m| m.0 <= col),
            (true, _) => matches.first(),
            (false, 0) => matches.iter().rev().find(|m| m.0 < col),
            (false, s) if s == total => matches.iter().rev().find(|m| m.0 >= col),
            (false, _) => matches.last(),
        }
        .copied()?;
        Some(Hit {
            line: at,
            start,
            end,
        })
    });
    match hit {
        Some(hit) => {
            reveal(state, vt, hit);
            true
        }
        None => false,
    }
}

/// Every match of `query` in a whole row, as the first and last cells its
/// text occupies: a wide glyph owns both of its cells.
fn row_matches(row: &Row, query: &str) -> Vec<(usize, usize)> {
    let mut text = String::new();
    let mut owner = Vec::new();
    for c in 0..row.len() {
        let cell = &row[c];
        if cell.is_wide_continuation() {
            continue;
        }
        cell.push_display_text(&mut text);
        owner.resize(text.len(), c);
    }
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = text.get(from..).and_then(|rest| rest.find(query)) {
        let begin = from + at;
        let last = owner[begin + query.len() - 1];
        let end = if row[last].width == 2 { last + 1 } else { last };
        out.push((owner[begin], end));
        from = begin + text[begin..].chars().next().map_or(1, char::len_utf8);
    }
    out
}

/// Bring a hit into the slice and put the cursor on its first cell. The view
/// moves only as far as it must: vertically not at all while the hit is
/// readable, else to centre it; across just enough to show it whole, or its
/// first cell when it is wider than the slice.
fn reveal(state: &mut CopyModeState, vt: &VirtualTerminal, hit: Hit) {
    let v = state.view;
    let readable = v.readable();
    if hit.line < v.top || hit.line >= v.top + readable {
        let top = hit.line.saturating_sub(readable.saturating_sub(1) / 2);
        state.view.top = top.min(v.max_top(vt));
    }
    let cols = v.cols();
    if hit.start < v.col || hit.end >= v.col + cols {
        state.view.col = if hit.start < v.col || hit.end - hit.start >= cols {
            hit.start
        } else {
            hit.end + 1 - cols
        };
    }
    state.cursor = (hit.start, hit.line);
    state.pending_g = false;
}

fn move_by(state: &mut CopyModeState, dx: isize, dy: isize) -> bool {
    state.pending_g = false;
    let old = state.cursor;
    state.cursor = (
        old.0.saturating_add_signed(dx),
        old.1.saturating_add_signed(dy),
    );
    state.keep_cursor_in_view();
    state.cursor != old
}

pub fn scroll_up(state: &mut CopyModeState, lines: usize) -> bool {
    state.pending_g = false;
    state.scroll_to(state.view.top.saturating_sub(lines.max(1)))
}

pub fn scroll_down(state: &mut CopyModeState, vt: &VirtualTerminal, lines: usize) -> bool {
    state.pending_g = false;
    let max = state.view.max_top(vt);
    let top = state.view.top.saturating_add(lines.max(1)).min(max);
    top > state.view.top && state.scroll_to(top)
}

/// Which grid cell a pane-local cell names.
#[derive(Clone, Copy)]
enum Slice<'a> {
    /// Modal copy mode's view.
    Copy(&'a CopyView),
    /// The slice a frame showed, so a mouse selection reads what it covered.
    Frame(&'a PaneViewport),
}

impl Slice<'_> {
    fn cell<'v>(&self, vt: &'v VirtualTerminal, x: u16, y: u16) -> Option<Cow<'v, Cell>> {
        match *self {
            Slice::Copy(view) => {
                if usize::from(y) >= view.rows() {
                    return None;
                }
                view.cell(vt, x.into(), view.top + usize::from(y))
            }
            Slice::Frame(view) => {
                let (x, y) = (usize::from(x), usize::from(y));
                let (row, col) = (view.row_offset + y, view.col_offset + x);
                // A view from before a resize can name cells the grid no
                // longer has.
                let on_grid = row < vt.grid().rows() && col < vt.grid().cols();
                (on_grid && y < view.shown_rows() && x < view.shown_cols())
                    .then(|| view.shown_cell(vt.grid().visible_row(row), col))
            }
        }
    }
}

trait BoolExt {
    fn then_updated(self) -> CopyKey;
}
impl BoolExt for bool {
    fn then_updated(self) -> CopyKey {
        if self {
            CopyKey::Updated
        } else {
            CopyKey::Ignored
        }
    }
}

/// The text modal copy mode's selection covers: inclusive at both ends, the
/// cursor's cell alone with no anchor. Every line reads the columns the view
/// shows, as drawn, widened to both ends when a pan has left one outside it.
pub fn extract_selection(vt: &VirtualTerminal, state: &CopyModeState) -> String {
    let v = &state.view;
    let ends = ordered(state.anchor.unwrap_or(state.cursor), state.cursor);
    let ((a, _), (b, _)) = ends;
    let (shown_first, shown_last) = (v.col, v.col + v.cols().saturating_sub(1));
    let (first, last) = (shown_first.min(a).min(b), shown_last.max(a).max(b));
    let cell = |c: usize, line: usize| -> Option<Cow<'_, Cell>> {
        let row = vt.presented_row(line)?;
        let cell = row.get(c).filter(|_| (first..=last).contains(&c))?;
        // A wide glyph the view's own edge cuts is drawn as its blank half.
        Some(
            if c == shown_first && c > 0 && cell.is_wide_continuation() {
                Cow::Owned(blank_like(&row[c - 1]))
            } else if c == shown_last && cell.width == 2 {
                Cow::Owned(blank_like(cell))
            } else {
                Cow::Borrowed(cell)
            },
        )
    };
    read(ends, first, |line| span(ends, line, first, last), cell)
}

/// A selection's ends, as `(column, line)`, in reading order.
type Ends = ((usize, usize), (usize, usize));

fn ordered(anchor: (usize, usize), cursor: (usize, usize)) -> Ends {
    if (anchor.1, anchor.0) <= (cursor.1, cursor.0) {
        (anchor, cursor)
    } else {
        (cursor, anchor)
    }
}

/// The columns of `line` a selection covers, within `first..=last`.
fn span(
    ((start_col, start), (end_col, end)): Ends,
    line: usize,
    first: usize,
    last: usize,
) -> Option<(usize, usize)> {
    if line < start || line > end {
        return None;
    }
    let lo = if line == start {
        start_col.max(first)
    } else {
        first
    };
    let hi = if line == end { end_col.min(last) } else { last };
    (lo <= hi).then_some((lo, hi))
}

/// [`span`] in modal copy mode's view, as columns local to it.
fn modal_span(v: &CopyView, ends: Ends, line: usize) -> Option<(usize, usize)> {
    let (lo, hi) = span(ends, line, v.col, v.col + v.cols().saturating_sub(1))?;
    Some((lo - v.col, hi - v.col))
}

/// The text a selection covers: each line's span of cells, read through
/// `cell`, trailing blanks trimmed. A line with no cell at column `first`
/// is skipped.
fn read<'v>(
    ends: Ends,
    first: usize,
    span: impl Fn(usize) -> Option<(usize, usize)>,
    cell: impl Fn(usize, usize) -> Option<Cow<'v, Cell>>,
) -> String {
    let mut out = String::new();
    let ((_, start), (_, end)) = ends;
    for line in start..=end {
        if cell(first, line).is_none() {
            continue;
        }
        for x in span(line).into_iter().flat_map(|(lo, hi)| lo..=hi) {
            let Some(cell) = cell(x, line) else {
                break;
            };
            if !cell.is_wide_continuation() {
                cell.push_display_text(&mut out);
            }
        }
        if line != end {
            out.truncate(out.trim_end_matches(' ').len());
            out.push('\n');
        }
    }
    out.trim_end_matches(' ').to_string()
}

fn frame_ends(selection: &Selection) -> Ends {
    let local = |(x, y): (u16, u16)| (usize::from(x), usize::from(y));
    ordered(
        local(selection.anchor.unwrap_or(selection.cursor)),
        local(selection.cursor),
    )
}

/// [`extract_selection`] for a mouse selection made over `view`: the cells
/// that frame drew, cut glyphs at its edges blank as drawn.
pub fn extract_frame_selection(
    vt: &VirtualTerminal,
    selection: &Selection,
    view: &PaneViewport,
) -> String {
    let slice = Slice::Frame(view);
    let last = view.shown_cols().saturating_sub(1);
    let cell = |x: usize, y: usize| slice.cell(vt, u16::try_from(x).ok()?, u16::try_from(y).ok()?);
    let ends = frame_ends(selection);
    read(ends, 0, |row| span(ends, row, 0, last), cell)
}

/// A mouse selection's `(row, first, last)` runs in the cells `view` showed.
fn frame_runs(selection: &Selection, view: &PaneViewport) -> Vec<(u16, u16, u16)> {
    let (ends, last) = (frame_ends(selection), view.shown_cols().saturating_sub(1));
    (0..view.shown_rows())
        .filter_map(|row| {
            span(ends, row, 0, last).map(|(lo, hi)| (row as u16, lo as u16, hi as u16))
        })
        .collect()
}

/// Draw modal copy mode's slice over the frame, for a view the frame's own
/// pane cells cannot show (history, or columns past the grid). Only the
/// shown cells: surplus beside them stays the compositor's.
pub fn render_copy_view_into(buf: &mut Vec<u8>, vt: &VirtualTerminal, state: &CopyModeState) {
    let v = &state.view;
    let rect = v.rect();
    for y in 0..v.rows() {
        let line = v.top + y;
        if vt.presented_row(line).is_none() {
            continue;
        }
        let _ = write!(buf, "\x1b[{};{}H", usize::from(rect.y) + y + 1, rect.x + 1);
        for x in 0..v.cols() {
            match v.cell(vt, x, line) {
                // The wide glyph before it covered this cell; a space here
                // would push the rest of the row one column right.
                Some(cell) if cell.is_wide_continuation() => {}
                Some(cell) => write_cell(buf, &cell),
                None => {
                    let _ = write!(buf, " ");
                }
            }
        }
        let _ = write!(buf, "\x1b[0m");
    }
}

fn write_cell(buf: &mut Vec<u8>, cell: &shux_vt::Cell) {
    write_style(buf, cell);
    let _ = write!(buf, "{}", cell.display_text());
}

fn write_style(buf: &mut Vec<u8>, cell: &shux_vt::Cell) {
    let _ = write!(buf, "\x1b[0m");
    write_color(buf, cell.style.fg, true);
    write_color(buf, cell.style.bg, false);
    let flags = cell.style.flags;
    if flags.contains(CellFlags::BOLD) {
        let _ = write!(buf, "\x1b[1m");
    }
    if flags.contains(CellFlags::DIM) {
        let _ = write!(buf, "\x1b[2m");
    }
    if flags.contains(CellFlags::ITALIC) {
        let _ = write!(buf, "\x1b[3m");
    }
    if flags.contains(CellFlags::UNDERLINE) {
        let _ = write!(buf, "\x1b[4m");
    }
    if flags.contains(CellFlags::INVERSE) {
        let _ = write!(buf, "\x1b[7m");
    }
    if flags.contains(CellFlags::HIDDEN) {
        let _ = write!(buf, "\x1b[8m");
    }
    if flags.contains(CellFlags::STRIKETHROUGH) {
        let _ = write!(buf, "\x1b[9m");
    }
}

fn write_color(buf: &mut Vec<u8>, color: Color, fg: bool) {
    let base = if fg { 38 } else { 48 };
    match color {
        Color::Default => {}
        Color::Indexed(n) => {
            let _ = write!(buf, "\x1b[{base};5;{n}m");
        }
        Color::Rgb(r, g, b) => {
            let _ = write!(buf, "\x1b[{base};2;{r};{g};{b}m");
        }
    }
}

/// Append ANSI bytes that draw the copy-mode overlay over its pane. Use
/// [`render_copy_overlay_with_vt_into`] when a VT is available so selection
/// can repaint the selected glyphs with a high-contrast palette instead of
/// masking text with a blank band.
pub fn render_copy_overlay_into(buf: &mut Vec<u8>, state: &CopyModeState, theme: &Theme) {
    render_copy_overlay_inner(buf, None, state, theme);
}

/// Variant of [`render_copy_overlay_into`] that redraws selected text from
/// the copy view, keeping a visual selection legible.
pub fn render_copy_overlay_with_vt_into(
    buf: &mut Vec<u8>,
    vt: &VirtualTerminal,
    state: &CopyModeState,
    theme: &Theme,
) {
    render_copy_overlay_inner(buf, Some(vt), state, theme);
}

/// Draw only the selected text, without the copy-mode cursor or status hint.
///
/// This is used by the human mouse-selection path: a selected range can remain
/// visible while normal pane input keeps flowing to the PTY. It reads the
/// cells `view` showed, as [`extract_frame_selection`] yanks them.
pub fn render_selection_overlay_with_vt_into(
    buf: &mut Vec<u8>,
    view: &PaneViewport,
    vt: &VirtualTerminal,
    selection: &Selection,
    theme: &Theme,
) {
    // Only cells the frame drew: a run is not highlighted across surplus.
    let runs = frame_runs(selection, view);
    paint_runs(buf, view.rect, Some(vt), Slice::Frame(view), theme, &runs);
}

/// Variant of [`render_selection_overlay_with_vt_into`] used when the VT has
/// disappeared between frames.
pub fn render_selection_overlay_into(
    buf: &mut Vec<u8>,
    view: &PaneViewport,
    selection: &Selection,
    theme: &Theme,
) {
    let runs = frame_runs(selection, view);
    paint_runs(buf, view.rect, None, Slice::Frame(view), theme, &runs);
}

fn render_copy_overlay_inner(
    buf: &mut Vec<u8>,
    vt: Option<&VirtualTerminal>,
    state: &CopyModeState,
    theme: &Theme,
) {
    let v = &state.view;
    let pane = v.rect();
    if pane.width < 2 || pane.height < 2 {
        return;
    }

    if let Some(anchor) = state.anchor {
        let ends = ordered(anchor, state.cursor);
        let runs: Vec<(u16, u16, u16)> = (0..v.rows())
            .filter_map(|y| {
                let (lo, hi) = modal_span(v, ends, v.top + y)?;
                Some((y as u16, lo as u16, hi as u16))
            })
            .collect();
        paint_runs(buf, pane, vt, Slice::Copy(v), theme, &runs);
    }

    // Status-line hint at the bottom row of the pane: shows current
    // mode and dismiss/yank shortcuts. The text width changes with
    // selection state (`v select` disappears once an anchor is set),
    // so we pad to the full pane width — otherwise the tail of the
    // previous (longer) hint remains visible as ghost text.
    let hint = if let Some(search) = &state.search {
        match search.direction {
            SearchDirection::Forward => format!(" /{} ", search.query),
            SearchDirection::Backward => format!(" ?{} ", search.query),
        }
    } else if state.anchor.is_some() {
        " COPY  hjkl move  / ? search  n/N next  y yank  q exit ".to_string()
    } else {
        " COPY  hjkl move  PgUp/PgDn scroll  / ? search  v select  y yank  q exit ".to_string()
    };
    let hint_row = pane.y + pane.height.saturating_sub(1);
    let usable = pane.width as usize;
    let truncated = truncate_to_width(&hint, usable);
    let pad = usable.saturating_sub(display_width_str(&truncated));
    let _ = write!(
        buf,
        "\x1b[{};{}H{}{}{}{}\x1b[0m",
        hint_row + 1,
        pane.x + 1,
        sgr_bg(theme.status_accent),
        sgr_fg(theme.status_bg),
        truncated,
        " ".repeat(pad),
    );

    // Cursor block, after the hint so it shows on the hint's row too: the
    // selection palette inverted (fg=status_bg on bg=status_fg), so it stays
    // visible inside a selection run and over bare pane content.
    let (cx, cy) = state.cursor_local();
    let (col, glyph) = vt
        .and_then(|vt| cursor_glyph(vt, state))
        .unwrap_or((cx, " ".to_string()));
    cursor_block(
        buf,
        pane.x + col,
        pane.y + cy,
        &glyph,
        theme.status_bg,
        theme.status_fg,
    );
}

/// Highlight each `(row, first, last)` local run of `pane`, redrawing the
/// glyphs `slice` shows there when a VT is at hand.
fn paint_runs(
    buf: &mut Vec<u8>,
    pane: Rect,
    vt: Option<&VirtualTerminal>,
    slice: Slice<'_>,
    theme: &Theme,
    runs: &[(u16, u16, u16)],
) {
    for &(row, lo, hi) in runs {
        if row >= pane.height {
            continue;
        }
        match vt {
            Some(vt) => {
                let ctx = SelectionRenderCtx {
                    pane,
                    vt,
                    slice,
                    theme,
                };
                selection_text_run(buf, ctx, lo, hi, row);
            }
            None => highlight_run(buf, pane, lo, hi, row, theme.status_bg, theme.status_accent),
        }
    }
}

/// Action chosen from the inline copy context menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyMenuAction {
    Copy,
    Clear,
}

pub const COPY_MENU_WIDTH: u16 = 10;
pub const COPY_MENU_HEIGHT: u16 = 2;

pub fn copy_menu_origin(col: u16, row: u16, screen_cols: u16, screen_rows: u16) -> (u16, u16) {
    let max_col = screen_cols.saturating_sub(COPY_MENU_WIDTH);
    let max_row = screen_rows.saturating_sub(COPY_MENU_HEIGHT);
    (col.min(max_col), row.min(max_row))
}

pub fn copy_menu_action_at(
    menu_col: u16,
    menu_row: u16,
    click_col: u16,
    click_row: u16,
) -> Option<CopyMenuAction> {
    if click_col < menu_col || click_col >= menu_col.saturating_add(COPY_MENU_WIDTH) {
        return None;
    }
    match click_row.checked_sub(menu_row)? {
        0 => Some(CopyMenuAction::Copy),
        1 => Some(CopyMenuAction::Clear),
        _ => None,
    }
}

pub fn render_copy_menu_into(
    buf: &mut Vec<u8>,
    col: u16,
    row: u16,
    screen_cols: u16,
    screen_rows: u16,
    theme: &Theme,
) {
    if screen_cols == 0 || screen_rows == 0 {
        return;
    }
    let (x, y) = copy_menu_origin(col, row, screen_cols, screen_rows);
    let rows = [(" Copy", theme.status_accent), (" Clear", theme.status_bg)];
    for (idx, (label, bg)) in rows.into_iter().enumerate() {
        let text = format!("{label:<width$}", width = COPY_MENU_WIDTH as usize);
        let _ = write!(
            buf,
            "\x1b[{};{}H{}{}{}\x1b[0m",
            y + idx as u16 + 1,
            x + 1,
            sgr_bg(bg),
            sgr_fg(theme.status_fg),
            text,
        );
    }
}

struct SelectionRenderCtx<'a> {
    pane: Rect,
    vt: &'a VirtualTerminal,
    slice: Slice<'a>,
    theme: &'a Theme,
}

fn selection_text_run(
    buf: &mut Vec<u8>,
    ctx: SelectionRenderCtx<'_>,
    col_lo: u16,
    col_hi: u16,
    row: u16,
) {
    if ctx.slice.cell(ctx.vt, 0, row).is_none() {
        highlight_run(
            buf,
            ctx.pane,
            col_lo,
            col_hi,
            row,
            ctx.theme.status_bg,
            ctx.theme.status_accent,
        );
        return;
    }
    let col_hi = col_hi.min(ctx.pane.width.saturating_sub(1));
    for col in col_lo..=col_hi {
        let cell = ctx.slice.cell(ctx.vt, col, row);
        // A wide glyph drawn at its lead covers this cell; a space here would
        // erase it. A run that starts on the right half has no lead to draw.
        let continuation = cell.as_ref().is_some_and(|c| c.is_wide_continuation());
        if continuation && col > col_lo {
            continue;
        }
        let text = cell
            .filter(|cell| !cell.is_wide_continuation())
            .map(|cell| shown_text(&cell))
            .unwrap_or_else(|| " ".to_string());
        let _ = write!(
            buf,
            "\x1b[{};{}H{}{}\x1b[1m{}\x1b[0m",
            ctx.pane.y + row + 1,
            ctx.pane.x + col + 1,
            sgr_bg(ctx.theme.status_accent),
            sgr_fg(ctx.theme.status_bg),
            text,
        );
    }
}

fn display_width_str(s: &str) -> usize {
    use unicode_width::UnicodeWidthStr;
    s.width()
}

fn highlight_run(
    buf: &mut Vec<u8>,
    pane: Rect,
    col_lo: u16,
    col_hi: u16,
    row: u16,
    fg: Rgb,
    bg: Rgb,
) {
    let len = (col_hi - col_lo + 1).min(pane.width.saturating_sub(col_lo)) as usize;
    if len == 0 {
        return;
    }
    // Solid colored block — explicit bg + fg, no `\x1b[7m` (reverse
    // video) on top: it inverts whatever the previous SGR state was
    // and made the cursor block hard to read against the selection.
    let _ = write!(
        buf,
        "\x1b[{};{}H{}{}{:width$}\x1b[0m",
        pane.y + row + 1,
        pane.x + col_lo + 1,
        sgr_bg(bg),
        sgr_fg(fg),
        " ",
        width = len,
    );
}

/// The pane column the cursor block starts at, and the glyph it shows. On the
/// right half of a wide character that is the character's own cell, so the
/// block covers the whole glyph instead of splitting it.
fn cursor_glyph(vt: &VirtualTerminal, state: &CopyModeState) -> Option<(u16, String)> {
    let v = &state.view;
    let (mut x, y) = state.cursor_local();
    let line = v.top + usize::from(y);
    let mut cell = v.cell(vt, x.into(), line)?;
    if cell.is_wide_continuation() {
        x = x.checked_sub(1)?;
        cell = v.cell(vt, x.into(), line)?;
    }
    Some((x, shown_text(&cell)))
}

/// What an overlay that recolours `cell` may show of it: concealed (SGR 8)
/// text becomes blanks of the same width, an empty cell a blank.
fn shown_text(cell: &shux_vt::Cell) -> String {
    let text = cell.display_text();
    if cell.style.flags.contains(CellFlags::HIDDEN) {
        " ".repeat(if cell.is_wide() { 2 } else { 1 })
    } else if text.is_empty() {
        " ".into()
    } else {
        text.into_owned()
    }
}

fn cursor_block(buf: &mut Vec<u8>, x: u16, y: u16, glyph: &str, fg: Rgb, bg: Rgb) {
    let _ = write!(
        buf,
        "\x1b[{};{}H{}{}{glyph}\x1b[0m",
        y + 1,
        x + 1,
        sgr_bg(bg),
        sgr_fg(fg),
    );
}

fn sgr_fg(c: Rgb) -> String {
    format!("\x1b[38;2;{};{};{}m", c.r, c.g, c.b)
}
fn sgr_bg(c: Rgb) -> String {
    format!("\x1b[48;2;{};{};{}m", c.r, c.g, c.b)
}

fn truncate_to_width(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0usize;
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > max {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out
}

/// Build the OSC 52 clipboard write sequence for `text`. Most modern
/// terminals (iTerm2, Kitty, WezTerm, Alacritty, foot, recent xterm)
/// honor this and copy the bytes into the system clipboard without
/// any side-channel auth. `c;` selects the system clipboard.
pub fn osc52_copy(text: &str) -> Vec<u8> {
    let encoded = BASE64.encode(text.as_bytes());
    format!("\x1b]52;c;{encoded}\x07").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vt_with(rows: u16, cols: u16, payload: &str) -> VirtualTerminal {
        let mut vt = VirtualTerminal::new(rows as usize, cols as usize);
        vt.process(payload.as_bytes());
        vt
    }

    fn vt_with_lines(rows: u16, cols: u16, count: usize) -> VirtualTerminal {
        let mut vt = VirtualTerminal::new(rows as usize, cols as usize);
        for i in 0..count {
            vt.process(format!("line-{i:02}\r\n").as_bytes());
        }
        vt
    }

    /// Copy mode over `vt` as a frame drawing it in `rect` shows it.
    fn modal(vt: &VirtualTerminal, rect: Rect) -> CopyModeState {
        let pane = PaneId::from_uuid(uuid::Uuid::from_u128(1));
        CopyModeState::enter(pane, PaneViewport::of(rect, vt, 0), Drawn::of(vt), vt)
            .expect("copy mode")
    }

    fn whole(vt: &VirtualTerminal) -> CopyModeState {
        let (rows, cols) = (vt.grid().rows() as u16, vt.grid().cols() as u16);
        modal(vt, Rect::new(0, 0, cols, rows))
    }

    /// The cursor at local `(x, y)`, the anchor at local `anchor`.
    fn select(s: &mut CopyModeState, anchor: Option<(u16, u16)>, cursor: (u16, u16)) {
        s.anchor = anchor.map(|a| s.view.source(a));
        s.set_cursor(s.view.source(cursor));
    }

    fn strip_ansi(input: &str) -> String {
        let mut out = String::new();
        let mut chars = input.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '\x1b' {
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(ch);
            }
        }
        out
    }

    #[test]
    fn handle_key_hjkl_clamps_to_pane() {
        let vt = vt_with(5, 10, "");
        let mut s = whole(&vt);
        assert_eq!(handle_key(b"h", &mut s, &vt), CopyKey::Ignored);
        assert_eq!(handle_key(b"k", &mut s, &vt), CopyKey::Ignored);
        assert_eq!(s.cursor_local(), (0, 0));
        assert_eq!(handle_key(b"j", &mut s, &vt), CopyKey::Updated);
        assert_eq!(handle_key(b"l", &mut s, &vt), CopyKey::Updated);
        assert_eq!(s.cursor_local(), (1, 1));
        for _ in 0..20 {
            handle_key(b"l", &mut s, &vt);
            handle_key(b"j", &mut s, &vt);
        }
        // The last row is under the hint bar: never drawn, never reachable.
        assert_eq!(s.cursor_local(), (9, 3), "the cursor left what is readable");
    }

    #[test]
    fn handle_key_arrow_keys_work() {
        let vt = vt_with(5, 10, "");
        let mut s = whole(&vt);
        assert_eq!(handle_key(b"\x1b[B", &mut s, &vt), CopyKey::Updated);
        assert_eq!(handle_key(b"\x1b[C", &mut s, &vt), CopyKey::Updated);
        assert_eq!(s.cursor_local(), (1, 1));
        handle_key(b"\x1b[A", &mut s, &vt);
        handle_key(b"\x1b[D", &mut s, &vt);
        assert_eq!(s.cursor_local(), (0, 0));
    }

    #[test]
    fn handle_key_v_toggles_anchor() {
        let vt = vt_with(5, 10, "");
        let mut s = whole(&vt);
        select(&mut s, None, (3, 2));
        handle_key(b"v", &mut s, &vt);
        assert_eq!(s.anchor, Some((3, 2)));
        handle_key(b"v", &mut s, &vt);
        assert_eq!(s.anchor, None);
    }

    #[test]
    fn handle_key_y_returns_yank() {
        let vt = vt_with(5, 10, "");
        assert_eq!(handle_key(b"y", &mut whole(&vt), &vt), CopyKey::Yank);
    }

    #[test]
    fn handle_key_q_and_esc_exit() {
        let vt = vt_with(5, 10, "");
        let mut s = whole(&vt);
        assert_eq!(handle_key(b"q", &mut s, &vt), CopyKey::Exit);
        assert_eq!(handle_key(b"\x1b", &mut s, &vt), CopyKey::Exit);
    }

    #[test]
    fn page_keys_adjust_scroll_offset() {
        // A page is the READABLE height: the bottom row of the rect carries
        // the hint bar, so five rows show four lines.
        let vt = vt_with_lines(5, 10, 20);
        let mut s = whole(&vt);
        let top = s.view.top;
        assert_eq!(handle_key(b"\x1b[5~", &mut s, &vt), CopyKey::Updated);
        assert_eq!(s.view.top, top - 4);
        assert_eq!(handle_key(b"\x1b[6~", &mut s, &vt), CopyKey::Updated);
        assert_eq!(s.view.top, top);
    }

    /// Paging must TILE the lines a person can read: none skipped between
    /// pages, none shown twice. The hint bar covers a full-height slice's last
    /// row, so a page is one line fewer than the rect.
    #[test]
    fn paging_tiles_the_readable_lines_without_skipping_any() {
        for pane_rows in [3u16, 5, 12, 24, 30] {
            let vt = vt_with_lines(pane_rows, 40, 400);
            let readable = pane_rows as usize - 1;
            let mut s = whole(&vt);
            let mut seen: Vec<usize> = Vec::new();
            for _ in 0..6 {
                seen.extend(s.view.top..s.view.top + readable);
                if handle_key(b"\x1b[5~", &mut s, &vt) != CopyKey::Updated {
                    break;
                }
            }
            seen.sort_unstable();
            seen.dedup();
            let (lo, hi) = (seen[0], *seen.last().expect("lines"));
            assert_eq!(seen.len(), hi - lo + 1, "{pane_rows} rows skipped lines");
            assert_eq!(
                seen.len(),
                6 * readable,
                "{pane_rows} rows overlapped pages"
            );
        }
    }

    #[test]
    fn gg_and_g_jump_between_history_edges() {
        let vt = vt_with_lines(5, 10, 20);
        let mut s = whole(&vt);
        assert_eq!(handle_key(b"g", &mut s, &vt), CopyKey::Ignored);
        assert_eq!(handle_key(b"g", &mut s, &vt), CopyKey::Updated);
        assert_eq!((s.view.top, s.cursor), (0, (0, 0)));
        assert_eq!(handle_key(b"G", &mut s, &vt), CopyKey::Updated);
        // Four rows readable: the last line sits just above the hint bar.
        assert_eq!(s.view.top, vt.presented_total_lines() - 4);
        assert_eq!(
            s.cursor_local(),
            (0, 3),
            "G put the cursor under the hint bar"
        );
    }

    #[test]
    fn slash_search_focuses_scrollback_match() {
        let vt = vt_with_lines(5, 20, 20);
        let mut s = whole(&vt);
        for key in ["/", "l", "i", "n", "e", "-", "0", "7", "\r"] {
            handle_key(key.as_bytes(), &mut s, &vt);
        }
        assert!(s.view.top < s.view.entry_top, "{:?}", s.view);
        assert_eq!(extract_selection(&vt, &s), "l");
        assert_eq!(
            vt.presented_row(s.cursor.1).map(|r| r[5].ch),
            Some('0'),
            "the cursor is not on line-07"
        );
        assert_eq!(
            s.last_search,
            Some(SearchMatch {
                direction: SearchDirection::Forward,
                query: "line-07".to_string()
            })
        );
    }

    #[test]
    fn repeat_search_moves_to_next_and_previous_match() {
        let vt = vt_with(5, 20, "apple\r\nbanana\r\napple\r\nbanana");
        let mut s = whole(&vt);
        for key in ["/", "a", "p", "p", "l", "e", "\r"] {
            handle_key(key.as_bytes(), &mut s, &vt);
        }
        let first = s.cursor;
        assert_eq!(handle_key(b"n", &mut s, &vt), CopyKey::Updated);
        assert_ne!(s.cursor, first);
        assert_eq!(handle_key(b"N", &mut s, &vt), CopyKey::Updated);
        assert_eq!(s.cursor, first);
    }

    /// A search that pans the slice keeps the selection's source anchor: the
    /// yank still starts where `v` was pressed.
    #[test]
    fn a_pan_keeps_the_selection_it_started_with() {
        let vt = vt_with(3, 60, "abc\x1b[1;50Hzz\x1b[1;1H");
        let mut s = modal(&vt, Rect::new(0, 0, 20, 3));
        handle_key(b"v", &mut s, &vt);
        let anchor = s.anchor;
        for key in ["/", "z", "z", "\r"] {
            handle_key(key.as_bytes(), &mut s, &vt);
        }
        assert_eq!(s.view.col, 49 + 2 - 20, "panned past what it had to");
        assert_eq!(s.anchor, anchor, "the pan moved the anchor");
        assert_eq!(s.cursor, (49, 0));
    }

    fn yank(payload: &str, anchor: Option<(u16, u16)>, cursor: (u16, u16)) -> String {
        let vt = vt_with(5, 20, payload);
        let mut s = whole(&vt);
        select(&mut s, anchor, cursor);
        extract_selection(&vt, &s)
    }

    #[test]
    fn extract_selection_single_line() {
        assert_eq!(yank("hello world", Some((6, 0)), (10, 0)), "world");
    }

    #[test]
    fn extract_selection_no_anchor_returns_single_cell() {
        assert_eq!(yank("hello world", None, (0, 0)), "h");
    }

    #[test]
    fn extract_selection_multi_line() {
        assert_eq!(
            yank("first\r\nsecond", Some((0, 0)), (5, 1)),
            "first\nsecond"
        );
    }

    #[test]
    fn extract_selection_handles_reversed_endpoints() {
        assert_eq!(yank("hello world", Some((10, 0)), (6, 0)), "world");
    }

    /// Copy mode scrolled to the top of history.
    fn at_top(vt: &VirtualTerminal) -> CopyModeState {
        let mut s = whole(vt);
        handle_key(b"g", &mut s, vt);
        handle_key(b"g", &mut s, vt);
        s
    }

    #[test]
    fn extract_selection_reads_scrolled_history_view() {
        let vt = vt_with_lines(3, 20, 8);
        let mut s = at_top(&vt);
        select(&mut s, Some((0, 0)), (6, 0));
        assert_eq!(extract_selection(&vt, &s), "line-00");
    }

    #[test]
    fn render_copy_view_draws_scrolled_history() {
        let vt = vt_with_lines(3, 20, 8);
        let s = at_top(&vt);
        let (_, over) = s.view.frame_view(&vt);
        assert!(over, "history is not the grid's to draw");
        let mut buf = Vec::new();
        render_copy_view_into(&mut buf, &vt, &s);
        assert!(strip_ansi(&String::from_utf8_lossy(&buf)).contains("line-00"));
    }

    /// A grid taller than its rect, content and cursor at the top: the frame
    /// shows the top rows, and copy mode reads the rows it shows.
    #[test]
    fn extract_selection_reads_shown_rows_when_grid_exceeds_viewport() {
        let mut vt = VirtualTerminal::new(60, 40);
        vt.process(b"TOPLINE-UNIQUE\r\n");
        let mut s = modal(&vt, Rect::new(0, 0, 40, 20));
        select(&mut s, Some((0, 0)), (13, 0));
        assert_eq!(extract_selection(&vt, &s), "TOPLINE-UNIQUE");
    }

    /// A grid that fits its rect opens copy mode on the live rows, below all
    /// of its history, drawn as the frame draws them.
    #[test]
    fn effective_total_lines_is_unchanged_when_grid_fits() {
        let mut vt = VirtualTerminal::new(24, 80);
        for _ in 0..50 {
            vt.process(b"line\r\n");
        }
        let s = whole(&vt);
        assert_eq!(s.view.top, vt.presented_total_lines() - 24);
        let live = PaneViewport::of(s.view.rect(), &vt, 0);
        assert_eq!(s.view.frame_view(&vt), (live, false));
    }

    /// The view opens where the frame showed the pane, on both axes, and the
    /// frame keeps drawing it there while the application moves its cursor.
    #[test]
    fn copy_view_render_matches_frame_for_oversized_pane() {
        let mut vt = VirtualTerminal::new(20, 60);
        for r in 0..20 {
            vt.process(format!("\x1b[{};1H{r:02}{}", r + 1, "x".repeat(50)).as_bytes());
        }
        vt.process(b"\x1b[15;45H");
        let rect = Rect::new(2, 1, 30, 10);
        let entry = PaneViewport::of(rect, &vt, 0);
        assert_eq!((entry.row_offset, entry.col_offset), (5, 15));
        let s = modal(&vt, rect);
        vt.process(b"\x1b[1;1H");
        assert_eq!(s.view.frame_view(&vt), (entry, false));
        assert!(s.view.maps(rect, &vt));
        assert!(!s.view.maps(Rect::new(2, 1, 31, 10), &vt));
        vt.resize(20, 59);
        assert!(!s.view.maps(rect, &vt), "a resized grid still mapped");
    }

    #[test]
    fn osc52_emits_well_formed_sequence() {
        let bytes = osc52_copy("hi");
        assert_eq!(&bytes[..6], b"\x1b]52;c");
        assert_eq!(*bytes.last().unwrap(), 0x07);
        let payload = std::str::from_utf8(&bytes[7..bytes.len() - 1]).unwrap();
        assert_eq!(BASE64.decode(payload).unwrap(), b"hi");
    }

    #[test]
    fn render_overlay_emits_cursor_position() {
        let vt = VirtualTerminal::new(10, 20);
        let mut s = modal(&vt, Rect::new(2, 3, 20, 10));
        select(&mut s, None, (4, 2));
        let mut buf = Vec::new();
        render_copy_overlay_into(&mut buf, &s, &Theme::DEFAULT);
        let out = String::from_utf8_lossy(&buf);
        assert!(
            out.contains("\x1b[6;7H"),
            "missing cursor position: {out:?}"
        );
        assert!(out.contains("COPY"), "missing status hint");
    }

    #[test]
    fn render_overlay_with_vt_keeps_selected_text_legible() {
        let vt = vt_with(3, 20, "abcdef");
        let mut s = whole(&vt);
        select(&mut s, Some((0, 0)), (2, 0));
        let mut buf = Vec::new();
        render_copy_overlay_with_vt_into(&mut buf, &vt, &s, &Theme::DEFAULT);
        let out = String::from_utf8_lossy(&buf);
        assert!(strip_ansi(&out).contains("abc"), "{out:?}");
        assert!(out.contains("\x1b[48;2;116;199;236m"), "accent background");
        assert!(out.contains("\x1b[38;2;30;32;48m"), "dark foreground");
    }

    /// Draw the overlay into a fresh VT and read back the cell at `(col, row)`.
    fn overlay_cell(vt: &VirtualTerminal, s: &CopyModeState, col: u16, row: u16) -> Cell {
        let mut buf = Vec::new();
        render_copy_overlay_with_vt_into(&mut buf, vt, s, &Theme::DEFAULT);
        let r = s.view.rect();
        let mut screen = VirtualTerminal::new(r.height as usize, r.width as usize);
        screen.process(&buf);
        screen.grid().visible_row(row.into())[usize::from(col)].clone()
    }

    fn cursor_colours(cell: &Cell) {
        let (bg, fg) = (Theme::DEFAULT.status_bg, Theme::DEFAULT.status_fg);
        assert_eq!(
            (cell.style.fg, cell.style.bg),
            (Color::Rgb(bg.r, bg.g, bg.b), Color::Rgb(fg.r, fg.g, fg.b)),
            "the cursor cell is not in the cursor colours"
        );
    }

    #[test]
    fn the_copy_cursor_shows_the_glyph_under_it() {
        let vt = vt_with(
            3,
            20,
            "\x1b[38;2;120;220;180mTRUECOLOR\x1b[0m \x1b[38;5;208mINDEXED\x1b[0m \x1b[34mBASIC\x1b[0m",
        );
        let s = whole(&vt);
        let cell = overlay_cell(&vt, &s, 0, 0);
        assert_eq!(cell.display_text(), "T", "the cursor blanked the glyph");
        cursor_colours(&cell);
    }

    #[test]
    fn the_copy_cursor_covers_a_wide_glyph_from_either_half() {
        let vt = vt_with(3, 20, "a漢b");
        for col in [1, 2] {
            let mut s = whole(&vt);
            select(&mut s, None, (col, 0));
            let cell = overlay_cell(&vt, &s, 1, 0);
            assert_eq!(cell.display_text(), "漢", "cursor at {col} split it");
            assert!(cell.is_wide());
            cursor_colours(&cell);
        }
    }

    #[test]
    fn copy_mode_keeps_concealed_text_concealed() {
        let vt = vt_with(3, 20, "\x1b[8mSECRET\u{6f22}\x1b[0m");
        for (cursor, block) in [(0, 0), (6, 6), (7, 6)] {
            let mut s = whole(&vt);
            select(&mut s, None, (cursor, 0));
            let cell = overlay_cell(&vt, &s, block, 0);
            assert!(cell.display_text().trim().is_empty(), "cursor at {cursor}");
            cursor_colours(&cell);
        }
        let mut s = whole(&vt);
        select(&mut s, Some((0, 0)), (5, 0));
        let cell = overlay_cell(&vt, &s, 2, 0);
        assert!(
            cell.display_text().trim().is_empty(),
            "the selection showed it"
        );

        let mut buf = Vec::new();
        render_copy_view_into(&mut buf, &vt, &whole(&vt));
        let mut screen = VirtualTerminal::new(3, 20);
        screen.process(&buf);
        let cell = &screen.grid().visible_row(0)[2];
        assert!(
            cell.style.flags.contains(CellFlags::HIDDEN),
            "the view showed it"
        );
    }

    /// The hint bar sits on the pane's last row, which the cursor can reach.
    #[test]
    fn the_copy_cursor_shows_over_the_hint_bar() {
        // The hint bar covers the last row, so neither the cursor, a drag nor
        // a selection reaches it: what is yanked is what was drawn.
        let vt = vt_with(3, 20, "one\r\ntwo\r\nXYZ");
        let mut s = whole(&vt);
        select(&mut s, Some((0, 0)), (2, 2));
        assert_eq!(s.cursor_local(), (2, 1));
        assert_eq!(
            s.view.source((2, 2)),
            (2, 1),
            "a drag on the hint row named it"
        );
        assert_eq!(extract_selection(&vt, &s), "one\ntwo");
        let cell = overlay_cell(&vt, &s, 2, 1);
        assert_eq!(cell.display_text(), "o");
        cursor_colours(&cell);
    }

    #[test]
    fn render_overlay_no_cursor_when_pane_too_small() {
        let vt = VirtualTerminal::new(1, 1);
        let s = whole(&vt);
        let mut buf = Vec::new();
        render_copy_overlay_into(&mut buf, &s, &Theme::DEFAULT);
        assert!(buf.is_empty());
    }

    /// A selection whose anchor line has been evicted from history is
    /// dropped; the view and cursor move down with what is left.
    #[test]
    fn rebasing_over_evicted_history_drops_the_selection() {
        let config = shux_vt::GridConfig {
            max_scrollback: 4,
            ..Default::default()
        };
        let mut vt = VirtualTerminal::with_config(3, 10, config);
        for i in 0..6 {
            vt.process(format!("e{i}\r\n").as_bytes());
        }
        let mut s = whole(&vt);
        handle_key(b"v", &mut s, &vt);
        for i in 6..20 {
            vt.process(format!("e{i}\r\n").as_bytes());
        }
        s.rebase(&vt);
        assert_eq!(s.anchor, None);
        assert!(s.view.local(s.cursor).is_some(), "the cursor left the view");
    }

    /// An upward selection whose CURSOR's line is evicted, its anchor still
    /// in history, is dropped too: `y` says so rather than copy a shorter one.
    #[test]
    fn evicting_the_cursors_line_drops_an_upward_selection() {
        let config = shux_vt::GridConfig {
            max_scrollback: 6,
            ..Default::default()
        };
        let mut vt = VirtualTerminal::with_config(3, 10, config);
        for i in 0..9 {
            vt.process(format!("e{i}\r\n").as_bytes());
        }
        let mut s = at_top(&vt);
        handle_key(b"j", &mut s, &vt);
        handle_key(b"v", &mut s, &vt);
        handle_key(b"k", &mut s, &vt);
        assert_eq!(
            (s.anchor.map(|a| a.1), s.cursor.1),
            (Some(1), 0),
            "setup: an upward selection from line 1 to 0"
        );
        let evicted = vt.presented_evicted();
        vt.process(b"e9\r\n");
        assert_eq!(
            vt.presented_evicted(),
            evicted + 1,
            "setup: one more line evicted"
        );
        assert_eq!(handle_key(b"y", &mut s, &vt), CopyKey::SelectionEvicted);
        assert_eq!(s.anchor, None);
    }

    /// A search match on the last line is brought into the readable rows,
    /// never left under the hint bar; `G` reaches that line too.
    #[test]
    fn the_last_line_is_reachable_above_the_hint_bar() {
        let vt = vt_with_lines_ending(5, 20, 12, "LAST");
        let readable = 4;
        let mut s = at_top(&vt);
        for key in ["/", "L", "A", "S", "T", "\r"] {
            handle_key(key.as_bytes(), &mut s, &vt);
        }
        assert_eq!(
            vt.presented_row(s.cursor.1).map(|r| r[0].ch),
            Some('L'),
            "the search did not find the last line"
        );
        assert!(
            s.cursor_local().1 < readable,
            "the match is under the hint bar: {:?}",
            s.cursor_local()
        );
        let mut s = at_top(&vt);
        handle_key(b"G", &mut s, &vt);
        assert_eq!(
            vt.presented_row(s.cursor.1).map(|r| r[0].ch),
            Some('L'),
            "G stopped short of the last line"
        );
        assert!(s.cursor_local().1 < readable);
        // The frame draws it from the grid; the row past the grid's last is
        // the hint bar's.
        let (view, over) = s.view.frame_view(&vt);
        let y = usize::from(s.cursor_local().1);
        assert!(!over);
        assert_eq!(view.row_offset + y, 4, "the frame does not draw LAST there");
        assert_eq!(view.shown_rows(), usize::from(readable));
    }

    fn vt_with_lines_ending(rows: u16, cols: u16, count: usize, last: &str) -> VirtualTerminal {
        let mut vt = vt_with_lines(rows, cols, count);
        vt.process(last.as_bytes());
        vt
    }

    /// A pan by search keeps the whole selection: the yank reads its source
    /// columns from end to end, wherever the view has moved since, in both
    /// directions, wide glyphs whole.
    #[test]
    fn a_panned_selection_yanks_every_column_it_covers() {
        let text = "ab中c\x1b[1;30H文de\x1b[1;50Hzz\x1b[1;1H";
        let vt = vt_with(3, 60, text);
        // Columns 0-4, blank to 28, 29-32, blank to 48, then the first `z`.
        let want = format!("ab中c{}文de{}z", " ".repeat(24), " ".repeat(16));
        // Right: `v` at column 0, then a search pans to `zz`.
        let mut s = modal(&vt, Rect::new(0, 0, 20, 3));
        handle_key(b"v", &mut s, &vt);
        for key in ["/", "z", "z", "\r"] {
            handle_key(key.as_bytes(), &mut s, &vt);
        }
        assert!(s.view.col > 0, "setup: the search did not pan");
        assert_eq!(extract_selection(&vt, &s), want, "panned right");
        // Left: `v` on `zz`, then a backward search pans back to `ab`.
        let mut s = modal(&vt, Rect::new(0, 0, 20, 3));
        for key in ["/", "z", "z", "\r"] {
            handle_key(key.as_bytes(), &mut s, &vt);
        }
        handle_key(b"v", &mut s, &vt);
        for key in ["?", "a", "b", "\r"] {
            handle_key(key.as_bytes(), &mut s, &vt);
        }
        assert_eq!(s.view.col, 0, "setup: the search did not pan back");
        assert_eq!(extract_selection(&vt, &s), want, "panned left");
    }

    #[test]
    fn render_selection_overlay_omits_modal_cursor_and_hint() {
        let vt = vt_with(3, 20, "abcdef");
        let sel = Selection {
            cursor: (2, 0),
            anchor: Some((0, 0)),
        };
        let mut buf = Vec::new();
        let view = PaneViewport::of(Rect::new(0, 0, 20, 3), &vt, 0);
        render_selection_overlay_with_vt_into(&mut buf, &view, &vt, &sel, &Theme::DEFAULT);
        let plain = strip_ansi(&String::from_utf8_lossy(&buf));
        assert!(plain.contains("abc"));
        assert!(!plain.contains("COPY"));
    }

    #[test]
    fn copy_menu_origin_stays_on_screen() {
        assert_eq!(copy_menu_origin(95, 23, 100, 24), (90, 22));
        assert_eq!(copy_menu_origin(4, 5, 100, 24), (4, 5));
    }

    #[test]
    fn copy_menu_action_maps_rows() {
        assert_eq!(
            copy_menu_action_at(10, 5, 12, 5),
            Some(CopyMenuAction::Copy)
        );
        assert_eq!(
            copy_menu_action_at(10, 5, 12, 6),
            Some(CopyMenuAction::Clear)
        );
        assert_eq!(copy_menu_action_at(10, 5, 9, 5), None);
        assert_eq!(copy_menu_action_at(10, 5, 12, 7), None);
    }

    /// A mouse selection over a slice cropped on both axes highlights and
    /// yanks the same cells: those the frame drew, a glyph cut by the left
    /// edge blank in both.
    #[test]
    fn a_frame_selection_highlights_and_yanks_the_slice_it_was_made_over() {
        let mut vt = VirtualTerminal::new(8, 30);
        for r in 0..8 {
            vt.process(format!("\x1b[{};1H{r}abcdefghi界jklmnopqrstuvw", r + 1).as_bytes());
        }
        let view = PaneViewport {
            rect: Rect::new(2, 1, 6, 3),
            grid_rows: 8,
            grid_cols: 30,
            row_offset: 3,
            col_offset: 11,
        };
        let sel = Selection {
            anchor: Some((0, 0)),
            cursor: (3, 1),
        };
        assert_eq!(extract_frame_selection(&vt, &sel, &view), " jklmn\n jkl");
        let mut buf = Vec::new();
        render_selection_overlay_with_vt_into(&mut buf, &view, &vt, &sel, &Theme::DEFAULT);
        let mut screen = VirtualTerminal::new(5, 10);
        screen.process(&buf);
        let line =
            |y: usize| -> String { (2..8).map(|x| screen.grid().visible_row(y)[x].ch).collect() };
        assert_eq!(line(1), " jklmn");
        assert_eq!(&line(2)[..4], " jkl");
    }

    #[test]
    fn a_selection_highlight_keeps_its_wide_glyphs() {
        let text = "A界😀B";
        let mut vt = VirtualTerminal::new(3, 20);
        vt.process(text.as_bytes());
        let view = PaneViewport::of(Rect::new(0, 0, 20, 3), &vt, 0);
        let sel = Selection {
            anchor: Some((0, 0)),
            cursor: (5, 0),
        };
        assert_eq!(extract_frame_selection(&vt, &sel, &view), text);
        let mut buf = Vec::new();
        render_selection_overlay_with_vt_into(&mut buf, &view, &vt, &sel, &Theme::DEFAULT);
        let mut screen = VirtualTerminal::new(3, 20);
        screen.process(text.as_bytes());
        screen.process(&buf);
        let row = screen.grid().visible_row(0);
        let shown: String = (0..6)
            .filter(|&c| !row[c].is_wide_continuation())
            .map(|c| row[c].display_text())
            .collect();
        assert_eq!(shown, text, "the highlight erased a glyph");
    }

    #[test]
    fn a_frame_selection_over_a_shrunk_grid_reads_no_cell_past_it() {
        let view = PaneViewport {
            rect: Rect::new(0, 0, 6, 3),
            grid_rows: 8,
            grid_cols: 30,
            row_offset: 3,
            col_offset: 11,
        };
        let sel = Selection {
            anchor: Some((0, 0)),
            cursor: (5, 2),
        };
        for (rows, cols) in [(4, 30), (8, 12)] {
            let mut vt = VirtualTerminal::new(8, 30);
            vt.process(b"abcdefghijklmnopqrstuvwxyz");
            vt.resize(rows, cols);
            let _ = extract_frame_selection(&vt, &sel, &view);
            let mut buf = Vec::new();
            render_selection_overlay_with_vt_into(&mut buf, &view, &vt, &sel, &Theme::DEFAULT);
        }
    }

    /// Modal copy mode draws and yanks wide glyphs by one rule: one on the
    /// slice's last column would spill into the neighbour, so it is its blank
    /// half on screen and in the yank.
    #[test]
    fn modal_copy_mode_draws_and_yanks_wide_glyphs_as_shown() {
        let mut vt = VirtualTerminal::new(2, 10);
        vt.process("a界b界xyz\x1b[1;1H".as_bytes());
        let mut s = modal(&vt, Rect::new(0, 0, 5, 2));
        let mut buf = Vec::new();
        render_copy_view_into(&mut buf, &vt, &s);
        let mut screen = VirtualTerminal::new(2, 8);
        screen.process(b"\x1b[1;6HNN");
        screen.process(&buf);
        let row = screen.grid().visible_row(0);
        let shown: String = (0..7)
            .filter(|&c| !row[c].is_wide_continuation())
            .map(|c| row[c].display_text())
            .collect();
        assert_eq!(shown, "a界b NN", "a glyph shifted or spilled");
        select(&mut s, Some((0, 0)), (4, 0));
        assert_eq!(
            extract_selection(&vt, &s),
            "a界b",
            "yanked what was not shown"
        );
    }

    #[test]
    fn a_frame_selection_highlights_no_surplus() {
        let mut vt = VirtualTerminal::new(3, 4);
        vt.process(b"abcd\r\nefgh");
        let view = PaneViewport {
            rect: Rect::new(0, 0, 10, 3),
            grid_rows: 3,
            grid_cols: 4,
            row_offset: 0,
            col_offset: 0,
        };
        let sel = Selection {
            anchor: Some((1, 0)),
            cursor: (2, 1),
        };
        let mut buf = Vec::new();
        render_selection_overlay_with_vt_into(&mut buf, &view, &vt, &sel, &Theme::DEFAULT);
        let mut screen = VirtualTerminal::new(3, 10);
        screen.process(&buf);
        let row = screen.grid().visible_row(0);
        let lit: Vec<usize> = (0..10)
            .filter(|&c| row[c].style.bg != Color::Default)
            .collect();
        assert_eq!(lit, vec![1, 2, 3], "highlighted past the grid");
    }
}
