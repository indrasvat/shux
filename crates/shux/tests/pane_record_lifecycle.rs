//! `shux pane record` and the recording it starts end together: whichever of
//! the command, its pane or the daemon goes first, the others follow.

use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const BUDGET: Duration = Duration::from_secs(15);

/// Truecolor, indexed and basic colour, so a recording that dropped or
/// rewrote escapes cannot pass.
const COLOUR_PROBE: &str = "\\033[38;2;120;220;180mT\\033[38;5;208mI\\033[34mB\\033[0m";

struct Rig {
    bin: PathBuf,
    runtime: tempfile::TempDir,
    out: tempfile::TempDir,
}

impl Rig {
    fn new() -> Self {
        Self {
            bin: PathBuf::from(env!("CARGO_BIN_EXE_shux")),
            runtime: tempfile::tempdir().expect("runtime dir"),
            out: tempfile::tempdir().expect("output dir"),
        }
    }

    fn cmd(&self) -> Command {
        let mut cmd = Command::new(&self.bin);
        cmd.env("XDG_RUNTIME_DIR", self.runtime.path())
            .env("SHELL", "/bin/sh")
            .env_remove("SHUX");
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd().args(args).output().expect("run shux")
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "shux {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// A session whose pane runs `sh`, and that pane's id.
    fn session(&self, name: &str) -> String {
        self.ok(&["session", "create", name, "-d", "--", "sh"]);
        let list = self.ok(&["--format", "json", "pane", "list", "-s", name]);
        let panes: serde_json::Value = serde_json::from_str(&list).expect("pane list json");
        panes[0]["id"].as_str().expect("pane id").to_string()
    }

    fn type_line(&self, session: &str, pane: &str, line: &str) {
        self.ok(&[
            "pane",
            "send-keys",
            "-s",
            session,
            "-p",
            pane,
            "--text",
            line,
        ]);
        self.ok(&[
            "pane",
            "send-keys",
            "-s",
            session,
            "-p",
            pane,
            "--data",
            "DQ==",
        ]);
    }

    /// Wait for `needle` on screen; the line that prints it is split so the
    /// echo of the typed command cannot satisfy the wait.
    fn wait_text(&self, session: &str, pane: &str, needle: &str) {
        let out = self.run(&[
            "pane",
            "wait-for",
            "-s",
            session,
            "-p",
            pane,
            "--text",
            needle,
            "--timeout-ms",
            "15000",
        ]);
        assert!(
            out.status.success(),
            "{needle} never appeared in {session}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn path(&self, name: &str) -> PathBuf {
        self.out.path().join(name)
    }

    /// Start `shux pane record` and return once its recording is running.
    fn record(&self, session: &str, pane: &str, to: &Path) -> Recorder {
        let mut child = self
            .cmd()
            .args([
                "pane", "record", "-s", session, "-p", pane, "--force", "--to",
            ])
            .arg(to)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn pane record");
        let stderr = Arc::new(Mutex::new(String::new()));
        let mut pipe = child.stderr.take().expect("stderr");
        let sink = stderr.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            while let Ok(n) = pipe.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock()
                    .expect("stderr sink")
                    .push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        });
        let recorder = Recorder { child, stderr };
        shux_testkit::wait_until("the recording to start", || {
            recorder.stderr().contains("press Ctrl-C")
        });
        recorder
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let pid: Option<i32> = std::fs::read_to_string(self.runtime.path().join("shux/shux.pid"))
            .ok()
            .and_then(|s| s.trim().parse().ok());
        // By pidfile, never by an argv substring.
        if let Some(pid) = pid
            && !shux_testkit::terminate(pid)
            && !std::thread::panicking()
        {
            panic!("daemon {pid} survived SIGTERM");
        }
    }
}

struct Recorder {
    child: Child,
    stderr: Arc<Mutex<String>>,
}

impl Recorder {
    fn stderr(&self) -> String {
        self.stderr.lock().expect("stderr sink").clone()
    }

    fn signal(&self, sig: nix::sys::signal::Signal) {
        let pid = nix::unistd::Pid::from_raw(self.child.id() as i32);
        nix::sys::signal::kill(pid, sig).expect("signal the recorder");
    }

    /// Its exit status and stdout, if it exits by itself within the budget.
    fn exits(&mut self) -> Option<(std::process::ExitStatus, String)> {
        let status = shux_testkit::poll(BUDGET, || self.child.try_wait().expect("try_wait"))?;
        let mut stdout = String::new();
        if let Some(mut out) = self.child.stdout.take() {
            out.read_to_string(&mut stdout).expect("read stdout");
        }
        Some((status, stdout))
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[test]
fn stopping_the_daemon_ends_the_command_with_an_error() {
    let rig = Rig::new();
    let pane = rig.session("ds");
    let mut rec = rig.record("ds", &pane, &rig.path("ds.raw"));

    rig.ok(&["daemon", "stop"]);

    let (status, _) = rec
        .exits()
        .expect("`pane record` kept waiting after its daemon stopped");
    assert!(!status.success(), "it reported success: {}", rec.stderr());
    assert!(
        rec.stderr().contains("daemon"),
        "the error does not say the daemon went away: {}",
        rec.stderr()
    );
}

#[test]
fn the_recorded_pane_exiting_ends_the_command() {
    let rig = Rig::new();
    let pane = rig.session("px");
    let to = rig.path("px.raw");
    let mut rec = rig.record("px", &pane, &to);

    rig.type_line(
        "px",
        &pane,
        &format!("printf '{COLOUR_PROBE}PX%s\\n' DONE; exit 0"),
    );

    let (status, stdout) = rec
        .exits()
        .expect("`pane record` kept waiting after its pane exited");
    assert!(status.success(), "{}", rec.stderr());
    assert!(stdout.contains("complete"), "no outcome reported: {stdout}");
    let raw = std::fs::read(&to).expect("recording");
    let raw = String::from_utf8_lossy(&raw);
    assert!(
        raw.contains("PXDONE"),
        "the last output is missing: {raw:?}"
    );
    assert!(
        raw.contains("\x1b[38;2;120;220;180mT\x1b[38;5;208mI\x1b[34mB\x1b[0m"),
        "colour escapes were not recorded verbatim: {raw:?}"
    );
}

/// A command ended by a signal it can catch stops its recording and says what
/// it wrote; one that cannot catch it (SIGKILL) has its recording stopped by
/// the daemon. Either way, later output stays out of the file and the pane
/// can be recorded again.
fn a_signalled_command_takes_its_recording_with_it(sig: nix::sys::signal::Signal, tag: &str) {
    let rig = Rig::new();
    let pane = rig.session(tag);
    let to = rig.path("first.raw");
    let mut rec = rig.record(tag, &pane, &to);
    rig.type_line(tag, &pane, &format!("printf '{COLOUR_PROBE}BEF%s\\n' ORE"));
    rig.wait_text(tag, &pane, "BEFORE");

    rec.signal(sig);
    rec.exits().expect("the signalled command did not exit");

    // A new recording is accepted only once the old one has stopped.
    let again = rig.path("second.raw");
    let accepted = shux_testkit::poll_until(BUDGET, || {
        rig.run(&[
            "pane",
            "record",
            "-s",
            tag,
            "-p",
            &pane,
            "--to",
            again.to_str().expect("utf-8"),
            "--force",
            "--duration-ms",
            "50",
        ])
        .status
        .success()
    });
    assert!(accepted, "the pane's recording outlived the {sig} command");

    rig.type_line(tag, &pane, "printf 'AFT%s\\n' ER");
    rig.wait_text(tag, &pane, "AFTER");
    let raw = String::from_utf8_lossy(&std::fs::read(&to).expect("recording")).into_owned();
    assert!(
        raw.contains("BEFORE"),
        "output before the signal is missing: {raw:?}"
    );
    assert!(
        !raw.contains("AFTER"),
        "output after the {sig} reached the file: {raw:?}"
    );
}

#[test]
fn sigterm_takes_the_recording_with_the_command() {
    a_signalled_command_takes_its_recording_with_it(nix::sys::signal::Signal::SIGTERM, "term");
}

#[test]
fn sighup_takes_the_recording_with_the_command() {
    a_signalled_command_takes_its_recording_with_it(nix::sys::signal::Signal::SIGHUP, "hup");
}

#[test]
fn sigkill_takes_the_recording_with_the_command() {
    a_signalled_command_takes_its_recording_with_it(nix::sys::signal::Signal::SIGKILL, "kill");
}

/// Everything the pane printed before `daemon stop` is in the file once the
/// daemon has gone.
#[test]
fn a_daemon_stop_leaves_the_whole_recording_on_disk() {
    let rig = Rig::new();
    let pane = rig.session("tail");
    let to = rig.path("tail.raw");
    let _rec = rig.record("tail", &pane, &to);
    // About 4 MiB, then a marker: the recorder is still writing when the
    // daemon is told to stop.
    rig.type_line(
        "tail",
        &pane,
        &format!(
            "i=0; while [ $i -lt 65536 ]; do printf '{COLOUR_PROBE}%060d\\n' $i; i=$((i+1)); done; printf 'TA%s\\n' IL"
        ),
    );
    rig.wait_text("tail", &pane, "TAIL");
    rig.ok(&["daemon", "stop"]);

    let raw = std::fs::read(&to).expect("recording");
    let text = String::from_utf8_lossy(&raw);
    let last = format!("{:060}", 65535);
    assert!(
        text.contains(&last) && text.contains("TAIL"),
        "the recording lost its tail: {} bytes, ends {:?}",
        raw.len(),
        &text[text.len().saturating_sub(120)..]
    );
}

/// Scripts that start a recording on one connection and stop it on another
/// keep working: an unowned recording outlives the connection that started it.
#[test]
fn a_recording_started_and_stopped_by_separate_calls_still_works() {
    let rig = Rig::new();
    let pane = rig.session("rpc");
    let to = rig.path("rpc.raw");
    let start = rig.ok(&[
        "--format",
        "json",
        "rpc",
        "call",
        "pane.record.start",
        "--params",
        &serde_json::json!({ "pane_id": pane, "path": to }).to_string(),
    ]);
    let start: serde_json::Value = serde_json::from_str(&start).expect("start json");
    let id = start["result"]["recording_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no recording_id in {start}"))
        .to_string();

    rig.type_line("rpc", &pane, &format!("printf '{COLOUR_PROBE}RP%s\\n' C"));
    rig.wait_text("rpc", &pane, "RPC");

    let stop = rig.ok(&[
        "--format",
        "json",
        "rpc",
        "call",
        "pane.record.stop",
        "--params",
        &serde_json::json!({ "recording_id": id }).to_string(),
    ]);
    assert!(stop.contains("\"complete\""), "{stop}");
    let raw = String::from_utf8_lossy(&std::fs::read(&to).expect("recording")).into_owned();
    assert!(raw.contains("RPC"), "{raw:?}");
}

/// How a seeded schedule ends its recording.
#[derive(Clone, Copy, Debug)]
enum Ending {
    Signal(nix::sys::signal::Signal),
    PaneExit,
    DaemonStop,
}

/// Seeded schedules over the real binary: some output, a refused second
/// recorder, then one ending. Whatever the ending, the command exits with
/// the status the contract gives it, the file holds the output from before
/// the ending, and (daemon still up) the pane can be recorded again.
#[test]
fn seeded_schedules_end_command_and_recording_together() {
    use nix::sys::signal::Signal::{SIGHUP, SIGINT, SIGKILL, SIGTERM};
    let endings = [
        Ending::Signal(SIGINT),
        Ending::Signal(SIGTERM),
        Ending::Signal(SIGHUP),
        Ending::Signal(SIGKILL),
        Ending::PaneExit,
        Ending::DaemonStop,
    ];
    let mut seed: u64 = 0x2370_5eed;
    let mut next = move |n: u64| {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (seed >> 33) % n
    };
    for round in 0..endings.len() * 2 {
        let ending = endings[round % endings.len()];
        let rig = Rig::new();
        let s = format!("s{round}");
        let pane = rig.session(&s);
        let to = rig.path("rec.raw");
        let mut rec = rig.record(&s, &pane, &to);
        let lines = 1 + next(4);
        for i in 0..lines {
            rig.type_line(
                &s,
                &pane,
                &format!("printf '{COLOUR_PROBE}L{round}x%s\\n' {i}"),
            );
            rig.wait_text(&s, &pane, &format!("L{round}x{i}"));
        }
        if next(2) == 0 {
            let refused = rig.run(&[
                "pane",
                "record",
                "-s",
                &s,
                "-p",
                &pane,
                "--to",
                rig.path("second.raw").to_str().expect("utf-8"),
                "--duration-ms",
                "50",
            ]);
            assert!(
                !refused.status.success(),
                "round {round}: a second recorder was admitted"
            );
            assert!(
                String::from_utf8_lossy(&refused.stderr).contains("already running"),
                "round {round}: {}",
                String::from_utf8_lossy(&refused.stderr)
            );
        }
        match ending {
            Ending::Signal(sig) => rec.signal(sig),
            Ending::PaneExit => rig.type_line(&s, &pane, "exit 0"),
            Ending::DaemonStop => {
                rig.ok(&["daemon", "stop"]);
            }
        }
        let (status, _) = rec
            .exits()
            .unwrap_or_else(|| panic!("round {round} {ending:?}: the command never exited"));
        match ending {
            Ending::Signal(SIGKILL) => assert_eq!(status.signal(), Some(9)),
            Ending::DaemonStop => assert!(
                !status.success(),
                "round {round}: daemon stop reported success"
            ),
            _ => assert!(
                status.success(),
                "round {round} {ending:?}: {}",
                rec.stderr()
            ),
        }
        let file = String::from_utf8_lossy(&std::fs::read(&to).expect("recording")).into_owned();
        for i in 0..lines {
            assert!(
                file.contains(&format!("L{round}x{i}")),
                "round {round} {ending:?}: line {i} is missing from the file"
            );
        }
        if !matches!(ending, Ending::DaemonStop | Ending::PaneExit) {
            let again = rig.path("again.raw");
            let accepted = shux_testkit::poll_until(BUDGET, || {
                rig.run(&[
                    "pane",
                    "record",
                    "-s",
                    &s,
                    "-p",
                    &pane,
                    "--to",
                    again.to_str().expect("utf-8"),
                    "--force",
                    "--duration-ms",
                    "50",
                ])
                .status
                .success()
            });
            assert!(
                accepted,
                "round {round} {ending:?}: the pane could not be recorded again"
            );
        }
    }
}

/// A FIFO nobody reads would block the open forever; it is refused at once.
#[test]
fn a_fifo_nobody_reads_is_refused_at_once() {
    let rig = Rig::new();
    let pane = rig.session("fifo");
    let fifo = rig.path("unread.fifo");
    nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRWXU).expect("mkfifo");
    let mut child = rig
        .cmd()
        .args([
            "pane",
            "record",
            "-s",
            "fifo",
            "-p",
            &pane,
            "--force",
            "--duration-ms",
            "100",
            "--to",
        ])
        .arg(&fifo)
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn pane record");
    let status = shux_testkit::poll(BUDGET, || child.try_wait().expect("try_wait"));
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
        panic!("recording to a FIFO nobody reads never returned");
    }
    let mut err = String::new();
    child
        .stderr
        .take()
        .expect("stderr")
        .read_to_string(&mut err)
        .expect("stderr");
    assert!(!status.expect("status").success(), "{err}");
    assert!(err.contains("reading"), "the error does not say why: {err}");
}

/// A recording stuck writing to a reader that stopped reading cannot keep
/// the daemon alive past `daemon stop`.
#[test]
fn a_stalled_reader_does_not_keep_the_daemon_alive() {
    let rig = Rig::new();
    let pane = rig.session("stall");
    let fifo = rig.path("stalled.fifo");
    nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRWXU).expect("mkfifo");
    // A reader that never reads: the pipe fills and the writer blocks.
    let _reader = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK)
        .open(&fifo)
        .expect("open the FIFO's read end");
    let _rec = rig.record("stall", &pane, &fifo);
    rig.type_line(
        "stall",
        &pane,
        &format!("i=0; while [ $i -lt 8000 ]; do printf '{COLOUR_PROBE}%060d\\n' $i; i=$((i+1)); done; printf 'FU%s\\n' LL"),
    );
    // The recording backpressures the pane, so output stops once the pipe
    // is full: wait for an early line, not for the end.
    let seen = rig.run(&[
        "pane",
        "wait-for",
        "-s",
        "stall",
        "-p",
        &pane,
        "--regex",
        "[0-9]{60}",
        "--timeout-ms",
        "15000",
    ]);
    assert!(seen.status.success(), "the output never started");
    let pid: i32 = std::fs::read_to_string(rig.runtime.path().join("shux/shux.pid"))
        .expect("pidfile")
        .trim()
        .parse()
        .expect("pid");
    let _ = rig.run(&["daemon", "stop"]);
    let gone = shux_testkit::poll_until(BUDGET, || {
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err()
    });
    if !gone {
        let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), nix::sys::signal::SIGKILL);
        panic!("the daemon outlived `daemon stop` behind a stalled recording");
    }
}
