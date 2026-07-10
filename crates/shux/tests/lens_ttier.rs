//! Red suite — T-tier real, rich, Unicode-heavy TUIs (§13 TEST-3; T1–T4).
//!
//! FROZEN after P0 (§16.2). Run via `make test-lens-t`. Each test SKIPS with a
//! loud notice when its binary (`nidhi` / `vivecaka`) is absent (§13 — allowed
//! only in CI, never for the P6 DoD). When the binary IS present, every test
//! leads with `lens.run`, so in Phase P0 it fails with `method_not_found
//! (-32601)`.

mod lens_common;
use lens_common::*;

use std::process::Command;

/// Build the deterministic nidhi repo (pinned dates, exactly 3 stashes).
fn build_nidhi_repo(h: &Harness) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("nidhi repo tmp");
    let repo = dir.path().join("repo");
    let script = h
        .repo_root()
        .join(".shux/fixtures/lens/t/make_nidhi_repo.sh");
    let out = Command::new("sh")
        .arg(&script)
        .arg(&repo)
        .output()
        .expect("run make_nidhi_repo.sh");
    assert!(out.status.success(), "make_nidhi_repo failed: {:?}", out);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "3",
        "nidhi repo must have exactly 3 stashes"
    );
    let repo_str = repo.to_string_lossy().to_string();
    (dir, repo_str)
}

fn glance_png(h: &Harness, pane: &str, ctx: &str) -> Vec<u8> {
    let g = h
        .rpc_raw("pane.glance", serde_json::json!({ "pane_id": pane }))
        .expect_result(ctx);
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(g["png_base64"].as_str().expect("glance png"))
        .expect("decode png")
}

fn settle(h: &Harness, pane: &str, ctx: &str) {
    let s = h
        .rpc_raw(
            "pane.wait_settled",
            serde_json::json!({ "pane_id": pane, "quiet_ms": 400, "timeout_ms": 10_000 }),
        )
        .expect_result(ctx);
    assert_eq!(
        s["settled"],
        serde_json::Value::Bool(true),
        "{ctx}: expected settle"
    );
}

/// Dismiss nidhi's mandatory welcome screen (LENS-TEST-CHANGE approved
/// 2026-07-10, task 077 P6). nidhi 0.1.0-alpha.1 renders a "Press Enter to
/// continue" welcome card on EVERY launch — no CLI flag, env var, or config
/// key bypasses it (binary strings-scanned for `nidhi.*`/`NIDHI_*` keys) and
/// it never auto-advances (verified by an 8s idle probe). The BOUNDED
/// `wait_for` on the prompt text IS the assertion that the welcome screen
/// rendered — Enter is only sent after it is proven on screen, never
/// speculatively. The caller's original stash-list sentinel wait follows
/// unchanged, so T1/T2/T3's assertions and purpose are untouched.
fn dismiss_nidhi_welcome(h: &Harness, pane: &str, ctx: &str) {
    h.wait_for(pane, "Press Enter to continue", 10_000)
        .unwrap_or_else(|e| panic!("{ctx}: nidhi welcome screen never appeared: {e}"));
    // Enter (0x0d), same byte as `pane send-keys --data DQ==`.
    h.send_raw(pane, "\r");
}

// T1 — nidhi golden (rich + Unicode).
#[test]
fn t1_nidhi_golden() {
    if !skip_unless_bin("nidhi", "T1 nidhi golden") {
        return;
    }
    let h = Harness::new();
    let (_repo_dir, repo) = build_nidhi_repo(&h);

    let r = h
        .rpc_raw(
            "lens.run",
            serde_json::json!({
                "argv": ["nidhi", "-C", repo, "--no-animation", "--icons", "nerd"],
                "cols": 120, "rows": 40
            }),
        )
        .expect_result("T1 lens.run nidhi");
    let pane = r["pane_id"].as_str().expect("pane_id").to_string();

    dismiss_nidhi_welcome(&h, &pane, "T1");
    h.wait_for(&pane, "विवेचक", 10_000)
        .expect("T1: nidhi drew the stashes");
    settle(&h, &pane, "T1 settle");
    let png = glance_png(&h, &pane, "T1 glance");
    // p0-council-r1 major 2: this COLOR case must prove NO_COLOR is absent
    // from the pane environment — a near-grayscale render means the harness
    // (or daemon) leaked NO_COLOR into the scratch. Near-grayscale predicate
    // per the approved LENS-TEST-CHANGE (see is_near_grayscale_png).
    assert!(
        !is_near_grayscale_png(&png),
        "T1: truecolor nidhi render came out near-grayscale — NO_COLOR poisoning"
    );
    assert_png_golden(&h, &png, "t1_nidhi_nerd_color_120x40.png");

    h.rpc_raw("session.kill", serde_json::json!({ "id": r["session_id"] }));
}

// T2 — nidhi keyboard truth (selection move confined to the stash rows).
#[test]
fn t2_nidhi_keyboard_truth() {
    if !skip_unless_bin("nidhi", "T2 nidhi keyboard truth") {
        return;
    }
    let h = Harness::new();
    let (_repo_dir, repo) = build_nidhi_repo(&h);

    let r = h
        .rpc_raw(
            "lens.run",
            serde_json::json!({
                "argv": ["nidhi", "-C", repo, "--no-animation", "--icons", "nerd"],
                "cols": 120, "rows": 40
            }),
        )
        .expect_result("T2 lens.run nidhi");
    let pane = r["pane_id"].as_str().expect("pane_id").to_string();
    dismiss_nidhi_welcome(&h, &pane, "T2");
    h.wait_for(&pane, "विवेचक", 10_000).expect("T2: nidhi up");
    settle(&h, &pane, "T2 settle initial");

    let c = h
        .rpc_raw("pane.checkpoint", serde_json::json!({ "pane_id": pane }))
        .expect_result("T2 checkpoint")["revision"]
        .as_u64()
        .expect("rev");

    // `j` moves the selection down by one stash row.
    h.send_raw(&pane, "j");
    settle(&h, &pane, "T2 settle after j");
    let d = h
        .rpc_raw(
            "pane.diff_since",
            serde_json::json!({ "pane_id": pane, "since_revision": c }),
        )
        .expect_result("T2 diff j");
    assert!(
        d["cells_changed"].as_u64().unwrap() > 0,
        "T2: `j` must change cells"
    );
    // Confined to the stash-row band: the change must not span the whole screen.
    let bb = &d["bounding_box"];
    let row_span = bb["row_end"].as_u64().unwrap() - bb["row_start"].as_u64().unwrap();
    assert!(
        row_span <= 4,
        "T2: a single-line selection move must stay within the stash rows (row span {row_span})"
    );

    // `k` returns to the original selection.
    h.send_raw(&pane, "k");
    settle(&h, &pane, "T2 settle after k");
    let d = h
        .rpc_raw(
            "pane.diff_since",
            serde_json::json!({ "pane_id": pane, "since_revision": c }),
        )
        .expect_result("T2 diff k-return");
    assert_eq!(
        d["cells_changed"], 0,
        "T2: `k` must return to the checkpointed selection (zero net delta)"
    );

    h.rpc_raw("session.kill", serde_json::json!({ "id": r["session_id"] }));
}

// T3 — nidhi 4-way matrix (icons × color) + NO_COLOR near-grayscale anchor.
#[test]
fn t3_nidhi_matrix() {
    if !skip_unless_bin("nidhi", "T3 nidhi matrix") {
        return;
    }
    let h = Harness::new();
    let (_repo_dir, repo) = build_nidhi_repo(&h);

    struct Cell {
        icons: &'static str,
        no_color: bool,
        golden: &'static str,
    }
    let cells = [
        Cell {
            icons: "nerd",
            no_color: false,
            golden: "t3_nidhi_nerd_color.png",
        },
        Cell {
            icons: "ascii",
            no_color: false,
            golden: "t3_nidhi_ascii_color.png",
        },
        Cell {
            icons: "nerd",
            no_color: true,
            golden: "t3_nidhi_nerd_nocolor.png",
        },
        Cell {
            icons: "ascii",
            no_color: true,
            golden: "t3_nidhi_ascii_nocolor.png",
        },
    ];

    for cell in cells {
        let mut argv = vec![
            "nidhi".to_string(),
            "-C".to_string(),
            repo.clone(),
            "--no-animation".to_string(),
            "--icons".to_string(),
            cell.icons.to_string(),
        ];
        let mut env = serde_json::Map::new();
        if cell.no_color {
            argv.push("--no-color".to_string());
            env.insert("NO_COLOR".to_string(), serde_json::json!("1"));
        }
        let r = h
            .rpc_raw(
                "lens.run",
                serde_json::json!({ "argv": argv, "cols": 120, "rows": 40, "env": env }),
            )
            .expect_result("T3 lens.run nidhi");
        let pane = r["pane_id"].as_str().expect("pane_id").to_string();
        dismiss_nidhi_welcome(&h, &pane, "T3");
        h.wait_for(&pane, "विवेचक", 10_000).expect("T3: nidhi up");
        settle(&h, &pane, "T3 settle");
        let png = glance_png(&h, &pane, "T3 glance");
        // p0-council-r1 major 2 + the approved near-grayscale
        // LENS-TEST-CHANGE (see is_near_grayscale_png for the measured
        // anchors: OSC-11 theme bg spread 7, raster default bg [16,16,24]
        // spread 8): color cells are the DISCRIMINATING CONTROL — they must
        // FAIL the near-grayscale predicate with meaningful signal
        // (max_spread > 8 AND at least one pixel above spread 8; the exact
        // pixel count is deliberately NOT pinned). No-color cells must PASS
        // the predicate (every pixel spread <= 8).
        if !cell.no_color {
            let (max_spread, gt8) = png_channel_spread_stats(&png);
            assert!(
                max_spread > 8 && gt8 > 0,
                "T3: color render must carry meaningful color signal beyond \
                 the near-grayscale threshold (max_spread {max_spread}, \
                 pixels with spread > 8: {gt8}) — NO_COLOR poisoning ({})",
                cell.golden
            );
        }
        assert_png_golden(&h, &png, cell.golden);
        if cell.no_color {
            assert!(
                is_near_grayscale_png(&png),
                "T3: --no-color render must be NEAR-grayscale (every pixel \
                 channel spread <= 8) ({})",
                cell.golden
            );
        }
        h.rpc_raw("session.kill", serde_json::json!({ "id": r["session_id"] }));
    }
}

// T4 — vivecaka help card (network-free) at two sizes.
#[test]
fn t4_vivecaka_help_card() {
    if !skip_unless_bin("vivecaka", "T4 vivecaka help card") {
        return;
    }
    let h = Harness::new();

    for (cols, rows, golden) in [
        (100u16, 30u16, "t4_vivecaka_help_100x30.png"),
        (60, 20, "t4_vivecaka_help_60x20.png"),
    ] {
        let r = h
            .rpc_raw(
                "lens.run",
                serde_json::json!({ "argv": ["vivecaka", "--help"], "cols": cols, "rows": rows }),
            )
            .expect_result("T4 lens.run vivecaka");
        let pane = r["pane_id"].as_str().expect("pane_id").to_string();
        settle(&h, &pane, "T4 settle");
        let png = glance_png(&h, &pane, "T4 glance");
        assert_png_golden(&h, &png, golden);
        h.rpc_raw("session.kill", serde_json::json!({ "id": r["session_id"] }));
    }
}
