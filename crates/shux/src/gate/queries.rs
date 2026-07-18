//! Terminal query-response determinism (design D9). Pure, CI-run.
//!
//! `respond_to_queries` is reserved-honest: the shux terminal answers OSC 11 / DA /
//! XTVERSION deterministically + byte-exact and cannot be silenced yet (081 does not
//! plumb suppression — a scenario relies on this for reproducible frames). These tests
//! pin the exact response bytes so a drift that would move a golden is caught. 081 adds
//! no code here; the responses are `shux_vt`'s. This module documents + guards the
//! contract 081 depends on.

#[cfg(test)]
mod tests {
    use shux_vt::VirtualTerminal;

    /// The runner's fixed terminal answers DA + OSC 11 (bg) byte-exact and
    /// deterministically — the version-independent contract a golden depends on.
    #[test]
    fn da_and_osc11_are_byte_exact_and_deterministic() {
        let mut a = VirtualTerminal::new(24, 80);
        let mut b = VirtualTerminal::new(24, 80);
        // Primary DA, secondary DA, OSC 11 (bg query, BEL-terminated).
        let prog = b"\x1b[c\x1b[>c\x1b]11;?\x07";
        let ra = a.process_with_responses(prog);
        let rb = b.process_with_responses(prog);
        assert_eq!(ra, rb, "query responses must be deterministic across VTs");
        assert_eq!(
            ra,
            vec![
                b"\x1b[?62;1;2;6;9;15;22c".to_vec(),
                b"\x1b[>0;95;0c".to_vec(),
                b"\x1b]11;rgb:0000/0000/0000\x07".to_vec(),
            ],
            "DA / secondary-DA / OSC-11 bg answers are byte-exact"
        );
    }

    /// XTVERSION is version-STAMPED but otherwise deterministic + byte-exact for a
    /// given build (the frozen DCS envelope). A scenario that queries it gets a stable
    /// answer per build; the version churn is the child's concern, not the terminal's.
    #[test]
    fn xtversion_is_byte_exact_for_the_build() {
        let mut vt = VirtualTerminal::new(24, 80);
        let responses = vt.process_with_responses(b"\x1b[>q");
        let want = format!("\x1bP>|shux {}\x1b\\", env!("CARGO_PKG_VERSION"));
        assert_eq!(responses, vec![want.into_bytes()]);
    }

    /// An OSC-11 query with an ST terminator is answered with ST (the runner never
    /// silences it — design D9 reserved suppression).
    #[test]
    fn osc11_honors_st_terminator() {
        let mut vt = VirtualTerminal::new(24, 80);
        let responses = vt.process_with_responses(b"\x1b]11;?\x1b\\");
        assert_eq!(
            responses,
            vec![b"\x1b]11;rgb:0000/0000/0000\x1b\\".to_vec()]
        );
    }
}
