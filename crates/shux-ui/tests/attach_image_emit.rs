//! What the compositor writes to the terminal a user is attached from.
//!
//! shux cannot audit this by rendering it itself — `shux-raster` clips while
//! compositing, so anything shux EMITS is never drawn by one in a shux-rendered
//! check. These tests read the bytes; `make test-gui-terminal` photographs a
//! real terminal drawing them.

use std::collections::HashMap;
use std::io::Cursor;

use shux_core::layout::{Direction, LayoutNode, WindowLayout};
use shux_core::model::PaneId;
use shux_ui::{BorderStyle, CompositorConfig, MultiPaneFrame, RenderCompositor};
use shux_vt::VirtualTerminal;
use uuid::Uuid;

fn pane(n: u128) -> PaneId {
    PaneId::from_uuid(Uuid::from_u128(n))
}

fn make_compositor(width: u16, height: u16) -> RenderCompositor<Cursor<Vec<u8>>> {
    let cfg = CompositorConfig {
        show_border: false,
        status_bar_height: 0,
        border_style: BorderStyle::None,
        ..Default::default()
    };
    let mut c = RenderCompositor::new(width, height, Cursor::new(Vec::new()), cfg);
    c.set_graphics(true);
    c
}

/// A real kitty transmit-and-display command, as a pane's application sends it:
/// `f=24` raw RGB, chunked at the protocol's 4096, with `a=T` repeated on every
/// continuation the way `kitten icat` does.
fn kitty_rgb(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    use base64::Engine as _;
    let mut px = Vec::with_capacity((w * h * 3) as usize);
    for _ in 0..w * h {
        px.extend_from_slice(&rgb);
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(&px);
    let bytes = b64.as_bytes();
    let total = bytes.len().div_ceil(4096).max(1);
    let mut out = Vec::new();
    for (i, chunk) in bytes.chunks(4096).enumerate() {
        let more = u8::from(i + 1 < total);
        out.extend_from_slice(
            format!(
                "\x1b_Ga=T,f=24,s={w},v={h},t=d,m={more};{}\x1b\\",
                std::str::from_utf8(chunk).unwrap()
            )
            .as_bytes(),
        );
    }
    out
}

fn frame<'a>(
    layout: &'a LayoutNode,
    vts: &'a HashMap<PaneId, &'a VirtualTerminal>,
    focused: PaneId,
) -> MultiPaneFrame<'a> {
    MultiPaneFrame {
        layout,
        zoom: None,
        focused,
        vts,
        titles: None,
        status_bar: None,
    }
}

/// Bytes written since the last call, and reset.
fn drain(c: &mut RenderCompositor<Cursor<Vec<u8>>>) -> String {
    let buf = std::mem::take(c.inner_mut().get_mut());
    c.inner_mut().set_position(0);
    String::from_utf8_lossy(&buf).into_owned()
}

/// The graphics COMMANDS written, in order. Continuation chunks carry only
/// `m=`/`q=` and are not commands, so counting them would make every assertion
/// here a function of the payload size.
fn graphics(out: &str) -> Vec<String> {
    out.split("\x1b_G")
        .skip(1)
        .map(|rest| rest.split(';').next().unwrap_or("").to_string())
        .filter(|k| k.starts_with("a="))
        .collect()
}

#[test]
fn an_image_in_a_pane_reaches_the_attached_terminal() {
    let p = pane(1);
    let layout = LayoutNode::leaf(p);
    let mut vt = VirtualTerminal::new(10, 20);
    vt.process(&kitty_rgb(18, 38, [200, 40, 40]));
    let mut vts = HashMap::new();
    vts.insert(p, &vt);

    let mut c = make_compositor(20, 10);
    c.render_multi_pane(frame(&layout, &vts, p)).unwrap();
    let out = drain(&mut c);
    let cmds = graphics(&out);

    assert_eq!(cmds.len(), 1, "expected one transmit, got {cmds:?}");
    let cmd = &cmds[0];
    assert!(cmd.starts_with("a=T,f=24"), "{cmd}");
    assert!(cmd.contains("s=18,v=38"), "{cmd}");
    // 18x38 at the declared 9x19 cell is exactly 2x2 cells.
    assert!(cmd.contains("c=2,r=2"), "{cmd}");
    // Without C=1 kitty advances the cursor and scrolls the user's whole
    // screen for an image at the bottom margin; without q=2 its `OK` is
    // decoded as keystrokes and typed into the pane.
    assert!(cmd.contains("C=1"), "{cmd}");
    assert!(cmd.contains("q=2"), "{cmd}");
}

#[test]
fn an_unchanged_picture_costs_nothing_on_the_next_frame() {
    let p = pane(1);
    let layout = LayoutNode::leaf(p);
    let mut vt = VirtualTerminal::new(10, 20);
    vt.process(&kitty_rgb(18, 38, [200, 40, 40]));
    let mut vts = HashMap::new();
    vts.insert(p, &vt);

    let mut c = make_compositor(20, 10);
    c.render_multi_pane(frame(&layout, &vts, p)).unwrap();
    let first = drain(&mut c);
    assert_eq!(graphics(&first).len(), 1);

    c.render_multi_pane(frame(&layout, &vts, p)).unwrap();
    let second = drain(&mut c);
    assert!(
        graphics(&second).is_empty(),
        "re-sent an unchanged picture: {:?}",
        graphics(&second)
    );
}

#[test]
fn new_pixels_under_the_same_image_id_are_re_transmitted() {
    // The case a geometry-only signature cannot see, and the one that matters
    // most in practice: terminal-browser redraws `i=1` at an unchanged
    // `s=`/`v=` on every frame, so identical geometry is the steady state.
    let p = pane(1);
    let layout = LayoutNode::leaf(p);
    let mut vt = VirtualTerminal::new(10, 20);
    vt.process(&kitty_rgb(18, 38, [200, 40, 40]));
    let mut vts = HashMap::new();
    vts.insert(p, &vt);

    let mut c = make_compositor(20, 10);
    c.render_multi_pane(frame(&layout, &vts, p)).unwrap();
    let first = graphics(&drain(&mut c));
    assert_eq!(first.len(), 1);

    let mut vt2 = VirtualTerminal::new(10, 20);
    vt2.process(&kitty_rgb(18, 38, [40, 40, 200]));
    let mut vts2 = HashMap::new();
    vts2.insert(p, &vt2);
    c.render_multi_pane(frame(&layout, &vts2, p)).unwrap();
    let second = graphics(&drain(&mut c));

    let transmits: Vec<_> = second.iter().filter(|k| k.starts_with("a=T")).collect();
    assert_eq!(transmits.len(), 1, "new pixels were not sent: {second:?}");
    // The replacement goes to a fresh id and the old one is retired only after
    // it is up: re-transmitting under a live id frees the old image on the
    // FIRST chunk and blanks the pane for the whole transfer.
    let deletes: Vec<_> = second.iter().filter(|k| k.starts_with("a=d")).collect();
    assert_eq!(deletes.len(), 1, "old id not retired: {second:?}");
    let new_id = transmits[0]
        .split(',')
        .find_map(|k| k.strip_prefix("i="))
        .unwrap();
    assert!(
        !deletes[0].contains(&format!("i={new_id},")),
        "deleted the id it just transmitted: {second:?}"
    );
}

#[test]
fn a_picture_taller_than_its_pane_is_cropped_not_squashed() {
    // `c=`/`r=` only ever STRETCH into a cell box; they never crop. Bounding a
    // placement to its pane is the emitter's job, in the source rectangle.
    let p = pane(1);
    let layout = LayoutNode::leaf(p);
    let mut vt = VirtualTerminal::new(4, 20);
    // 10 cells tall in a 4-row pane.
    vt.process(&kitty_rgb(18, 190, [200, 40, 40]));
    let mut vts = HashMap::new();
    vts.insert(p, &vt);

    let mut c = make_compositor(20, 4);
    c.render_multi_pane(frame(&layout, &vts, p)).unwrap();
    let cmds = graphics(&drain(&mut c));
    assert_eq!(cmds.len(), 1, "{cmds:?}");
    let cmd = &cmds[0];

    // Four rows of cells, and a source rectangle of exactly those four rows'
    // worth of pixels — not the whole 190.
    assert!(cmd.contains("r=4"), "{cmd}");
    assert!(cmd.contains("h=76"), "expected h=4*19, got {cmd}");
    assert!(!cmd.contains("h=190"), "sent the whole bitmap: {cmd}");
}

#[test]
fn a_picture_scrolled_above_its_pane_is_drawn_from_where_it_enters() {
    let p = pane(1);
    let layout = LayoutNode::leaf(p);
    let mut vt = VirtualTerminal::new(6, 20);
    vt.process(&kitty_rgb(18, 190, [200, 40, 40]));
    // Push the anchor above the viewport.
    vt.process(b"\n\n\n\n\n\n\n\n");
    let mut vts = HashMap::new();
    vts.insert(p, &vt);

    let mut c = make_compositor(20, 6);
    c.render_multi_pane(frame(&layout, &vts, p)).unwrap();
    let cmds = graphics(&drain(&mut c));
    assert_eq!(cmds.len(), 1, "the fixture scrolled clean off: {cmds:?}");
    let cmd = &cmds[0];
    let y: u32 = cmd
        .split(',')
        .find_map(|k| k.strip_prefix("y="))
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("no source y in {cmd}"));
    assert!(y > 0, "top rows above the pane were not skipped: {cmd}");
    assert_eq!(y % 19, 0, "source y is not a whole cell row: {cmd}");
}

#[test]
fn a_second_panes_picture_stays_out_of_its_neighbour() {
    let a = pane(1);
    let b = pane(2);
    let mut wl = WindowLayout::new(a);
    wl.split_pane(a, b, Direction::Vertical, 0.5);
    let layout = wl.tree.clone();
    let mut va = VirtualTerminal::new(10, 40);
    // 30 cells wide in a 20-cell pane. Sized to OVERFLOW: at exactly pane
    // width the horizontal bound is a no-op and this test cannot fail.
    va.process(&kitty_rgb(270, 190, [200, 40, 40]));
    let vb = VirtualTerminal::new(10, 40);
    let mut vts = HashMap::new();
    vts.insert(a, &va);
    vts.insert(b, &vb);

    let mut c = make_compositor(40, 10);
    c.render_multi_pane(frame(&layout, &vts, a)).unwrap();
    let out = drain(&mut c);
    let cmds = graphics(&out);
    assert_eq!(cmds.len(), 1, "{cmds:?}");

    let (_, left) = wl
        .compute_rects(shux_core::layout::Rect::new(0, 0, 40, 10))
        .into_iter()
        .find(|(id, _)| *id == a)
        .unwrap();
    let cols: u16 = cmds[0]
        .split(',')
        .find_map(|f| f.strip_prefix("c="))
        .and_then(|v| v.parse().ok())
        .unwrap();
    assert!(
        cols <= left.width,
        "picture spans {cols} cells in a {}-cell pane: {}",
        left.width,
        cmds[0]
    );
}

#[test]
fn a_pane_that_loses_its_picture_has_it_removed_from_the_host() {
    let p = pane(1);
    let layout = LayoutNode::leaf(p);
    let mut vt = VirtualTerminal::new(10, 20);
    vt.process(&kitty_rgb(18, 38, [200, 40, 40]));
    let mut vts = HashMap::new();
    vts.insert(p, &vt);

    let mut c = make_compositor(20, 10);
    c.render_multi_pane(frame(&layout, &vts, p)).unwrap();
    drain(&mut c);

    let empty = VirtualTerminal::new(10, 20);
    let mut vts2 = HashMap::new();
    vts2.insert(p, &empty);
    c.render_multi_pane(frame(&layout, &vts2, p)).unwrap();
    let cmds = graphics(&drain(&mut c));
    assert_eq!(cmds.len(), 1, "{cmds:?}");
    assert!(cmds[0].starts_with("a=d"), "{:?}", cmds[0]);
}

#[test]
fn the_cursor_ends_where_the_frame_wanted_it_not_on_the_picture() {
    // The emit CUPs to each picture. Leaving the cursor there parks the user's
    // caret on the image; forgetting where it is instead costs a hide/show
    // cycle on every frame that follows.
    let p = pane(1);
    let layout = LayoutNode::leaf(p);
    let mut vt = VirtualTerminal::new(10, 20);
    vt.process(&kitty_rgb(18, 38, [200, 40, 40]));
    vt.process(b"\x1b[9;7Hx");
    let mut vts = HashMap::new();
    vts.insert(p, &vt);

    let mut c = make_compositor(20, 10);
    c.render_multi_pane(frame(&layout, &vts, p)).unwrap();
    let out = drain(&mut c);

    // The exact target, not merely "not on the picture": asserting only the
    // negative half let a cursor put back at the WRONG place pass.
    let last_cup = out
        .rsplit("\x1b[")
        .find(|t| t.starts_with(|ch: char| ch.is_ascii_digit()) && t.contains('H'))
        .unwrap_or_else(|| panic!("no cursor positioning at all in {out:?}"));
    let (row, col) = last_cup.split('H').next().unwrap().split_once(';').unwrap();
    // `\x1b[9;7Hx` put the pane cursor on row 9, col 8; the frame is 1-based.
    assert_eq!(
        (row, col),
        ("9", "8"),
        "the frame did not leave the cursor where it wanted it"
    );
}

#[test]
fn a_picture_that_is_not_a_whole_number_of_cells_still_claims_the_cells_it_owns() {
    // Every other fixture here is 18x38 -- exactly 2x2 cells -- so none of them
    // can see what happens when a dimension is not a multiple of the cell.
    //
    // The two render paths cannot agree on PIXELS: `shux-raster` draws into its
    // own 9x19 cell, while the outer terminal has a cell size of its own, and an
    // image sized for one is not the same number of pixels in the other. What
    // they can and must agree on is the CELLS a picture occupies, which is what
    // "the picture is in its pane, beside the right text" means. `c=`/`r=` are
    // what buy that: the host scales into the cell box rather than laying the
    // bitmap down at a pixel size that means something different on its grid.
    let p = pane(1);
    let layout = LayoutNode::leaf(p);
    let mut vt = VirtualTerminal::new(10, 20);
    vt.process(&kitty_rgb(16, 20, [200, 40, 40]));
    let mut vts = HashMap::new();
    vts.insert(p, &vt);

    let mut c = make_compositor(20, 10);
    c.render_multi_pane(frame(&layout, &vts, p)).unwrap();
    let cmds = graphics(&drain(&mut c));
    assert_eq!(cmds.len(), 1, "{cmds:?}");
    let cmd = &cmds[0];

    // 16px over a 9px cell is 2 cells; 20px over 19px is 2. The same ceiling
    // `shux-vt`'s `place_image` uses to reserve them and `shux-raster` uses to
    // lay them out, so all three paths agree on the footprint.
    assert!(cmd.contains("c=2,r=2"), "{cmd}");
    // …and the source rect is the whole bitmap, not a cell-rounded lie about it.
    assert!(cmd.contains("w=16"), "{cmd}");
    assert!(cmd.contains("h=20"), "{cmd}");
}

#[test]
fn a_picture_is_drawn_under_text_so_overlays_stay_legible() {
    // `shux attach` writes copy mode, the copy menu, the help sheet and the
    // welcome toast into the frame AFTER the compositor emits images. At the
    // protocol's default z=0 a placement is drawn above text, so a picture
    // would blank whichever overlay landed on it -- measured in real kitty as
    // 22 surviving glyph pixels against 1638 at z=-1. Those overlays were
    // always legible before this path existed.
    let p = pane(1);
    let layout = LayoutNode::leaf(p);
    let mut vt = VirtualTerminal::new(10, 20);
    vt.process(&kitty_rgb(18, 38, [200, 40, 40]));
    let mut vts = HashMap::new();
    vts.insert(p, &vt);

    let mut c = make_compositor(20, 10);
    c.render_multi_pane(frame(&layout, &vts, p)).unwrap();
    for cmd in graphics(&drain(&mut c)) {
        assert!(cmd.contains("z=-1"), "drawn over the overlays: {cmd}");
    }
}

#[test]
fn a_full_repaint_re_places_without_re_sending_pixels() {
    // A host that drops placements on a full repaint is the model this emitter
    // defends against, so a repainted frame must re-issue each placement --
    // with an `a=p`, never a re-transmit, which is what the first version of
    // this behaviour was cut for.
    let p = pane(1);
    let layout = LayoutNode::leaf(p);
    let mut vt = VirtualTerminal::new(10, 20);
    vt.process(&kitty_rgb(18, 38, [200, 40, 40]));
    let mut vts = HashMap::new();
    vts.insert(p, &vt);

    let mut c = make_compositor(20, 10);
    c.render_multi_pane(frame(&layout, &vts, p)).unwrap();
    drain(&mut c);
    // Unchanged frame: silence.
    c.render_multi_pane(frame(&layout, &vts, p)).unwrap();
    assert!(graphics(&drain(&mut c)).is_empty());

    c.force_redraw();
    c.render_multi_pane(frame(&layout, &vts, p)).unwrap();
    let out = drain(&mut c);
    let cmds = graphics(&out);
    assert_eq!(cmds.len(), 1, "{cmds:?}");
    assert!(
        cmds[0].starts_with("a=p"),
        "re-sent the pixels: {}",
        cmds[0]
    );
    assert!(
        out.len() < 4096,
        "a re-place cost {} bytes; it should carry no payload",
        out.len()
    );
}

#[test]
fn a_terminal_that_never_answered_the_probe_is_sent_no_graphics() {
    // Not an optimisation. Measured with a shux attach running inside tmux 3.4:
    // the emitter's own continuation header became the tmux window title --
    // `Gq=2,m=0;AP+H...` where the base build showed `vm` -- rewritten once per
    // frame by any pane that redraws. An outer multiplexer is not a terminal
    // that quietly ignores an APC block.
    let p = pane(1);
    let layout = LayoutNode::leaf(p);
    let mut vt = VirtualTerminal::new(10, 20);
    vt.process(&kitty_rgb(18, 38, [200, 40, 40]));
    let mut vts = HashMap::new();
    vts.insert(p, &vt);

    let mut c = make_compositor(20, 10);
    c.set_graphics(false);
    c.render_multi_pane(frame(&layout, &vts, p)).unwrap();
    let out = drain(&mut c);
    assert!(
        !out.contains("\x1b_G"),
        "emitted graphics to a terminal that never claimed to draw them"
    );
    assert!(out.contains('\x1b'), "wrote no cells either");
}
