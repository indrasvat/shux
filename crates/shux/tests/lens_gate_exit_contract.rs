//! FROZEN exit/status contract pins (task 078).
//!
//! Unlike the RED contract lane, this file is a NORMAL test target — it runs in
//! `make check` and CI `nextest --workspace`, and it is FROZEN (the `lens_gate_`
//! prefix ⇒ GATE-TEST-CHANGE: trailer). It exists to close adv-gate M3: the exit
//! map + status set live in `crates/shux-vt/src/gate.rs`, which is in NEITHER
//! freeze lane and whose co-located unit tests are editable in the same breath.
//! By pinning the exact status→exit VALUES here — in a guard-frozen, CI-run file
//! that does NOT depend on `worst()` as its own oracle — weakening the exit map
//! (or the closed status set) fails CI and cannot land without a trailer.
//!
//! Pure assertions on `shux_vt::GateStatus` — no daemon, no subprocess.

use shux_vt::GateStatus;

/// The frozen exit-code map (§7.4). Hard-coded VALUES, not derived — this is the
/// independent oracle the RED contract lane's rollup test cannot be.
#[test]
fn exit_map_values_are_frozen() {
    use GateStatus::*;
    let expect: &[(GateStatus, u8)] = &[
        (Pass, 0),
        (Xfail, 0),
        (Fail, 1),
        (Xpass, 1),
        (MissingGolden, 1),
        (XfailExpired, 1),
        (StaleGolden, 1),
        (SettleNeverStable, 1),
        (ScenarioError, 2),
        (InfraError, 3),
        (ChildError, 5),
        (UpdateRefused, 6),
    ];
    assert_eq!(
        expect.len(),
        12,
        "the exit map must cover the closed set of 12"
    );
    for (status, code) in expect {
        assert_eq!(status.exit_code(), *code, "frozen exit code for {status:?}");
    }
    // Exit 4 (permission) is a CLI-level code produced by NO status.
    for s in GateStatus::ALL {
        assert_ne!(
            s.exit_code(),
            4,
            "{s:?} must not map to the reserved perm code"
        );
    }
}

/// The status set is closed at exactly 12, with the frozen snake_case names
/// (covers the `Fail` name that gate.rs's co-located test omits — adv-gate A-min1).
#[test]
fn status_set_and_names_are_frozen() {
    assert_eq!(GateStatus::ALL.len(), 12);
    let expect: &[(GateStatus, &str)] = &[
        (GateStatus::Pass, "pass"),
        (GateStatus::Fail, "fail"),
        (GateStatus::Xfail, "xfail"),
        (GateStatus::Xpass, "xpass"),
        (GateStatus::MissingGolden, "missing_golden"),
        (GateStatus::XfailExpired, "xfail_expired"),
        (GateStatus::StaleGolden, "stale_golden"),
        (GateStatus::ChildError, "child_error"),
        (GateStatus::SettleNeverStable, "settle_never_stable"),
        (GateStatus::ScenarioError, "scenario_error"),
        (GateStatus::InfraError, "infra_error"),
        (GateStatus::UpdateRefused, "update_refused"),
    ];
    assert_eq!(expect.len(), 12);
    for (status, name) in expect {
        let json = serde_json::to_string(status).unwrap();
        assert_eq!(json, format!("\"{name}\""), "frozen name for {status:?}");
    }
    // `palette_unportable` is a `fail` reason, never a status.
    assert!(serde_json::from_str::<GateStatus>("\"palette_unportable\"").is_err());
}

/// The worst-frame rollup must never mask a regression with a higher-exit
/// operational error (adv-gate M2) — pinned here independently of gate.rs.
#[test]
fn rollup_never_masks_a_regression() {
    use GateStatus::*;
    let regressions = [
        Fail,
        Xpass,
        MissingGolden,
        XfailExpired,
        StaleGolden,
        SettleNeverStable,
    ];
    let errors = [ChildError, ScenarioError, InfraError, UpdateRefused];
    for r in regressions {
        for e in errors {
            assert_eq!(
                r.worst(e).exit_code(),
                1,
                "worst({r:?}, {e:?}) must stay exit 1"
            );
            assert_eq!(e.worst(r).exit_code(), 1, "order-independent");
        }
    }
}
