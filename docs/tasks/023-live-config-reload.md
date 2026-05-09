# 023 — Live Config Reload

**Status:** Pending
**Depends On:** 022
**Parallelizable With:** 024

---

## Problem

The PRD requires that config changes trigger live updates within 500ms, with no daemon restart needed. Users edit `~/.config/shux/config.toml` in their editor, save, and shux immediately reflects the change -- new prefix key, different border style, mouse toggle, log level change, etc. This is a core quality-of-life feature that differentiates shux from tmux (which requires `tmux source-file` or server restart for most config changes).

The implementation must handle real-world file system behavior: editors that use temp-file-then-rename (atomic writes), rapid multi-save bursts (debouncing), partial writes (detection and rejection), concurrent edits from multiple sources, and the SIGHUP reload trigger.

## PRD Reference

- **SS 10.1** (Live reload within 500ms, merge semantics)
- **SS 10.2** (Config merge semantics -- per-key deep merge, debounce 100ms, atomic detection)
- **SS 4.5** (SIGHUP triggers config reload)
- **SS 6.1** (Live theme editing -- theme file changes trigger update within 500ms)

---

## Files to Create

- `crates/shux-core/src/config_reload.rs` -- File watcher, debounce, atomic detection, reload orchestration
- `crates/shux-core/tests/config_reload_test.rs` -- Tests for reload behavior

## Files to Modify

- `crates/shux-core/src/config.rs` -- Add reload method to `LoadedConfig`, integrate with watcher
- `crates/shux-core/src/lib.rs` -- Register config_reload module
- `crates/shux-core/Cargo.toml` -- Add `notify` crate dependency
- `crates/shux/src/main.rs` -- Start config watcher during daemon initialization, wire SIGHUP handler

---

## Execution Steps

### Step 1: Add the notify crate for file watching

The `notify` crate is the standard Rust file watching library. Use the recommended watcher (inotify on Linux, FSEvents on macOS, kqueue on BSD).

```toml
# In crates/shux-core/Cargo.toml
[dependencies]
notify = { version = "7", features = ["macos_kqueue"] }
# Note: macos_kqueue feature provides more reliable watching on macOS.
# The default FSEvents backend can miss rapid writes.
```

### Step 2: Implement the config watcher

The watcher monitors all config file paths (system, user, project) and their parent directories (to catch atomic renames). It debounces events and triggers a reload.

```rust
// crates/shux-core/src/config_reload.rs

use notify::{Watcher, RecommendedWatcher, RecursiveMode, Event, EventKind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Configuration reload events sent to the main event loop.
#[derive(Debug, Clone)]
pub enum ConfigReloadEvent {
    /// Config files changed and have been successfully reloaded.
    Reloaded {
        source: ReloadSource,
        changes: Vec<ConfigChange>,
    },
    /// Reload attempted but failed (previous config retained).
    Failed {
        source: ReloadSource,
        error: String,
    },
}

#[derive(Debug, Clone)]
pub enum ReloadSource {
    /// File system change detected.
    FileChange(PathBuf),
    /// SIGHUP signal received.
    Signal,
    /// Explicit API call (config.reload).
    Api,
}

/// Debounce window -- multiple file events within this window are coalesced.
const DEBOUNCE_MS: u64 = 100;

pub struct ConfigWatcher {
    /// The notify file watcher handle.
    _watcher: RecommendedWatcher,
    /// Channel to receive reload events.
    reload_rx: mpsc::Receiver<ConfigReloadEvent>,
    /// Paths being watched.
    watched_paths: Vec<PathBuf>,
}

impl ConfigWatcher {
    /// Start watching all config file paths.
    pub fn new(
        config_paths: Vec<PathBuf>,
        loaded_config: Arc<tokio::sync::RwLock<LoadedConfig>>,
    ) -> Result<Self, ConfigWatchError> {
        let (raw_tx, mut raw_rx) = mpsc::channel::<PathBuf>(64);
        let (reload_tx, reload_rx) = mpsc::channel::<ConfigReloadEvent>(16);

        // Create the file system watcher.
        let tx_clone = raw_tx.clone();
        let mut watcher = RecommendedWatcher::new(
            move |result: Result<Event, notify::Error>| {
                if let Ok(event) = result {
                    match event.kind {
                        EventKind::Create(_)
                        | EventKind::Modify(_)
                        | EventKind::Remove(_) => {
                            for path in &event.paths {
                                let _ = tx_clone.try_send(path.clone());
                            }
                        }
                        _ => {}
                    }
                }
            },
            notify::Config::default()
                .with_poll_interval(Duration::from_secs(2)),
        )
        .map_err(|e| ConfigWatchError::WatcherCreation(e.to_string()))?;

        // Watch each config file's parent directory (to catch rename-based writes).
        let mut watched = Vec::new();
        for path in &config_paths {
            if let Some(parent) = path.parent() {
                if parent.exists() {
                    if let Err(e) = watcher.watch(parent, RecursiveMode::NonRecursive) {
                        tracing::warn!("Failed to watch {}: {}", parent.display(), e);
                    } else {
                        watched.push(path.clone());
                    }
                }
            }
        }

        // Spawn the debounce task.
        let config_paths_clone = config_paths.clone();
        let loaded_config_clone = loaded_config.clone();
        tokio::spawn(async move {
            Self::debounce_loop(
                &mut raw_rx,
                &reload_tx,
                &config_paths_clone,
                &loaded_config_clone,
            )
            .await;
        });

        Ok(Self {
            _watcher: watcher,
            reload_rx,
            watched_paths: watched,
        })
    }

    /// Debounce loop: coalesces file events within DEBOUNCE_MS.
    async fn debounce_loop(
        raw_rx: &mut mpsc::Receiver<PathBuf>,
        reload_tx: &mpsc::Sender<ConfigReloadEvent>,
        config_paths: &[PathBuf],
        loaded_config: &Arc<tokio::sync::RwLock<LoadedConfig>>,
    ) {
        let debounce = Duration::from_millis(DEBOUNCE_MS);
        let mut last_event: Option<Instant> = None;
        let mut pending_paths: Vec<PathBuf> = Vec::new();

        loop {
            let timeout = if last_event.is_some() {
                debounce
            } else {
                Duration::from_secs(86400) // Effectively infinite when idle.
            };

            tokio::select! {
                path = raw_rx.recv() => {
                    match path {
                        Some(p) => {
                            // Only process if it's one of our config paths.
                            if config_paths.iter().any(|cp| Self::path_matches(&p, cp)) {
                                if !pending_paths.contains(&p) {
                                    pending_paths.push(p);
                                }
                                last_event = Some(Instant::now());
                            }
                        }
                        None => break, // Channel closed.
                    }
                }
                _ = tokio::time::sleep(timeout), if last_event.is_some() => {
                    if let Some(last) = last_event {
                        if last.elapsed() >= debounce {
                            // Debounce window expired -- trigger reload.
                            let trigger = pending_paths.first().cloned()
                                .unwrap_or_else(|| PathBuf::from("<unknown>"));
                            let event = Self::execute_reload(
                                loaded_config,
                                ReloadSource::FileChange(trigger),
                            ).await;
                            let _ = reload_tx.send(event).await;
                            pending_paths.clear();
                            last_event = None;
                        }
                    }
                }
            }
        }
    }

    /// Check if a filesystem event path matches a config path.
    /// Handles both exact match and parent-directory match (for rename detection).
    fn path_matches(event_path: &Path, config_path: &Path) -> bool {
        event_path == config_path
            || event_path
                .file_name()
                .and_then(|f| config_path.file_name().map(|cf| f == cf))
                .unwrap_or(false)
    }

    /// Execute the actual config reload.
    async fn execute_reload(
        loaded_config: &Arc<tokio::sync::RwLock<LoadedConfig>>,
        source: ReloadSource,
    ) -> ConfigReloadEvent {
        let mut config = loaded_config.write().await;
        let old_config = config.config.clone();

        // Attempt to reload from files.
        match config.reload_from_files() {
            Ok(()) => {
                let changes = diff_configs(&old_config, &config.config);
                if changes.is_empty() {
                    tracing::debug!("Config reload: no changes detected");
                } else {
                    tracing::info!(
                        "Config reloaded: {} change(s) from {:?}",
                        changes.len(),
                        source
                    );
                    for change in &changes {
                        tracing::debug!(
                            "  {} : {:?} -> {:?}",
                            change.key,
                            change.old_value,
                            change.new_value
                        );
                    }
                }
                ConfigReloadEvent::Reloaded {
                    source,
                    changes,
                }
            }
            Err(e) => {
                tracing::error!("Config reload failed (keeping previous config): {}", e);
                ConfigReloadEvent::Failed {
                    source,
                    error: e.to_string(),
                }
            }
        }
    }

    /// Get the receiver for reload events (consumed by the main event loop).
    pub fn subscribe(&mut self) -> &mut mpsc::Receiver<ConfigReloadEvent> {
        &mut self.reload_rx
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigWatchError {
    #[error("failed to create file watcher: {0}")]
    WatcherCreation(String),
}
```

### Step 3: Implement reload on LoadedConfig

Add a `reload_from_files` method that re-reads all config sources, re-merges, validates, and replaces the config atomically. If the new config is invalid, the previous config is retained.

```rust
// In crates/shux-core/src/config.rs (additions)

impl LoadedConfig {
    /// Reload config from all file sources.
    /// On success, replaces self.config. On failure, returns error and
    /// self.config remains unchanged.
    pub fn reload_from_files(&mut self) -> Result<(), ConfigError> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let sources = discover_config_sources(&cwd);
        let mut layers = Vec::new();

        // Layer 1: built-in defaults.
        let defaults = toml::Value::try_from(&ShuxConfig::default())
            .expect("default config must be serializable");
        layers.push(defaults);

        // Layer 2-4: file-based layers.
        let file_layers = [
            (&sources.system, "system"),
            (&sources.user, "user"),
            (&sources.project, "project"),
        ];

        let mut warnings = Vec::new();
        for (path_opt, label) in &file_layers {
            if let Some(ref path) = path_opt {
                match self.load_with_atomic_check(path) {
                    Ok(val) => layers.push(val),
                    Err(ConfigError::PartialWrite { .. }) => {
                        // Partial write detected -- skip this file, keep previous.
                        warnings.push(format!(
                            "{} config: partial write detected, skipping", label
                        ));
                        continue;
                    }
                    Err(e) => {
                        warnings.push(format!("{} config: {}", label, e));
                    }
                }
            }
        }

        // Merge file layers.
        let mut merged = merge_layers(&layers);

        // Layer 5: runtime overrides.
        deep_merge(&mut merged, &self.runtime_overrides);

        // Deserialize.
        let new_config: ShuxConfig = merged
            .try_into()
            .map_err(|e| ConfigError::ValidationError(format!(
                "deserialization failed: {e}"
            )))?;

        // Validate.
        if let Err(errors) = validate_config(&new_config) {
            return Err(ConfigError::ValidationErrors(errors));
        }

        // Success: replace config.
        self.config = new_config;
        self.sources = sources;
        self.warnings = warnings;

        Ok(())
    }

    /// Load a TOML file with atomic write detection.
    /// Rejects files that appear to be partial writes (empty, or containing
    /// null bytes).
    fn load_with_atomic_check(&self, path: &Path) -> Result<toml::Value, ConfigError> {
        let content = std::fs::read(path).map_err(|e| ConfigError::ReadError {
            path: path.to_path_buf(),
            source: e,
        })?;

        // Check for partial write indicators.
        if content.is_empty() {
            return Err(ConfigError::PartialWrite {
                path: path.to_path_buf(),
            });
        }
        if content.contains(&0u8) {
            return Err(ConfigError::PartialWrite {
                path: path.to_path_buf(),
            });
        }

        let text = String::from_utf8(content).map_err(|_| ConfigError::PartialWrite {
            path: path.to_path_buf(),
        })?;

        text.parse::<toml::Value>().map_err(|e| ConfigError::ParseError {
            path: path.to_path_buf(),
            message: e.to_string(),
        })
    }
}
```

Add the `PartialWrite` variant to `ConfigError`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    // ... existing variants ...

    #[error("partial write detected in {path} (file may be incomplete)")]
    PartialWrite { path: PathBuf },
}
```

### Step 4: Wire SIGHUP handler

SIGHUP triggers an immediate config reload (bypassing the debounce window). This is the traditional Unix mechanism for daemon config reload.

```rust
// In crates/shux/src/main.rs (daemon initialization)

use tokio::signal::unix::{signal, SignalKind};

/// Start the SIGHUP handler that triggers config reload.
pub fn start_sighup_handler(
    loaded_config: Arc<tokio::sync::RwLock<LoadedConfig>>,
    event_bus: EventBus,
) {
    tokio::spawn(async move {
        let mut sighup = signal(SignalKind::hangup())
            .expect("failed to register SIGHUP handler");

        loop {
            sighup.recv().await;
            tracing::info!("SIGHUP received: reloading configuration");

            let mut config = loaded_config.write().await;
            let old_config = config.config.clone();

            match config.reload_from_files() {
                Ok(()) => {
                    let changes = diff_configs(&old_config, &config.config);
                    tracing::info!(
                        "Config reloaded via SIGHUP: {} change(s)",
                        changes.len()
                    );
                    // Emit config.reloaded event.
                    event_bus.emit(Event::ConfigReloaded {
                        source: "sighup".to_string(),
                        changes: changes.clone(),
                    });
                }
                Err(e) => {
                    tracing::error!(
                        "Config reload via SIGHUP failed (keeping previous): {}",
                        e
                    );
                }
            }
        }
    });
}
```

### Step 5: Emit config.reloaded event

When config is reloaded (from any source), emit a typed event on the event bus. This allows the UI, plugins, and subscribers to react to config changes.

```rust
/// Event emitted when configuration is reloaded.
#[derive(Debug, Clone, Serialize)]
pub struct ConfigReloadedEvent {
    /// What triggered the reload.
    pub source: String,
    /// List of changed keys.
    pub changes: Vec<ConfigChangeEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfigChangeEntry {
    pub key: String,
    pub old_value: serde_json::Value,
    pub new_value: serde_json::Value,
}
```

### Step 6: Apply changed config values to running subsystems

When specific config keys change, the corresponding subsystems must be updated. This is done by the daemon's main loop upon receiving a `ConfigReloaded` event.

```rust
/// Apply config changes to running subsystems.
pub async fn apply_config_changes(
    changes: &[ConfigChange],
    ctx: &mut DaemonContext,
) {
    for change in changes {
        match change.key.as_str() {
            "daemon.log_level" => {
                if let Some(ref new_val) = change.new_value {
                    if let Some(level) = new_val.as_str() {
                        tracing::info!("Changing log level to: {}", level);
                        ctx.update_log_level(level);
                    }
                }
            }
            "ui.mouse" => {
                if let Some(ref new_val) = change.new_value {
                    if let Some(enabled) = new_val.as_bool() {
                        ctx.update_mouse_enabled(enabled);
                    }
                }
            }
            "ui.prefix" => {
                if let Some(ref new_val) = change.new_value {
                    if let Some(prefix_str) = new_val.as_str() {
                        ctx.update_prefix_key(prefix_str);
                    }
                }
            }
            "ui.pane_border_style" => {
                if let Some(ref new_val) = change.new_value {
                    if let Some(style) = new_val.as_str() {
                        ctx.update_border_style(style);
                    }
                }
            }
            "ui.status_bar" => {
                if let Some(ref new_val) = change.new_value {
                    if let Some(visible) = new_val.as_bool() {
                        ctx.update_status_bar_visible(visible);
                    }
                }
            }
            "ui.status_bar_position" => {
                if let Some(ref new_val) = change.new_value {
                    if let Some(pos) = new_val.as_str() {
                        ctx.update_status_bar_position(pos);
                    }
                }
            }
            "ui.show_pane_titles" => {
                if let Some(ref new_val) = change.new_value {
                    if let Some(show) = new_val.as_bool() {
                        ctx.update_show_pane_titles(show);
                    }
                }
            }
            "theme.name" => {
                // Theme change: delegate to theme engine (task 024).
                if let Some(ref new_val) = change.new_value {
                    if let Some(name) = new_val.as_str() {
                        ctx.switch_theme(name);
                    }
                }
            }
            "copy.osc52" | "copy.vi_keys" | "copy.mouse_select_copies" => {
                // Copy config changes: apply to copy mode state.
                ctx.update_copy_config(&ctx.config().copy);
            }
            key if key.starts_with("keybindings.") => {
                // Keybinding change: rebuild keybinding map (task 031).
                ctx.rebuild_keybindings();
            }
            _ => {
                tracing::debug!("Config change for {}: no live handler", change.key);
            }
        }
    }

    // Always trigger a redraw after config changes.
    ctx.request_redraw();
}
```

### Step 7: Integrate watcher with daemon startup

Start the config watcher and SIGHUP handler during daemon initialization.

```rust
// In daemon startup:

// Load initial config.
let loaded_config = Arc::new(tokio::sync::RwLock::new(LoadedConfig::load(&cwd)));

// Collect paths to watch.
let config = loaded_config.read().await;
let watch_paths = config.sources.all_file_paths();
drop(config);

// Start file watcher.
let mut config_watcher = ConfigWatcher::new(
    watch_paths,
    loaded_config.clone(),
)?;

// Start SIGHUP handler.
start_sighup_handler(loaded_config.clone(), event_bus.clone());

// In the main event loop, poll the config watcher:
loop {
    tokio::select! {
        // ... other event sources ...

        Some(reload_event) = config_watcher.subscribe().recv() => {
            match reload_event {
                ConfigReloadEvent::Reloaded { source, changes } => {
                    // Emit event on bus.
                    event_bus.emit(Event::ConfigReloaded {
                        source: format!("{:?}", source),
                        changes: changes.iter().map(|c| ConfigChangeEntry {
                            key: c.key.clone(),
                            old_value: c.old_value.clone().map(toml_to_json).unwrap_or(JsonValue::Null),
                            new_value: c.new_value.clone().map(toml_to_json).unwrap_or(JsonValue::Null),
                        }).collect(),
                    });
                    // Apply changes to subsystems.
                    apply_config_changes(&changes, &mut daemon_ctx).await;
                }
                ConfigReloadEvent::Failed { error, .. } => {
                    tracing::error!("Config reload failed: {}", error);
                }
            }
        }
    }
}
```

### Step 8: Handle edge cases

Several important edge cases in config reload:

```rust
/// Edge cases handled:
///
/// 1. **Editor temp-file-then-rename (atomic writes)**:
///    Editors like vim write to a temp file then rename it over the target.
///    We watch the parent directory, not just the file, so we catch the rename.
///    The debounce window coalesces the delete+create events into one reload.
///
/// 2. **Rapid saves (debounce)**:
///    Users who save frequently (or auto-save) trigger many events.
///    The 100ms debounce window coalesces them into a single reload.
///
/// 3. **Partial writes (detection)**:
///    If a file is empty or contains null bytes, it's likely a partial write.
///    We skip it and keep the previous config. The next modification event
///    will trigger another reload attempt.
///
/// 4. **File deletion**:
///    If a config file is deleted, the next reload will use defaults for
///    that layer. The deletion is treated as a config change.
///
/// 5. **New file creation**:
///    If a project config (.shux/config.toml) is created where none existed,
///    it will be picked up on the next reload.
///
/// 6. **Permission changes**:
///    If a config file becomes unreadable, the reload fails gracefully.
///    The previous config is retained and a warning is logged.
///
/// 7. **Concurrent SIGHUP and file change**:
///    Both trigger reload. The RwLock on LoadedConfig serializes them.
///    Only one reload runs at a time; the other waits.
///
/// 8. **Config watcher failure**:
///    If the notify watcher fails (e.g., inotify limit), fall back to
///    SIGHUP-only reload. Log a warning.
```

---

## Verification

### Functional

```bash
# Build
cargo build --workspace

# Start daemon
cargo run -p shux -- new -s test

# In another terminal, edit the user config:
echo '[ui]
pane_border_style = "double"' >> ~/.config/shux/config.toml

# Within 500ms, the TUI should show double borders.

# Change back:
echo '[ui]
pane_border_style = "rounded"' > ~/.config/shux/config.toml

# Borders should revert.

# Test SIGHUP:
# Find the daemon PID
kill -HUP $(pgrep -f 'shux.*daemon')
# Should log: "SIGHUP received: reloading configuration"

# Test invalid config:
echo 'invalid toml {{{{' > ~/.config/shux/config.toml
# Daemon should log error, keep previous config.
# Fix the config:
echo '[ui]
mouse = true' > ~/.config/shux/config.toml
# Should recover.

# Test rapid saves (debounce):
for i in $(seq 1 10); do
  echo "[ui]\nscrollback_lines = $((5000 + i))" > ~/.config/shux/config.toml
  sleep 0.01
done
# Should only trigger 1-2 reloads, not 10.
```

### Tests

```bash
# Unit tests
cargo nextest run -p shux-core --lib config_reload

# Integration tests
cargo nextest run -p shux-core --test config_reload_test

# Test scenarios:
# - File change triggers reload within 500ms
# - Debounce: 10 rapid changes produce 1-2 reloads
# - Atomic write (rename) detected correctly
# - Partial write (empty file) rejected, previous config retained
# - Invalid TOML rejected, previous config retained
# - SIGHUP triggers immediate reload (no debounce)
# - Config diff correctly identifies changes
# - config.reloaded event emitted with correct change list
# - Subsystem handlers called for known keys
# - Unknown key changes are logged but don't crash
# - Concurrent reload requests are serialized
# - Watcher handles file deletion gracefully
```

---

## Completion Criteria

- [ ] Config file changes trigger reload within 500ms (PRD requirement)
- [ ] Debounce: 100ms window coalesces multiple file events
- [ ] Atomic write detection: temp-file-then-rename handled correctly
- [ ] Partial write detection: empty/corrupt files rejected, previous config retained
- [ ] SIGHUP triggers immediate config reload
- [ ] Invalid config on reload: previous config retained, error logged
- [ ] `config.reloaded` event emitted with source and change list
- [ ] Config diff correctly identifies changed, added, and removed keys
- [ ] Known config key changes applied to running subsystems (log level, mouse, prefix, borders, etc.)
- [ ] Unknown key changes logged but do not crash
- [ ] Concurrent reloads serialized via RwLock
- [ ] Watcher handles file deletion and creation gracefully
- [ ] Unit tests for debounce, atomic detection, partial write detection
- [ ] Integration tests for end-to-end reload flow
- [ ] No panics or crashes from any file system edge case

---

## Commit Message

```
feat(core): implement live config reload with debounce and atomic detection

- File watcher on all config paths via notify crate (PRD §10.1)
- 100ms debounce window coalesces rapid file events
- Atomic write detection: handles editor temp-file-then-rename pattern
- Partial write rejection: keeps previous config if file is incomplete
- SIGHUP triggers immediate reload (PRD §4.5)
- config.reloaded event emitted with detailed change list
- Subsystem hot-patching: log level, mouse, prefix, borders, theme
- Reload within 500ms of file change (PRD requirement)
```

---

## Session Protocol

1. **Before starting:** Read task 022 (TOML config system) to understand `LoadedConfig`, `ConfigSources`, and `diff_configs`. Review the `notify` crate v7 API. Understand how editors like vim, nano, and VS Code write files (temp+rename vs. in-place).
2. **During:** Implement in order: notify setup (Steps 1-2), reload logic (Step 3), SIGHUP (Step 4), events (Step 5), subsystem application (Step 6), integration (Step 7), edge cases (Step 8). Run `cargo check` after each step. Manual test with real file edits after Steps 2-3.
3. **After:** Run full verification suite. Time the reload latency (must be < 500ms). Test with vim, nano, and VS Code saves. Update `docs/PROGRESS.md` (mark 023 done). Update `CLAUDE.md` Learnings with file watcher behavior observations per platform.
