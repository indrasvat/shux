//! Adversarial verification of the "include the store generation in the emit
//! signature" fix for `spike_emit_images`.
//!
//! Every test here drives the REAL path: VT store -> compositor emit -> VT
//! parse -> compare. No browser, no daemon.

use std::collections::HashMap;
use std::io::Cursor;

use shux_core::layout::{Direction, LayoutNode, WindowLayout};
use shux_core::model::PaneId;
use shux_ui::{BorderStyle, CompositorConfig, MultiPaneFrame, RenderCompositor, RenderStats};
use shux_vt::VirtualTerminal;
use uuid::Uuid;

const IW: u32 = 36;
const IH: u32 = 38;

fn pane(n: u128) -> PaneId {
    PaneId::from_uuid(Uuid::from_u128(n))
}

fn solid(rgb: [u8; 3]) -> Vec<u8> {
    let mut v = Vec::with_capacity((IW * IH * 4) as usize);
    for _ in 0..(IW * IH) {
        v.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
    }
    v
}

fn transmit(id: u32, iw: u32, ih: u32, rgba: &[u8]) -> Vec<u8> {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(rgba);
    let bytes = b64.as_bytes();
    let total = bytes.len().div_ceil(4096).max(1);
    let mut out = Vec::new();
    for (i, chunk) in bytes.chunks(4096).enumerate() {
        let more = u8::from(i + 1 < total);
        let payload = std::str::from_utf8(chunk).unwrap();
        if i == 0 {
            out.extend_from_slice(
                format!("\x1b_Ga=T,f=32,t=d,s={iw},v={ih},i={id},m={more};{payload}\x1b\\")
                    .as_bytes(),
            );
        } else {
            out.extend_from_slice(format!("\x1b_Gm={more};{payload}\x1b\\").as_bytes());
        }
    }
    out
}

struct Harness {
    comp: RenderCompositor<Cursor<Vec<u8>>>,
    seen: usize,
}

impl Harness {
    fn new(w: u16, h: u16) -> Self {
        let cfg = CompositorConfig {
            show_border: false,
            status_bar_height: 0,
            border_style: BorderStyle::None,
            ..Default::default()
        };
        Harness {
            comp: RenderCompositor::new(w, h, Cursor::new(Vec::new()), cfg),
            seen: 0,
        }
    }

    /// Render one frame; return the bytes the client wrote for it.
    fn frame(
        &mut self,
        layout: &LayoutNode,
        focused: PaneId,
        vts: &HashMap<PaneId, &VirtualTerminal>,
    ) -> Vec<u8> {
        let _: RenderStats = self
            .comp
            .render_multi_pane(MultiPaneFrame {
                layout,
                zoom: None,
                focused,
                vts,
                titles: None,
                status_bar: None,
            })
            .unwrap();
        let all = self.comp.inner_mut().get_ref().clone();
        let fresh = all[self.seen..].to_vec();
        self.seen = all.len();
        fresh
    }
}

fn one(p: PaneId, vt: &VirtualTerminal) -> HashMap<PaneId, &VirtualTerminal> {
    let mut m = HashMap::new();
    m.insert(p, vt);
    m
}

/// What colour does the outer terminal believe the image is?
fn outer_rgb(outer: &VirtualTerminal) -> Option<(u8, u8, u8)> {
    outer
        .grid()
        .spike_images
        .first()
        .map(|i| (i.rgba[0], i.rgba[1], i.rgba[2]))
}

// ── ATTACK 1: synchronized output (CSI ?2026h) ──────────────────────────
//
// `VirtualTerminal::grid()` returns the FROZEN grid while a sync window is
// open, and the frozen grid is built by `Grid::clone_presented_viewport`,
// which hardcodes `spike_image_gen: 0` (crates/shux-vt/src/grid.rs:562).
// Two consecutive renders that both land inside sync windows therefore both
// read generation 0.
#[test]
fn attack_sync_output_window_hides_a_repaint() {
    let p = pane(1);
    let layout = LayoutNode::leaf(p);
    let mut inner = VirtualTerminal::new(10, 20);
    let mut outer = VirtualTerminal::new(10, 20);
    let mut h = Harness::new(20, 10);

    // Window 1: RED is what the presented frame holds.
    inner.process(&transmit(1, IW, IH, &solid([255, 0, 0])));
    inner.process(b"\x1b[?2026h");
    // A write far below the image: it takes the freeze snapshot and dirties
    // only row 8. Home the cursor again so the NEXT transmit anchors at the
    // same row/col and the signature is genuinely identical.
    inner.process(b"\x1b[9;1Hx\x1b[H");
    let f1 = h.frame(&layout, p, &one(p, &inner));
    outer.process(&f1);
    eprintln!(
        "window 1: gen={} emitted={} outer={:?}",
        inner.grid().spike_image_gen,
        f1.len(),
        outer_rgb(&outer)
    );

    // Window 2: same id, same geometry, BLUE pixels.
    inner.process(b"\x1b[?2026l");
    inner.process(b"\x1b[H");
    inner.process(&transmit(1, IW, IH, &solid([0, 0, 255])));
    inner.process(b"\x1b[?2026h");
    inner.process(b"\x1b[9;1Hy\x1b[H");
    let f2 = h.frame(&layout, p, &one(p, &inner));
    outer.process(&f2);
    eprintln!(
        "window 2: gen={} emitted={} outer={:?}",
        inner.grid().spike_image_gen,
        f2.len(),
        outer_rgb(&outer)
    );

    assert_eq!(
        outer_rgb(&outer),
        Some((0, 0, 255)),
        "the repaint inside a ?2026 window never reached the outer terminal"
    );
}

// ── ATTACK 2: alternate-screen swap ─────────────────────────────────────
//
// Generations belong to the GRID, and `ScreenSwap` mem::replaces the grid.
// Leaving the alternate screen restores a grid whose generation can equal
// the last one emitted, with different pixels underneath.
#[test]
fn attack_alt_screen_swap_restores_a_colliding_generation() {
    let p = pane(1);
    let layout = LayoutNode::leaf(p);
    let mut inner = VirtualTerminal::new(10, 20);
    let mut outer = VirtualTerminal::new(10, 20);
    let mut h = Harness::new(20, 10);

    // Primary screen: RED. (No text anywhere, so no cell ever goes dirty and
    // the `overpainted` escape hatch cannot fire.)
    inner.process(&transmit(1, IW, IH, &solid([255, 0, 0])));
    let f = h.frame(&layout, p, &one(p, &inner));
    outer.process(&f);
    eprintln!("primary: gen={} outer={:?}", inner.grid().spike_image_gen, outer_rgb(&outer));

    // Into the alternate screen: fresh grid, generation back to 0.
    inner.process(b"\x1b[?1049h");
    let f = h.frame(&layout, p, &one(p, &inner));
    outer.process(&f);
    eprintln!("alt entered: gen={} outer={:?}", inner.grid().spike_image_gen, outer_rgb(&outer));

    // BLUE inside the alternate screen: alt generation reaches 1.
    inner.process(&transmit(1, IW, IH, &solid([0, 0, 255])));
    let f = h.frame(&layout, p, &one(p, &inner));
    outer.process(&f);
    eprintln!("alt painted: gen={} outer={:?}", inner.grid().spike_image_gen, outer_rgb(&outer));

    // Back to the primary screen: RED again, at generation 1 — the value
    // already emitted.
    inner.process(b"\x1b[?1049l");
    let f = h.frame(&layout, p, &one(p, &inner));
    outer.process(&f);
    eprintln!(
        "alt left: gen={} emitted={} outer={:?}",
        inner.grid().spike_image_gen,
        f.len(),
        outer_rgb(&outer)
    );

    assert_eq!(
        outer_rgb(&outer),
        Some((255, 0, 0)),
        "leaving the alternate screen left the outer terminal on the alt image"
    );
}

// ── ATTACK 3: the alternate-screen spare buffer resurrects images ───────
//
// `Grid::reset_blank` (grid.rs:645) blanks cells and clears the write tally
// but never touches `spike_images` / `spike_image_gen`, and
// `is_blank_canvas` (grid.rs:619) only consults the write tally — which
// images never bump. A retired alt buffer holding a picture is therefore
// recycled as "a blank canvas".
#[test]
fn attack_alt_spare_buffer_resurrects_a_dead_image() {
    let mut vt = VirtualTerminal::new(10, 20);
    vt.process(b"\x1b[?1049h");
    vt.process(&transmit(1, IW, IH, &solid([255, 0, 0])));
    assert_eq!(vt.grid().spike_images.len(), 1, "alt screen did not store it");
    let gen_in_alt = vt.grid().spike_image_gen;

    vt.process(b"\x1b[?1049l");
    assert!(vt.grid().spike_images.is_empty(), "primary should have no image");

    vt.process(b"\x1b[?1049h");
    eprintln!(
        "re-entered alt: images={} gen={} (was {gen_in_alt} last time)",
        vt.grid().spike_images.len(),
        vt.grid().spike_image_gen
    );
    assert!(
        vt.grid().spike_images.is_empty(),
        "a fresh alternate screen came up holding the PREVIOUS alt session's image"
    );
}

// ── ATTACK 4: cross-pane amplification ──────────────────────────────────
//
// The emit is all-or-nothing: any change deletes every placement and
// re-transmits every image. A pane repainting a 1-pixel image therefore
// re-sends its neighbour's full-size one.
#[test]
fn attack_one_tiny_repaint_re_sends_every_pane() {
    let a = pane(1);
    let b = pane(2);
    let mut wl = WindowLayout::new(a);
    wl.split_pane(a, b, Direction::Vertical, 0.5);
    let layout = wl.tree.clone();

    let big_w = 500u32;
    let big_h = 200u32;
    let big: Vec<u8> = (0..(big_w * big_h * 4)).map(|i| (i % 251) as u8).collect();

    let mut vt_a = VirtualTerminal::new(38, 58);
    vt_a.process(&transmit(1, big_w, big_h, &big));
    let mut vt_b = VirtualTerminal::new(38, 58);

    let mut h = Harness::new(116, 38);
    let mut sizes = Vec::new();
    for n in 0..4u32 {
        // Pane B repaints a single pixel, over and over.
        vt_b.process(&transmit(7, 1, 1, &[n as u8, 0, 0, 255]));
        let mut vts: HashMap<PaneId, &VirtualTerminal> = HashMap::new();
        vts.insert(a, &vt_a);
        vts.insert(b, &vt_b);
        sizes.push(h.frame(&layout, a, &vts).len());
    }
    eprintln!("bytes per frame, 1-pixel repaints in pane B: {sizes:?}");
    let steady = sizes[3];
    assert!(
        steady < 10_000,
        "a 1-pixel repaint in one pane re-transmitted {steady} bytes (the other pane's \
         {} B image goes out again every frame)",
        big.len()
    );
}

// ── ATTACK 5: does an idle client stay silent? ──────────────────────────
#[test]
fn attack_idle_client_with_no_images_emits_no_graphics() {
    let p = pane(1);
    let layout = LayoutNode::leaf(p);
    let mut vt = VirtualTerminal::new(10, 20);
    vt.process(b"hello");
    let mut h = Harness::new(20, 10);
    let f1 = h.frame(&layout, p, &one(p, &vt));
    let g1 = f1.windows(3).filter(|w| *w == b"\x1b_G").count();
    let f2 = h.frame(&layout, p, &one(p, &vt));
    let g2 = f2.windows(3).filter(|w| *w == b"\x1b_G").count();
    eprintln!("no-image frames: {} then {} APC-G sequences", g1, g2);
    assert_eq!(g1 + g2, 0, "a pane with no images still emitted graphics commands");
}

// ── ATTACK 6: pane churn — a different pane, same rect, same geometry ───
#[test]
fn attack_pane_churn_at_the_same_rect_is_not_mistaken_for_the_same_image() {
    let a = pane(1);
    let b = pane(2);
    let mut outer = VirtualTerminal::new(10, 20);
    let mut h = Harness::new(20, 10);

    let mut vt_a = VirtualTerminal::new(10, 20);
    vt_a.process(&transmit(1, IW, IH, &solid([255, 0, 0])));
    let f = h.frame(&LayoutNode::leaf(a), a, &one(a, &vt_a));
    outer.process(&f);
    eprintln!("pane A: outer={:?}", outer_rgb(&outer));

    // Pane A closes; pane B takes the whole window and happens to draw the
    // same id at the same size, with different pixels — and at the same
    // epoch, because it is a different terminal that has stored one image.
    let mut vt_b = VirtualTerminal::new(10, 20);
    vt_b.process(&transmit(1, IW, IH, &solid([0, 255, 0])));
    eprintln!("epochs: A={} B={}", vt_a.spike_image_epoch(), vt_b.spike_image_epoch());
    let f = h.frame(&LayoutNode::leaf(b), b, &one(b, &vt_b));
    outer.process(&f);
    eprintln!("pane B: emitted={} outer={:?}", f.len(), outer_rgb(&outer));

    assert_eq!(
        outer_rgb(&outer),
        Some((0, 255, 0)),
        "a new pane's image at the same rect/geometry was suppressed"
    );
}

// ── MEASUREMENT: what does a full-canvas repaint actually cost? ─────────
#[test]
fn measure_full_canvas_repaint_cost() {
    let p = pane(1);
    let layout = LayoutNode::leaf(p);
    let (w, hh) = (1026u32, 665u32);
    let mut vt = VirtualTerminal::new(38, 116);
    let mut h = Harness::new(116, 38);
    let mut sizes = Vec::new();
    for n in 0..3u32 {
        let px: Vec<u8> = (0..(w * hh * 4)).map(|i| ((i + n) % 251) as u8).collect();
        vt.process(&transmit(1, w, hh, &px));
        sizes.push(h.frame(&layout, p, &one(p, &vt)).len());
    }
    eprintln!(
        "1026x665 repaint: {sizes:?} bytes/frame  => {:.1} MB/s at 60fps",
        sizes[2] as f64 * 60.0 / 1e6
    );
}

// ── ATTACK 7: does RIS clear the image store? ───────────────────────────
#[test]
fn attack_ris_clears_the_image_store() {
    let mut vt = VirtualTerminal::new(10, 20);
    vt.process(&transmit(1, IW, IH, &solid([255, 0, 0])));
    assert_eq!(vt.grid().spike_images.len(), 1);
    let epoch_before = vt.spike_image_epoch();
    vt.process(b"\x1bc"); // RIS
    eprintln!(
        "after RIS: images={} epoch {} -> {}",
        vt.grid().spike_images.len(),
        epoch_before,
        vt.spike_image_epoch()
    );
    assert!(
        vt.grid().spike_images.is_empty(),
        "RIS left the picture on a terminal that was told to reset"
    );
}
