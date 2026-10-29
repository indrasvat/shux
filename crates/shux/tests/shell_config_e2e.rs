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

use shux_testkit::{BUDGET, poll_until, wait_until};

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

    /// Replace `config.toml` under a RUNNING daemon with one whose
    /// `[shell].command` is the unspawnable `typo`, and wait until the daemon
    /// has reloaded it. Validated for the same reason as the initial write.
    #[track_caller]
    fn break_shell_config(&self, config_toml: &str, typo: &str) {
        std::fs::write(
            self.root.path().join("config/shux/config.toml"),
            config_toml,
        )
        .expect("rewrite config.toml");
        let out = self.run(&["config", "validate"]);
        assert!(
            out.status.success(),
            "rewritten fixture config.toml is invalid:\n{}\n--- config.toml ---\n{config_toml}",
            String::from_utf8_lossy(&out.stderr)
        );
        // The reload is observable as a default-shell spawn that fails naming
        // `typo`. A probe that still spawns ran the old config: remove it.
        let mut attempt = 0;
        wait_until("the daemon to hot-reload config.toml", || {
            attempt += 1;
            let name = format!("reload-probe-{attempt}");
            let out = self.run(&["session", "create", &name, "-d"]);
            if out.status.success() {
                self.ok(&["session", "kill", &name]);
                return false;
            }
            String::from_utf8_lossy(&out.stderr).contains(typo)
                || String::from_utf8_lossy(&out.stdout).contains(typo)
        });
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

    /// `session create <name> -d -- <argv>` — an EXPLICIT argv, so the pane is
    /// unaffected by `[shell].command` and survives a config flip.
    #[track_caller]
    fn session_with_argv(&mut self, name: &str, argv: &[&str]) -> String {
        let mut args = vec!["session", "create", name, "-d", "--"];
        args.extend_from_slice(argv);
        self.ok(&args);
        self.sessions.push(name.to_string());
        self.first_pane(name)
    }

    #[track_caller]
    fn send_keys(&self, session: &str, text: &str) {
        self.ok(&["pane", "send-keys", "-s", session, "-t", text]);
    }

    /// How many `kind` events the daemon's ring buffer holds.
    fn event_count(&self, kind: &str) -> usize {
        self.ok(&["events", "history", "-f", kind, "-n", "1000"])
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count()
    }

    /// Every (window, pane) in `session`, with whether the pane has a live PTY.
    ///
    /// `pane list -s X` alone reads only the ACTIVE window — which a new-window
    /// action moves — so a phantom left in the window you started in would not
    /// even appear. Walk every window.
    fn panes_with_liveness(&self, session: &str) -> Vec<(String, bool)> {
        let windows: Vec<String> = serde_json::from_str::<serde_json::Value>(
            &self.ok(&["--format", "json", "window", "list", "-s", session]),
        )
        .expect("window list json")
        .as_array()
        .expect("window array")
        .iter()
        .map(|w| w["id"].as_str().unwrap_or_default().to_string())
        .collect();

        let mut out = Vec::new();
        for wid in windows {
            let v: serde_json::Value = serde_json::from_str(&self.ok(&[
                "--format", "json", "pane", "list", "-s", session, "-w", &wid,
            ]))
            .expect("pane list json");
            for p in v.as_array().expect("pane array") {
                let pid = p["id"].as_str().unwrap_or_default().to_string();
                // A pane with no PTY cannot be captured — that is exactly what
                // "phantom" means to every verb a user reaches for next.
                let live = self
                    .run(&["pane", "capture", "-s", session, "-p", &pid])
                    .status
                    .success();
                out.push((pid, live));
            }
        }
        out
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
            "30000",
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
            && !shux_testkit::terminate(pid)
            // A second panic during unwinding aborts and hides the first.
            && !std::thread::panicking()
        {
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

// ── blank configured program (Codex P2 on PR #145) ──────────────────────

/// `command = ["   "]` parses and validates — the schema is `Vec<String>` and
/// nothing there knows what a program name is. Treated as an override it execs
/// a blank program, so the default pane dies while `--cmd` keeps working,
/// because `interpreting_shell` filters the same blank and falls back to
/// `$SHELL`. Blank means unset, on both paths.
#[test]
fn a_blank_configured_shell_is_treated_as_unset_on_both_paths() {
    let mut env = Env::new("[shell]\ncommand = [\"   \"]\n", "/bin/sh");

    // The default-pane path: a pane, not a spawn failure.
    let pane = env.session("blankdefault");
    env.ok(&[
        "pane",
        "run",
        "-s",
        "blankdefault",
        "-p",
        &pane,
        "--command",
        &format!("printf 'DEFAULT_PANE_ALIVE\\n'; {}", probe()),
    ]);
    env.expect_screen("blankdefault", &pane, "DEFAULT_PANE_ALIVE", &[]);

    // The `--cmd` path: same shell, so the same answer.
    let cmd_pane = env.session_with_cmd(
        "blankcmd",
        &format!("{}; printf 'CMD_PANE_ALIVE\\n'", probe()),
    );
    env.expect_screen("blankcmd", &cmd_pane, "CMD_PANE_ALIVE", &[]);
}

// ── attach rollback (Codex P1 on PR #145) ───────────────────────────────

/// Prefix is rebound to ctrl-a because the default ctrl-space is NUL, and NUL
/// is the one byte `pane send-keys` will not carry into a pane.
const ATTACH_KEYS: &str = "[keys]\nprefix = \"ctrl-a\"\n\n";

/// Drive the real attach client from inside a shux pane, split and create a
/// window while `[shell].command` names a program that does not exist, and
/// prove the graph is left with no phantom.
///
/// Before the fix both attach paths mutated the graph and then dropped the
/// spawn error with `.ok()`: the split left a focused third pane that answered
/// "pane VT not found" to every later verb, and `prefix c` created a whole
/// window whose only pane could never render — with the attach UI switching to
/// it as if it had worked. The `pane.split` and `window.create` RPCs have
/// rolled this back since #125; the attach paths never did, and
/// `[shell].command` turns that latent case into a reachable one.
#[test]
fn a_failed_attach_split_or_new_window_leaves_no_phantom() {
    let mut env = Env::new(&format!("{ATTACH_KEYS}[shell]\ncommand = []\n"), "/bin/sh");

    // A host pane running a real SHELL — the attach client has to be typed
    // into something that reads its input. The target session's own pane gets
    // an EXPLICIT argv so the config flip below cannot kill it.
    let host_pane = env.session("host");
    env.session_with_argv("target", &["sleep", "900"]);

    let bin = env.bin.display().to_string();
    let root = env.root.path().display().to_string();
    env.send_keys(
        "host",
        &format!(
            "XDG_RUNTIME_DIR={root}/runtime XDG_CONFIG_HOME={root}/config \
             {bin} session attach target\n"
        ),
    );
    // Wait for something only the ATTACHED UI draws. The session name is no
    // good as a needle — the shell echoes the command line that contains it,
    // so it is on screen before the client has started (a not-yet-started app
    // is quiet, and matching its own echo is how you race one).
    env.wait_for("host", &host_pane, "1 pane");

    // CONTROL: with a working shell the same keystrokes really do split. This
    // is what makes the negative assertion below mean anything — without it,
    // "no phantom" is equally satisfied by keys that never arrived.
    env.send_keys("host", "\x01v");
    poll_until(BUDGET, || env.panes_with_liveness("target").len() >= 2);
    let control = env.panes_with_liveness("target");
    assert_eq!(
        control.len(),
        2,
        "control split never happened, so this test cannot detect a phantom: {control:?}"
    );
    assert!(control.iter().all(|(_, live)| *live), "{control:?}");

    // Now make every default-pane spawn fail, and repeat.
    env.break_shell_config(
        &format!("{ATTACH_KEYS}[shell]\ncommand = [\"/usr/bin/shux-issue-132-typo\"]\n"),
        "/usr/bin/shux-issue-132-typo",
    );

    // The attach loop runs actions in order, so the copy-mode hint that
    // `prefix [` draws proves the split and the new window, rollbacks
    // included, have both finished.
    assert!(
        !env.capture("host", &host_pane).contains("COPY"),
        "the copy-mode marker is on screen before copy mode, so it proves nothing"
    );
    env.send_keys("host", "\x01v");
    env.send_keys("host", "\x01c");
    env.send_keys("host", "\x01[");
    env.wait_for("host", &host_pane, "COPY");

    let after = env.panes_with_liveness("target");
    assert!(
        after.iter().all(|(_, live)| *live),
        "a failed spawn left a pane with no PTY in the graph: {after:?}"
    );
    assert_eq!(
        after.len(),
        2,
        "the rolled-back split and window should leave the graph as the control did: {after:?}"
    );
}

/// `(zoomed pane, focused pane)` in the session's active window.
fn zoom_and_focus(env: &Env, session: &str) -> (Option<String>, Option<String>) {
    let v: serde_json::Value =
        serde_json::from_str(&env.ok(&["--format", "json", "pane", "list", "-s", session]))
            .expect("pane list json");
    let pick = |field: &str| {
        v.as_array()
            .expect("pane array")
            .iter()
            .find(|p| p[field] == true)
            .map(|p| p["id"].as_str().unwrap_or_default().to_string())
    };
    (pick("is_zoomed"), pick("is_focused"))
}

/// Splitting unzooms, so the undo of a failed keyboard split has to zoom back,
/// exactly as the `pane.split` RPC's undo does (issue #152).
///
/// Every step is a real keystroke typed into a real attach client: the zoom
/// is `prefix z` and the split is `prefix v`, so the window is zoomed the way
/// an operator zooms it, not through an RPC the attach path never sees.
#[test]
fn a_failed_attach_split_leaves_the_window_zoomed_on_the_same_pane() {
    // Panes that fall back to the configured shell print the colour probe, so
    // the attach UI drawing the zoomed pane is what the host screen shows.
    let mut env = Env::new(
        &format!(
            "{ATTACH_KEYS}{}",
            shell_command_config(&format!("{}; {PARK}", probe()))
        ),
        "/bin/sh",
    );
    // The host must read its input, so it gets an explicit interactive shell
    // the config cannot replace; the target's first pane gets an explicit argv
    // so the config flip below cannot kill it.
    let host_pane = env.session_with_argv("host", &["/bin/sh"]);
    env.session_with_argv("target", &["sleep", "900"]);

    let bin = env.bin.display().to_string();
    let root = env.root.path().display().to_string();
    env.send_keys(
        "host",
        &format!(
            "XDG_RUNTIME_DIR={root}/runtime XDG_CONFIG_HOME={root}/config \
             {bin} session attach target\n"
        ),
    );
    env.wait_for("host", &host_pane, "1 pane");

    // CONTROL: a working split, then a keyboard zoom of the pane it made.
    env.send_keys("host", "\x01v");
    poll_until(BUDGET, || env.panes_with_liveness("target").len() >= 2);
    assert_eq!(
        env.panes_with_liveness("target").len(),
        2,
        "control split never happened, so this test cannot tell a lost zoom from lost keys"
    );
    env.send_keys("host", "\x01z");
    poll_until(BUDGET, || zoom_and_focus(&env, "target").0.is_some());
    let (zoomed, focused) = zoom_and_focus(&env, "target");
    let zoomed = zoomed.expect("control zoom never happened");
    assert_eq!(
        Some(&zoomed),
        focused.as_ref(),
        "prefix z zooms the focused pane"
    );
    env.expect_screen("host", &host_pane, "TRUECOLOR", &[]);

    env.break_shell_config(
        &format!("{ATTACH_KEYS}[shell]\ncommand = [\"/usr/bin/shux-issue-152-typo\"]\n"),
        "/usr/bin/shux-issue-152-typo",
    );

    // The failed split, then copy mode. The attach loop runs actions in order,
    // so the copy-mode hint on screen proves the split, rollback included, has
    // finished.
    assert!(
        !env.capture("host", &host_pane).contains("COPY"),
        "the copy-mode marker is on screen before copy mode, so it proves nothing"
    );
    let (created, destroyed) = (
        env.event_count("pane.created"),
        env.event_count("pane.exited"),
    );
    env.send_keys("host", "\x01v");
    env.send_keys("host", "\x01[");
    env.wait_for("host", &host_pane, "COPY");

    // Copy mode only proves `prefix [` ran. A split that never happened would
    // leave the zoom alone too, so prove this one was made and undone.
    assert_eq!(
        (
            env.event_count("pane.created") - created,
            env.event_count("pane.exited") - destroyed
        ),
        (1, 1),
        "expected one split made and undone (pane.created, then pane.exited)"
    );
    let after = env.panes_with_liveness("target");
    assert!(
        after.len() == 2 && after.iter().all(|(_, live)| *live),
        "expected the 2 live panes from before the split: {after:?}"
    );
    let (zoomed_after, focused_after) = zoom_and_focus(&env, "target");
    assert_eq!(
        zoomed_after.as_ref(),
        Some(&zoomed),
        "a failed split left the window un-zoomed"
    );
    assert_eq!(focused_after.as_ref(), Some(&zoomed), "focus moved");
}
