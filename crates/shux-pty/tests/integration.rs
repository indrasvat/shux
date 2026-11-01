//! Integration tests for shux-pty: spawn real PTY processes.

use std::path::PathBuf;
use std::time::Duration;

use shux_pty::{PtyConfig, PtyHandle, PtySize};

fn test_cwd() -> PathBuf {
    std::env::temp_dir()
}

/// Read the PTY to EOF. A read error or a stuck read fails the test with what
/// was read so far, rather than handing a truncated capture to the caller.
async fn read_pty_to_exit(handle: &mut PtyHandle) -> String {
    let mut output = Vec::new();
    let mut buf = [0u8; 4096];

    let result = tokio::time::timeout(shux_testkit::BUDGET, async {
        loop {
            match handle.read(&mut buf).await {
                Ok(0) => return Ok(()),
                Ok(n) => output.extend_from_slice(&buf[..n]),
                Err(e) => return Err(e),
            }
        }
    })
    .await;

    let text = String::from_utf8_lossy(&output).into_owned();
    match result {
        Ok(Ok(())) => text,
        Ok(Err(e)) => panic!("PTY read failed before EOF ({e}); read so far: {text:?}"),
        Err(_) => panic!("PTY never reached EOF; read so far: {text:?}"),
    }
}

#[tokio::test]
async fn test_spawn_echo() {
    let config = PtyConfig::with_command(vec!["echo".into(), "hello shux".into()], test_cwd());

    let mut handle = PtyHandle::spawn(&config).unwrap();
    let output_str = read_pty_to_exit(&mut handle).await;
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

    let result = tokio::time::timeout(shux_testkit::BUDGET, async {
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

    read_pty_until(&mut handle, "hello from test").await;
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

    // A bare `echo` that exits at once: the handle keeps the slave open
    // until the output is drained, so the child need not linger for it.
    let config = PtyConfig::with_command(vec!["echo".into(), "event test".into()], test_cwd());
    let handle = PtyHandle::spawn(&config).unwrap();

    let shutdown_clone = shutdown.clone();
    tokio::spawn(async move {
        shux_pty::manager::run_pty_read_loop(pane_id, handle, event_tx, shutdown_clone).await;
    });

    let mut got_output = false;
    let mut got_exit = false;

    let result = tokio::time::timeout(shux_testkit::BUDGET, async {
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
#[tokio::test(start_paused = true)]
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

    // The exit polls only mean something once there is an exit to see.
    shux_testkit::wait_until("the pane's leader to exit", || is_zombie(handle.pid()));
    // The clock is paused, so this second is fifty exit polls, each run
    // against the zombie. The read cannot return: the descendant holds the
    // tty open and writes nothing.
    let mut buf = [0u8; 1024];
    let _ = tokio::time::timeout(Duration::from_secs(1), handle.read(&mut buf)).await;

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

/// Whether `pid` has exited and is waiting to be reaped.
fn is_zombie(pid: u32) -> bool {
    std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .is_ok_and(|out| {
            String::from_utf8_lossy(&out.stdout)
                .trim_start()
                .starts_with('Z')
        })
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

/// A pane child must be TOLD its pixel geometry, at spawn AND on every resize.
///
/// Asserts what the CHILD reads from `TIOCGWINSZ`, not what the handle
/// remembers: `test_resize` only checks `handle.size()`, so both call sites
/// could go on writing zeros with every test green.
#[tokio::test]
async fn winsize_declares_pixel_geometry_to_the_child_at_spawn_and_on_resize() {
    let python = which_python().expect(
        "python3 or python is required: nothing else asserts a child reads \
         non-zero pixels out of TIOCGWINSZ",
    );
    // Prints winsize, then waits for the SIGWINCH the resize sends and prints
    // again. SIGWINCH is blocked before SPAWN is printed, so a resize that
    // lands before the wait stays pending instead of being lost. It needs a
    // handler too: macOS discards a default-ignored signal even while blocked.
    let program = r#"
import fcntl, signal, struct, sys, termios
def ws():
    return struct.unpack("HHHH", fcntl.ioctl(sys.stdout.fileno(), termios.TIOCGWINSZ, b"\0" * 8))
signal.signal(signal.SIGWINCH, lambda *a: None)
signal.pthread_sigmask(signal.SIG_BLOCK, [signal.SIGWINCH])
r, c, x, y = ws()
print("SPAWN %d %d %d %d" % (c, r, x, y), flush=True)
r2, c2, x2, y2 = r, c, x, y
while (c2, r2) == (c, r):
    signal.sigwait([signal.SIGWINCH])
    r2, c2, x2, y2 = ws()
print("RESIZED %d %d %d %d" % (c2, r2, x2, y2), flush=True)
"#;
    let mut config = PtyConfig::with_command(
        vec![python, "-c".to_string(), program.to_string()],
        test_cwd(),
    );
    config.size = PtySize::new(80, 24);
    let (cell_w, cell_h) = shux_pty::DECLARED_CELL_PIXELS;

    let mut handle = PtyHandle::spawn(&config).unwrap();
    // Resize only after the child has reported SPAWN: resize first and its
    // first read already sees the new size, so it never observes a change.
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
    let got = tokio::time::timeout(shux_testkit::BUDGET, async {
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

/// A handle dropped while its child still runs (a pane task that panicked,
/// for one) must not leave the child behind with nobody to reap it.
#[tokio::test]
async fn dropping_a_live_handle_kills_and_reaps_its_child() {
    let config = PtyConfig::with_command(vec!["sleep".into(), "30".into()], test_cwd());
    let handle = PtyHandle::spawn(&config).unwrap();
    let pid = nix::unistd::Pid::from_raw(handle.pid() as i32);
    drop(handle);
    shux_testkit::wait_until("the dropped handle's child to be reaped", || {
        nix::sys::signal::kill(pid, None).is_err()
    });
}

/// A pane's process must hold its own terminal on 0-2 and nothing else of the
/// daemon's: an inherited master of ANOTHER pane lets it read and type into
/// that pane, and an inherited master of its own keeps it from ever getting a
/// hangup, so it outlives the daemon.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_pane_inherits_no_other_terminal_descriptors() {
    let first = PtyHandle::spawn(&PtyConfig::with_command(
        vec!["sleep".into(), "30".into()],
        test_cwd(),
    ))
    .unwrap();
    // The marker is printed by the running shell, so its exec, and with it
    // every close-on-exec, is done when it arrives.
    let mut second = PtyHandle::spawn(&PtyConfig::with_command(
        vec![
            "sh".into(),
            "-c".into(),
            "echo FDS_SETTLED; sleep 30".into(),
        ],
        test_cwd(),
    ))
    .unwrap();
    read_pty_until(&mut second, "FDS_SETTLED").await;

    let leaked = terminal_descriptors(second.pid());
    let _ = second.kill();
    drop(first);
    assert!(
        leaked.is_empty(),
        "pane child inherited terminal descriptors: {leaked:?}"
    );
}

/// A child forked by any other thread while a pane is being opened must not
/// inherit that pane's terminal either.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
async fn a_process_forked_while_a_pane_opens_inherits_no_terminal_descriptors() {
    use std::io::{Read, Write};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    // Set on every exit, so a panic below cannot leave the opener forking.
    struct Stop(Arc<AtomicBool>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }
    let stop = Arc::new(AtomicBool::new(false));
    let stopper = Stop(Arc::clone(&stop));
    let opener = {
        let stop = Arc::clone(&stop);
        let runtime = tokio::runtime::Handle::current();
        std::thread::spawn(move || {
            let _runtime = runtime.enter();
            while !stop.load(Ordering::Relaxed) {
                let mut pane =
                    PtyHandle::spawn(&PtyConfig::with_command(vec!["true".into()], test_cwd()))
                        .unwrap();
                let _ = pane.kill();
            }
        })
    };
    // Bounded by attempts as well as time: the forks churn the machine's pid
    // space, which every other test shares.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut leaked = Vec::new();
    for _ in 0..2000 {
        if !leaked.is_empty() || std::time::Instant::now() >= deadline {
            break;
        }
        let mut child = std::process::Command::new("cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        // A byte back from `cat` proves its exec, close-on-exec included, is done.
        let mut echoed = [0u8; 1];
        child.stdin.as_mut().unwrap().write_all(b"x").unwrap();
        child
            .stdout
            .as_mut()
            .unwrap()
            .read_exact(&mut echoed)
            .unwrap();
        leaked = terminal_descriptors(child.id());
        let _ = child.kill();
        let _ = child.wait();
    }
    drop(stopper);
    opener.join().unwrap();
    assert!(
        leaked.is_empty(),
        "a concurrently forked child inherited a pane's terminal: {leaked:?}"
    );
}

/// The terminal descriptors above stdio that `pid` holds. Only meaningful once
/// its exec has finished: the kernel wakes a spawning parent before it has run
/// every close-on-exec, so an earlier look sees descriptors about to close.
#[cfg(target_os = "linux")]
fn terminal_descriptors(pid: u32) -> Vec<String> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(format!("/proc/{pid}/fd")).expect("read child fd table") {
        let entry = entry.unwrap();
        let fd: u32 = entry.file_name().to_string_lossy().parse().unwrap();
        let target = std::fs::read_link(entry.path()).unwrap_or_default();
        let target = target.to_string_lossy().into_owned();
        if fd > 2 && (target == "/dev/ptmx" || target.starts_with("/dev/pts/")) {
            found.push(format!("{fd} -> {target}"));
        }
    }
    found
}

/// A descriptor the daemon holds without close-on-exec must not reach a pane.
/// On macOS every pipe, socket and PTY pair is made close-on-exec in a second
/// step, so a pane forked on one thread while another opens one inherits it;
/// a pipe made without the flag stands in for one caught in that gap.
#[tokio::test]
async fn a_pane_inherits_no_descriptor_beyond_its_terminal() {
    let (read, write) = nix::unistd::pipe().expect("pipe");
    let config = PtyConfig::with_command(vec!["sleep".into(), "3600".into()], test_cwd());
    let mut handle = PtyHandle::spawn(&config).unwrap();
    drop(write);
    let closed = tokio::task::spawn_blocking(move || shux_testkit::pipe_closes(read))
        .await
        .unwrap();
    let alive = handle.try_wait().unwrap().is_none();
    handle.kill().unwrap();
    assert!(alive, "the pane exited, so the pipe proves nothing");
    assert!(closed, "the pane still holds a pipe its spawner had open");
}

/// The output probe sees, from its own copy of the master fd, output the child
/// wrote that nobody has read, and nothing once it has been read.
#[tokio::test]
async fn the_output_probe_sees_unread_output() {
    const OUTPUT: &str = "queued-output";
    let config = PtyConfig::with_command(
        vec![
            "sh".into(),
            "-c".into(),
            format!("printf {OUTPUT}; exec cat"),
        ],
        test_cwd(),
    );
    let mut handle = PtyHandle::spawn(&config).unwrap();
    let probe = handle.output_probe().unwrap();

    shux_testkit::wait_until_async("the child's output is queued", || async {
        probe.has_output().unwrap()
    })
    .await;

    let mut buf = [0u8; 64];
    let mut got = Vec::new();
    tokio::time::timeout(shux_testkit::BUDGET, async {
        while got.len() < OUTPUT.len() {
            let n = handle.read(&mut buf).await.unwrap();
            got.extend_from_slice(&buf[..n]);
        }
    })
    .await
    .expect("read the queued output");
    assert_eq!(String::from_utf8_lossy(&got), OUTPUT);
    assert!(
        !probe.has_output().unwrap(),
        "read output is no longer queued"
    );
}
