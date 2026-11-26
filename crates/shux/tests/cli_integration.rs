//! The shipped CLI against a stub daemon that answers only `system.version`
//! (the version-skew report), and in a sandbox with no daemon at all.
//! Everything that needs a real daemon is in the binaries that start one.

use std::path::PathBuf;

/// Serves `router` on `socket_path` until `cancel`, bound before this returns
/// so every caller can connect at once.
async fn serve(
    socket_path: &std::path::Path,
    router: shux_rpc::Router,
    cancel: &tokio_util::sync::CancellationToken,
) {
    let config = shux_rpc::ServerConfig {
        socket_path: socket_path.to_path_buf(),
        tcp_addr: String::new(),
        auth_token: None,
    };
    let server = shux_rpc::Server::new(config, router, cancel.clone())
        .bind()
        .await
        .expect("bind test server");
    tokio::spawn(async move {
        let _ = server.serve().await;
    });
}

/// A daemon that answers `system.version` as `version` / `git_sha`, and
/// nothing else.
async fn start_versioned_server(
    dir: &std::path::Path,
    version: &'static str,
    git_sha: &'static str,
) -> (PathBuf, tokio_util::sync::CancellationToken) {
    let socket_path = dir.join("v.sock");
    let cancel = tokio_util::sync::CancellationToken::new();
    let router = shux_rpc::Router::builder()
        .register(
            "system.version",
            move |_: Option<serde_json::Value>| async move {
                Ok(serde_json::json!({"version": version, "git_sha": git_sha, "name": "shux"}))
            },
        )
        .build();
    serve(&socket_path, router, &cancel).await;
    (socket_path, cancel)
}

async fn shux_version(
    socket: &std::path::Path,
    format: &str,
) -> (String, std::process::ExitStatus) {
    shux_version_patient(socket, format, None).await
}

/// `patience_ms` shortens how long the CLI waits for an answer, for a peer
/// that never gives one.
async fn shux_version_patient(
    socket: &std::path::Path,
    format: &str,
    patience_ms: Option<&str>,
) -> (String, std::process::ExitStatus) {
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_shux"));
    if let Some(ms) = patience_ms {
        cmd.env("SHUX_PEER_PATIENCE_MS", ms);
    }
    let run = cmd
        .args([
            "--format",
            format,
            "--socket",
            socket.to_str().unwrap(),
            "version",
        ])
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(shux_testkit::BUDGET, run)
        .await
        .expect("shux version never returned")
        .unwrap();
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        output.status,
    )
}

/// After an upgrade the old daemon keeps running: `shux version` must say
/// which version is the CLI and which the daemon, and that they differ (#200).
#[tokio::test]
async fn version_names_the_cli_and_a_daemon_of_another_version() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, cancel) = start_versioned_server(dir.path(), "0.0.1", "b623470").await;

    let (stdout, status) = shux_version(&socket, "json").await;
    assert!(status.success(), "{stdout}");
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        parsed["cli"]["version"],
        env!("CARGO_PKG_VERSION"),
        "{parsed}"
    );
    assert_eq!(parsed["cli"]["git_sha"], shux_rpc::GIT_SHA, "{parsed}");
    assert_eq!(parsed["version"], env!("CARGO_PKG_VERSION"), "{parsed}");
    assert_eq!(parsed["daemon"]["version"], "0.0.1", "{parsed}");
    assert_eq!(parsed["daemon"]["git_sha"], "b623470", "{parsed}");
    assert_eq!(parsed["daemon"]["status"], "running", "{parsed}");
    assert_eq!(parsed["mismatch"], true, "{parsed}");

    let (stdout, status) = shux_version(&socket, "text").await;
    assert!(status.success(), "{stdout}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(
        lines[0].contains(env!("CARGO_PKG_VERSION")) && !lines[0].contains("0.0.1"),
        "first line is the CLI's: {stdout}"
    );
    assert!(
        stdout.contains("daemon 0.0.1 (b623470)") && stdout.contains("differs"),
        "daemon shown and flagged: {stdout}"
    );

    let (stdout, status) = shux_version(&socket, "plain").await;
    assert!(status.success(), "{stdout}");
    assert_eq!(
        stdout,
        format!(
            "cli\t{}\t{}\ndaemon\trunning\t0.0.1\tb623470\tmismatch\t-\n",
            env!("CARGO_PKG_VERSION"),
            shux_rpc::GIT_SHA
        ),
        "plain names the same builds and mismatch as JSON and text"
    );
    cancel.cancel();
}

/// The same version from another commit is another build.
#[tokio::test]
async fn version_flags_a_daemon_built_from_another_commit() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, cancel) =
        start_versioned_server(dir.path(), env!("CARGO_PKG_VERSION"), "0000000").await;
    let (stdout, _) = shux_version(&socket, "json").await;
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(parsed["mismatch"], true, "{parsed}");
    cancel.cancel();
}

/// A daemon of this very build matches.
#[tokio::test]
async fn version_reports_a_matching_daemon_as_matching() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, cancel) =
        start_versioned_server(dir.path(), env!("CARGO_PKG_VERSION"), shux_rpc::GIT_SHA).await;
    let (stdout, _) = shux_version(&socket, "json").await;
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        parsed["daemon"]["version"],
        env!("CARGO_PKG_VERSION"),
        "{parsed}"
    );
    assert_eq!(parsed["mismatch"], false, "{parsed}");
    let (stdout, _) = shux_version(&socket, "text").await;
    assert!(!stdout.contains("differs"), "{stdout}");
    cancel.cancel();
}

/// A server that will not say its version may not be a shux daemon at all,
/// and the client refuses to stop one it cannot identify: `version` must not
/// promise a restart.
#[tokio::test]
async fn version_promises_no_restart_of_a_server_without_a_version() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("v.sock");
    let cancel = tokio_util::sync::CancellationToken::new();
    serve(&socket, shux_rpc::Router::builder().build(), &cancel).await;

    let (stdout, _) = shux_version(&socket, "json").await;
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(parsed["daemon"]["status"], "unreadable", "{parsed}");
    assert_eq!(parsed["mismatch"], true, "{parsed}");

    let (stdout, _) = shux_version(&socket, "text").await;
    assert!(
        stdout.contains("did not say which build")
            && stdout.contains("refuse to run")
            && !stdout.contains("restarts it"),
        "{stdout}"
    );
    cancel.cancel();
}

/// A socket that accepts and never answers is reported, not waited on forever.
#[tokio::test]
async fn version_does_not_hang_on_a_daemon_that_never_answers() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("mute.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let held = tokio::spawn(async move {
        let mut conns = Vec::new();
        while let Ok((conn, _)) = listener.accept().await {
            conns.push(conn);
        }
    });
    let (stdout, status) = shux_version_patient(&socket, "json", Some("2000")).await;
    assert!(status.success(), "{stdout}");
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        parsed["cli"]["version"],
        env!("CARGO_PKG_VERSION"),
        "{parsed}"
    );
    assert_eq!(parsed["daemon"]["status"], "unresponsive", "{parsed}");
    assert!(parsed["daemon"]["error"].is_string(), "{parsed}");
    assert!(parsed["mismatch"].is_null(), "{parsed}");
    held.abort();
}

// ══════════════════════════════════════════════════════════════
// Smoke tests (no daemon needed) — use binary path from env
// ══════════════════════════════════════════════════════════════

/// The real binary, pointed at a sandbox instead of at the developer.
///
/// These are the "no daemon needed" smoke cases, but "needed" is not the same
/// as "reachable": anything that gets past clap into dispatch will happily
/// auto-start a daemon, and with an inherited environment that daemon lands in
/// the *operator's* real `XDG_RUNTIME_DIR`, next to the sessions they are
/// actually using — while also reading their real `~/.config/shux/config.toml`,
/// so a local config could decide whether the assertion holds.
///
/// Every spawn therefore gets a per-binary sandbox for the three roots shux
/// resolves state from, and `SHUX_SOCKET` is cleared so an exported one cannot
/// redirect the test back out of it. This is what makes the suite safe to run
/// beside anything else; it was never actually safe serially either, it just
/// had less company.
/// A `Command` that owns the sandbox it points at.
///
/// The sandbox is a real `TempDir` held BY VALUE here, so it is removed when the
/// command is done with. The obvious alternative — a `static OnceLock<TempDir>`
/// shared by the whole test binary — does not work, and it fails in a way that
/// reads as working: **Rust never runs destructors on statics**, so the
/// `TempDir` is never dropped and every test process leaves a `shux-cli*`
/// directory in `/tmp` for good. Wrapping the cleanup in a second static with a
/// `Drop` impl does not help either; that static is never dropped for exactly
/// the same reason. (Both were written here before this comment was.)
///
/// Every call site is a single `shux_bin().…().output()` expression, so the
/// temporary lives to the end of the statement — the child has run and been
/// waited on before the sandbox goes away.
struct Sandboxed {
    cmd: std::process::Command,
    _root: tempfile::TempDir,
}

impl std::ops::Deref for Sandboxed {
    type Target = std::process::Command;
    fn deref(&self) -> &Self::Target {
        &self.cmd
    }
}

impl std::ops::DerefMut for Sandboxed {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.cmd
    }
}

/// A sandbox directory, for tests that need to name a path inside one.
fn sandbox_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        // Unix socket paths cap out near 104 bytes, so the prefix stays short:
        // a long one truncates the daemon socket and the failure reads as a
        // hang rather than a name-too-long error.
        .prefix("shux-cli")
        .tempdir()
        .expect("sandbox tempdir")
}

fn shux_bin() -> Sandboxed {
    let root = sandbox_dir();
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_shux"));
    cmd.env("XDG_RUNTIME_DIR", root.path())
        .env("XDG_CONFIG_HOME", root.path())
        .env("XDG_STATE_HOME", root.path())
        .env_remove("SHUX_SOCKET");
    Sandboxed { cmd, _root: root }
}

#[test]
fn test_cli_invalid_subcommand() {
    let output = shux_bin().arg("nonexistent").output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("unrecognized subcommand"));
}

#[test]
fn test_cli_session_kill_requires_target() {
    // `shux session kill` with no positional/--session must fail.
    let output = shux_bin().args(["session", "kill"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    // Either clap rejects (no required), or dispatch reports missing.
    assert!(
        stderr.contains("session") || stderr.contains("required") || stderr.contains("missing"),
        "expected clear error for missing session target, got: {stderr}"
    );
}

#[test]
fn test_plugin_scaffold_generates_sh_process_plugin_without_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("hello-plugin");

    let output = shux_bin()
        .args([
            "plugin",
            "scaffold",
            plugin_dir.to_str().unwrap(),
            "--runtime",
            "sh",
            "--name",
            "hello-plugin",
            "--id",
            "dev.shux.hello-plugin",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let manifest = std::fs::read_to_string(plugin_dir.join("shux-plugin.toml")).unwrap();
    assert!(manifest.contains("name = \"hello-plugin\""));
    assert!(manifest.contains("id = \"dev.shux.hello-plugin\""));
    assert!(manifest.contains("runtime = \"process\""));
    assert!(plugin_dir.join("bin/hello-plugin.sh").is_file());
    assert!(plugin_dir.join("README.md").is_file());
    assert!(plugin_dir.join("LICENSE").is_file());
}

#[test]
fn test_cli_config_validate_clean() {
    use std::io::Write;
    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    writeln!(
        tmp,
        "[appearance]\nborder_style = \"rounded\"\n\n[keys]\nprefix = \"ctrl-space\"\n"
    )
    .unwrap();
    let output = shux_bin()
        .args([
            "config",
            "validate",
            "--config",
            tmp.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("Config valid"));
}

#[test]
fn test_cli_config_validate_outer_error() {
    use std::io::Write;
    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    // 42 is not a string — fails type check at line 2 col 16.
    writeln!(tmp, "[appearance]\nborder_style = 42").unwrap();
    let output = shux_bin()
        .args([
            "config",
            "validate",
            "--config",
            tmp.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "expected non-zero exit");
    assert!(
        stderr.contains(":2:"),
        "stderr should include line 2: {stderr}"
    );
    assert!(stderr.contains("error found"));
}

#[test]
fn test_cli_config_validate_inner_starship_error() {
    use std::io::Write;
    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    write!(
        tmp,
        r#"[[statusbar.segment]]
zone = "left"
command = ["echo", "x"]
starship_config = """
[character]
this is not valid toml
"""
"#,
    )
    .unwrap();
    let output = shux_bin()
        .args([
            "config",
            "validate",
            "--config",
            tmp.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("statusbar.segment[0].starship_config"),
        "stderr: {stderr}"
    );
}

#[test]
fn test_cli_config_validate_missing_file() {
    let dir = sandbox_dir();
    let missing = dir.path().join("nonexistent-validate.toml");
    let output = shux_bin()
        .args(["config", "validate", "--config", missing.to_str().unwrap()])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("config file not found"));
}

#[test]
fn test_cli_version_without_daemon() {
    let dir = sandbox_dir();
    let missing = dir.path().join("nonexistent.sock");
    let output = shux_bin()
        .arg("version")
        .env("SHUX_SOCKET", missing.to_str().unwrap())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(stdout.contains("shux"));
    assert!(stdout.contains("daemon not running"));
}

#[test]
fn test_cli_version_json_without_daemon() {
    let dir = sandbox_dir();
    let missing = dir.path().join("nonexistent.sock");
    let output = shux_bin()
        .args(["--format", "json", "version"])
        .env("SHUX_SOCKET", missing.to_str().unwrap())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        parsed["cli"]["version"],
        env!("CARGO_PKG_VERSION"),
        "{parsed}"
    );
    assert!(parsed["daemon"].is_null(), "{parsed}");
    assert!(parsed["mismatch"].is_null(), "{parsed}");
}
