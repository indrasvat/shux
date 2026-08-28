//! Integration tests for shux-pty: spawn real PTY processes.

use std::path::PathBuf;
use std::time::Duration;

use shux_pty::{PtyConfig, PtyHandle, PtySize};

fn test_cwd() -> PathBuf {
    std::env::temp_dir()
}

async fn read_pty_to_exit(handle: &mut PtyHandle) -> String {
    let mut output = Vec::new();
    let mut buf = [0u8; 4096];

    let result = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match handle.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => output.extend_from_slice(&buf[..n]),
                Err(_) => break,
            }
        }
    })
    .await;

    assert!(result.is_ok(), "Read timed out");
    String::from_utf8_lossy(&output).into_owned()
}

#[tokio::test]
async fn test_spawn_echo() {
    let config = PtyConfig::with_command(vec!["echo".into(), "hello shux".into()], test_cwd());

    let mut handle = PtyHandle::spawn(&config).unwrap();
    let mut output = Vec::new();
    let mut buf = [0u8; 4096];

    let result = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match handle.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => output.extend_from_slice(&buf[..n]),
                Err(_) => break,
            }
        }
    })
    .await;

    assert!(result.is_ok(), "Read timed out");

    let output_str = String::from_utf8_lossy(&output);
    assert!(
        output_str.contains("hello shux"),
        "Expected 'hello shux' in output, got: {output_str}"
    );
}

#[tokio::test]
async fn test_spawn_interactive_env_enables_color_by_default() {
    let config = PtyConfig::with_command(
        vec![
            "sh".into(),
            "-c".into(),
            "printf '%s|%s|%s|%s\n' \"$TERM\" \"${COLORTERM-unset}\" \"${CLICOLOR-unset}\" \"${NO_COLOR-unset}\"".into(),
        ],
        test_cwd(),
    );

    let mut handle = PtyHandle::spawn(&config).unwrap();
    let output = read_pty_to_exit(&mut handle).await;

    let has_supported_term = ["tmux-256color", "screen-256color", "xterm-256color"]
        .iter()
        .any(|term| output.contains(&format!("{term}|truecolor|1|unset")));
    assert!(
        has_supported_term,
        "expected shux pane color defaults and no inherited NO_COLOR, got: {output:?}"
    );
}

#[tokio::test]
async fn test_colored_startup_burst_reads_without_timeout_stall() {
    let payload = "printf '\\033[38;2;75;85;99m'; yes startup | head -n 200";
    let config =
        PtyConfig::with_command(vec!["sh".into(), "-c".into(), payload.into()], test_cwd());
    let mut handle = PtyHandle::spawn(&config).unwrap();
    let mut buf = [0u8; 8192];

    let result = tokio::time::timeout(Duration::from_millis(500), async {
        let mut output = Vec::new();
        while output.len() < 1024 {
            let n = handle.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            output.extend_from_slice(&buf[..n]);
        }
        output
    })
    .await;

    let output = result.expect("colored startup burst should be readable promptly");
    let output = String::from_utf8_lossy(&output);
    assert!(
        output.contains("startup"),
        "expected startup burst output, got: {output:?}"
    );
}

#[tokio::test]
async fn test_spawn_explicit_env_can_restore_no_color() {
    let mut config = PtyConfig::with_command(
        vec![
            "sh".into(),
            "-c".into(),
            "printf '%s|%s\n' \"$CLICOLOR\" \"${NO_COLOR-unset}\"".into(),
        ],
        test_cwd(),
    );
    config.env.push(("NO_COLOR".into(), "1".into()));
    config.env.push(("CLICOLOR".into(), "0".into()));

    let mut handle = PtyHandle::spawn(&config).unwrap();
    let output = read_pty_to_exit(&mut handle).await;

    assert!(
        output.contains("0|1"),
        "expected explicit config.env to override pane color defaults, got: {output:?}"
    );
}

#[tokio::test]
async fn test_spawn_and_exit_status() {
    let config = PtyConfig::with_command(vec!["true".into()], test_cwd());

    let mut handle = PtyHandle::spawn(&config).unwrap();
    let status = handle.wait().await.unwrap();
    assert!(status.success(), "Expected exit code 0");
}

#[tokio::test]
async fn test_spawn_failing_command() {
    let config = PtyConfig::with_command(vec!["false".into()], test_cwd());

    let mut handle = PtyHandle::spawn(&config).unwrap();
    let status = handle.wait().await.unwrap();
    assert!(!status.success(), "Expected non-zero exit code");
}

#[tokio::test]
async fn test_write_and_read() {
    let config = PtyConfig::with_command(vec!["cat".into()], test_cwd());

    let mut handle = PtyHandle::spawn(&config).unwrap();

    handle.write(b"hello from test\n").await.unwrap();
    handle.flush().await.unwrap();

    let mut buf = [0u8; 4096];
    let mut output = Vec::new();

    let result = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match handle.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    output.extend_from_slice(&buf[..n]);
                    let s = String::from_utf8_lossy(&output);
                    if s.contains("hello from test") {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    })
    .await;

    assert!(result.is_ok(), "Read timed out");

    let output_str = String::from_utf8_lossy(&output);
    assert!(
        output_str.contains("hello from test"),
        "Expected echoed input, got: {output_str}"
    );

    handle.kill().ok();
}

#[tokio::test]
async fn test_resize() {
    let mut config = PtyConfig::default_shell(test_cwd());
    config.size = PtySize::new(80, 24);

    let mut handle = PtyHandle::spawn(&config).unwrap();
    assert_eq!(handle.size().cols, 80);
    assert_eq!(handle.size().rows, 24);

    handle.resize(PtySize::new(120, 40)).unwrap();
    assert_eq!(handle.size().cols, 120);
    assert_eq!(handle.size().rows, 40);

    handle.kill().ok();
}

#[tokio::test]
async fn test_initial_cwd() {
    let cwd = std::env::temp_dir();
    let config = PtyConfig::default_shell(cwd.clone());

    let mut handle = PtyHandle::spawn(&config).unwrap();
    assert_eq!(handle.initial_cwd(), &cwd);

    handle.kill().ok();
}

#[tokio::test]
async fn test_pty_event_output() {
    use shux_pty::manager::{PaneId, PtyEvent};
    use tokio::sync::mpsc;
    use uuid::Uuid;

    let pane_id = PaneId(Uuid::new_v4());
    let (event_tx, mut event_rx) = mpsc::channel(32);
    let shutdown = tokio_util::sync::CancellationToken::new();

    // Use bash -c with a small sleep after the echo. With a bare `echo`,
    // the child exits in ~1ms — fast enough on Linux/CI runners that the
    // master-side read can race with EOF and miss the output entirely.
    // The sleep keeps the slave open long enough for the kernel to flush
    // the writes through to the master before the child reaps. Same
    // pattern is used by tmux / iTerm2 PTY tests for the same reason.
    let config = PtyConfig::with_command(
        vec![
            "bash".into(),
            "-c".into(),
            "echo event test; sleep 0.1".into(),
        ],
        test_cwd(),
    );
    let handle = PtyHandle::spawn(&config).unwrap();

    let shutdown_clone = shutdown.clone();
    tokio::spawn(async move {
        shux_pty::manager::run_pty_read_loop(pane_id, handle, event_tx, shutdown_clone).await;
    });

    let mut got_output = false;
    let mut got_exit = false;

    let result = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = event_rx.recv().await {
            match event {
                PtyEvent::Output { pane_id: pid, data } => {
                    assert_eq!(pid, pane_id);
                    let s = String::from_utf8_lossy(&data);
                    if s.contains("event test") {
                        got_output = true;
                    }
                }
                PtyEvent::Exited {
                    pane_id: pid,
                    exit_code,
                } => {
                    assert_eq!(pid, pane_id);
                    assert_eq!(exit_code, Some(0));
                    got_exit = true;
                    break;
                }
                _ => {}
            }
        }
    })
    .await;

    assert!(result.is_ok(), "Event collection timed out");
    assert!(got_output, "Did not receive output event");
    assert!(got_exit, "Did not receive exit event");
}

/// The read loop must not reap the pane's child (#163 review).
///
/// It watches for the child's exit so it can drop the slave fd it holds
/// (issue #162), and `Child::try_wait` would answer that question by reaping —
/// which frees the pid. That pid is also the pane's process GROUP id, and a
/// pane whose child exits while a descendant keeps the tty open is still a
/// live group that teardown has to signal. Reaping early hands that pgid back
/// to the OS while the group is still running.
///
/// A zombie answers `kill(pid, 0)`; a reaped pid does not.
#[tokio::test]
async fn the_read_loop_leaves_the_child_reapable_for_teardown() {
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;

    // The leader exits immediately; the descendant holds the slave open, so
    // there is no EOF and the loop keeps running with the child gone.
    let config = PtyConfig::with_command(
        vec!["sh".into(), "-c".into(), "sleep 30 & exit 7".into()],
        test_cwd(),
    );
    let mut handle = PtyHandle::spawn(&config).unwrap();
    let pid = Pid::from_raw(handle.pid() as i32);

    // Long enough for several exit polls; the read cannot return, because the
    // descendant is holding the tty open and writing nothing.
    let mut buf = [0u8; 1024];
    let _ = tokio::time::timeout(Duration::from_millis(400), handle.read(&mut buf)).await;

    let still_waitable = nix::sys::signal::kill(pid, None).is_ok();

    // Clean up the whole group before asserting, so a failure cannot leak the
    // `sleep` into the rest of the run.
    let _ = killpg(pid, Signal::SIGKILL);
    let _ = handle.wait().await;

    assert!(
        still_waitable,
        "the read loop reaped the child, so teardown lost its handle on a \
         process group that is still alive"
    );
}

/// A pane child must not inherit the *outer* terminal's identity: a pane still
/// advertising `KITTY_WINDOW_ID` makes every tool that sniffs it address the
/// wrong terminal, silently. See `OUTER_TERMINAL_IDENTITY_VARS`, which this
/// reads rather than restates -- a second copy would drift and leave a
/// newly-added variable covered by nothing.
#[tokio::test]
async fn pane_child_does_not_inherit_outer_terminal_identity() {
    const SENTINEL: &str = "leaked-outer-terminal";

    // Every exact name, plus a synthesised member of each prefix family --
    // including one nobody has written down, which is the point of prefixes.
    let mut expected: Vec<String> = shux_pty::OUTER_TERMINAL_IDENTITY_VARS
        .iter()
        .map(|k| (*k).to_string())
        .collect();
    for prefix in shux_pty::OUTER_TERMINAL_IDENTITY_PREFIXES {
        expected.push(format!("{prefix}WINDOW_ID"));
        expected.push(format!("{prefix}SOMETHING_INVENTED_LATER"));
        // Anchored exemption: the word must be a whole segment. `CONFIG` after
        // the start is identity, and so is `CONFIG` that merely starts the
        // segment -- only anchoring one of the two words let this one through.
        expected.push(format!("{prefix}SESSION_CONFIGURED_ID"));
        expected.push(format!("{prefix}CONFIGURED_ID"));
        expected.push(format!("{prefix}AUTOMATION_ID"));
    }
    expected.push("WINDOW".to_string());

    let pairs: Vec<(&str, &str)> = expected
        .iter()
        .map(|k| (k.as_str(), SENTINEL))
        // Not an emulator handle: proves this is a deny-list and not
        // `env_clear`, which would take the user's whole environment with it.
        .chain(std::iter::once(("SHUX_TEST_UNRELATED_VAR", SENTINEL)))
        .collect();
    let _env = EnvGuard::set(&pairs);

    let mut handle = PtyHandle::spawn(&env_dump_config()).unwrap();
    let output = read_pty_to_exit(&mut handle).await;

    assert!(output.contains("ENV_DONE"), "child did not run: {output}");

    let leaked: Vec<&String> = expected
        .iter()
        .filter(|key| env_has(&output, key, SENTINEL))
        .collect();
    assert!(
        leaked.is_empty(),
        "pane child inherited the outer terminal's identity: {leaked:?}"
    );
    assert!(
        env_has(&output, "SHUX_TEST_UNRELATED_VAR", SENTINEL),
        "the scrub is a deny-list, not env_clear -- an unrelated variable must survive"
    );
    // Backstop, not the guard: the ordering at the scrub site is what makes a
    // collision unrepresentable. This fires only on a reorder AND a collision.
    for (key, value) in [("TERM_PROGRAM", "shux"), ("SHUX", "1")] {
        assert!(
            env_has(&output, key, value),
            "the scrub took shux's own {key}, which it sets itself: {output}"
        );
    }
}

/// Restores every variable it set, including on panic -- so a test that fails
/// mid-way cannot leave sentinels for anything later in its own process.
struct EnvGuard(Vec<(String, Option<std::ffi::OsString>)>);

impl EnvGuard {
    fn set(pairs: &[(&str, &str)]) -> Self {
        let saved = pairs
            .iter()
            .map(|(k, v)| {
                let prior = std::env::var_os(k);
                // SAFETY: nextest gives each test its own process, and the
                // read this races is `PtyHandle::spawn` on this same thread.
                // Restored on drop, so a later test in-process sees the prior
                // value even if this one panics.
                unsafe { std::env::set_var(k, v) };
                ((*k).to_string(), prior)
            })
            .collect();
        Self(saved)
    }

    fn unset(keys: &[&str]) -> Self {
        let saved = keys
            .iter()
            .map(|k| {
                let prior = std::env::var_os(k);
                // SAFETY: as above.
                unsafe { std::env::remove_var(k) };
                ((*k).to_string(), prior)
            })
            .collect();
        Self(saved)
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, prior) in &self.0 {
            // SAFETY: as above.
            match prior {
                Some(value) => unsafe { std::env::set_var(key, value) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
    }
}

/// Line-exact `KEY=VALUE` lookup. `contains` would report `TERM_SESSION_ID` as
/// leaked whenever `ITERM_SESSION_ID` is, sending the reader at the wrong name.
fn env_has(output: &str, key: &str, value: &str) -> bool {
    let needle = format!("{key}={value}");
    output
        .lines()
        .any(|line| line.trim_end_matches('\r') == needle)
}

fn env_dump_config() -> PtyConfig {
    PtyConfig::with_command(
        vec!["sh".into(), "-c".into(), "env; echo ENV_DONE".into()],
        test_cwd(),
    )
}

/// screen's bare `WINDOW` is only screen's when `STY` proves it. Without this
/// test an UNCONDITIONAL `WINDOW` scrub passes the suite, taking a variable
/// that belongs to the user.
#[tokio::test]
async fn window_survives_when_sty_is_unset() {
    let _sty = EnvGuard::unset(&["STY"]);
    let _window = EnvGuard::set(&[("WINDOW", "not-screens-window")]);

    let mut handle = PtyHandle::spawn(&env_dump_config()).unwrap();
    let output = read_pty_to_exit(&mut handle).await;

    assert!(
        env_has(&output, "WINDOW", "not-screens-window"),
        "a non-screen WINDOW was scrubbed even though STY was unset: {output}"
    );
}

/// An EMPTY `STY` is not screen either -- screen always writes a session id --
/// so `is_some()` alone takes a `WINDOW` that belongs to the user.
#[tokio::test]
async fn window_survives_when_sty_is_set_but_empty() {
    let _env = EnvGuard::set(&[("STY", ""), ("WINDOW", "not-screens-window")]);

    let mut handle = PtyHandle::spawn(&env_dump_config()).unwrap();
    let output = read_pty_to_exit(&mut handle).await;

    assert!(
        env_has(&output, "WINDOW", "not-screens-window"),
        "an empty STY does not prove screen set WINDOW, but it was scrubbed: {output}"
    );
}

/// The scrub's documented escape hatch. Nothing else covers env-over-SCRUB --
/// the existing NO_COLOR test covers env-over-DEFAULTS, which is a different
/// ordering.
#[tokio::test]
async fn config_env_restores_a_scrubbed_var() {
    let _outer = EnvGuard::set(&[("KITTY_WINDOW_ID", "outer-value")]);

    let mut config = env_dump_config();
    config
        .env
        .push(("KITTY_WINDOW_ID".to_string(), "restored".to_string()));

    let mut handle = PtyHandle::spawn(&config).unwrap();
    let output = read_pty_to_exit(&mut handle).await;

    assert!(output.contains("ENV_DONE"), "child did not run: {output}");
    assert!(
        env_has(&output, "KITTY_WINDOW_ID", "restored"),
        "PtyConfig::env could not restore a scrubbed variable: {output}"
    );
}

/// See `is_user_config_in_vendor_namespace` for why this boundary exists.
#[tokio::test]
async fn user_config_survives_inside_a_scrubbed_vendor_namespace() {
    const KEPT: &[(&str, &str)] = &[
        ("ZELLIJ_CONFIG_DIR", "cfg-dir"),
        ("ZELLIJ_CONFIG_FILE", "cfg-file"),
        // Bare words: the segment IS the whole remainder. Anchoring to
        // `CONFIG_`/`AUTO_` instead would scrub these.
        ("ZELLIJ_CONFIG", "cfg"),
        ("ZELLIJ_AUTO", "auto"),
        ("ZELLIJ_AUTO_ATTACH", "true"),
        ("KITTY_CONFIG_DIRECTORY", "kitty-cfg"),
        ("WEZTERM_CONFIG_FILE", "wez-cfg"),
    ];
    // Proves the namespace really is scrubbed, so KEPT below means something.
    const SCRUBBED: &[(&str, &str)] = &[("ZELLIJ_SESSION_NAME", "outer-session")];

    let _kept = EnvGuard::set(KEPT);
    let _scrubbed = EnvGuard::set(SCRUBBED);

    let mut handle = PtyHandle::spawn(&env_dump_config()).unwrap();
    let output = read_pty_to_exit(&mut handle).await;

    assert!(output.contains("ENV_DONE"), "child did not run: {output}");
    for (key, value) in KEPT {
        assert!(
            env_has(&output, key, value),
            "user config {key} was scrubbed with the vendor's identity: {output}"
        );
    }
    for (key, value) in SCRUBBED {
        assert!(
            !env_has(&output, key, value),
            "{key} is identity, not config, and must not survive: {output}"
        );
    }
}

/// See `OUTER_TERMINAL_IDENTITY_VARS` for why removing this is worse than
/// leaking it.
#[tokio::test]
async fn colorfgbg_survives_because_removing_it_misleads_vim() {
    let _env = EnvGuard::set(&[("COLORFGBG", "15;0")]);

    let mut handle = PtyHandle::spawn(&env_dump_config()).unwrap();
    let output = read_pty_to_exit(&mut handle).await;

    assert!(
        env_has(&output, "COLORFGBG", "15;0"),
        "COLORFGBG was scrubbed; vim will pick background=light in a dark pane: {output}"
    );
}

/// ncurses prefers `COLUMNS`/`LINES` over the pty ioctl, so a stale pair from
/// the outer terminal makes every `tput cols` script render at the wrong width.
#[tokio::test]
async fn outer_terminal_geometry_does_not_reach_the_child() {
    let _env = EnvGuard::set(&[("COLUMNS", "203"), ("LINES", "51")]);

    let mut handle = PtyHandle::spawn(&env_dump_config()).unwrap();
    let output = read_pty_to_exit(&mut handle).await;

    assert!(output.contains("ENV_DONE"), "child did not run: {output}");
    for (key, value) in [("COLUMNS", "203"), ("LINES", "51")] {
        assert!(
            !env_has(&output, key, value),
            "the outer terminal's {key} reached the pane; `tput cols` will lie: {output}"
        );
    }
}

/// Issue #174 A: a pane child must be TOLD its pixel geometry, at spawn AND on
/// every resize.
///
/// Asserts what the CHILD reads out of `TIOCGWINSZ`, not what the handle
/// remembers -- `test_resize` above only ever checked `handle.size()`, so both
/// call sites could have gone on writing zeros with every test green. The
/// reader runs against its own tty, which is the same fd `vim`, `htop` and
/// terminal-browser read.
///
/// Run against the tree before the fix, both lines report `0 0`.
#[tokio::test]
async fn winsize_declares_pixel_geometry_to_the_child_at_spawn_and_on_resize() {
    let Some(python) = which_python() else {
        eprintln!("skipping: no python3 on PATH");
        return;
    };
    // Prints the four winsize fields, then polls until the cell geometry
    // actually changes and prints again. Polling rather than sleeping: a fixed
    // sleep races the parent's ioctl and would read the OLD size, manufacturing
    // a failure that means nothing.
    let program = r#"
import fcntl, struct, sys, termios, time
def ws():
    return struct.unpack("HHHH", fcntl.ioctl(sys.stdout.fileno(), termios.TIOCGWINSZ, b"\0" * 8))
r, c, x, y = ws()
print("SPAWN %d %d %d %d" % (c, r, x, y), flush=True)
r2, c2, x2, y2 = r, c, x, y
deadline = time.time() + 10
while time.time() < deadline:
    r2, c2, x2, y2 = ws()
    if (c2, r2) != (c, r):
        break
    time.sleep(0.02)
print("RESIZED %d %d %d %d" % (c2, r2, x2, y2), flush=True)
"#;
    let mut config = PtyConfig::with_command(
        vec![python, "-c".to_string(), program.to_string()],
        test_cwd(),
    );
    config.size = PtySize::new(80, 24);
    let (cell_w, cell_h) = shux_pty::DECLARED_CELL_PIXELS;

    let mut handle = PtyHandle::spawn(&config).unwrap();
    // Resize only AFTER the child has reported the spawn geometry. Resizing
    // first would let the child's very first read return the NEW size, so it
    // would never observe a change, and the test would time out rather than
    // measure anything.
    let mut output = read_pty_until(&mut handle, "SPAWN").await;
    handle.resize(PtySize::new(120, 40)).unwrap();
    output.push_str(&read_pty_until(&mut handle, "RESIZED").await);
    handle.kill().ok();

    let line = |tag: &str| -> (u16, u16, u16, u16) {
        let l = output
            .lines()
            .find(|l| l.trim_start().starts_with(tag))
            .unwrap_or_else(|| panic!("no {tag} line in child output: {output:?}"));
        let n: Vec<u16> = l
            .split_whitespace()
            .skip(1)
            .map(|v| v.parse().expect("winsize field"))
            .collect();
        assert_eq!(n.len(), 4, "malformed {tag} line: {l:?}");
        (n[0], n[1], n[2], n[3])
    };

    let (cols, rows, xpixel, ypixel) = line("SPAWN");
    assert_eq!((cols, rows), (80, 24), "child read the wrong cell geometry");
    assert_eq!(
        (xpixel, ypixel),
        (80 * cell_w, 24 * cell_h),
        "spawn declared no pixel geometry (0/0 is the pre-#174 defect)"
    );

    let (cols, rows, xpixel, ypixel) = line("RESIZED");
    assert_eq!((cols, rows), (120, 40), "child never saw the resize");
    assert_eq!(
        (xpixel, ypixel),
        (120 * cell_w, 40 * cell_h),
        "resize dropped the pixel geometry the spawn declared"
    );
}

/// Read the PTY until `needle` appears. Panics on timeout rather than returning
/// what it has: a partial read that silently satisfies a later `find` is how a
/// test ends up asserting nothing.
async fn read_pty_until(handle: &mut PtyHandle, needle: &str) -> String {
    let mut output = Vec::new();
    let mut buf = [0u8; 4096];
    let got = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match handle.read(&mut buf).await {
                Ok(0) => return false,
                Ok(n) => {
                    output.extend_from_slice(&buf[..n]);
                    if String::from_utf8_lossy(&output).contains(needle) {
                        return true;
                    }
                }
                Err(_) => return false,
            }
        }
    })
    .await;
    let text = String::from_utf8_lossy(&output).to_string();
    assert!(
        matches!(got, Ok(true)),
        "never saw {needle:?} in child output: {text:?}"
    );
    text
}

fn which_python() -> Option<String> {
    for candidate in ["python3", "python"] {
        if std::process::Command::new(candidate)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
        {
            return Some(candidate.to_string());
        }
    }
    None
}
