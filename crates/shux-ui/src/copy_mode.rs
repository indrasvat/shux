//! Copy mode — vim-style cursor + selection on the focused pane,
//! yanking via OSC 52 (terminal-driven system clipboard).
//!
//! M1 slice of task 021. Captures the most common ad-hoc copy use
//! case: select something visible on screen and put it on the system
//! clipboard. Deferred to follow-ups: real scrollback navigation
//! (the grid has scrollback but we don't expose viewport scrolling
//! yet), search (/?nN), word/line/page motions, visual line/block
//! variants.
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
use shux_vt::VirtualTerminal;

/// Per-attach session copy-mode state. `None` means the user is in
/// normal pane-input mode.
#[derive(Debug, Clone)]
pub struct CopyModeState {
    /// 0-based cursor position relative to the focused pane content
    /// rect (NOT screen-absolute). 0,0 = top-left of the pane.
    pub cursor: (u16, u16),
    /// 0-based anchor for the selection. `None` means no selection
    /// has been started — `y` yanks just the cursor cell, which is
    /// rarely what users want, so the UI hint nudges them to press
    /// `v` first.
    pub anchor: Option<(u16, u16)>,
}

impl CopyModeState {
    pub fn new() -> Self {
        Self {
            cursor: (0, 0),
            anchor: None,
        }
    }
}

impl Default for CopyModeState {
    fn default() -> Self {
        Self::new()
    }
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
) -> CopyKey {
    if bytes.is_empty() {
        return CopyKey::Ignored;
    }
    if pane_cols == 0 || pane_rows == 0 {
        return CopyKey::Ignored;
    }
    let max_col = pane_cols.saturating_sub(1);
    let max_row = pane_rows.saturating_sub(1);

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

    match bytes[0] {
        b'h' => move_left(state, 1).then_updated(),
        b'j' => move_down(state, 1, max_row).then_updated(),
        b'k' => move_up(state, 1).then_updated(),
        b'l' => move_right(state, 1, max_col).then_updated(),
        b'v' => {
            toggle_anchor(state);
            CopyKey::Updated
        }
        b'y' => CopyKey::Yank,
        b'q' | 0x1b => CopyKey::Exit,
        _ => CopyKey::Ignored,
    }
}

fn move_left(state: &mut CopyModeState, n: u16) -> bool {
    let (col, row) = state.cursor;
    let new_col = col.saturating_sub(n);
    if new_col == col {
        return false;
    }
    state.cursor = (new_col, row);
    true
}

fn move_right(state: &mut CopyModeState, n: u16, max_col: u16) -> bool {
    let (col, row) = state.cursor;
    let new_col = col.saturating_add(n).min(max_col);
    if new_col == col {
        return false;
    }
    state.cursor = (new_col, row);
    true
}

fn move_up(state: &mut CopyModeState, n: u16) -> bool {
    let (col, row) = state.cursor;
    let new_row = row.saturating_sub(n);
    if new_row == row {
        return false;
    }
    state.cursor = (col, new_row);
    true
}

fn move_down(state: &mut CopyModeState, n: u16, max_row: u16) -> bool {
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

        let row_ref = grid.visible_row(row as usize);
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

/// Order two (col, row) endpoints in reading order (top-left → bottom-right).
fn order_endpoints(a: (u16, u16), b: (u16, u16)) -> ((u16, u16), (u16, u16)) {
    if a.1 < b.1 || (a.1 == b.1 && a.0 <= b.0) {
        (a, b)
    } else {
        (b, a)
    }
}

/// Append ANSI bytes that draw the copy-mode overlay over the focused
/// pane's content rect. Two layers: a selection background (if anchor
/// is set) and a cursor block. Both use the theme's status_accent /
/// status_bg / status_fg so they match the rest of the chrome.
pub fn render_copy_overlay_into(
    buf: &mut Vec<u8>,
    pane: Rect,
    state: &CopyModeState,
    theme: &Theme,
) {
    if pane.width < 2 || pane.height < 2 {
        return;
    }

    // Selection highlight, if any.
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
            highlight_run(
                buf,
                pane,
                col_lo,
                col_hi,
                row,
                theme.status_accent,
                theme.status_bg,
            );
        }
    }

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
    let hint = if state.anchor.is_some() {
        " COPY  hjkl move  y yank  q exit "
    } else {
        " COPY  hjkl move  v select  y yank  q exit "
    };
    let hint_row = pane.y + pane.height.saturating_sub(1);
    let hint_col = pane.x;
    let usable = pane.width as usize;
    let truncated = truncate_to_width(hint, usable);
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

    #[test]
    fn handle_key_hjkl_clamps_to_pane() {
        let mut s = CopyModeState::new();
        // Try to go up/left at origin — no-op.
        assert_eq!(handle_key(b"h", &mut s, 10, 5), CopyKey::Ignored);
        assert_eq!(handle_key(b"k", &mut s, 10, 5), CopyKey::Ignored);
        assert_eq!(s.cursor, (0, 0));
        // Move down 1 + right 1.
        assert_eq!(handle_key(b"j", &mut s, 10, 5), CopyKey::Updated);
        assert_eq!(handle_key(b"l", &mut s, 10, 5), CopyKey::Updated);
        assert_eq!(s.cursor, (1, 1));
    }

    #[test]
    fn handle_key_arrow_keys_work() {
        let mut s = CopyModeState::new();
        assert_eq!(handle_key(b"\x1b[B", &mut s, 10, 5), CopyKey::Updated);
        assert_eq!(handle_key(b"\x1b[C", &mut s, 10, 5), CopyKey::Updated);
        assert_eq!(s.cursor, (1, 1));
    }

    #[test]
    fn handle_key_v_toggles_anchor() {
        let mut s = CopyModeState::new();
        s.cursor = (3, 2);
        handle_key(b"v", &mut s, 10, 5);
        assert_eq!(s.anchor, Some((3, 2)));
        handle_key(b"v", &mut s, 10, 5);
        assert_eq!(s.anchor, None);
    }

    #[test]
    fn handle_key_y_returns_yank() {
        let mut s = CopyModeState::new();
        assert_eq!(handle_key(b"y", &mut s, 10, 5), CopyKey::Yank);
    }

    #[test]
    fn handle_key_q_and_esc_exit() {
        let mut s = CopyModeState::new();
        assert_eq!(handle_key(b"q", &mut s, 10, 5), CopyKey::Exit);
        assert_eq!(handle_key(b"\x1b", &mut s, 10, 5), CopyKey::Exit);
    }

    #[test]
    fn extract_selection_single_line() {
        let vt = vt_with(5, 20, "hello world");
        let state = CopyModeState {
            cursor: (10, 0),
            anchor: Some((6, 0)),
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
        };
        let s = extract_selection(&vt, &state, 20, 5);
        assert_eq!(s, "world");
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
        };
        render_copy_overlay_into(&mut buf, pane, &state, &Theme::DEFAULT);
        let s = String::from_utf8_lossy(&buf);
        // Cursor is at (pane.x + 4, pane.y + 2) = (6, 5). ANSI is
        // 1-based, so we expect CSI 6;7 H somewhere.
        assert!(s.contains("\x1b[6;7H"), "missing cursor position: {s:?}");
        assert!(s.contains("COPY"), "missing status hint");
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
