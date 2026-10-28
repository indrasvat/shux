//! A runtime dir reached through a short symlink must work even when its
//! resolved path is too long for a Unix socket address: identity uses the
//! resolved path, the socket itself the one that fits (issue #206).

use std::path::PathBuf;
use std::process::Command;

fn shux(runtime: &std::path::Path) -> Command {
    let mut cmd = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_shux")));
    cmd.env("XDG_RUNTIME_DIR", runtime)
        .env_remove("SHUX_SOCKET")
        .env("NO_COLOR", "1");
    cmd
}

#[test]
fn a_daemon_starts_where_only_the_unresolved_socket_path_fits() {
    let root = tempfile::tempdir().expect("temp root");
    // Resolved: well past any platform's limit (104 bytes on macOS, 108 on Linux).
    let real = root.path().join("r".repeat(120));
    std::fs::create_dir(&real).unwrap();
    let link = root.path().join("l");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let typed = link.join("shux/shux.sock");
    assert!(
        nix::sys::socket::UnixAddr::new(&typed).is_ok(),
        "set-up: the typed path must fit ({} bytes)",
        typed.as_os_str().len()
    );

    let out = shux(&link)
        .args([
            "--format", "json", "session", "create", "limit", "-d", "--", "sleep", "30",
        ])
        .output()
        .expect("spawn shux");
    let _ = shux(&link).args(["daemon", "stop"]).output();
    assert!(
        out.status.success(),
        "no daemon at a typed path that fits:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
