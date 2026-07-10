//! CLI output styling — consistent colors and formatting for shux CLI output.
//!
//! Color palette:
//! - Accent (cyan):    brand color, used for shux name and key identifiers
//! - Success (green):  confirmations, creation messages
//! - Warning (yellow): warnings, "not running" messages
//! - Error (red):      errors
//! - Muted (gray):     secondary info (IDs, timestamps, hints)
//! - Bold white:       primary content (session names, versions)

use std::fmt;
use std::io::{self, IsTerminal, Write};

use crossterm::style::{self, Attribute, Color, Stylize};
use unicode_width::UnicodeWidthStr;

// ── Terminal Context ────────────────────────────────────────────

/// Captures terminal capabilities and format preferences for output rendering.
#[allow(dead_code)]
pub struct TerminalContext {
    pub is_tty: bool,
    pub colors: bool,
    pub unicode: bool,
    pub width: u16,
    pub format: OutputFormat,
}

/// Output format (mirrors cli::OutputFormat but available in style module).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Text,
    Json,
    Plain,
}

impl TerminalContext {
    /// Detect terminal capabilities from the current environment.
    pub fn detect(format: OutputFormat) -> Self {
        let is_tty = io::stdout().is_terminal();
        let no_color = std::env::var_os("NO_COLOR").is_some();
        let term = std::env::var("TERM").unwrap_or_default();
        let is_dumb = term == "dumb";

        // Auto-switch to plain when piped or dumb terminal
        let effective_format = if (!is_tty || is_dumb) && format == OutputFormat::Text {
            OutputFormat::Plain
        } else {
            format
        };

        let width = crossterm::terminal::size().map(|(w, _)| w).unwrap_or(80);

        Self {
            is_tty,
            colors: !no_color && is_tty,
            unicode: !is_dumb,
            width,
            format: effective_format,
        }
    }
}

// ── Styled Text Helper ─────────────────────────────────────────

/// Whether to emit ANSI color codes. NO_COLOR wins; CLICOLOR_FORCE
/// (or FORCE_COLOR, the npm convention) forces on even when stdout is
/// piped — useful for capturing the banner into a file or screenshot
/// pipeline; otherwise auto-detects from stdout.
fn colors_enabled() -> bool {
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    if std::env::var_os("CLICOLOR_FORCE").is_some() || std::env::var_os("FORCE_COLOR").is_some() {
        return true;
    }
    io::stdout().is_terminal()
}

/// Styled text helper that respects NO_COLOR and terminal detection.
struct Styled {
    text: String,
    fg: Option<Color>,
    bold: bool,
    dim: bool,
}

impl Styled {
    fn new(text: impl fmt::Display) -> Self {
        Self {
            text: text.to_string(),
            fg: None,
            bold: false,
            dim: false,
        }
    }

    fn fg(mut self, color: Color) -> Self {
        self.fg = Some(color);
        self
    }

    fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    fn dim(mut self) -> Self {
        self.dim = true;
        self
    }
}

impl fmt::Display for Styled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !colors_enabled() {
            return write!(f, "{}", self.text);
        }

        let mut styled = style::style(&self.text);
        if let Some(color) = self.fg {
            styled = styled.with(color);
        }
        if self.bold {
            styled = styled.attribute(Attribute::Bold);
        }
        if self.dim {
            styled = styled.attribute(Attribute::Dim);
        }
        write!(f, "{styled}")
    }
}

/// Styled text with explicit color control (for TerminalContext-aware rendering).
fn styled_if(text: &str, colors: bool, fg: Option<Color>, is_bold: bool, is_dim: bool) -> String {
    if !colors {
        return text.to_string();
    }
    let mut s = style::style(text);
    if let Some(color) = fg {
        s = s.with(color);
    }
    if is_bold {
        s = s.attribute(Attribute::Bold);
    }
    if is_dim {
        s = s.attribute(Attribute::Dim);
    }
    s.to_string()
}

// ── Banner ─────────────────────────────────────────────────────

/// Generate the shux ASCII art banner with a warm terracotta→amber gradient
/// (matches the landing-page logo + accent palette: #c75a2a → #d97c4a → #e69561).
/// Respects NO_COLOR and terminal detection.
pub fn banner() -> String {
    const ART: [&str; 6] = [
        r"      _               ",
        r" ___ | |__  _   ___  __",
        r"/ __|| '_ \| | | \ \/ /",
        r"\__ \| | | | |_| |>  < ",
        r"|___/|_| |_|\__,_/_/\_\",
        "",
    ];

    if !colors_enabled() {
        return ART.join("\n");
    }

    // Warm terracotta → amber gradient using truecolor RGB. Matches the
    // landing-page accent palette (--accent #c75a2a) shading lighter
    // toward the bottom of the wordmark.
    const GRADIENT: [(u8, u8, u8); 5] = [
        (199, 90, 42),   // #c75a2a — accent
        (213, 105, 55),  // warmer
        (224, 124, 75),  // brighter
        (232, 145, 100), // softer
        (240, 168, 128), // softest
    ];

    let mut out = String::with_capacity(384);
    for (line, &(r, g, b)) in ART[..5].iter().zip(&GRADIENT) {
        out.push_str(&format!("\x1b[1;38;2;{r};{g};{b}m{line}\x1b[0m\n"));
    }
    out
}

// ── Public Color Helpers ───────────────────────────────────────

/// Brand accent (cyan) — for "shux" name, key identifiers.
pub fn accent(text: impl fmt::Display) -> impl fmt::Display {
    Styled::new(text).fg(Color::Cyan).bold()
}

/// Success (green) — for creation/operation confirmations.
pub fn success(text: impl fmt::Display) -> impl fmt::Display {
    Styled::new(text).fg(Color::Green)
}

/// Warning (yellow) — for "not running", degraded states.
pub fn warning(text: impl fmt::Display) -> impl fmt::Display {
    Styled::new(text).fg(Color::Yellow)
}

/// Error (red) — for failures.
pub fn error(text: impl fmt::Display) -> impl fmt::Display {
    Styled::new(text).fg(Color::Red).bold()
}

/// Muted (gray/dim) — for IDs, timestamps, secondary info.
pub fn muted(text: impl fmt::Display) -> impl fmt::Display {
    Styled::new(text).dim()
}

/// Bold white — for primary content (session names, versions).
pub fn bold(text: impl fmt::Display) -> impl fmt::Display {
    Styled::new(text).bold()
}

// ── Display Width Helper ──────────────────────────────────────

/// Display width of a string in terminal columns.
/// Uses the `unicode-width` crate for accurate column counting, handling
/// wide characters (CJK), zero-width combiners, and other Unicode properly.
fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Pad a string to `width` display columns (left-aligned).
/// Unlike `format!("{:<width$}")` which pads by char count, this uses
/// display width so wide/zero-width characters are handled correctly.
fn pad_right(s: &str, width: usize) -> String {
    let current = display_width(s);
    if current >= width {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(width - current))
    }
}

/// Pad a string to `width` display columns (right-aligned).
fn pad_left(s: &str, width: usize) -> String {
    let current = display_width(s);
    if current >= width {
        s.to_string()
    } else {
        format!("{}{}", " ".repeat(width - current), s)
    }
}

// ── Short ID Helper ────────────────────────────────────────────

/// Truncate a UUID to its 8-char short prefix (like git short SHA).
pub fn short_id(id: &str) -> &str {
    if id.len() >= 8 { &id[..8] } else { id }
}

// ── Box Renderer ───────────────────────────────────────────────

/// Unicode box-drawing frame renderer with dynamic width.
pub struct BoxRenderer {
    ctx_colors: bool,
    ctx_unicode: bool,
    inner_width: usize,
    title: Option<String>,
    footer: Option<String>,
}

impl BoxRenderer {
    pub fn new(ctx: &TerminalContext, min_width: usize) -> Self {
        Self {
            ctx_colors: ctx.colors,
            ctx_unicode: ctx.unicode,
            inner_width: min_width,
            title: None,
            footer: None,
        }
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn footer(mut self, footer: impl Into<String>) -> Self {
        self.footer = Some(footer.into());
        self
    }

    /// Render the top border: ╭─ Title ──...──╮
    pub fn header(&self) -> String {
        let (tl, h, tr) = if self.ctx_unicode {
            ("╭", "─", "╮")
        } else {
            ("+", "-", "+")
        };

        match &self.title {
            Some(title) => {
                let title_display = if self.ctx_colors {
                    styled_if(title, true, Some(Color::Cyan), true, false)
                } else {
                    title.clone()
                };
                // Title: ╭─ Title ──...──╮
                // Between corners: "─ Title ────╮" should fill inner_width + 2
                let prefix_between_corners = 1 + 1 + display_width(title) + 1; // "─ Title "
                let remaining = if self.inner_width + 2 > prefix_between_corners {
                    self.inner_width + 2 - prefix_between_corners
                } else {
                    1
                };
                let pad = h.repeat(remaining);
                format!("{tl}{h} {title_display} {pad}{tr}",)
            }
            None => {
                let pad = h.repeat(self.inner_width + 2);
                format!("{tl}{pad}{tr}")
            }
        }
    }

    /// Render a content row: │ content...   │
    /// `content` is the pre-formatted, possibly colored string.
    /// `visible_len` is the display width of `content` (without ANSI).
    pub fn row(&self, content: &str, visible_len: usize) -> String {
        let v = if self.ctx_unicode { "│" } else { "|" };
        let padding = self.inner_width.saturating_sub(visible_len);
        format!("{v} {content}{} {v}", " ".repeat(padding))
    }

    /// Render an empty row: │               │
    pub fn empty_row(&self) -> String {
        let v = if self.ctx_unicode { "│" } else { "|" };
        format!("{v} {} {v}", " ".repeat(self.inner_width))
    }

    /// Render the bottom border: ╰──── footer ───╯
    pub fn footer_line(&self) -> String {
        let (bl, h, br) = if self.ctx_unicode {
            ("╰", "─", "╯")
        } else {
            ("+", "-", "+")
        };

        match &self.footer {
            Some(footer) => {
                let footer_display = if self.ctx_colors {
                    styled_if(footer, true, None, false, true)
                } else {
                    footer.clone()
                };
                let suffix_visible_len = 1 + display_width(footer) + 1; // " footer ╯"
                let remaining = if self.inner_width + 2 > suffix_visible_len {
                    self.inner_width + 2 - suffix_visible_len
                } else {
                    1
                };
                let pad = h.repeat(remaining);
                format!("{bl}{pad} {footer_display} {br}")
            }
            None => {
                let pad = h.repeat(self.inner_width + 2);
                format!("{bl}{pad}{br}")
            }
        }
    }
}

// ── Column Layout ──────────────────────────────────────────────

/// Column alignment.
#[derive(Clone, Copy)]
pub enum Align {
    Left,
    Right,
}

/// Column definition for the layout engine.
pub struct Column {
    pub header: String,
    pub align: Align,
    pub min_width: usize,
}

/// A mini column-alignment engine for tabular output.
pub struct ColumnLayout {
    columns: Vec<Column>,
    rows: Vec<Vec<String>>,
    gap: usize,
}

impl ColumnLayout {
    pub fn new(columns: Vec<Column>) -> Self {
        Self {
            columns,
            rows: Vec::new(),
            gap: 3, // spaces between columns
        }
    }

    pub fn add_row(&mut self, cells: Vec<String>) {
        self.rows.push(cells);
    }

    /// Calculate the max display width for each column.
    fn col_widths(&self) -> Vec<usize> {
        self.columns
            .iter()
            .enumerate()
            .map(|(i, col)| {
                let header_width = display_width(&col.header);
                let max_cell = self
                    .rows
                    .iter()
                    .map(|row| row.get(i).map(|c| display_width(c)).unwrap_or(0))
                    .max()
                    .unwrap_or(0);
                header_width.max(max_cell).max(col.min_width)
            })
            .collect()
    }

    /// Render the header line (dim/muted).
    pub fn render_header(&self, colors: bool) -> (String, usize) {
        let widths = self.col_widths();
        let mut parts = Vec::new();
        for (i, col) in self.columns.iter().enumerate() {
            let w = widths[i];
            let cell = match col.align {
                Align::Left => pad_right(&col.header, w),
                Align::Right => pad_left(&col.header, w),
            };
            parts.push(cell);
        }
        let line = parts.join(&" ".repeat(self.gap));
        let visible_len = self.total_width();
        let styled_line = styled_if(&line, colors, None, false, true);
        (styled_line, visible_len)
    }

    /// Render a data row. Returns (colored_string, visible_length).
    /// `color_fn` takes (col_index, cell_text) and returns the styled string.
    pub fn render_row(
        &self,
        row_idx: usize,
        color_fn: &dyn Fn(usize, &str) -> String,
    ) -> (String, usize) {
        let widths = self.col_widths();
        let row = &self.rows[row_idx];
        let mut parts_colored = Vec::new();

        for (i, col) in self.columns.iter().enumerate() {
            let raw = row.get(i).map(|s| s.as_str()).unwrap_or("");
            let w = widths[i];
            let padded = match col.align {
                Align::Left => pad_right(raw, w),
                Align::Right => pad_left(raw, w),
            };

            let colored = color_fn(i, &padded);
            parts_colored.push(colored);
        }

        let gap_str = " ".repeat(self.gap);
        let colored = parts_colored.join(&gap_str);
        (colored, self.total_width())
    }

    /// Total visible width of a rendered row.
    pub fn total_width(&self) -> usize {
        let widths = self.col_widths();
        if widths.is_empty() {
            return 0;
        }
        let cols_width: usize = widths.iter().sum();
        cols_width + (widths.len() - 1) * self.gap
    }
}

// ── Rich List Renderers ────────────────────────────────────────

/// Render a rich session list with box frame, aligned columns, and summary footer.
pub fn render_session_list(ctx: &TerminalContext, sessions: &[SessionInfo]) {
    let mut out = io::stdout().lock();

    match ctx.format {
        OutputFormat::Plain => {
            for s in sessions {
                let _ = writeln!(
                    out,
                    "{}\t{}\t{}\t{}",
                    s.name,
                    s.window_count,
                    s.created,
                    short_id(&s.id),
                );
            }
        }
        OutputFormat::Json => unreachable!("JSON handled before render"),
        OutputFormat::Text => {
            if sessions.is_empty() {
                render_empty_state(
                    &mut out,
                    ctx,
                    "Sessions",
                    "(no sessions)",
                    "Create one: shux session create my-project",
                );
                return;
            }

            // Build column data
            let mut layout = ColumnLayout::new(vec![
                Column {
                    header: String::new(),
                    align: Align::Left,
                    min_width: 1,
                }, // diamond
                Column {
                    header: String::new(),
                    align: Align::Left,
                    min_width: 8,
                }, // name
                Column {
                    header: String::new(),
                    align: Align::Left,
                    min_width: 6,
                }, // windows
                Column {
                    header: String::new(),
                    align: Align::Right,
                    min_width: 5,
                }, // created
                Column {
                    header: String::new(),
                    align: Align::Right,
                    min_width: 8,
                }, // id
            ]);

            let mut total_windows: usize = 0;
            for s in sessions {
                total_windows += s.window_count;
                let diamond = if s.is_active { "\u{25C6}" } else { "\u{25C7}" }; // ◆/◇
                let win_text = format!(
                    "{} window{}",
                    s.window_count,
                    if s.window_count == 1 { "" } else { "s" }
                );
                layout.add_row(vec![
                    diamond.to_string(),
                    s.name.clone(),
                    win_text,
                    s.created.clone(),
                    short_id(&s.id).to_string(),
                ]);
            }

            let content_width = layout.total_width();

            let footer_text = format!(
                "{} session{} \u{00B7} {} window{} total",
                sessions.len(),
                if sessions.len() == 1 { "" } else { "s" },
                total_windows,
                if total_windows == 1 { "" } else { "s" },
            );

            let title_text = "Sessions";
            let box_width = content_width
                .max(display_width(title_text) + 4)
                .max(display_width(&footer_text) + 4);

            let bx = BoxRenderer::new(ctx, box_width)
                .title(title_text)
                .footer(footer_text);

            let _ = writeln!(out, "{}", bx.header());
            let _ = writeln!(out, "{}", bx.empty_row());

            for (i, session) in sessions.iter().enumerate() {
                let is_active = session.is_active;
                let colors = ctx.colors;
                let (colored, visible_len) = layout.render_row(i, &|col_idx, cell| {
                    match col_idx {
                        0 => {
                            // Diamond: ◆ cyan bold if active, ◇ dim if not
                            if is_active {
                                styled_if(cell.trim(), colors, Some(Color::Cyan), true, false)
                                    + &" ".repeat(display_width(cell) - display_width(cell.trim()))
                            } else {
                                styled_if(cell, colors, None, false, true)
                            }
                        }
                        1 => {
                            // Name: bold
                            styled_if(cell, colors, None, true, false)
                        }
                        4 => {
                            // Short ID: muted
                            styled_if(cell, colors, None, false, true)
                        }
                        _ => cell.to_string(),
                    }
                });
                let _ = writeln!(out, "{}", bx.row(&colored, visible_len));
            }

            let _ = writeln!(out, "{}", bx.empty_row());
            let _ = writeln!(out, "{}", bx.footer_line());
        }
    }
}

/// Render a rich window list with box frame, context header, and summary footer.
pub fn render_window_list(ctx: &TerminalContext, session_name: &str, windows: &[WindowInfo]) {
    let mut out = io::stdout().lock();

    match ctx.format {
        OutputFormat::Plain => {
            for w in windows {
                let _ = writeln!(out, "{}\t{}\t{}", w.index, w.title, w.pane_count,);
            }
        }
        OutputFormat::Json => unreachable!("JSON handled before render"),
        OutputFormat::Text => {
            if windows.is_empty() {
                let title = format!("Windows \u{2500}\u{2500} session: {session_name}");
                render_empty_state(&mut out, ctx, &title, "(no windows)", "");
                return;
            }

            let mut layout = ColumnLayout::new(vec![
                Column {
                    header: "#".to_string(),
                    align: Align::Right,
                    min_width: 2,
                },
                Column {
                    header: "NAME".to_string(),
                    align: Align::Left,
                    min_width: 8,
                },
                Column {
                    header: "PANES".to_string(),
                    align: Align::Right,
                    min_width: 5,
                },
                Column {
                    header: String::new(),
                    align: Align::Left,
                    min_width: 0,
                }, // active marker
            ]);

            let mut total_panes: usize = 0;
            for w in windows {
                total_panes += w.pane_count;
                let marker = if w.is_active {
                    "\u{25C0} active".to_string() // ◀ active
                } else {
                    String::new()
                };
                layout.add_row(vec![
                    w.index.to_string(),
                    w.title.clone(),
                    w.pane_count.to_string(),
                    marker,
                ]);
            }

            let content_width = layout.total_width();
            let header_text = format!("Windows \u{2500}\u{2500} session: {session_name}");

            let footer_text = format!(
                "{} window{} \u{00B7} {} pane{} \u{2500}\u{2500} {session_name}",
                windows.len(),
                if windows.len() == 1 { "" } else { "s" },
                total_panes,
                if total_panes == 1 { "" } else { "s" },
            );

            let box_width = content_width
                .max(display_width(&header_text) + 4)
                .max(display_width(&footer_text) + 4);

            let bx = BoxRenderer::new(ctx, box_width)
                .title(header_text)
                .footer(footer_text);

            let _ = writeln!(out, "{}", bx.header());
            let _ = writeln!(out, "{}", bx.empty_row());

            // Column headers
            let (header_colored, header_len) = layout.render_header(ctx.colors);
            let _ = writeln!(out, "{}", bx.row(&header_colored, header_len));

            for (i, window) in windows.iter().enumerate() {
                let is_active = window.is_active;
                let colors = ctx.colors;
                let (colored, visible_len) = layout.render_row(i, &|col_idx, cell| {
                    match col_idx {
                        1 => styled_if(cell, colors, None, true, false), // name bold
                        3 if is_active => styled_if(cell, colors, Some(Color::Cyan), true, false), // active marker
                        _ => cell.to_string(),
                    }
                });
                let _ = writeln!(out, "{}", bx.row(&colored, visible_len));
            }

            let _ = writeln!(out, "{}", bx.empty_row());
            let _ = writeln!(out, "{}", bx.footer_line());
        }
    }
}

/// Render a rich pane list with box frame, hierarchy header, and summary footer.
pub fn render_pane_list(
    ctx: &TerminalContext,
    session_name: &str,
    window_name: &str,
    panes: &[PaneInfo],
) {
    let mut out = io::stdout().lock();

    match ctx.format {
        OutputFormat::Plain => {
            for p in panes {
                let _ = writeln!(out, "{}\t{}\t{}", short_id(&p.id), p.cwd, p.command,);
            }
        }
        OutputFormat::Json => unreachable!("JSON handled before render"),
        OutputFormat::Text => {
            if panes.is_empty() {
                let title = format!(
                    "Panes \u{2500}\u{2500} window: {window_name} \u{2500}\u{2500} session: {session_name}"
                );
                render_empty_state(&mut out, ctx, &title, "(no panes)", "");
                return;
            }

            let mut layout = ColumnLayout::new(vec![
                Column {
                    header: "ID".to_string(),
                    align: Align::Left,
                    min_width: 8,
                },
                Column {
                    header: String::new(),
                    align: Align::Left,
                    min_width: 0,
                }, // focus/zoom marker
            ]);

            for p in panes {
                let marker = if p.is_focused && p.is_zoomed {
                    "\u{25C0} focus [zoomed]".to_string()
                } else if p.is_focused {
                    "\u{25C0} focus".to_string()
                } else {
                    String::new()
                };
                layout.add_row(vec![short_id(&p.id).to_string(), marker]);
            }

            let content_width = layout.total_width();
            let header_text = format!(
                "Panes \u{2500}\u{2500} window: {window_name} \u{2500}\u{2500} session: {session_name}"
            );
            let footer_ctx = format!("{window_name}:{session_name}");

            let footer_text = format!(
                "{} pane{} \u{2500}\u{2500} {footer_ctx}",
                panes.len(),
                if panes.len() == 1 { "" } else { "s" },
            );

            let box_width = content_width
                .max(display_width(&header_text) + 4)
                .max(display_width(&footer_text) + 4);

            let bx = BoxRenderer::new(ctx, box_width)
                .title(header_text)
                .footer(footer_text);

            let _ = writeln!(out, "{}", bx.header());
            let _ = writeln!(out, "{}", bx.empty_row());

            // Column headers
            let (header_colored, header_len) = layout.render_header(ctx.colors);
            let _ = writeln!(out, "{}", bx.row(&header_colored, header_len));

            for (i, pane) in panes.iter().enumerate() {
                let is_focused = pane.is_focused;
                let is_zoomed = pane.is_zoomed;
                let colors = ctx.colors;
                let (colored, visible_len) = layout.render_row(i, &|col_idx, cell| {
                    match col_idx {
                        0 => styled_if(cell, colors, None, false, true), // ID: muted
                        1 if is_focused && is_zoomed => {
                            // Split the marker: "◀ focus" in cyan, "[zoomed]" in yellow
                            let trimmed = cell.trim_end();
                            if let Some(pos) = trimmed.find("[zoomed]") {
                                let focus_part = &trimmed[..pos];
                                let zoom_part = &trimmed[pos..];
                                let trail = &cell[trimmed.len()..]; // trailing spaces
                                format!(
                                    "{}{}{}",
                                    styled_if(focus_part, colors, Some(Color::Cyan), true, false),
                                    styled_if(zoom_part, colors, Some(Color::Yellow), true, false),
                                    trail,
                                )
                            } else {
                                styled_if(cell, colors, Some(Color::Cyan), true, false)
                            }
                        }
                        1 if is_focused => styled_if(cell, colors, Some(Color::Cyan), true, false),
                        _ => cell.to_string(),
                    }
                });
                let _ = writeln!(out, "{}", bx.row(&colored, visible_len));
            }

            let _ = writeln!(out, "{}", bx.empty_row());
            let _ = writeln!(out, "{}", bx.footer_line());
        }
    }
}

/// Render an empty state inside a box frame.
fn render_empty_state(
    out: &mut impl Write,
    ctx: &TerminalContext,
    title: &str,
    message: &str,
    hint: &str,
) {
    let msg_w = display_width(message);
    let hint_w = display_width(hint);
    let title_w = display_width(title);
    let content_len = msg_w.max(hint_w).max(40);
    let inner = content_len.max(title_w + 4);

    let bx = BoxRenderer::new(ctx, inner).title(title.to_string());

    let _ = writeln!(out, "{}", bx.header());
    let _ = writeln!(out, "{}", bx.empty_row());

    let msg_styled = styled_if(message, ctx.colors, None, false, true);
    let msg_padding = inner.saturating_sub(msg_w);
    let v = if ctx.unicode { "│" } else { "|" };
    let _ = writeln!(out, "{v} {msg_styled}{} {v}", " ".repeat(msg_padding));

    let _ = writeln!(out, "{}", bx.empty_row());

    if !hint.is_empty() {
        let hint_styled = styled_if(hint, ctx.colors, None, false, true);
        let hint_padding = inner.saturating_sub(hint_w);
        let _ = writeln!(out, "{v} {hint_styled}{} {v}", " ".repeat(hint_padding));
        let _ = writeln!(out, "{}", bx.empty_row());
    }

    let _ = writeln!(out, "{}", bx.footer_line());
}

// ── Data Structs for List Rendering ────────────────────────────

/// Session info for list rendering.
pub struct SessionInfo {
    pub name: String,
    pub id: String,
    pub window_count: usize,
    pub created: String,
    pub is_active: bool,
}

/// Window info for list rendering.
pub struct WindowInfo {
    pub title: String,
    pub index: usize,
    pub pane_count: usize,
    pub is_active: bool,
}

/// Pane info for list rendering.
pub struct PaneInfo {
    pub id: String,
    pub cwd: String,
    pub command: String,
    pub is_focused: bool,
    pub is_zoomed: bool,
}

// ── Version & Confirmation Printers ────────────────────────────

/// Print the shux banner (used for version output).
pub fn print_version(version: &str, git_sha: Option<&str>, daemon_status: Option<&str>) {
    let mut out = io::stdout().lock();
    let _ = write!(out, "{} ", accent("shux"));
    let _ = write!(out, "{}", bold(version));
    if let Some(sha) = git_sha {
        let _ = write!(out, " {}", muted(format!("({sha})")));
    }
    if let Some(status) = daemon_status {
        let _ = write!(out, " {}", warning(format!("[{status}]")));
    }
    let _ = writeln!(out);
}

/// Print a success confirmation with ✓ prefix and short ID.
pub fn print_success(action: &str, subject: &str, id: Option<&str>) {
    let mut out = io::stdout().lock();
    let _ = write!(out, "{} ", success("\u{2713}")); // ✓
    let _ = write!(out, "{action} {}", bold(subject));
    if let Some(id) = id {
        let _ = write!(out, "  {}", muted(short_id(id)));
    }
    let _ = writeln!(out);
}

/// Print an error with ✗ prefix.
pub fn print_error(msg: &str) {
    let mut err = io::stderr().lock();
    let _ = write!(err, "{} ", error("\u{2717}")); // ✗
    let _ = writeln!(err, "{msg}");
}

// ── Legacy Confirmation Helpers (now using ✓ prefix + short IDs) ──

/// Print a session creation confirmation.
pub fn print_session_created(name: &str, id: &str, ensured: bool) {
    let action = if ensured { "Ensured" } else { "Created" };
    print_success(action, &format!("session '{name}'"), Some(id));
}

/// Print a session kill confirmation.
pub fn print_session_killed(name: &str) {
    print_success("Killed", &format!("session '{name}'"), None);
}

/// Print a session rename confirmation.
pub fn print_session_renamed(old_name: &str, new_name: &str) {
    print_success(
        "Renamed",
        &format!("session '{old_name}' -> '{new_name}'"),
        None,
    );
}

/// Print a window creation confirmation.
pub fn print_window_created(title: &str, index: u64) {
    let mut out = io::stdout().lock();
    let _ = write!(out, "{} ", success("\u{2713}"));
    let _ = write!(out, "Created window '{}' ", bold(title));
    let _ = write!(out, "{}", muted(format!("(index {index})")));
    let _ = writeln!(out);
}

/// Print a window kill confirmation.
pub fn print_window_killed(title: &str) {
    print_success("Killed", &format!("window '{title}'"), None);
}

/// Print a window rename confirmation.
pub fn print_window_renamed(old_name: &str, new_name: &str) {
    print_success(
        "Renamed",
        &format!("window '{old_name}' -> '{new_name}'"),
        None,
    );
}

/// Print a window focus confirmation.
pub fn print_window_focused(title: &str) {
    print_success("Focused", &format!("window '{title}'"), None);
}

/// Print a window reorder confirmation.
pub fn print_window_reordered(title: &str, new_index: usize) {
    let mut out = io::stdout().lock();
    let _ = write!(out, "{} ", success("\u{2713}"));
    let _ = write!(out, "Moved window '{}' to index {}", bold(title), new_index);
    let _ = writeln!(out);
}

/// Print a pane split confirmation.
pub fn print_pane_split(pane_id: &str, direction: &str) {
    let mut out = io::stdout().lock();
    let _ = write!(out, "{} ", success("\u{2713}"));
    let _ = write!(out, "Split pane ({direction}) ");
    let _ = write!(out, "{}", muted(short_id(pane_id)));
    let _ = writeln!(out);
}

/// Print a pane focus confirmation.
pub fn print_pane_focused(pane_id: &str) {
    print_success("Focused", "pane", Some(pane_id));
}

/// Print a pane zoom confirmation.
pub fn print_pane_zoomed(pane_id: &str, is_zoomed: bool) {
    let action = if is_zoomed { "Zoomed" } else { "Unzoomed" };
    print_success(action, "pane", Some(pane_id));
}

/// Print a pane swap confirmation.
pub fn print_pane_swapped(pane_a: &str, pane_b: &str) {
    let mut out = io::stdout().lock();
    let _ = write!(out, "{} ", success("\u{2713}"));
    let _ = write!(
        out,
        "Swapped pane {} <-> {}",
        muted(short_id(pane_a)),
        muted(short_id(pane_b)),
    );
    let _ = writeln!(out);
}

/// Print a pane kill confirmation.
pub fn print_pane_killed(pane_id: &str) {
    print_success("Killed", "pane", Some(pane_id));
}

/// Print a pane resize confirmation.
pub fn print_pane_resized(pane_id: &str) {
    print_success("Resized", "pane", Some(pane_id));
}

/// Print a pane title-set confirmation (PR 4 / task 027).
pub fn print_pane_title_set(pane_id: &str, displayed: &str) {
    let mut out = std::io::stdout();
    let _ = write!(
        out,
        "{} Set title on pane {} → {}",
        success("✓"),
        muted(&short_id(pane_id)),
        bold(displayed),
    );
    let _ = writeln!(out);
}

/// Print a send-keys confirmation.
pub fn print_send_keys(pane_id: &str, bytes_written: u64) {
    println!(
        "{} Sent {} bytes to pane {}",
        success("✓"),
        bold(&bytes_written.to_string()),
        muted(&short_id(pane_id)),
    );
}

/// Print a run-command result.
pub fn print_run_command(result: &serde_json::Value, is_async: bool) {
    if is_async {
        let cmd_id = result
            .get("command_id")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        println!(
            "{} Command started {}",
            success("✓"),
            muted(&short_id(cmd_id)),
        );
    } else {
        let state = result
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let exit_code = result.get("exit_code").and_then(|v| v.as_i64());
        let runtime_ms = result
            .get("runtime_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let stdout = result.get("stdout").and_then(|v| v.as_str()).unwrap_or("");

        let status = match (state, exit_code) {
            ("completed", Some(0)) => format!("{}", success("✓ completed")),
            ("completed", Some(code)) => format!("{}", error(&format!("✗ exit {code}"))),
            ("timed_out", _) => format!("{}", warning("⏱ timed out")),
            ("cancelled", _) => format!("{}", warning("⊘ cancelled")),
            _ => format!("{}", muted(state)),
        };
        println!("{status} {}", muted(&format!("({runtime_ms}ms)")));
        if !stdout.is_empty() {
            print!("{stdout}");
        }
    }
}

/// Print a `pane glance` summary (lens PRD §5/§10). PNG bytes are never
/// printed here — only the file-write confirmation when `--png` was given.
#[allow(clippy::too_many_arguments)]
pub fn print_pane_glance(
    pane_id: &str,
    revision: u64,
    cols: u64,
    rows: u64,
    cursor_row: u64,
    cursor_col: u64,
    cursor_visible: bool,
    alt_screen: bool,
    checkpointed: bool,
    evicted_revision: Option<u64>,
    text: &str,
    png_written: Option<(&std::path::Path, u64)>,
) {
    println!(
        "{} glance {} rev {} {}×{} cursor ({},{}) {} alt_screen {}",
        success("✓"),
        muted(&short_id(pane_id)),
        bold(&revision.to_string()),
        cols,
        rows,
        cursor_row,
        cursor_col,
        if cursor_visible { "visible" } else { "hidden" },
        if alt_screen { "yes" } else { "no" },
    );
    if checkpointed {
        match evicted_revision {
            Some(ev) => println!("  {} checkpointed (evicted revision {ev})", accent("✓")),
            None => println!("  {} checkpointed", accent("✓")),
        }
    }
    if let Some((path, len)) = png_written {
        println!(
            "  {} png → {} ({len} bytes)",
            success("✓"),
            bold(&path.display().to_string()),
        );
    }
    if !text.is_empty() {
        println!();
        print!("{text}");
        if !text.ends_with('\n') {
            println!();
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_styled_plain_text() {
        let styled = Styled::new("hello").fg(Color::Red).bold();
        let _ = styled.to_string();
    }

    #[test]
    fn test_accent_display() {
        let text = accent("shux");
        let _ = text.to_string();
    }

    #[test]
    fn test_muted_display() {
        let text = muted("[abc-123]");
        let _ = text.to_string();
    }

    #[test]
    fn test_banner_contains_shux() {
        let b = banner();
        assert!(b.contains("___"), "banner should contain ASCII art");
    }

    #[test]
    fn test_short_id_truncates() {
        assert_eq!(short_id("bfdb89fb-dbc5-49cc-b1fc-613a0ca00f66"), "bfdb89fb");
        assert_eq!(short_id("abcd1234"), "abcd1234");
        assert_eq!(short_id("abc"), "abc");
    }

    #[test]
    fn test_column_layout_widths() {
        let mut layout = ColumnLayout::new(vec![
            Column {
                header: "#".to_string(),
                align: Align::Right,
                min_width: 2,
            },
            Column {
                header: "NAME".to_string(),
                align: Align::Left,
                min_width: 4,
            },
        ]);
        layout.add_row(vec!["0".to_string(), "editor".to_string()]);
        layout.add_row(vec!["1".to_string(), "my-very-long-name".to_string()]);

        let widths = layout.col_widths();
        assert_eq!(widths[0], 2); // max(1, 1, min=2) = 2
        assert_eq!(widths[1], 17); // max(4, 17, min=4) = 17
    }

    #[test]
    fn test_box_renderer_header() {
        let ctx = TerminalContext {
            is_tty: false,
            colors: false,
            unicode: true,
            width: 80,
            format: OutputFormat::Text,
        };
        let bx = BoxRenderer::new(&ctx, 20).title("Sessions");
        let header = bx.header();
        assert!(header.starts_with("╭─"));
        assert!(header.contains("Sessions"));
        assert!(header.ends_with("╮"));
    }

    #[test]
    fn test_box_renderer_ascii_fallback() {
        let ctx = TerminalContext {
            is_tty: false,
            colors: false,
            unicode: false,
            width: 80,
            format: OutputFormat::Text,
        };
        let bx = BoxRenderer::new(&ctx, 20).title("Sessions");
        let header = bx.header();
        assert!(header.starts_with("+-"));
        assert!(header.contains("Sessions"));
        assert!(header.ends_with("+"));
    }

    #[test]
    fn test_box_renderer_row() {
        let ctx = TerminalContext {
            is_tty: false,
            colors: false,
            unicode: true,
            width: 80,
            format: OutputFormat::Text,
        };
        let bx = BoxRenderer::new(&ctx, 20);
        let row = bx.row("hello", 5);
        assert!(row.starts_with("│"));
        assert!(row.ends_with("│"));
        assert!(row.contains("hello"));
    }

    #[test]
    fn test_terminal_context_detect() {
        // In test environment, stdout is not a terminal
        let ctx = TerminalContext::detect(OutputFormat::Text);
        // When not a TTY, should auto-switch to Plain
        assert_eq!(ctx.format, OutputFormat::Plain);
        assert!(!ctx.is_tty);
    }

    fn text_ctx(colors: bool, unicode: bool) -> TerminalContext {
        TerminalContext {
            is_tty: true,
            colors,
            unicode,
            width: 100,
            format: OutputFormat::Text,
        }
    }

    fn plain_ctx() -> TerminalContext {
        TerminalContext {
            is_tty: false,
            colors: false,
            unicode: false,
            width: 100,
            format: OutputFormat::Plain,
        }
    }

    #[test]
    fn styled_if_applies_all_requested_attributes_when_enabled() {
        let styled = styled_if("active", true, Some(Color::Cyan), true, true);
        assert!(styled.contains("\x1b["));
        assert!(styled.contains("active"));
        assert_eq!(
            styled_if("plain", false, Some(Color::Red), true, true),
            "plain"
        );
    }

    #[test]
    fn display_width_padding_and_empty_columns_are_stable() {
        assert_eq!(display_width("क"), 1);
        assert_eq!(display_width("界"), 2);
        assert_eq!(pad_right("界", 4), "界  ");
        assert_eq!(pad_left("界", 4), "  界");

        let layout = ColumnLayout::new(Vec::new());
        assert_eq!(layout.total_width(), 0);
    }

    #[test]
    fn box_renderer_covers_titleless_footer_and_colored_variants() {
        let ctx = text_ctx(true, true);
        let bx = BoxRenderer::new(&ctx, 12).footer("done");
        assert_eq!(bx.header(), "╭──────────────╮");
        assert!(bx.footer_line().contains("done"));
        assert!(bx.empty_row().starts_with("│ "));

        let ascii = BoxRenderer::new(&text_ctx(false, false), 8).footer("ok");
        assert_eq!(ascii.header(), "+----------+");
        assert!(ascii.footer_line().starts_with("+"));
    }

    #[test]
    fn column_layout_renders_headers_rows_missing_cells_and_color_callbacks() {
        let mut layout = ColumnLayout::new(vec![
            Column {
                header: "NAME".to_string(),
                align: Align::Left,
                min_width: 4,
            },
            Column {
                header: "COUNT".to_string(),
                align: Align::Right,
                min_width: 5,
            },
            Column {
                header: "MISSING".to_string(),
                align: Align::Left,
                min_width: 7,
            },
        ]);
        layout.add_row(vec!["dev".to_string(), "2".to_string()]);

        let (header, header_width) = layout.render_header(true);
        assert!(header.contains("NAME"));
        assert!(header.contains("\x1b["));
        assert_eq!(header_width, layout.total_width());

        let (row, row_width) = layout.render_row(0, &|idx, cell| format!("{idx}:{cell}"));
        assert!(row.contains("0:dev"));
        assert!(row.contains("1:"));
        assert!(row.contains("2:"));
        assert_eq!(row_width, layout.total_width());
    }

    #[test]
    fn empty_state_renderer_handles_hints_ascii_and_unicode() {
        let mut unicode = Vec::new();
        render_empty_state(
            &mut unicode,
            &text_ctx(false, true),
            "Sessions",
            "(no sessions)",
            "Create one",
        );
        let unicode = String::from_utf8(unicode).expect("unicode output");
        assert!(unicode.contains("Sessions"));
        assert!(unicode.contains("Create one"));
        assert!(unicode.contains("│"));

        let mut ascii = Vec::new();
        render_empty_state(
            &mut ascii,
            &text_ctx(false, false),
            "Windows",
            "(no windows)",
            "",
        );
        let ascii = String::from_utf8(ascii).expect("ascii output");
        assert!(ascii.contains("Windows"));
        assert!(ascii.contains("|"));
        assert!(!ascii.contains("Create one"));
    }

    #[test]
    fn rich_list_renderers_cover_plain_empty_and_active_text_paths() {
        let sessions = vec![
            SessionInfo {
                name: "dev".to_string(),
                id: "12345678-aaaa-bbbb-cccc-000000000000".to_string(),
                window_count: 1,
                created: "now".to_string(),
                is_active: true,
            },
            SessionInfo {
                name: "ops".to_string(),
                id: "87654321-aaaa-bbbb-cccc-000000000000".to_string(),
                window_count: 2,
                created: "later".to_string(),
                is_active: false,
            },
        ];
        let windows = vec![
            WindowInfo {
                title: "editor".to_string(),
                index: 1,
                pane_count: 2,
                is_active: true,
            },
            WindowInfo {
                title: "logs".to_string(),
                index: 2,
                pane_count: 1,
                is_active: false,
            },
        ];
        let panes = vec![
            PaneInfo {
                id: "abcdef0123456789".to_string(),
                cwd: "/tmp".to_string(),
                command: "bash".to_string(),
                is_focused: true,
                is_zoomed: true,
            },
            PaneInfo {
                id: "fedcba9876543210".to_string(),
                cwd: "/var/log".to_string(),
                command: "tail -f app.log".to_string(),
                is_focused: false,
                is_zoomed: false,
            },
        ];

        let colored = text_ctx(true, true);
        render_session_list(&colored, &sessions);
        render_window_list(&colored, "dev", &windows);
        render_pane_list(&colored, "dev", "editor", &panes);

        let plain = plain_ctx();
        render_session_list(&plain, &sessions);
        render_window_list(&plain, "dev", &windows);
        render_pane_list(&plain, "dev", "editor", &panes);

        let ascii_text = text_ctx(false, false);
        render_session_list(&ascii_text, &[]);
        render_window_list(&ascii_text, "dev", &[]);
        render_pane_list(&ascii_text, "dev", "editor", &[]);
    }

    #[test]
    fn confirmation_printers_cover_optional_and_status_branches() {
        print_version("0.26.0", Some("abc1234"), Some("daemon offline"));
        print_version("0.26.0", None, None);
        print_success("Created", "session", Some("123456789"));
        print_success("Updated", "config", None);
        print_error("boom");

        print_session_created("dev", "123456789", false);
        print_session_created("dev", "123456789", true);
        print_session_killed("dev");
        print_session_renamed("old", "new");
        print_window_created("editor", 2);
        print_window_killed("editor");
        print_window_renamed("old", "new");
        print_window_focused("editor");
        print_window_reordered("editor", 1);
        print_pane_split("abcdef012345", "vertical");
        print_pane_focused("abcdef012345");
        print_pane_zoomed("abcdef012345", true);
        print_pane_zoomed("abcdef012345", false);
        print_pane_swapped("abcdef012345", "fedcba987654");
        print_pane_killed("abcdef012345");
        print_pane_resized("abcdef012345");
        print_pane_title_set("abcdef012345", "editor");
        print_send_keys("abcdef012345", 42);

        print_run_command(&serde_json::json!({"command_id": "123456789abcdef"}), true);
        print_run_command(
            &serde_json::json!({
                "state": "completed",
                "exit_code": 0,
                "runtime_ms": 12,
                "stdout": "ok\n",
            }),
            false,
        );
        print_run_command(
            &serde_json::json!({
                "state": "completed",
                "exit_code": 2,
                "runtime_ms": 13,
            }),
            false,
        );
        print_run_command(
            &serde_json::json!({"state": "timed_out", "runtime_ms": 14}),
            false,
        );
        print_run_command(
            &serde_json::json!({"state": "cancelled", "runtime_ms": 15}),
            false,
        );
        print_run_command(&serde_json::json!({"state": "weird"}), false);
        print_run_command(&serde_json::json!({}), false);
    }
}
