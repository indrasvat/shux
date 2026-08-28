//! `window.snapshot` must compose with the border style the user configured.
//!
//! `snapshot.rs` passed a hardcoded `BorderStyle::Rounded`. Beyond the cosmetic
//! wrong outline, `compose` derives the pane viewport from the style, so a
//! snapshot under `border_style = "none"` cropped the pane's last column and
//! row out of the PNG. Drives the real binary, daemon and CLI: `attach.rs`'s
//! unit test calls `compose` with a style it parsed itself and stayed green.

use std::path::PathBuf;
use std::process::Command;

/// Truecolor pane fill; a cropped column reads as frame background instead.
const FILL: (u8, u8, u8) = (200, 40, 40);
const TOL: i32 = 8;

struct Env {
    bin: PathBuf,
    runtime: tempfile::TempDir,
    config: tempfile::TempDir,
    state: tempfile::TempDir,
}

impl Env {
    /// `border_style` is written BEFORE anything starts the daemon: it reads its
    /// config exactly once, on first use.
    fn new(border_style: &str) -> Self {
        let config = tempfile::tempdir().expect("config dir");
        let dir = config.path().join("shux");
        std::fs::create_dir_all(&dir).expect("config subdir");
        std::fs::write(
            dir.join("config.toml"),
            format!("[appearance]\nborder_style = \"{border_style}\"\n"),
        )
        .expect("write config");
        Self {
            bin: PathBuf::from(env!("CARGO_BIN_EXE_shux")),
            runtime: tempfile::tempdir().expect("runtime dir"),
            config,
            state: tempfile::tempdir().expect("state dir"),
        }
    }

    fn shux(&self) -> Command {
        let mut c = Command::new(&self.bin);
        c.env_remove("SHUX_SOCKET")
            .env("XDG_RUNTIME_DIR", self.runtime.path())
            .env("XDG_CONFIG_HOME", self.config.path())
            .env("XDG_STATE_HOME", self.state.path())
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor");
        c
    }

    fn run(&self, args: &[&str]) -> String {
        let out = self.shux().args(args).output().expect("run shux");
        assert!(
            out.status.success(),
            "shux {args:?} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        // Hard rule: no leaked daemons, including on the panic path.
        let _ = self.shux().args(["daemon", "stop"]).output();
    }
}

fn near(actual: (u8, u8, u8, u8), expected: (u8, u8, u8)) -> bool {
    (actual.0 as i32 - expected.0 as i32).abs() <= TOL
        && (actual.1 as i32 - expected.1 as i32).abs() <= TOL
        && (actual.2 as i32 - expected.2 as i32).abs() <= TOL
}

/// Start a pane whose every column carries the fill, and snapshot the window.
fn snapshot_filled_window(env: &Env, cols: u16, rows: u16) -> (image::RgbaImage, u32, u32) {
    let script = format!(
        "printf 'BORDERSTYLE-MARKER\\n'; \
         i=1; while [ \"$i\" -le 8 ]; do \
           printf '\\033[48;2;200;40;40m%s\\033[0m\\n' \
             \"$(printf 'X%.0s' $(seq 1 {cols}))\"; \
           i=$((i+1)); \
         done; exec cat"
    );
    let name = format!("bstyle-{}", std::process::id());
    env.run(&["session", "create", &name, "-d", "--", "sh", "-c", &script]);
    let panes = env.run(&["--format", "json", "pane", "list", "-s", &name]);
    let v: serde_json::Value = serde_json::from_str(&panes).expect("pane list json");
    let pane_id = v[0]["id"].as_str().expect("pane id").to_string();

    env.run(&[
        "pane",
        "set-size",
        "-s",
        &name,
        "-p",
        &pane_id,
        "--cols",
        &cols.to_string(),
        "--rows",
        &rows.to_string(),
    ]);
    env.run(&[
        "pane",
        "wait-for",
        "-s",
        &name,
        "-p",
        &pane_id,
        "-t",
        "BORDERSTYLE-MARKER",
        "--timeout-ms",
        "20000",
    ]);

    let png = env.runtime.path().join("snap.png");
    // `window snapshot` has its own grid, defaulting to 120x40. Pin it to the
    // pane's size or the composed window is a different picture from the one
    // under test.
    env.run(&[
        "window",
        "snapshot",
        "-s",
        &name,
        "--cols",
        &cols.to_string(),
        "--rows",
        &rows.to_string(),
        "-o",
        png.to_str().expect("png path"),
    ]);
    let img = image::open(&png).expect("decode snapshot png").to_rgba8();
    let (cw, ch) = (
        img.width() / u32::from(cols),
        img.height() / u32::from(rows),
    );
    env.run(&["session", "kill", &name]);
    (img, cw, ch)
}

/// Sample a cell's interior, away from glyph strokes and cell edges.
fn cell_bg(img: &image::RgbaImage, cw: u32, ch: u32, col: u32, row: u32) -> (u8, u8, u8, u8) {
    let x = (col * cw + cw / 2).min(img.width() - 1);
    let y = (row * ch + 2).min(img.height() - 1);
    let p = img.get_pixel(x, y);
    (p[0], p[1], p[2], p[3])
}

/// Under `border_style = "none"` the snapshot must draw no outline and must
/// carry the pane's LAST column — the one the frozen `Rounded` cropped.
#[test]
fn window_snapshot_honours_border_style_none() {
    let env = Env::new("none");
    let (cols, rows) = (60u16, 20u16);
    let (img, cw, ch) = snapshot_filled_window(&env, cols, rows);

    // With no outline the pane starts at the origin: row 0 is the marker, the
    // fill begins at row 1.
    let first = cell_bg(&img, cw, ch, 0, 1);
    let last = cell_bg(&img, cw, ch, u32::from(cols) - 1, 1);
    assert!(
        near(first, FILL),
        "first column is not the pane's fill ({first:?}); the snapshot inset for \
         an outline the user turned off"
    );
    assert!(
        near(last, FILL),
        "LAST column is not the pane's fill ({last:?}) — the snapshot composed \
         the pane into a rect narrower than its grid and cropped it away"
    );
}

/// The control: the default style DOES inset for an outline, so the two are not
/// the same picture and the test above is not vacuous.
///
/// Asserts the INSET, which is the only thing that separates `rounded` from
/// `none`. An earlier version asserted the top pixel row was not one flat
/// colour — true on any image with a glyph in it, so it passed on a build that
/// ignored the config and always composed `none`.
#[test]
fn window_snapshot_still_draws_the_default_outline() {
    let env = Env::new("rounded");
    let (cols, rows) = (60u16, 20u16);
    let (img, cw, ch) = snapshot_filled_window(&env, cols, rows);

    // Inset by one: cell column 0 is the outline ring, column 1 the pane's
    // first column. Row 2 is a fill row either way (`rounded` puts the marker
    // at row 1, `none` at row 0), so only the column distinction is under test.
    let outside = cell_bg(&img, cw, ch, 0, 2);
    let inside = cell_bg(&img, cw, ch, 1, 2);
    assert!(
        !near(outside, FILL),
        "column 0 carries the pane fill ({outside:?}); nothing is inset for an \
         outline, so the `none` test above proves nothing"
    );
    assert!(
        near(inside, FILL),
        "column 1 is not the pane's fill ({inside:?})"
    );
}
