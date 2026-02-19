# 049 — Bundled Plugin: shux-theme-pack

**Status:** Pending
**Depends On:** 041
**Parallelizable With:** 048, 050

---

## Problem

shux ships with a single hardcoded default-dark theme from M1, but the PRD requires five polished themes out of the box. The theme-pack plugin proves the theme extension point by registering themes via the plugin API rather than hardcoding them in core. This validates that third-party theme authors can use the same mechanism and that the theme engine auto-discovers plugin-provided themes. Each theme must define a complete token set so users have real choices on first launch, and SREs can immediately color-code prod panes (red accent) without writing any config.

## PRD Reference

- **section 7.7** Bundled plugins: `shux-theme-pack` ships 5 themes via theme extension point
- **section 6.1** Theming: token-based themes, per-pane theming, theme files, live editing
- **section 5.3** Theme cascade: built-in -> global -> session -> window -> pane -> runtime override
- **section 7.2** Extension points: theme packs provide named themes
- **section 7.5** WIT: `init` with config JSON + theme registration at startup

---

## Files to Create

- `plugins/shux-theme-pack/plugin.toml` — Plugin manifest
- `plugins/shux-theme-pack/Cargo.toml` — Rust crate for Wasm compilation
- `plugins/shux-theme-pack/src/lib.rs` — Plugin implementation: register 5 themes on init
- `plugins/shux-theme-pack/themes/default-dark.toml` — Default dark theme definition
- `plugins/shux-theme-pack/themes/default-light.toml` — Default light theme definition
- `plugins/shux-theme-pack/themes/prod.toml` — Production theme (red accent)
- `plugins/shux-theme-pack/themes/solarized.toml` — Solarized dark theme
- `plugins/shux-theme-pack/themes/catppuccin-mocha.toml` — Catppuccin Mocha theme
- `plugins/shux-theme-pack/README.md` — User-facing documentation

## Files to Modify

- `Cargo.toml` — Add `plugins/shux-theme-pack` to workspace members
- `crates/shux-plugin/src/builtin.rs` — Register shux-theme-pack as a bundled plugin (compiled-in Wasm or native)
- `docs/PROGRESS.md` — Mark task 049 complete

---

## Execution Steps

### Step 1: Define Plugin Manifest

Create `plugins/shux-theme-pack/plugin.toml`:

```toml
[plugin]
id = "com.shux.theme-pack"
name = "shux Theme Pack"
version = "1.0.0"
api = "shux:plugin@1.0.0"
kind = "wasm"
description = "Ships 5 built-in themes: default-dark, default-light, prod, solarized, catppuccin-mocha"
license = "MIT"
min_shux = "1.0.0"

[plugin.metadata]
categories = ["themes"]
keywords = ["theme", "dark", "light", "solarized", "catppuccin"]

[permissions]
events = []

[extensions]
themes = ["default-dark", "default-light", "prod", "solarized", "catppuccin-mocha"]
```

### Step 2: Define Theme Token Schema

Each theme TOML file follows the token schema from task 024 (theme engine). A complete token set includes:

```toml
# Theme TOML schema — every field is required for a complete theme
[theme]
name = "theme-name"
description = "Human-readable description"
variant = "dark"  # "dark" or "light" — affects fallback behavior

[colors]
bg = "#1e1e2e"
fg = "#cdd6f4"
accent = "#89b4fa"
cursor = "#f5e0dc"
selection_bg = "#45475a"
selection_fg = "#cdd6f4"

[colors.border]
focused = "#89b4fa"
unfocused = "#45475a"

[colors.status_bar]
bg = "#181825"
fg = "#cdd6f4"
active_fg = "#1e1e2e"
active_bg = "#89b4fa"
inactive_fg = "#6c7086"
separator_fg = "#45475a"

[colors.semantic]
error = "#f38ba8"
warning = "#fab387"
info = "#89b4fa"
success = "#a6e3a1"
hint = "#94e2d5"

[colors.ansi]
black = "#45475a"
red = "#f38ba8"
green = "#a6e3a1"
yellow = "#f9e2af"
blue = "#89b4fa"
magenta = "#f5c2e7"
cyan = "#94e2d5"
white = "#bac2de"
bright_black = "#585b70"
bright_red = "#f38ba8"
bright_green = "#a6e3a1"
bright_yellow = "#f9e2af"
bright_blue = "#89b4fa"
bright_magenta = "#f5c2e7"
bright_cyan = "#94e2d5"
bright_white = "#a6adc8"
```

### Step 3: Create Default Dark Theme

Create `plugins/shux-theme-pack/themes/default-dark.toml` — the flagship theme that ships as the default. Inspired by modern terminal aesthetics (Catppuccin Mocha adjacent but distinctly shux):

```toml
[theme]
name = "default-dark"
description = "shux default dark theme — polished dark with blue accents"
variant = "dark"

[colors]
bg = "#1a1b26"
fg = "#c0caf5"
accent = "#7aa2f7"
cursor = "#c0caf5"
selection_bg = "#33467c"
selection_fg = "#c0caf5"

[colors.border]
focused = "#7aa2f7"
unfocused = "#3b4261"

[colors.status_bar]
bg = "#16161e"
fg = "#a9b1d6"
active_fg = "#16161e"
active_bg = "#7aa2f7"
inactive_fg = "#565f89"
separator_fg = "#3b4261"

[colors.semantic]
error = "#f7768e"
warning = "#e0af68"
info = "#7aa2f7"
success = "#9ece6a"
hint = "#73daca"

[colors.ansi]
black = "#414868"
red = "#f7768e"
green = "#9ece6a"
yellow = "#e0af68"
blue = "#7aa2f7"
magenta = "#bb9af7"
cyan = "#7dcfff"
white = "#a9b1d6"
bright_black = "#565f89"
bright_red = "#f7768e"
bright_green = "#9ece6a"
bright_yellow = "#e0af68"
bright_blue = "#7aa2f7"
bright_magenta = "#bb9af7"
bright_cyan = "#7dcfff"
bright_white = "#c0caf5"
```

### Step 4: Create Default Light Theme

Create `plugins/shux-theme-pack/themes/default-light.toml`:

```toml
[theme]
name = "default-light"
description = "shux default light theme — clean light with blue accents"
variant = "light"

[colors]
bg = "#f0f0f4"
fg = "#3b4252"
accent = "#2e7de9"
cursor = "#3b4252"
selection_bg = "#b6d7ff"
selection_fg = "#3b4252"

[colors.border]
focused = "#2e7de9"
unfocused = "#c4c8da"

[colors.status_bar]
bg = "#e1e2e7"
fg = "#3b4252"
active_fg = "#f0f0f4"
active_bg = "#2e7de9"
inactive_fg = "#8990b3"
separator_fg = "#c4c8da"

[colors.semantic]
error = "#c53b53"
warning = "#b15c00"
info = "#2e7de9"
success = "#587539"
hint = "#118c74"

[colors.ansi]
black = "#8990b3"
red = "#c53b53"
green = "#587539"
yellow = "#b15c00"
blue = "#2e7de9"
magenta = "#9854f1"
cyan = "#007197"
white = "#6172b0"
bright_black = "#a1a6c5"
bright_red = "#c53b53"
bright_green = "#587539"
bright_yellow = "#b15c00"
bright_blue = "#2e7de9"
bright_magenta = "#9854f1"
bright_cyan = "#007197"
bright_white = "#3b4252"
```

### Step 5: Create Prod Theme (Red Accent)

Create `plugins/shux-theme-pack/themes/prod.toml` — designed for SRE use with immediately recognizable red accent for production environments:

```toml
[theme]
name = "prod"
description = "Production environment theme — red accents signal danger/awareness"
variant = "dark"

[colors]
bg = "#1c1c1c"
fg = "#d4d4d4"
accent = "#e06c75"
cursor = "#e06c75"
selection_bg = "#4d2c2c"
selection_fg = "#d4d4d4"

[colors.border]
focused = "#e06c75"
unfocused = "#3e3e3e"

[colors.status_bar]
bg = "#2d1111"
fg = "#d4d4d4"
active_fg = "#1c1c1c"
active_bg = "#e06c75"
inactive_fg = "#7a7a7a"
separator_fg = "#3e3e3e"

[colors.semantic]
error = "#ff6b6b"
warning = "#e5c07b"
info = "#61afef"
success = "#98c379"
hint = "#56b6c2"

[colors.ansi]
black = "#3e3e3e"
red = "#e06c75"
green = "#98c379"
yellow = "#e5c07b"
blue = "#61afef"
magenta = "#c678dd"
cyan = "#56b6c2"
white = "#abb2bf"
bright_black = "#5c6370"
bright_red = "#e06c75"
bright_green = "#98c379"
bright_yellow = "#e5c07b"
bright_blue = "#61afef"
bright_magenta = "#c678dd"
bright_cyan = "#56b6c2"
bright_white = "#d4d4d4"
```

### Step 6: Create Solarized Theme

Create `plugins/shux-theme-pack/themes/solarized.toml` — faithfully implements Ethan Schoonover's Solarized Dark palette:

```toml
[theme]
name = "solarized"
description = "Solarized Dark — Ethan Schoonover's precision color scheme"
variant = "dark"

[colors]
bg = "#002b36"
fg = "#839496"
accent = "#268bd2"
cursor = "#839496"
selection_bg = "#073642"
selection_fg = "#93a1a1"

[colors.border]
focused = "#268bd2"
unfocused = "#073642"

[colors.status_bar]
bg = "#073642"
fg = "#93a1a1"
active_fg = "#002b36"
active_bg = "#268bd2"
inactive_fg = "#586e75"
separator_fg = "#073642"

[colors.semantic]
error = "#dc322f"
warning = "#cb4b16"
info = "#268bd2"
success = "#859900"
hint = "#2aa198"

[colors.ansi]
black = "#073642"
red = "#dc322f"
green = "#859900"
yellow = "#b58900"
blue = "#268bd2"
magenta = "#d33682"
cyan = "#2aa198"
white = "#eee8d5"
bright_black = "#002b36"
bright_red = "#cb4b16"
bright_green = "#586e75"
bright_yellow = "#657b83"
bright_blue = "#839496"
bright_magenta = "#6c71c4"
bright_cyan = "#93a1a1"
bright_white = "#fdf6e3"
```

### Step 7: Create Catppuccin Mocha Theme

Create `plugins/shux-theme-pack/themes/catppuccin-mocha.toml` — implements the Catppuccin Mocha palette:

```toml
[theme]
name = "catppuccin-mocha"
description = "Catppuccin Mocha — warm, cozy dark theme"
variant = "dark"

[colors]
bg = "#1e1e2e"
fg = "#cdd6f4"
accent = "#89b4fa"
cursor = "#f5e0dc"
selection_bg = "#45475a"
selection_fg = "#cdd6f4"

[colors.border]
focused = "#89b4fa"
unfocused = "#45475a"

[colors.status_bar]
bg = "#181825"
fg = "#cdd6f4"
active_fg = "#1e1e2e"
active_bg = "#89b4fa"
inactive_fg = "#6c7086"
separator_fg = "#45475a"

[colors.semantic]
error = "#f38ba8"
warning = "#fab387"
info = "#89b4fa"
success = "#a6e3a1"
hint = "#94e2d5"

[colors.ansi]
black = "#45475a"
red = "#f38ba8"
green = "#a6e3a1"
yellow = "#f9e2af"
blue = "#89b4fa"
magenta = "#f5c2e7"
cyan = "#94e2d5"
white = "#bac2de"
bright_black = "#585b70"
bright_red = "#f38ba8"
bright_green = "#a6e3a1"
bright_yellow = "#f9e2af"
bright_blue = "#89b4fa"
bright_magenta = "#f5c2e7"
bright_cyan = "#94e2d5"
bright_white = "#a6adc8"
```

### Step 8: Implement Plugin Logic

Create `plugins/shux-theme-pack/src/lib.rs`:

```rust
//! shux-theme-pack — Bundled theme pack plugin.
//!
//! Registers 5 themes on init: default-dark, default-light, prod,
//! solarized, catppuccin-mocha. Each theme is a complete token set
//! parsed from embedded TOML at compile time.

use shux_plugin_api::prelude::*;

/// Embedded theme TOML files (included at compile time).
const THEMES: &[(&str, &str)] = &[
    ("default-dark", include_str!("../themes/default-dark.toml")),
    ("default-light", include_str!("../themes/default-light.toml")),
    ("prod", include_str!("../themes/prod.toml")),
    ("solarized", include_str!("../themes/solarized.toml")),
    ("catppuccin-mocha", include_str!("../themes/catppuccin-mocha.toml")),
];

struct ThemePackPlugin;

impl Plugin for ThemePackPlugin {
    fn init(&mut self, _config: &str) -> Result<(), PluginError> {
        // Register each theme with the host during init.
        // The host's theme engine will auto-discover these and make them
        // available for selection via CLI, API, or config.
        for (name, toml_content) in THEMES {
            host::log(LogLevel::Info, &format!("Registering theme: {}", name));

            // Parse the TOML into a theme definition and send to host.
            // The host validates the token set completeness.
            match host::register_theme(name, toml_content) {
                Ok(()) => {
                    host::log(LogLevel::Debug, &format!("Theme '{}' registered", name));
                }
                Err(e) => {
                    host::log(
                        LogLevel::Error,
                        &format!("Failed to register theme '{}': {}", name, e.message),
                    );
                    // Continue registering other themes — don't fail the whole plugin
                }
            }
        }

        host::log(
            LogLevel::Info,
            &format!("shux-theme-pack initialized with {} themes", THEMES.len()),
        );
        Ok(())
    }

    fn shutdown(&mut self) {
        host::log(LogLevel::Debug, "shux-theme-pack shutting down");
    }

    fn on_event(&mut self, _event_json: &str) -> Result<(), PluginError> {
        // Theme pack is passive — no event handling needed.
        Ok(())
    }

    fn on_command(&mut self, name: &str, args: &[String]) -> Result<String, PluginError> {
        match name {
            "theme-pack.list" => {
                let names: Vec<&str> = THEMES.iter().map(|(n, _)| *n).collect();
                Ok(serde_json::to_string(&names).unwrap_or_default())
            }
            "theme-pack.info" => {
                let theme_name = args.first().map(|s| s.as_str()).unwrap_or("");
                match THEMES.iter().find(|(n, _)| *n == theme_name) {
                    Some((_, toml)) => Ok(toml.to_string()),
                    None => Err(PluginError {
                        code: -1,
                        message: format!("Unknown theme: {}", theme_name),
                    }),
                }
            }
            _ => Err(PluginError {
                code: -1,
                message: format!("Unknown command: {}", name),
            }),
        }
    }

    fn render_segment(&mut self, _id: &str, _width: u16) -> Result<String, PluginError> {
        // Theme pack does not provide status bar segments
        Ok(String::new())
    }

    fn render_overlay(
        &mut self,
        _pane_id: &str,
        _width: u16,
        _height: u16,
    ) -> Result<Option<String>, PluginError> {
        Ok(None)
    }

    fn on_overlay_input(
        &mut self,
        _pane_id: &str,
        _key_event_json: &str,
    ) -> Result<bool, PluginError> {
        Ok(false)
    }

    fn intercept_event(&mut self, event_json: &str) -> Result<Option<String>, PluginError> {
        // Pass through all events unmodified
        Ok(Some(event_json.to_string()))
    }
}

export_plugin!(ThemePackPlugin);
```

### Step 9: Create Cargo.toml for the Plugin

Create `plugins/shux-theme-pack/Cargo.toml`:

```toml
[package]
name = "shux-theme-pack"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
description = "shux bundled theme pack: 5 polished themes"

[lib]
crate-type = ["cdylib"]

[dependencies]
shux-plugin-api = { path = "../../crates/shux-plugin-api" }
serde_json.workspace = true
```

### Step 10: Add Unit Tests

Add tests to `plugins/shux-theme-pack/src/lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_five_themes_are_embedded() {
        assert_eq!(THEMES.len(), 5);
        let names: Vec<&str> = THEMES.iter().map(|(n, _)| *n).collect();
        assert!(names.contains(&"default-dark"));
        assert!(names.contains(&"default-light"));
        assert!(names.contains(&"prod"));
        assert!(names.contains(&"solarized"));
        assert!(names.contains(&"catppuccin-mocha"));
    }

    #[test]
    fn all_themes_parse_as_valid_toml() {
        for (name, content) in THEMES {
            let parsed: Result<toml::Value, _> = toml::from_str(content);
            assert!(parsed.is_ok(), "Theme '{}' failed to parse as TOML: {:?}", name, parsed.err());
        }
    }

    #[test]
    fn all_themes_have_complete_token_set() {
        let required_paths = [
            "theme.name", "theme.variant",
            "colors.bg", "colors.fg", "colors.accent", "colors.cursor",
            "colors.selection_bg", "colors.selection_fg",
            "colors.border.focused", "colors.border.unfocused",
            "colors.status_bar.bg", "colors.status_bar.fg",
            "colors.status_bar.active_fg", "colors.status_bar.active_bg",
            "colors.status_bar.inactive_fg", "colors.status_bar.separator_fg",
            "colors.semantic.error", "colors.semantic.warning",
            "colors.semantic.info", "colors.semantic.success",
            "colors.semantic.hint",
            "colors.ansi.black", "colors.ansi.red", "colors.ansi.green",
            "colors.ansi.yellow", "colors.ansi.blue", "colors.ansi.magenta",
            "colors.ansi.cyan", "colors.ansi.white",
            "colors.ansi.bright_black", "colors.ansi.bright_red",
            "colors.ansi.bright_green", "colors.ansi.bright_yellow",
            "colors.ansi.bright_blue", "colors.ansi.bright_magenta",
            "colors.ansi.bright_cyan", "colors.ansi.bright_white",
        ];

        for (name, content) in THEMES {
            let parsed: toml::Value = toml::from_str(content)
                .unwrap_or_else(|e| panic!("Theme '{}' TOML parse error: {}", name, e));

            for path in &required_paths {
                let parts: Vec<&str> = path.split('.').collect();
                let mut current = &parsed;
                for part in &parts {
                    current = current.get(part).unwrap_or_else(|| {
                        panic!("Theme '{}' missing required token: {}", name, path);
                    });
                }
            }
        }
    }

    #[test]
    fn all_color_values_are_valid_hex() {
        let hex_re = regex::Regex::new(r"^#[0-9a-fA-F]{6}$").unwrap();

        for (name, content) in THEMES {
            let parsed: toml::Value = toml::from_str(content).unwrap();
            check_hex_colors(&parsed, name, "", &hex_re);
        }
    }

    fn check_hex_colors(value: &toml::Value, theme: &str, path: &str, re: &regex::Regex) {
        match value {
            toml::Value::Table(map) => {
                for (key, val) in map {
                    let new_path = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{}.{}", path, key)
                    };
                    check_hex_colors(val, theme, &new_path, re);
                }
            }
            toml::Value::String(s) if s.starts_with('#') => {
                assert!(
                    re.is_match(s),
                    "Theme '{}' invalid hex color at {}: '{}'",
                    theme, path, s
                );
            }
            _ => {}
        }
    }

    #[test]
    fn prod_theme_has_red_accent() {
        let (_, content) = THEMES.iter().find(|(n, _)| *n == "prod").unwrap();
        let parsed: toml::Value = toml::from_str(content).unwrap();
        let accent = parsed["colors"]["accent"].as_str().unwrap();
        // Verify the accent is in the red spectrum (R channel dominant)
        let r = u8::from_str_radix(&accent[1..3], 16).unwrap();
        let g = u8::from_str_radix(&accent[3..5], 16).unwrap();
        let b = u8::from_str_radix(&accent[5..7], 16).unwrap();
        assert!(r > g && r > b, "Prod theme accent should be red-dominant, got {}", accent);
    }

    #[test]
    fn default_dark_and_light_variants_correct() {
        for (name, content) in THEMES {
            let parsed: toml::Value = toml::from_str(content).unwrap();
            let variant = parsed["theme"]["variant"].as_str().unwrap();
            if *name == "default-light" {
                assert_eq!(variant, "light");
            } else {
                assert_eq!(variant, "dark", "Theme '{}' should be dark variant", name);
            }
        }
    }
}
```

### Step 11: Validate Theme Manager Contract

Add integration coverage that proves the plugin-host theme contract (not just local TOML parsing):

- Plugin-registered themes are discoverable by host theme listing (`theme.list` / `shux theme ls`).
- Runtime theme selection emits `theme.changed` and updates rendering without restart.
- Per-pane overrides still compose correctly with globally selected plugin themes.
- Unknown/extra theme fields are ignored safely for forward compatibility.
- Registration failures are isolated per-theme (one bad theme does not block others).

---

## Verification

### Functional

```bash
# Build the theme-pack plugin
cargo build -p shux-theme-pack

# Verify all theme TOML files parse correctly
cargo check -p shux-theme-pack

# After full daemon integration, verify themes are discoverable
shux theme ls
# Expected: default-dark, default-light, prod, solarized, catppuccin-mocha

# Verify theme application
shux theme set default-light
# Expected: UI updates to light theme within 500ms

shux theme set -p <pane-id> prod
# Expected: pane borders and status bar segment reflect red accent

# Verify theme.changed emission and runtime switching
shux events watch --filter theme.changed &
shux theme set solarized
# Expected: theme.changed event emitted with selected theme name
```

### Tests

```bash
# Run theme-pack unit tests
cargo nextest run -p shux-theme-pack

# Expected tests passing:
# - all_five_themes_are_embedded
# - all_themes_parse_as_valid_toml
# - all_themes_have_complete_token_set
# - all_color_values_are_valid_hex
# - prod_theme_has_red_accent
# - default_dark_and_light_variants_correct
```

---

## Completion Criteria

- [ ] Plugin manifest (`plugin.toml`) declares kind=wasm, extensions.themes lists all 5 theme names
- [ ] All 5 themes defined as complete TOML files with every token present
- [ ] default-dark: polished dark with blue accents, variant=dark
- [ ] default-light: clean light with blue accents, variant=light
- [ ] prod: dark with red accent (R channel dominant), border.focused is red
- [ ] solarized: faithful Solarized Dark palette (Ethan Schoonover's hex values)
- [ ] catppuccin-mocha: faithful Catppuccin Mocha palette
- [ ] Every theme has: bg, fg, accent, cursor, selection_bg, selection_fg, border (focused/unfocused), status_bar (6 tokens), semantic (5 tokens), ANSI palette (16 colors)
- [ ] All color values are valid 6-digit hex codes
- [ ] Plugin registers all themes during `init()` via host API
- [ ] Failed theme registration logs error but does not crash the plugin
- [ ] Themes are auto-discovered by the theme engine after plugin loads
- [ ] Runtime theme switch emits `theme.changed` and applies without daemon restart
- [ ] Per-pane cascade remains correct when global theme comes from plugin pack
- [ ] Unknown optional fields in theme TOML are ignored (forward-compatible parsing)
- [ ] `theme-pack.list` command returns all 5 theme names
- [ ] `theme-pack.info <name>` returns the TOML definition
- [ ] Unit tests validate TOML parsing, completeness, hex validity, variant correctness
- [ ] Plugin compiles to Wasm target (wasm32-wasip2)

---

## Commit Message

```
feat(plugins): add shux-theme-pack with 5 bundled themes

- Wasm plugin providing default-dark, default-light, prod (red accent),
  solarized, and catppuccin-mocha themes
- Each theme defines complete token set: bg, fg, accent, cursor,
  selection, borders, status bar, semantic colors, full ANSI palette
- Themes embedded at compile time and registered on plugin init
- Auto-discovered by theme engine for CLI/API/config selection
- Proves theme extension point for third-party theme authors
```

---

## Session Protocol

1. **Before starting:** Read task 041 (plugin lifecycle) completion output to understand how plugins register during init. Read task 024 (theme engine) for the `ThemeDefinition` struct and token schema. Verify the host-side `register_theme` function exists in the WIT/host implementation.
2. **During:** Create theme TOML files first (Steps 3-7), validate them with `toml::from_str` manually, then implement plugin logic (Step 8), package metadata/tests (Steps 9-10), and host contract validation (Step 11). Run `cargo check` after each theme file. Compile to wasm32-wasip2 target to verify Wasm compatibility.
3. **Color accuracy:** Cross-reference Solarized hex values against the official solarized.ethanschoonover.com palette. Cross-reference Catppuccin Mocha against the official catppuccin/catppuccin GitHub repository. Do not approximate.
4. **Edge cases to watch for:**
   - Theme TOML parsing failures should not crash the plugin (log and continue)
   - Color contrast: verify status bar text is readable against status bar bg for each theme
   - Light theme: ensure variant="light" is used so fallback behavior works correctly
   - Prod theme: red accent must be visually distinct from error color
5. **After:** Run full test suite (`cargo nextest run --workspace`). Verify themes load correctly in a running daemon. Update `docs/PROGRESS.md`. Update `CLAUDE.md` Learnings.
