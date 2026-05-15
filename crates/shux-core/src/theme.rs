//! Theme tokens and parsing.
//!
//! This is the M1 slice of the theme engine described in task 024 — a
//! minimal, render-time token system that lets users override the
//! hardcoded Catppuccin Macchiato palette via `[theme]` in their
//! `~/.config/shux/config.toml`. The full cascade (session / window /
//! pane levels and built-in named themes) lands later alongside the
//! `shux-theme-pack` plugin (task 049).
//!
//! Schema:
//!
//! ```toml
//! [theme]
//! # Pane border colors (focused gets the accent treatment).
//! border_focused   = "#74c7ec"   # Catppuccin Macchiato Sapphire (default)
//! border_unfocused = "#5b6078"   # Catppuccin Macchiato Surface2 (default)
//!
//! # Status bar colors. status_accent is also used for the leading
//! # session-name segment.
//! status_bg     = "#1e2030"      # Catppuccin Macchiato Crust
//! status_fg     = "#cad3f5"      # Catppuccin Macchiato Text
//! status_accent = "#74c7ec"      # Catppuccin Macchiato Sapphire
//! ```
//!
//! Every key is optional. Missing keys fall through to the built-in
//! defaults, so an empty `[theme]` table is equivalent to no `[theme]`
//! at all.

use serde::{Deserialize, Serialize};

/// User-facing config: a thin wrapper around five optional color
/// strings. Lives inside `Config::theme` and is reloaded by the same
/// `ConfigHandle` watcher that handles the rest of the file, so theme
/// edits are live without restarting shux.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThemeConfig {
    #[serde(default)]
    pub border_focused: Option<String>,
    #[serde(default)]
    pub border_unfocused: Option<String>,
    #[serde(default)]
    pub status_bg: Option<String>,
    #[serde(default)]
    pub status_fg: Option<String>,
    #[serde(default)]
    pub status_accent: Option<String>,
    /// Used by the built-in status bar for the onboarding hint and any
    /// secondary muted-info segments (uptime, multi-client count).
    #[serde(default)]
    pub status_muted: Option<String>,
    /// Used by the built-in status bar for the project / git branch
    /// segment in the left zone.
    #[serde(default)]
    pub status_branch: Option<String>,
}

/// A single RGB color, kept as a plain triple so renderers in
/// downstream crates (which import `crossterm`) can do their own
/// `Color::Rgb { r, g, b }` construction without `shux-core` needing a
/// crossterm dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// The fully-resolved theme used by the renderer. Every field is a
/// concrete `Rgb` value — there are no `Option`s here. Built by walking
/// the `ThemeConfig` and falling back to the `Theme::DEFAULT` palette
/// on any missing or unparseable entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub border_focused: Rgb,
    pub border_unfocused: Rgb,
    pub status_bg: Rgb,
    pub status_fg: Rgb,
    pub status_accent: Rgb,
    pub status_muted: Rgb,
    pub status_branch: Rgb,
}

impl Theme {
    /// The hardcoded baseline (Catppuccin Macchiato). Matches the
    /// pre-theme behavior so users who don't write a `[theme]` block
    /// see no visual change.
    pub const DEFAULT: Self = Self {
        // Sapphire #74c7ec
        border_focused: Rgb::new(116, 199, 236),
        // Surface2 #5b6078
        border_unfocused: Rgb::new(91, 96, 120),
        // Crust #1e2030
        status_bg: Rgb::new(30, 32, 48),
        // Text #cad3f5
        status_fg: Rgb::new(202, 211, 245),
        // Sapphire (same as focused border)
        status_accent: Rgb::new(116, 199, 236),
        // Surface2 #5b6078 — same family as unfocused border. Used
        // for the onboarding hint and other quiet right-zone signals.
        status_muted: Rgb::new(91, 96, 120),
        // Mauve #c6a0f6 — git-branch identity in the left zone.
        status_branch: Rgb::new(198, 160, 246),
    };

    /// Resolve `cfg` against `Theme::DEFAULT`. Unparseable hex strings
    /// fall back to the default field rather than failing loudly —
    /// theme edits should never break rendering, and `shux config
    /// validate` is the right place to surface bad colors.
    pub fn resolve(cfg: &ThemeConfig) -> Self {
        let d = Self::DEFAULT;
        Self {
            border_focused: parse_or(cfg.border_focused.as_deref(), d.border_focused),
            border_unfocused: parse_or(cfg.border_unfocused.as_deref(), d.border_unfocused),
            status_bg: parse_or(cfg.status_bg.as_deref(), d.status_bg),
            status_fg: parse_or(cfg.status_fg.as_deref(), d.status_fg),
            status_accent: parse_or(cfg.status_accent.as_deref(), d.status_accent),
            status_muted: parse_or(cfg.status_muted.as_deref(), d.status_muted),
            status_branch: parse_or(cfg.status_branch.as_deref(), d.status_branch),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Parse a `#RRGGBB` (with or without leading `#`) into RGB. Tolerant
/// of upper/lower case; returns `None` on any other shape.
pub fn parse_hex(s: &str) -> Option<Rgb> {
    let s = s.trim_start_matches('#');
    if s.len() != 6 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some(Rgb { r, g, b })
}

fn parse_or(s: Option<&str>, fallback: Rgb) -> Rgb {
    s.and_then(parse_hex).unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_with_hash() {
        assert_eq!(parse_hex("#74c7ec"), Some(Rgb::new(116, 199, 236)));
    }

    #[test]
    fn parse_hex_without_hash() {
        assert_eq!(parse_hex("74c7ec"), Some(Rgb::new(116, 199, 236)));
    }

    #[test]
    fn parse_hex_uppercase() {
        assert_eq!(parse_hex("#74C7EC"), Some(Rgb::new(116, 199, 236)));
    }

    #[test]
    fn parse_hex_rejects_short() {
        assert_eq!(parse_hex("#abc"), None);
    }

    #[test]
    fn parse_hex_rejects_garbage() {
        assert_eq!(parse_hex("not-a-color"), None);
        assert_eq!(parse_hex("#xyzxyz"), None);
        assert_eq!(parse_hex(""), None);
    }

    #[test]
    fn empty_config_resolves_to_defaults() {
        let cfg = ThemeConfig::default();
        assert_eq!(Theme::resolve(&cfg), Theme::DEFAULT);
    }

    #[test]
    fn resolve_overrides_each_field() {
        let cfg = ThemeConfig {
            border_focused: Some("#ff0000".into()),
            border_unfocused: Some("#00ff00".into()),
            status_bg: Some("#0000ff".into()),
            status_fg: Some("#ffffff".into()),
            status_accent: Some("#abcdef".into()),
            status_muted: None,
            status_branch: None,
        };
        let t = Theme::resolve(&cfg);
        assert_eq!(t.border_focused, Rgb::new(255, 0, 0));
        assert_eq!(t.border_unfocused, Rgb::new(0, 255, 0));
        assert_eq!(t.status_bg, Rgb::new(0, 0, 255));
        assert_eq!(t.status_fg, Rgb::new(255, 255, 255));
        assert_eq!(t.status_accent, Rgb::new(0xab, 0xcd, 0xef));
    }

    #[test]
    fn resolve_falls_through_per_field() {
        // Override only border_focused; the rest stay default.
        let cfg = ThemeConfig {
            border_focused: Some("#abcdef".into()),
            ..Default::default()
        };
        let t = Theme::resolve(&cfg);
        assert_eq!(t.border_focused, Rgb::new(0xab, 0xcd, 0xef));
        assert_eq!(t.border_unfocused, Theme::DEFAULT.border_unfocused);
        assert_eq!(t.status_bg, Theme::DEFAULT.status_bg);
    }

    #[test]
    fn unparseable_color_falls_back_to_default() {
        let cfg = ThemeConfig {
            border_focused: Some("not-a-color".into()),
            ..Default::default()
        };
        let t = Theme::resolve(&cfg);
        assert_eq!(t.border_focused, Theme::DEFAULT.border_focused);
    }

    #[test]
    fn theme_default_matches_const() {
        assert_eq!(Theme::default(), Theme::DEFAULT);
    }
}
