//! End-to-end CLI contract tests for issues #133, #136 and #137.
//!
//! All three are defects in what the *shipped binary* prints or accepts, so
//! every case here drives `CARGO_BIN_EXE_shux` against a real daemon in an
//! isolated `XDG_RUNTIME_DIR`. A unit test on the formatting helpers would
//! have passed throughout: #133 lives in the gap between `style::print_error`
//! and `main`'s `Termination` impl, #136 in the gap between clap's declared
//! range and the layout engine's clamp, #137 in two verbs that print the same
//! payload through different `println!`s.

use std::path::PathBuf;
use std::process::{Command, Output};

/// Keeps a pane alive past the assertion.
const PARK: &str = "exec sleep 900";

struct Env {
    bin: PathBuf,
    root: tempfile::TempDir,
    sessions: Vec<String>,
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
            sessions: Vec::new(),
        }
    }

    fn work(&self) -> PathBuf {
        self.root.path().join("work")
    }

    fn shux(&self) -> Command {
        let mut cmd = Command::new(&self.bin);
        cmd.env("XDG_RUNTIME_DIR", self.root.path().join("runtime"))
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env_remove("SHUX_SOCKET")
            .env_remove("RUST_BACKTRACE")
            .env("NO_COLOR", "1")
            .env("SHELL", "/bin/sh")
            .current_dir(self.work());
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.shux().args(args).output().expect("spawn shux")
    }

    #[track_caller]
    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{args:?} unexpectedly failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// Run expecting a non-zero exit; returns stderr.
    #[track_caller]
    fn fails(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            !out.status.success(),
            "{args:?} unexpectedly SUCCEEDED:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
        String::from_utf8_lossy(&out.stderr).to_string()
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let mut full = vec!["--format", "json"];
        full.extend_from_slice(args);
        let out = self.ok(&full);
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("not json ({e}): {out}"))
    }

    #[track_caller]
    fn session(&mut self, name: &str) -> String {
        self.ok(&["session", "create", name, "-d", "--cmd", PARK]);
        self.sessions.push(name.to_string());
        name.to_string()
    }

    fn pane_count(&self, session: &str) -> usize {
        self.json(&["pane", "list", "-s", session])
            .as_array()
            .map(|a| a.len())
            .unwrap_or(0)
    }

    fn write_template(&self, name: &str, body: &str) -> PathBuf {
        let p = self.work().join(name);
        std::fs::write(&p, body).expect("write template");
        p
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        for s in self.sessions.clone() {
            let _ = self.run(&["session", "kill", "-s", &s]);
        }
        let _ = self.run(&["daemon", "stop"]);
    }
}

// ── #133: every CLI error prints twice, the second time with a backtrace ────

#[test]
fn a_failing_command_prints_its_error_exactly_once() {
    let env = Env::new();
    let stderr = env.fails(&["session", "kill", "no-such-session"]);

    let hits = stderr.matches("not found").count();
    assert_eq!(
        hits, 1,
        "expected the error once, saw it {hits}x:\n---\n{stderr}\n---"
    );
    assert!(
        stderr.contains("no-such-session"),
        "the one line must still name the session:\n{stderr}"
    );
}

#[test]
fn a_failing_command_prints_no_backtrace_by_default() {
    let env = Env::new();
    let stderr = env.fails(&["session", "kill", "no-such-session"]);

    assert!(
        !stderr.contains("Stack backtrace"),
        "a user error must not carry a backtrace:\n---\n{stderr}\n---"
    );
    assert!(
        !stderr.contains("anyhow"),
        "dependency paths must not leak into a user error:\n---\n{stderr}\n---"
    );
    // anyhow's `Termination` impl is what adds the second copy; its prefix is
    // the cheapest proof that path is no longer reached.
    assert!(
        !stderr.contains("Error:"),
        "the raw `Error:` re-print must be gone:\n---\n{stderr}\n---"
    );
}

#[test]
fn a_backtrace_is_available_behind_rust_backtrace() {
    let env = Env::new();
    let out = env
        .shux()
        .args(["session", "kill", "no-such-session"])
        .env("RUST_BACKTRACE", "1")
        .output()
        .expect("spawn shux");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    assert!(!out.status.success(), "should still fail");
    assert!(
        stderr.contains("Stack backtrace") || stderr.contains("stack backtrace"),
        "RUST_BACKTRACE=1 is the documented opt-in; it must still produce one:\n---\n{stderr}\n---"
    );
    // Even opted in, the human line is printed once.
    assert_eq!(
        stderr.matches("✗").count(),
        1,
        "the human line stays single even with the opt-in:\n---\n{stderr}\n---"
    );
}

#[test]
fn several_distinct_error_paths_all_print_once() {
    let mut env = Env::new();
    let s = env.session("errpaths");

    for args in [
        vec!["session", "kill", "no-such-session"],
        vec!["pane", "list", "-s", "no-such-session"],
        vec!["pane", "capture", "-s", &s, "-p", "ffffffff"],
        vec!["window", "kill", "-s", &s, "-w", "no-such-window"],
    ] {
        let stderr = env.fails(&args);
        assert!(
            !stderr.contains("Stack backtrace") && !stderr.contains("Error:"),
            "{args:?} still double-prints:\n---\n{stderr}\n---"
        );
        assert_eq!(
            stderr.matches("✗").count(),
            1,
            "{args:?} printed the marker more than once:\n---\n{stderr}\n---"
        );
    }
}

// ── #136: `pane split --ratio 5.0` accepted silently ────────────────────────

#[test]
fn split_rejects_a_ratio_outside_the_documented_range() {
    let mut env = Env::new();
    let s = env.session("ratio-oob");
    let before = env.pane_count(&s);

    // `--ratio=V`, not `--ratio V`: clap treats a bare leading-dash token as a
    // flag and rejects it before any value parser runs, so the space form
    // would never reach the range check for negatives.
    for bad in ["5.0", "-1.0", "0.0", "1.0", "nan", "inf"] {
        let arg = format!("--ratio={bad}");
        let stderr = env.fails(&["pane", "split", "-s", &s, &arg]);
        assert!(
            stderr.contains("0.0") && stderr.contains("1.0"),
            "--ratio {bad} must be refused with the range named:\n---\n{stderr}\n---"
        );
        assert_eq!(
            env.pane_count(&s),
            before,
            "--ratio {bad} was refused but still created a pane"
        );
    }

    // The space form still has to fail, even though clap answers it first.
    for bad in ["5.0", "0.0"] {
        env.fails(&["pane", "split", "-s", &s, "--ratio", bad]);
    }
    assert_eq!(env.pane_count(&s), before, "no pane survived a bad ratio");
}

#[test]
fn split_still_accepts_ratios_inside_the_range() {
    let mut env = Env::new();
    let s = env.session("ratio-ok");
    let before = env.pane_count(&s);

    for good in ["0.25", "0.5", "0.75"] {
        env.ok(&["pane", "split", "-s", &s, "--ratio", good, "--cmd", PARK]);
    }
    assert_eq!(
        env.pane_count(&s),
        before + 3,
        "valid ratios must keep working"
    );
}

#[test]
fn a_template_with_an_out_of_range_ratio_is_refused() {
    let mut env = Env::new();
    let tpl = env.write_template(
        "bad-ratio.toml",
        r#"
[session]
name = "tpl-bad-ratio"

[[windows]]
title = "w"

[[windows.panes]]
command = ["/bin/sleep", "900"]

[[windows.panes]]
direction = "vertical"
ratio = 5.0
command = ["/bin/sleep", "900"]
"#,
    );
    let p = tpl.to_string_lossy().to_string();

    // Both the preview and the real apply must refuse it.
    let dry = env.fails(&["state", "apply", &p, "--dry-run"]);
    assert!(
        dry.contains("ratio"),
        "--dry-run must refuse an out-of-range ratio:\n---\n{dry}\n---"
    );

    let applied = env.fails(&["state", "apply", &p]);
    assert!(
        applied.contains("ratio"),
        "apply must refuse an out-of-range ratio:\n---\n{applied}\n---"
    );
    env.sessions.push("tpl-bad-ratio".to_string());
}

// ── #137: the two dry-run verbs print different JSON shapes ─────────────────

#[test]
fn both_dry_run_verbs_print_the_same_shape() {
    let env = Env::new();
    let tpl = env.write_template(
        "ok.toml",
        r#"
[session]
name = "tpl-shape"

[[windows]]
title = "w"

[[windows.panes]]
command = ["/bin/sleep", "900"]

[[windows.panes]]
direction = "vertical"
ratio = 0.4
command = ["/bin/sleep", "900"]
"#,
    );
    let p = tpl.to_string_lossy().to_string();

    let apply: serde_json::Value =
        serde_json::from_str(&env.ok(&["state", "apply", &p, "--dry-run"])).expect("apply json");
    let restore: serde_json::Value =
        serde_json::from_str(&env.ok(&["session", "restore", &p, "--dry-run"]))
            .expect("restore json");

    assert_eq!(
        apply, restore,
        "the same template through two verbs must preview identically:\n\
         state apply  -> {apply}\n\
         session restore -> {restore}"
    );
    assert!(
        apply.get("ops").and_then(|v| v.as_array()).is_some(),
        "the shared shape is the wire shape, {{\"ops\": [...]}}: {apply}"
    );
}

#[test]
fn both_dry_run_verbs_report_a_bad_template_the_same_way() {
    let env = Env::new();
    let missing = env.work().join("nope.toml").to_string_lossy().to_string();

    let apply = env.fails(&["state", "apply", &missing, "--dry-run"]);
    let restore = env.fails(&["session", "restore", &missing, "--dry-run"]);
    assert_eq!(
        apply.trim(),
        restore.trim(),
        "the same broken template must read the same through both verbs:\n\
         state apply     -> {apply}\n\
         session restore -> {restore}"
    );

    // A malformed template, not just a missing one — that is the TOML
    // diagnostic path, which quotes the source line.
    let bad = env.write_template("bad.toml", "this is not = = toml [[[\n");
    let b = bad.to_string_lossy().to_string();
    let apply_b = env.fails(&["state", "apply", &b, "--dry-run"]);
    let restore_b = env.fails(&["session", "restore", &b, "--dry-run"]);
    assert_eq!(
        apply_b.trim(),
        restore_b.trim(),
        "TOML errors must match too"
    );
}

/// An unbindable `--socket` must not leave a daemon behind.
///
/// The client honoured `--socket`/`SHUX_SOCKET` and the daemon did not, and a
/// bind failure inside the RPC server's detached task was only ever logged —
/// onto a subscriber whose output is /dev/null. So an auto-start against a
/// path the daemon could not bind produced a live daemon serving nobody, the
/// client retried its ten times, gave up, and orphaned it. Once per
/// invocation, each new daemon overwriting the pidfile so `daemon stop` could
/// only ever reap the last. Straight through the repo's zero-leaked-daemons
/// rule, from an ordinary typo.
///
/// The needle is this test's own `XDG_RUNTIME_DIR`, a fresh tempdir per `Env`.
/// Matching on the binary path alone is a machine-wide view that also counts
/// the daemons every other test in this file is running concurrently — the
/// mistake CLAUDE.md warns about for `ps`, and it read as a product failure
/// the first time round.
#[cfg(target_os = "linux")]
#[test]
fn an_unbindable_socket_leaves_no_daemon_behind() {
    let env = Env::new();
    let exe = std::fs::canonicalize(&env.bin).expect("canonicalize bin");
    let needle = format!(
        "XDG_RUNTIME_DIR={}",
        env.root.path().join("runtime").display()
    );

    let count = || -> usize {
        std::fs::read_dir("/proc")
            .expect("read /proc")
            .filter_map(|e| e.ok())
            .filter(|e| {
                // Ours, by runtime dir ...
                std::fs::read(e.path().join("environ"))
                    .is_ok_and(|b| b.split(|c| *c == 0).any(|v| v == needle.as_bytes()))
                    // ... and actually this binary.
                    && std::fs::read_link(e.path().join("exe"))
                        .ok()
                        .and_then(|p| std::fs::canonicalize(p).ok())
                        .is_some_and(|p| p == exe)
            })
            .count()
    };

    // A regular file where a directory would have to be: `mkdir` fails with
    // ENOTDIR, so the bind cannot succeed. Stays inside the test's own tempdir.
    let blocker = env.root.path().join("blocker");
    std::fs::write(&blocker, b"not a directory").expect("write blocker");
    let bogus = blocker.join("x.sock");

    let before = count();
    for _ in 0..2 {
        let out = env
            .shux()
            .args(["--socket", bogus.to_str().unwrap(), "session", "list"])
            .output()
            .expect("spawn shux");
        assert!(
            !out.status.success(),
            "an unbindable socket must fail, not succeed"
        );
    }

    // Let any daemon that got as far as daemonizing finish; counting too early
    // would pass for the wrong reason.
    std::thread::sleep(std::time::Duration::from_millis(2000));
    let after = count();
    assert_eq!(
        after,
        before,
        "{} daemon(s) leaked after two unbindable-socket invocations",
        after.saturating_sub(before)
    );
}

/// The other half of the same fix: `--socket` now genuinely selects the socket
/// the daemon serves, instead of the client asking for one path while the
/// daemon bound another.
#[test]
fn an_explicit_socket_path_is_actually_served() {
    let env = Env::new();
    let custom = env.root.path().join("custom").join("shux.sock");
    let sock = custom.to_str().unwrap().to_string();

    let out = env
        .shux()
        .args(["--socket", &sock, "session", "list"])
        .output()
        .expect("spawn shux");
    assert!(
        out.status.success(),
        "a creatable --socket path must work:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        custom.exists(),
        "the daemon bound {sock:?}, so it must exist"
    );

    let _ = env
        .shux()
        .args(["--socket", &sock, "daemon", "stop"])
        .output();
}

/// The backtrace opt-in is an egress path like any other.
///
/// The rest of this file strips `RUST_BACKTRACE` to test default behaviour,
/// which is exactly how the opt-in branch escaped scrutiny: it prints the
/// anyhow chain, and that chain carries a TOML diagnostic quoting the
/// offending source line verbatim. A template containing a raw ESC therefore
/// replayed it at the operator's terminal for anyone who had the variable set.
#[test]
fn the_backtrace_opt_in_is_still_inert_against_a_hostile_template() {
    let env = Env::new();
    let path = env.work().join("hostile.toml");
    // A RAW ESC byte — TOML forbids it in a basic string, which is why this
    // is a parse error and why the diagnostic quotes it back.
    std::fs::write(
        &path,
        b"[session]\nname = \"h\"\ntitle = \"\x1b]0;PWNED\x07\"\n".as_slice(),
    )
    .expect("write");

    let out = env
        .shux()
        .args(["state", "apply", path.to_str().unwrap()])
        .env("RUST_BACKTRACE", "1")
        .output()
        .expect("spawn shux");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(!out.status.success());
    // `\n` and `\t` are structure; nothing else may reach the terminal raw.
    let raw: Vec<u32> = combined
        .chars()
        .filter(|c| (c.is_control() && *c != '\n' && *c != '\t') || *c == '\u{7f}')
        .map(|c| c as u32)
        .collect();
    assert!(
        raw.is_empty(),
        "opted-in backtrace leaked {} raw control byte(s) {:?}:\n{}",
        raw.len(),
        raw,
        combined.escape_debug()
    );
    assert!(
        combined.contains("\\u{1b}"),
        "the payload should still be VISIBLE, escaped:\n{}",
        combined.escape_debug()
    );
}

/// `#[error("… {0}")]` on a `#[from]` field makes Display interpolate the very
/// source `{:#}` then walks, so the OS error landed on screen twice inside one
/// message: `… (os error 2): No such file or directory (os error 2)`.
#[test]
fn a_template_error_names_its_cause_exactly_once() {
    let env = Env::new();
    let missing = env.work().join("nope.toml").to_string_lossy().to_string();

    for verb in [
        vec!["state", "apply", &missing, "--dry-run"],
        vec!["session", "restore", &missing, "--dry-run"],
    ] {
        let stderr = env.fails(&verb);
        let hits = stderr.matches("os error 2").count();
        assert_eq!(
            hits, 1,
            "{verb:?} named the cause {hits}x in one message:\n---\n{stderr}\n---"
        );
        // Naming the layer without the cause would be the opposite regression.
        assert!(
            stderr.contains("No such file or directory"),
            "{verb:?} lost the cause entirely:\n---\n{stderr}\n---"
        );
    }
}

/// The clap value parser judged an `f64` while `Op::SplitPane::ratio` is `f32`,
/// so anything in `(1 - 2^-25, 1.0)` passed the guard and then became exactly
/// `1.0f32` — the same unusable sliver the range exists to refuse.
#[test]
fn split_rejects_ratios_that_only_collapse_after_the_f32_cast() {
    let mut env = Env::new();
    let s = env.session("ratio-f32");
    let before = env.pane_count(&s);

    // NOT included: `9e-46`, which casts to the smallest f32 subnormal and so
    // is genuinely inside the open interval. It still draws a sliver, but so
    // does `0.001` on a 120-column window — the practical floor depends on the
    // target pane's width in cells, not on the type, and this guard has no
    // access to that. The contract enforced here is the documented interval.
    for bad in [
        "0.99999999",
        "0.9999999999999999",
        "0.999999999999",
        "1e-300",
    ] {
        let arg = format!("--ratio={bad}");
        let stderr = env.fails(&["pane", "split", "-s", &s, &arg, "--cmd", PARK]);
        assert!(
            stderr.contains("0.0") && stderr.contains("1.0"),
            "--ratio {bad} collapses to 0.0/1.0 in f32 and must be refused:\n---\n{stderr}\n---"
        );
    }
    assert_eq!(
        env.pane_count(&s),
        before,
        "a ratio that collapses on the cast still created a pane"
    );
}

/// `rpc call` is a shipped surface — every subcommand mirrors an RPC method
/// 1:1 — so a guard living only in the clap value parser left the documented
/// range fully reachable by anyone typing the method name.
#[test]
fn the_split_rpc_rejects_an_out_of_range_ratio() {
    let mut env = Env::new();
    let s = env.session("ratio-rpc");
    let before = env.pane_count(&s);
    let pane = env.json(&["pane", "list", "-s", &s])[0]["id"]
        .as_str()
        .expect("pane id")
        .to_string();

    for bad in ["5.0", "1.0", "0.0", "-3.0", "1e9", "0.99999999"] {
        let params = format!(r#"{{"pane_id":"{pane}","ratio":{bad},"command":"{PARK}"}}"#);
        let out = env.run(&["rpc", "call", "pane.split", "--params", &params]);
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            combined.contains("out of range"),
            "pane.split accepted ratio {bad} over RPC:\n---\n{combined}\n---"
        );
    }
    assert_eq!(
        env.pane_count(&s),
        before,
        "an RPC split with a bad ratio still created a pane"
    );

    // Control: a good ratio still splits through the same RPC path.
    let params = format!(r#"{{"pane_id":"{pane}","ratio":0.5,"command":"{PARK}"}}"#);
    env.ok(&["rpc", "call", "pane.split", "--params", &params]);
    assert_eq!(
        env.pane_count(&s),
        before + 1,
        "a valid RPC split must work"
    );
}
