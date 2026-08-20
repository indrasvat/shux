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

/// A pane child must not inherit the *outer* terminal's identity.
///
/// shux is the terminal its children talk to, so a pane that still advertises
/// `KITTY_WINDOW_ID` (or `TMUX`, `GHOSTTY_RESOURCES_DIR`, ...) makes every tool
/// that sniffs those variables address the wrong terminal. The failure is
/// silent and total: terminal-browser detects the *outer* emulator, concludes
/// graphics are supported without probing, and streams images into a pane shux
/// discards -- no message, no non-zero exit, just a blank pane. Its `--split`
/// would likewise cut a pane in the outer terminal.
///
/// The list is read from `OUTER_TERMINAL_IDENTITY_VARS` rather than restated
/// here. A second hand-maintained copy is a copy that drifts: it would leave a
/// newly-added variable covered by nothing, which is exactly the coverage this
/// test exists to provide.
#[tokio::test]
async fn pane_child_does_not_inherit_outer_terminal_identity() {
    const SENTINEL: &str = "leaked-outer-terminal";

    // SAFETY: single-threaded setup before the child is spawned, and removed
    // again below. nextest runs each test in its own process, so no other test
    // observes these. Mirrors what a real outer emulator would export.
    for key in shux_pty::OUTER_TERMINAL_IDENTITY_VARS {
        unsafe { std::env::set_var(key, SENTINEL) };
    }
    // Not an emulator handle: proves this is a deny-list and not `env_clear`,
    // which would take the user's whole environment with it.
    unsafe { std::env::set_var("SHUX_TEST_UNRELATED_VAR", SENTINEL) };

    let mut config = PtyConfig::with_command(
        vec!["sh".into(), "-c".into(), "env; echo ENV_DONE".into()],
        test_cwd(),
    );
    config.env.clear();

    let mut handle = PtyHandle::spawn(&config).unwrap();
    let output = read_pty_to_exit(&mut handle).await;

    for key in shux_pty::OUTER_TERMINAL_IDENTITY_VARS {
        unsafe { std::env::remove_var(key) };
    }
    unsafe { std::env::remove_var("SHUX_TEST_UNRELATED_VAR") };

    assert!(output.contains("ENV_DONE"), "child did not run: {output}");

    let leaked: Vec<&str> = shux_pty::OUTER_TERMINAL_IDENTITY_VARS
        .iter()
        .copied()
        .filter(|key| output.contains(&format!("{key}={SENTINEL}")))
        .collect();
    assert!(
        leaked.is_empty(),
        "pane child inherited the outer terminal's identity: {leaked:?}"
    );
    assert!(
        output.contains(&format!("SHUX_TEST_UNRELATED_VAR={SENTINEL}")),
        "the scrub is a deny-list, not env_clear -- an unrelated variable must survive"
    );
    // TERM_PROGRAM is overwritten rather than removed (tmux does the same), so
    // it must be present AND must name shux.
    assert!(
        output.contains("TERM_PROGRAM=shux"),
        "TERM_PROGRAM should be claimed by shux, not merely removed: {output}"
    );
}
