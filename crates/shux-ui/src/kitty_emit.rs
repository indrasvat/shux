//! Re-transmit pane images to the terminal the user is attached from.
//!
//! The compositor writes cells. Images are not cells, so without this an
//! attached client renders every pane's text and none of its pictures.
//!
//! Payloads leave here exactly as they arrived — still deflated, still PNG —
//! and the pane clip is a source rectangle, not a cropped bitmap.

use std::io::{self, Write};
use std::sync::Arc;

use base64::Engine as _;
use shux_core::model::PaneId;
use shux_vt::{ComposedPlacement, DECLARED_CELL_PIXELS, Grid, ImageFormat, StoredImage};

use crate::viewport::PaneViewport;

/// Base64 bytes per chunk: the protocol's maximum, and konsole caps control
/// data at 1024 so nothing larger is portable anyway.
const CHUNK: usize = 4096;

/// Host image ids start here so they cannot collide with the ids a pane's own
/// application chose for the same terminal.
const HOST_ID_BASE: u32 = 2_000_000_000;

/// Base64 image bytes one frame may emit. The daemon ships the whole composited
/// frame as one base64 `Render` and the codec caps it at `MAX_FRAME_SIZE`; 6 MiB
/// here leaves the outer encode of that, plus a frame's cells, inside the cap.
const FRAME_IMAGE_BUDGET: usize = 6 * 1024 * 1024;

/// Where one placement was put, in the terms the host was told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Placed {
    row: u16,
    col: u16,
    /// Source rectangle as `x, y, w, h`: what of the crop the clip leaves.
    src: (u32, u32, u32, u32),
    cells: (u16, u16),
    /// Pixels into the first cell the picture starts: `X=`, `Y=`.
    offset: (u64, u64),
}

/// Which placement of which pane a composed one is, from frame to frame,
/// whatever else that frame clips.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlacementKey {
    /// One its application can address again, and so move.
    Addressed {
        pane: PaneId,
        image: u32,
        placement: u32,
    },
    /// One nothing can move: its anchor, and how many earlier ones share it.
    Anchored {
        pane: PaneId,
        row: u64,
        col: usize,
        nth: usize,
    },
}

pub(crate) struct Keyed {
    key: PlacementKey,
    placement: ComposedPlacement,
}

/// `pane`'s placements in the frame that shows it through `view`.
pub(crate) fn keyed(pane: PaneId, view: &PaneViewport, src: &Grid) -> Vec<Keyed> {
    let all = src.placements();
    let addressed = |q: &shux_vt::Placement| q.image_id != 0 && q.placement_id != 0;
    all.iter()
        .enumerate()
        .map(|(i, p)| {
            let key = if addressed(p) {
                PlacementKey::Addressed {
                    pane,
                    image: p.image_id,
                    placement: p.placement_id,
                }
            } else {
                let same = |q: &&shux_vt::Placement| {
                    !addressed(q) && (q.abs_row, q.col) == (p.abs_row, p.col)
                };
                PlacementKey::Anchored {
                    pane,
                    row: p.abs_row,
                    col: p.col,
                    nth: all[..i].iter().filter(same).count(),
                }
            };
            Keyed {
                key,
                placement: crate::composed::resolve_placement(view, src, p),
            }
        })
        .collect()
}

struct Live {
    key: PlacementKey,
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
    enabled: bool,
    live: Vec<Live>,
    next_id: u32,
    /// One of the client's cells in pixels, as its terminal reported it.
    cell_px: Option<(u32, u32)>,
}

impl KittyEmitter {
    /// Gate every emission on the client's own answer.
    pub(crate) fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    pub(crate) fn set_cell_pixels(&mut self, cell: Option<(u16, u16)>) {
        self.cell_px = cell
            .filter(|&(w, h)| w > 0 && h > 0)
            .map(|(w, h)| (u32::from(w), u32::from(h)));
    }

    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    /// Emit whatever this frame changed, and report whether it wrote anything.
    /// `replace_all` re-issues every placement even when nothing moved, for a
    /// frame that repainted the whole screen: the host may have dropped its
    /// placements. It costs an `a=p` each and never re-sends pixels.
    pub(crate) fn emit(
        &mut self,
        out: &mut impl Write,
        placements: &[Keyed],
        replace_all: bool,
    ) -> io::Result<bool> {
        let mut spent = 0usize;
        let want: Vec<(&Keyed, Placed)> = placements
            .iter()
            .filter_map(|k| resolve(&k.placement, self.cell_px).map(|r| (k, r)))
            // Charged whether or not this frame re-transmits it: any resolved
            // placement can be sent whole on any frame, and that frame is the
            // one that has to fit.
            .filter(|(k, _)| {
                let p = &k.placement;
                let cost = p.image.payload.len().div_ceil(3) * 4;
                if spent + cost > FRAME_IMAGE_BUDGET {
                    return false;
                }
                spent += cost;
                true
            })
            .collect();
        if want.is_empty() && self.live.is_empty() {
            return Ok(false);
        }
        let mut wrote = false;
        let mut was = std::mem::take(&mut self.live);
        // What the host already holds for each wanted placement: its own key's
        // with the same pixels, else, once every key has claimed its own, any
        // left over with the same pixels, as when a picture moves under no
        // placement id or the one stacked before it goes.
        let same = |l: &Live, k: &Keyed| Arc::ptr_eq(&l.image, &k.placement.image);
        let mut held: Vec<Option<Live>> = want
            .iter()
            .map(|(k, _)| {
                let i = was.iter().position(|l| l.key == k.key && same(l, k))?;
                Some(was.swap_remove(i))
            })
            .collect();
        for ((k, _), held) in want.iter().zip(&mut held) {
            if held.is_none()
                && let Some(i) = was.iter().position(|l| same(l, k))
            {
                *held = Some(was.swap_remove(i));
            }
        }
        for ((k, placed), held) in want.into_iter().zip(held) {
            match held {
                Some(mut live) => {
                    if live.placed != placed || replace_all {
                        // Same pixels, new geometry. Re-putting under the same
                        // image AND placement id replaces the placement in
                        // place, which is the protocol's own answer to flicker.
                        cup(out, &placed)?;
                        write!(out, "\x1b_Ga=p,i={},{}\x1b\\", live.host_id, keys(&placed))?;
                        live.placed = placed;
                        wrote = true;
                    }
                    live.key = k.key;
                    self.live.push(live);
                }
                None => {
                    let id = self.take_id();
                    transmit(out, id, &k.placement.image, &placed)?;
                    self.live.push(Live {
                        key: k.key,
                        image: k.placement.image.clone(),
                        host_id: id,
                        placed,
                    });
                    wrote = true;
                }
            }
        }

        // Retired only now, every replacement already up.
        for gone in was {
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
/// inside. The visible part, in drawn pixels at the declared cell size, maps
/// back through the placement's scale to the source rectangle sent.
fn resolve(p: &ComposedPlacement, cell_px: Option<(u32, u32)>) -> Option<Placed> {
    // An empty payload is placeable, and transmitting it would record a host id
    // the terminal never heard of. Judged here, so it never enters `want`.
    if p.image.payload.is_empty() {
        return None;
    }
    let (cw, ch) = DECLARED_CELL_PIXELS;
    let (clip_top, clip_left) = (p.clip.row as i64, p.clip.col as i64);
    // The cells the fitted picture covers, not the box it was fitted into:
    // with both `c` and `r` kitty 0.32 stretches where the protocol now
    // letterboxes, and a box of the picture's own shape is drawn alike by both.
    let (dest_w, dest_h) = p.geometry.drawn_px(cw, ch);
    let (nat_cols, nat_rows) = (
        i64::from(dest_w.div_ceil(cw).max(1)),
        i64::from(dest_h.div_ceil(ch).max(1)),
    );

    let top = p.row.max(clip_top);
    let bottom = p
        .row
        .saturating_add(nat_rows)
        .min(clip_top + p.clip.rows as i64);
    let left = p.col.max(clip_left);
    let right = p
        .col
        .saturating_add(nat_cols)
        .min(clip_left + p.clip.cols as i64);
    if bottom <= top || right <= left {
        return None;
    }

    // The source rows and columns the visible cells show, in drawn pixels at
    // the declared cell size. The end rounds out: the host stretches the
    // source to the last visible cell's edge. The start is the first whole
    // source pixel, `X=`/`Y=` pixels into its cell.
    let (dest_w, dest_h) = (u64::from(dest_w), u64::from(dest_h));
    let (cw, ch) = (u64::from(cw), u64::from(ch));
    let vy0 = (top - p.row) as u64 * ch;
    let vx0 = (left - p.col) as u64 * cw;
    let vy1 = vy0 + (bottom - top) as u64 * ch;
    let vx1 = vx0 + (right - left) as u64 * cw;
    let crop = p.geometry.crop;
    let (real_w, real_h) = (cell_px.map(|c| c.0), cell_px.map(|c| c.1));
    let (sy, ey, skip_y, off_y) = place(crop.h, dest_h, vy0, vy1, ch, real_h)?;
    let (sx, ex, skip_x, off_x) = place(crop.w, dest_w, vx0, vx1, cw, real_w)?;
    let (top, left) = (top + skip_y as i64, left + skip_x as i64);
    if bottom <= top || right <= left {
        return None;
    }
    Some(Placed {
        row: u16::try_from(top).ok()?,
        col: u16::try_from(left).ok()?,
        src: (crop.x + sx, crop.y + sy, ex - sx, ey - sy),
        cells: (
            u16::try_from(right - left).ok()?,
            u16::try_from(bottom - top).ok()?,
        ),
        offset: (off_x, off_y),
    })
}

/// One axis of [`resolve`]: the source pixels `first..end` shown, and where
/// the first begins, in whole declared cells and then pixels of the client's
/// own `real` cell, as the host applies `X=`/`Y=`. Not knowing the client's
/// cell, a pixel that begins partway into one cannot be placed truly: the
/// first later pixel that begins on a cell edge is used, or none.
fn place(
    extent: u32,
    dest: u64,
    v0: u64,
    v1: u64,
    cell: u64,
    real: Option<u32>,
) -> Option<(u32, u32, u64, u64)> {
    let (first, end, _) = shux_vt::visible_span(extent, dest, v0, v1)?;
    let e = u64::from(extent);
    // Where pixel `j` begins past `v0`, in 1/e of a declared pixel.
    let lead = |j: u32| (u64::from(j) * dest).saturating_sub(v0 * e);
    let whole = cell * e;
    let at = |j: u32| (j, end, lead(j) / whole);
    match real {
        Some(real) => {
            let (j, end, cells) = at(first);
            Some((j, end, cells, (lead(j) % whole) * u64::from(real) / whole))
        }
        None => {
            let j = (first..end).find(|&j| lead(j) % whole == 0)?;
            let (j, end, cells) = at(j);
            Some((j, end, cells, 0))
        }
    }
}

/// `C=1` so display never moves the host's cursor: kitty otherwise advances it
/// and scrolls the whole screen for an image at the bottom margin. `q=2` so the
/// host's `OK` never lands in the client's key decoder.
///
/// `z=-1` puts the picture UNDER text: the client writes copy mode, the help
/// sheet and the welcome toast into the frame after the compositor has emitted
/// images, and at the default z=0 kitty draws a placement above them. Negative
/// z still draws above the cell BACKGROUND, so a picture is not hidden by the
/// blank cells it covers.
fn keys(p: &Placed) -> String {
    let (x, y, w, h) = p.src;
    let (c, r) = p.cells;
    let x = if x == 0 {
        String::new()
    } else {
        format!("x={x},")
    };
    let (dx, dy) = p.offset;
    let dx = if dx == 0 {
        String::new()
    } else {
        format!(",X={dx}")
    };
    let dy = if dy == 0 {
        String::new()
    } else {
        format!(",Y={dy}")
    };
    format!("p=1,C=1,q=2,z=-1,{x}y={y},w={w},h={h},c={c},r={r}{dx}{dy}")
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
    let chunks = bytes.len().div_ceil(CHUNK);
    cup(out, placed)?;
    for (i, chunk) in bytes.chunks(CHUNK).enumerate() {
        let more = u8::from(i + 1 < chunks);
        if i == 0 {
            write!(
                out,
                "\x1b_Ga=T,f={fmt}{zlib},t=d,i={id},s={},v={},{},m={more};",
                img.width,
                img.height,
                keys(placed)
            )?;
        } else {
            write!(out, "\x1b_Gq=2,m={more};")?;
        }
        out.write_all(chunk)?;
        out.write_all(b"\x1b\\")?;
    }
    Ok(())
}

/// `d=I` frees the pixels as well as the placement. Every id retired here has
/// already been replaced, so nothing wants them back.
fn delete(out: &mut impl Write, id: u32) -> io::Result<()> {
    write!(out, "\x1b_Ga=d,d=I,i={id},q=2\x1b\\")
}
