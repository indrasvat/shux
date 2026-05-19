use tracing::trace;

use crate::cell::{Cell, CellFlags, CellStyle, Color, TerminalDefaultColors};
use crate::cursor::{Cursor, CursorShape};
use crate::grid::{Grid, GridConfig};

/// Terminal mode flags (DECSET/DECRST).
#[derive(Debug, Clone)]
pub struct TerminalModes {
    /// DECAWM -- auto-wrap mode (default: true).
    pub auto_wrap: bool,
    /// DECCKM -- cursor keys mode (application vs normal).
    pub application_cursor_keys: bool,
    /// DECOM -- origin mode (cursor relative to scroll region).
    pub origin_mode: bool,
    /// DECTCEM -- text cursor enable mode (cursor visibility via mode).
    pub cursor_visible: bool,
    /// Bracketed paste mode (Mode 2004).
    pub bracketed_paste: bool,
    /// Mouse tracking modes.
    pub mouse_tracking: MouseMode,
    /// Alternate screen buffer active.
    pub alternate_screen: bool,
    /// Insert mode (IRM).
    pub insert_mode: bool,
    /// Newline mode (LNM): LF also does CR.
    pub newline_mode: bool,
}

impl Default for TerminalModes {
    fn default() -> Self {
        TerminalModes {
            auto_wrap: true,
            application_cursor_keys: false,
            origin_mode: false,
            cursor_visible: true,
            bracketed_paste: false,
            mouse_tracking: MouseMode::None,
            alternate_screen: false,
            insert_mode: false,
            newline_mode: false,
        }
    }
}

/// Mouse tracking mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseMode {
    #[default]
    None,
    /// Mode 1000 -- normal tracking (button press/release).
    Normal,
    /// Mode 1002 -- button event tracking (press/release/motion with button).
    ButtonEvent,
    /// Mode 1003 -- any event tracking (all motion).
    AnyEvent,
}

/// Scroll region (top and bottom margins, 0-indexed inclusive).
#[derive(Debug, Clone, Copy)]
pub struct ScrollRegion {
    pub top: usize,
    pub bottom: usize,
}

/// The VT handler that translates escape sequences into grid operations.
///
/// This struct is NOT the public API -- VirtualTerminal (in lib.rs) owns this
/// and delegates parsed bytes to it. The handler modifies the grid and cursor
/// directly.
pub struct VtHandler<'a> {
    pub grid: &'a mut Grid,
    pub cursor: &'a mut Cursor,
    pub modes: &'a mut TerminalModes,
    pub scroll_region: &'a mut ScrollRegion,
    pub title: &'a mut Option<String>,
    pub default_colors: &'a mut TerminalDefaultColors,
    pub alt_grid: &'a mut Option<Grid>,
    pub alt_cursor: &'a mut Option<Cursor>,
}

impl<'a> VtHandler<'a> {
    /// Write a character at the current cursor position, advancing the cursor.
    fn write_char(&mut self, ch: char) {
        let width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
        let cols = self.grid.cols();
        let rows = self.grid.rows();

        // Handle auto-wrap pending state.
        if self.cursor.auto_wrap_pending {
            if self.modes.auto_wrap {
                self.cursor.col = 0;
                self.cursor.auto_wrap_pending = false;
                // Mark the current row as wrapped.
                self.grid.visible_row_mut(self.cursor.row).wrapped = true;
                if self.cursor.row == self.scroll_region.bottom {
                    self.grid
                        .scroll_up(self.scroll_region.top, self.scroll_region.bottom);
                } else {
                    self.cursor.row += 1;
                }
            } else {
                // No auto-wrap: overwrite last column.
                self.cursor.col = cols.saturating_sub(1);
                self.cursor.auto_wrap_pending = false;
            }
        }

        // Ensure cursor is in bounds.
        if self.cursor.col >= cols {
            self.cursor.col = cols.saturating_sub(1);
        }
        if self.cursor.row >= rows {
            self.cursor.row = rows.saturating_sub(1);
        }

        // Insert mode: shift characters right.
        if self.modes.insert_mode {
            self.grid
                .insert_chars(self.cursor.row, self.cursor.col, width);
        }

        // Write the cell.
        let row = self.grid.visible_row_mut(self.cursor.row);
        row[self.cursor.col] = Cell {
            ch,
            width: width as u8,
            style: self.cursor.style,
            extended: None,
        };

        // For wide characters, write a continuation cell.
        if width == 2 && self.cursor.col + 1 < cols {
            row[self.cursor.col + 1] = Cell::wide_continuation();
        }

        // Advance cursor.
        self.cursor.col += width;
        if self.cursor.col >= cols {
            self.cursor.col = cols.saturating_sub(1);
            self.cursor.auto_wrap_pending = true;
        }
    }

    /// Carriage return: move cursor to column 0.
    fn carriage_return(&mut self) {
        self.cursor.col = 0;
        self.cursor.auto_wrap_pending = false;
    }

    /// Line feed: move cursor down, scrolling if at bottom of scroll region.
    fn linefeed(&mut self) {
        if self.cursor.row == self.scroll_region.bottom {
            self.grid
                .scroll_up(self.scroll_region.top, self.scroll_region.bottom);
        } else if self.cursor.row < self.grid.rows() - 1 {
            self.cursor.row += 1;
        }
        if self.modes.newline_mode {
            self.cursor.col = 0;
        }
        self.cursor.auto_wrap_pending = false;
    }

    /// Reverse index (ESC M): move cursor up, scrolling down if at top of scroll region.
    fn reverse_index(&mut self) {
        if self.cursor.row == self.scroll_region.top {
            self.grid
                .scroll_down(self.scroll_region.top, self.scroll_region.bottom);
        } else if self.cursor.row > 0 {
            self.cursor.row -= 1;
        }
        self.cursor.auto_wrap_pending = false;
    }

    /// Apply an SGR (Select Graphic Rendition) parameter to the cursor style.
    fn apply_sgr(&mut self, param: u16) {
        match param {
            0 => self.cursor.style = CellStyle::default(),
            1 => self.cursor.style.flags.set(CellFlags::BOLD),
            2 => self.cursor.style.flags.set(CellFlags::DIM),
            3 => self.cursor.style.flags.set(CellFlags::ITALIC),
            4 => self.cursor.style.flags.set(CellFlags::UNDERLINE),
            5 | 6 => self.cursor.style.flags.set(CellFlags::BLINK),
            7 => self.cursor.style.flags.set(CellFlags::INVERSE),
            8 => self.cursor.style.flags.set(CellFlags::HIDDEN),
            9 => self.cursor.style.flags.set(CellFlags::STRIKETHROUGH),
            21 => self.cursor.style.flags.unset(CellFlags::BOLD),
            22 => {
                self.cursor.style.flags.unset(CellFlags::BOLD);
                self.cursor.style.flags.unset(CellFlags::DIM);
            }
            23 => self.cursor.style.flags.unset(CellFlags::ITALIC),
            24 => self.cursor.style.flags.unset(CellFlags::UNDERLINE),
            25 => self.cursor.style.flags.unset(CellFlags::BLINK),
            27 => self.cursor.style.flags.unset(CellFlags::INVERSE),
            28 => self.cursor.style.flags.unset(CellFlags::HIDDEN),
            29 => self.cursor.style.flags.unset(CellFlags::STRIKETHROUGH),
            // Standard foreground colors (30-37).
            30..=37 => self.cursor.style.fg = Color::Indexed((param - 30) as u8),
            38 => {} // Extended foreground (handled via sub-params in csi_dispatch).
            39 => self.cursor.style.fg = Color::Default,
            // Standard background colors (40-47).
            40..=47 => self.cursor.style.bg = Color::Indexed((param - 40) as u8),
            48 => {} // Extended background (handled via sub-params in csi_dispatch).
            49 => self.cursor.style.bg = Color::Default,
            // Bright foreground colors (90-97).
            90..=97 => self.cursor.style.fg = Color::Indexed((param - 90 + 8) as u8),
            // Bright background colors (100-107).
            100..=107 => self.cursor.style.bg = Color::Indexed((param - 100 + 8) as u8),
            _ => trace!(sgr = param, "unhandled SGR parameter"),
        }
    }

    /// Handle DECSET/DECRST private mode toggles.
    fn set_private_mode(&mut self, mode: u16, enable: bool) {
        match mode {
            // DECCKM -- Cursor keys mode.
            1 => self.modes.application_cursor_keys = enable,
            // DECOM -- Origin mode.
            6 => self.modes.origin_mode = enable,
            // DECAWM -- Auto-wrap mode.
            7 => self.modes.auto_wrap = enable,
            // DECTCEM -- Text cursor enable.
            25 => {
                self.modes.cursor_visible = enable;
                self.cursor.visible = enable;
            }
            // Mouse tracking: normal (1000).
            1000 => {
                self.modes.mouse_tracking = if enable {
                    MouseMode::Normal
                } else {
                    MouseMode::None
                };
            }
            // Mouse tracking: button event (1002).
            1002 => {
                self.modes.mouse_tracking = if enable {
                    MouseMode::ButtonEvent
                } else {
                    MouseMode::None
                };
            }
            // Mouse tracking: any event (1003).
            1003 => {
                self.modes.mouse_tracking = if enable {
                    MouseMode::AnyEvent
                } else {
                    MouseMode::None
                };
            }
            // Alternate screen buffer (1047, 1049).
            1047 | 1049 => {
                if enable {
                    if mode == 1049 {
                        self.cursor.save(self.modes.origin_mode);
                    }
                    // Enter alternate screen: swap grids.
                    let rows = self.grid.rows();
                    let cols = self.grid.cols();
                    let config = GridConfig { max_scrollback: 0 };
                    let alt_grid = Grid::new(rows, cols, config);
                    let alt_cursor = Cursor::new();
                    *self.alt_grid = Some(std::mem::replace(self.grid, alt_grid));
                    *self.alt_cursor = Some(std::mem::replace(self.cursor, alt_cursor));
                    self.modes.alternate_screen = true;
                } else {
                    // Leave alternate screen: restore grids.
                    if let Some(primary_grid) = self.alt_grid.take() {
                        *self.grid = primary_grid;
                    }
                    if let Some(primary_cursor) = self.alt_cursor.take() {
                        *self.cursor = primary_cursor;
                    }
                    self.modes.alternate_screen = false;
                    if mode == 1049 {
                        let origin = self.cursor.restore();
                        if let Some(o) = origin {
                            self.modes.origin_mode = o;
                        }
                    }
                }
            }
            // Bracketed paste mode (2004).
            2004 => self.modes.bracketed_paste = enable,
            _ => trace!(mode, enable, "unhandled private mode"),
        }
    }
}

impl<'a> vte::Perform for VtHandler<'a> {
    fn print(&mut self, ch: char) {
        self.write_char(ch);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            // BEL -- bell.
            0x07 => { /* emit bell event in the future */ }
            // BS -- backspace.
            0x08 => {
                if self.cursor.col > 0 {
                    self.cursor.col -= 1;
                    self.cursor.auto_wrap_pending = false;
                }
            }
            // HT -- horizontal tab.
            0x09 => {
                let next_tab = (self.cursor.col / 8 + 1) * 8;
                self.cursor.col = next_tab.min(self.grid.cols() - 1);
                self.cursor.auto_wrap_pending = false;
            }
            // LF, VT, FF -- linefeed variants.
            0x0A..=0x0C => self.linefeed(),
            // CR -- carriage return.
            0x0D => self.carriage_return(),
            // SO (0x0E), SI (0x0F) -- character set shift (ignored for now).
            _ => trace!(byte, "unhandled C0 control"),
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        intermediates: &[u8],
        _ignore: bool,
        action: char,
    ) {
        // Flatten params: each subparam slice is collected into a flat Vec<u16>.
        let params_vec: Vec<u16> = params
            .iter()
            .flat_map(|subparam| subparam.iter().copied())
            .collect();
        let p = |idx: usize, default: u16| -> u16 {
            params_vec
                .get(idx)
                .copied()
                .filter(|&v| v != 0)
                .unwrap_or(default)
        };
        let rows = self.grid.rows();
        let cols = self.grid.cols();

        match (action, intermediates) {
            // CUU -- Cursor Up.
            ('A', []) => {
                let n = p(0, 1) as usize;
                self.cursor.row = self.cursor.row.saturating_sub(n);
                self.cursor.auto_wrap_pending = false;
            }
            // CUD -- Cursor Down.
            ('B', []) => {
                let n = p(0, 1) as usize;
                self.cursor.row = (self.cursor.row + n).min(rows - 1);
                self.cursor.auto_wrap_pending = false;
            }
            // CUF -- Cursor Forward.
            ('C', []) => {
                let n = p(0, 1) as usize;
                self.cursor.col = (self.cursor.col + n).min(cols - 1);
                self.cursor.auto_wrap_pending = false;
            }
            // CUB -- Cursor Backward.
            ('D', []) => {
                let n = p(0, 1) as usize;
                self.cursor.col = self.cursor.col.saturating_sub(n);
                self.cursor.auto_wrap_pending = false;
            }
            // CNL -- Cursor Next Line.
            ('E', []) => {
                let n = p(0, 1) as usize;
                self.cursor.row = (self.cursor.row + n).min(rows - 1);
                self.cursor.col = 0;
                self.cursor.auto_wrap_pending = false;
            }
            // CPL -- Cursor Previous Line.
            ('F', []) => {
                let n = p(0, 1) as usize;
                self.cursor.row = self.cursor.row.saturating_sub(n);
                self.cursor.col = 0;
                self.cursor.auto_wrap_pending = false;
            }
            // CHA -- Cursor Character Absolute (column).
            ('G', []) => {
                let col = (p(0, 1) as usize).saturating_sub(1).min(cols - 1);
                self.cursor.col = col;
                self.cursor.auto_wrap_pending = false;
            }
            // CUP / HVP -- Cursor Position.
            ('H', []) | ('f', []) => {
                let row = (p(0, 1) as usize).saturating_sub(1).min(rows - 1);
                let col = (p(1, 1) as usize).saturating_sub(1).min(cols - 1);
                self.cursor.row = row;
                self.cursor.col = col;
                self.cursor.auto_wrap_pending = false;
            }
            // ED -- Erase in Display.
            ('J', []) => {
                let bg = self.cursor.style.bg;
                match p(0, 0) {
                    0 => {
                        // Clear from cursor to end.
                        self.grid.erase_chars(
                            self.cursor.row,
                            self.cursor.col,
                            cols - self.cursor.col,
                            bg,
                        );
                        if self.cursor.row + 1 < rows {
                            self.grid.clear_below(self.cursor.row + 1, bg);
                        }
                    }
                    1 => {
                        // Clear from beginning to cursor.
                        if self.cursor.row > 0 {
                            self.grid.clear_above(self.cursor.row - 1, bg);
                        }
                        self.grid
                            .erase_chars(self.cursor.row, 0, self.cursor.col + 1, bg);
                    }
                    2 => {
                        // Clear entire screen.
                        self.grid.clear_visible(bg);
                    }
                    3 => {
                        // Clear screen + scrollback (xterm extension).
                        self.grid.clear_visible(bg);
                        self.grid.clear_scrollback();
                    }
                    _ => {}
                }
            }
            // EL -- Erase in Line.
            ('K', []) => {
                let bg = self.cursor.style.bg;
                match p(0, 0) {
                    0 => self.grid.erase_chars(
                        self.cursor.row,
                        self.cursor.col,
                        cols - self.cursor.col,
                        bg,
                    ),
                    1 => self
                        .grid
                        .erase_chars(self.cursor.row, 0, self.cursor.col + 1, bg),
                    2 => self.grid.erase_chars(self.cursor.row, 0, cols, bg),
                    _ => {}
                }
            }
            // IL -- Insert Lines.
            ('L', []) => {
                let n = p(0, 1) as usize;
                for _ in 0..n {
                    self.grid
                        .scroll_down(self.cursor.row, self.scroll_region.bottom);
                }
            }
            // DL -- Delete Lines.
            ('M', []) => {
                let n = p(0, 1) as usize;
                for _ in 0..n {
                    self.grid
                        .scroll_up(self.cursor.row, self.scroll_region.bottom);
                }
            }
            // ICH -- Insert Characters.
            ('@', []) => {
                let n = p(0, 1) as usize;
                self.grid.insert_chars(self.cursor.row, self.cursor.col, n);
            }
            // DCH -- Delete Characters.
            ('P', []) => {
                let n = p(0, 1) as usize;
                self.grid.delete_chars(self.cursor.row, self.cursor.col, n);
            }
            // ECH -- Erase Characters.
            ('X', []) => {
                let n = p(0, 1) as usize;
                self.grid
                    .erase_chars(self.cursor.row, self.cursor.col, n, self.cursor.style.bg);
            }
            // VPA -- Vertical Line Position Absolute.
            ('d', []) => {
                let row = (p(0, 1) as usize).saturating_sub(1).min(rows - 1);
                self.cursor.row = row;
                self.cursor.auto_wrap_pending = false;
            }
            // SGR -- Select Graphic Rendition.
            ('m', []) => {
                if params_vec.is_empty() {
                    self.apply_sgr(0);
                    return;
                }
                let mut i = 0;
                while i < params_vec.len() {
                    match params_vec[i] {
                        38 if i + 4 < params_vec.len() && params_vec[i + 1] == 2 => {
                            // 38;2;R;G;B -- 24-bit foreground.
                            self.cursor.style.fg = Color::Rgb(
                                params_vec[i + 2] as u8,
                                params_vec[i + 3] as u8,
                                params_vec[i + 4] as u8,
                            );
                            i += 5;
                        }
                        38 if i + 2 < params_vec.len() && params_vec[i + 1] == 5 => {
                            // 38;5;N -- 256-color foreground.
                            self.cursor.style.fg = Color::Indexed(params_vec[i + 2] as u8);
                            i += 3;
                        }
                        48 if i + 4 < params_vec.len() && params_vec[i + 1] == 2 => {
                            // 48;2;R;G;B -- 24-bit background.
                            self.cursor.style.bg = Color::Rgb(
                                params_vec[i + 2] as u8,
                                params_vec[i + 3] as u8,
                                params_vec[i + 4] as u8,
                            );
                            i += 5;
                        }
                        48 if i + 2 < params_vec.len() && params_vec[i + 1] == 5 => {
                            // 48;5;N -- 256-color background.
                            self.cursor.style.bg = Color::Indexed(params_vec[i + 2] as u8);
                            i += 3;
                        }
                        other => {
                            self.apply_sgr(other);
                            i += 1;
                        }
                    }
                }
            }
            // DECSTBM -- Set Scrolling Region.
            ('r', []) => {
                let top = (p(0, 1) as usize).saturating_sub(1);
                let bottom = (p(1, rows as u16) as usize).saturating_sub(1).min(rows - 1);
                if top < bottom {
                    self.scroll_region.top = top;
                    self.scroll_region.bottom = bottom;
                }
                self.cursor.row = 0;
                self.cursor.col = 0;
                self.cursor.auto_wrap_pending = false;
            }
            // SM -- Set Mode (standard modes).
            ('h', []) => {
                for &param in &params_vec {
                    match param {
                        // IRM -- Insert/Replace mode.
                        4 => self.modes.insert_mode = true,
                        // LNM -- Newline mode.
                        20 => self.modes.newline_mode = true,
                        _ => trace!(param, "unhandled SM mode"),
                    }
                }
            }
            // RM -- Reset Mode (standard modes).
            ('l', []) => {
                for &param in &params_vec {
                    match param {
                        // IRM -- Insert/Replace mode.
                        4 => self.modes.insert_mode = false,
                        // LNM -- Newline mode.
                        20 => self.modes.newline_mode = false,
                        _ => trace!(param, "unhandled RM mode"),
                    }
                }
            }
            // DECSET -- set private mode.
            ('h', [b'?']) => {
                for &param in &params_vec {
                    self.set_private_mode(param, true);
                }
            }
            // DECRST -- reset private mode.
            ('l', [b'?']) => {
                for &param in &params_vec {
                    self.set_private_mode(param, false);
                }
            }
            // DECSCUSR -- Set Cursor Style (CSI Ps SP q).
            ('q', [b' ']) => {
                self.cursor.shape = match p(0, 1) {
                    0 | 1 => CursorShape::Block,
                    2 => CursorShape::Block, // steady block
                    3 | 4 => CursorShape::Underline,
                    5 | 6 => CursorShape::Bar,
                    _ => CursorShape::Block,
                };
            }
            _ => {
                trace!(
                    action = %action,
                    intermediates = ?intermediates,
                    params = ?params_vec,
                    "unhandled CSI sequence"
                );
            }
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        match (byte, intermediates) {
            // DECSC -- Save Cursor (ESC 7).
            (b'7', []) => self.cursor.save(self.modes.origin_mode),
            // DECRC -- Restore Cursor (ESC 8).
            (b'8', []) => {
                if let Some(origin) = self.cursor.restore() {
                    self.modes.origin_mode = origin;
                }
            }
            // RI -- Reverse Index (ESC M).
            (b'M', []) => self.reverse_index(),
            // IND -- Index (ESC D) -- move cursor down, scroll if needed.
            (b'D', []) => self.linefeed(),
            // NEL -- Next Line (ESC E).
            (b'E', []) => {
                self.carriage_return();
                self.linefeed();
            }
            // RIS -- Full Reset (ESC c).
            (b'c', []) => {
                self.grid.clear_visible(Color::Default);
                self.grid.clear_scrollback();
                *self.cursor = Cursor::new();
                *self.modes = TerminalModes::default();
                *self.default_colors = TerminalDefaultColors::default();
                self.scroll_region.top = 0;
                self.scroll_region.bottom = self.grid.rows().saturating_sub(1);
            }
            _ => {
                trace!(
                    byte,
                    intermediates = ?intermediates,
                    "unhandled ESC sequence"
                );
            }
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if params.is_empty() {
            return;
        }
        match params[0] {
            // OSC 0 -- Set Icon Name and Window Title.
            // OSC 2 -- Set Window Title.
            b"0" | b"2" => {
                if let Some(title_bytes) = params.get(1) {
                    if let Ok(title) = std::str::from_utf8(title_bytes) {
                        *self.title = Some(title.to_string());
                    }
                }
            }
            // OSC 10/11 -- Set dynamic default foreground/background.
            b"10" | b"11" => {
                if let Some(color_bytes) = params.get(1) {
                    if let Ok(color) = parse_osc_color(color_bytes) {
                        if params[0] == b"10" {
                            self.default_colors.fg = Some(color);
                        } else {
                            self.default_colors.bg = Some(color);
                        }
                    }
                }
            }
            // OSC 110/111 -- Reset dynamic default foreground/background.
            b"110" => self.default_colors.fg = None,
            b"111" => self.default_colors.bg = None,
            _ => {
                trace!(osc = ?params[0], "unhandled OSC sequence");
            }
        }
    }

    fn hook(&mut self, _params: &vte::Params, _intermediates: &[u8], _ignore: bool, _action: char) {
        // DCS sequences -- not needed for M0.
    }

    fn put(&mut self, _byte: u8) {
        // DCS payload -- not needed for M0.
    }

    fn unhook(&mut self) {
        // DCS termination -- not needed for M0.
    }
}

fn parse_osc_color(bytes: &[u8]) -> Result<[u8; 3], ()> {
    let s = std::str::from_utf8(bytes).map_err(|_| ())?;
    if let Some(hex) = s.strip_prefix('#') {
        return parse_hex_color(hex);
    }
    if let Some(rgb) = s.strip_prefix("rgb:") {
        return parse_rgb_color(rgb);
    }
    Err(())
}

fn parse_hex_color(hex: &str) -> Result<[u8; 3], ()> {
    if hex.len() != 6 {
        return Err(());
    }
    let r = u8::from_str_radix(&hex[0..2], 16).map_err(|_| ())?;
    let g = u8::from_str_radix(&hex[2..4], 16).map_err(|_| ())?;
    let b = u8::from_str_radix(&hex[4..6], 16).map_err(|_| ())?;
    Ok([r, g, b])
}

fn parse_rgb_color(rgb: &str) -> Result<[u8; 3], ()> {
    let mut parts = rgb.split('/');
    let r = parse_rgb_component(parts.next().ok_or(())?)?;
    let g = parse_rgb_component(parts.next().ok_or(())?)?;
    let b = parse_rgb_component(parts.next().ok_or(())?)?;
    if parts.next().is_some() {
        return Err(());
    }
    Ok([r, g, b])
}

fn parse_rgb_component(component: &str) -> Result<u8, ()> {
    if component.is_empty() || component.len() > 4 {
        return Err(());
    }
    let value = u16::from_str_radix(component, 16).map_err(|_| ())?;
    let max = (1u32 << (component.len() * 4)) - 1;
    Ok(((value as u32 * 255 + max / 2) / max) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::GridConfig;

    /// Helper to create a VtHandler and all backing state for testing.
    struct TestTerminal {
        grid: Grid,
        cursor: Cursor,
        modes: TerminalModes,
        scroll_region: ScrollRegion,
        title: Option<String>,
        default_colors: TerminalDefaultColors,
        alt_grid: Option<Grid>,
        alt_cursor: Option<Cursor>,
        parser: vte::Parser,
    }

    impl TestTerminal {
        fn new(rows: usize, cols: usize) -> Self {
            TestTerminal {
                grid: Grid::new(rows, cols, GridConfig::default()),
                cursor: Cursor::new(),
                modes: TerminalModes::default(),
                scroll_region: ScrollRegion {
                    top: 0,
                    bottom: rows.saturating_sub(1),
                },
                title: None,
                default_colors: TerminalDefaultColors::default(),
                alt_grid: None,
                alt_cursor: None,
                parser: vte::Parser::new(),
            }
        }

        fn process(&mut self, bytes: &[u8]) {
            let mut handler = VtHandler {
                grid: &mut self.grid,
                cursor: &mut self.cursor,
                modes: &mut self.modes,
                scroll_region: &mut self.scroll_region,
                title: &mut self.title,
                default_colors: &mut self.default_colors,
                alt_grid: &mut self.alt_grid,
                alt_cursor: &mut self.alt_cursor,
            };
            self.parser.advance(&mut handler, bytes);
        }
    }

    #[test]
    fn test_write_char() {
        let mut t = TestTerminal::new(24, 80);
        t.process(b"A");
        assert_eq!(t.grid.visible_row(0)[0].ch, 'A');
        assert_eq!(t.cursor.col, 1);
    }

    #[test]
    fn test_linefeed() {
        let mut t = TestTerminal::new(3, 10);
        t.process(b"A\r\nB");
        assert_eq!(t.grid.visible_row(0)[0].ch, 'A');
        assert_eq!(t.grid.visible_row(1)[0].ch, 'B');
        assert_eq!(t.cursor.row, 1);
    }

    #[test]
    fn test_linefeed_without_cr() {
        let mut t = TestTerminal::new(3, 10);
        t.process(b"A\nB");
        assert_eq!(t.grid.visible_row(0)[0].ch, 'A');
        // LF only moves down, not to column 0. Cursor was at col 1 after 'A'.
        assert_eq!(t.grid.visible_row(1)[1].ch, 'B');
        assert_eq!(t.cursor.row, 1);
        assert_eq!(t.cursor.col, 2);
    }

    #[test]
    fn test_carriage_return() {
        let mut t = TestTerminal::new(3, 10);
        t.process(b"ABC\rD");
        assert_eq!(t.grid.visible_row(0)[0].ch, 'D');
        assert_eq!(t.cursor.col, 1);
    }

    #[test]
    fn test_backspace() {
        let mut t = TestTerminal::new(3, 10);
        t.process(b"AB\x08C");
        assert_eq!(t.grid.visible_row(0)[0].ch, 'A');
        assert_eq!(t.grid.visible_row(0)[1].ch, 'C');
    }

    #[test]
    fn test_tab() {
        let mut t = TestTerminal::new(3, 80);
        t.process(b"A\tB");
        assert_eq!(t.grid.visible_row(0)[0].ch, 'A');
        assert_eq!(t.cursor.col, 9); // 'B' at col 8, cursor at 9
        assert_eq!(t.grid.visible_row(0)[8].ch, 'B');
    }

    #[test]
    fn test_sgr_bold() {
        let mut t = TestTerminal::new(24, 80);
        t.process(b"\x1b[1mX");
        let cell = &t.grid.visible_row(0)[0];
        assert!(cell.style.flags.contains(CellFlags::BOLD));
    }

    #[test]
    fn test_sgr_reset() {
        let mut t = TestTerminal::new(24, 80);
        t.process(b"\x1b[1;31m\x1b[0mX");
        let cell = &t.grid.visible_row(0)[0];
        assert!(!cell.style.flags.contains(CellFlags::BOLD));
        assert_eq!(cell.style.fg, Color::Default);
    }

    #[test]
    fn test_cursor_position() {
        let mut t = TestTerminal::new(24, 80);
        t.process(b"\x1b[5;10H");
        assert_eq!(t.cursor.row, 4); // 0-indexed
        assert_eq!(t.cursor.col, 9); // 0-indexed
    }

    #[test]
    fn test_scroll_region_set() {
        let mut t = TestTerminal::new(24, 80);
        t.process(b"\x1b[5;20r");
        assert_eq!(t.scroll_region.top, 4);
        assert_eq!(t.scroll_region.bottom, 19);
        // Cursor should be homed.
        assert_eq!(t.cursor.row, 0);
        assert_eq!(t.cursor.col, 0);
    }

    #[test]
    fn test_reverse_index() {
        let mut t = TestTerminal::new(5, 10);
        t.process(b"\x1b[2;4r"); // Set scroll region lines 2-4.
        t.process(b"\x1b[2;1H"); // Move to top of region.
        t.process(b"\x1bM"); // Reverse index -- should scroll down.
        // Row 1 (top of region) should be blank (new row inserted).
        assert_eq!(t.grid.visible_row(1)[0].ch, ' ');
    }

    #[test]
    fn test_decset_cursor_visibility() {
        let mut t = TestTerminal::new(24, 80);
        assert!(t.cursor.visible);
        t.process(b"\x1b[?25l"); // Hide cursor.
        assert!(!t.cursor.visible);
        t.process(b"\x1b[?25h"); // Show cursor.
        assert!(t.cursor.visible);
    }

    #[test]
    fn test_ris_full_reset() {
        let mut t = TestTerminal::new(24, 80);
        t.process(b"Hello\x1b[31m"); // Write text and set color.
        t.process(b"\x1bc"); // Full reset.
        assert_eq!(t.grid.visible_row(0)[0].ch, ' ');
        assert_eq!(t.cursor.row, 0);
        assert_eq!(t.cursor.col, 0);
        assert_eq!(t.cursor.style.fg, Color::Default);
    }

    #[test]
    fn test_ris_full_reset_clears_dynamic_default_colors() {
        let mut t = TestTerminal::new(24, 80);
        t.process(b"\x1b]10;#ff8000\x07\x1b]11;#120a08\x07");
        assert_eq!(t.default_colors.fg, Some([0xff, 0x80, 0x00]));
        assert_eq!(t.default_colors.bg, Some([0x12, 0x0a, 0x08]));

        t.process(b"\x1bc");

        assert_eq!(t.default_colors, TerminalDefaultColors::default());
    }

    #[test]
    fn test_osc_title() {
        let mut t = TestTerminal::new(24, 80);
        t.process(b"\x1b]2;test title\x07");
        assert_eq!(t.title.as_deref(), Some("test title"));
    }

    #[test]
    fn test_osc_dynamic_default_background_hex() {
        let mut t = TestTerminal::new(24, 80);
        t.process(b"\x1b]11;#120A08\x07");
        assert_eq!(t.default_colors.bg, Some([0x12, 0x0a, 0x08]));
    }

    #[test]
    fn test_osc_dynamic_default_foreground_rgb_and_reset() {
        let mut t = TestTerminal::new(24, 80);
        t.process(b"\x1b]10;rgb:ffff/8000/0000\x07");
        assert_eq!(t.default_colors.fg, Some([255, 128, 0]));
        t.process(b"\x1b]110\x07");
        assert_eq!(t.default_colors.fg, None);
    }

    #[test]
    fn test_insert_mode() {
        let mut t = TestTerminal::new(1, 10);
        t.process(b"ABC");
        t.process(b"\x1b[4h"); // Enable insert mode.
        t.process(b"\x1b[1;2H"); // Move to col 1.
        t.process(b"X");
        assert_eq!(t.grid.visible_row(0)[0].ch, 'A');
        assert_eq!(t.grid.visible_row(0)[1].ch, 'X');
        assert_eq!(t.grid.visible_row(0)[2].ch, 'B');
        assert_eq!(t.grid.visible_row(0)[3].ch, 'C');
    }
}
