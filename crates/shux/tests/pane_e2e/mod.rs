//! Shared harness for daemon-backed pane end-to-end tests: a real daemon in an
//! isolated `XDG_RUNTIME_DIR`, real panes running real shell scripts, and the
//! same CLI an operator runs. The daemon is identified by pidfile and must be
//! gone when the test ends.

#![allow(dead_code)]

use std::path::PathBuf;
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

/// Truecolor + 256-indexed + basic ANSI, all on one line. Mandatory on every
/// daemon-backed capture (see CLAUDE.md).
pub const COLOUR_PROBE: &str = "\\033[38;2;120;220;180mTRUECOLOR\\033[0m \\033[38;5;208mINDEXED\\033[0m \\033[34mBASIC\\033[0m";

pub struct Env {
    pub cols: usize,
    pub rows: usize,
    bin: PathBuf,
    root: tempfile::TempDir,
    sessions: Vec<String>,
}

impl Env {
    pub fn new(cols: usize, rows: usize) -> Self {
        let root = tempfile::tempdir().expect("temp root");
        for sub in ["runtime", "config/shux", "work"] {
            std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
        }
        Self {
            cols,
            rows,
            bin: PathBuf::from(env!("CARGO_BIN_EXE_shux")),
            root,
            sessions: Vec::new(),
        }
    }

    pub fn shux(&self) -> Command {
        let mut cmd = Command::new(&self.bin);
        cmd.env("XDG_RUNTIME_DIR", self.root.path().join("runtime"))
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env_remove("SHUX_SOCKET")
            .env("NO_COLOR", "1")
            .env("SHELL", "/bin/sh")
            .current_dir(self.root.path().join("work"));
        cmd
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.shux().args(args).output().expect("spawn shux")
    }

    #[track_caller]
    pub fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{args:?} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// Spawn a pane running `script` under `sh`, sized to `cols` x `rows`, and
    /// return its pane id. The script is kept alive afterwards so the pane's
    /// screen is not torn down before it is captured.
    #[track_caller]
    pub fn pane_running(&mut self, session: &str, script: &str) -> String {
        // The pane starts at the daemon's default size; the script waits for
        // the resize so it draws on the geometry the test asked for.
        let go = self.root.path().join("work").join(format!("{session}.go"));
        let _ = std::fs::remove_file(&go);
        let body = format!(
            "while [ ! -e '{}' ]; do sleep 0.05; done\n{script}\nexec sleep 900\n",
            go.display()
        );
        let path = self.root.path().join("work").join(format!("{session}.sh"));
        std::fs::write(&path, &body).expect("write script");

        self.ok(&[
            "session",
            "create",
            session,
            "-d",
            "--",
            "sh",
            // A tempdir path built from ASCII components is valid UTF-8.
            path.to_str().unwrap(),
        ]);
        self.sessions.push(session.to_string());

        let json = self.ok(&["--format", "json", "pane", "list", "-s", session]);
        let v: serde_json::Value = serde_json::from_str(&json).expect("pane list json");
        let pane = v[0]["id"].as_str().expect("pane id").to_string();

        self.ok(&[
            "pane",
            "set-size",
            "-s",
            session,
            "-p",
            &pane,
            "--cols",
            &self.cols.to_string(),
            "--rows",
            &self.rows.to_string(),
        ]);
        std::fs::write(&go, b"go").expect("release the pane script");
        pane
    }

    pub fn wait_for(&self, session: &str, pane: &str, needle: &str) {
        // A deadline for reporting failure, not an assertion: the screen is
        // checked right after. Generous because the coverage job runs the whole
        // workspace instrumented and in parallel.
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
            "60000",
        ]);
        assert!(
            out.status.success(),
            "pane never showed {needle:?}; screen was:\n{}",
            self.capture(session, pane)
        );
    }

    pub fn capture(&self, session: &str, pane: &str) -> String {
        self.ok(&["pane", "capture", "-s", session, "-p", pane])
    }

    /// The pane's visible rows, padded to the full width — `pane glance` is
    /// byte-stable in a way `pane capture` deliberately is not, which is what
    /// makes "every cell on every row" assertable.
    pub fn glance_rows(&self, pane: &str) -> Vec<String> {
        let json = self.ok(&["--format", "json", "pane", "glance", pane, "--text-only"]);
        let v: serde_json::Value = serde_json::from_str(&json).expect("glance json");
        let text = v["result"]["text"]
            .as_str()
            .or_else(|| v["text"].as_str())
            .unwrap_or_else(|| panic!("no text in glance output: {json}"));
        text.lines().map(str::to_string).collect()
    }
    /// The pane's styled cell frame, from `pane glance --cells`.
    pub fn cells(&self, pane: &str) -> serde_json::Value {
        let json = self.ok(&["--format", "json", "pane", "glance", pane, "--cells"]);
        let v: serde_json::Value = serde_json::from_str(&json).expect("glance json");
        v.get("result").unwrap_or(&v)["cells"].clone()
    }

    /// Assert the colour probe reached the frame in all three colour forms, so
    /// a monochrome or `NO_COLOR` regression cannot pass on the words alone.
    #[track_caller]
    pub fn assert_colour_probe(&self, pane: &str) {
        let cells = self.cells(pane);
        let styles: Vec<&serde_json::Value> = cells["rows"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|row| row["runs"].as_array().into_iter().flatten())
            .filter_map(|run| run.get(2))
            .collect();
        let has = |want: serde_json::Value| styles.iter().any(|s| s["fg"] == want);
        assert!(
            has(serde_json::json!({"rgb": [120, 220, 180]})),
            "truecolor probe: {cells}"
        );
        assert!(
            has(serde_json::json!({"idx": 208})),
            "indexed probe: {cells}"
        );
        assert!(has(serde_json::json!({"idx": 4})), "basic probe: {cells}");
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
            // A second panic during unwinding aborts and hides the first.
            if !thread::panicking() {
                panic!("daemon {pid} outlived the test");
            }
        }
    }
}
