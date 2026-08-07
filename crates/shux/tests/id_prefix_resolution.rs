//! Issue #120 — an id shux PRINTS must be an id shux ACCEPTS.
//!
//! Every human-readable listing (`session list`, `window list`, `pane list`)
//! prints the 8-character short form of an entity's UUID, and every success
//! line echoes the same short form back. Before this suite, none of those short
//! forms resolved: the RPC layer handed the string straight to
//! `Uuid::parse_str`, which rejected 8 characters as malformed. The documented
//! lens loop (`lens run` → `pane wait-settled` → `pane glance`) was therefore
//! unfollowable from anything a human or an agent could read off the screen.
//!
//! These tests drive the REAL `shux` binary against a REAL daemon over a real
//! UDS — no re-implemented handlers, no mocked graph.
//!
//! THE FIXTURE IS DELIBERATELY CROWDED. Two panes with different screen
//! content in one window, plus a second window. With a single pane, "resolved
//! the short id" and "ignored the argument and used the only pane" are
//! indistinguishable, and every assertion below would pass against a resolver
//! that read no id at all. Every test drives pane **B** — never the session's
//! first pane — and checks a post-condition on B that is absent on A.
//!
//! Ambiguity (one prefix naming several entities) is not exercised here. Ids
//! are random v4 UUIDs, so provoking a shared 4-hex prefix takes on the order
//! of a thousand live sessions — confirmed once by hand at that scale (see
//! `docs/tasks/091-entity-id-references.md`), which is not something to run on
//! every commit. It is pinned deterministically one layer down instead: over
//! hand-built snapshots in `shux_core::idref`, over crafted listings in the CLI
//! resolvers, and over the `RefError` → RPC mapping in the daemon.

mod lens_common;
use lens_common::*;

/// A session created for one test, with every id it exposes.
struct Fixed {
    name: String,
    session_id: String,
    /// Window 0 — holds `pane_a` and `pane_b`.
    window_id: String,
    /// Window 1 — exists so window resolution has a wrong answer available.
    window2_id: String,
    /// The session's original pane. Screen says ALPHA, plus the colour probe.
    pane_a: String,
    /// Split from A. Screen says BRAVO. This is the pane the tests drive.
    pane_b: String,
}

impl Fixed {
    fn short_session(&self) -> &str {
        &self.session_id[..8]
    }
    fn short_window(&self) -> &str {
        &self.window_id[..8]
    }
    /// The id under test — a pane that is NOT the first one in the session.
    fn short_pane(&self) -> &str {
        &self.pane_b[..8]
    }
}

/// Pane A's command: a colour probe, then park.
///
/// The probe is truecolor + 256-indexed + basic ANSI, so a capture or snapshot
/// taken through this pane shows a monochrome regression instead of passing on
/// a blank screen (CLAUDE.md colour-probe rule).
const PROBE: &str = concat!(
    "printf 'ALPHA ",
    "\\033[38;2;255;0;128mTRUECOLOR\\033[0m ",
    "\\033[38;5;208mINDEXED\\033[0m ",
    "\\033[31mBASIC\\033[0m\\n'; ",
    "sleep 300"
);

fn fixture(h: &Harness, tag: &str) -> Fixed {
    let name = format!("id120-{tag}-{}", unique());
    // Trailing argv, not `--cmd`: `--cmd` is split on whitespace rather than
    // run through a shell (issue #125), which would exec `printf` with `sleep`
    // as an argument and leave the pane dead.
    let out = h.cli(&[
        "--format", "json", "session", "create", &name, "-d", "--", "sh", "-c", PROBE,
    ]);
    assert!(
        out.status.success(),
        "session create failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let listed = h.rpc_ok("session.list", serde_json::json!({}));
    let s = listed["sessions"]
        .as_array()
        .expect("sessions array")
        .iter()
        .find(|s| s["name"] == serde_json::Value::String(name.clone()))
        .unwrap_or_else(|| panic!("session {name} missing from session.list"))
        .clone();

    let session_id = s["id"].as_str().expect("session id").to_string();
    let window_id = s["active_window_id"]
        .as_str()
        .expect("active window id")
        .to_string();
    let pane_a = s["pane_id"].as_str().expect("pane id").to_string();

    // Content, THEN settle: a not-yet-started pane and a quiet one look
    // identical to a wait on stillness alone.
    h.wait_for(&pane_a, "ALPHA", 15_000)
        .expect("pane A never printed its colour probe");

    // A second pane in the same window, with its own marker on screen.
    let split = h.rpc_ok(
        "pane.split",
        serde_json::json!({ "pane_id": pane_a, "direction": "horizontal" }),
    );
    let pane_b = split["pane"]["id"]
        .as_str()
        .expect("split returned no pane id")
        .to_string();
    h.rpc_ok(
        "pane.send_keys",
        serde_json::json!({ "pane_id": pane_b, "text": "printf 'BRAVO\\n'\n" }),
    );
    h.wait_for(&pane_b, "BRAVO", 15_000)
        .expect("pane B never printed its marker");

    // A second window, so `-w 0` has something to be wrong about.
    let w2 = h.rpc_ok(
        "window.create",
        serde_json::json!({ "session_id": session_id, "name": "second" }),
    );
    let window2_id = w2["id"]
        .as_str()
        .or_else(|| w2["window"]["id"].as_str())
        .expect("window.create returned no id")
        .to_string();

    // Focus back to window 0 so "the active window" is unambiguous.
    h.rpc_ok("window.focus", serde_json::json!({ "id": window_id }));

    Fixed {
        name,
        session_id,
        window_id,
        window2_id,
        pane_a,
        pane_b,
    }
}

/// Every pane in the session, as `pane.list` reports it across all windows.
fn all_panes(h: &Harness, f: &Fixed) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for wid in [&f.window_id, &f.window2_id] {
        let panes = h.rpc_ok(
            "pane.list",
            serde_json::json!({ "session_id": f.session_id, "window_id": wid }),
        );
        out.extend(panes.as_array().cloned().unwrap_or_default());
    }
    out
}

/// A pane's title as the daemon currently reports it — the post-condition that
/// proves a mutation landed on the pane the caller named and no other.
fn pane_title(h: &Harness, f: &Fixed, pane_id: &str) -> String {
    all_panes(h, f)
        .into_iter()
        .find(|p| p["id"].as_str() == Some(pane_id))
        .map(|p| p["title"].as_str().unwrap_or_default().to_string())
        .unwrap_or_else(|| panic!("pane {pane_id} not in pane.list"))
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn stdout_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Count pixels that differ from the image's own corner colour. A snapshot of
/// a live pane has thousands; a blank grid has none. Asserting on file size
/// instead would pass on an empty screen.
fn non_background_pixels(png: &[u8]) -> usize {
    let img = decode_png(png);
    let (w, h) = (img.width(), img.height());
    let bg = *img.get_pixel(1, 1);
    let mut n = 0;
    for y in 0..h {
        for x in 0..w {
            if *img.get_pixel(x, y) != bg {
                n += 1;
            }
        }
    }
    n
}

/// `pane list` in its default human format prints the short pane id. Feed that
/// exact string back to every command that takes a pane, and each must act on
/// THAT pane.
#[test]
fn short_pane_id_from_pane_list_is_accepted_by_every_pane_command() {
    let h = Harness::new();
    let f = fixture(&h, "roundtrip");

    // Take the id the way a human would: read pane B's row off `pane list`.
    let listed = h.cli(&["--format", "plain", "pane", "list", "-s", &f.name]);
    assert!(
        listed.status.success(),
        "pane list failed: {}",
        stderr_of(&listed)
    );
    let printed = stdout_of(&listed);
    let shorts: Vec<&str> = printed
        .lines()
        .filter_map(|l| l.split('\t').next())
        .collect();
    assert!(
        shorts.contains(&f.short_pane()) && shorts.contains(&&f.pane_a[..8]),
        "pane list must print the 8-char short id of BOTH panes; got {shorts:?}"
    );
    let short = f.short_pane().to_string();

    // Every non-destructive verb that takes a pane. This list is exhaustive
    // over `PaneCommand`'s pane-taking variants minus the destructive ones,
    // which are covered separately — a verb missing here is a verb whose
    // prefix support nothing checks.
    let cases: Vec<Vec<&str>> = vec![
        vec!["pane", "glance", &short, "--text-only"],
        vec![
            "pane",
            "wait-settled",
            &short,
            "--quiet",
            "100",
            "--timeout",
            "5000",
        ],
        vec!["pane", "checkpoint", &short],
        vec!["pane", "capture", "-s", &f.name, "-p", &short],
        vec![
            "pane",
            "wait-for",
            "-s",
            &f.name,
            "-p",
            &short,
            "--text",
            "BRAVO",
            "--timeout-ms",
            "5000",
        ],
        vec!["pane", "title", "-s", &f.name, "-p", &short, "-t", "marker"],
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
        vec![
            "pane", "set-size", "-s", &f.name, "-p", &short, "--cols", "80", "--rows", "24",
        ],
        vec!["pane", "zoom", "-s", &f.name, "-p", &short],
        vec!["pane", "zoom", "-s", &f.name, "-p", &short],
        vec![
            "pane",
            "resize",
            "-s",
            &f.name,
            "-p",
            &short,
            "--direction",
            "horizontal",
            "--delta",
            "1",
        ],
        vec![
            "pane",
            "run",
            "-s",
            &f.name,
            "-p",
            &short,
            "--command",
            "true",
        ],
    ];

    for args in cases {
        let out = h.cli(&args);
        assert!(
            out.status.success(),
            "`shux {}` rejected the short id `{short}` that `pane list` printed\n\
             stdout: {}\nstderr: {}",
            args.join(" "),
            stdout_of(&out),
            stderr_of(&out),
        );
    }

    // The mutation must have landed on B and NOWHERE else. Without this, a
    // resolver that discarded `-p` and used the active pane would pass every
    // assertion above.
    assert_eq!(
        pane_title(&h, &f, &f.pane_b),
        "marker",
        "`pane title -p <B's short id>` did not retitle B"
    );
    assert_ne!(
        pane_title(&h, &f, &f.pane_a),
        "marker",
        "`pane title -p <B's short id>` retitled the WRONG pane"
    );

    // `pane watch` is a STREAM, so it cannot be run like the one-shot verbs
    // above: with no `--limit` it never returns. Drive it properly — start the
    // watcher on the short id, make the pane speak, and require those bytes to
    // come back out. That proves the id resolved AND that the stream is wired
    // to the pane it named.
    //
    // Deliberately NOT `--limit 1`: the pane may still be flushing output from
    // the verbs above, so the first chunk to arrive is whatever was already in
    // flight. Watch until the marker shows up, then stop.
    {
        h.rpc_ok(
            "pane.wait_settled",
            serde_json::json!({ "pane_id": f.pane_b, "quiet_ms": 300, "timeout_ms": 5000 }),
        );
        let log = h.state_dir().join("watch.out");
        let sink = std::fs::File::create(&log).expect("create watch log");
        let mut child = h
            .shux()
            .args([
                "pane",
                "watch",
                "-s",
                &f.name,
                "-p",
                &short,
                "--timeout-ms",
                "1000",
            ])
            .stdout(std::process::Stdio::from(sink))
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn pane watch");

        // Give the watcher a moment to subscribe — the data plane keeps no
        // history, so anything emitted before it attaches is simply gone.
        std::thread::sleep(std::time::Duration::from_millis(600));
        h.rpc_ok(
            "pane.send_keys",
            serde_json::json!({ "pane_id": f.pane_b, "text": "printf 'WATCHED\n'
" }),
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut seen = String::new();
        while std::time::Instant::now() < deadline {
            seen = std::fs::read_to_string(&log).unwrap_or_default();
            if seen.contains("WATCHED") {
                break;
            }
            // A watcher that died is a failure, not something to wait out.
            if let Some(st) = child.try_wait().expect("try_wait") {
                panic!("`pane watch` exited early ({st}) — output so far: {seen:?}");
            }
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
        let _ = child.kill();
        let _ = child.wait();
        assert!(
            seen.contains("WATCHED"),
            "`pane watch` on the short id `{short}` never delivered the pane's \
             output; saw: {seen:?}"
        );
    }

    // `pane record` writes a file — assert the recorder reported completion.
    let rec = h.state_dir().join("short.rec");
    let rec_arg = rec.to_string_lossy().to_string();
    let out = h.cli(&[
        "pane",
        "record",
        "-s",
        &f.name,
        "-p",
        &short,
        "--to",
        &rec_arg,
        "--duration-ms",
        "400",
    ]);
    assert!(
        out.status.success(),
        "`pane record` rejected the short id: {}",
        stderr_of(&out)
    );
    assert!(rec.exists(), "pane record wrote no file");

    // `pane snapshot` writes a PNG — assert on its CONTENT. A blank grid is
    // still tens of kilobytes, so a size check proves nothing.
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
    let lit = non_background_pixels(&bytes);
    assert!(
        lit > 500,
        "snapshot of a live pane has only {lit} non-background pixels — it is blank"
    );

    h.kill_session(&f.session_id);
}

/// Resolving must not merely stop erroring — the short id and the full UUID
/// must reach the SAME pane, and it must be the one named, not the active one.
#[test]
fn short_and_full_pane_ids_address_the_same_pane_and_not_the_active_one() {
    let h = Harness::new();
    let f = fixture(&h, "same");

    let by_full = h.rpc_ok(
        "pane.glance",
        serde_json::json!({ "pane_id": f.pane_b, "include_png": false }),
    );
    let by_short = h.rpc_ok(
        "pane.glance",
        serde_json::json!({ "pane_id": f.short_pane(), "include_png": false }),
    );
    let other = h.rpc_ok(
        "pane.glance",
        serde_json::json!({ "pane_id": f.pane_a, "include_png": false }),
    );

    let short_text = by_short["text"].as_str().expect("glance text").to_string();
    assert_eq!(
        by_full["text"], by_short["text"],
        "short id resolved to a different pane than the full uuid"
    );
    assert!(
        short_text.contains("BRAVO"),
        "the short id did not land on pane B; screen was: {short_text}"
    );
    assert!(
        !short_text.contains("ALPHA"),
        "the short id landed on pane A — the fallback, not the pane named"
    );
    assert!(
        other["text"]
            .as_str()
            .expect("glance text")
            .contains("TRUECOLOR"),
        "pane A lost its colour probe — the fixture is not what the test assumes"
    );

    h.kill_session(&f.session_id);
}

/// `session list` prints a short session id too, and every `-s` takes it —
/// including the session verbs that end the documented loop.
#[test]
fn short_session_id_from_session_list_is_accepted_by_every_session_command() {
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
        stdout_of(&out).contains(&f.pane_a),
        "short session id resolved to the wrong session"
    );

    // `session save` and `session rename` take `-s` too. They were the two
    // verbs that silently did not.
    let out = h.cli(&["session", "save", "-s", short]);
    assert!(
        out.status.success(),
        "`session save -s <short id>` was rejected: {}",
        stderr_of(&out)
    );
    assert!(
        stdout_of(&out).contains(&f.name),
        "session save exported the wrong session: {}",
        stdout_of(&out)
    );

    let renamed = format!("{}-renamed", f.name);
    let out = h.cli(&["session", "rename", "-s", short, "-n", &renamed]);
    assert!(
        out.status.success(),
        "`session rename -s <short id>` was rejected: {}",
        stderr_of(&out)
    );
    let after = h.rpc_ok("session.list", serde_json::json!({}));
    let found = after["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .find(|s| s["id"].as_str() == Some(f.session_id.as_str()))
        .expect("session vanished");
    assert_eq!(
        found["name"].as_str(),
        Some(renamed.as_str()),
        "rename by short id renamed the wrong session"
    );

    // …and `session kill`, the loop's last step.
    let out = h.cli(&["session", "kill", short]);
    assert!(
        out.status.success(),
        "`session kill <short id>` was rejected: {}",
        stderr_of(&out)
    );
    let after = h.rpc_ok("session.list", serde_json::json!({}));
    assert!(
        !after["sessions"]
            .as_array()
            .expect("sessions")
            .iter()
            .any(|s| s["id"].as_str() == Some(f.session_id.as_str())),
        "session kill by short id left the session alive"
    );
}

/// An exact session NAME must still beat an id prefix, even when the name is
/// itself a valid hex prefix of a DIFFERENT session's id.
#[test]
fn an_exact_session_name_beats_an_id_prefix() {
    let h = Harness::new();
    let f = fixture(&h, "prec");

    // A second session literally NAMED the first session's short id.
    let impostor = f.short_session().to_string();
    let out = h.cli(&[
        "--format",
        "json",
        "session",
        "create",
        &impostor,
        "-d",
        "--",
        "sh",
        "-c",
        "printf 'IMPOSTOR\\n'; sleep 300",
    ]);
    assert!(
        out.status.success(),
        "could not create the impostor session: {}",
        stderr_of(&out)
    );

    let listed = h.cli(&["--format", "json", "pane", "list", "-s", &impostor]);
    assert!(out.status.success(), "{}", stderr_of(&listed));
    assert!(
        !stdout_of(&listed).contains(&f.pane_a),
        "the exact NAME lost to an id prefix — `-s {impostor}` hit the other session"
    );

    // Killing by that string must kill the NAMED session, not the one whose
    // id starts with it.
    let out = h.cli(&["session", "kill", &impostor]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let after = h.rpc_ok("session.list", serde_json::json!({}));
    assert!(
        after["sessions"]
            .as_array()
            .expect("sessions")
            .iter()
            .any(|s| s["id"].as_str() == Some(f.session_id.as_str())),
        "`session kill <name>` killed the session whose ID started with that name"
    );

    h.kill_session(&f.session_id);
}

/// `window list` must print an id, and `--window` must accept it — full UUID
/// and the printed short form both — while a numeric index still wins.
#[test]
fn window_list_prints_an_id_that_window_commands_accept() {
    let h = Harness::new();
    let f = fixture(&h, "win");

    let listed = h.cli(&["--format", "plain", "window", "list", "-s", &f.name]);
    let printed = stdout_of(&listed);
    assert!(
        printed.lines().any(|l| l.contains(f.short_window())),
        "window list must print the window's short id; got {printed:?}"
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
            stdout_of(&out).contains(&f.pane_a),
            "`--window {spec}` resolved to the wrong window"
        );
    }

    // The numeric index must keep winning, and with two windows present this
    // can actually tell index-resolution apart from id-resolution.
    let out = h.cli(&["--format", "json", "pane", "list", "-s", &f.name, "-w", "0"]);
    assert!(
        out.status.success(),
        "`-w 0` regressed: {}",
        stderr_of(&out)
    );
    assert!(
        stdout_of(&out).contains(&f.pane_a),
        "`-w 0` did not select window 0"
    );
    let out = h.cli(&["--format", "json", "pane", "list", "-s", &f.name, "-w", "1"]);
    assert!(out.status.success(), "`-w 1` failed: {}", stderr_of(&out));
    assert!(
        !stdout_of(&out).contains(&f.pane_a),
        "`-w 1` returned window 0's panes"
    );

    h.kill_session(&f.session_id);
}

/// A partially-hyphenated paste is what you get from double-clicking half a
/// UUID. It must resolve like any other prefix.
#[test]
fn partial_hyphenated_and_uppercase_prefixes_resolve() {
    let h = Harness::new();
    let f = fixture(&h, "forms");

    // Settle first. The shell prints a fresh prompt after BRAVO, so a
    // reference frame grabbed too early differs from the ones the variants
    // grab a moment later — a flake that has nothing to do with id resolution.
    h.rpc_ok(
        "pane.wait_settled",
        serde_json::json!({ "pane_id": f.pane_b, "quiet_ms": 300, "timeout_ms": 5000 }),
    );
    let expected = h.rpc_ok(
        "pane.glance",
        serde_json::json!({ "pane_id": f.pane_b, "include_png": false }),
    );

    let variants = vec![
        f.pane_b[..13].to_string(),                  // "xxxxxxxx-xxxx"
        f.pane_b[..8].to_uppercase(),                // shouted short id
        f.pane_b.replace('-', "")[..12].to_string(), // hyphen-free prefix
        format!("  {}  ", f.short_pane()),           // pasted with space
        format!("{}\n", f.short_pane()),             // pasted with a newline
    ];

    for v in variants {
        let got = h.rpc_raw(
            "pane.glance",
            serde_json::json!({ "pane_id": v, "include_png": false }),
        );
        let got = got.expect_result(&format!("glance via `{v}`"));
        assert_eq!(
            got["text"], expected["text"],
            "`{v}` did not resolve to pane B"
        );
    }

    h.kill_session(&f.session_id);
}

/// An id parameter that is PRESENT but not a string is a caller mistake, not
/// an invitation to act on whatever is active. Without this, `{"pane_id": 123}`
/// silently zoomed, resized or typed into a pane nobody named.
#[test]
fn a_wrongly_typed_id_parameter_never_falls_back_to_the_active_pane() {
    let h = Harness::new();
    let f = fixture(&h, "types");

    let before = pane_title(&h, &f, &f.pane_a);

    for bad in [
        serde_json::json!(12345),
        serde_json::json!(true),
        serde_json::json!(["abcd1234"]),
        serde_json::json!({ "id": "abcd1234" }),
    ] {
        for method in ["pane.glance", "pane.zoom", "pane.set_title"] {
            let env = h.rpc_raw(
                method,
                serde_json::json!({
                    "pane_id": bad, "session_id": f.session_id, "title": "hijacked"
                }),
            );
            env.expect_error_code(
                -32602,
                &format!("{method} with pane_id={bad} must be invalid_params"),
            );
        }
    }

    // A wrong-typed window_id must not silently become the active window.
    h.rpc_raw(
        "pane.glance",
        serde_json::json!({ "window_id": 7, "session_id": f.session_id }),
    )
    .expect_error_code(-32602, "numeric window_id must be invalid_params");

    // Nothing was mutated by any of the above.
    assert_eq!(
        pane_title(&h, &f, &f.pane_a),
        before,
        "a wrong-typed pane_id mutated the active pane"
    );

    // An ABSENT id still means "use the active one" — that is the behaviour
    // the strictness must not break.
    h.rpc_raw(
        "pane.glance",
        serde_json::json!({ "session_id": f.session_id, "include_png": false }),
    )
    .expect_result("an omitted pane_id must still fall back to the active pane");
    // …and so does an explicit null.
    h.rpc_raw(
        "pane.glance",
        serde_json::json!({
            "pane_id": serde_json::Value::Null,
            "session_id": f.session_id,
            "include_png": false
        }),
    )
    .expect_result("an explicitly null pane_id means 'unset'");

    h.kill_session(&f.session_id);
}

/// Garbage must still be `invalid_params`, and a well-formed-but-unknown
/// prefix must be `not_found` — a resolver that guesses is worse than one that
/// refuses.
#[test]
fn malformed_and_unmatched_refs_keep_their_distinct_errors() {
    let h = Harness::new();
    let f = fixture(&h, "errs");

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

    for (bad, why) in [
        ("zzzzzzzz", "non-hex"),
        ("", "empty"),
        ("----", "only hyphens"),
        ("00000000000000000000000000000000000000", "over-long"),
    ] {
        h.rpc_raw("pane.glance", serde_json::json!({ "pane_id": bad }))
            .expect_error_code(-32602, &format!("{why} ref must be invalid_params"));
    }

    // A whole uuid's worth of hex with the hyphens misplaced is a malformed
    // uuid, not a 32-character prefix.
    let misplaced = format!("{}-{}", &f.pane_b[..10], f.pane_b[10..].replace('-', ""));
    h.rpc_raw(
        "pane.glance",
        serde_json::json!({ "pane_id": misplaced.replace("--", "-") }),
    )
    .expect_error_code(
        -32602,
        "32 hex with misplaced hyphens must be invalid_params",
    );

    // Well-formed prefix, nothing matches it.
    let live = f.pane_b.replace('-', "");
    let orphan = if live.starts_with("dead") {
        "beef"
    } else {
        "dead"
    };
    h.rpc_raw("pane.glance", serde_json::json!({ "pane_id": orphan }))
        .expect_error_code(-32004, "unmatched prefix must be not_found");

    // Regression guard: a syntactically perfect UUID that does not exist stays
    // not_found, exactly as before the fix.
    h.rpc_raw(
        "pane.glance",
        serde_json::json!({ "pane_id": "00000000-0000-4000-8000-000000000001" }),
    )
    .expect_error_code(-32004, "unknown full UUID must stay not_found");

    h.kill_session(&f.session_id);
}

/// The CLI's exit codes for the lens verbs are part of the contract: 2 for a
/// malformed reference, 3 for one that resolves to nothing — and the message
/// has to say which.
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
    // characters has to be told how many are needed, and which parameter is at
    // fault — "invalid_params (code -32602)" says neither.
    let msg = stderr_of(&bad);
    assert!(
        msg.contains("pane_id") && msg.contains('4'),
        "the error must name the parameter and the minimum length: {msg}"
    );

    let live = f.pane_b.replace('-', "");
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
    assert!(
        stderr_of(&unknown).contains(orphan),
        "the not-found error must quote what was asked for: {}",
        stderr_of(&unknown)
    );

    h.kill_session(&f.session_id);
}

/// Destructive verbs last: killing a pane by its short id must kill the pane
/// that id names, and nothing else.
#[test]
fn destructive_pane_verbs_accept_the_short_id() {
    let h = Harness::new();
    let f = fixture(&h, "kill");

    let out = h.cli(&["pane", "focus", "-s", &f.name, "-p", f.short_pane()]);
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
        &f.pane_a[..8],
    ]);
    assert!(
        out.status.success(),
        "swap by short id: {}",
        stderr_of(&out)
    );

    let out = h.cli(&["pane", "kill", "-s", &f.name, "-p", f.short_pane()]);
    assert!(
        out.status.success(),
        "kill by short id: {}",
        stderr_of(&out)
    );

    let ids: Vec<String> = all_panes(&h, &f)
        .iter()
        .map(|p| p["id"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        !ids.contains(&f.pane_b),
        "the pane named by the short id is still listed"
    );
    assert!(
        ids.contains(&f.pane_a),
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

    for (args, needle) in [
        (vec!["pane", "list", "-s", "nosuchsession"], "nosuchsession"),
        (
            vec!["pane", "list", "-s", &f.name, "-w", "nosuchwindow"],
            "nosuchwindow",
        ),
        // A syntactically valid session id that names nothing must blame the
        // SESSION, not window resolution.
        (
            vec!["pane", "list", "-s", "11111111-2222-4333-8444-555555555555"],
            "11111111",
        ),
    ] {
        let out = h.cli(&args);
        let msg = stderr_of(&out);
        assert!(
            msg.contains(needle),
            "`shux {}` must quote `{needle}` in its error; got {msg}",
            args.join(" ")
        );
    }

    // A pane id that names nothing must not double-wrap into
    // `pane 'pane not found: <uuid>' not found`.
    let out = h.cli(&[
        "pane",
        "kill",
        "-s",
        &f.name,
        "-p",
        "11111111-2222-4333-8444-555555555555",
    ]);
    let msg = stderr_of(&out);
    assert!(
        !msg.contains("'pane not found"),
        "the not-found message is wrapped inside itself: {msg}"
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

/// `session attach` resolves its argument like every other verb, and — this is
/// the dangerous part — creates a session when it matches nothing. Feed it the
/// short id `session list` prints and it must ATTACH, never mint a blank
/// session named after the id.
///
/// Attaching needs a terminal, which the test harness has not got, so the
/// attach itself fails at raw-mode setup. That is fine: the defect is what
/// happens BEFORE that, and it is visible in `session list`.
#[test]
fn session_attach_resolves_an_id_instead_of_creating_a_session_named_after_it() {
    let h = Harness::new();
    let f = fixture(&h, "attach");

    let before = h.rpc_ok("session.list", serde_json::json!({}));
    let before_n = before["sessions"].as_array().expect("sessions").len();

    // Fails on raw mode; we only care that it did not create anything.
    let _ = h.cli(&["session", "attach", f.short_session()]);

    let after = h.rpc_ok("session.list", serde_json::json!({}));
    let names: Vec<String> = after["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .map(|s| s["name"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        !names.iter().any(|n| n == f.short_session()),
        "`session attach <short id>` created a blank session NAMED after the \
         id instead of attaching to it; sessions are now {names:?}"
    );
    assert_eq!(
        after["sessions"].as_array().expect("sessions").len(),
        before_n,
        "`session attach <short id>` changed the session count"
    );

    // The same argument by full uuid must behave identically.
    let _ = h.cli(&["session", "attach", &f.session_id]);
    let after = h.rpc_ok("session.list", serde_json::json!({}));
    assert_eq!(
        after["sessions"].as_array().expect("sessions").len(),
        before_n,
        "`session attach <full uuid>` created a session"
    );

    // The complementary case — a genuinely new NAME must still CREATE — is
    // not asserted here on purpose. It depends on how far the attach path
    // gets before it fails on the missing terminal, which differs between a
    // developer's machine and a CI runner. It is pinned deterministically in
    // `attach::tests::attach_resolves_an_id_before_it_creates`, which calls
    // the resolver directly.

    h.kill_session(&f.session_id);
}
