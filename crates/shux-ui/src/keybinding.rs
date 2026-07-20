//! Attach keybinding registry.
//!
//! This is the first production slice of task 031: root and prefix
//! key tables for built-in attach actions, plus user overrides from
//! config. Copy-mode's internal modal keys stay in `copy_mode.rs`.

use std::collections::HashMap;

use crossterm::event::KeyEvent;
use shux_rpc::attach::{ActionArgs, ActionKind};

use crate::keys::{KeyPress, parse_key_notation};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingTarget {
    Action(ActionKind, ActionArgs),
    Detach,
}

#[derive(Debug, Clone)]
pub struct Binding {
    pub key: KeyPress,
    pub target: BindingTarget,
    pub source: BindingSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingSource {
    BuiltIn,
    User,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingTable {
    Root,
    Prefix,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeybindingError {
    InvalidKey(String),
    UnknownAction(String),
    Conflict {
        table: BindingTable,
        key: String,
        existing: BindingSource,
    },
}

#[derive(Debug, Clone)]
pub struct KeybindingRegistry {
    prefix_key: KeyPress,
    root: HashMap<KeyPress, Binding>,
    prefix: HashMap<KeyPress, Binding>,
}

impl KeybindingRegistry {
    pub fn new(prefix: &str) -> Result<Self, KeybindingError> {
        let prefix_key =
            parse_key_notation(prefix).ok_or_else(|| KeybindingError::InvalidKey(prefix.into()))?;
        let mut this = Self {
            prefix_key,
            root: HashMap::new(),
            prefix: HashMap::new(),
        };
        this.load_defaults()?;
        Ok(this)
    }

    pub fn with_overrides(
        prefix: &str,
        overrides: &HashMap<String, String>,
    ) -> Result<Self, KeybindingError> {
        let mut this = Self::new(prefix)?;
        for (key, action) in overrides {
            this.bind_user(key, action)?;
        }
        Ok(this)
    }

    pub fn prefix_key(&self) -> KeyPress {
        self.prefix_key
    }

    pub fn resolve_root(&self, key: KeyEvent) -> Option<&BindingTarget> {
        let key = KeyPress::from_crossterm(key)?;
        self.root.get(&key).map(|b| &b.target)
    }

    pub fn resolve_prefix(&self, key: KeyEvent) -> Option<&BindingTarget> {
        let key = KeyPress::from_crossterm(key)?;
        self.prefix.get(&key).map(|b| &b.target)
    }

    pub fn is_prefix_key(&self, key: KeyEvent) -> bool {
        KeyPress::from_crossterm(key) == Some(self.prefix_key)
    }

    fn load_defaults(&mut self) -> Result<(), KeybindingError> {
        for (key, target) in [
            ("alt-enter", action(ActionKind::SplitSmart)),
            ("alt-|", action(ActionKind::SplitVertical)),
            ("alt-\\", action(ActionKind::SplitVertical)),
            ("alt+minus", action(ActionKind::SplitHorizontal)),
            ("alt-left", action(ActionKind::FocusLeft)),
            ("alt-h", action(ActionKind::FocusLeft)),
            ("alt-right", action(ActionKind::FocusRight)),
            ("alt-l", action(ActionKind::FocusRight)),
            ("alt-up", action(ActionKind::FocusUp)),
            ("alt-k", action(ActionKind::FocusUp)),
            ("alt-down", action(ActionKind::FocusDown)),
            ("alt-j", action(ActionKind::FocusDown)),
            ("alt-z", action(ActionKind::ToggleZoom)),
            ("alt-x", action(ActionKind::KillPane)),
            ("alt-tab", action(ActionKind::FocusNext)),
            ("alt-n", action(ActionKind::NextWindow)),
            ("alt-p", action(ActionKind::PrevWindow)),
        ] {
            self.insert(BindingTable::Root, key, target, BindingSource::BuiltIn)?;
        }
        for idx in 1..=9 {
            self.insert(
                BindingTable::Root,
                &format!("alt-{idx}"),
                BindingTarget::Action(
                    ActionKind::SwitchToWindow,
                    ActionArgs {
                        window_index: Some(idx),
                        ..Default::default()
                    },
                ),
                BindingSource::BuiltIn,
            )?;
        }

        for (key, target) in [
            ("|", action(ActionKind::SplitVertical)),
            ("v", action(ActionKind::SplitVertical)),
            ("-", action(ActionKind::SplitHorizontal)),
            ("s", action(ActionKind::SplitHorizontal)),
            ("space", action(ActionKind::SplitSmart)),
            ("h", action(ActionKind::FocusLeft)),
            ("j", action(ActionKind::FocusDown)),
            ("k", action(ActionKind::FocusUp)),
            ("l", action(ActionKind::FocusRight)),
            ("o", action(ActionKind::FocusNext)),
            ("z", action(ActionKind::ToggleZoom)),
            ("x", action(ActionKind::KillPane)),
            ("c", action(ActionKind::NewWindow)),
            ("n", action(ActionKind::NextWindow)),
            ("p", action(ActionKind::PrevWindow)),
            ("r", action(ActionKind::Redraw)),
            ("left", action(ActionKind::ResizeLeft)),
            ("right", action(ActionKind::ResizeRight)),
            ("up", action(ActionKind::ResizeUp)),
            ("down", action(ActionKind::ResizeDown)),
            ("?", action(ActionKind::ToggleHelp)),
            ("[", action(ActionKind::EnterCopyMode)),
            ("d", BindingTarget::Detach),
        ] {
            self.insert(BindingTable::Prefix, key, target, BindingSource::BuiltIn)?;
        }
        Ok(())
    }

    fn bind_user(&mut self, key: &str, action_name: &str) -> Result<(), KeybindingError> {
        let (table, key) = parse_sequence(key, self.prefix_key)?;
        let target = parse_action(action_name)?;
        self.insert(table, &key, target, BindingSource::User)
    }

    fn insert(
        &mut self,
        table: BindingTable,
        key: &str,
        target: BindingTarget,
        source: BindingSource,
    ) -> Result<(), KeybindingError> {
        let key_press =
            parse_key_notation(key).ok_or_else(|| KeybindingError::InvalidKey(key.into()))?;
        let bindings = match table {
            BindingTable::Root => &mut self.root,
            BindingTable::Prefix => &mut self.prefix,
        };
        if let Some(existing) = bindings.get(&key_press)
            && (source != BindingSource::User || existing.source == BindingSource::User)
        {
            return Err(KeybindingError::Conflict {
                table,
                key: key.into(),
                existing: existing.source,
            });
        }
        bindings.insert(
            key_press,
            Binding {
                key: key_press,
                target,
                source,
            },
        );
        Ok(())
    }
}

fn action(kind: ActionKind) -> BindingTarget {
    BindingTarget::Action(kind, ActionArgs::default())
}

fn parse_sequence(
    sequence: &str,
    prefix_key: KeyPress,
) -> Result<(BindingTable, String), KeybindingError> {
    let parts: Vec<_> = sequence.split_whitespace().collect();
    match parts.as_slice() {
        [single] => Ok((BindingTable::Root, (*single).to_string())),
        ["prefix", key] => Ok((BindingTable::Prefix, (*key).to_string())),
        [prefix, key] => {
            let parsed = parse_key_notation(prefix)
                .ok_or_else(|| KeybindingError::InvalidKey((*prefix).into()))?;
            if parsed == prefix_key {
                Ok((BindingTable::Prefix, (*key).to_string()))
            } else {
                Err(KeybindingError::InvalidKey(sequence.into()))
            }
        }
        _ => Err(KeybindingError::InvalidKey(sequence.into())),
    }
}

fn parse_action(name: &str) -> Result<BindingTarget, KeybindingError> {
    let normalized = name.replace(['_', '.'], "-").to_ascii_lowercase();
    let target = match normalized.as_str() {
        "split-smart" | "pane-split-smart" => action(ActionKind::SplitSmart),
        "split-vertical" | "pane-split-vertical" => action(ActionKind::SplitVertical),
        "split-horizontal" | "pane-split-horizontal" => action(ActionKind::SplitHorizontal),
        "focus-left" | "pane-focus-left" => action(ActionKind::FocusLeft),
        "focus-right" | "pane-focus-right" => action(ActionKind::FocusRight),
        "focus-up" | "pane-focus-up" => action(ActionKind::FocusUp),
        "focus-down" | "pane-focus-down" => action(ActionKind::FocusDown),
        "focus-next" | "pane-focus-next" => action(ActionKind::FocusNext),
        "focus-prev" | "pane-focus-prev" => action(ActionKind::FocusPrev),
        "toggle-zoom" | "pane-toggle-zoom" => action(ActionKind::ToggleZoom),
        "kill-pane" | "pane-kill" => action(ActionKind::KillPane),
        "new-window" | "window-new" => action(ActionKind::NewWindow),
        "next-window" | "window-next" => action(ActionKind::NextWindow),
        "prev-window" | "previous-window" | "window-prev" => action(ActionKind::PrevWindow),
        "resize-left" | "pane-resize-left" => action(ActionKind::ResizeLeft),
        "resize-right" | "pane-resize-right" => action(ActionKind::ResizeRight),
        "resize-up" | "pane-resize-up" => action(ActionKind::ResizeUp),
        "resize-down" | "pane-resize-down" => action(ActionKind::ResizeDown),
        "redraw" => action(ActionKind::Redraw),
        "toggle-help" | "help" => action(ActionKind::ToggleHelp),
        "copy-mode" | "copy-enter" | "enter-copy-mode" => action(ActionKind::EnterCopyMode),
        "detach" | "session-detach" => BindingTarget::Detach,
        _ => return Err(KeybindingError::UnknownAction(name.into())),
    };
    Ok(target)
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;

    #[test]
    fn default_root_binding_resolves_alt_h() {
        let reg = KeybindingRegistry::new("ctrl-space").unwrap();
        let target = reg.resolve_root(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::ALT));
        assert_eq!(target, Some(&action(ActionKind::FocusLeft)));
    }

    #[test]
    fn default_prefix_binding_resolves_copy_mode() {
        let reg = KeybindingRegistry::new("ctrl-space").unwrap();
        let target = reg.resolve_prefix(KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE));
        assert_eq!(target, Some(&action(ActionKind::EnterCopyMode)));
    }

    #[test]
    fn user_override_replaces_builtin() {
        let overrides = HashMap::from([("alt-h".to_string(), "next-window".to_string())]);
        let reg = KeybindingRegistry::with_overrides("ctrl-space", &overrides).unwrap();
        let target = reg.resolve_root(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::ALT));
        assert_eq!(target, Some(&action(ActionKind::NextWindow)));
    }

    #[test]
    fn prefix_alias_targets_prefix_table() {
        let overrides = HashMap::from([("prefix t".to_string(), "toggle-help".to_string())]);
        let reg = KeybindingRegistry::with_overrides("ctrl-space", &overrides).unwrap();
        let target = reg.resolve_prefix(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
        assert_eq!(target, Some(&action(ActionKind::ToggleHelp)));
    }

    #[test]
    fn unknown_action_is_rejected() {
        let overrides = HashMap::from([("alt-h".to_string(), "does-not-exist".to_string())]);
        assert!(matches!(
            KeybindingRegistry::with_overrides("ctrl-space", &overrides),
            Err(KeybindingError::UnknownAction(_))
        ));
    }
}
