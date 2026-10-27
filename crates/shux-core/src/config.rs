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
    pub keybindings: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub program_status: ProgramStatusConfig,
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
    /// Unicode-only set (false). Default ON — shux bundles the full
    /// JetBrains Mono Nerd Font Mono Regular (2.4 MB) so the PNG
    /// rasterizer resolves every NF codepoint out of the box. In live
    /// attach mode the user's terminal font is what decides; if you
    /// see tofu (◻) in your terminal because your terminal font lacks
    /// NF coverage, flip this to false here for the ASCII fallback
    /// (◆ ± ▶ @).
    #[serde(default = "default_nerd_fonts")]
    pub nerd_fonts: bool,

    /// Optional path to a custom primary text font (`.ttf` or `.otf`)
    /// used by the PNG rasterizer (window.snapshot / pane.snapshot /
    /// session.snapshot). When unset, shux uses the bundled
    /// JetBrains Mono Nerd Font Mono Regular. When set, your font
    /// becomes primary and the bundled NF JBM stays in the fallback
    /// chain so any NF glyph your font lacks still resolves via the
    /// bundled font, unless `font_fallbacks` is set explicitly. This is
    /// the only appearance key that changes snapshot cell metrics.
    ///
    /// Doesn't affect live attach rendering (that's whatever font your
    /// terminal is configured to use).
    #[serde(default)]
    pub font: Option<std::path::PathBuf>,

    /// Optional ordered fallback font chain used only by the PNG
    /// rasterizer. Entries are file paths or builtin tokens such as
    /// `builtin:nerd-font`, `builtin:math`, `builtin:symbols`,
    /// `builtin:symbols-legacy`, and `builtin:emoji`. When omitted, shux
    /// uses its default builtin fallback chain. When present, the
    /// configured non-empty list is used exactly after the primary font.
    /// If `font` is unset, bundled JetBrains Mono remains the metrics
    /// anchor and this list only affects fallback glyph coverage.
    #[serde(default)]
    pub font_fallbacks: Option<Vec<String>>,
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            border_style: default_border_style(),
            nerd_fonts: default_nerd_fonts(),
            font: None,
            font_fallbacks: None,
        }
    }
}

fn default_border_style() -> String {
    "rounded".to_string()
}
fn default_nerd_fonts() -> bool {
    true
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

/// OSC 7501 program status: programs in a pane report idle / working / done /
/// blocked / error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgramStatusConfig {
    /// Off: no probe reply, no records, nothing published; the terminal
    /// behaves as if the protocol did not exist.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for ProgramStatusConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShellConfig {
    /// Override the default shell argv. When empty, the daemon uses
    /// `$SHELL -l -i`.
    ///
    /// This is the *default* — it governs panes that asked for no command of
    /// their own. An explicit argv (`shux new -- vim a.rs`) still runs that.
    /// The program (`command[0]`) is also the shell that interprets a
    /// string-form `command` RPC parameter / `--cmd` line, so a pane opened by
    /// hand and a `--cmd` line cannot end up speaking different shell dialects.
    #[serde(default)]
    pub command: Vec<String>,
    /// Extra env vars to inject into every spawned pane (in addition to
    /// the daemon's TERM_PROGRAM/SHUX/COLORTERM defaults).
    ///
    /// Layered *under* the caller's own environment plan: a pane spawned with
    /// an explicit value for the same variable keeps its value.
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
    /// this into the daemon's private runtime directory at startup (mode
    /// 0600, created with exclusive/no-follow semantics) and exports
    /// `STARSHIP_CONFIG=<that file>` for the spawned command. Lets the
    /// status-bar starship be configured **inside the same shux config
    /// file** as everything else — no second `~/.config/shux/statusbar.toml`
    /// to maintain. The user's actual PS1 starship (`~/.config/starship.toml`)
    /// is unaffected because shux only sets this env for its own spawns.
    /// The status-bar runner also defaults Starship spawns to raw ANSI
    /// output (`STARSHIP_SHELL=cmd`, `TERM=xterm-256color`) unless the
    /// segment's `env` overrides those keys.
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

/// A file that exists but cannot be read or parsed.
type LoadError = String;

/// One look at the config file.
enum ConfigFile {
    /// Missing or empty: the defaults, unless an editor is mid-save.
    Absent,
    Present(Result<Box<Config>, LoadError>),
}

fn read_config_file(path: &Path) -> ConfigFile {
    match std::fs::read_to_string(path) {
        Ok(contents) if contents.is_empty() => ConfigFile::Absent,
        Ok(contents) => ConfigFile::Present(
            toml::from_str::<Config>(&contents)
                .map(Box::new)
                .map_err(|e| e.to_string()),
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ConfigFile::Absent,
        Err(e) => ConfigFile::Present(Err(e.to_string())),
    }
}

/// A missing file is the defaults; anything unreadable is an error.
fn try_read_config(path: &Path) -> Result<Config, LoadError> {
    match read_config_file(path) {
        ConfigFile::Absent => {
            tracing::debug!(path = %path.display(), "config: not present, using defaults");
            Ok(Config::default())
        }
        ConfigFile::Present(read) => read.map(|c| *c),
    }
}

/// Startup read: with nothing loaded yet, a broken file means defaults.
fn read_config(path: &Path) -> Config {
    try_read_config(path).unwrap_or_else(|e| {
        tracing::warn!(error = %e, path = %path.display(), "config: load failed, using defaults");
        Config::default()
    })
}

/// How long a missing or empty config file may stay that way before it
/// counts as deleted or emptied. Editors that save by delete-then-write or
/// truncate-then-write leave a short gap.
const DELETE_GRACE: std::time::Duration = std::time::Duration::from_millis(150);

/// [`reload`], but a missing or empty file gets [`DELETE_GRACE`] to be
/// rewritten before it installs the defaults.
async fn reload_settled(path: &Path, handle: &ConfigHandle) {
    let mut file = read_config_file(path);
    if matches!(file, ConfigFile::Absent) {
        tokio::time::sleep(DELETE_GRACE).await;
        file = read_config_file(path);
    }
    apply_reload(path, file, handle);
}

/// One hot-reload step: re-read `path` and store the result behind `handle`.
/// A file that no longer parses keeps the running config, so a half-saved
/// edit cannot reset every other section to its default.
#[cfg(test)]
fn reload(path: &Path, handle: &ConfigHandle) {
    apply_reload(path, read_config_file(path), handle);
}

fn apply_reload(path: &Path, file: ConfigFile, handle: &ConfigHandle) {
    let read = match file {
        ConfigFile::Absent => Ok(Config::default()),
        ConfigFile::Present(read) => read.map(|c| *c),
    };
    match read {
        Ok(cfg) => {
            handle.replace(cfg);
            tracing::info!("config: reloaded");
        }
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "config: reload failed, keeping the running config");
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
    if !parent.exists()
        && let Err(e) = std::fs::create_dir_all(&parent)
    {
        tracing::warn!(error = %e, "config hot-reload: failed to create parent");
        return;
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
                reload_settled(&path, &handle).await;
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

    #[test]
    fn a_malformed_edit_during_hot_reload_keeps_the_running_config() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[keys]\nprefix = \"ctrl-b\"\n[appearance]\nborder_style = \"thin\"\n",
        )
        .unwrap();
        let h = ConfigHandle::load_or_default(&path);
        assert_eq!(h.current().keys.prefix, "ctrl-b");

        // A save mid-edit: one bad key must not reset every other section.
        std::fs::write(
            &path,
            "[keys]\nprefix = \"ctrl-b\"\n[appearance]\nborder_style = 7\n",
        )
        .unwrap();
        reload(&path, &h);
        assert_eq!(h.current().keys.prefix, "ctrl-b");
        assert_eq!(h.current().appearance.border_style, "thin");

        std::fs::write(&path, "[appearance]\nborder_style = \"double\"\n").unwrap();
        reload(&path, &h);
        assert_eq!(h.current().appearance.border_style, "double");
        assert_eq!(
            h.current().keys.prefix,
            "ctrl-space",
            "a valid file still replaces"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_file_recreated_inside_the_grace_window_never_reads_as_deleted() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[keys]\nprefix = \"ctrl-b\"\n").unwrap();
        let h = ConfigHandle::load_or_default(&path);

        // An editor that saves by delete-then-write.
        std::fs::remove_file(&path).unwrap();
        let (p, hh) = (path.clone(), h.clone());
        let task = tokio::spawn(async move { reload_settled(&p, &hh).await });
        tokio::task::yield_now().await;
        assert_eq!(h.current().keys.prefix, "ctrl-b", "defaults leaked in");
        std::fs::write(&path, "[keys]\nprefix = \"ctrl-a\"\n").unwrap();
        tokio::time::advance(DELETE_GRACE).await;
        task.await.unwrap();
        assert_eq!(
            h.current().keys.prefix,
            "ctrl-a",
            "the recreated file was not read"
        );

        // Still gone after the grace window: that is a real deletion.
        std::fs::remove_file(&path).unwrap();
        let (p, hh) = (path.clone(), h.clone());
        let task = tokio::spawn(async move { reload_settled(&p, &hh).await });
        tokio::task::yield_now().await;
        assert_eq!(
            h.current().keys.prefix,
            "ctrl-a",
            "no grace before the defaults"
        );
        tokio::time::advance(DELETE_GRACE).await;
        task.await.unwrap();
        assert_eq!(h.current().keys.prefix, "ctrl-space");
    }

    /// An editor that saves by truncate-then-write leaves an empty file for a
    /// moment. It parses as all defaults, so it gets the same grace.
    #[tokio::test(start_paused = true)]
    async fn a_file_truncated_inside_the_grace_window_never_reads_as_empty() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[keys]\nprefix = \"ctrl-b\"\n").unwrap();
        let h = ConfigHandle::load_or_default(&path);

        std::fs::write(&path, "").unwrap();
        let (p, hh) = (path.clone(), h.clone());
        let task = tokio::spawn(async move { reload_settled(&p, &hh).await });
        tokio::task::yield_now().await;
        assert_eq!(
            h.current().keys.prefix,
            "ctrl-b",
            "the truncated file installed defaults"
        );
        std::fs::write(&path, "[keys]\nprefix = \"ctrl-a\"\n").unwrap();
        tokio::time::advance(DELETE_GRACE).await;
        task.await.unwrap();
        assert_eq!(h.current().keys.prefix, "ctrl-a");

        // Still empty after the grace window: the user emptied it.
        std::fs::write(&path, "").unwrap();
        let (p, hh) = (path.clone(), h.clone());
        let task = tokio::spawn(async move { reload_settled(&p, &hh).await });
        tokio::time::advance(DELETE_GRACE).await;
        task.await.unwrap();
        assert_eq!(h.current().keys.prefix, "ctrl-space");
    }

    #[test]
    fn deleting_the_config_during_hot_reload_restores_defaults() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[keys]\nprefix = \"ctrl-b\"\n").unwrap();
        let h = ConfigHandle::load_or_default(&path);
        std::fs::remove_file(&path).unwrap();
        reload(&path, &h);
        assert_eq!(h.current().keys.prefix, "ctrl-space");
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
