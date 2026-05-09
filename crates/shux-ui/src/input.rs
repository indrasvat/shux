//! Input event types and crossterm event translation.
//!
//! Reads raw crossterm events and normalizes them into shux's InputEvent
//! enum. Handles legacy input mode and Kitty keyboard protocol differences.

use crossterm::event::{
    Event as CtEvent, KeyCode as CtKeyCode, KeyEvent as CtKeyEvent, KeyEventKind,
    MouseButton as CtMouseButton, MouseEvent as CtMouseEvent, MouseEventKind as CtMouseEventKind,
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
    Resize { cols: u16, rows: u16 },
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeyboardProtocol {
    /// Legacy ANSI input decoding.
    #[default]
    Legacy,
    /// Kitty keyboard protocol (progressive enhancement flags active).
    Kitty,
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
fn translate_key_event(event: CtKeyEvent, _protocol: KeyboardProtocol) -> Option<InputEvent> {
    // In Kitty mode, crossterm reports key press, repeat, and release separately.
    // We only care about press and repeat events.
    // In legacy mode, crossterm only reports press events.
    match event.kind {
        KeyEventKind::Press | KeyEventKind::Repeat => {}
        KeyEventKind::Release => return None,
    }

    let modifiers = Modifiers::from_crossterm(event.modifiers);

    let key_value = match event.code {
        CtKeyCode::Char(c) => KeyValue::Char(c),
        CtKeyCode::Backspace => KeyValue::Named(NamedKey::Backspace),
        CtKeyCode::Enter => KeyValue::Named(NamedKey::Enter),
        CtKeyCode::Tab => KeyValue::Named(NamedKey::Tab),
        CtKeyCode::BackTab => {
            // BackTab is Shift+Tab. We normalize it by ensuring SHIFT is set.
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
    use crossterm::event::{KeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
    use crossterm::execute;

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
    use crossterm::event::PopKeyboardEnhancementFlags;
    use crossterm::execute;

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
/// Uses `crossterm::event::poll` with the given timeout.
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
            Some(InputEvent::Key(KeyPress::new(KeyValue::Named(
                NamedKey::F(5)
            ))))
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
    fn test_translate_backtab_without_shift() {
        // Even if crossterm doesn't send SHIFT with BackTab, we ensure it's set.
        let ct_event = CtEvent::Key(make_key_event(KeyCode::BackTab, KeyModifiers::NONE));
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
    fn test_translate_ctrl_space_null_without_ctrl() {
        // KeyCode::Null without CONTROL modifier should still get CTRL added.
        let ct_event = CtEvent::Key(make_key_event(KeyCode::Null, KeyModifiers::NONE));
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
        assert_eq!(
            result,
            Some(InputEvent::Resize {
                cols: 120,
                rows: 40
            })
        );
    }

    #[test]
    fn test_translate_paste() {
        let ct_event = CtEvent::Paste("hello world".to_string());
        let result = translate_event(ct_event, KeyboardProtocol::Legacy);
        assert_eq!(result, Some(InputEvent::Paste("hello world".to_string())));
    }

    #[test]
    fn test_translate_paste_empty() {
        let ct_event = CtEvent::Paste(String::new());
        let result = translate_event(ct_event, KeyboardProtocol::Legacy);
        assert_eq!(result, Some(InputEvent::Paste(String::new())));
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
                assert!(me.modifiers.is_empty());
            }
            _ => panic!("expected mouse event"),
        }
    }

    #[test]
    fn test_translate_mouse_release() {
        let ct_event = CtEvent::Mouse(CtMouseEvent {
            kind: CtMouseEventKind::Up(CtMouseButton::Right),
            column: 3,
            row: 7,
            modifiers: KeyModifiers::NONE,
        });
        let result = translate_event(ct_event, KeyboardProtocol::Legacy);
        match result {
            Some(InputEvent::Mouse(me)) => {
                assert_eq!(me.action, MouseAction::Release(MouseButton::Right));
                assert_eq!(me.col, 3);
                assert_eq!(me.row, 7);
            }
            _ => panic!("expected mouse event"),
        }
    }

    #[test]
    fn test_translate_mouse_drag() {
        let ct_event = CtEvent::Mouse(CtMouseEvent {
            kind: CtMouseEventKind::Drag(CtMouseButton::Middle),
            column: 15,
            row: 20,
            modifiers: KeyModifiers::NONE,
        });
        let result = translate_event(ct_event, KeyboardProtocol::Legacy);
        match result {
            Some(InputEvent::Mouse(me)) => {
                assert_eq!(me.action, MouseAction::Drag(MouseButton::Middle));
                assert_eq!(me.col, 15);
                assert_eq!(me.row, 20);
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
    fn test_translate_scroll_down() {
        let ct_event = CtEvent::Mouse(CtMouseEvent {
            kind: CtMouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        let result = translate_event(ct_event, KeyboardProtocol::Legacy);
        match result {
            Some(InputEvent::Mouse(me)) => {
                assert_eq!(me.action, MouseAction::ScrollDown);
            }
            _ => panic!("expected mouse event"),
        }
    }

    #[test]
    fn test_translate_scroll_left_right() {
        let ct_left = CtEvent::Mouse(CtMouseEvent {
            kind: CtMouseEventKind::ScrollLeft,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        let result_left = translate_event(ct_left, KeyboardProtocol::Legacy);
        match result_left {
            Some(InputEvent::Mouse(me)) => {
                assert_eq!(me.action, MouseAction::ScrollLeft);
            }
            _ => panic!("expected mouse event"),
        }

        let ct_right = CtEvent::Mouse(CtMouseEvent {
            kind: CtMouseEventKind::ScrollRight,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        let result_right = translate_event(ct_right, KeyboardProtocol::Legacy);
        match result_right {
            Some(InputEvent::Mouse(me)) => {
                assert_eq!(me.action, MouseAction::ScrollRight);
            }
            _ => panic!("expected mouse event"),
        }
    }

    #[test]
    fn test_translate_mouse_move() {
        let ct_event = CtEvent::Mouse(CtMouseEvent {
            kind: CtMouseEventKind::Moved,
            column: 50,
            row: 25,
            modifiers: KeyModifiers::NONE,
        });
        let result = translate_event(ct_event, KeyboardProtocol::Legacy);
        match result {
            Some(InputEvent::Mouse(me)) => {
                assert_eq!(me.action, MouseAction::Move);
                assert_eq!(me.col, 50);
                assert_eq!(me.row, 25);
            }
            _ => panic!("expected mouse event"),
        }
    }

    #[test]
    fn test_translate_mouse_with_modifiers() {
        let ct_event = CtEvent::Mouse(CtMouseEvent {
            kind: CtMouseEventKind::Down(CtMouseButton::Left),
            column: 5,
            row: 3,
            modifiers: KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        });
        let result = translate_event(ct_event, KeyboardProtocol::Legacy);
        match result {
            Some(InputEvent::Mouse(me)) => {
                assert_eq!(me.action, MouseAction::Press(MouseButton::Left));
                assert!(me.modifiers.contains(Modifiers::CTRL));
                assert!(me.modifiers.contains(Modifiers::SHIFT));
                assert!(!me.modifiers.contains(Modifiers::ALT));
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

    #[test]
    fn test_translate_named_keys() {
        let test_cases = vec![
            (KeyCode::Backspace, NamedKey::Backspace),
            (KeyCode::Enter, NamedKey::Enter),
            (KeyCode::Tab, NamedKey::Tab),
            (KeyCode::Esc, NamedKey::Escape),
            (KeyCode::Insert, NamedKey::Insert),
            (KeyCode::Delete, NamedKey::Delete),
            (KeyCode::Home, NamedKey::Home),
            (KeyCode::End, NamedKey::End),
            (KeyCode::PageUp, NamedKey::PageUp),
            (KeyCode::PageDown, NamedKey::PageDown),
            (KeyCode::Up, NamedKey::Up),
            (KeyCode::Down, NamedKey::Down),
            (KeyCode::Left, NamedKey::Left),
            (KeyCode::Right, NamedKey::Right),
            (KeyCode::CapsLock, NamedKey::CapsLock),
            (KeyCode::ScrollLock, NamedKey::ScrollLock),
            (KeyCode::NumLock, NamedKey::NumLock),
            (KeyCode::PrintScreen, NamedKey::PrintScreen),
            (KeyCode::Pause, NamedKey::Pause),
            (KeyCode::Menu, NamedKey::Menu),
        ];

        for (ct_code, expected_named) in test_cases {
            let ct_event = CtEvent::Key(make_key_event(ct_code, KeyModifiers::NONE));
            let result = translate_event(ct_event, KeyboardProtocol::Legacy);
            assert_eq!(
                result,
                Some(InputEvent::Key(KeyPress::new(KeyValue::Named(
                    expected_named
                )))),
                "failed for key code {:?}",
                ct_code
            );
        }
    }

    #[test]
    fn test_translate_f_keys_range() {
        for n in 1..=12 {
            let ct_event = CtEvent::Key(make_key_event(KeyCode::F(n), KeyModifiers::NONE));
            let result = translate_event(ct_event, KeyboardProtocol::Legacy);
            assert_eq!(
                result,
                Some(InputEvent::Key(KeyPress::new(KeyValue::Named(
                    NamedKey::F(n)
                )))),
                "failed for F{n}"
            );
        }
    }

    #[test]
    fn test_keyboard_protocol_default() {
        assert_eq!(KeyboardProtocol::default(), KeyboardProtocol::Legacy);
    }

    #[test]
    fn test_translate_special_characters() {
        // Test various special characters.
        for ch in ['/', '\\', '.', ',', ';', ':', '!', '@', '#', '$'] {
            let ct_event = CtEvent::Key(make_key_event(KeyCode::Char(ch), KeyModifiers::NONE));
            let result = translate_event(ct_event, KeyboardProtocol::Legacy);
            assert_eq!(
                result,
                Some(InputEvent::Key(KeyPress::new(KeyValue::Char(ch)))),
                "failed for char '{ch}'"
            );
        }
    }

    #[test]
    fn test_translate_shift_char() {
        // Shift+a should come as 'A' with SHIFT modifier in some terminals.
        let ct_event = CtEvent::Key(make_key_event(KeyCode::Char('A'), KeyModifiers::SHIFT));
        let result = translate_event(ct_event, KeyboardProtocol::Legacy);
        assert_eq!(
            result,
            Some(InputEvent::Key(KeyPress::char_with_mods(
                'A',
                Modifiers::NONE.with(Modifiers::SHIFT)
            )))
        );
    }

    #[test]
    fn test_translate_all_modifier_combinations() {
        let ct_event = CtEvent::Key(make_key_event(
            KeyCode::Char('z'),
            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT | KeyModifiers::SUPER,
        ));
        let result = translate_event(ct_event, KeyboardProtocol::Legacy);
        let expected = InputEvent::Key(KeyPress {
            key: KeyValue::Char('z'),
            modifiers: Modifiers::NONE
                .with(Modifiers::CTRL)
                .with(Modifiers::ALT)
                .with(Modifiers::SHIFT)
                .with(Modifiers::SUPER),
        });
        assert_eq!(result, Some(expected));
    }
}
