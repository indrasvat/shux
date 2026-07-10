//! Shared black-box harness for the lens red suite (§12 TEST-2 / §13 TEST-3).
//!
//! FROZEN after Phase P0 (§16.2). Any change to files under
//! `crates/shux/tests/lens_*` requires a `LENS-TEST-CHANGE:` commit trailer.
//!
//! The red suite drives the system ONLY through the `shux` CLI binary and
//! `shux rpc call` — never through in-process daemon internals. Every lens
//! capability is exercised as an RPC method first, so in Phase P0 (no
//! implementation) each lens test fails with `method_not_found (-32601)` or a
//! missing result field — that observed failure log is the red receipt.
//!
//! Pre-P5 tests use ORDINARY sessions (scratch ships in P5): create a session,
//! size the pane with `pane.set_size`, `exec` the fixture over the pane shell,
//! and wait for a fixture sentinel. Fixtures are referenced by explicit
//! repo-relative path (`sh .shux/fixtures/lens/fN.sh`).

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

/// Single documented CI-class tolerance for timing assertions (S3/S4/R*).
/// Raising it is a LENS-TEST-CHANGE event (§17), never a silent widening.
pub const LENS_TEST_TOL_MS: u64 = 500;

/// One JSON-RPC error `{code,message,data}` as surfaced by `shux rpc call`.
#[derive(Debug, Clone)]
pub struct RpcErr {
    pub code: i64,
    pub message: String,
    pub data: Option<serde_json::Value>,
}

/// Parsed `{result|error}` envelope from `shux --format json rpc call ...`,
/// plus the CLI process exit code.
#[derive(Debug, Clone)]
pub struct RpcEnvelope {
    pub result: Option<serde_json::Value>,
    pub error: Option<RpcErr>,
    pub exit_code: i32,
    pub raw_stdout: String,
}

impl RpcEnvelope {
    /// Assert a successful result and return it. In Phase P0 the lens methods
    /// are unregistered, so this panics with the `-32601` root cause — the
    /// intended red-receipt failure.
    pub fn expect_result(&self, ctx: &str) -> serde_json::Value {
        if let Some(err) = &self.error {
            panic!(
                "{ctx}: expected a result but got RPC error code={} message={:?} data={:?}\n\
                 (Phase-P0 red receipt: this is the missing lens RPC method / field — \
                 method_not_found is -32601.)",
                err.code, err.message, err.data
            );
        }
        self.result.clone().unwrap_or_else(|| {
            panic!(
                "{ctx}: envelope had neither result nor error:\n{}",
                self.raw_stdout
            )
        })
    }

    /// Assert an RPC error with a specific code and return it.
    pub fn expect_error_code(&self, code: i64, ctx: &str) -> RpcErr {
        match &self.error {
            Some(err) if err.code == code => err.clone(),
            Some(err) => panic!(
                "{ctx}: expected RPC error {code} but got {} ({:?})",
                err.code, err.message
            ),
            None => panic!(
                "{ctx}: expected RPC error {code} but call succeeded:\n{:?}",
                self.result
            ),
        }
    }
}

/// A launched fixture session (ordinary, pre-P5 arrangement).
#[derive(Debug, Clone)]
pub struct Fixture {
    pub session_id: String,
    pub pane_id: String,
}

/// Black-box harness: the built `shux` binary against an isolated XDG env.
pub struct Harness {
    bin: PathBuf,
    repo_root: PathBuf,
    runtime: tempfile::TempDir,
    xdg_config: tempfile::TempDir,
    xdg_state: tempfile::TempDir,
}

impl Harness {
    pub fn new() -> Self {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")); // crates/shux
        let repo_root = manifest
            .join("..")
            .join("..")
            .canonicalize()
            .expect("canonicalize repo root");
        let xdg_config = tempfile::tempdir().expect("config dir");

        // LENS-TEST-CHANGE p2-fonts (user adjudication, PRD §17): the
        // harness daemon appends committed FIXTURE fonts to the builtin
        // fallback chain so lens goldens carry real Devanagari + the
        // fixtures' CJK glyphs instead of tofu boxes. This config exists
        // only in the harness's isolated XDG dir — the default raster
        // chain (and the vt-corpus goldens) are untouched. The bundled
        // primary is NOT replaced, so cell metrics stay identical to the
        // default chain. Provenance + sha256: .shux/fixtures/fonts/ and
        // the lens evidence manifest.
        let dev_font = repo_root.join(".shux/fixtures/fonts/NotoSansDevanagari-Regular.ttf");
        let cjk_font = repo_root.join(".shux/fixtures/fonts/NotoSansJP-shuxlens-subset.ttf");
        let config_dir = xdg_config.path().join("shux");
        std::fs::create_dir_all(&config_dir).expect("create shux config dir");
        std::fs::write(
            config_dir.join("config.toml"),
            format!(
                "[appearance]\nfont_fallbacks = [\n  \
                 \"builtin:nerd-font\",\n  \
                 {dev:?},\n  \
                 {cjk:?},\n  \
                 \"builtin:math\",\n  \
                 \"builtin:symbols\",\n  \
                 \"builtin:symbols-legacy\",\n  \
                 \"builtin:emoji\",\n]\n",
                dev = dev_font.display().to_string(),
                cjk = cjk_font.display().to_string(),
            ),
        )
        .expect("write lens harness config.toml");

        Self {
            bin: PathBuf::from(env!("CARGO_BIN_EXE_shux")),
            repo_root,
            runtime: tempfile::tempdir().expect("runtime dir"),
            xdg_config,
            xdg_state: tempfile::tempdir().expect("state dir"),
        }
    }

    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    pub fn runtime_dir(&self) -> &Path {
        self.runtime.path()
    }

    pub fn state_dir(&self) -> &Path {
        self.xdg_state.path()
    }

    /// A `shux` command with the isolated XDG env and cwd == repo root (so
    /// repo-relative fixture paths resolve and the auto-started daemon inherits
    /// the repo root as its cwd).
    ///
    /// Deliberately does NOT set NO_COLOR/CLICOLOR (p0-council-r1 major 2): the
    /// auto-started daemon inherits this environment and would propagate it to
    /// every pane child, poisoning T-tier color assertions. No-color cases
    /// inject NO_COLOR per-test (T3 `env` param); color cases assert its
    /// absence via non-grayscale pixel checks.
    pub fn shux(&self) -> Command {
        let mut cmd = Command::new(&self.bin);
        cmd.current_dir(&self.repo_root)
            .env("XDG_RUNTIME_DIR", self.runtime.path())
            .env("XDG_CONFIG_HOME", self.xdg_config.path())
            .env("XDG_STATE_HOME", self.xdg_state.path())
            .env_remove("NO_COLOR")
            .env_remove("CLICOLOR")
            .env("SHELL", "/bin/sh");
        cmd
    }

    /// Run `shux <args...>` and return the raw process output (for CLI exit-code
    /// and file-output assertions).
    pub fn cli(&self, args: &[&str]) -> Output {
        self.shux()
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("failed to run shux {args:?}: {e}"))
    }

    /// Raw `shux rpc call` — never panics on an RPC-level error; returns the
    /// parsed envelope so tests can assert on `-32601` etc.
    pub fn rpc_raw(&self, method: &str, params: serde_json::Value) -> RpcEnvelope {
        let params = params.to_string();
        let out = self
            .shux()
            .args([
                "--format", "json", "rpc", "call", method, "--params", &params,
            ])
            .output()
            .unwrap_or_else(|e| panic!("failed to spawn shux rpc {method}: {e}"));
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let exit_code = out.status.code().unwrap_or(-1);
        let value: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
            panic!(
                "shux rpc {method}: stdout was not JSON: {e}\nstdout:\n{stdout}\nstderr:\n{}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
        let result = value.get("result").cloned();
        let error = value.get("error").map(|e| RpcErr {
            code: e.get("code").and_then(|v| v.as_i64()).unwrap_or(0),
            message: e
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            data: e.get("data").cloned(),
        });
        RpcEnvelope {
            result,
            error,
            exit_code,
            raw_stdout: stdout,
        }
    }

    /// `shux rpc call` that MUST succeed (setup on existing methods). Panics on
    /// any RPC error — used only for pre-lens machinery.
    pub fn rpc_ok(&self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.rpc_raw(method, params).expect_result(method)
    }

    /// Run a lens CLI subcommand with `--format json` and parse the raw RPC
    /// `{result|error}` envelope it emits (§10: json format emits the raw RPC
    /// result envelope). The CLI-parity twin of `rpc_raw` (M9). In Phase P0 the
    /// lens subcommands do not exist, so clap rejects them — the panic message
    /// carries stderr, rooting the failure in the missing CLI verb.
    pub fn cli_envelope(&self, args: &[&str]) -> RpcEnvelope {
        let mut full: Vec<&str> = vec!["--format", "json"];
        full.extend_from_slice(args);
        let out = self.cli(&full);
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let exit_code = out.status.code().unwrap_or(-1);
        let value: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
            panic!(
                "shux {args:?}: stdout was not a JSON envelope (exit {exit_code}): {e}\n\
                 stdout:\n{stdout}\nstderr:\n{}\n\
                 (Phase-P0 red receipt: missing lens CLI verb — subcommand not implemented.)",
                String::from_utf8_lossy(&out.stderr)
            )
        });
        let result = value.get("result").cloned();
        let error = value.get("error").map(|e| RpcErr {
            code: e.get("code").and_then(|v| v.as_i64()).unwrap_or(0),
            message: e
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            data: e.get("data").cloned(),
        });
        RpcEnvelope {
            result,
            error,
            exit_code,
            raw_stdout: stdout,
        }
    }

    // ── Fixture launch (ordinary session; §12 discipline) ────────────────

    /// Explicit repo-relative fixture path (delta 4: never bare names).
    pub fn fixture_rel(name: &str) -> String {
        format!(".shux/fixtures/lens/{name}")
    }

    /// The repo-relative fixture path anchored at THIS harness's repo root
    /// (p0-council-r3 item 1). Every fixture spawn uses this exact absolute
    /// form so `count_fixture_procs` can match argv anchored at the start —
    /// immune to co-tenant processes that merely mention a fixture filename.
    pub fn fixture_abs(&self, name: &str) -> String {
        self.repo_root
            .join(Self::fixture_rel(name))
            .display()
            .to_string()
    }

    /// Create an ordinary session, size the pane, `exec` the fixture over the
    /// pane shell, and block until `sentinel` is visible. Returns the ids.
    pub fn launch_fixture(&self, name: &str, cols: u16, rows: u16, sentinel: &str) -> Fixture {
        let session_name = format!("lens-{}-{}", name.replace(['.', '_'], "-"), unique());
        let created = self.rpc_ok(
            "session.create",
            serde_json::json!({
                "name": session_name,
                "cwd": self.repo_root.display().to_string(),
            }),
        );
        let session_id = created["id"].as_str().expect("session id").to_string();
        let pane_id = created["pane_id"].as_str().expect("pane id").to_string();

        // Size BEFORE the fixture draws (pane.set_size is synchronous).
        self.rpc_ok(
            "pane.set_size",
            serde_json::json!({ "pane_id": pane_id, "cols": cols, "rows": rows }),
        );

        // Replace the pane shell with the fixture. The absolute-anchored path
        // keeps the argv matchable by count_fixture_procs (r3 item 1); `exec`
        // leaves no residual shell to reap.
        let abs = self.fixture_abs(name);
        self.rpc_ok(
            "pane.send_keys",
            serde_json::json!({ "pane_id": pane_id, "text": format!("exec sh {abs}\n") }),
        );

        self.wait_for(&pane_id, sentinel, 10_000)
            .unwrap_or_else(|e| panic!("fixture {name} never drew sentinel {sentinel:?}: {e}"));

        Fixture {
            session_id,
            pane_id,
        }
    }

    pub fn kill_session(&self, session_id: &str) {
        let _ = self.rpc_raw("session.kill", serde_json::json!({ "id": session_id }));
    }

    // ── Pre-lens driving primitives (existing RPC methods) ───────────────

    /// Send a newline-terminated token to a read-based fixture (F2/F3/F8/F9/F10).
    pub fn send_line_token(&self, pane_id: &str, tok: &str) {
        self.rpc_ok(
            "pane.send_keys",
            serde_json::json!({ "pane_id": pane_id, "text": format!("{tok}\n") }),
        );
    }

    /// Send raw bytes (F4 raw-mode single-byte tokens; e.g. "a", "s", "\t").
    pub fn send_raw(&self, pane_id: &str, bytes: &str) {
        self.rpc_ok(
            "pane.send_keys",
            serde_json::json!({ "pane_id": pane_id, "text": bytes }),
        );
    }

    /// Pump N empty newline tokens in a single write (max-rate frame flips).
    pub fn pump_line_tokens(&self, pane_id: &str, count: usize) {
        let blob = "\n".repeat(count);
        self.rpc_ok(
            "pane.send_keys",
            serde_json::json!({ "pane_id": pane_id, "text": blob }),
        );
    }

    pub fn wait_for(&self, pane_id: &str, text: &str, timeout_ms: u64) -> Result<(), String> {
        let env = self.rpc_raw(
            "pane.wait_for",
            serde_json::json!({ "pane_id": pane_id, "text": text, "timeout_ms": timeout_ms }),
        );
        if env.result.is_some() {
            Ok(())
        } else {
            Err(format!("{:?}", env.error))
        }
    }

    pub fn capture_text(&self, pane_id: &str) -> String {
        self.rpc_ok("pane.capture", serde_json::json!({ "pane_id": pane_id }))["text"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    pub fn session_snapshot(&self, session_id: &str) -> serde_json::Value {
        self.rpc_ok(
            "session.snapshot",
            serde_json::json!({ "session_id": session_id }),
        )
    }

    /// A raw pane PNG plus the raster's fixed cell metrics (cell_width,
    /// cell_height). Same rasterizer/fonts as `pane.glance`, so this maps
    /// glance-cell coordinates → pixels for the G1 probes.
    pub fn snapshot_png(&self, pane_id: &str) -> (Vec<u8>, u32, u32) {
        let snap = self.rpc_ok("pane.snapshot", serde_json::json!({ "pane_id": pane_id }));
        let b64 = snap["png_base64"].as_str().expect("png_base64");
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .expect("decode pane png");
        // Hard-assert the cell metrics (PR #86 bot review): a silent 0 would
        // pin every pixel probe to x=0/y=0 and fake-classify frames.
        let cw = snap["cell_width"]
            .as_u64()
            .unwrap_or_else(|| panic!("pane.snapshot missing cell_width: {snap}"))
            as u32;
        let ch = snap["cell_height"]
            .as_u64()
            .unwrap_or_else(|| panic!("pane.snapshot missing cell_height: {snap}"))
            as u32;
        assert!(
            cw > 0 && ch > 0,
            "pane.snapshot reported zero cell metrics (cw={cw}, ch={ch}) — pixel probes would all pin to origin"
        );
        (bytes, cw, ch)
    }

    /// The pane's `content_revision` as exposed by `session.snapshot`
    /// (LENS-R-006). In P0 the pane entries carry no such field, so the lookup
    /// panics — the intended red-receipt failure for G3/G4.
    pub fn content_revision(&self, session_id: &str, pane_id: &str) -> u64 {
        let snap = self.session_snapshot(session_id);
        snapshot_pane_entry(&snap, pane_id, "content_revision")
            .as_u64()
            .unwrap_or_else(|| panic!("content_revision not a u64 in session.snapshot pane entry"))
    }

    /// The pane's structural `version` as exposed by `session.snapshot`
    /// (used by G4 to prove revision is NOT the graph version).
    pub fn snapshot_pane_structural_version(&self, session_id: &str, pane_id: &str) -> u64 {
        let snap = self.session_snapshot(session_id);
        snapshot_pane_entry(&snap, pane_id, "version")
            .as_u64()
            .unwrap_or_else(|| {
                panic!("structural version not a u64 in session.snapshot pane entry")
            })
    }

    /// The SESSION-level structural `version` as exposed by `session.snapshot`
    /// (p0-council-r1 major 5: G4 asserts session AND pane versions unchanged).
    /// P1 must expose it as a top-level `session_version` field.
    pub fn snapshot_session_structural_version(&self, session_id: &str) -> u64 {
        let snap = self.session_snapshot(session_id);
        snap.get("session_version")
            .and_then(|v| v.as_u64())
            .unwrap_or_else(|| {
                panic!(
                    "session.snapshot has no `session_version` u64 field — the P1 \
                     LENS-R-006 surface must expose the session's structural version.\n\
                     snapshot keys: {:?}",
                    snap.as_object().map(|o| o.keys().collect::<Vec<_>>())
                )
            })
    }

    // ── Scratch / lifecycle helpers (R-series, P5) ───────────────────────

    /// Is `session_id` present in `session.list` (optionally including scratch)?
    pub fn session_listed(&self, session_id: &str, include_scratch: bool) -> bool {
        let params = if include_scratch {
            serde_json::json!({ "include_scratch": true })
        } else {
            serde_json::json!({})
        };
        let list = self.rpc_ok("session.list", params);
        list["sessions"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .any(|s| s.get("id").and_then(|v| v.as_str()) == Some(session_id))
            })
            .unwrap_or(false)
    }

    /// The scratch entry (if any) for `session_id` from `session.list
    /// --include-scratch`.
    pub fn scratch_entry(&self, session_id: &str) -> Option<serde_json::Value> {
        let list = self.rpc_ok(
            "session.list",
            serde_json::json!({ "include_scratch": true }),
        );
        list["sessions"].as_array().and_then(|arr| {
            arr.iter()
                .find(|s| s.get("id").and_then(|v| v.as_str()) == Some(session_id))
                .cloned()
        })
    }

    /// The daemon PID from the isolated runtime dir, if the pidfile exists.
    pub fn daemon_pid(&self) -> Option<i32> {
        let path = self.runtime.path().join("shux").join("shux.pid");
        std::fs::read_to_string(path).ok()?.trim().parse().ok()
    }

    /// `system.health` responds without error.
    pub fn system_health_ok(&self) -> bool {
        self.rpc_raw("system.health", serde_json::json!({}))
            .result
            .is_some()
    }

    /// Count live fixture processes by ANCHORED argv match: only processes
    /// whose command line BEGINS with `sh <abs-repo-root>/.shux/fixtures/lens/
    /// <script>` count (p0-council-r3 item 1). A substring match on the bare
    /// script name false-matched co-tenant processes whose argv merely
    /// mentioned the filename (e.g. a review agent holding the diff text in
    /// its prompt), flaking the EOF-exit proofs under parallel load. Fixtures
    /// are always exec'd with this exact absolute-path argv, so the anchored
    /// prefix is both necessary and sufficient.
    pub fn count_fixture_procs(&self, name: &str) -> usize {
        let prefix = format!("sh {}", self.fixture_abs(name));
        let out = Command::new("ps").args(["-axo", "args="]).output();
        match out {
            Ok(o) => String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| l.trim_start().starts_with(&prefix))
                .count(),
            Err(_) => 0,
        }
    }

    /// Any audit entry whose serialized JSON contains ALL of `needles`.
    /// (The lens audit-log path/schema is a P5 concern; matching on content is
    /// resilient to where the daemon writes it — see spec-questions.)
    pub fn audit_has(&self, needles: &[&str]) -> bool {
        self.audit_entries().iter().any(|e| {
            let s = e.to_string();
            needles.iter().all(|n| s.contains(n))
        })
    }

    // ── Audit-log reading (R1/R4, best-effort broad scan) ────────────────

    /// All NDJSON audit entries found anywhere under the isolated XDG state /
    /// runtime dirs (path of the lens audit log is a P5 concern; scan broadly).
    pub fn audit_entries(&self) -> Vec<serde_json::Value> {
        let mut out = Vec::new();
        for root in [self.xdg_state.path(), self.runtime.path()] {
            collect_audit(root, &mut out);
        }
        out
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        // Terminate the daemon so it reaps every pane child (no leaked shux
        // processes / orphan fixtures). Best-effort; the leak guard is the net.
        let pid_path = self.runtime.path().join("shux").join("shux.pid");
        if let Ok(txt) = std::fs::read_to_string(&pid_path) {
            if let Ok(pid) = txt.trim().parse::<i32>() {
                use nix::sys::signal::{Signal, kill};
                use nix::unistd::Pid;
                let _ = kill(Pid::from_raw(pid), Signal::SIGTERM);
                let deadline = Instant::now() + Duration::from_secs(5);
                while Instant::now() < deadline {
                    if kill(Pid::from_raw(pid), None).is_err() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }
}

// ── free helpers (also frozen) ───────────────────────────────────────────

/// Locate the pane's entry inside a `session.snapshot` result and return the
/// named field. Panics loudly when the snapshot carries no pane entries at all
/// (Phase-P0 state: LENS-R-006 not yet implemented).
pub fn snapshot_pane_entry(
    snap: &serde_json::Value,
    pane_id: &str,
    field: &str,
) -> serde_json::Value {
    let panes = snap
        .get("panes")
        .and_then(|p| p.as_array())
        .unwrap_or_else(|| {
            panic!(
                "session.snapshot has no `panes` array — LENS-R-006 (content_revision \
             on snapshot pane entries) is not implemented yet.\nsnapshot keys: {:?}",
                snap.as_object().map(|o| o.keys().collect::<Vec<_>>())
            )
        });
    let entry = panes
        .iter()
        .find(|p| {
            p.get("pane_id").and_then(|v| v.as_str()) == Some(pane_id)
                || p.get("id").and_then(|v| v.as_str()) == Some(pane_id)
        })
        .unwrap_or_else(|| panic!("pane {pane_id} not found in session.snapshot panes"));
    entry
        .get(field)
        .cloned()
        .unwrap_or_else(|| panic!("field {field:?} missing on session.snapshot pane entry"))
}

fn collect_audit(dir: &Path, out: &mut Vec<serde_json::Value>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for ent in rd.flatten() {
        let path = ent.path();
        if path.is_dir() {
            collect_audit(&path, out);
        } else if path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.contains("audit"))
            .unwrap_or(false)
        {
            if let Ok(text) = std::fs::read_to_string(&path) {
                for line in text.lines() {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                        out.push(v);
                    }
                }
            }
        }
    }
}

/// Decode a PNG once into an RGBA image (p0-council-r1 minor 14: decode once
/// per glance, probe the decoded image many times).
pub fn decode_png(png: &[u8]) -> image::RgbaImage {
    image::load_from_memory(png).expect("decode png").to_rgba8()
}

/// RGBA pixel at the top-left interior of a grid cell of an already-decoded
/// image (avoids the glyph, so it reads the cell BACKGROUND) using the
/// raster's fixed cell metrics.
pub fn probe_cell_bg_img(
    img: &image::RgbaImage,
    col: u32,
    row: u32,
    cw: u32,
    ch: u32,
) -> (u8, u8, u8, u8) {
    let x = (col * cw + 1).min(img.width().saturating_sub(1));
    let y = (row * ch + 1).min(img.height().saturating_sub(1));
    let p = img.get_pixel(x, y);
    (p[0], p[1], p[2], p[3])
}

/// One-shot convenience: decode + probe (single-probe call sites only).
pub fn probe_cell_bg(png: &[u8], col: u32, row: u32, cw: u32, ch: u32) -> (u8, u8, u8, u8) {
    probe_cell_bg_img(&decode_png(png), col, row, cw, ch)
}

/// F3's exact expected background RGB for frame `frame` at grid row `row`.
/// F3 spreads backgrounds by row%3: truecolor / 256-color / basic ANSI. The
/// 256 and basic values are the raster's pinned palette (indexed_to_rgb).
pub fn f3_expected_bg(frame: char, row: u32) -> (u8, u8, u8) {
    match (frame, row % 3) {
        ('A', 0) => (190, 30, 40),  // 48;2;190;30;40
        ('A', 1) => (255, 0, 0),    // 48;5;196 → xterm cube
        ('A', 2) => (205, 49, 49),  // SGR 41 → palette index 1
        ('B', 0) => (30, 60, 200),  // 48;2;30;60;200
        ('B', 1) => (0, 0, 255),    // 48;5;21 → xterm cube
        ('B', 2) => (36, 114, 200), // SGR 44 → palette index 4
        _ => unreachable!("frame is 'A' or 'B'"),
    }
}

/// Classify an F3 background probe as frame 'A' (red) or 'B' (blue) by EXACT
/// expected RGB for that row (p0-council-r1 minor 13). Any other value is a
/// hard failure — a torn/blended/wrong-color pixel must never silently
/// classify as a frame.
pub fn classify_frame_exact(rgba: (u8, u8, u8, u8), row: u32) -> char {
    let rgb = (rgba.0, rgba.1, rgba.2);
    if rgb == f3_expected_bg('A', row) {
        'A'
    } else if rgb == f3_expected_bg('B', row) {
        'B'
    } else {
        panic!(
            "F3 probe at row {row} read {rgba:?} — matches neither frame A {:?} nor \
             frame B {:?} (torn or mis-rendered cell)",
            f3_expected_bg('A', row),
            f3_expected_bg('B', row)
        )
    }
}

/// Golden comparison via the repo's `pixel_verify.py` (exact: 0.0 tolerances).
/// Panics with a mint-me message when the golden is absent — the intended red
/// state AFTER the method exists but BEFORE the golden is approved (§16.3).
pub fn assert_png_golden(harness: &Harness, actual_png: &[u8], golden_rel: &str) {
    let golden = harness
        .repo_root()
        .join(".shux/goldens/lens")
        .join(golden_rel);
    let scratch = std::env::temp_dir().join(format!("lens_actual_{}.png", unique()));
    std::fs::write(&scratch, actual_png).expect("write actual png");
    assert!(
        golden.exists(),
        "golden not found: {} — mint per §16.3 (BASELINE-APPROVAL + evidence-manifest) \
         before this test can go green.",
        golden.display()
    );
    let script = harness
        .repo_root()
        .join(".claude/automations/pixel_verify.py");
    let out = Command::new(&script)
        .arg(&scratch)
        .arg(&golden)
        .output()
        .unwrap_or_else(|e| panic!("run pixel_verify.py: {e}"));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let verdict: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("pixel_verify.py output not JSON: {e}\n{stdout}"));
    assert_eq!(
        verdict.get("status").and_then(|v| v.as_str()),
        Some("pass"),
        "PNG differs from golden {golden_rel}: {stdout}"
    );
}

/// Compare golden text byte-for-byte (LENS-R-012 byte-stability).
pub fn assert_text_golden(harness: &Harness, actual_text: &str, golden_rel: &str) {
    let golden = harness
        .repo_root()
        .join(".shux/goldens/lens")
        .join(golden_rel);
    assert!(
        golden.exists(),
        "text golden not found: {} — mint per §16.3 before this test can go green.",
        golden.display()
    );
    let want = std::fs::read_to_string(&golden).expect("read text golden");
    assert_eq!(actual_text, want, "text differs from golden {golden_rel}");
}

/// T-tier skip gate (§13): if `bin` is not on PATH, print a LOUD notice
/// explaining WHY the test is skipped and return false (the test then returns
/// early — a pass, NOT `#[ignore]`). Skips are allowed only in CI, never for
/// the P6 DoD evidence.
pub fn skip_unless_bin(bin: &str, test: &str) -> bool {
    let found = Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {bin}"))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !found {
        eprintln!(
            "\n=== T-TIER SKIP: {test} ===\n\
             `{bin}` is not installed on this machine, so this real-TUI test is \
             SKIPPED (see §13). This is allowed ONLY in CI — the P6 DoD requires \
             {bin} present with committed SOLID-QA evidence.\n"
        );
    }
    found
}

/// True iff every pixel of `png` is grayscale (R==G==B) — the NO_COLOR anchor
/// check (T3).
pub fn is_grayscale_png(png: &[u8]) -> bool {
    let img = image::load_from_memory(png).expect("decode png").to_rgba8();
    img.pixels().all(|p| p[0] == p[1] && p[1] == p[2])
}

/// Deadline-bounded condition wait. §16.1 permits "deadline-bounded event
/// waits"; scratch reaping is timer-driven, so polling a condition until a
/// bounded deadline is the honest mechanism (NOT output synchronization — no
/// bare sleep waits on drawn output anywhere in this suite).
pub fn wait_until<F: FnMut() -> bool>(timeout: Duration, mut cond: F) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Monotonic-ish unique suffix for names/paths without extra deps.
pub fn unique() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{t}-{n}-{}", std::process::id())
}
