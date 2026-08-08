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

/// Fit `s` into exactly `width` display columns, truncating with `marker`
/// when it is too long.
///
/// Truncation walks display width, not bytes or `char`s, so a wide (CJK)
/// character is never cut in half. It also refuses to end on a zero-width
/// character: a trailing ZWJ or variation selector would be a dangling half of
/// a grapheme cluster, and issue #104 deliberately keeps those code points in
/// titles, so they reach here.
///
/// **Width is measured on the accumulated string, never summed per character.**
/// `UnicodeWidthStr::width` is a property of the whole string: `☀️`
/// (U+2600 U+FE0F) is 1 if you add up its characters and **2** as a string, and
/// a ZWJ family is 6 added up and **2** as a string. Summing gave back cells
/// whose real width was not the width they were asked for — `pad_right` then
/// added no padding, the row under-reported its own length, and the box printed
/// lines up to twice the terminal's width with a ragged frame. Everything else
/// in this module (`display_width`, `pad_right`, `col_widths`) measures whole
/// strings, so this must too or the two disagree by construction.
fn fit_width(s: &str, width: usize, marker: &str) -> String {
    if display_width(s) <= width {
        return s.to_string();
    }
    let marker_width = display_width(marker);
    if width <= marker_width {
        // Not even room for the marker — take what fits of it and stop.
        return take_width(marker, width);
    }
    let mut out = take_width(s, width - marker_width);
    while out
        .chars()
        .next_back()
        .is_some_and(|c| display_width(&out) == display_width(&out[..out.len() - c.len_utf8()]))
    {
        out.pop();
    }
    out.push_str(marker);
    out
}

/// The longest prefix of `s` that is at most `width` display columns wide,
/// measured the way [`display_width`] measures.
fn take_width(s: &str, width: usize) -> String {
    let mut out = String::new();
    for c in s.chars() {
        let mut candidate = out.clone();
        candidate.push(c);
        if display_width(&candidate) > width {
            break;
        }
        out = candidate;
    }
    out
}

/// A mini column-alignment engine for tabular output.
pub struct ColumnLayout {
    columns: Vec<Column>,
    rows: Vec<Vec<String>>,
    gap: usize,
    /// Total display columns the table may occupy, and which columns give up
    /// width to get there. `None` — the historical behaviour — means the table
    /// is as wide as its widest cell.
    budget: Option<usize>,
    flex: Vec<usize>,
    ellipsis: &'static str,
    /// `col_widths` is asked for by `render_header`, every `render_row` and
    /// `total_width`. It is a pure function of the columns, the rows and the
    /// budget, so caching it is only a cost question — but the three callers
    /// MUST agree, or the row's reported visible length and the box's inner
    /// width drift apart and every border misaligns.
    resolved: std::cell::OnceCell<Vec<usize>>,
}

impl ColumnLayout {
    pub fn new(columns: Vec<Column>) -> Self {
        Self {
            columns,
            rows: Vec::new(),
            gap: GAP,
            budget: None,
            flex: Vec::new(),
            ellipsis: "\u{2026}",
            resolved: std::cell::OnceCell::new(),
        }
    }

    /// Cap the table at `max_total` display columns, shrinking the columns
    /// whose indices are in `flex` (and truncating their cells) until it fits.
    ///
    /// Opt-in per listing: without it nothing about the layout changes, which
    /// is why `session list` and `window list` render byte-identically to
    /// before.
    pub fn budget(mut self, max_total: usize, flex: &[usize], unicode: bool) -> Self {
        self.budget = Some(max_total);
        self.flex = flex.to_vec();
        self.ellipsis = if unicode { "\u{2026}" } else { ".." };
        self.resolved = std::cell::OnceCell::new();
        self
    }

    pub fn add_row(&mut self, cells: Vec<String>) {
        self.rows.push(cells);
        self.resolved = std::cell::OnceCell::new();
    }

    /// Calculate the display width for each column.
    fn col_widths(&self) -> &[usize] {
        self.resolved.get_or_init(|| {
            let mut widths: Vec<usize> = self
                .columns
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
                .collect();

            let Some(budget) = self.budget else {
                return widths;
            };
            if widths.is_empty() {
                return widths;
            }
            let gaps = (widths.len() - 1) * self.gap;
            let flex: Vec<usize> = self
                .flex
                .iter()
                .copied()
                .filter(|i| *i < widths.len())
                .collect();
            if flex.is_empty() {
                return widths;
            }
            // Everything that will not shrink, plus the gaps, is a floor the
            // budget cannot go under. What is left is shared out among the flex
            // columns **max-min fair**: take them in order of what they ask
            // for, smallest first, and give each either its whole ask or an
            // equal share of what is left, whichever is smaller. So a six-cell
            // TITLE is never clipped to make room for a forty-cell COMMAND that
            // is going to be truncated regardless — a proportional split does
            // exactly that, and it is the difference between a readable title
            // and `prin…`. No flex column drops below its declared `min_width`;
            // under that a column is an ellipsis with nothing before it.
            let fixed: usize = widths
                .iter()
                .enumerate()
                .filter(|(i, _)| !flex.contains(i))
                .map(|(_, w)| *w)
                .sum();
            let wanted: usize = flex.iter().map(|i| widths[*i]).sum();
            let available = budget.saturating_sub(fixed + gaps);
            if wanted <= available {
                return widths;
            }
            let mut order = flex.clone();
            order.sort_by_key(|i| widths[*i]);
            let mut remaining = available;
            let mut left = order.len();
            // What the columns after this one still need at their floors. An
            // early column rounding its share UP past this is how a fair split
            // overruns the budget by exactly the amount the last column's floor
            // then reclaims.
            let mut later_floors: usize = order.iter().map(|i| self.columns[*i].min_width).sum();
            for i in order {
                let floor = self.columns[i].min_width;
                later_floors -= floor;
                let share = (remaining / left).min(remaining.saturating_sub(later_floors));
                let give = widths[i].min(share.max(floor));
                widths[i] = give;
                remaining = remaining.saturating_sub(give);
                left -= 1;
            }
            widths
        })
    }

    /// Fit a cell to its column, truncating when the budget shrank it.
    fn fit_cell(&self, col_idx: usize, cell: &str, width: usize) -> String {
        let sized = if self.budget.is_some() && self.flex.contains(&col_idx) {
            fit_width(cell, width, self.ellipsis)
        } else {
            cell.to_string()
        };
        match self.columns[col_idx].align {
            Align::Left => pad_right(&sized, width),
            Align::Right => pad_left(&sized, width),
        }
    }

    /// Render the header line (dim/muted).
    pub fn render_header(&self, colors: bool) -> (String, usize) {
        let widths = self.col_widths();
        let mut parts = Vec::new();
        for (i, col) in self.columns.iter().enumerate() {
            parts.push(self.fit_cell(i, &col.header, widths[i]));
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

        for (i, width) in widths.iter().enumerate() {
            let raw = row.get(i).map(|s| s.as_str()).unwrap_or("");
            let padded = self.fit_cell(i, raw, *width);
            parts_colored.push(color_fn(i, &padded));
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
                if s.scratch {
                    // 5th column only on scratch rows (which only appear
                    // under --include-scratch) — ordinary rows keep the
                    // stable 4-column shape scripts already parse.
                    let _ = writeln!(
                        out,
                        "{}\t{}\t{}\t{}\tscratch",
                        safe_label(&s.name),
                        s.window_count,
                        s.created,
                        short_id(&s.id),
                    );
                } else {
                    let _ = writeln!(
                        out,
                        "{}\t{}\t{}\t{}",
                        safe_label(&s.name),
                        s.window_count,
                        s.created,
                        short_id(&s.id),
                    );
                }
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
                // Visible scratch tag (LENS-R-041 --include-scratch rows).
                let name_cell = if s.scratch {
                    format!("{} [scratch]", safe_label(&s.name))
                } else {
                    safe_label(&s.name)
                };
                layout.add_row(vec![
                    diamond.to_string(),
                    name_cell,
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
    // Egress guard (issue #104) — see `safe_label`.
    let session_name = &safe_label(session_name);

    match ctx.format {
        OutputFormat::Plain => {
            for w in windows {
                // The id column is last so the three columns scripts already
                // parse keep their positions (issue #120 added it: `--window`
                // takes an id, and this was the only listing that printed
                // none).
                let _ = writeln!(
                    out,
                    "{}\t{}\t{}\t{}",
                    w.index,
                    safe_label(&w.title),
                    w.pane_count,
                    short_id(&w.id),
                );
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
                    header: "ID".to_string(),
                    align: Align::Left,
                    min_width: 8,
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
                    safe_label(&w.title),
                    w.pane_count.to_string(),
                    short_id(&w.id).to_string(),
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
    render_pane_list_into(&mut out, ctx, session_name, window_name, panes);
}

/// Which pane field a text-arm column shows. The set varies with the
/// terminal's width, so the row builder cannot assume fixed indices.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PaneField {
    Id,
    Title,
    Cwd,
    Command,
}

/// Spaces `ColumnLayout` puts between columns. Named here because the text arm
/// has to do the same arithmetic to decide which columns fit.
const GAP: usize = 3;

/// Zoom is reported whether or not the pane also has focus.
///
/// `pane.zoom` takes a pane id, so zoomed-without-focus is an ordinary state a
/// caller can reach — and it is the state an operator most needs told about,
/// because a zoomed pane is why the others are not on screen. The marker used
/// to gate `[zoomed]` behind focus, so `--format json` said `is_zoomed: true`
/// and both human formats said nothing at all.
fn pane_marker(is_focused: bool, is_zoomed: bool) -> String {
    match (is_focused, is_zoomed) {
        (true, true) => "\u{25C0} focus [zoomed]".to_string(),
        (true, false) => "\u{25C0} focus".to_string(),
        (false, true) => "[zoomed]".to_string(),
        (false, false) => String::new(),
    }
}

/// The body of [`render_pane_list`], against any sink, so the output can be
/// asserted on rather than only smoke-tested.
fn render_pane_list_into(
    out: &mut impl Write,
    ctx: &TerminalContext,
    session_name: &str,
    window_name: &str,
    panes: &[PaneInfo],
) {
    // Egress guard (issue #104) — see `safe_label`.
    let session_name = &safe_label(session_name);
    let window_name = &safe_label(window_name);

    match ctx.format {
        OutputFormat::Plain => {
            for p in panes {
                // `cwd`, `command` and `title` are caller-supplied and are NOT
                // sanitized on the way in — a path or an argv is
                // legitimately arbitrary text, so the guard has to be here
                // (issue #104).
                //
                // Quoting runs before the guard, and the order does not matter:
                // every character `safe_label` rewrites is a control or
                // separator character, none of which is on the quoting
                // allowlist, so an argument the guard would touch is quoted
                // under either order. Checked rather than assumed — 200,000
                // random argvs over a hostile alphabet, zero differences. This
                // order is simply the one that walks the line once.
                //
                // `title` is appended LAST so the three fields scripts already
                // parse keep their positions — the same rule issue #120 followed
                // when `window list` grew an id column.
                let _ = writeln!(
                    out,
                    "{}\t{}\t{}\t{}",
                    short_id(&p.id),
                    safe_label(&p.cwd),
                    safe_label(&render_argv(&p.command)),
                    safe_label(&p.title),
                );
            }
        }
        OutputFormat::Json => unreachable!("JSON handled before render"),
        OutputFormat::Text => {
            if panes.is_empty() {
                let title = format!(
                    "Panes \u{2500}\u{2500} window: {window_name} \u{2500}\u{2500} session: {session_name}"
                );
                render_empty_state(out, ctx, &title, "(no panes)", "");
                return;
            }

            // Task 060 §C specified ID / CWD / CMD here and only ID ever
            // shipped; issue #135 adds the title the border draws.
            //
            // Which of them appear depends on the terminal. A column that
            // cannot reach a width where it says anything is dropped, not
            // rendered as a bare ellipsis — task 060's own "narrow terminal →
            // shrink the box" fallback, which `TerminalContext::width` was
            // captured for and which nothing ever implemented. Priority order
            // is ID (the handle you act with), then TITLE (the handle you
            // recognise), then COMMAND (the subject of issue #135), then CWD.
            let budget = inner_budget(ctx);
            let marker_width = panes
                .iter()
                .map(|p| display_width(&pane_marker(p.is_focused, p.is_zoomed)))
                .max()
                .unwrap_or(0);
            let mut columns = vec![Column {
                header: "ID".to_string(),
                align: Align::Left,
                min_width: 8,
            }];
            let mut fields = vec![PaneField::Id];
            // ID + the marker and their gap are the floor; each further column
            // has to fit beside them.
            let mut committed = 8 + GAP + marker_width;
            for (field, header, min_width) in [
                (PaneField::Title, "TITLE", 5),
                (PaneField::Command, "COMMAND", 7),
                (PaneField::Cwd, "CWD", 3),
            ] {
                if committed + GAP + min_width > budget {
                    continue;
                }
                committed += GAP + min_width;
                columns.push(Column {
                    header: header.to_string(),
                    align: Align::Left,
                    min_width,
                });
                fields.push(field);
            }
            // CWD is appended last above but reads better between TITLE and
            // COMMAND, which is also the order task 060 drew.
            if let (Some(cwd_at), Some(cmd_at)) = (
                fields.iter().position(|f| *f == PaneField::Cwd),
                fields.iter().position(|f| *f == PaneField::Command),
            ) && cwd_at > cmd_at
            {
                columns.swap(cwd_at, cmd_at);
                fields.swap(cwd_at, cmd_at);
            }
            let marker_col = columns.len();
            columns.push(Column {
                header: String::new(),
                align: Align::Left,
                min_width: 0,
            });
            // Everything but the id and the marker carries arbitrary user text
            // and therefore flexes.
            let flex: Vec<usize> = (1..marker_col).collect();
            let mut layout = ColumnLayout::new(columns).budget(budget, &flex, ctx.unicode);

            for p in panes {
                let mut row: Vec<String> = fields
                    .iter()
                    .map(|f| match f {
                        PaneField::Id => short_id(&p.id).to_string(),
                        PaneField::Title => safe_label(&p.title),
                        PaneField::Cwd => safe_label(&p.cwd),
                        PaneField::Command => safe_label(&render_argv(&p.command)),
                    })
                    .collect();
                row.push(pane_marker(p.is_focused, p.is_zoomed));
                layout.add_row(row);
            }

            let content_width = layout.total_width();
            // The frame is not bounded by its columns alone: a long window or
            // session name overflows through the header, and the footer quotes
            // both again. So the box takes its width from the table, widening
            // for the header or footer only as far as the budget allows, and
            // then those two are trimmed to the width that was settled on.
            //
            // The trims are NOT the same number. `BoxRenderer` draws the header
            // as `╭─ {title} …╮` and the footer as `╰… {footer} ╯`, which cost
            // one column apart — and both fall back to a one-glyph pad when
            // they do not fit, quietly making the line a column wider than
            // every other. That is what an off-by-one here looks like: a frame
            // with one longer edge.
            let ellipsis = if ctx.unicode { "\u{2026}" } else { ".." };
            let header_raw = format!(
                "Panes \u{2500}\u{2500} window: {window_name} \u{2500}\u{2500} session: {session_name}"
            );
            let footer_raw = format!(
                "{} pane{} \u{2500}\u{2500} {window_name}:{session_name}",
                panes.len(),
                if panes.len() == 1 { "" } else { "s" },
            );
            let box_width = content_width
                .max((display_width(&header_raw) + 4).min(budget))
                .max((display_width(&footer_raw) + 4).min(budget));
            let header_text = fit_width(&header_raw, box_width.saturating_sub(2), ellipsis);
            let footer_text = fit_width(&footer_raw, box_width.saturating_sub(1), ellipsis);

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
                        c if c == marker_col && is_focused && is_zoomed => {
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
                        c if c == marker_col && is_focused => {
                            styled_if(cell, colors, Some(Color::Cyan), true, false)
                        }
                        // Zoomed without focus — same yellow the zoom half of
                        // the combined marker carries.
                        c if c == marker_col && is_zoomed => {
                            styled_if(cell, colors, Some(Color::Yellow), true, false)
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

/// Display columns a box's *inner* area may occupy on this terminal.
///
/// A rendered row is `│ ` + inner + ` │`, so the frame costs four columns.
/// `TerminalContext::width` has been captured since task 060 and read by
/// nothing; this is its first consumer, and task 060's own "narrow terminal →
/// shrink the box" fallback.
/// The narrowest terminal whose `pane list` box can be honoured.
///
/// Below it the frame overflows — as it always has — rather than printing a row
/// that names no pane: the id is what every other verb takes as an argument,
/// and the focus marker is the only thing that says which pane is current, so
/// neither is droppable. Everything else is.
///
/// DERIVED, not written down. The first cut hardcoded 24 from "an id and a
/// `◀ focus` marker with a gap between them are 18 columns", which forgot that
/// a *zoomed* pane's marker is `◀ focus [zoomed]` — 16 columns, not 7 — so the
/// stated guarantee was wrong by seven for every zoomed pane. A constant that
/// restates an arithmetic the code does elsewhere is a constant that will drift
/// again.
#[cfg(test)]
fn min_boxable_width() -> usize {
    // │ + space + ID + gap + widest marker + space + │
    2 + 8 + GAP + display_width(&pane_marker(true, true)) + 2
}

fn inner_budget(ctx: &TerminalContext) -> usize {
    (ctx.width as usize).saturating_sub(4)
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
    /// Lens scratch session (only ever true under `--include-scratch`,
    /// LENS-R-041). Text mode renders a visible `[scratch]` tag; plain
    /// mode appends a `scratch` column so scripts can tell them apart.
    pub scratch: bool,
}

/// Window info for list rendering.
pub struct WindowInfo {
    pub id: String,
    pub title: String,
    pub index: usize,
    pub pane_count: usize,
    pub is_active: bool,
}

/// Pane info for list rendering.
pub struct PaneInfo {
    pub id: String,
    /// The pane's displayed title — the text drawn in its border and the thing
    /// an operator identifies a pane by. It reached `--format json` and neither
    /// human format (issue #135).
    pub title: String,
    pub cwd: String,
    /// The argv, NOT a pre-joined string. Joining it in the caller is how the
    /// text and plain arms would be free to render the same pane differently;
    /// [`render_pane_list`] joins it once, for both.
    pub command: Vec<String>,
    pub is_focused: bool,
    pub is_zoomed: bool,
}

/// Render a pane's argv the way a shell would have to be given it.
///
/// A bare `join(" ")` made `["sh", "-c", "printf 'hi'; sleep 1"]` and a
/// five-element argv print identically, which is exactly the case that matters
/// since #125 made shell-wrapped argv the normal shape of a `--cmd` pane
/// (issue #135). This is the same function that builds the line `pane.run`
/// injects, so there is one quoting dialect rather than two free to disagree.
///
/// **What this guarantees is argument BOUNDARIES, not a byte-exact round trip.**
/// The caller runs the output through [`safe_label`] afterwards, and it must:
/// an argv is arbitrary text and the plain arm is tab-separated, so a raw TAB
/// would forge a column and a raw ESC would reach the operator's terminal. The
/// guard rewrites those bytes into visible `\u{9}`-style escapes *inside* the
/// quotes, so feeding the printed line back to a shell yields the escape text,
/// not the original byte. Measured: boundaries survived 2,637 out of 2,637
/// end-to-end cases; content did not, in 73% of cases containing a control
/// character. `--format json` remains the byte-exact contract, and says so.
fn render_argv(argv: &[String]) -> String {
    shux_pty::shell_escape_args(argv)
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
    // Guard here, not only in the callers: this is the funnel every
    // confirmation goes through, so the invariant holds for future callers
    // too. `safe_label` is idempotent, so the wrappers that already escape
    // their name or title are unaffected (issue #104).
    let _ = write!(out, "{action} {}", bold(safe_label(subject)));
    if let Some(id) = id {
        let _ = write!(out, "  {}", muted(short_id(id)));
    }
    let _ = writeln!(out);
}

/// Print an error with ✗ prefix.
pub fn print_error(msg: &str) {
    let mut err = io::stderr().lock();
    let _ = write!(err, "{} ", error("\u{2717}")); // ✗
    let _ = writeln!(err, "{}", safe_diagnostic(msg));
}

// ── Egress Guard (issue #104) ──────────────────────────────────

/// Render an untrusted label — an entity name, a title, an error message
/// quoting one — so it cannot carry an active control sequence into the
/// operator's terminal.
///
/// The daemon sanitizes every title it *stores*
/// ([`shux_core::model::sanitize_title`]), which handles the reported
/// vector at its source. This is the second, independent layer, and it
/// covers the case ingress sanitizing structurally cannot: input the
/// daemon **rejected** is echoed back in an error message without ever
/// having met a sanitizer. Applied inside every `print_*` helper that
/// interpolates a name or a title.
///
/// Hostile characters become their visible `\u{...}` form, so the operator
/// still sees what was in the template. The escaped classes match
/// `sanitize_title`'s: C0/DEL/C1 controls, `U+2028`/`U+2029` separators,
/// and the bidi override/isolate formatting characters. Everything else —
/// including quotes, backslashes and every non-Latin script — is passed
/// through byte for byte, so wiring this in cannot change normal output.
/// True for characters that must never reach the terminal as themselves.
/// Mirrors `shux_core::model::sanitize_title`'s rule.
fn is_hostile_out(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{2028}' | '\u{2029}')
        || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        || matches!(c, '\u{200e}' | '\u{200f}' | '\u{061c}')
}

/// Like [`safe_label`], but for multi-line diagnostic text: `\n` and `\t`
/// are structure here, not payload, so they survive.
///
/// A TOML parse error is three lines of source excerpt with a caret, and
/// it quotes the offending line **verbatim** — including any escape
/// sequence the attacker put there. That excerpt is printed before the
/// daemon is ever contacted, so no amount of ingress sanitizing reaches
/// it; this is where it gets neutralized (issue #104).
pub fn safe_diagnostic(raw: &str) -> String {
    if !raw
        .chars()
        .any(|c| is_hostile_out(c) && c != '\n' && c != '\t')
    {
        return raw.to_string();
    }
    raw.chars()
        .map(|c| {
            if c == '\n' || c == '\t' {
                c.to_string()
            } else if is_hostile_out(c) {
                format!("\\u{{{:x}}}", c as u32)
            } else {
                c.to_string()
            }
        })
        .collect()
}

/// Re-escape a **serialized JSON** document so it cannot carry an active
/// control sequence.
///
/// `serde_json` escapes C0 and DEL **inside strings**, but not
/// `U+0080`–`U+009F`, `U+2028`/`U+2029` or the bidi formatting characters
/// — a terminal in 8-bit mode reads `U+009B` as CSI. Rewriting those to
/// their `\uXXXX` form keeps the document valid JSON with identical
/// semantics for any parser, while making it inert for a human piping it
/// to a terminal.
///
/// The pretty-printer's own newlines, carriage returns and tabs are
/// **structure**, not payload, and are left alone: any C0 that came from
/// the data is already `\n`-style escaped by `serde_json`, so a raw one
/// in the serialized document can only be layout. Escaping it would emit
/// a `\u000a` outside a string literal and produce invalid JSON.
pub fn json_safe(serialized: &str) -> String {
    fn hostile_in_json(c: char) -> bool {
        !matches!(c, '\n' | '\r' | '\t') && is_hostile_out(c)
    }
    if !serialized.chars().any(hostile_in_json) {
        return serialized.to_string();
    }
    serialized
        .chars()
        .map(|c| {
            if hostile_in_json(c) {
                format!("\\u{:04x}", c as u32)
            } else {
                c.to_string()
            }
        })
        .collect()
}

pub fn safe_label(raw: &str) -> String {
    if !raw.chars().any(is_hostile_out) {
        return raw.to_string();
    }
    raw.chars()
        .map(|c| {
            if is_hostile_out(c) {
                format!("\\u{{{:x}}}", c as u32)
            } else {
                c.to_string()
            }
        })
        .collect()
}

// ── Legacy Confirmation Helpers (now using ✓ prefix + short IDs) ──

/// Print a session creation confirmation.
pub fn print_session_created(name: &str, id: &str, ensured: bool) {
    let action = if ensured { "Ensured" } else { "Created" };
    let name = safe_label(name);
    print_success(action, &format!("session '{name}'"), Some(id));
}

/// Print a session kill confirmation.
pub fn print_session_killed(name: &str) {
    let name = safe_label(name);
    print_success("Killed", &format!("session '{name}'"), None);
}

/// Print a session rename confirmation.
pub fn print_session_renamed(old_name: &str, new_name: &str) {
    let (old_name, new_name) = (safe_label(old_name), safe_label(new_name));
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
    let _ = write!(out, "Created window '{}' ", bold(safe_label(title)));
    let _ = write!(out, "{}", muted(format!("(index {index})")));
    let _ = writeln!(out);
}

/// Print a window kill confirmation.
pub fn print_window_killed(title: &str) {
    let title = safe_label(title);
    print_success("Killed", &format!("window '{title}'"), None);
}

/// Print a window rename confirmation.
pub fn print_window_renamed(old_name: &str, new_name: &str) {
    let (old_name, new_name) = (safe_label(old_name), safe_label(new_name));
    print_success(
        "Renamed",
        &format!("window '{old_name}' -> '{new_name}'"),
        None,
    );
}

/// Print a window focus confirmation.
pub fn print_window_focused(title: &str) {
    let title = safe_label(title);
    print_success("Focused", &format!("window '{title}'"), None);
}

/// Print a window reorder confirmation.
pub fn print_window_reordered(title: &str, new_index: usize) {
    let mut out = io::stdout().lock();
    let _ = write!(out, "{} ", success("\u{2713}"));
    let _ = write!(
        out,
        "Moved window '{}' to index {}",
        bold(safe_label(title)),
        new_index
    );
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
        bold(safe_label(displayed)),
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

/// Print a `pane wait-settled` summary (lens PRD §6/§10). Settled prints a
/// green ✓; a timeout prints a yellow degraded marker (the CLI still exits 1).
pub fn print_pane_wait_settled(pane_id: &str, settled: bool, revision: u64, waited_ms: u64) {
    if settled {
        println!(
            "{} settled {} rev {} after {}ms",
            success("✓"),
            muted(&short_id(pane_id)),
            bold(&revision.to_string()),
            waited_ms,
        );
    } else {
        println!(
            "{} not settled {} rev {} after {}ms (timeout)",
            warning("✗"),
            muted(&short_id(pane_id)),
            bold(&revision.to_string()),
            waited_ms,
        );
    }
}

/// Print a `pane checkpoint` summary (lens PRD §7/§10): the keyed revision
/// and, when a 5th checkpoint evicted the FIFO-oldest, the evicted revision.
pub fn print_pane_checkpoint(pane_id: &str, revision: u64, evicted: Option<u64>) {
    println!(
        "{} checkpoint {} rev {}",
        success("✓"),
        muted(&short_id(pane_id)),
        bold(&revision.to_string()),
    );
    if let Some(ev) = evicted {
        println!("  {} evicted oldest checkpoint (revision {ev})", muted("·"));
    }
}

/// Print a `lens run` summary (lens PRD §8/§10): the scratch session/pane
/// ids, the pane's revision right after spawn, and — only for `--wait` —
/// the child's exit code.
pub fn print_lens_run(session_id: &str, pane_id: &str, revision: u64, exit_code: Option<i64>) {
    // Label both ids. This line is step ONE of the documented loop, and two
    // bare hex tokens leave the reader unable to tell which is which without
    // re-running under --format json (issue #120 dogfood).
    println!(
        "{} lens run  session {}  pane {}  rev {}",
        success("✓"),
        muted(&short_id(session_id)),
        muted(&short_id(pane_id)),
        bold(&revision.to_string()),
    );
    if let Some(code) = exit_code {
        println!("  {} exit code {}", muted("·"), bold(&code.to_string()));
    }
}

/// Print a `pane diff` summary (lens PRD §7/§10): the revision span and the
/// structured delta. Diff is data, not a verdict, so this always reads as a
/// neutral ✓ (the CLI exits 0 regardless of delta size).
#[allow(clippy::too_many_arguments)]
pub fn print_pane_diff(
    pane_id: &str,
    from_revision: u64,
    to_revision: u64,
    cells_changed: u64,
    regions: usize,
    regions_truncated: bool,
    cursor_moved: bool,
    heat_written: Option<(&std::path::Path, u64)>,
) {
    println!(
        "{} diff {} rev {}→{} {} cells changed",
        success("✓"),
        muted(&short_id(pane_id)),
        bold(&from_revision.to_string()),
        bold(&to_revision.to_string()),
        bold(&cells_changed.to_string()),
    );
    if regions_truncated {
        println!(
            "  {} regions truncated (>256 spans); see bounding_box",
            warning("·"),
        );
    } else {
        println!("  {} {regions} region span(s)", muted("·"));
    }
    if cursor_moved {
        println!("  {} cursor moved", muted("·"));
    }
    if let Some((path, len)) = heat_written {
        println!(
            "  {} heat png → {} ({len} bytes)",
            success("✓"),
            bold(&path.display().to_string()),
        );
    }
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── issue #104: egress guard ────────────────────────────────

    /// `safe_label` is the CLI's last line of defence. Ingress sanitising
    /// covers everything the daemon stores; this covers everything the CLI
    /// prints, including values the daemon *rejected* and echoed back in an
    /// error, which by definition never met a sanitiser.
    #[test]
    fn test_safe_label_escapes_control_bytes() {
        assert_eq!(
            safe_label("\u{1b}]0;PWNED\u{7}deploy"),
            "\\u{1b}]0;PWNED\\u{7}deploy"
        );
        for ch in [
            '\u{0}', '\u{7}', '\u{8}', '\u{a}', '\u{d}', '\u{1b}', '\u{7f}',
        ] {
            let out = safe_label(&format!("a{ch}b"));
            assert!(
                !out.chars().any(|c| c.is_control()),
                "U+{:04X} survived: {out:?}",
                ch as u32
            );
        }
    }

    /// C1 (0x80..=0x9F) is CSI/OSC with no ESC in an 8-bit terminal.
    #[test]
    fn test_safe_label_escapes_c1_and_separators_and_bidi() {
        for ch in [
            '\u{80}', '\u{9b}', '\u{9d}', '\u{9f}', '\u{2028}', '\u{2029}', '\u{202a}', '\u{202e}',
            '\u{2066}', '\u{2069}',
        ] {
            let out = safe_label(&format!("a{ch}b"));
            assert_eq!(
                out,
                format!("a\\u{{{:x}}}b", ch as u32),
                "U+{:04X} not escaped",
                ch as u32
            );
        }
    }

    /// Byte-identical for anything an operator would legitimately type, so
    /// wiring it into every `print_*` helper cannot change normal output.
    #[test]
    fn test_safe_label_is_identity_for_clean_input() {
        for s in [
            "deploy",
            "agent-1 · build",
            "日本語 セッション",
            "مرحبا",
            "build ✓ 🚀",
            "",
            "a\"b\\c",
        ] {
            assert_eq!(safe_label(s), s, "mutated clean input: {s:?}");
        }
    }

    /// A sanitised title is already inert, so the two layers compose without
    /// double-escaping.
    /// A TOML diagnostic is three lines of source excerpt with a caret.
    /// Escaping its newlines would turn it into an unreadable ribbon, so
    /// `\n` and `\t` are structure here and survive.
    #[test]
    fn test_safe_diagnostic_preserves_layout_but_neutralises_payload() {
        let diag = "TOML parse error at line 3\n  |\n3 | title = \"\u{1b}]0;PWNED\u{7}\"\n  |\t^";
        let out = safe_diagnostic(diag);
        assert_eq!(out.matches('\n').count(), 3, "layout lost: {out:?}");
        assert!(out.contains('\t'), "tab lost: {out:?}");
        assert!(out.contains("\\u{1b}") && out.contains("\\u{7}"), "{out:?}");
        assert!(
            !out.chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t'),
            "{out:?}"
        );
    }

    #[test]
    fn test_safe_diagnostic_is_identity_for_clean_text() {
        for s in ["plain", "two\nlines", "tab\there", ""] {
            assert_eq!(safe_diagnostic(s), s);
        }
    }

    /// `serde_json` escapes C0 inside strings but not C1, the separators
    /// or the bidi class — a terminal in 8-bit mode reads U+009B as CSI.
    #[test]
    fn test_json_safe_escapes_what_serde_json_leaves_raw() {
        let doc = serde_json::to_string_pretty(&serde_json::json!({
            "cwd": "/tmp/d\u{9b}31m\u{202e}x",
            "title": "a\u{2028}b",
        }))
        .unwrap();
        let out = json_safe(&doc);
        assert!(out.contains("\\u009b"), "{out}");
        assert!(out.contains("\\u202e"), "{out}");
        assert!(out.contains("\\u2028"), "{out}");
        assert!(
            !out.chars()
                .any(|c| matches!(c, '\u{80}'..='\u{9f}' | '\u{2028}' | '\u{2029}')),
            "{out}"
        );
    }

    /// The pretty-printer's own newlines are STRUCTURE. Escaping them
    /// emits `\u000a` outside a string literal and produces invalid JSON.
    #[test]
    fn test_json_safe_keeps_the_document_valid_and_pretty() {
        let doc = serde_json::to_string_pretty(&serde_json::json!({
            "cwd": "/tmp/\u{9b}evil",
            "nested": { "k": [1, 2] },
        }))
        .unwrap();
        let out = json_safe(&doc);
        assert!(out.contains('\n'), "pretty layout lost");
        let back: serde_json::Value =
            serde_json::from_str(&out).expect("json_safe must emit valid JSON");
        // Semantics are preserved exactly — an escape is the same value.
        assert_eq!(back["cwd"], "/tmp/\u{9b}evil");
        assert_eq!(back["nested"]["k"][1], 2);
    }

    #[test]
    fn test_json_safe_is_identity_for_clean_documents() {
        let doc = serde_json::to_string_pretty(&serde_json::json!({"a": "plain", "b": 1})).unwrap();
        assert_eq!(json_safe(&doc), doc);
    }

    /// The egress guard is applied at the funnel as well as at the
    /// wrappers, so it has to be idempotent or the double application
    /// would mangle an already-escaped payload.
    #[test]
    fn test_safe_label_is_idempotent() {
        for s in [
            "\u{1b}]0;PWNED\u{7}deploy",
            "plain",
            "\u{202e}spoof",
            "a\u{9b}b",
            "",
        ] {
            let once = safe_label(s);
            assert_eq!(safe_label(&once), once, "not a fixed point: {s:?}");
        }
    }

    #[test]
    fn test_safe_label_of_sanitized_title_is_unchanged() {
        let sanitized = shux_core::model::sanitize_title("\u{1b}]0;PWNED\u{7}deploy");
        assert_eq!(safe_label(&sanitized), sanitized);
    }

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
                scratch: false,
            },
            SessionInfo {
                name: "ops".to_string(),
                id: "87654321-aaaa-bbbb-cccc-000000000000".to_string(),
                window_count: 2,
                created: "later".to_string(),
                is_active: false,
                scratch: true,
            },
        ];
        let windows = vec![
            WindowInfo {
                id: "11111111-2222-3333-4444-555555555555".to_string(),
                title: "editor".to_string(),
                index: 1,
                pane_count: 2,
                is_active: true,
            },
            WindowInfo {
                id: "66666666-7777-8888-9999-aaaaaaaaaaaa".to_string(),
                title: "logs".to_string(),
                index: 2,
                pane_count: 1,
                is_active: false,
            },
        ];
        let panes = vec![
            PaneInfo {
                id: "abcdef0123456789".to_string(),
                title: "shell".to_string(),
                cwd: "/tmp".to_string(),
                command: vec!["bash".to_string()],
                is_focused: true,
                is_zoomed: true,
            },
            PaneInfo {
                id: "fedcba9876543210".to_string(),
                title: "tail".to_string(),
                cwd: "/var/log".to_string(),
                command: ["tail", "-f", "app.log"].map(String::from).to_vec(),
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

    // ── issue #135: the human pane list named no pane and lost argv shape ──

    fn pane_ctx(format: OutputFormat, width: u16, unicode: bool) -> TerminalContext {
        TerminalContext {
            is_tty: format == OutputFormat::Text,
            colors: false,
            unicode,
            width,
            format,
        }
    }

    fn pane(id: &str, title: &str, cwd: &str, argv: &[&str], focused: bool) -> PaneInfo {
        PaneInfo {
            id: id.to_string(),
            title: title.to_string(),
            cwd: cwd.to_string(),
            command: argv.iter().map(|s| s.to_string()).collect(),
            is_focused: focused,
            is_zoomed: false,
        }
    }

    fn zoomed_pane(id: &str, title: &str, cwd: &str, argv: &[&str]) -> PaneInfo {
        PaneInfo {
            is_zoomed: true,
            ..pane(id, title, cwd, argv, true)
        }
    }

    fn render_panes(
        ctx: &TerminalContext,
        session: &str,
        window: &str,
        panes: &[PaneInfo],
    ) -> String {
        let mut buf: Vec<u8> = Vec::new();
        render_pane_list_into(&mut buf, ctx, session, window, panes);
        String::from_utf8(buf).expect("utf8")
    }

    /// The plain arm gained a title column, and it goes LAST so the three
    /// fields scripts already parse keep their positions (the rule issue #120
    /// followed for `window list`).
    #[test]
    fn the_plain_pane_list_carries_the_title_in_a_fourth_column() {
        let out = render_panes(
            &pane_ctx(OutputFormat::Plain, 100, false),
            "dev",
            "editor",
            &[pane(
                "abcdef0123456789",
                "nvim",
                "/home/u/p",
                &["nvim", "main.rs"],
                true,
            )],
        );
        assert_eq!(out, "abcdef01\t/home/u/p\tnvim main.rs\tnvim\n");
        let fields: Vec<&str> = out.trim_end().split('\t').collect();
        assert_eq!(fields.len(), 4, "field count is the script-facing contract");
        assert_eq!(fields[0], "abcdef01");
        assert_eq!(fields[1], "/home/u/p");
        assert_eq!(fields[3], "nvim");
    }

    /// The issue itself: a quoted argument and several arguments rendered
    /// identically, so the output could not say which one it was.
    #[test]
    fn one_argument_with_a_space_is_distinguishable_from_two() {
        let ctx = pane_ctx(OutputFormat::Plain, 100, false);
        let one = render_panes(
            &ctx,
            "d",
            "w",
            &[pane("aaaaaaaa11", "sh", "/", &["sh", "-c", "a b"], false)],
        );
        let two = render_panes(
            &ctx,
            "d",
            "w",
            &[pane(
                "aaaaaaaa11",
                "sh",
                "/",
                &["sh", "-c", "a", "b"],
                false,
            )],
        );
        assert_ne!(one, two, "argv shape is not recoverable from the output");
        assert!(one.contains("sh -c 'a b'"), "{one}");
        assert!(two.contains("sh -c a b"), "{two}");
    }

    /// An empty argument used to disappear entirely, so `["sh","-c",""]`
    /// printed as a two-element argv.
    #[test]
    fn an_empty_argument_is_visible_in_the_pane_list() {
        let out = render_panes(
            &pane_ctx(OutputFormat::Plain, 100, false),
            "d",
            "w",
            &[pane("aaaaaaaa11", "sh", "/", &["sh", "-c", ""], false)],
        );
        assert!(out.contains("sh -c ''"), "{out}");
    }

    /// The text arm shipped as an ID column and a focus marker — no title, no
    /// command, no cwd, though task 060 §C specified CWD and CMD and the CLI
    /// had been computing both and throwing them away.
    #[test]
    fn the_text_pane_list_names_every_pane_and_says_what_it_runs() {
        let out = render_panes(
            &pane_ctx(OutputFormat::Text, 100, true),
            "dev",
            "editor",
            &[
                pane(
                    "aaaaaaaa11",
                    "nvim",
                    "/home/u/p",
                    &["nvim", "main.rs"],
                    true,
                ),
                pane(
                    "bbbbbbbb22",
                    "make",
                    "/home/u/p",
                    &["make", "-j", "8"],
                    false,
                ),
            ],
        );
        for needle in [
            "TITLE",
            "CWD",
            "COMMAND",
            "nvim",
            "make",
            "main.rs",
            "-j 8",
            "/home/u/p",
            "aaaaaaaa",
            "bbbbbbbb",
        ] {
            assert!(
                out.contains(needle),
                "text list is missing {needle:?}:\n{out}"
            );
        }
    }

    /// `TerminalContext::width` had been captured since task 060 and read by
    /// nothing, so a wide column was free to push the frame off the screen —
    /// and the frame is not bounded by its columns alone: the header quotes the
    /// window and session names and the footer quotes them again.
    #[test]
    fn the_box_never_renders_wider_than_the_terminal() {
        let long_cmd: Vec<&str> = vec![
            "/usr/bin/bash",
            "-c",
            "for i in $(seq 1 100); do printf 'a very long line indeed %s\\n' \"$i\"; done; exec sleep 900",
        ];
        let panes = [
            pane(
                "aaaaaaaa11",
                "a-rather-long-pane-title-that-keeps-going",
                "/home/user/very/deep/project/tree/that/keeps/going/further",
                &long_cmd,
                true,
            ),
            pane("bbbbbbbb22", "sh", "/", &["sh"], false),
        ];
        // A ZOOMED pane, whose marker is `◀ focus [zoomed]` — nine columns
        // wider than `◀ focus`, and the case the first cut of the minimum
        // width forgot.
        let zoomed = [
            zoomed_pane("cccccccc33", "vim", "/home/user", &["vim", "a b.rs"]),
            pane("dddddddd44", "sh", "/", &["sh"], false),
        ];
        for width in (min_boxable_width() as u16)..=200 {
            for unicode in [true, false] {
                let ctx = pane_ctx(OutputFormat::Text, width, unicode);
                for set in [&panes[..], &zoomed[..]] {
                    let out = render_panes(
                        &ctx,
                        "a-session-name-long-enough-to-overflow-on-its-own",
                        "a-window-name-that-is-also-far-too-long-to-fit",
                        set,
                    );
                    let budget = inner_budget(&ctx) + 4;
                    for line in out.lines() {
                        assert!(
                            display_width(line) <= budget,
                            "width={width} unicode={unicode}: line is {} wide, budget {budget}:\n{line}\n--- full ---\n{out}",
                            display_width(line)
                        );
                    }
                    // Every line of one box must be the SAME width, or the frame
                    // is not a frame.
                    let widths: std::collections::BTreeSet<usize> =
                        out.lines().map(display_width).collect();
                    assert_eq!(
                        widths.len(),
                        1,
                        "width={width} unicode={unicode}: ragged frame {widths:?}:\n{out}"
                    );
                }
            }
        }
    }

    /// An empty list takes a different code path — `render_empty_state`,
    /// which no listing budgets. Pinned as a KNOWN residual rather than left
    /// unstated: it is shared by `session list` and `window list`, so bounding
    /// it belongs with bounding those.
    #[test]
    fn the_empty_pane_list_is_rendered_and_its_overflow_is_documented() {
        let ctx = pane_ctx(OutputFormat::Text, 40, true);
        let out = render_panes(&ctx, "dev", "editor", &[]);
        assert!(out.contains("(no panes)"), "{out}");
        let wide = render_panes(
            &ctx,
            "a-session-name-long-enough-to-overflow-on-its-own",
            "a-window-name-that-is-also-far-too-long-to-fit",
            &[],
        );
        assert!(
            wide.lines().any(|l| display_width(l) > 40),
            "render_empty_state started honouring the terminal width — good; \
             fold it into the budget and delete this pin"
        );
    }

    /// `fit_width` and `display_width` must measure the SAME way.
    ///
    /// `UnicodeWidthStr::width` is a property of the whole string, not the sum
    /// of its characters' widths, and for several ordinary sequences the two
    /// disagree badly: `☀️` (U+2600 U+FE0F) is 1 per character and **2** as a
    /// string, and a ZWJ family is 6 per character and **2** as a string. A
    /// truncator that adds up characters therefore hands back a cell whose real
    /// width is not the width it was asked for — `pad_right` then adds no
    /// padding, the row's reported visible length under-counts, and the box
    /// emits a line WIDER than the terminal.
    #[test]
    fn a_fitted_cell_is_exactly_as_wide_as_it_was_asked_to_be() {
        let samples = [
            "\u{2600}\u{fe0f}".repeat(30), // emoji presentation selector
            "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}".repeat(10), // ZWJ family
            "\u{1f1fa}\u{1f1f8}".repeat(10), // regional-indicator flags
            "\u{4e2d}\u{6587}".repeat(10), // CJK
            "e\u{301}".repeat(20),         // combining marks
            "plain ascii text that is quite long".to_string(),
            "\u{fe0f}".repeat(5), // a lone selector
        ];
        for s in &samples {
            for width in 0..40usize {
                let fitted = fit_width(s, width, "\u{2026}");
                assert!(
                    display_width(&fitted) <= width,
                    "{s:?} fitted to {width} came back {} wide: {fitted:?}",
                    display_width(&fitted)
                );
                // And it must not claim there is more when there is not.
                if display_width(s) <= width {
                    assert_eq!(
                        fitted, *s,
                        "{s:?} was truncated at width {width} for nothing"
                    );
                }
            }
        }
    }

    /// Truncation walks display width. A CJK character is two columns wide and
    /// cutting one in half corrupts the row and the frame with it.
    #[test]
    fn truncation_never_splits_a_wide_character() {
        for width in 24u16..=48 {
            let ctx = pane_ctx(OutputFormat::Text, width, true);
            let out = render_panes(
                &ctx,
                "d",
                "w",
                &[
                    pane(
                        "aaaaaaaa11",
                        "編集",
                        "/一二三四五六七八九十/一二三四五六七八九十",
                        &["編集", "一二三四五六七八九十.rs"],
                        true,
                    ),
                    // The sequences where per-character and whole-string width
                    // disagree — the ones that made the frame ragged.
                    pane(
                        "bbbbbbbb22",
                        &"\u{2600}\u{fe0f}".repeat(30),
                        &format!(
                            "/{}",
                            "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}".repeat(10)
                        ),
                        &[
                            "\u{1f1fa}\u{1f1f8}".repeat(10).as_str(),
                            "e\u{301}".repeat(20).as_str(),
                        ],
                        false,
                    ),
                ],
            );
            let widths: std::collections::BTreeSet<usize> =
                out.lines().map(display_width).collect();
            assert_eq!(
                widths.len(),
                1,
                "width={width}: ragged frame {widths:?}:\n{out}"
            );
        }
    }

    /// Cutting a grapheme cluster after its ZWJ or variation selector leaves a
    /// dangling half. There is no segmenter in this workspace, so the rule is
    /// "never end on a zero-width character".
    #[test]
    fn truncation_never_ends_on_a_zero_width_character() {
        for s in ["ab\u{200d}cd", "ab\u{fe0f}cd", "e\u{301}f\u{301}g"] {
            for w in 1..=6 {
                let cut = fit_width(s, w, "\u{2026}");
                let before_marker = cut.strip_suffix('\u{2026}').unwrap_or(&cut);
                assert!(
                    !before_marker
                        .chars()
                        .next_back()
                        .is_some_and(|c| UnicodeWidthStr::width(c.to_string().as_str()) == 0),
                    "{s:?} at width {w} was cut to {cut:?}, ending on a zero-width char"
                );
                assert!(
                    display_width(&cut) <= w,
                    "{s:?} at width {w} produced {cut:?}, {} wide",
                    display_width(&cut)
                );
            }
        }
    }

    /// The plain arm is tab-separated, so a TAB in a pane's own text would
    /// forge a column. The issue #104 egress guard escapes control characters —
    /// this pins that it still covers the new column, and that quoting runs
    /// BEFORE the guard so the escape is not itself quoted.
    #[test]
    fn a_control_character_cannot_forge_a_plain_column() {
        let out = render_panes(
            &pane_ctx(OutputFormat::Plain, 100, false),
            "d",
            "w",
            &[pane(
                "aaaaaaaa11",
                "ti\ttle\u{1b}]0;X\u{7}",
                "/cw\td",
                &["sh", "-c", "a\tb"],
                false,
            )],
        );
        assert_eq!(
            out.matches('\t').count(),
            3,
            "a payload tab became a column separator:\n{out:?}"
        );
        assert!(
            !out.chars()
                .any(|c| c.is_control() && c != '\t' && c != '\n'),
            "raw control byte reached the terminal: {out:?}"
        );
    }

    /// `pane.zoom` takes a pane id, so a pane can be zoomed without being
    /// focused — and that is the state the operator most needs told about,
    /// since a zoomed pane is why the others are off screen. Both human formats
    /// used to say nothing while `--format json` said `is_zoomed: true`.
    #[test]
    fn a_zoomed_pane_is_marked_even_when_it_does_not_have_focus() {
        let unfocused_zoom = PaneInfo {
            is_focused: false,
            is_zoomed: true,
            ..pane("aaaaaaaa11", "vim", "/home/u", &["vim"], false)
        };
        let out = render_panes(
            &pane_ctx(OutputFormat::Text, 100, true),
            "dev",
            "editor",
            &[unfocused_zoom, pane("bbbbbbbb22", "sh", "/", &["sh"], true)],
        );
        assert!(out.contains("[zoomed]"), "zoom is invisible:\n{out}");
        assert!(out.contains("\u{25c0} focus"), "focus marker lost:\n{out}");
    }

    /// `session list` and `window list` share `ColumnLayout` and `BoxRenderer`.
    /// The budget is opt-in precisely so they are untouched; this pins it.
    #[test]
    fn the_other_listings_are_byte_identical_to_before() {
        let mut layout = ColumnLayout::new(vec![
            Column {
                header: "A".to_string(),
                align: Align::Left,
                min_width: 2,
            },
            Column {
                header: "BB".to_string(),
                align: Align::Right,
                min_width: 1,
            },
        ]);
        layout.add_row(vec!["a-very-long-cell-indeed".to_string(), "9".to_string()]);
        assert_eq!(layout.total_width(), 23 + 3 + 2);
        let (header, len) = layout.render_header(false);
        assert_eq!(header, "A                         BB");
        assert_eq!(len, 28);
        let (row, len) = layout.render_row(0, &|_, c| c.to_string());
        assert_eq!(row, "a-very-long-cell-indeed    9");
        assert_eq!(len, 28);
    }

    /// The three consumers of `col_widths` must agree, or the row's reported
    /// visible length and the box's inner width drift and every border
    /// misaligns.
    #[test]
    fn a_budgeted_layout_reports_one_width_to_all_three_callers() {
        let mut layout = ColumnLayout::new(vec![
            Column {
                header: "ID".to_string(),
                align: Align::Left,
                min_width: 8,
            },
            Column {
                header: "CMD".to_string(),
                align: Align::Left,
                min_width: 3,
            },
        ])
        .budget(30, &[1], true);
        layout.add_row(vec!["abcdefgh".to_string(), "x".repeat(200)]);
        let total = layout.total_width();
        let (_, header_len) = layout.render_header(false);
        let (row, row_len) = layout.render_row(0, &|_, c| c.to_string());
        assert_eq!(total, header_len);
        assert_eq!(total, row_len);
        assert_eq!(display_width(&row), total);
        assert!(total <= 30, "budget ignored: {total}");
    }
}
