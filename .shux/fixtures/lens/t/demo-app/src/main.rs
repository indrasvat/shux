//! lens T-tier demo app (§13 T5) — a tiny ratatui UI with ONE seeded visual
//! bug: the top border has a break at column 80. Text capture cannot see it;
//! only a pixel glance can. An unaided agent, given the rewritten skill, must
//! find and fix it (see `SEEDED BUG` below) and attach before/after PNGs.
//!
//! Run inside a shux scratch pane sized wider than 80 columns (e.g. 120x30):
//!   shux lens run --size 120x30 -- lens-demo-app
//! Quit with `q`.

use std::io;
use std::time::Duration;

use ratatui::crossterm::event::{self, Event, KeyCode};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

/// Column at which the top border is (buggily) broken.
const BREAK_COL: usize = 80;

fn top_border(width: usize) -> String {
    if width < 2 {
        return "─".repeat(width);
    }
    // A correct top border: ┌────…────┐
    let mut cells: Vec<char> = std::iter::once('┌')
        .chain(std::iter::repeat_n('─', width - 2))
        .chain(std::iter::once('┐'))
        .collect();

    // ── SEEDED BUG ────────────────────────────────────────────────────────
    // Punch a one-cell gap in the top border at column 80. The fix is to
    // delete this block so the border stays continuous.
    if width > BREAK_COL {
        cells[BREAK_COL] = ' ';
    }
    // ──────────────────────────────────────────────────────────────────────

    cells.into_iter().collect()
}

fn draw(frame: &mut Frame) {
    let area: Rect = frame.area();
    let w = area.width as usize;
    let h = area.height as usize;
    if w < 2 || h < 2 {
        return;
    }

    let border = Style::default().fg(Color::Cyan);
    let title = Style::default().fg(Color::Yellow);

    let mut lines: Vec<Line> = Vec::with_capacity(h);
    lines.push(Line::from(Span::styled(top_border(w), border)));

    let inner = "│".to_string() + &" ".repeat(w - 2) + "│";
    for row in 1..h - 1 {
        if row == 2 {
            let label = " demo-app — spot the border break (q to quit) ";
            let mut mid = String::from("│");
            mid.push_str(label);
            while mid.chars().count() < w - 1 {
                mid.push(' ');
            }
            mid.push('│');
            lines.push(Line::from(Span::styled(mid, title)));
        } else {
            lines.push(Line::from(Span::styled(inner.clone(), border)));
        }
    }

    let bottom = "└".to_string() + &"─".repeat(w - 2) + "┘";
    lines.push(Line::from(Span::styled(bottom, border)));

    frame.render_widget(Paragraph::new(Text::from(lines)), area);
}

fn main() -> io::Result<()> {
    let mut terminal = ratatui::init();
    let result = run(&mut terminal);
    ratatui::restore();
    result
}

fn run(terminal: &mut ratatui::DefaultTerminal) -> io::Result<()> {
    loop {
        terminal.draw(draw)?;
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                    return Ok(());
                }
            }
        }
    }
}
