//! Security regression tests for issue #105.
//!
//! An inline `[[statusbar.segment]].starship_config` used to be materialised
//! to a fully predictable path in shared temp (`$TMPDIR/shux-segment-<idx>.toml`)
//! with a symlink-following `std::fs::write`. On a shared `/tmp`, a local
//! attacker could pre-plant that path as a symlink and redirect the daemon's
//! write onto any file the daemon user can write — an arbitrary-file clobber
//! primitive (CWE-59).
//!
//! These tests drive the REAL `shux` binary. The daemon runs with an isolated
//! `XDG_RUNTIME_DIR` / `XDG_CONFIG_HOME` and a stand-in shared `TMPDIR`, exactly
//! as the reproduction does. We play the attacker (plant the symlink), start the
//! daemon (the victim), and assert the write never follows the link and lands in
//! the daemon's own 0700 runtime directory at mode 0600.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

/// Isolated daemon environment: private runtime dir, private config dir, and a
/// world-writable stand-in for `/tmp` where the attacker plants the symlink.
struct Env {
    bin: PathBuf,
    root: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("temp root");
        for sub in ["shared-tmp", "runtime", "config/shux"] {
            std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
        }
        // Make the stand-in tmp world-writable + sticky, like a real /tmp.
        std::fs::set_permissions(
            root.path().join("shared-tmp"),
            std::fs::Permissions::from_mode(0o1777),
        )
        .expect("chmod shared-tmp");
        Self {
            bin: PathBuf::from(env!("CARGO_BIN_EXE_shux")),
            root,
        }
    }

    fn shared_tmp(&self) -> PathBuf {
        self.root.path().join("shared-tmp")
    }
    fn runtime(&self) -> PathBuf {
        self.root.path().join("runtime")
    }
    fn config_home(&self) -> PathBuf {
        self.root.path().join("config")
    }
    /// The daemon's private runtime subdir (holds the socket + pid file).
    fn runtime_shux(&self) -> PathBuf {
        self.runtime().join("shux")
    }

    fn write_config(&self, body: &str) {
        std::fs::write(self.config_home().join("shux/config.toml"), body).expect("write config");
    }

    fn shux(&self) -> Command {
        let mut cmd = Command::new(&self.bin);
        cmd.env("XDG_RUNTIME_DIR", self.runtime())
            .env("XDG_CONFIG_HOME", self.config_home())
            .env("TMPDIR", self.shared_tmp())
            .env("NO_COLOR", "1")
            .env("SHELL", "/bin/sh");
        cmd
    }

    /// Auto-start the daemon by issuing one RPC call.
    fn boot(&self) {
        let _ = self
            .shux()
            .args([
                "--format",
                "json",
                "rpc",
                "call",
                "session.list",
                "--params",
                "{}",
            ])
            .output()
            .expect("spawn shux");
    }

    fn daemon_pid(&self) -> Option<u32> {
        std::fs::read_to_string(self.runtime_shux().join("shux.pid"))
            .ok()?
            .trim()
            .parse()
            .ok()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        if let Some(pid) = self.daemon_pid() {
            let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if kill(Pid::from_raw(pid as i32), None).is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

const INLINE_CONFIG: &str = "add_newline = false\nformat = \"$time\"\n";

fn config_with_inline_segment() -> String {
    format!(
        "[[statusbar.segment]]\n\
         zone = \"right\"\n\
         command = [\"true\"]\n\
         interval_ms = 100000\n\
         starship_config = '''\n{INLINE_CONFIG}'''\n"
    )
}

/// Find the materialised segment config inside the daemon's runtime dir, if any.
fn find_runtime_segment_file(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for e in entries.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("segment-") && name.ends_with(".toml") {
            return Some(e.path());
        }
    }
    None
}

/// The attacker plants a symlink at the legacy predictable path; the daemon's
/// write must NOT follow it and clobber the victim.
#[test]
fn inline_starship_config_does_not_follow_planted_symlink() {
    let env = Env::new();
    env.write_config(&config_with_inline_segment());

    // Victim file the attacker wants destroyed.
    let victim = env.root.path().join("victim.txt");
    let victim_original = "CRITICAL DATA — MUST NOT BE CLOBBERED\n";
    std::fs::write(&victim, victim_original).expect("write victim");

    // Attacker pre-plants the symlink at the fully predictable legacy path.
    let legacy = env.shared_tmp().join("shux-segment-0.toml");
    std::os::unix::fs::symlink(&victim, &legacy).expect("plant symlink");

    env.boot();

    // Wait until the daemon materialises the config somewhere (safe location on
    // a fixed build; the victim on a vulnerable build) or a short budget lapses.
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let clobbered = std::fs::read_to_string(&victim).unwrap_or_default() != victim_original;
        let materialised = find_runtime_segment_file(&env.runtime_shux()).is_some();
        if clobbered || materialised || Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }

    // 1. The victim must be byte-for-byte untouched.
    let victim_after = std::fs::read_to_string(&victim).expect("read victim");
    assert_eq!(
        victim_after, victim_original,
        "victim file was clobbered through the planted symlink (CWE-59)"
    );

    // 2. The planted symlink must still be a symlink and still point at the
    //    victim — i.e. the daemon neither followed nor replaced it.
    let meta = std::fs::symlink_metadata(&legacy).expect("legacy path metadata");
    assert!(
        meta.file_type().is_symlink(),
        "legacy path is no longer the attacker's symlink"
    );
    assert_eq!(
        std::fs::read_link(&legacy).expect("read_link"),
        victim,
        "symlink target changed"
    );

    // 3. The config must have been materialised into the daemon's own 0700
    //    runtime dir, as a private 0600 file carrying the inline TOML.
    let seg = find_runtime_segment_file(&env.runtime_shux())
        .expect("segment config was not materialised in the runtime dir");
    let mode = std::fs::metadata(&seg)
        .expect("seg metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o600,
        "segment config is not mode 0600 (got {mode:o})"
    );
    let body = std::fs::read_to_string(&seg).expect("read seg");
    assert!(
        body.contains("format = \"$time\""),
        "materialised config does not contain the inline TOML"
    );
}

/// The fix's "private 0700 dir" guarantee depends on the runtime dir itself not
/// being attacker-controlled. If `$XDG_RUNTIME_DIR/shux` is a pre-planted
/// symlink, the daemon must refuse it — never chmod-follow it or write its
/// socket/pidfile/segment files into the attacker's directory.
#[test]
fn daemon_refuses_a_symlinked_runtime_dir() {
    let env = Env::new();
    env.write_config(&config_with_inline_segment());

    // Attacker: the runtime dir the daemon will use is a symlink to their dir.
    let victim_dir = env.root.path().join("victim_dir");
    std::fs::create_dir(&victim_dir).unwrap();
    std::fs::set_permissions(&victim_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink(&victim_dir, env.runtime_shux()).unwrap();

    env.boot();
    thread::sleep(Duration::from_millis(800));

    // Directory mode must be untouched (not chmod-followed to 0700)...
    let mode = std::fs::metadata(&victim_dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o755,
        "runtime-dir symlink was chmod-followed (got {mode:o})"
    );
    // ...and the daemon must not have written anything into the attacker's dir.
    let leaked: Vec<String> = std::fs::read_dir(&victim_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        leaked.is_empty(),
        "daemon wrote into the attacker-controlled dir: {leaked:?}"
    );
}
