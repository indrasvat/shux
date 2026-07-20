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
    pub const NONE: Modifiers = Modifiers(0);
    pub const SHIFT: u8 = 0b0001;
    pub const CTRL: u8 = 0b0010;
    pub const ALT: u8 = 0b0100;
    pub const SUPER: u8 = 0b1000;

    /// Create a new Modifiers from raw flags.
    pub const fn from_bits(bits: u8) -> Self {
        Modifiers(bits)
    }

    /// Check if a modifier is active.
    pub fn contains(self, flag: u8) -> bool {
        self.0 & flag != 0
    }

    /// Whether no modifiers are active.
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Combine with another modifier.
    pub fn with(self, flag: u8) -> Self {
        Modifiers(self.0 | flag)
    }

    /// Remove a modifier.
    pub fn without(self, flag: u8) -> Self {
        Modifiers(self.0 & !flag)
    }

    /// The raw bits.
    pub fn bits(self) -> u8 {
        self.0
    }

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
        if self.contains(Self::CTRL) {
            parts.push("Ctrl");
        }
        if self.contains(Self::ALT) {
            parts.push("Alt");
        }
        if self.contains(Self::SHIFT) {
            parts.push("Shift");
        }
        if self.contains(Self::SUPER) {
            parts.push("Super");
        }
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

/// The logical key value -- either a character or a named key.
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

    /// Convert a crossterm key event into SHUX's canonical key press
    /// representation. This mirrors the input decoder but is public so
    /// the attach keybinding registry can resolve shortcuts before the
    /// key is encoded and forwarded to the PTY.
    pub fn from_crossterm(event: crossterm::event::KeyEvent) -> Option<Self> {
        use crossterm::event::KeyCode;

        let modifiers = Modifiers::from_crossterm(event.modifiers);
        let key = match event.code {
            KeyCode::Char(c) => KeyValue::Char(c),
            KeyCode::Backspace => KeyValue::Named(NamedKey::Backspace),
            KeyCode::Enter => KeyValue::Named(NamedKey::Enter),
            KeyCode::Tab => KeyValue::Named(NamedKey::Tab),
            KeyCode::BackTab => {
                return Some(KeyPress {
                    key: KeyValue::Named(NamedKey::BackTab),
                    modifiers: modifiers.with(Modifiers::SHIFT),
                });
            }
            KeyCode::Esc => KeyValue::Named(NamedKey::Escape),
            KeyCode::Insert => KeyValue::Named(NamedKey::Insert),
            KeyCode::Delete => KeyValue::Named(NamedKey::Delete),
            KeyCode::Home => KeyValue::Named(NamedKey::Home),
            KeyCode::End => KeyValue::Named(NamedKey::End),
            KeyCode::PageUp => KeyValue::Named(NamedKey::PageUp),
            KeyCode::PageDown => KeyValue::Named(NamedKey::PageDown),
            KeyCode::Up => KeyValue::Named(NamedKey::Up),
            KeyCode::Down => KeyValue::Named(NamedKey::Down),
            KeyCode::Left => KeyValue::Named(NamedKey::Left),
            KeyCode::Right => KeyValue::Named(NamedKey::Right),
            KeyCode::F(n) => KeyValue::Named(NamedKey::F(n)),
            KeyCode::Null => {
                return Some(KeyPress {
                    key: KeyValue::Char(' '),
                    modifiers: modifiers.with(Modifiers::CTRL),
                });
            }
            _ => return None,
        };

        Some(KeyPress { key, modifiers })
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
    if s.is_empty() {
        return None;
    }
    if s == "-" {
        return Some(KeyPress::new(KeyValue::Char('-')));
    }

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
        "minus" | "dash" => Some(KeyValue::Char('-')),
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
        "capslock" => Some(KeyValue::Named(NamedKey::CapsLock)),
        "scrolllock" => Some(KeyValue::Named(NamedKey::ScrollLock)),
        "numlock" => Some(KeyValue::Named(NamedKey::NumLock)),
        "printscreen" => Some(KeyValue::Named(NamedKey::PrintScreen)),
        "pause" => Some(KeyValue::Named(NamedKey::Pause)),
        "menu" => Some(KeyValue::Named(NamedKey::Menu)),
        other => {
            // Check for F-keys: "f1", "f12", etc.
            if let Some(n_str) = other.strip_prefix('f')
                && let Ok(n) = n_str.parse::<u8>()
                && (1..=24).contains(&n)
            {
                return Some(KeyValue::Named(NamedKey::F(n)));
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
    fn test_modifiers_empty_display() {
        let mods = Modifiers::NONE;
        assert_eq!(format!("{mods}"), "");
    }

    #[test]
    fn test_modifiers_with_and_without() {
        let mods = Modifiers::NONE
            .with(Modifiers::CTRL)
            .with(Modifiers::ALT)
            .with(Modifiers::SHIFT);
        assert!(mods.contains(Modifiers::CTRL));
        assert!(mods.contains(Modifiers::ALT));
        assert!(mods.contains(Modifiers::SHIFT));

        let mods2 = mods.without(Modifiers::ALT);
        assert!(mods2.contains(Modifiers::CTRL));
        assert!(!mods2.contains(Modifiers::ALT));
        assert!(mods2.contains(Modifiers::SHIFT));
    }

    #[test]
    fn test_modifiers_bits_roundtrip() {
        let mods = Modifiers::NONE.with(Modifiers::CTRL).with(Modifiers::SUPER);
        let bits = mods.bits();
        let mods2 = Modifiers::from_bits(bits);
        assert_eq!(mods, mods2);
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
    fn test_key_press_display_with_multiple_modifiers() {
        let kp = KeyPress::char_with_mods(
            'x',
            Modifiers::NONE
                .with(Modifiers::CTRL)
                .with(Modifiers::ALT)
                .with(Modifiers::SHIFT),
        );
        assert_eq!(format!("{kp}"), "Ctrl+Alt+Shift+x");
    }

    #[test]
    fn test_key_value_display() {
        assert_eq!(format!("{}", KeyValue::Char('a')), "a");
        assert_eq!(format!("{}", KeyValue::Char(' ')), "Space");
        assert_eq!(format!("{}", KeyValue::Named(NamedKey::Enter)), "Enter");
        assert_eq!(format!("{}", KeyValue::Named(NamedKey::F(12))), "F12");
    }

    #[test]
    fn test_parse_key_notation_simple_char() {
        assert_eq!(
            parse_key_notation("a"),
            Some(KeyPress::new(KeyValue::Char('a')))
        );
        assert_eq!(
            parse_key_notation("A"),
            Some(KeyPress::new(KeyValue::Char('A')))
        );
    }

    #[test]
    fn test_parse_key_notation_named_keys() {
        assert_eq!(
            parse_key_notation("Enter"),
            Some(KeyPress::new(KeyValue::Named(NamedKey::Enter)))
        );
        assert_eq!(
            parse_key_notation("Escape"),
            Some(KeyPress::new(KeyValue::Named(NamedKey::Escape)))
        );
        assert_eq!(
            parse_key_notation("esc"),
            Some(KeyPress::new(KeyValue::Named(NamedKey::Escape)))
        );
        assert_eq!(
            parse_key_notation("Space"),
            Some(KeyPress::new(KeyValue::Char(' ')))
        );
        assert_eq!(
            parse_key_notation("Backspace"),
            Some(KeyPress::new(KeyValue::Named(NamedKey::Backspace)))
        );
        assert_eq!(
            parse_key_notation("Tab"),
            Some(KeyPress::new(KeyValue::Named(NamedKey::Tab)))
        );
        assert_eq!(
            parse_key_notation("BackTab"),
            Some(KeyPress::new(KeyValue::Named(NamedKey::BackTab)))
        );
    }

    #[test]
    fn test_parse_key_notation_with_modifiers() {
        assert_eq!(
            parse_key_notation("ctrl+a"),
            Some(KeyPress::char_with_mods(
                'a',
                Modifiers::NONE.with(Modifiers::CTRL)
            ))
        );
        assert_eq!(
            parse_key_notation("alt+h"),
            Some(KeyPress::char_with_mods(
                'h',
                Modifiers::NONE.with(Modifiers::ALT)
            ))
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
    }

    #[test]
    fn test_parse_key_notation_hyphen_separator() {
        assert_eq!(
            parse_key_notation("ctrl-a"),
            Some(KeyPress::char_with_mods(
                'a',
                Modifiers::NONE.with(Modifiers::CTRL)
            ))
        );
    }

    #[test]
    fn test_parse_key_notation_f_keys() {
        assert_eq!(
            parse_key_notation("F1"),
            Some(KeyPress::new(KeyValue::Named(NamedKey::F(1))))
        );
        assert_eq!(
            parse_key_notation("f12"),
            Some(KeyPress::new(KeyValue::Named(NamedKey::F(12))))
        );
        assert_eq!(
            parse_key_notation("F24"),
            Some(KeyPress::new(KeyValue::Named(NamedKey::F(24))))
        );
        // F0 and F25 are out of range.
        assert_eq!(parse_key_notation("F0"), None);
        assert_eq!(parse_key_notation("F25"), None);
    }

    #[test]
    fn test_parse_key_notation_aliases() {
        assert_eq!(
            parse_key_notation("bs"),
            Some(KeyPress::new(KeyValue::Named(NamedKey::Backspace)))
        );
        assert_eq!(
            parse_key_notation("return"),
            Some(KeyPress::new(KeyValue::Named(NamedKey::Enter)))
        );
        assert_eq!(
            parse_key_notation("cr"),
            Some(KeyPress::new(KeyValue::Named(NamedKey::Enter)))
        );
        assert_eq!(
            parse_key_notation("ins"),
            Some(KeyPress::new(KeyValue::Named(NamedKey::Insert)))
        );
        assert_eq!(
            parse_key_notation("del"),
            Some(KeyPress::new(KeyValue::Named(NamedKey::Delete)))
        );
        assert_eq!(
            parse_key_notation("pgup"),
            Some(KeyPress::new(KeyValue::Named(NamedKey::PageUp)))
        );
        assert_eq!(
            parse_key_notation("pgdn"),
            Some(KeyPress::new(KeyValue::Named(NamedKey::PageDown)))
        );
        assert_eq!(
            parse_key_notation("pgdown"),
            Some(KeyPress::new(KeyValue::Named(NamedKey::PageDown)))
        );
    }

    #[test]
    fn test_parse_key_notation_modifier_aliases() {
        // "control" alias for "ctrl"
        assert_eq!(
            parse_key_notation("control+a"),
            Some(KeyPress::char_with_mods(
                'a',
                Modifiers::NONE.with(Modifiers::CTRL)
            ))
        );
        // "meta" alias for "alt"
        assert_eq!(
            parse_key_notation("meta+h"),
            Some(KeyPress::char_with_mods(
                'h',
                Modifiers::NONE.with(Modifiers::ALT)
            ))
        );
        // "super" modifier
        assert_eq!(
            parse_key_notation("super+a"),
            Some(KeyPress::char_with_mods(
                'a',
                Modifiers::NONE.with(Modifiers::SUPER)
            ))
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

    #[test]
    fn test_key_press_named_matching() {
        let kp = KeyPress::named_with_mods(NamedKey::Enter, Modifiers::NONE.with(Modifiers::CTRL));
        assert!(kp.is_named(NamedKey::Enter, Modifiers::CTRL));
        assert!(!kp.is_named(NamedKey::Enter, 0));
        assert!(!kp.is_named(NamedKey::Tab, Modifiers::CTRL));
    }

    #[test]
    fn test_key_press_hash_eq() {
        // Verify KeyPress can be used as HashMap key.
        use std::collections::HashMap;
        let mut map = HashMap::new();
        let kp = KeyPress::char_with_mods('a', Modifiers::NONE.with(Modifiers::CTRL));
        map.insert(kp, "ctrl+a action");
        assert_eq!(map.get(&kp), Some(&"ctrl+a action"));

        // Different key should not match.
        let kp2 = KeyPress::char_with_mods('b', Modifiers::NONE.with(Modifiers::CTRL));
        assert_eq!(map.get(&kp2), None);
    }

    #[test]
    fn test_named_key_display_all() {
        assert_eq!(format!("{}", NamedKey::Backspace), "Backspace");
        assert_eq!(format!("{}", NamedKey::Enter), "Enter");
        assert_eq!(format!("{}", NamedKey::Tab), "Tab");
        assert_eq!(format!("{}", NamedKey::BackTab), "BackTab");
        assert_eq!(format!("{}", NamedKey::Escape), "Escape");
        assert_eq!(format!("{}", NamedKey::Insert), "Insert");
        assert_eq!(format!("{}", NamedKey::Delete), "Delete");
        assert_eq!(format!("{}", NamedKey::Home), "Home");
        assert_eq!(format!("{}", NamedKey::End), "End");
        assert_eq!(format!("{}", NamedKey::PageUp), "PageUp");
        assert_eq!(format!("{}", NamedKey::PageDown), "PageDown");
        assert_eq!(format!("{}", NamedKey::Up), "Up");
        assert_eq!(format!("{}", NamedKey::Down), "Down");
        assert_eq!(format!("{}", NamedKey::Left), "Left");
        assert_eq!(format!("{}", NamedKey::Right), "Right");
        assert_eq!(format!("{}", NamedKey::CapsLock), "CapsLock");
        assert_eq!(format!("{}", NamedKey::ScrollLock), "ScrollLock");
        assert_eq!(format!("{}", NamedKey::NumLock), "NumLock");
        assert_eq!(format!("{}", NamedKey::PrintScreen), "PrintScreen");
        assert_eq!(format!("{}", NamedKey::Pause), "Pause");
        assert_eq!(format!("{}", NamedKey::Menu), "Menu");
    }
}
