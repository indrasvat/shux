//! Alternate-screen state-machine correctness (issue #106).
//!
//! Reusing the retired alternate-screen buffer is only safe if every path that
//! leaves the alternate screen actually goes through the swap. These tests pin
//! the observable behaviour of each of them, independently of how the buffers
//! are allocated.

use shux_vt::VirtualTerminal;

const ROWS: usize = 8;
const COLS: usize = 24;

fn vt() -> VirtualTerminal {
    VirtualTerminal::new(ROWS, COLS)
}

fn screen(vt: &VirtualTerminal) -> String {
    vt.capture_text(None).trim_end().to_string()
}

// ---------------------------------------------------------------------------
// RIS (ESC c)
// ---------------------------------------------------------------------------

/// RIS resets the terminal to its initial state, and the initial state is the
/// PRIMARY screen. Clearing the alternate buffer and merely lowering the mode
/// flag leaves the pane showing the alternate buffer while reporting the
/// primary one.
#[test]
fn ris_on_the_alt_screen_returns_to_the_primary_buffer() {
    let mut t = vt();
    t.process(b"PRIMARY");
    t.process(b"\x1b[?1049h");
    t.process(b"ALTERNATE");
    t.process(b"\x1bc");

    assert!(
        !t.is_alternate_screen(),
        "RIS must leave the alternate screen"
    );
    assert_eq!(screen(&t), "", "RIS must clear the screen it returns to");

    // The primary buffer is the live one now, so a later 1049l has nothing to
    // restore and must not resurrect the alternate buffer's contents.
    t.process(b"\x1b[?1049l");
    assert_eq!(screen(&t), "");
    assert!(!t.is_alternate_screen());
}

/// RIS must clear the scrollback of the buffer it resets to. While the
/// alternate screen was live, `clear_scrollback` hit the alternate buffer —
/// which never has any — and the pane's real history survived a full reset.
#[test]
fn ris_on_the_alt_screen_clears_the_primary_scrollback() {
    let mut t = vt();
    for i in 0..40 {
        t.process(format!("line{i}\r\n").as_bytes());
    }
    assert!(t.scrollback_len() > 0, "test setup produced no scrollback");

    t.process(b"\x1b[?1049h");
    t.process(b"\x1bc");

    assert_eq!(
        t.scrollback_len(),
        0,
        "RIS left the primary scrollback intact behind the alternate screen"
    );
}

/// The sharp end of the same defect. The alternate buffer is built with no
/// scrollback budget, on purpose. If RIS resets the terminal without first
/// swapping back, that zero-scrollback buffer BECOMES the pane's primary
/// buffer — and the pane silently loses scrollback for the rest of its life.
/// `reset(1)` and a crashed full-screen app both send exactly this.
#[test]
fn scrollback_still_accumulates_after_ris_on_the_alt_screen() {
    let mut reset_on_alt = vt();
    reset_on_alt.process(b"\x1b[?1049h");
    reset_on_alt.process(b"\x1bc");

    let mut reset_on_primary = vt();
    reset_on_primary.process(b"\x1bc");

    for i in 0..100 {
        let line = format!("after-reset-{i}\r\n");
        reset_on_alt.process(line.as_bytes());
        reset_on_primary.process(line.as_bytes());
    }

    assert_eq!(
        reset_on_alt.scrollback_len(),
        reset_on_primary.scrollback_len(),
        "a pane that reset while on the alternate screen kept the alternate \
         buffer's zero-scrollback budget as its primary buffer"
    );
    assert!(reset_on_alt.scrollback_len() > 0);
}

/// ...and it must not merely be deferred: a later alternate-screen round trip
/// used to re-stash the crippled buffer and restore it as "primary" again.
#[test]
fn scrollback_survives_ris_on_alt_followed_by_an_alt_round_trip() {
    let mut t = vt();
    t.process(b"\x1b[?1049h");
    t.process(b"\x1bc");
    t.process(b"\x1b[?1049h");
    t.process(b"\x1b[?1049l");

    for i in 0..100 {
        t.process(format!("later-{i}\r\n").as_bytes());
    }
    assert!(
        t.scrollback_len() > 0,
        "scrollback stayed dead after RIS-on-alt plus a full alternate-screen cycle"
    );
}

/// After RIS the alternate buffer is reachable again through an ordinary
/// enter/leave cycle, and it starts blank.
#[test]
fn alt_screen_still_works_after_ris() {
    let mut t = vt();
    t.process(b"\x1b[?1049h");
    t.process(b"STALE ALT");
    t.process(b"\x1bc");

    t.process(b"PRIMARY AGAIN");
    t.process(b"\x1b[?1049h");
    assert_eq!(
        screen(&t),
        "",
        "the alternate screen must start blank, not show the pre-RIS contents"
    );

    t.process(b"\x1b[?1049l");
    assert_eq!(screen(&t), "PRIMARY AGAIN");
}

// ---------------------------------------------------------------------------
// Ordinary enter/leave, exercised repeatedly so buffer reuse is in play
// ---------------------------------------------------------------------------

/// The alternate screen is blank on every entry, not just the first — a reused
/// buffer that kept the previous session's pixels would leak one application's
/// screen into the next.
#[test]
fn every_alt_screen_entry_starts_blank() {
    let mut t = vt();
    t.process(b"PRIMARY");
    for round in 0..8 {
        t.process(b"\x1b[?1049h");
        assert_eq!(
            screen(&t),
            "",
            "alternate screen was not blank on entry {round}"
        );
        t.process(format!("ROUND{round}").as_bytes());
        t.process(b"\x1b[?1049l");
        assert_eq!(
            screen(&t),
            "PRIMARY",
            "primary buffer was damaged by round {round}"
        );
    }
}

/// Same, for 1047 — which carries the cursor into the alternate screen instead
/// of resetting it, and so exercises a different branch of the swap.
#[test]
fn every_1047_entry_starts_blank() {
    let mut t = vt();
    t.process(b"PRIMARY");
    for round in 0..8 {
        t.process(b"\x1b[?1047h");
        assert_eq!(screen(&t), "", "1047 alt screen not blank on entry {round}");
        t.process(b"\x1b[HALT");
        t.process(b"\x1b[?1047l");
        assert_eq!(screen(&t), "PRIMARY", "1047 round {round} damaged primary");
    }
}

/// The alternate screen has no scrollback, however many times it is re-entered:
/// a reused buffer must not inherit the primary buffer's scrollback budget.
#[test]
fn alt_screen_never_accumulates_scrollback() {
    let mut t = vt();
    for round in 0..6 {
        t.process(b"\x1b[?1049h");
        for i in 0..(ROWS * 4) {
            t.process(format!("alt{round}-{i}\r\n").as_bytes());
        }
        assert_eq!(
            t.scrollback_len(),
            0,
            "alternate screen accumulated scrollback on entry {round}"
        );
        t.process(b"\x1b[?1049l");
    }
}

/// The primary buffer's scrollback survives a trip through the alternate
/// screen — this is what makes `less`/`vim` feel right, and a swap that
/// rebuilds the wrong buffer would silently drop it.
#[test]
fn primary_scrollback_survives_alt_screen_round_trips() {
    let mut t = vt();
    for i in 0..60 {
        t.process(format!("history{i}\r\n").as_bytes());
    }
    let before = t.scrollback_len();
    assert!(before > 0);

    for _ in 0..8 {
        t.process(b"\x1b[?1049h");
        t.process(b"\x1b[2Jfullscreen app");
        t.process(b"\x1b[?1049l");
    }

    assert_eq!(
        t.scrollback_len(),
        before,
        "primary scrollback changed across alternate-screen round trips"
    );
}

/// Resizing while the alternate screen is live, then leaving, must land on a
/// correctly-sized primary buffer — and a subsequent re-entry must produce an
/// alternate buffer at the NEW size, not the size the retired one had.
#[test]
fn resize_across_the_swap_yields_correctly_sized_buffers() {
    let mut t = vt();
    t.process(b"PRIMARY");

    t.process(b"\x1b[?1049h");
    t.resize(20, 60);
    t.process(b"\x1b[?1049l");
    assert_eq!((t.grid().rows(), t.grid().cols()), (20, 60));

    t.process(b"\x1b[?1049h");
    assert_eq!(
        (t.grid().rows(), t.grid().cols()),
        (20, 60),
        "re-entered alternate screen kept a stale geometry"
    );
    // Every cell of the new geometry must be addressable.
    t.process(b"\x1b[20;60Hz");
    assert_eq!(t.grid().visible_row(19)[59].ch, 'z');

    t.resize(6, 12);
    t.process(b"\x1b[?1049l");
    t.process(b"\x1b[?1049h");
    assert_eq!((t.grid().rows(), t.grid().cols()), (6, 12));
    t.process(b"\x1b[6;12Hy");
    assert_eq!(t.grid().visible_row(5)[11].ch, 'y');
}

/// A pane can leave a screen it never entered, repeatedly. Nothing should
/// swap, and nothing should be consumed.
#[test]
fn leaving_a_screen_never_entered_is_inert() {
    let mut t = vt();
    t.process(b"PRIMARY");
    for _ in 0..8 {
        t.process(b"\x1b[?1049l");
        t.process(b"\x1b[?1047l");
    }
    assert!(!t.is_alternate_screen());
    assert_eq!(screen(&t), "PRIMARY");
}

/// Re-entering the alternate screen while already on it clears it in place
/// (1049) or is inert (1047) — and in neither case may the stashed primary
/// buffer be replaced, which would strand it.
#[test]
fn repeated_enter_while_already_on_the_alt_screen_preserves_primary() {
    let mut t = vt();
    t.process(b"PRIMARY");
    t.process(b"\x1b[?1049h");
    t.process(b"ALT");
    for _ in 0..8 {
        t.process(b"\x1b[?1049h");
        assert_eq!(screen(&t), "", "1049h on the alt screen must clear it");
        t.process(b"ALT");
        t.process(b"\x1b[?1047h");
        assert_eq!(screen(&t), "ALT", "1047h on the alt screen must be inert");
    }
    t.process(b"\x1b[?1049l");
    assert_eq!(screen(&t), "PRIMARY");
}

/// Synchronized output freezes the presented frame. An alternate-screen switch
/// inside that window must not be visible until the window closes — the
/// presented alt flag, grid and cursor all come from the same frozen frame.
#[test]
fn alt_switch_inside_a_sync_window_stays_frozen() {
    let mut t = vt();
    t.process(b"PRIMARY");
    t.process(b"\x1b[?2026h");
    t.process(b"\x1b[?1049h");
    t.process(b"ALT");

    assert!(
        !t.is_alternate_screen(),
        "frozen frame leaked a future alt flag"
    );
    assert_eq!(
        screen(&t),
        "PRIMARY",
        "frozen frame leaked alternate pixels"
    );

    t.process(b"\x1b[?2026l");
    assert!(t.is_alternate_screen());
    assert_eq!(screen(&t), "ALT");
}
