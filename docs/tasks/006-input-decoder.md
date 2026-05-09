# 006 — Input Decoder

**Status:** Done
**Depends On:** 000
**Parallelizable With:** 001, 002, 005

---

## Problem

shux must interpret raw terminal input (keypresses, mouse events, paste, resize) and translate it into a normalized event stream that the rest of the system can consume. Terminal input encoding is notoriously complex: different terminals encode the same key differently, modifier keys have inconsistent representations, and newer protocols (Kitty keyboard protocol) coexist with legacy ANSI input encoding.

This task builds the input decoder that reads crossterm events and normalizes them into shux's own `InputEvent` enum. It also handles Kitty keyboard protocol detection and opt-in, which enables unambiguous key reporting (distinguishing e.g. `Ctrl+I` from `Tab`, `Ctrl+M` from `Enter`).

The input decoder sits in `shux-ui` because it is part of the client-side TUI — the daemon never reads terminal input directly (clients send normalized events to the daemon via the API).

## PRD Reference

- §9 — Keybinding system (graded approach, Tier 1/2/3 keys)
- §12.1 — Capability detection strategy (Kitty keyboard query, env checks)
- §12.2 — Enhanced features: Kitty keyboard protocol detection and fallback
- §15.2 — Technology choices: crossterm 0.29 (Kitty keyboard, synchronized output, OSC 52)
- §14.1 — Performance budgets: input decode duration histogram
- §4.4 — ClientCaps (negotiated per-client capability profile)

---

## Files to Create

- `crates/shux-ui/src/input.rs` — InputEvent enum and crossterm event translation
- `crates/shux-ui/src/keys.rs` — Key types, modifier representation, key matching

## Files to Modify

- `crates/shux-ui/Cargo.toml` — Add dependencies (crossterm)
- `crates/shux-ui/src/lib.rs` — Re-export input module (replaces stub)

---

## Execution Steps

### Step 1: Add dependencies to shux-ui

Update `crates/shux-ui/Cargo.toml`:

```toml
[package]
name = "shux-ui"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
crossterm = { workspace = true }
tracing = { workspace = true }
thiserror = { workspace = true }

[dev-dependencies]
```

### Step 2: Define key types and modifiers (`keys.rs`)

This module defines the canonical representation of keys and modifiers in shux. All internal keybinding logic and configuration uses these types — never raw crossterm types.

```rust
//! Key types and modifier representation for shux.
//!
//! These types normalize the various ways terminals represent keys
//! into a consistent, matchable format. Used by the keybinding system
//! (task 018+) and the input decoder.

use std::fmt;

/// Modifier key flags.
///
/// Packed into a u8 for efficient matching. Multiple modifiers can be
/// combined (e.g., Ctrl+Shift).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Modifiers(u8);

impl Modifiers {
    pub const NONE: Modifiers  = Modifiers(0);
    pub const SHIFT: u8        = 0b0001;
    pub const CTRL: u8         = 0b0010;
    pub const ALT: u8          = 0b0100;
    pub const SUPER: u8        = 0b1000;

    /// Create a new Modifiers from raw flags.
    pub const fn from_bits(bits: u8) -> Self { Modifiers(bits) }

    /// Check if a modifier is active.
    pub fn contains(self, flag: u8) -> bool { self.0 & flag != 0 }

    /// Whether no modifiers are active.
    pub fn is_empty(self) -> bool { self.0 == 0 }

    /// Combine with another modifier.
    pub fn with(self, flag: u8) -> Self { Modifiers(self.0 | flag) }

    /// Remove a modifier.
    pub fn without(self, flag: u8) -> Self { Modifiers(self.0 & !flag) }

    /// The raw bits.
    pub fn bits(self) -> u8 { self.0 }

    /// Convert from crossterm KeyModifiers.
    pub fn from_crossterm(mods: crossterm::event::KeyModifiers) -> Self {
        let mut result = 0u8;
        if mods.contains(crossterm::event::KeyModifiers::SHIFT) {
            result |= Self::SHIFT;
        }
        if mods.contains(crossterm::event::KeyModifiers::CONTROL) {
            result |= Self::CTRL;
        }
        if mods.contains(crossterm::event::KeyModifiers::ALT) {
            result |= Self::ALT;
        }
        if mods.contains(crossterm::event::KeyModifiers::SUPER) {
            result |= Self::SUPER;
        }
        Modifiers(result)
    }
}

impl fmt::Display for Modifiers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = Vec::new();
        if self.contains(Self::CTRL) { parts.push("Ctrl"); }
        if self.contains(Self::ALT) { parts.push("Alt"); }
        if self.contains(Self::SHIFT) { parts.push("Shift"); }
        if self.contains(Self::SUPER) { parts.push("Super"); }
        write!(f, "{}", parts.join("+"))
    }
}

/// A named key (non-character keys).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NamedKey {
    Backspace,
    Enter,
    Tab,
    BackTab, // Shift+Tab
    Escape,
    Insert,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    F(u8), // F1-F24
    CapsLock,
    ScrollLock,
    NumLock,
    PrintScreen,
    Pause,
    Menu,
}

impl fmt::Display for NamedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NamedKey::Backspace => write!(f, "Backspace"),
            NamedKey::Enter => write!(f, "Enter"),
            NamedKey::Tab => write!(f, "Tab"),
            NamedKey::BackTab => write!(f, "BackTab"),
            NamedKey::Escape => write!(f, "Escape"),
            NamedKey::Insert => write!(f, "Insert"),
            NamedKey::Delete => write!(f, "Delete"),
            NamedKey::Home => write!(f, "Home"),
            NamedKey::End => write!(f, "End"),
            NamedKey::PageUp => write!(f, "PageUp"),
            NamedKey::PageDown => write!(f, "PageDown"),
            NamedKey::Up => write!(f, "Up"),
            NamedKey::Down => write!(f, "Down"),
            NamedKey::Left => write!(f, "Left"),
            NamedKey::Right => write!(f, "Right"),
            NamedKey::F(n) => write!(f, "F{n}"),
            NamedKey::CapsLock => write!(f, "CapsLock"),
            NamedKey::ScrollLock => write!(f, "ScrollLock"),
            NamedKey::NumLock => write!(f, "NumLock"),
            NamedKey::PrintScreen => write!(f, "PrintScreen"),
            NamedKey::Pause => write!(f, "Pause"),
            NamedKey::Menu => write!(f, "Menu"),
        }
    }
}

/// The logical key value — either a character or a named key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyValue {
    /// A Unicode character (e.g., 'a', 'A', '/', ' ').
    Char(char),
    /// A named (non-character) key.
    Named(NamedKey),
}

impl fmt::Display for KeyValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyValue::Char(' ') => write!(f, "Space"),
            KeyValue::Char(c) => write!(f, "{c}"),
            KeyValue::Named(k) => write!(f, "{k}"),
        }
    }
}

/// A complete key event: key value + modifiers.
///
/// This is the canonical representation used throughout shux for
/// keybinding matching, configuration, and display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyPress {
    pub key: KeyValue,
    pub modifiers: Modifiers,
}

impl KeyPress {
    /// Create a key press with no modifiers.
    pub fn new(key: KeyValue) -> Self {
        KeyPress {
            key,
            modifiers: Modifiers::NONE,
        }
    }

    /// Create a character key press with modifiers.
    pub fn char_with_mods(ch: char, mods: Modifiers) -> Self {
        KeyPress {
            key: KeyValue::Char(ch),
            modifiers: mods,
        }
    }

    /// Create a named key press with modifiers.
    pub fn named_with_mods(key: NamedKey, mods: Modifiers) -> Self {
        KeyPress {
            key: KeyValue::Named(key),
            modifiers: mods,
        }
    }

    /// Whether this is the given character with the given modifier.
    pub fn is_char(&self, ch: char, modifier: u8) -> bool {
        self.key == KeyValue::Char(ch) && self.modifiers == Modifiers::from_bits(modifier)
    }

    /// Whether this is the given named key with the given modifier.
    pub fn is_named(&self, key: NamedKey, modifier: u8) -> bool {
        self.key == KeyValue::Named(key) && self.modifiers == Modifiers::from_bits(modifier)
    }
}

impl fmt::Display for KeyPress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.modifiers.is_empty() {
            write!(f, "{}+{}", self.modifiers, self.key)
        } else {
            write!(f, "{}", self.key)
        }
    }
}

/// Parse a key notation string (e.g., "ctrl+a", "alt-h", "F5") into a KeyPress.
///
/// Supported formats (case-insensitive):
/// - "a", "A", "Space", "Enter", "F1"
/// - "ctrl+a", "alt+h", "shift+Tab"
/// - "ctrl+alt+Delete"
/// - "ctrl-a" (hyphen also accepted as separator)
///
/// Returns None if the string cannot be parsed.
pub fn parse_key_notation(s: &str) -> Option<KeyPress> {
    let s = s.trim();
    // Split on '+' or '-' (but not inside key names like "BackTab").
    let parts: Vec<&str> = s.split(['+', '-']).collect();
    let mut modifiers = Modifiers::NONE;

    for part in &parts[..parts.len() - 1] {
        let part_lower = part.trim().to_lowercase();
        match part_lower.as_str() {
            "ctrl" | "control" | "c" => modifiers = modifiers.with(Modifiers::CTRL),
            "alt" | "meta" | "m" => modifiers = modifiers.with(Modifiers::ALT),
            "shift" | "s" => modifiers = modifiers.with(Modifiers::SHIFT),
            "super" | "win" | "cmd" => modifiers = modifiers.with(Modifiers::SUPER),
            _ => return None, // Unknown modifier.
        }
    }

    let key_str = parts.last()?.trim();
    let key = parse_key_value(key_str)?;

    Some(KeyPress { key, modifiers })
}

/// Parse a single key value string into a KeyValue.
fn parse_key_value(s: &str) -> Option<KeyValue> {
    let lower = s.to_lowercase();
    match lower.as_str() {
        "space" | " " => Some(KeyValue::Char(' ')),
        "backspace" | "bs" => Some(KeyValue::Named(NamedKey::Backspace)),
        "enter" | "return" | "cr" => Some(KeyValue::Named(NamedKey::Enter)),
        "tab" => Some(KeyValue::Named(NamedKey::Tab)),
        "backtab" => Some(KeyValue::Named(NamedKey::BackTab)),
        "escape" | "esc" => Some(KeyValue::Named(NamedKey::Escape)),
        "insert" | "ins" => Some(KeyValue::Named(NamedKey::Insert)),
        "delete" | "del" => Some(KeyValue::Named(NamedKey::Delete)),
        "home" => Some(KeyValue::Named(NamedKey::Home)),
        "end" => Some(KeyValue::Named(NamedKey::End)),
        "pageup" | "pgup" => Some(KeyValue::Named(NamedKey::PageUp)),
        "pagedown" | "pgdn" | "pgdown" => Some(KeyValue::Named(NamedKey::PageDown)),
        "up" => Some(KeyValue::Named(NamedKey::Up)),
        "down" => Some(KeyValue::Named(NamedKey::Down)),
        "left" => Some(KeyValue::Named(NamedKey::Left)),
        "right" => Some(KeyValue::Named(NamedKey::Right)),
        other => {
            // Check for F-keys: "f1", "f12", etc.
            if let Some(n_str) = other.strip_prefix('f') {
                if let Ok(n) = n_str.parse::<u8>() {
                    if (1..=24).contains(&n) {
                        return Some(KeyValue::Named(NamedKey::F(n)));
                    }
                }
            }
            // Single character.
            let chars: Vec<char> = s.chars().collect();
            if chars.len() == 1 {
                Some(KeyValue::Char(chars[0]))
            } else {
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_modifiers_from_crossterm() {
        use crossterm::event::KeyModifiers;
        let mods = Modifiers::from_crossterm(KeyModifiers::CONTROL | KeyModifiers::ALT);
        assert!(mods.contains(Modifiers::CTRL));
        assert!(mods.contains(Modifiers::ALT));
        assert!(!mods.contains(Modifiers::SHIFT));
    }

    #[test]
    fn test_modifiers_display() {
        let mods = Modifiers::NONE.with(Modifiers::CTRL).with(Modifiers::ALT);
        assert_eq!(format!("{mods}"), "Ctrl+Alt");
    }

    #[test]
    fn test_key_press_display() {
        let kp = KeyPress::char_with_mods('a', Modifiers::NONE.with(Modifiers::CTRL));
        assert_eq!(format!("{kp}"), "Ctrl+a");

        let kp2 = KeyPress::new(KeyValue::Named(NamedKey::F(5)));
        assert_eq!(format!("{kp2}"), "F5");

        let kp3 = KeyPress::new(KeyValue::Char(' '));
        assert_eq!(format!("{kp3}"), "Space");
    }

    #[test]
    fn test_parse_key_notation() {
        assert_eq!(
            parse_key_notation("ctrl+a"),
            Some(KeyPress::char_with_mods('a', Modifiers::NONE.with(Modifiers::CTRL)))
        );
        assert_eq!(
            parse_key_notation("alt+h"),
            Some(KeyPress::char_with_mods('h', Modifiers::NONE.with(Modifiers::ALT)))
        );
        assert_eq!(
            parse_key_notation("F5"),
            Some(KeyPress::new(KeyValue::Named(NamedKey::F(5))))
        );
        assert_eq!(
            parse_key_notation("ctrl+alt+Delete"),
            Some(KeyPress::named_with_mods(
                NamedKey::Delete,
                Modifiers::NONE.with(Modifiers::CTRL).with(Modifiers::ALT)
            ))
        );
        assert_eq!(
            parse_key_notation("Space"),
            Some(KeyPress::new(KeyValue::Char(' ')))
        );
        assert_eq!(
            parse_key_notation("ctrl-a"),
            Some(KeyPress::char_with_mods('a', Modifiers::NONE.with(Modifiers::CTRL)))
        );
    }

    #[test]
    fn test_parse_key_notation_invalid() {
        assert_eq!(parse_key_notation(""), None);
        assert_eq!(parse_key_notation("ctrl+"), None);
        assert_eq!(parse_key_notation("unknown_key_name"), None);
    }

    #[test]
    fn test_key_press_matching() {
        let kp = KeyPress::char_with_mods('h', Modifiers::NONE.with(Modifiers::ALT));
        assert!(kp.is_char('h', Modifiers::ALT));
        assert!(!kp.is_char('h', Modifiers::CTRL));
        assert!(!kp.is_char('j', Modifiers::ALT));
    }
}
```

### Step 3: Define InputEvent and crossterm translation (`input.rs`)

This module reads crossterm events and normalizes them into shux's `InputEvent` enum.

```rust
//! Input event types and crossterm event translation.
//!
//! Reads raw crossterm events and normalizes them into shux's InputEvent
//! enum. Handles legacy input mode and Kitty keyboard protocol differences.

use crossterm::event::{
    Event as CtEvent, KeyCode as CtKeyCode, KeyEvent as CtKeyEvent,
    KeyEventKind, KeyModifiers as CtKeyModifiers,
    MouseButton as CtMouseButton, MouseEvent as CtMouseEvent,
    MouseEventKind as CtMouseEventKind,
};
use tracing::trace;

use crate::keys::{KeyPress, KeyValue, Modifiers, NamedKey};

/// Mouse button identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// Mouse event kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    /// Button pressed at position.
    Press(MouseButton),
    /// Button released at position.
    Release(MouseButton),
    /// Mouse dragged with button held.
    Drag(MouseButton),
    /// Scroll up.
    ScrollUp,
    /// Scroll down.
    ScrollDown,
    /// Scroll left.
    ScrollLeft,
    /// Scroll right.
    ScrollRight,
    /// Mouse moved (no button pressed).
    Move,
}

/// A mouse event with position and modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    pub action: MouseAction,
    /// Column (0-indexed).
    pub col: u16,
    /// Row (0-indexed).
    pub row: u16,
    /// Modifier keys held during the mouse event.
    pub modifiers: Modifiers,
}

/// Normalized input event for shux.
///
/// This is the canonical event type consumed by the keybinding system
/// and the input handler. All crossterm-specific details are erased.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    /// A key press event.
    Key(KeyPress),
    /// A mouse event.
    Mouse(MouseEvent),
    /// Terminal was resized to new dimensions.
    Resize {
        cols: u16,
        rows: u16,
    },
    /// Bracketed paste content.
    Paste(String),
    /// Focus gained (terminal gained focus).
    FocusGained,
    /// Focus lost (terminal lost focus).
    FocusLost,
}

/// Whether the Kitty keyboard protocol is active.
///
/// When Kitty protocol is active, crossterm provides unambiguous key
/// identification (e.g., distinguishing Ctrl+I from Tab). When not active,
/// we use legacy decoding with disambiguation heuristics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyboardProtocol {
    /// Legacy ANSI input decoding.
    Legacy,
    /// Kitty keyboard protocol (progressive enhancement flags active).
    Kitty,
}

impl Default for KeyboardProtocol {
    fn default() -> Self { KeyboardProtocol::Legacy }
}

/// Translate a crossterm Event into a shux InputEvent.
///
/// Returns `None` for events we don't care about (e.g., key release
/// in legacy mode, unknown event types).
///
/// The `protocol` parameter affects how key events are decoded:
/// - In `Legacy` mode, certain ambiguous keys are disambiguated by convention.
/// - In `Kitty` mode, crossterm provides full key information directly.
pub fn translate_event(event: CtEvent, protocol: KeyboardProtocol) -> Option<InputEvent> {
    match event {
        CtEvent::Key(key_event) => translate_key_event(key_event, protocol),
        CtEvent::Mouse(mouse_event) => Some(translate_mouse_event(mouse_event)),
        CtEvent::Resize(cols, rows) => Some(InputEvent::Resize { cols, rows }),
        CtEvent::Paste(text) => Some(InputEvent::Paste(text)),
        CtEvent::FocusGained => Some(InputEvent::FocusGained),
        CtEvent::FocusLost => Some(InputEvent::FocusLost),
    }
}

/// Translate a crossterm key event into a shux InputEvent.
fn translate_key_event(
    event: CtKeyEvent,
    protocol: KeyboardProtocol,
) -> Option<InputEvent> {
    // In Kitty mode, crossterm reports key press, repeat, and release separately.
    // We only care about press and repeat events.
    // In legacy mode, crossterm only reports press events.
    match event.kind {
        KeyEventKind::Press | KeyEventKind::Repeat => {}
        KeyEventKind::Release => return None,
    }

    let modifiers = Modifiers::from_crossterm(event.modifiers);

    let key_value = match event.code {
        CtKeyCode::Char(c) => {
            // In legacy mode, crossterm may report Ctrl+letter as the control
            // character (e.g., Ctrl+A as '\x01'). crossterm 0.29 normalizes
            // this for us, but we handle it defensively.
            if protocol == KeyboardProtocol::Legacy && modifiers.contains(Modifiers::CTRL) {
                // crossterm already normalizes Ctrl+letter to the letter char
                // with CONTROL modifier, so we just pass through.
                KeyValue::Char(c)
            } else {
                KeyValue::Char(c)
            }
        }
        CtKeyCode::Backspace => KeyValue::Named(NamedKey::Backspace),
        CtKeyCode::Enter => KeyValue::Named(NamedKey::Enter),
        CtKeyCode::Tab => {
            // Shift+Tab comes as BackTab in crossterm.
            KeyValue::Named(NamedKey::Tab)
        }
        CtKeyCode::BackTab => {
            // BackTab is Shift+Tab. We normalize it.
            return Some(InputEvent::Key(KeyPress {
                key: KeyValue::Named(NamedKey::BackTab),
                modifiers: modifiers.with(Modifiers::SHIFT),
            }));
        }
        CtKeyCode::Esc => KeyValue::Named(NamedKey::Escape),
        CtKeyCode::Insert => KeyValue::Named(NamedKey::Insert),
        CtKeyCode::Delete => KeyValue::Named(NamedKey::Delete),
        CtKeyCode::Home => KeyValue::Named(NamedKey::Home),
        CtKeyCode::End => KeyValue::Named(NamedKey::End),
        CtKeyCode::PageUp => KeyValue::Named(NamedKey::PageUp),
        CtKeyCode::PageDown => KeyValue::Named(NamedKey::PageDown),
        CtKeyCode::Up => KeyValue::Named(NamedKey::Up),
        CtKeyCode::Down => KeyValue::Named(NamedKey::Down),
        CtKeyCode::Left => KeyValue::Named(NamedKey::Left),
        CtKeyCode::Right => KeyValue::Named(NamedKey::Right),
        CtKeyCode::F(n) => KeyValue::Named(NamedKey::F(n)),
        CtKeyCode::CapsLock => KeyValue::Named(NamedKey::CapsLock),
        CtKeyCode::ScrollLock => KeyValue::Named(NamedKey::ScrollLock),
        CtKeyCode::NumLock => KeyValue::Named(NamedKey::NumLock),
        CtKeyCode::PrintScreen => KeyValue::Named(NamedKey::PrintScreen),
        CtKeyCode::Pause => KeyValue::Named(NamedKey::Pause),
        CtKeyCode::Menu => KeyValue::Named(NamedKey::Menu),
        CtKeyCode::Null => {
            // Ctrl+Space in legacy mode. Normalize to Char(' ') with Ctrl.
            return Some(InputEvent::Key(KeyPress {
                key: KeyValue::Char(' '),
                modifiers: modifiers.with(Modifiers::CTRL),
            }));
        }
        _ => {
            trace!(code = ?event.code, "unhandled crossterm key code");
            return None;
        }
    };

    Some(InputEvent::Key(KeyPress {
        key: key_value,
        modifiers,
    }))
}

/// Translate a crossterm mouse event into a shux InputEvent.
fn translate_mouse_event(event: CtMouseEvent) -> InputEvent {
    let modifiers = Modifiers::from_crossterm(event.modifiers);

    let action = match event.kind {
        CtMouseEventKind::Down(button) => MouseAction::Press(translate_mouse_button(button)),
        CtMouseEventKind::Up(button) => MouseAction::Release(translate_mouse_button(button)),
        CtMouseEventKind::Drag(button) => MouseAction::Drag(translate_mouse_button(button)),
        CtMouseEventKind::ScrollUp => MouseAction::ScrollUp,
        CtMouseEventKind::ScrollDown => MouseAction::ScrollDown,
        CtMouseEventKind::ScrollLeft => MouseAction::ScrollLeft,
        CtMouseEventKind::ScrollRight => MouseAction::ScrollRight,
        CtMouseEventKind::Moved => MouseAction::Move,
    };

    InputEvent::Mouse(MouseEvent {
        action,
        col: event.column,
        row: event.row,
        modifiers,
    })
}

/// Translate a crossterm mouse button.
fn translate_mouse_button(button: CtMouseButton) -> MouseButton {
    match button {
        CtMouseButton::Left => MouseButton::Left,
        CtMouseButton::Right => MouseButton::Right,
        CtMouseButton::Middle => MouseButton::Middle,
    }
}

/// Enable the Kitty keyboard protocol on the terminal.
///
/// Sends the appropriate escape sequence to request enhanced keyboard
/// reporting. Returns `Ok(())` if successful.
///
/// The flags we request (from crossterm's PushKeyboardEnhancementFlags):
/// - DISAMBIGUATE_ESCAPE_CODES: distinguish e.g. Ctrl+I from Tab
/// - REPORT_EVENT_TYPES: get key press, repeat, and release events
/// - REPORT_ALL_KEYS_AS_ESCAPE_CODES: all keys use CSI u format
///
/// Call this after detecting that the terminal supports the Kitty protocol
/// (via capability detection in task 028).
pub fn enable_kitty_protocol() -> std::io::Result<()> {
    use crossterm::execute;
    use crossterm::event::PushKeyboardEnhancementFlags;
    use crossterm::event::KeyboardEnhancementFlags;

    execute!(
        std::io::stdout(),
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
        )
    )
}

/// Disable the Kitty keyboard protocol (restore legacy mode).
///
/// Call this on client detach or exit to restore the terminal to normal.
pub fn disable_kitty_protocol() -> std::io::Result<()> {
    use crossterm::execute;
    use crossterm::event::PopKeyboardEnhancementFlags;

    execute!(std::io::stdout(), PopKeyboardEnhancementFlags)
}

/// Read the next input event from the terminal.
///
/// This is a blocking call. For async usage, use `crossterm::event::EventStream`
/// (with the `event-stream` feature) in the TUI client's event loop.
///
/// The `protocol` parameter controls key decoding.
pub fn read_event(protocol: KeyboardProtocol) -> std::io::Result<Option<InputEvent>> {
    let event = crossterm::event::read()?;
    Ok(translate_event(event, protocol))
}

/// Check if an input event is available without blocking.
///
/// Uses `crossterm::event::poll` with zero timeout.
pub fn poll_event(timeout: std::time::Duration) -> std::io::Result<bool> {
    crossterm::event::poll(timeout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    fn make_key_event(code: KeyCode, modifiers: KeyModifiers) -> CtKeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    #[test]
    fn test_translate_char_key() {
        let ct_event = CtEvent::Key(make_key_event(KeyCode::Char('a'), KeyModifiers::NONE));
        let result = translate_event(ct_event, KeyboardProtocol::Legacy);
        assert_eq!(
            result,
            Some(InputEvent::Key(KeyPress::new(KeyValue::Char('a'))))
        );
    }

    #[test]
    fn test_translate_ctrl_char() {
        let ct_event = CtEvent::Key(make_key_event(KeyCode::Char('c'), KeyModifiers::CONTROL));
        let result = translate_event(ct_event, KeyboardProtocol::Legacy);
        assert_eq!(
            result,
            Some(InputEvent::Key(KeyPress::char_with_mods(
                'c',
                Modifiers::NONE.with(Modifiers::CTRL)
            )))
        );
    }

    #[test]
    fn test_translate_alt_h() {
        let ct_event = CtEvent::Key(make_key_event(KeyCode::Char('h'), KeyModifiers::ALT));
        let result = translate_event(ct_event, KeyboardProtocol::Legacy);
        assert_eq!(
            result,
            Some(InputEvent::Key(KeyPress::char_with_mods(
                'h',
                Modifiers::NONE.with(Modifiers::ALT)
            )))
        );
    }

    #[test]
    fn test_translate_f_key() {
        let ct_event = CtEvent::Key(make_key_event(KeyCode::F(5), KeyModifiers::NONE));
        let result = translate_event(ct_event, KeyboardProtocol::Legacy);
        assert_eq!(
            result,
            Some(InputEvent::Key(KeyPress::new(KeyValue::Named(NamedKey::F(5)))))
        );
    }

    #[test]
    fn test_translate_backtab() {
        let ct_event = CtEvent::Key(make_key_event(KeyCode::BackTab, KeyModifiers::SHIFT));
        let result = translate_event(ct_event, KeyboardProtocol::Legacy);
        let expected = InputEvent::Key(KeyPress {
            key: KeyValue::Named(NamedKey::BackTab),
            modifiers: Modifiers::NONE.with(Modifiers::SHIFT),
        });
        assert_eq!(result, Some(expected));
    }

    #[test]
    fn test_translate_ctrl_space() {
        // Ctrl+Space comes as KeyCode::Null in legacy mode.
        let ct_event = CtEvent::Key(make_key_event(KeyCode::Null, KeyModifiers::CONTROL));
        let result = translate_event(ct_event, KeyboardProtocol::Legacy);
        let expected = InputEvent::Key(KeyPress {
            key: KeyValue::Char(' '),
            modifiers: Modifiers::NONE.with(Modifiers::CTRL),
        });
        assert_eq!(result, Some(expected));
    }

    #[test]
    fn test_translate_resize() {
        let ct_event = CtEvent::Resize(120, 40);
        let result = translate_event(ct_event, KeyboardProtocol::Legacy);
        assert_eq!(result, Some(InputEvent::Resize { cols: 120, rows: 40 }));
    }

    #[test]
    fn test_translate_paste() {
        let ct_event = CtEvent::Paste("hello world".to_string());
        let result = translate_event(ct_event, KeyboardProtocol::Legacy);
        assert_eq!(result, Some(InputEvent::Paste("hello world".to_string())));
    }

    #[test]
    fn test_translate_mouse_click() {
        let ct_event = CtEvent::Mouse(CtMouseEvent {
            kind: CtMouseEventKind::Down(CtMouseButton::Left),
            column: 10,
            row: 5,
            modifiers: KeyModifiers::NONE,
        });
        let result = translate_event(ct_event, KeyboardProtocol::Legacy);
        match result {
            Some(InputEvent::Mouse(me)) => {
                assert_eq!(me.action, MouseAction::Press(MouseButton::Left));
                assert_eq!(me.col, 10);
                assert_eq!(me.row, 5);
            }
            _ => panic!("expected mouse event"),
        }
    }

    #[test]
    fn test_translate_scroll() {
        let ct_event = CtEvent::Mouse(CtMouseEvent {
            kind: CtMouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        let result = translate_event(ct_event, KeyboardProtocol::Legacy);
        match result {
            Some(InputEvent::Mouse(me)) => {
                assert_eq!(me.action, MouseAction::ScrollUp);
            }
            _ => panic!("expected mouse event"),
        }
    }

    #[test]
    fn test_key_release_ignored() {
        let ct_event = CtEvent::Key(KeyEvent {
            code: KeyCode::Char('a'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Release,
            state: KeyEventState::empty(),
        });
        let result = translate_event(ct_event, KeyboardProtocol::Kitty);
        assert_eq!(result, None);
    }

    #[test]
    fn test_key_repeat_accepted() {
        let ct_event = CtEvent::Key(KeyEvent {
            code: KeyCode::Char('a'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Repeat,
            state: KeyEventState::empty(),
        });
        let result = translate_event(ct_event, KeyboardProtocol::Kitty);
        assert!(result.is_some());
    }

    #[test]
    fn test_focus_events() {
        assert_eq!(
            translate_event(CtEvent::FocusGained, KeyboardProtocol::Legacy),
            Some(InputEvent::FocusGained)
        );
        assert_eq!(
            translate_event(CtEvent::FocusLost, KeyboardProtocol::Legacy),
            Some(InputEvent::FocusLost)
        );
    }

    #[test]
    fn test_modifier_combinations() {
        let ct_event = CtEvent::Key(make_key_event(
            KeyCode::Char('d'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ));
        let result = translate_event(ct_event, KeyboardProtocol::Legacy);
        let expected = InputEvent::Key(KeyPress {
            key: KeyValue::Char('d'),
            modifiers: Modifiers::NONE.with(Modifiers::CTRL).with(Modifiers::ALT),
        });
        assert_eq!(result, Some(expected));
    }
}
```

### Step 4: Update lib.rs

Replace the stub `lib.rs` with module declarations and re-exports.

```rust
//! shux-ui — TUI client and input handling.
//!
//! This crate provides the terminal user interface for shux clients:
//! input decoding (crossterm events → shux InputEvents), key types,
//! and (in future tasks) the render compositor and TUI chrome.

pub mod input;
pub mod keys;

// Re-export commonly used types.
pub use input::{InputEvent, KeyboardProtocol, MouseAction, MouseButton, MouseEvent};
pub use keys::{KeyPress, KeyValue, Modifiers, NamedKey};
```

### Step 5: Verify crossterm 0.29 API compatibility

Before finalizing, the implementing agent should verify:

1. **`crossterm::event::KeyEventKind`** exists and has `Press`, `Repeat`, `Release` variants (added in crossterm 0.27+, stable in 0.29).
2. **`PushKeyboardEnhancementFlags`** and `PopKeyboardEnhancementFlags`** are available in `crossterm::event` (added for Kitty protocol support).
3. **`KeyboardEnhancementFlags`** has the flags `DISAMBIGUATE_ESCAPE_CODES`, `REPORT_EVENT_TYPES`, `REPORT_ALL_KEYS_AS_ESCAPE_CODES`.
4. **`CtMouseEventKind::ScrollLeft`** and **`ScrollRight`** exist (added in crossterm 0.27+).
5. **`CtEvent::FocusGained`** and **`FocusLost`** exist (added in crossterm 0.26+).
6. **`CtEvent::Paste`** exists (for bracketed paste support).

If any of these APIs have changed in crossterm 0.29, adapt the implementation accordingly. Check `crossterm` docs or source at `https://docs.rs/crossterm/0.29`.

### Step 6: Handle edge cases

Important edge cases to handle (or document for future tasks):

1. **Ctrl+Space as prefix key (PRD §9.2)**: In legacy mode, Ctrl+Space is `KeyCode::Null`. We normalize it to `KeyValue::Char(' ')` with `Modifiers::CTRL`. This matches the PRD's default prefix key `ctrl+space`.

2. **Alt+letter case sensitivity**: `Alt+h` (lowercase) and `Alt+H` (uppercase, i.e., Alt+Shift+h) are distinct. crossterm reports the actual character, so `Alt+H` comes as `Char('H')` with `ALT` modifier. The keybinding system (task 018+) must handle this.

3. **Tab vs Ctrl+I**: In legacy mode, these are indistinguishable (both send `\t`). In Kitty mode, they are distinct. We report what crossterm gives us. The keybinding system should handle the legacy ambiguity.

4. **Numpad keys**: crossterm does not distinguish numpad keys from regular keys in most terminal emulators. This is acceptable for v1.

---

## Verification

### Functional

```bash
# Build the shux-ui crate
cargo build -p shux-ui

# Check for clippy warnings
cargo clippy -p shux-ui -- -D warnings

# Format check
cargo fmt -p shux-ui -- --check
```

### Tests

```bash
# Run all shux-ui tests
cargo nextest run -p shux-ui

# Kitty capability fallback behavior
cargo nextest run -p shux-ui -- input::tests::kitty_fallback

# Run with output
cargo nextest run -p shux-ui --no-capture

# Run specific test module
cargo nextest run -p shux-ui -- keys::tests
cargo nextest run -p shux-ui -- input::tests
```

---

## Completion Criteria

- [ ] `crates/shux-ui/src/keys.rs` — Modifiers, NamedKey, KeyValue, KeyPress types with Display and Hash impls
- [ ] `crates/shux-ui/src/keys.rs` — `parse_key_notation()` function for config parsing (e.g., "ctrl+a" -> KeyPress)
- [ ] `crates/shux-ui/src/input.rs` — InputEvent enum: Key, Mouse, Resize, Paste, FocusGained, FocusLost
- [ ] `crates/shux-ui/src/input.rs` — MouseEvent with MouseAction and MouseButton
- [ ] `crates/shux-ui/src/input.rs` — `translate_event()` converts crossterm Event to InputEvent
- [ ] `crates/shux-ui/src/input.rs` — Legacy mode key decoding (all standard keys, modifiers, F-keys)
- [ ] `crates/shux-ui/src/input.rs` — Kitty keyboard protocol enable/disable functions
- [ ] `crates/shux-ui/src/input.rs` — Key release events filtered in Kitty mode, key repeat events accepted
- [ ] `crates/shux-ui/src/input.rs` — Ctrl+Space normalized to Char(' ') + CTRL (prefix key support)
- [ ] Capability fallback test verifies graceful downgrade when Kitty negotiation fails
- [ ] `crates/shux-ui/src/input.rs` — BackTab normalized to BackTab + SHIFT
- [ ] `crates/shux-ui/src/input.rs` — Mouse events: press, release, drag, scroll (all directions), move
- [ ] `crates/shux-ui/src/input.rs` — Resize events pass through terminal dimensions
- [ ] `crates/shux-ui/src/input.rs` — Paste events pass through text content
- [ ] `crates/shux-ui/src/lib.rs` — Module declarations and re-exports
- [ ] `crates/shux-ui/Cargo.toml` — crossterm workspace dependency added
- [ ] Unit tests for key event normalization pass
- [ ] Unit tests for key notation parsing pass
- [ ] Unit tests for mouse event translation pass
- [ ] Unit tests for modifier handling pass
- [ ] `cargo clippy -p shux-ui -- -D warnings` passes
- [ ] `cargo fmt -p shux-ui -- --check` passes

---

## Commit Message

```
feat(ui): implement input decoder with crossterm event normalization

- InputEvent enum normalizing crossterm Key, Mouse, Resize, Paste events
- KeyPress/KeyValue/Modifiers types for canonical key representation
- Key notation parser for config strings (e.g., "ctrl+a", "alt-h")
- Legacy and Kitty keyboard protocol mode support
- Ctrl+Space normalization for prefix key (PRD §9.2)
- Mouse event handling: click, drag, scroll, move
- Unit tests for all event translation paths
```

---

## Session Protocol

1. **Before starting:** Read `CLAUDE.md`, `docs/PRD.md` §9 (Keybinding system), §12.2 (Kitty keyboard protocol), and §15.2 (crossterm 0.29). Verify task 000 is complete (workspace compiles).
2. **During implementation:**
   - Start with `keys.rs` — the types that the rest of the system depends on. These types will be used by the keybinding system (task 018+), the command palette (task 032), config parsing (task 022), and the help overlay (task 033). Get the API right.
   - Then `input.rs` — the crossterm translation layer. Test each event type.
   - Verify crossterm 0.29 API compatibility before implementing Kitty protocol functions. If `PushKeyboardEnhancementFlags` is not available, leave the functions as stubs with a TODO.
   - Run `cargo clippy -p shux-ui -- -D warnings` after each file.
3. **Key design decisions:**
   - `InputEvent` is the only event type the rest of shux sees. No crossterm types leak outside this crate.
   - `KeyPress` implements `Hash` + `Eq` so it can be used as HashMap keys (for keybinding lookup).
   - `Modifiers` is a bitfield (u8) for efficient matching.
   - The `parse_key_notation` function supports the notation used in TOML config files (PRD §10.2 `[keybindings]` section).
4. **After:** Run full test suite (`cargo nextest run -p shux-ui`). Update `docs/PROGRESS.md` (mark 006 done). Update `CLAUDE.md` Learnings with any crossterm API discoveries.
