//! A pane chooses `s=`/`v=`, so a placement's decoded size is untrusted input.
//!
//! The bomb is not malformed: it is a valid PNG that honestly decodes to
//! 5000x5000 -- 100 MB of RGBA from ~25 KB on the wire, on EVERY snapshot.
//! That is under `image`'s own 512 MB default, so the library will not refuse
//! it; comparing the decoded size against the declared one afterwards cannot
//! either, because by then the allocation has happened. Only a ceiling applied
//! to the DECLARED size, before the decoder runs, bounds it.
//!
//! The size is chosen to discriminate: at 20000x20000 `image`'s own default
//! rejects the decode and the test passes either way, proving nothing.
use std::io::Write as _;

use shux_raster::{RasterOptions, Rasterizer};
use shux_vt::VirtualTerminal;

/// A real single-colour WHITE greyscale PNG of `w` x `h`: kilobytes on the
/// wire, a hundred megabytes decoded.
fn png_bomb(w: u32, h: u32) -> Vec<u8> {
    fn chunk(kind: &[u8], data: &[u8]) -> Vec<u8> {
        let mut out = (data.len() as u32).to_be_bytes().to_vec();
        let body: Vec<u8> = kind.iter().chain(data).copied().collect();
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
        out
    }
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
    let mut ihdr = w.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 0, 0, 0, 0]); // 8-bit greyscale

    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    // WHITE, not black: the canvas ground is dark, so a black bomb would be
    // invisible to the assertion below and the test would pass on a tree that
    // draws it.
    let mut row = vec![0xFFu8; w as usize + 1];
    row[0] = 0; // PNG filter byte
    for _ in 0..h {
        z.write_all(&row).expect("deflate");
    }
    let idat = z.finish().expect("deflate");

    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend(chunk(b"IHDR", &ihdr));
    png.extend(chunk(b"IDAT", &idat));
    png.extend(chunk(b"IEND", b""));
    png
}

fn base64(bytes: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
        for i in 0..4 {
            out.push(if i <= c.len() {
                A[(n >> (18 - 6 * i)) as usize & 63] as char
            } else {
                '='
            });
        }
    }
    out
}

#[test]
fn a_png_that_decodes_to_gigabytes_is_refused_before_it_allocates() {
    let (w, h) = (5_000u32, 5_000u32);
    let payload = png_bomb(w, h);
    assert!(
        payload.len() < 1024 * 1024,
        "the bomb must be small on the wire to be a bomb ({} bytes)",
        payload.len()
    );
    let decoded = u64::from(w) * u64::from(h) * 4;
    assert!(
        decoded > 64 * 1024 * 1024 && decoded < 512 * 1024 * 1024,
        "the bomb must sit ABOVE shux's ceiling and BELOW image's own default, \
         or this test cannot tell the two apart ({decoded} bytes)"
    );

    // Delivered in chunks, the way real `kitten icat` sends anything over a
    // few KB -- and the only way past the 64 KiB cap on a single APC body.
    let mut vt = VirtualTerminal::new(24, 80);
    let b64 = base64(&payload);
    let pieces: Vec<&str> = b64
        .as_bytes()
        .chunks(4096)
        .map(|c| std::str::from_utf8(c).expect("base64 is ascii"))
        .collect();
    for (i, piece) in pieces.iter().enumerate() {
        let more = u8::from(i + 1 < pieces.len());
        let head = if i == 0 {
            format!("a=T,f=100,s={w},v={h},m={more}")
        } else {
            format!("a=T,m={more}")
        };
        vt.process(format!("\x1b_G{head};{piece}\x1b\\").as_bytes());
    }
    assert_eq!(
        vt.grid().placements().len(),
        1,
        "the chunked bomb did not even reach a placement; \
         this test would then prove nothing about the decoder"
    );

    // Refused before the decoder runs, so nothing reaches the canvas.
    let r = Rasterizer::new(14.0).expect("rasterizer");
    let img = r.render(vt.grid(), &RasterOptions::default());
    let white = img
        .pixels()
        .filter(|p| p.0[0] > 200 && p.0[1] > 200 && p.0[2] > 200)
        .count();
    assert_eq!(
        white, 0,
        "the bomb was decoded and drawn: {white} px of it reached the canvas"
    );
}
