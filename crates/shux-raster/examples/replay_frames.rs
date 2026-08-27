//! SPIKE: replay recorded raw PTY bytes through shux's own VT + rasterizer,
//! emitting one PNG per time slice. Set REPLAY_NO_IMAGES=1 to render the same
//! bytes as the pre-spike build would -- identical input, two renderers.
use std::env;
use std::fs;
use std::path::PathBuf;

use shux_raster::{RasterOptions, Rasterizer};
use shux_vt::VirtualTerminal;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut a = env::args().skip(1);
    let raw_path = PathBuf::from(a.next().expect("usage: replay_frames <raw> <outdir> <frames>"));
    let out = PathBuf::from(a.next().expect("outdir"));
    let frames: usize = a.next().unwrap_or_else(|| "80".into()).parse()?;
    let cols: usize = env::var("REPLAY_COLS").ok().and_then(|v| v.parse().ok()).unwrap_or(120);
    let rows: usize = env::var("REPLAY_ROWS").ok().and_then(|v| v.parse().ok()).unwrap_or(38);
    let no_images = env::var("REPLAY_NO_IMAGES").is_ok();

    fs::create_dir_all(&out)?;
    let raw = fs::read(&raw_path)?;
    let font = fs::read("crates/shux-raster/assets/JetBrainsMonoNerdFontMono-Regular.ttf")?;
    let r = Rasterizer::with_primary_font(14.0, &font)?;
    let opts = RasterOptions::default();

    let mut vt = VirtualTerminal::new(rows, cols);
    let step = raw.len().div_ceil(frames).max(1);
    let mut written = 0usize;
    for (i, chunk) in raw.chunks(step).enumerate() {
        vt.process(chunk);
        let mut grid = vt.grid().clone_visible();
        if no_images {
            grid.spike_images.clear();
        }
        let img = r.render(&grid, &opts);
        img.save(out.join(format!("f{i:04}.png")))?;
        written += 1;
    }
    eprintln!(
        "{} frames, {} bytes, images={}",
        written,
        raw.len(),
        if no_images { "OFF" } else { "ON" }
    );
    Ok(())
}
