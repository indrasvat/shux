//! The `shux` library target is reachable from an integration test.
//!
//! This file's existence is the assertion. `crates/shux` used to declare a
//! `[[bin]]` and nothing else, and a binary's internals cannot be imported —
//! which is why the lens-gate vocabulary spent three modules and 2,065 lines
//! stranded in `shux-vt` and `shux-raster`, two layers below where it belongs.
//! Both files said so in their own placement notes.
//!
//! So this is a regression test for the packaging, not for the functions it
//! calls: delete the library target and this file stops compiling, loudly,
//! instead of the next person rediscovering the constraint and working around
//! it a third time. The behaviour it pins is chosen to be reachable ONLY from
//! outside the crate — `report_fatal` in `main.rs` is not callable from here,
//! but the escaping it depends on is.

/// `main.rs`'s `report_fatal` prints an anyhow chain when `RUST_BACKTRACE` is
/// set, and that chain quotes untrusted input verbatim — a TOML parse error
/// echoes the offending source line, so a template carrying a raw ESC would
/// replay it straight at the operator's terminal. That is issue #104's class,
/// and `safe_diagnostic` is the thing standing between the two.
#[test]
fn safe_diagnostic_neutralises_an_escape_sequence_from_outside_the_crate() {
    let hostile = "parse error at line 3: \x1b]0;pwned\x07";
    let safe = shux::style::safe_diagnostic(hostile);

    assert!(
        !safe.contains('\x1b'),
        "ESC survived into a diagnostic: {safe:?}"
    );
    assert!(
        !safe.contains('\x07'),
        "BEL survived into a diagnostic: {safe:?}"
    );
    assert!(
        safe.contains("\\u{1b}"),
        "ESC should be escaped, not dropped: {safe:?}"
    );
    assert!(
        safe.starts_with("parse error at line 3: "),
        "the readable part of the message must survive: {safe:?}"
    );
}

/// Newline and tab are this block's structure, not an attack, so they are the
/// two control characters `safe_diagnostic` deliberately keeps. A multi-frame
/// backtrace escaped into one line would be unreadable.
#[test]
fn safe_diagnostic_keeps_the_layout_characters_a_backtrace_needs() {
    let chain = "Error: bad config\n\tat crates/shux/src/cli.rs:12\n";
    assert_eq!(shux::style::safe_diagnostic(chain), chain);
}
