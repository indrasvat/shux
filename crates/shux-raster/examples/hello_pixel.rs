//! `cargo run -p shux-raster --example hello_pixel [out.png]`
//!
//! Renders a stylized banner into a `VirtualTerminal`, rasterizes the resulting
//! grid to PNG without involving any terminal emulator, and writes it out.

use shux_raster::{RasterOptions, Rasterizer};
use shux_vt::VirtualTerminal;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Banner avoids ┌─┐│└┘ box-drawing — those rely on every interior
    // cell being exactly 1 column wide, and even one stray wide glyph
    // (em-dash, soft-hyphen, an unintentional emoji) misaligns the right
    // edge. A simple double rule above + below the title is robust at
    // any cell count.
    let mut vt = VirtualTerminal::new(20, 72);
    let banner = "\x1b[1;36m  ══════════════════════════════════════════════════════════════════\r\n\
\x1b[1;36m   \x1b[1;33mshux-raster\x1b[0;1;36m  ·  pixels without a terminal emulator\r\n\
\x1b[1;36m  ══════════════════════════════════════════════════════════════════\x1b[0m\r\n\
\r\n\
  Hello, \x1b[1;31mshux!\x1b[0m  This PNG was produced by \x1b[1;35mshux-raster\x1b[0m,\r\n\
  not by iTerm2, not by Alacritty — just shux-vt + fontdue + image.\r\n\
\r\n\
  Attributes:  \x1b[7m INVERSE \x1b[0m  \x1b[4mUNDERLINE\x1b[0m  \x1b[9mSTRIKE\x1b[0m  \x1b[1mBOLD\x1b[0m  \x1b[2mdim\x1b[0m\r\n\
\r\n\
  Palette ramp:  \x1b[38;5;46m\u{2588}\x1b[38;5;82m\u{2588}\x1b[38;5;118m\u{2588}\x1b[38;5;154m\u{2588}\x1b[38;5;190m\u{2588}\x1b[38;5;226m\u{2588}\x1b[38;5;220m\u{2588}\x1b[38;5;214m\u{2588}\x1b[38;5;208m\u{2588}\x1b[38;5;202m\u{2588}\x1b[38;5;196m\u{2588}\x1b[0m\r\n\
  Truecolor RGB: \x1b[38;2;255;100;200m\u{2588}\u{2588}\u{2588}\x1b[38;2;100;200;255m\u{2588}\u{2588}\u{2588}\x1b[38;2;180;255;120m\u{2588}\u{2588}\u{2588}\x1b[0m  \x1b[48;2;30;30;60;38;2;255;255;255m\u{2588} on-bg \u{2588}\x1b[0m\r\n\
\r\n\
  This is the \x1b[1;35mMOAT\x1b[0m: shux can pixel-snapshot itself,\r\n\
  feed PNGs to a vision LLM, and run golden-image L4 tests\r\n\
  on a Linux CI box — no iTerm2, no display server, no GUI runner.\r\n\
\r\n\
  $ \x1b[1mshux api session.snapshot \x1b[3m'{\"session_id\":\"...\"}'\x1b[0m\r\n";
    vt.process(banner.as_bytes());

    let r = Rasterizer::new(16.0)?;
    let opts = RasterOptions {
        cursor: Some((vt.cursor().row, vt.cursor().col)),
        ..Default::default()
    };
    let img = r.render(vt.grid(), &opts);

    let out_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| ".claude/screenshots/raster_hello.png".to_string());
    if let Some(parent) = std::path::Path::new(&out_path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    img.save(&out_path)?;
    let (cw, ch) = r.cell_size();
    println!(
        "wrote {out_path} ({}x{} px, cell {cw}x{ch}px, grid {}x{})",
        img.width(),
        img.height(),
        vt.grid().cols(),
        vt.grid().rows(),
    );
    Ok(())
}
