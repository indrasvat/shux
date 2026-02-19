//! CLI output styling — consistent colors and formatting for shux CLI output.
//!
//! Color palette:
//! - Accent (cyan):    brand color, used for shux name and key identifiers
//! - Success (green):  confirmations, creation messages
//! - Warning (yellow): warnings, "not running" messages
//! - Error (red):      errors
//! - Muted (gray):     secondary info (IDs, timestamps, hints)
//! - Bold white:       primary content (session names, versions)

use std::fmt;
use std::io::{self, IsTerminal, Write};

use crossterm::style::{self, Attribute, Color, Stylize};

/// Whether to emit ANSI color codes. Auto-detects from stderr/stdout.
fn colors_enabled() -> bool {
    // Respect NO_COLOR convention (https://no-color.org)
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    io::stdout().is_terminal()
}

/// Styled text helper that respects NO_COLOR and terminal detection.
struct Styled {
    text: String,
    fg: Option<Color>,
    bold: bool,
    dim: bool,
}

impl Styled {
    fn new(text: impl fmt::Display) -> Self {
        Self {
            text: text.to_string(),
            fg: None,
            bold: false,
            dim: false,
        }
    }

    fn fg(mut self, color: Color) -> Self {
        self.fg = Some(color);
        self
    }

    fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    fn dim(mut self) -> Self {
        self.dim = true;
        self
    }
}

impl fmt::Display for Styled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !colors_enabled() {
            return write!(f, "{}", self.text);
        }

        let mut styled = style::style(&self.text);
        if let Some(color) = self.fg {
            styled = styled.with(color);
        }
        if self.bold {
            styled = styled.attribute(Attribute::Bold);
        }
        if self.dim {
            styled = styled.attribute(Attribute::Dim);
        }
        write!(f, "{styled}")
    }
}

// ── Banner ─────────────────────────────────────────────────────

/// Generate the shux ASCII art banner with a cyan→blue→indigo gradient.
/// Respects NO_COLOR and terminal detection.
pub fn banner() -> String {
    const ART: [&str; 6] = [
        r"      _               ",
        r" ___ | |__  _   ___  __",
        r"/ __|| '_ \| | | \ \/ /",
        r"\__ \| | | | |_| |>  < ",
        r"|___/|_| |_|\__,_/_/\_\",
        "",
    ];

    if !colors_enabled() {
        return ART.join("\n");
    }

    // Cyan → blue → indigo gradient (256-color)
    const GRADIENT: [u8; 5] = [51, 45, 39, 33, 27];

    let mut out = String::with_capacity(256);
    for (line, &color) in ART[..5].iter().zip(&GRADIENT) {
        out.push_str(&format!("\x1b[1;38;5;{color}m{line}\x1b[0m\n"));
    }
    out
}

// ── Public helpers ──────────────────────────────────────────────

/// Brand accent (cyan) — for "shux" name, key identifiers.
pub fn accent(text: impl fmt::Display) -> impl fmt::Display {
    Styled::new(text).fg(Color::Cyan).bold()
}

/// Success (green) — for creation/operation confirmations.
pub fn success(text: impl fmt::Display) -> impl fmt::Display {
    Styled::new(text).fg(Color::Green)
}

/// Warning (yellow) — for "not running", degraded states.
pub fn warning(text: impl fmt::Display) -> impl fmt::Display {
    Styled::new(text).fg(Color::Yellow)
}

/// Error (red) — for failures.
pub fn error(text: impl fmt::Display) -> impl fmt::Display {
    Styled::new(text).fg(Color::Red).bold()
}

/// Muted (gray/dim) — for IDs, timestamps, secondary info.
pub fn muted(text: impl fmt::Display) -> impl fmt::Display {
    Styled::new(text).dim()
}

/// Bold white — for primary content (session names, versions).
pub fn bold(text: impl fmt::Display) -> impl fmt::Display {
    Styled::new(text).bold()
}

/// Print the shux banner (used for version output).
pub fn print_version(version: &str, git_sha: Option<&str>, daemon_status: Option<&str>) {
    let mut out = io::stdout().lock();
    let _ = write!(out, "{} ", accent("shux"));
    let _ = write!(out, "{}", bold(version));
    if let Some(sha) = git_sha {
        let _ = write!(out, " {}", muted(format!("({sha})")));
    }
    if let Some(status) = daemon_status {
        let _ = write!(out, " {}", warning(format!("[{status}]")));
    }
    let _ = writeln!(out);
}

/// Print a session list entry.
pub fn print_session_entry(name: &str, windows: usize, created: &str, id: &str) {
    let mut out = io::stdout().lock();
    let _ = write!(out, "{}", bold(name));
    let _ = write!(
        out,
        ": {} window{}",
        windows,
        if windows == 1 { "" } else { "s" }
    );
    let _ = write!(out, " {}", muted(format!("(created {created})")));
    let _ = write!(out, " {}", muted(format!("[{id}]")));
    let _ = writeln!(out);
}

/// Print a "no sessions" message.
pub fn print_no_sessions() {
    println!("{}", muted("no sessions"));
}

/// Print a session creation confirmation.
pub fn print_session_created(name: &str, id: &str, ensured: bool) {
    let action = if ensured { "Ensured" } else { "Created" };
    let mut out = io::stdout().lock();
    let _ = write!(out, "{} ", success(action));
    let _ = write!(out, "session '{}'", bold(name));
    let _ = write!(out, " {}", muted(format!("[{id}]")));
    let _ = writeln!(out);
}

/// Print a session kill confirmation.
pub fn print_session_killed(name: &str) {
    let mut out = io::stdout().lock();
    let _ = write!(out, "{} ", success("Killed"));
    let _ = write!(out, "session '{}'", bold(name));
    let _ = writeln!(out);
}

/// Print a session rename confirmation.
pub fn print_session_renamed(old_name: &str, new_name: &str) {
    let mut out = io::stdout().lock();
    let _ = write!(out, "{} ", success("Renamed"));
    let _ = write!(out, "session '{}' -> '{}'", bold(old_name), bold(new_name));
    let _ = writeln!(out);
}

/// Print a window list entry.
pub fn print_window_entry(index: usize, title: &str, pane_count: usize, is_active: bool) {
    let mut out = io::stdout().lock();
    let _ = write!(out, "{}", index);
    if is_active {
        let _ = write!(out, "{}", accent("*"));
    } else {
        let _ = write!(out, " ");
    }
    let _ = write!(out, ": {}", bold(title));
    let _ = write!(
        out,
        " ({} pane{})",
        pane_count,
        if pane_count == 1 { "" } else { "s" }
    );
    let _ = writeln!(out);
}

/// Print a window creation confirmation.
pub fn print_window_created(title: &str, index: u64) {
    let mut out = io::stdout().lock();
    let _ = write!(out, "{} ", success("Created"));
    let _ = write!(out, "window '{}' ", bold(title));
    let _ = write!(out, "{}", muted(format!("(index {index})")));
    let _ = writeln!(out);
}

/// Print a window kill confirmation.
pub fn print_window_killed(title: &str) {
    let mut out = io::stdout().lock();
    let _ = write!(out, "{} ", success("Killed"));
    let _ = write!(out, "window '{}'", bold(title));
    let _ = writeln!(out);
}

/// Print a window rename confirmation.
pub fn print_window_renamed(old_name: &str, new_name: &str) {
    let mut out = io::stdout().lock();
    let _ = write!(out, "{} ", success("Renamed"));
    let _ = write!(out, "window '{}' -> '{}'", bold(old_name), bold(new_name));
    let _ = writeln!(out);
}

/// Print a window focus confirmation.
pub fn print_window_focused(title: &str) {
    let mut out = io::stdout().lock();
    let _ = write!(out, "{} ", success("Focused"));
    let _ = write!(out, "window '{}'", bold(title));
    let _ = writeln!(out);
}

/// Print a window reorder confirmation.
pub fn print_window_reordered(title: &str, new_index: usize) {
    let mut out = io::stdout().lock();
    let _ = write!(out, "{} ", success("Moved"));
    let _ = write!(out, "window '{}' to index {}", bold(title), new_index);
    let _ = writeln!(out);
}

/// Print a styled error message to stderr.
pub fn print_error(msg: &str) {
    let mut err = io::stderr().lock();
    let _ = write!(err, "{}: ", error("error"));
    let _ = writeln!(err, "{msg}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_styled_plain_text() {
        // With NO_COLOR set, should produce plain text
        let styled = Styled::new("hello").fg(Color::Red).bold();
        // Can't easily test terminal detection in unit tests,
        // but verify it doesn't panic
        let _ = styled.to_string();
    }

    #[test]
    fn test_accent_display() {
        let text = accent("shux");
        let _ = text.to_string();
    }

    #[test]
    fn test_muted_display() {
        let text = muted("[abc-123]");
        let _ = text.to_string();
    }

    #[test]
    fn test_banner_contains_shux() {
        let b = banner();
        assert!(b.contains("___"), "banner should contain ASCII art");
    }
}
