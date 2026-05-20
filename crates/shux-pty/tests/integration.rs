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
