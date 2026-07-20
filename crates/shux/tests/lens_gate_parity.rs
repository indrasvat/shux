//! Task 079 — frozen parity corpus (GATE lane; `GATE-TEST-CHANGE:` to touch).
//!
//! Proves the extraction of `compute_lens_diff` → `shux_vt::diff_frames` preserved
//! semantics EXACTLY, without being self-referential (council #3 / design D6). The
//! oracle in `.shux/fixtures/lens-gate/parity/<n>.diff.json` was minted by the OLD
//! `compute_lens_diff` over live grids BEFORE the function was deleted (generator:
//! `main.rs::tests::gen_lens_gate_parity_corpus`, now removed with the old fn). Here
//! we reload each frozen frame pair, run `diff_frames` over the golden `try_view`
//! path, and assert the serialized `FrameDiff` reproduces the frozen oracle
//! bit-for-bit. The frozen data is the independent oracle — not the live moved fn.

use std::path::PathBuf;

use shux_vt::{FrameDiff, FrameEnvelope};

fn parity_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.shux/fixtures/lens-gate/parity")
}

/// Canonical (sorted-key) pretty JSON — the exact form the generator froze.
fn canon(fd: &FrameDiff) -> String {
    serde_json::to_string_pretty(&serde_json::to_value(fd).expect("FrameDiff→Value"))
        .expect("Value→string")
}

fn load_env(path: &std::path::Path) -> FrameEnvelope {
    let json =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    FrameEnvelope::from_canonical_json(&json)
        .unwrap_or_else(|e| panic!("parse {}: {e:?}", path.display()))
}

/// Every `<name>.diff.json` in the corpus is reproduced bit-for-bit by
/// `diff_frames` over the reloaded `<name>.a.json` / `<name>.b.json` frames.
#[test]
fn diff_frames_reproduces_frozen_parity_corpus() {
    let dir = parity_dir();
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read dir {}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|f| f.strip_suffix(".diff.json").map(str::to_string))
        .collect();
    names.sort();
    assert!(
        names.len() >= 11,
        "expected the full parity corpus (≥11 scenarios), found {}: {names:?}",
        names.len()
    );

    for name in &names {
        let a = load_env(&dir.join(format!("{name}.a.json")));
        let b = load_env(&dir.join(format!("{name}.b.json")));
        let va = a
            .try_view()
            .unwrap_or_else(|e| panic!("{name}.a not canonical: {e:?}"));
        let vb = b
            .try_view()
            .unwrap_or_else(|e| panic!("{name}.b not canonical: {e:?}"));
        let got = canon(&shux_vt::diff_frames(&va, &vb));

        let want = std::fs::read_to_string(dir.join(format!("{name}.diff.json")))
            .unwrap_or_else(|e| panic!("read {name}.diff.json: {e}"));
        assert_eq!(
            got, want,
            "{name}: diff_frames must reproduce the frozen oracle bit-for-bit"
        );
    }
}
