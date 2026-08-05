//! Security regression tests for issue #104 — terminal escape injection
//! through window titles.
//!
//! Window titles used to be stored raw and printed raw. A template from an
//! untrusted repo could therefore carry an OSC set-title payload:
//!
//! ```toml
//! title = "]0;attacker-controlleddeploy"
//! ```
//!
//! TOML forbids a raw control byte inside a basic string, but its own
//! `\uXXXX` escape decodes to one before shux ever sees the value — so a
//! fixture built from raw bytes fails to parse and makes the vector look
//! imaginary. Every hostile fixture here is built the way an attacker would.
//!
//! These tests drive the REAL `shux` binary against a REAL daemon in an
//! isolated `XDG_RUNTIME_DIR`, and scan every byte the CLI writes to stdout
//! and stderr. The assertion is deliberately blunt: **no C0, DEL or C1 byte
//! may appear in terminal-facing output**, whatever the input was.

use std::path::PathBuf;
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

/// The reported payload: ESC ] 0 ; … BEL retitles the operator's terminal.
const OSC_PAYLOAD: &str = "\u{1b}]0;PWNED\u{7}deploy";
/// What survives `sanitize_title`: control bytes gone, OSC syntax inert text.
const OSC_SANITIZED: &str = "]0;PWNEDdeploy";

// ── isolated daemon environment ─────────────────────────────────────────

struct Env {
    bin: PathBuf,
    root: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("temp root");
        for sub in ["runtime", "config/shux", "work"] {
            std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
        }
        Self {
            bin: PathBuf::from(env!("CARGO_BIN_EXE_shux")),
            root,
        }
    }

    fn runtime(&self) -> PathBuf {
        self.root.path().join("runtime")
    }
    fn work(&self) -> PathBuf {
        self.root.path().join("work")
    }

    fn shux(&self) -> Command {
        let mut cmd = Command::new(&self.bin);
        cmd.env("XDG_RUNTIME_DIR", self.runtime())
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("NO_COLOR", "1")
            .env("SHELL", "/bin/sh")
            .current_dir(self.work());
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.shux().args(args).output().expect("spawn shux")
    }

    /// Write a template and apply it. Returns the `state apply` output.
    fn apply(&self, name: &str, body: &str) -> Output {
        let path = self.work().join(format!("{name}.toml"));
        std::fs::write(&path, body).expect("write template");
        self.run(&["state", "apply", path.to_str().unwrap()])
    }

    fn daemon_pid(&self) -> Option<u32> {
        std::fs::read_to_string(self.runtime().join("shux/shux.pid"))
            .ok()?
            .trim()
            .parse()
            .ok()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        if let Some(pid) = self.daemon_pid() {
            let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if kill(Pid::from_raw(pid as i32), None).is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

// ── the byte scanner ────────────────────────────────────────────────────

/// Bytes that must never reach a terminal from shux's own output.
///
/// C0 minus the two an operator's terminal treats as ordinary layout (`\n`,
/// `\t`), plus DEL. C1 (0x80..=0x9F) cannot appear as a standalone byte in
/// valid UTF-8 output, so it is caught at the `char` level below instead.
fn offending_bytes(buf: &[u8]) -> Vec<(usize, u8)> {
    buf.iter()
        .enumerate()
        .filter(|(_, b)| matches!(b, 0x00..=0x08 | 0x0b..=0x1f | 0x7f))
        .map(|(i, b)| (i, *b))
        .collect()
}

/// C1 controls and the separator/bidi-override class, at the character level.
fn offending_chars(text: &str) -> Vec<(usize, char)> {
    text.char_indices()
        .filter(|(_, c)| {
            matches!(c, '\u{80}'..='\u{9f}')
                || matches!(c, '\u{2028}' | '\u{2029}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .collect()
}

/// Assert that neither stream of a command carries an active control
/// sequence. Reports the offending offset and a hexdump-ish window so a
/// failure names the byte instead of just "output differed".
#[track_caller]
fn assert_output_inert(label: &str, out: &Output) {
    for (stream, buf) in [("stdout", &out.stdout), ("stderr", &out.stderr)] {
        let bad = offending_bytes(buf);
        assert!(
            bad.is_empty(),
            "{label}: {stream} carries {} control byte(s), first at offset {} (0x{:02x}).\n\
             ---\n{}\n---",
            bad.len(),
            bad[0].0,
            bad[0].1,
            String::from_utf8_lossy(buf).escape_debug(),
        );
        let text = String::from_utf8_lossy(buf);
        let bad = offending_chars(&text);
        assert!(
            bad.is_empty(),
            "{label}: {stream} carries {} hostile char(s), first U+{:04X} at byte {}.\n\
             ---\n{}\n---",
            bad.len(),
            bad[0].1 as u32,
            bad[0].0,
            text.escape_debug(),
        );
    }
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn combined(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// A template carrying `payload` as the title of its single window.
fn hostile_template(session: &str, payload_escape: &str) -> String {
    format!(
        "[session]\nname = \"{session}\"\n\n\
         [[windows]]\ntitle = \"{payload_escape}\"\n\n\
         [[windows.panes]]\ncommand = [\"sh\"]\n"
    )
}

// ── the reported vector, end to end ─────────────────────────────────────

/// Issue #104's exact recipe: `shux state apply` a template whose window
/// title embeds an OSC set-title payload, then list the windows.
#[test]
fn hostile_template_title_never_reaches_the_terminal() {
    let env = Env::new();

    let applied = env.apply(
        "evil",
        &hostile_template("ev", "\\u001B]0;PWNED\\u0007deploy"),
    );
    assert!(
        applied.status.success(),
        "apply failed: {}",
        combined(&applied)
    );
    assert_output_inert("state apply", &applied);

    for args in [
        &["window", "list", "-s", "ev"][..],
        &["--format", "plain", "window", "list", "-s", "ev"][..],
        &["--format", "json", "window", "list", "-s", "ev"][..],
    ] {
        let out = env.run(args);
        assert_output_inert(&format!("{args:?}"), &out);
    }

    // Strip, don't reject: the printable remainder is still shown, so the
    // operator can see what the template asked for.
    let listed = stdout_of(&env.run(&["--format", "plain", "window", "list", "-s", "ev"]));
    assert!(
        listed.contains(OSC_SANITIZED),
        "sanitised title should still be displayed, got: {listed:?}"
    );

    // And the daemon stored the sanitised form, not just printed it.
    let json = stdout_of(&env.run(&["--format", "json", "window", "list", "-s", "ev"]));
    let v: serde_json::Value = serde_json::from_str(&json).expect("json");
    assert_eq!(v[0]["title"], OSC_SANITIZED);
}

/// A second window in the same template — the `CreateWindow` op path rather
/// than the `CreateSession { initial_window_title }` path.
#[test]
fn hostile_template_second_window_title_is_sanitized() {
    let env = Env::new();
    let body = "[session]\nname = \"ev2\"\n\n\
                [[windows]]\ntitle = \"first\"\n\n\
                [[windows.panes]]\ncommand = [\"sh\"]\n\n\
                [[windows]]\ntitle = \"\\u001B]0;PWNED\\u0007build\"\n\n\
                [[windows.panes]]\ncommand = [\"sh\"]\n";
    let applied = env.apply("evil2", body);
    assert!(applied.status.success(), "{}", combined(&applied));

    let out = env.run(&["--format", "json", "window", "list", "-s", "ev2"]);
    assert_output_inert("window list json", &out);
    let v: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("json");
    let titles: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["title"].as_str().unwrap())
        .collect();
    assert!(titles.contains(&"]0;PWNEDbuild"), "{titles:?}");
}

// ── the CLI paths ───────────────────────────────────────────────────────

/// `window create --name` echoes a confirmation containing the title.
#[test]
fn window_create_with_escape_argument_prints_inert_confirmation() {
    let env = Env::new();
    env.apply("base", &hostile_template("cw", "safe"));

    let out = env
        .shux()
        .args(["window", "create", "-s", "cw", "--name"])
        .arg(OSC_PAYLOAD)
        .output()
        .expect("spawn");
    assert!(out.status.success(), "{}", combined(&out));
    assert_output_inert("window create", &out);
    assert!(
        stdout_of(&out).contains(OSC_SANITIZED),
        "got: {:?}",
        stdout_of(&out)
    );
}

/// `window rename --name` used to echo the client's raw argument, so even a
/// fixed daemon would have been replayed through the operator's terminal.
#[test]
fn window_rename_with_escape_argument_prints_inert_confirmation() {
    let env = Env::new();
    env.apply("base", &hostile_template("wr", "original"));

    let out = env
        .shux()
        .args(["window", "rename", "-s", "wr", "-w", "0", "--name"])
        .arg(OSC_PAYLOAD)
        .output()
        .expect("spawn");
    assert!(out.status.success(), "{}", combined(&out));
    assert_output_inert("window rename", &out);
    // The confirmation must report what was actually stored, not the request.
    assert!(
        stdout_of(&out).contains(OSC_SANITIZED),
        "confirmation should show the stored title, got: {:?}",
        stdout_of(&out)
    );

    let json = stdout_of(&env.run(&["--format", "json", "window", "list", "-s", "wr"]));
    let v: serde_json::Value = serde_json::from_str(&json).expect("json");
    assert_eq!(v[0]["title"], OSC_SANITIZED);
}

/// `window focus` prints the stored title back.
#[test]
fn window_focus_prints_inert_title() {
    let env = Env::new();
    env.apply(
        "base",
        &hostile_template("wf", "\\u001B]0;PWNED\\u0007deploy"),
    );
    let out = env.run(&["window", "focus", "-s", "wf", "-w", "0"]);
    assert_output_inert("window focus", &out);
}

// ── sanitize-then-validate ordering ─────────────────────────────────────

/// A title made entirely of control bytes sanitises to empty. It must be
/// rejected by the existing name validation, never stored as `""` — the
/// ordering bug the issue calls out.
#[test]
fn title_that_sanitizes_to_empty_is_rejected_not_stored() {
    let env = Env::new();
    env.apply("base", &hostile_template("empt", "keepme"));

    for hostile in ["\u{1b}\u{7}", "\u{9b}", "\u{202e}\u{2028}"] {
        let out = env
            .shux()
            .args(["window", "rename", "-s", "empt", "-w", "0", "--name"])
            .arg(hostile)
            .output()
            .expect("spawn");
        assert!(
            !out.status.success(),
            "rename to {hostile:?} should fail, got: {}",
            combined(&out)
        );
        assert_output_inert("rejected rename", &out);
    }

    // The window kept its original title through every rejection.
    let json = stdout_of(&env.run(&["--format", "json", "window", "list", "-s", "empt"]));
    let v: serde_json::Value = serde_json::from_str(&json).expect("json");
    assert_eq!(v[0]["title"], "keepme");
}

/// The same rule on the template path: a batch carrying a control-only title
/// is rejected atomically, leaving no half-built session behind.
#[test]
fn template_with_control_only_title_is_rejected_atomically() {
    let env = Env::new();
    let out = env.apply("ctl", &hostile_template("ctl", "\\u001B\\u0007"));
    assert!(
        !out.status.success(),
        "apply should fail, got: {}",
        combined(&out)
    );
    assert_output_inert("rejected apply", &out);

    let sessions = stdout_of(&env.run(&["--format", "json", "session", "list"]));
    assert!(
        !sessions.contains("\"ctl\""),
        "rejected batch left a session behind: {sessions}"
    );
}

// ── egress: rejected input is echoed, and must be escaped ───────────────

/// A hostile `[session] name` is rejected by the allowlist — and the
/// rejection message replays the payload. Rejected input never meets a
/// sanitiser, so the message itself has to be escaped.
#[test]
fn rejected_session_name_is_echoed_escaped() {
    let env = Env::new();
    let out = env.apply("sess", &hostile_template("\\u001B]0;PWNED\\u0007evil", "w"));
    assert!(!out.status.success(), "{}", combined(&out));
    assert_output_inert("rejected session name", &out);
    // Visible-but-inert: the operator still learns what was in the template.
    assert!(
        combined(&out).contains("\\u{1b}"),
        "payload should be shown escaped, got: {}",
        combined(&out).escape_debug()
    );
}

#[test]
fn rejected_session_name_via_cli_is_echoed_escaped() {
    let env = Env::new();
    let out = env
        .shux()
        .args(["session", "create"])
        .arg(OSC_PAYLOAD)
        .output()
        .expect("spawn");
    assert!(!out.status.success(), "{}", combined(&out));
    assert_output_inert("session create rejection", &out);
}

// ── lookup must normalise the same way storage does ─────────────────────

/// `window.ensure` is idempotent by name. Titles are stored sanitised, so a
/// lookup by the RAW name must resolve to the window that name created —
/// otherwise every `ensure` misses and stacks up another window with an
/// identical displayed title.
#[test]
fn window_ensure_is_idempotent_for_a_hostile_name() {
    let env = Env::new();
    env.apply("base", &hostile_template("ens", "seed"));

    let sessions = stdout_of(&env.run(&["--format", "json", "session", "list"]));
    let sid =
        serde_json::from_str::<serde_json::Value>(&sessions).expect("json")["sessions"][0]["id"]
            .as_str()
            .expect("session id")
            .to_string();

    let params = serde_json::json!({ "session_id": sid, "name": OSC_PAYLOAD }).to_string();
    for round in 0..3 {
        let out = env.run(&[
            "--format",
            "json",
            "rpc",
            "call",
            "window.ensure",
            "--params",
            &params,
        ]);
        assert_output_inert(&format!("window.ensure round {round}"), &out);
        let body: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("json");
        assert_eq!(body["result"]["title"], OSC_SANITIZED, "round {round}");
        assert_eq!(
            body["result"]["created"],
            round == 0,
            "round {round} should {} have created a window: {}",
            if round == 0 { "" } else { "NOT" },
            stdout_of(&out)
        );
    }

    let json = stdout_of(&env.run(&["--format", "json", "window", "list", "-s", "ens"]));
    let v: serde_json::Value = serde_json::from_str(&json).expect("json");
    let dupes = v
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["title"] == OSC_SANITIZED)
        .count();
    assert_eq!(dupes, 1, "ensure stacked duplicate windows: {json}");
}

/// `-w <name>` targets a window by title. The operator types the name they
/// see, but a script may pass the raw value from the template — both have to
/// land on the same window.
#[test]
fn window_selector_resolves_a_hostile_name() {
    let env = Env::new();
    let body = "[session]\nname = \"sel\"\n\n\
                [[windows]]\ntitle = \"first\"\n\n\
                [[windows.panes]]\ncommand = [\"sh\"]\n\n\
                [[windows]]\ntitle = \"\\u001B]0;PWNED\\u0007deploy\"\n\n\
                [[windows.panes]]\ncommand = [\"sh\"]\n";
    env.apply("sel", body);

    // By the sanitised name the operator actually sees.
    let out = env.run(&["window", "focus", "-s", "sel", "-w", OSC_SANITIZED]);
    assert!(out.status.success(), "{}", combined(&out));
    assert_output_inert("focus by sanitised name", &out);

    // And by the raw name a script would carry over from the template.
    let out = env
        .shux()
        .args(["window", "focus", "-s", "sel", "-w"])
        .arg(OSC_PAYLOAD)
        .output()
        .expect("spawn");
    assert!(
        out.status.success(),
        "raw selector should resolve to the same window: {}",
        combined(&out)
    );
    assert_output_inert("focus by raw name", &out);
}

/// A selector that matches nothing is echoed back in the error.
#[test]
fn unresolvable_hostile_selector_is_echoed_escaped() {
    let env = Env::new();
    env.apply("nf", &hostile_template("nf", "only"));
    let out = env
        .shux()
        .args(["window", "focus", "-s", "nf", "-w"])
        .arg("\u{1b}]0;PWNED\u{7}nosuchwindow")
        .output()
        .expect("spawn");
    assert!(!out.status.success(), "{}", combined(&out));
    assert_output_inert("unresolvable selector", &out);
}

// ── cross-path consistency ──────────────────────────────────────────────

/// Every read path must agree on the stored title: text, plain and JSON
/// window listings, plus the event stream `events.watch` subscribers see.
#[test]
fn all_read_paths_agree_on_the_sanitized_title() {
    let env = Env::new();
    env.apply(
        "xp",
        &hostile_template("xp", "\\u001B]0;PWNED\\u0007deploy"),
    );

    let json = stdout_of(&env.run(&["--format", "json", "window", "list", "-s", "xp"]));
    let v: serde_json::Value = serde_json::from_str(&json).expect("json");
    let stored = v[0]["title"].as_str().unwrap().to_string();
    assert_eq!(stored, OSC_SANITIZED);

    let plain = stdout_of(&env.run(&["--format", "plain", "window", "list", "-s", "xp"]));
    assert!(plain.contains(&stored), "plain: {plain:?}");

    let text = stdout_of(&env.run(&["window", "list", "-s", "xp"]));
    assert!(text.contains(&stored), "text: {text:?}");

    let history = env.run(&["events", "history"]);
    assert_output_inert("events history", &history);
    let hist = stdout_of(&history);
    assert!(
        hist.contains(OSC_SANITIZED),
        "event stream should carry the sanitised title: {hist}"
    );
    assert!(
        !hist.contains("\\u001b") && !hist.contains("\\u0007"),
        "event stream still carries the raw payload (JSON-escaped): {hist}"
    );
}

// ── breadth: every hostile class, through the real binary ───────────────

/// One template per hostile character class. Anything that survives to the
/// operator's terminal fails the byte scan.
#[test]
fn every_hostile_character_class_is_neutralised_end_to_end() {
    let env = Env::new();

    // (label, TOML escape, the char it decodes to)
    let cases: &[(&str, &str, char)] = &[
        ("NUL", "\\u0000", '\u{0}'),
        ("BEL", "\\u0007", '\u{7}'),
        ("BS", "\\u0008", '\u{8}'),
        ("TAB", "\\u0009", '\u{9}'),
        ("LF", "\\u000A", '\u{a}'),
        ("CR", "\\u000D", '\u{d}'),
        ("ESC", "\\u001B", '\u{1b}'),
        ("DEL", "\\u007F", '\u{7f}'),
        ("C1-PAD", "\\u0080", '\u{80}'),
        ("C1-CSI", "\\u009B", '\u{9b}'),
        ("C1-OSC", "\\u009D", '\u{9d}'),
        ("C1-APC", "\\u009F", '\u{9f}'),
        ("LINE-SEP", "\\u2028", '\u{2028}'),
        ("PARA-SEP", "\\u2029", '\u{2029}'),
        ("RLO", "\\u202E", '\u{202e}'),
        ("LRI", "\\u2066", '\u{2066}'),
        ("PDI", "\\u2069", '\u{2069}'),
    ];

    for (i, (label, escape, ch)) in cases.iter().enumerate() {
        let session = format!("cls{i}");
        let title = format!("A{escape}B");
        let applied = env.apply(&format!("cls{i}"), &hostile_template(&session, &title));
        assert!(
            applied.status.success(),
            "{label}: apply failed: {}",
            combined(&applied)
        );
        assert_output_inert(&format!("{label} apply"), &applied);

        for args in [
            vec!["window", "list", "-s", &session],
            vec!["--format", "plain", "window", "list", "-s", &session],
            vec!["--format", "json", "window", "list", "-s", &session],
        ] {
            let out = env.run(&args);
            assert_output_inert(&format!("{label} {args:?}"), &out);
        }

        let json = stdout_of(&env.run(&["--format", "json", "window", "list", "-s", &session]));
        let v: serde_json::Value = serde_json::from_str(&json).expect("json");
        let stored = v[0]["title"].as_str().unwrap();
        assert_eq!(stored, "AB", "{label}: stored {stored:?}");
        assert!(
            !stored.contains(*ch),
            "{label}: U+{:04X} survived into the graph",
            *ch as u32
        );
    }
}

/// A window title is a lookup key, so it is bounded by **rejection**, not
/// by truncation: silently cutting it would make two distinct requested
/// names resolve to one window.
#[test]
fn over_long_window_titles_are_rejected_not_truncated() {
    let env = Env::new();
    let out = env.apply("len", &hostile_template("len", &"x".repeat(200)));
    assert!(
        !out.status.success(),
        "a 200-char title should be refused: {}",
        combined(&out)
    );
    assert!(
        combined(&out).contains("too long"),
        "expected a length error, got: {}",
        combined(&out)
    );
    assert_output_inert("over-long rejection", &out);

    // At the bound, the title is stored WHOLE — no silent shortening.
    let at_bound = "y".repeat(128);
    let env2 = Env::new();
    let out = env2.apply("bound", &hostile_template("bound", &at_bound));
    assert!(out.status.success(), "{}", combined(&out));
    let json = stdout_of(&env2.run(&["--format", "json", "window", "list", "-s", "bound"]));
    let v: serde_json::Value = serde_json::from_str(&json).expect("json");
    assert_eq!(v[0]["title"], at_bound);
}

/// The strip runs BEFORE the length check, so an attacker cannot use
/// hostile filler to change how the bound is applied — and cannot push a
/// payload out of a window that no longer exists.
#[test]
fn hostile_filler_does_not_survive_or_shift_the_payload() {
    let env = Env::new();
    let filler = "\\u001B".repeat(100);
    env.apply("pad", &hostile_template("pad", &format!("{filler}MARKER")));
    let json = stdout_of(&env.run(&["--format", "json", "window", "list", "-s", "pad"]));
    let v: serde_json::Value = serde_json::from_str(&json).expect("json");
    assert_eq!(v[0]["title"], "MARKER");
}

/// Two titles that differ only past the old 64-char clamp must stay two
/// windows, and each name must resolve to its own. Truncating the lookup
/// key made `window rename -w <B>` silently rename window A.
#[test]
fn long_titles_differing_past_the_old_clamp_stay_distinct() {
    let env = Env::new();
    env.apply("sel", &hostile_template("sel", "seed"));
    let one = format!("{}-one", "B".repeat(100));
    let two = format!("{}-two", "B".repeat(100));

    for name in [&one, &two] {
        let out = env
            .shux()
            .args(["window", "create", "-s", "sel", "--name"])
            .arg(name)
            .output()
            .expect("spawn");
        assert!(out.status.success(), "{}", combined(&out));
    }

    let out = env
        .shux()
        .args(["window", "rename", "-s", "sel", "-w"])
        .arg(&two)
        .args(["--name", "RESOLVED"])
        .output()
        .expect("spawn");
    assert!(out.status.success(), "{}", combined(&out));

    let json = stdout_of(&env.run(&["--format", "json", "window", "list", "-s", "sel"]));
    let v: serde_json::Value = serde_json::from_str(&json).expect("json");
    let titles: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["title"].as_str().unwrap())
        .collect();
    assert!(
        titles.contains(&one.as_str()),
        "the '-one' window was renamed instead: {titles:?}"
    );
    assert!(titles.contains(&"RESOLVED"), "{titles:?}");
    assert!(!titles.contains(&two.as_str()), "{titles:?}");
}

/// `window.ensure` is idempotent BY NAME. Two long names that differ only
/// past the old clamp must not alias onto one window.
#[test]
fn ensure_does_not_alias_long_names() {
    let env = Env::new();
    env.apply("ens", &hostile_template("ens", "seed"));
    let sessions = stdout_of(&env.run(&["--format", "json", "session", "list"]));
    let sid =
        serde_json::from_str::<serde_json::Value>(&sessions).expect("json")["sessions"][0]["id"]
            .as_str()
            .expect("session id")
            .to_string();

    let alpha = format!("{}-alpha", "A".repeat(100));
    let beta = format!("{}-beta", "A".repeat(100));
    let out = env
        .shux()
        .args(["window", "create", "-s", "ens", "--name"])
        .arg(&alpha)
        .output()
        .expect("spawn");
    assert!(out.status.success(), "{}", combined(&out));

    let params = serde_json::json!({ "session_id": sid, "name": beta }).to_string();
    let out = env.run(&[
        "--format",
        "json",
        "rpc",
        "call",
        "window.ensure",
        "--params",
        &params,
    ]);
    let body: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("json");
    assert_eq!(
        body["result"]["created"],
        true,
        "ensure handed back the wrong window: {}",
        stdout_of(&out)
    );
    assert_eq!(body["result"]["title"], beta);
}

// ── egress paths the ingress sanitizer structurally cannot reach ────────

/// `cwd` and `command` are caller-supplied and legitimately arbitrary, so
/// they are never sanitized on the way in. `pane list` prints both.
#[test]
fn pane_list_never_prints_a_raw_cwd_or_command() {
    let env = Env::new();
    // JSON's own \u escapes decode to real control bytes before shux sees
    // them — the same trap TOML sets. A raw byte in the request is refused.
    let params = r#"{"name":"pl","cwd":"/tmp/\u001b]0;PWNED-CWD\u0007","command":["sh\u001b]0;PWNED-CMD\u0007","-c","sleep 30"]}"#;
    let out = env.run(&["rpc", "call", "session.create", "--params", params]);
    assert!(out.status.success(), "{}", combined(&out));

    for args in [
        &["pane", "list", "-s", "pl"][..],
        &["--format", "plain", "pane", "list", "-s", "pl"][..],
        &["--format", "json", "pane", "list", "-s", "pl"][..],
    ] {
        let out = env.run(args);
        assert_output_inert(&format!("{args:?}"), &out);
    }

    // The auto-derived pane title goes through the shared sanitizer too.
    let json = stdout_of(&env.run(&["--format", "json", "pane", "list", "-s", "pl"]));
    let v: serde_json::Value = serde_json::from_str(&json).expect("json");
    let title = v[0]["title"].as_str().unwrap();
    assert!(
        !title.chars().any(|c| c.is_control()),
        "auto title kept control bytes: {title:?}"
    );
}

/// A TOML parse error quotes the offending source line verbatim, and it
/// is printed before the daemon is ever contacted — no ingress sanitizer
/// can reach it.
#[test]
fn template_parse_errors_never_replay_the_source_line_raw() {
    let env = Env::new();
    let path = env.work().join("raw.toml");
    // A RAW ESC byte. TOML forbids it inside a basic string, which is
    // exactly why this is a parse error — and why the diagnostic quotes it.
    std::fs::write(
        &path,
        b"[session]\nname = \"raw\"\ntitle = \"\x1b]0;PWNED-RAW\x07\"\n".as_slice(),
    )
    .expect("write");

    let out = env.run(&["state", "apply", path.to_str().unwrap()]);
    assert!(!out.status.success(), "{}", combined(&out));
    assert_output_inert("toml parse error", &out);
    // The multi-line diagnostic layout survives — \n and \t are structure.
    assert!(
        combined(&out).lines().count() >= 3,
        "diagnostic layout was flattened: {}",
        combined(&out).escape_debug()
    );
    assert!(
        combined(&out).contains("\\u{1b}"),
        "payload should be shown escaped: {}",
        combined(&out).escape_debug()
    );
}

/// `--dry-run` is the advertised way to inspect an untrusted template. It
/// prints the ops BEFORE the graph sanitizes them, so it is the one place
/// a hostile title is meant to be shown verbatim — and must be inert.
#[test]
fn dry_run_output_is_inert_and_still_valid_json() {
    let env = Env::new();
    let path = env.work().join("dry.toml");
    std::fs::write(
        &path,
        "[session]\nname = \"dry\"\n\n[[windows]]\n\
         title = \"A\\u009BB\\u0085C\\u2028D\\u202EE\\u001BF\"\n\n\
         [[windows.panes]]\ncommand = [\"sh\"]\n",
    )
    .expect("write");

    let out = env.run(&["state", "apply", "--dry-run", path.to_str().unwrap()]);
    assert!(out.status.success(), "{}", combined(&out));
    assert_output_inert("dry-run", &out);
    let v: serde_json::Value =
        serde_json::from_str(&stdout_of(&out)).expect("dry-run must stay valid JSON");
    assert_eq!(v["ops"][0]["op"], "create_session");
}

/// Ordinary Unicode titles keep working — the sanitiser drops explicit
/// overrides, not scripts.
#[test]
fn legitimate_unicode_titles_are_untouched() {
    let env = Env::new();
    for (i, title) in ["日本語", "مرحبا", "build ✓", "agent-1 · deploy"]
        .iter()
        .enumerate()
    {
        let session = format!("uni{i}");
        let applied = env.apply(&format!("uni{i}"), &hostile_template(&session, title));
        assert!(applied.status.success(), "{}", combined(&applied));
        let json = stdout_of(&env.run(&["--format", "json", "window", "list", "-s", &session]));
        let v: serde_json::Value = serde_json::from_str(&json).expect("json");
        assert_eq!(v[0]["title"], *title, "mangled a legitimate title");
    }
}

/// The `window.rename` RPC is the same ingress as the CLI, driven directly
/// so a future CLI-only guard cannot be mistaken for a fix.
#[test]
fn window_rename_rpc_sanitizes_at_the_daemon() {
    let env = Env::new();
    env.apply("rpc", &hostile_template("rpc", "original"));

    let json = stdout_of(&env.run(&["--format", "json", "window", "list", "-s", "rpc"]));
    let v: serde_json::Value = serde_json::from_str(&json).expect("json");
    let wid = v[0]["id"].as_str().unwrap().to_string();

    let params = serde_json::json!({ "id": wid, "name": OSC_PAYLOAD }).to_string();
    let out = env.run(&[
        "--format",
        "json",
        "rpc",
        "call",
        "window.rename",
        "--params",
        &params,
    ]);
    assert_output_inert("window.rename rpc", &out);
    // `rpc call` prints the raw `{result|error}` envelope.
    let body: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("json");
    assert_eq!(
        body["result"]["title"],
        OSC_SANITIZED,
        "rpc response: {}",
        stdout_of(&out)
    );

    // And a control-only name is rejected at the RPC boundary too.
    let params = serde_json::json!({ "id": wid, "name": "\u{1b}\u{7}" }).to_string();
    let out = env.run(&[
        "--format",
        "json",
        "rpc",
        "call",
        "window.rename",
        "--params",
        &params,
    ]);
    assert_output_inert("window.rename rpc reject", &out);
    assert!(
        combined(&out).contains("error") || !out.status.success(),
        "control-only rename should be rejected: {}",
        combined(&out)
    );
}
