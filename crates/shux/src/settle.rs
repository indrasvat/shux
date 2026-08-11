//! `pane.wait_settled` settle math (§6 SPEC-C).
//!
//! The decision of whether one wake settles, times out, or keeps waiting is a
//! pure function of three booleans, kept that way so the precedence rules —
//! which are the whole correctness story — are unit-testable without a pane.

use std::sync::Arc;

use tokio::sync::{Mutex, watch};

use crate::pane_io::{PaneIoState, PaneRevision};

// ── `pane.wait_settled` settle math (§6 SPEC-C; pure, unit-tested) ────────
//
// LENS-R-025 parameter bounds. Kept as named constants so the RPC handler and
// the unit tests share one source of truth.
pub(crate) const SETTLE_QUIET_MIN_MS: u64 = 10;
pub(crate) const SETTLE_QUIET_MAX_MS: u64 = 60_000;
pub(crate) const SETTLE_TIMEOUT_MAX_MS: u64 = 600_000;
/// `stable_frames` upper bound (task 083). 1 = disabled (quiet mode); a small ceiling keeps a
/// typo from demanding an unreachable contiguous run.
pub(crate) const SETTLE_STABLE_FRAMES_MAX: u32 = 1_000;

/// LENS-R-020: a pane is settled once it has been quiet for `quiet_ms`, i.e.
/// `monotonic_now_ns − last_mutation_ns ≥ quiet_ms × 1_000_000`. The unit
/// conversion is EXPLICIT and both sides are nanoseconds — the ns↔ms mixup is
/// the councils-caught deadline-math bug class. `saturating_*` keeps a clock
/// that briefly reads below `last_mutation_ns` (never happens on a monotonic
/// clock, but cheap insurance) from underflowing into "settled".
pub(crate) fn settle_is_quiet(now_ns: u64, last_mutation_ns: u64, quiet_ms: u64) -> bool {
    now_ns.saturating_sub(last_mutation_ns) >= quiet_ms.saturating_mul(1_000_000)
}

/// Nanoseconds of quiet still owed before settle (0 once already settled). Used
/// to size the event-driven sleep so it is never shorter than the remaining
/// deadline (LENS-R-021: no polling).
pub(crate) fn settle_remaining_quiet_ns(now_ns: u64, last_mutation_ns: u64, quiet_ms: u64) -> u64 {
    quiet_ms
        .saturating_mul(1_000_000)
        .saturating_sub(now_ns.saturating_sub(last_mutation_ns))
}

/// One wake of the `pane.wait_settled` loop, decided as a pure function so the
/// precedence rules are unit-testable (codex P3 B1 + claude TOCTOU guard).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettleWake {
    /// Quiet window satisfied on a snapshot with no pending revision.
    Settled,
    /// Quiet still unsatisfied and the timeout deadline has elapsed.
    TimedOut,
    /// Keep waiting (or restart evaluation on a fresh snapshot).
    KeepWaiting,
}

/// Decide one wake of the settle loop. Priority order is the whole fix:
///
/// 1. `pending_revision` (claude P3 TOCTOU guard): a revision published AFTER
///    the `borrow_and_update` snapshot was taken must RESTART evaluation on
///    the fresh value — never settle on the stale snapshot, never report a
///    stale revision in a timeout.
/// 2. `quiet` (codex P3 B1: quiet precedence): on ANY wake — sleep expiry,
///    watch wake, or late scheduler wake — a satisfied quiet window returns
///    `settled:true` even if the timeout deadline has ALSO elapsed. With
///    `timeout_ms == quiet_ms` allowed (LENS-R-025 lower bound), a pane quiet
///    exactly at the shared deadline must settle, not time out.
/// 3. `past_timeout`: only when quiet is still false may the deadline expire
///    the wait (`settled:false` — a RESULT, not an error; DEC-19).
pub(crate) fn settle_decide(quiet: bool, past_timeout: bool, pending_revision: bool) -> SettleWake {
    if pending_revision {
        return SettleWake::KeepWaiting;
    }
    if quiet {
        return SettleWake::Settled;
    }
    if past_timeout {
        return SettleWake::TimedOut;
    }
    SettleWake::KeepWaiting
}

/// Strict optional-u64 parameter parse (codex P3 M2): absent → default;
/// PRESENT but not an unsigned integer (string `"5ms"`, float `5.5`, `null`,
/// negative) → INVALID_PARAMS (-32602). The previous `and_then(as_u64)
/// .unwrap_or(default)` silently replaced mistyped values with the default —
/// a caller sending `quiet_ms: "5ms"` got a 300 ms wait instead of an error.
pub(crate) fn settle_u64_param(
    params: &serde_json::Value,
    key: &str,
    default: u64,
) -> Result<u64, shux_rpc::RpcError> {
    match params.get(key) {
        None => Ok(default),
        Some(v) => v.as_u64().ok_or_else(|| {
            shux_rpc::RpcError::invalid_params(&format!(
                "{key} must be an unsigned integer of milliseconds, got {v}"
            ))
        }),
    }
}

/// The settle waiter's watch sender dropped mid-wait (codex P3 M1): pane
/// teardown removes the VT and its revision publisher together, so the normal
/// outcome is pane-gone → NOT_FOUND (-32004) — never a `settled` verdict on a
/// frozen value from a dead pane. The re-subscribe arm is defensive: if a
/// publisher somehow exists again for this pane id, the waiter continues on
/// the live channel instead of erroring spuriously.
pub(crate) async fn settle_reacquire_watch(
    io: &Arc<Mutex<PaneIoState>>,
    pane_id: shux_core::model::PaneId,
) -> Result<watch::Receiver<PaneRevision>, shux_rpc::RpcError> {
    let state = io.lock().await;
    state
        .revisions
        .get(&pane_id)
        .map(|tx| tx.subscribe())
        .ok_or_else(|| shux_rpc::RpcError::not_found("pane VT", &pane_id.to_string()))
}

/// LENS-R-025 parameter validation: `quiet_ms ∈ [10, 60_000]`,
/// `timeout_ms ∈ [quiet_ms, 600_000]`. Violations → INVALID_PARAMS (-32602),
/// which the CLI maps to exit 2 (§10 exit table, V1).
pub(crate) fn validate_wait_settled_params(
    quiet_ms: u64,
    timeout_ms: u64,
) -> Result<(), shux_rpc::RpcError> {
    if !(SETTLE_QUIET_MIN_MS..=SETTLE_QUIET_MAX_MS).contains(&quiet_ms) {
        return Err(shux_rpc::RpcError::invalid_params(&format!(
            "quiet_ms {quiet_ms} out of range [{SETTLE_QUIET_MIN_MS}, {SETTLE_QUIET_MAX_MS}]"
        )));
    }
    if !(quiet_ms..=SETTLE_TIMEOUT_MAX_MS).contains(&timeout_ms) {
        return Err(shux_rpc::RpcError::invalid_params(&format!(
            "timeout_ms {timeout_ms} out of range [quiet_ms={quiet_ms}, {SETTLE_TIMEOUT_MAX_MS}]"
        )));
    }
    Ok(())
}

/// Strict optional-u32 parameter parse (task 083; mirrors [`settle_u64_param`]). Absent →
/// default; PRESENT but not an unsigned integer that fits u32 → INVALID_PARAMS (-32602). Never
/// a silent default on a mistyped value.
pub(crate) fn settle_u32_param(
    params: &serde_json::Value,
    key: &str,
    default: u32,
) -> Result<u32, shux_rpc::RpcError> {
    match params.get(key) {
        None => Ok(default),
        Some(v) => v
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| {
                shux_rpc::RpcError::invalid_params(&format!(
                    "{key} must be an unsigned 32-bit integer, got {v}"
                ))
            }),
    }
}

/// Frame-stability param validation (task 083, council #1/#2/#3). `hold_ms ∈ {0} ∪ [10, 60_000]`
/// and ≤ `timeout_ms` (a hold longer than the budget can never succeed); `stable_frames ∈ [1,
/// 1000]` (1 = disabled). Violations → INVALID_PARAMS (-32602 → CLI exit 2). Default quiet mode
/// (both off) is unaffected — this is only reached when a caller opts into a stability criterion.
pub(crate) fn validate_stability_params(
    hold_ms: u64,
    stable_frames: u32,
    timeout_ms: u64,
) -> Result<(), shux_rpc::RpcError> {
    if hold_ms != 0 && !(SETTLE_QUIET_MIN_MS..=SETTLE_QUIET_MAX_MS).contains(&hold_ms) {
        return Err(shux_rpc::RpcError::invalid_params(&format!(
            "hold_ms {hold_ms} out of range [{SETTLE_QUIET_MIN_MS}, {SETTLE_QUIET_MAX_MS}] (0 = off)"
        )));
    }
    if hold_ms > timeout_ms {
        return Err(shux_rpc::RpcError::invalid_params(&format!(
            "hold_ms {hold_ms} must be <= timeout_ms {timeout_ms}"
        )));
    }
    if !(1..=SETTLE_STABLE_FRAMES_MAX).contains(&stable_frames) {
        return Err(shux_rpc::RpcError::invalid_params(&format!(
            "stable_frames {stable_frames} out of range [1, {SETTLE_STABLE_FRAMES_MAX}] (1 = off)"
        )));
    }
    Ok(())
}

/// Task 083 frame-stability settle (`hold_ms` / `stable_frames`). Event-driven like the quiet
/// loop: on each revision wake it hashes the presented, mask-applied frame and folds it into
/// [`shux_vt::FrameStability`]; it ALSO wakes on the hold deadline so a pane that goes silent
/// still settles `hold_ms` after its last content change (silence is stability, council #2).
/// `quiet_ms` never independently settles here (council #1). A pane that never reaches the
/// requested stability by `timeout_deadline` returns `settled:false` — which the runner maps to
/// `settle_never_stable` (a FAILURE, never infra; the frozen 078/082 contract).
///
/// The presented-frame hash is read together with `content_revision` under ONE io lock (a
/// consistent snapshot); the lock is never held across an `.await`. A revision that skips (the
/// watch coalesced) is detected by [`shux_vt::FrameStability::observe`] and RESETS the contiguous
/// run — an `A→B→A` alias can never false-settle `stable_frames` (council #3).
/// The next-wake instant for the frame-stability loop (task 083, impl-review Surface 1). Waking at
/// the hold deadline is useful ONLY while the hold window is UNSATISFIED (`remaining_hold_ns > 0`)
/// — it wakes exactly when hold becomes met (or a revision arrives). Once hold is satisfied, or
/// there is no hold criterion (`remaining_hold_ns == 0`), only a new revision or the timeout can
/// change the decision, so wake straight to the timeout: a `now + 0`-sized sleep here would
/// busy-spin when hold is met but the stable-frame count is still pending. Pure, so the anti-spin
/// rule is unit-tested.
pub(crate) fn stability_wake(
    now_inst: tokio::time::Instant,
    remaining_hold_ns: u64,
    timeout_deadline: tokio::time::Instant,
) -> tokio::time::Instant {
    if remaining_hold_ns > 0 {
        (now_inst + std::time::Duration::from_nanos(remaining_hold_ns)).min(timeout_deadline)
    } else {
        timeout_deadline
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn wait_settled_frame_stability(
    io: &Arc<Mutex<PaneIoState>>,
    pane_id: shux_core::model::PaneId,
    rx: &mut watch::Receiver<PaneRevision>,
    masks: &shux_vt::MaskSet,
    hold_ms: u64,
    stable_frames: u32,
    accept: tokio::time::Instant,
    timeout_deadline: tokio::time::Instant,
) -> Result<serde_json::Value, shux_rpc::RpcError> {
    use shux_vt::{FrameEnvelope, FrameStability, frame_stability_hash};

    // Read the current presented frame's (revision, hash) together under one lock so the seed is
    // a consistent snapshot. NOT_FOUND if the pane was torn down (never settle on a dead pane).
    let read_frame = |io: &Arc<Mutex<PaneIoState>>| {
        let io = io.clone();
        let masks = masks.clone();
        async move {
            let state = io.lock().await;
            state
                .vts
                .get(&pane_id)
                .map(|vt| {
                    (
                        vt.content_revision(),
                        frame_stability_hash(&FrameEnvelope::from_terminal(vt, &masks)),
                    )
                })
                .ok_or_else(|| shux_rpc::RpcError::not_found("pane VT", &pane_id.to_string()))
        }
    };

    // Seed the frame AND the watch cursor under ONE io lock (impl-review Surface 1 seed race):
    // the PTY task bumps `content_revision` and publishes the watch under this SAME lock, so while
    // we hold it the VT and the watch are frozen at the same batch. Marking the cursor here — not
    // after releasing the lock — closes the window where the frame advances (the watch is
    // last-value-wins, so its cursor would jump PAST the seeded frame) and the loop then settles
    // in hold mode on a STALE seed that no longer matches the pane.
    let (seed_rev, seed_hash) = {
        let state = io.lock().await;
        let vt = state
            .vts
            .get(&pane_id)
            .ok_or_else(|| shux_rpc::RpcError::not_found("pane VT", &pane_id.to_string()))?;
        let seed = (
            vt.content_revision(),
            frame_stability_hash(&FrameEnvelope::from_terminal(vt, masks)),
        );
        rx.borrow_and_update();
        seed
    };
    let mut stability = FrameStability::seed(seed_rev, seed_hash, shux_vt::monotonic_now_ns());

    let waited_ms = |now: tokio::time::Instant| -> u64 {
        now.saturating_duration_since(accept)
            .as_millis()
            .min(u128::from(u32::MAX)) as u64
    };

    loop {
        let now_ns = shux_vt::monotonic_now_ns();
        let now_inst = tokio::time::Instant::now();
        if stability.is_settled(stable_frames, hold_ms, now_ns) {
            return Ok(serde_json::json!({
                "settled": true,
                "revision": stability.last_rev(),
                "waited_ms": waited_ms(now_inst),
                "coalesced": stability.coalesced(),
            }));
        }
        if now_inst >= timeout_deadline {
            return Ok(serde_json::json!({
                "settled": false,
                "revision": stability.last_rev(),
                "waited_ms": waited_ms(now_inst),
                "coalesced": stability.coalesced(),
            }));
        }

        // Next wake: the hold deadline (so a silent pane still settles) while the hold window is
        // UNSATISFIED, else the timeout — a `now`-sized sleep when hold is already met but the
        // count criterion is not would busy-spin (impl-review Surface 1). Woken early by a
        // revision regardless.
        let remaining_hold_ns = if hold_ms > 0 {
            stability.ns_until_hold(hold_ms, now_ns)
        } else {
            0
        };
        let wake = stability_wake(now_inst, remaining_hold_ns, timeout_deadline);

        tokio::select! {
            changed = rx.changed() => {
                if changed.is_err() {
                    // Pane teardown mid-wait → NOT_FOUND (never settle on a dead pane).
                    *rx = settle_reacquire_watch(io, pane_id).await?;
                    continue;
                }
                rx.borrow_and_update();
                let (rev, hash) = read_frame(io).await?;
                stability.observe(rev, hash, shux_vt::monotonic_now_ns());
            }
            _ = tokio::time::sleep_until(wake) => {
                // Hold-deadline / silence wake: re-evaluate `is_settled` at the top.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn settle_u64_param_strict_typing() {
        let params = serde_json::json!({
            "ok": 250,
            "str": "5ms",
            "float": 5.5,
            "null": null,
            "neg": -5,
        });
        // Absent → default.
        assert_eq!(settle_u64_param(&params, "missing", 300).unwrap(), 300);
        // Present u64 → the value.
        assert_eq!(settle_u64_param(&params, "ok", 300).unwrap(), 250);
        // Present-but-wrong-type → INVALID_PARAMS, never the default.
        for key in ["str", "float", "null", "neg"] {
            let err = settle_u64_param(&params, key, 300).unwrap_err();
            assert_eq!(
                err.code,
                shux_rpc::ErrorCode::InvalidParams.code(),
                "{key} must be rejected"
            );
        }
    }

    // ── `pane.wait_settled` settle-math unit tests (§6, L0 supporting) ────
    //
    // These pin the pure decision layer the RPC handler leans on. The
    // black-box behavior (S1–S5, V1) is proven by the frozen red suite; these
    // guard the ns↔ms conversion, the bounds table, the already-quiet fast
    // path, and the waiter-drop primitive against silent regression.

    #[test]
    fn settle_math_ns_conversion_is_explicit_ms_times_million() {
        // 300 ms of quiet == 300_000_000 ns. Exactly at the boundary settles;
        // one ns short does not. This is the councils-caught bug class: a
        // handler comparing raw ms against ns (or forgetting ×1_000_000)
        // would settle ~a million times too eagerly.
        let last = 1_000_000_000u64;
        let quiet_ms = 300u64;
        assert!(!settle_is_quiet(last + 299_999_999, last, quiet_ms));
        assert!(settle_is_quiet(last + 300_000_000, last, quiet_ms));
        assert!(settle_is_quiet(last + 300_000_001, last, quiet_ms));
    }

    #[test]
    fn settle_math_already_quiet_returns_true_immediately() {
        // A pane whose last mutation is far in the past is settled at call
        // time (LENS-R-020 immediate return; S4's second call). `now` well
        // beyond last + quiet.
        let last = 5_000_000_000u64;
        assert!(settle_is_quiet(last + 2_000_000_000, last, 300));
    }

    #[test]
    fn settle_math_remaining_quiet_shrinks_then_zeroes() {
        let last = 1_000_000_000u64;
        let quiet_ms = 300u64; // 300_000_000 ns
        // Just after a mutation: nearly the whole window remains.
        assert_eq!(settle_remaining_quiet_ns(last, last, quiet_ms), 300_000_000);
        // Half elapsed → half remains.
        assert_eq!(
            settle_remaining_quiet_ns(last + 150_000_000, last, quiet_ms),
            150_000_000
        );
        // Fully elapsed → zero (never negative/underflow).
        assert_eq!(
            settle_remaining_quiet_ns(last + 300_000_000, last, quiet_ms),
            0
        );
        assert_eq!(
            settle_remaining_quiet_ns(last + 900_000_000, last, quiet_ms),
            0
        );
    }

    #[test]
    fn settle_math_saturates_on_backwards_clock() {
        // A `now` below `last_mutation_ns` (impossible on a monotonic clock,
        // but the guard must not underflow into a bogus "quiet forever").
        assert!(!settle_is_quiet(500, 1_000, 300));
        assert_eq!(settle_remaining_quiet_ns(500, 1_000, 300), 300_000_000);
    }

    #[test]
    fn settle_param_bounds_accept_valid_and_defaults() {
        // Defaults (300 / 10_000) are valid.
        assert!(validate_wait_settled_params(300, 10_000).is_ok());
        // Exact boundaries are inclusive.
        assert!(validate_wait_settled_params(SETTLE_QUIET_MIN_MS, SETTLE_QUIET_MIN_MS).is_ok());
        assert!(validate_wait_settled_params(SETTLE_QUIET_MAX_MS, SETTLE_TIMEOUT_MAX_MS).is_ok());
        // timeout == quiet is allowed (range is [quiet, 600_000]).
        assert!(validate_wait_settled_params(500, 500).is_ok());
    }

    #[test]
    fn settle_param_bounds_reject_out_of_range() {
        // quiet below min (V1 case: 5 ms) → INVALID_PARAMS.
        let e = validate_wait_settled_params(5, 10_000).unwrap_err();
        assert_eq!(e.code, shux_rpc::ErrorCode::InvalidParams.code());
        // quiet above max.
        assert!(validate_wait_settled_params(60_001, 60_001).is_err());
        // timeout below quiet (V1 case: quiet 300, timeout 100) → INVALID_PARAMS.
        let e = validate_wait_settled_params(300, 100).unwrap_err();
        assert_eq!(e.code, shux_rpc::ErrorCode::InvalidParams.code());
        // timeout above max.
        assert!(validate_wait_settled_params(300, 600_001).is_err());
    }

    #[test]
    fn settle_equal_deadlines_prefers_settled() {
        // codex P3 B1: with timeout_ms == quiet_ms (allowed — LENS-R-025's
        // timeout lower bound IS quiet_ms), a pane quiet exactly at the shared
        // deadline must return settled:true, not a timeout. Model the wake at
        // the exact shared deadline: quiet satisfied to the nanosecond AND the
        // timeout elapsed — quiet wins.
        let last = 1_000_000_000u64;
        let quiet_ms = 300u64;
        let now_ns = last + 300_000_000; // exactly quiet
        let quiet = settle_is_quiet(now_ns, last, quiet_ms);
        assert!(quiet);
        assert_eq!(
            settle_decide(quiet, /*past_timeout*/ true, /*pending*/ false),
            SettleWake::Settled,
            "quiet at the shared deadline must settle, not time out"
        );
    }

    #[test]
    fn settle_late_wake_past_timeout_with_quiet_satisfied_settles() {
        // codex P3 B1 second face: a scheduler that wakes the loop LATE (well
        // past the timeout deadline) must still report settled when the quiet
        // window was satisfied — the old code returned timeout on any
        // post-deadline wake without re-evaluating quiet first.
        let last = 1_000_000_000u64;
        let quiet_ms = 300u64;
        let now_ns = last + 5_000_000_000; // woke 4.7s late; quiet long since satisfied
        let quiet = settle_is_quiet(now_ns, last, quiet_ms);
        assert!(quiet);
        assert_eq!(
            settle_decide(quiet, /*past_timeout*/ true, /*pending*/ false),
            SettleWake::Settled,
            "late wake after the deadline with quiet satisfied must settle"
        );
    }

    #[test]
    fn settle_revision_in_return_window_does_not_settle() {
        // claude P3 TOCTOU guard: a revision published AFTER the
        // borrow_and_update snapshot but BEFORE the settled return must
        // restart the evaluation. Drive the REAL mechanism: a watch channel
        // whose pending state is read exactly the way the handler reads it.
        let (tx, mut rx) = watch::channel(PaneRevision {
            content_revision: 7,
            last_mutation_ns: 1_000,
        });
        let snapshot = *rx.borrow_and_update();
        // Quiet is satisfied ON THE SNAPSHOT (stale view says "still")...
        let now_ns = snapshot.last_mutation_ns + 400_000_000;
        let quiet = settle_is_quiet(now_ns, snapshot.last_mutation_ns, 300);
        assert!(quiet);
        // ...but a new revision lands in the return window.
        tx.send(PaneRevision {
            content_revision: 8,
            last_mutation_ns: now_ns,
        })
        .expect("send");
        let pending = rx.has_changed().expect("channel open");
        assert!(pending, "the in-window revision must be visible as pending");
        assert_eq!(
            settle_decide(quiet, false, pending),
            SettleWake::KeepWaiting,
            "a pending revision must restart evaluation, never settle stale"
        );
        // The restart sees the fresh value: no longer quiet at `now_ns`.
        let fresh = *rx.borrow_and_update();
        assert_eq!(fresh.content_revision, 8);
        assert!(!settle_is_quiet(now_ns, fresh.last_mutation_ns, 300));
        // And if the timeout has ALSO elapsed by then, the restart reports an
        // honest timeout on the fresh revision (not a stale settled).
        assert_eq!(settle_decide(false, true, false), SettleWake::TimedOut);
    }

    #[test]
    fn settle_decide_priority_table() {
        use SettleWake::*;
        // pending > quiet > timeout > wait — the full truth table.
        assert_eq!(settle_decide(true, true, true), KeepWaiting);
        assert_eq!(settle_decide(true, false, true), KeepWaiting);
        assert_eq!(settle_decide(false, true, true), KeepWaiting);
        assert_eq!(settle_decide(false, false, true), KeepWaiting);
        assert_eq!(settle_decide(true, true, false), Settled);
        assert_eq!(settle_decide(true, false, false), Settled);
        assert_eq!(settle_decide(false, true, false), TimedOut);
        assert_eq!(settle_decide(false, false, false), KeepWaiting);
    }

    #[test]
    fn stability_wake_avoids_busy_spin_when_hold_is_satisfied() {
        // impl-review Surface 1: with the hold window already met (remaining 0) but the count
        // criterion still pending, the wake must be the TIMEOUT — a `now`-sized sleep would spin.
        let now = tokio::time::Instant::now();
        let timeout = now + Duration::from_secs(10);
        assert_eq!(
            stability_wake(now, 0, timeout),
            timeout,
            "hold satisfied / no-hold: wake straight to the timeout, never `now`"
        );
        // Hold still owed: wake at the hold deadline (before the timeout).
        let w = stability_wake(now, 500_000_000, timeout); // 500ms owed
        assert!(
            w > now && w < timeout,
            "hold unsatisfied wakes at the hold deadline"
        );
        // A hold deadline past the timeout is capped at the timeout.
        assert_eq!(
            stability_wake(now, 20_000_000_000, timeout),
            timeout,
            "the hold wake never exceeds the timeout"
        );
    }

    #[test]
    fn settle_waiter_subscribe_and_drop_is_bounded() {
        // LENS-R-023: a waiter is just a `watch::Receiver` subscription; when
        // the waiter future is dropped (client disconnect), the receiver
        // drops with it and the daemon does NOT grow. Prove the primitive:
        // subscribing adds a receiver, dropping removes it, and the sender
        // survives with zero receivers (a torn-down waiter never wedges the
        // pane's publisher).
        let (tx, rx0) = watch::channel(PaneRevision {
            content_revision: 1,
            last_mutation_ns: 1,
        });
        assert_eq!(tx.receiver_count(), 1);
        let waiter_a = tx.subscribe();
        let waiter_b = tx.subscribe();
        assert_eq!(tx.receiver_count(), 3);
        drop(waiter_a);
        drop(waiter_b);
        assert_eq!(tx.receiver_count(), 1);
        drop(rx0);
        assert_eq!(tx.receiver_count(), 0);
        // Publisher still usable with no waiters — send_if_modified reports a
        // real change and does not error on the receiver-less channel.
        let changed = tx.send_if_modified(|cur| {
            cur.content_revision = 2;
            true
        });
        assert!(changed);
        assert_eq!(tx.borrow().content_revision, 2);
    }
}
