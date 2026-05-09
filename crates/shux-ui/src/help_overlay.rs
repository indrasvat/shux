//! Static keybinding cheat sheet rendered as an ANSI overlay.
//!
//! M1 slice of task 033 — discoverability without the full
//! KeybindingRegistry / command palette stack the spec calls for. The
//! entries here mirror what `shux-ui::attach::key_to_prefix_action`
//! and `key_to_bare_action` actually map today, so the overlay never
//! lies about the live behavior.
//!
//! Activated by `prefix + ?` (mapped to `ActionKind::ToggleHelp`),
//! dismissed by Escape or `q`. The daemon-side render loop calls
//! `render_help_overlay_into` after the multipane render completes
//! and before the frame is shipped to the client.

use std::io::Write as _;

use unicode_width::UnicodeWidthStr;

use shux_core::theme::{Rgb, Theme};

/// One row in the overlay: the displayable key combo + the action
/// description. Both are static strings keyed off what the input map
/// actually does — see `attach::key_to_prefix_action`.
struct Entry {
    key: &'static str,
    desc: &'static str,
}

/// Sectioned cheat sheet. Categories are ordered the way a new user
/// usually discovers features: navigation first, then layout, then
/// session/window management, then misc.
struct Section {
    title: &'static str,
    entries: &'static [Entry],
}

// Sections kept dense enough to fit a typical 24-row terminal: ~14 body
// lines + 4 chrome rows = 18 rows total. Bare (Alt+...) shortcuts get
// one summary line per category instead of being enumerated, since the
// prefix bindings are the primary documented path.
const SECTIONS: &[Section] = &[
    Section {
        title: "Pane focus",
        entries: &[
            Entry {
                key: "Prefix h / j / k / l",
                desc: "Focus pane left / down / up / right",
            },
            Entry {
                key: "Prefix o",
                desc: "Cycle focus to next pane",
            },
            Entry {
                key: "Alt + arrows / Tab",
                desc: "Same, no prefix needed",
            },
        ],
    },
    Section {
        title: "Pane layout",
        entries: &[
            Entry {
                key: "Prefix | / -",
                desc: "Split vertical / horizontal",
            },
            Entry {
                key: "Prefix Space",
                desc: "Smart split (auto-axis)",
            },
            Entry {
                key: "Prefix z",
                desc: "Toggle zoom on focused pane",
            },
            Entry {
                key: "Prefix x",
                desc: "Kill focused pane",
            },
            Entry {
                key: "Prefix ←/→/↑/↓",
                desc: "Resize focused pane (5%)",
            },
        ],
    },
    Section {
        title: "Windows",
        entries: &[Entry {
            key: "Prefix c / n / p",
            desc: "Create / next / previous window",
        }],
    },
    Section {
        title: "Session & misc",
        entries: &[
            Entry {
                key: "Prefix d",
                desc: "Detach client",
            },
            Entry {
                key: "Prefix r",
                desc: "Force full redraw",
            },
            Entry {
                key: "Prefix ?",
                desc: "Toggle this help overlay",
            },
            Entry {
                key: "Esc or q",
                desc: "Dismiss overlay",
            },
        ],
    },
];

/// Default prefix label. Updated when the keybinding system supports
/// configurable prefixes.
const PREFIX_LABEL: &str = "Ctrl+Space";

/// Append ANSI bytes that draw a centered rounded box with the cheat
/// sheet over `cols × rows`. Caller is responsible for forcing a full
/// redraw on the next frame so the cells UNDER the overlay come back
/// when the user dismisses it.
///
/// The function never panics on small terminals — if the box would
/// not fit, it falls back to a single-line "Help: prefix + ? to view"
/// hint at the top so users still get a discoverability cue.
pub fn render_help_overlay_into(buf: &mut Vec<u8>, cols: u16, rows: u16, theme: &Theme) {
    let cols = cols as usize;
    let rows = rows as usize;
    if cols < 8 || rows < 4 {
        return;
    }

    // Lay out the body lines once so we know the box dimensions.
    let body_lines = build_body_lines();
    let inner_w = body_lines
        .iter()
        .map(|s| display_width(s))
        .max()
        .unwrap_or(0);
    let title = format!(" shux — keybindings (prefix = {PREFIX_LABEL}) ");
    let title_w = display_width(&title);
    let inner_w = inner_w.max(title_w).max(40);

    // 2-cell horizontal padding inside the border, plus 1 cell each
    // side for the border itself = +6 total width.
    let box_w = inner_w + 6;
    let box_h = body_lines.len() + 4; // top border, blank, body, blank, bottom border? we use 2 borders + body

    // If it doesn't fit, render a single-line hint instead.
    if box_w + 2 > cols || box_h + 2 > rows {
        write_hint_line(buf, cols, theme);
        return;
    }

    let x0 = (cols - box_w) / 2;
    let y0 = (rows - box_h) / 2;

    let bg = theme.status_bg;
    let fg = theme.status_fg;
    let accent = theme.status_accent;

    // Helper: emit the SGR + position + payload for one row.
    let put = |buf: &mut Vec<u8>, row: usize, text: String| {
        // Move cursor to (row, x0+1) — ANSI is 1-based.
        let _ = write!(buf, "\x1b[{};{}H", row + 1, x0 + 1);
        // Reset, then style for this row.
        buf.extend_from_slice(b"\x1b[0m");
        buf.extend_from_slice(text.as_bytes());
    };

    let bg_seq = sgr_bg(bg);
    let fg_seq = sgr_fg(fg);
    let accent_seq = sgr_fg(accent);
    let dim_seq = b"\x1b[2m";
    let reset = b"\x1b[0m";

    // Top border with title centered.
    {
        let mut line = String::new();
        line.push_str(&bg_seq);
        line.push_str(&accent_seq);
        line.push('╭');
        let title_pad = (inner_w + 4).saturating_sub(title_w);
        let left = title_pad / 2;
        let right = title_pad - left;
        line.push_str(&"─".repeat(left));
        line.push_str(&title);
        line.push_str(&"─".repeat(right));
        line.push('╮');
        line.push_str(&String::from_utf8_lossy(reset));
        put(buf, y0, line);
    }

    // Body lines.
    for (idx, body) in body_lines.iter().enumerate() {
        let mut line = String::new();
        line.push_str(&bg_seq);
        line.push_str(&accent_seq);
        line.push('│');
        line.push_str(&String::from_utf8_lossy(reset));
        line.push_str(&bg_seq);
        line.push_str(&fg_seq);
        line.push_str("  ");
        line.push_str(body);
        let pad = inner_w.saturating_sub(display_width(body));
        line.push_str(&" ".repeat(pad));
        line.push_str("  ");
        line.push_str(&String::from_utf8_lossy(reset));
        line.push_str(&bg_seq);
        line.push_str(&accent_seq);
        line.push('│');
        line.push_str(&String::from_utf8_lossy(reset));
        put(buf, y0 + 1 + idx, line);
    }

    // Footer: one blank line then dismiss hint, then bottom border.
    {
        let mut line = String::new();
        line.push_str(&bg_seq);
        line.push_str(&accent_seq);
        line.push('│');
        line.push_str(&String::from_utf8_lossy(reset));
        line.push_str(&bg_seq);
        line.push_str(&String::from_utf8_lossy(dim_seq));
        line.push_str(&fg_seq);
        let hint = "  Esc or q to dismiss";
        line.push_str(hint);
        let pad = (inner_w + 4).saturating_sub(display_width(hint));
        line.push_str(&" ".repeat(pad));
        line.push_str(&String::from_utf8_lossy(reset));
        line.push_str(&bg_seq);
        line.push_str(&accent_seq);
        line.push('│');
        line.push_str(&String::from_utf8_lossy(reset));
        put(buf, y0 + 1 + body_lines.len(), line);
    }

    {
        let mut line = String::new();
        line.push_str(&bg_seq);
        line.push_str(&accent_seq);
        line.push('╰');
        line.push_str(&"─".repeat(inner_w + 4));
        line.push('╯');
        line.push_str(&String::from_utf8_lossy(reset));
        put(buf, y0 + 2 + body_lines.len(), line);
    }
}

fn build_body_lines() -> Vec<String> {
    // Two-column layout: key column padded to a fixed width, then desc.
    let key_col_w = SECTIONS
        .iter()
        .flat_map(|s| s.entries.iter())
        .map(|e| display_width(e.key))
        .max()
        .unwrap_or(20);

    let mut out = Vec::new();
    for (i, section) in SECTIONS.iter().enumerate() {
        if i > 0 {
            out.push(String::new()); // blank line between sections
        }
        // Section header: uppercase title, dim style applied at render time.
        out.push(format!("── {} ──", section.title));
        for entry in section.entries {
            let pad = key_col_w.saturating_sub(display_width(entry.key));
            out.push(format!(
                "  {}{}    {}",
                entry.key,
                " ".repeat(pad),
                entry.desc
            ));
        }
    }
    out
}

fn write_hint_line(buf: &mut Vec<u8>, cols: usize, theme: &Theme) {
    let hint = format!(" {PREFIX_LABEL} + ?  for help ");
    let hint_w = display_width(&hint);
    if hint_w + 2 > cols {
        return;
    }
    let x = (cols - hint_w) / 2 + 1;
    let _ = write!(
        buf,
        "\x1b[1;{x}H{}{}{}\x1b[0m",
        sgr_bg(theme.status_bg),
        sgr_fg(theme.status_accent),
        hint,
    );
}

fn sgr_fg(c: Rgb) -> String {
    format!("\x1b[38;2;{};{};{}m", c.r, c.g, c.b)
}

fn sgr_bg(c: Rgb) -> String {
    format!("\x1b[48;2;{};{};{}m", c.r, c.g, c.b)
}

fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_into_buffer_for_reasonable_size() {
        let mut buf = Vec::new();
        render_help_overlay_into(&mut buf, 100, 30, &Theme::DEFAULT);
        // Should contain CSI cursor moves and at least one section title.
        let s = String::from_utf8_lossy(&buf);
        assert!(s.contains("Pane focus"), "missing 'Pane focus' section");
        assert!(s.contains("Esc or q"), "missing dismiss hint");
        assert!(s.contains("\x1b["), "missing ANSI escape sequences");
    }

    #[test]
    fn falls_back_to_hint_for_tiny_terminal() {
        let mut buf = Vec::new();
        render_help_overlay_into(&mut buf, 30, 5, &Theme::DEFAULT);
        let s = String::from_utf8_lossy(&buf);
        // Tiny: should only emit the single-line hint, no section title.
        assert!(!s.contains("Pane focus"));
        assert!(s.contains("for help"));
    }

    #[test]
    fn produces_no_output_when_terminal_too_small() {
        let mut buf = Vec::new();
        render_help_overlay_into(&mut buf, 4, 2, &Theme::DEFAULT);
        assert!(buf.is_empty());
    }

    #[test]
    fn body_lines_has_all_sections() {
        let lines = build_body_lines();
        let joined = lines.join("\n");
        for s in SECTIONS {
            assert!(joined.contains(s.title), "missing section: {}", s.title);
        }
    }

    #[test]
    fn sgr_helpers_emit_truecolor_sequences() {
        let fg = sgr_fg(Rgb::new(255, 128, 0));
        assert_eq!(fg, "\x1b[38;2;255;128;0m");
        let bg = sgr_bg(Rgb::new(0, 0, 0));
        assert_eq!(bg, "\x1b[48;2;0;0;0m");
    }
}
