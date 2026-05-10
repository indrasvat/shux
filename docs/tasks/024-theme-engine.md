# 024 — Theme Engine and Token System

**Status:** Partial (PR #5, 2026-05-09). `[theme]` config section overrides border colors (focused/unfocused) and status-bar fg/bg colors; live-reloads via the same hot-reload path as `[appearance]`. The full PRD §6.1 token cascade (per-pane theming, theme files in `~/.config/shux/themes/`, ANSI palette overrides, named theme references) is **not yet shipped** — see task 025 for per-pane theming and the M1 closeout for the full token engine.
**Depends On:** 022
**Parallelizable With:** 021, 023

---

## Problem

shux's per-pane theming is a key differentiator (PRD SS 2.4 point 4). Before individual panes can be themed (task 025), the underlying theme engine must exist: a token-based system with a stable set of semantic color tokens, theme loading from TOML files, a cascade resolution algorithm (built-in default -> user global -> session -> window -> pane -> runtime override), and built-in themes (default-dark, default-light) that make shux look great out of the box.

The theme engine must resolve at render time -- the compositor asks "what color is `border.focused` for this pane?" and the engine walks the cascade, returning the resolved value. This approach avoids pre-computing merged themes for every possible pane configuration and supports live theme editing (file changes trigger re-resolution within 500ms, leveraging the config reload infrastructure from task 023).

## PRD Reference

- **SS 6.1** (Theming: token-based themes, theme files, live theme editing)
- **SS 5.3** (Theme cascade: Built-in Default -> User Global -> Session -> Window -> Pane -> Runtime Override)
- **SS 10.2** (`[theme] name, paths` configuration)
- **SS 8.2** (theme.list, theme.get, theme.set API methods)

---

## Files to Create

- `crates/shux-core/src/theme.rs` -- Theme data types, token definitions, TOML parsing
- `crates/shux-core/src/theme_engine.rs` -- Cascade resolution, theme registry, render-time resolution
- `crates/shux-core/src/themes/default_dark.rs` -- Built-in default-dark theme (compiled into binary)
- `crates/shux-core/src/themes/default_light.rs` -- Built-in default-light theme
- `crates/shux-core/src/themes/mod.rs` -- Built-in theme module
- `crates/shux-core/tests/theme_test.rs` -- Theme engine tests

## Files to Modify

- `crates/shux-core/src/lib.rs` -- Register theme modules
- `crates/shux-core/Cargo.toml` -- Dependencies (if any beyond existing serde/toml)
- `crates/shux-core/src/config_reload.rs` -- Watch theme files for live editing

---

## Execution Steps

### Step 1: Define the theme token set

The token set is stable and semantic -- it describes what things mean, not what color they are. This allows themes to be swapped without changing any rendering code.

```rust
// crates/shux-core/src/theme.rs

use serde::{Deserialize, Serialize};

/// A color value. Supports hex (#RRGGBB), named colors, and ANSI indices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Color {
    /// Hex color: "#1a1b26"
    Hex(String),
    /// Named color: "red", "blue", etc.
    Named(String),
    /// ANSI 256-color index.
    Ansi256(u8),
}

impl Color {
    /// Convert to crossterm Color.
    pub fn to_crossterm(&self) -> crossterm::style::Color {
        match self {
            Color::Hex(hex) => {
                let hex = hex.trim_start_matches('#');
                let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
                let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
                let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
                crossterm::style::Color::Rgb { r, g, b }
            }
            Color::Named(name) => match name.to_lowercase().as_str() {
                "black" => crossterm::style::Color::Black,
                "red" => crossterm::style::Color::Red,
                "green" => crossterm::style::Color::Green,
                "yellow" => crossterm::style::Color::Yellow,
                "blue" => crossterm::style::Color::Blue,
                "magenta" => crossterm::style::Color::Magenta,
                "cyan" => crossterm::style::Color::Cyan,
                "white" => crossterm::style::Color::White,
                "dark_grey" | "darkgrey" => crossterm::style::Color::DarkGrey,
                _ => crossterm::style::Color::Reset,
            },
            Color::Ansi256(idx) => crossterm::style::Color::AnsiValue(*idx),
        }
    }
}

/// The complete set of theme tokens.
/// Each token represents a semantic role, not a specific visual element.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ThemeTokens {
    // === Background tokens ===
    /// Deep background (behind everything, terminal background).
    pub bg_deep: Color,
    /// Surface background (pane content area).
    pub bg_surface: Color,
    /// Subtle background (slightly elevated surfaces, hover states).
    pub bg_subtle: Color,

    // === Foreground tokens ===
    /// Primary text color.
    pub fg_primary: Color,
    /// Secondary text (less important, comments, hints).
    pub fg_secondary: Color,
    /// Muted text (disabled, decorative).
    pub fg_muted: Color,

    // === Accent tokens ===
    /// Primary accent (interactive elements, focus indicators).
    pub accent_primary: Color,
    /// Secondary accent (complementary highlight).
    pub accent_secondary: Color,

    // === Border tokens ===
    /// Focused pane border.
    pub border_focused: Color,
    /// Unfocused pane border.
    pub border_unfocused: Color,

    // === Status bar tokens ===
    /// Status bar background.
    pub status_bar_bg: Color,
    /// Status bar foreground (text).
    pub status_bar_fg: Color,
    /// Status bar accent (active tab, prefix indicator).
    pub status_bar_accent: Color,

    // === Selection tokens (copy mode) ===
    /// Selection background.
    pub selection_bg: Color,
    /// Selection foreground.
    pub selection_fg: Color,

    // === Semantic tokens ===
    /// Error indicator.
    pub error: Color,
    /// Warning indicator.
    pub warn: Color,
    /// Info indicator.
    pub info: Color,
    /// Success indicator.
    pub success: Color,

    // === ANSI palette overrides (16 colors) ===
    /// Override the standard 16 ANSI colors.
    /// If None, the terminal's default colors are used.
    pub ansi: Option<AnsiPalette>,
}

/// The 16 ANSI color overrides.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AnsiPalette {
    pub black: Color,
    pub red: Color,
    pub green: Color,
    pub yellow: Color,
    pub blue: Color,
    pub magenta: Color,
    pub cyan: Color,
    pub white: Color,
    pub bright_black: Color,
    pub bright_red: Color,
    pub bright_green: Color,
    pub bright_yellow: Color,
    pub bright_blue: Color,
    pub bright_magenta: Color,
    pub bright_cyan: Color,
    pub bright_white: Color,
}
```

### Step 2: Define the Theme struct

A theme is a named collection of token values, loaded from TOML files or compiled into the binary.

```rust
/// A named theme.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Theme {
    /// Theme metadata.
    pub meta: ThemeMeta,
    /// Token values (partial -- None values inherit from parent in cascade).
    pub tokens: ThemeTokens,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeMeta {
    /// Theme name (unique identifier).
    pub name: String,
    /// Human-readable display name.
    pub display_name: Option<String>,
    /// Theme author.
    pub author: Option<String>,
    /// Theme description.
    pub description: Option<String>,
    /// Whether this is a dark or light theme.
    pub variant: ThemeVariant,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ThemeVariant {
    Dark,
    Light,
}

/// A partial theme override -- only the tokens that are specified.
/// Used for session/window/pane-level overrides where you only want
/// to change a few tokens.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ThemeOverride {
    pub bg_deep: Option<Color>,
    pub bg_surface: Option<Color>,
    pub bg_subtle: Option<Color>,
    pub fg_primary: Option<Color>,
    pub fg_secondary: Option<Color>,
    pub fg_muted: Option<Color>,
    pub accent_primary: Option<Color>,
    pub accent_secondary: Option<Color>,
    pub border_focused: Option<Color>,
    pub border_unfocused: Option<Color>,
    pub status_bar_bg: Option<Color>,
    pub status_bar_fg: Option<Color>,
    pub status_bar_accent: Option<Color>,
    pub selection_bg: Option<Color>,
    pub selection_fg: Option<Color>,
    pub error: Option<Color>,
    pub warn: Option<Color>,
    pub info: Option<Color>,
    pub success: Option<Color>,
}

/// Reference to a theme (used in session/window/pane data model).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeRef {
    /// Named theme to use as base.
    pub theme_name: Option<String>,
    /// Per-token overrides applied on top of the named theme.
    pub overrides: ThemeOverride,
}
```

### Step 3: Implement built-in themes

Compile the default themes into the binary so shux looks great without any config files.

```rust
// crates/shux-core/src/themes/default_dark.rs

use crate::theme::*;

pub fn default_dark() -> Theme {
    Theme {
        meta: ThemeMeta {
            name: "default-dark".to_string(),
            display_name: Some("Default Dark".to_string()),
            author: Some("shux".to_string()),
            description: Some("The default dark theme for shux".to_string()),
            variant: ThemeVariant::Dark,
        },
        tokens: ThemeTokens {
            // Background: deep navy/charcoal palette.
            bg_deep: Color::Hex("#1a1b26".to_string()),
            bg_surface: Color::Hex("#1e1f2b".to_string()),
            bg_subtle: Color::Hex("#292a37".to_string()),

            // Foreground: bright but not harsh.
            fg_primary: Color::Hex("#c0caf5".to_string()),
            fg_secondary: Color::Hex("#a9b1d6".to_string()),
            fg_muted: Color::Hex("#565f89".to_string()),

            // Accent: vibrant blue/purple.
            accent_primary: Color::Hex("#7aa2f7".to_string()),
            accent_secondary: Color::Hex("#bb9af7".to_string()),

            // Borders.
            border_focused: Color::Hex("#7aa2f7".to_string()),
            border_unfocused: Color::Hex("#3b3d57".to_string()),

            // Status bar.
            status_bar_bg: Color::Hex("#16161e".to_string()),
            status_bar_fg: Color::Hex("#a9b1d6".to_string()),
            status_bar_accent: Color::Hex("#7aa2f7".to_string()),

            // Selection.
            selection_bg: Color::Hex("#33467c".to_string()),
            selection_fg: Color::Hex("#c0caf5".to_string()),

            // Semantic.
            error: Color::Hex("#f7768e".to_string()),
            warn: Color::Hex("#e0af68".to_string()),
            info: Color::Hex("#7dcfff".to_string()),
            success: Color::Hex("#9ece6a".to_string()),

            // ANSI palette (Tokyo Night inspired).
            ansi: Some(AnsiPalette {
                black: Color::Hex("#15161e".to_string()),
                red: Color::Hex("#f7768e".to_string()),
                green: Color::Hex("#9ece6a".to_string()),
                yellow: Color::Hex("#e0af68".to_string()),
                blue: Color::Hex("#7aa2f7".to_string()),
                magenta: Color::Hex("#bb9af7".to_string()),
                cyan: Color::Hex("#7dcfff".to_string()),
                white: Color::Hex("#a9b1d6".to_string()),
                bright_black: Color::Hex("#414868".to_string()),
                bright_red: Color::Hex("#f7768e".to_string()),
                bright_green: Color::Hex("#9ece6a".to_string()),
                bright_yellow: Color::Hex("#e0af68".to_string()),
                bright_blue: Color::Hex("#7aa2f7".to_string()),
                bright_magenta: Color::Hex("#bb9af7".to_string()),
                bright_cyan: Color::Hex("#7dcfff".to_string()),
                bright_white: Color::Hex("#c0caf5".to_string()),
            }),
        },
    }
}
```

```rust
// crates/shux-core/src/themes/default_light.rs

use crate::theme::*;

pub fn default_light() -> Theme {
    Theme {
        meta: ThemeMeta {
            name: "default-light".to_string(),
            display_name: Some("Default Light".to_string()),
            author: Some("shux".to_string()),
            description: Some("The default light theme for shux".to_string()),
            variant: ThemeVariant::Light,
        },
        tokens: ThemeTokens {
            bg_deep: Color::Hex("#d5d6db".to_string()),
            bg_surface: Color::Hex("#e1e2e7".to_string()),
            bg_subtle: Color::Hex("#d0d5e3".to_string()),

            fg_primary: Color::Hex("#343b58".to_string()),
            fg_secondary: Color::Hex("#4e5579".to_string()),
            fg_muted: Color::Hex("#9699a3".to_string()),

            accent_primary: Color::Hex("#34548a".to_string()),
            accent_secondary: Color::Hex("#5a4a78".to_string()),

            border_focused: Color::Hex("#34548a".to_string()),
            border_unfocused: Color::Hex("#b4b5bd".to_string()),

            status_bar_bg: Color::Hex("#c4c5cb".to_string()),
            status_bar_fg: Color::Hex("#343b58".to_string()),
            status_bar_accent: Color::Hex("#34548a".to_string()),

            selection_bg: Color::Hex("#99a7df".to_string()),
            selection_fg: Color::Hex("#343b58".to_string()),

            error: Color::Hex("#8c4351".to_string()),
            warn: Color::Hex("#8f5e15".to_string()),
            info: Color::Hex("#0f4b6e".to_string()),
            success: Color::Hex("#33635c".to_string()),

            ansi: Some(AnsiPalette {
                black: Color::Hex("#0f0f14".to_string()),
                red: Color::Hex("#8c4351".to_string()),
                green: Color::Hex("#33635c".to_string()),
                yellow: Color::Hex("#8f5e15".to_string()),
                blue: Color::Hex("#34548a".to_string()),
                magenta: Color::Hex("#5a4a78".to_string()),
                cyan: Color::Hex("#0f4b6e".to_string()),
                white: Color::Hex("#343b58".to_string()),
                bright_black: Color::Hex("#9699a3".to_string()),
                bright_red: Color::Hex("#8c4351".to_string()),
                bright_green: Color::Hex("#33635c".to_string()),
                bright_yellow: Color::Hex("#8f5e15".to_string()),
                bright_blue: Color::Hex("#34548a".to_string()),
                bright_magenta: Color::Hex("#5a4a78".to_string()),
                bright_cyan: Color::Hex("#0f4b6e".to_string()),
                bright_white: Color::Hex("#343b58".to_string()),
            }),
        },
    }
}
```

### Step 4: Implement theme file loading

Load themes from TOML files on disk. The theme file format matches the `Theme` struct.

```rust
// crates/shux-core/src/theme.rs (additional functions)

use std::path::{Path, PathBuf};

/// Load a theme from a TOML file.
pub fn load_theme_file(path: &Path) -> Result<Theme, ThemeError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| ThemeError::ReadError(path.to_path_buf(), e.to_string()))?;

    let theme: Theme = toml::from_str(&content)
        .map_err(|e| ThemeError::ParseError(path.to_path_buf(), e.to_string()))?;

    Ok(theme)
}

/// Discover all theme files in the given search paths.
pub fn discover_themes(search_paths: &[String]) -> Vec<PathBuf> {
    let mut theme_files = Vec::new();

    for path_str in search_paths {
        let expanded = crate::config_discovery::expand_path(path_str);
        if expanded.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&expanded) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("toml") {
                        theme_files.push(path);
                    }
                }
            }
        }
    }

    theme_files
}

/// Theme TOML file format example (for documentation):
///
/// ```toml
/// [meta]
/// name = "catppuccin-mocha"
/// display_name = "Catppuccin Mocha"
/// author = "Catppuccin contributors"
/// description = "Soothing pastel theme"
/// variant = "dark"
///
/// [tokens]
/// bg_deep = "#1e1e2e"
/// bg_surface = "#181825"
/// bg_subtle = "#313244"
/// fg_primary = "#cdd6f4"
/// fg_secondary = "#bac2de"
/// fg_muted = "#6c7086"
/// accent_primary = "#89b4fa"
/// accent_secondary = "#cba6f7"
/// border_focused = "#89b4fa"
/// border_unfocused = "#45475a"
/// # ... etc.
/// ```

#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    #[error("failed to read theme file {0}: {1}")]
    ReadError(PathBuf, String),
    #[error("failed to parse theme file {0}: {1}")]
    ParseError(PathBuf, String),
    #[error("theme not found: {0}")]
    NotFound(String),
}
```

### Step 5: Implement the ThemeEngine

The ThemeEngine is the core component that resolves themes at render time. It maintains a registry of loaded themes and resolves the cascade for any entity (session, window, pane).

```rust
// crates/shux-core/src/theme_engine.rs

use std::collections::HashMap;
use uuid::Uuid;
use crate::theme::*;

/// The theme engine manages theme loading, registration, and cascade resolution.
pub struct ThemeEngine {
    /// Registry of all known themes (name -> Theme).
    themes: HashMap<String, Theme>,
    /// The global (user-level) theme name from config.
    global_theme: String,
    /// Session-level theme overrides.
    session_themes: HashMap<Uuid, ThemeRef>,
    /// Window-level theme overrides.
    window_themes: HashMap<Uuid, ThemeRef>,
    /// Pane-level theme overrides.
    pane_themes: HashMap<Uuid, ThemeRef>,
}

impl ThemeEngine {
    /// Create a new ThemeEngine with built-in themes.
    pub fn new(global_theme_name: &str) -> Self {
        let mut themes = HashMap::new();

        // Register built-in themes.
        let dark = crate::themes::default_dark::default_dark();
        let light = crate::themes::default_light::default_light();
        themes.insert(dark.meta.name.clone(), dark);
        themes.insert(light.meta.name.clone(), light);

        Self {
            themes,
            global_theme: global_theme_name.to_string(),
            session_themes: HashMap::new(),
            window_themes: HashMap::new(),
            pane_themes: HashMap::new(),
        }
    }

    /// Load and register themes from disk.
    pub fn load_themes_from_paths(&mut self, paths: &[String]) {
        let theme_files = discover_themes(paths);
        for path in theme_files {
            match load_theme_file(&path) {
                Ok(theme) => {
                    tracing::info!("Loaded theme: {} from {}", theme.meta.name, path.display());
                    self.themes.insert(theme.meta.name.clone(), theme);
                }
                Err(e) => {
                    tracing::warn!("Failed to load theme from {}: {}", path.display(), e);
                }
            }
        }
    }

    /// Register a theme programmatically.
    pub fn register_theme(&mut self, theme: Theme) {
        self.themes.insert(theme.meta.name.clone(), theme);
    }

    /// Set the global theme (from config).
    pub fn set_global_theme(&mut self, name: &str) {
        self.global_theme = name.to_string();
    }

    /// Set a session-level theme.
    pub fn set_session_theme(&mut self, session_id: Uuid, theme_ref: ThemeRef) {
        self.session_themes.insert(session_id, theme_ref);
    }

    /// Set a window-level theme.
    pub fn set_window_theme(&mut self, window_id: Uuid, theme_ref: ThemeRef) {
        self.window_themes.insert(window_id, theme_ref);
    }

    /// Set a pane-level theme.
    pub fn set_pane_theme(&mut self, pane_id: Uuid, theme_ref: ThemeRef) {
        self.pane_themes.insert(pane_id, theme_ref);
    }

    /// Clear a pane-level theme (revert to window/session/global).
    pub fn clear_pane_theme(&mut self, pane_id: &Uuid) {
        self.pane_themes.remove(pane_id);
    }

    /// Resolve the complete theme for a specific pane.
    /// Walks the cascade: built-in -> global -> session -> window -> pane.
    pub fn resolve_for_pane(
        &self,
        pane_id: Uuid,
        window_id: Uuid,
        session_id: Uuid,
    ) -> ResolvedTheme {
        // Step 1: Start with the global theme tokens.
        let global_theme = self.themes.get(&self.global_theme)
            .or_else(|| self.themes.get("default-dark"))
            .expect("at least one built-in theme must exist");
        let mut tokens = global_theme.tokens.clone();

        // Step 2: Apply session-level overrides.
        if let Some(session_ref) = self.session_themes.get(&session_id) {
            self.apply_theme_ref(&mut tokens, session_ref);
        }

        // Step 3: Apply window-level overrides.
        if let Some(window_ref) = self.window_themes.get(&window_id) {
            self.apply_theme_ref(&mut tokens, window_ref);
        }

        // Step 4: Apply pane-level overrides.
        if let Some(pane_ref) = self.pane_themes.get(&pane_id) {
            self.apply_theme_ref(&mut tokens, pane_ref);
        }

        ResolvedTheme { tokens }
    }

    /// Apply a ThemeRef's overrides to a set of tokens.
    fn apply_theme_ref(&self, tokens: &mut ThemeTokens, theme_ref: &ThemeRef) {
        // If a named theme is specified, replace all tokens with that theme's values.
        if let Some(ref name) = theme_ref.theme_name {
            if let Some(theme) = self.themes.get(name) {
                *tokens = theme.tokens.clone();
            }
        }

        // Apply per-token overrides on top.
        let o = &theme_ref.overrides;
        if let Some(ref c) = o.bg_deep { tokens.bg_deep = c.clone(); }
        if let Some(ref c) = o.bg_surface { tokens.bg_surface = c.clone(); }
        if let Some(ref c) = o.bg_subtle { tokens.bg_subtle = c.clone(); }
        if let Some(ref c) = o.fg_primary { tokens.fg_primary = c.clone(); }
        if let Some(ref c) = o.fg_secondary { tokens.fg_secondary = c.clone(); }
        if let Some(ref c) = o.fg_muted { tokens.fg_muted = c.clone(); }
        if let Some(ref c) = o.accent_primary { tokens.accent_primary = c.clone(); }
        if let Some(ref c) = o.accent_secondary { tokens.accent_secondary = c.clone(); }
        if let Some(ref c) = o.border_focused { tokens.border_focused = c.clone(); }
        if let Some(ref c) = o.border_unfocused { tokens.border_unfocused = c.clone(); }
        if let Some(ref c) = o.status_bar_bg { tokens.status_bar_bg = c.clone(); }
        if let Some(ref c) = o.status_bar_fg { tokens.status_bar_fg = c.clone(); }
        if let Some(ref c) = o.status_bar_accent { tokens.status_bar_accent = c.clone(); }
        if let Some(ref c) = o.selection_bg { tokens.selection_bg = c.clone(); }
        if let Some(ref c) = o.selection_fg { tokens.selection_fg = c.clone(); }
        if let Some(ref c) = o.error { tokens.error = c.clone(); }
        if let Some(ref c) = o.warn { tokens.warn = c.clone(); }
        if let Some(ref c) = o.info { tokens.info = c.clone(); }
        if let Some(ref c) = o.success { tokens.success = c.clone(); }
    }

    /// List all available themes.
    pub fn list_themes(&self) -> Vec<&ThemeMeta> {
        self.themes.values().map(|t| &t.meta).collect()
    }

    /// Get a theme by name.
    pub fn get_theme(&self, name: &str) -> Option<&Theme> {
        self.themes.get(name)
    }

    /// Reload a theme from disk (for live theme editing).
    pub fn reload_theme(&mut self, name: &str, paths: &[String]) -> Result<(), ThemeError> {
        // Find the theme file.
        let theme_files = discover_themes(paths);
        for path in theme_files {
            if let Ok(theme) = load_theme_file(&path) {
                if theme.meta.name == name {
                    self.themes.insert(name.to_string(), theme);
                    return Ok(());
                }
            }
        }
        Err(ThemeError::NotFound(name.to_string()))
    }
}

/// A fully resolved theme with no cascade indirection.
/// Used by the compositor at render time.
#[derive(Debug, Clone)]
pub struct ResolvedTheme {
    pub tokens: ThemeTokens,
}

impl ResolvedTheme {
    /// Convenience accessor for border color based on focus state.
    pub fn border_color(&self, focused: bool) -> &Color {
        if focused {
            &self.tokens.border_focused
        } else {
            &self.tokens.border_unfocused
        }
    }
}
```

### Step 6: Implement theme.* API methods

```rust
// In crates/shux-rpc/src/methods/theme.rs (stub -- full wiring in task 025)

use serde::{Deserialize, Serialize};

/// theme.list -- list all available themes.
#[derive(Serialize)]
pub struct ThemeListResult {
    pub themes: Vec<ThemeListEntry>,
}

#[derive(Serialize)]
pub struct ThemeListEntry {
    pub name: String,
    pub display_name: Option<String>,
    pub author: Option<String>,
    pub variant: String,
    pub source: String, // "builtin", "user", "plugin"
}

/// theme.get -- get a theme's full token set.
#[derive(Deserialize)]
pub struct ThemeGetParams {
    pub name: String,
}

#[derive(Serialize)]
pub struct ThemeGetResult {
    pub name: String,
    pub tokens: serde_json::Value, // Serialized ThemeTokens.
}

/// theme.set -- set theme at a specific scope.
#[derive(Deserialize)]
pub struct ThemeSetParams {
    /// The scope: "global", "session", "window", or "pane".
    pub scope: String,
    /// The target ID (session/window/pane UUID). Required for non-global.
    pub target_id: Option<String>,
    /// Theme name to apply.
    pub theme_name: Option<String>,
    /// Individual token overrides.
    pub overrides: Option<serde_json::Value>,
}
```

### Step 7: Integrate live theme editing

Extend the config reload watcher (task 023) to also watch theme directories. When a theme file changes, reload that specific theme and trigger a redraw.

```rust
// In the config watcher setup, also watch theme paths:

fn watch_theme_paths(
    watcher: &mut RecommendedWatcher,
    theme_paths: &[String],
) {
    for path_str in theme_paths {
        let expanded = expand_path(path_str);
        if expanded.is_dir() {
            match watcher.watch(&expanded, RecursiveMode::NonRecursive) {
                Ok(()) => tracing::debug!("Watching theme directory: {}", expanded.display()),
                Err(e) => tracing::warn!("Failed to watch theme dir {}: {}", expanded.display(), e),
            }
        }
    }
}

// When a theme file changes:
// 1. Resolve path -> theme name from a maintained map
//    (`HashMap<PathBuf, String>` populated when themes load/register).
// 2. Reload that theme in the ThemeEngine.
// 3. Emit a theme.reloaded event.
// 4. Trigger a full redraw (all panes re-resolve their theme).
```

### Step 8: Wire theme engine into daemon startup

```rust
// In daemon initialization:

let config = loaded_config.read().await;
let mut theme_engine = ThemeEngine::new(&config.config.theme.name);
theme_engine.load_themes_from_paths(&config.config.theme.paths);

tracing::info!(
    "Theme engine initialized: {} themes loaded, global = {}",
    theme_engine.list_themes().len(),
    config.config.theme.name,
);
```

---

## Verification

### Functional

```bash
# Build
cargo build --workspace

# List themes
cargo run -p shux -- theme ls
# Should show: default-dark, default-light

# Get theme details
cargo run -p shux -- theme get default-dark
# Should print all token values

# Set global theme
cargo run -p shux -- theme set --scope global --theme default-light
# TUI should switch to light theme

# Create a custom theme file
mkdir -p ~/.config/shux/themes
cat > ~/.config/shux/themes/my-theme.toml << 'EOF'
[meta]
name = "my-theme"
display_name = "My Custom Theme"
variant = "dark"

[tokens]
bg_deep = "#000000"
bg_surface = "#111111"
accent_primary = "#ff0000"
border_focused = "#ff0000"
EOF

# List themes again
cargo run -p shux -- theme ls
# Should now include "my-theme"

# Apply custom theme
cargo run -p shux -- theme set --scope global --theme my-theme
# TUI should show red accents and borders

# Live theme editing: modify the file
# Change accent_primary to "#00ff00" in my-theme.toml
# Within 500ms, TUI should update to green accents
```

### Tests

```bash
# Unit tests
cargo nextest run -p shux-core --lib theme
cargo nextest run -p shux-core --lib theme_engine

# Integration tests
cargo nextest run -p shux-core --test theme_test

# Test scenarios:
# - Built-in default-dark and default-light themes compile correctly
# - All tokens have reasonable values (not empty, valid hex)
# - Color::to_crossterm() converts correctly for Hex, Named, Ansi256
# - Theme cascade: global -> session -> window -> pane resolves correctly
# - ThemeOverride applies only specified tokens (others inherited)
# - ThemeRef with theme_name replaces all tokens before applying overrides
# - Theme file loading from TOML works
# - Theme file discovery in search paths finds .toml files
# - Invalid theme files are skipped with warnings
# - theme.list returns all registered themes
# - theme.get returns correct tokens for a named theme
# - theme.set at different scopes works
# - Clearing a pane theme reverts to parent cascade
# - ResolvedTheme border_color() returns correct color for focus state
```

---

## Completion Criteria

- [ ] Stable token set implemented: bg (3), fg (3), accent (2), border (2), status_bar (3), selection (2), semantic (4), ANSI (16 optional)
- [ ] Theme struct with metadata (name, display_name, author, variant)
- [ ] ThemeOverride for partial per-token overrides
- [ ] ThemeRef combining named theme + per-token overrides
- [ ] Built-in themes: default-dark and default-light compiled into binary
- [ ] Theme loading from TOML files on disk
- [ ] Theme file auto-discovery in search paths (`~/.config/shux/themes/`)
- [ ] ThemeEngine with cascade resolution: built-in -> global -> session -> window -> pane
- [ ] `resolve_for_pane()` returns complete `ResolvedTheme` at render time
- [ ] theme.list, theme.get, theme.set API methods implemented
- [ ] Live theme editing: file changes trigger theme reload (via config watcher)
- [ ] Color conversion: Hex, Named, Ansi256 -> crossterm::style::Color
- [ ] Comprehensive unit tests for token set, cascade, file loading
- [ ] Default themes look good (validated visually in TUI)

---

## Commit Message

```
feat(core): implement token-based theme engine with cascade resolution

- Define stable semantic token set: bg, fg, accent, border, status_bar, selection, semantic, ANSI (PRD §6.1)
- Theme cascade: built-in -> global -> session -> window -> pane (PRD §5.3)
- Built-in themes: default-dark (Tokyo Night inspired), default-light
- Theme loading from TOML files with auto-discovery in search paths
- ThemeEngine resolves cascade at render time via resolve_for_pane()
- ThemeOverride for partial per-token customization at any level
- theme.list, theme.get, theme.set API methods
- Live theme editing via config watcher integration
```

---

## Session Protocol

1. **Before starting:** Read PRD SS 5.3 (Theme cascade) and SS 6.1 (Theming). Read task 022 (config system) for theme config paths. Review crossterm 0.29 color API. Look at Tokyo Night and Catppuccin color palettes for inspiration.
2. **During:** Implement in order: tokens + Color type (Step 1), Theme struct (Step 2), built-in themes (Step 3), file loading (Step 4), ThemeEngine (Step 5), API methods (Step 6), live editing (Step 7), integration (Step 8). Run `cargo check` after each step. Visually verify themes in TUI after Step 3.
3. **After:** Run full verification suite. Visually inspect both built-in themes in the TUI. Verify theme file loading with a custom theme. Update `docs/PROGRESS.md` (mark 024 done). Update `CLAUDE.md` Learnings with any color rendering observations.
