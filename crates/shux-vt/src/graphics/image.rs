//! Chunk assembly for an incoming kitty image.
//!
//! Payloads are held base64-decoded but otherwise as they arrived: still zlib-
//! deflated when `o=z`, still PNG when `f=100`. `shux-raster` decodes, so no
//! decompressor runs inside the lock `process_with_responses` holds.

use base64::Engine as _;

use super::kitty::Format;

/// Bytes one image may occupy, and the ceiling on what a pane may hold in
/// placements (`Grid::place`).
pub(crate) const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;

/// An image as it arrived, plus what the client claimed about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredImage {
    /// base64-decoded; still deflated and/or PNG-encoded.
    pub payload: Vec<u8>,
    pub format: Format,
    pub compressed: bool,
    /// The client's claim, not a measurement. `shux-raster` rejects a payload
    /// that decodes to anything else.
    pub width: u32,
    pub height: u32,
}

/// Accumulates one chunked transmission (`m=1` … `m=0`).
#[derive(Debug, Default, Clone)]
pub struct Assembler {
    open: Option<StoredImage>,
}

impl Assembler {
    /// Feed one command's payload, returning the image when it completes.
    ///
    /// An open transfer continues whatever the new command says, which is
    /// kitty's rule (`graphics.c:838`) and the only workable one: real
    /// `kitten icat` repeats `a=T` on every continuation chunk. `abort` is how
    /// a transfer ends early.
    pub(crate) fn feed(
        &mut self,
        cmd: &super::kitty::Command,
        payload: &[u8],
    ) -> Option<StoredImage> {
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(payload) else {
            self.open = None;
            return None;
        };
        if self.open.is_none() {
            self.open = Some(StoredImage {
                payload: Vec::new(),
                format: cmd.format,
                compressed: cmd.compressed,
                width: cmd.width,
                height: cmd.height,
            });
        }
        let open = self.open.as_mut()?;
        if open.payload.len() + bytes.len() > MAX_IMAGE_BYTES {
            self.open = None;
            return None;
        }
        open.payload.extend_from_slice(&bytes);
        if cmd.more {
            return None;
        }
        let done = self.open.take()?;
        // A raw payload's length is derivable from the declared size, so a
        // truncated one is knowable here -- without a decoder, which stays out
        // of this crate. Placing it would spend a slot and move the cursor for
        // something that can never be drawn. A PNG's length is not derivable;
        // `shux-raster` refuses that one when it decodes.
        let bpp = match done.format {
            Format::Rgba32 => 4u64,
            Format::Rgb24 => 3,
            Format::Png => return Some(done),
        };
        if done.compressed {
            return Some(done); // still deflated; length means nothing yet
        }
        let want = u64::from(done.width)
            .checked_mul(u64::from(done.height))?
            .checked_mul(bpp)?;
        (done.payload.len() as u64 == want).then_some(done)
    }

    pub(crate) fn abort(&mut self) {
        self.open = None;
    }
}
