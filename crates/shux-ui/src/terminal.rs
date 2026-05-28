//! Terminal state management: raw mode, alternate screen, cleanup.
//!
//! [`TerminalGuard`] uses RAII to ensure the user's terminal is always
//! restored to its original state, even after a panic.

use std::io;

use crossterm::{
    Command, cursor,
    event::{
        DisableMouseCapture, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{
        self, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    },
};

/// Tracks the state of the host terminal so we can restore it correctly
/// on exit, even after a panic.
pub struct TerminalGuard {
    raw_mode_enabled: bool,
    alternate_screen: bool,
    mouse_capture: bool,
    kitty_keyboard: bool,
}

impl TerminalGuard {
    /// Enter the TUI state: raw mode + alternate screen.
    /// Returns a guard that will restore the terminal on drop.
    pub fn enter() -> io::Result<Self> {
        let mut guard = Self {
            raw_mode_enabled: false,
            alternate_screen: false,
            mouse_capture: false,
            kitty_keyboard: false,
        };

        // Order matters: enable raw mode first so that escape sequences
        // for alternate screen are not echoed as text.
        enable_raw_mode()?;
        guard.raw_mode_enabled = true;

        // Switch to alternate screen (preserves the user's scrollback)
        execute!(io::stdout(), EnterAlternateScreen)?;
        guard.alternate_screen = true;

        // Enable only the mouse modes shux consumes: press/release,
        // button-drag, and SGR coordinates. Crossterm's
        // EnableMouseCapture also enables ?1003 any-motion tracking,
        // which shux does not use and which makes native
        // modifier-drag selection less reliable in common terminals.
        execute!(io::stdout(), EnableButtonMouseCapture)?;
        guard.mouse_capture = true;

        // Try to enable Kitty keyboard protocol for improved key detection.
        // This silently fails on terminals that do not support it.
        let result = execute!(
            io::stdout(),
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
            )
        );
        if result.is_ok() {
            guard.kitty_keyboard = true;
        }

        Ok(guard)
    }

    /// Restore the terminal to its original state. This is also called
    /// automatically by Drop, but calling it explicitly allows error handling.
    pub fn leave(&mut self) -> io::Result<()> {
        if self.kitty_keyboard {
            let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
            self.kitty_keyboard = false;
        }

        if self.mouse_capture {
            let _ = execute!(io::stdout(), DisableMouseCapture);
            self.mouse_capture = false;
        }

        if self.alternate_screen {
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            self.alternate_screen = false;
        }

        if self.raw_mode_enabled {
            let _ = disable_raw_mode();
            self.raw_mode_enabled = false;
        }

        // Restore cursor presentation in case a pane requested a custom
        // shape or OSC 12 cursor color during rendering.
        let _ = execute!(io::stdout(), cursor::SetCursorStyle::DefaultUserShape);
        let _ = execute!(io::stdout(), ResetCursorColor);
        let _ = execute!(io::stdout(), cursor::Show);

        Ok(())
    }

    /// Query the current terminal size.
    pub fn size() -> io::Result<(u16, u16)> {
        terminal::size()
    }
}

struct ResetCursorColor;

impl Command for ResetCursorColor {
    fn write_ansi(&self, f: &mut impl std::fmt::Write) -> std::fmt::Result {
        f.write_str("\x1b]112\x1b\\")
    }
}

/// Mouse capture profile used by shux attach.
///
/// Crossterm's `EnableMouseCapture` intentionally turns on every
/// common xterm mouse mode, including `?1003h` any-motion. shux only
/// needs button press/release and button-held drags for pane focus and
/// border resize, so avoid any-motion to keep host-terminal selection
/// escape hatches (Shift/Option drag, depending on terminal) working
/// where the terminal supports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EnableButtonMouseCapture;

impl Command for EnableButtonMouseCapture {
    fn write_ansi(&self, f: &mut impl std::fmt::Write) -> std::fmt::Result {
        f.write_str(concat!(
            "\x1b[?1000h", // normal tracking: button press/release
            "\x1b[?1002h", // button-event tracking: drag while pressed
            "\x1b[?1006h", // SGR coordinates for large terminals
        ))
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        // Match crossterm's Windows fallback; the ANSI profile above
        // only applies on terminals that understand xterm modes.
        crossterm::event::EnableMouseCapture.execute_winapi()
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        false
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Best-effort restore on drop. Errors are silently ignored because
        // we may be in a panic handler and cannot propagate errors.
        let _ = self.leave();
    }
}

/// Install a panic hook that restores the terminal before printing the
/// panic message. Without this, a panic leaves the terminal in raw mode
/// and the error message is invisible.
pub fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Best-effort terminal restore
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        let _ = execute!(io::stdout(), DisableMouseCapture);
        let _ = execute!(io::stdout(), cursor::Show);

        // Now print the panic info on the restored terminal
        default_hook(info);
    }));
}

/// Set up signal handlers that trigger graceful shutdown.
/// Returns a future that resolves when a shutdown signal is received.
pub async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sigterm = signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt()).expect("failed to install SIGINT handler");

    tokio::select! {
        _ = sigterm.recv() => {
            tracing::info!("Received SIGTERM");
        }
        _ = sigint.recv() => {
            tracing::info!("Received SIGINT");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_terminal_guard_is_send() {
        // Verify TerminalGuard can be sent across threads (needed for
        // tokio tasks)
        fn assert_send<T: Send>() {}
        assert_send::<TerminalGuard>();
    }

    #[test]
    fn test_terminal_size() {
        // This test may fail in CI without a real terminal, so we just
        // check it does not panic.
        let _ = TerminalGuard::size();
    }

    #[test]
    fn button_mouse_capture_does_not_enable_any_motion() {
        let mut ansi = String::new();
        EnableButtonMouseCapture.write_ansi(&mut ansi).unwrap();
        assert!(ansi.contains("\x1b[?1000h"));
        assert!(ansi.contains("\x1b[?1002h"));
        assert!(ansi.contains("\x1b[?1006h"));
        assert!(
            !ansi.contains("\x1b[?1003h"),
            "shux should not enable any-motion tracking"
        );
    }
}
