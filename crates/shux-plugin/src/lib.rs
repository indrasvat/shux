//! shux plugin host — process plugins, JSON-RPC over stdin/stdout.
//!
//! Task 044a (phase 0): protocol + spawn + manual install. No hot
//! reload, no override-by-name, no event interception, no sandbox.
//! See docs/tasks/044a-process-plugins-v0.md for the full plan.
//!
//! Wire model:
//! - Daemon → plugin: lines on the child's stdin.
//! - Plugin → daemon: lines on the child's stdout.
//! - Plugin diagnostics: stderr is relayed to the daemon log,
//!   tagged with the plugin name.
//!
//! Three flows multiplexed on the pair:
//! 1. Handshake (`plugin.init` request → manifest response).
//! 2. Plugin → daemon RPC: any registered method on `Router`.
//! 3. Daemon → plugin events: notification frames matching the
//!    plugin's declared `subscribes` filters.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use shux_core::bus::{EventBus, Subscription, SubscriptionEvent};
use shux_rpc::router::Router;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

pub const PROTOCOL_VERSION: &str = "1";
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// What a plugin reports about itself on handshake.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub subscribes: Vec<String>,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// Public, RPC-shaped view of a running plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    pub name: String,
    pub version: String,
    pub source: String,
    pub pid: Option<u32>,
    pub status: String,
    pub uptime_ms: u64,
    pub subscribes: Vec<String>,
    pub provides: Vec<String>,
    pub last_error: Option<String>,
}

/// How to spawn a plugin executable.
#[derive(Debug, Clone)]
pub struct PluginSource {
    pub path: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
}

impl PluginSource {
    pub fn from_path(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            args: Vec::new(),
            cwd: None,
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum PluginError {
    #[error("plugin not found: {0}")]
    NotFound(String),
    #[error("plugin name already installed: {0}")]
    NameConflict(String),
    #[error("plugin handshake failed: {0}")]
    HandshakeFailed(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol error: {0}")]
    Proto(String),
}

struct Running {
    manifest: PluginManifest,
    source: PluginSource,
    inbox_tx: mpsc::Sender<String>,
    kill_tx: Option<oneshot::Sender<()>>,
    started_at: Instant,
    pid: Option<u32>,
    last_error: Arc<Mutex<Option<String>>>,
    _join: JoinHandle<()>,
}

/// Plugin host. Cheap to clone (Arc internally).
#[derive(Clone)]
pub struct PluginManager {
    inner: Arc<Mutex<HashMap<String, Running>>>,
    router: Arc<tokio::sync::OnceCell<Router>>,
    event_bus: EventBus,
}

impl PluginManager {
    pub fn new(event_bus: EventBus) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            router: Arc::new(tokio::sync::OnceCell::new()),
            event_bus,
        }
    }

    /// Wire in the daemon's RPC router. Called once during daemon
    /// startup after the router is built. Plugin → daemon RPC
    /// requests dispatch through this.
    pub fn set_router(&self, router: Router) {
        let _ = self.router.set(router);
    }

    /// Spawn a plugin from a source. Performs the handshake, registers
    /// the plugin under the name it reports in its manifest, and
    /// starts the I/O task.
    pub async fn install(&self, source: PluginSource) -> Result<PluginInfo, PluginError> {
        let mut cmd = Command::new(&source.path);
        cmd.args(&source.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(cwd) = &source.cwd {
            cmd.current_dir(cwd);
        }
        let mut child = cmd.spawn().map_err(|e| {
            PluginError::HandshakeFailed(format!("failed to spawn {}: {e}", source.path.display()))
        })?;

        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");
        let pid = child.id();

        // Stage 1: handshake. Send plugin.init, read one line from
        // stdout, expect a JSON-RPC response with the manifest.
        let mut reader = BufReader::new(stdout).lines();
        let mut stdin = stdin;

        let init_req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "plugin.init",
            "params": {
                "shux_version": env!("CARGO_PKG_VERSION"),
                "protocol": PROTOCOL_VERSION,
            },
            "id": "init",
        });
        let init_line = format!("{init_req}\n");
        stdin.write_all(init_line.as_bytes()).await?;
        stdin.flush().await?;

        let manifest = match tokio::time::timeout(HANDSHAKE_TIMEOUT, reader.next_line()).await {
            Ok(Ok(Some(line))) => parse_manifest(&line)?,
            Ok(Ok(None)) => {
                let _ = child.kill().await;
                return Err(PluginError::HandshakeFailed("plugin closed stdout".into()));
            }
            Ok(Err(e)) => {
                let _ = child.kill().await;
                return Err(PluginError::HandshakeFailed(format!("read: {e}")));
            }
            Err(_) => {
                let _ = child.kill().await;
                return Err(PluginError::HandshakeFailed(
                    "manifest not received within 5s".into(),
                ));
            }
        };

        if manifest.name.is_empty() {
            let _ = child.kill().await;
            return Err(PluginError::HandshakeFailed(
                "plugin manifest missing 'name'".into(),
            ));
        }

        // Stage 2: dedup + spawn + register atomically. Held across
        // the spawn so two concurrent installs of plugins reporting
        // the same manifest name can't both pass the contains_key
        // check and overwrite each other's `Running` entry, which
        // would orphan one of the child processes. Spawn itself is
        // non-blocking, so the lock window stays in microseconds.
        let mut inner = self.inner.lock().await;
        if inner.contains_key(&manifest.name) {
            let _ = child.start_kill();
            return Err(PluginError::NameConflict(manifest.name.clone()));
        }

        let (inbox_tx, inbox_rx) = mpsc::channel::<String>(64);
        let (kill_tx, kill_rx) = oneshot::channel::<()>();
        let last_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        tokio::spawn(relay_stderr(manifest.name.clone(), stderr));

        let sub_filters = manifest.subscribes.clone();
        let sub = if sub_filters.is_empty() {
            None
        } else {
            Some(self.event_bus.subscribe_filtered(sub_filters))
        };

        let join = tokio::spawn(run_plugin_io(
            manifest.name.clone(),
            child,
            stdin,
            reader,
            inbox_rx,
            kill_rx,
            sub,
            self.router.clone(),
            last_error.clone(),
        ));

        let info = PluginInfo {
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            source: source.path.display().to_string(),
            pid,
            status: "running".into(),
            uptime_ms: 0,
            subscribes: manifest.subscribes.clone(),
            provides: manifest.provides.clone(),
            last_error: None,
        };

        let running = Running {
            manifest: manifest.clone(),
            source,
            inbox_tx,
            kill_tx: Some(kill_tx),
            started_at: Instant::now(),
            pid,
            last_error,
            _join: join,
        };

        inner.insert(manifest.name.clone(), running);
        drop(inner);
        info!(plugin = %manifest.name, "plugin installed");
        Ok(info)
    }

    /// Snapshot every running plugin.
    pub async fn list(&self) -> Vec<PluginInfo> {
        let inner = self.inner.lock().await;
        let mut out = Vec::with_capacity(inner.len());
        for (name, r) in inner.iter() {
            let last = r.last_error.lock().await.clone();
            out.push(PluginInfo {
                name: name.clone(),
                version: r.manifest.version.clone(),
                source: r.source.path.display().to_string(),
                pid: r.pid,
                status: if r.inbox_tx.is_closed() {
                    "stopped".into()
                } else {
                    "running".into()
                },
                uptime_ms: r.started_at.elapsed().as_millis() as u64,
                subscribes: r.manifest.subscribes.clone(),
                provides: r.manifest.provides.clone(),
                last_error: last,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Tear down a plugin. Sends `plugin.shutdown`, triggers the
    /// kill signal, and removes the entry. The I/O task drops the
    /// child (kill_on_drop) which sends SIGKILL on macOS/Linux.
    pub async fn kill(&self, name: &str) -> Result<(), PluginError> {
        let mut inner = self.inner.lock().await;
        let mut running = inner
            .remove(name)
            .ok_or_else(|| PluginError::NotFound(name.to_string()))?;

        let shutdown = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "plugin.shutdown",
            "params": {},
        });
        let _ = running.inbox_tx.send(format!("{shutdown}\n")).await;
        if let Some(tx) = running.kill_tx.take() {
            let _ = tx.send(());
        }
        Ok(())
    }
}

fn parse_manifest(line: &str) -> Result<PluginManifest, PluginError> {
    let v: Value = serde_json::from_str(line)
        .map_err(|e| PluginError::HandshakeFailed(format!("bad json: {e}")))?;
    if let Some(err) = v.get("error") {
        return Err(PluginError::HandshakeFailed(format!(
            "plugin returned error: {err}"
        )));
    }
    let result = v
        .get("result")
        .ok_or_else(|| PluginError::HandshakeFailed("missing 'result'".into()))?;
    serde_json::from_value(result.clone())
        .map_err(|e| PluginError::HandshakeFailed(format!("bad manifest: {e}")))
}

async fn relay_stderr(name: String, stderr: ChildStderr) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        debug!(plugin = %name, "plugin stderr: {}", line);
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_plugin_io(
    name: String,
    mut child: Child,
    mut stdin: ChildStdin,
    mut reader: tokio::io::Lines<BufReader<ChildStdout>>,
    mut inbox_rx: mpsc::Receiver<String>,
    mut kill_rx: oneshot::Receiver<()>,
    mut sub: Option<Subscription>,
    router: Arc<tokio::sync::OnceCell<Router>>,
    last_error: Arc<Mutex<Option<String>>>,
) {
    // Plugin→daemon RPC dispatches run in spawned tasks so a slow
    // handler can't stall this loop; responses come back via resp_tx
    // and get serialized onto stdin like any other outbound frame.
    let (resp_tx, mut resp_rx) = mpsc::channel::<String>(64);

    loop {
        tokio::select! {
            biased;

            _ = &mut kill_rx => {
                // Drain any frames already queued on inbox — notably the
                // `plugin.shutdown` notification that `PluginManager::kill`
                // pushes immediately before signaling us. The biased
                // `select!` would otherwise jump straight to the grace
                // wait without ever writing those bytes, force-killing
                // plugins that expected the documented 2s graceful window.
                while let Ok(line) = inbox_rx.try_recv() {
                    if let Err(e) = stdin.write_all(line.as_bytes()).await {
                        *last_error.lock().await = Some(format!("stdin write (drain): {e}"));
                        break;
                    }
                }
                let _ = stdin.flush().await;
                debug!(plugin = %name, "kill signal received; draining grace");
                let _ = tokio::time::timeout(SHUTDOWN_GRACE, child.wait()).await;
                let _ = child.start_kill();
                break;
            }

            line = inbox_rx.recv() => {
                let Some(line) = line else { break };
                if let Err(e) = stdin.write_all(line.as_bytes()).await {
                    *last_error.lock().await = Some(format!("stdin write: {e}"));
                    break;
                }
                let _ = stdin.flush().await;
            }

            line = resp_rx.recv() => {
                let Some(line) = line else { continue };
                if let Err(e) = stdin.write_all(line.as_bytes()).await {
                    *last_error.lock().await = Some(format!("stdin write (resp): {e}"));
                    break;
                }
                let _ = stdin.flush().await;
            }

            ev = next_event(&mut sub) => {
                let Some(ev) = ev else { continue };
                let frame = serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "event",
                    "params": ev,
                });
                if let Err(e) = stdin.write_all(format!("{frame}\n").as_bytes()).await {
                    *last_error.lock().await = Some(format!("event write: {e}"));
                    break;
                }
                let _ = stdin.flush().await;
            }

            res = reader.next_line() => {
                match res {
                    Ok(Some(line)) if line.is_empty() => continue,
                    Ok(Some(line)) => {
                        dispatch_plugin_frame(&name, line, &router, resp_tx.clone());
                    }
                    Ok(None) => {
                        debug!(plugin = %name, "plugin closed stdout; exiting io task");
                        break;
                    }
                    Err(e) => {
                        *last_error.lock().await = Some(format!("stdout read: {e}"));
                        warn!(plugin = %name, "stdout error: {e}");
                        break;
                    }
                }
            }
        }
    }

    info!(plugin = %name, "plugin io task exited");
}

/// Read one event from a (possibly absent) subscription. Returns
/// `None` if there's no subscription (suspended forever), the
/// receiver lagged, or the bus closed. Events flatten via
/// `Event::to_wire_json()` so plugins see the same shape as
/// `events.watch` / `events.history` consumers (top-level `type`,
/// `seq`, `timestamp`, plus payload under `data`).
async fn next_event(sub: &mut Option<Subscription>) -> Option<serde_json::Value> {
    let Some(s) = sub.as_mut() else {
        std::future::pending::<()>().await;
        return None;
    };
    match s.recv().await {
        Some(SubscriptionEvent::Event(e)) => Some(e.to_wire_json()),
        Some(SubscriptionEvent::Lagged(skipped)) => {
            warn!("plugin event subscription lagged: skipped {skipped}");
            None
        }
        None => None,
    }
}

fn dispatch_plugin_frame(
    plugin: &str,
    line: String,
    router: &Arc<tokio::sync::OnceCell<Router>>,
    resp_tx: mpsc::Sender<String>,
) {
    let parsed: Value = match serde_json::from_str(&line) {
        Ok(v) => v,
        Err(e) => {
            warn!(plugin, "bad json from plugin: {e}");
            return;
        }
    };

    let id = parsed.get("id").cloned();
    let method = parsed
        .get("method")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    let params = parsed.get("params").cloned();

    if method.is_empty() {
        // Response to a daemon→plugin RPC (none defined in v0).
        return;
    }

    if id.is_none() {
        // Notification from plugin (none defined in v0).
        return;
    }

    let Some(router) = router.get().cloned() else {
        let err = serde_json::json!({
            "jsonrpc": "2.0",
            "error": {"code": -32603, "message": "router not ready"},
            "id": id,
        });
        let _ = resp_tx.try_send(format!("{err}\n"));
        return;
    };

    let plugin = plugin.to_string();
    tokio::spawn(async move {
        let result = router.dispatch(&method, params).await;
        let frame = match result {
            Ok(value) => serde_json::json!({
                "jsonrpc": "2.0",
                "result": value,
                "id": id,
            }),
            Err(e) => serde_json::json!({
                "jsonrpc": "2.0",
                "error": {
                    "code": e.code,
                    "message": e.message,
                    "data": e.data,
                },
                "id": id,
            }),
        };
        if let Err(send_err) = resp_tx.send(format!("{frame}\n")).await {
            warn!(plugin, "couldn't deliver response to plugin: {send_err}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_parses() {
        let line = r#"{"jsonrpc":"2.0","id":"init","result":{"name":"x","version":"0.1.0","subscribes":["session.created"]}}"#;
        let m = parse_manifest(line).unwrap();
        assert_eq!(m.name, "x");
        assert_eq!(m.version, "0.1.0");
        assert_eq!(m.subscribes, vec!["session.created".to_string()]);
    }

    #[test]
    fn manifest_rejects_error_frame() {
        let line = r#"{"jsonrpc":"2.0","id":"init","error":{"code":-1,"message":"nope"}}"#;
        assert!(parse_manifest(line).is_err());
    }

    #[test]
    fn manifest_rejects_missing_result() {
        let line = r#"{"jsonrpc":"2.0","id":"init"}"#;
        assert!(parse_manifest(line).is_err());
    }
}
