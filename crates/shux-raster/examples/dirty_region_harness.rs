use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;
use shux_raster::{RasterOptions, Rasterizer};
use shux_vt::{DirtyRegion, GridConfig, VirtualTerminal};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const TASK: &str = "074-shux-vt-dirty-region-tracking";
const FONT_SIZE: f32 = 14.0;
const DEFAULT_FG: [u8; 3] = [220, 220, 220];
const DEFAULT_BG: [u8; 3] = [16, 16, 24];

#[derive(Debug, Serialize)]
struct DirtyReport {
    schema_version: u32,
    task: &'static str,
    rows: usize,
    cols: usize,
    cursor_policy: &'static str,
    steps: Vec<DirtyStep>,
}

#[derive(Debug, Serialize)]
struct DirtyStep {
    label: &'static str,
    bytes: usize,
    dirty_regions: Vec<RegionJson>,
}

#[derive(Debug, Serialize)]
struct RegionJson {
    row: usize,
    start_col: usize,
    end_col: usize,
}

#[derive(Debug, Serialize)]
struct PerformanceReport {
    schema_version: u32,
    task: &'static str,
    replay_bytes: usize,
    replay_chunks: usize,
    replay_tracking_off_median_ms: f64,
    replay_tracking_on_median_ms: f64,
    replay_tracking_overhead_pct: f64,
    replay_tracking_overhead_budget_pct: f64,
    replay_tracking_status: &'static str,
    idle_take_iterations: usize,
    idle_take_avg_ms_per_frame: f64,
    idle_take_budget_ms_per_frame: f64,
    idle_take_status: &'static str,
    methodology: &'static str,
}

fn main() -> Result<()> {
    let qa_dir = std::env::var_os("SHUX_DIRTY_QA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".shux/qa").join(TASK));
    fs::create_dir_all(&qa_dir)?;

    let report = write_dirty_report_and_pngs(&qa_dir)?;
    fs::write(
        qa_dir.join("dirty-region-report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;

    let performance = measure_performance();
    fs::write(
        qa_dir.join("performance.json"),
        serde_json::to_vec_pretty(&performance)?,
    )?;

    if performance.replay_tracking_status != "pass" || performance.idle_take_status != "pass" {
        return Err("dirty-region performance budget failed".into());
    }

    Ok(())
}

fn write_dirty_report_and_pngs(qa_dir: &Path) -> Result<DirtyReport> {
    let rows = 30;
    let cols = 120;
    let steps = dirty_fixture_steps();

    let mut vt = VirtualTerminal::with_config(
        rows,
        cols,
        GridConfig {
            track_dirty: true,
            ..GridConfig::default()
        },
    );
    let mut report_steps = Vec::new();
    for (label, bytes) in &steps {
        vt.process(bytes);
        let dirty_regions = vt
            .take_dirty_regions()
            .into_iter()
            .map(region_json)
            .collect();
        report_steps.push(DirtyStep {
            label,
            bytes: bytes.len(),
            dirty_regions,
        });
    }

    let mut expected_vt = VirtualTerminal::with_config(
        rows,
        cols,
        GridConfig {
            track_dirty: false,
            ..GridConfig::default()
        },
    );
    for (_, bytes) in &steps {
        expected_vt.process(bytes);
    }

    let rasterizer = Rasterizer::new(FONT_SIZE)?;
    let opts = RasterOptions {
        fg_default: DEFAULT_FG,
        bg_default: DEFAULT_BG,
        ..RasterOptions::default()
    };

    let expected_grid = expected_vt.grid().clone_visible();
    let actual_grid = vt.grid().clone_visible();
    let expected = rasterizer.render(&expected_grid, &opts);
    let actual = rasterizer.render(&actual_grid, &opts);
    expected.save(qa_dir.join("dirty-120x30-expected.png"))?;
    actual.save(qa_dir.join("dirty-120x30-actual.png"))?;

    Ok(DirtyReport {
        schema_version: 1,
        task: TASK,
        rows,
        cols,
        cursor_policy: "cursor-only movement is outside grid dirty regions",
        steps: report_steps,
    })
}

fn dirty_fixture_steps() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (
            "print-header",
            b"\x1b[2J\x1b[1;1HSHUX DIRTY REGION TRACKING".to_vec(),
        ),
        ("seed-edit-row", b"\x1b[4;10Habcdef".to_vec()),
        (
            "insert-delete-erase",
            b"\x1b[4;12H\x1b[2@\x1b[4;18H\x1b[3P\x1b[4;20H\x1b[4X".to_vec(),
        ),
        (
            "scroll-region",
            b"\x1b[8;18r\x1b[18;1Hone\r\ntwo\r\nthree\r\nfour\x1b[r".to_vec(),
        ),
        (
            "default-colors",
            b"\x1b]10;#f2f2f2\x1b\\\x1b]11;#101820\x1b\\".to_vec(),
        ),
        (
            "sync-output-release",
            b"\x1b[?2026h\x1b[6;1Hhidden while synchronized\x1b[?2026l".to_vec(),
        ),
    ]
}

fn measure_performance() -> PerformanceReport {
    let stream = high_output_stream(100_000);
    let chunk_size = 65_536;
    let chunks: Vec<&[u8]> = stream.chunks(chunk_size).collect();

    let paired = paired_median_replay(&chunks, 9);
    let tracking_off = paired.tracking_off;
    let tracking_on = paired.tracking_on;
    let overhead_pct = paired.overhead_pct;

    let idle_iterations = 10_000;
    let idle = measure_idle_take(idle_iterations);
    let idle_avg_ms = duration_ms(idle) / idle_iterations as f64;

    PerformanceReport {
        schema_version: 1,
        task: TASK,
        replay_bytes: stream.len(),
        replay_chunks: chunks.len(),
        replay_tracking_off_median_ms: duration_ms(tracking_off),
        replay_tracking_on_median_ms: duration_ms(tracking_on),
        replay_tracking_overhead_pct: overhead_pct,
        replay_tracking_overhead_budget_pct: 5.0,
        replay_tracking_status: if overhead_pct <= 5.0 { "pass" } else { "fail" },
        idle_take_iterations: idle_iterations,
        idle_take_avg_ms_per_frame: idle_avg_ms,
        idle_take_budget_ms_per_frame: 2.0,
        idle_take_status: if idle_avg_ms <= 2.0 { "pass" } else { "fail" },
        methodology: "median paired high-output VT replay compares shux-vt parsing with GridConfig.track_dirty=false against track_dirty=true; repeated pairs filter scheduler noise, and idle measures clean take_dirty_regions on a 200x60 VT",
    }
}

fn replay_chunks(chunks: &[&[u8]], track_dirty: bool) -> Duration {
    let mut vt = VirtualTerminal::with_config(
        60,
        200,
        GridConfig {
            track_dirty,
            ..GridConfig::default()
        },
    );
    let start = Instant::now();
    for chunk in chunks {
        vt.process(chunk);
    }
    black_box(vt.capture_text(Some(1)));
    start.elapsed()
}

fn measure_idle_take(iterations: usize) -> Duration {
    let mut vt = VirtualTerminal::new(60, 200);
    vt.process(b"warm");
    vt.take_dirty_regions();
    let start = Instant::now();
    for _ in 0..iterations {
        black_box(vt.take_dirty_regions());
    }
    start.elapsed()
}

struct PairedReplay {
    tracking_off: Duration,
    tracking_on: Duration,
    overhead_pct: f64,
}

fn paired_median_replay(chunks: &[&[u8]], iterations: usize) -> PairedReplay {
    black_box(replay_chunks(chunks, false));
    black_box(replay_chunks(chunks, true));

    let mut off = Vec::with_capacity(iterations);
    let mut on = Vec::with_capacity(iterations);
    let mut ratios = Vec::with_capacity(iterations);
    for idx in 0..iterations {
        let (off_duration, on_duration) = if idx % 2 == 0 {
            let off_duration = replay_chunks(chunks, false);
            let on_duration = replay_chunks(chunks, true);
            (off_duration, on_duration)
        } else {
            let on_duration = replay_chunks(chunks, true);
            let off_duration = replay_chunks(chunks, false);
            (off_duration, on_duration)
        };
        off.push(off_duration);
        on.push(on_duration);
        if !off_duration.is_zero() {
            ratios.push(
                ((duration_ms(on_duration) / duration_ms(off_duration)) - 1.0).max(0.0) * 100.0,
            );
        }
    }
    off.sort();
    on.sort();
    ratios.sort_by(f64::total_cmp);
    PairedReplay {
        tracking_off: off[off.len() / 2],
        tracking_on: on[on.len() / 2],
        overhead_pct: ratios[ratios.len() / 2],
    }
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn high_output_stream(lines: usize) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1b[2J\x1b[H");
    for idx in 0..lines {
        out.extend_from_slice(format!("row {idx:04} ").as_bytes());
        out.extend_from_slice(b"abcdefghijklmnopqrstuvwxyz0123456789");
        out.extend_from_slice(b"\r\n");
        if idx % 25 == 0 {
            out.extend_from_slice(b"\x1b[5;10Htick\x1b[60;1H");
        }
    }
    out
}

fn region_json(region: DirtyRegion) -> RegionJson {
    RegionJson {
        row: region.row,
        start_col: region.cols.start,
        end_col: region.cols.end,
    }
}
