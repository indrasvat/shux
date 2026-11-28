//! Workspace restore (#329), end to end: a real daemon, killed or stopped in
//! each way a daemon ends, and the real next daemon that brings its workspace
//! back. Every wait is for a condition the daemon reports, never for time.

mod pane_e2e;

use pane_e2e::{COLOUR_PROBE, Env};
use serde_json::Value;

/// A named thing to do to a fixture.
type Step<'a> = (&'a str, &'a dyn Fn(&Env));

fn json(env: &Env, args: &[&str]) -> Value {
    let mut full = vec!["--format", "json"];
    full.extend_from_slice(args);
    let out = env.ok(&full);
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("{args:?}: {e}: {out}"))
}

fn pane_ids(env: &Env, session: &str) -> Vec<String> {
    let mut ids: Vec<String> = json(env, &["pane", "list", "-s", session])
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| p["id"].as_str().unwrap().to_string())
        .collect();
    ids.sort();
    ids
}

fn sessions(env: &Env) -> Vec<String> {
    json(env, &["session", "list"])["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .map(|s| s["name"].as_str().unwrap().to_string())
        .collect()
}

fn daemon_pid(env: &Env) -> i32 {
    std::fs::read_to_string(env.path("runtime/shux/shux.pid"))
        .expect("pidfile")
        .trim()
        .parse()
        .expect("pid")
}

/// SIGKILL the daemon and wait until it is gone: no final save happens.
fn kill_daemon(env: &Env) {
    let pid = daemon_pid(env);
    // SAFETY: plain kill(2) on a pid this fixture's pidfile names.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
    shux_testkit::wait_until("the killed daemon to go", || unsafe {
        libc::kill(pid, 0) != 0
    });
}

/// Wait until the daemon has saved every change it holds, and `also` holds of
/// the saved file.
fn wait_saved(env: &Env, also: impl Fn(&str) -> bool) {
    shux_testkit::wait_until("the workspace to be saved", || {
        let st = json(env, &["daemon", "status"]);
        let ws = &st["workspace"];
        ws["state"] == "ok"
            && ws["durable_version"] == ws["graph_version"]
            && saved_file(env).is_some_and(|f| also(&f))
    });
}

fn server_dir(env: &Env) -> Option<std::path::PathBuf> {
    let servers = env.path("state/shux/servers");
    Some(std::fs::read_dir(servers).ok()?.next()?.ok()?.path())
}

fn saved_file(env: &Env) -> Option<String> {
    std::fs::read_to_string(server_dir(env)?.join("workspace.json")).ok()
}

/// Run `cmd` in the pane's shell and return what it printed.
fn run(env: &Env, session: &str, pane: &str, cmd: &str) -> String {
    let r = json(
        env,
        &[
            "pane",
            "run",
            "-s",
            session,
            "-p",
            pane,
            "--timeout",
            "60s",
            "--boundaries",
            "markers",
            "-c",
            cmd,
        ],
    );
    r["stdout"].as_str().unwrap_or("").trim().to_string()
}

/// Everything restore promises to keep, as the CLI reports it.
fn picture(env: &Env, session: &str) -> Value {
    let windows = json(env, &["window", "list", "-s", session]);
    let mut layouts = Vec::new();
    for w in windows.as_array().unwrap() {
        let id = w["id"].as_str().unwrap();
        layouts.push(json(
            env,
            &[
                "window", "layout", "-s", session, "-w", id, "--cols", "120", "--rows", "40",
            ],
        ));
    }
    let mut panes: Vec<Value> = json(env, &["pane", "list", "-s", session])
        .as_array()
        .unwrap()
        .iter()
        .map(|p| serde_json::json!({"id": p["id"], "title": p["title"], "focused": p["is_focused"], "zoomed": p["is_zoomed"], "window": p["window_id"]}))
        .collect();
    panes.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    let windows: Vec<Value> = windows
        .as_array()
        .unwrap()
        .iter()
        .map(|w| serde_json::json!({"id": w["id"], "title": w["title"], "active": w["is_active"], "index": w["index"]}))
        .collect();
    serde_json::json!({"windows": windows, "layouts": layouts, "panes": panes})
}

/// A session with a window whose root split has both branches split again,
/// a manual and a pinned title, focus away from the first pane, and a second
/// window holding a zoomed pane. One pane has `cd`'d away from where it
/// started. Returns that pane and the directory it is in.
fn workspace(env: &mut Env) -> (String, std::path::PathBuf) {
    let start = env.work("start");
    let moved = env.work("start/moved");
    std::fs::create_dir_all(&moved).unwrap();
    env.ok(&[
        "session",
        "create",
        "ws",
        "-d",
        "--cwd",
        start.to_str().unwrap(),
    ]);
    env.owns("ws");
    let p0 = pane_ids(env, "ws").remove(0);
    let split = |env: &Env, pane: &str, dir: &str, ratio: &str| {
        let before = pane_ids(env, "ws");
        env.ok(&[
            "pane", "split", "-s", "ws", "-p", pane, "-d", dir, "-r", ratio,
        ]);
        pane_ids(env, "ws")
            .into_iter()
            .find(|p| !before.contains(p))
            .expect("new pane")
    };
    let p1 = split(env, &p0, "vertical", "0.3");
    split(env, &p0, "horizontal", "0.6");
    let p3 = split(env, &p1, "horizontal", "0.25");
    env.ok(&["pane", "title", "-s", "ws", "-p", &p0, "-t", "left-top"]);
    env.ok(&["pane", "title", "-s", "ws", "-p", &p3, "--no-auto"]);
    env.ok(&["pane", "focus", "-s", "ws", "-p", &p1]);

    env.ok(&[
        "window",
        "create",
        "-s",
        "ws",
        "-n",
        "second",
        "--cwd",
        start.to_str().unwrap(),
    ]);
    let second: Vec<String> = json(env, &["pane", "list", "-s", "ws", "-w", "second"])
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap().to_string())
        .collect();
    let z = split(env, &second[0], "vertical", "0.5");
    env.ok(&["pane", "zoom", "-s", "ws", "-p", &z]);
    env.ok(&["window", "focus", "-s", "ws", "-w", "1"]);

    run(env, "ws", &p1, &format!("cd '{}'", moved.display()));
    (p1, moved.canonicalize().unwrap())
}

/// Every way a daemon ends keeps the workspace: `kill -9`, `daemon stop`,
/// `daemon upgrade --force`, and a restart whose runtime directory is gone
/// (a reboot). Ids, layout, titles, focus, zoom and each pane's directory
/// come back; the restored shells run.
#[test]
fn every_way_a_daemon_ends_keeps_its_workspace() {
    let mut env = Env::new(120, 40);
    let (moved_pane, moved) = workspace(&mut env);
    let want_dir = moved.to_string_lossy().into_owned();
    wait_saved(&env, |f| f.contains(&want_dir));
    let before = picture(&env, "ws");

    let ends: [Step; 4] = [
        ("kill -9", &|env| kill_daemon(env)),
        ("daemon stop", &|env| {
            env.ok(&["daemon", "stop"]);
        }),
        ("daemon upgrade --force", &|env| {
            env.ok(&["daemon", "upgrade", "--force"]);
        }),
        ("runtime dir removed", &|env| {
            kill_daemon(env);
            std::fs::remove_dir_all(env.path("runtime")).unwrap();
            std::fs::create_dir_all(env.path("runtime")).unwrap();
        }),
    ];
    for (how, end) in ends {
        end(&env);
        let after = picture(&env, "ws");
        for part in ["windows", "layouts", "panes"] {
            assert_eq!(
                serde_json::to_string_pretty(&after[part]).unwrap(),
                serde_json::to_string_pretty(&before[part]).unwrap(),
                "after {how}: {part} changed"
            );
        }
        assert_eq!(
            run(&env, "ws", &moved_pane, "pwd"),
            want_dir,
            "after {how}: wrong directory"
        );
        run(
            &env,
            "ws",
            &moved_pane,
            &format!("printf '{COLOUR_PROBE}\\n'"),
        );
        env.assert_colour_probe(&moved_pane);
        wait_saved(&env, |f| f.contains(&want_dir));
    }
}

/// A close is saved before it is acknowledged: a daemon killed the moment
/// a kill returns does not bring the closed thing back.
#[test]
fn a_close_stays_closed_when_the_daemon_dies_at_once() {
    let mut env = Env::new(100, 30);
    env.ok(&["session", "create", "keep", "-d"]);
    env.owns("keep");
    env.ok(&["session", "create", "doomed", "-d"]);
    let keep_pane = pane_ids(&env, "keep").remove(0);
    env.ok(&[
        "pane", "split", "-s", "keep", "-p", &keep_pane, "-d", "vertical",
    ]);
    env.ok(&["window", "create", "-s", "keep", "-n", "extra"]);
    wait_saved(&env, |f| f.contains("doomed") && f.contains("extra"));

    let first_window = |env: &Env| -> Vec<String> {
        json(env, &["pane", "list", "-s", "keep", "-w", "1"])
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["id"].as_str().unwrap().to_string())
            .collect()
    };
    let doomed_pane = first_window(&env)
        .into_iter()
        .find(|p| *p != keep_pane)
        .unwrap();
    env.ok(&["pane", "kill", "-s", "keep", "-p", &doomed_pane]);
    kill_daemon(&env);
    assert_eq!(
        first_window(&env),
        vec![keep_pane],
        "a killed pane came back"
    );

    env.ok(&["window", "kill", "-s", "keep", "-w", "extra"]);
    kill_daemon(&env);
    let windows = json(&env, &["window", "list", "-s", "keep"]);
    assert_eq!(
        windows.as_array().unwrap().len(),
        1,
        "a killed window came back: {windows}"
    );

    env.ok(&["session", "kill", "doomed"]);
    kill_daemon(&env);
    assert_eq!(sessions(&env), vec!["keep"], "a killed session came back");
}

/// A workspace the next daemon cannot trust is set aside, byte for byte, and
/// the daemon starts empty and says why: a corrupt file, one from a newer
/// build, and one whose last restore crashed.
#[test]
fn a_workspace_it_cannot_trust_is_set_aside() {
    let mut env = Env::new(100, 30);
    env.ok(&["session", "create", "s", "-d"]);
    env.owns("s");
    wait_saved(&env, |f| f.contains("\"s\""));
    env.ok(&["daemon", "stop"]);
    let dir = server_dir(&env).expect("a server dir");
    let good = std::fs::read(dir.join("workspace.json")).unwrap();
    let future =
        String::from_utf8(good.clone())
            .unwrap()
            .replacen("\"schema\":1", "\"schema\":2", 1);
    let cases: [(&str, Vec<u8>, bool); 3] = [
        (
            "invalid",
            b"{\"schema\":1,\"sessions\":[{}]}".to_vec(),
            false,
        ),
        ("future-schema", future.into_bytes(), false),
        ("crashed-during-restore", good.clone(), true),
    ];
    for (reason, bytes, marked) in cases {
        std::fs::write(dir.join("workspace.json"), &bytes).unwrap();
        if marked {
            // Two restores of these bytes already died part way.
            std::fs::write(dir.join("restoring"), "2").unwrap();
        }
        assert_eq!(
            sessions(&env),
            Vec::<String>::new(),
            "{reason}: restored anyway"
        );
        let status = json(&env, &["daemon", "status"]);
        let q = status["workspace"]["quarantined"].as_str().unwrap_or("");
        assert!(q.starts_with(reason), "{reason}: status says {status}");
        let kept = std::fs::read_dir(dir.join("quarantine"))
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| {
                e.file_name().to_string_lossy().contains(reason)
                    && std::fs::read(e.path()).unwrap() == bytes
            });
        assert!(kept, "{reason}: the refused file was not kept");
        assert!(
            !dir.join("restoring").exists(),
            "{reason}: marker left behind"
        );
        env.ok(&["daemon", "stop"]);
    }
}

/// `daemon stop --discard` forgets the workspace whether the daemon runs,
/// is stopped (SIGSTOP) or is not there at all.
#[test]
fn discard_forgets_the_workspace() {
    let mut env = Env::new(100, 30);
    let states: [Step; 4] = [
        ("running", &|_| {}),
        ("stopped", &|env| {
            // SAFETY: kill(2) on this fixture's own daemon.
            assert_eq!(unsafe { libc::kill(daemon_pid(env), libc::SIGSTOP) }, 0);
        }),
        ("absent", &|env| kill_daemon(env)),
        ("after a reboot", &|env| {
            kill_daemon(env);
            std::fs::remove_dir_all(env.path("runtime/shux")).unwrap();
        }),
    ];
    for (state, prepare) in states {
        env.ok(&["session", "create", "gone", "-d"]);
        env.owns("gone");
        wait_saved(&env, |f| f.contains("gone"));
        prepare(&env);
        env.ok(&["daemon", "stop", "--discard"]);
        assert_eq!(
            sessions(&env),
            Vec::<String>::new(),
            "{state}: restored after discard"
        );
    }
}

/// A shell that ignores SIGHUP outlives a `kill -9`'d daemon. The next daemon
/// ends it before starting a fresh shell in its pane, and so does
/// `daemon stop --discard`, which deletes the only record of it.
#[test]
fn a_shell_left_by_a_killed_daemon_is_reaped() {
    let mut env = Env::new(100, 30);
    let ends: [Step; 2] = [
        ("restart", &|env| {
            env.ok(&["session", "list"]);
        }),
        ("discard", &|env| {
            env.ok(&["daemon", "stop", "--discard"]);
        }),
    ];
    for (how, end) in ends {
        let pidfile = env.work(&format!("{how}.pid"));
        let script = format!(
            "trap '' HUP; echo $$ > '{}'; while :; do sleep 1; done",
            pidfile.display()
        );
        env.ok(&["session", "create", how, "-d", "--", "sh", "-c", &script]);
        env.owns(how);
        let orphan: i32 = shux_testkit::wait_for("the pane's shell pid", || {
            std::fs::read_to_string(&pidfile).ok()?.trim().parse().ok()
        });
        wait_saved(&env, |f| f.contains(how) && f.contains("start_token"));
        kill_daemon(&env);
        // SAFETY: kill(2) with signal 0 only probes.
        assert_eq!(
            unsafe { libc::kill(orphan, 0) },
            0,
            "{how}: the shell did not survive"
        );
        end(&env);
        shux_testkit::wait_until(
            &format!("{how}: the orphaned shell to be reaped"),
            || unsafe { libc::kill(orphan, 0) != 0 },
        );
    }
}

/// A state directory made read-only can save nothing, so a session killed
/// there would come back from the old file: the daemon refuses to restore
/// from a directory it cannot write.
#[test]
fn a_read_only_state_dir_never_resurrects_a_close() {
    let mut env = Env::new(100, 30);
    env.ok(&["session", "create", "keep", "-d"]);
    env.owns("keep");
    env.ok(&["session", "create", "doomed", "-d"]);
    wait_saved(&env, |f| f.contains("doomed"));
    let dir = server_dir(&env).expect("a server dir");
    let mode = |m| std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(m));
    mode(0o500).unwrap();
    // Nothing durable can be recorded: refused, saying where.
    let refused = env.run(&["session", "kill", "doomed"]);
    mode(0o700).unwrap();
    let said = String::from_utf8_lossy(&refused.stdout).into_owned()
        + &String::from_utf8_lossy(&refused.stderr);
    assert!(
        !refused.status.success() && said.contains("not done") && said.contains("servers"),
        "{said}"
    );
    assert!(sessions(&env).contains(&"doomed".to_string()));
    env.ok(&["session", "kill", "doomed"]);
    kill_daemon(&env);
    let back = sessions(&env);
    assert!(
        !back.contains(&"doomed".to_string()),
        "a killed session came back: {back:?}"
    );
}

/// Killing a session ends every pane's program, a pane hidden behind a zoom
/// included.
#[test]
fn a_session_kill_ends_a_pane_hidden_by_zoom() {
    let env = Env::new(100, 30);
    let pidfile = env.work("hidden.pid");
    env.ok(&["session", "create", "z", "-d"]);
    let first = pane_ids(&env, "z").remove(0);
    let cmd = format!("echo $$ > '{}'; exec sleep 600", pidfile.display());
    env.ok(&[
        "pane", "split", "-s", "z", "-p", &first, "-d", "vertical", "--cmd", &cmd,
    ]);
    let hidden: i32 = shux_testkit::wait_for("the hidden pane's shell pid", || {
        std::fs::read_to_string(&pidfile).ok()?.trim().parse().ok()
    });
    env.ok(&["pane", "focus", "-s", "z", "-p", &first]);
    env.ok(&["pane", "zoom", "-s", "z", "-p", &first]);
    env.ok(&["session", "kill", "z"]);
    shux_testkit::wait_until("the hidden pane's program to end", || unsafe {
        libc::kill(hidden, 0) != 0
    });
}

/// A pane whose shell cannot start again comes back as an exited pane that
/// says why, never as a pane with no program behind it.
#[test]
fn a_pane_that_cannot_restart_says_why() {
    let mut env = Env::new(100, 30);
    env.ok(&["session", "create", "s", "-d"]);
    env.owns("s");
    wait_saved(&env, |f| f.contains("\"s\""));
    env.ok(&["daemon", "stop"]);
    let out = env
        .shux()
        .env("SHELL", "/nonexistent/shell")
        .args(["--format", "json", "pane", "list", "-s", "s"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let pane = pane_ids(&env, "s").remove(0);
    shux_testkit::wait_until("the pane to be marked exited", || {
        json(&env, &["pane", "list", "-s", "s"])[0]["exit_status"] == 127
    });
    let screen = env.capture("s", &pane);
    assert!(screen.contains("could not restart"), "{screen}");
}

/// `[restore] workspace = false`: nothing is saved and nothing comes back.
#[test]
fn restore_off_saves_and_restores_nothing() {
    let mut env = Env::new(100, 30);
    std::fs::write(
        env.path("config/shux/config.toml"),
        "[restore]\nworkspace = false\n",
    )
    .unwrap();
    env.ok(&["session", "create", "x", "-d"]);
    env.owns("x");
    let st = json(&env, &["daemon", "status"]);
    assert_eq!(st["workspace"]["state"], "disabled", "{st}");
    kill_daemon(&env);
    assert_eq!(sessions(&env), Vec::<String>::new());
    assert!(saved_file(&env).is_none(), "a workspace was written");
}
