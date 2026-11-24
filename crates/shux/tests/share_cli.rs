//! `shux share` on a real daemon, with a stand-in for cloudflared (#297).
//!
//! The stand-in prints cloudflared's startup record for a made-up quick-tunnel
//! host and then idles, as cloudflared does. A debug daemon told
//! `SHUX_TEST_SHARE_PROBE=loopback` fetches the tunnel's marker from its own
//! listener, naming that host, so everything but the edge is the real path:
//! the CLI, the owner-local RPCs, the exec'd supervisor, the record parser,
//! the marker and the teardown. `make test-share-tunnel` runs the real thing.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use shux_testkit::{running, wait_for, wait_until};

struct Harness {
    bin: PathBuf,
    runtime: tempfile::TempDir,
    config: tempfile::TempDir,
    state: tempfile::TempDir,
    fake: tempfile::TempDir,
    /// The quick-tunnel host the stand-in announces.
    host: String,
    /// What the stand-in idles as, so `ps` finds this run's and no other.
    needle: String,
}

/// The stand-in: writes `pid ppid args` to `cloudflared.pid`, announces
/// `host` (or exits at once for `exit`), then idles until killed.
fn stand_in(dir: &Path, host: &str, needle: &str, mode: &str) -> PathBuf {
    let pidfile = dir.join("cloudflared.pid");
    let body = if mode == "exit" {
        "echo 'ERR failed to request quick Tunnel' >&2\nexit 1\n".to_string()
    } else if mode == "silent" {
        // Runs, but never announces a tunnel: a startup that hangs.
        format!("exec /bin/sleep {needle}\n")
    } else if mode == "rate-limited" {
        // What cloudflared 2026.11 prints when Cloudflare says no.
        "echo '2026-11-24T14:22:48Z INF Requesting new quick Tunnel on trycloudflare.com...' >&2\n\
         echo 'quick tunnel provisioning failed with status 429: [10000] Tunnelstore request failed' >&2\n\
         exit 1\n"
            .to_string()
    } else {
        let record = format!(
            "cat >&2 <<'EOT'\n\
             2026-11-24T07:40:53Z INF Requesting new quick Tunnel on trycloudflare.com...\n\
             2026-11-24T07:40:57Z INF +------------------------------------------------------+\n\
             2026-11-24T07:40:57Z INF |  Your quick Tunnel has been created! Visit it at (it may take some time to be reachable):  |\n\
             2026-11-24T07:40:57Z INF |  https://{host}                                     |\n\
             2026-11-24T07:40:57Z INF +------------------------------------------------------+\n\
             2026-11-24T07:40:58Z INF Registered tunnel connection connIndex=0 protocol=quic\n\
             EOT\n"
        );
        let then = match mode {
            // Stops logging and keeps running: only its pid says it is alive.
            "closes-stderr" => format!("exec 2>&-\nexec /bin/sleep {needle}\n"),
            // Leaves a child in its group, then exits.
            "forks-then-exits" => format!(
                "/bin/sleep {needle}1 &\necho $! > '{}'\nexit 0\n",
                dir.join("child.pid").display()
            ),
            _ => format!("exec /bin/sleep {needle}\n"),
        };
        format!("{record}{then}")
    };
    // What it was handed: whether fd 3 (the supervisor's listener) is open,
    // what its stdin is, and the names in its environment.
    let inherited = format!(
        "fd3=closed; {{ true <&3; }} 2>/dev/null && fd3=open\n\
         if [ -p /dev/stdin ]; then in=pipe; else in=other; fi\n\
         echo \"$fd3 $in $(/usr/bin/env | /usr/bin/cut -d= -f1 | /usr/bin/tr '\\n' ' ')\" > '{}'\n",
        dir.join("cloudflared.inherited").display()
    );
    let script = format!(
        "#!/bin/sh\n{inherited}echo \"$$ $PPID $*\" > '{}'\n{body}",
        pidfile.display()
    );
    let path = dir.join("cloudflared");
    std::fs::write(&path, script).expect("write the stand-in");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

/// A clipboard that writes what it is given to `clipboard.txt`.
fn fake_clipboard(dir: &Path) -> PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).expect("bin dir");
    // PATH holds only this directory: the clipboard names its own cat.
    let script = format!(
        "#!/bin/sh\n/bin/cat > '{}'\n",
        dir.join("clipboard.txt").display()
    );
    use std::os::unix::fs::PermissionsExt;
    for name in ["pbcopy", "xclip"] {
        let p = bin.join(name);
        std::fs::write(&p, &script).expect("write the clipboard");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    bin
}

struct Tunnel {
    cloudflared: i32,
    supervisor: i32,
    port: u16,
}

impl Harness {
    fn new() -> Self {
        Self::with_mode("record")
    }

    fn with_mode(mode: &str) -> Self {
        let fake = tempfile::tempdir().expect("fake dir");
        let tag: String = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
        let host = format!("t-{tag}.trycloudflare.com");
        let needle = format!("3600.{}", u64::from_str_radix(&tag[..8], 16).unwrap_or(1));
        if mode != "missing" {
            stand_in(fake.path(), &host, &needle, mode);
        }
        Self {
            bin: PathBuf::from(env!("CARGO_BIN_EXE_shux")),
            runtime: tempfile::tempdir().expect("runtime dir"),
            config: tempfile::tempdir().expect("config dir"),
            state: tempfile::tempdir().expect("state dir"),
            fake,
            host,
            needle,
        }
    }

    fn command(&self) -> Command {
        let mut c = Command::new(&self.bin);
        c.env("XDG_RUNTIME_DIR", self.runtime.path())
            .env("XDG_CONFIG_HOME", self.config.path())
            .env("XDG_STATE_HOME", self.state.path())
            .env("SHELL", "/bin/sh")
            .env("SHUX_CLOUDFLARED", self.fake.path().join("cloudflared"))
            .env("SHUX_TEST_SHARE_PROBE", "loopback")
            .env_remove("SHUX_SOCKET")
            .env_remove("SHUX_PANE_ID");
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().expect("run shux")
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let mut all = vec!["--format", "json"];
        all.extend_from_slice(args);
        let out = self.run(&all);
        assert!(
            out.status.success(),
            "shux {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).expect("json")
    }

    fn rpc(&self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.json(&["rpc", "call", method, "--params", &params.to_string()])
    }

    /// A session running a shell, and its one pane.
    fn session(&self, name: &str) -> String {
        let created = self.rpc("session.create", serde_json::json!({ "name": name }));
        created["result"]["pane_id"]
            .as_str()
            .expect("session.create names its pane")
            .to_string()
    }

    fn links(&self) -> Vec<serde_json::Value> {
        self.json(&["share", "list"])["links"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// The tunnel the stand-in is running for, once it is.
    fn tunnel(&self) -> Tunnel {
        let line = wait_for("the stand-in to start", || {
            std::fs::read_to_string(self.fake.path().join("cloudflared.pid"))
                .ok()
                .filter(|s| s.ends_with('\n'))
        });
        let mut words = line.split_whitespace();
        let cloudflared = words.next().and_then(|w| w.parse().ok()).expect("pid");
        let supervisor = words.next().and_then(|w| w.parse().ok()).expect("ppid");
        let url = line
            .split_whitespace()
            .skip_while(|w| *w != "--url")
            .nth(1)
            .expect("--url");
        let port = url
            .rsplit(':')
            .next()
            .and_then(|p| p.parse().ok())
            .expect("a port");
        Tunnel {
            cloudflared,
            supervisor,
            port,
        }
    }

    fn cloudflared_runs(&self, pid: i32) -> bool {
        running(pid, &self.needle)
    }

    fn events(&self, filter: &str) -> Vec<serde_json::Value> {
        let h = self.rpc(
            "events.history",
            serde_json::json!({ "filter": [filter], "count": 200 }),
        );
        h["result"]["events"]
            .as_array()
            .cloned()
            .expect("events.history lists events")
    }

    fn daemon_pid(&self) -> Option<i32> {
        std::fs::read_to_string(self.runtime.path().join("shux/shux.pid"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(pid) = self.daemon_pid()
            && !shux_testkit::terminate(pid)
            && !std::thread::panicking()
        {
            panic!("daemon {pid} survived SIGTERM");
        }
    }
}

fn ok_and_quiet(out: &Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Every new link reminds its owner what it shows and how to end it, in
/// text and in JSON alike.
#[test]
fn a_new_link_says_what_it_exposes_and_how_to_end_it() {
    let h = Harness::new();
    let pane = h.session("demo");
    let made = h.json(&["share", "-s", "demo"]);
    let notice = made["notice"].as_str().unwrap_or_default();
    assert!(
        notice.starts_with("Anyone with this link sees everything in demo, live, until ")
            && notice.ends_with(". End it with: shux share stop"),
        "{made}"
    );
    // Copied from a pane: the summary prints whatever the output is.
    let bin = fake_clipboard(h.fake.path());
    let copied = h
        .command()
        .env("SHUX_PANE_ID", &pane)
        .env("PATH", &bin)
        .env("DISPLAY", ":99")
        .args(["share", "-s", "demo"])
        .output()
        .expect("run shux");
    let text = ok_and_quiet(&copied);
    assert!(
        text.contains(notice.split(", until").next().unwrap_or("?")),
        "{text}"
    );
    assert!(text.contains("End it with: shux share stop"), "{text}");
}

#[test]
fn a_link_runs_its_tunnel_and_the_last_link_ending_reaps_it() {
    let h = Harness::new();
    h.session("demo");
    let made = h.json(&["share", "-s", "demo", "--label", "for review"]);
    let url = made["url"].as_str().expect("a url");
    let id = made["id"].as_str().expect("an id");
    let prefix = format!("https://{}/v/{id}#", h.host);
    assert!(url.starts_with(&prefix), "{url} under {prefix}");
    let secret = &url[prefix.len()..];
    assert!(secret.len() >= 43, "a 256-bit secret, not {secret:?}");
    assert_eq!(made["deliver"], "print");
    assert_eq!(made["uses"], 1);
    assert_eq!(made["session"], "demo");

    let t = h.tunnel();
    assert!(h.cloudflared_runs(t.cloudflared));
    let inherited = std::fs::read_to_string(h.fake.path().join("cloudflared.inherited"))
        .expect("what the stand-in was handed");
    let mut words = inherited.split_whitespace();
    assert_eq!(
        words.next(),
        Some("closed"),
        "cloudflared holds the listener: {inherited}"
    );
    assert_eq!(
        words.next(),
        Some("other"),
        "cloudflared reads the liveness pipe: {inherited}"
    );
    for name in words {
        assert!(
            ["HOME", "PATH", "PWD", "SHLVL", "_", "OLDPWD"].contains(&name),
            "cloudflared inherited {name}: {inherited}"
        );
    }
    let listed = h.json(&["share", "list"]);
    assert_eq!(listed["tunnel"], "up", "{listed}");
    assert_eq!(listed["links"][0]["id"], id);
    assert_eq!(listed["links"][0]["uses_left"], 1);
    assert_eq!(listed["links"][0]["label"], "for review");
    assert!(
        !listed.to_string().contains(secret),
        "list never shows a secret"
    );

    let revoked = h.json(&["share", "revoke", id]);
    assert_eq!(revoked["revoked"][0], id);
    assert!(h.links().is_empty());
    wait_until("the connector to be reaped", || {
        !h.cloudflared_runs(t.cloudflared)
    });
    wait_until("the supervisor to exit", || {
        !running(t.supervisor, &format!("--port {}", t.port))
    });
    wait_until("the port to be free", || {
        std::net::TcpListener::bind(("127.0.0.1", t.port)).is_ok()
    });

    let history = h.events("share.");
    let kinds: Vec<&str> = history.iter().filter_map(|e| e["type"].as_str()).collect();
    assert_eq!(
        kinds,
        [
            "share.tunnel",
            "share.created",
            "share.ended",
            "share.tunnel"
        ],
        "in the order they happened"
    );
    let text = serde_json::Value::Array(history).to_string();
    assert!(text.contains(r#""reason":"revoked""#), "{text}");
    assert!(
        text.contains(r#""state":"up""#) && text.contains(r#""reason":"idle""#),
        "{text}"
    );
    for leak in [secret, h.host.as_str(), "for review"] {
        assert!(!text.contains(leak), "an event carried {leak:?}: {text}");
    }
}

#[test]
fn stop_ends_every_link_and_a_new_link_needs_a_new_tunnel() {
    let h = Harness::new();
    h.session("demo");
    let first = h.json(&["share", "-s", "demo", "--uses", "3"]);
    let t1 = h.tunnel();
    h.json(&["share", "-s", "demo", "--uses", "0", "--for", "2h"]);
    assert_eq!(h.links().len(), 2, "one tunnel carries both");
    let stopped = h.json(&["share", "stop"]);
    assert_eq!(stopped["links_ended"], 2);
    assert!(h.links().is_empty());
    wait_until("stop to reap the connector", || {
        !h.cloudflared_runs(t1.cloudflared)
    });
    assert_eq!(h.json(&["share", "list"])["tunnel"], "down");
    // Idempotent.
    assert_eq!(h.json(&["share", "stop"])["links_ended"], 0);

    std::fs::remove_file(h.fake.path().join("cloudflared.pid")).expect("clear the pidfile");
    let second = h.json(&["share", "-s", "demo"]);
    let t2 = h.tunnel();
    assert_ne!(t1.cloudflared, t2.cloudflared, "a fresh connector");
    assert_ne!(first["id"], second["id"]);
    assert_eq!(h.links().len(), 1, "the old links stayed ended");
}

#[test]
fn a_connector_that_dies_ends_its_links() {
    let h = Harness::new();
    h.session("demo");
    h.json(&["share", "-s", "demo"]);
    let t = h.tunnel();
    assert!(shux_testkit::terminate(t.cloudflared));
    wait_until("the links to end with the tunnel", || h.links().is_empty());
    wait_until("the tunnel to read as down", || {
        h.json(&["share", "list"])["tunnel"] == "down"
    });
    let text = serde_json::Value::Array(h.events("share.")).to_string();
    assert!(text.contains(r#""reason":"tunnel_lost""#), "{text}");
    assert!(text.contains(r#""reason":"lost""#), "{text}");
}

#[test]
fn a_daemon_killed_outright_still_reaps_its_connector_and_frees_the_port() {
    let h = Harness::new();
    h.session("demo");
    h.json(&["share", "-s", "demo"]);
    let t = h.tunnel();
    let daemon = h.daemon_pid().expect("a daemon");
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(daemon),
        nix::sys::signal::Signal::SIGKILL,
    )
    .expect("SIGKILL the daemon");
    wait_until("the connector to be reaped", || {
        !h.cloudflared_runs(t.cloudflared)
    });
    wait_until("the supervisor to exit", || {
        !running(t.supervisor, &format!("--port {}", t.port))
    });
    wait_until("the port to be free", || {
        std::net::TcpListener::bind(("127.0.0.1", t.port)).is_ok()
    });
}

#[test]
fn a_stand_in_that_exits_at_once_is_an_error_and_mints_nothing() {
    let h = Harness::with_mode("exit");
    h.session("demo");
    let out = h.run(&["share", "-s", "demo"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("exited"), "{err}");
    assert!(h.links().is_empty());
    let t = h.tunnel();
    wait_until("the failed startup to stop listening", || {
        std::net::TcpListener::bind(("127.0.0.1", t.port)).is_ok()
    });
}

#[test]
fn stop_ends_a_tunnel_still_starting() {
    let h = Harness::with_mode("silent");
    h.session("demo");
    let pending = h
        .command()
        .args(["share", "-s", "demo"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("start sharing");
    let t = h.tunnel();
    let stopped = h.json(&["share", "stop"]);
    assert_eq!(stopped["links_ended"], 0);
    let out = pending.wait_with_output().expect("the share returns");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("stopped while the tunnel was starting"),
        "{err}"
    );
    wait_until("the connector to be reaped", || {
        !h.cloudflared_runs(t.cloudflared)
    });
    assert!(h.links().is_empty());
}

#[test]
fn a_caller_gone_mid_startup_leaves_no_connector() {
    let h = Harness::with_mode("silent");
    h.session("demo");
    let mut pending = h
        .command()
        .args(["share", "-s", "demo"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("start sharing");
    let t = h.tunnel();
    pending.kill().expect("kill the caller");
    pending.wait().expect("reap the caller");
    wait_until("the connector to be reaped", || {
        !h.cloudflared_runs(t.cloudflared)
    });
    wait_until("the supervisor to exit", || {
        !running(t.supervisor, &format!("--port {}", t.port))
    });
    wait_until("the abandoned startup to stop listening", || {
        std::net::TcpListener::bind(("127.0.0.1", t.port)).is_ok()
    });
    assert_eq!(h.json(&["share", "list"])["tunnel"], "down");
}

#[test]
fn a_refusal_from_cloudflare_says_to_wait() {
    let h = Harness::with_mode("rate-limited");
    h.session("demo");
    let out = h.run(&["share", "-s", "demo"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("429") && err.contains("try again"), "{err}");
    let text = serde_json::Value::Array(h.events("share.tunnel")).to_string();
    assert!(text.contains(r#""reason":"rate_limited""#), "{text}");
    assert!(h.links().is_empty());
}

#[test]
fn no_cloudflared_is_an_install_hint() {
    // The daemon looks for cloudflared, so it is the daemon's environment
    // that has none.
    let h = Harness::with_mode("missing");
    h.session("demo");
    let out = h.run(&["share", "-s", "demo"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("brew install cloudflared"), "{err}");
    assert!(h.links().is_empty());
}

#[test]
fn an_unlimited_link_must_say_how_long() {
    let h = Harness::new();
    h.session("demo");
    let out = h.run(&["share", "-s", "demo", "--uses", "0"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--for"));
    // The RPC says so too, for a caller that is not this CLI.
    let out = h.run(&[
        "--format",
        "json",
        "rpc",
        "call",
        "share.create",
        "--params",
        r#"{"session":"demo","uses":0,"from_pane":null}"#,
    ]);
    let reply = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(reply.contains("unbounded"), "{reply}");
    assert!(h.links().is_empty());
    // And past the caps: 24 hours, and the 32 guests one link can let in.
    let out = h.run(&["share", "-s", "demo", "--for", "25h"]);
    assert!(!out.status.success());
    let out = h.run(&["share", "-s", "demo", "--uses", "33"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("at most 32 guests"), "{err}");
    assert!(h.links().is_empty());
}

#[test]
fn inside_the_shared_pane_without_a_clipboard_nothing_is_minted() {
    let h = Harness::new();
    let pane = h.session("demo");
    let empty = tempfile::tempdir().expect("an empty PATH");
    let out = h
        .command()
        .env("SHUX_PANE_ID", &pane)
        .env("PATH", empty.path())
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .args(["share"])
        .output()
        .expect("run shux");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("outside shux"), "{err}");
    assert!(!String::from_utf8_lossy(&out.stdout).contains("https://"));
    assert!(h.links().is_empty());
    assert!(
        !h.fake.path().join("cloudflared.pid").exists(),
        "no tunnel was started for a link never minted"
    );
}

#[test]
fn inside_the_shared_pane_the_link_goes_to_the_clipboard_only() {
    let h = Harness::new();
    let pane = h.session("demo");
    let bin = fake_clipboard(h.fake.path());
    let run = |format: &str| {
        h.command()
            .env("SHUX_PANE_ID", &pane)
            .env("PATH", &bin)
            .env("DISPLAY", ":99")
            .args(["--format", format, "share"])
            .output()
            .expect("run shux")
    };
    let shown = ok_and_quiet(&run("text"));
    let copied =
        std::fs::read_to_string(h.fake.path().join("clipboard.txt")).expect("the clipboard");
    assert!(
        copied.starts_with(&format!("https://{}/v/", h.host)),
        "{copied}"
    );
    let secret = copied.rsplit('#').next().expect("a secret");
    assert!(shown.contains("copied to the clipboard"), "{shown}");
    for leak in ["https://", secret] {
        assert!(!shown.contains(leak), "printed {leak:?}: {shown}");
    }
    let json: serde_json::Value = serde_json::from_str(&ok_and_quiet(&run("json"))).expect("json");
    assert_eq!(json["deliver"], "clipboard");
    assert!(json["url"].is_null(), "{json}");

    // A QR code on a shared screen is the link on a shared screen.
    let out = h
        .command()
        .env("SHUX_PANE_ID", &pane)
        .env("PATH", &bin)
        .env("DISPLAY", ":99")
        .args(["share", "--qr"])
        .output()
        .expect("run shux");
    assert!(!out.status.success());
    assert_eq!(h.links().len(), 2, "the refused QR minted nothing");
}

/// The rule: inside any shux pane a link is never printed. Clipboard, or
/// refuse; printed only to a caller outside shux.
#[test]
fn any_pane_even_of_another_session_gets_the_clipboard() {
    let h = Harness::new();
    h.session("demo");
    let elsewhere = h.session("desk");
    let bin = fake_clipboard(h.fake.path());
    let out = h
        .command()
        .env("SHUX_PANE_ID", &elsewhere)
        .env("PATH", &bin)
        .env("DISPLAY", ":99")
        .args(["--format", "json", "share", "-s", "demo"])
        .output()
        .expect("run shux");
    let made: serde_json::Value = serde_json::from_str(&ok_and_quiet(&out)).expect("json");
    assert_eq!(made["deliver"], "clipboard", "{made}");
    assert!(made["url"].is_null(), "{made}");
}

#[test]
fn a_pane_id_the_daemon_does_not_know_still_means_in_a_pane() {
    let h = Harness::new();
    h.session("demo");
    // Stale, bogus, or a nested shux's: whatever it names, the caller is in
    // a terminal someone may be watching.
    for id in ["0b0e5a6e-0000-4000-8000-00000000dead", "not-a-pane"] {
        let out = h
            .command()
            .env("SHUX_PANE_ID", id)
            .env("PATH", tempfile::tempdir().expect("empty").path())
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .args(["share", "-s", "demo"])
            .output()
            .expect("run shux");
        assert!(
            !out.status.success(),
            "{id}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(!String::from_utf8_lossy(&out.stdout).contains("https://"));
    }
    assert!(h.links().is_empty());
}

#[test]
fn raw_rpc_from_a_pane_cannot_print_a_link() {
    let h = Harness::new();
    let pane = h.session("demo");
    let out = h
        .command()
        .env("SHUX_PANE_ID", &pane)
        .args([
            "--format",
            "json",
            "rpc",
            "call",
            "share.create",
            "--params",
        ])
        .arg(r#"{"session":"demo"}"#)
        .output()
        .expect("run shux");
    let reply = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!reply.contains("https://"), "{reply}");
    assert!(reply.contains("in_pane"), "{reply}");
    assert!(h.links().is_empty());
}

#[test]
fn raw_rpc_from_a_pane_cannot_talk_its_way_into_printing_a_link() {
    let h = Harness::new();
    let pane = h.session("demo");
    let bin = fake_clipboard(h.fake.path());
    for params in [
        r#"{"session":"demo","from_pane":null}"#,
        r#"{"session":"demo","can_copy":true}"#,
        r#"{"session":"demo","from_pane":null,"can_copy":true}"#,
    ] {
        let out = h
            .command()
            .env("SHUX_PANE_ID", &pane)
            .env("PATH", &bin)
            .env("DISPLAY", ":99")
            .args([
                "--format",
                "json",
                "rpc",
                "call",
                "share.create",
                "--params",
            ])
            .arg(params)
            .output()
            .expect("run shux");
        let reply = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!reply.contains("https://"), "{params}: {reply}");
    }
    assert!(h.links().is_empty());
}

#[test]
fn from_pane_is_required_and_null_means_outside_shux() {
    let h = Harness::new();
    h.session("demo");
    let call = |params: &str| {
        h.command()
            .args([
                "--format",
                "json",
                "rpc",
                "call",
                "share.create",
                "--params",
                params,
            ])
            .output()
            .expect("run shux")
    };
    let missing = call(r#"{"session":"demo"}"#);
    let text = String::from_utf8_lossy(&missing.stdout);
    assert!(
        text.contains("from_pane") && !text.contains("https://"),
        "{text}"
    );
    assert!(h.links().is_empty());
    let outside = call(r#"{"session":"demo","from_pane":null}"#);
    let made: serde_json::Value = serde_json::from_slice(&outside.stdout).expect("json");
    assert_eq!(made["result"]["deliver"], "print", "{made}");
}

#[test]
fn a_qr_code_is_never_dropped_from_json() {
    let h = Harness::new();
    h.session("demo");
    let out = h.run(&["--format", "json", "share", "-s", "demo", "--qr"]);
    assert!(!out.status.success());
    let err: serde_json::Value = serde_json::from_slice(&out.stdout).expect("a json error");
    assert_eq!(err["error"]["data"]["reason"], "qr_json", "{err}");
    assert!(h.links().is_empty());
}

#[test]
fn list_and_stop_refuse_unknown_fields() {
    let h = Harness::new();
    for method in ["share.list", "share.stop"] {
        let out = h.run(&[
            "--format",
            "json",
            "rpc",
            "call",
            method,
            "--params",
            r#"{"al":true}"#,
        ]);
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("invalid_params"), "{method}: {text}");
    }
}

#[test]
fn a_connector_that_closes_its_log_is_still_reaped_on_stop() {
    let h = Harness::with_mode("closes-stderr");
    h.session("demo");
    h.json(&["share", "-s", "demo"]);
    let t = h.tunnel();
    h.json(&["share", "stop"]);
    wait_until("stop to reap a connector with no log", || {
        !h.cloudflared_runs(t.cloudflared)
    });
}

#[test]
fn a_connector_that_closes_its_log_dies_with_the_daemon() {
    let h = Harness::with_mode("closes-stderr");
    h.session("demo");
    h.json(&["share", "-s", "demo"]);
    let t = h.tunnel();
    let daemon = h.daemon_pid().expect("a daemon");
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(daemon),
        nix::sys::signal::Signal::SIGKILL,
    )
    .expect("SIGKILL the daemon");
    wait_until("the connector to be reaped", || {
        !h.cloudflared_runs(t.cloudflared)
    });
    wait_until("the port to be free", || {
        std::net::TcpListener::bind(("127.0.0.1", t.port)).is_ok()
    });
}

#[test]
fn a_connector_that_exits_leaving_a_child_takes_the_child_and_the_tunnel() {
    let h = Harness::with_mode("forks-then-exits");
    h.session("demo");
    // It may or may not get a link out before the exit is seen.
    let _ = h.run(&["share", "-s", "demo"]);
    let child: i32 = wait_for("the stand-in's child", || {
        std::fs::read_to_string(h.fake.path().join("child.pid"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
    });
    wait_until("the child left in its group to be killed", || {
        !running(child, &format!("{}1", h.needle))
    });
    wait_until("the tunnel to read as down", || {
        h.json(&["share", "list"])["tunnel"] == "down"
    });
    assert!(h.links().is_empty());
}

#[test]
fn list_answers_while_a_tunnel_is_starting() {
    let h = Harness::with_mode("silent");
    h.session("demo");
    let pending = h
        .command()
        .args(["share", "-s", "demo"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("start sharing");
    h.tunnel();
    assert_eq!(h.json(&["share", "list"])["tunnel"], "starting");
    h.json(&["share", "stop"]);
    let _ = pending.wait_with_output();
}

/// A clipboard that never returns, and says when it started.
fn hanging_clipboard(dir: &Path) -> PathBuf {
    let bin = dir.join("hang");
    std::fs::create_dir_all(&bin).expect("bin dir");
    let script = format!(
        "#!/bin/sh\necho $$ > '{}'\nexec /bin/sleep 297.25\n",
        dir.join("pbcopy.pid").display()
    );
    use std::os::unix::fs::PermissionsExt;
    for name in ["pbcopy", "xclip"] {
        let p = bin.join(name);
        std::fs::write(&p, &script).expect("write the clipboard");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    bin
}

#[test]
fn a_copy_interrupted_by_ctrl_c_revokes_its_link() {
    let h = Harness::new();
    let pane = h.session("demo");
    let bin = hanging_clipboard(h.fake.path());
    let copying = h
        .command()
        .env("SHUX_PANE_ID", &pane)
        .env("PATH", &bin)
        .env("DISPLAY", ":99")
        .args(["share", "-s", "demo"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("start sharing");
    wait_for("the clipboard to start", || {
        std::fs::read_to_string(h.fake.path().join("pbcopy.pid")).ok()
    });
    assert_eq!(h.links().len(), 1);
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(copying.id() as i32),
        nix::sys::signal::Signal::SIGINT,
    )
    .expect("SIGINT the CLI");
    let out = copying.wait_with_output().expect("the CLI returns");
    assert!(!out.status.success());
    assert!(!String::from_utf8_lossy(&out.stdout).contains("https://"));
    assert!(
        h.links().is_empty(),
        "the link outlived its interrupted copy"
    );
}

#[test]
fn a_share_killed_before_it_delivers_leaves_no_link() {
    let h = Harness::new();
    let pane = h.session("demo");
    let bin = hanging_clipboard(h.fake.path());
    let mut copying = h
        .command()
        .env("SHUX_PANE_ID", &pane)
        .env("PATH", &bin)
        .env("DISPLAY", ":99")
        .args(["share", "-s", "demo"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("start sharing");
    let clipboard: i32 = wait_for("the clipboard to start", || {
        std::fs::read_to_string(h.fake.path().join("pbcopy.pid"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
    });
    copying.kill().expect("SIGKILL the CLI");
    copying.wait().expect("reap the CLI");
    // Nothing of the CLI's survives SIGKILL to end its clipboard.
    assert!(shux_testkit::terminate(clipboard));
    wait_until("the undelivered link to be revoked", || {
        h.links().is_empty()
    });
}

#[test]
fn a_clipboard_that_never_returns_times_out_and_revokes() {
    let h = Harness::new();
    let pane = h.session("demo");
    let bin = hanging_clipboard(h.fake.path());
    let out = h
        .command()
        .env("SHUX_PANE_ID", &pane)
        .env("PATH", &bin)
        .env("DISPLAY", ":99")
        .args(["share", "-s", "demo"])
        .output()
        .expect("run shux");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("clipboard") && err.contains("revoked"),
        "{err}"
    );
    assert!(h.links().is_empty());
}

#[test]
fn a_label_with_invisible_characters_is_refused() {
    let h = Harness::new();
    h.session("demo");
    for label in ["re\u{200b}view", "a\u{202e}b", "\u{feff}x"] {
        let out = h.run(&["share", "-s", "demo", "--label", label]);
        assert!(!out.status.success(), "{label:?} was accepted");
    }
    assert!(h.links().is_empty());
}
