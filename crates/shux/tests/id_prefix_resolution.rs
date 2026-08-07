//! Issue #120 — an id shux PRINTS must be an id shux ACCEPTS.
//!
//! Every human-readable listing (`session list`, `pane list`) prints the
//! 8-character short form of an entity's UUID, and every success line echoes
//! the same short form back. Before this suite, none of those short forms
//! resolved: the RPC layer handed the string straight to `Uuid::parse_str`,
//! which rejected it as `invalid_params`. The documented lens loop
//! (`lens run` → `pane wait-settled` → `pane glance`) was therefore
//! unfollowable from anything a human or an agent could read off the screen.
//!
//! These tests drive the REAL `shux` binary against a REAL daemon over a real
//! UDS — no re-implemented handlers, no mocked graph. What they assert is the
//! round trip: take the id out of the listing, feed it back in, and land on
//! the same entity a full UUID would.
//!
//! Ambiguity (one prefix naming several entities) is not exercised here. Ids
//! are random v4 UUIDs, so provoking a shared 4-hex prefix takes on the order
//! of a thousand live sessions — it was confirmed once by hand at that scale
//! (see `docs/tasks/091-entity-id-references.md`), which is not something to
//! run on every commit. It is pinned exhaustively and deterministically one
//! layer down instead: over hand-built snapshots in `shux_core::idref`, over
//! crafted listings in the CLI resolvers, and over the `RefError` -> RPC
//! mapping in the daemon.

mod lens_common;
use lens_common::*;

/// A session created for one test, with every id it exposes.
struct Fixed {
    name: String,
    session_id: String,
    window_id: String,
    pane_id: String,
}

impl Fixed {
    fn short_session(&self) -> &str {
        &self.session_id[..8]
    }
    fn short_window(&self) -> &str {
        &self.window_id[..8]
    }
    fn short_pane(&self) -> &str {
        &self.pane_id[..8]
    }
}

/// Create a detached session running a long-lived, colour-probed command, and
/// read back every id the daemon assigned it.
///
/// The pane prints a truecolor + indexed + basic colour probe before parking,
/// so any capture taken through it would show a monochrome regression rather
/// than passing on a blank screen (CLAUDE.md colour-probe rule).
fn fixture(h: &Harness, tag: &str) -> Fixed {
    let name = format!("id120-{tag}-{}", unique());
    // Trailing argv (exec'd directly), so the probe and the park are one
    // long-lived shell rather than a command that prints and exits.
    let cmd = "printf '\\033[38;2;255;0;128mTRUECOLOR\\033[0m \
                \\033[38;5;208mINDEXED\\033[0m \\033[31mBASIC\\033[0m\\n'; \
               sleep 300";
    let out = h.cli(&[
        "--format", "json", "session", "create", &name, "-d", "--", "sh", "-c", cmd,
    ]);
    assert!(
        out.status.success(),
        "session create failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let listed = h.rpc_ok("session.list", serde_json::json!({}));
    let sessions = listed["sessions"].as_array().expect("sessions array");
    let s = sessions
        .iter()
        .find(|s| s["name"] == serde_json::Value::String(name.clone()))
        .unwrap_or_else(|| panic!("session {name} missing from session.list"));

    let session_id = s["id"].as_str().expect("session id").to_string();
    let window_id = s["active_window_id"]
        .as_str()
        .expect("active window id")
        .to_string();
    let pane_id = s["pane_id"].as_str().expect("pane id").to_string();

    // The pane has to have produced its probe before anything captures it —
    // "not yet started" and "settled" look identical to a quiet-window wait.
    h.wait_for(&pane_id, "TRUECOLOR", 10_000)
        .expect("fixture pane never printed its colour probe");

    Fixed {
        name,
        session_id,
        window_id,
        pane_id,
    }
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn stdout_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// `pane list` in its default human format prints the short pane id. Feed that
/// exact string back to every command that takes a pane and it must land.
#[test]
fn short_pane_id_from_pane_list_is_accepted_by_every_pane_command() {
    let h = Harness::new();
    let f = fixture(&h, "roundtrip");

    // Take the id the way a human would: read it off `pane list`.
    let listed = h.cli(&["--format", "plain", "pane", "list", "-s", &f.name]);
    assert!(
        listed.status.success(),
        "pane list failed: {}",
        stderr_of(&listed)
    );
    let printed = stdout_of(&listed);
    let short = printed
        .lines()
        .next()
        .and_then(|l| l.split('\t').next())
        .expect("pane list printed no rows")
        .to_string();
    assert_eq!(
        short,
        f.short_pane(),
        "pane list must print the 8-char short id"
    );

    // Non-destructive readers and drivers, in the order the lens loop uses
    // them. Every one of these rejected `short` before the fix.
    let cases: Vec<(&str, Vec<&str>)> = vec![
        ("glance", vec!["pane", "glance", &short, "--text-only"]),
        (
            "wait-settled",
            vec![
                "pane",
                "wait-settled",
                &short,
                "--quiet",
                "100",
                "--timeout",
                "5000",
            ],
        ),
        ("checkpoint", vec!["pane", "checkpoint", &short]),
        (
            "capture",
            vec!["pane", "capture", "-s", &f.name, "-p", &short],
        ),
        (
            "wait-for",
            vec![
                "pane",
                "wait-for",
                "-s",
                &f.name,
                "-p",
                &short,
                "--text",
                "TRUECOLOR",
                "--timeout-ms",
                "5000",
            ],
        ),
        (
            "title",
            vec![
                "pane", "title", "-s", &f.name, "-p", &short, "-t", "renamed",
            ],
        ),
        (
            "send-keys",
            vec![
                "pane",
                "send-keys",
                "-s",
                &f.name,
                "-p",
                &short,
                "--text",
                " ",
            ],
        ),
        (
            "set-size",
            vec![
                "pane", "set-size", "-s", &f.name, "-p", &short, "--cols", "80", "--rows", "24",
            ],
        ),
        ("zoom", vec!["pane", "zoom", "-s", &f.name, "-p", &short]),
    ];

    for (label, args) in cases {
        let out = h.cli(&args);
        assert!(
            out.status.success(),
            "`shux {}` rejected the short id `{short}` that `pane list` printed\n\
             stdout: {}\nstderr: {}",
            args.join(" "),
            stdout_of(&out),
            stderr_of(&out),
        );
        let _ = label;
    }

    // `pane snapshot` writes a file — assert the file, not just the exit code.
    let png = h.state_dir().join("short.png");
    let png_arg = png.to_string_lossy().to_string();
    let out = h.cli(&[
        "pane", "snapshot", "-s", &f.name, "-p", &short, "-o", &png_arg,
    ]);
    assert!(
        out.status.success(),
        "`pane snapshot` rejected the short id: {}",
        stderr_of(&out)
    );
    let bytes = std::fs::read(&png).expect("snapshot png missing");
    assert!(bytes.len() > 1000, "snapshot png is suspiciously small");

    h.kill_session(&f.session_id);
}

/// The short id and the full UUID must address the SAME pane — resolving must
/// not merely stop erroring, it must land on the right entity.
#[test]
fn short_and_full_pane_ids_address_the_same_pane() {
    let h = Harness::new();
    let f = fixture(&h, "same");

    let by_full = h.rpc_ok(
        "pane.glance",
        serde_json::json!({ "pane_id": f.pane_id, "include_png": false }),
    );
    let by_short = h.rpc_ok(
        "pane.glance",
        serde_json::json!({ "pane_id": f.short_pane(), "include_png": false }),
    );

    assert_eq!(
        by_full["text"], by_short["text"],
        "short id resolved to a different pane's screen"
    );
    assert!(
        by_full["text"]
            .as_str()
            .expect("glance text")
            .contains("TRUECOLOR"),
        "fixture probe missing — the test would pass on a blank pane"
    );

    h.kill_session(&f.session_id);
}

/// `session list` prints a short session id too; `-s` must take it.
#[test]
fn short_session_id_from_session_list_is_accepted() {
    let h = Harness::new();
    let f = fixture(&h, "sess");

    let listed = h.cli(&["--format", "plain", "session", "list"]);
    let printed = stdout_of(&listed);
    let row = printed
        .lines()
        .find(|l| l.starts_with(&f.name))
        .expect("session missing from session list");
    let short = row.split('\t').nth(3).expect("session list id column");
    assert_eq!(short, f.short_session());

    let out = h.cli(&["--format", "json", "pane", "list", "-s", short]);
    assert!(
        out.status.success(),
        "`pane list -s {short}` rejected the id `session list` printed: {}",
        stderr_of(&out)
    );
    assert!(
        stdout_of(&out).contains(&f.pane_id),
        "short session id resolved to the wrong session"
    );

    h.kill_session(&f.session_id);
}

/// `window list` must print an id, and `--window` must accept it — both the
/// full UUID (which `--help` already promised) and the printed short form.
#[test]
fn window_list_prints_an_id_that_window_commands_accept() {
    let h = Harness::new();
    let f = fixture(&h, "win");

    let listed = h.cli(&["--format", "plain", "window", "list", "-s", &f.name]);
    let printed = stdout_of(&listed);
    let row = printed.lines().next().expect("window list printed no rows");
    let cols: Vec<&str> = row.split('\t').collect();
    assert!(
        cols.iter().any(|c| *c == f.short_window()),
        "window list must print the window's short id; got row {row:?}"
    );

    for spec in [f.short_window(), f.window_id.as_str()] {
        let out = h.cli(&[
            "--format", "json", "pane", "list", "-s", &f.name, "-w", spec,
        ]);
        assert!(
            out.status.success(),
            "`--window {spec}` was rejected: {}",
            stderr_of(&out)
        );
        assert!(
            stdout_of(&out).contains(&f.pane_id),
            "`--window {spec}` resolved to the wrong window"
        );
    }

    // The numeric index must keep winning over anything else — a script that
    // passes `-w 0` today must not start matching an id that begins with 0.
    let out = h.cli(&["--format", "json", "pane", "list", "-s", &f.name, "-w", "0"]);
    assert!(
        out.status.success(),
        "`-w 0` regressed: {}",
        stderr_of(&out)
    );

    h.kill_session(&f.session_id);
}

/// A partially-hyphenated paste (`b57c601b-5f61`) is what you get when you
/// double-click half a UUID. It must resolve like any other prefix.
#[test]
fn partial_hyphenated_and_uppercase_prefixes_resolve() {
    let h = Harness::new();
    let f = fixture(&h, "forms");

    let expected = h.rpc_ok(
        "pane.glance",
        serde_json::json!({ "pane_id": f.pane_id, "include_png": false }),
    );

    let variants = vec![
        f.pane_id[..13].to_string(),                  // "xxxxxxxx-xxxx"
        f.pane_id[..8].to_uppercase(),                // shouted short id
        f.pane_id.replace('-', "")[..12].to_string(), // hyphen-free prefix
        format!("  {}  ", f.short_pane()),            // pasted with surrounding space
    ];

    for v in variants {
        let got = h.rpc_raw(
            "pane.glance",
            serde_json::json!({ "pane_id": v, "include_png": false }),
        );
        let got = got.expect_result(&format!("glance via `{v}`"));
        assert_eq!(
            got["text"], expected["text"],
            "`{v}` did not resolve to the fixture pane"
        );
    }

    h.kill_session(&f.session_id);
}

/// Garbage in must still be `invalid_params`, and a well-formed-but-unknown
/// prefix must be `not_found` — a resolver that guesses is worse than one that
/// rejects.
#[test]
fn malformed_and_unmatched_refs_keep_their_distinct_errors() {
    let h = Harness::new();
    let f = fixture(&h, "errs");

    // Too short to be safe: 3 hex characters could match anything.
    let too_short = h.rpc_raw("pane.glance", serde_json::json!({ "pane_id": "abc" }));
    let e = too_short.expect_error_code(-32602, "3-char ref must be invalid_params");
    let detail = e
        .data
        .as_ref()
        .and_then(|d| d["detail"].as_str())
        .unwrap_or_default()
        .to_string();
    assert!(
        detail.contains('4'),
        "the error must say how many characters are needed; got {detail:?}"
    );

    // Not hex at all.
    h.rpc_raw("pane.glance", serde_json::json!({ "pane_id": "zzzzzzzz" }))
        .expect_error_code(-32602, "non-hex ref must be invalid_params");

    // Empty string.
    h.rpc_raw("pane.glance", serde_json::json!({ "pane_id": "" }))
        .expect_error_code(-32602, "empty ref must be invalid_params");

    // Longer than a UUID.
    h.rpc_raw(
        "pane.glance",
        serde_json::json!({ "pane_id": "0".repeat(40) }),
    )
    .expect_error_code(-32602, "over-long ref must be invalid_params");

    // Well-formed prefix, nothing matches it. Pick a prefix that cannot
    // collide with the live pane.
    let live = f.pane_id.replace('-', "");
    let orphan = if live.starts_with("dead") {
        "beef"
    } else {
        "dead"
    };
    h.rpc_raw("pane.glance", serde_json::json!({ "pane_id": orphan }))
        .expect_error_code(-32004, "unmatched prefix must be not_found");

    // Regression guard: a syntactically perfect UUID that does not exist must
    // still be not_found, exactly as before the fix.
    h.rpc_raw(
        "pane.glance",
        serde_json::json!({ "pane_id": "00000000-0000-4000-8000-000000000001" }),
    )
    .expect_error_code(-32004, "unknown full UUID must stay not_found");

    h.kill_session(&f.session_id);
}

/// The CLI's exit codes for the lens verbs are part of the contract: 2 for a
/// malformed reference, 3 for one that resolves to nothing.
#[test]
fn cli_exit_codes_distinguish_malformed_from_unknown() {
    let h = Harness::new();
    let f = fixture(&h, "exit");

    let bad = h.cli(&["pane", "glance", "abc", "--text-only"]);
    assert_eq!(
        bad.status.code(),
        Some(2),
        "malformed ref must exit 2; stderr: {}",
        stderr_of(&bad)
    );
    // The exit code alone is not an answer. Someone who pasted three
    // characters has to be told how many are needed, and which parameter is
    // at fault — "invalid_params (code -32602)" says neither.
    let msg = stderr_of(&bad);
    assert!(
        msg.contains("pane_id") && msg.contains('4'),
        "the error must name the parameter and the minimum length: {msg}"
    );

    let live = f.pane_id.replace('-', "");
    let orphan = if live.starts_with("dead") {
        "beef"
    } else {
        "dead"
    };
    let unknown = h.cli(&["pane", "glance", orphan, "--text-only"]);
    assert_eq!(
        unknown.status.code(),
        Some(3),
        "unmatched ref must exit 3; stderr: {}",
        stderr_of(&unknown)
    );

    h.kill_session(&f.session_id);
}

/// Destructive verbs last: killing a pane by its short id is the end of the
/// round trip, and it must kill the pane the short id named.
#[test]
fn destructive_pane_verbs_accept_the_short_id() {
    let h = Harness::new();
    let f = fixture(&h, "kill");

    // Split so there are two panes: one to swap with, one to kill.
    let split = h.rpc_ok(
        "pane.split",
        serde_json::json!({ "pane_id": f.short_pane(), "direction": "horizontal" }),
    );
    let second = split["pane"]["id"]
        .as_str()
        .expect("split returned no pane id")
        .to_string();

    let out = h.cli(&["pane", "focus", "-s", &f.name, "-p", &second[..8]]);
    assert!(
        out.status.success(),
        "focus by short id: {}",
        stderr_of(&out)
    );

    let out = h.cli(&[
        "pane",
        "swap",
        "-s",
        &f.name,
        "-p",
        f.short_pane(),
        "-t",
        &second[..8],
    ]);
    assert!(
        out.status.success(),
        "swap by short id: {}",
        stderr_of(&out)
    );

    let out = h.cli(&["pane", "kill", "-s", &f.name, "-p", &second[..8]]);
    assert!(
        out.status.success(),
        "kill by short id: {}",
        stderr_of(&out)
    );

    let remaining = h.rpc_ok(
        "pane.list",
        serde_json::json!({ "session_id": f.session_id }),
    );
    let ids: Vec<String> = remaining
        .as_array()
        .expect("pane list array")
        .iter()
        .map(|p| p["id"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(!ids.contains(&second), "the killed pane is still listed");
    assert!(
        ids.contains(&f.pane_id),
        "kill by short id killed the WRONG pane"
    );

    h.kill_session(&f.session_id);
}

/// A reference that resolves to nothing must SAY so. The CLI composes these
/// messages itself, and they were being rendered as a contentless "resource
/// not found" — the least useful thing to print to someone who has just
/// mistyped an id (found while fixing #120; same error path).
#[test]
fn a_failed_reference_names_what_was_missing() {
    let h = Harness::new();
    let f = fixture(&h, "msg");

    let out = h.cli(&["pane", "list", "-s", "nosuchsession"]);
    let msg = stderr_of(&out);
    assert!(
        msg.contains("nosuchsession"),
        "the error must quote the session that was not found: {msg}"
    );

    let out = h.cli(&["pane", "list", "-s", &f.name, "-w", "nosuchwindow"]);
    let msg = stderr_of(&out);
    assert!(
        msg.contains("nosuchwindow"),
        "the error must quote the window that was not found: {msg}"
    );

    // A duplicate session name reports the name it collided with, rather than
    // the raw error code.
    let out = h.cli(&[
        "session", "create", &f.name, "-d", "--", "sh", "-c", "sleep 1",
    ]);
    let msg = format!("{}{}", stdout_of(&out), stderr_of(&out));
    assert!(
        msg.contains(&f.name) && msg.contains("already exists"),
        "a duplicate name must be reported as such: {msg}"
    );

    h.kill_session(&f.session_id);
}
