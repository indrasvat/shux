//! TOML-driven user configuration with hot reload.
//!
//! Loaded once at daemon start from `${XDG_CONFIG_HOME:-$HOME/.config}/shux/config.toml`.
//! When that file changes on disk, a tokio task watching via `notify`
//! parses the new contents and atomically swaps the live config behind
//! an `ArcSwap`. Readers (status-bar builder, attach handler) call
//! `ConfigHandle::current()` to get a snapshot — no locks, never stale
//! beyond one render tick.
//!
//! Schema:
//!
//! ```toml
//! [appearance]
//! border_style = "rounded"  # thin|thick|double|rounded|ascii|none
//!
//! [keys]
//! prefix = "ctrl-space"     # any "<mod>-<key>" combo crossterm understands
//!
//! [shell]
//! # By default: spawn `$SHELL -l -i`. Override with explicit argv.
//! command = ["zsh", "-l", "-i"]
//! # Extra env injected into every spawned pane.
//! env = { LC_ALL = "en_US.UTF-8" }
//!
//! [statusbar]
//! left  = " ◆ #S "          # tmux-like format strings (#S = session, etc.)
//! right = " %H:%M:%S "
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

/// Top-level config struct. Every section is optional; defaults match
/// the daemon's hardcoded behavior so an empty/missing file is always
/// valid.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub appearance: AppearanceConfig,
    #[serde(default)]
    pub keys: KeysConfig,
    #[serde(default)]
    pub shell: ShellConfig,
    #[serde(default)]
    pub statusbar: StatusBarConfig,
    #[serde(default)]
    pub theme: crate::theme::ThemeConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppearanceConfig {
    /// Pane border style. One of: thin, thick, double, rounded, ascii, none.
    #[serde(default = "default_border_style")]
    pub border_style: String,
    /// Whether the status bar uses Nerd Font icons (true) or a curated
    /// Unicode-only set (false). Default OFF — NF glyphs render as
    /// tofu in any terminal/font that doesn't have them, which is a
    /// trust-killer for a first-launch impression. The Unicode fallback
    /// (◆ ± ▶ @) is Catppuccin-friendly and looks clean everywhere.
    /// Flip to true in `~/.config/shux/config.toml` when you have a
    /// NF installed — `shux config init`'s generated template enables
    /// it by default because users opting into the config file are
    /// almost certainly running a modern dev terminal.
    #[serde(default = "default_nerd_fonts")]
    pub nerd_fonts: bool,
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            border_style: default_border_style(),
            nerd_fonts: default_nerd_fonts(),
        }
    }
}

fn default_border_style() -> String {
    "rounded".to_string()
}
fn default_nerd_fonts() -> bool {
    false
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeysConfig {
    /// Prefix key, e.g. "ctrl-space", "ctrl-b", "alt-w".
    #[serde(default = "default_prefix")]
    pub prefix: String,
}

impl Default for KeysConfig {
    fn default() -> Self {
        Self {
            prefix: default_prefix(),
        }
    }
}

fn default_prefix() -> String {
    "ctrl-space".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShellConfig {
    /// Override the default shell argv. When empty, the daemon uses
    /// `$SHELL -l -i`.
    #[serde(default)]
    pub command: Vec<String>,
    /// Extra env vars to inject into every spawned pane (in addition to
    /// the daemon's TERM_PROGRAM/SHUX/COLORTERM defaults).
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StatusBarConfig {
    /// Left zone format string. tmux-like placeholders (reserved for a
    /// future format-string interpreter; not yet consumed).
    #[serde(default)]
    pub left: Option<String>,
    #[serde(default)]
    pub center: Option<String>,
    #[serde(default)]
    pub right: Option<String>,
    /// Script-driven segments. Each entry runs `command` (with optional
    /// `env`) every `interval_ms` and uses the captured stdout as a
    /// status-bar segment. Use this with `starship prompt` for
    /// rich prompts, or with any shell snippet for one-shot info.
    #[serde(default)]
    pub segment: Vec<SegmentDef>,
}

/// One script-driven status-bar segment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentDef {
    /// Where to place the segment: "left", "center", or "right".
    #[serde(default = "default_zone")]
    pub zone: String,
    /// argv to spawn. e.g. `["starship", "prompt"]` or `["bash", "-c", "..."]`.
    pub command: Vec<String>,
    /// Extra env vars for the spawn. Useful for one-off overrides.
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    /// Inline starship config (TOML text). When set, the runner writes
    /// this to a tempfile at startup and exports
    /// `STARSHIP_CONFIG=<tempfile>` for the spawned command. Lets the
    /// status-bar starship be configured **inside the same shux config
    /// file** as everything else — no second `~/.config/shux/statusbar.toml`
    /// to maintain. The user's actual PS1 starship (`~/.config/starship.toml`)
    /// is unaffected because shux only sets this env for its own spawns.
    #[serde(default)]
    pub starship_config: Option<String>,
    /// Refresh interval (ms). Lower bound is 100ms; the runner clamps to
    /// avoid spawn storms. Default 2000.
    #[serde(default = "default_interval_ms")]
    pub interval_ms: u64,
    /// Optional fallback text if the command fails / is missing. Lets
    /// the bar stay pretty even if starship isn't installed. Defaults
    /// to empty (segment renders blank).
    #[serde(default)]
    pub fallback: Option<String>,
}

fn default_zone() -> String {
    "left".to_string()
}
fn default_interval_ms() -> u64 {
    2000
}

/// A live, hot-reloadable handle to the current config. Cheap to clone.
#[derive(Clone)]
pub struct ConfigHandle {
    inner: Arc<ArcSwap<Config>>,
    notify: Arc<Notify>,
}

impl ConfigHandle {
    /// Load config from `path`. If the file doesn't exist, returns
    /// `Config::default()`. If it exists but parses incorrectly, returns
    /// `Config::default()` and logs a warning.
    pub fn load_or_default(path: &Path) -> Self {
        let cfg = read_config(path);
        Self {
            inner: Arc::new(ArcSwap::from(Arc::new(cfg))),
            notify: Arc::new(Notify::new()),
        }
    }

    /// Current snapshot. Lock-free.
    pub fn current(&self) -> Arc<Config> {
        self.inner.load_full()
    }

    /// Replace the config in-place. Used by the hot-reload watcher.
    pub fn replace(&self, new: Config) {
        self.inner.store(Arc::new(new));
        self.notify.notify_waiters();
    }

    /// Notification fires every time the config is replaced. Consumers
    /// (e.g. the attach render loop) can `.notified().await` and force
    /// a redraw on changes so users see the new border style or status
    /// bar segments instantly.
    pub fn change_notify(&self) -> Arc<Notify> {
        self.notify.clone()
    }
}

/// Default config file path: `$XDG_CONFIG_HOME/shux/config.toml` or
/// `$HOME/.config/shux/config.toml`.
pub fn default_config_path() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("shux").join("config.toml");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home)
            .join(".config")
            .join("shux")
            .join("config.toml");
    }
    PathBuf::from("config.toml")
}

fn read_config(path: &Path) -> Config {
    match std::fs::read_to_string(path) {
        Ok(contents) => match toml::from_str::<Config>(&contents) {
            Ok(cfg) => {
                tracing::info!(path = %path.display(), "config: loaded");
                cfg
            }
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(),
                    "config: parse failed, using defaults");
                Config::default()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(path = %path.display(), "config: not present, using defaults");
            Config::default()
        }
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(),
                "config: read failed, using defaults");
            Config::default()
        }
    }
}

/// Spawn a hot-reload watcher. On any modify event for `path` (or its
/// parent dir), re-parse and store the new config behind the handle.
/// Runs until `cancel` fires.
pub async fn run_hot_reload(
    path: PathBuf,
    handle: ConfigHandle,
    cancel: tokio_util::sync::CancellationToken,
) {
    use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

    // Watch the parent directory because editors typically write to a
    // tempfile and atomic-rename it over the real file; watching the
    // file directly would miss the rename event.
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => {
            tracing::warn!("config hot-reload: no parent dir, skipping watcher");
            return;
        }
    };
    if !parent.exists() {
        if let Err(e) = std::fs::create_dir_all(&parent) {
            tracing::warn!(error = %e, "config hot-reload: failed to create parent");
            return;
        }
    }

    // Bridge from notify (sync callback) to a tokio mpsc.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(8);
    let watch_path = path.clone();
    let mut watcher = match RecommendedWatcher::new(
        move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res {
                let touches_target = event
                    .paths
                    .iter()
                    .any(|p| p.file_name() == watch_path.file_name());
                if touches_target
                    && matches!(
                        event.kind,
                        EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                    )
                {
                    let _ = tx.try_send(());
                }
            }
        },
        notify::Config::default(),
    ) {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!(error = %e, "config hot-reload: watcher init failed");
            return;
        }
    };
    if let Err(e) = watcher.watch(&parent, RecursiveMode::NonRecursive) {
        tracing::warn!(error = %e, "config hot-reload: watch attach failed");
        return;
    }
    tracing::info!(path = %path.display(), "config hot-reload: watching");

    // Debounce: editors often emit a flurry of events for a single save.
    // Coalesce to one reload per ~150ms quiet window.
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            ev = rx.recv() => {
                if ev.is_none() { break; }
                // Drain any extra events that arrived in the same burst.
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                while rx.try_recv().is_ok() {}
                let new_cfg = read_config(&path);
                handle.replace(new_cfg);
                tracing::info!("config: reloaded");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_default_config_is_valid() {
        let cfg = Config::default();
        assert_eq!(cfg.appearance.border_style, "rounded");
        assert_eq!(cfg.keys.prefix, "ctrl-space");
        assert!(cfg.shell.command.is_empty());
    }

    #[test]
    fn test_parse_full_config() {
        let toml = r#"
            [appearance]
            border_style = "double"

            [keys]
            prefix = "ctrl-b"

            [shell]
            command = ["zsh", "-l"]
            env = { LC_ALL = "en_US.UTF-8" }

            [statusbar]
            left = " ◆ #S "
            right = " %H:%M "
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.appearance.border_style, "double");
        assert_eq!(cfg.keys.prefix, "ctrl-b");
        assert_eq!(cfg.shell.command, vec!["zsh", "-l"]);
        assert_eq!(
            cfg.shell.env.get("LC_ALL"),
            Some(&"en_US.UTF-8".to_string())
        );
        assert_eq!(cfg.statusbar.left.as_deref(), Some(" ◆ #S "));
    }

    #[test]
    fn test_load_missing_file_uses_default() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("does-not-exist.toml");
        let h = ConfigHandle::load_or_default(&path);
        let cfg = h.current();
        assert_eq!(cfg.appearance.border_style, "rounded");
    }

    #[test]
    fn test_replace_updates_snapshot() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        let h = ConfigHandle::load_or_default(&path);

        let mut new_cfg = Config::default();
        new_cfg.appearance.border_style = "thick".into();
        h.replace(new_cfg);

        assert_eq!(h.current().appearance.border_style, "thick");
    }

    #[tokio::test]
    async fn test_hot_reload_picks_up_file_change() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[appearance]\nborder_style = \"thin\"\n").unwrap();

        let h = ConfigHandle::load_or_default(&path);
        assert_eq!(h.current().appearance.border_style, "thin");

        let cancel = tokio_util::sync::CancellationToken::new();
        let watcher_handle = h.clone();
        let watcher_path = path.clone();
        let cancel_for_watcher = cancel.clone();
        tokio::spawn(async move {
            run_hot_reload(watcher_path, watcher_handle, cancel_for_watcher).await;
        });

        // Give the watcher time to attach.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        std::fs::write(&path, "[appearance]\nborder_style = \"double\"\n").unwrap();

        // Wait up to 2s for the change to land.
        let mut got_double = false;
        for _ in 0..40 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            if h.current().appearance.border_style == "double" {
                got_double = true;
                break;
            }
        }
        cancel.cancel();
        assert!(got_double, "hot reload did not pick up the file change");
    }
}
