//! SPIKE repro: does a same-geometry image REPAINT reach the outer terminal?
//!
//! Drives the real path: inner VT store -> compositor `spike_emit_images`
//! -> outer VT parse -> raster blend. No browser, no daemon.

use std::collections::HashMap;
use std::io::Cursor;

use shux_core::layout::LayoutNode;
use shux_core::model::PaneId;
use shux_ui::{BorderStyle, CompositorConfig, MultiPaneFrame, RenderCompositor};
use shux_vt::VirtualTerminal;
use uuid::Uuid;

const IW: u32 = 36;
const IH: u32 = 38;

fn solid(rgb: [u8; 3]) -> Vec<u8> {
    let mut v = Vec::with_capacity((IW * IH * 4) as usize);
    for _ in 0..(IW * IH) {
        v.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
    }
    v
}

/// Same wire shape the compositor emits: a=T,f=32,t=d, chunked at 4096.
fn transmit(id: u32, rgba: &[u8]) -> Vec<u8> {
    transmit_wh(id, IW, IH, rgba)
}

fn transmit_wh(id: u32, iw: u32, ih: u32, rgba: &[u8]) -> Vec<u8> {
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
                format!(
                    "\x1b_Ga=T,f=32,t=d,s={iw},v={ih},i={id},m={more};{payload}\x1b\\"
                )
                .as_bytes(),
            );
        } else {
            out.extend_from_slice(format!("\x1b_Gm={more};{payload}\x1b\\").as_bytes());
        }
    }
    out
}

fn count(img: &image::RgbaImage, rgb: [u8; 3]) -> usize {
    img.pixels()
        .filter(|p| p.0[0] == rgb[0] && p.0[1] == rgb[1] && p.0[2] == rgb[2])
        .count()
}

#[test]
fn same_geometry_repaint_reaches_the_outer_terminal() {
    let p = PaneId::from_uuid(Uuid::from_u128(1));
    let layout = LayoutNode::leaf(p);
    let mut inner = VirtualTerminal::new(10, 20);
    let mut outer = VirtualTerminal::new(10, 20);

    let cfg = CompositorConfig {
        show_border: false,
        status_bar_height: 0,
        border_style: BorderStyle::None,
        ..Default::default()
    };
    let mut comp = RenderCompositor::new(20, 10, Cursor::new(Vec::new()), cfg);

    let red = solid([255, 0, 0]);
    let blue = solid([0, 0, 255]);

    // Frame 1: inner shows the RED image.
    inner.process(&transmit(1, &red));
    let mut seen = 0usize;
    for (n, rgba) in [(1u32, &red), (2u32, &blue)] {
        if n == 2 {
            // Frame 2: SAME id, SAME geometry, DIFFERENT pixels — exactly what
            // terminal-browser sends on every repaint.
            inner.process(&transmit(1, rgba));
        }
        let mut vts: HashMap<PaneId, &VirtualTerminal> = HashMap::new();
        vts.insert(p, &inner);
        comp.render_multi_pane(MultiPaneFrame {
            layout: &layout,
            zoom: None,
            focused: p,
            vts: &vts,
            titles: None,
            status_bar: None,
        })
        .unwrap();
        let all = comp.inner_mut().get_ref().clone();
        let fresh = all[seen..].to_vec();
        seen = all.len();
        let graphics = fresh.windows(3).filter(|w| w == b"\x1b_G").count();
        eprintln!("frame {n}: {} emitted bytes, {graphics} APC-G openers", fresh.len());
        outer.process(&fresh);
    }

    // Frame 3: nothing changed in the store -> the bandwidth guard must still
    // hold. Without this the "fix" is just an unconditional re-emit.
    let mut vts: HashMap<PaneId, &VirtualTerminal> = HashMap::new();
    vts.insert(p, &inner);
    comp.render_multi_pane(MultiPaneFrame {
        layout: &layout,
        zoom: None,
        focused: p,
        vts: &vts,
        titles: None,
        status_bar: None,
    })
    .unwrap();
    let idle = comp.inner_mut().get_ref().len() - seen;
    eprintln!("frame 3 (no store change): {idle} emitted bytes");
    assert_eq!(idle, 0, "an idle frame re-transmitted the image");

    let store: Vec<_> = outer
        .grid()
        .spike_images
        .iter()
        .map(|i| (i.image_id, i.width, i.height, i.rgba[0], i.rgba[2]))
        .collect();
    eprintln!("outer store (id,w,h,r,b) = {store:?}");

    let rast = shux_raster::Rasterizer::new(14.0).unwrap();
    let png = rast.render(&outer.grid().clone_visible(), &shux_raster::RasterOptions::default());
    let reds = count(&png, [255, 0, 0]);
    let blues = count(&png, [0, 0, 255]);
    eprintln!("rendered: red={reds} blue={blues} (expect blue={})", IW * IH);

    assert_eq!(reds, 0, "stale RED pixels survived a repaint");
    assert_eq!(blues, (IW * IH) as usize, "the new BLUE image did not render");
}

/// Rules out the chunking hypotheses: a real-size 1026x665 image (~889 base64
/// chunks) must survive emit -> parse byte-for-byte.
#[test]
fn full_size_image_survives_chunked_emit_byte_exact() {
    const W: u32 = 1026;
    const H: u32 = 665;
    let p = PaneId::from_uuid(Uuid::from_u128(1));
    let layout = LayoutNode::leaf(p);

    // Deterministic non-uniform payload: a uniform one cannot catch a
    // reordered or duplicated chunk.
    let mut rgba = Vec::with_capacity((W * H * 4) as usize);
    let mut x: u32 = 0x1234_5678;
    for _ in 0..(W * H * 4) {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        rgba.push((x & 0xff) as u8);
    }

    let mut inner = VirtualTerminal::new(38, 116);
    let mut outer = VirtualTerminal::new(38, 116);
    inner.process(&transmit_wh(1, W, H, &rgba));
    assert_eq!(inner.grid().spike_images.len(), 1, "inner did not store it");

    let cfg = CompositorConfig {
        show_border: false,
        status_bar_height: 0,
        border_style: BorderStyle::None,
        ..Default::default()
    };
    let mut comp = RenderCompositor::new(116, 38, Cursor::new(Vec::new()), cfg);
    let mut vts: HashMap<PaneId, &VirtualTerminal> = HashMap::new();
    vts.insert(p, &inner);
    comp.render_multi_pane(MultiPaneFrame {
        layout: &layout,
        zoom: None,
        focused: p,
        vts: &vts,
        titles: None,
        status_bar: None,
    })
    .unwrap();
    let wire = comp.inner_mut().get_ref().clone();
    eprintln!("wire bytes = {}", wire.len());
    outer.process(&wire);

    let got = &outer.grid().spike_images;
    assert_eq!(got.len(), 1, "outer did not store the image");
    assert_eq!((got[0].width, got[0].height), (W, H));
    assert_eq!(got[0].rgba.len(), rgba.len());
    assert!(got[0].rgba == rgba, "payload corrupted across chunks");
    eprintln!("byte-exact across {} chunks", (rgba.len().div_ceil(3) * 4).div_ceil(4096));
}
