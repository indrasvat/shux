//! Red suite — ContentRevision substrate (§4 SPEC-A; tests G3, G4 from §12).
//!
//! FROZEN after P0 (§16.2). These P1 tests read `content_revision` ONLY via
//! `session.snapshot` (LENS-R-006) — no glance, no settle. In Phase P0 the
//! snapshot carries no pane entries, so `content_revision(...)` panics with the
//! "LENS-R-006 not implemented" root cause — the red receipt.

mod lens_common;
use lens_common::*;

/// F8 paints `FRAME:<glyph>` where glyph cycles 0-9A-Z; the i-th token paints
/// the i-th glyph.
fn frame_marker(i: usize) -> String {
    let glyphs = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    format!("FRAME:{}", glyphs[i % glyphs.len()] as char)
}

// G3 ⇄ — revision monotonicity (no glance, no settle).
#[test]
fn g3_revision_monotonicity() {
    let h = Harness::new();
    let f = h.launch_fixture("f8_repaint.sh", 80, 24, "LENS-F8-REPAINT");

    // First read is the P0 red-receipt failure (missing content_revision field).
    let mut prev = h.content_revision(&f.session_id, &f.pane_id);
    for i in 0..3 {
        h.send_line_token(&f.pane_id, "");
        h.wait_for(&f.pane_id, &frame_marker(i), 5_000)
            .unwrap_or_else(|e| panic!("G3: repaint {i} never landed: {e}"));
        let now = h.content_revision(&f.session_id, &f.pane_id);
        assert!(
            now > prev,
            "G3: revision must strictly increase ({prev} -> {now})"
        );
        prev = now;
    }

    // Two idle reads (no tokens) return the same revision.
    let a = h.content_revision(&f.session_id, &f.pane_id);
    let b = h.content_revision(&f.session_id, &f.pane_id);
    assert_eq!(a, b, "G3: idle snapshots must report an identical revision");

    h.kill_session(&f.session_id);
}

// G4 — the graph-version trap (council D3).
#[test]
fn g4_revision_is_not_graph_version() {
    let h = Harness::new();
    let f = h.launch_fixture("f8_repaint.sh", 80, 24, "LENS-F8-REPAINT");

    let rev_first = h.content_revision(&f.session_id, &f.pane_id);
    let ver_first = h.snapshot_pane_structural_version(&f.session_id, &f.pane_id);

    // Five pure repaints — no splits/renames/resizes.
    for i in 0..5 {
        h.send_line_token(&f.pane_id, "");
        h.wait_for(&f.pane_id, &frame_marker(i), 5_000)
            .unwrap_or_else(|e| panic!("G4: repaint {i} never landed: {e}"));
    }

    let rev_last = h.content_revision(&f.session_id, &f.pane_id);
    let ver_last = h.snapshot_pane_structural_version(&f.session_id, &f.pane_id);

    assert!(
        rev_last >= rev_first + 5,
        "G4: content_revision must climb by >=5 over five repaints ({rev_first} -> {rev_last})"
    );
    assert_eq!(
        ver_first, ver_last,
        "G4: structural version must NOT move on a pure repaint — revision is not graph version"
    );

    h.kill_session(&f.session_id);
}
