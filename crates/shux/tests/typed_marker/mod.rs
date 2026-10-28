//! A line typed into a pane is echoed by the PTY before the shell runs it, so a
//! wait for a marker the line contains verbatim returns on the echo.

/// Assert `typed` contains none of `markers`, and that running it through `sh`
/// with `stdin` on its standard input still prints every one.
pub fn assert_echo_cannot_satisfy(typed: &str, stdin: &str, markers: &[&str]) {
    for m in markers {
        assert!(
            !typed.contains(m),
            "the typed text contains {m:?} verbatim, so the wait returns on its echo: {typed}"
        );
    }
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("printf '%s' '{stdin}' | {typed}"))
        .output()
        .expect("spawn sh");
    let out = String::from_utf8_lossy(&out.stdout);
    for m in markers {
        assert!(
            out.contains(m),
            "running the typed text no longer prints {m:?}: {out:?}"
        );
    }
}
