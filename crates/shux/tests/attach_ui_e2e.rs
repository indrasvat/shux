//! End-to-end: what the attach UI draws for the help overlay and the copy-mode
//! cursor (issue #224).
//!
//! The real attach client runs inside a host shux pane, so its output is the
//! host pane's screen, read back with `pane capture`. Real daemon, real config
//! file, real PTYs, real keystrokes. The target pane prints the colour probe
//! (truecolor + 256-indexed + basic ANSI) so a monochrome regression cannot pass.

use std::path::PathBuf;
use std::process::{Command, Output};

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
        let root = tempfile::tempdir().expect("temp root");
        for sub in ["runtime", "config/shux", "work"] {
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
        env.ok(&[
            "pane", "set-size", "-s", "host", "--cols", "100", "--rows", "40",
        ]);
        env.session_with_argv("target", &["/bin/sh", "-c", TARGET_SCRIPT]);

        let bin = env.bin.display().to_string();
        let root = env.root.path().display().to_string();
        env.send_keys(&format!(
            "XDG_RUNTIME_DIR={root}/runtime XDG_CONFIG_HOME={root}/config \
             {bin} session attach target\n"
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
            .env_remove("SHUX_SOCKET")
            .env("NO_COLOR", "1")
            .env("SHELL", "/bin/sh")
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
