//! End-to-end: the `[shell]` config section reaches the panes it documents
//! (issue #132).
//!
//! `shell.command` and `shell.env` parsed, validated through `shux config
//! validate`, and were then dropped on the floor — nothing read either one.
//! That is invisible from a return value: every pane still started, it just
//! ran `$SHELL -l -i` with the user's config ignored. So this suite drives the
//! shipped binary — real daemon in an isolated `XDG_RUNTIME_DIR`, real config
//! file, real PTYs — and reads the pane's screen.
//!
//! Every capture carries a colour probe (truecolor + 256-indexed + basic ANSI)
//! so a monochrome or `NO_COLOR` regression cannot pass a run that only ever
//! compared characters.

use std::path::PathBuf;
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

/// Truecolor + 256-indexed + basic ANSI on one line. Mandatory on every
/// daemon-backed capture (CLAUDE.md).
const COLOUR_PROBE: &str = "\\033[38;2;120;220;180mTRUECOLOR\\033[0m \\033[38;5;208mINDEXED\\033[0m \\033[34mBASIC\\033[0m";

fn probe() -> String {
    format!("printf '{COLOUR_PROBE}\\n'")
}

/// Keeps the pane's screen alive after the interesting part has printed.
const PARK: &str = "exec sleep 900";

/// A `[shell] command = ["/bin/sh", "-c", <script>]` fixture.
///
/// `script` goes in a TOML **multi-line literal** string: it carries `\033`
/// and single quotes, and neither survives a basic `"…"` string — `\0` is not
/// a legal TOML escape, so the whole file fails to parse and the daemon falls
/// back to defaults.
fn shell_command_config(script: &str) -> String {
    format!("[shell]\ncommand = [\"/bin/sh\", \"-c\", '''{script}''']\n")
}

/// A `$SHELL` that cannot be exec'd.
///
/// Pinned deliberately: it makes "did the config decide the shell?" a
/// yes/no question rather than a comparison between two shells that behave
/// the same. Any pane that falls back to `$SHELL` here dies instead of
/// quietly looking correct.
const UNUSABLE_SHELL: &str = "/nonexistent/shux-issue-132-no-such-shell";

// ── isolated daemon environment ─────────────────────────────────────────

struct Env {
    bin: PathBuf,
    root: tempfile::TempDir,
    sessions: Vec<String>,
    shell_env: String,
}

impl Env {
    /// Write `config.toml` and pin `$SHELL`, both BEFORE the daemon starts —
    /// the daemon loads config at boot, so a file written afterwards would be
    /// testing the hot-reload watcher instead of the spawn path.
    fn new(config_toml: &str, shell_env: &str) -> Self {
        let root = tempfile::tempdir().expect("temp root");
        for sub in ["runtime", "config/shux", "work"] {
            std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
        }
        std::fs::write(root.path().join("config/shux/config.toml"), config_toml)
            .expect("write config.toml");
        let env = Self {
            bin: PathBuf::from(env!("CARGO_BIN_EXE_shux")),
            root,
            sessions: Vec::new(),
            shell_env: shell_env.to_string(),
        };

        // A config that does not parse is loaded as `Config::default()` with a
        // warning nobody reads. That silently turns every case here into "no
        // `[shell]` at all" — which is the pre-fix behaviour, so the tests that
        // assert the override fail with a confusing message and the one that
        // asserts the default PASSES for the wrong reason. Bought once, here.
        // (Found the hard way: the colour probe's `\033` is not a legal TOML
        // escape, so the fixtures below use literal `'''` strings.)
        let out = env.run(&["config", "validate"]);
        assert!(
            out.status.success(),
            "test fixture config.toml is invalid, so the daemon would silently \
             use defaults:\n{}\n{}\n--- config.toml ---\n{config_toml}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        env
    }

    fn shux(&self) -> Command {
        let mut cmd = Command::new(&self.bin);
        cmd.env("XDG_RUNTIME_DIR", self.root.path().join("runtime"))
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env_remove("SHUX_SOCKET")
            .env("NO_COLOR", "1")
            .env("SHELL", &self.shell_env)
            .current_dir(self.root.path().join("work"));
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
            "{args:?} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// `shux session create <name> -d` with no `--cmd` — the case
    /// `[shell].command` is supposed to govern. Returns the initial pane id.
    #[track_caller]
    fn session(&mut self, name: &str) -> String {
        self.ok(&["session", "create", name, "-d"]);
        self.sessions.push(name.to_string());
        self.first_pane(name)
    }

    /// `shux session create <name> -d --cmd <shell string>`, parked so the
    /// screen survives. Returns the initial pane id.
    #[track_caller]
    fn session_with_cmd(&mut self, name: &str, script: &str) -> String {
        let full = format!("{script}; {PARK}");
        self.ok(&["session", "create", name, "-d", "--cmd", &full]);
        self.sessions.push(name.to_string());
        self.first_pane(name)
    }

    #[track_caller]
    fn first_pane(&self, session: &str) -> String {
        let v: serde_json::Value =
            serde_json::from_str(&self.ok(&["--format", "json", "pane", "list", "-s", session]))
                .expect("pane list json");
        v[0]["id"].as_str().expect("pane id").to_string()
    }

    fn capture(&self, session: &str, pane: &str) -> String {
        self.ok(&["pane", "capture", "-s", session, "-p", pane])
    }

    #[track_caller]
    fn wait_for(&self, session: &str, pane: &str, needle: &str) {
        let out = self.run(&[
            "pane",
            "wait-for",
            "-s",
            session,
            "-p",
            pane,
            "-t",
            needle,
            "--timeout-ms",
            "20000",
        ]);
        assert!(
            out.status.success(),
            "pane never showed {needle:?}; screen was:\n{}",
            self.capture(session, pane)
        );
    }

    /// Assert the pane's screen contains `needle` and the colour probe, and
    /// does NOT contain any of `absent`.
    #[track_caller]
    fn expect_screen(&self, session: &str, pane: &str, needle: &str, absent: &[&str]) {
        self.wait_for(session, pane, needle);
        let screen = self.capture(session, pane);
        for p in ["TRUECOLOR", "INDEXED", "BASIC"] {
            assert!(
                screen.contains(p),
                "colour probe {p} missing from pane screen:\n{screen}"
            );
        }
        for bad in absent {
            assert!(
                !screen.contains(bad),
                "screen still contains {bad:?}:\n{screen}"
            );
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        for session in std::mem::take(&mut self.sessions) {
            let _ = self.run(&["session", "kill", &session]);
        }
        let _ = self.run(&["daemon", "stop"]);

        // Hard proof of no leak: the recorded pid must be gone. Identified by
        // pidfile, never by matching a command line.
        let pidfile = self.root.path().join("runtime/shux/shux.pid");
        if let Some(pid) = std::fs::read_to_string(&pidfile)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok())
        {
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                if kill(Pid::from_raw(pid), None).is_err() {
                    return;
                }
                thread::sleep(Duration::from_millis(50));
            }
            let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
            panic!("daemon {pid} outlived the test");
        }
    }
}

// ── the issue's own reproduction ────────────────────────────────────────

/// Issue #132, verbatim: a `[shell].command` override must be the argv a
/// default pane execs. Before the fix the field was parsed, validated and
/// discarded, and the pane ran `$SHELL -l -i`.
#[test]
fn the_configured_shell_argv_is_what_a_default_pane_runs() {
    let mut env = Env::new(
        &shell_command_config(&format!(
            "printf 'CONFIG_SHELL_USED\\n'; {}; {PARK}",
            probe()
        )),
        UNUSABLE_SHELL,
    );
    let pane = env.session("cfgshell");
    env.expect_screen("cfgshell", &pane, "CONFIG_SHELL_USED", &[]);
}

/// `shell.env` is documented as "extra env vars to inject into every spawned
/// pane". Same defect, same struct: nothing read it either. The pane here runs
/// the configured shell, which prints the variable it was given.
#[test]
fn configured_env_reaches_a_spawned_pane() {
    let mut env = Env::new(
        &format!(
            "{}env = {{ SHUX_ISSUE_132 = \"from-config\" }}\n",
            shell_command_config(&format!(
                "printf 'ENV=[%s]\\n' \"$SHUX_ISSUE_132\"; {}; {PARK}",
                probe()
            ))
        ),
        UNUSABLE_SHELL,
    );
    let pane = env.session("cfgenv");
    env.expect_screen("cfgenv", &pane, "ENV=[from-config]", &["ENV=[]"]);
}

/// The override is for panes that asked for *no* command. An explicit argv or
/// `--cmd` string still wins — otherwise `shux new -- vim a.rs` would silently
/// open a shell on any machine with `[shell]` configured.
///
/// `$SHELL` is usable here on purpose: this case must isolate *precedence*,
/// and it passes both before and after the fix. It is the guard against the
/// obvious wrong implementation — applying `[shell].command` unconditionally.
#[test]
fn an_explicit_command_still_beats_the_configured_shell() {
    let mut env = Env::new(
        &shell_command_config(&format!("printf 'CONFIG_SHELL_USED\\n'; {PARK}")),
        "/bin/sh",
    );
    let pane = env.session_with_cmd("explicit", &format!("{}; printf 'EXPLICIT\\n'", probe()));
    env.expect_screen("explicit", &pane, "EXPLICIT", &["CONFIG_SHELL_USED"]);
}

/// A `--cmd` string is interpreted by whatever shell a pane opened by hand
/// runs — that is the contract `pane_command.rs` documents. Once `[shell]`
/// decides the pane shell, it has to decide this one too, or the same line
/// means bash syntax in one place and fish syntax in the other.
///
/// `$SHELL` is unexecutable here, so the pre-fix path does not merely differ:
/// the spawn fails outright.
#[test]
fn the_configured_shell_interprets_a_cmd_string() {
    let mut env = Env::new(
        "[shell]\ncommand = [\"/bin/sh\", \"-l\", \"-i\"]\n",
        UNUSABLE_SHELL,
    );
    let pane = env.session_with_cmd("interp", &format!("{}; printf 'INTERPRETED\\n'", probe()));
    env.expect_screen("interp", &pane, "INTERPRETED", &[]);
}

/// A typo in `[shell].command` is a new way for a spawn to fail: argv[0] now
/// comes from a file the user may have edited days ago, not from the line they
/// just typed. The failure must say so.
///
/// Before this change the CLI printed the bare OS error — "No such file or
/// directory (os error 2)" — and dropped `data.hint` entirely, so even the
/// generic "check argv[0]" never reached anyone.
#[test]
fn a_typo_in_the_configured_shell_is_diagnosed_by_name() {
    let env = Env::new(
        "[shell]\ncommand = [\"/usr/bin/shux-issue-132-typo\", \"-l\"]\n",
        "/bin/sh",
    );
    let out = env.run(&["session", "create", "broken", "-d"]);
    let joined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "a pane that never spawned is not a session:\n{joined}"
    );
    assert!(
        joined.contains("/usr/bin/shux-issue-132-typo"),
        "the failure never names the program the config asked for:\n{joined}"
    );
    assert!(
        joined.contains("[shell].command"),
        "the failure never points at the config file:\n{joined}"
    );
}

/// An empty `[shell]` block must leave the default shell exactly where it was:
/// `$SHELL -l -i`, no config in the way. `[shell].env` still applies, because
/// injecting env is not an override of anything.
#[test]
fn an_empty_shell_command_leaves_the_default_shell_alone() {
    let mut env = Env::new(
        "[shell]\ncommand = []\nenv = { SHUX_ISSUE_132 = \"from-config\" }\n",
        "/bin/sh",
    );
    let pane = env.session("defaults");

    // The pane is a live, readable shell — not the configured argv, because
    // there isn't one.
    env.ok(&[
        "pane",
        "run",
        "-s",
        "defaults",
        "-p",
        &pane,
        "--command",
        &format!("printf 'ENV=[%s]\\n' \"$SHUX_ISSUE_132\"; {}", probe()),
    ]);
    env.expect_screen("defaults", &pane, "ENV=[from-config]", &["ENV=[]"]);
}
