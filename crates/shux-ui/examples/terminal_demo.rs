//! Standalone demo exercising TerminalGuard + compositor + key encoding.
//!
//! This demo enters raw mode + alternate screen, creates a local
//! VirtualTerminal (not daemon-connected), feeds keyboard input through
//! the key encoder into the VT, and renders via the compositor.
//!
//! Press Ctrl+Space then 'd' to detach (clean exit).
//!
//! Usage: cargo run --example terminal_demo -p shux-ui

use std::io::{self, Write};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};

use shux_ui::buffer::RenderCell;
use shux_ui::client::{encode_key_event, parse_key_from_bytes};
use shux_ui::compositor::{CompositorConfig, RenderCompositor};
use shux_ui::terminal::TerminalGuard;
use shux_vt::VirtualTerminal;

fn main() -> anyhow::Result<()> {
    // Install panic hook before entering raw mode
    shux_ui::terminal::install_panic_hook();

    // Enter TUI mode
    let mut guard = TerminalGuard::enter()?;
    let (cols, rows) = TerminalGuard::size()?;

    // Create a local VirtualTerminal (not daemon-connected)
    let mut vt = VirtualTerminal::new(rows as usize, cols as usize);

    // Create compositor writing to stdout
    let config = CompositorConfig::default();
    let stdout = io::stdout();
    let mut compositor = RenderCompositor::new(cols, rows, stdout.lock(), config);
    compositor.clear()?;

    // Write banner into the VT
    let banner = format!(
        "\x1b[1;32mshux terminal demo\x1b[0m (Ctrl+Space d to exit)\r\n\
         Terminal size: {cols}x{rows}\r\n\
         \r\n$ "
    );
    vt.process(banner.as_bytes());

    // Initial render
    render_vt(&mut compositor, &vt, cols, rows)?;

    // State for prefix key detection
    let mut prefix_active = false;
    let prefix_key = crossterm::event::KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL);

    // Event loop
    loop {
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) => {
                    let encoded = encode_key_event(key);
                    if encoded.is_empty() {
                        continue;
                    }

                    // Check for prefix key handling
                    if let Some(parsed) = parse_key_from_bytes(&encoded) {
                        if prefix_active {
                            prefix_active = false;
                            if parsed.code == KeyCode::Char('d') {
                                // Detach
                                break;
                            }
                            // Not a recognized prefix command; feed the key to VT
                            vt.process(&encoded);
                        } else if parsed == prefix_key {
                            prefix_active = true;
                            continue;
                        } else {
                            // Regular input: feed to VT
                            vt.process(&encoded);
                        }
                    } else {
                        // Multi-byte sequence (arrows, etc): feed to VT
                        vt.process(&encoded);
                    }

                    // Re-render after input
                    render_vt(&mut compositor, &vt, cols, rows)?;
                }
                Event::Resize(new_cols, new_rows) => {
                    vt.resize(new_rows as usize, new_cols as usize);
                    compositor.resize(new_cols, new_rows);
                    compositor.force_redraw();
                    render_vt(&mut compositor, &vt, new_cols, new_rows)?;
                }
                _ => {}
            }
        }
    }

    // Clean exit
    guard.leave()?;
    println!("[detached from demo]");

    Ok(())
}

fn render_vt<W: Write>(
    compositor: &mut RenderCompositor<W>,
    vt: &VirtualTerminal,
    cols: u16,
    rows: u16,
) -> io::Result<()> {
    let grid = vt.grid();
    let cursor = vt.cursor();

    compositor.render_frame(
        |col, row| {
            let vt_row = grid.visible_row(row as usize);
            if (col as usize) < vt_row.len() {
                RenderCell::from(&vt_row[col as usize])
            } else {
                RenderCell::default()
            }
        },
        cols,
        rows,
        Some((cursor.col as u16, cursor.row as u16)),
    )?;

    Ok(())
}
