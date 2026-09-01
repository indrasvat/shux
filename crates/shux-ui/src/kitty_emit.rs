//! Re-transmit pane images to the terminal the user is attached from.
//!
//! The compositor writes cells. Images are not cells, so without this an
//! attached client renders every pane's text and none of its pictures.
//!
//! Payloads leave here exactly as they arrived — still deflated, still PNG —
//! and the pane clip is a source rectangle (`x/y/w/h`) rather than a cropped
//! bitmap, which is how zellij bounds a placement to its pane. `c=`/`r=` only
//! ever STRETCH into a cell box, so nothing else stops a picture taller than
//! its pane painting over the status bar.

use std::io::{self, Write};
use std::sync::Arc;

use base64::Engine as _;
use shux_vt::{ComposedPlacement, DECLARED_CELL_PIXELS, ImageFormat, StoredImage};

/// Base64 bytes per chunk: the protocol's maximum, and konsole caps control
/// data at 1024 so nothing larger is portable anyway.
const CHUNK: usize = 4096;

/// Host image ids start here so they cannot collide with the ids a pane's own
/// application chose for the same terminal.
const HOST_ID_BASE: u32 = 2_000_000_000;

/// Where one placement was put, in the terms the host was told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Placed {
    row: u16,
    col: u16,
    /// Source rectangle as `y, w, h`. No `x`: a placement can never start left
    /// of its own clip, the same invariant `shux-raster`'s `blit` relies on.
    src: (u32, u32, u32),
    cells: (u16, u16),
}

struct Live {
    /// Held, not just compared by address: dropping it would let the allocator
    /// hand the same address to the next image and make a changed picture
    /// compare equal.
    image: Arc<StoredImage>,
    host_id: u32,
    placed: Placed,
}

/// What the user's terminal currently holds, for one attach.
#[derive(Default)]
pub(crate) struct KittyEmitter {
    live: Vec<Live>,
    next_id: u32,
}

impl KittyEmitter {
    /// Emit whatever this frame changed, and report whether it wrote anything.
    pub(crate) fn emit(
        &mut self,
        out: &mut impl Write,
        placements: &[ComposedPlacement],
    ) -> io::Result<bool> {
        let want: Vec<(&ComposedPlacement, Placed)> = placements
            .iter()
            .filter_map(|p| resolve(p).map(|r| (p, r)))
            .collect();
        if want.is_empty() && self.live.is_empty() {
            return Ok(false);
        }
        let mut wrote = false;

        for (i, (p, placed)) in want.iter().enumerate() {
            match self.live.get(i) {
                Some(live) if Arc::ptr_eq(&live.image, &p.image) => {
                    if live.placed == *placed {
                        continue;
                    }
                    // Same pixels, new geometry. Re-putting under the same
                    // image AND placement id replaces the placement in place,
                    // which is the protocol's own answer to flicker.
                    let id = live.host_id;
                    cup(out, placed)?;
                    write!(out, "\x1b_Ga=p,i={id},{}\x1b\\", keys(placed))?;
                    self.live[i].placed = *placed;
                }
                _ => {
                    let id = self.take_id();
                    transmit(out, id, &p.image, placed)?;
                    // Only now is the replacement up: re-transmitting under a
                    // live id frees the old image and its placement on the
                    // FIRST chunk, blanking the pane for the whole transfer.
                    let fresh = Live {
                        image: p.image.clone(),
                        host_id: id,
                        placed: *placed,
                    };
                    if let Some(old) = self.live.get(i) {
                        delete(out, old.host_id)?;
                        self.live[i] = fresh;
                    } else {
                        self.live.push(fresh);
                    }
                }
            }
            wrote = true;
        }

        for gone in self.live.drain(want.len()..) {
            delete(out, gone.host_id)?;
            wrote = true;
        }
        Ok(wrote)
    }

    fn take_id(&mut self) -> u32 {
        let id = HOST_ID_BASE.wrapping_add(self.next_id);
        self.next_id = self.next_id.wrapping_add(1);
        id
    }
}

/// Intersect a placement with its pane. `None` once nothing of it is left
/// inside; rows above the clip become the source rectangle's `y`.
fn resolve(p: &ComposedPlacement) -> Option<Placed> {
    let (cw, ch) = DECLARED_CELL_PIXELS;
    let clip_top = p.clip.row as i64;
    let nat_rows = p.image.height.div_ceil(ch).max(1) as i64;
    let nat_cols = p.image.width.div_ceil(cw).max(1) as i64;

    let top = p.row.max(clip_top);
    let bottom = (p.row + nat_rows).min(clip_top + p.clip.rows as i64);
    let rows = (bottom - top).max(0);
    let right = ((p.col + nat_cols as usize).min(p.clip.col + p.clip.cols)) as i64;
    let cols = (right - p.col as i64).max(0);
    if rows == 0 || cols == 0 {
        return None;
    }

    let y = ((top - p.row) as u32).saturating_mul(ch);
    let h = (rows as u32)
        .saturating_mul(ch)
        .min(p.image.height.saturating_sub(y));
    let w = (cols as u32).saturating_mul(cw).min(p.image.width);
    if w == 0 || h == 0 {
        return None;
    }
    Some(Placed {
        row: u16::try_from(top).ok()?,
        col: u16::try_from(p.col).ok()?,
        src: (y, w, h),
        cells: (u16::try_from(cols).ok()?, u16::try_from(rows).ok()?),
    })
}

/// `C=1` so display never moves the host's cursor: kitty otherwise advances it
/// and scrolls the whole screen for an image at the bottom margin. `q=2` so the
/// host's `OK` never lands in the client's key decoder.
fn keys(p: &Placed) -> String {
    let (y, w, h) = p.src;
    let (c, r) = p.cells;
    format!("p=1,C=1,q=2,y={y},w={w},h={h},c={c},r={r}")
}

fn cup(out: &mut impl Write, p: &Placed) -> io::Result<()> {
    write!(out, "\x1b[{};{}H", p.row + 1, p.col + 1)
}

fn transmit(out: &mut impl Write, id: u32, img: &StoredImage, placed: &Placed) -> io::Result<()> {
    let fmt = match img.format {
        ImageFormat::Rgba32 => 32,
        ImageFormat::Rgb24 => 24,
        ImageFormat::Png => 100,
    };
    let zlib = if img.compressed { ",o=z" } else { "" };
    let b64 = base64::engine::general_purpose::STANDARD.encode(&img.payload);
    let bytes = b64.as_bytes();
    if bytes.is_empty() {
        return Ok(());
    }
    let chunks = bytes.len().div_ceil(CHUNK);
    cup(out, placed)?;
    for (i, chunk) in bytes.chunks(CHUNK).enumerate() {
        let more = u8::from(i + 1 < chunks);
        let payload = std::str::from_utf8(chunk).unwrap_or("");
        if i == 0 {
            write!(
                out,
                "\x1b_Ga=T,f={fmt}{zlib},t=d,i={id},s={},v={},{},m={more};{payload}\x1b\\",
                img.width,
                img.height,
                keys(placed)
            )?;
        } else {
            write!(out, "\x1b_Gq=2,m={more};{payload}\x1b\\")?;
        }
    }
    Ok(())
}

/// `d=I` frees the pixels as well as the placement. Every id retired here has
/// already been replaced, so nothing wants them back.
fn delete(out: &mut impl Write, id: u32) -> io::Result<()> {
    write!(out, "\x1b_Ga=d,d=I,i={id},q=2\x1b\\")
}
