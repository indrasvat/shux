# 031 — Keybinding Configuration and Conflict Detection

**Status:** Pending
**Depends On:** 019, 022
**Parallelizable With:** 030

---

## Problem

shux ships with sensible default keybindings (Tier 1 bare keys and Tier 2 prefix keys, defined in tasks 018 and 019), but power users must be able to customize them. A vim user might remap `Alt+h/j/k/l` to different actions, or a user switching from tmux might want `Ctrl+b` as the prefix instead of `Ctrl+Space`.

The keybinding system must support: (1) user overrides in config.toml, (2) conflict detection between user, built-in, and plugin bindings, (3) reserved key validation (certain critical keys cannot be unbound), (4) runtime API for querying and modifying bindings, and (5) a clear provenance system showing where each binding comes from (built-in, user, plugin).

This is a prerequisite for the command palette (task 032) and help overlay (task 033), which need to display current keybindings with their sources.

## PRD Reference

- **section 9.4** Customization: keybindings remappable in TOML + plugin conflict detection
- **section 10.2** Config reference: `[keybindings]` section with crossterm notation
- **section 8.2** API methods: `keybinding.list`, `keybinding.set`, `keybinding.reset`
- **section 6.1** P0 feature matrix: graded keybindings discoverable via command palette

---

## Files to Create

- `crates/shux-core/src/keybinding.rs` — Keybinding registry, conflict detection, reserved keys
- `crates/shux-rpc/src/methods/keybinding.rs` — API methods: keybinding.list, keybinding.set, keybinding.reset

## Files to Modify

- `crates/shux-core/src/lib.rs` — Add `pub mod keybinding;`
- `crates/shux-core/src/config.rs` — Parse [keybindings] section from config.toml
- `crates/shux-rpc/src/methods/mod.rs` — Register keybinding API methods
- `crates/shux-ui/src/input.rs` — Use keybinding registry to resolve key events to actions

---

## Execution Steps

### Step 1: Define Key Notation and Parsing

crossterm uses a specific notation for key events. The config uses a string-based notation that must be parsed into crossterm `KeyEvent` structures:

```rust
//! Keybinding configuration and conflict detection.
//!
//! Supports crossterm notation for key combos:
//! - Simple keys: "a", "enter", "esc", "tab", "space", "backspace"
//! - Modifier keys: "ctrl-c", "alt-h", "shift-tab", "ctrl-shift-a"
//! - Function keys: "f1", "f12"
//! - Special: "up", "down", "left", "right", "home", "end", "pageup", "pagedown"
//! - Sequences (prefix): "ctrl-space c" (space-separated for multi-key)

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum KeybindingError {
    #[error("Invalid key notation: '{0}'")]
    InvalidNotation(String),

    #[error("Cannot unbind reserved key: '{key}' (action: {action})")]
    ReservedKey { key: String, action: String },

    #[error("Keybinding conflict: '{key}' is already bound to '{existing_action}' (source: {existing_source}). New binding: '{new_action}' (source: {new_source})")]
    Conflict {
        key: String,
        existing_action: String,
        existing_source: String,
        new_action: String,
        new_source: String,
    },

    #[error("Unknown action: '{0}'")]
    UnknownAction(String),
}

/// A parsed key combination (single key with modifiers).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KeyCombo {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl KeyCombo {
    pub fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }

    /// Parse a key notation string like "ctrl-c", "alt-h", "f1", "enter".
    pub fn parse(notation: &str) -> Result<Self, KeybindingError> {
        let notation = notation.trim().to_lowercase();
        let parts: Vec<&str> = notation.split('-').collect();

        let mut modifiers = KeyModifiers::empty();
        let key_part;

        if parts.len() == 1 {
            key_part = parts[0];
        } else {
            // Parse modifiers
            for part in &parts[..parts.len() - 1] {
                match *part {
                    "ctrl" | "c" => modifiers |= KeyModifiers::CONTROL,
                    "alt" | "meta" | "m" => modifiers |= KeyModifiers::ALT,
                    "shift" | "s" => modifiers |= KeyModifiers::SHIFT,
                    "super" => modifiers |= KeyModifiers::SUPER,
                    _ => {
                        return Err(KeybindingError::InvalidNotation(notation.clone()));
                    }
                }
            }
            key_part = parts[parts.len() - 1];
        }

        let code = match key_part {
            "space" => KeyCode::Char(' '),
            "enter" | "return" | "cr" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Esc,
            "tab" => KeyCode::Tab,
            "backspace" | "bs" => KeyCode::Backspace,
            "delete" | "del" => KeyCode::Delete,
            "insert" | "ins" => KeyCode::Insert,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" | "pgup" => KeyCode::PageUp,
            "pagedown" | "pgdn" => KeyCode::PageDown,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            s if s.starts_with('f') && s.len() > 1 => {
                let num: u8 = s[1..]
                    .parse()
                    .map_err(|_| KeybindingError::InvalidNotation(notation.clone()))?;
                KeyCode::F(num)
            }
            s if s.len() == 1 => {
                let ch = s.chars().next().unwrap();
                if modifiers.contains(KeyModifiers::SHIFT) && ch.is_ascii_lowercase() {
                    KeyCode::Char(ch.to_ascii_uppercase())
                } else {
                    KeyCode::Char(ch)
                }
            }
            "|" => KeyCode::Char('|'),
            "\\" => KeyCode::Char('\\'),
            "[" => KeyCode::Char('['),
            "]" => KeyCode::Char(']'),
            _ => return Err(KeybindingError::InvalidNotation(notation.clone())),
        };

        Ok(KeyCombo { code, modifiers })
    }
}

impl fmt::Display for KeyCombo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = Vec::new();
        if self.modifiers.contains(KeyModifiers::CONTROL) {
            parts.push("Ctrl");
        }
        if self.modifiers.contains(KeyModifiers::ALT) {
            parts.push("Alt");
        }
        if self.modifiers.contains(KeyModifiers::SHIFT) {
            parts.push("Shift");
        }

        let key_name = match self.code {
            KeyCode::Char(' ') => "Space".to_string(),
            KeyCode::Char(c) => {
                if c.is_uppercase() || parts.is_empty() {
                    c.to_string()
                } else {
                    c.to_uppercase().to_string()
                }
            }
            KeyCode::Enter => "Enter".to_string(),
            KeyCode::Esc => "Esc".to_string(),
            KeyCode::Tab => "Tab".to_string(),
            KeyCode::Backspace => "Backspace".to_string(),
            KeyCode::Delete => "Del".to_string(),
            KeyCode::F(n) => format!("F{n}"),
            KeyCode::Up => "Up".to_string(),
            KeyCode::Down => "Down".to_string(),
            KeyCode::Left => "Left".to_string(),
            KeyCode::Right => "Right".to_string(),
            KeyCode::Home => "Home".to_string(),
            KeyCode::End => "End".to_string(),
            KeyCode::PageUp => "PgUp".to_string(),
            KeyCode::PageDown => "PgDn".to_string(),
            _ => "?".to_string(),
        };

        parts.push(&key_name);
        write!(f, "{}", parts.join("+"))
    }
}
```

### Step 2: Define Keybinding Sequences and the Binding Registry

```rust
/// A keybinding sequence (single key or prefix + key).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KeySequence {
    /// Single key combo (e.g., Alt+h)
    Single(KeyCombo),
    /// Prefix sequence: first key activates prefix mode, second key executes action
    /// (e.g., Ctrl+Space followed by c)
    Prefix(KeyCombo, KeyCombo),
}

impl KeySequence {
    /// Parse a key sequence notation.
    /// Single key: "alt-h"
    /// Prefix sequence: "ctrl-space c" (space-separated)
    pub fn parse(notation: &str) -> Result<Self, KeybindingError> {
        let parts: Vec<&str> = notation.trim().split_whitespace().collect();
        match parts.len() {
            1 => Ok(KeySequence::Single(KeyCombo::parse(parts[0])?)),
            2 => Ok(KeySequence::Prefix(
                KeyCombo::parse(parts[0])?,
                KeyCombo::parse(parts[1])?,
            )),
            _ => Err(KeybindingError::InvalidNotation(notation.to_string())),
        }
    }
}

impl fmt::Display for KeySequence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeySequence::Single(k) => write!(f, "{k}"),
            KeySequence::Prefix(p, k) => write!(f, "{p} {k}"),
        }
    }
}

/// Where a keybinding comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BindingSource {
    /// Built-in default binding
    BuiltIn,
    /// User configuration (config.toml [keybindings])
    User,
    /// Plugin-registered binding
    Plugin(String), // plugin ID
    /// Runtime override (via API keybinding.set)
    Runtime,
}

impl fmt::Display for BindingSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BindingSource::BuiltIn => write!(f, "built-in"),
            BindingSource::User => write!(f, "user"),
            BindingSource::Plugin(id) => write!(f, "plugin:{id}"),
            BindingSource::Runtime => write!(f, "runtime"),
        }
    }
}

/// A keybinding category for organization in help overlay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BindingCategory {
    Navigation,
    Windows,
    Panes,
    Copy,
    Config,
    Session,
    General,
    Plugin,
}

/// A single keybinding entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Binding {
    /// The key sequence that triggers this binding
    pub key: KeySequence,
    /// The action to execute
    pub action: String,
    /// Human-readable description
    pub description: String,
    /// Where this binding comes from
    pub source: BindingSource,
    /// Category for organization in help
    pub category: BindingCategory,
    /// Whether this is a reserved binding that cannot be unbound
    pub reserved: bool,
}

/// The keybinding registry.
///
/// Manages all active keybindings with conflict detection and
/// provenance tracking.
pub struct KeybindingRegistry {
    /// All active bindings, indexed by key sequence
    bindings: HashMap<KeySequence, Binding>,
    /// Built-in default bindings (kept for reset)
    defaults: HashMap<KeySequence, Binding>,
    /// Conflicts detected during loading (warnings, not errors)
    conflicts: Vec<KeybindingConflict>,
}

#[derive(Debug, Clone)]
pub struct KeybindingConflict {
    pub key: KeySequence,
    pub existing: Binding,
    pub attempted: Binding,
    pub resolved: ConflictResolution,
}

#[derive(Debug, Clone)]
pub enum ConflictResolution {
    /// The new binding replaced the existing one
    Replaced,
    /// The new binding was rejected (e.g., reserved key)
    Rejected,
}
```

### Step 3: Implement Built-in Defaults and Reserved Keys

```rust
impl KeybindingRegistry {
    /// Create a new registry with built-in defaults.
    pub fn new() -> Self {
        let mut registry = Self {
            bindings: HashMap::new(),
            defaults: HashMap::new(),
            conflicts: Vec::new(),
        };
        registry.load_defaults();
        registry
    }

    /// Load built-in default keybindings (Tier 1 and Tier 2).
    fn load_defaults(&mut self) {
        let defaults = vec![
            // Tier 1: Bare keys (no prefix)
            Binding {
                key: KeySequence::parse("alt-h").unwrap(),
                action: "pane.focus-left".into(),
                description: "Focus pane to the left".into(),
                source: BindingSource::BuiltIn,
                category: BindingCategory::Navigation,
                reserved: false,
            },
            Binding {
                key: KeySequence::parse("alt-j").unwrap(),
                action: "pane.focus-down".into(),
                description: "Focus pane below".into(),
                source: BindingSource::BuiltIn,
                category: BindingCategory::Navigation,
                reserved: false,
            },
            Binding {
                key: KeySequence::parse("alt-k").unwrap(),
                action: "pane.focus-up".into(),
                description: "Focus pane above".into(),
                source: BindingSource::BuiltIn,
                category: BindingCategory::Navigation,
                reserved: false,
            },
            Binding {
                key: KeySequence::parse("alt-l").unwrap(),
                action: "pane.focus-right".into(),
                description: "Focus pane to the right".into(),
                source: BindingSource::BuiltIn,
                category: BindingCategory::Navigation,
                reserved: false,
            },
            Binding {
                key: KeySequence::parse("alt-shift-h").unwrap(),
                action: "pane.resize-left".into(),
                description: "Resize pane left".into(),
                source: BindingSource::BuiltIn,
                category: BindingCategory::Panes,
                reserved: false,
            },
            Binding {
                key: KeySequence::parse("alt-shift-j").unwrap(),
                action: "pane.resize-down".into(),
                description: "Resize pane down".into(),
                source: BindingSource::BuiltIn,
                category: BindingCategory::Panes,
                reserved: false,
            },
            Binding {
                key: KeySequence::parse("alt-shift-k").unwrap(),
                action: "pane.resize-up".into(),
                description: "Resize pane up".into(),
                source: BindingSource::BuiltIn,
                category: BindingCategory::Panes,
                reserved: false,
            },
            Binding {
                key: KeySequence::parse("alt-shift-l").unwrap(),
                action: "pane.resize-right".into(),
                description: "Resize pane right".into(),
                source: BindingSource::BuiltIn,
                category: BindingCategory::Panes,
                reserved: false,
            },
            Binding {
                key: KeySequence::parse("alt-n").unwrap(),
                action: "window.next".into(),
                description: "Next window".into(),
                source: BindingSource::BuiltIn,
                category: BindingCategory::Windows,
                reserved: false,
            },
            Binding {
                key: KeySequence::parse("alt-p").unwrap(),
                action: "window.previous".into(),
                description: "Previous window".into(),
                source: BindingSource::BuiltIn,
                category: BindingCategory::Windows,
                reserved: false,
            },
            Binding {
                key: KeySequence::parse("alt-z").unwrap(),
                action: "pane.zoom-toggle".into(),
                description: "Toggle pane zoom".into(),
                source: BindingSource::BuiltIn,
                category: BindingCategory::Panes,
                reserved: false,
            },
            Binding {
                key: KeySequence::parse("alt-enter").unwrap(),
                action: "pane.smart-split".into(),
                description: "Split pane (smart direction)".into(),
                source: BindingSource::BuiltIn,
                category: BindingCategory::Panes,
                reserved: false,
            },

            // Tier 2: Prefix keys (Ctrl+Space + key)
            Binding {
                key: KeySequence::parse("ctrl-space c").unwrap(),
                action: "window.create".into(),
                description: "Create new window".into(),
                source: BindingSource::BuiltIn,
                category: BindingCategory::Windows,
                reserved: false,
            },
            Binding {
                key: KeySequence::parse("ctrl-space x").unwrap(),
                action: "pane.close".into(),
                description: "Close current pane".into(),
                source: BindingSource::BuiltIn,
                category: BindingCategory::Panes,
                reserved: false,
            },
            Binding {
                key: KeySequence::parse("ctrl-space |").unwrap(),
                action: "pane.split-vertical".into(),
                description: "Split pane vertically".into(),
                source: BindingSource::BuiltIn,
                category: BindingCategory::Panes,
                reserved: false,
            },
            Binding {
                key: KeySequence::parse("ctrl-space -").unwrap(),
                action: "pane.split-horizontal".into(),
                description: "Split pane horizontally".into(),
                source: BindingSource::BuiltIn,
                category: BindingCategory::Panes,
                reserved: false,
            },
            Binding {
                key: KeySequence::parse("ctrl-space d").unwrap(),
                action: "session.detach".into(),
                description: "Detach from session".into(),
                source: BindingSource::BuiltIn,
                category: BindingCategory::Session,
                reserved: false,
            },
            Binding {
                key: KeySequence::parse("ctrl-space [").unwrap(),
                action: "copy.enter".into(),
                description: "Enter copy mode".into(),
                source: BindingSource::BuiltIn,
                category: BindingCategory::Copy,
                reserved: false,
            },
            Binding {
                key: KeySequence::parse("ctrl-space :").unwrap(),
                action: "command-palette.open".into(),
                description: "Open command palette".into(),
                source: BindingSource::BuiltIn,
                category: BindingCategory::General,
                reserved: true, // Reserved — always accessible
            },
            Binding {
                key: KeySequence::parse("ctrl-space ?").unwrap(),
                action: "help.open".into(),
                description: "Open keybinding help".into(),
                source: BindingSource::BuiltIn,
                category: BindingCategory::General,
                reserved: true, // Reserved — always accessible
            },
            // ... additional Tier 2 bindings ...
        ];

        for binding in defaults {
            self.defaults.insert(binding.key.clone(), binding.clone());
            self.bindings.insert(binding.key.clone(), binding);
        }

        // Alt+1..9 for window switching
        for i in 1..=9u8 {
            let key_str = format!("alt-{i}");
            let binding = Binding {
                key: KeySequence::parse(&key_str).unwrap(),
                action: format!("window.focus-{i}"),
                description: format!("Switch to window {i}"),
                source: BindingSource::BuiltIn,
                category: BindingCategory::Windows,
                reserved: false,
            };
            self.defaults.insert(binding.key.clone(), binding.clone());
            self.bindings.insert(binding.key.clone(), binding);
        }
    }

    /// Reserved keys that cannot be unbound.
    fn reserved_keys() -> Vec<&'static str> {
        vec![
            "ctrl-space :", // Command palette — always accessible
            "ctrl-space ?", // Help overlay — always accessible
        ]
    }
}
```

### Step 4: Implement User Override Loading and Conflict Detection

```rust
impl KeybindingRegistry {
    /// Load user keybinding overrides from config.
    ///
    /// Format in config.toml:
    /// ```toml
    /// [keybindings]
    /// "alt-h" = "focus-left"
    /// "ctrl-space c" = "window.create"
    /// ```
    pub fn load_user_overrides(
        &mut self,
        overrides: &HashMap<String, String>,
    ) -> Vec<KeybindingConflict> {
        let mut conflicts = Vec::new();

        for (key_notation, action) in overrides {
            match self.set_binding(
                key_notation,
                action,
                BindingSource::User,
            ) {
                Ok(()) => {}
                Err(KeybindingError::ReservedKey { key, action }) => {
                    tracing::warn!(key = %key, action = %action, "Cannot override reserved keybinding");
                }
                Err(KeybindingError::Conflict { key, existing_action, .. }) => {
                    tracing::info!(
                        key = %key,
                        overriding = %existing_action,
                        new_action = %action,
                        "User keybinding overrides built-in"
                    );
                    // User overrides are allowed — this is informational
                }
                Err(e) => {
                    tracing::error!(error = %e, "Failed to load keybinding");
                }
            }
        }

        conflicts
    }

    /// Set a keybinding. Handles conflict detection and reserved key validation.
    pub fn set_binding(
        &mut self,
        key_notation: &str,
        action: &str,
        source: BindingSource,
    ) -> Result<(), KeybindingError> {
        let key_seq = KeySequence::parse(key_notation)?;

        // Check if this is a reserved key being unbound
        if let Some(existing) = self.bindings.get(&key_seq) {
            if existing.reserved && source != BindingSource::BuiltIn {
                return Err(KeybindingError::ReservedKey {
                    key: key_notation.to_string(),
                    action: existing.action.clone(),
                });
            }
        }

        // Check for conflicts
        if let Some(existing) = self.bindings.get(&key_seq) {
            if existing.source != source {
                let conflict = KeybindingConflict {
                    key: key_seq.clone(),
                    existing: existing.clone(),
                    attempted: Binding {
                        key: key_seq.clone(),
                        action: action.to_string(),
                        description: String::new(),
                        source: source.clone(),
                        category: BindingCategory::General,
                        reserved: false,
                    },
                    resolved: if matches!(source, BindingSource::User | BindingSource::Runtime) {
                        ConflictResolution::Replaced
                    } else {
                        ConflictResolution::Rejected
                    },
                };

                // User and runtime overrides win; plugin bindings are rejected on conflict
                match &source {
                    BindingSource::User | BindingSource::Runtime => {
                        self.conflicts.push(conflict);
                        // Fall through to set the binding
                    }
                    BindingSource::Plugin(plugin_id) => {
                        self.conflicts.push(conflict);
                        return Err(KeybindingError::Conflict {
                            key: key_notation.to_string(),
                            existing_action: existing.action.clone(),
                            existing_source: existing.source.to_string(),
                            new_action: action.to_string(),
                            new_source: format!("plugin:{plugin_id}"),
                        });
                    }
                    BindingSource::BuiltIn => {} // Should not happen
                }
            }
        }

        // Set the binding
        let binding = Binding {
            key: key_seq.clone(),
            action: action.to_string(),
            description: self.describe_action(action),
            source,
            category: self.categorize_action(action),
            reserved: false,
        };
        self.bindings.insert(key_seq, binding);

        Ok(())
    }

    /// Reset a keybinding to its default.
    pub fn reset_binding(&mut self, key_notation: &str) -> Result<(), KeybindingError> {
        let key_seq = KeySequence::parse(key_notation)?;

        if let Some(default) = self.defaults.get(&key_seq) {
            self.bindings.insert(key_seq, default.clone());
            Ok(())
        } else {
            // No default — just remove the binding
            self.bindings.remove(&key_seq);
            Ok(())
        }
    }

    /// Look up the action for a key event.
    pub fn resolve(&self, key: &KeySequence) -> Option<&Binding> {
        self.bindings.get(key)
    }

    /// Get all current bindings, sorted by category and key.
    pub fn list_all(&self) -> Vec<&Binding> {
        let mut bindings: Vec<&Binding> = self.bindings.values().collect();
        bindings.sort_by(|a, b| {
            a.category
                .cmp(&b.category)
                .then_with(|| a.key.to_string().cmp(&b.key.to_string()))
        });
        bindings
    }

    /// Get bindings by category.
    pub fn list_by_category(&self, category: &BindingCategory) -> Vec<&Binding> {
        self.bindings
            .values()
            .filter(|b| &b.category == category)
            .collect()
    }

    /// Get all detected conflicts.
    pub fn conflicts(&self) -> &[KeybindingConflict] {
        &self.conflicts
    }

    fn describe_action(&self, action: &str) -> String {
        // Map action names to descriptions
        match action {
            "pane.focus-left" => "Focus pane to the left".into(),
            "pane.focus-right" => "Focus pane to the right".into(),
            "pane.focus-up" => "Focus pane above".into(),
            "pane.focus-down" => "Focus pane below".into(),
            "window.create" => "Create new window".into(),
            "window.next" => "Next window".into(),
            "window.previous" => "Previous window".into(),
            "command-palette.open" => "Open command palette".into(),
            "help.open" => "Open keybinding help".into(),
            _ => action.replace('.', " ").replace('-', " "),
        }
    }

    fn categorize_action(&self, action: &str) -> BindingCategory {
        if action.starts_with("pane.focus") || action.starts_with("window.focus") {
            BindingCategory::Navigation
        } else if action.starts_with("pane.") {
            BindingCategory::Panes
        } else if action.starts_with("window.") {
            BindingCategory::Windows
        } else if action.starts_with("copy.") {
            BindingCategory::Copy
        } else if action.starts_with("session.") {
            BindingCategory::Session
        } else if action.starts_with("config.") {
            BindingCategory::Config
        } else {
            BindingCategory::General
        }
    }
}
```

### Step 5: Implement API Methods

Create `crates/shux-rpc/src/methods/keybinding.rs`:

```rust
//! JSON-RPC methods for keybinding management.
//!
//! Methods:
//! - keybinding.list — List all current keybindings with source
//! - keybinding.set — Override a keybinding at runtime
//! - keybinding.reset — Reset a keybinding to its default

use serde_json::Value;

/// keybinding.list
///
/// Params (optional):
///   category: String — filter by category ("navigation", "windows", etc.)
///   source: String — filter by source ("built-in", "user", "plugin", "runtime")
///
/// Returns: Array of binding objects with key, action, description, source, category
pub async fn handle_list(
    params: Value,
    state: &AppState,
) -> RpcResult<Value> {
    let registry = state.keybinding_registry();

    let bindings = registry.list_all();

    // Apply filters if provided
    let category_filter = params
        .get("category")
        .and_then(|v| v.as_str());
    let source_filter = params
        .get("source")
        .and_then(|v| v.as_str());

    let filtered: Vec<Value> = bindings
        .iter()
        .filter(|b| {
            if let Some(cat) = category_filter {
                format!("{:?}", b.category).to_lowercase() != cat.to_lowercase() {
                    return false;
                }
            }
            if let Some(src) = source_filter {
                if b.source.to_string() != src {
                    return false;
                }
            }
            true
        })
        .map(|b| {
            serde_json::json!({
                "key": b.key.to_string(),
                "action": b.action,
                "description": b.description,
                "source": b.source.to_string(),
                "category": format!("{:?}", b.category).to_lowercase(),
                "reserved": b.reserved,
            })
        })
        .collect();

    Ok(serde_json::json!({ "bindings": filtered }))
}

/// keybinding.set
///
/// Params:
///   key: String — key notation (e.g., "alt-h", "ctrl-space c")
///   action: String — action name (e.g., "pane.focus-left")
pub async fn handle_set(
    params: Value,
    state: &AppState,
) -> RpcResult<Value> {
    let key = params
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("Missing 'key' parameter"))?;
    let action = params
        .get("action")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("Missing 'action' parameter"))?;

    let mut registry = state.keybinding_registry_mut();
    registry
        .set_binding(key, action, BindingSource::Runtime)
        .map_err(|e| RpcError::invalid_params(&e.to_string()))?;

    Ok(serde_json::json!({
        "key": key,
        "action": action,
        "source": "runtime",
    }))
}

/// keybinding.reset
///
/// Params:
///   key: String — key notation to reset to default
pub async fn handle_reset(
    params: Value,
    state: &AppState,
) -> RpcResult<Value> {
    let key = params
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("Missing 'key' parameter"))?;

    let mut registry = state.keybinding_registry_mut();
    registry
        .reset_binding(key)
        .map_err(|e| RpcError::invalid_params(&e.to_string()))?;

    // Return the current binding (may be default or none)
    let key_seq = KeySequence::parse(key)
        .map_err(|e| RpcError::invalid_params(&e.to_string()))?;

    if let Some(binding) = registry.resolve(&key_seq) {
        Ok(serde_json::json!({
            "key": key,
            "action": binding.action,
            "source": binding.source.to_string(),
            "status": "reset",
        }))
    } else {
        Ok(serde_json::json!({
            "key": key,
            "status": "unbound",
        }))
    }
}
```

### Step 6: Add Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_key() {
        let combo = KeyCombo::parse("a").unwrap();
        assert_eq!(combo.code, KeyCode::Char('a'));
        assert!(combo.modifiers.is_empty());
    }

    #[test]
    fn test_parse_ctrl_key() {
        let combo = KeyCombo::parse("ctrl-c").unwrap();
        assert_eq!(combo.code, KeyCode::Char('c'));
        assert!(combo.modifiers.contains(KeyModifiers::CONTROL));
    }

    #[test]
    fn test_parse_alt_key() {
        let combo = KeyCombo::parse("alt-h").unwrap();
        assert_eq!(combo.code, KeyCode::Char('h'));
        assert!(combo.modifiers.contains(KeyModifiers::ALT));
    }

    #[test]
    fn test_parse_ctrl_space() {
        let combo = KeyCombo::parse("ctrl-space").unwrap();
        assert_eq!(combo.code, KeyCode::Char(' '));
        assert!(combo.modifiers.contains(KeyModifiers::CONTROL));
    }

    #[test]
    fn test_parse_function_key() {
        let combo = KeyCombo::parse("f12").unwrap();
        assert_eq!(combo.code, KeyCode::F(12));
    }

    #[test]
    fn test_parse_sequence_prefix() {
        let seq = KeySequence::parse("ctrl-space c").unwrap();
        match seq {
            KeySequence::Prefix(prefix, key) => {
                assert_eq!(prefix.code, KeyCode::Char(' '));
                assert!(prefix.modifiers.contains(KeyModifiers::CONTROL));
                assert_eq!(key.code, KeyCode::Char('c'));
            }
            _ => panic!("Expected prefix sequence"),
        }
    }

    #[test]
    fn test_parse_invalid_notation() {
        assert!(KeyCombo::parse("ctrl-alt-shift-extra-x").is_err());
    }

    #[test]
    fn test_display_key_combo() {
        let combo = KeyCombo::parse("ctrl-space").unwrap();
        assert_eq!(combo.to_string(), "Ctrl+Space");

        let combo = KeyCombo::parse("alt-h").unwrap();
        assert_eq!(combo.to_string(), "Alt+H");
    }

    #[test]
    fn test_registry_defaults_loaded() {
        let registry = KeybindingRegistry::new();
        let bindings = registry.list_all();
        assert!(!bindings.is_empty(), "Should have built-in bindings");

        // Check a specific default exists
        let alt_h = KeySequence::parse("alt-h").unwrap();
        let binding = registry.resolve(&alt_h);
        assert!(binding.is_some());
        assert_eq!(binding.unwrap().action, "pane.focus-left");
    }

    #[test]
    fn test_user_override() {
        let mut registry = KeybindingRegistry::new();

        // Override alt-h to do something else
        registry
            .set_binding("alt-h", "custom.action", BindingSource::User)
            .unwrap();

        let alt_h = KeySequence::parse("alt-h").unwrap();
        let binding = registry.resolve(&alt_h).unwrap();
        assert_eq!(binding.action, "custom.action");
        assert_eq!(binding.source, BindingSource::User);
    }

    #[test]
    fn test_reserved_key_cannot_be_overridden() {
        let mut registry = KeybindingRegistry::new();

        let result = registry.set_binding(
            "ctrl-space :",
            "custom.action",
            BindingSource::User,
        );
        assert!(matches!(result, Err(KeybindingError::ReservedKey { .. })));
    }

    #[test]
    fn test_plugin_conflict_rejected() {
        let mut registry = KeybindingRegistry::new();

        let result = registry.set_binding(
            "alt-h",
            "plugin.action",
            BindingSource::Plugin("my-plugin".into()),
        );
        assert!(matches!(result, Err(KeybindingError::Conflict { .. })));
    }

    #[test]
    fn test_reset_to_default() {
        let mut registry = KeybindingRegistry::new();

        // Override
        registry
            .set_binding("alt-h", "custom.action", BindingSource::User)
            .unwrap();

        // Reset
        registry.reset_binding("alt-h").unwrap();

        let alt_h = KeySequence::parse("alt-h").unwrap();
        let binding = registry.resolve(&alt_h).unwrap();
        assert_eq!(binding.action, "pane.focus-left"); // Back to default
        assert_eq!(binding.source, BindingSource::BuiltIn);
    }

    #[test]
    fn test_list_by_category() {
        let registry = KeybindingRegistry::new();
        let nav_bindings = registry.list_by_category(&BindingCategory::Navigation);
        assert!(!nav_bindings.is_empty());
        assert!(nav_bindings.iter().all(|b| b.category == BindingCategory::Navigation));
    }

    #[test]
    fn test_conflict_tracking() {
        let mut registry = KeybindingRegistry::new();

        registry
            .set_binding("alt-h", "custom.action", BindingSource::User)
            .unwrap();

        let conflicts = registry.conflicts();
        assert_eq!(conflicts.len(), 1);
        assert!(matches!(conflicts[0].resolved, ConflictResolution::Replaced));
    }
}
```

---

## Verification

### Functional

```bash
# Build the workspace
cargo build --workspace

# Verify keybinding module compiles
cargo check -p shux-core
cargo check -p shux-rpc

# List all keybindings via API
shux api keybinding.list --format json | jq '.bindings | length'
# Expected: 20+ bindings

# Override a keybinding
shux api keybinding.set '{"key": "alt-h", "action": "custom.action"}'
# Expected: success with source "runtime"

# Reset a keybinding
shux api keybinding.reset '{"key": "alt-h"}'
# Expected: reset to "pane.focus-left" with source "built-in"

# Try to override reserved key
shux api keybinding.set '{"key": "ctrl-space :", "action": "noop"}'
# Expected: error about reserved key
```

### Tests

```bash
# Run keybinding tests
cargo nextest run -p shux-core -- keybinding

# Expected passing tests:
# - test_parse_simple_key
# - test_parse_ctrl_key
# - test_parse_alt_key
# - test_parse_ctrl_space
# - test_parse_function_key
# - test_parse_sequence_prefix
# - test_parse_invalid_notation
# - test_display_key_combo
# - test_registry_defaults_loaded
# - test_user_override
# - test_reserved_key_cannot_be_overridden
# - test_plugin_conflict_rejected
# - test_reset_to_default
# - test_list_by_category
# - test_conflict_tracking
```

---

## Completion Criteria

- [ ] Key notation parsing: simple keys, modifiers (ctrl, alt, shift), function keys, special keys
- [ ] Key sequence support: single keys and prefix sequences (e.g., "ctrl-space c")
- [ ] KeyCombo Display impl produces human-readable format (e.g., "Ctrl+Space")
- [ ] KeybindingRegistry loaded with all built-in Tier 1 and Tier 2 defaults
- [ ] User overrides from `[keybindings]` config section loaded and applied
- [ ] Reserved key validation: command palette and help overlay cannot be unbound
- [ ] Conflict detection: user overrides built-in (allowed with warning), plugin overrides built-in (rejected)
- [ ] Provenance tracking: each binding tagged with source (built-in/user/plugin/runtime)
- [ ] Category organization: Navigation, Windows, Panes, Copy, Config, Session, General, Plugin
- [ ] `keybinding.list` API returns all bindings with filtering by category and source
- [ ] `keybinding.set` API overrides a binding at runtime
- [ ] `keybinding.reset` API resets a binding to its built-in default
- [ ] Input handler uses registry to resolve key events to actions
- [ ] Alt+1..9 window switching bindings generated programmatically
- [ ] Unit tests pass for parsing, registry, overrides, conflicts, and reset

---

## Commit Message

```
feat(core,rpc): add configurable keybindings with conflict detection

- Key notation parser: modifiers (ctrl/alt/shift), special keys, F-keys,
  prefix sequences (e.g., "ctrl-space c")
- KeybindingRegistry with built-in Tier 1/2 defaults, user overrides,
  plugin registration, and runtime API
- Conflict detection: user overrides built-in (warn), plugin rejects on
  conflict, reserved keys cannot be unbound
- Provenance tracking: built-in, user, plugin, runtime sources
- API methods: keybinding.list, keybinding.set, keybinding.reset
- Category-based organization for help overlay consumption
```

---

## Session Protocol

1. **Before starting:** Read task 019 (prefix key system) for the current keybinding dispatch logic. Read task 022 (TOML config) for how `[keybindings]` is parsed from config. Read task 018 (Tier 1 keybindings) for the full list of bare-key defaults.
2. **During:** Implement in order: key notation parsing (Step 1), sequence and registry types (Step 2), defaults and reserved keys (Step 3), user overrides and conflict detection (Step 4), API methods (Step 5), tests (Step 6). Run `cargo check` after each step.
3. **Edge cases to watch for:**
   - Key notation case sensitivity ("Ctrl-C" vs "ctrl-c" — should be case-insensitive)
   - Platform differences: Ctrl+Space may not be distinguishable from Ctrl+@ on some terminals
   - Modifier-only keys (e.g., just "ctrl" with no key — should be rejected)
   - Empty action string (should be rejected or treated as unbind)
   - Conflicting prefix: if user binds "alt-h x", does "alt-h" still work as bare key?
   - Config reload: keybinding overrides must be re-applied when config changes
4. **After:** Run full test suite. Manually test keybinding override in config.toml and via API. Update `docs/PROGRESS.md`. Update `CLAUDE.md` Learnings.
