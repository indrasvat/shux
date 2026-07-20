//! Frame-stability decisions for `pane.wait_settled` hold-ms / stable-frames modes (task 083).
//!
//! The default settle mode is QUIET (no Class-A mutation for `quiet_ms`) and lives in the
//! daemon (`settle_is_quiet`/`settle_decide`). It keys on mutation TIMING, which is
//! value-independent — an identical repaint still bumps the revision — so it (a) times out on a
//! fast identical-repainter that never goes quiet and (b) false-settles a slow spinner in the
//! gap between frames. Task 083 adds two frame-CONTENT-keyed criteria that fix both, keyed on
//! the masked-frame hash so daemon stability matches the golden-compare domain.
//!
//! This module is the PURE decision core (unit-tested here); the daemon feeds it the freshly
//! read `(revision, frame-hash, now_ns)` on each settle-loop wake and reads back
//! [`FrameStability::is_settled`]. Design + council record: `.local/083-design.md`.

use std::hash::{Hash, Hasher};

use crate::FrameEnvelope;

/// A cheap `u64` content hash of a captured frame for IN-PROCESS frame-stability tracking
/// (`pane.wait_settled` hold-ms / stable-frames). It keys the SAME canonical, mask-applied form
/// as [`crate::capture_sha256`], so daemon-side stability tracks exactly what the golden compare
/// would see. It is NOT persisted and NOT a golden pin — only a transient wake-to-wake identity,
/// so a process-local hasher is fine (no cross-version stability required).
///
/// Accepted risk (adv-083 Agent A MINOR): a 64-bit hash is the single aliasing point — a
/// collision would make two visibly-different frames look identical and false-satisfy BOTH
/// stability criteria at once (they key on the same hash, so the AND-composition is no backstop).
/// This is acceptable HERE and only here: the value is transient, non-persisted, and bounded by
/// the settle `timeout_ms`; a `DefaultHasher` (SipHash-1-3) collision needs ~2³² work and 200k
/// real frames produced none. The golden COMPARE never trusts this — it uses the full
/// `capture_sha256` (SHA-256) content pin.
pub fn frame_stability_hash(env: &FrameEnvelope) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    env.to_canonical_json().hash(&mut h);
    h.finish()
}

/// Tracks frame stability across `pane.wait_settled` loop wakes (task 083; council #2/#3 in
/// `.local/dootsabha-083-design.json`).
///
/// Seeded at settle entry with the current `(revision, hash)`; [`observe`](Self::observe) is
/// called on each later wake with the newly-read `(revision, hash, now_ns)`; then
/// [`is_settled`](Self::is_settled) decides against the requested criteria. Two independent
/// criteria, AND-composed when both are active:
///
/// - **stable_frames K** (count): K CONTIGUOUS identical-hash revisions. A wake whose revision
///   SKIPS (the revision `watch` coalesced away intermediate revisions) cannot prove contiguity
///   — an `A→B→A` churn observed as `A,A` would false-pass — so it RESETS the run, taints the
///   hold clock, and sets the [`coalesced`](Self::coalesced) diagnostic (council #3). The hard
///   `timeout_ms` bound (owned by the daemon loop) turns a never-stable pane into
///   `settle_never_stable`, never a hang.
/// - **hold_ms H** (duration): the frame hash unchanged for ≥H continuous ms. Silence advances
///   the clock (no revision → no change); an identical repaint advances it (hash unchanged); a
///   hash-CHANGING revision resets it (council #2). Seeded to entry time, so an already-stable
///   pane still holds a full H from entry (conservative — never under-waits).
#[derive(Debug, Clone)]
pub struct FrameStability {
    /// Highest revision observed so far.
    last_rev: u64,
    /// Frame hash at `last_rev`.
    last_hash: u64,
    /// Length of the current contiguous identical-hash run (in observations).
    run_len: u32,
    /// Monotonic-ns anchor of the last CONTENT change (hash change / tainting gap) — the hold
    /// clock's zero.
    last_change_ns: u64,
    /// Set once a revision gap (coalescing) was observed; surfaced as a diagnostic.
    coalesced: bool,
}

impl FrameStability {
    /// Seed at settle entry from the current `(revision, hash)`. The hold clock starts NOW —
    /// the frame must stay stable for a full `hold_ms` measured from entry.
    pub fn seed(rev: u64, hash: u64, now_ns: u64) -> Self {
        Self {
            last_rev: rev,
            last_hash: hash,
            run_len: 1,
            last_change_ns: now_ns,
            coalesced: false,
        }
    }

    /// Fold one settle-loop wake at `(rev, hash, now_ns)` into the run/hold state. A wake that
    /// does not advance the revision (a spurious or late scheduler wake) is a no-op.
    pub fn observe(&mut self, rev: u64, hash: u64, now_ns: u64) {
        if rev <= self.last_rev {
            // No strictly-newer revision — nothing to fold (do not double-count a re-read).
            return;
        }
        let contiguous = rev == self.last_rev + 1;
        let changed = hash != self.last_hash;
        if !contiguous {
            // Intermediate revisions were coalesced away: we cannot prove the frame held equal
            // across the gap (an `A→B→A` alias reads as equal endpoints). Reset the run and,
            // conservatively, the hold clock; flag the gap for diagnostics.
            self.coalesced = true;
            self.run_len = 1;
            self.last_change_ns = now_ns;
        } else if changed {
            // Contiguous but the content moved — a fresh run, hold clock restarts.
            self.run_len = 1;
            self.last_change_ns = now_ns;
        } else {
            // Contiguous and identical — extend the run; the hold clock keeps running.
            self.run_len = self.run_len.saturating_add(1);
        }
        self.last_rev = rev;
        self.last_hash = hash;
    }

    /// Decide against the requested criteria. `stable_frames <= 1` disables the count criterion;
    /// `hold_ms == 0` disables the hold criterion; when both are active they AND. With NEITHER
    /// active this is a vacuous `true` — the daemon only consults it in frame-stability mode.
    pub fn is_settled(&self, stable_frames: u32, hold_ms: u64, now_ns: u64) -> bool {
        let count_ok = stable_frames <= 1 || self.run_len >= stable_frames;
        let hold_ok = hold_ms == 0
            || now_ns.saturating_sub(self.last_change_ns) >= hold_ms.saturating_mul(1_000_000);
        count_ok && hold_ok
    }

    /// Current contiguous identical-hash run length (for diagnostics / `waited` reporting).
    pub fn run_len(&self) -> u32 {
        self.run_len
    }

    /// The highest revision folded in so far — the `revision` reported when settle returns.
    pub fn last_rev(&self) -> u64 {
        self.last_rev
    }

    /// Nanoseconds still owed on the hold window (0 once `hold_ms` is satisfied). The daemon
    /// sizes its silence-wake so a pane that goes quiet still settles `hold_ms` after its last
    /// change without polling. `hold_ms == 0` (hold disabled) returns 0.
    pub fn ns_until_hold(&self, hold_ms: u64, now_ns: u64) -> u64 {
        hold_ms
            .saturating_mul(1_000_000)
            .saturating_sub(now_ns.saturating_sub(self.last_change_ns))
    }

    /// Whether a revision gap (watch coalescing) was ever observed — surfaced so a CI
    /// `settle_never_stable` under heavy churn is diagnosable (raise `timeout_ms`, lower the
    /// repaint rate, or use `hold_ms`, which is coalescing-immune).
    pub fn coalesced(&self) -> bool {
        self.coalesced
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000; // ns per ms

    #[test]
    fn seed_starts_a_unit_run() {
        let s = FrameStability::seed(5, 0xAAAA, 0);
        assert_eq!(s.run_len(), 1);
        assert!(!s.coalesced());
    }

    #[test]
    fn stable_frames_settles_after_k_contiguous_identical() {
        // K=3 identical CONTIGUOUS revisions → settled at the 3rd.
        let mut s = FrameStability::seed(1, 0xAA, 0);
        assert!(!s.is_settled(3, 0, 0), "1 frame is not 3");
        s.observe(2, 0xAA, 10 * MS);
        assert!(!s.is_settled(3, 0, 10 * MS), "2 frames is not 3");
        s.observe(3, 0xAA, 20 * MS);
        assert!(
            s.is_settled(3, 0, 20 * MS),
            "3 identical contiguous → settled"
        );
    }

    #[test]
    fn flip_never_reaches_two_identical() {
        // A genuine animation (A,B,A,B) never gets 2 identical in a row → never settles under
        // stable_frames — the perpetual-animation → settle_never_stable case.
        let mut s = FrameStability::seed(1, 0xA, 0);
        for (i, h) in [(2u64, 0xBu64), (3, 0xA), (4, 0xB), (5, 0xA)] {
            s.observe(i, h, i * MS);
            assert!(
                !s.is_settled(2, 0, i * MS),
                "flip must not settle under stable_frames=2"
            );
        }
    }

    #[test]
    fn coalesced_gap_resets_the_run_defeating_a_b_a_alias() {
        // council #3: seed A@rev5; a wake at rev8 skips 6,7 (which could have been B). Even
        // though the OBSERVED hash equals the seed, the run must RESET (not read as A,A) and the
        // gap must be flagged.
        let mut s = FrameStability::seed(5, 0xA, 0);
        s.observe(8, 0xA, 10 * MS); // rev jumped 5→8: coalescing
        assert!(s.coalesced(), "a revision gap must be flagged");
        assert_eq!(
            s.run_len(),
            1,
            "an aliased A,A across a gap must not count as 2"
        );
        assert!(
            !s.is_settled(2, 0, 10 * MS),
            "stable_frames=2 must not be satisfied by an A→(B?)→A alias"
        );
        // A subsequent CONTIGUOUS identical revision resumes counting from the reset run.
        s.observe(9, 0xA, 20 * MS);
        assert!(
            s.is_settled(2, 0, 20 * MS),
            "two contiguous identical after the gap → settled"
        );
    }

    #[test]
    fn hold_ms_requires_the_frame_unchanged_for_the_full_window() {
        // council #2: seed at t=0; identical contiguous frames keep the clock running.
        let mut s = FrameStability::seed(1, 0xA, 0);
        s.observe(2, 0xA, 200 * MS);
        assert!(!s.is_settled(0, 300, 200 * MS), "200ms < hold 300ms");
        s.observe(3, 0xA, 350 * MS);
        assert!(
            s.is_settled(0, 300, 350 * MS),
            "unchanged 350ms ≥ hold 300ms → settled"
        );
    }

    #[test]
    fn hold_ms_resets_on_a_content_change_slow_spinner() {
        // The slow-spinner fix: a frame that changes every 200ms with hold_ms=300 never holds.
        let mut s = FrameStability::seed(1, 0xA, 0);
        s.observe(2, 0xB, 200 * MS); // changed at 200ms → clock resets to 200ms
        assert!(!s.is_settled(0, 300, 200 * MS));
        // 250ms after the change (t=450) still < 300ms since the last change.
        assert!(
            !s.is_settled(0, 300, 450 * MS),
            "only 250ms since last change"
        );
        s.observe(3, 0xA, 400 * MS); // changed again at 400ms
        assert!(
            !s.is_settled(0, 300, 650 * MS),
            "250ms since the 400ms change < 300ms"
        );
    }

    #[test]
    fn hold_ms_counts_silence_after_a_sampled_frame() {
        // A pane that paints once then goes silent settles hold_ms after its last change — no
        // further revisions needed (silence is stability).
        let s = FrameStability::seed(1, 0xA, 0);
        assert!(!s.is_settled(0, 300, 100 * MS), "100ms of silence < 300ms");
        assert!(
            s.is_settled(0, 300, 300 * MS),
            "300ms of silence ≥ hold 300ms → settled"
        );
    }

    #[test]
    fn both_criteria_and_compose() {
        // stable_frames=3 AND hold_ms=100: need BOTH K contiguous identical AND ≥100ms stable.
        let mut s = FrameStability::seed(1, 0xA, 0);
        s.observe(2, 0xA, 40 * MS);
        s.observe(3, 0xA, 80 * MS);
        // run_len is 3 (count ok) but only 80ms held (hold not ok).
        assert!(!s.is_settled(3, 100, 80 * MS), "count ok, hold not yet");
        // hold satisfied at 100ms with the run still ≥3.
        assert!(s.is_settled(3, 100, 100 * MS), "both criteria satisfied");
    }

    #[test]
    fn spurious_wake_without_a_new_revision_is_a_noop() {
        let mut s = FrameStability::seed(4, 0xA, 0);
        s.observe(4, 0xB, 50 * MS); // same revision (stale re-read) — ignored
        assert_eq!(s.run_len(), 1);
        assert_eq!(
            s.last_hash, 0xA,
            "a non-advancing wake must not adopt a new hash"
        );
    }

    #[test]
    fn stable_frames_disabled_when_k_le_one() {
        let s = FrameStability::seed(1, 0xA, 0);
        assert!(
            s.is_settled(1, 0, 0),
            "K<=1 disables the count criterion (vacuous)"
        );
        assert!(s.is_settled(0, 0, 0), "K=0 also disables it");
    }
}
