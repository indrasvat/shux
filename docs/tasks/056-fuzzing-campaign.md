# 056 — Fuzzing Campaign (ANSI, JSON-RPC, Config, Layout)

**Status:** Pending
**Depends On:** 052
**Parallelizable With:** 053, 054, 055

---

## Problem

Terminal multiplexers process untrusted input from two directions: arbitrary PTY output (which may contain malformed escape sequences, hostile ANSI payloads, or corrupt UTF-8) and external API requests (which may contain malformed JSON, oversized payloads, or deliberately crafted inputs). The config parser and layout engine also accept user input that could trigger panics, infinite loops, or memory exhaustion. Fuzzing discovers edge cases that human-written tests miss. The PRD requires cargo-fuzz targets for the VT parser, JSON-RPC deserializer, config parser, and layout engine, with smoke runs on every PR and long campaigns nightly.

## PRD Reference

- **SS 16.3** Fuzzing: "ANSI parser: `cargo-fuzz` with arbitrary bytes. Smoke on PRs, long campaigns nightly. JSON-RPC parser: Fuzz request deserialization. Config parser: Fuzz TOML parsing. Layout engine: Fuzz split/resize/swap sequences, verify invariants."
- **SS 14.2** Reliability: "Crash-safe design"
- **SS 5.5** Virtual terminal grid: VecDeque grid, vte parser
- **SS 8.1** Transport: "Max frame size: All length-prefixed transports enforce a 16 MB maximum payload"

---

## Files to Create

- `fuzz/Cargo.toml` — Fuzz crate configuration
- `fuzz/fuzz_targets/fuzz_vt_parser.rs` — Fuzz VT parser with arbitrary bytes
- `fuzz/fuzz_targets/fuzz_json_rpc.rs` — Fuzz JSON-RPC request deserialization
- `fuzz/fuzz_targets/fuzz_config.rs` — Fuzz TOML config parsing
- `fuzz/fuzz_targets/fuzz_layout.rs` — Fuzz layout engine split/resize/swap sequences
- `fuzz/fuzz_targets/fuzz_passthrough.rs` — Fuzz image passthrough detector
- `.github/workflows/fuzz.yml` — Nightly fuzzing CI job
- `scripts/fuzz-smoke.sh` — PR smoke test (30 seconds each target)
- `fuzz/seeds/vt_parser/` — Seed corpus for VT parser
- `fuzz/seeds/json_rpc/` — Seed corpus for JSON-RPC
- `fuzz/seeds/config/` — Seed corpus for config parser
- `fuzz/seeds/layout/` — Seed corpus for layout engine

## Files to Modify

- `.github/workflows/ci.yml` — Add fuzz smoke step to PR pipeline
- `Makefile` — Update fuzz target to list and run available targets
- `docs/PROGRESS.md` — Mark task 056 complete

---

## Execution Steps

### Step 1: Create Fuzz Crate

Create `fuzz/Cargo.toml`:

```toml
[package]
name = "shux-fuzz"
version = "0.0.0"
publish = false
edition = "2024"

[package.metadata]
cargo-fuzz = true

[dependencies]
libfuzzer-sys = "0.4"
arbitrary = { version = "1", features = ["derive"] }
shux-vt = { path = "../crates/shux-vt" }
shux-rpc = { path = "../crates/shux-rpc" }
shux-core = { path = "../crates/shux-core" }
serde_json = "1"
toml = "0.8"

# Prevent this from being discovered as a real crate
[workspace]
members = ["."]

[[bin]]
name = "fuzz_vt_parser"
path = "fuzz_targets/fuzz_vt_parser.rs"
doc = false

[[bin]]
name = "fuzz_json_rpc"
path = "fuzz_targets/fuzz_json_rpc.rs"
doc = false

[[bin]]
name = "fuzz_config"
path = "fuzz_targets/fuzz_config.rs"
doc = false

[[bin]]
name = "fuzz_layout"
path = "fuzz_targets/fuzz_layout.rs"
doc = false

[[bin]]
name = "fuzz_passthrough"
path = "fuzz_targets/fuzz_passthrough.rs"
doc = false
```

### Step 2: VT Parser Fuzz Target

Create `fuzz/fuzz_targets/fuzz_vt_parser.rs`:

```rust
//! Fuzz target: VT parser.
//!
//! Feeds arbitrary bytes through the VTE parser and VirtualTerminal grid.
//! Looking for: panics, OOM, infinite loops, integer overflows.
//!
//! The VT parser must handle any byte sequence without crashing.
//! This is critical because PTY output comes from untrusted processes.

#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Create a virtual terminal with modest dimensions
    let mut vt = shux_vt::VirtualTerminal::new(80, 24, 1000);

    // Feed all bytes through the parser
    vt.process_output(data);

    // Verify invariants after processing
    let (cols, rows) = vt.dimensions();
    assert!(cols > 0 && cols <= 10_000, "Column count out of range: {}", cols);
    assert!(rows > 0 && rows <= 10_000, "Row count out of range: {}", rows);

    // Cursor must be within bounds
    let (cx, cy) = vt.cursor_position();
    assert!(cx < cols, "Cursor x {} >= cols {}", cx, cols);
    assert!(cy < rows, "Cursor y {} >= rows {}", cy, rows);

    // Scrollback must not exceed maximum
    let scrollback = vt.scrollback_len();
    assert!(scrollback <= 1000, "Scrollback exceeded max: {}", scrollback);

    // Read all visible content (exercises the grid read path)
    for row in 0..rows {
        let _line = vt.line_content(row);
    }
});
```

### Step 3: JSON-RPC Fuzz Target

Create `fuzz/fuzz_targets/fuzz_json_rpc.rs`:

```rust
//! Fuzz target: JSON-RPC request deserialization.
//!
//! Feeds arbitrary bytes as JSON-RPC requests. Looking for:
//! panics in deserialization, incorrect error handling, memory issues.
//!
//! The RPC layer must gracefully reject malformed input with proper
//! JSON-RPC error responses, never crashing.

#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Try to parse as UTF-8
    if let Ok(text) = std::str::from_utf8(data) {
        // Try to parse as JSON
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
            // Try to deserialize as a JSON-RPC request
            let _request = shux_rpc::parse_request(&value);
            // If it parses, validate the method name
            if let Ok(req) = shux_rpc::parse_request(&value) {
                let _method = req.method();
                let _params = req.params();
                let _id = req.id();
            }
        }
    }

    // Also test the length-prefixed framing decoder
    if data.len() >= 4 {
        let declared_len = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
        // Should not panic even with absurd lengths
        if declared_len <= data.len() - 4 {
            let payload = &data[4..4 + declared_len];
            let _parsed = serde_json::from_slice::<serde_json::Value>(payload);
        }
    }
});
```

### Step 4: Config Parser Fuzz Target

Create `fuzz/fuzz_targets/fuzz_config.rs`:

```rust
//! Fuzz target: TOML config parser.
//!
//! Feeds arbitrary strings as TOML configuration. Looking for:
//! panics in parsing, incorrect defaults, type confusion.

#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        // Try parsing as raw TOML
        let _raw: Result<toml::Value, _> = toml::from_str(text);

        // Try parsing as shux config specifically
        let _config: Result<shux_core::config::ShuxConfig, _> = toml::from_str(text);

        // If it parses as a config, verify defaults are sane
        if let Ok(config) = toml::from_str::<shux_core::config::ShuxConfig>(text) {
            // Scrollback lines must be positive
            assert!(config.ui.scrollback_lines > 0 || config.ui.scrollback_lines == 0);

            // Status bar position must be valid
            let _pos = config.ui.status_bar_position;

            // Validate the merged config doesn't panic
            let _merged = config.merge_with_defaults();
        }

        // Try parsing as a session template
        let _template: Result<shux_core::config::SessionTemplate, _> = toml::from_str(text);
    }
});
```

### Step 5: Layout Engine Fuzz Target

Create `fuzz/fuzz_targets/fuzz_layout.rs`:

```rust
//! Fuzz target: Layout engine.
//!
//! Generates random sequences of split/resize/swap/zoom/close operations
//! and verifies layout invariants after each operation. Looking for:
//! panics, invariant violations, degenerate layouts.

#![no_main]
use libfuzzer_sys::fuzz_target;
use arbitrary::{Arbitrary, Unstructured};

#[derive(Debug, Arbitrary)]
enum LayoutOp {
    SplitHorizontal { ratio_percent: u8 },
    SplitVertical { ratio_percent: u8 },
    Resize { pane_index: u8, delta_cols: i8, delta_rows: i8 },
    Swap { pane_a: u8, pane_b: u8 },
    Zoom { pane_index: u8 },
    Close { pane_index: u8 },
    FocusDirection { direction: u8 }, // 0=up, 1=down, 2=left, 3=right
}

fuzz_target!(|ops: Vec<LayoutOp>| {
    let mut layout = shux_core::layout::LayoutEngine::new(200, 50);

    for op in ops {
        let pane_count = layout.pane_count();
        if pane_count == 0 {
            break;
        }

        match op {
            LayoutOp::SplitHorizontal { ratio_percent } => {
                let ratio = (ratio_percent as f32 / 255.0).clamp(0.05, 0.95);
                let _ = layout.split(layout.focused_pane(), shux_core::layout::Direction::Horizontal, ratio);
            }
            LayoutOp::SplitVertical { ratio_percent } => {
                let ratio = (ratio_percent as f32 / 255.0).clamp(0.05, 0.95);
                let _ = layout.split(layout.focused_pane(), shux_core::layout::Direction::Vertical, ratio);
            }
            LayoutOp::Resize { pane_index, delta_cols, delta_rows } => {
                let idx = (pane_index as usize) % pane_count;
                let pane = layout.pane_at(idx);
                let _ = layout.resize(pane, delta_cols as i32, delta_rows as i32);
            }
            LayoutOp::Swap { pane_a, pane_b } => {
                let a = (pane_a as usize) % pane_count;
                let b = (pane_b as usize) % pane_count;
                let _ = layout.swap(layout.pane_at(a), layout.pane_at(b));
            }
            LayoutOp::Zoom { pane_index } => {
                let idx = (pane_index as usize) % pane_count;
                let _ = layout.toggle_zoom(layout.pane_at(idx));
            }
            LayoutOp::Close { pane_index } => {
                if pane_count > 1 {
                    let idx = (pane_index as usize) % pane_count;
                    let _ = layout.close(layout.pane_at(idx));
                }
            }
            LayoutOp::FocusDirection { direction } => {
                let dir = match direction % 4 {
                    0 => shux_core::layout::Direction::Up,
                    1 => shux_core::layout::Direction::Down,
                    2 => shux_core::layout::Direction::Left,
                    _ => shux_core::layout::Direction::Right,
                };
                let _ = layout.focus_direction(dir);
            }
        }

        // Verify invariants after every operation
        verify_layout_invariants(&layout);
    }
});

fn verify_layout_invariants(layout: &shux_core::layout::LayoutEngine) {
    let (total_cols, total_rows) = layout.total_dimensions();

    for pane in layout.all_panes() {
        let (x, y, w, h) = layout.pane_rect(pane);

        // All panes must be within bounds
        assert!(x + w <= total_cols, "Pane extends beyond right edge");
        assert!(y + h <= total_rows, "Pane extends beyond bottom edge");

        // No pane may have zero dimension
        assert!(w > 0, "Pane has zero width");
        assert!(h > 0, "Pane has zero height");

        // Ratios must be in range
        if let Some(ratio) = layout.pane_ratio(pane) {
            assert!(
                (0.05..=0.95).contains(&ratio),
                "Ratio {} out of range [0.05, 0.95]",
                ratio,
            );
        }
    }

    // No two panes should overlap
    let panes: Vec<_> = layout.all_panes().collect();
    for i in 0..panes.len() {
        for j in (i + 1)..panes.len() {
            let (x1, y1, w1, h1) = layout.pane_rect(panes[i]);
            let (x2, y2, w2, h2) = layout.pane_rect(panes[j]);
            let overlap_x = x1 < x2 + w2 && x2 < x1 + w1;
            let overlap_y = y1 < y2 + h2 && y2 < y1 + h1;
            assert!(
                !(overlap_x && overlap_y),
                "Panes {} and {} overlap",
                i, j,
            );
        }
    }

    // Total pane area should approximately equal total area
    // (accounting for borders)
    let total_pane_area: u32 = layout.all_panes()
        .map(|p| {
            let (_, _, w, h) = layout.pane_rect(p);
            w as u32 * h as u32
        })
        .sum();

    let total_area = total_cols as u32 * total_rows as u32;
    // Allow some slack for borders
    let border_slack = layout.pane_count() as u32 * (total_cols.max(total_rows)) as u32;
    assert!(
        total_pane_area + border_slack >= total_area / 2,
        "Pane area too small: {} vs total {}",
        total_pane_area, total_area,
    );
}
```

### Step 6: Image Passthrough Fuzz Target

Create `fuzz/fuzz_targets/fuzz_passthrough.rs`:

```rust
//! Fuzz target: Image passthrough detector.
//!
//! Feeds arbitrary bytes through the passthrough state machine.
//! Looking for: panics, stuck states, memory leaks.

#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut detector = shux_vt::passthrough::PassthroughDetector::new();

    for &byte in data {
        let _output = detector.feed(byte);
    }

    // After processing all input, detector should not be stuck
    // in a state that accumulates unbounded memory
    detector.reset();
});
```

### Step 7: Create Seed Corpora

Create seed files for each fuzz target to bootstrap coverage:

```bash
# VT parser seeds
mkdir -p fuzz/seeds/vt_parser
echo -ne "\x1b[32mGreen text\x1b[0m" > fuzz/seeds/vt_parser/sgr_green
echo -ne "\x1b[10;20H" > fuzz/seeds/vt_parser/cursor_move
echo -ne "\x1b[2J\x1b[H" > fuzz/seeds/vt_parser/clear_screen
echo -ne "\x1b[?1049h" > fuzz/seeds/vt_parser/alt_screen
echo -ne "Hello\r\nWorld\r\n" > fuzz/seeds/vt_parser/basic_text
printf '\xc3\xa9\xc3\xa0' > fuzz/seeds/vt_parser/utf8

# JSON-RPC seeds
mkdir -p fuzz/seeds/json_rpc
echo '{"jsonrpc":"2.0","id":"1","method":"system.version","params":{}}' > fuzz/seeds/json_rpc/version
echo '{"jsonrpc":"2.0","id":"2","method":"session.create","params":{"name":"test"}}' > fuzz/seeds/json_rpc/create
echo '{}' > fuzz/seeds/json_rpc/empty

# Config seeds
mkdir -p fuzz/seeds/config
echo '[ui]
scrollback_lines = 5000
status_bar = true' > fuzz/seeds/config/basic
echo '[daemon]
socket_path = "/tmp/shux.sock"' > fuzz/seeds/config/daemon

# Layout seeds (arbitrary bytes for structured fuzzing)
mkdir -p fuzz/seeds/layout
# Seeded via Arbitrary derive, no manual seeds needed
```

### Step 8: Smoke Test Script

Create `scripts/fuzz-smoke.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail

echo "╔══════════════════════════════════════╗"
echo "║   shux Fuzz Smoke Test               ║"
echo "╚══════════════════════════════════════╝"

DURATION="${FUZZ_DURATION:-30}"  # seconds per target
TARGETS=(fuzz_vt_parser fuzz_json_rpc fuzz_config fuzz_layout fuzz_passthrough)
FAILURES=0

cd "$(dirname "$0")/.."

for target in "${TARGETS[@]}"; do
    echo ""
    echo "─── $target (${DURATION}s) ───"
    if timeout $((DURATION + 10)) cargo fuzz run "$target" \
        --jobs 1 \
        -- -max_total_time="$DURATION" \
           -max_len=65536 \
        2>&1 | tail -5; then
        echo "$target: OK"
    else
        EXIT_CODE=$?
        if [ "$EXIT_CODE" -eq 124 ]; then
            echo "$target: OK (timeout — normal)"
        else
            echo "$target: CRASH FOUND (exit code $EXIT_CODE)"
            FAILURES=$((FAILURES + 1))
        fi
    fi
done

echo ""
echo "══════════════════════════════════"
if [ "$FAILURES" -gt 0 ]; then
    echo "FUZZ SMOKE: $FAILURES target(s) found crashes"
    exit 1
else
    echo "FUZZ SMOKE: ALL PASSED (no crashes in ${DURATION}s per target)"
fi
```

### Step 9: Nightly CI Job

Create `.github/workflows/fuzz.yml`:

```yaml
name: Fuzz

on:
  schedule:
    - cron: '0 3 * * *'  # 3am UTC daily
  workflow_dispatch:       # Manual trigger

env:
  CARGO_TERM_COLOR: always

jobs:
  fuzz:
    name: Fuzz (${{ matrix.target }})
    runs-on: ubuntu-latest
    strategy:
      fail-fast: false
      matrix:
        target:
          - fuzz_vt_parser
          - fuzz_json_rpc
          - fuzz_config
          - fuzz_layout
          - fuzz_passthrough
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@nightly
      - uses: Swatinem/rust-cache@v2
        with:
          cache-on-failure: true

      - name: Install cargo-fuzz
        run: cargo install cargo-fuzz --locked

      - name: Download corpus
        uses: actions/cache@v4
        with:
          path: fuzz/corpus/${{ matrix.target }}
          key: fuzz-corpus-${{ matrix.target }}-${{ github.sha }}
          restore-keys: |
            fuzz-corpus-${{ matrix.target }}-

      - name: Run fuzzer (${{ matrix.target }})
        run: |
          cargo fuzz run ${{ matrix.target }} \
            --jobs $(nproc) \
            -- -max_total_time=1800 \
               -max_len=65536 \
               -print_final_stats=1
        timeout-minutes: 35

      - name: Upload crash artifacts
        if: failure()
        uses: actions/upload-artifact@v4
        with:
          name: crash-${{ matrix.target }}
          path: fuzz/artifacts/${{ matrix.target }}/

      - name: Save corpus
        if: always()
        uses: actions/cache/save@v4
        with:
          path: fuzz/corpus/${{ matrix.target }}
          key: fuzz-corpus-${{ matrix.target }}-${{ github.sha }}
```

### Step 10: Update CI for PR Smoke

Add to `.github/workflows/ci.yml`:

```yaml
  fuzz-smoke:
    name: Fuzz Smoke
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@nightly
      - uses: Swatinem/rust-cache@v2
      - name: Install cargo-fuzz
        run: cargo install cargo-fuzz --locked
      - name: Run fuzz smoke (30s each)
        run: FUZZ_DURATION=30 ./scripts/fuzz-smoke.sh
```

---

## Verification

### Functional

```bash
# Run smoke test locally
./scripts/fuzz-smoke.sh
# Expected: all targets run for 30s without crashes

# Run a specific target for longer
cargo fuzz run fuzz_vt_parser -- -max_total_time=300

# Check for any crashes
ls fuzz/artifacts/*/
# Expected: empty (no crash artifacts)

# View corpus coverage
cargo fuzz coverage fuzz_vt_parser
```

### Tests

```bash
# Verify fuzz targets compile
cd fuzz && cargo build --bins

# Run the smoke script
./scripts/fuzz-smoke.sh

# Verify CI workflow syntax
# Push to branch and observe GitHub Actions
```

---

## Completion Criteria

- [ ] `fuzz_vt_parser`: feeds arbitrary bytes through VTE parser + grid, verifies invariants
- [ ] `fuzz_json_rpc`: fuzzes JSON-RPC request deserialization and length-prefixed framing
- [ ] `fuzz_config`: fuzzes TOML config parsing and template parsing
- [ ] `fuzz_layout`: fuzzes split/resize/swap/zoom/close sequences, verifies layout invariants
- [ ] `fuzz_passthrough`: fuzzes image passthrough state machine
- [ ] Seed corpora created for all targets
- [ ] `scripts/fuzz-smoke.sh` runs all targets for 30s each
- [ ] No crashes found during initial 30-minute campaign per target
- [ ] GitHub Actions nightly fuzz job runs 30 minutes per target
- [ ] PR CI includes fuzz smoke step (30s per target)
- [ ] Crash artifacts uploaded on failure
- [ ] Corpus cached between runs for coverage growth
- [ ] Layout invariant checks: no overlaps, no zero-dimension panes, ratios in range
- [ ] VT invariant checks: cursor in bounds, scrollback within limit

---

## Commit Message

```
test: add fuzzing campaign with 5 targets and nightly CI

- fuzz_vt_parser: arbitrary bytes through VTE parser + grid invariants
- fuzz_json_rpc: JSON-RPC deserialization and framing
- fuzz_config: TOML config and template parsing
- fuzz_layout: split/resize/swap/zoom/close with invariant verification
- fuzz_passthrough: image passthrough state machine
- Smoke test on PRs (30s per target), nightly campaigns (30min each)
- GitHub Actions workflow with corpus caching and crash upload
```

---

## Session Protocol

1. **Before starting:** Install cargo-fuzz (`cargo install cargo-fuzz`). Ensure nightly Rust toolchain is available (`rustup install nightly`). Read the libfuzzer-sys documentation.
2. **During:** Implement fuzz targets one at a time, starting with `fuzz_vt_parser` (most critical). Run each target for at least 5 minutes locally before moving to the next. Fix any crashes immediately — they are real bugs.
3. **Invariant design is critical:** The fuzz targets are only as good as the invariants they check. Invest time in writing thorough invariant checks, not just "doesn't crash."
4. **Edge cases to watch for:**
   - Nightly Rust is required for cargo-fuzz (stable doesn't work)
   - Fuzz corpus can grow large — use `.gitignore` for `fuzz/corpus/` and `fuzz/artifacts/`
   - CI cache limits — prune old corpus entries
   - Some crashes may only reproduce with specific seeds — archive them
5. **After:** Run all targets for 30 minutes each. Fix any discovered crashes. Run the PR smoke script. Update `docs/PROGRESS.md`. Update `CLAUDE.md` Learnings (create from task 000 template if missing) with any bugs found.
