//! End-to-end: what the attach UI draws for the help overlay and the copy-mode
//! cursor (issue #224).
//!
//! The real attach client runs inside a host shux pane, so its output is the
//! host pane's screen, read back with `pane capture`. Real daemon, real config
//! file, real PTYs, real keystrokes. The target pane prints the colour probe
//! (truecolor + 256-indexed + basic ANSI) so a monochrome regression cannot pass.

use std::path::PathBuf;
use std::process::{Command, Output};

use proptest::prelude::*;
use shux_testkit::wait_until;

/// The probe, then a wide-character line, then park.
const TARGET_SCRIPT: &str = "printf '\\033[38;2;120;220;180mTRUECOLOR\\033[0m \
    \\033[38;5;208mINDEXED\\033[0m \\033[34mBASIC\\033[0m\\n\\346\\274\\242\\345\\255\\227WIDE\\n'; \
    exec sleep 900";

/// `ctrl-a`, because the default `ctrl-space` is NUL, the one byte
/// `pane send-keys` will not carry into a pane.
fn keys_config(prefix: &str) -> String {
    format!("[keys]\nprefix = \"{prefix}\"\n")
}

struct Env {
    bin: PathBuf,
    root: tempfile::TempDir,
    sessions: Vec<String>,
    host_pane: String,
}

impl Env {
    /// A target session running the probe, and a 100x40 host pane with the
    /// attach client running in it, attached and drawing.
    fn attached(config_toml: &str) -> Self {
        Self::attached_with(config_toml, "", "")
    }

    /// [`Self::attached`], with `env_vars` before the client's command line
    /// and `redirect` after it, `{root}` in it naming the test's root.
    fn attached_with(config_toml: &str, env_vars: &str, redirect: &str) -> Self {
        Self::attached_command(config_toml, env_vars, redirect, &["/bin/sh", "-c", TARGET_SCRIPT])
    }

    fn attached_command(config_toml: &str, env_vars: &str, redirect: &str, argv: &[&str]) -> Self {
        let root = tempfile::tempdir().expect("temp root");
        for sub in ["runtime", "config/shux", "state", "work"] {
            std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
        }
        let mut env = Self {
            bin: PathBuf::from(env!("CARGO_BIN_EXE_shux")),
            root,
            sessions: Vec::new(),
            host_pane: String::new(),
        };
        env.write_config(config_toml);
        env.host_pane = env.session_with_argv("host", &["/bin/sh"]);
        // Wide enough that the status bar never truncates the pane count the
        // wait below needs: it also carries the git branch and window name.
        env.ok(&[
            "pane", "set-size", "-s", "host", "--cols", "100", "--rows", "40",
        ]);
        let argv: Vec<String> = argv.iter()
            .map(|arg| arg.replace("{root}", &env.root.path().display().to_string()))
            .collect();
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        env.session_with_argv("target", &argv);

        let bin = env.bin.display().to_string();
        let root = env.root.path().display().to_string();
        let redirect = redirect.replace("{root}", &root);
        env.send_keys(&format!(
            "XDG_RUNTIME_DIR={root}/runtime XDG_CONFIG_HOME={root}/config {env_vars} \
             {bin} session attach target {redirect}\n"
        ));
        // Only the attached UI draws the pane count; the shell's echo of the
        // command line cannot contain it.
        env.wait_for("1 pane");
        env.wait_for("INDEXED");
        env
    }

    /// Validated: a config that does not parse is loaded as defaults with a
    /// warning nobody reads, and every assertion here would then be about
    /// `ctrl-space`.
    #[track_caller]
    fn write_config(&self, config_toml: &str) {
        std::fs::write(
            self.root.path().join("config/shux/config.toml"),
            config_toml,
        )
        .expect("write config.toml");
        let out = self.run(&["config", "validate"]);
        assert!(
            out.status.success(),
            "fixture config.toml is invalid:\n{}\n--- config.toml ---\n{config_toml}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn shux(&self) -> Command {
        let mut cmd = Command::new(&self.bin);
        cmd.env("XDG_RUNTIME_DIR", self.root.path().join("runtime"))
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("XDG_STATE_HOME", self.root.path().join("state"))
            .env_remove("SHUX_SOCKET")
            .env("NO_COLOR", "1")
            .env("SHELL", "/bin/sh")
            .current_dir(self.root.path().join("work"));
        // A git hook exports these, and the status bar's git segment would then
        // describe this repository instead of the test's scratch directory.
        for var in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_COMMON_DIR",
        ] {
            cmd.env_remove(var);
        }
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

    #[track_caller]
    fn session_with_argv(&mut self, name: &str, argv: &[&str]) -> String {
        let mut args = vec!["session", "create", name, "-d", "--"];
        args.extend_from_slice(argv);
        self.ok(&args);
        self.sessions.push(name.to_string());
        let v: serde_json::Value =
            serde_json::from_str(&self.ok(&["--format", "json", "pane", "list", "-s", name]))
                .expect("pane list json");
        v[0]["id"].as_str().expect("pane id").to_string()
    }

    #[track_caller]
    fn send_keys(&self, text: &str) {
        self.ok(&["pane", "send-keys", "-s", "host", "-t", text]);
    }

    fn screen(&self) -> String {
        self.ok(&["pane", "capture", "-s", "host", "-p", &self.host_pane])
    }

    #[track_caller]
    fn wait_for(&self, needle: &str) {
        let out = self.run(&[
            "pane",
            "wait-for",
            "-s",
            "host",
            "-p",
            &self.host_pane,
            "-t",
            needle,
            "--timeout-ms",
            "30000",
        ]);
        assert!(
            out.status.success(),
            "the attached UI never showed {needle:?}; screen was:\n{}",
            self.screen()
        );
    }

    /// Whether the host pane's terminal cursor is showing.
    fn cursor_visible(&self) -> bool {
        self.cursor().2
    }

    /// The host pane's terminal cursor: `(row, col, visible)`.
    fn cursor(&self) -> (u64, u64, bool) {
        let v: serde_json::Value = serde_json::from_str(&self.ok(&[
            "--format",
            "json",
            "pane",
            "capture",
            "-s",
            "host",
            "-p",
            &self.host_pane,
        ]))
        .expect("capture json");
        let c = &v["cursor"];
        (
            c["row"].as_u64().expect("cursor.row"),
            c["col"].as_u64().expect("cursor.col"),
            c["visible"].as_bool().expect("cursor.visible"),
        )
    }

    /// The host screen row holding `needle`.
    #[track_caller]
    fn row_with(&self, needle: &str) -> String {
        let screen = self.screen();
        screen
            .lines()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no row holds {needle:?}:\n{screen}"))
            .to_string()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        for session in std::mem::take(&mut self.sessions) {
            let _ = self.run(&["session", "kill", &session]);
        }
        let _ = self.run(&["daemon", "stop"]);
        let pidfile = self.root.path().join("runtime/shux/shux.pid");
        if let Some(pid) = std::fs::read_to_string(&pidfile)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok())
            && !shux_testkit::terminate(pid)
            && !std::thread::panicking()
        {
            panic!("daemon {pid} outlived the test");
        }
    }
}

/// The help overlay names the prefix the client is using, and follows it when
/// a hot-reload changes it. The last step presses the NEW prefix: a label that
/// moved while the client kept the old key would name a key that does nothing.
#[test]
fn the_help_overlay_names_the_configured_prefix_through_a_reload() {
    let env = Env::attached(&keys_config("ctrl-a"));

    env.send_keys("\x01?");
    env.wait_for("keybindings (prefix = ^A)");
    assert!(
        !env.screen().contains("Ctrl+Space"),
        "the overlay still names the default prefix:\n{}",
        env.screen()
    );

    env.write_config(&keys_config("ctrl-b"));
    env.wait_for("keybindings (prefix = ^B)");

    // `prefix ?` toggles the overlay; only a client that took the new prefix
    // can close it this way.
    env.send_keys("\x02?");
    wait_until("the new prefix to close the help overlay", || {
        !env.screen().contains("keybindings (prefix")
    });
}

/// The client's logs never land on the terminal its UI is drawing on: they
/// are held while attached and written once the terminal is restored.
///
/// The watcher logs `watching` before it reads any change, so by the time
/// the reloaded prefix is on screen that line has been written, wherever
/// the client writes it.
#[test]
fn the_attached_client_logs_after_the_terminal_is_restored() {
    let env = Env::attached_with(
        &keys_config("ctrl-a"),
        "RUST_LOG=info",
        "2>{root}/client.err",
    );
    let log = env.root.path().join("client.err");
    let read_log = || std::fs::read_to_string(&log).unwrap_or_default();

    env.send_keys("\x01?");
    env.wait_for("keybindings (prefix = ^A)");
    env.write_config(&keys_config("ctrl-b"));
    env.wait_for("keybindings (prefix = ^B)");
    assert_eq!(
        read_log(),
        "",
        "the client logged onto the attached terminal"
    );

    env.send_keys("\x02?");
    wait_until("the help overlay to close", || {
        !env.screen().contains("keybindings (prefix")
    });
    env.send_keys("\x02d");
    env.wait_for("[detached from session 'target']");
    wait_until("the held logs to be written", || {
        read_log().contains("config hot-reload: watching")
    });
}

/// Copy mode draws its own cursor and the help overlay covers the pane, so
/// the terminal's cursor stays hidden under both: left showing, it sits
/// wherever the overlay's last write ended, a second block beside the copy
/// cursor or at the edge of the help box.
#[test]
fn copy_mode_and_the_help_overlay_hide_the_terminal_cursor() {
    let env = Env::attached(&keys_config("ctrl-a"));
    wait_until("the shell's cursor to show", || env.cursor_visible());

    env.send_keys("\x01[");
    env.wait_for("PgUp/PgDn");
    assert!(
        !env.cursor_visible(),
        "copy mode left the terminal cursor showing"
    );
    env.send_keys("q");
    wait_until("the cursor to come back after copy mode", || {
        env.cursor_visible()
    });

    env.send_keys("\x01?");
    env.wait_for("keybindings (prefix = ^A)");
    assert!(
        !env.cursor_visible(),
        "the help overlay left the terminal cursor showing"
    );
    env.send_keys("\x01?");
    wait_until("the cursor to come back after help", || {
        env.cursor_visible()
    });
}

/// The selection overlay and its menu are written after the frame too, and
/// must leave the terminal cursor on the pane's cursor, not where their last
/// write ended.
#[test]
fn a_mouse_selection_leaves_the_terminal_cursor_on_the_pane_cursor() {
    let env = Env::attached(&keys_config("ctrl-a"));
    wait_until("the pane's cursor to show", || env.cursor_visible());
    let parked = env.cursor();

    env.send_keys("\x1b[<0;3;2M\x1b[<32;8;2M\x1b[<0;8;2m");
    env.wait_for("copied selection");
    assert_eq!(
        env.cursor(),
        parked,
        "the selection moved the terminal cursor"
    );

    env.send_keys("\x1b[<2;5;2M\x1b[<2;5;2m");
    env.wait_for("Copy");
    assert_eq!(
        env.cursor(),
        parked,
        "the selection menu moved the terminal cursor"
    );
}

/// The copy-mode cursor shows the character under it, in the cursor colours,
/// narrow and wide alike: with the cursor on its `T`, the probe still reads
/// `TRUECOLOR`.
#[test]
fn the_copy_mode_cursor_keeps_the_character_under_it() {
    let env = Env::attached(&keys_config("ctrl-a"));

    // Copy mode starts with the cursor on the pane's top-left cell.
    env.send_keys("\x01[");
    env.wait_for("COPY");
    let row = env.row_with("INDEXED");
    assert!(
        row.contains("TRUECOLOR INDEXED BASIC"),
        "the cursor blanked the glyph under it: {row:?}"
    );

    // Down onto the wide character, then anchor: the anchored hint is the
    // proof that both keys were handled before the screen is read.
    env.send_keys("jv");
    env.wait_for("n/N next");
    let row = env.row_with("WIDE");
    assert!(
        row.contains("漢字WIDE"),
        "the cursor blanked the wide glyph under it: {row:?}"
    );
}


const ALT_ARROWS: [&str; 4] = ["\x1b[1;3D", "\x1b[1;3C", "\x1b[1;3A", "\x1b[1;3B"];
const COLOUR_PROBE: &str = "printf '\\033[38;2;120;220;180mTRUECOLOR\\033[0m \\
    \\033[38;5;208mINDEXED\\033[0m \\033[34mBASIC\\033[0m\\n';";

/// The acknowledgement is emitted after the sentinel reaches the raw PTY;
/// it fences earlier input without a sleep or an assertion about missing output.
fn raw_key_script(name: &str) -> String {
    format!(
        "stty raw -echo; : > '{{root}}/{name}.keys'; {COLOUR_PROBE} \
         printf 'RAW-READY-{name}\\r\\n'; n=0; \
         while IFS= read -r -N 1 byte; do \
           printf '%s' \"$byte\" >> '{{root}}/{name}.keys'; \
           if [ \"$byte\" = '!' ]; then n=$((n+1)); printf 'ACK-{name}-%s\\r\\n' \"$n\"; fi; \
           if [ \"$byte\" = '%' ]; then printf 'FENCE-{name}\\r\\n'; fi; \
         done"
    )
}

fn raw_key_env() -> Env {
    let env = Env::attached_command(
        &keys_config("ctrl-a"), "", "",
        &["/bin/bash", "--noprofile", "--norc", "-c", &raw_key_script("first")],
    );
    env.wait_for("RAW-READY-first");
    env
}

fn key_log(env: &Env, name: &str) -> String {
    std::fs::read_to_string(env.root.path().join(format!("{name}.keys")))
        .expect("the raw PTY's input log")
}

#[test]
fn alt_arrows_without_a_neighbour_reach_the_real_attach_pty() {
    let env = raw_key_env();
    let input = format!("{}!", ALT_ARROWS.concat());
    env.send_keys(&input);
    env.wait_for("ACK-first-1");
    assert_eq!(key_log(&env, "first"), input);

    // Prefix commands are commands even when their focus action has no target.
    env.send_keys("\x01h\x01j\x01k\x01l!");
    env.wait_for("ACK-first-2");
    assert_eq!(key_log(&env, "first"), format!("{input}!"));
}

#[test]
fn alt_arrows_in_a_zoomed_split_reach_only_the_visible_pane() {
    let env = raw_key_env();
    let second = raw_key_script("second").replace("{root}", &env.root.path().display().to_string());
    env.ok(&["pane", "split", "-s", "target", "-d", "vertical", "--",
        "/bin/bash", "--noprofile", "--norc", "-c", &second]);
    env.wait_for("RAW-READY-second");
    env.send_keys("\x1bz");
    env.wait_for("zoom");
    let input = format!("{}!", ALT_ARROWS.concat());
    env.send_keys(&input);
    env.wait_for("ACK-second-1");
    assert_eq!(key_log(&env, "second"), input);
    assert_eq!(key_log(&env, "first"), "");
    let panes: serde_json::Value = serde_json::from_str(
        &env.ok(&["--format", "json", "pane", "list", "-s", "target"]),
    ).expect("pane list");
    assert!(panes.as_array().expect("panes").iter().any(|p|
        p["is_focused"] == true && p["is_zoomed"] == true));
}

#[test]
fn alt_arrows_focus_real_split_neighbours_and_fall_through_at_edges() {
    for (direction, inward, outward) in [("vertical", 0, 1), ("horizontal", 2, 3)] {
        let env = raw_key_env();
        let second = raw_key_script("second").replace("{root}", &env.root.path().display().to_string());
        env.ok(&["pane", "split", "-s", "target", "-d", direction, "--",
            "/bin/bash", "--noprofile", "--norc", "-c", &second]);
        env.wait_for("RAW-READY-second");

        // New split is focused: its outer edge forwards, then the inward key
        // focuses the original pane. A sentinel proves the ordered result.
        env.send_keys(&format!("{}!", ALT_ARROWS[outward]));
        env.wait_for("ACK-second-1");
        env.send_keys(&format!("{}!", ALT_ARROWS[inward]));
        env.wait_for("ACK-first-1");
        assert_eq!(key_log(&env, "first"), "!");
        assert_eq!(key_log(&env, "second"), format!("{}!", ALT_ARROWS[outward]));

        env.send_keys(&format!("{}!", ALT_ARROWS[inward]));
        env.wait_for("ACK-first-2");
        env.send_keys(&format!("{}!", ALT_ARROWS[outward]));
        env.wait_for("ACK-second-2");
        assert_eq!(key_log(&env, "first"), format!("!{}!", ALT_ARROWS[inward]));
        assert_eq!(key_log(&env, "second"), format!("{}!!", ALT_ARROWS[outward]));
    }
}

#[test]
fn option_left_and_right_edit_words_in_a_real_attached_bash() {
    let script = format!("{COLOUR_PROBE} exec /bin/bash --noprofile --norc -i");
    let env = Env::attached_command(&keys_config("ctrl-a"), "", "",
        &["/bin/sh", "-c", &script]);
    // Pin readline bindings instead of depending on a host's inputrc.
    env.send_keys("bind '\"\\e[1;3D\": backward-word'; bind '\"\\e[1;3C\": forward-word'; \
        PS1='NAV> '; printf '%s\\n' 'READ''LINE-READY'\n");
    env.wait_for("READLINE-READY");

    env.send_keys("printf '<%s>\\n' aaa bbb\x1b[1;3DZ\n");
    env.wait_for("<Zbbb>");
    env.send_keys("printf '<%s>\\n' aaa bbb\x1b[1;3D\x1b[1;3D\x1b[1;3CZ\n");
    env.wait_for("<aaaZ>");
}


proptest! {
    #![proptest_config(ProptestConfig { cases: 16, ..ProptestConfig::default() })]

    #[test]
    fn generated_focus_and_zoom_sequences_preserve_exact_pty_input(
        operations in prop::collection::vec(0u8..9, 1..128),
    ) {
        let env = raw_key_env();
        let second = raw_key_script("second")
            .replace("{root}", &env.root.path().display().to_string());
        env.ok(&["pane", "split", "-s", "target", "-d", "vertical", "--",
            "/bin/bash", "--noprofile", "--norc", "-c", &second]);
        env.wait_for("RAW-READY-second");
        // A vertical split puts the original pane on the left and focuses
        // the new right pane. Equal-height panes have no up/down neighbour.
        let mut side = 1usize;
        let mut zoomed = false;
        let mut expected = [String::new(), String::new()];
        let mut input = String::new();
        for &operation in &operations {
            match operation {
                0..=3 => {
                    let key = ALT_ARROWS[operation as usize];
                    input.push_str(key);
                    match (operation, side, zoomed) {
                        (0, 1, false) => side = 0,
                        (1, 0, false) => side = 1,
                        _ => expected[side].push_str(key),
                    }
                }
                4 => {
                    input.push_str("\x1bz");
                    zoomed = !zoomed;
                }
                5..=8 => {
                    input.push('\x01');
                    input.push(['h', 'j', 'k', 'l'][(operation - 5) as usize]);
                    if !zoomed {
                        match operation {
                            5 => side = 0,
                            8 => side = 1,
                            _ => {}
                        }
                    }
                }
                _ => unreachable!("the generator bounds operations"),
            }
        }
        if zoomed {
            input.push_str("\x1bz");
        }
        // Fence each PTY independently: seeing the second pane's fence
        // cannot establish that the first pane has drained its own input.
        input.push_str("\x01h%\x01l%");
        env.send_keys(&input);
        env.wait_for("FENCE-first");
        env.wait_for("FENCE-second");
        expected[0].push('%');
        expected[1].push('%');
        prop_assert_eq!(&key_log(&env, "first"), &expected[0], "left pane, operations: {:?}", operations);
        prop_assert_eq!(&key_log(&env, "second"), &expected[1], "right pane, operations: {:?}", operations);
    }
}
