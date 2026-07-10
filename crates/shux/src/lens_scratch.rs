//! Scratch sessions + `lens.run` composite (lens PRD §8 SPEC-E, task 077 P5).
//!
//! Scratch sessions are created ONLY by `lens.run` (DEC-21): there is no
//! public `session.create` scratch parameter and no other way to allocate
//! one. This module owns:
//! - the scratch registry (in-memory `ScratchRegistry` + a mirrored
//!   `$XDG_RUNTIME_DIR/shux/scratch-registry.json`, LENS-R-044) so a fresh
//!   daemon can kill orphaned scratch process groups left by a prior
//!   incarnation (scratch never survives restart, DEC-7/B6).
//! - per-scratch reap timers (`post_exit_ttl_ms` / `max_runtime_ms`,
//!   LENS-R-042), event-driven off the same `pane.exited` bus event the
//!   daemon already fires (no polling, no sleep-based synchronization —
//!   §16.1 guardrail 3).
//! - the `lens.run` RPC handler (LENS-R-040/041/045/046) and its audit
//!   trail (LENS-R-052, `$XDG_STATE_HOME/shux/lens-audit.ndjson`).
//!
//! `lens.run`'s response is `{session_id, pane_id, revision}` (+
//! `exit_code` when `wait:true`) per §8.1 — it does NOT call
//! `pane.glance`/`pane.wait_settled`/`pane.diff_since` internally. Those are
//! separate RPCs an agent chains itself (see E1, §12): `lens.run` only owns
//! allocate → exec → optional completion-wait → reap-on-a-timer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
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
const DEFAULT_COLS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;
const MIN_COLS: u16 = 20;
const MAX_COLS: u16 = 500;
const MIN_ROWS: u16 = 5;
const MAX_ROWS: u16 = 200;
const DEFAULT_POST_EXIT_TTL_MS: u32 = 30_000;
const MIN_POST_EXIT_TTL_MS: u32 = 0;
const MAX_POST_EXIT_TTL_MS: u32 = 300_000;
const DEFAULT_MAX_RUNTIME_MS: u32 = 3_600_000;
const MIN_MAX_RUNTIME_MS: u32 = 1_000;
const MAX_MAX_RUNTIME_MS: u32 = 86_400_000;

// ── registry (LENS-R-044) ──────────────────────────────────────────────────

/// One live scratch session's bookkeeping. The `Serialize`/`Deserialize`
/// subset below (`RegistryRow`) is what actually hits disk; `explicit_kill`
/// is an in-memory-only control handle for THIS daemon incarnation (a
/// restarted daemon has none — it just kills the pgid). The reaper task
/// itself is fire-and-forget (`tokio::spawn`, not joined anywhere) —
/// nothing needs to await its completion; cancelling `explicit_kill` is
/// sufficient to make it return promptly.
struct ScratchState {
    pane_id: PaneId,
    pgid: u32,
    created_at_unix_ms: u64,
    max_runtime_deadline_unix_ms: u64,
    /// Cancelled by `on_session_killed` (explicit `session.kill`) so the
    /// reaper task returns immediately instead of racing its own reap.
    explicit_kill: CancellationToken,
}

/// On-disk registry row (LENS-R-044 schema, normative field names):
/// `{session_id, pgid, created_at, max_runtime_deadline}`.
#[derive(Serialize, Deserialize)]
struct RegistryRow {
    session_id: String,
    pgid: u32,
    created_at: u64,
    max_runtime_deadline: u64,
}

#[derive(Clone)]
pub struct ScratchRegistry {
    inner: Arc<Mutex<HashMap<SessionId, ScratchState>>>,
    registry_path: PathBuf,
}

impl ScratchRegistry {
    pub fn new(runtime_dir: &Path) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            registry_path: runtime_dir.join("scratch-registry.json"),
        }
    }

    pub async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }

    /// Snapshot of every currently-registered scratch session id. Used by
    /// `session.list` to filter/annotate without holding the registry lock
    /// across the (synchronous) JSON-building loop.
    pub async fn ids(&self) -> std::collections::HashSet<SessionId> {
        self.inner.lock().await.keys().copied().collect()
    }

    async fn insert(&self, id: SessionId, state: ScratchState) {
        let mut map = self.inner.lock().await;
        map.insert(id, state);
        self.persist(&map);
    }

    /// Remove and return the entry (if any), cancelling its reaper's
    /// explicit-kill token so an in-flight reap loop exits cleanly instead
    /// of racing whatever the caller is about to do (explicit
    /// `session.kill`, or the reaper's own timer branch already firing).
    async fn remove(&self, id: &SessionId) -> Option<ScratchState> {
        let mut map = self.inner.lock().await;
        let removed = map.remove(id);
        self.persist(&map);
        removed
    }

    /// Rewrite the registry file from the current in-memory map (LENS-R-044:
    /// "persist ... on every change"). Synchronous — the same small-file
    /// tradeoff `shux-plugin`'s audit log makes (§ its own doc comment);
    /// the registry never exceeds `SCRATCH_QUOTA` (16) rows.
    fn persist(&self, map: &HashMap<SessionId, ScratchState>) {
        let rows: Vec<RegistryRow> = map
            .iter()
            .map(|(id, s)| RegistryRow {
                session_id: id.to_string(),
                pgid: s.pgid,
                created_at: s.created_at_unix_ms,
                max_runtime_deadline: s.max_runtime_deadline_unix_ms,
            })
            .collect();
        if rows.is_empty() {
            let _ = std::fs::remove_file(&self.registry_path);
            return;
        }
        if let Some(parent) = self.registry_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_vec_pretty(&rows) {
            if let Err(e) = std::fs::write(&self.registry_path, json) {
                tracing::warn!(error = %e, path = %self.registry_path.display(), "scratch-registry: persist failed");
            }
        }
    }

    /// Startup reap (LENS-R-044/DEC-7): read a leftover registry file from a
    /// prior daemon incarnation, `killpg` every registered pgid that is
    /// still alive, delete the file, and write one audit entry per killed
    /// row. Scratch never survives a restart — this runs BEFORE the RPC
    /// server starts accepting `lens.run` calls that would populate a fresh
    /// registry.
    ///
    /// Known limitation (documented, matches the repo's existing M14
    /// tolerance for double-forked escapees): this probes pgid liveness via
    /// a signal-0 kill, not a full process-start-time comparison against
    /// `created_at` — a PID that wrapped around to an unrelated process in
    /// the same narrow window would be killed too. `killpg` only ever
    /// targets pgids this daemon itself created as scratch process-group
    /// leaders, so the blast radius of that edge case is bounded to
    /// "processes sharing a recycled pgid", the same class of risk the PRD
    /// already accepts for scratch process-group teardown generally.
    pub async fn startup_reap(runtime_dir: &Path, state_dir_hint: Option<&Path>) -> usize {
        let path = runtime_dir.join("scratch-registry.json");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return 0;
        };
        let rows: Vec<RegistryRow> = match serde_json::from_str(&text) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "scratch-registry: unreadable, discarding");
                let _ = std::fs::remove_file(&path);
                return 0;
            }
        };
        let mut killed = 0usize;
        for row in &rows {
            if kill_pgid_if_alive(row.pgid) {
                killed += 1;
            }
            append_lens_audit(
                state_dir_hint,
                json!({
                    "ts": iso_now(),
                    "caller": "uds",
                    "method": "scratch.reap",
                    "reason": "registry",
                    "session_id": row.session_id,
                    "pgid": row.pgid,
                }),
            );
        }
        let _ = std::fs::remove_file(&path);
        killed
    }
}

/// Probe-then-killpg: SIGTERM, then SIGKILL if still alive after a short
/// grace window (LENS-R-042's "killpg(SIGTERM), 500ms grace, killpg(SIGKILL)"
/// reap contract, applied here to an orphan from a PRIOR daemon — no
/// per-pane PTY task exists to do the graceful escalation itself).
fn kill_pgid_if_alive(pgid: u32) -> bool {
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;
    let pid = Pid::from_raw(pgid as i32);
    if killpg(pid, None).is_err() {
        return false; // not alive (or not ours) — nothing to do
    }
    let _ = killpg(pid, Signal::SIGTERM);
    std::thread::sleep(Duration::from_millis(500));
    if killpg(pid, None).is_ok() {
        let _ = killpg(pid, Signal::SIGKILL);
    }
    true
}

// ── audit (LENS-R-052) ──────────────────────────────────────────────────

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

fn iso_now() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

/// Append one sha256-chained NDJSON line to the daemon-level lens audit log
/// (LENS-R-052, `$XDG_STATE_HOME/shux/lens-audit.ndjson`). `entry` should NOT
/// set `prev_hash`/`hash` — this function computes and adds them, chaining
/// off the previous line's `hash` (genesis = 64 zeros) so the log is
/// tamper-evident. Best-effort: a write failure is logged, never surfaced —
/// losing an audit line must not break the scratch lifecycle it's
/// documenting (same posture as the existing plugin audit log).
fn append_lens_audit(state_dir_override: Option<&Path>, mut entry: serde_json::Value) {
    let dir = state_dir_override
        .map(Path::to_path_buf)
        .unwrap_or_else(xdg_state_home);
    let path = dir.join("lens-audit.ndjson");

    let prev_hash = last_hash(&path).unwrap_or_else(|| "0".repeat(64));
    if let Some(obj) = entry.as_object_mut() {
        obj.insert("prev_hash".into(), json!(prev_hash));
    }
    let canonical = serde_json::to_vec(&entry).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(prev_hash.as_bytes());
    hasher.update(&canonical);
    let hash = hex_encode(&hasher.finalize());
    if let Some(obj) = entry.as_object_mut() {
        obj.insert("hash".into(), json!(hash));
    }

    if let Some(parent) = path.parent() {
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
        .open(&path)
    {
        Ok(mut f) => {
            if let Err(e) = f.write_all(&line) {
                tracing::warn!(error = %e, path = %path.display(), "lens-audit: write failed");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "lens-audit: open failed");
        }
    }
}

fn last_hash(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let last_line = text.lines().next_back()?;
    let v: serde_json::Value = serde_json::from_str(last_line).ok()?;
    v.get("hash").and_then(|h| h.as_str()).map(str::to_string)
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unix_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ── param parsing (LENS-R-040/046) ──────────────────────────────────────

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

    let cols = params
        .get("cols")
        .and_then(|v| v.as_u64())
        .map(|v| v as u16)
        .unwrap_or(DEFAULT_COLS);
    let rows = params
        .get("rows")
        .and_then(|v| v.as_u64())
        .map(|v| v as u16)
        .unwrap_or(DEFAULT_ROWS);
    if !(MIN_COLS..=MAX_COLS).contains(&cols) {
        return Err(shux_rpc::RpcError::invalid_params(&format!(
            "'cols' {cols} out of range [{MIN_COLS}, {MAX_COLS}]"
        )));
    }
    if !(MIN_ROWS..=MAX_ROWS).contains(&rows) {
        return Err(shux_rpc::RpcError::invalid_params(&format!(
            "'rows' {rows} out of range [{MIN_ROWS}, {MAX_ROWS}]"
        )));
    }

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

    let post_exit_ttl_ms = params
        .get("post_exit_ttl_ms")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .unwrap_or(DEFAULT_POST_EXIT_TTL_MS);
    if !(MIN_POST_EXIT_TTL_MS..=MAX_POST_EXIT_TTL_MS).contains(&post_exit_ttl_ms) {
        return Err(shux_rpc::RpcError::invalid_params(&format!(
            "'post_exit_ttl_ms' {post_exit_ttl_ms} out of range [{MIN_POST_EXIT_TTL_MS}, {MAX_POST_EXIT_TTL_MS}]"
        )));
    }

    let max_runtime_ms = params
        .get("max_runtime_ms")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .unwrap_or(DEFAULT_MAX_RUNTIME_MS);
    if !(MIN_MAX_RUNTIME_MS..=MAX_MAX_RUNTIME_MS).contains(&max_runtime_ms) {
        return Err(shux_rpc::RpcError::invalid_params(&format!(
            "'max_runtime_ms' {max_runtime_ms} out of range [{MIN_MAX_RUNTIME_MS}, {MAX_MAX_RUNTIME_MS}]"
        )));
    }

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

/// Reap a scratch session: tear down its (only) pane through the SAME
/// teardown path `session.kill` uses (cancel the pane's shutdown token,
/// which drives `run_pane_pty_task`'s existing SIGHUP→SIGKILL escalation —
/// LENS-R-042's reap contract reuses this rather than re-implementing
/// process-group signalling), then destroy the graph session and drop the
/// registry entry. `reason` is one of exit|max_runtime|explicit — written
/// to the audit log (R1/R4/R7 assert on it).
async fn reap_scratch(
    session_id: SessionId,
    pane_id: PaneId,
    graph: &GraphHandle,
    io_state: &Arc<Mutex<PaneIoState>>,
    reason: &str,
) {
    let _ = graph.destroy_session(session_id, None).await;
    {
        let mut state = io_state.lock().await;
        let pulse = state.teardown_panes(&[pane_id], true);
        drop(state);
        pulse.notify_one();
    }
    append_lens_audit(
        None,
        json!({
            "ts": iso_now(),
            "caller": "uds",
            "method": "scratch.reap",
            "reason": reason,
            "session_id": session_id.to_string(),
            "pane_id": pane_id.to_string(),
        }),
    );
}

/// Per-scratch reap timer (LENS-R-042 a/b): races `max_runtime_ms` against
/// the child's exit (then `post_exit_ttl_ms` more) and an explicit-kill
/// cancellation from `on_session_killed`. All three arms are
/// deadline-bounded event waits, never a synchronization sleep on rendered
/// output (§16.1 guardrail 3) — `post_exit_ttl_ms`/`max_runtime_ms` ARE the
/// timed product behavior LENS-R-042 specifies, not a test-sync hack.
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
) {
    let reason = tokio::select! {
        _ = explicit_kill.cancelled() => {
            // on_session_killed already reaped + audited; nothing to do.
            return;
        }
        _ = tokio::time::sleep_until(max_runtime_deadline.into()) => {
            "max_runtime"
        }
        exit_status = wait_for_pane_exit(exit_sub, pane_id) => {
            let _ = exit_status;
            tokio::select! {
                _ = explicit_kill.cancelled() => return,
                _ = tokio::time::sleep_until(max_runtime_deadline.into()) => "max_runtime",
                _ = tokio::time::sleep(Duration::from_millis(post_exit_ttl_ms as u64)) => "exit",
            }
        }
    };
    reap_scratch(session_id, pane_id, &graph, &io_state, reason).await;
    registry.remove(&session_id).await;
}

/// Hook for `session.kill` (LENS-R-042c: explicit kill reaps immediately).
/// The session.kill handler has ALREADY torn the pane/graph down by the
/// time this runs — this only cancels the scratch's reaper (so it doesn't
/// also try to reap an already-gone pane), drops the registry entry, and
/// writes the audit(reason=explicit) entry R4 asserts on indirectly via the
/// general reap contract.
pub async fn on_session_killed(registry: &ScratchRegistry, session_id: SessionId) {
    let Some(state) = registry.remove(&session_id).await else {
        return; // not a scratch session — nothing to do
    };
    state.explicit_kill.cancel();
    append_lens_audit(
        None,
        json!({
            "ts": iso_now(),
            "caller": "uds",
            "method": "scratch.reap",
            "reason": "explicit",
            "session_id": session_id.to_string(),
            "pane_id": state.pane_id.to_string(),
        }),
    );
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

    // LENS-R-043: quota BEFORE any allocation.
    let current = registry.len().await;
    if current >= SCRATCH_QUOTA {
        return Err(shux_rpc::RpcError::resource_exhausted(
            "scratch_session",
            current,
            SCRATCH_QUOTA,
        ));
    }

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
        // — no session, no pane, no PTY, absent from `--include-scratch`.
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

    let now = SystemTime::now();
    let created_at_unix_ms = unix_ms(now);
    let max_runtime_deadline = Instant::now() + Duration::from_millis(p.max_runtime_ms as u64);
    let max_runtime_deadline_unix_ms = created_at_unix_ms + p.max_runtime_ms as u64;
    let explicit_kill = CancellationToken::new();

    // Register BEFORE spawning the reaper task (not after): the reaper can
    // call `registry.remove()` as soon as one of its own branches fires
    // (bounded below by MIN_MAX_RUNTIME_MS=1000ms in practice, but this
    // ordering makes the invariant "insert always happens-before any
    // matching remove" true by construction rather than by timing).
    registry
        .insert(
            session_id,
            ScratchState {
                pane_id,
                pgid,
                created_at_unix_ms,
                max_runtime_deadline_unix_ms,
                explicit_kill: explicit_kill.clone(),
            },
        )
        .await;

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
    ));

    append_lens_audit(
        None,
        json!({
            "ts": iso_now(),
            "caller": "uds",
            "method": "scratch.create",
            "session_id": session_id.to_string(),
            "pane_id": pane_id.to_string(),
            "argv": p.argv,
            "cols": p.cols,
            "rows": p.rows,
        }),
    );

    let revision = {
        let state = io_state.lock().await;
        state
            .vts
            .get(&pane_id)
            .map(|vt| vt.content_revision())
            .unwrap_or(1)
    };

    let mut result = json!({
        "session_id": session_id.to_string(),
        "pane_id": pane_id.to_string(),
        "revision": revision,
    });

    if let Some(sub) = exit_sub_for_wait {
        let exit_code = wait_for_pane_exit(sub, pane_id).await.unwrap_or(-1);
        result["exit_code"] = json!(exit_code);
    }

    Ok(result)
}
