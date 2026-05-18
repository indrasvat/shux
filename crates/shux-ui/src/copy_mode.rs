//! Copy mode — vim-style cursor + selection on the focused pane,
//! yanking via OSC 52 (terminal-driven system clipboard).
//!
//! Captures the common ad-hoc copy use case: move through visible output or
//! scrollback, search, select text, and put it on the system clipboard.
//!
//! Activated by `prefix + [` (tmux convention) → `ActionKind::
//! EnterCopyMode`. While the mode is active the daemon swallows
//! every Input frame and routes the bytes through `handle_key`,
//! which mutates a `CopyModeState`. The render loop overlays a
//! cursor block and (if a selection anchor is set) a reverse-video
//! highlight on the focused pane's rect.

use std::io::Write as _;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

use shux_core::layout::Rect;
use shux_core::theme::{Rgb, Theme};
use shux_vt::{CellFlags, Color, Row, VirtualTerminal};

/// Per-attach session copy-mode state. `None` means the user is in
/// normal pane-input mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyModeState {
    /// 0-based cursor position relative to the focused pane content
    /// rect (NOT screen-absolute). 0,0 = top-left of the pane.
    pub cursor: (u16, u16),
    /// 0-based anchor for the selection. `None` means no selection
    /// has been started — `y` yanks just the cursor cell, which is
    /// rarely what users want, so the UI hint nudges them to press
    /// `v` first.
    pub anchor: Option<(u16, u16)>,
    /// Rows scrolled back from the live bottom of `scrollback + visible`.
    /// 0 means the normal live viewport. Positive values show older rows.
    pub scroll_offset: usize,
    /// Active `/` or `?` prompt while the user is typing a search.
    pub search: Option<SearchState>,
    /// Last accepted search, used by `n` / `N`.
    pub last_search: Option<SearchMatch>,
    /// Tracks the first `g` for the vim-style `gg` top-of-history motion.
    pending_g: bool,
}

impl CopyModeState {
    pub fn new() -> Self {
        Self {
            cursor: (0, 0),
            anchor: None,
            scroll_offset: 0,
            search: None,
            last_search: None,
            pending_g: false,
        }
    }
}

impl Default for CopyModeState {
    fn default() -> Self {
        Self::new()
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
    /// Key not bound; ignored.
    Ignored,
}

/// Decode raw bytes from a single Input frame and update `state`.
/// `pane_cols` / `pane_rows` are the focused pane's content
/// dimensions (excluding borders) — used to clamp cursor motion so
/// it can't run off the pane.
///
/// Only the first byte that maps to a known action is consumed;
/// everything else is dropped. This keeps key-repeat behavior
/// predictable when terminals send escape sequences for arrow keys.
pub fn handle_key(
    bytes: &[u8],
    state: &mut CopyModeState,
    pane_cols: u16,
    pane_rows: u16,
    total_lines: usize,
) -> CopyKey {
    handle_key_with_vt(bytes, state, pane_cols, pane_rows, total_lines, None)
}

pub fn handle_key_with_vt(
    bytes: &[u8],
    state: &mut CopyModeState,
    pane_cols: u16,
    pane_rows: u16,
    total_lines: usize,
    vt: Option<&VirtualTerminal>,
) -> CopyKey {
    if bytes.is_empty() {
        return CopyKey::Ignored;
    }
    if pane_cols == 0 || pane_rows == 0 {
        return CopyKey::Ignored;
    }
    if state.search.is_some() {
        return handle_search_key(bytes, state, pane_cols, pane_rows, vt);
    }
    let max_col = pane_cols.saturating_sub(1);
    let max_row = pane_rows.saturating_sub(1);
    clamp_scroll_offset(state, total_lines, pane_rows);

    // Arrow keys arrive as the 3-byte sequence ESC [ A/B/C/D. Match
    // the prefix first so users can navigate with arrows AND hjkl.
    if bytes.len() >= 3 && bytes[0] == 0x1b && bytes[1] == b'[' {
        match bytes[2] {
            b'A' => return move_up(state, 1).then_updated(),
            b'B' => return move_down(state, 1, max_row).then_updated(),
            b'C' => return move_right(state, 1, max_col).then_updated(),
            b'D' => return move_left(state, 1).then_updated(),
            _ => {}
        }
    }
    // PageUp/PageDown arrive as ESC [ 5 ~ / ESC [ 6 ~.
    if bytes.len() >= 4 && bytes[0] == 0x1b && bytes[1] == b'[' && bytes[3] == b'~' {
        match bytes[2] {
            b'5' => {
                return scroll_up(state, pane_rows as usize, total_lines, pane_rows).then_updated();
            }
            b'6' => {
                return scroll_down(state, pane_rows as usize, total_lines, pane_rows)
                    .then_updated();
            }
            _ => {}
        }
    }

    match bytes[0] {
        b'h' => move_left(state, 1).then_updated(),
        b'j' => move_down(state, 1, max_row).then_updated(),
        b'k' => move_up(state, 1).then_updated(),
        b'l' => move_right(state, 1, max_col).then_updated(),
        b'/' => {
            state.pending_g = false;
            state.search = Some(SearchState {
                direction: SearchDirection::Forward,
                query: String::new(),
            });
            CopyKey::Updated
        }
        b'?' => {
            state.pending_g = false;
            state.search = Some(SearchState {
                direction: SearchDirection::Backward,
                query: String::new(),
            });
            CopyKey::Updated
        }
        b'n' => repeat_search(state, pane_cols, pane_rows, vt, false).then_updated(),
        b'N' => repeat_search(state, pane_cols, pane_rows, vt, true).then_updated(),
        // Ctrl-b / Ctrl-f: full-page up/down. Ctrl-u / Ctrl-d: half-page.
        0x02 => scroll_up(state, pane_rows as usize, total_lines, pane_rows).then_updated(),
        0x06 => scroll_down(state, pane_rows as usize, total_lines, pane_rows).then_updated(),
        0x15 => scroll_up(
            state,
            (pane_rows as usize).max(1) / 2,
            total_lines,
            pane_rows,
        )
        .then_updated(),
        0x04 => scroll_down(
            state,
            (pane_rows as usize).max(1) / 2,
            total_lines,
            pane_rows,
        )
        .then_updated(),
        b'g' => {
            if state.pending_g {
                state.scroll_offset = max_scroll_offset(total_lines, pane_rows);
                state.cursor = (0, 0);
                state.pending_g = false;
                CopyKey::Updated
            } else {
                state.pending_g = true;
                CopyKey::Ignored
            }
        }
        b'G' => {
            state.scroll_offset = 0;
            state.cursor = (0, max_row);
            state.pending_g = false;
            CopyKey::Updated
        }
        b'v' => {
            toggle_anchor(state);
            state.pending_g = false;
            CopyKey::Updated
        }
        b'y' => {
            state.pending_g = false;
            CopyKey::Yank
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

fn handle_search_key(
    bytes: &[u8],
    state: &mut CopyModeState,
    pane_cols: u16,
    pane_rows: u16,
    vt: Option<&VirtualTerminal>,
) -> CopyKey {
    let Some(mut search) = state.search.take() else {
        return CopyKey::Ignored;
    };
    match bytes[0] {
        b'\r' | b'\n' => {
            if search.query.is_empty() {
                return CopyKey::Ignored;
            }
            let matched = find_and_focus(
                state,
                pane_cols,
                pane_rows,
                vt,
                &search.query,
                search.direction,
            );
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
        byte if byte.is_ascii_graphic() || byte == b' ' => {
            if let Ok(text) = std::str::from_utf8(bytes) {
                search.query.push_str(text);
                state.search = Some(search);
                CopyKey::Updated
            } else {
                state.search = Some(search);
                CopyKey::Ignored
            }
        }
        _ => {
            state.search = Some(search);
            CopyKey::Ignored
        }
    }
}

fn repeat_search(
    state: &mut CopyModeState,
    pane_cols: u16,
    pane_rows: u16,
    vt: Option<&VirtualTerminal>,
    reverse: bool,
) -> bool {
    let Some(last) = state.last_search.clone() else {
        return false;
    };
    let direction = if reverse {
        match last.direction {
            SearchDirection::Forward => SearchDirection::Backward,
            SearchDirection::Backward => SearchDirection::Forward,
        }
    } else {
        last.direction
    };
    find_and_focus(state, pane_cols, pane_rows, vt, &last.query, direction)
}

fn find_and_focus(
    state: &mut CopyModeState,
    pane_cols: u16,
    pane_rows: u16,
    vt: Option<&VirtualTerminal>,
    query: &str,
    direction: SearchDirection,
) -> bool {
    if query.is_empty() {
        return false;
    }
    let Some(vt) = vt else {
        return false;
    };
    let grid = vt.grid();
    let total_lines = grid.total_lines();
    if total_lines == 0 {
        return false;
    }

    let view = view_start(total_lines, pane_rows, state.scroll_offset);
    let current_row = view
        .saturating_add(state.cursor.1 as usize)
        .min(total_lines.saturating_sub(1));
    let current_col = state.cursor.0 as usize;
    let found = match direction {
        SearchDirection::Forward => {
            search_forward(grid, pane_cols, current_row, current_col, query)
        }
        SearchDirection::Backward => {
            search_backward(grid, pane_cols, current_row, current_col, query)
        }
    };
    if let Some((row, col)) = found {
        focus_absolute_cell(state, total_lines, pane_rows, row, col as u16);
        true
    } else {
        false
    }
}

fn search_forward(
    grid: &shux_vt::Grid,
    pane_cols: u16,
    current_row: usize,
    current_col: usize,
    query: &str,
) -> Option<(usize, usize)> {
    let total = grid.total_lines();
    for step in 0..total {
        let row_idx = (current_row + step) % total;
        let start_col = if step == 0 {
            current_col.saturating_add(1)
        } else {
            0
        };
        let Some((col, _)) = search_row_forward(grid.row(row_idx)?, pane_cols, start_col, query)
        else {
            continue;
        };
        return Some((row_idx, col));
    }
    None
}

fn search_backward(
    grid: &shux_vt::Grid,
    pane_cols: u16,
    current_row: usize,
    current_col: usize,
    query: &str,
) -> Option<(usize, usize)> {
    let total = grid.total_lines();
    for step in 0..total {
        let row_idx = (current_row + total - (step % total)) % total;
        let before_col = if step == 0 {
            current_col
        } else {
            pane_cols as usize
        };
        let Some((col, _)) = search_row_backward(grid.row(row_idx)?, pane_cols, before_col, query)
        else {
            continue;
        };
        return Some((row_idx, col));
    }
    None
}

fn search_row_forward(
    row: &Row,
    pane_cols: u16,
    start_col: usize,
    query: &str,
) -> Option<(usize, usize)> {
    let text = row_text(row, pane_cols);
    if start_col >= text.chars().count() {
        return None;
    }
    let suffix: String = text.chars().skip(start_col).collect();
    let byte_idx = suffix.find(query)?;
    let rel_col = suffix[..byte_idx].chars().count();
    Some((start_col + rel_col, query.chars().count()))
}

fn search_row_backward(
    row: &Row,
    pane_cols: u16,
    before_col: usize,
    query: &str,
) -> Option<(usize, usize)> {
    let text = row_text(row, pane_cols);
    let prefix: String = text.chars().take(before_col).collect();
    let byte_idx = prefix.rfind(query)?;
    Some((prefix[..byte_idx].chars().count(), query.chars().count()))
}

fn row_text(row: &Row, pane_cols: u16) -> String {
    let mut out = String::new();
    for col in 0..pane_cols as usize {
        let Some(cell) = row.get(col) else {
            break;
        };
        if !cell.is_wide_continuation() {
            out.push(cell.ch);
        }
    }
    out
}

fn focus_absolute_cell(
    state: &mut CopyModeState,
    total_lines: usize,
    pane_rows: u16,
    abs_row: usize,
    col: u16,
) {
    let max_start = total_lines.saturating_sub(pane_rows as usize);
    let preferred_start = abs_row.saturating_sub((pane_rows as usize).saturating_sub(1) / 2);
    let start = preferred_start.min(max_start);
    state.scroll_offset = max_start.saturating_sub(start);
    state.cursor = (
        col,
        abs_row
            .saturating_sub(start)
            .min(pane_rows.saturating_sub(1) as usize) as u16,
    );
    state.pending_g = false;
}

fn move_left(state: &mut CopyModeState, n: u16) -> bool {
    state.pending_g = false;
    let (col, row) = state.cursor;
    let new_col = col.saturating_sub(n);
    if new_col == col {
        return false;
    }
    state.cursor = (new_col, row);
    true
}

fn move_right(state: &mut CopyModeState, n: u16, max_col: u16) -> bool {
    state.pending_g = false;
    let (col, row) = state.cursor;
    let new_col = col.saturating_add(n).min(max_col);
    if new_col == col {
        return false;
    }
    state.cursor = (new_col, row);
    true
}

fn move_up(state: &mut CopyModeState, n: u16) -> bool {
    state.pending_g = false;
    let (col, row) = state.cursor;
    let new_row = row.saturating_sub(n);
    if new_row == row {
        return false;
    }
    state.cursor = (col, new_row);
    true
}

fn move_down(state: &mut CopyModeState, n: u16, max_row: u16) -> bool {
    state.pending_g = false;
    let (col, row) = state.cursor;
    let new_row = row.saturating_add(n).min(max_row);
    if new_row == row {
        return false;
    }
    state.cursor = (col, new_row);
    true
}

fn toggle_anchor(state: &mut CopyModeState) {
    if state.anchor.is_some() {
        state.anchor = None;
    } else {
        state.anchor = Some(state.cursor);
    }
}

/// Maximum scrollback offset for a pane view of `pane_rows` rows.
pub fn max_scroll_offset(total_lines: usize, pane_rows: u16) -> usize {
    total_lines.saturating_sub(pane_rows as usize)
}

/// Start absolute row for the currently displayed copy view.
pub fn view_start(total_lines: usize, pane_rows: u16, scroll_offset: usize) -> usize {
    total_lines
        .saturating_sub(pane_rows as usize)
        .saturating_sub(scroll_offset.min(max_scroll_offset(total_lines, pane_rows)))
}

pub fn scroll_up(
    state: &mut CopyModeState,
    lines: usize,
    total_lines: usize,
    pane_rows: u16,
) -> bool {
    state.pending_g = false;
    let old = state.scroll_offset;
    state.scroll_offset = state
        .scroll_offset
        .saturating_add(lines.max(1))
        .min(max_scroll_offset(total_lines, pane_rows));
    old != state.scroll_offset
}

pub fn scroll_down(
    state: &mut CopyModeState,
    lines: usize,
    _total_lines: usize,
    _pane_rows: u16,
) -> bool {
    state.pending_g = false;
    let old = state.scroll_offset;
    state.scroll_offset = state.scroll_offset.saturating_sub(lines.max(1));
    old != state.scroll_offset
}

fn clamp_scroll_offset(state: &mut CopyModeState, total_lines: usize, pane_rows: u16) {
    state.scroll_offset = state
        .scroll_offset
        .min(max_scroll_offset(total_lines, pane_rows));
}

fn row_for_view(
    vt: &VirtualTerminal,
    pane_rows: u16,
    scroll_offset: usize,
    view_row: u16,
) -> Option<&Row> {
    let grid = vt.grid();
    let abs =
        view_start(grid.total_lines(), pane_rows, scroll_offset).saturating_add(view_row as usize);
    grid.row(abs)
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

/// Extract the text the user has currently selected. The selection is
/// inclusive on both ends; if `anchor` is None, returns just the
/// single character under the cursor (so `y` always emits something).
///
/// Reads from the focused pane's visible grid (no scrollback). Cells
/// outside the pane bounds are treated as blank. The two endpoints
/// are normalized so the user can drag the cursor in any direction.
pub fn extract_selection(
    vt: &VirtualTerminal,
    state: &CopyModeState,
    pane_cols: u16,
    pane_rows: u16,
) -> String {
    let cursor = state.cursor;
    let anchor = state.anchor.unwrap_or(cursor);
    let (start_col, end_col) = if cursor.1 == anchor.1 {
        // Single-line selection: order columns directly.
        if cursor.0 <= anchor.0 {
            (cursor.0, anchor.0)
        } else {
            (anchor.0, cursor.0)
        }
    } else {
        (0, 0) // multi-line: column ordering doesn't apply, see below
    };

    let (start, end) = order_endpoints(anchor, cursor);

    let grid = vt.grid();
    let max_row = pane_rows.min(grid.rows() as u16);
    let max_col = pane_cols.min(grid.cols() as u16);

    let mut out = String::new();
    for row in start.1..=end.1 {
        if row >= max_row {
            break;
        }
        let line_start = if row == start.1 { start.0 } else { 0 };
        let line_end = if row == end.1 {
            end.0
        } else {
            max_col.saturating_sub(1)
        };
        // Single-line case: use directly-ordered columns instead of the
        // reading-order (top→bottom) ordering that order_endpoints
        // gives us. This keeps backwards selection intuitive.
        let (col_lo, col_hi) = if start.1 == end.1 {
            (start_col, end_col)
        } else {
            (line_start, line_end)
        };

        let Some(row_ref) = row_for_view(vt, pane_rows, state.scroll_offset, row) else {
            continue;
        };
        let row_len = row_ref.len() as u16;
        for col in col_lo..=col_hi {
            if col >= max_col || col >= row_len {
                break;
            }
            let cell = &row_ref[col as usize];
            if cell.is_wide_continuation() {
                continue;
            }
            out.push(cell.ch);
        }
        if row != end.1 {
            // Trim trailing whitespace per line for cleaner copies.
            let trimmed = out.trim_end_matches(' ').to_string();
            out = trimmed;
            out.push('\n');
        }
    }
    // Final-line trim too.
    out.trim_end_matches(' ').to_string()
}

/// Draw the focused pane's copy viewport over the normal live pane
/// contents. This is used only while copy mode is active; regular
/// attach rendering and all snapshot paths still use the normal VT
/// visible grid.
pub fn render_copy_view_into(
    buf: &mut Vec<u8>,
    pane: Rect,
    vt: &VirtualTerminal,
    state: &CopyModeState,
) {
    if pane.width == 0 || pane.height == 0 {
        return;
    }
    let grid = vt.grid();
    let start = view_start(grid.total_lines(), pane.height, state.scroll_offset);
    for row in 0..pane.height {
        let abs = start + row as usize;
        let Some(row_ref) = grid.row(abs) else {
            continue;
        };
        let _ = write!(buf, "\x1b[{};{}H", pane.y + row + 1, pane.x + 1);
        for col in 0..pane.width {
            let cell = row_ref.get(col as usize);
            match cell {
                Some(cell) if !cell.is_wide_continuation() => {
                    write_cell(buf, cell);
                }
                _ => {
                    let _ = write!(buf, " ");
                }
            }
        }
        let _ = write!(buf, "\x1b[0m");
    }
}

fn write_cell(buf: &mut Vec<u8>, cell: &shux_vt::Cell) {
    write_style(buf, cell);
    let _ = write!(buf, "{}", cell.ch);
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

/// Order two (col, row) endpoints in reading order (top-left → bottom-right).
fn order_endpoints(a: (u16, u16), b: (u16, u16)) -> ((u16, u16), (u16, u16)) {
    if a.1 < b.1 || (a.1 == b.1 && a.0 <= b.0) {
        (a, b)
    } else {
        (b, a)
    }
}

/// Append ANSI bytes that draw the copy-mode overlay over the focused pane's
/// content rect. Use [`render_copy_overlay_with_vt_into`] when a VT is
/// available so selection can repaint the selected glyphs with a high-contrast
/// palette instead of masking text with a blank band.
pub fn render_copy_overlay_into(
    buf: &mut Vec<u8>,
    pane: Rect,
    state: &CopyModeState,
    theme: &Theme,
) {
    render_copy_overlay_inner(buf, pane, None, state, theme);
}

/// Variant of [`render_copy_overlay_into`] that can redraw selected text from
/// the active VT viewport. This keeps visual selection legible instead of
/// painting an opaque band over the glyphs.
pub fn render_copy_overlay_with_vt_into(
    buf: &mut Vec<u8>,
    pane: Rect,
    vt: &VirtualTerminal,
    state: &CopyModeState,
    theme: &Theme,
) {
    render_copy_overlay_inner(buf, pane, Some(vt), state, theme);
}

/// Draw only the selected text, without the copy-mode cursor or status hint.
///
/// This is used by the human mouse-selection path: a selected range can remain
/// visible while normal pane input keeps flowing to the PTY.
pub fn render_selection_overlay_with_vt_into(
    buf: &mut Vec<u8>,
    pane: Rect,
    vt: &VirtualTerminal,
    state: &CopyModeState,
    theme: &Theme,
) {
    render_selection_overlay_inner(buf, pane, Some(vt), state, theme);
}

/// Variant of [`render_selection_overlay_with_vt_into`] used when the VT has
/// disappeared between frames.
pub fn render_selection_overlay_into(
    buf: &mut Vec<u8>,
    pane: Rect,
    state: &CopyModeState,
    theme: &Theme,
) {
    render_selection_overlay_inner(buf, pane, None, state, theme);
}

fn render_copy_overlay_inner(
    buf: &mut Vec<u8>,
    pane: Rect,
    vt: Option<&VirtualTerminal>,
    state: &CopyModeState,
    theme: &Theme,
) {
    if pane.width < 2 || pane.height < 2 {
        return;
    }

    render_selection_overlay_inner(buf, pane, vt, state, theme);

    // Cursor block — high-contrast inversion of the selection
    // palette so it stays visible whether or not it sits inside a
    // selection run. Selection draws fg=accent on bg=status_bg; the
    // cursor uses fg=status_bg on bg=status_fg (white-ish text bg),
    // which pops against both the selection band and bare pane
    // content.
    let (cx, cy) = state.cursor;
    if cx < pane.width && cy < pane.height {
        cursor_block(
            buf,
            pane.x + cx,
            pane.y + cy,
            theme.status_bg,
            theme.status_fg,
        );
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
    let hint_col = pane.x;
    let usable = pane.width as usize;
    let truncated = truncate_to_width(&hint, usable);
    let pad = usable.saturating_sub(display_width_str(&truncated));
    let _ = write!(
        buf,
        "\x1b[{};{}H{}{}{}{}\x1b[0m",
        hint_row + 1,
        hint_col + 1,
        sgr_bg(theme.status_accent),
        sgr_fg(theme.status_bg),
        truncated,
        " ".repeat(pad),
    );
}

fn render_selection_overlay_inner(
    buf: &mut Vec<u8>,
    pane: Rect,
    vt: Option<&VirtualTerminal>,
    state: &CopyModeState,
    theme: &Theme,
) {
    if pane.width < 1 || pane.height < 1 {
        return;
    }

    if let Some(anchor) = state.anchor {
        let (start, end) = order_endpoints(anchor, state.cursor);
        for row in start.1..=end.1 {
            if row >= pane.height {
                break;
            }
            let (col_lo, col_hi) = if start.1 == end.1 {
                if state.cursor.0 <= anchor.0 {
                    (state.cursor.0, anchor.0)
                } else {
                    (anchor.0, state.cursor.0)
                }
            } else if row == start.1 {
                (start.0, pane.width.saturating_sub(1))
            } else if row == end.1 {
                (0, end.0)
            } else {
                (0, pane.width.saturating_sub(1))
            };
            if let Some(vt) = vt {
                let ctx = SelectionRenderCtx {
                    pane,
                    vt,
                    state,
                    theme,
                };
                selection_text_run(buf, ctx, col_lo, col_hi, row);
            } else {
                highlight_run(
                    buf,
                    pane,
                    col_lo,
                    col_hi,
                    row,
                    theme.status_bg,
                    theme.status_accent,
                );
            }
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
    state: &'a CopyModeState,
    theme: &'a Theme,
}

fn selection_text_run(
    buf: &mut Vec<u8>,
    ctx: SelectionRenderCtx<'_>,
    col_lo: u16,
    col_hi: u16,
    row: u16,
) {
    let Some(row_ref) = row_for_view(ctx.vt, ctx.pane.height, ctx.state.scroll_offset, row) else {
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
    };
    let col_hi = col_hi.min(ctx.pane.width.saturating_sub(1));
    for col in col_lo..=col_hi {
        let ch = row_ref
            .get(col as usize)
            .filter(|cell| !cell.is_wide_continuation())
            .map(|cell| cell.ch)
            .unwrap_or(' ');
        let _ = write!(
            buf,
            "\x1b[{};{}H{}{}\x1b[1m{}\x1b[0m",
            ctx.pane.y + row + 1,
            ctx.pane.x + col + 1,
            sgr_bg(ctx.theme.status_accent),
            sgr_fg(ctx.theme.status_bg),
            ch,
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

fn cursor_block(buf: &mut Vec<u8>, x: u16, y: u16, fg: Rgb, bg: Rgb) {
    let _ = write!(
        buf,
        "\x1b[{};{}H{}{} \x1b[0m",
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
        let mut s = CopyModeState::new();
        // Try to go up/left at origin — no-op.
        assert_eq!(handle_key(b"h", &mut s, 10, 5, 20), CopyKey::Ignored);
        assert_eq!(handle_key(b"k", &mut s, 10, 5, 20), CopyKey::Ignored);
        assert_eq!(s.cursor, (0, 0));
        // Move down 1 + right 1.
        assert_eq!(handle_key(b"j", &mut s, 10, 5, 20), CopyKey::Updated);
        assert_eq!(handle_key(b"l", &mut s, 10, 5, 20), CopyKey::Updated);
        assert_eq!(s.cursor, (1, 1));
    }

    #[test]
    fn handle_key_arrow_keys_work() {
        let mut s = CopyModeState::new();
        assert_eq!(handle_key(b"\x1b[B", &mut s, 10, 5, 20), CopyKey::Updated);
        assert_eq!(handle_key(b"\x1b[C", &mut s, 10, 5, 20), CopyKey::Updated);
        assert_eq!(s.cursor, (1, 1));
    }

    #[test]
    fn handle_key_v_toggles_anchor() {
        let mut s = CopyModeState::new();
        s.cursor = (3, 2);
        handle_key(b"v", &mut s, 10, 5, 20);
        assert_eq!(s.anchor, Some((3, 2)));
        handle_key(b"v", &mut s, 10, 5, 20);
        assert_eq!(s.anchor, None);
    }

    #[test]
    fn handle_key_y_returns_yank() {
        let mut s = CopyModeState::new();
        assert_eq!(handle_key(b"y", &mut s, 10, 5, 20), CopyKey::Yank);
    }

    #[test]
    fn handle_key_q_and_esc_exit() {
        let mut s = CopyModeState::new();
        assert_eq!(handle_key(b"q", &mut s, 10, 5, 20), CopyKey::Exit);
        assert_eq!(handle_key(b"\x1b", &mut s, 10, 5, 20), CopyKey::Exit);
    }

    #[test]
    fn page_keys_adjust_scroll_offset() {
        let mut s = CopyModeState::new();
        assert_eq!(handle_key(b"\x1b[5~", &mut s, 10, 5, 20), CopyKey::Updated);
        assert_eq!(s.scroll_offset, 5);
        assert_eq!(handle_key(b"\x1b[6~", &mut s, 10, 5, 20), CopyKey::Updated);
        assert_eq!(s.scroll_offset, 0);
    }

    #[test]
    fn gg_and_g_jump_between_history_edges() {
        let mut s = CopyModeState::new();
        assert_eq!(handle_key(b"g", &mut s, 10, 5, 20), CopyKey::Ignored);
        assert_eq!(handle_key(b"g", &mut s, 10, 5, 20), CopyKey::Updated);
        assert_eq!(s.scroll_offset, 15);
        assert_eq!(s.cursor, (0, 0));
        assert_eq!(handle_key(b"G", &mut s, 10, 5, 20), CopyKey::Updated);
        assert_eq!(s.scroll_offset, 0);
        assert_eq!(s.cursor, (0, 4));
    }

    #[test]
    fn slash_search_focuses_scrollback_match() {
        let vt = vt_with_lines(5, 20, 20);
        let mut s = CopyModeState {
            cursor: (0, 4),
            ..Default::default()
        };
        assert_eq!(
            handle_key_with_vt(b"/", &mut s, 20, 5, vt.grid().total_lines(), Some(&vt)),
            CopyKey::Updated
        );
        assert_eq!(
            handle_key_with_vt(
                b"line-07",
                &mut s,
                20,
                5,
                vt.grid().total_lines(),
                Some(&vt)
            ),
            CopyKey::Updated
        );
        assert_eq!(
            handle_key_with_vt(b"\r", &mut s, 20, 5, vt.grid().total_lines(), Some(&vt)),
            CopyKey::Updated
        );

        assert_eq!(s.cursor.0, 0);
        assert!(s.scroll_offset > 0);
        assert_eq!(extract_selection(&vt, &s, 20, 5), "l");
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
        let mut s = CopyModeState::new();
        handle_key_with_vt(b"/", &mut s, 20, 5, vt.grid().total_lines(), Some(&vt));
        handle_key_with_vt(b"apple", &mut s, 20, 5, vt.grid().total_lines(), Some(&vt));
        handle_key_with_vt(b"\r", &mut s, 20, 5, vt.grid().total_lines(), Some(&vt));
        let first = s.cursor;

        assert_eq!(
            handle_key_with_vt(b"n", &mut s, 20, 5, vt.grid().total_lines(), Some(&vt)),
            CopyKey::Updated
        );
        assert_ne!(s.cursor, first);
        assert_eq!(
            handle_key_with_vt(b"N", &mut s, 20, 5, vt.grid().total_lines(), Some(&vt)),
            CopyKey::Updated
        );
        assert_eq!(s.cursor, first);
    }

    #[test]
    fn extract_selection_single_line() {
        let vt = vt_with(5, 20, "hello world");
        let state = CopyModeState {
            cursor: (10, 0),
            anchor: Some((6, 0)),
            ..Default::default()
        };
        let s = extract_selection(&vt, &state, 20, 5);
        assert_eq!(s, "world");
    }

    #[test]
    fn extract_selection_no_anchor_returns_single_cell() {
        let vt = vt_with(5, 20, "hello world");
        let state = CopyModeState {
            cursor: (0, 0),
            anchor: None,
            ..Default::default()
        };
        let s = extract_selection(&vt, &state, 20, 5);
        assert_eq!(s, "h");
    }

    #[test]
    fn extract_selection_multi_line() {
        // Two lines: "first" (newline) "second"
        let vt = vt_with(5, 20, "first\r\nsecond");
        let state = CopyModeState {
            cursor: (5, 1),
            anchor: Some((0, 0)),
            ..Default::default()
        };
        let s = extract_selection(&vt, &state, 20, 5);
        assert_eq!(s, "first\nsecond");
    }

    #[test]
    fn extract_selection_handles_reversed_endpoints() {
        let vt = vt_with(5, 20, "hello world");
        let state = CopyModeState {
            cursor: (6, 0),
            anchor: Some((10, 0)),
            ..Default::default()
        };
        let s = extract_selection(&vt, &state, 20, 5);
        assert_eq!(s, "world");
    }

    #[test]
    fn extract_selection_reads_scrolled_history_view() {
        let vt = vt_with_lines(3, 20, 8);
        let total = vt.grid().total_lines();
        let mut state = CopyModeState::new();
        state.scroll_offset = max_scroll_offset(total, 3);
        state.cursor = (6, 0);
        state.anchor = Some((0, 0));
        let s = extract_selection(&vt, &state, 20, 3);
        assert!(s.starts_with("line-00"), "expected oldest row, got {s:?}");
    }

    #[test]
    fn render_copy_view_draws_scrolled_history() {
        let vt = vt_with_lines(3, 20, 8);
        let total = vt.grid().total_lines();
        let state = CopyModeState {
            scroll_offset: max_scroll_offset(total, 3),
            ..Default::default()
        };
        let mut buf = Vec::new();
        render_copy_view_into(&mut buf, Rect::new(0, 0, 20, 3), &vt, &state);
        let rendered = String::from_utf8_lossy(&buf);
        let plain = strip_ansi(&rendered);
        assert!(plain.contains("line-00"), "rendered={rendered:?}");
    }

    #[test]
    fn osc52_emits_well_formed_sequence() {
        let bytes = osc52_copy("hi");
        // ESC ] 52 ; c ; <base64> BEL
        assert_eq!(&bytes[..6], b"\x1b]52;c");
        assert_eq!(*bytes.last().unwrap(), 0x07);
        let payload = std::str::from_utf8(&bytes[7..bytes.len() - 1]).unwrap();
        let decoded = BASE64.decode(payload).unwrap();
        assert_eq!(decoded, b"hi");
    }

    #[test]
    fn render_overlay_emits_cursor_position() {
        let mut buf = Vec::new();
        let pane = Rect::new(2, 3, 20, 10);
        let state = CopyModeState {
            cursor: (4, 2),
            anchor: None,
            ..Default::default()
        };
        render_copy_overlay_into(&mut buf, pane, &state, &Theme::DEFAULT);
        let s = String::from_utf8_lossy(&buf);
        // Cursor is at (pane.x + 4, pane.y + 2) = (6, 5). ANSI is
        // 1-based, so we expect CSI 6;7 H somewhere.
        assert!(s.contains("\x1b[6;7H"), "missing cursor position: {s:?}");
        assert!(s.contains("COPY"), "missing status hint");
    }

    #[test]
    fn render_overlay_with_vt_keeps_selected_text_legible() {
        let vt = vt_with(3, 20, "abcdef");
        let pane = Rect::new(0, 0, 20, 3);
        let state = CopyModeState {
            cursor: (2, 0),
            anchor: Some((0, 0)),
            ..Default::default()
        };
        let mut buf = Vec::new();
        render_copy_overlay_with_vt_into(&mut buf, pane, &vt, &state, &Theme::DEFAULT);
        let rendered = String::from_utf8_lossy(&buf);
        let plain = strip_ansi(&rendered);
        assert!(
            plain.contains("abc"),
            "selection should repaint selected glyphs, rendered={rendered:?}"
        );
        assert!(
            rendered.contains("\x1b[48;2;116;199;236m"),
            "selection should use high-contrast accent background"
        );
        assert!(
            rendered.contains("\x1b[38;2;30;32;48m"),
            "selection should use dark foreground on accent background"
        );
    }

    #[test]
    fn render_selection_overlay_omits_modal_cursor_and_hint() {
        let vt = vt_with(3, 20, "abcdef");
        let pane = Rect::new(0, 0, 20, 3);
        let state = CopyModeState {
            cursor: (2, 0),
            anchor: Some((0, 0)),
            ..Default::default()
        };
        let mut buf = Vec::new();
        render_selection_overlay_with_vt_into(&mut buf, pane, &vt, &state, &Theme::DEFAULT);
        let rendered = String::from_utf8_lossy(&buf);
        let plain = strip_ansi(&rendered);
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

    #[test]
    fn render_overlay_no_cursor_when_pane_too_small() {
        let mut buf = Vec::new();
        let pane = Rect::new(0, 0, 1, 1);
        let state = CopyModeState::new();
        render_copy_overlay_into(&mut buf, pane, &state, &Theme::DEFAULT);
        assert!(buf.is_empty());
    }
}
