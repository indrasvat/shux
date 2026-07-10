//! Scratch sessions + `lens.run` composite (lens PRD §8 SPEC-E, task 077 P5).
//!
//! Scratch sessions are created ONLY by `lens.run` (DEC-21): there is no
//! public `session.create` scratch parameter and no other way to allocate
//! one. This module owns:
//! - the scratch registry (in-memory `ScratchRegistry` + a mirrored
//!   `$XDG_RUNTIME_DIR/shux/scratch-registry.json`, LENS-R-044) so a fresh
//!   daemon can kill orphaned scratch process groups left by a prior
//!   incarnation (scratch never survives restart, DEC-7/B6). Persisted
//!   atomically (temp-file + rename); a corrupt file is preserved as
//!   `.corrupt` evidence, never silently deleted.
//! - quota accounting (LENS-R-043): `try_reserve` checks the quota and
//!   reserves a slot in ONE critical section, so concurrent `lens.run`
//!   calls can never overshoot 16 (P5 convergence round 1, codex B1).
//! - per-scratch reap timers (`post_exit_ttl_ms` / `max_runtime_ms`,
//!   LENS-R-042), event-driven off the same `pane.exited` bus event the
//!   daemon already fires (no polling — §16.1 guardrail 3). The reap
//!   itself performs the LENS-R-042 sequence directly — killpg(SIGTERM) →
//!   500 ms grace → killpg(SIGKILL) → confirm dead → close PTY → remove
//!   session → audit — and the registry row is removed only AFTER the
//!   group is confirmed dead (codex B3: a daemon crash mid-reap must
//!   leave the row for the next incarnation's startup reap).
//! - the `lens.run` RPC handler (LENS-R-040/041/045/046).
//! - the daemon-level lens audit log (`LensAuditLog`, LENS-R-052):
//!   sha256-chained NDJSON at `$XDG_STATE_HOME/shux/lens-audit.ndjson`,
//!   appends serialized behind a mutex with the chain head cached in
//!   memory (no chain forks under concurrency, no O(n²) file re-reads),
//!   rotated at 1 MiB (each rotated file carries its own genesis-rooted
//!   chain), with a `verify_chain` checker so tampering is detectable.
//!
//! `lens.run`'s response is `{session_id, pane_id, revision}` (+
//! `exit_code` when `wait:true`) per §8.1 — it does NOT call
//! `pane.glance`/`pane.wait_settled`/`pane.diff_since` internally. Those are
//! separate RPCs an agent chains itself (see E1, §12): `lens.run` only owns
//! allocate → exec → optional completion-wait → reap-on-a-timer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use shux_rpc::{Policy, Sensitivity};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use shux_core::bus::{EventBus, SubscriptionEvent};
use shux_core::event::EventData;
use shux_core::graph::GraphHandle;
use shux_core::model::{PaneId, SessionId};

use crate::PaneIoState;

// ── bounds (LENS-R-040) ───────────────────────────────────────────────────

pub const SCRATCH_QUOTA: usize = 16;
const DEFAULT_COLS: u64 = 80;
const DEFAULT_ROWS: u64 = 24;
const MIN_COLS: u64 = 20;
const MAX_COLS: u64 = 500;
const MIN_ROWS: u64 = 5;
const MAX_ROWS: u64 = 200;
const DEFAULT_POST_EXIT_TTL_MS: u64 = 30_000;
const MIN_POST_EXIT_TTL_MS: u64 = 0;
const MAX_POST_EXIT_TTL_MS: u64 = 300_000;
const DEFAULT_MAX_RUNTIME_MS: u64 = 3_600_000;
const MIN_MAX_RUNTIME_MS: u64 = 1_000;
const MAX_MAX_RUNTIME_MS: u64 = 86_400_000;

// ── audit (LENS-R-052) ──────────────────────────────────────────────────

const AUDIT_ROTATE_AT_BYTES: u64 = 1024 * 1024;
const AUDIT_KEEP_ROTATIONS: usize = 5;
const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Daemon-level lens audit log (LENS-R-052): append-only sha256-chained
/// NDJSON at `<state dir>/lens-audit.ndjson`.
///
/// Concurrency (P5 convergence round 1, codex M2b): every append runs under
/// one mutex with the chain head CACHED in memory (read from the file once
/// at construction) — two concurrent appends can never both chain off the
/// same `prev_hash` (a forked chain would read as a false tamper alarm),
/// and appends stop re-reading the whole file per entry (O(n²)).
///
/// Rotation: mirrors the per-plugin audit log — at 1 MiB the current file
/// rotates to `.1` (existing `.N` shift up; `.5` is discarded). The hash
/// chain CARRIES ACROSS files (P5 round-2 codex minor): the fresh file
/// opens with an `audit.rotate` header entry whose `prev_hash` is the
/// rotated-out file's final hash (and which names its predecessor — a
/// historical label; later rotations shift filenames, continuity is
/// verified by hash, not name). `verify_chain_set` walks the whole
/// rotation set as one chain, so deleting or reordering an interior
/// rotated file is detectable. Inherent residual: the OLDEST file's
/// predecessor is legitimately discarded (keep-5), so its first
/// `prev_hash` is a trust anchor — deleting the ENTIRE rotated set (or
/// the single oldest file) remains undetectable by construction.
///
/// Caller field: entries read `shux_rpc::current_caller()` — a
/// `tokio::task_local!` the plugin dispatch wrapper scopes to
/// `plugin:<uuid>` around each router dispatch (P5 round-1 claude N3,
/// adjudicated). UDS requests and daemon-internal tasks (reap timers,
/// startup reap) carry no scope and default to `"uds"`.
///
/// Writes are best-effort: a failure is logged, never surfaced — losing an
/// audit line must not break the scratch lifecycle it documents (same
/// posture as the per-plugin audit log).
pub struct LensAuditLog {
    path: PathBuf,
    /// Chain head of the CURRENT file. Guards the whole read-modify-append
    /// (rotate-check + hash + write) so appends serialize.
    last_hash: StdMutex<String>,
}

impl LensAuditLog {
    /// Open (or start) the audit log inside `state_dir` (the caller
    /// resolves `$XDG_STATE_HOME/shux`). Reads the existing file's last
    /// line ONCE to seed the chain-head cache.
    pub fn open(state_dir: &Path) -> Arc<Self> {
        let path = state_dir.join("lens-audit.ndjson");
        let last = last_hash_in_file(&path).unwrap_or_else(|| GENESIS_HASH.to_string());
        Arc::new(Self {
            path,
            last_hash: StdMutex::new(last),
        })
    }

    /// Open at the default daemon location (`$XDG_STATE_HOME/shux`).
    pub fn open_default() -> Arc<Self> {
        Self::open(&xdg_state_home())
    }

    /// Append one chained NDJSON line. `entry` must be a JSON object and
    /// must NOT set `prev_hash`/`hash` — both are computed here, under the
    /// serializing lock.
    pub fn append(&self, entry: serde_json::Value) {
        // The lock is std::sync (never held across an await); the file I/O
        // under it is one small append — the same tradeoff the per-plugin
        // audit log makes on the hot path of every plugin RPC.
        let mut last = match self.last_hash.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };

        // Rotate BEFORE the write, under the same lock. The chain head is
        // NOT reset (P5 round-2 codex minor: independent per-file genesis
        // chains made deleting/reordering a whole rotated file
        // undetectable) — the fresh file's first entry is a rotation
        // header that chains directly off the rotated-out file's final
        // hash and names its predecessor, so `verify_chain_set` can walk
        // the whole rotation set as ONE chain.
        if let Ok(meta) = std::fs::metadata(&self.path) {
            if meta.len() > AUDIT_ROTATE_AT_BYTES {
                if let Err(e) = rotate_audit(&self.path) {
                    tracing::warn!(error = %e, "lens-audit: rotation failed; appending to the oversized file");
                } else {
                    let predecessor = self
                        .path
                        .with_extension("ndjson.1")
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "lens-audit.ndjson.1".to_string());
                    self.append_locked(
                        &mut last,
                        json!({
                            "ts": iso_now(),
                            "method": "audit.rotate",
                            "predecessor": predecessor,
                        }),
                    );
                }
            }
        }

        self.append_locked(&mut last, entry);
    }

    /// The chained write itself, under the caller-held head lock: stamp
    /// `prev_hash` from the cached head, hash, append, advance the head.
    fn append_locked(&self, last: &mut String, mut entry: serde_json::Value) {
        let prev_hash = last.clone();
        if let Some(obj) = entry.as_object_mut() {
            obj.insert("prev_hash".into(), json!(prev_hash));
        }
        let canonical = serde_json::to_vec(&entry).unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(prev_hash.as_bytes());
        hasher.update(&canonical);
        let hash = hex_encode(&hasher.finalize());
        if let Some(obj) = entry.as_object_mut() {
            obj.insert("hash".into(), json!(hash.clone()));
        }

        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut line = match serde_json::to_vec(&entry) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(error = %e, "lens-audit: serialize failed");
                return;
            }
        };
        line.push(b'\n');
        use std::io::Write;
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            Ok(mut f) => {
                if let Err(e) = f.write_all(&line) {
                    tracing::warn!(error = %e, path = %self.path.display(), "lens-audit: write failed");
                    return;
                }
                *last = hash;
            }
            Err(e) => {
                tracing::warn!(error = %e, path = %self.path.display(), "lens-audit: open failed");
            }
        }
    }
}

/// Shift `lens-audit.ndjson.{N}` → `.{N+1}` for N in 4..=1, then rotate the
/// live file to `.1`. Discards the oldest (`.5`). Same scheme as the
/// per-plugin audit log.
fn rotate_audit(path: &Path) -> std::io::Result<()> {
    for n in (1..AUDIT_KEEP_ROTATIONS).rev() {
        let from = path.with_extension(format!("ndjson.{n}"));
        let to = path.with_extension(format!("ndjson.{}", n + 1));
        if from.exists() {
            let _ = std::fs::rename(&from, &to);
        }
    }
    std::fs::rename(path, path.with_extension("ndjson.1"))
}

fn last_hash_in_file(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let last_line = text.lines().next_back()?;
    let v: serde_json::Value = serde_json::from_str(last_line).ok()?;
    v.get("hash").and_then(|h| h.as_str()).map(str::to_string)
}

/// Anchor requirement for the first entry of an audit file under
/// verification (P5 round-3 codex minor: a freely-adopted first
/// `prev_hash` made deleting a PREFIX of lines undetectable — the anchor
/// must be externally justified, never self-declared).
#[derive(Clone, Copy)]
enum ChainAnchor<'a> {
    /// The first entry's `prev_hash` must equal this exact hash (the zero
    /// genesis, or the verified final hash of the predecessor file
    /// supplied by the set walker).
    Exact(&'a str),
    /// The predecessor was LEGITIMATELY discarded (keep-5 rotation): the
    /// first entry must still justify its anchor structurally — either
    /// the zero genesis (true first file) or an `audit.rotate`
    /// continuation header (whose stored prev is then adopted). A plain
    /// entry with an arbitrary prev is rejected: that is exactly what a
    /// prefix deletion leaves behind.
    TrustedStart,
}

/// Verify one audit file's internal hash chain (P5 convergence round 1,
/// codex M2d: a write-only chain is decoration — nothing could detect
/// tampering). Returns `(entries_verified, final_hash)` — `final_hash` is
/// the anchor itself for an empty file (only meaningful under `Exact`).
/// Fails on: a non-JSON line, an unjustified first anchor (see
/// [`ChainAnchor`]), a `prev_hash` that does not match the running head,
/// or a stored `hash` that does not match the recomputation over
/// `sha256(prev_hash ‖ canonical(entry sans `hash`))`.
#[allow(dead_code)]
fn verify_chain_file(path: &Path, anchor: ChainAnchor<'_>) -> Result<(usize, String), String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut prev: Option<String> = match anchor {
        ChainAnchor::Exact(h) => Some(h.to_string()),
        ChainAnchor::TrustedStart => None,
    };
    let mut count = 0usize;
    for (i, line) in text.lines().enumerate() {
        let mut v: serde_json::Value =
            serde_json::from_str(line).map_err(|e| format!("line {}: not JSON: {e}", i + 1))?;
        let stored_prev = v
            .get("prev_hash")
            .and_then(|h| h.as_str())
            .ok_or_else(|| format!("line {}: missing prev_hash", i + 1))?
            .to_string();
        let expected = match &prev {
            Some(h) => h.clone(),
            None => {
                // TrustedStart, first entry: the anchor must be
                // structurally justified — genesis, or a rotation
                // continuation header (codex round-3: never adopt an
                // arbitrary prev; a prefix deletion leaves exactly that).
                let is_rotate_header =
                    v.get("method").and_then(|m| m.as_str()) == Some("audit.rotate");
                if stored_prev != GENESIS_HASH && !is_rotate_header {
                    return Err(format!(
                        "line 1: unjustified chain anchor — first entry is neither \
                         genesis-rooted nor an audit.rotate continuation \
                         (prefix deletion?): prev_hash {stored_prev}"
                    ));
                }
                stored_prev.clone()
            }
        };
        if stored_prev != expected {
            return Err(format!(
                "line {}: chain break — prev_hash {} != expected {}",
                i + 1,
                stored_prev,
                expected
            ));
        }
        let stored_hash = v
            .get("hash")
            .and_then(|h| h.as_str())
            .ok_or_else(|| format!("line {}: missing hash", i + 1))?
            .to_string();
        // Recompute over the entry WITHOUT its `hash` field — exactly what
        // `append` hashed (serde_json object keys serialize sorted, so the
        // canonical bytes are reproducible).
        if let Some(obj) = v.as_object_mut() {
            obj.remove("hash");
        }
        let canonical = serde_json::to_vec(&v).map_err(|e| e.to_string())?;
        let mut hasher = Sha256::new();
        hasher.update(expected.as_bytes());
        hasher.update(&canonical);
        let recomputed = hex_encode(&hasher.finalize());
        if recomputed != stored_hash {
            return Err(format!(
                "line {}: hash mismatch — stored {stored_hash}, recomputed {recomputed}",
                i + 1
            ));
        }
        prev = Some(stored_hash);
        count += 1;
    }
    Ok((count, prev.unwrap_or_else(|| GENESIS_HASH.to_string())))
}

/// True when `path` names a ROTATED audit file (`….ndjson.N`). Direct
/// verification of a rotated file is rejected (P5 round-4 codex minor):
/// `verify_chain`'s rotate-header delegation and `verify_chain_set`'s
/// sibling resolution both derive the rotation set from the LIVE path via
/// `with_extension("ndjson.N")` — handed `lens-audit.ndjson.1` directly,
/// they would resolve the wrong set (`….ndjson.1.N`) and TrustedStart
/// would accept the header without a verified predecessor.
fn is_rotated_audit_path(path: &Path) -> bool {
    let numeric_ext = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| !e.is_empty() && e.bytes().all(|b| b.is_ascii_digit()));
    numeric_ext
        && path
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.ends_with(".ndjson"))
}

/// Verify a standalone audit file with a STRICT external anchor (P5
/// round-3 codex minor): genesis-rooted files verify directly; a file
/// opening with an `audit.rotate` continuation header delegates to the
/// full [`verify_chain_set`] walk (the anchor is only justified by its
/// verified predecessor); anything else — e.g. the remains of a prefix
/// deletion — is rejected. Rotated files (`….ndjson.N`) must be verified
/// through `verify_chain_set` on the LIVE path (P5 round-4 codex minor —
/// a direct rotated-file argument would resolve the wrong sibling set and
/// self-justify its continuation header).
///
/// Consumed by the unit + black-box test suites today (hence the
/// dead-code allowance on the non-test build); the natural future surface
/// is a `shux lens audit verify` CLI verb — P6 CLI-polish material.
#[allow(dead_code)]
pub fn verify_chain(path: &Path) -> Result<usize, String> {
    if is_rotated_audit_path(path) {
        return Err(format!(
            "{} is a rotated audit file: verify it via verify_chain_set on \
             the live log path (a direct rotated-file verification would \
             resolve the wrong sibling set and cannot justify its \
             continuation anchor)",
            path.display()
        ));
    }
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let Some(first_line) = text.lines().next() else {
        return Ok(0); // empty log — nothing to verify
    };
    let first: serde_json::Value =
        serde_json::from_str(first_line).map_err(|e| format!("line 1: not JSON: {e}"))?;
    let first_prev = first.get("prev_hash").and_then(|h| h.as_str());
    if first_prev == Some(GENESIS_HASH) {
        return verify_chain_file(path, ChainAnchor::Exact(GENESIS_HASH)).map(|(n, _)| n);
    }
    if first.get("method").and_then(|m| m.as_str()) == Some("audit.rotate") {
        // Continuation: only the set walk can justify the anchor.
        return verify_chain_set(path);
    }
    Err(format!(
        "line 1: unjustified chain anchor — first entry is neither genesis-rooted \
         nor an audit.rotate continuation (prefix deletion?): prev_hash {:?}",
        first_prev.unwrap_or("<missing>")
    ))
}

/// Verify the WHOLE rotation set as one chain (P5 round-2 codex minor):
/// oldest present rotated file (`.5` … `.1`) through the live file, each
/// subsequent file required to chain exactly off its predecessor's final
/// hash — so deleting or reordering an interior rotated file breaks
/// verification. The oldest present file's anchor must still be
/// structurally justified (genesis or an `audit.rotate` continuation
/// header — its predecessor was legitimately discarded by keep-5), so a
/// prefix deletion inside the oldest file is caught too.
/// Returns the total number of verified entries across the set.
#[allow(dead_code)]
pub fn verify_chain_set(live_path: &Path) -> Result<usize, String> {
    if is_rotated_audit_path(live_path) {
        return Err(format!(
            "{} is a rotated audit file, not the live log: the rotation set \
             must be resolved from the live path",
            live_path.display()
        ));
    }
    let mut files: Vec<PathBuf> = (1..=AUDIT_KEEP_ROTATIONS)
        .map(|n| live_path.with_extension(format!("ndjson.{n}")))
        .filter(|p| p.exists())
        .collect();
    files.reverse(); // .5 (oldest) first … .1 last
    files.push(live_path.to_path_buf());

    let mut anchor: Option<String> = None;
    let mut total = 0usize;
    for f in &files {
        let file_anchor = match &anchor {
            Some(h) => ChainAnchor::Exact(h),
            None => ChainAnchor::TrustedStart,
        };
        let (n, final_hash) =
            verify_chain_file(f, file_anchor).map_err(|e| format!("{}: {e}", f.display()))?;
        anchor = Some(final_hash);
        total += n;
    }
    Ok(total)
}

fn xdg_state_home() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_STATE_HOME") {
        return PathBuf::from(xdg).join("shux");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("shux");
    }
    PathBuf::from("shux-state")
}

pub(crate) fn iso_now() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Exact decoded byte length of a standard (padded) base64 string —
/// `len/4*3` minus the trailing `=` padding (P5 round-2 claude nit: the
/// unpadded formula over-reported `bytes_returned` by up to 2).
pub(crate) fn b64_decoded_len(b64: &str) -> usize {
    let pad = b64.bytes().rev().take_while(|&b| b == b'=').count();
    (b64.len() / 4 * 3).saturating_sub(pad)
}

fn unix_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ── process start-time token (LENS-R-044 "start time matches") ──────────

/// An OS-reported process start token for `pid`, used to disambiguate a
/// recycled PID from the process the registry actually recorded (P5
/// convergence round 1, codex M4 — adjudicated IMPLEMENT). The token is
/// only ever compared against another token read the same way on the same
/// machine:
/// - macOS: `proc_pidinfo(PROC_PIDTBSDINFO)` → `pbi_start_tvsec/usec`
///   (µs-resolution wall-clock start time).
/// - Linux: `/proc/<pid>/stat` field 22 (`starttime`, clock ticks since
///   boot — stable across daemon restarts within one boot; after a real
///   reboot ticks differ and the mismatch correctly suppresses the kill).
///
/// `None` when the process is gone or the read fails.
#[cfg(target_os = "macos")]
fn process_start_token(pid: u32) -> Option<u64> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: proc_pidinfo writes at most `size` bytes into `info`, which is
    // a properly aligned, zeroed proc_bsdinfo we own; no aliasing.
    let ret = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    if ret != size {
        return None;
    }
    Some(info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec)
}

#[cfg(target_os = "linux")]
fn process_start_token(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // comm (field 2) may contain spaces/parens; fields 3.. start after the
    // LAST ')'. starttime is field 22 overall → the 20th token after state.
    let after = stat.rsplit_once(')')?.1;
    after.split_whitespace().nth(19)?.parse().ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process_start_token(_pid: u32) -> Option<u64> {
    // Unsupported platform: registration stores 0 → startup reap falls
    // back to liveness-only (documented).
    None
}

// ── registry (LENS-R-043/044) ────────────────────────────────────────────

/// One live scratch session's bookkeeping. The `Serialize`/`Deserialize`
/// subset (`RegistryRow`) is what hits disk; `explicit_kill`/`claimed` are
/// in-memory control state for THIS daemon incarnation (a restarted daemon
/// has neither — it kills by pgid + start-token match). The reaper task is
/// fire-and-forget (`tokio::spawn`, never joined); cancelling
/// `explicit_kill` makes it return promptly.
struct ScratchState {
    pane_id: PaneId,
    pgid: u32,
    /// OS-reported start token of the group leader at registration
    /// (LENS-R-044 "start time matches"); 0 when capture failed.
    start_time: u64,
    created_at_unix_ms: u64,
    max_runtime_deadline_unix_ms: u64,
    /// Cancelled by `on_session_killed` (explicit `session.kill`) so the
    /// reaper task returns immediately instead of racing its own reap.
    explicit_kill: CancellationToken,
    /// Reap-ownership guard (P5 round-1 claude N6): whoever flips this via
    /// `claim()` owns the kill/audit/remove sequence; the loser backs off.
    /// Prevents a timer-reap and an explicit kill double-auditing the same
    /// scratch.
    claimed: bool,
}

/// On-disk registry row (LENS-R-044 schema + the adjudicated `start_time`
/// extension): `{session_id, pgid, start_time, created_at,
/// max_runtime_deadline}`.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct RegistryRow {
    session_id: String,
    pgid: u32,
    #[serde(default)]
    start_time: u64,
    created_at: u64,
    max_runtime_deadline: u64,
}

struct RegistryInner {
    rows: HashMap<SessionId, ScratchState>,
    /// Slots reserved by in-flight `lens.run` calls that have passed the
    /// quota check but not yet committed a row (LENS-R-043 atomicity —
    /// P5 round-1 codex B1). `rows.len() + reserved + opaque_unresolved`
    /// is the quota-relevant total.
    reserved: usize,
    /// Recovered rows whose `session_id` does not parse (P5 round-5
    /// codex): no session-keyed reaper can be armed for them, but they
    /// represent REAL live groups whose kill stayed unconfirmed at seed
    /// time — every normal persist must carry them (the invariant: never
    /// silently lost) until the next incarnation's startup reap retries.
    opaque_unresolved: Vec<RegistryRow>,
}

/// The info a reap path needs once it has claimed a scratch (see
/// `ScratchRegistry::claim`).
struct ClaimedScratch {
    pane_id: PaneId,
    pgid: u32,
    explicit_kill: CancellationToken,
}

#[derive(Clone)]
pub struct ScratchRegistry {
    inner: Arc<StdMutex<RegistryInner>>,
    registry_path: PathBuf,
    audit: Arc<LensAuditLog>,
}

/// A reserved-but-uncommitted quota slot (LENS-R-043 atomic
/// check-and-reserve). Dropping it without `commit` releases the slot —
/// every failure/rollback path in `lens.run` releases by construction.
struct ScratchReservation {
    registry: ScratchRegistry,
    committed: bool,
}

impl ScratchReservation {
    /// Convert the reservation into a committed registry row. Runs the
    /// release + insert + persist in ONE critical section.
    fn commit(mut self, id: SessionId, state: ScratchState) {
        self.committed = true;
        let mut inner = self.registry.lock_inner();
        inner.reserved = inner.reserved.saturating_sub(1);
        inner.rows.insert(id, state);
        self.registry.persist(&inner);
    }
}

impl Drop for ScratchReservation {
    fn drop(&mut self) {
        if !self.committed {
            let mut inner = self.registry.lock_inner();
            inner.reserved = inner.reserved.saturating_sub(1);
        }
    }
}

impl ScratchRegistry {
    pub fn new(runtime_dir: &Path, audit: Arc<LensAuditLog>) -> Self {
        Self {
            inner: Arc::new(StdMutex::new(RegistryInner {
                rows: HashMap::new(),
                reserved: 0,
                opaque_unresolved: Vec::new(),
            })),
            registry_path: runtime_dir.join("scratch-registry.json"),
            audit,
        }
    }

    fn lock_inner(&self) -> std::sync::MutexGuard<'_, RegistryInner> {
        match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Atomic quota check-and-reserve (LENS-R-043; P5 round-1 codex B1).
    /// `Err((current_total, quota))` when full. The returned reservation
    /// counts toward the quota until committed or dropped, so N concurrent
    /// `lens.run` calls racing at `quota - 1` admit exactly one.
    fn try_reserve(&self) -> Result<ScratchReservation, (usize, usize)> {
        let mut inner = self.lock_inner();
        let total = inner.rows.len() + inner.reserved + inner.opaque_unresolved.len();
        if total >= SCRATCH_QUOTA {
            return Err((total, SCRATCH_QUOTA));
        }
        inner.reserved += 1;
        drop(inner);
        Ok(ScratchReservation {
            registry: self.clone(),
            committed: false,
        })
    }

    /// Snapshot of every currently-registered scratch session id. Used by
    /// `session.list` to filter/annotate without holding the registry lock
    /// across the JSON-building loop.
    pub fn ids(&self) -> std::collections::HashSet<SessionId> {
        self.lock_inner().rows.keys().copied().collect()
    }

    /// Claim reap ownership of a scratch (P5 round-1 claude N6): returns
    /// the kill info exactly once — a second claimant (timer reap racing an
    /// explicit `session.kill`) gets `None` and must back off. The row
    /// STAYS in the registry (and on disk) until `remove` — a daemon crash
    /// between claim and kill leaves the row for the next incarnation's
    /// startup reap (codex B3).
    fn claim(&self, id: &SessionId) -> Option<ClaimedScratch> {
        let mut inner = self.lock_inner();
        let state = inner.rows.get_mut(id)?;
        if state.claimed {
            return None;
        }
        state.claimed = true;
        Some(ClaimedScratch {
            pane_id: state.pane_id,
            pgid: state.pgid,
            explicit_kill: state.explicit_kill.clone(),
        })
    }

    /// Remove a row + persist. Called only AFTER the reap sequence has
    /// confirmed the process group dead (codex B3 ordering).
    fn remove(&self, id: &SessionId) {
        let mut inner = self.lock_inner();
        inner.rows.remove(id);
        self.persist(&inner);
    }

    /// Rewrite the registry file from the in-memory rows (LENS-R-044:
    /// "persist ... on every change"). ATOMIC: writes a sibling temp file
    /// and renames it over the target (P5 round-1 codex B2 — a crash
    /// mid-`std::fs::write` truncate left partial JSON that the next
    /// startup could not act on). Small sync I/O under the registry lock —
    /// ≤ `SCRATCH_QUOTA` rows, same tradeoff as the plugin audit log.
    fn persist(&self, inner: &RegistryInner) {
        let mut rows: Vec<RegistryRow> = inner
            .rows
            .iter()
            .map(|(id, s)| RegistryRow {
                session_id: id.to_string(),
                pgid: s.pgid,
                start_time: s.start_time,
                created_at: s.created_at_unix_ms,
                max_runtime_deadline: s.max_runtime_deadline_unix_ms,
            })
            .collect();
        // Opaque unresolved rows ride along in EVERY persist (P5 round-5
        // codex — without this, the first normal persist clobbered them,
        // the same B3-class hole one branch deep).
        rows.extend(inner.opaque_unresolved.iter().cloned());
        persist_rows_atomic(&self.registry_path, &rows);
    }

    /// Startup reap (LENS-R-044/DEC-7): read a leftover registry file from
    /// a prior daemon incarnation; kill every row's process GROUP unless
    /// the leader's OS start token proves the PID was recycled; delete the
    /// file; write one audit entry per row. Runs BEFORE the RPC server
    /// accepts `lens.run` calls.
    ///
    /// Decision table (codex M4 + P5 round-2 codex N2):
    /// - recorded token 0 (capture failed at registration): liveness-only
    ///   fallback — the kill sequence's own group probe decides (logged).
    /// - leader alive, token MATCHES: ours — kill.
    /// - leader alive, token DIFFERS: recycled PID — spare (row still
    ///   cleared with the file).
    /// - leader GONE (`process_start_token` None): the group may still
    ///   hold our descendants (`sh -c 'sleep 999 & exit'` leaves the sleep
    ///   in the group after the leader exits) — a pgid stays allocated,
    ///   and cannot be recycled as a new process's PID, while ANY member
    ///   lives, so a live group with a dead leader is OURS: kill it (the
    ///   sequence's killpg(pgid, 0) probe is the liveness check). Residual
    ///   edge: the whole group died AND the pgid was recycled by an
    ///   unrelated NEW group inside the restart window — same class as
    ///   the §17 M14 double-fork tolerance, documented in the task file.
    ///
    /// Corrupt file (codex B2): preserved as `<path>.corrupt.<unix_ms>`
    /// (timestamped so repeated corrupt startups never overwrite earlier
    /// evidence — P5 round-2 codex minor), logged at ERROR, never silently
    /// deleted; nothing is killed.
    ///
    /// Per-row resolution (P5 round-3 codex — N3 at the startup path):
    /// rows resolve INDIVIDUALLY. Died/AlreadyDead rows are audited and
    /// dropped; an UNCONFIRMED row gets an ERROR log, NO reap audit, and
    /// is RE-PERSISTED (atomic persist of the surviving subset) so the
    /// next restart retries it — the file is deleted only when every row
    /// resolved. Unconditional deletion here was the B3-class hole moved
    /// to startup: a stubborn group surviving this reap became invisible
    /// to every future restart.
    ///
    /// Returns `(killed, unresolved)` (P5 round-4 codex): the caller MUST
    /// seed its live registry with the unresolved rows via
    /// [`ScratchRegistry::seed_unresolved`] — otherwise the daemon's very
    /// first normal persist (which rewrites the file from in-memory rows)
    /// would clobber them before the next restart could retry.
    pub async fn startup_reap(
        runtime_dir: &Path,
        audit: &LensAuditLog,
    ) -> (usize, Vec<RegistryRow>) {
        let path = runtime_dir.join("scratch-registry.json");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return (0, Vec::new());
        };
        let rows: Vec<RegistryRow> = match serde_json::from_str(&text) {
            Ok(r) => r,
            Err(e) => {
                let corrupt =
                    path.with_extension(format!("json.corrupt.{}", unix_ms(SystemTime::now())));
                tracing::error!(
                    error = %e,
                    preserved = %corrupt.display(),
                    "scratch-registry: unreadable (crash mid-write or tampering); \
                     preserving as evidence — NOT killing anything; inspect + remove manually"
                );
                if let Err(re) = std::fs::rename(&path, &corrupt) {
                    tracing::error!(error = %re, "scratch-registry: could not preserve corrupt file");
                }
                return (0, Vec::new());
            }
        };
        let mut killed = 0usize;
        let mut unresolved: Vec<RegistryRow> = Vec::new();
        for row in rows {
            let should_kill = match (row.start_time, process_start_token(row.pgid)) {
                (0, _) => {
                    tracing::warn!(
                        pgid = row.pgid,
                        "scratch-registry: row has no recorded start token; \
                         falling back to liveness-only reap"
                    );
                    true
                }
                (recorded, Some(current)) if recorded == current => true,
                (_, Some(_)) => {
                    tracing::info!(
                        pgid = row.pgid,
                        "scratch-registry: pgid alive but start token mismatch \
                         (recycled PID) — not killing"
                    );
                    false
                }
                // Leader gone (codex N2): kill the group if any member
                // still lives — the sequence's own probe decides.
                (_, None) => true,
            };
            let outcome = if should_kill {
                kill_pgid_lens_sequence(row.pgid).await
            } else {
                KillOutcome::AlreadyDead
            };
            let was_killed = match outcome {
                KillOutcome::Died => true,
                KillOutcome::AlreadyDead => false,
                KillOutcome::Unconfirmed => {
                    // Row NOT resolved: no reap audit (nothing was reaped),
                    // re-persist so the next restart retries.
                    tracing::error!(
                        pgid = row.pgid,
                        session_id = %row.session_id,
                        "scratch-registry startup reap: group death UNCONFIRMED; \
                         re-persisting the row for the next restart's reap"
                    );
                    unresolved.push(row);
                    continue;
                }
            };
            if was_killed {
                killed += 1;
            }
            audit.append(json!({
                "ts": iso_now(),
                "caller": shux_rpc::current_caller(),
                "method": "scratch.reap",
                "reason": "registry",
                "session_id": row.session_id,
                "pgid": row.pgid,
                "killed": was_killed,
            }));
        }
        // persist_rows_atomic removes the file when the list is empty —
        // deletion happens ONLY when every row resolved. The unresolved
        // rows also go back to the caller for live-registry seeding (P5
        // round-4 codex: without seeding, the first normal persist of the
        // fresh daemon would rewrite this file from its empty in-memory
        // rows and silently lose them).
        persist_rows_atomic(&path, &unresolved);
        (killed, unresolved)
    }

    /// Seed the LIVE registry with rows a startup reap could not resolve
    /// (P5 round-4 codex — the daemon-lifecycle half of N3): each row is
    /// inserted as a real registry row, so (a) every normal persist
    /// carries it, (b) it counts toward the quota (the group is genuinely
    /// alive), and (c) a standard reaper is armed with a short deadline so
    /// the RUNNING daemon retries the kill — row removal stays conditional
    /// on confirmed death via the existing honest-verdict machinery; a
    /// retry that is again unconfirmed leaves the row persisted for the
    /// next restart. The seeded pane id is a fresh ghost (the pane/session
    /// died with the previous daemon); pane teardown and graph destroy are
    /// no-ops for it by construction.
    ///
    /// A row whose `session_id` does not parse (P5 round-5 codex — the
    /// last clobber branch) cannot be keyed into `rows`, but the KILL only
    /// needs the pgid: it is retried inline right here. Confirmed dead →
    /// audited (reason=registry, the raw string as session_id) and
    /// dropped; unconfirmed → pushed onto the OPAQUE unresolved list that
    /// `persist` serializes alongside `rows` on every write, so the next
    /// incarnation's startup reap picks it up again. Never silently lost.
    ///
    /// Call BEFORE the RPC server starts serving, so seeded rows are
    /// quota-visible to the very first `lens.run`.
    pub async fn seed_unresolved(
        &self,
        rows: Vec<RegistryRow>,
        graph: &GraphHandle,
        io_state: &Arc<Mutex<PaneIoState>>,
        event_bus: &EventBus,
        retry_delay: Duration,
    ) {
        for row in rows {
            let Ok(session_id) = row.session_id.parse::<SessionId>() else {
                self.seed_opaque_row(row).await;
                continue;
            };
            let explicit_kill = CancellationToken::new();
            let ghost_pane = PaneId::new();
            {
                let mut inner = self.lock_inner();
                inner.rows.insert(
                    session_id,
                    ScratchState {
                        pane_id: ghost_pane,
                        pgid: row.pgid,
                        start_time: row.start_time,
                        created_at_unix_ms: row.created_at,
                        max_runtime_deadline_unix_ms: row.max_runtime_deadline,
                        explicit_kill: explicit_kill.clone(),
                        claimed: false,
                    },
                );
                self.persist(&inner);
            }
            tracing::warn!(
                %session_id,
                pgid = row.pgid,
                retry_in_ms = retry_delay.as_millis() as u64,
                "scratch-registry seed: unresolved row from the previous \
                 daemon; retrying its reap shortly"
            );
            // Standard reaper, short deadline, honest audit reason: the
            // deadline arm fires (no pane-exit event can ever match the
            // ghost pane) and the reap retries through kill_confirmed.
            tokio::spawn(scratch_reaper(
                session_id,
                ghost_pane,
                event_bus.subscribe_filtered(vec!["pane.exited".to_string()]),
                Instant::now() + retry_delay,
                0,
                graph.clone(),
                io_state.clone(),
                self.clone(),
                explicit_kill,
                "registry",
            ));
        }
    }

    /// The unparseable-session_id arm of `seed_unresolved` (P5 round-5
    /// codex): the kill sequence only needs the PGID, so the reap is
    /// retried INLINE at seed time — no session-keyed reaper required.
    /// Confirmed dead → audit + drop; unconfirmed → the row joins the
    /// opaque unresolved list so every normal persist carries it until the
    /// next incarnation's startup reap retries.
    async fn seed_opaque_row(&self, row: RegistryRow) {
        tracing::error!(
            session_id = %row.session_id,
            pgid = row.pgid,
            "scratch-registry seed: unparseable session id; retrying the \
             kill inline (the sequence only needs the pgid)"
        );
        let outcome = kill_with_retry(row.pgid).await;
        if outcome != KillOutcome::Unconfirmed {
            // Resolved: audited with the RAW id string and dropped.
            self.audit.append(json!({
                "ts": iso_now(),
                "caller": shux_rpc::current_caller(),
                "method": "scratch.reap",
                "reason": "registry",
                "session_id": row.session_id,
                "pgid": row.pgid,
                "killed": outcome == KillOutcome::Died,
            }));
            // Durable drop (P5 round-6 codex): startup_reap re-persisted
            // this row to disk BEFORE returning it for seeding — without
            // an immediate persist here, scratch-registry.json keeps the
            // now-resolved row until some unrelated later persist, and a
            // daemon restart in that window would reprocess it and
            // duplicate the registry reap audit. The row is in neither
            // `rows` nor `opaque_unresolved` at this point, so a plain
            // persist reflects the drop (and removes the file when this
            // was the last row). The PARSEABLE seeding path needs no
            // equivalent: its resolution goes through `registry.remove`,
            // which persists as part of itself, never incidentally.
            let inner = self.lock_inner();
            self.persist(&inner);
            return;
        }
        tracing::error!(
            session_id = %row.session_id,
            pgid = row.pgid,
            "scratch-registry seed: inline kill UNCONFIRMED; carrying the \
             row opaquely through every persist for the next restart"
        );
        let mut inner = self.lock_inner();
        inner.opaque_unresolved.push(row);
        self.persist(&inner);
    }
}

/// Atomically write `rows` to `path` (temp-file + rename; P5 round-1 codex
/// B2). An EMPTY list removes the file instead — "no registry" and
/// "registry with zero rows" are the same state to the startup reap.
/// Shared by `ScratchRegistry::persist` (in-memory rows) and
/// `startup_reap`'s unresolved-row re-persist (P5 round-3 codex).
fn persist_rows_atomic(path: &Path, rows: &[RegistryRow]) {
    if rows.is_empty() {
        let _ = std::fs::remove_file(path);
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("json.tmp");
    let json = match serde_json::to_vec_pretty(rows) {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!(error = %e, "scratch-registry: serialize failed");
            return;
        }
    };
    if let Err(e) = std::fs::write(&tmp, &json) {
        tracing::warn!(error = %e, path = %tmp.display(), "scratch-registry: temp write failed");
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        tracing::warn!(error = %e, path = %path.display(), "scratch-registry: rename failed");
    }
}

/// Outcome of the LENS-R-042 kill sequence — an HONEST tri-state (P5
/// round-2 codex N3: the old bool reported "killed" even when the
/// post-SIGKILL confirmation loop timed out with the group still
/// signalable, and callers then removed the registry row — resurrecting
/// the B3 orphan window for stubborn/unreaped groups).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KillOutcome {
    /// The group did not exist at entry (nothing to kill).
    AlreadyDead,
    /// The group existed and its death was CONFIRMED (signal-0 probe
    /// fails) within the bounded sequence.
    Died,
    /// The group existed and still answered the probe after SIGKILL +
    /// the confirmation window. Callers MUST NOT treat the scratch as
    /// reaped — in particular the registry row must survive so the next
    /// daemon's startup reap can finish the job.
    Unconfirmed,
}

/// Test hook (P5 round-2 codex N3 — injectable confirmation): when set,
/// `kill_pgid_lens_sequence` short-circuits to `Unconfirmed` WITHOUT
/// sending any signal, simulating a group that survives SIGKILL
/// unreaped. nextest runs one process per test, so the static cannot
/// leak across tests.
#[cfg(test)]
pub(crate) static TEST_FORCE_UNCONFIRMED_KILL: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The LENS-R-042 kill sequence: probe → killpg(SIGTERM) → 500 ms grace
/// (polling for group death — the only "wait for a process group to die"
/// primitive Unix offers a non-parent; deadline-bounded, never a sync-on-
/// output sleep) → killpg(SIGKILL) → bounded death confirmation.
///
/// pgid 0 is REJECTED (P5 round-1 claude N5): `killpg(0, sig)` signals the
/// CALLER's own process group — a persisted 0 would kill the daemon.
/// (Reported as `AlreadyDead`: there is nothing this caller may kill.)
///
/// Zombie caveat: a dead-but-unreaped group leader still "exists" to a
/// signal-0 probe until its parent (the pane PTY task) reaps it, so the
/// grace loop can conservatively run its full 500 ms for an already-dead
/// child; the SIGKILL is then a no-op and the confirm loop exits as soon
/// as the reap lands.
async fn kill_pgid_lens_sequence(pgid: u32) -> KillOutcome {
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;
    #[cfg(test)]
    if TEST_FORCE_UNCONFIRMED_KILL.load(std::sync::atomic::Ordering::SeqCst) {
        return KillOutcome::Unconfirmed;
    }
    if pgid == 0 {
        tracing::warn!("scratch reap: refusing pgid 0 (would signal our own group)");
        return KillOutcome::AlreadyDead;
    }
    let pid = Pid::from_raw(pgid as i32);
    if killpg(pid, None).is_err() {
        return KillOutcome::AlreadyDead;
    }
    let _ = killpg(pid, Signal::SIGTERM);
    let grace_deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < grace_deadline {
        if killpg(pid, None).is_err() {
            return KillOutcome::Died;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let _ = killpg(pid, Signal::SIGKILL);
    let confirm_deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < confirm_deadline {
        if killpg(pid, None).is_err() {
            return KillOutcome::Died;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    KillOutcome::Unconfirmed
}

/// The kill sequence with one bounded retry of the full sequence on an
/// unconfirmed first pass (P5 round-2 codex N3's optional retry — covers
/// the transient case where a zombie leader's reap lands just past the
/// first confirmation window). Returns the FINAL outcome.
async fn kill_with_retry(pgid: u32) -> KillOutcome {
    let first = kill_pgid_lens_sequence(pgid).await;
    if first != KillOutcome::Unconfirmed {
        return first;
    }
    tracing::warn!(
        pgid,
        "scratch reap: group survived the kill sequence unconfirmed; retrying once"
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    kill_pgid_lens_sequence(pgid).await
}

/// Kill the group and require CONFIRMED death (retry included). `false`
/// means the caller must leave the registry row in place for the next
/// daemon's startup reap.
async fn kill_confirmed(pgid: u32) -> bool {
    matches!(
        kill_with_retry(pgid).await,
        KillOutcome::AlreadyDead | KillOutcome::Died
    )
}

// ── param parsing (LENS-R-040/046) ──────────────────────────────────────

#[derive(Debug)]
struct LensRunParams {
    argv: Vec<String>,
    cols: u16,
    rows: u16,
    env: Vec<(String, String)>,
    cwd: Option<PathBuf>,
    post_exit_ttl_ms: u32,
    max_runtime_ms: u32,
    wait: bool,
}

/// Strict ranged u64 param: absent → default; present-but-not-a-u64 →
/// INVALID_PARAMS (never a silent default — the P3 strict-typing rule);
/// out of `[min, max]` → INVALID_PARAMS. Validation happens on the FULL
/// u64 BEFORE any narrowing cast (P5 convergence round 1, codex M3:
/// `{"cols": 66000}` used to wrap through `as u16` into a legal 464, and
/// `{"post_exit_ttl_ms": 4294967297}` wrapped through `as u32` into 1).
fn ranged_u64_param(
    params: &serde_json::Value,
    key: &str,
    default: u64,
    min: u64,
    max: u64,
) -> Result<u64, shux_rpc::RpcError> {
    match params.get(key) {
        None | Some(serde_json::Value::Null) => Ok(default),
        Some(v) => {
            let n = v.as_u64().ok_or_else(|| {
                shux_rpc::RpcError::invalid_params(&format!(
                    "'{key}' must be a non-negative integer"
                ))
            })?;
            if !(min..=max).contains(&n) {
                return Err(shux_rpc::RpcError::invalid_params(&format!(
                    "'{key}' {n} out of range [{min}, {max}]"
                )));
            }
            Ok(n)
        }
    }
}

fn parse_lens_run_params(params: &serde_json::Value) -> Result<LensRunParams, shux_rpc::RpcError> {
    let argv: Vec<String> = params
        .get("argv")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if argv.is_empty() {
        return Err(shux_rpc::RpcError::invalid_params(
            "'argv' is required and must be a non-empty array of strings",
        ));
    }

    // Range-checked as u64 FIRST; the casts below are provably lossless
    // (max bounds fit the target types).
    let cols = ranged_u64_param(params, "cols", DEFAULT_COLS, MIN_COLS, MAX_COLS)? as u16;
    let rows = ranged_u64_param(params, "rows", DEFAULT_ROWS, MIN_ROWS, MAX_ROWS)? as u16;
    let post_exit_ttl_ms = ranged_u64_param(
        params,
        "post_exit_ttl_ms",
        DEFAULT_POST_EXIT_TTL_MS,
        MIN_POST_EXIT_TTL_MS,
        MAX_POST_EXIT_TTL_MS,
    )? as u32;
    let max_runtime_ms = ranged_u64_param(
        params,
        "max_runtime_ms",
        DEFAULT_MAX_RUNTIME_MS,
        MIN_MAX_RUNTIME_MS,
        MAX_MAX_RUNTIME_MS,
    )? as u32;

    let env: Vec<(String, String)> = params
        .get("env")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    let cwd = params
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(PathBuf::from);

    let wait = params
        .get("wait")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    Ok(LensRunParams {
        argv,
        cols,
        rows,
        env,
        cwd,
        post_exit_ttl_ms,
        max_runtime_ms,
        wait,
    })
}

// ── reap loop (LENS-R-042) ──────────────────────────────────────────────

/// Wait for the next `pane.exited` event matching `pane_id`. Event-driven
/// (§16.1 guardrail 3 — no polling): the caller MUST have subscribed before
/// the pane's PTY was spawned so the event can't be published-and-missed
/// before this task starts listening.
async fn wait_for_pane_exit(mut sub: shux_core::bus::Subscription, pane_id: PaneId) -> Option<i32> {
    loop {
        match sub.recv().await {
            Some(SubscriptionEvent::Event(ev)) => {
                if let EventData::PaneExited {
                    pane_id: pid,
                    exit_status,
                    ..
                } = ev.data
                {
                    if pid == pane_id {
                        return exit_status;
                    }
                }
            }
            Some(SubscriptionEvent::Lagged(_)) => continue,
            None => return None,
        }
    }
}

/// Reap a claimed scratch session per LENS-R-042's exact sequence:
/// killpg(SIGTERM) → 500 ms grace → killpg(SIGKILL) → confirm dead → close
/// PTY (pane teardown) → remove session → audit. The registry row is NOT
/// touched here — the caller removes it only after this returns `true`
/// (codex B3: the row must survive any crash window so the next daemon's
/// startup reap can finish the job; the kill is idempotent).
///
/// Returns `false` when the group's death could NOT be confirmed (P5
/// round-2 codex N3) — in that case NOTHING else happens (no teardown, no
/// session destroy, no reap audit): the scratch stays visible and its row
/// stays persisted so the startup reap of the next daemon incarnation owns
/// the retry. The caller must log/propagate accordingly.
///
/// `reason` is one of exit|max_runtime|explicit (audited; R1/R4/R7 assert
/// on it).
async fn reap_scratch(
    claim: &ClaimedScratch,
    session_id: SessionId,
    graph: &GraphHandle,
    io_state: &Arc<Mutex<PaneIoState>>,
    audit: &LensAuditLog,
    reason: &str,
) -> bool {
    // 1. Kill the process group directly (LENS-R-042's own signal
    //    contract: SIGTERM → grace → SIGKILL, not the PTY task's
    //    SIGHUP-flavored teardown escalation) and CONFIRM it dead before
    //    anything else (one bounded retry inside kill_confirmed).
    if !kill_confirmed(claim.pgid).await {
        tracing::error!(
            pgid = claim.pgid,
            session_id = %session_id,
            "scratch reap: group death UNCONFIRMED after retry; leaving the \
             registry row (and the session) for the next daemon's startup reap"
        );
        return false;
    }

    // 2. Close the PTY / free the VT. The group is already dead, so the
    //    pane's PTY task sees EOF and exits promptly; teardown also clears
    //    writers/resizers/checkpoints.
    {
        let mut state = io_state.lock().await;
        let pulse = state.teardown_panes(&[claim.pane_id], true);
        drop(state);
        pulse.notify_one();
    }

    // 3. Remove the session from the graph.
    let _ = graph.destroy_session(session_id, None).await;

    // 4. Audit (LENS-R-052).
    audit.append(json!({
        "ts": iso_now(),
        "caller": shux_rpc::current_caller(),
        "method": "scratch.reap",
        "reason": reason,
        "session_id": session_id.to_string(),
        "pane_id": claim.pane_id.to_string(),
        "pgid": claim.pgid,
    }));
    true
}

/// Per-scratch reap timer (LENS-R-042 a/b): races `max_runtime_ms` against
/// the child's exit (then `post_exit_ttl_ms` more) and an explicit-kill
/// cancellation from `on_session_killed`. `biased` puts the explicit-kill
/// arm first so a simultaneous wake can never start a timer reap that an
/// explicit kill already owns (P5 round-1 claude N6); the `claim()` below
/// closes the remaining race window (both paths reap exactly once).
#[allow(clippy::too_many_arguments)]
async fn scratch_reaper(
    session_id: SessionId,
    pane_id: PaneId,
    exit_sub: shux_core::bus::Subscription,
    max_runtime_deadline: Instant,
    post_exit_ttl_ms: u32,
    graph: GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    registry: ScratchRegistry,
    explicit_kill: CancellationToken,
    // Audit reason when the DEADLINE arm fires: "max_runtime" for normal
    // scratch, "registry" for rows seeded from a prior daemon's unresolved
    // startup reap (P5 round-4 codex — that retry is a registry recovery,
    // not a runtime cap; the audit must say so).
    deadline_reason: &'static str,
) {
    let reason = tokio::select! {
        biased;
        _ = explicit_kill.cancelled() => {
            // on_session_killed owns (or already finished) the reap.
            return;
        }
        _ = tokio::time::sleep_until(max_runtime_deadline.into()) => {
            deadline_reason
        }
        exit_status = wait_for_pane_exit(exit_sub, pane_id) => {
            let _ = exit_status;
            tokio::select! {
                biased;
                _ = explicit_kill.cancelled() => return,
                _ = tokio::time::sleep_until(max_runtime_deadline.into()) => deadline_reason,
                _ = tokio::time::sleep(Duration::from_millis(post_exit_ttl_ms as u64)) => "exit",
            }
        }
    };
    // Reap-ownership claim: if an explicit kill got here first, back off.
    let Some(claim) = registry.claim(&session_id) else {
        return;
    };
    // Row removal LAST (codex B3), and ONLY on confirmed group death (P5
    // round-2 codex N3): an unconfirmed kill leaves the row — the ERROR is
    // logged inside reap_scratch, and the next daemon's startup reap owns
    // the retry.
    if reap_scratch(
        &claim,
        session_id,
        &graph,
        &io_state,
        &registry.audit,
        reason,
    )
    .await
    {
        registry.remove(&session_id);
    }
}

/// Hook for `session.kill` (LENS-R-042c: explicit kill reaps immediately).
/// The session.kill handler has already destroyed the graph session and
/// started the pane teardown by the time this runs; this claims reap
/// ownership (backing off if the timer reaper already owns it), cancels
/// the reaper, enforces the LENS-R-042 group-kill + death confirmation,
/// audits reason=explicit, and only then removes the registry row.
pub async fn on_session_killed(
    registry: &ScratchRegistry,
    io_state: &Arc<Mutex<PaneIoState>>,
    session_id: SessionId,
) {
    let Some(claim) = registry.claim(&session_id) else {
        // Not a scratch session, or the timer reaper already owns the reap.
        return;
    };
    claim.explicit_kill.cancel();
    // session.kill's own teardown escalates via the PTY task (SIGHUP →
    // SIGKILL); this enforces the LENS-R-042 SIGTERM contract on the whole
    // group and, more importantly, CONFIRMS death before the registry row
    // disappears. Signals to an already-dead group are no-ops.
    //
    // Unconfirmed death (P5 round-2 codex N3): do NOT remove the row and
    // do NOT audit a reap that didn't complete — the row (still claimed,
    // still persisted) is exactly what the next daemon's startup reap
    // needs to finish the job.
    if !kill_confirmed(claim.pgid).await {
        tracing::error!(
            pgid = claim.pgid,
            session_id = %session_id,
            "explicit scratch kill: group death UNCONFIRMED after retry; \
             leaving the registry row for the next daemon's startup reap"
        );
        return;
    }
    // Belt-and-suspenders teardown: normally a no-op (session.kill already
    // tore the pane down); covers any future caller of this hook.
    {
        let mut state = io_state.lock().await;
        let pulse = state.teardown_panes(&[claim.pane_id], true);
        drop(state);
        pulse.notify_one();
    }
    registry.audit.append(json!({
        "ts": iso_now(),
        "caller": shux_rpc::current_caller(),
        "method": "scratch.reap",
        "reason": "explicit",
        "session_id": session_id.to_string(),
        "pane_id": claim.pane_id.to_string(),
        "pgid": claim.pgid,
    }));
    registry.remove(&session_id);
}

// ── lens.run RPC (LENS-R-040/041/045/046) ────────────────────────────────

pub fn register_lens_run_method(
    builder: shux_rpc::RouterBuilder,
    graph: GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    cancel: CancellationToken,
    event_bus: EventBus,
    registry: ScratchRegistry,
) -> shux_rpc::RouterBuilder {
    builder.register_with_policy(
        // Spawns an arbitrary argv process — meaningfully more privileged
        // than the ContentRead tier the other four lens RPCs use (§5-§7).
        // No target entity exists to auto-allow ownership against before
        // creation, so this mirrors `state.apply`'s posture: grantable, but
        // never default-allow for plugins (LENS-R-050's `scratch:create`
        // scope maps onto the existing coarse Sensitivity tiers the same
        // way P2-P4's `pane:observe` intent mapped onto ContentRead — no
        // finer-grained scope strings exist in the permission model yet).
        // Caveat (accepted by both P5 reviewers): the grant NAME is
        // `lens.run`, not `scratch:create`, and a Grantable-granted plugin
        // inherits scratch-spawn authority — a pre-existing limit of the
        // 4-tier model, not new surface.
        "lens.run",
        Policy::fixed(Sensitivity::Grantable),
        move |params: Option<serde_json::Value>| {
            let graph = graph.clone();
            let io_state = io_state.clone();
            let cancel = cancel.clone();
            let event_bus = event_bus.clone();
            let registry = registry.clone();
            async move {
                handle_lens_run(
                    params.unwrap_or_default(),
                    graph,
                    io_state,
                    cancel,
                    event_bus,
                    registry,
                )
                .await
            }
        },
    )
}

async fn handle_lens_run(
    params: serde_json::Value,
    graph: GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    cancel: CancellationToken,
    event_bus: EventBus,
    registry: ScratchRegistry,
) -> Result<serde_json::Value, shux_rpc::RpcError> {
    let p = parse_lens_run_params(&params)?;

    // ── Cancellation shield (P5 round-2 codex N1 ≡ claude round-2 major) ─
    // The shux-rpc server DROPS in-flight handler futures on client
    // disconnect (the P3 cancellable-execution contract). A disconnect
    // between graph-session creation and registry commit used to leak an
    // unregistered __scratch session + PTY: the reservation Drop-released,
    // but nothing owned the session — no reaper, no registry row, no
    // startup-reap coverage (the PTY task's own shutdown binds to the ROOT
    // daemon token, never to the request, so handler-drop cannot cascade
    // teardown either). Fix: the non-idempotent core (reserve → create
    // session → spawn PTY → commit + arm reaper) runs in its OWN spawned
    // task; the handler awaits its JoinHandle. Dropping a JoinHandle does
    // NOT abort the task, so a disconnected client simply never reads the
    // response while the composite completes — from commit onward the
    // ttl/max_runtime reaper owns the scratch. Everything after the core
    // (the `--wait` tail) stays freely cancellable per P3 semantics.
    //
    // The task-local caller identity does not cross `tokio::spawn`, so the
    // core future is re-wrapped in the captured caller's scope (keeps
    // scratch.create audit attribution truthful — LENS-R-052).
    let caller = shux_rpc::current_caller();
    let core = tokio::spawn(shux_rpc::with_caller(
        caller,
        spawn_scratch_core(p, graph, io_state, cancel, event_bus, registry),
    ));
    let (session_id, pane_id, revision, wait_sub) = match core.await {
        Ok(core_result) => core_result?,
        Err(join_err) => {
            // Only reachable on a core panic (never aborted). Repo law
            // forbids panics; surface honestly instead of unwrapping.
            return Err(shux_rpc::RpcError::internal(&format!(
                "lens.run core task failed: {join_err}"
            )));
        }
    };

    let mut result = json!({
        "session_id": session_id.to_string(),
        "pane_id": pane_id.to_string(),
        "revision": revision,
    });

    if let Some(sub) = wait_sub {
        let exit_code = wait_for_pane_exit(sub, pane_id).await.unwrap_or(-1);
        result["exit_code"] = json!(exit_code);
    }

    Ok(result)
}

/// Test pause points for the N1 cancellation tests: a slot, when armed,
/// makes the core signal `reached` and block until `release` — so a test
/// can drop the dispatching future at an exact interior point and prove
/// the shielded core still completes. nextest's process-per-test isolates
/// the statics.
#[cfg(test)]
pub(crate) mod test_hooks {
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::sync::Notify;

    pub(crate) struct Pause {
        pub reached: Notify,
        pub release: Notify,
    }

    impl Pause {
        pub(crate) fn arm(slot: &StdMutex<Option<Arc<Pause>>>) -> Arc<Pause> {
            let p = Arc::new(Pause {
                reached: Notify::new(),
                release: Notify::new(),
            });
            *slot.lock().unwrap() = Some(p.clone());
            p
        }
    }

    /// Between graph-session creation and PTY spawn.
    pub(crate) static PAUSE_AFTER_CREATE: StdMutex<Option<Arc<Pause>>> = StdMutex::new(None);
    /// Between PTY spawn and registry commit.
    pub(crate) static PAUSE_BEFORE_COMMIT: StdMutex<Option<Arc<Pause>>> = StdMutex::new(None);

    pub(crate) async fn maybe_pause(slot: &StdMutex<Option<Arc<Pause>>>) {
        let armed = slot.lock().unwrap().clone();
        if let Some(p) = armed {
            p.reached.notify_one();
            p.release.notified().await;
        }
    }
}

/// The non-idempotent core of `lens.run` (see the shield comment in
/// `handle_lens_run`): reserve quota → create the graph session → spawn
/// the PTY → commit the registry row + arm the reaper → audit. Runs as its
/// own task so client disconnect can never leave a partially-created
/// scratch behind; every failure path inside rolls back completely (the
/// reservation by Drop, the session by explicit destroy).
async fn spawn_scratch_core(
    p: LensRunParams,
    graph: GraphHandle,
    io_state: Arc<Mutex<PaneIoState>>,
    cancel: CancellationToken,
    event_bus: EventBus,
    registry: ScratchRegistry,
) -> Result<(SessionId, PaneId, u64, Option<shux_core::bus::Subscription>), shux_rpc::RpcError> {
    // LENS-R-043: atomic check-and-reserve BEFORE any allocation (codex
    // B1 — a bare len() check raced concurrent lens.run calls past the
    // quota). The reservation releases itself on EVERY early return below
    // (Drop) and converts into the committed row on success.
    let reservation = registry.try_reserve().map_err(|(current, max)| {
        shux_rpc::RpcError::resource_exhausted("scratch_session", current, max)
    })?;

    let cwd = p
        .cwd
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp")));

    // Subscribe to pane-exit events BEFORE the graph/PTY allocation exists,
    // so a fast-exiting command (F6: prints BYE, exits 42) can never race
    // ahead of the listener (happens-before by construction — both
    // subscriptions below are created strictly before `spawn_pane_pty`
    // returns, which is strictly before the PTY task can publish the
    // event).
    let exit_sub_for_reaper = event_bus.subscribe_filtered(vec!["pane.exited".to_string()]);
    let exit_sub_for_wait = p
        .wait
        .then(|| event_bus.subscribe_filtered(vec!["pane.exited".to_string()]));

    // DEC-21: allocate session+window+pane via the SAME internal graph
    // entrypoint session.create/ensure use — but there is no public
    // scratch parameter on it and this is the ONLY caller with a path to
    // it that then execs argv directly (no shell, ever).
    let scratch_name = format!("__scratch-{}", uuid::Uuid::new_v4());
    let session_id = graph
        .create_session_with_command(scratch_name, cwd.clone(), p.argv.clone())
        .await
        .map_err(|e| shux_rpc::RpcError::internal(&format!("scratch allocation failed: {e}")))?;

    #[cfg(test)]
    test_hooks::maybe_pause(&test_hooks::PAUSE_AFTER_CREATE).await;

    let snap = graph.snapshot();
    let pane_id = snap
        .sessions
        .get(&session_id)
        .and_then(|s| s.windows.first())
        .and_then(|wid| snap.windows.get(wid))
        .map(|w| w.active_pane);
    let Some(pane_id) = pane_id else {
        let _ = graph.destroy_session(session_id, None).await;
        return Err(shux_rpc::RpcError::internal(
            "scratch session created with no pane",
        ));
    };
    drop(snap);

    let size = shux_pty::handle::PtySize::new(p.cols, p.rows);
    let spawn_result = crate::spawn_pane_pty(
        pane_id,
        cwd,
        p.argv.clone(),
        size,
        p.env,
        io_state.clone(),
        cancel.clone(),
        graph.clone(),
    )
    .await;

    if let Err(e) = spawn_result {
        // LENS-R-040/045: SPAWN_FAILED rolls back the allocation completely
        // — no session, no pane, no PTY, absent from `--include-scratch`
        // (and the quota reservation releases via Drop).
        let _ = graph.destroy_session(session_id, None).await;
        return Err(shux_rpc::RpcError::spawn_failed(&e.to_string()));
    }

    let pgid = {
        // The PTY child is its own process group leader (setsid in
        // pre_exec, shux-pty/handle.rs), so pid == pgid. `spawn_pane_pty`
        // records the pid in `PaneIoState.pty_pids` under the same lock it
        // inserts every other per-pane state into, so this read is always
        // consistent with the spawn we just awaited.
        let state = io_state.lock().await;
        state.pty_pids.get(&pane_id).copied().unwrap_or(0)
    };
    if pgid == 0 {
        // Should be unreachable (spawn succeeded ⇒ pid recorded), but a 0
        // must NEVER be persisted: killpg(0) signals the daemon's own
        // group (claude N5). Roll back rather than register an unkillable
        // row.
        tracing::error!(%pane_id, "lens.run: spawned pane has no recorded pgid; rolling back");
        let _ = graph.destroy_session(session_id, None).await;
        let mut state = io_state.lock().await;
        let pulse = state.teardown_panes(&[pane_id], true);
        drop(state);
        pulse.notify_one();
        return Err(shux_rpc::RpcError::internal(
            "scratch spawn lost its process id",
        ));
    }
    // LENS-R-044 (codex M4, adjudicated): record the leader's OS start
    // token so a future startup reap can tell this process apart from a
    // recycled PID. Best-effort — 0 means "unknown" (startup reap then
    // falls back to liveness-only).
    let start_time = process_start_token(pgid).unwrap_or(0);

    #[cfg(test)]
    test_hooks::maybe_pause(&test_hooks::PAUSE_BEFORE_COMMIT).await;

    let now = SystemTime::now();
    let created_at_unix_ms = unix_ms(now);
    let max_runtime_deadline = Instant::now() + Duration::from_millis(p.max_runtime_ms as u64);
    let max_runtime_deadline_unix_ms = created_at_unix_ms + p.max_runtime_ms as u64;
    let explicit_kill = CancellationToken::new();

    // Commit the reservation into the registry row BEFORE spawning the
    // reaper task: the reaper can call `registry.claim/remove` as soon as
    // one of its branches fires, and "insert happens-before any matching
    // remove" must hold by construction, not by timing.
    reservation.commit(
        session_id,
        ScratchState {
            pane_id,
            pgid,
            start_time,
            created_at_unix_ms,
            max_runtime_deadline_unix_ms,
            explicit_kill: explicit_kill.clone(),
            claimed: false,
        },
    );

    // Fire-and-forget: nothing joins this task. Cancelling `explicit_kill`
    // (via `on_session_killed`) is how a caller makes it return promptly;
    // otherwise it runs until one of its own timer/event branches fires.
    tokio::spawn(scratch_reaper(
        session_id,
        pane_id,
        exit_sub_for_reaper,
        max_runtime_deadline,
        p.post_exit_ttl_ms,
        graph.clone(),
        io_state.clone(),
        registry.clone(),
        explicit_kill,
        "max_runtime",
    ));

    registry.audit.append(json!({
        "ts": iso_now(),
        "caller": shux_rpc::current_caller(),
        "method": "scratch.create",
        "session_id": session_id.to_string(),
        "pane_id": pane_id.to_string(),
        "pgid": pgid,
        "argv": p.argv,
        "cols": p.cols,
        "rows": p.rows,
    }));

    let revision = {
        let state = io_state.lock().await;
        state
            .vts
            .get(&pane_id)
            .map(|vt| vt.content_revision())
            .unwrap_or(1)
    };

    Ok((session_id, pane_id, revision, exit_sub_for_wait))
}

// ── test hooks ───────────────────────────────────────────────────────────

#[cfg(test)]
impl ScratchRegistry {
    /// Occupy `n` quota slots without any real scratch behind them (quota
    /// concurrency tests: fill to `SCRATCH_QUOTA - 1`, then race real
    /// `lens.run` calls for the last slot).
    pub fn test_occupy(&self, n: usize) {
        self.lock_inner().reserved += n;
    }

    /// Current `rows + reserved + opaque` total (quota accounting
    /// observable — mirrors `try_reserve`'s arithmetic exactly).
    pub fn test_total(&self) -> usize {
        let inner = self.lock_inner();
        inner.rows.len() + inner.reserved + inner.opaque_unresolved.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn test_registry(dir: &Path) -> ScratchRegistry {
        let audit = LensAuditLog::open(dir);
        ScratchRegistry::new(dir, audit)
    }

    fn dummy_state(pgid: u32) -> ScratchState {
        ScratchState {
            pane_id: PaneId::new(),
            pgid,
            start_time: 12345,
            created_at_unix_ms: 1,
            max_runtime_deadline_unix_ms: 2,
            explicit_kill: CancellationToken::new(),
            claimed: false,
        }
    }

    // ── B1: atomic quota reservation ────────────────────────────────────

    #[test]
    fn reserve_admits_exactly_one_at_the_last_slot() {
        let dir = tmpdir();
        let reg = test_registry(dir.path());
        reg.test_occupy(SCRATCH_QUOTA - 1);
        // Two racers at 15/16: exactly one reservation may win, atomically.
        let a = reg.try_reserve();
        let b = reg.try_reserve();
        assert!(a.is_ok() != b.is_ok(), "exactly one racer wins the slot");
        let (current, max) = match (a, b) {
            (Ok(_keep), Err(e)) | (Err(e), Ok(_keep)) => e,
            _ => unreachable!("asserted exactly-one above"),
        };
        assert_eq!(current, SCRATCH_QUOTA);
        assert_eq!(max, SCRATCH_QUOTA);
    }

    #[test]
    fn dropped_reservation_releases_its_slot() {
        let dir = tmpdir();
        let reg = test_registry(dir.path());
        reg.test_occupy(SCRATCH_QUOTA - 1);
        {
            let r = reg.try_reserve().expect("slot free");
            assert_eq!(reg.test_total(), SCRATCH_QUOTA);
            drop(r); // failure path: rollback without commit
        }
        assert_eq!(reg.test_total(), SCRATCH_QUOTA - 1);
        assert!(reg.try_reserve().is_ok(), "released slot is reusable");
    }

    #[test]
    fn committed_reservation_becomes_a_row_without_double_counting() {
        let dir = tmpdir();
        let reg = test_registry(dir.path());
        let r = reg.try_reserve().expect("reserve");
        let id = SessionId::new();
        r.commit(id, dummy_state(4242));
        assert_eq!(reg.test_total(), 1, "one row, zero reserved");
        assert!(reg.ids().contains(&id));
    }

    // ── B2: crash-safe persist + corrupt preservation ───────────────────

    #[test]
    fn persist_is_atomic_rename_and_leaves_no_temp_file() {
        let dir = tmpdir();
        let reg = test_registry(dir.path());
        let r = reg.try_reserve().expect("reserve");
        r.commit(SessionId::new(), dummy_state(777));

        let path = dir.path().join("scratch-registry.json");
        let tmp = path.with_extension("json.tmp");
        assert!(path.exists(), "registry persisted");
        assert!(!tmp.exists(), "no temp residue after rename");
        let rows: Vec<RegistryRow> = serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
            .expect("persisted registry is valid JSON");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pgid, 777);
        assert_eq!(rows[0].start_time, 12345);

        // Second persist overwrites atomically (rename over existing).
        let r2 = reg.try_reserve().expect("reserve 2");
        r2.commit(SessionId::new(), dummy_state(888));
        let rows: Vec<RegistryRow> = serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
            .expect("still valid JSON after overwrite");
        assert_eq!(rows.len(), 2);
        assert!(!tmp.exists());
    }

    /// Files under `dir` whose name marks them as preserved corrupt
    /// registries (`scratch-registry.json.corrupt.<unix_ms>`).
    fn corrupt_files(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.contains(".json.corrupt."))
            })
            .collect()
    }

    #[tokio::test]
    async fn startup_reap_preserves_corrupt_registry_as_evidence() {
        let dir = tmpdir();
        let path = dir.path().join("scratch-registry.json");
        // A crash mid-write leaves truncated JSON.
        let truncated = "[{\"session_id\": \"abc\", \"pgid\": 12";
        std::fs::write(&path, truncated).unwrap();

        let audit = LensAuditLog::open(dir.path());
        let (killed, unresolved) = ScratchRegistry::startup_reap(dir.path(), &audit).await;
        assert!(unresolved.is_empty(), "corrupt file resolves nothing");
        assert_eq!(killed, 0, "corrupt registry kills nothing");
        assert!(!path.exists(), "original name freed for the new daemon");
        let preserved = corrupt_files(dir.path());
        assert_eq!(preserved.len(), 1, "evidence preserved (timestamped)");
        assert_eq!(
            std::fs::read_to_string(&preserved[0]).unwrap(),
            truncated,
            "evidence bytes untouched"
        );

        // P5 round-2 codex minor: a SECOND corrupt startup must not
        // overwrite the first evidence file.
        std::fs::write(&path, "{second corruption").unwrap();
        // The timestamp suffix has ms resolution; ensure it differs.
        std::thread::sleep(Duration::from_millis(5));
        let (killed, unresolved) = ScratchRegistry::startup_reap(dir.path(), &audit).await;
        assert!(
            unresolved.is_empty(),
            "second corrupt startup resolves nothing"
        );
        assert_eq!(killed, 0);
        let preserved = corrupt_files(dir.path());
        assert_eq!(
            preserved.len(),
            2,
            "repeated corrupt startups keep distinct evidence files: {preserved:?}"
        );
    }

    // ── M4: start-token match ───────────────────────────────────────────

    /// Spawn a real throwaway process in its OWN process group (so the
    /// kill paths under test can never signal the test runner's group).
    fn spawn_group_leader() -> std::process::Child {
        use std::os::unix::process::CommandExt;
        std::process::Command::new("sleep")
            .arg("300")
            .process_group(0)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn sleep group leader")
    }

    fn write_registry_row(dir: &Path, pgid: u32, start_time: u64) {
        let rows = vec![RegistryRow {
            session_id: SessionId::new().to_string(),
            pgid,
            start_time,
            created_at: 1,
            max_runtime_deadline: 2,
        }];
        std::fs::write(
            dir.join("scratch-registry.json"),
            serde_json::to_vec_pretty(&rows).unwrap(),
        )
        .unwrap();
    }

    fn pid_alive(pid: u32) -> bool {
        use nix::sys::signal::kill;
        use nix::unistd::Pid;
        kill(Pid::from_raw(pid as i32), None).is_ok()
    }

    #[tokio::test]
    async fn startup_reap_spares_mismatched_start_token_but_clears_the_row() {
        let dir = tmpdir();
        let mut child = spawn_group_leader();
        let pid = child.id();
        let real_token = process_start_token(pid).expect("start token of live child");
        // Recycled-PID simulation: recorded token differs from the live one.
        write_registry_row(dir.path(), pid, real_token + 1);

        let audit = LensAuditLog::open(dir.path());
        let (killed, unresolved) = ScratchRegistry::startup_reap(dir.path(), &audit).await;
        assert!(
            unresolved.is_empty(),
            "a spared recycled-PID row is resolved (dropped)"
        );
        assert_eq!(killed, 0, "mismatched start token must not kill");
        assert!(pid_alive(pid), "innocent recycled-PID process survives");
        assert!(
            !dir.path().join("scratch-registry.json").exists(),
            "row still cleared with the registry file"
        );

        let _ = child.kill();
        let _ = child.wait();
    }

    #[tokio::test]
    async fn startup_reap_kills_on_matching_start_token() {
        let dir = tmpdir();
        let mut child = spawn_group_leader();
        let pid = child.id();
        let token = process_start_token(pid).expect("start token of live child");
        write_registry_row(dir.path(), pid, token);

        let audit = LensAuditLog::open(dir.path());
        let (killed, unresolved) = ScratchRegistry::startup_reap(dir.path(), &audit).await;
        assert!(
            unresolved.is_empty(),
            "a confirmed kill leaves nothing unresolved"
        );
        assert_eq!(killed, 1, "matching start token reaps the group");
        // Reap the zombie so the liveness probe below is honest.
        let _ = child.wait();
        assert!(!pid_alive(pid), "group leader killed");
        assert!(!dir.path().join("scratch-registry.json").exists());
        // Audit entry with reason=registry landed and the chain verifies.
        let n = verify_chain(&dir.path().join("lens-audit.ndjson")).expect("chain verifies");
        assert_eq!(n, 1);
    }

    // ── N2 (round 2): leader-gone groups still get reaped ───────────────

    #[tokio::test]
    async fn startup_reap_kills_orphaned_descendants_when_leader_is_gone() {
        // `sh -c 'sleep 300 & exit'` in its own group: the leader exits
        // immediately, the sleep survives IN THE GROUP — the exact shape
        // codex N2 proved the old start-token gate orphaned (token None →
        // skip). A pgid stays allocated while any member lives, so the
        // group probe must kill it.
        use std::os::unix::process::CommandExt;
        let dir = tmpdir();
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sleep 300 & exit 0"])
            .process_group(0)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn leader-exits group");
        let pgid = child.id();
        // Reap the leader zombie so only the live sleep keeps the group
        // alive (a zombie leader would also hold it, masking the point).
        let _ = child.wait();
        assert!(
            process_start_token(pgid).is_none(),
            "leader must be gone (start token unreadable)"
        );
        {
            use nix::sys::signal::killpg;
            use nix::unistd::Pid;
            assert!(
                killpg(Pid::from_raw(pgid as i32), None).is_ok(),
                "group must still be alive via the surviving descendant"
            );
        }

        // Row recorded with a real (nonzero) token — by restart time the
        // leader is unreadable, which is precisely the (recorded, None) arm.
        write_registry_row(dir.path(), pgid, 98765);
        let audit = LensAuditLog::open(dir.path());
        let (killed, unresolved) = ScratchRegistry::startup_reap(dir.path(), &audit).await;
        assert!(
            unresolved.is_empty(),
            "a reaped leader-gone group leaves nothing unresolved"
        );
        assert_eq!(killed, 1, "leader-gone live group must be reaped");
        {
            use nix::sys::signal::killpg;
            use nix::unistd::Pid;
            assert!(
                killpg(Pid::from_raw(pgid as i32), None).is_err(),
                "descendant sleep killed with the group"
            );
        }
        assert!(!dir.path().join("scratch-registry.json").exists());
    }

    // ── N5: pgid 0 guard ────────────────────────────────────────────────

    #[tokio::test]
    async fn kill_sequence_refuses_pgid_zero() {
        // killpg(0) would signal OUR own process group; the guard must
        // refuse before any signal is sent (the test surviving IS the
        // assertion). AlreadyDead = "nothing this caller may kill".
        assert_eq!(kill_pgid_lens_sequence(0).await, KillOutcome::AlreadyDead);
    }

    // ── N3 (round 2): honest kill confirmation ──────────────────────────

    #[tokio::test]
    async fn forced_unconfirmed_kill_is_reported_honestly() {
        use std::sync::atomic::Ordering;
        TEST_FORCE_UNCONFIRMED_KILL.store(true, Ordering::SeqCst);
        assert_eq!(
            kill_pgid_lens_sequence(12345).await,
            KillOutcome::Unconfirmed,
            "forced path reports Unconfirmed, never a false kill"
        );
        // kill_confirmed retries once, stays honest, returns false.
        assert!(
            !kill_confirmed(12345).await,
            "unconfirmed death must not read as confirmed"
        );
        TEST_FORCE_UNCONFIRMED_KILL.store(false, Ordering::SeqCst);
    }

    // ── N3 at the STARTUP path (round 3): unconfirmed rows re-persist ───

    #[tokio::test]
    async fn startup_reap_repersists_unconfirmed_rows_for_the_next_restart() {
        use std::sync::atomic::Ordering;
        let dir = tmpdir();
        let path = dir.path().join("scratch-registry.json");
        // A REAL live group, so the second (unforced) startup genuinely
        // reaps something.
        let mut child = spawn_group_leader();
        let pid = child.id();
        let token = process_start_token(pid).expect("start token of live child");
        write_registry_row(dir.path(), pid, token);
        let audit = LensAuditLog::open(dir.path());

        // First startup: the kill sequence is forced Unconfirmed — the row
        // must survive (re-persisted), with NO reap audit (round 3: the
        // old unconditional delete made a stubborn group invisible to all
        // future restart reaps — the B3-class hole at startup).
        TEST_FORCE_UNCONFIRMED_KILL.store(true, Ordering::SeqCst);
        let (killed, unresolved) = ScratchRegistry::startup_reap(dir.path(), &audit).await;
        assert_eq!(
            unresolved.len(),
            1,
            "the unconfirmed row is returned for seeding"
        );
        assert_eq!(unresolved[0].pgid, pid, "returned row is the original");
        TEST_FORCE_UNCONFIRMED_KILL.store(false, Ordering::SeqCst);
        assert_eq!(killed, 0, "unconfirmed rows are not counted as killed");
        assert!(
            path.exists(),
            "registry survives an unconfirmed startup reap"
        );
        let survivors: Vec<RegistryRow> =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
                .expect("re-persisted registry is valid JSON");
        assert_eq!(survivors.len(), 1, "the unresolved row was re-persisted");
        assert_eq!(survivors[0].pgid, pid);
        let audit_path = dir.path().join("lens-audit.ndjson");
        let reap_audits = std::fs::read_to_string(&audit_path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|e| e["method"] == "scratch.reap")
            .count();
        assert_eq!(reap_audits, 0, "no reap audit for an unresolved row");

        // Second startup, flag cleared: the retry reaps for real.
        let (killed, unresolved) = ScratchRegistry::startup_reap(dir.path(), &audit).await;
        assert!(
            unresolved.is_empty(),
            "the confirmed retry resolves the row"
        );
        assert_eq!(killed, 1, "the re-persisted row is reaped on retry");
        let _ = child.wait(); // reap the zombie before probing
        assert!(!pid_alive(pid), "group leader killed on the retry");
        assert!(!path.exists(), "registry removed once every row resolved");
        assert!(
            std::fs::read_to_string(&audit_path)
                .unwrap_or_default()
                .lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .any(|e| e["method"] == "scratch.reap" && e["reason"] == "registry"),
            "the completed retry IS audited"
        );
    }

    // ── M3: validate before casting ─────────────────────────────────────

    #[test]
    fn params_reject_u16_wrapping_cols() {
        // 66000 % 65536 = 464 — the wrap would land back inside a legal-
        // looking range for a u16 comparison done after the cast.
        let err = parse_lens_run_params(&json!({"argv": ["sleep"], "cols": 66000}))
            .expect_err("66000 cols must be INVALID_PARAMS");
        assert_eq!(err.code, -32602);
        let err = parse_lens_run_params(&json!({"argv": ["sleep"], "rows": 65536 + 24}))
            .expect_err("wrapped rows must be INVALID_PARAMS");
        assert_eq!(err.code, -32602);
    }

    #[test]
    fn params_reject_u32_wrapping_ttl_and_runtime() {
        // 2^32 + 1 wraps to 1 through `as u32`.
        let err = parse_lens_run_params(
            &json!({"argv": ["sleep"], "post_exit_ttl_ms": 4_294_967_297u64}),
        )
        .expect_err("wrapped ttl must be INVALID_PARAMS");
        assert_eq!(err.code, -32602);
        let err =
            parse_lens_run_params(&json!({"argv": ["sleep"], "max_runtime_ms": 4_294_967_297u64}))
                .expect_err("wrapped max_runtime must be INVALID_PARAMS");
        assert_eq!(err.code, -32602);
    }

    #[test]
    fn params_reject_wrong_types_strictly() {
        // P3 strict-typing rule: present-but-mistyped is an error, never
        // a silent default.
        assert_eq!(
            parse_lens_run_params(&json!({"argv": ["sleep"], "cols": "80"}))
                .unwrap_err()
                .code,
            -32602
        );
        assert_eq!(
            parse_lens_run_params(&json!({"argv": ["sleep"], "rows": 24.5}))
                .unwrap_err()
                .code,
            -32602
        );
        assert_eq!(
            parse_lens_run_params(&json!({"argv": ["sleep"], "post_exit_ttl_ms": true}))
                .unwrap_err()
                .code,
            -32602
        );
        assert_eq!(
            parse_lens_run_params(&json!({"argv": ["sleep"], "max_runtime_ms": -1}))
                .unwrap_err()
                .code,
            -32602
        );
    }

    #[test]
    fn params_defaults_and_boundaries_accepted() {
        let p = parse_lens_run_params(&json!({"argv": ["sleep"]})).expect("defaults");
        assert_eq!((p.cols, p.rows), (80, 24));
        assert_eq!(p.post_exit_ttl_ms, 30_000);
        assert_eq!(p.max_runtime_ms, 3_600_000);
        let p = parse_lens_run_params(&json!({
            "argv": ["sleep"], "cols": 500, "rows": 200,
            "post_exit_ttl_ms": 300_000, "max_runtime_ms": 86_400_000
        }))
        .expect("max bounds accepted");
        assert_eq!((p.cols, p.rows), (500, 200));
        assert_eq!(p.post_exit_ttl_ms, 300_000);
        assert_eq!(p.max_runtime_ms, 86_400_000);
    }

    // ── M2: audit chain (concurrency, rotation, verification) ──────────

    #[test]
    fn audit_chain_verifies_and_detects_tampering() {
        let dir = tmpdir();
        let audit = LensAuditLog::open(dir.path());
        for i in 0..5 {
            audit.append(
                json!({"ts": iso_now(), "caller": "uds", "method": "scratch.create", "i": i}),
            );
        }
        let path = dir.path().join("lens-audit.ndjson");
        assert_eq!(verify_chain(&path).expect("clean chain verifies"), 5);

        // Tamper one byte in the middle entry's payload.
        let text = std::fs::read_to_string(&path).unwrap();
        let tampered = text.replacen("\"i\":2", "\"i\":9", 1);
        assert_ne!(text, tampered, "tamper applied");
        std::fs::write(&path, tampered).unwrap();
        assert!(
            verify_chain(&path).is_err(),
            "tampered chain must fail verification"
        );
    }

    // ── prefix deletion (round 3): the anchor is never self-declared ────

    #[test]
    fn audit_prefix_deletion_is_detected() {
        let dir = tmpdir();
        let audit = LensAuditLog::open(dir.path());
        for i in 0..5 {
            audit.append(
                json!({"ts": iso_now(), "caller": "uds", "method": "scratch.create", "i": i}),
            );
        }
        let path = dir.path().join("lens-audit.ndjson");
        assert_eq!(verify_chain(&path).expect("intact log verifies"), 5);

        // Delete the first 2 lines: the remaining chain is internally
        // consistent, but its first entry's prev_hash is neither genesis
        // nor a rotation continuation — a freely-adopted anchor used to
        // wave this through (codex round-3 minor).
        let text = std::fs::read_to_string(&path).unwrap();
        let truncated: String = text.lines().skip(2).map(|l| format!("{l}\n")).collect();
        std::fs::write(&path, truncated).unwrap();
        let err = verify_chain(&path).expect_err("prefix deletion must fail");
        assert!(
            err.contains("unjustified chain anchor"),
            "failure names the unjustified anchor: {err}"
        );
        // The set walker applies the same strictness to its oldest file.
        let err = verify_chain_set(&path).expect_err("set walk rejects it too");
        assert!(err.contains("unjustified chain anchor"), "{err}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn audit_concurrent_appends_never_fork_the_chain() {
        let dir = tmpdir();
        let audit = LensAuditLog::open(dir.path());
        let mut handles = Vec::new();
        for i in 0..24 {
            let a = audit.clone();
            handles.push(tokio::task::spawn_blocking(move || {
                a.append(
                    json!({"ts": iso_now(), "caller": "uds", "method": "scratch.create", "n": i}),
                );
            }));
        }
        for h in handles {
            h.await.expect("append task");
        }
        let n = verify_chain(&dir.path().join("lens-audit.ndjson"))
            .expect("chain intact under concurrency");
        assert_eq!(n, 24, "every append landed exactly once");
    }

    /// Append entries with a fat padding field until the live file exceeds
    /// the rotation cap and one more append triggers the rotation.
    fn fill_one_rotation(audit: &LensAuditLog, live: &Path, tag: &str) {
        let pad = "x".repeat(16 * 1024);
        let mut i = 0usize;
        while std::fs::metadata(live).map(|m| m.len()).unwrap_or(0) <= AUDIT_ROTATE_AT_BYTES {
            audit.append(json!({
                "ts": iso_now(), "caller": "uds", "method": "scratch.create",
                "tag": tag, "n": i, "pad": pad,
            }));
            i += 1;
        }
        // One more append rotates (the size check runs BEFORE the write).
        audit.append(json!({
            "ts": iso_now(), "caller": "uds", "method": "scratch.create",
            "tag": tag, "n": i, "pad": pad,
        }));
    }

    #[test]
    fn audit_rotation_carries_the_chain_across_files() {
        let dir = tmpdir();
        let path = dir.path().join("lens-audit.ndjson");
        let audit = LensAuditLog::open(dir.path());

        // Two full rotations of REAL entries → .2, .1, live all chained.
        fill_one_rotation(&audit, &path, "gen1");
        assert!(path.with_extension("ndjson.1").exists(), "first rotation");
        fill_one_rotation(&audit, &path, "gen2");
        assert!(
            path.with_extension("ndjson.2").exists(),
            "second rotation shifted .1 → .2"
        );

        // The live file opens with the rotation header, chained off the
        // rotated-out file's final hash (NOT genesis — codex round-2 minor).
        let live_text = std::fs::read_to_string(&path).unwrap();
        let first: serde_json::Value =
            serde_json::from_str(live_text.lines().next().unwrap()).unwrap();
        assert_eq!(first["method"], "audit.rotate", "rotation header first");
        assert!(first["predecessor"].as_str().unwrap().contains("ndjson.1"));
        assert_ne!(
            first["prev_hash"], GENESIS_HASH,
            "chain carries across the boundary instead of restarting"
        );

        // The whole set verifies as ONE chain.
        let total = verify_chain_set(&path).expect("cross-file chain verifies");
        assert!(total > 0);

        // Reordering rotated files (swap .1 and .2) breaks the walk.
        let f1 = path.with_extension("ndjson.1");
        let f2 = path.with_extension("ndjson.2");
        let (b1, b2) = (std::fs::read(&f1).unwrap(), std::fs::read(&f2).unwrap());
        std::fs::write(&f1, &b2).unwrap();
        std::fs::write(&f2, &b1).unwrap();
        assert!(
            verify_chain_set(&path).is_err(),
            "reordered rotation files must fail set verification"
        );
        std::fs::write(&f1, &b1).unwrap();
        std::fs::write(&f2, &b2).unwrap();
        assert!(verify_chain_set(&path).is_ok(), "restored set verifies");

        // Deleting an INTERIOR rotated file (.1 — between .2 and live)
        // breaks the walk: the live file's anchor no longer matches .2's
        // final hash.
        std::fs::remove_file(&f1).unwrap();
        assert!(
            verify_chain_set(&path).is_err(),
            "deleted interior rotation file must fail set verification"
        );
    }

    // ── rotated-file verification guard (round 4) ───────────────────────

    #[test]
    fn verify_chain_rejects_direct_rotated_file_arguments() {
        let dir = tmpdir();
        let path = dir.path().join("lens-audit.ndjson");
        let audit = LensAuditLog::open(dir.path());
        fill_one_rotation(&audit, &path, "gen1");
        let rotated = path.with_extension("ndjson.1");
        assert!(rotated.exists(), "rotation happened");

        // Direct rotated-file verification is rejected — handed the .1
        // path, the set resolution would derive `….ndjson.1.N` siblings
        // and TrustedStart would self-justify the continuation header
        // even with a tampered/absent predecessor (codex round-4 minor).
        let err = verify_chain(&rotated).expect_err("rotated file must be rejected");
        assert!(err.contains("rotated audit file"), "clear error: {err}");
        let err = verify_chain_set(&rotated).expect_err("set walk rejects it too");
        assert!(err.contains("rotated audit file"), "clear error: {err}");

        // The right entrypoint still works: the LIVE path verifies the set.
        assert!(
            verify_chain_set(&path).is_ok(),
            "live-path set walk is the API"
        );
    }

    // ── reap claim dedup (N6) ───────────────────────────────────────────

    #[test]
    fn claim_is_exactly_once() {
        let dir = tmpdir();
        let reg = test_registry(dir.path());
        let id = SessionId::new();
        let r = reg.try_reserve().expect("reserve");
        r.commit(id, dummy_state(999));

        let first = reg.claim(&id);
        assert!(first.is_some(), "first claimant owns the reap");
        assert!(reg.claim(&id).is_none(), "second claimant backs off");
        // The claimed row is still registered (and persisted) until remove.
        assert!(reg.ids().contains(&id));
        reg.remove(&id);
        assert!(!reg.ids().contains(&id));
        assert!(reg.claim(&id).is_none(), "removed row cannot be claimed");
    }
}
