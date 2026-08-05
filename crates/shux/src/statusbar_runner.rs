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
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
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
        // Reclaim segment configs orphaned by a previous daemon that died
        // without cleanup (issue #105 hardening). Once per daemon, before any
        // runner materialises its own file.
        if let Ok(dir) = crate::daemon::runtime_dir() {
            sweep_stale_segment_configs(&dir);
        }
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

    // If the user supplied an inline starship config, materialise it into the
    // daemon's own per-user runtime directory (the 0700 dir that already holds
    // the socket) and inject STARSHIP_CONFIG. The file is created with
    // exclusive, symlink-refusing semantics at mode 0600; its handle is held
    // for this segment's lifetime (see `StarshipConfigFile`) and it is unlinked
    // when the task tears down — on config reload the runner is rebuilt, which
    // gives us a clean rewrite. Issue #105: this used to write to a fully
    // predictable path in shared temp with a symlink-following call, which a
    // local user could redirect onto any file the daemon can write.
    let starship_tmp = if let Some(toml_text) = seg.starship_config.clone() {
        match materialise_inline_starship_config(idx, toml_text.as_bytes()) {
            Ok(file) => {
                seg.env
                    .entry("STARSHIP_CONFIG".to_string())
                    .or_insert_with(|| file.path().to_string_lossy().into_owned());
                apply_starship_statusbar_env_defaults(&mut seg);
                Some(file)
            }
            Err(e) => {
                warn!(idx, error = %e,
                    "statusbar segment: failed to materialise inline starship config");
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
    // The materialised starship config (if any) is unlinked here as
    // `starship_tmp` drops — see `StarshipConfigFile::drop`.
    drop(starship_tmp);
}

/// A starship config materialised into the daemon's private runtime directory.
///
/// Issue #105: inline `starship_config` used to be written to a fully
/// predictable path in shared temp (`$TMPDIR/shux-segment-<idx>.toml`) with a
/// symlink-following `std::fs::write`. On a shared `/tmp` a local user could
/// pre-plant that path as a symlink and redirect the daemon's write onto any
/// file the daemon user can write — an arbitrary-file clobber primitive
/// (CWE-59). We now materialise into the same per-user 0700 runtime directory
/// that holds the socket, create the file with exclusive + no-follow semantics
/// at mode 0600, keep the open handle for the file's lifetime (never reopening
/// by name), and unlink it on teardown.
struct StarshipConfigFile {
    path: PathBuf,
    /// Held open for the file's lifetime; we never reopen by name. Dropping the
    /// handle closes the fd; `Drop` on the struct unlinks the path.
    _handle: File,
}

impl StarshipConfigFile {
    /// Materialise `contents` for segment `idx` inside `dir` — a directory the
    /// daemon owns (mode 0700). The filename carries the daemon PID so two
    /// concurrent daemons never share a file, and the create refuses to follow
    /// a symlink or open an existing entry (see `create_private_file`).
    fn materialise(dir: &Path, idx: usize, contents: &[u8]) -> std::io::Result<Self> {
        let path = dir.join(segment_config_name(std::process::id(), idx));
        let handle = create_private_file(&path, contents)?;
        Ok(Self {
            path,
            _handle: handle,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for StarshipConfigFile {
    fn drop(&mut self) {
        // `remove_file` unlinks the name itself (it never follows a symlink),
        // and the file lives in a dir only the daemon user can write, so this
        // is safe best-effort cleanup.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Resolve the daemon's private runtime directory and materialise `contents`
/// for segment `idx` inside it. `ensure_runtime_dir` creates the dir at mode
/// 0700 if needed (idempotent — the daemon already made it for the socket).
fn materialise_inline_starship_config(
    idx: usize,
    contents: &[u8],
) -> std::io::Result<StarshipConfigFile> {
    let dir = crate::daemon::ensure_runtime_dir().map_err(std::io::Error::other)?;
    StarshipConfigFile::materialise(&dir, idx, contents)
}

/// Per-daemon, per-segment filename. The PID keeps two concurrent daemons — or
/// a fresh daemon that inherited a crashed one's PID — from colliding on a
/// shared name.
fn segment_config_name(pid: u32, idx: usize) -> String {
    format!("segment-{pid}-{idx}.toml")
}

/// Reclaim `segment-<pid>-<idx>.toml` files left behind by daemons that died
/// without running `StarshipConfigFile::drop` (SIGKILL, power loss, OOM). The
/// PID-scoped name means a fresh daemon never reuses them, so without this sweep
/// they accumulate in the runtime dir across crashes. Only files whose embedded
/// PID is no longer alive are removed, so a (theoretical) concurrent daemon's
/// live file is never touched. The dir is 0700 — nothing hostile can appear
/// here — and `remove_file` unlinks by name without following symlinks; we
/// still restrict removal to plain files so a stray symlink is never traversed.
fn sweep_stale_segment_configs(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let me = std::process::id();
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        let Some(pid) = parse_segment_config_pid(name) else {
            continue;
        };
        // Never remove our own (not yet created) or a live daemon's file.
        if pid == me || pid_is_alive(pid) {
            continue;
        }
        // Regular files only — `file_type()` does not follow symlinks, so a
        // stray link is left in place rather than traversed.
        if matches!(entry.file_type(), Ok(ft) if ft.is_file()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Parse the PID out of a `segment-<pid>-<idx>.toml` name; `None` if it does not
/// match the exact shape (so unrelated files in the runtime dir are ignored).
fn parse_segment_config_pid(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("segment-")?.strip_suffix(".toml")?;
    let (pid, idx) = rest.split_once('-')?;
    // Require the index to be numeric too, so we only match our own names.
    idx.parse::<usize>().ok()?;
    pid.parse::<u32>().ok()
}

/// True unless the process is known-dead (`ESRCH`). `EPERM` (a live process we
/// can't signal) counts as alive — we err toward keeping files, never toward
/// deleting a live daemon's.
fn pid_is_alive(pid: u32) -> bool {
    use nix::errno::Errno;
    match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None) {
        Ok(()) => true,
        Err(Errno::ESRCH) => false,
        Err(_) => true,
    }
}

/// Create `path` as a fresh, private (0600) regular file with exclusive,
/// symlink-refusing semantics and write `contents` through the returned handle.
///
/// `O_NOFOLLOW` makes the open fail if `path` is a symlink; `create_new`
/// (`O_CREAT | O_EXCL`) makes it fail if `path` exists at all. Together they
/// guarantee we created a brand-new regular file and never traversed a link, so
/// a pre-planted symlink can never redirect the write. A pre-existing entry can
/// only be a stale file left by a previous daemon that shared our PID — the
/// parent dir is 0700, so nothing hostile can appear there — which we unlink
/// (`remove_file` does not follow symlinks) and recreate exactly once; a second
/// collision is a real error and propagates.
#[cfg(unix)]
fn create_private_file(path: &Path, contents: &[u8]) -> std::io::Result<File> {
    fn open_exclusive(path: &Path) -> std::io::Result<File> {
        OpenOptions::new()
            .write(true)
            .create_new(true) // O_CREAT | O_EXCL
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
            .open(path)
    }

    let mut handle = match open_exclusive(path) {
        Ok(handle) => handle,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            std::fs::remove_file(path)?;
            open_exclusive(path)?
        }
        Err(e) => return Err(e),
    };
    handle.write_all(contents)?;
    Ok(handle)
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
                if let Some(c) = current.take()
                    && !c.text.is_empty()
                {
                    out.push(c);
                }
                current = Some(seg);
            }
        }
    }
    if let Some(c) = current
        && !c.text.is_empty()
    {
        out.push(c);
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

    // ── issue #105: secure materialisation of inline starship config ──────────
    use std::os::unix::fs::PermissionsExt;

    /// A 0700 directory the "daemon" owns, standing in for the runtime dir.
    fn owner_dir() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        d
    }

    fn mode_of(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn create_private_file_writes_a_0600_regular_file() {
        let dir = owner_dir();
        let path = dir.path().join("segment-1-0.toml");
        let handle = create_private_file(&path, b"add_newline = false\n").unwrap();
        drop(handle);
        assert_eq!(std::fs::read(&path).unwrap(), b"add_newline = false\n");
        assert_eq!(mode_of(&path), 0o600, "materialised file must be 0600");
        assert!(
            !std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn create_private_file_never_writes_through_a_planted_symlink() {
        // The heart of the fix: a symlink at the target path must never be
        // followed, so the victim it points at is never clobbered.
        let dir = owner_dir();
        let victim = dir.path().join("victim");
        std::fs::write(&victim, b"KEEP ME").unwrap();
        let target = dir.path().join("segment-1-0.toml");
        std::os::unix::fs::symlink(&victim, &target).unwrap();

        let handle = create_private_file(&target, b"attacker payload").unwrap();
        drop(handle);

        // Victim is byte-for-byte intact — the write did not follow the link.
        assert_eq!(
            std::fs::read(&victim).unwrap(),
            b"KEEP ME",
            "write followed the symlink and clobbered the victim"
        );
        // The path now holds a fresh regular file with our content, not a link.
        assert!(
            !std::fs::symlink_metadata(&target)
                .unwrap()
                .file_type()
                .is_symlink(),
            "target is still a symlink"
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"attacker payload");
        assert_eq!(mode_of(&target), 0o600);
    }

    #[test]
    fn create_private_file_self_heals_a_stale_regular_file() {
        // A leftover file from a previous same-PID daemon is replaced in place.
        let dir = owner_dir();
        let path = dir.path().join("segment-1-0.toml");
        std::fs::write(&path, b"stale contents from a crashed daemon").unwrap();
        let handle = create_private_file(&path, b"fresh").unwrap();
        drop(handle);
        assert_eq!(std::fs::read(&path).unwrap(), b"fresh");
        assert_eq!(mode_of(&path), 0o600);
    }

    #[test]
    fn materialise_uses_a_pid_scoped_name_and_unlinks_on_drop() {
        let dir = owner_dir();
        let file = StarshipConfigFile::materialise(dir.path(), 0, b"x = 1\n").unwrap();
        let path = file.path().to_path_buf();
        assert!(path.exists());
        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            segment_config_name(std::process::id(), 0),
            "filename must be PID- and index-scoped"
        );
        assert_eq!(mode_of(&path), 0o600);
        drop(file);
        assert!(
            !path.exists(),
            "file must be unlinked when the handle drops"
        );
    }

    #[test]
    fn segment_config_names_are_distinct_across_daemons_and_segments() {
        // Two concurrent daemons (distinct PIDs) never share a file; neither do
        // two segments of the same daemon.
        assert_ne!(segment_config_name(1000, 0), segment_config_name(1001, 0));
        assert_ne!(segment_config_name(1000, 0), segment_config_name(1000, 1));
    }

    #[test]
    fn sweep_reclaims_dead_pid_files_but_keeps_live_and_unrelated() {
        let dir = owner_dir();
        // A guaranteed-dead PID: spawn a trivial child and reap it.
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead_pid = child.id();
        child.wait().unwrap();

        let me = std::process::id();
        let dead = dir.path().join(segment_config_name(dead_pid, 0));
        let live = dir.path().join(segment_config_name(me, 0));
        let unrelated = dir.path().join("config.toml");
        let malformed = dir.path().join("segment-notanumber-0.toml");
        for p in [&dead, &live, &unrelated, &malformed] {
            std::fs::write(p, b"x").unwrap();
        }

        sweep_stale_segment_configs(dir.path());

        assert!(!dead.exists(), "dead-PID orphan must be reclaimed");
        assert!(live.exists(), "a live daemon's file must be kept");
        assert!(unrelated.exists(), "non-segment files must be untouched");
        assert!(malformed.exists(), "non-matching names must be untouched");
    }

    #[test]
    fn sweep_never_follows_a_stray_symlink() {
        let dir = owner_dir();
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead_pid = child.id();
        child.wait().unwrap();

        let victim = dir.path().join("victim");
        std::fs::write(&victim, b"KEEP").unwrap();
        // A dead-PID-named symlink pointing at the victim.
        let link = dir.path().join(segment_config_name(dead_pid, 3));
        std::os::unix::fs::symlink(&victim, &link).unwrap();

        sweep_stale_segment_configs(dir.path());

        // We neither followed the link (victim intact) nor removed the target.
        assert_eq!(std::fs::read(&victim).unwrap(), b"KEEP");
    }

    #[test]
    fn parse_segment_config_pid_matches_only_our_shape() {
        assert_eq!(parse_segment_config_pid("segment-1234-0.toml"), Some(1234));
        assert_eq!(parse_segment_config_pid("segment-1-42.toml"), Some(1));
        assert_eq!(parse_segment_config_pid("segment-x-0.toml"), None);
        assert_eq!(parse_segment_config_pid("segment-1234-x.toml"), None);
        assert_eq!(parse_segment_config_pid("config.toml"), None);
        assert_eq!(parse_segment_config_pid("segment-1234-0.txt"), None);
    }

    #[test]
    fn materialise_two_segments_coexist_as_distinct_files() {
        let dir = owner_dir();
        let a = StarshipConfigFile::materialise(dir.path(), 0, b"a").unwrap();
        let b = StarshipConfigFile::materialise(dir.path(), 1, b"b").unwrap();
        assert_ne!(a.path(), b.path());
        assert!(a.path().exists() && b.path().exists());
        assert_eq!(std::fs::read(a.path()).unwrap(), b"a");
        assert_eq!(std::fs::read(b.path()).unwrap(), b"b");
    }

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
