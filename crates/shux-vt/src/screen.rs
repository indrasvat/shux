//! Alternate-screen buffer swapping.
//!
//! Two callers need this: the VT parser (`DECSET 1047`/`1049`, and `RIS`,
//! which must return to the primary buffer before it resets anything) and
//! `VirtualTerminal`'s direct enter/leave API. They used to carry a copy each.
//! One of the copies is now the only one, because the reuse rule below is only
//! sound if every path that retires an alternate buffer actually hands it back.
//!
//! ## The reuse rule (issue #106)
//!
//! Entering the alternate screen used to build a fresh grid every time, and
//! leaving it used to drop that grid on the floor. A pane emitting nothing but
//! `ESC[?1049h ESC[?1049l` therefore bought a full-grid allocate-zero-free
//! cycle for every eight bytes it wrote — 372 KB per toggle on a 240x64 pane —
//! and it bought it inside the daemon-wide pane-IO mutex, so the bill went to
//! every other pane in every other session.
//!
//! Leaving now parks the retired buffer in a single spare slot and entering
//! takes it back. The pane keeps exactly two screen-sized buffers, which is
//! what it had while the alternate screen was live anyway.

use crate::cursor::Cursor;
use crate::grid::{Grid, GridConfig};

/// The alternate screen never has scrollback: a fullscreen application owns
/// the viewport and repaints it, and history belongs to the primary buffer.
pub(crate) fn alt_grid_config() -> GridConfig {
    GridConfig {
        max_scrollback: 0,
        ..GridConfig::default()
    }
}

/// Every piece of state the alternate-screen swap touches, borrowed together
/// so the parser path and the `VirtualTerminal` path cannot drift apart.
pub(crate) struct ScreenSwap<'a> {
    /// The buffer currently being drawn into and rendered.
    pub grid: &'a mut Grid,
    pub cursor: &'a mut Cursor,
    /// The buffer that is NOT live: the primary one, while the alternate
    /// screen is showing.
    pub stashed_grid: &'a mut Option<Grid>,
    pub stashed_cursor: &'a mut Option<Cursor>,
    /// The retired alternate-screen buffer, held for the next entry.
    pub spare: &'a mut Option<Grid>,
    /// Whether to recycle at all. Production is always `true`; the
    /// differential tests run a second terminal with it `false` so the
    /// recycling path can be compared against the allocate-every-time one.
    pub reuse: bool,
}

impl ScreenSwap<'_> {
    /// Switch to the alternate screen. The caller has already checked that the
    /// primary screen is live.
    ///
    /// `save_cursor` distinguishes the two mode numbers: `1049` parks the
    /// primary cursor and starts the alternate screen at home, `1047` carries
    /// the cursor across untouched.
    pub(crate) fn enter(&mut self, save_cursor: bool) {
        let rows = self.grid.rows();
        let cols = self.grid.cols();
        let config = alt_grid_config();

        let mut alt = match self.spare.take().filter(|_| self.reuse) {
            // Nothing was drawn on the retired buffer, and the geometry has
            // not moved: it is already the blank canvas a fresh grid would be.
            // This is the toggle-without-drawing case, and it is the one an
            // abusive pane can drive at speed.
            Some(spare) if spare.is_blank_canvas(rows, cols, &config) => spare,
            // Drawn on, or the pane was resized while it sat in the slot.
            // Blank it in place — same cell writes, no allocator traffic.
            Some(mut spare) => {
                spare.reset_blank(rows, cols, config);
                spare
            }
            None => Grid::new(rows, cols, config),
        };
        alt.mark_all_dirty();

        *self.stashed_grid = Some(std::mem::replace(self.grid, alt));
        if save_cursor {
            *self.stashed_cursor = Some(std::mem::take(self.cursor));
        } else {
            // 1047 keeps the cursor where it is, and leaves nothing to restore.
            self.stashed_cursor.take();
        }
    }

    /// Switch back to the primary screen. Inert if the primary screen is
    /// already live.
    ///
    /// `restore_cursor` is the `1047`/`1049` distinction again: only `1049`
    /// parked a cursor to put back.
    pub(crate) fn leave(&mut self, restore_cursor: bool) {
        if let Some(primary) = self.stashed_grid.take() {
            let retired = std::mem::replace(self.grid, primary);
            self.grid.mark_all_dirty();
            if self.reuse {
                // One slot, overwritten rather than appended to: the pane
                // holds at most one retired buffer however often it toggles.
                *self.spare = Some(retired);
            }
        }
        if restore_cursor {
            if let Some(primary_cursor) = self.stashed_cursor.take() {
                *self.cursor = primary_cursor;
            }
        } else {
            self.stashed_cursor.take();
        }
    }
}
