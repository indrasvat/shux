//! Issue #174 — `window.snapshot` must compose with the border style the user
//! configured, not with a constant.
//!
//! `snapshot.rs` passed a hardcoded `BorderStyle::Rounded` to `shux_ui::compose`
//! and that is the ONLY production caller of the composer. Two bugs sat on that
//! one argument. The cosmetic one is old: a user configured `thick` / `ascii` /
//! `none` got rounded borders in every snapshot PNG. The one that is not
//! cosmetic arrived with #174 — `compose` derives the pane viewport from the
//! style, so once a pane's PTY started following the LIVE compositor's rule, a
//! snapshot under `border_style = "none"` composed panes into rects two columns
//! and two rows smaller than their grids and silently cropped the right and
//! bottom edges out of the image.
//!
//! Why this file rather than a case in `attach.rs`'s unit tests:
//! `every_render_path_agrees_on_the_pane_viewport` calls `shux_ui::compose`
//! directly with a style it parsed itself, so it asserts agreement between two
//! things it wired together and stayed green while production disagreed. This
//! drives the real binary, the real daemon and the real CLI, so the constant is
//! in the path.
//!
//! Self-contained on purpose: `tests/lens_common` is a frozen path (PRD §16.2)
//! and none of this is about lens.
//!
//! Colour probes are mandatory (CLAUDE.md), and here the fill colour is
//! load-bearing — it is what makes a cropped column detectable at all.

use std::path::PathBuf;
use std::process::Command;

/// Truecolor fill for the pane body. A cropped column reads as the frame's
/// background instead of this, which is the whole assertion.
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
        // Zero leaked daemons is a hard rule, and it has to hold on the panic
        // path too — an assertion failure must not leave a daemon behind.
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
    // Cell metrics come from the declared box, which the snapshot rasterizer
    // renders at; asserting them keeps the probes from drifting silently.
    let (cw, ch) = (
        u32::from(shux_pty::DECLARED_CELL_PIXELS.0),
        u32::from(shux_pty::DECLARED_CELL_PIXELS.1),
    );
    assert_eq!(
        img.width(),
        u32::from(cols) * cw,
        "snapshot width is not cols x the declared cell box"
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

/// The control: the default style DOES draw an outline, so the two are not the
/// same picture. Without it, the test above would also pass on a build that
/// ignores the setting in the other direction.
#[test]
fn window_snapshot_still_draws_the_default_outline() {
    let env = Env::new("rounded");
    let (cols, rows) = (60u16, 20u16);
    let (img, _cw, ch) = snapshot_filled_window(&env, cols, rows);

    // Scan the whole first cell-row of PIXELS rather than one sample per cell:
    // a horizontal box-drawing rule sits at the cell's vertical centre, so a
    // fixed offset near the top misses it and reports a uniform row.
    let mut distinct = std::collections::HashSet::new();
    for y in 0..ch.min(img.height()) {
        for x in 0..img.width() {
            let p = img.get_pixel(x, y);
            distinct.insert((p[0], p[1], p[2]));
        }
    }
    assert!(
        distinct.len() > 1,
        "the default style drew a uniform top row; no outline is being composed \
         at all, so the `none` test above proves nothing"
    );
}
