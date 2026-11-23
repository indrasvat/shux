VERDICT: PASS
Trivial tier: the one VT path this diff touches is crates/shux-ui/src/statusbar.rs, where StatusSegment gains a `group` field that only the status-bar fitting code in crates/shux/src/statusbar_build.rs reads; render_row is unchanged, and make vt-neutrality is 0/0 on every layer (base d36b738 → head 237778b).
