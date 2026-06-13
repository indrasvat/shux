//! Script-driven status-bar segment runner.
//!
//! For each `[[statusbar.segment]]` in `~/.config/shux/config.toml` we
//! spawn a tokio task that runs the configured command on its
//! `interval_ms`, captures stdout, and stores the result behind a
//! cheap `Arc<RwLock<>>` keyed by segment index. The render loop reads
//! that map, parses each cached output through a 1-row VT to recover
//! ANSI colors, and emits `StatusSegment`s.
//!
//! Failure modes the runner has to handle gracefully:
//!   - Command not found (`starship` not installed)         → fallback text
//!   - Non-zero exit                                        → fallback text
//!   - Hang / runaway                                       → 1s timeout
//!   - Config reload changes the segment list               → restart all
//!
//! This is the spike implementation: minimal schema, single happy
//! path, but the fallback story is real so OOTB still looks good.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::process::Command;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use shux_core::config::{ConfigHandle, SegmentDef};
use shux_ui::{StatusBar, StatusSegment};
use shux_vt::{Cell, CellFlags, VirtualTerminal};

/// Per-segment cache: latest captured stdout (raw bytes including ANSI).
/// Kept simple — no need for atomic swap ceremonies.
#[derive(Clone, Default)]
pub struct SegmentCache {
    inner: Arc<RwLock<HashMap<usize, Vec<u8>>>>,
}

impl SegmentCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn get(&self, idx: usize) -> Vec<u8> {
        self.inner
            .read()
            .await
            .get(&idx)
            .cloned()
            .unwrap_or_default()
    }

    async fn set(&self, idx: usize, bytes: Vec<u8>) {
        self.inner.write().await.insert(idx, bytes);
    }

    /// Wait until each segment index in `0..expected_count` has a
    /// cache entry, or `timeout` elapses. Returns true on success,
    /// false on timeout. Used by the snapshot RPC path to bridge a
    /// cold-start race: when a snapshot fires right after daemon
    /// start (or a config reload), the runner tasks may not have
    /// completed their first tick yet, so `populate_bar` would see
    /// an empty cache and silently emit no segments. The exact-key
    /// check (not a length check) matches what `populate_bar`
    /// actually reads, so a sparse cache where index 1 is present
    /// but index 0 is missing keeps us waiting — codex round-4 nit.
    /// Polling at 25 ms is cheap; the timeout should slightly exceed
    /// the runner's per-command budget (1 s) so the runner's
    /// fallback write has room to land before we give up.
    pub async fn wait_for_first_outputs(&self, expected_count: usize, timeout: Duration) -> bool {
        if expected_count == 0 {
            return true;
        }
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            {
                let g = self.inner.read().await;
                if (0..expected_count).all(|i| g.contains_key(&i)) {
                    return true;
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// Test-only setter so other modules can pre-populate the cache.
    /// Keeps the production `set` module-private (only the runner task
    /// writes to the cache in real builds).
    #[cfg(test)]
    pub async fn set_for_test(&self, idx: usize, bytes: Vec<u8>) {
        self.set(idx, bytes).await;
    }
}

/// Spawn one runner task per segment in the current config; restart
/// everything whenever the config changes. `cancel` shuts every task
/// down on daemon exit.
pub fn spawn_segment_runners(config: ConfigHandle, cache: SegmentCache, cancel: CancellationToken) {
    tokio::spawn(async move {
        let change_notify = config.change_notify();
        loop {
            let cfg_snap = config.current();
            let segments = cfg_snap.statusbar.segment.clone();
            let group_cancel = cancel.child_token();
            let mut handles = Vec::new();

            for (idx, seg) in segments.iter().enumerate() {
                let seg = seg.clone();
                let c = cache.clone();
                let ct = group_cancel.clone();
                handles.push(tokio::spawn(async move {
                    run_one_segment(idx, seg, c, ct).await;
                }));
            }

            // Wait for either cancellation or a config change.
            let listener = change_notify.notified();
            tokio::select! {
                _ = cancel.cancelled() => {
                    group_cancel.cancel();
                    for h in handles { let _ = h.await; }
                    break;
                }
                _ = listener => {
                    // Config changed: tear down this group and respawn.
                    group_cancel.cancel();
                    for h in handles { let _ = h.await; }
                    debug!("statusbar runner: config changed, respawning segments");
                }
            }
        }
    });
}

/// One segment's run-loop: tick, exec, cache, repeat.
async fn run_one_segment(
    idx: usize,
    mut seg: SegmentDef,
    cache: SegmentCache,
    cancel: CancellationToken,
) {
    if seg.command.is_empty() {
        warn!(idx, "statusbar segment has empty command; skipping");
        return;
    }

    // If the user supplied an inline starship config, materialise it
    // to a tempfile and inject STARSHIP_CONFIG. The tempfile lives for
    // this segment's lifetime; on config reload the runner is torn down
    // and rebuilt, which gives us a clean rewrite. We do NOT delete the
    // file on drop — daemon shutdown wipes /tmp/shux-segment-* via
    // best-effort cleanup at startup (idempotent, cheap).
    let starship_tmp = if let Some(toml_text) = seg.starship_config.clone() {
        let path = std::env::temp_dir().join(format!("shux-segment-{idx}.toml"));
        match std::fs::write(&path, toml_text.as_bytes()) {
            Ok(()) => {
                seg.env
                    .entry("STARSHIP_CONFIG".to_string())
                    .or_insert_with(|| path.to_string_lossy().into_owned());
                apply_starship_statusbar_env_defaults(&mut seg);
                Some(path)
            }
            Err(e) => {
                warn!(idx, error = %e,
                    "statusbar segment: failed to write inline starship config");
                None
            }
        }
    } else {
        None
    };

    let interval = Duration::from_millis(seg.interval_ms.max(100));
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        // Run once per tick. Bound runtime so a hung script can't
        // starve the bar.
        let result = tokio::time::timeout(Duration::from_secs(1), run_segment_command(&seg)).await;

        let bytes = match result {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => {
                debug!(idx, error = %e, "statusbar segment failed");
                fallback_bytes(&seg)
            }
            Err(_) => {
                debug!(idx, "statusbar segment timed out");
                fallback_bytes(&seg)
            }
        };
        cache.set(idx, bytes).await;

        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tick.tick() => {}
        }
    }
    // Best-effort cleanup of the materialised starship config tempfile.
    if let Some(p) = starship_tmp {
        let _ = std::fs::remove_file(p);
    }
}

fn apply_starship_statusbar_env_defaults(seg: &mut SegmentDef) {
    // Starship defaults to Bash-shaped prompt output in many non-shell
    // spawns. Bash wraps non-printing escape sequences in `\[` / `\]`,
    // which a real PS1 consumes as metadata but shux's statusbar renders
    // literally. `cmd` mode emits plain ANSI, which is the contract this
    // runner parses.
    seg.env
        .entry("STARSHIP_SHELL".to_string())
        .or_insert_with(|| "cmd".to_string());
    seg.env
        .entry("TERM".to_string())
        .or_insert_with(|| "xterm-256color".to_string());
}

async fn run_segment_command(seg: &SegmentDef) -> std::io::Result<Vec<u8>> {
    let program = &seg.command[0];
    let args = &seg.command[1..];
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null());
    for (k, v) in &seg.env {
        cmd.env(k, v);
    }
    let out = cmd.output().await?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!(
            "exit {:?}",
            out.status.code()
        )));
    }
    Ok(out.stdout)
}

fn fallback_bytes(seg: &SegmentDef) -> Vec<u8> {
    seg.fallback.as_deref().unwrap_or("").as_bytes().to_vec()
}

/// Convert a cache map into `StatusSegment`s populating the bar's
/// three zones. Each segment's bytes are fed through a small
/// VirtualTerminal so we recover ANSI fg/bg/bold/etc. without hand-
/// rolling an SGR parser. The trailing newline / CR that prompts
/// usually emit is stripped.
pub async fn populate_bar(bar: &mut StatusBar, config: &ConfigHandle, cache: &SegmentCache) {
    let cfg = config.current();
    if cfg.statusbar.segment.is_empty() {
        return;
    }

    // Group segment indices by zone, in declaration order.
    let mut groups: HashMap<&'static str, Vec<usize>> = HashMap::new();
    for (idx, seg) in cfg.statusbar.segment.iter().enumerate() {
        let zone: &'static str = match seg.zone.to_ascii_lowercase().as_str() {
            "left" => "left",
            "center" | "centre" => "center",
            "right" => "right",
            _ => "left",
        };
        groups.entry(zone).or_default().push(idx);
    }

    for (zone, idxs) in groups.iter() {
        let mut zone_segments: Vec<StatusSegment> = Vec::new();
        for &idx in idxs {
            let bytes = cache.get(idx).await;
            let parsed = ansi_to_segments(&bytes);
            zone_segments.extend(parsed);
        }
        if zone_segments.is_empty() {
            continue;
        }
        match *zone {
            "left" => bar.left.extend(zone_segments),
            "center" => bar.center.extend(zone_segments),
            "right" => bar.right.extend(zone_segments),
            _ => {}
        }
    }
}

/// Feed `bytes` into a multi-row × N-col VT, then return one
/// `StatusSegment` per run of cells that share the same fg/bg/bold,
/// scanning the FIRST non-blank row of the rendered output. Empty
/// trailing cells are dropped.
///
/// Why multi-row: starship's default prompt ends with `\n` and a
/// chevron (`❯ `) on the next line. A 1-row VT would scroll on the
/// newline and we'd lose the meaningful first line. Rendering into a
/// taller VT and scanning the first non-blank row preserves the
/// status-info line — exactly the part you want in a status bar.
fn ansi_to_segments(bytes: &[u8]) -> Vec<StatusSegment> {
    if bytes.is_empty() {
        return Vec::new();
    }

    const VT_ROWS: usize = 6; // tall enough for starship's two-line default
    const VT_COLS: usize = 512; // wide enough that nothing wraps prematurely
    let mut vt = VirtualTerminal::new(VT_ROWS, VT_COLS);

    let mut payload: Vec<u8> = bytes.iter().copied().filter(|b| *b != b'\r').collect();
    while matches!(payload.last(), Some(b'\n')) {
        payload.pop();
    }
    vt.process(&payload);

    // Find the first row that has any non-default-colored / non-blank
    // cell. That's where the status content lives.
    let grid = vt.grid();
    let mut chosen = 0usize;
    'outer: for r in 0..VT_ROWS.min(grid.rows()) {
        let row = grid.visible_row(r);
        for i in 0..row.len() {
            let c = &row[i];
            if c.ch != ' ' || c.style.bg != shux_vt::Color::Default {
                chosen = r;
                break 'outer;
            }
        }
    }
    let row = grid.visible_row(chosen);
    let mut out: Vec<StatusSegment> = Vec::new();
    let mut current: Option<StatusSegment> = None;
    let row_len = row.len();
    let mut last_non_blank = 0usize;

    for i in 0..row_len {
        let cell = &row[i];
        if cell.ch != ' ' || cell.has_grapheme_payload() || cell.style.bg != shux_vt::Color::Default
        {
            last_non_blank = i + 1;
        }
    }

    for i in 0..last_non_blank {
        let cell = &row[i];
        let seg = cell_to_seg(cell);
        match &mut current {
            Some(c) if styles_match(c, &seg) => {
                c.text.push_str(&seg.text);
            }
            _ => {
                if let Some(c) = current.take() {
                    if !c.text.is_empty() {
                        out.push(c);
                    }
                }
                current = Some(seg);
            }
        }
    }
    if let Some(c) = current {
        if !c.text.is_empty() {
            out.push(c);
        }
    }
    out
}

fn cell_to_seg(cell: &Cell) -> StatusSegment {
    StatusSegment {
        text: cell.display_text().into_owned(),
        fg: vt_color(cell.style.fg),
        bg: vt_color(cell.style.bg),
        bold: cell.style.flags.contains(CellFlags::BOLD),
    }
}

fn styles_match(a: &StatusSegment, b: &StatusSegment) -> bool {
    a.fg == b.fg && a.bg == b.bg && a.bold == b.bold
}

fn vt_color(c: shux_vt::Color) -> Option<crossterm::style::Color> {
    match c {
        shux_vt::Color::Default => None,
        shux_vt::Color::Indexed(i) => Some(crossterm::style::Color::AnsiValue(i)),
        shux_vt::Color::Rgb(r, g, b) => Some(crossterm::style::Color::Rgb { r, g, b }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ansi_red_text_becomes_one_segment() {
        let bytes = b"\x1b[31mhello\x1b[0m";
        let segs = ansi_to_segments(bytes);
        assert!(!segs.is_empty());
        let combined: String = segs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(combined.trim(), "hello");
        // First segment should be red (Indexed(1))
        assert_eq!(
            segs[0].fg,
            Some(crossterm::style::Color::AnsiValue(1)),
            "first segment should carry the red SGR"
        );
    }

    #[test]
    fn test_ansi_to_segments_groups_by_style() {
        // "RED" + space + "GREEN", styles must change at the boundary.
        let bytes = b"\x1b[31mRED\x1b[0m \x1b[32mGREEN\x1b[0m";
        let segs = ansi_to_segments(bytes);
        // We expect at least 3 runs: RED, ' ', GREEN
        let texts: Vec<String> = segs.iter().map(|s| s.text.clone()).collect();
        let joined = texts.join("|");
        assert!(joined.contains("RED"));
        assert!(joined.contains("GREEN"));
    }

    #[test]
    fn test_ansi_to_segments_empty_input() {
        assert!(ansi_to_segments(b"").is_empty());
        // Pure whitespace and nothing else → nothing to render.
        assert!(ansi_to_segments(b"   ").is_empty());
    }

    #[test]
    fn test_ansi_strips_trailing_newline() {
        let bytes = b"\x1b[36mhi\x1b[0m\n";
        let segs = ansi_to_segments(bytes);
        let combined: String = segs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(combined.trim(), "hi");
    }

    #[test]
    fn starship_statusbar_defaults_request_raw_ansi_output() {
        let mut seg = SegmentDef {
            zone: "right".to_string(),
            command: vec!["starship".to_string(), "prompt".to_string()],
            env: HashMap::new(),
            starship_config: Some("add_newline = false".to_string()),
            interval_ms: 1_000,
            fallback: None,
        };

        apply_starship_statusbar_env_defaults(&mut seg);

        assert_eq!(
            seg.env.get("STARSHIP_SHELL").map(String::as_str),
            Some("cmd")
        );
        assert_eq!(
            seg.env.get("TERM").map(String::as_str),
            Some("xterm-256color")
        );
    }

    #[test]
    fn starship_statusbar_defaults_preserve_explicit_env() {
        let mut env = HashMap::new();
        env.insert("STARSHIP_SHELL".to_string(), "fish".to_string());
        env.insert("TERM".to_string(), "screen-256color".to_string());
        let mut seg = SegmentDef {
            zone: "right".to_string(),
            command: vec!["starship".to_string(), "prompt".to_string()],
            env,
            starship_config: Some("add_newline = false".to_string()),
            interval_ms: 1_000,
            fallback: None,
        };

        apply_starship_statusbar_env_defaults(&mut seg);

        assert_eq!(
            seg.env.get("STARSHIP_SHELL").map(String::as_str),
            Some("fish")
        );
        assert_eq!(
            seg.env.get("TERM").map(String::as_str),
            Some("screen-256color")
        );
    }

    #[tokio::test]
    async fn run_one_segment_injects_starship_env_defaults_for_inline_config() {
        let seg = SegmentDef {
            zone: "right".to_string(),
            command: vec!["env".to_string()],
            env: HashMap::new(),
            starship_config: Some("add_newline = false".to_string()),
            interval_ms: 10_000,
            fallback: None,
        };
        let cache = SegmentCache::new();
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run_one_segment(0, seg, cache.clone(), cancel.clone()));

        assert!(
            cache
                .wait_for_first_outputs(1, Duration::from_secs(2))
                .await,
            "segment runner did not publish first output"
        );

        cancel.cancel();
        task.await.unwrap();

        let output = String::from_utf8(cache.get(0).await).unwrap();
        assert!(output.contains("STARSHIP_SHELL=cmd"));
        assert!(output.contains("TERM=xterm-256color"));
        assert!(
            output.contains("STARSHIP_CONFIG="),
            "inline starship config should be materialized and exported"
        );
    }

    #[tokio::test]
    async fn wait_for_first_outputs_returns_true_immediately_when_zero_expected() {
        let cache = SegmentCache::new();
        assert!(
            cache
                .wait_for_first_outputs(0, Duration::from_millis(10))
                .await
        );
    }

    #[tokio::test]
    async fn wait_for_first_outputs_returns_true_when_already_populated() {
        let cache = SegmentCache::new();
        cache.set(0, b"x".to_vec()).await;
        assert!(
            cache
                .wait_for_first_outputs(1, Duration::from_millis(10))
                .await
        );
    }

    #[tokio::test]
    async fn wait_for_first_outputs_times_out_when_missing() {
        let cache = SegmentCache::new();
        // Expect two entries, only one present → must timeout.
        cache.set(0, b"x".to_vec()).await;
        assert!(
            !cache
                .wait_for_first_outputs(2, Duration::from_millis(100))
                .await
        );
    }

    #[tokio::test]
    async fn wait_for_first_outputs_requires_exact_indices_not_just_len() {
        // Sparse cache: index 1 populated, index 0 missing. `len() >= 1`
        // would falsely succeed; the exact-key check must keep waiting.
        let cache = SegmentCache::new();
        cache.set(1, b"y".to_vec()).await;
        assert!(
            !cache
                .wait_for_first_outputs(1, Duration::from_millis(100))
                .await,
            "wait should fail when index 0 is missing, even though len()>=1"
        );
    }

    #[tokio::test]
    async fn wait_for_first_outputs_unblocks_on_late_write() {
        let cache = SegmentCache::new();
        let c2 = cache.clone();
        // Background task writes the cache entry after a short delay.
        let writer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            c2.set(0, b"late".to_vec()).await;
        });
        let start = tokio::time::Instant::now();
        assert!(
            cache
                .wait_for_first_outputs(1, Duration::from_millis(500))
                .await
        );
        assert!(start.elapsed() < Duration::from_millis(300));
        writer.await.unwrap();
    }
}
