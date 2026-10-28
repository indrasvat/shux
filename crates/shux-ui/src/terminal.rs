//! Terminal state management: raw mode, alternate screen, cleanup.
//!
//! [`TerminalGuard`] uses RAII to ensure the user's terminal is always
//! restored to its original state, even after a panic.

use std::io;
use std::time::Duration;

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
    /// Cleared by the first `leave`, so a later `leave` or `Drop` writes nothing.
    active: bool,
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
            active: true,
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
        if !std::mem::replace(&mut self.active, false) {
            return Ok(());
        }
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

    /// Restore without waiting on stdout, for an exit that must happen even
    /// when the terminal has stopped reading and another thread holds stdout
    /// blocked mid-write. Termios needs no stdout lock, so it is restored
    /// first; the escape sequences follow from a helper thread for at most
    /// `timeout`.
    pub fn leave_bounded(mut self, timeout: Duration) {
        if self.raw_mode_enabled {
            let _ = disable_raw_mode();
            self.raw_mode_enabled = false;
        }
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            // An interrupted frame may have left an APC string or a
            // synchronized update open; close both before restoring.
            let _ = execute!(io::stdout(), CloseInterruptedFrame);
            let _ = self.leave();
            let _ = done_tx.send(());
        });
        let _ = done_rx.recv_timeout(timeout);
    }

    /// Query the current terminal size.
    pub fn size() -> io::Result<(u16, u16)> {
        terminal::size()
    }
}

struct ResetCursorColor;

struct CloseInterruptedFrame;

impl Command for CloseInterruptedFrame {
    fn write_ansi(&self, f: &mut impl std::fmt::Write) -> std::fmt::Result {
        f.write_str("\x1b\\\x1b[?2026l")
    }
}

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
        let _ = execute!(io::stdout(), cursor::SetCursorStyle::DefaultUserShape);
        let _ = execute!(io::stdout(), ResetCursorColor);
        let _ = execute!(io::stdout(), cursor::Show);

        // Now print the panic info on the restored terminal
        default_hook(info);
    }));
}

/// The signals that end a TUI client. Their default action would kill the
/// process with the terminal still raw, so they are caught instead: the
/// caller restores the terminal and then re-raises.
///
/// Install BEFORE [`TerminalGuard::enter`], and keep [`ShutdownSignals::recv`]
/// in the select for as long as the terminal is raw: once installed, the
/// default action is gone for the life of the process.
pub struct ShutdownSignals {
    term: tokio::signal::unix::Signal,
    int: tokio::signal::unix::Signal,
    hup: tokio::signal::unix::Signal,
    quit: tokio::signal::unix::Signal,
}

impl ShutdownSignals {
    pub fn install() -> io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            term: signal(SignalKind::terminate())?,
            int: signal(SignalKind::interrupt())?,
            hup: signal(SignalKind::hangup())?,
            quit: signal(SignalKind::quit())?,
        })
    }

    /// Resolves with the raw number of the first shutdown signal received.
    pub async fn recv(&mut self) -> i32 {
        use tokio::signal::unix::SignalKind;
        let kind = tokio::select! {
            _ = self.term.recv() => SignalKind::terminate(),
            _ = self.int.recv() => SignalKind::interrupt(),
            _ = self.hup.recv() => SignalKind::hangup(),
            _ = self.quit.recv() => SignalKind::quit(),
        };
        let signo = kind.as_raw_value();
        tracing::info!(signo, "client: shutdown signal");
        signo
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
