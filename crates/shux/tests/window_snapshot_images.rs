//! An inline image must survive multi-pane composition, and must not leave its
//! pane.

use std::collections::HashMap;

use base64::Engine as _;
use shux_core::layout::{Direction, LayoutNode, Rect};
use shux_core::model::PaneId;
use shux_ui::{BorderColors, BorderStyle, ComposeInputs, compose, pane_viewport};
use shux_vt::{Cursor, Grid, VirtualTerminal};
use uuid::Uuid;

const COLS: u16 = 100;
const ROWS: u16 = 30;
const STATUS_BAR_ROWS: u16 = 1;
const MAGENTA: [u8; 3] = [255, 0, 255];

/// The three probe colours must all reach the composed frame. Compositing runs
/// AFTER every cell is drawn, so a picture that overpaints its pane's text shows
/// up here as a missing probe.
fn assert_probes_survived(img: &image::RgbaImage, what: &str) {
    let mut truecolor = 0usize;
    let mut indexed = 0usize;
    let mut basic = 0usize;
    for p in img.pixels() {
        let [r, g, b, _] = p.0;
        if r < 60 && g > 150 && b > 40 && b < 140 {
            truecolor += 1;
        }
        if r > 200 && g > 90 && g < 180 && b < 60 {
            indexed += 1;
        }
        if r < 60 && g < 90 && b > 150 {
            basic += 1;
        }
    }
    assert!(
        truecolor > 0 && indexed > 0 && basic > 0,
        "{what}: a colour probe vanished (truecolor={truecolor} indexed={indexed} basic={basic})"
    );
}

/// A pixel from the BANDED picture, which blends toward magenta at every alpha
/// so an exact triple will not do. Not for the opaque cases: the focused border
/// is magenta-ish too.
fn is_banded_picture(p: [u8; 4]) -> bool {
    p[0] > p[1] && p[2] > p[1] && p[0] > 90 && p[2] > 90
}

/// An image delivered the way real `kitten icat` delivers one: 4096-byte base64
/// chunks, `a=T` repeated on every continuation.
///
/// `banded` gives every source row a distinct alpha, so which SLICE of the
/// picture reached the canvas is recoverable from the pixels.
fn kitty_image(w: u32, h: u32, rgb: [u8; 3], banded: bool) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let a = if banded { 40 + (y % 8) as u8 * 25 } else { 255 };
        for _ in 0..w {
            rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], a]);
        }
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(&rgba);
    let bytes = b64.as_bytes();
    let total = bytes.len().div_ceil(4096).max(1);
    let mut out = Vec::new();
    for (i, chunk) in bytes.chunks(4096).enumerate() {
        let more = u8::from(i + 1 < total);
        let payload = std::str::from_utf8(chunk).expect("base64 is ascii");
        if i == 0 {
            out.extend_from_slice(
                format!("\x1b_Ga=T,f=32,t=d,s={w},v={h},i=1,m={more};{payload}\x1b\\").as_bytes(),
            );
        } else {
            out.extend_from_slice(format!("\x1b_Ga=T,i=1,m={more};{payload}\x1b\\").as_bytes());
        }
    }
    out
}

struct Split {
    layout: LayoutNode,
    left: PaneId,
    right: PaneId,
    left_rect: Rect,
    right_rect: Rect,
    style: BorderStyle,
}

fn split() -> Split {
    let left = PaneId::from_uuid(Uuid::from_u128(1));
    let right = PaneId::from_uuid(Uuid::from_u128(2));
    let layout = LayoutNode::split(
        Direction::Vertical,
        0.5,
        LayoutNode::leaf(left),
        LayoutNode::leaf(right),
    );
    let style = BorderStyle::parse("rounded");
    let content = Rect::new(0, 0, COLS, ROWS - STATUS_BAR_ROWS);
    let rects: HashMap<PaneId, Rect> = layout
        .compute_rects(pane_viewport(content, style, false))
        .into_iter()
        .collect();
    let (left_rect, right_rect) = (rects[&left], rects[&right]);
    Split {
        layout,
        left,
        right,
        left_rect,
        right_rect,
        style,
    }
}

/// Compose the split and rasterize it exactly as `window snapshot` does.
fn window_png(
    s: &Split,
    l: (&Grid, &Cursor),
    r: (&Grid, &Cursor),
    font: f32,
) -> (image::RgbaImage, u32, u32) {
    let mut panes: HashMap<PaneId, (&Grid, &Cursor)> = HashMap::new();
    panes.insert(s.left, l);
    panes.insert(s.right, r);
    let frame = composed(s, &panes);
    let rast = shux_raster::Rasterizer::new(font).expect("rasterizer");
    let (cw, ch) = rast.cell_size();
    let mut img = rast.render(&frame.grid, &shux_raster::RasterOptions::default());
    rast.composite_composed(&mut img, &frame.placements);
    (img, cw, ch)
}

fn composed(s: &Split, panes: &HashMap<PaneId, (&Grid, &Cursor)>) -> shux_ui::ComposedFrame {
    compose(
        &ComposeInputs {
            layout: &s.layout,
            zoom: None,
            focused: s.left,
            panes,
            titles: None,
            status_bar: None,
        },
        COLS,
        ROWS,
        s.style,
        BorderColors::default(),
        STATUS_BAR_ROWS,
    )
}

/// Colour probes per CLAUDE.md. `assert_probes_survived` reads them back, so a
/// monochrome regression fails rather than passing unnoticed.
fn probe_text(vt: &mut VirtualTerminal) {
    vt.process(
        b"\x1b[38;2;0;200;90mTRUECOLOR\x1b[0m \x1b[38;5;208mIDX\x1b[0m \x1b[34mBASIC\x1b[0m\r\n",
    );
}

/// Count magenta inside and outside the given pixel box.
struct Magenta {
    inside: usize,
    outside: usize,
    /// Top and bottom of the drawn picture, RELATIVE to the pane's top edge, so
    /// the single-pane and composed renders are directly comparable.
    top: i64,
    bottom: i64,
}

/// Every drawn pixel of the picture, keyed relative to the pane's top-left, with
/// its colour. Two render paths showing the same slice of the same image agree
/// here exactly; a one-row shift does not.
fn picture(img: &image::RgbaImage, r: Rect, cw: u32, ch: u32) -> Vec<((u32, u32), [u8; 4])> {
    let (x0, y0) = (r.x as u32 * cw, r.y as u32 * ch);
    let (x1, y1) = ((r.x + r.width) as u32 * cw, (r.y + r.height) as u32 * ch);
    let mut out = Vec::new();
    for (x, y, p) in img.enumerate_pixels() {
        if x < x0 || x >= x1 || y < y0 || y >= y1 || !is_banded_picture(p.0) {
            continue;
        }
        out.push(((x - x0, y - y0), p.0));
    }
    out
}

/// A PNG placement: kilobytes on the wire, `w*h*4` bytes charged against the
/// decode budget. The asymmetry a hostile pane exploits.
fn kitty_png(w: u32, h: u32, id: u32) -> Vec<u8> {
    kitty_png_rgb(w, h, [0, 0, 0], id)
}

/// The same, in a nameable colour, so which placements reached the canvas is
/// recoverable rather than merely counted.
fn kitty_png_rgb(w: u32, h: u32, rgb: [u8; 3], id: u32) -> Vec<u8> {
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &b in bytes {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xEDB8_8320 & (!(crc & 1)).wrapping_add(1));
            }
        }
        !crc
    }
    fn chunk(kind: &[u8], data: &[u8]) -> Vec<u8> {
        let mut out = (data.len() as u32).to_be_bytes().to_vec();
        let body: Vec<u8> = kind.iter().chain(data).copied().collect();
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
        out
    }
    use std::io::Write as _;
    let mut ihdr = w.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit truecolour
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    let mut row = vec![0u8; w as usize * 3 + 1];
    for px in row[1..].as_chunks_mut::<3>().0 {
        *px = rgb;
    }
    for _ in 0..h {
        z.write_all(&row).expect("deflate");
    }
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend(chunk(b"IHDR", &ihdr));
    png.extend(chunk(b"IDAT", &z.finish().expect("deflate")));
    png.extend(chunk(b"IEND", b""));

    let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
    let bytes = b64.as_bytes();
    let total = bytes.len().div_ceil(4096).max(1);
    let mut out = Vec::new();
    for (i, c) in bytes.chunks(4096).enumerate() {
        let more = u8::from(i + 1 < total);
        let payload = std::str::from_utf8(c).expect("base64 is ascii");
        let head = if i == 0 {
            format!("a=T,f=100,t=d,s={w},v={h},i={id},C=1,m={more}")
        } else {
            format!("a=T,i={id},m={more}")
        };
        out.extend_from_slice(format!("\x1b_G{head};{payload}\x1b\\").as_bytes());
    }
    out
}

fn magenta(img: &image::RgbaImage, r: Rect, cw: u32, ch: u32) -> Magenta {
    let (x0, y0) = (r.x as u32 * cw, r.y as u32 * ch);
    let (x1, y1) = ((r.x + r.width) as u32 * cw, (r.y + r.height) as u32 * ch);
    let (mut inside, mut outside) = (0usize, 0usize);
    let (mut min_y, mut max_y) = (u32::MAX, 0u32);
    for (x, y, p) in img.enumerate_pixels() {
        if p.0[0] != MAGENTA[0] || p.0[1] != MAGENTA[1] || p.0[2] != MAGENTA[2] {
            continue;
        }
        min_y = min_y.min(y);
        max_y = max_y.max(y);
        if x >= x0 && x < x1 && y >= y0 && y < y1 {
            inside += 1;
        } else {
            outside += 1;
        }
    }
    Magenta {
        inside,
        outside,
        top: i64::from(min_y) - i64::from(y0),
        bottom: i64::from(max_y) - i64::from(y0),
    }
}

#[test]
fn an_image_larger_than_its_pane_is_drawn_and_stays_inside_it() {
    let s = split();
    // The grid is SMALLER than its rect, as during a resize lag: `compose_pane`
    // tolerates that, so a picture must respect the grid's bound and not just
    // the pane's. 367x553 declared px is 40.8 x 29.1 cells -- wider and taller
    // than this grid on both axes, and not a multiple of any cell size, so a
    // clip that is cell-granular rather than pixel-exact shows up too.
    const GC: usize = 37;
    const GR: usize = 23;
    let mut lvt = VirtualTerminal::new(GR, GC);
    let mut rvt = VirtualTerminal::new(GR, GC);
    probe_text(&mut lvt);
    probe_text(&mut rvt);
    lvt.process(b"\x1b[H");
    lvt.process(&kitty_image(367, 553, MAGENTA, false));

    let (lg, rg) = (lvt.grid().clone_visible(), rvt.grid().clone_visible());
    let (lc, rc) = (lvt.cursor().clone(), rvt.cursor().clone());
    let owned = Rect::new(s.left_rect.x, s.left_rect.y, GC as u16, GR as u16);

    // Several font sizes: `appearance.font` moves the drawn cell box, and the
    // clip is stated in cells while the blit is in pixels.
    let mut boxes = std::collections::BTreeSet::new();
    for font in [10.0f32, 14.0, 20.0] {
        let (img, cw, ch) = window_png(&s, (&lg, &lc), (&rg, &rc), font);
        boxes.insert((cw, ch));
        // The neighbouring pane's text must survive: compositing runs after
        // every cell is drawn, so an unclipped picture erases it.
        assert_probes_survived(&img, &format!("font {font}"));
        let m = magenta(&img, owned, cw, ch);
        assert!(
            m.inside > 0,
            "font {font}: the image never reached the composed frame"
        );
        assert_eq!(
            m.outside, 0,
            "font {font}: the image escaped the area its grid owns"
        );
        assert_eq!(
            m.bottom + 1,
            GR as i64 * i64::from(ch),
            "font {font}: the image stopped short of the grid's last row"
        );
    }
    assert!(
        boxes.len() >= 2,
        "font sizes collapsed to one cell box: {boxes:?}"
    );
}

/// Cross-path consistency (protocol step 10). A picture taller than its pane
/// scrolls its own anchor line above the viewport — the ordinary case for
/// `kitten icat` on any photo. `pane snapshot` already draws that correctly, so
/// the composed path must agree; clamping a negative row to zero shifts the
/// picture down and is the failure this catches.
#[test]
fn a_window_snapshot_draws_the_same_image_pixels_as_a_pane_snapshot() {
    let s = split();
    let mut lvt = VirtualTerminal::new(s.left_rect.height as usize, s.left_rect.width as usize);
    probe_text(&mut lvt);
    // Taller than the pane, then the trailing newlines real `kitten icat`
    // emits. The image's footprint is clamped to the pane, so the cursor ends
    // on the last row and each newline scrolls — carrying the anchor line up
    // out of the viewport while most of the picture is still on screen.
    let px_h = (s.left_rect.height as u32 + 6) * 19;
    lvt.process(&kitty_image(180, px_h, MAGENTA, true));
    lvt.process(b"\r\n\r\n");
    let vr = lvt.grid().placements()[0].viewport_row(lvt.grid());
    assert!(
        vr < 0,
        "setup did not reproduce the ordinary tall-image case: viewport_row={vr}"
    );

    let r = shux_raster::Rasterizer::new(14.0).expect("rasterizer");
    let (cw, ch) = r.cell_size();

    // Single-pane render: canvas == pane, which is what `pane snapshot` does.
    let solo = lvt.grid().clone_visible();
    let pane_img = r.render(&solo, &shux_raster::RasterOptions::default());
    let solo_rect = Rect::new(0, 0, s.left_rect.width, s.left_rect.height);
    let solo_px = picture(&pane_img, solo_rect, cw, ch);
    assert!(
        !solo_px.is_empty(),
        "the single-pane baseline drew no picture"
    );

    let mut rvt = VirtualTerminal::new(s.left_rect.height as usize, s.left_rect.width as usize);
    probe_text(&mut rvt);
    let (lg, rg) = (lvt.grid().clone_visible(), rvt.grid().clone_visible());
    let (lc, rc) = (lvt.cursor().clone(), rvt.cursor().clone());
    let (win_img, _, _) = window_png(&s, (&lg, &lc), (&rg, &rc), 14.0);
    assert_probes_survived(&win_img, "cross-path");
    let win_px = picture(&win_img, s.left_rect, cw, ch);
    // `picture` over the whole canvas is a superset of `picture` over the pane,
    // so equal lengths is exactly "nothing was drawn outside it". This is the
    // only check that can see a pixel drawn ABOVE the pane by a negative anchor.
    assert_eq!(
        picture(&win_img, Rect::new(0, 0, COLS, ROWS), cw, ch).len(),
        win_px.len(),
        "the composed image escaped its pane"
    );

    // The real cross-path claim: the same slice of the same picture, pixel for
    // pixel, relative to the pane. Extent or a tally alone would miss a clamped
    // row -- the picture keeps its size and shows a different part of itself.
    let first_diff = solo_px
        .iter()
        .zip(&win_px)
        .find(|(a, b)| a != b)
        .map(|(a, b)| format!("at {:?}: pane {:?} vs window {:?}", a.0, a.1, b.1));
    assert_eq!(
        (solo_px.len(), first_diff.clone()),
        (win_px.len(), None),
        "window snapshot and pane snapshot disagree on the same image"
    );
}

/// `compose_pane` shows the region the CURSOR lives in whenever a pane's grid
/// is taller than its rect, so the picture has to move with the text. Dropping
/// that offset leaves the image pinned while the text scrolls under it — and it
/// only reproduces when the source grid is taller than the rect, which the
/// other tests here never make it.
#[test]
fn the_image_tracks_the_cursor_following_viewport() {
    let s = split();
    let tall = s.left_rect.height as usize * 2;
    let mut lvt = VirtualTerminal::new(tall, s.left_rect.width as usize);
    probe_text(&mut lvt);
    lvt.process(b"\x1b[H");
    lvt.process(&kitty_image(90, 190, MAGENTA, false)); // 10 cells wide, 10 tall
    assert_eq!(
        lvt.grid().placements().len(),
        1,
        "the VT did not store the image"
    );

    let rvt = VirtualTerminal::new(tall, s.left_rect.width as usize);
    // Park the cursor progressively lower. Once it passes the rect height the
    // viewport scrolls, and the image must rise with the text it sits in.
    let mut tops = Vec::new();
    for cursor_row in [0usize, tall - 1] {
        let lg = lvt.grid().clone();
        let mut lc = lvt.cursor().clone();
        lc.row = cursor_row;
        let rg = rvt.grid().clone();
        let rc = rvt.cursor().clone();
        let (img, cw, ch) = window_png(&s, (&lg, &lc), (&rg, &rc), 14.0);
        let m = magenta(&img, s.left_rect, cw, ch);
        assert_eq!(
            m.outside, 0,
            "cursor_row={cursor_row}: the image escaped its pane"
        );
        tops.push((m.inside, m.top));
    }

    // Cursor at the top: the whole picture is visible at the pane's first row.
    assert_eq!(
        tops[0].1, 0,
        "the image was not at the pane top for a top cursor"
    );
    // Cursor at the bottom: the viewport has scrolled past the image entirely.
    assert_eq!(
        tops[1].0, 0,
        "the image stayed on screen after the viewport scrolled past it"
    );
}

/// A greedy pane must not delete its neighbour's picture.
///
/// The decode budget is per PANE. Shared across the window it was worse than
/// the stall it bounds: four hostile placements in the pane that composes first
/// took the whole budget, the neighbour's picture vanished from `window
/// snapshot` while `pane snapshot` still drew it, and swapping the panes
/// reversed which one survived.
#[test]
fn a_greedy_pane_does_not_starve_its_neighbours_picture() {
    let s = split();
    let mut lvt = VirtualTerminal::new(s.left_rect.height as usize, s.left_rect.width as usize);
    let mut rvt = VirtualTerminal::new(s.left_rect.height as usize, s.left_rect.width as usize);
    probe_text(&mut lvt);
    probe_text(&mut rvt);

    // Four maximal placements: each declares 4096x4096 -- the full per-image
    // ceiling -- and `C=1` keeps them all on screen. They must be PNG: raw RGBA
    // at that size is 89 MB of base64 and the assembler's 32 MiB cap refuses
    // it, which is how the first version of this test passed on the defect.
    // A distinct id per placement: re-transmitting one id replaces that image
    // and every placement of it, so a shared id would collapse these four into
    // one and the budget would never be reached.
    for id in 1..=4u32 {
        lvt.process(b"\x1b[H");
        lvt.process(&kitty_png(4096, 4096, id));
    }
    // The victim: one ordinary picture in the OTHER pane.
    rvt.process(b"\x1b[H");
    rvt.process(&kitty_image(90, 190, MAGENTA, false));

    let (lg, rg) = (lvt.grid().clone_visible(), rvt.grid().clone_visible());
    let (lc, rc) = (lvt.cursor().clone(), rvt.cursor().clone());

    // Both orders: the defect was order-dependent, so a single arrangement
    // would have passed on the shared budget half the time.
    for (name, l, r) in [
        ("victim second", (&lg, &lc), (&rg, &rc)),
        ("victim first", (&rg, &rc), (&lg, &lc)),
    ] {
        let (img, cw, ch) = window_png(&s, l, r, 14.0);
        let victim = if name == "victim second" {
            s.right_rect
        } else {
            s.left_rect
        };
        let m = magenta(&img, victim, cw, ch);
        assert!(
            m.inside > 0,
            "{name}: the neighbouring pane's picture was starved by a greedy pane"
        );
        assert_eq!(m.outside, 0, "{name}: the picture escaped its pane");
    }
}

/// The budget must actually refuse, not merely be charged.
///
/// `a_greedy_pane_does_not_starve_its_neighbours_picture` asserts a picture
/// SURVIVES, which is also what an inert budget looks like -- a version that
/// counted bytes and never refused passed the whole suite. Five maximal
/// placements in distinct colours, drawn in order: the ceiling is 256 MiB and
/// each charges 64 MiB, so the fourth must be the last one drawn and the fifth
/// must be refused, identically on both render paths.
#[test]
fn the_decode_budget_refuses_past_its_ceiling_identically_on_both_paths() {
    const HUES: [[u8; 3]; 5] = [
        [255, 0, 0],
        [0, 255, 0],
        [0, 0, 255],
        [255, 255, 0],
        [0, 255, 255],
    ];
    let s = split();
    let mut vt = VirtualTerminal::new(s.left_rect.height as usize, s.left_rect.width as usize);
    probe_text(&mut vt);
    for (n, hue) in HUES.iter().enumerate() {
        vt.process(b"\x1b[H");
        vt.process(&kitty_png_rgb(4096, 4096, *hue, n as u32 + 1));
    }

    let count = |img: &image::RgbaImage, hue: [u8; 3]| {
        img.pixels()
            .filter(|p| {
                p.0[0].abs_diff(hue[0]) < 40
                    && p.0[1].abs_diff(hue[1]) < 40
                    && p.0[2].abs_diff(hue[2]) < 40
            })
            .count()
    };

    // Single-pane path.
    let solo = vt.grid().clone_visible();
    let r = shux_raster::Rasterizer::new(14.0).expect("rasterizer");
    let pane_img = r.render(&solo, &shux_raster::RasterOptions::default());

    // Composed path, same content.
    let rvt = VirtualTerminal::new(s.left_rect.height as usize, s.left_rect.width as usize);
    let (lg, rg) = (vt.grid().clone_visible(), rvt.grid().clone_visible());
    let (lc, rc) = (vt.cursor().clone(), rvt.cursor().clone());
    let (win_img, _, _) = window_png(&s, (&lg, &lc), (&rg, &rc), 14.0);

    for (what, img) in [("pane", &pane_img), ("window", &win_img)] {
        let fourth = count(img, HUES[3]);
        let fifth = count(img, HUES[4]);
        assert!(
            fourth > 1000,
            "{what}: the 4th placement is within the ceiling and must be drawn ({fourth} px)"
        );
        assert_eq!(
            fifth, 0,
            "{what}: the 5th placement is past the ceiling and was drawn anyway ({fifth} px) \
             -- the budget is charged but never refuses"
        );
    }
}
