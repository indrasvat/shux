//! SPIKE: prove the LIVE half — shux re-emitting stored images as kitty
//! commands, the way it would to an attached client's terminal.
//! Reads raw PTY bytes, decodes images, prints re-emission bytes to stdout.
use std::io::Write;
use base64::Engine as _;
use shux_vt::VirtualTerminal;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut a = std::env::args().skip(1);
    let raw = std::fs::read(a.next().expect("usage: reemit <raw>"))?;
    let cols: usize = std::env::var("COLS").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
    let rows: usize = std::env::var("ROWS").ok().and_then(|v| v.parse().ok()).unwrap_or(12);
    let mut vt = VirtualTerminal::new(rows, cols);
    vt.process(&raw);
    let grid = vt.grid().clone_visible();
    let out = std::io::stdout();
    let mut w = out.lock();
    for img in &grid.spike_images {
        // re-encode canonically (D4: store a re-emittable copy, not wire bytes)
        let b64 = base64::engine::general_purpose::STANDARD.encode(&img.rgba);
        let chunks: Vec<&str> = b64.as_bytes().chunks(4096)
            .map(|c| std::str::from_utf8(c).unwrap()).collect();
        for (i, c) in chunks.iter().enumerate() {
            let more = u8::from(i != chunks.len() - 1);
            if i == 0 {
                write!(w, "\x1b_Ga=T,f=32,s={},v={},t=d,i={},p=1,C=1,q=2,m={};{}\x1b\\",
                    img.width, img.height, img.image_id, more, c)?;
            } else {
                write!(w, "\x1b_Gm={more};{c}\x1b\\")?;
            }
        }
    }
    eprintln!("re-emitted {} image(s)", grid.spike_images.len());
    Ok(())
}
