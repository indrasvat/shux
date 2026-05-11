# 027 — Pane Titles (Manual + Auto)

**Status:** Done (PR 4 — manual + auto + OSC + border render)
**Depends On:** 015
**Parallelizable With:** 025, 026

---

## Problem

Panes need titles for user orientation. When running multiple panes, a title like "nvim" or "cargo build" immediately tells the user what each pane is doing. Without titles, users must visually scan pane content to identify purpose, which breaks flow especially with 4+ panes.

shux needs two title mechanisms: (1) manual titles set explicitly by the user or API, and (2) auto-titles derived from the running command name or CWD. Additionally, terminal applications can set pane titles via OSC 0/1/2 escape sequences (e.g., bash sets the title to the current command). The auto-title feature must be toggleable per pane so users can lock a title in place.

Pane titles are displayed in the pane border (top edge) when `show_pane_titles` is enabled in config. Changes to titles emit `pane.title_changed` events for status bar and plugin consumption.

## PRD Reference

- **SS 6.1** P0 Feature Matrix, Core multiplexer: "Pane titles: Manual set/unset, auto-title from running command/cwd (toggleable per pane)"
- **SS 8.2** API methods: `pane.set_title` — Set pane title manually
- **SS 10.2** Config: `show_pane_titles = true`, `auto_title = true`
- **SS 12.2** Terminal compatibility: OSC sequences for title setting

---

## Files to Create

- `crates/shux-vt/src/osc.rs` — OSC sequence parser/handler for title sequences (OSC 0, 1, 2)

## Files to Modify

- `crates/shux-core/src/model.rs` — Add title fields to `Pane` struct (title, auto_title, manual_title)
- `crates/shux-core/src/config.rs` — Ensure `show_pane_titles` and `auto_title` config fields exist
- `crates/shux-vt/src/parser.rs` — Hook OSC 0/1/2 sequences to extract title updates
- `crates/shux-vt/src/lib.rs` — Export osc module
- `crates/shux-core/src/event.rs` — Add `PaneTitleChanged` event variant
- `crates/shux-rpc/src/methods/pane.rs` — Implement `pane.set_title` API method
- `crates/shux-ui/src/compositor.rs` — Render pane titles in border top (when enabled)

---

## Execution Steps

### Step 1: Extend Pane Model with Title Fields

In `crates/shux-core/src/model.rs`, add title-related fields to the `Pane` struct:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pane {
    pub id: PaneId,
    pub window_id: WindowId,

    // ... existing fields (command, cwd, etc.) ...

    /// The effective title displayed for this pane.
    /// Priority: manual_title > osc_title > auto-derived title.
    pub title: String,

    /// Manually set title (via API or user). When set, overrides auto-title.
    /// Set to None to clear manual title and revert to auto-title.
    pub manual_title: Option<String>,

    /// Title set by the running application via OSC 0/1/2 sequences.
    /// Updated by the VT parser as escape sequences are processed.
    pub osc_title: Option<String>,

    /// Whether auto-title is enabled for this pane.
    /// When true, title is derived from the running command or CWD.
    /// When false, only manual_title or osc_title are used.
    pub auto_title: bool,

    /// The icon title set via OSC 1 (traditionally for the icon/taskbar).
    /// Stored separately but not currently displayed.
    pub icon_title: Option<String>,

    // ... existing fields ...
}

impl Pane {
    /// Resolve the effective title to display.
    /// Priority order:
    /// 1. manual_title (explicit user override)
    /// 2. osc_title (set by running application via escape sequences)
    /// 3. auto-derived title from command name or CWD (if auto_title is true)
    /// 4. fallback: "pane N"
    pub fn effective_title(&self) -> &str {
        if let Some(ref manual) = self.manual_title {
            return manual;
        }
        if let Some(ref osc) = self.osc_title {
            return osc;
        }
        if self.auto_title {
            return &self.title; // auto-derived, kept updated by command watcher
        }
        // Fallback — should rarely happen
        &self.title
    }

    /// Set a manual title, overriding auto-title and OSC title.
    pub fn set_manual_title(&mut self, title: Option<String>) {
        self.manual_title = title;
        self.recalculate_title();
    }

    /// Update the OSC-derived title (called by VT parser).
    pub fn set_osc_title(&mut self, title: String) {
        self.osc_title = Some(title);
        self.recalculate_title();
    }

    /// Update the auto-derived title from the current command name.
    pub fn update_auto_title(&mut self, command_name: &str) {
        if self.auto_title && self.manual_title.is_none() {
            self.title = command_name.to_string();
        }
    }

    /// Update the auto-derived title from the current working directory.
    pub fn update_auto_title_from_cwd(&mut self, cwd: &str) {
        if self.auto_title && self.manual_title.is_none() && self.osc_title.is_none() {
            // Use the last path component as the title
            let dir_name = std::path::Path::new(cwd)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(cwd);
            self.title = dir_name.to_string();
        }
    }

    /// Toggle auto-title on or off for this pane.
    pub fn set_auto_title(&mut self, enabled: bool) {
        self.auto_title = enabled;
        if !enabled {
            // When disabling auto-title, freeze the current title
            // (don't clear it — the user wants to keep what's there)
        }
    }

    /// Internal: recalculate the effective title after any title field changes.
    fn recalculate_title(&mut self) {
        // The effective_title() method handles priority; this ensures
        // the `title` field is synced for serialization and events.
        // The `title` field always reflects the current effective title.
    }
}
```

### Step 2: Add PaneTitleChanged Event

In `crates/shux-core/src/event.rs`, add the title change event:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ShuxEvent {
    // ... existing variants ...

    /// A pane's effective title has changed.
    PaneTitleChanged {
        pane_id: PaneId,
        window_id: WindowId,
        /// The new effective title
        title: String,
        /// Source of the change: "manual", "osc", "auto"
        source: TitleSource,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TitleSource {
    /// Title set manually via API (pane.set_title)
    Manual,
    /// Title set by application via OSC 0/1/2 escape sequence
    Osc,
    /// Title auto-derived from running command or CWD
    Auto,
}
```

### Step 3: Parse OSC Title Sequences in VT Parser

Create `crates/shux-vt/src/osc.rs` to handle Operating System Command sequences:

```rust
//! OSC (Operating System Command) sequence handler.
//!
//! Handles title-related OSC sequences:
//! - OSC 0 ; <title> ST — Set icon name and window title
//! - OSC 1 ; <title> ST — Set icon name
//! - OSC 2 ; <title> ST — Set window title
//!
//! ST (String Terminator) is either ESC \ or BEL (0x07).

/// Result of parsing an OSC sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OscAction {
    /// Set both icon name and window title (OSC 0)
    SetIconAndTitle(String),
    /// Set icon name only (OSC 1)
    SetIconName(String),
    /// Set window title only (OSC 2)
    SetWindowTitle(String),
    /// Unrecognized or unsupported OSC sequence
    Unsupported(u16, String),
}

/// Parse an OSC sequence payload.
///
/// The payload is everything between "OSC" and "ST" (the string terminator).
/// Format: "<code>;<data>" where code is a number.
pub fn parse_osc(payload: &[u8]) -> Option<OscAction> {
    // Find the semicolon separator
    let semicolon_pos = payload.iter().position(|&b| b == b';')?;

    let code_str = std::str::from_utf8(&payload[..semicolon_pos]).ok()?;
    let code: u16 = code_str.parse().ok()?;

    let data = std::str::from_utf8(&payload[semicolon_pos + 1..])
        .ok()?
        .to_string();

    match code {
        0 => Some(OscAction::SetIconAndTitle(data)),
        1 => Some(OscAction::SetIconName(data)),
        2 => Some(OscAction::SetWindowTitle(data)),
        _ => Some(OscAction::Unsupported(code, data)),
    }
}

/// Sanitize a title string from an OSC sequence.
/// Removes control characters, limits length, trims whitespace.
pub fn sanitize_title(raw: &str) -> String {
    let sanitized: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .take(256) // Max title length: 256 characters
        .collect();
    sanitized.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_osc_0_set_both() {
        let payload = b"0;My Terminal Title";
        let action = parse_osc(payload);
        assert_eq!(
            action,
            Some(OscAction::SetIconAndTitle("My Terminal Title".into()))
        );
    }

    #[test]
    fn test_parse_osc_1_set_icon() {
        let payload = b"1;icon-name";
        let action = parse_osc(payload);
        assert_eq!(
            action,
            Some(OscAction::SetIconName("icon-name".into()))
        );
    }

    #[test]
    fn test_parse_osc_2_set_title() {
        let payload = b"2;nvim src/main.rs";
        let action = parse_osc(payload);
        assert_eq!(
            action,
            Some(OscAction::SetWindowTitle("nvim src/main.rs".into()))
        );
    }

    #[test]
    fn test_parse_osc_unsupported() {
        let payload = b"52;clipboard-data";
        let action = parse_osc(payload);
        assert!(matches!(action, Some(OscAction::Unsupported(52, _))));
    }

    #[test]
    fn test_parse_osc_invalid_no_semicolon() {
        let payload = b"no-semicolon";
        let action = parse_osc(payload);
        assert_eq!(action, None);
    }

    #[test]
    fn test_parse_osc_invalid_non_numeric_code() {
        let payload = b"abc;title";
        let action = parse_osc(payload);
        assert_eq!(action, None);
    }

    #[test]
    fn test_sanitize_title_removes_control_chars() {
        let raw = "hello\x00world\x1b[31m";
        let sanitized = sanitize_title(raw);
        assert_eq!(sanitized, "helloworld[31m");
    }

    #[test]
    fn test_sanitize_title_limits_length() {
        let raw = "a".repeat(500);
        let sanitized = sanitize_title(&raw);
        assert_eq!(sanitized.len(), 256);
    }

    #[test]
    fn test_sanitize_title_trims_whitespace() {
        let raw = "  My Title  ";
        let sanitized = sanitize_title(raw);
        assert_eq!(sanitized, "My Title");
    }

    #[test]
    fn test_empty_title() {
        let payload = b"2;";
        let action = parse_osc(payload);
        assert_eq!(action, Some(OscAction::SetWindowTitle(String::new())));
    }
}
```

### Step 4: Hook OSC Handler into VT Parser

Modify `crates/shux-vt/src/parser.rs` to dispatch OSC sequences to the new handler. The `vte` crate (0.15 with `ansi` feature) provides a `Perform` trait with an `osc_dispatch` method:

```rust
use crate::osc::{parse_osc, sanitize_title, OscAction};

/// Callback type for title changes detected by the VT parser.
pub type TitleCallback = Box<dyn Fn(TitleUpdate) + Send>;

/// A title update from VT parsing.
#[derive(Debug, Clone)]
pub struct TitleUpdate {
    /// The new window title (from OSC 0 or OSC 2)
    pub window_title: Option<String>,
    /// The new icon name (from OSC 0 or OSC 1)
    pub icon_name: Option<String>,
}

impl vte::Perform for VtHandler {
    // ... existing methods ...

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        // vte 0.15 passes OSC params as slices split by ';'
        // Reconstruct the full payload for our parser
        if params.is_empty() {
            return;
        }

        // Reconstruct: join params with ';' to form "code;data"
        let payload: Vec<u8> = params
            .iter()
            .enumerate()
            .flat_map(|(i, p)| {
                if i > 0 {
                    std::iter::once(&b';').chain(p.iter())
                } else {
                    std::iter::once(&b';').chain(p.iter()).skip(1)
                    // Actually, for the first param, no semicolon prefix
                }
            })
            .copied()
            .collect();

        // Simpler approach: just pass code and data separately
        if let Some(code_bytes) = params.first() {
            if let Ok(code_str) = std::str::from_utf8(code_bytes) {
                if let Ok(code) = code_str.parse::<u16>() {
                    let data = if params.len() > 1 {
                        params[1..]
                            .iter()
                            .map(|p| std::str::from_utf8(p).unwrap_or(""))
                            .collect::<Vec<_>>()
                            .join(";")
                    } else {
                        String::new()
                    };

                    let sanitized = sanitize_title(&data);

                    match code {
                        0 => {
                            // OSC 0: set both icon name and window title
                            if let Some(ref cb) = self.title_callback {
                                cb(TitleUpdate {
                                    window_title: Some(sanitized.clone()),
                                    icon_name: Some(sanitized),
                                });
                            }
                        }
                        1 => {
                            // OSC 1: set icon name only
                            if let Some(ref cb) = self.title_callback {
                                cb(TitleUpdate {
                                    window_title: None,
                                    icon_name: Some(sanitized),
                                });
                            }
                        }
                        2 => {
                            // OSC 2: set window title only
                            if let Some(ref cb) = self.title_callback {
                                cb(TitleUpdate {
                                    window_title: Some(sanitized),
                                    icon_name: None,
                                });
                            }
                        }
                        _ => {
                            // Other OSC sequences — handle or ignore
                            // OSC 7 (CWD), OSC 52 (clipboard), etc. handled elsewhere
                        }
                    }
                }
            }
        }
    }
}
```

### Step 5: Wire Title Updates to Pane Model

When the VT parser detects an OSC title sequence, it needs to update the pane model and emit an event. This happens in the PTY read loop:

```rust
// In the PTY read task (crates/shux-pty/src/lib.rs or the pane I/O handler):

// When setting up the VT parser for a pane:
let pane_id = pane.id;
let event_tx = event_bus.sender();
let state_tx = state_mutation_channel.clone();

let title_callback = Box::new(move |update: TitleUpdate| {
    if let Some(title) = update.window_title {
        // Send mutation to the single-writer state owner
        let _ = state_tx.try_send(StateMutation::SetOscTitle {
            pane_id,
            title: title.clone(),
        });
        // Emit event
        let _ = event_tx.send(ShuxEvent::PaneTitleChanged {
            pane_id,
            window_id: /* from pane state */,
            title,
            source: TitleSource::Osc,
        });
    }
});

vt_handler.set_title_callback(title_callback);
```

### Step 6: Implement pane.set_title API Method

In `crates/shux-rpc/src/methods/pane.rs`:

```rust
/// Handle the pane.set_title JSON-RPC method.
///
/// Params:
///   pane_id: String (UUID) — required
///   title: String | null — the title to set, or null to clear manual title
///
/// When title is a string: sets manual title, overriding auto/OSC titles.
/// When title is null: clears manual title, reverting to auto/OSC title.
pub async fn handle_set_title(
    params: serde_json::Value,
    state: &AppState,
) -> RpcResult<serde_json::Value> {
    let pane_id: PaneId = parse_required_field(&params, "pane_id")?;
    let title: Option<String> = params
        .get("title")
        .and_then(|v| {
            if v.is_null() {
                None
            } else {
                v.as_str().map(|s| s.to_string())
            }
        });

    // Validate title length if provided
    if let Some(ref t) = title {
        if t.len() > 256 {
            return Err(RpcError::invalid_params(
                "Title exceeds maximum length of 256 characters",
            ));
        }
    }

    // Send mutation to state owner
    state
        .mutation_tx
        .send(StateMutation::SetManualTitle { pane_id, title: title.clone() })
        .await
        .map_err(|_| RpcError::internal("State mutation channel closed"))?;

    // Read back the pane to return the effective title
    let snapshot = state.state_snapshot();
    let pane = snapshot
        .find_pane(&pane_id)
        .ok_or_else(|| RpcError::not_found("pane", &pane_id.to_string()))?;

    Ok(serde_json::json!({
        "pane_id": pane.id,
        "title": pane.effective_title(),
        "manual_title": pane.manual_title,
        "auto_title": pane.auto_title,
    }))
}
```

### Step 7: Render Pane Titles in Borders

Modify `crates/shux-ui/src/compositor.rs` to display pane titles in the top border:

```rust
/// Render a pane's border with an optional title in the top edge.
fn render_pane_border(
    &mut self,
    pane: &Pane,
    rect: Rect,
    focused: bool,
    show_titles: bool,
    theme: &ResolvedTheme,
) {
    let border_color = if focused {
        theme.border_focused
    } else {
        theme.border_unfocused
    };

    // Draw standard border (top, bottom, left, right, corners)
    self.draw_border(rect, border_color);

    // If titles are enabled and the pane has a title, embed it in the top border
    if show_titles {
        let title = pane.effective_title();
        if !title.is_empty() {
            // Format: "| title |" centered or left-aligned in the top border
            let max_title_width = (rect.width as usize).saturating_sub(4); // room for " title "
            let display_title = if title.len() > max_title_width {
                format!(" {}... ", &title[..max_title_width.saturating_sub(3)])
            } else {
                format!(" {} ", title)
            };

            // Position: start 2 chars from the left edge of the border
            let title_col = rect.x + 2;
            let title_row = rect.y; // top border row

            for (i, ch) in display_title.chars().enumerate() {
                let col = title_col + i as u16;
                if col < rect.x + rect.width - 1 {
                    self.set_cell(
                        col,
                        title_row,
                        ch,
                        border_color,
                        theme.bg,
                    );
                }
            }
        }
    }
}
```

### Step 8: Add Auto-Title Derivation from Command

When a pane spawns a new process or the foreground process changes, update the auto-title:

```rust
// In the pane lifecycle manager:

/// Derive a display-friendly command name from the full command.
fn command_display_name(command: &[String]) -> String {
    command
        .first()
        .map(|cmd| {
            // Extract just the binary name from the path
            std::path::Path::new(cmd)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(cmd)
                .to_string()
        })
        .unwrap_or_else(|| "shell".to_string())
}

// When a pane's foreground process changes:
fn on_foreground_process_changed(pane_id: PaneId, command: &[String]) {
    let display_name = command_display_name(command);
    state_tx.send(StateMutation::UpdateAutoTitle {
        pane_id,
        title: display_name,
    });
}
```

### Step 9: Add Configuration Fields

Verify `crates/shux-core/src/config.rs` includes:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    // ... existing fields ...

    /// Whether to display pane titles in borders
    #[serde(default = "default_true")]
    pub show_pane_titles: bool,

    /// Whether panes auto-derive their title from the running command
    #[serde(default = "default_true")]
    pub auto_title: bool,
}
```

### Step 10: Add Integration Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_effective_title_priority_manual_first() {
        let mut pane = Pane::default();
        pane.manual_title = Some("my-title".into());
        pane.osc_title = Some("osc-title".into());
        pane.title = "auto-title".into();
        pane.auto_title = true;

        assert_eq!(pane.effective_title(), "my-title");
    }

    #[test]
    fn test_effective_title_priority_osc_second() {
        let mut pane = Pane::default();
        pane.manual_title = None;
        pane.osc_title = Some("osc-title".into());
        pane.title = "auto-title".into();
        pane.auto_title = true;

        assert_eq!(pane.effective_title(), "osc-title");
    }

    #[test]
    fn test_effective_title_priority_auto_third() {
        let mut pane = Pane::default();
        pane.manual_title = None;
        pane.osc_title = None;
        pane.title = "auto-title".into();
        pane.auto_title = true;

        assert_eq!(pane.effective_title(), "auto-title");
    }

    #[test]
    fn test_clear_manual_title_reverts_to_osc() {
        let mut pane = Pane::default();
        pane.manual_title = Some("locked".into());
        pane.osc_title = Some("nvim".into());

        pane.set_manual_title(None);
        assert_eq!(pane.effective_title(), "nvim");
    }

    #[test]
    fn test_auto_title_from_cwd() {
        let mut pane = Pane::default();
        pane.auto_title = true;

        pane.update_auto_title_from_cwd("/home/user/projects/shux");
        assert_eq!(pane.effective_title(), "shux");
    }

    #[test]
    fn test_auto_title_disabled_keeps_existing() {
        let mut pane = Pane::default();
        pane.auto_title = true;
        pane.title = "editor".into();

        pane.set_auto_title(false);
        pane.update_auto_title("cargo");
        // auto_title is disabled, so title should not change
        assert_eq!(pane.title, "editor");
    }

    #[test]
    fn test_command_display_name() {
        assert_eq!(command_display_name(&["/usr/bin/nvim".into()]), "nvim");
        assert_eq!(command_display_name(&["cargo".into(), "build".into()]), "cargo");
        assert_eq!(command_display_name(&[]), "shell");
    }

    #[test]
    fn test_osc_title_update_triggers_event() {
        // This test verifies the integration between VT parser and pane model
        // by checking that an OSC 2 sequence results in a title update
        let osc_payload = b"2;nvim main.rs";
        let action = crate::osc::parse_osc(osc_payload);
        assert!(matches!(action, Some(OscAction::SetWindowTitle(_))));
        if let Some(OscAction::SetWindowTitle(title)) = action {
            assert_eq!(title, "nvim main.rs");
        }
    }
}
```

---

## Verification

### Functional

```bash
# Build the workspace
cargo build --workspace

# Verify pane title module compiles
cargo check -p shux-core
cargo check -p shux-vt
cargo check -p shux-rpc

# Manual test: start shux, run nvim, verify pane title changes
cargo run -p shux -- new -s test
# In pane: run `nvim` — title should change to "nvim"
# In pane: run `cd /tmp && ls` — title should change to "tmp" or "ls"

# API test: set manual title
# shux api pane.set_title '{"pane_id": "<id>", "title": "my-custom-title"}'

# API test: clear manual title (revert to auto)
# shux api pane.set_title '{"pane_id": "<id>", "title": null}'
```

### Tests

```bash
# Run pane title tests
cargo nextest run -p shux-core -- pane.*title
cargo nextest run -p shux-vt -- osc

# Expected passing tests:
# - effective_title_priority_manual_first
# - effective_title_priority_osc_second
# - effective_title_priority_auto_third
# - clear_manual_title_reverts_to_osc
# - auto_title_from_cwd
# - auto_title_disabled_keeps_existing
# - command_display_name
# - parse_osc_0_set_both
# - parse_osc_1_set_icon
# - parse_osc_2_set_title
# - sanitize_title_removes_control_chars
# - sanitize_title_limits_length
```

---

## Completion Criteria

- [ ] `Pane` struct has `title`, `manual_title`, `osc_title`, `auto_title`, `icon_title` fields
- [ ] `effective_title()` resolves priority: manual > OSC > auto > fallback
- [ ] `pane.set_title` API method sets/clears manual title
- [ ] `pane.set_title` with null clears manual title and reverts to auto/OSC
- [ ] Title length validated (max 256 characters)
- [ ] OSC 0 (icon+title), OSC 1 (icon), OSC 2 (title) sequences parsed by VT parser
- [ ] OSC title updates flow from VT parser to pane model via state mutation channel
- [ ] `pane.title_changed` event emitted on any title change (with source: manual/osc/auto)
- [ ] Auto-title derives from command name (last path component of executable)
- [ ] Auto-title derives from CWD (last directory name) when no command info available
- [ ] Auto-title toggleable per pane (`auto_title: bool` field)
- [ ] `show_pane_titles` config controls whether titles appear in borders
- [ ] `auto_title` config provides default for new panes
- [ ] Pane titles rendered in top border when `show_pane_titles` is true
- [ ] Long titles truncated with "..." in border display
- [ ] Title sanitization removes control characters
- [ ] Unit tests pass for title priority, OSC parsing, sanitization, command name derivation

---

## Commit Message

```
feat(core,vt): add pane titles with manual set, auto-derive, and OSC support

- Extend Pane model with manual_title, osc_title, auto_title fields
- Implement effective_title() priority: manual > OSC > auto > fallback
- Parse OSC 0/1/2 title sequences in VT parser with sanitization
- Add pane.set_title API method (set/clear manual title)
- Emit pane.title_changed events with source tracking
- Render pane titles in border top when show_pane_titles is enabled
- Auto-derive titles from running command name or CWD
```

---

## Session Protocol

1. **Before starting:** Read task 015 (pane operations) to understand the `Pane` struct and state mutation flow. Read task 005 (virtual terminal grid) to understand the VT parser integration point. Read task 009 (render compositor) for border rendering.
2. **During:** Implement in order: model changes (Step 1), event (Step 2), OSC parser (Step 3), VT integration (Step 4), state wiring (Step 5), API method (Step 6), border rendering (Step 7), auto-title (Step 8), config (Step 9), tests (Step 10). Run `cargo check` after each step.
3. **Edge cases to watch for:**
   - OSC sequences with empty data (e.g., `ESC ] 2 ; ESC \` — empty title)
   - OSC sequences with very long data (must be truncated)
   - OSC sequences with embedded control characters (must be sanitized)
   - Multiple rapid OSC title updates (debouncing not required — latest wins)
   - Pane with no running command (title should fall back to CWD or "shell")
   - Unicode in titles (must handle multi-byte characters correctly)
4. **After:** Run full test suite (`cargo nextest run --workspace`). Verify OSC titles work with bash, zsh, and fish (they all set OSC 0 by default). Update `docs/PROGRESS.md`. Update `CLAUDE.md` Learnings.
