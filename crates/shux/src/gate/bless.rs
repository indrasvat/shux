//! The approval-gated golden writer (task §5/§7, council #3). `--update` and
//! `--on-missing create` route here. A bless is a PRIVILEGED WRITE: it commits captured
//! terminal state into the repo, so every write passes a guard set BEFORE any byte lands:
//!
//!   1. CI mode → refuse (the driver checks first; re-checked here defensively).
//!   2. Dirty golden tree (`git status --porcelain -- <golden-dir>` non-empty: a tracked
//!      change OR a target that exists untracked) → refuse.
//!   3. Pre-bless SECRET SCAN of the material being committed (captures + names + reason)
//!      → refuse, reporting rule IDs only (never the secret).
//!   4. Only fail / missing / stale frames are blessable; a passing frame is a no-op and
//!      an xfail-green frame is NEVER blessed silently.
//!
//! Writes are containment-checked (canonical root), symlink-refused at the target, and
//! atomic (temp-in-dir + rename). A successful bless appends who/when/why to
//! `BASELINE-APPROVAL.md` and writes a changed-golden manifest for PR review.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use shux_raster::{
    Rasterizer, encode_png, os_arch, pixel_baseline_path, png_sha256, render_envelope, rgba_sha256,
};
use shux_vt::{Fingerprint, FrameEnvelope, GateStatus, ScenarioReport, Tier};

use super::compare::{cell_json_path, fp_path};
use super::driver::{GateRunOptions, is_ci};
use super::outcome::{FrameOutcome, RunOutcome};
use super::scenario::Scenario;
use super::secrets;

const FONT_SIZE: f32 = 16.0;

/// The result of a bless attempt.
pub enum BlessOutcome {
    /// A guard tripped; no byte was written. Carries a privacy-safe reason.
    Refused(String),
    /// Goldens were written; carries the changed-golden manifest.
    Blessed(BlessManifest),
}

/// One blessed frame, for the PR-review manifest + `apply_blessed`.
pub struct BlessedEntry {
    pub name: String,
    pub tier: Tier,
    pub old_fingerprint: Option<String>,
    pub new_fingerprint: String,
}

/// The changed-golden manifest (task §5): what was re-blessed, for PR review.
pub struct BlessManifest {
    pub entries: Vec<BlessedEntry>,
}

/// Re-bless the failing frames selected by `--update [failing|<name>]`.
pub fn run_update(
    scenario: &Scenario,
    outcome: &RunOutcome,
    reports: &[ScenarioReport],
    golden_dir: &Path,
    selector: &str,
    opts: &GateRunOptions,
) -> anyhow::Result<BlessOutcome> {
    let statuses = frame_statuses(reports);
    let targets = match select_targets(outcome, &statuses, selector) {
        Ok(t) => t,
        Err(reason) => return Ok(BlessOutcome::Refused(reason)),
    };
    write_targets(scenario, outcome, golden_dir, &targets, opts)
}

/// Write first goldens for every `missing_golden` frame (`--on-missing create`).
pub fn create_missing(
    scenario: &Scenario,
    outcome: &RunOutcome,
    reports: &[ScenarioReport],
    golden_dir: &Path,
    opts: &GateRunOptions,
) -> anyhow::Result<BlessOutcome> {
    let statuses = frame_statuses(reports);
    let targets: Vec<usize> = (0..outcome.frames.len())
        .filter(|&i| statuses.get(i) == Some(&GateStatus::MissingGolden))
        .collect();
    write_targets(scenario, outcome, golden_dir, &targets, opts)
}

/// After a successful bless, mark the blessed frames `pass` in the report and recompute
/// the scenario rollup (the golden now equals the capture, so a re-run would pass).
pub fn apply_blessed(reports: &mut [ScenarioReport], manifest: &BlessManifest) {
    let blessed: std::collections::HashSet<&str> =
        manifest.entries.iter().map(|e| e.name.as_str()).collect();
    for sr in reports.iter_mut() {
        for f in sr.frames.iter_mut() {
            if blessed.contains(f.name.as_str()) {
                f.status = GateStatus::Pass;
                f.reason = Some("blessed".to_string());
            }
        }
        let rolled = sr
            .frames
            .iter()
            .fold(GateStatus::Pass, |acc, f| acc.worst(f.status));
        sr.status = rolled;
        sr.note = Some(match sr.note.take() {
            Some(n) => format!("{n}; blessed {} golden(s)", manifest.entries.len()),
            None => format!("blessed {} golden(s)", manifest.entries.len()),
        });
    }
}

/// The per-frame statuses from the (single) scenario report, in frame order.
fn frame_statuses(reports: &[ScenarioReport]) -> Vec<GateStatus> {
    reports
        .first()
        .map(|r| r.frames.iter().map(|f| f.status).collect())
        .unwrap_or_default()
}

/// A frame is blessable iff its verdict is a real regression the author can accept:
/// `fail` / `missing_golden` / `stale_golden`. A `pass`/`xpass` is a no-op; an `xfail`
/// (green) is NEVER blessed silently; a `child_error`/etc. is not a frame status.
fn is_blessable(status: GateStatus) -> bool {
    matches!(
        status,
        GateStatus::Fail | GateStatus::MissingGolden | GateStatus::StaleGolden
    )
}

/// Resolve the target frame indices from the selector, or a refusal reason.
fn select_targets(
    outcome: &RunOutcome,
    statuses: &[GateStatus],
    selector: &str,
) -> Result<Vec<usize>, String> {
    if selector == "failing" {
        Ok((0..outcome.frames.len())
            .filter(|&i| statuses.get(i).copied().is_some_and(is_blessable))
            .collect())
    } else {
        // A named frame.
        match outcome.frames.iter().position(|f| f.name == selector) {
            None => Err(format!("no expect_golden frame named {selector:?}")),
            Some(i) => {
                let st = statuses.get(i).copied().unwrap_or(GateStatus::Pass);
                if st == GateStatus::Xfail {
                    Err(format!(
                        "frame {selector:?} is xfail (expected-failing) — bless is refused; \
                         remove the xfail to promote it"
                    ))
                } else if is_blessable(st) {
                    Ok(vec![i])
                } else {
                    Err(format!(
                        "frame {selector:?} is {} — nothing to bless",
                        status_label(st)
                    ))
                }
            }
        }
    }
}

/// Run the guard set, then atomically write each target golden + the approval log +
/// manifest. Returns `Refused` if any guard trips (no byte written).
fn write_targets(
    scenario: &Scenario,
    outcome: &RunOutcome,
    golden_dir: &Path,
    targets: &[usize],
    opts: &GateRunOptions,
) -> anyhow::Result<BlessOutcome> {
    if is_ci() {
        return Ok(BlessOutcome::Refused(
            "CI mode: goldens are never self-minted here".to_string(),
        ));
    }
    if targets.is_empty() {
        // Nothing to bless (e.g. `--update` on an already-green scenario, or `create` with
        // no missing frames) — a clean no-op, not a refusal. The driver applies an empty
        // manifest and exits on the verdict (0 when everything already passes).
        return Ok(BlessOutcome::Blessed(BlessManifest { entries: vec![] }));
    }

    // Guard 1: pre-bless secret scan over EVERYTHING about to be committed — the
    // REASSEMBLED VISIBLE TEXT of each capture (NOT the rows[].runs[] JSON envelope, so a
    // secret that line-wraps or is per-cell styled can't hide from the scanner — adv Agent
    // B, MAJOR-1), the frame names, the scenario name (adv MINOR-4, it lands in the
    // approval log), and the approval reason (council #3). Rule IDs only.
    let mut scan_hits: Vec<String> = Vec::new();
    for &i in targets {
        let f = &outcome.frames[i];
        match FrameEnvelope::from_canonical_json(&f.live_capture_json) {
            Ok(env) => scan_hits.extend(secrets::scan(&visible_text(&env))),
            // A capture we can't parse can't be safely vetted — scan the raw bytes too.
            Err(_) => scan_hits.extend(secrets::scan(&f.live_capture_json)),
        }
        scan_hits.extend(secrets::scan(&f.name));
    }
    scan_hits.extend(secrets::scan(&scenario.name));
    if let Some(reason) = &opts.reason {
        scan_hits.extend(secrets::scan(reason));
    }
    scan_hits.sort();
    scan_hits.dedup();
    if !scan_hits.is_empty() {
        return Ok(BlessOutcome::Refused(format!(
            "pre-bless secret scan tripped (rules: {})",
            scan_hits.join(", ")
        )));
    }

    // Guard 2: dirty golden tree. A tracked change or an untracked existing target under
    // the golden dir means an unreviewed edit — refuse rather than clobber it.
    if git_tree_is_dirty(golden_dir) {
        return Ok(BlessOutcome::Refused(format!(
            "golden tree {} has uncommitted changes — commit or stash before blessing",
            golden_dir.display()
        )));
    }

    std::fs::create_dir_all(golden_dir)?;
    let canonical_root = std::fs::canonicalize(golden_dir)
        .map_err(|e| anyhow::anyhow!("canonicalize golden dir {}: {e}", golden_dir.display()))?;
    let rasterizer = Rasterizer::new(FONT_SIZE)?;

    let mut entries = Vec::with_capacity(targets.len());
    let mut header_written = false;
    for &i in targets {
        let f = &outcome.frames[i];
        let old_fp = read_old_fingerprint(golden_dir, &f.name);
        match bless_one(&canonical_root, golden_dir, f, opts, &rasterizer) {
            Ok(new_fp) => {
                let entry = BlessedEntry {
                    name: f.name.clone(),
                    tier: f.tier,
                    old_fingerprint: old_fp,
                    new_fingerprint: new_fp,
                };
                // Record the audit line RIGHT AFTER the golden is written, before the next
                // one — so a mid-batch failure never leaves a committed golden with no
                // who/when/why record (adv Agent B, MAJOR-3).
                if !header_written {
                    write_approval_header(golden_dir, scenario, opts.reason.as_deref())?;
                    header_written = true;
                }
                append_approval_line(golden_dir, &entry)?;
                entries.push(entry);
            }
            Err(e) => return Ok(BlessOutcome::Refused(e)),
        }
    }

    write_manifest(scenario, &entries, opts)?;
    Ok(BlessOutcome::Blessed(BlessManifest { entries }))
}

/// Reassemble the VISIBLE TEXT of a captured frame for the secret scanner (adv Agent B,
/// MAJOR-1). Concatenating the padded grid rows with NO separator makes a secret that
/// line-WRAPS at the pane edge contiguous (a full wrapped row has no trailing pad), while
/// short lines stay separated by their trailing-space padding — so `key=value` AND wrapped
/// secrets are both scannable, unlike the styled rows[].runs[] JSON envelope. Without the
/// VT wrap-flag this is optimal: only an exactly-full NON-wrapped row abutting the next can
/// fabricate a cross-row token (impl-review #5), which merely OVER-flags → a manual review,
/// the safe direction for a bless guard.
fn visible_text(env: &FrameEnvelope) -> String {
    let mut out = String::new();
    for row in env.to_cells() {
        for cell in &row {
            match cell.grapheme() {
                Some(g) => out.push_str(g),
                None => out.push(cell.ch),
            }
        }
    }
    out
}

/// Write one golden (cell JSON + fingerprint sidecar, plus the platform PNG for
/// pixel/exact). Returns the new capture fingerprint (short) for the manifest.
///
/// The three artifacts are written per-`safe_write` (atomic rename) but not as one
/// transaction (impl-review #4). The ORDER is deliberately fail-closed: PNG → cell.json →
/// **fingerprint sidecar LAST**. A crash after any prefix leaves a golden WITHOUT its
/// sidecar (or without its cell.json), which the next `compare_frame` resolves to
/// `missing_golden` — a re-blessable safe state, never a trusted false pass. A fully
/// atomic staged-bundle promote is a possible future hardening.
fn bless_one(
    canonical_root: &Path,
    golden_dir: &Path,
    f: &FrameOutcome,
    opts: &GateRunOptions,
    rasterizer: &Rasterizer,
) -> Result<String, String> {
    let env = FrameEnvelope::from_canonical_json(&f.live_capture_json).map_err(|e| {
        format!(
            "frame {}: capture is not valid canonical JSON: {e:?}",
            f.name
        )
    })?;

    // Build the sidecar from the run's freshly-computed fingerprint (real scenario/env
    // hashes), pinning the live capture. Tolerance comes from `--tol` or the default.
    let mut fp: Fingerprint = f.live_fingerprint.clone();
    fp.capture_sha256 = f.live_capture_sha256.clone();
    if let Some(tol) = opts.tol {
        fp.tol_params = tol;
    }

    // Pixel/exact: render the live envelope, pin the image, and write the platform PNG.
    if f.tier != Tier::Cell {
        let img = render_envelope(rasterizer, &env);
        let png = encode_png(&img).map_err(|e| format!("frame {}: encode PNG: {e}", f.name))?;
        fp.rgba_sha256 = Some(rgba_sha256(&img));
        fp.png_sha256 = Some(png_sha256(&png));
        let png_path = pixel_baseline_path(golden_dir, &f.name, &os_arch());
        safe_write(canonical_root, &png_path, &png)?;
    }

    let sidecar = serde_json::to_string_pretty(&fp)
        .map_err(|e| format!("frame {}: serialize fingerprint: {e}", f.name))?;
    safe_write(
        canonical_root,
        &cell_json_path(golden_dir, &f.name),
        f.live_capture_json.as_bytes(),
    )?;
    safe_write(
        canonical_root,
        &fp_path(golden_dir, &f.name),
        sidecar.as_bytes(),
    )?;

    Ok(short(&f.live_capture_sha256))
}

/// Containment-checked, symlink-refused, atomic write. `root` is the CANONICAL golden dir;
/// every target must resolve inside it. Rejects a symlink or non-regular-file at `target`;
/// writes a temp file in the target's dir and renames it over the target.
///
/// TOCTOU (impl-review #3): the containment + symlink checks and the write are separate
/// path operations, so this is BEST-EFFORT with portable `std` — a fully race-free version
/// needs descriptor-relative syscalls (`openat`/`mkdirat`/`renameat` + `O_NOFOLLOW` +
/// `fstatat` anchored to an opened golden-root fd). The residual window requires an
/// attacker with WRITE access to the golden dir, who could corrupt goldens directly — out
/// of scope for this local-authoring guard.
fn safe_write(root: &Path, target: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = target
        .parent()
        .ok_or_else(|| format!("target {} has no parent", target.display()))?;
    // Containment FIRST (adv Agent B, MINOR-5): canonicalize the deepest EXISTING ancestor
    // of `parent` and confirm it is inside `root` BEFORE creating any directory — so an
    // intermediate symlink pointing outside root can't cause `create_dir_all` to mkdir
    // outside the golden tree (the write itself was already refused, but the dirs leaked).
    let mut probe = parent;
    while !probe.exists() {
        probe = probe
            .parent()
            .ok_or_else(|| format!("no existing ancestor for {}", parent.display()))?;
    }
    let canonical_probe = std::fs::canonicalize(probe)
        .map_err(|e| format!("canonicalize {}: {e}", probe.display()))?;
    if !canonical_probe.starts_with(root) {
        return Err(format!(
            "refusing to write outside the golden dir: {} escapes {}",
            target.display(),
            root.display()
        ));
    }
    std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    let canonical_parent = std::fs::canonicalize(parent)
        .map_err(|e| format!("canonicalize {}: {e}", parent.display()))?;
    if !canonical_parent.starts_with(root) {
        return Err(format!(
            "refusing to write outside the golden dir: {} escapes {}",
            target.display(),
            root.display()
        ));
    }
    // Refuse a symlink (or any non-regular existing file) AT the target — never follow it.
    if let Ok(meta) = std::fs::symlink_metadata(target) {
        if meta.file_type().is_symlink() {
            return Err(format!(
                "refusing to write through a symlink at {}",
                target.display()
            ));
        }
        if !meta.is_file() {
            return Err(format!(
                "refusing to overwrite a non-regular file at {}",
                target.display()
            ));
        }
    }
    let file_name = target
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("bad target name {}", target.display()))?;
    let tmp = canonical_parent.join(format!(".tmp-{file_name}-{}", unique_suffix()));
    // Fresh temp, flushed to disk, then atomically renamed over the target. A failure at
    // ANY step removes the temp so no `.tmp-*` orphan is left behind (adv Agent B, MINOR-7).
    let write_tmp = || -> Result<(), String> {
        use std::io::Write as _;
        let mut fh = std::fs::File::create(&tmp).map_err(|e| format!("create temp: {e}"))?;
        fh.write_all(bytes)
            .map_err(|e| format!("write temp: {e}"))?;
        fh.sync_all().map_err(|e| format!("fsync temp: {e}"))?;
        Ok(())
    };
    if let Err(e) = write_tmp() {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, target).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("rename into place {}: {e}", target.display())
    })?;
    Ok(())
}

/// `git status --porcelain -- <golden_dir>` is non-empty (a tracked change or an untracked
/// entry under the golden dir). If git is unavailable or the path is not in a repo, we
/// cannot assess dirtiness → treat as clean (the CI + secret guards still apply).
///
/// KNOWN LIMITATION (adv Agent B, MINOR-6): `--porcelain` omits gitignored paths, so a
/// golden dir that is itself `.gitignore`d reports clean. This is bounded — an ignored
/// golden is never committed, so a bless into it cannot clobber reviewed VCS history; a
/// golden that matters is tracked (or its parent is), and this guard covers that.
fn git_tree_is_dirty(golden_dir: &Path) -> bool {
    // Run git from the nearest existing ancestor so a not-yet-created golden dir still
    // resolves a repo.
    let mut anchor = golden_dir;
    while !anchor.exists() {
        match anchor.parent() {
            Some(p) => anchor = p,
            None => return false,
        }
    }
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(anchor)
        .args(["status", "--porcelain", "--"])
        .arg(golden_dir)
        .output();
    match out {
        Ok(o) if o.status.success() => !o.stdout.is_empty(),
        _ => false,
    }
}

/// The prior golden's capture fingerprint (short), if it existed — for the manifest diff.
fn read_old_fingerprint(golden_dir: &Path, name: &str) -> Option<String> {
    let text = std::fs::read_to_string(fp_path(golden_dir, name)).ok()?;
    let fp: Fingerprint = serde_json::from_str(&text).ok()?;
    Some(short(&fp.capture_sha256))
}

/// Path of the per-golden-dir approval record (task §5).
fn approval_log_path(golden_dir: &Path) -> PathBuf {
    golden_dir.join("BASELINE-APPROVAL.md")
}

/// Open `<golden_dir>/BASELINE-APPROVAL.md` for append, appending `body`.
fn append_to_approval_log(golden_dir: &Path, body: &str) -> anyhow::Result<()> {
    use std::io::Write as _;
    let mut fh = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(approval_log_path(golden_dir))?;
    fh.write_all(body.as_bytes())?;
    Ok(())
}

/// Write the who/when/why SECTION HEADER for a bless batch. Called once, before the first
/// golden's audit line, so a committed golden always has a preceding record (adv MAJOR-3).
fn write_approval_header(
    golden_dir: &Path,
    scenario: &Scenario,
    reason: Option<&str>,
) -> anyhow::Result<()> {
    let who = git_identity();
    let when = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let mut body = String::new();
    if approval_log_path(golden_dir).exists() {
        // Blank line before a new `##` section so strict-markdown renders the heading
        // (dogfood: appended sections abutted the prior line).
        body.push('\n');
    } else {
        body.push_str("# Lens-gate goldens — baseline approval record\n\n");
    }
    body.push_str(&format!("## {when} — {} ({who})\n\n", scenario.name));
    if let Some(r) = reason {
        body.push_str(&format!("Reason: {r}\n\n"));
    }
    append_to_approval_log(golden_dir, &body)
}

/// Append ONE blessed frame's audit line — written immediately after that golden is
/// committed, before the next write (adv Agent B, MAJOR-3: no committed golden without a
/// who/when/why record, even on a partial-batch failure).
fn append_approval_line(golden_dir: &Path, e: &BlessedEntry) -> anyhow::Result<()> {
    let from = e.old_fingerprint.as_deref().unwrap_or("(new)");
    append_to_approval_log(
        golden_dir,
        &format!(
            "- `{}` ({}) — {from} → {}\n",
            e.name,
            tier_str(e.tier),
            e.new_fingerprint
        ),
    )
}

/// Write the changed-golden manifest (names + old/new fingerprints) for PR review.
fn write_manifest(
    scenario: &Scenario,
    entries: &[BlessedEntry],
    opts: &GateRunOptions,
) -> anyhow::Result<()> {
    let out_dir = opts
        .out
        .clone()
        .unwrap_or_else(|| PathBuf::from(".shux/out").join(&scenario.name));
    std::fs::create_dir_all(&out_dir)?;
    let manifest = serde_json::json!({
        "scenario": scenario.name,
        "blessed": entries.iter().map(|e| serde_json::json!({
            "name": e.name,
            "tier": tier_str(e.tier),
            "old_fingerprint": e.old_fingerprint,
            "new_fingerprint": e.new_fingerprint,
        })).collect::<Vec<_>>(),
    });
    let path = out_dir.join(format!("{}-changed-goldens.json", scenario.name));
    std::fs::write(
        &path,
        format!("{}\n", serde_json::to_string_pretty(&manifest)?),
    )?;
    Ok(())
}

/// The committer identity for the approval log: `git config user.name <email>`, or a
/// portable fallback. Never fails the bless.
fn git_identity() -> String {
    let cfg = |k: &str| {
        std::process::Command::new("git")
            .args(["config", k])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    };
    match (cfg("user.name"), cfg("user.email")) {
        (Some(n), Some(e)) => format!("{n} <{e}>"),
        (Some(n), None) => n,
        (None, Some(e)) => e,
        (None, None) => "unknown".to_string(),
    }
}

fn tier_str(t: Tier) -> &'static str {
    match t {
        Tier::Cell => "cell",
        Tier::Pixel => "pixel",
        Tier::Exact => "exact",
    }
}

fn status_label(s: GateStatus) -> &'static str {
    match s {
        GateStatus::Pass => "pass",
        GateStatus::Xfail => "xfail",
        GateStatus::Xpass => "xpass",
        _ => "not-failing",
    }
}

/// A short (12-hex) fingerprint prefix for human manifests.
fn short(sha: &str) -> String {
    sha.chars().take(12).collect()
}

fn unique_suffix() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{n}-{t}", std::process::id())
}

/// Map a compare-signal frame kind to its report status without re-running the verdict —
/// used only by tests that construct a `FrameOutcome` directly.
#[cfg(test)]
fn kind_status(kind: super::outcome::FrameKind) -> GateStatus {
    use super::outcome::FrameKind;
    match kind {
        FrameKind::Match => GateStatus::Pass,
        FrameKind::Mismatch => GateStatus::Fail,
        FrameKind::GoldenAbsent => GateStatus::MissingGolden,
        FrameKind::GoldenUntrusted => GateStatus::StaleGolden,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::outcome::FrameKind;
    use shux_vt::{
        FINGERPRINT_SCHEMA, MaskSet, RENDERER_FORMAT_VERSION, SCHEMA_VERSION, TolParams,
        VirtualTerminal, capture_sha256, mask_hash, unicode_width_version,
    };

    fn envelope(prog: &[u8]) -> FrameEnvelope {
        let mut vt = VirtualTerminal::new(3, 20);
        vt.process(prog);
        FrameEnvelope::from_terminal(&vt, &MaskSet::new())
    }

    fn cell_fp() -> Fingerprint {
        Fingerprint {
            fp_schema: FINGERPRINT_SCHEMA,
            schema: SCHEMA_VERSION,
            renderer_format_version: RENDERER_FORMAT_VERSION,
            raster_font_fingerprint: shux_raster::builtin_font_fingerprint(FONT_SIZE),
            unicode_width_ver: unicode_width_version(),
            tol: Tier::Cell,
            tol_params: TolParams::default(),
            mask_hash: mask_hash(&MaskSet::new()),
            platform: None,
            shux_version: "test".into(),
            capture_sha256: String::new(),
            rgba_sha256: None,
            png_sha256: None,
            scenario_hash: "scn".into(),
            cmd_env_hash: "cmd".into(),
        }
    }

    fn frame(name: &str, kind: FrameKind, json: String, sha: String) -> FrameOutcome {
        FrameOutcome {
            name: name.into(),
            tier: Tier::Cell,
            kind,
            reason: None,
            verdict: None,
            golden_json: format!("{name}.capture.json"),
            live_capture_json: json,
            live_capture_sha256: sha,
            live_fingerprint: cell_fp(),
            xfail: None,
            retry_note: None,
        }
    }

    fn dummy_opts() -> GateRunOptions {
        GateRunOptions {
            scenario_path: "scn.toml".into(),
            golden_dir: None,
            report: None,
            on_missing: crate::cli::OnMissing::Fail,
            update: None,
            reason: None,
            tol: None,
            out: None,
            retries: None,
            cast: None,
            trace: None,
            argv: vec![],
            format: crate::cli::OutputFormat::Text,
        }
    }

    #[test]
    fn safe_write_writes_and_then_compares_as_match() {
        let dir = tempfile::tempdir().unwrap();
        let golden = envelope(b"\x1b[38;2;9;9;9mhello\x1b[0m");
        let json = golden.to_canonical_json();
        let sha = capture_sha256(&golden);
        let f = frame("main", FrameKind::GoldenAbsent, json.clone(), sha.clone());
        let r = Rasterizer::new(FONT_SIZE).unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let new_fp = bless_one(&root, dir.path(), &f, &dummy_opts(), &r).unwrap();
        assert_eq!(new_fp, super::short(&sha));

        // The written golden must now compare as a match.
        let mut cur = cell_fp();
        cur.capture_sha256 = String::new();
        let fc =
            super::super::compare::compare_frame(dir.path(), "main", Tier::Cell, &golden, &cur, &r);
        assert_eq!(fc.signal.kind(), "frame_match");
    }

    #[test]
    fn safe_write_refuses_a_symlink_target() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let target = dir.path().join("main.capture.json");
        // Plant a symlink where the golden would be written.
        std::os::unix::fs::symlink("/etc/hosts", &target).unwrap();
        let err = safe_write(&root, &target, b"{}").unwrap_err();
        assert!(err.contains("symlink"), "{err}");
        // The symlink was NOT replaced (nor followed): it is still a symlink, unwritten.
        let meta = std::fs::symlink_metadata(&target).unwrap();
        assert!(
            meta.file_type().is_symlink(),
            "target must remain the untouched symlink"
        );
    }

    #[test]
    fn secret_in_capture_refuses_the_bless() {
        let dir = tempfile::tempdir().unwrap();
        // A capture whose text contains an AWS key.
        let golden = envelope(b"AKIAIOSFODNN7EXAMPLE");
        let json = golden.to_canonical_json();
        let sha = capture_sha256(&golden);
        let f = frame("leak", FrameKind::GoldenAbsent, json, sha);
        let scenario = crate::gate::scenario::parse("name=\"s\"\ncommand=[\"true\"]\n").unwrap();
        let out = super::write_targets(
            &scenario,
            &fake_outcome(vec![f]),
            dir.path(),
            &[0],
            &dummy_opts(),
        )
        .unwrap();
        match out {
            BlessOutcome::Refused(reason) => {
                assert!(reason.contains("secret"), "{reason}");
                assert!(reason.contains("aws-access-key"), "{reason}");
                // The secret value itself must NOT appear in the refusal reason.
                assert!(
                    !reason.contains("AKIAIOSFODNN7EXAMPLE"),
                    "leaked the secret: {reason}"
                );
            }
            BlessOutcome::Blessed(_) => panic!("must refuse on a secret hit"),
        }
        // Nothing was written.
        assert!(!dir.path().join("leak.capture.json").exists());
    }

    #[test]
    fn xfail_frame_is_not_blessed_by_name() {
        let f = frame("f", FrameKind::Mismatch, "{}".into(), "sha".into());
        let outcome = fake_outcome(vec![f]);
        let err = select_targets(&outcome, &[GateStatus::Xfail], "f").unwrap_err();
        assert!(err.contains("xfail"), "{err}");
    }

    fn envelope_w(prog: &[u8], cols: usize) -> FrameEnvelope {
        let mut vt = VirtualTerminal::new(3, cols);
        vt.process(prog);
        FrameEnvelope::from_terminal(&vt, &MaskSet::new())
    }

    fn scn() -> crate::gate::scenario::Scenario {
        crate::gate::scenario::parse("name=\"s\"\ncommand=[\"true\"]\n").unwrap()
    }

    #[test]
    fn wrapped_secret_is_caught_by_visible_text_scan() {
        // adv Agent B, MAJOR-1: an AWS key that WRAPS at the pane edge is invisible in the
        // rows[].runs[] JSON envelope but must be caught via the reassembled visible text.
        let env = envelope_w(b"AKIAIOSFODNN7EXAMPLE", 12); // 20 chars → wraps at col 12
        let json = env.to_canonical_json();
        // The old JSON-envelope scan misses it (proving the fix is load-bearing)...
        assert!(
            secrets::scan(&json).is_empty(),
            "wrapped secret should be hidden in the JSON envelope"
        );
        // ...but the visible-text reconstruction restores contiguity and catches it.
        assert!(
            secrets::scan(&visible_text(&env)).contains(&"aws-access-key".to_string()),
            "visible_text must catch the wrapped key"
        );
        // End-to-end: write_targets refuses, nothing written.
        let dir = tempfile::tempdir().unwrap();
        let f = frame("wrap", FrameKind::GoldenAbsent, json, capture_sha256(&env));
        let out = write_targets(
            &scn(),
            &fake_outcome(vec![f]),
            dir.path(),
            &[0],
            &dummy_opts(),
        )
        .unwrap();
        assert!(matches!(out, BlessOutcome::Refused(r) if r.contains("aws-access-key")));
        assert!(!dir.path().join("wrap.capture.json").exists());
    }

    #[test]
    fn secret_in_scenario_name_refuses() {
        // adv Agent B, MINOR-4: the scenario name lands in the approval log — scan it.
        let dir = tempfile::tempdir().unwrap();
        let mut scenario = scn();
        scenario.name = "AKIAIOSFODNN7EXAMPLE".into();
        let clean = envelope_w(b"hi", 20);
        let f = frame(
            "f",
            FrameKind::GoldenAbsent,
            clean.to_canonical_json(),
            capture_sha256(&clean),
        );
        let out = write_targets(
            &scenario,
            &fake_outcome(vec![f]),
            dir.path(),
            &[0],
            &dummy_opts(),
        )
        .unwrap();
        assert!(matches!(out, BlessOutcome::Refused(r) if r.contains("aws-access-key")));
    }

    #[test]
    fn partial_bless_keeps_the_audit_record() {
        // adv Agent B, MAJOR-3: a mid-batch failure must still leave the committed golden's
        // who/when/why record (the approval line is written per-frame, before the next).
        let dir = tempfile::tempdir().unwrap();
        // Block the SECOND target's write: a directory sits where its golden would go.
        std::fs::create_dir_all(dir.path().join("second.capture.json")).unwrap();
        let a = {
            let e = envelope_w(b"first", 20);
            frame(
                "first",
                FrameKind::GoldenAbsent,
                e.to_canonical_json(),
                capture_sha256(&e),
            )
        };
        let b = {
            let e = envelope_w(b"second", 20);
            frame(
                "second",
                FrameKind::GoldenAbsent,
                e.to_canonical_json(),
                capture_sha256(&e),
            )
        };
        let out = write_targets(
            &scn(),
            &fake_outcome(vec![a, b]),
            dir.path(),
            &[0, 1],
            &dummy_opts(),
        )
        .unwrap();
        assert!(
            matches!(out, BlessOutcome::Refused(_)),
            "second write must fail"
        );
        // The first golden IS committed AND its approval line is recorded.
        assert!(
            dir.path().join("first.capture.json").exists(),
            "first golden committed"
        );
        let log = std::fs::read_to_string(dir.path().join("BASELINE-APPROVAL.md")).unwrap();
        assert!(
            log.contains("first"),
            "committed golden must have an audit line: {log}"
        );
    }

    #[test]
    fn only_failing_frames_are_selected() {
        let outcome = fake_outcome(vec![
            frame("a", FrameKind::Match, "{}".into(), "a".into()),
            frame("b", FrameKind::Mismatch, "{}".into(), "b".into()),
            frame("c", FrameKind::GoldenAbsent, "{}".into(), "c".into()),
        ]);
        let statuses = [
            GateStatus::Pass,
            GateStatus::Fail,
            GateStatus::MissingGolden,
        ];
        let t = select_targets(&outcome, &statuses, "failing").unwrap();
        assert_eq!(t, vec![1, 2]);
    }

    fn fake_outcome(frames: Vec<FrameOutcome>) -> RunOutcome {
        RunOutcome {
            scenario_name: "s".into(),
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            font_chain_sha256: None,
            font_size_px: 16,
            started_at_ms: 0,
            duration_ms: 0,
            frames,
            terminal: None,
            has_visual_check: true,
        }
    }

    #[test]
    fn kind_status_maps() {
        assert_eq!(kind_status(FrameKind::Match), GateStatus::Pass);
        assert_eq!(
            kind_status(FrameKind::GoldenUntrusted),
            GateStatus::StaleGolden
        );
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .expect("git available")
            .status
            .success();
        assert!(ok, "git {args:?} failed");
    }

    #[test]
    fn git_tree_dirty_detects_uncommitted_golden_changes() {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q"]);
        let gdir = repo.path().join("goldens");
        std::fs::create_dir_all(&gdir).unwrap();
        std::fs::write(gdir.join("frame.capture.json"), "{}").unwrap();
        // Untracked golden under the dir → dirty.
        assert!(git_tree_is_dirty(&gdir), "an untracked golden is dirty");
        // Commit it → clean.
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-q", "-m", "add golden"]);
        assert!(
            !git_tree_is_dirty(&gdir),
            "a committed golden tree is clean"
        );
        // Modify the tracked golden → dirty again.
        std::fs::write(gdir.join("frame.capture.json"), "{\"x\":1}").unwrap();
        assert!(
            git_tree_is_dirty(&gdir),
            "a modified tracked golden is dirty"
        );
    }

    #[test]
    fn git_tree_dirty_is_false_outside_a_repo() {
        // A golden dir not under any repo cannot be assessed → treated as clean (the CI +
        // secret guards still apply).
        let dir = tempfile::tempdir().unwrap();
        assert!(!git_tree_is_dirty(&dir.path().join("goldens")));
    }
}
