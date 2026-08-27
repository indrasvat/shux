//! THROWAWAY SPIKE. Not for merge.
//!
//! Minimal kitty-graphics decode, to answer one question: can a picture reach
//! `pane snapshot`'s PNG? Key/value parse shape adapted from Zellij's
//! `kitty_graphics/parser.rs` (MIT).
//!
//! Handles only what terminal-browser actually sends: a=T, f=32, o=z, t=d,
//! chunked with m=1/m=0, placed at the cursor.

/// A decoded image and where it was placed, in cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpikeImage {
    pub image_id: u32,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// SPIKE FIX: absolute line number (evicted + scrollback_len + viewport row)
    /// at the moment of placement. Survives scrolling; translated to a viewport
    /// row by `clone_visible`.
    pub anchor: u64,
    /// Viewport row. Meaningful only on a `clone_visible` grid, where the
    /// anchor has been resolved against that viewport.
    pub row: usize,
    pub col: usize,
    /// `a=T` places immediately; `a=t` only stores. Zellij separates the two so
    /// a bitmap is transmitted once and re-placed cheaply thereafter.
    pub place_now: bool,
    /// Destination box in CELLS from `c=`/`r=`, so the receiver scales into its
    /// own cell geometry rather than assuming the sender's pixel size.
    pub dest_cells: Option<(u16, u16)>,
}

/// Chunk accumulator: kitty splits one image across many APCs.
#[derive(Debug, Default, Clone)]
pub struct SpikeAssembler {
    pending: Option<Pending>,
    place_now: bool,
    dest_cells: Option<(u16, u16)>,
}

#[derive(Debug, Clone)]
struct Pending {
    image_id: u32,
    width: u32,
    height: u32,
    compressed: bool,
    payload: Vec<u8>,
}

/// Spike bound: refuse anything that would decode past this.
const MAX_DECODED: usize = 64 * 1024 * 1024;

fn kv(control: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut out = Vec::new();
    for part in control.split(|b| *b == b',') {
        let mut it = part.splitn(2, |b| *b == b'=');
        let (Some(k), Some(v)) = (it.next(), it.next()) else {
            continue;
        };
        if k.len() == 1 {
            out.push((k[0], v.to_vec()));
        }
    }
    out
}

fn num(v: &[u8]) -> Option<u32> {
    std::str::from_utf8(v).ok()?.parse().ok()
}

impl SpikeAssembler {
    /// Feed one APC body's command half (everything after the leading `G`).
    /// Returns a decoded image once the final chunk lands.
    pub fn feed(&mut self, command: &[u8], anchor: u64, cursor_col: usize) -> Option<SpikeImage> {
        let (control, payload) = match command.iter().position(|b| *b == b';') {
            Some(i) => (&command[..i], &command[i + 1..]),
            None => (command, &b""[..]),
        };
        let pairs = kv(control);
        let get = |k: u8| pairs.iter().find(|(pk, _)| *pk == k).map(|(_, v)| v.as_slice());

        let more = get(b'm').and_then(num).unwrap_or(0) == 1;

        // A first chunk carries the geometry; continuations carry only m= and payload.
        if let Some(action) = get(b'a') {
            if action != b"T" && action != b"t" {
                return None; // spike: transmit (with or without immediate place)
            }
            self.place_now = action == b"T";
            self.dest_cells = match (get(b'c').and_then(num), get(b'r').and_then(num)) {
                (Some(c), Some(r)) => Some((c as u16, r as u16)),
                _ => None,
            };
            if get(b'f').and_then(num) != Some(32) {
                return None; // spike: RGBA only
            }
            if get(b't').map(|t| t != b"d").unwrap_or(false) {
                return None; // spike: direct transmission only
            }
            self.pending = Some(Pending {
                image_id: get(b'i').and_then(num).unwrap_or(0),
                width: get(b's').and_then(num)?,
                height: get(b'v').and_then(num)?,
                compressed: get(b'o') == Some(b"z"),
                payload: Vec::new(),
            });
        }

        let pending = self.pending.as_mut()?;
        pending.payload.extend_from_slice(payload);
        if pending.payload.len() > MAX_DECODED {
            self.pending = None;
            return None;
        }
        if more {
            return None;
        }

        let done = self.pending.take()?;
        let expect = (done.width as usize)
            .checked_mul(done.height as usize)?
            .checked_mul(4)?;
        if expect == 0 || expect > MAX_DECODED {
            return None;
        }

        let raw = base64_decode(&done.payload)?;
        let rgba = if done.compressed {
            inflate_exact(&raw, expect)?
        } else {
            (raw.len() == expect).then_some(raw)?
        };

        Some(SpikeImage {
            image_id: done.image_id,
            width: done.width,
            height: done.height,
            rgba,
            anchor,
            row: 0,
            col: cursor_col,
            place_now: self.place_now,
            dest_cells: self.dest_cells,
        })
    }
}

fn base64_decode(input: &[u8]) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(input).ok()
}

/// kitty's rule: inflation must land on EXACTLY the declared size.
fn inflate_exact(input: &[u8], expect: usize) -> Option<Vec<u8>> {
    let mut out = vec![0u8; expect];
    let mut d = flate2::Decompress::new(true);
    let status = d
        .decompress(input, &mut out, flate2::FlushDecompress::Finish)
        .ok()?;
    if status != flate2::Status::StreamEnd {
        return None;
    }
    (d.total_out() as usize == expect).then_some(out)
}
