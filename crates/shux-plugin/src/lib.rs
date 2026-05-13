//! shux plugin host — process plugins, JSON-RPC over stdin/stdout.
//!
//! Task 044a (phase 0): protocol + spawn + install + hot reload via
//! a filesystem watcher (debounced kill+respawn on every save). No
//! override-by-name, no event interception, no sandbox yet — those
//! land in subsequent phases. See `docs/tasks/044a-process-plugins-v0.md`
//! for the full plan.
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
use shux_core::event::EventData;
use shux_core::graph::GraphHandle;
use shux_core::model::{PaneId, PluginId, SessionId, WindowId};
use shux_rpc::router::Router;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

pub mod audit;
pub mod grants;
pub mod permissions;

use crate::audit::AuditEntry;
use crate::grants::Grants;
use crate::permissions::{CheckCtx, Decision, TargetOwners, Targets};

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
    /// `true` if the plugin's source file is being watched for
    /// changes; on every save the daemon kills and respawns the
    /// process so the edit lands in <500ms.
    #[serde(default)]
    pub watching: bool,
    /// Stable per-install UUID. Identifies the plugin across hot
    /// reload AND daemon restart for permission purposes (grants are
    /// keyed on UUID, not name). `None` only on legacy plugins
    /// installed before the permission model landed; new installs
    /// always populate this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<PluginId>,
}

/// How to spawn a plugin executable.
#[derive(Debug, Clone)]
pub struct PluginSource {
    pub path: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    /// Watch `path` for modifications and respawn the plugin on
    /// every save. Default `false` for backwards compatibility;
    /// `shux plugin install` sets it `true` unless `--no-watch`.
    pub watch: bool,
    /// Per-install override for the plugin's persisted-state root.
    /// When set, `plugin.state.*` calls from this plugin land under
    /// `<state_root>/<plugin_name>/state.json` instead of the
    /// daemon-wide default. The CLI resolves this from the calling
    /// CLIENT's cwd so a daemon shared across projects keeps each
    /// project's plugin state isolated. (codex P2 review on PR #32.)
    pub state_root: Option<PathBuf>,
}

impl PluginSource {
    pub fn from_path(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            args: Vec::new(),
            cwd: None,
            watch: false,
            state_root: None,
        }
    }

    pub fn with_watch(mut self, watch: bool) -> Self {
        self.watch = watch;
        self
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
    /// Cancellation flag for the file-watcher task (when `source.watch`
    /// is true). Set to `true` on kill/reload to stop the watcher
    /// before it triggers another reload on the now-dead plugin.
    watch_cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Stable install-time identity. Used as the permission key
    /// (grants + entity ownership). Survives hot reload because the
    /// reload path re-uses the existing `Running`'s id; does NOT
    /// survive uninstall + reinstall — the by-id directory is purged
    /// unless `--keep-grants` is passed.
    /// See `docs/designs/permissions/README.md` §9.2.
    plugin_id: PluginId,
}

/// Plugin host. Cheap to clone (Arc internally).
#[derive(Clone)]
pub struct PluginManager {
    inner: Arc<Mutex<HashMap<String, Running>>>,
    router: Arc<tokio::sync::OnceCell<Router>>,
    event_bus: EventBus,
    /// Root for per-plugin persisted state. Each plugin's state lives at
    /// `<state_root>/<plugin_name>/state.json` and survives hot reload.
    state_root: Arc<std::path::PathBuf>,
    /// Optional graph handle, used by the permission enforcer to look
    /// up entity ownership. `None` in test harnesses that don't need
    /// ownership checks (they only exercise grant-based decisions).
    graph: Arc<tokio::sync::OnceCell<GraphHandle>>,
}

impl PluginManager {
    /// Build a host with the default state root (`<cwd>/.shux/plugins`).
    pub fn new(event_bus: EventBus) -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        Self::with_state_root(event_bus, cwd.join(".shux").join("plugins"))
    }

    /// Build a host with an explicit state root. Used by tests so they
    /// can point each test at a fresh tempdir.
    pub fn with_state_root(event_bus: EventBus, state_root: std::path::PathBuf) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            router: Arc::new(tokio::sync::OnceCell::new()),
            event_bus,
            state_root: Arc::new(state_root),
            graph: Arc::new(tokio::sync::OnceCell::new()),
        }
    }

    /// Root for grant + audit files (`<state_root>/by-id/<uuid>/`).
    /// Co-located with state so a single by-id dir holds everything
    /// tied to one plugin install.
    pub fn permissions_root(&self, plugin_id: &PluginId) -> std::path::PathBuf {
        self.state_root.join("by-id").join(plugin_id.to_string())
    }

    /// Convenience: by-id name link target. We persist
    /// `<state_root>/by-name/<plugin_name>` as a tiny file containing
    /// the UUID string so users can map between the two without
    /// crawling the by-id tree. Symlinks would be cleaner but symlink
    /// rejection in the audit path makes them awkward; a plain file
    /// is enough.
    pub fn name_link_path(&self, plugin_name: &str) -> std::path::PathBuf {
        self.state_root.join("by-name").join(plugin_name)
    }

    /// Wire in the graph handle for ownership lookups. Called once
    /// during daemon startup, after the graph is built. Without this
    /// call, plugin RPC checks treat every entity as user-owned (so
    /// any ownership-gated method needs an explicit grant) — the
    /// safe default for test harnesses.
    pub async fn set_graph(&self, graph: GraphHandle) {
        let _ = self.graph.set(graph);
    }

    /// Wire in the daemon's RPC router. Called once during daemon
    /// startup after the router is built. Plugin → daemon RPC
    /// requests dispatch through this.
    pub fn set_router(&self, router: Router) {
        let _ = self.router.set(router);
    }

    /// Resolve a plugin's stable install-time UUID. Reads
    /// `<state_root>/by-name/<plugin_name>` if present; otherwise
    /// returns `None` (caller generates a new UUID + persists).
    fn resolve_plugin_id_for_name(&self, name: &str) -> Option<PluginId> {
        let link = self.name_link_path(name);
        let raw = std::fs::read_to_string(&link).ok()?;
        raw.trim().parse::<PluginId>().ok()
    }

    /// Persist `<state_root>/by-name/<name>` = `<uuid>` so the same
    /// install across daemon restart maps back to the same UUID.
    fn persist_name_link(&self, name: &str, id: &PluginId) -> std::io::Result<()> {
        let link = self.name_link_path(name);
        if let Some(parent) = link.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = link.with_extension("tmp");
        std::fs::write(&tmp, id.to_string())?;
        std::fs::rename(&tmp, &link)
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
        // Names must be filter-safe — they're concatenated verbatim
        // into the `plugin.<name>.<type>` namespace used by
        // `event.publish`. Codex bot review on PR #31: a manifest
        // name like `git-status.evil` would let it publish events
        // matching subscribers filtering for the legitimate
        // `plugin.git-status.` prefix.
        if manifest.name.contains('.') || manifest.name.contains(char::is_whitespace) {
            let _ = child.kill().await;
            return Err(PluginError::HandshakeFailed(format!(
                "plugin name {:?} must not contain '.' or whitespace \
                 (used verbatim in the plugin.<name>.<type> event namespace)",
                manifest.name
            )));
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

        // Stage 2a: resolve stable PluginId. Re-use the by-name link's
        // UUID if it exists (so reload + daemon restart preserve the
        // identity that grants are keyed on); otherwise generate +
        // persist. See `docs/designs/permissions/README.md` §9.2.
        let plugin_id = self
            .resolve_plugin_id_for_name(&manifest.name)
            .unwrap_or_default();
        if let Err(e) = self.persist_name_link(&manifest.name, &plugin_id) {
            warn!(
                plugin = %manifest.name,
                "could not persist by-name link: {e} — grants may not survive restart"
            );
        }

        // Stage 2b: enforce hot-reload manifest.subscribes diff (§9.4).
        // First install with no grants file: every initial subscribe
        // is implicitly granted (persisted into grants.subscribes).
        // Subsequent installs may only widen the set if the user has
        // already added the new filter via `shux plugin grant ... subscribe`.
        let perm_root = self.permissions_root(&plugin_id);
        let grants_path = perm_root.join("grants.toml");
        let existed = grants_path.exists();
        let mut grants = match Grants::load(&grants_path) {
            Ok(g) => g,
            Err(e) => {
                let _ = child.start_kill();
                return Err(PluginError::HandshakeFailed(format!(
                    "could not load grants at {}: {e}",
                    grants_path.display()
                )));
            }
        };
        if !existed {
            // Fresh install: snapshot the initial subscribes as the
            // baseline allow-set.
            for filter in &manifest.subscribes {
                grants.add_subscribe(filter);
            }
            if let Err(e) = grants.save(&grants_path) {
                let _ = child.start_kill();
                return Err(PluginError::HandshakeFailed(format!(
                    "could not initialise grants at {}: {e}",
                    grants_path.display()
                )));
            }
        } else {
            // Reload / re-install: net-new entries must already be
            // present in grants.subscribes.allowed.
            let unauthorised: Vec<String> = manifest
                .subscribes
                .iter()
                .filter(|f| !grants.subscribes_allows(f))
                .cloned()
                .collect();
            if !unauthorised.is_empty() {
                let _ = child.start_kill();
                return Err(PluginError::HandshakeFailed(format!(
                    "manifest.subscribes added unauthorised filters since last install: {unauthorised:?}. \
                     Run `shux plugin grant {} subscribe <filter>` for each, then retry.",
                    manifest.name
                )));
            }
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

        // Per-plugin state root resolution: use the install-time
        // override from `source.state_root` if the CLI gave us one
        // (resolved from the calling client's cwd). Falls back to the
        // daemon-wide default so existing tests + bare RPC callers
        // still work. (codex P2 review on PR #32.)
        let resolved_state_root: Arc<PathBuf> = match &source.state_root {
            Some(path) => Arc::new(path.clone()),
            None => self.state_root.clone(),
        };

        let join = tokio::spawn(run_plugin_io(
            manifest.name.clone(),
            plugin_id,
            child,
            stdin,
            reader,
            inbox_rx,
            kill_rx,
            sub,
            self.router.clone(),
            self.event_bus.clone(),
            resolved_state_root.clone(),
            Arc::new(perm_root.clone()),
            self.graph.clone(),
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
            watching: false,
            plugin_id: Some(plugin_id),
        };

        // Hot reload: if `source.watch` is true, spawn a filesystem
        // watcher that drops + reinstalls the plugin on every save.
        // Cancellation flag lives in the Running entry so kill() and
        // reload() can stop the watcher before triggering a respawn
        // on a dead plugin entry.
        let watch_cancel = if source.watch {
            let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
            spawn_reload_watcher(
                self.clone(),
                manifest.name.clone(),
                source.clone(),
                cancel.clone(),
            );
            Some(cancel)
        } else {
            None
        };
        let info = PluginInfo {
            watching: watch_cancel.is_some(),
            ..info
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
            watch_cancel,
            plugin_id,
        };

        inner.insert(manifest.name.clone(), running);
        drop(inner);
        info!(plugin = %manifest.name, "plugin installed");
        Ok(info)
    }

    /// Drop the currently-installed plugin named `name` and respawn
    /// it from the same source. The watch task calls this on every
    /// save; users can also call it directly via `shux plugin
    /// reload <name>` for a manual hot-reload.
    pub async fn reload(&self, name: &str) -> Result<PluginInfo, PluginError> {
        // Pull the source out under lock; the kill below also needs
        // the lock so we drop it first.
        let source = {
            let inner = self.inner.lock().await;
            inner
                .get(name)
                .ok_or_else(|| PluginError::NotFound(name.to_string()))?
                .source
                .clone()
        };
        self.kill(name).await?;
        // Brief delay so the child reaps cleanly before respawn.
        // Without this, `install` can race the old child's stdin
        // close, and the new child sometimes inherits a stale
        // file descriptor on macOS.
        tokio::time::sleep(Duration::from_millis(150)).await;
        match self.install(source.clone()).await {
            Ok(info) => Ok(info),
            Err(e) => {
                // Install failed (handshake error, syntax error in the
                // edited source, …) — but if the original was being
                // watched, restart a fresh watcher so the next save can
                // retry. Without this, `kill()`'s cancel-flag write
                // tears down the watcher, the registry entry is gone,
                // and the loop is dead even though the user is still
                // saving the file. (Codex bot review, May 2026.)
                if source.watch {
                    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
                    spawn_reload_watcher(self.clone(), name.to_string(), source.clone(), cancel);
                    info!(
                        plugin = %name,
                        "install failed during reload; watcher re-armed for next save"
                    );
                }
                Err(e)
            }
        }
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
                watching: r.watch_cancel.is_some(),
                plugin_id: Some(r.plugin_id),
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Grant a plugin authority to call `method`, optionally scoped
    /// to a specific entity ID (`target`). `subscribe == true` adds
    /// `method` to the manifest-subscribes allow-set instead (for
    /// post-handshake additions to `subscribes:`).
    pub async fn grant(
        &self,
        plugin_name: &str,
        method: &str,
        target: Option<&str>,
        subscribe: bool,
    ) -> Result<(), PluginError> {
        let plugin_id = self
            .plugin_id_for_name(plugin_name)
            .await
            .ok_or_else(|| PluginError::NotFound(plugin_name.to_string()))?;
        let path = self.permissions_root(&plugin_id).join("grants.toml");
        let mut g = Grants::load(&path).map_err(|e| {
            PluginError::HandshakeFailed(format!("load grants {}: {e}", path.display()))
        })?;
        if subscribe {
            g.add_subscribe(method);
        } else {
            g.add(method, target);
        }
        g.save(&path).map_err(|e| {
            PluginError::HandshakeFailed(format!("save grants {}: {e}", path.display()))
        })?;
        info!(plugin = %plugin_name, method, target, subscribe, "grant added");
        Ok(())
    }

    /// Drop a previously-issued grant. Inverse of [`Self::grant`].
    pub async fn revoke(
        &self,
        plugin_name: &str,
        method: &str,
        target: Option<&str>,
        subscribe: bool,
    ) -> Result<(), PluginError> {
        let plugin_id = self
            .plugin_id_for_name(plugin_name)
            .await
            .ok_or_else(|| PluginError::NotFound(plugin_name.to_string()))?;
        let path = self.permissions_root(&plugin_id).join("grants.toml");
        let mut g = Grants::load(&path).map_err(|e| {
            PluginError::HandshakeFailed(format!("load grants {}: {e}", path.display()))
        })?;
        if subscribe {
            g.subscribes.allowed.retain(|s| s != method);
        } else {
            g.remove(method, target);
        }
        g.save(&path).map_err(|e| {
            PluginError::HandshakeFailed(format!("save grants {}: {e}", path.display()))
        })?;
        info!(plugin = %plugin_name, method, target, subscribe, "grant revoked");
        Ok(())
    }

    /// Snapshot of the grants file for `plugin_name`. Returns an
    /// empty `Grants` for a plugin that exists but has never had
    /// any grant issued; errors if the plugin is unknown.
    pub async fn grants_for(&self, plugin_name: &str) -> Result<Grants, PluginError> {
        let plugin_id = self
            .plugin_id_for_name(plugin_name)
            .await
            .ok_or_else(|| PluginError::NotFound(plugin_name.to_string()))?;
        let path = self.permissions_root(&plugin_id).join("grants.toml");
        Grants::load(&path).map_err(|e| {
            PluginError::HandshakeFailed(format!("load grants {}: {e}", path.display()))
        })
    }

    /// Path to the audit log for `plugin_name`. Returns `Err(NotFound)`
    /// for unknown plugins. The file may not exist if no RPC frames
    /// have arrived yet — callers should treat that as "empty audit".
    pub async fn audit_path(&self, plugin_name: &str) -> Result<PathBuf, PluginError> {
        let plugin_id = self
            .plugin_id_for_name(plugin_name)
            .await
            .ok_or_else(|| PluginError::NotFound(plugin_name.to_string()))?;
        Ok(self.permissions_root(&plugin_id).join("audit.log"))
    }

    /// Resolve plugin name → install-time UUID. Checks the in-memory
    /// registry first (fast path for running plugins) then falls back
    /// to the persisted by-name file (so users can grant before /
    /// after the plugin is running).
    async fn plugin_id_for_name(&self, plugin_name: &str) -> Option<PluginId> {
        if let Some(r) = self.inner.lock().await.get(plugin_name) {
            return Some(r.plugin_id);
        }
        self.resolve_plugin_id_for_name(plugin_name)
    }

    /// Tear down a plugin. Sends `plugin.shutdown`, triggers the
    /// kill signal, and removes the entry. The I/O task drops the
    /// child (kill_on_drop) which sends SIGKILL on macOS/Linux.
    pub async fn kill(&self, name: &str) -> Result<(), PluginError> {
        let mut inner = self.inner.lock().await;
        let mut running = inner
            .remove(name)
            .ok_or_else(|| PluginError::NotFound(name.to_string()))?;

        // Stop any active file watcher BEFORE the kill so a save
        // racing with `shux plugin kill` doesn't respawn a plugin
        // that's already been removed from the registry.
        if let Some(cancel) = &running.watch_cancel {
            cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        }

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

/// Spawn a filesystem watcher that triggers `mgr.reload(name)` on
/// every save of `source.path`. Uses notify's recommended_watcher
/// (FSEvents on macOS, inotify on Linux) wrapped to forward events
/// onto a tokio channel so the reload runs in the right runtime.
/// Self-debounces at 250ms so rapid saves coalesce into one respawn.
fn spawn_reload_watcher(
    mgr: PluginManager,
    name: String,
    source: PluginSource,
    cancel: Arc<std::sync::atomic::AtomicBool>,
) {
    use notify::{EventKind, RecursiveMode, Watcher};
    use std::sync::atomic::Ordering;

    let path = source.path.clone();
    let (tx, mut rx) = mpsc::channel::<()>(16);

    let watcher_name = name.clone();
    let watcher_cancel = cancel.clone();
    tokio::task::spawn_blocking(move || {
        let name = watcher_name;
        let cancel = watcher_cancel;
        // Build the watcher inside the blocking thread so the notify
        // backend's lifetime is tied to the watcher loop; dropping it
        // tears down the OS-level handle. Watch the parent directory
        // rather than the file itself — editors that atomic-rename
        // (vim, neovim with `backupcopy=auto`) replace the inode
        // entirely, which a file-level watcher would miss.
        let parent = match path.parent() {
            Some(p) => p.to_path_buf(),
            None => {
                warn!(plugin = %name, "watcher: path has no parent dir; disabling watch");
                return;
            }
        };
        let watched_name = match path.file_name().map(|s| s.to_owned()) {
            Some(n) => n,
            None => {
                warn!(plugin = %name, "watcher: path has no filename; disabling watch");
                return;
            }
        };

        let watcher_tx = tx.clone();
        let mut watcher =
            match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                let Ok(event) = res else { return };
                // Only respawn on modify-ish events that touch our
                // file. Skip Access events (open/close/read) which
                // would otherwise fire on every `cat plugin.sh`.
                let interesting = matches!(
                    event.kind,
                    EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                );
                if !interesting {
                    return;
                }
                let hits_us = event
                    .paths
                    .iter()
                    .any(|p| p.file_name().map(|f| f == watched_name).unwrap_or(false));
                if !hits_us {
                    return;
                }
                // Non-blocking send — if the channel is full the
                // debouncer is already going to reload, so this
                // save folds into that pass.
                let _ = watcher_tx.try_send(());
            }) {
                Ok(w) => w,
                Err(e) => {
                    warn!(plugin = %name, error = %e, "watcher: failed to create");
                    return;
                }
            };
        if let Err(e) = watcher.watch(&parent, RecursiveMode::NonRecursive) {
            warn!(plugin = %name, error = %e, "watcher: failed to start");
            return;
        }
        // Park until cancel — the watcher object itself holds the OS
        // handle, dropping when this block exits.
        while !cancel.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(200));
        }
        drop(watcher);
        debug!(plugin = %name, "watcher: cancelled, exiting");
    });

    // Debouncer / reload trigger lives in the tokio runtime so the
    // reload itself can await the manager. If the plugin is currently
    // registered we call `reload(name)` (kill + respawn from the same
    // source). If it's NOT registered — which happens after a failed
    // reload in the "save broken file → save fixed file" loop — we
    // fall back to `install(source.clone())` so the next save can
    // recover. (Codex bot review, May 2026.)
    let reload_name = name.clone();
    let source_for_reinstall = source.clone();
    tokio::spawn(async move {
        while rx.recv().await.is_some() {
            // Coalesce a burst of saves into one reload by draining
            // anything that arrives within the next 250ms.
            tokio::time::sleep(Duration::from_millis(250)).await;
            while let Ok(()) = rx.try_recv() {}
            if cancel.load(Ordering::SeqCst) {
                break;
            }
            info!(plugin = %reload_name, "watcher: file changed, reloading");
            let registered = mgr.inner.lock().await.contains_key(&reload_name);
            let outcome = if registered {
                mgr.reload(&reload_name).await
            } else {
                mgr.install(source_for_reinstall.clone()).await
            };
            match outcome {
                Ok(_) => info!(plugin = %reload_name, "watcher: reload OK"),
                Err(e) => warn!(
                    plugin = %reload_name,
                    error = %e,
                    "watcher: reload failed — keeping watcher armed for next save"
                ),
            }
        }
        debug!(plugin = %reload_name, "watcher: debouncer exiting");
    });
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
    plugin_id: PluginId,
    mut child: Child,
    mut stdin: ChildStdin,
    mut reader: tokio::io::Lines<BufReader<ChildStdout>>,
    mut inbox_rx: mpsc::Receiver<String>,
    mut kill_rx: oneshot::Receiver<()>,
    mut sub: Option<Subscription>,
    router: Arc<tokio::sync::OnceCell<Router>>,
    event_bus: EventBus,
    state_root: Arc<std::path::PathBuf>,
    perm_root: Arc<std::path::PathBuf>,
    graph: Arc<tokio::sync::OnceCell<GraphHandle>>,
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
                        dispatch_plugin_frame(
                            &name,
                            &plugin_id,
                            line,
                            &router,
                            &event_bus,
                            &state_root,
                            &perm_root,
                            &graph,
                            resp_tx.clone(),
                        );
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

/// Validate + publish a plugin-emitted event. Returns the sequence
/// number assigned by the bus, or a JSON-RPC error tuple on bad
/// input. The `plugin_id` is supplied by the caller (the I/O loop
/// knows which child wrote the frame) — plugins cannot spoof it.
///
/// Param shape:
///
/// ```json
/// {"event_type": "branch_changed", "data": {"branch": "main"}}
/// ```
///
/// The published filterable type is `plugin.<plugin_id>.<event_type>`
/// (see `EventData::full_event_type`). Subscribers target a specific
/// plugin's events with `--filter "plugin.<plugin_id>."`.
fn publish_plugin_event(
    plugin_id: &str,
    params: Option<&Value>,
    event_bus: &EventBus,
) -> Result<u64, (i64, String)> {
    let p = params.ok_or((-32602, "missing params".to_string()))?;
    let event_type = p
        .get("event_type")
        .and_then(|v| v.as_str())
        .ok_or((-32602, "missing 'event_type' string".to_string()))?;
    if event_type.is_empty() {
        return Err((-32602, "'event_type' must be non-empty".to_string()));
    }
    // Block embedded dots so a plugin can't synthesise a fake
    // `plugin.<id>.<a>.<b>` event that fans out under a sibling prefix.
    if event_type.contains('.') {
        return Err((
            -32602,
            "'event_type' must not contain '.' (the daemon namespaces it under plugin.<id>.)"
                .to_string(),
        ));
    }
    let data = p.get("data").cloned().unwrap_or(Value::Null);
    let seq = event_bus.publish(EventData::PluginEvent {
        plugin_id: plugin_id.to_string(),
        event_type: event_type.to_string(),
        data,
    });
    Ok(seq)
}

/// Cap per-plugin persisted state at 256 KiB. Plugins that want
/// larger blobs should write to their own paths under
/// `<state_root>/<plugin_name>/` directly.
const PLUGIN_STATE_MAX_BYTES: usize = 256 * 1024;

/// Resolve the on-disk path for a plugin's state.json. Validates
/// that `plugin_id` is filter-safe (no dots / whitespace — the
/// install path already enforces this, defence in depth here).
fn plugin_state_path(
    root: &std::path::Path,
    plugin_id: &str,
) -> Result<std::path::PathBuf, (i64, String)> {
    if plugin_id.is_empty()
        || plugin_id.contains('.')
        || plugin_id.contains('/')
        || plugin_id.contains(char::is_whitespace)
    {
        return Err((-32603, "invalid plugin name for state path".to_string()));
    }
    Ok(root.join(plugin_id).join("state.json"))
}

fn read_plugin_state(root: &std::path::Path, plugin_id: &str) -> Result<Value, (i64, String)> {
    let path = plugin_state_path(root, plugin_id)?;
    match std::fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => Ok(v),
            Err(e) => Err((
                -32603,
                format!("state.json at {} is corrupt: {e}", path.display()),
            )),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Value::Null),
        Err(e) => Err((-32603, format!("read state.json: {e}"))),
    }
}

fn write_plugin_state(
    root: &std::path::Path,
    plugin_id: &str,
    value: &Value,
) -> Result<usize, (i64, String)> {
    let serialised =
        serde_json::to_vec_pretty(value).map_err(|e| (-32603, format!("serialise state: {e}")))?;
    if serialised.len() > PLUGIN_STATE_MAX_BYTES {
        return Err((
            -32602,
            format!(
                "state would be {} bytes — exceeds cap of {} bytes; persist large blobs to your own path",
                serialised.len(),
                PLUGIN_STATE_MAX_BYTES
            ),
        ));
    }
    let path = plugin_state_path(root, plugin_id)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| (-32603, format!("mkdir {}: {e}", parent.display())))?;
    }
    // Atomic write: temp file in the same dir, then rename. Same-fs
    // rename is atomic on POSIX so readers either see the old or the
    // new content, never partial.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &serialised)
        .map_err(|e| (-32603, format!("write {}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        (
            -32603,
            format!("rename {} -> {}: {e}", tmp.display(), path.display()),
        )
    })?;
    Ok(serialised.len())
}

fn delete_plugin_state(root: &std::path::Path, plugin_id: &str) -> Result<bool, (i64, String)> {
    let path = plugin_state_path(root, plugin_id)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err((-32603, format!("delete {}: {e}", path.display()))),
    }
}

#[allow(clippy::too_many_arguments)]
fn dispatch_plugin_frame(
    plugin: &str,
    plugin_id: &PluginId,
    line: String,
    router: &Arc<tokio::sync::OnceCell<Router>>,
    event_bus: &EventBus,
    state_root: &std::path::Path,
    perm_root: &std::path::Path,
    graph: &Arc<tokio::sync::OnceCell<GraphHandle>>,
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

    // §9.3: Audit EVERY parsed plugin RPC frame, including the
    // plugin-only intercepts (`event.publish`, `plugin.state.*`),
    // BEFORE the early-return that handles them. Otherwise those
    // calls are invisible to the audit log.
    let targets = permissions::extract_targets(params.as_ref());
    let audit_now = || -> AuditEntry<'_> {
        AuditEntry {
            ts: audit::iso_now(),
            plugin,
            method: method.as_str(),
            params_hash: audit::params_hash(params.as_ref()),
            target_pane: targets.pane_id.as_deref(),
            target_window: targets.window_id.as_deref(),
            target_session: targets.session_id.as_deref(),
            decision: "allow",
            reason: "plugin_self_namespace",
        }
    };

    // Plugin-only methods: intercept BEFORE the router dispatch so
    // the plugin's identity is captured from the caller context
    // (the `plugin: &str` here) and can't be spoofed via params.
    // These calls are scoped to the plugin's own namespace; we audit
    // them and skip the permission check.
    let plugin_only_result: Option<Result<Value, (i64, String)>> = match method.as_str() {
        "event.publish" => Some(
            publish_plugin_event(plugin, params.as_ref(), event_bus)
                .map(|seq| serde_json::json!({"seq": seq})),
        ),
        "plugin.state.get" => Some(
            read_plugin_state(state_root, plugin).map(|value| serde_json::json!({"value": value})),
        ),
        "plugin.state.set" => Some({
            let value = params
                .as_ref()
                .and_then(|p| p.get("value"))
                .cloned()
                .ok_or((-32602, "missing 'value' parameter".to_string()));
            value.and_then(|v| {
                write_plugin_state(state_root, plugin, &v)
                    .map(|n| serde_json::json!({"bytes_written": n}))
            })
        }),
        "plugin.state.delete" => Some(
            delete_plugin_state(state_root, plugin)
                .map(|deleted| serde_json::json!({"deleted": deleted})),
        ),
        _ => None,
    };

    let audit_path = perm_root.join("audit.log");

    if let Some(resp) = plugin_only_result {
        // Best-effort audit; failure here must not block the response.
        if let Err(e) = audit::append(&audit_path, &audit_now()) {
            warn!(plugin, "audit write failed: {e}");
        }
        let frame = match resp {
            Ok(result) => serde_json::json!({
                "jsonrpc": "2.0",
                "result": result,
                "id": id,
            }),
            Err((code, msg)) => serde_json::json!({
                "jsonrpc": "2.0",
                "error": {"code": code, "message": msg},
                "id": id,
            }),
        };
        let _ = resp_tx.try_send(format!("{frame}\n"));
        return;
    }

    // Router-bound RPC: check permissions before dispatch.
    let Some(router) = router.get().cloned() else {
        let err = serde_json::json!({
            "jsonrpc": "2.0",
            "error": {"code": -32603, "message": "router not ready"},
            "id": id,
        });
        let _ = resp_tx.try_send(format!("{err}\n"));
        return;
    };

    let grants_path = perm_root.join("grants.toml");
    let grants = Grants::load(&grants_path).unwrap_or_else(|e| {
        warn!(
            plugin,
            "could not load grants at {} ({}); defaulting to empty (deny-all)",
            grants_path.display(),
            e
        );
        Grants::default()
    });
    let owners = if let Some(g) = graph.get() {
        resolve_owners(g, &targets)
    } else {
        TargetOwners::default()
    };
    let decision = permissions::check(&CheckCtx {
        plugin_id,
        plugin_name: plugin,
        method: &method,
        policy: router.policy(&method),
        params: params.as_ref(),
        targets: &targets,
        owners: &owners,
        grants: &grants,
    });

    let entry = AuditEntry {
        ts: audit::iso_now(),
        plugin,
        method: method.as_str(),
        params_hash: audit::params_hash(params.as_ref()),
        target_pane: targets.pane_id.as_deref(),
        target_window: targets.window_id.as_deref(),
        target_session: targets.session_id.as_deref(),
        decision: decision.label(),
        reason: decision.reason_str(),
    };
    if let Err(e) = audit::append(&audit_path, &entry) {
        warn!(plugin, "audit write failed: {e}");
    }

    if let Decision::Deny { .. } = &decision {
        let err = serde_json::json!({
            "jsonrpc": "2.0",
            "error": {
                "code": -32004,
                "message": "permission denied",
                "data": {
                    "plugin": plugin,
                    "method": method,
                    "reason": decision.reason_str(),
                }
            },
            "id": id,
        });
        let _ = resp_tx.try_send(format!("{err}\n"));
        return;
    }

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

/// Look up `created_by_plugin` for each target named in `targets`
/// from the current graph snapshot.
fn resolve_owners(graph: &GraphHandle, targets: &Targets) -> TargetOwners {
    let snap = graph.snapshot();
    let pane_owner = targets
        .pane_id
        .as_ref()
        .and_then(|s| s.parse::<PaneId>().ok())
        .and_then(|id| snap.panes.get(&id).and_then(|p| p.created_by_plugin));
    let window_owner = targets
        .window_id
        .as_ref()
        .and_then(|s| s.parse::<WindowId>().ok())
        .and_then(|id| snap.windows.get(&id).and_then(|w| w.created_by_plugin));
    let session_owner = targets
        .session_id
        .as_ref()
        .and_then(|s| s.parse::<SessionId>().ok())
        .and_then(|id| snap.sessions.get(&id).and_then(|s| s.created_by_plugin));
    TargetOwners {
        pane_owner,
        window_owner,
        session_owner,
    }
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
