//! `shux config …` handlers, and the config file `init` writes.

use crate::style;

/// Default contents written by `shux config init`. ONE file. The
/// `[[statusbar.segment]]` entries embed their starship config inline
/// via `starship_config = """..."""` — no separate `statusbar.toml`
/// to maintain. `shux config show` returns the same bytes.
pub const DEFAULT_CONFIG_TOML: &str = r##"# ~/.config/shux/config.toml
#
# shux user configuration. The daemon hot-reloads this file: edits land
# in attached sessions on the next render frame, no restart needed.

[appearance]
# Pane border style: thin | thick | double | rounded | ascii | none
border_style = "rounded"
# Render the status bar with Nerd Font glyphs (terminal icon, git
# branch, window icon, ssh host). Default true — shux bundles the
# full JetBrains Mono Nerd Font (2.4 MB) so the PNG rasterizer
# resolves every NF codepoint OOTB, no tofu. In a live attach, your
# terminal's font decides; set to false here if your terminal lacks
# NF — the ASCII fallback (◆ ± ▶ @) works in any font.
nerd_fonts = true
# Optional custom primary text font for the PNG rasterizer. The
# bundled NF JetBrains Mono, text-symbol fallbacks, and Noto Emoji
# stay in the fallback chain so common glyphs your font lacks
# (typical for plain non-patched typefaces, TUI symbols, or standalone
# emoji) still resolve — no tofu either way. Doesn't affect live
# attach (your terminal font controls that).
# Font changes hot-reload: edit this line and the next snapshot uses
# the new font. On a bad path the last-good rasterizer is retained
# and a warning is logged.
# font = "/path/to/your-font.ttf"
#
# Optional ordered fallback chain for PNG snapshots only. Entries can
# be builtin tokens or absolute font paths. Omit this field to use the
# default builtin chain shown here. Set it explicitly when a TUI needs
# a local/system font without changing the primary metrics font.
# Empty lists are invalid. If font is unset, bundled JetBrains Mono
# remains the primary metrics font and this list only changes glyph
# fallback coverage.
# font_fallbacks = ["builtin:nerd-font", "builtin:math", "builtin:symbols", "builtin:symbols-legacy", "builtin:emoji"]
#
# For status-bar segments, paste either the literal glyph (UTF-8) into
# a single-quoted TOML string, or use TOML's escape inside a
# double-quoted string. Note TOML uses bare \uXXXX (4-hex BMP) or
# \UXXXXXXXX (8-hex, supplementary plane) — NOT Rust's \u{XXXX} form:
#   nf-pl-branch      U+E0A0   ''  or  "\uE0A0"
#   nf-md-kubernetes  U+F10FE  '󱃾'  or  "\U000F10FE"
#   nf-md-ship_wheel  U+F124A  '󱉊'  or  "\U000F124A"
# Common text UI glyphs (↻, ⠹, ✔, ✗, ⏎, ⌥) and standalone
# monochrome emoji (🍺 🧩 🦀 🚀 ⚡ …) render correctly in PNG snapshots
# via bundled fallbacks — no extra configuration needed. Colour emoji
# and composed emoji (ZWJ sequences like 👨‍💻, VS16 like 🛠️,
# regional-indicator flag pairs, skin-tone modifiers) are not yet
# supported — the parser splits them into separate cells. For composed
# glyphs in status bars, configure your starship language modules with
# the NF equivalent.
# Example for rust: symbol = ' ' (or
# symbol = "\uE7A8 " using TOML escape syntax).

[keys]
# Prefix key (e.g. "ctrl-space", "ctrl-b", "alt-w")
prefix = "ctrl-space"

[shell]
# Override the argv a pane runs when no command was given. Empty (the
# default) means `$SHELL -l -i`. `shux new -- vim a.rs` still runs vim;

# this only decides what "just a shell" means. The program named here is
# also what interprets a `--cmd` string.
#   command = ["/bin/zsh", "-l", "-i"]
#
# Extra env for every spawned pane. A pane that sets the same variable
# explicitly keeps its own value.
#   env = { LC_ALL = "en_US.UTF-8" }

[keybindings]
# Optional attach key overrides. Keys use the same notation as prefix:
# "alt-h" targets the root table; "prefix h" targets the key pressed
# after the configured prefix. Values are action names, for example:
#   "alt-h" = "focus-left"
#   "prefix c" = "new-window"
#   "prefix [" = "copy-mode"

# ─────────────────────────────────────────────────────────────────────
# Theme: override the built-in Catppuccin Macchiato palette. Every key
# is optional; missing keys fall through to the defaults so an empty
# (or absent) [theme] block is equivalent to no [theme] at all. Edits
# hot-reload like the rest of the file — borders + status bar pick up
# the new colors on the next render frame.
# ─────────────────────────────────────────────────────────────────────

# [theme]
# border_focused   = "#74c7ec"   # Catppuccin Sapphire (default)
# border_unfocused = "#5b6078"   # Catppuccin Surface2 (default)
# status_bg        = "#1e2030"   # Catppuccin Crust
# status_fg        = "#cad3f5"   # Catppuccin Text
# status_accent    = "#74c7ec"   # Catppuccin Sapphire

# ─────────────────────────────────────────────────────────────────────
# Status-bar segments. Each entry runs `command` every `interval_ms`
# and renders the captured stdout (ANSI colors preserved) into the
# named zone. Fallback text shows when the command is missing or
# fails — keeps the bar pretty even on machines without the binary.
#
# `starship_config` is an INLINE TOML string. shux materialises it into
# the daemon's private runtime dir (mode 0600) and exports
# `STARSHIP_CONFIG=<that file>` for the spawned `starship prompt`
# invocation. The runner also defaults
# Starship status-bar spawns to raw ANSI output (`STARSHIP_SHELL=cmd`,
# `TERM=xterm-256color`) so shell prompt guards like Bash `\[` / `\]`
# never leak into the bar. Your shell PS1 (driven by
# `~/.config/starship.toml`) is unaffected — only the segment spawn
# sees these overrides.
# ─────────────────────────────────────────────────────────────────────

[[statusbar.segment]]
zone = "right"
command = ["starship", "prompt"]
interval_ms = 1000
fallback = " (starship not installed) "
env = { STARSHIP_SHELL = "cmd", TERM = "xterm-256color" }
starship_config = """
add_newline = false
format = '''
$git_branch\
$git_status\
$rust\
$python\
$nodejs\
$cmd_duration\
$time\
'''

[time]
disabled = false
format = ' [$time](bold #f5a97f) '
time_format = '%H:%M'

[git_branch]
format = '[$symbol$branch]($style) '
style = 'bold #c6a0f6'
# nf-pl-branch (U+E0A0).
symbol = " "

[git_status]
format = '[$all_status$ahead_behind]($style)'
style = 'bold #ed8796'

[rust]
format = '[$symbol($version)]($style) '
style = 'bold #ee99a0'
# nf-dev-rust (U+E7A8).
symbol = " "

[python]
format = '[$symbol${pyenv_prefix}(${version} )(($virtualenv) )]($style)'
style = 'bold #eed49f'
# nf-dev-python (U+E73C).
symbol = " "

[nodejs]
format = '[$symbol($version)]($style) '
style = 'bold #a6da95'
# nf-dev-nodejs (U+E718).
symbol = " "

[cmd_duration]
min_time = 0
format = '[ $duration]($style) '
style = 'bold #91d7e3'
"""
"##;

pub const SHELL_HINT: &str = r##"
SUGGESTED ~/.bashrc / ~/.zshrc snippet:

  # Skip the rich starship PS1 when shux is hosting (the status bar has it).
  if command -v starship >/dev/null 2>&1; then
    if [[ -n $SHUX ]]; then
      PS1='\[\e[36m\]❯\[\e[0m\] '
    else
      eval "$(starship init bash)"
    fi
  fi

This makes the in-pane prompt a clean cyan chevron, while the status
bar at the bottom of the screen carries the rich starship segments.
"##;

/// `shux config init`: scaffold a single ~/.config/shux/config.toml
/// with an inline starship status-bar config. No second file.
pub fn handle_config_init(force: bool) -> anyhow::Result<()> {
    let cfg_path = shux_core::config::default_config_path();
    if let Some(parent) = cfg_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_or_skip(&cfg_path, DEFAULT_CONFIG_TOML, force)?;

    style::print_success(
        "Config initialised at",
        cfg_path.display().to_string().as_str(),
        None,
    );
    println!("{}", SHELL_HINT);
    Ok(())
}

pub fn write_or_skip(path: &std::path::Path, contents: &str, force: bool) -> anyhow::Result<()> {
    if path.exists() && !force {
        eprintln!(
            "skip {} (exists; pass --force to overwrite)",
            path.display()
        );
        return Ok(());
    }
    std::fs::write(path, contents)?;
    Ok(())
}

pub fn handle_config_path() -> anyhow::Result<()> {
    let p = shux_core::config::default_config_path();
    println!("{}", p.display());
    Ok(())
}

/// `shux config reset-hints` — wipe the onboarding state file so the
/// next attach shows the welcome toast and right-zone hint again.
/// Idempotent: silently succeeds if the file isn't there.
pub fn handle_config_reset_hints() -> anyhow::Result<()> {
    let path = onboarding_state_path();
    let existed = path.exists();
    if existed {
        std::fs::remove_file(&path)?;
    }
    if existed {
        crate::style::print_success(
            "Reset onboarding hints",
            path.display().to_string().as_str(),
            None,
        );
    } else {
        crate::style::print_success(
            "Onboarding state already clear",
            path.display().to_string().as_str(),
            None,
        );
    }
    println!(
        "  {} the welcome toast and right-zone hint will show again on the next `shux` attach.",
        crate::style::muted("→")
    );
    Ok(())
}

/// Where onboarding.json lives. Matches `onboarding::state_file_path`
/// but lives here too because the cli layer can't import the bin-only
/// `onboarding` module (Rust's privacy rules mean the path logic is
/// duplicated, but it's 5 lines and changes never).
pub fn onboarding_state_path() -> std::path::PathBuf {
    if let Ok(xdg) = std::env::var("XDG_STATE_HOME") {
        return std::path::PathBuf::from(xdg)
            .join("shux")
            .join("onboarding.json");
    }
    if let Ok(home) = std::env::var("HOME") {
        return std::path::PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("shux")
            .join("onboarding.json");
    }
    std::path::PathBuf::from("onboarding.json")
}

pub fn handle_config_show() -> anyhow::Result<()> {
    print!("{}", DEFAULT_CONFIG_TOML);
    Ok(())
}

/// `shux config validate [PATH | --config <path>]`. Returns the process
/// exit code that the caller should propagate (0 clean, 1 had diagnostics).
pub fn handle_config_validate(path: Option<std::path::PathBuf>) -> anyhow::Result<i32> {
    let resolved = path.unwrap_or_else(shux_core::config::default_config_path);
    let used_default = resolved == shux_core::config::default_config_path();

    if !resolved.exists() {
        if used_default {
            crate::style::print_error(&format!(
                "config file not found: {} — run `shux config init` to scaffold one, \
                 or pass a path: `shux config validate <PATH>`",
                resolved.display()
            ));
        } else {
            crate::style::print_error(&format!("config file not found: {}", resolved.display()));
        }
        return Ok(1);
    }

    let diags = crate::config_validate::validate(&resolved)?;
    Ok(crate::config_validate::print_diagnostics(&diags, &resolved))
}
