# 053 — Performance Optimization Campaign

**Status:** Pending
**Depends On:** 052
**Parallelizable With:** 054, 055

---

## Problem

The PRD defines strict P0 performance budgets (SS14.1) that must all be met before v1.0 release. These are not aspirational targets — they are hard requirements. Keypress latency (p99 <= 25ms), split pane (p99 <= 80ms), attach time (<= 150ms), throughput (>= 10K lines/s across 4 panes), idle memory (<= 80 MB for 10 panes), plugin call overhead (p99 <= 5ms), and Wasm instantiation (p99 <= 200us) each represent a user-visible quality bar. This task creates a benchmark suite that measures all budgets, identifies bottlenecks via profiling, and implements targeted optimizations until every budget is met.

## PRD Reference

- **SS 14.1** Performance budgets: Complete table of P0 metrics with targets and hard limits
- **SS 5.5** Virtual terminal grid: "Compact 4-byte cells for simple ASCII, extended storage for styled/wide characters" and "Lazy allocation: Scrollback is not pre-allocated"
- **SS 4.3** Architectural invariants: "Plugins that misbehave are killed, not tolerated"
- **SS 7.5** WIT performance: "sub-microsecond call overhead", "5ms budget per plugin call"
- **SS 17** M3: "Performance optimization against budgets"
- **SS 18** Success metrics: "All p99 budgets met — Benchmark suite"

---

## Files to Create

- `benches/keypress_latency.rs` — Benchmark: keypress to visible update
- `benches/split_pane.rs` — Benchmark: pane split operation
- `benches/attach.rs` — Benchmark: client attach time
- `benches/throughput.rs` — Benchmark: high-output throughput across panes
- `benches/memory.rs` — Memory measurement tool (not criterion — custom)
- `benches/plugin_call.rs` — Benchmark: Wasm plugin call overhead
- `benches/wasm_instantiation.rs` — Benchmark: Wasm module instantiation
- `benches/render_diff.rs` — Benchmark: compositor diff rendering
- `benches/vt_parser.rs` — Benchmark: VT parser throughput
- `benches/event_bus.rs` — Benchmark: event bus broadcast throughput
- `scripts/bench-all.sh` — Run all benchmarks and compare against budgets
- `scripts/flamegraph.sh` — Generate flamegraph for profiling
- `scripts/memory-profile.sh` — Memory profiling script

## Files to Modify

- `Cargo.toml` — Add criterion, dhat (memory profiling) to workspace dev-dependencies
- `crates/shux-vt/src/grid.rs` — Optimize cell representation
- `crates/shux-vt/src/scrollback.rs` — Lazy scrollback allocation
- `crates/shux-ui/src/compositor.rs` — Optimize render diffing
- `crates/shux-core/src/event_bus.rs` — Optimize broadcast throughput
- `crates/shux-plugin/src/host.rs` — Optimize plugin call path
- `docs/PROGRESS.md` — Mark task 053 complete

---

## Execution Steps

### Step 1: Set Up Criterion Benchmarks

Add criterion to workspace:

```toml
# Cargo.toml [workspace.dependencies]
criterion = { version = "0.5", features = ["html_reports"] }
dhat = "0.3"
```

### Step 2: Keypress Latency Benchmark

Create `benches/keypress_latency.rs`:

```rust
//! Benchmark: Keypress → visible update latency.
//!
//! PRD budget: p50 ≤ 8ms, p99 ≤ 25ms, hard limit p99 ≤ 50ms
//!
//! Measures the full pipeline:
//! 1. Input decode (crossterm event → Action)
//! 2. State mutation (Action → SessionGraph update)
//! 3. Render cycle (state → compositor diff → output buffer)
//!
//! Uses a headless test backend to avoid terminal I/O variance.

use criterion::{criterion_group, criterion_main, Criterion, BenchmarkId};
use std::time::Duration;

fn bench_keypress_to_render(c: &mut Criterion) {
    let mut group = c.benchmark_group("keypress_latency");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(1000);

    // Setup: headless daemon with a single pane
    let (mut engine, mut compositor) = setup_headless_single_pane();

    group.bench_function("single_char_input", |b| {
        b.iter(|| {
            // Simulate: receive 'a' keypress → decode → mutate → render
            let input = crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('a'),
                crossterm::event::KeyModifiers::empty(),
            );
            engine.handle_input(input);
            compositor.render_frame();
        });
    });

    group.bench_function("alt_hjkl_focus_switch", |b| {
        // Requires multi-pane setup
        let (mut engine, mut compositor) = setup_headless_four_panes();
        b.iter(|| {
            let input = crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('l'),
                crossterm::event::KeyModifiers::ALT,
            );
            engine.handle_input(input);
            compositor.render_frame();
        });
    });

    group.finish();
}

fn bench_input_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("input_decode");

    group.bench_function("single_key", |b| {
        let raw = b"\x1b[A"; // Up arrow
        b.iter(|| {
            decode_input(raw);
        });
    });

    group.bench_function("kitty_keyboard", |b| {
        let raw = b"\x1b[97;5u"; // Kitty: Ctrl+a
        b.iter(|| {
            decode_input(raw);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_keypress_to_render, bench_input_decode);
criterion_main!(benches);
```

### Step 3: Split Pane Benchmark

Create `benches/split_pane.rs`:

```rust
//! Benchmark: Pane split operation.
//!
//! PRD budget: p50 ≤ 25ms, p99 ≤ 80ms, hard limit p99 ≤ 150ms
//!
//! Measures: layout tree split + PTY spawn + VT allocation + re-render.

use criterion::{criterion_group, criterion_main, Criterion};

fn bench_split_pane(c: &mut Criterion) {
    let mut group = c.benchmark_group("split_pane");

    group.bench_function("vertical_split", |b| {
        b.iter_with_setup(
            || setup_single_pane_session(),
            |mut session| {
                session.split_pane(Direction::Vertical, 0.5);
            },
        );
    });

    group.bench_function("split_with_command", |b| {
        b.iter_with_setup(
            || setup_single_pane_session(),
            |mut session| {
                session.split_pane_with_command(
                    Direction::Horizontal,
                    0.5,
                    &["bash"],
                );
            },
        );
    });

    group.bench_function("many_splits_8_panes", |b| {
        b.iter_with_setup(
            || setup_single_pane_session(),
            |mut session| {
                for _ in 0..7 {
                    session.split_pane(Direction::Vertical, 0.5);
                }
            },
        );
    });

    group.finish();
}

criterion_group!(benches, bench_split_pane);
criterion_main!(benches);
```

### Step 4: Throughput Benchmark

Create `benches/throughput.rs`:

```rust
//! Benchmark: High-output throughput.
//!
//! PRD budget: ≥ 10K lines/s across 4 panes without UI lockup
//!
//! Simulates 4 panes each receiving rapid output and measures
//! the VT parser + grid update + compositor throughput.

use criterion::{criterion_group, criterion_main, Criterion, Throughput};

fn bench_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("throughput");

    // Generate realistic output: lines with ANSI color codes
    let line = format!(
        "\x1b[32m{}\x1b[0m some text with \x1b[1;34mcolors\x1b[0m and content\r\n",
        chrono_format_placeholder()
    );
    let batch = line.repeat(100); // 100 lines per batch

    group.throughput(Throughput::Elements(100));

    group.bench_function("single_pane_100_lines", |b| {
        let mut vt = VirtualTerminal::new(80, 24, 5000);
        b.iter(|| {
            vt.process_output(batch.as_bytes());
        });
    });

    group.bench_function("four_panes_100_lines_each", |b| {
        let mut vts: Vec<VirtualTerminal> = (0..4)
            .map(|_| VirtualTerminal::new(80, 24, 5000))
            .collect();
        b.iter(|| {
            for vt in &mut vts {
                vt.process_output(batch.as_bytes());
            }
        });
    });

    group.bench_function("vt_parser_raw_bytes", |b| {
        let raw = generate_ansi_stress_test(10_000);
        group.throughput(Throughput::Bytes(raw.len() as u64));
        let mut vt = VirtualTerminal::new(200, 50, 10_000);
        b.iter(|| {
            vt.process_output(&raw);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_throughput);
criterion_main!(benches);
```

### Step 5: Plugin Call Overhead Benchmark

Create `benches/plugin_call.rs`:

```rust
//! Benchmark: Plugin call overhead.
//!
//! PRD budget: p99 ≤ 5ms per call, kill at 100ms
//! Wasm instantiation: p50 ≤ 50μs, p99 ≤ 200μs
//! Wasm function call (warm): p99 ≤ 100μs

use criterion::{criterion_group, criterion_main, Criterion};

fn bench_plugin_call(c: &mut Criterion) {
    let mut group = c.benchmark_group("plugin_call");

    // Pre-compile and pre-instantiate a test plugin
    let (engine, module) = setup_wasm_engine_and_module();

    group.bench_function("warm_render_segment", |b| {
        let mut store = create_store(&engine);
        let instance = instantiate(&engine, &module, &mut store);
        b.iter(|| {
            call_render_segment(&mut store, &instance, "test_segment", 30);
        });
    });

    group.bench_function("warm_on_event", |b| {
        let mut store = create_store(&engine);
        let instance = instantiate(&engine, &module, &mut store);
        let event_json = r#"{"type":"pane.focused","pane_id":"test-123"}"#;
        b.iter(|| {
            call_on_event(&mut store, &instance, event_json);
        });
    });

    group.bench_function("wasm_instantiation_cold", |b| {
        b.iter(|| {
            let mut store = create_store(&engine);
            let _instance = instantiate(&engine, &module, &mut store);
        });
    });

    group.bench_function("wasm_instantiation_precompiled", |b| {
        let precompiled = engine.precompile_module(module.serialize().unwrap()).unwrap();
        b.iter(|| {
            let mut store = create_store(&engine);
            let module = unsafe { Module::deserialize(&engine, &precompiled) }.unwrap();
            let _instance = instantiate(&engine, &module, &mut store);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_plugin_call);
criterion_main!(benches);
```

### Step 6: Memory Profiling

Create `benches/memory.rs`:

```rust
//! Memory measurement: daemon idle memory.
//!
//! PRD budget: ≤ 80 MB RSS for 10 panes with 5K scrollback
//!
//! Not a criterion benchmark — this is a custom measurement tool
//! that creates a realistic daemon state and measures RSS.

fn main() {
    // Create daemon state: 10 panes, each with 5K lines of scrollback
    let mut session_graph = SessionGraph::new();

    // Create session with 2 windows, 5 panes each
    let session = session_graph.create_session("memory-test");
    for w in 0..2 {
        let window = session_graph.create_window(session.id, &format!("window-{}", w));
        for p in 0..5 {
            let pane = session_graph.split_pane(window.active_pane, Direction::Vertical);
            // Fill scrollback: 5000 lines * ~200 cols
            let line = format!(
                "{}: {}\r\n",
                p,
                "x".repeat(180)
            );
            for _ in 0..5000 {
                pane.vt.process_output(line.as_bytes());
            }
        }
    }

    // Measure RSS
    let rss_kb = get_rss_kb();
    let rss_mb = rss_kb as f64 / 1024.0;

    println!("╔════════════════════════════════════╗");
    println!("║  Memory Measurement                ║");
    println!("╟────────────────────────────────────╢");
    println!("║  Panes:     10                     ║");
    println!("║  Scrollback: 5000 lines each       ║");
    println!("║  RSS:       {:.1} MB{:>16}║", rss_mb,
        if rss_mb <= 80.0 { "PASS" } else if rss_mb <= 150.0 { "WARN" } else { "FAIL" }
    );
    println!("║  Budget:    ≤ 80 MB (goal)         ║");
    println!("║  Hard limit: ≤ 150 MB              ║");
    println!("╚════════════════════════════════════╝");

    if rss_mb > 150.0 {
        std::process::exit(1);
    }
}

#[cfg(target_os = "linux")]
fn get_rss_kb() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    for line in status.lines() {
        if line.starts_with("VmRSS:") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            return parts[1].parse().unwrap_or(0);
        }
    }
    0
}

#[cfg(target_os = "macos")]
fn get_rss_kb() -> u64 {
    use std::process::Command;
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap_or(0)
}
```

### Step 7: Bench-All Script

Create `scripts/bench-all.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail

echo "╔══════════════════════════════════════════════╗"
echo "║   shux Performance Budget Verification       ║"
echo "╚══════════════════════════════════════════════╝"
echo ""

PASS=0
FAIL=0
WARN=0

check_budget() {
    local name="$1"
    local value="$2"
    local target="$3"
    local hard_limit="$4"
    local unit="$5"

    if (( $(echo "$value <= $target" | bc -l) )); then
        echo "  ✓ $name: ${value}${unit} (target: ≤${target}${unit})"
        PASS=$((PASS + 1))
    elif (( $(echo "$value <= $hard_limit" | bc -l) )); then
        echo "  ! $name: ${value}${unit} (target: ≤${target}${unit}, limit: ≤${hard_limit}${unit})"
        WARN=$((WARN + 1))
    else
        echo "  ✗ $name: ${value}${unit} EXCEEDS hard limit ≤${hard_limit}${unit}"
        FAIL=$((FAIL + 1))
    fi
}

echo "Running criterion benchmarks..."
cargo bench --workspace -- --output-format json 2>/dev/null | tee /tmp/shux-bench.json

echo ""
echo "─── Budget Check ───"

# Parse criterion output and check each budget
# (Actual parsing logic depends on criterion JSON output format)

echo ""
echo "Running memory measurement..."
cargo run --release --bin memory-bench

echo ""
echo "═══════════════════════════════════"
echo "  PASS: $PASS  WARN: $WARN  FAIL: $FAIL"
if [ "$FAIL" -gt 0 ]; then
    echo "  BUDGET CHECK: FAILED"
    exit 1
else
    echo "  BUDGET CHECK: PASSED"
fi
```

### Step 8: Optimization — Compact Cell Representation

Optimize `crates/shux-vt/src/grid.rs` for memory efficiency:

```rust
/// Compact cell representation.
///
/// Simple ASCII cells use 4 bytes inline. Styled or wide characters
/// use an index into an extended storage arena.
///
/// Layout (4 bytes):
///   [0]: character byte (ASCII) or 0xFF (extended)
///   [1]: fg color index (palette) or 0xFF (truecolor → extended)
///   [2]: bg color index (palette) or 0xFF (truecolor → extended)
///   [3]: flags (bold, italic, underline, etc.)
///
/// For ~90% of cells (plain ASCII with palette colors), this saves
/// ~20 bytes per cell vs a full Cell struct.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CompactCell {
    data: [u8; 4],
}

impl CompactCell {
    pub const EMPTY: Self = Self { data: [b' ', 0, 0, 0] };

    pub fn ascii(ch: u8, fg: u8, bg: u8, flags: u8) -> Self {
        Self { data: [ch, fg, bg, flags] }
    }

    pub fn is_extended(&self) -> bool {
        self.data[0] == 0xFF
    }
}
```

### Step 9: Optimization — Lazy Scrollback

Optimize `crates/shux-vt/src/scrollback.rs`:

```rust
/// Lazy scrollback buffer that doesn't allocate until the pane
/// produces output that scrolls past the visible area.
///
/// PRD SS5.5: "Lazy allocation: Scrollback is not pre-allocated
/// for panes that haven't produced output"
pub struct LazyScrollback {
    inner: Option<VecDeque<Row>>,
    max_lines: usize,
}

impl LazyScrollback {
    pub fn new(max_lines: usize) -> Self {
        Self {
            inner: None,
            max_lines,
        }
    }

    pub fn push(&mut self, row: Row) {
        let buffer = self.inner.get_or_insert_with(|| {
            VecDeque::with_capacity(self.max_lines.min(1024))
        });
        if buffer.len() >= self.max_lines {
            buffer.pop_front();
        }
        buffer.push_back(row);
    }

    pub fn is_allocated(&self) -> bool {
        self.inner.is_some()
    }

    pub fn memory_bytes(&self) -> usize {
        self.inner.as_ref().map_or(0, |buf| {
            buf.iter().map(|row| row.memory_bytes()).sum::<usize>()
                + std::mem::size_of::<VecDeque<Row>>()
        })
    }
}
```

### Step 10: Optimization — Render Diff Batching

Optimize `crates/shux-ui/src/compositor.rs` to batch terminal writes:

```rust
/// Batch write optimization for the compositor.
///
/// Instead of writing individual cells, collect contiguous regions
/// with the same style and write them as a single string. This
/// reduces the number of escape sequences emitted.
pub fn write_diff_batched(
    writer: &mut impl std::io::Write,
    diff: &[(u16, u16, CompactCell)],
) -> std::io::Result<()> {
    if diff.is_empty() {
        return Ok(());
    }

    let mut batch_buf = String::with_capacity(diff.len() * 2);
    let mut current_row = u16::MAX;
    let mut current_col = u16::MAX;
    let mut current_fg = u8::MAX;
    let mut current_bg = u8::MAX;

    for &(row, col, cell) in diff {
        // Emit cursor move only if not contiguous
        if row != current_row || col != current_col {
            if !batch_buf.is_empty() {
                writer.write_all(batch_buf.as_bytes())?;
                batch_buf.clear();
            }
            write!(writer, "\x1b[{};{}H", row + 1, col + 1)?;
            current_row = row;
            current_col = col;
        }

        // Emit style change only if different
        if cell.data[1] != current_fg || cell.data[2] != current_bg {
            if !batch_buf.is_empty() {
                writer.write_all(batch_buf.as_bytes())?;
                batch_buf.clear();
            }
            // Emit SGR for new colors
            write!(writer, "\x1b[38;5;{}m\x1b[48;5;{}m", cell.data[1], cell.data[2])?;
            current_fg = cell.data[1];
            current_bg = cell.data[2];
        }

        batch_buf.push(cell.data[0] as char);
        current_col += 1;
    }

    if !batch_buf.is_empty() {
        writer.write_all(batch_buf.as_bytes())?;
    }

    Ok(())
}
```

---

## Verification

### Functional

```bash
# Run all benchmarks
./scripts/bench-all.sh
# Expected: all budgets met (PASS or WARN, no FAIL)

# Individual benchmarks
cargo bench --bench keypress_latency
cargo bench --bench throughput
cargo bench --bench plugin_call
cargo bench --bench wasm_instantiation

# Memory measurement
cargo run --release --bin memory-bench
# Expected: RSS ≤ 80 MB for 10 panes with 5K scrollback

# Generate flamegraph for profiling
./scripts/flamegraph.sh
# Expected: flamegraph.svg generated
```

### Tests

```bash
# Verify benchmarks compile
cargo bench --workspace --no-run

# Run the full test suite to verify no regressions
cargo nextest run --workspace

# Expected: all existing tests still pass after optimizations
```

---

## Completion Criteria

- [ ] Benchmark suite covers all P0 metrics from SS14.1
- [ ] Keypress latency: p50 <= 8ms, p99 <= 25ms
- [ ] Split pane: p50 <= 25ms, p99 <= 80ms
- [ ] Attach (< 10 panes): <= 150ms
- [ ] Throughput: >= 10K lines/s across 4 panes
- [ ] Daemon idle memory (10 panes, 5K scrollback): <= 80 MB goal
- [ ] Plugin call overhead: p99 <= 5ms
- [ ] Wasm instantiation: p50 <= 50us, p99 <= 200us
- [ ] Compact cell representation implemented (4 bytes for ASCII cells)
- [ ] Lazy scrollback allocation implemented (no alloc until needed)
- [ ] Render diff batching reduces escape sequence count
- [ ] `scripts/bench-all.sh` runs all benchmarks and reports budget status
- [ ] Flamegraph generation script works
- [ ] No performance regressions in existing tests after optimizations
- [ ] Criterion HTML reports generated for all benchmarks

---

## Commit Message

```
perf: benchmark suite and optimization campaign for P0 budgets

- Criterion benchmarks for keypress latency, split pane, throughput,
  plugin call overhead, Wasm instantiation, render diff, event bus
- Memory measurement tool for idle daemon RSS
- Compact 4-byte cell representation for ASCII (saves ~20 bytes/cell)
- Lazy scrollback allocation (zero memory until output scrolls)
- Render diff batching (fewer escape sequences per frame)
- bench-all.sh script verifying all P0 budgets from PRD SS14.1
- All performance budgets met
```

---

## Session Protocol

1. **Before starting:** Read PRD SS14.1 completely — write down every budget with its target and hard limit. Read SS5.5 for grid optimization hints. Set up criterion and verify it runs.
2. **During:** Write benchmarks first (Steps 2-6), measure current performance, then optimize (Steps 8-10). Profile before optimizing — do not guess. Use `cargo flamegraph` and `instruments` (macOS) to identify hot paths. After each optimization, re-run benchmarks to verify improvement and no regression.
3. **Measurement discipline:** Run benchmarks on a quiet machine (close browsers, disable notifications). Run each benchmark 3 times and take the median. Use criterion's statistical analysis.
4. **Optimization priorities:** Focus on the worst-performing budget first. If keypress latency is already met but memory is not, optimize memory. Do not over-optimize areas that already pass.
5. **After:** Run `./scripts/bench-all.sh` to verify all budgets. Run full test suite to verify no regressions. Update `docs/PROGRESS.md`. Update `CLAUDE.md` Learnings (create from task 000 template if missing) with profiling insights.
