//! Lazy freezing of the presented frame under synchronized output (issue #115).
//!
//! `CSI ?2026h` ("synchronized output", the sequence `vim`, `nvim`, `lazygit`
//! and `btop` wrap their redraws in) asks the terminal to keep showing the
//! frame it is already showing until `CSI ?2026l` arrives, so a half-drawn
//! screen is never seen. shux honours that by keeping a frozen copy of the
//! presented frame while the mode is held open.
//!
//! Taking that copy the instant `?2026h` arrives is what issue #115 is about:
//! sixteen bytes a pane chooses to emit bought a full grid copy — scrollback
//! included — inside the daemon-wide pane-IO lock, whether or not the pane
//! then drew anything at all.
//!
//! ## What is deferred, and why it is invisible
//!
//! Nothing about the freeze is observable until something would actually
//! change the presented frame. Between `?2026h` and the first such change the
//! frozen frame and the live frame are the same frame, so shux simply shows
//! the live one and copies nothing. The copy is taken by the write itself, out
//! of the state as it stood before the write — which is the state at `?2026h`,
//! because by construction nothing changed in between. `?2026l` on a frame
//! that was never written to throws away a freeze that was never taken.
//!
//! ## Why this is not a hook that can be forgotten
//!
//! The dangerous version of this optimisation is a hand-maintained list of
//! "places that mutate the presented frame": miss one and a synchronized
//! redraw tears in every rich TUI, silently, on some path nobody tested.
//!
//! So it is not a list. Each component of the presented frame — the grid, the
//! cursor, the window title, the dynamic default colours — is handed to the
//! parser wrapped in [`Presented`], which hands out a shared reference for
//! free and takes the snapshot on the way to handing out a mutable one. The
//! parser's existing code is unchanged: `self.grid.rows()` still borrows
//! immutably and still costs nothing, `self.grid.set_cell(..)` still borrows
//! mutably and now freezes first. A future mutation path cannot forget to
//! freeze, because there is no way to reach the mutable state except through
//! the freeze.
//!
//! It is also precise in the other direction, which coarser hooks are not: a
//! sequence that parses but changes nothing presented — a cursor-position
//! query, a mouse-mode toggle, an unhandled private mode — never reaches
//! `DerefMut`, so it cannot be used to re-arm the copy.
//!
//! The one component that does not live behind a `Presented` is the
//! alternate-screen flag, because it sits inside `TerminalModes` next to a
//! dozen fields that are not part of the presented frame (including
//! synchronized output itself, which `?2026h` sets — wrapping the whole struct
//! would freeze on the sequence that arms the freeze). It is frozen explicitly
//! by `VtHandler::set_alternate_screen`, the only writer, and the differential
//! oracle drives alternate-screen toggles inside synchronized-output windows
//! precisely so that a regression there fails a test rather than a user's
//! screen.

use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::cell::TerminalDefaultColors;
use crate::cursor::Cursor;
use crate::grid::Grid;

/// The frozen screen: the viewport as it stood when the window opened, plus
/// what it takes to keep reading history behind it.
///
/// The two extra numbers are not optional bookkeeping kept alongside the grid;
/// they are the only reason the grid can afford to be viewport-sized. History
/// is read live out of the writable grid, and the live grid's history moves
/// under the frozen frame as lines fall off the front of the scrollback — so
/// the frame has to remember how much history stood behind it and how far that
/// history has shifted since. Keeping all three in one value is what stops a
/// future edit from filling in the grid and forgetting the offsets.
pub(crate) struct FrozenScreen {
    pub(crate) grid: Grid,
    /// Lines of history behind the frame at freeze time.
    pub(crate) history_len: usize,
    /// The live grid's eviction counter at freeze time.
    pub(crate) evicted: u64,
}

/// A component of the presented frame, and how it is snapshotted.
///
/// Only the grid needs anything but a plain clone of itself. Its snapshot
/// carries the not-yet-repainted rows across, because from the moment it is
/// taken it is what a renderer draws from — and it holds the VIEWPORT only,
/// because history is not part of the presented frame and holding it is what
/// made freezing expensive in the first place (see
/// [`Grid::clone_presented_viewport`]).
pub(crate) trait PresentedFrame {
    type Frozen;
    fn snapshot(&self) -> Self::Frozen;
}

impl PresentedFrame for Grid {
    type Frozen = FrozenScreen;
    fn snapshot(&self) -> FrozenScreen {
        FrozenScreen {
            grid: self.clone_presented_viewport(),
            history_len: self.scrollback_len(),
            evicted: self.evicted(),
        }
    }
}
impl PresentedFrame for Cursor {
    type Frozen = Cursor;
    fn snapshot(&self) -> Cursor {
        self.clone()
    }
}
impl PresentedFrame for Option<String> {
    type Frozen = Option<String>;
    fn snapshot(&self) -> Option<String> {
        self.clone()
    }
}
impl PresentedFrame for TerminalDefaultColors {
    type Frozen = TerminalDefaultColors;
    fn snapshot(&self) -> TerminalDefaultColors {
        *self
    }
}

/// One component of the presented frame: the live value, the slot its frozen
/// copy goes in, and the shared flag saying whether synchronized output is
/// currently holding the presentation open.
///
/// The flag is an `AtomicBool` rather than a `Cell` only so that
/// `VirtualTerminal` stays `Sync` — the daemon shares terminals across tasks.
/// Every access is `Relaxed`: all five wrappers and the parser live on one
/// thread, inside one `&mut self`, so there is nothing to order against.
///
/// `Deref` reads the live value (the parser always parses against live state).
/// `DerefMut` snapshots first. Readers of the *presented* frame go to the
/// frozen slot when it is filled and to the live value when it is not — the
/// two are identical until the first write, which is the whole point.
pub(crate) struct Presented<'a, T: PresentedFrame> {
    live: &'a mut T,
    frozen: &'a mut Option<T::Frozen>,
    armed: &'a AtomicBool,
}

impl<'a, T: PresentedFrame> Presented<'a, T> {
    pub(crate) fn new(
        live: &'a mut T,
        frozen: &'a mut Option<T::Frozen>,
        armed: &'a AtomicBool,
    ) -> Self {
        Presented {
            live,
            frozen,
            armed,
        }
    }

    /// Take the snapshot now, if synchronized output is armed and this
    /// component has not been snapshotted yet.
    #[inline]
    pub(crate) fn freeze(&mut self) {
        if self.armed.load(Ordering::Relaxed) && self.frozen.is_none() {
            *self.frozen = Some(self.live.snapshot());
        }
    }

    /// Drop any snapshot: the presented frame is the live frame again.
    #[inline]
    pub(crate) fn discard(&mut self) {
        *self.frozen = None;
    }

    /// Mutable access that deliberately does NOT freeze.
    ///
    /// For the two operations that are not a presented-frame change at all:
    /// releasing the mode (the caller has already disarmed, so there is
    /// nothing left to protect) and marking the live buffer for repaint on the
    /// way out.
    #[inline]
    pub(crate) fn live_mut_unfrozen(&mut self) -> &mut T {
        self.live
    }
}

impl<T: PresentedFrame> Deref for Presented<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        self.live
    }
}

impl<T: PresentedFrame> DerefMut for Presented<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        self.freeze();
        self.live
    }
}
