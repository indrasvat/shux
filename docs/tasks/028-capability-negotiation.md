# 028 — Capability Negotiation (ClientCaps)

**Status:** Partial. The daemon claims `TERM_PROGRAM=shux` / `TERM_PROGRAM_VERSION=<pkg ver>` / `COLORTERM=truecolor` / `SHUX=1` on every PTY spawn (so user rc files don't mis-route), and panes now answer common xterm application probes (DA/DA2/DSR, OSC color queries, XTGETTCAP, and DECRQSS) through the PTY response path. The active client tracks size via the attach `Resize` frame. Full attached-client cap negotiation (XTVERSION, Kitty keyboard query, OSC 4 palette probe stored as a per-client `ClientCaps`) is still pending — synchronized output (Mode 2026) currently fires unconditionally.
**Depends On:** 010
**Parallelizable With:** 022, 024

---

## Problem

shux runs inside a wide range of host terminals: Ghostty, iTerm2, WezTerm, Kitty, Alacritty, macOS Terminal, xterm, and more. Each has different capabilities: truecolor support, Kitty keyboard protocol, synchronized output, OSC 52 clipboard, image passthrough. shux must detect these capabilities at attach time and gate features accordingly. Without capability negotiation, shux would either (a) use the lowest common denominator (ugly, missing features) or (b) emit unsupported escape sequences (garbled output, broken behavior).

The solution is a `ClientCaps` struct built at attach time for each client. Different clients attached to the same daemon may have different capabilities (e.g., one user attaches from Kitty, another from macOS Terminal). Capabilities are determined through a combination of fast environment variable heuristics and escape sequence probes with timeouts.

## PRD Reference

- **SS 6.1** P0 Feature Matrix, Terminal compatibility: "Capability negotiation: At attach: detect TERM, TERM_PROGRAM, COLORTERM, DA2, XTVERSION, Kitty keyboard query. Store as ClientCaps per client."
- **SS 12.1** Capability detection strategy: Fast heuristics (env vars) + escape sequence probes (200ms timeout)
- **SS 12.2** Enhanced features: Truecolor, Kitty keyboard, OSC 52, Synchronized output, Image passthrough — all gated on ClientCaps
- **SS 14.1** Performance: Attach (<10 panes) <= 150ms — capability detection must not blow this budget

---

## Files to Create

- `crates/shux-ui/src/caps.rs` — `ClientCaps` struct definition, detection logic, probe runners
- `crates/shux-ui/src/caps/probes.rs` — Individual escape sequence probe implementations
- `crates/shux-core/src/client.rs` — Client model with ClientCaps storage, per-client tracking

## Files to Modify

- `crates/shux-ui/src/lib.rs` — Add `pub mod caps;`
- `crates/shux-ui/Cargo.toml` — Add dependencies for terminal probing if needed
- `crates/shux-core/src/lib.rs` — Add `pub mod client;`
- `crates/shux-core/src/event.rs` — Add `ClientConnected` event variant with capabilities
- `crates/shux-core/src/model.rs` — Add client tracking to session state

---

## Execution Steps

### Step 1: Define the ClientCaps Struct

Create `crates/shux-ui/src/caps.rs`:

```rust
//! Terminal capability negotiation.
//!
//! At client attach time, shux probes the host terminal to determine its
//! capabilities. This allows shux to use advanced features (truecolor,
//! Kitty keyboard protocol, synchronized output) when available, and
//! fall back gracefully when not.
//!
//! Detection strategy (PRD SS 12.1):
//! 1. Fast heuristics: check TERM, TERM_PROGRAM, TERM_PROGRAM_VERSION, COLORTERM
//! 2. Escape sequence probes (200ms timeout each): DA2, XTVERSION, Kitty keyboard, DECRQM
//! 3. Cache per-client (different attached clients may have different caps)

mod probes;

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// The timeout for each individual escape sequence probe.
const PROBE_TIMEOUT: Duration = Duration::from_millis(200);

/// Maximum total time allowed for all probes combined.
/// This ensures attach stays within the 150ms attach budget even if
/// all probes are run sequentially. In practice, probes that time out
/// are assumed to be unsupported.
const TOTAL_PROBE_BUDGET: Duration = Duration::from_millis(400);

/// Terminal color depth support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColorDepth {
    /// 24-bit truecolor (16.7M colors)
    TrueColor,
    /// 256-color palette (xterm-256color)
    Color256,
    /// Basic 8-color ANSI palette
    Color8,
}

impl Default for ColorDepth {
    fn default() -> Self {
        ColorDepth::Color256 // Safe default
    }
}

/// Capabilities of the host terminal, detected at attach time.
///
/// Each field represents a feature that may or may not be supported
/// by the client's terminal emulator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientCaps {
    /// Color depth supported by the terminal.
    pub color_depth: ColorDepth,

    /// Whether the Kitty keyboard protocol is supported.
    /// When true, shux can use `CSI > flags u` for enhanced keyboard input
    /// (disambiguate keys, report key releases, report all keys as escape codes).
    pub kitty_keyboard: bool,

    /// Whether OSC 52 clipboard access is supported.
    /// When true, shux can read/write the system clipboard via escape sequences.
    pub osc52: bool,

    /// Whether synchronized output (Mode 2026) is supported.
    /// When true, shux wraps render frames in BeginSynchronizedUpdate/EndSynchronizedUpdate
    /// to prevent flicker.
    pub synchronized_output: bool,

    /// Whether the terminal supports image display protocols.
    /// Determines which image passthrough method to use.
    pub image_protocol: ImageProtocol,

    /// The name of the terminal emulator (e.g., "iTerm2", "Kitty", "Ghostty").
    pub terminal_name: String,

    /// The version of the terminal emulator (e.g., "3.5.0").
    pub terminal_version: String,

    /// The value of the TERM environment variable.
    pub term: String,

    /// Whether the terminal supports OSC 8 hyperlinks.
    pub osc8_hyperlinks: bool,

    /// Whether the terminal supports Unicode (including wide chars, emoji).
    pub unicode: bool,

    /// Raw probe results for debugging (included in `shux doctor` output).
    pub probe_results: ProbeResults,
}

/// Image display protocol support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageProtocol {
    /// No image support detected
    None,
    /// Kitty graphics protocol
    Kitty,
    /// Sixel graphics
    Sixel,
    /// iTerm2 inline images
    Iterm2,
}

impl Default for ImageProtocol {
    fn default() -> Self {
        ImageProtocol::None
    }
}

/// Raw results from escape sequence probes, for diagnostics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProbeResults {
    /// DA2 (Device Attributes) response, if any
    pub da2_response: Option<String>,
    /// XTVERSION response, if any
    pub xtversion_response: Option<String>,
    /// Whether Kitty keyboard probe got a valid response
    pub kitty_keyboard_probed: bool,
    /// DECRQM Mode 2026 response: 1=set, 2=reset, 0=not recognized
    pub decrqm_2026_response: Option<u8>,
    /// Time taken for all probes (milliseconds)
    pub probe_duration_ms: u64,
}

impl Default for ClientCaps {
    fn default() -> Self {
        Self {
            color_depth: ColorDepth::Color256,
            kitty_keyboard: false,
            osc52: false,
            synchronized_output: false,
            image_protocol: ImageProtocol::None,
            terminal_name: String::new(),
            terminal_version: String::new(),
            term: String::new(),
            osc8_hyperlinks: false,
            unicode: true, // Assume modern terminals support Unicode
            probe_results: ProbeResults::default(),
        }
    }
}

impl ClientCaps {
    /// Detect terminal capabilities.
    ///
    /// This is called once at client attach time. It first checks environment
    /// variables for fast heuristics, then runs escape sequence probes for
    /// definitive answers.
    pub async fn detect() -> Self {
        let start = std::time::Instant::now();
        let mut caps = Self::default();

        // Phase 1: Environment variable heuristics (instant, no I/O)
        caps.detect_from_env();

        // Phase 2: Escape sequence probes (up to TOTAL_PROBE_BUDGET)
        caps.run_probes().await;

        caps.probe_results.probe_duration_ms = start.elapsed().as_millis() as u64;

        tracing::info!(
            terminal = %caps.terminal_name,
            color_depth = ?caps.color_depth,
            kitty_keyboard = caps.kitty_keyboard,
            osc52 = caps.osc52,
            synchronized_output = caps.synchronized_output,
            probe_ms = caps.probe_results.probe_duration_ms,
            "Terminal capabilities detected"
        );

        caps
    }

    /// Phase 1: Detect capabilities from environment variables.
    fn detect_from_env(&mut self) {
        // TERM
        self.term = std::env::var("TERM").unwrap_or_default();

        // COLORTERM — most reliable indicator of truecolor
        let colorterm = std::env::var("COLORTERM").unwrap_or_default();
        if colorterm == "truecolor" || colorterm == "24bit" {
            self.color_depth = ColorDepth::TrueColor;
        } else if self.term.contains("256color") {
            self.color_depth = ColorDepth::Color256;
        }

        // TERM_PROGRAM — identify the terminal emulator
        let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();
        let term_version = std::env::var("TERM_PROGRAM_VERSION").unwrap_or_default();
        self.terminal_name = term_program.clone();
        self.terminal_version = term_version.clone();

        // Terminal-specific heuristics
        match term_program.to_lowercase().as_str() {
            "iterm2" | "iterm.app" => {
                self.color_depth = ColorDepth::TrueColor;
                self.osc52 = true;
                self.osc8_hyperlinks = true;
                self.synchronized_output = true;
                self.image_protocol = ImageProtocol::Iterm2;
            }
            "kitty" => {
                self.color_depth = ColorDepth::TrueColor;
                self.kitty_keyboard = true;
                self.osc52 = true;
                self.osc8_hyperlinks = true;
                self.synchronized_output = true;
                self.image_protocol = ImageProtocol::Kitty;
            }
            "wezterm" => {
                self.color_depth = ColorDepth::TrueColor;
                self.kitty_keyboard = true;
                self.osc52 = true;
                self.osc8_hyperlinks = true;
                self.synchronized_output = true;
                self.image_protocol = ImageProtocol::Iterm2; // WezTerm supports iTerm2 protocol
            }
            "ghostty" => {
                self.color_depth = ColorDepth::TrueColor;
                self.kitty_keyboard = true;
                self.osc52 = true;
                self.osc8_hyperlinks = true;
                self.synchronized_output = true;
                self.image_protocol = ImageProtocol::Kitty;
            }
            "alacritty" => {
                self.color_depth = ColorDepth::TrueColor;
                self.osc52 = true;
                // Alacritty does not support Kitty keyboard protocol
                // No image protocol support
            }
            "apple_terminal" | "terminal" => {
                self.color_depth = ColorDepth::Color256;
                // macOS Terminal has limited capabilities
                self.kitty_keyboard = false;
                self.osc52 = false;
                self.synchronized_output = false;
            }
            _ => {
                // Unknown terminal — rely on probes
            }
        }

        // VTE-based terminals (GNOME Terminal, Tilix, etc.)
        if std::env::var("VTE_VERSION").is_ok() {
            self.color_depth = ColorDepth::TrueColor;
            self.osc52 = true; // VTE >= 0.50 supports OSC 52
        }
    }

    /// Phase 2: Run escape sequence probes to confirm/override heuristics.
    async fn run_probes(&mut self) {
        let deadline = tokio::time::Instant::now() + TOTAL_PROBE_BUDGET;

        // Probe 1: DA2 (Device Attributes level 2)
        // Response: CSI > Pp ; Pv ; Pc c
        if let Ok(response) = tokio::time::timeout_at(
            deadline,
            probes::probe_da2(),
        )
        .await
        {
            if let Some(da2) = response {
                self.probe_results.da2_response = Some(da2.clone());
                probes::interpret_da2(&da2, self);
            }
        }

        // Probe 2: XTVERSION
        // Response: DCS > | <version> ST
        if tokio::time::Instant::now() < deadline {
            if let Ok(response) = tokio::time::timeout_at(
                deadline,
                probes::probe_xtversion(),
            )
            .await
            {
                if let Some(version) = response {
                    self.probe_results.xtversion_response = Some(version.clone());
                    probes::interpret_xtversion(&version, self);
                }
            }
        }

        // Probe 3: Kitty keyboard protocol
        // Send: CSI ? u
        // Response: CSI ? <flags> u (if supported)
        if tokio::time::Instant::now() < deadline && !self.kitty_keyboard {
            if let Ok(supported) = tokio::time::timeout_at(
                deadline,
                probes::probe_kitty_keyboard(),
            )
            .await
            {
                self.kitty_keyboard = supported;
                self.probe_results.kitty_keyboard_probed = true;
            }
        }

        // Probe 4: DECRQM for Mode 2026 (synchronized output)
        // Send: CSI ? 2026 $ p
        // Response: CSI ? 2026 ; Ps $ y  (Ps: 1=set, 2=reset, 0=not recognized)
        if tokio::time::Instant::now() < deadline && !self.synchronized_output {
            if let Ok(response) = tokio::time::timeout_at(
                deadline,
                probes::probe_decrqm_2026(),
            )
            .await
            {
                if let Some(mode) = response {
                    self.probe_results.decrqm_2026_response = Some(mode);
                    // Mode recognized (1=set or 2=reset) means the terminal
                    // supports synchronized output
                    self.synchronized_output = mode == 1 || mode == 2;
                }
            }
        }
    }

    /// Check if a specific feature is available.
    pub fn supports_truecolor(&self) -> bool {
        self.color_depth == ColorDepth::TrueColor
    }

    pub fn supports_kitty_keyboard(&self) -> bool {
        self.kitty_keyboard
    }

    pub fn supports_synchronized_output(&self) -> bool {
        self.synchronized_output
    }

    pub fn supports_osc52(&self) -> bool {
        self.osc52
    }
}
```

### Step 2: Implement Escape Sequence Probes

Create `crates/shux-ui/src/caps/probes.rs`:

```rust
//! Escape sequence probes for terminal capability detection.
//!
//! Each probe sends an escape sequence to the terminal and waits for a response.
//! Probes have individual timeouts (200ms) to avoid blocking on terminals that
//! don't respond to unrecognized sequences.

use super::{ClientCaps, ColorDepth, ImageProtocol};
use crossterm::event::{self, Event, KeyCode};
use std::io::Write;

/// Send DA2 (Device Attributes level 2) query and parse response.
///
/// Query:  ESC [ > c   (or ESC [ > 0 c)
/// Response: ESC [ > Pp ; Pv ; Pc c
///   Pp = terminal type, Pv = firmware version, Pc = ROM cartridge ID
pub async fn probe_da2() -> Option<String> {
    // Send the DA2 query
    let mut stdout = std::io::stdout();
    stdout.write_all(b"\x1b[>c").ok()?;
    stdout.flush().ok()?;

    // Read response — this is terminal-specific
    // In practice, we read from the terminal's input stream
    // The response arrives as input characters
    read_terminal_response(b"\x1b[>", b"c", super::PROBE_TIMEOUT).await
}

/// Send XTVERSION query and parse response.
///
/// Query:  CSI > 0 q   (or DCS + q 7 8 7 4 7 6 6 5 7 2 7 3 6 9 6 f 6 e ST)
/// Response: DCS > | <version-string> ST
pub async fn probe_xtversion() -> Option<String> {
    let mut stdout = std::io::stdout();
    // CSI > 0 q
    stdout.write_all(b"\x1b[>0q").ok()?;
    stdout.flush().ok()?;

    // Response format: DCS > | <text> ST
    // DCS = ESC P, ST = ESC \
    read_terminal_response(b"\x1bP>|", b"\x1b\\", super::PROBE_TIMEOUT).await
}

/// Probe for Kitty keyboard protocol support.
///
/// Query:  CSI ? u
/// Response: CSI ? <flags> u  (if supported; no response if not)
pub async fn probe_kitty_keyboard() -> bool {
    let mut stdout = std::io::stdout();
    stdout.write_all(b"\x1b[?u").ok()?;
    stdout.flush().ok()?;

    // If we get a CSI ? <number> u response, the protocol is supported
    if let Some(response) =
        read_terminal_response(b"\x1b[?", b"u", super::PROBE_TIMEOUT).await
    {
        // Any valid numeric response means support
        response.chars().all(|c| c.is_ascii_digit())
    } else {
        false
    }
}

/// Probe for synchronized output support via DECRQM.
///
/// Query:  CSI ? 2026 $ p
/// Response: CSI ? 2026 ; Ps $ y
///   Ps: 0 = not recognized, 1 = set, 2 = reset, 3 = permanently set, 4 = permanently reset
pub async fn probe_decrqm_2026() -> Option<u8> {
    let mut stdout = std::io::stdout();
    stdout.write_all(b"\x1b[?2026$p").ok()?;
    stdout.flush().ok()?;

    // Response: CSI ? 2026 ; <Ps> $ y
    if let Some(response) =
        read_terminal_response(b"\x1b[?2026;", b"$y", super::PROBE_TIMEOUT).await
    {
        response.parse::<u8>().ok()
    } else {
        None
    }
}

/// Interpret a DA2 response to extract terminal information.
pub fn interpret_da2(response: &str, caps: &mut ClientCaps) {
    // Response format: "Pp;Pv;Pc" (numbers separated by semicolons)
    let parts: Vec<&str> = response.split(';').collect();
    if parts.len() >= 2 {
        // Pp (terminal type):
        //   0 = VT100, 1 = VT220, 41 = xterm, 65 = VT500
        //   Some terminals use their own codes (e.g., iTerm2 = 0)
        if let Ok(terminal_type) = parts[0].parse::<u32>() {
            match terminal_type {
                41 => {
                    // xterm or xterm-compatible
                    if caps.terminal_name.is_empty() {
                        caps.terminal_name = "xterm".to_string();
                    }
                }
                _ => {}
            }
        }
        // Pv (firmware version) — store for diagnostics
        if caps.terminal_version.is_empty() {
            caps.terminal_version = parts[1].to_string();
        }
    }
}

/// Interpret an XTVERSION response to extract terminal name and version.
pub fn interpret_xtversion(response: &str, caps: &mut ClientCaps) {
    // Response format varies: "xterm(388)", "Kitty(0.36.4)", "tmux 3.6"
    // Extract name and version
    let response = response.trim();

    if let Some(paren_pos) = response.find('(') {
        let name = &response[..paren_pos];
        let version = response[paren_pos + 1..].trim_end_matches(')');
        if caps.terminal_name.is_empty() {
            caps.terminal_name = name.to_string();
        }
        if caps.terminal_version.is_empty() {
            caps.terminal_version = version.to_string();
        }
    } else if let Some(space_pos) = response.find(' ') {
        let name = &response[..space_pos];
        let version = &response[space_pos + 1..];
        if caps.terminal_name.is_empty() {
            caps.terminal_name = name.to_string();
        }
        if caps.terminal_version.is_empty() {
            caps.terminal_version = version.to_string();
        }
    }
}

/// Read a terminal response with a given prefix and suffix.
///
/// This reads bytes from the terminal input, looking for a response that
/// starts with `prefix` and ends with `suffix`. Returns the content between
/// prefix and suffix.
///
/// In practice, this uses crossterm's raw mode event reading, since the
/// terminal's response arrives as pseudo-input events.
async fn read_terminal_response(
    _prefix: &[u8],
    _suffix: &[u8],
    timeout: std::time::Duration,
) -> Option<String> {
    // Implementation note:
    // Terminal responses arrive as input events when in raw mode.
    // crossterm's event::read() will capture these bytes.
    // We need to:
    // 1. Switch to raw mode (should already be in raw mode at attach)
    // 2. Read events with timeout
    // 3. Parse the response from the raw bytes
    //
    // This is one of the trickier parts — terminal responses are
    // interleaved with any other input. In practice, probes are sent
    // during the attach sequence before user input is expected.

    // Simplified implementation using crossterm's poll + read:
    let deadline = std::time::Instant::now() + timeout;
    let mut buf = Vec::new();

    while std::time::Instant::now() < deadline {
        let remaining = deadline - std::time::Instant::now();
        if crossterm::event::poll(remaining).ok()? {
            // Read raw bytes from the controlling TTY/stdin stream and append.
            // Continue until we observe the expected suffix or timeout.
            // This must not rely solely on high-level Key events, because
            // probe replies (DA2/XTVERSION/DECRQM/Kitty) are raw escape bytes.
            //
            // Pseudocode:
            //   let chunk = tty_read_some_bytes().await?;
            //   buf.extend_from_slice(&chunk);
            //   if buf.ends_with(_suffix) && buf.starts_with(_prefix) {
            //       return decode_payload_between_prefix_suffix(&buf, _prefix, _suffix);
            //   }
        } else {
            // Timeout — no response
            return None;
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interpret_da2_xterm() {
        let mut caps = ClientCaps::default();
        interpret_da2("41;388;0", &mut caps);
        assert_eq!(caps.terminal_name, "xterm");
    }

    #[test]
    fn test_interpret_xtversion_with_parens() {
        let mut caps = ClientCaps::default();
        interpret_xtversion("Kitty(0.36.4)", &mut caps);
        assert_eq!(caps.terminal_name, "Kitty");
        assert_eq!(caps.terminal_version, "0.36.4");
    }

    #[test]
    fn test_interpret_xtversion_with_space() {
        let mut caps = ClientCaps::default();
        interpret_xtversion("tmux 3.6", &mut caps);
        assert_eq!(caps.terminal_name, "tmux");
        assert_eq!(caps.terminal_version, "3.6");
    }

    #[test]
    fn test_env_detection_truecolor() {
        std::env::set_var("COLORTERM", "truecolor");
        std::env::remove_var("TERM_PROGRAM");
        let mut caps = ClientCaps::default();
        caps.detect_from_env();
        assert_eq!(caps.color_depth, ColorDepth::TrueColor);
        std::env::remove_var("COLORTERM");
    }

    #[test]
    fn test_env_detection_kitty() {
        std::env::set_var("TERM_PROGRAM", "kitty");
        std::env::set_var("TERM_PROGRAM_VERSION", "0.36.4");
        let mut caps = ClientCaps::default();
        caps.detect_from_env();
        assert!(caps.kitty_keyboard);
        assert!(caps.osc52);
        assert!(caps.synchronized_output);
        assert_eq!(caps.color_depth, ColorDepth::TrueColor);
        assert_eq!(caps.image_protocol, ImageProtocol::Kitty);
        std::env::remove_var("TERM_PROGRAM");
        std::env::remove_var("TERM_PROGRAM_VERSION");
    }

    #[test]
    fn test_env_detection_ghostty() {
        std::env::set_var("TERM_PROGRAM", "ghostty");
        let mut caps = ClientCaps::default();
        caps.detect_from_env();
        assert!(caps.kitty_keyboard);
        assert!(caps.synchronized_output);
        assert_eq!(caps.color_depth, ColorDepth::TrueColor);
        std::env::remove_var("TERM_PROGRAM");
    }

    #[test]
    fn test_env_detection_apple_terminal() {
        std::env::set_var("TERM_PROGRAM", "Apple_Terminal");
        let mut caps = ClientCaps::default();
        caps.detect_from_env();
        assert!(!caps.kitty_keyboard);
        assert!(!caps.osc52);
        assert!(!caps.synchronized_output);
        assert_eq!(caps.color_depth, ColorDepth::Color256);
        std::env::remove_var("TERM_PROGRAM");
    }

    #[test]
    fn test_default_caps_conservative() {
        let caps = ClientCaps::default();
        assert_eq!(caps.color_depth, ColorDepth::Color256);
        assert!(!caps.kitty_keyboard);
        assert!(!caps.osc52);
        assert!(!caps.synchronized_output);
        assert_eq!(caps.image_protocol, ImageProtocol::None);
    }
}
```

### Step 3: Define Client Model

Create `crates/shux-core/src/client.rs`:

```rust
//! Client model — tracks attached TUI clients and their capabilities.
//!
//! Each attached client (TUI session) has its own set of terminal capabilities.
//! The daemon may have multiple clients attached simultaneously, each potentially
//! from a different terminal emulator.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Unique identifier for an attached client.
pub type ClientId = Uuid;

/// An attached TUI client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Client {
    /// Unique client identifier
    pub id: ClientId,

    /// Terminal capabilities detected at attach time
    pub caps: ClientCaps,

    /// The session this client is attached to
    pub session_id: SessionId,

    /// When the client connected (Unix timestamp milliseconds)
    pub connected_at: u64,

    /// Client's terminal size (columns, rows)
    pub terminal_size: (u16, u16),

    /// Whether this client is the primary (first-attached) client
    /// for its session. The primary client determines the session's
    /// effective terminal size when multiple clients are attached.
    pub primary: bool,
}

/// Tracks all attached clients.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClientRegistry {
    /// All currently attached clients, indexed by ID
    clients: HashMap<ClientId, Client>,
}

impl ClientRegistry {
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
        }
    }

    /// Register a new client.
    pub fn register(&mut self, client: Client) -> ClientId {
        let id = client.id;
        self.clients.insert(id, client);
        id
    }

    /// Remove a client (on detach/disconnect).
    pub fn deregister(&mut self, id: &ClientId) -> Option<Client> {
        self.clients.remove(id)
    }

    /// Get a client by ID.
    pub fn get(&self, id: &ClientId) -> Option<&Client> {
        self.clients.get(id)
    }

    /// Get the capabilities for a specific client.
    pub fn caps(&self, id: &ClientId) -> Option<&ClientCaps> {
        self.clients.get(id).map(|c| &c.caps)
    }

    /// Get all clients attached to a specific session.
    pub fn clients_for_session(&self, session_id: &SessionId) -> Vec<&Client> {
        self.clients
            .values()
            .filter(|c| &c.session_id == session_id)
            .collect()
    }

    /// Get the "effective" capabilities for a session.
    ///
    /// When multiple clients are attached, use the lowest common denominator
    /// for rendering decisions (since all clients see the same output).
    /// For the primary rendering client, use its caps directly.
    pub fn effective_caps_for_session(&self, session_id: &SessionId) -> ClientCaps {
        let session_clients = self.clients_for_session(session_id);

        if session_clients.is_empty() {
            return ClientCaps::default();
        }

        if session_clients.len() == 1 {
            return session_clients[0].caps.clone();
        }

        // Multiple clients: use lowest common denominator
        let mut effective = session_clients[0].caps.clone();
        for client in &session_clients[1..] {
            effective = effective.intersect(&client.caps);
        }
        effective
    }

    /// Number of attached clients.
    pub fn len(&self) -> usize {
        self.clients.len()
    }

    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }
}
```

### Step 4: Add ClientCaps Intersection Logic

Add to `crates/shux-ui/src/caps.rs`:

```rust
impl ClientCaps {
    /// Compute the intersection (lowest common denominator) of two capability sets.
    ///
    /// Used when multiple clients are attached to the same session.
    /// The result only enables features supported by both clients.
    pub fn intersect(&self, other: &ClientCaps) -> ClientCaps {
        ClientCaps {
            color_depth: match (&self.color_depth, &other.color_depth) {
                (ColorDepth::TrueColor, ColorDepth::TrueColor) => ColorDepth::TrueColor,
                (ColorDepth::Color8, _) | (_, ColorDepth::Color8) => ColorDepth::Color8,
                _ => ColorDepth::Color256,
            },
            kitty_keyboard: self.kitty_keyboard && other.kitty_keyboard,
            osc52: self.osc52 && other.osc52,
            synchronized_output: self.synchronized_output && other.synchronized_output,
            image_protocol: if self.image_protocol == other.image_protocol {
                self.image_protocol
            } else {
                ImageProtocol::None
            },
            terminal_name: self.terminal_name.clone(), // Keep primary's name
            terminal_version: self.terminal_version.clone(),
            term: self.term.clone(),
            osc8_hyperlinks: self.osc8_hyperlinks && other.osc8_hyperlinks,
            unicode: self.unicode && other.unicode,
            probe_results: self.probe_results.clone(), // Keep primary's probes
        }
    }
}
```

### Step 5: Add ClientConnected Event

In `crates/shux-core/src/event.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ShuxEvent {
    // ... existing variants ...

    /// A client has connected and capabilities have been detected.
    ClientConnected {
        client_id: ClientId,
        session_id: SessionId,
        terminal_name: String,
        color_depth: ColorDepth,
        kitty_keyboard: bool,
        synchronized_output: bool,
        osc52: bool,
    },

    /// A client has disconnected.
    ClientDisconnected {
        client_id: ClientId,
        session_id: SessionId,
    },
}
```

### Step 6: Integrate Capability Detection into Attach Flow

In the TUI client attach sequence (likely in `crates/shux-ui/src/lib.rs` or the attach command handler):

```rust
/// Called when a TUI client attaches to a session.
pub async fn attach(session_name: &str) -> Result<()> {
    // Step 1: Detect terminal capabilities
    let caps = ClientCaps::detect().await;

    tracing::info!(
        terminal = %caps.terminal_name,
        color_depth = ?caps.color_depth,
        kitty_keyboard = caps.kitty_keyboard,
        synchronized_output = caps.synchronized_output,
        "Attaching with detected capabilities"
    );

    // Step 2: Register client with the daemon
    let client_id = ClientId::new_v4();
    let client = Client {
        id: client_id,
        caps: caps.clone(),
        session_id: /* resolved session ID */,
        connected_at: current_timestamp_ms(),
        terminal_size: crossterm::terminal::size()?,
        primary: true, // TODO: check if first client
    };

    // Step 3: Send client registration to daemon
    // ... RPC call to register client ...

    // Step 4: Enable features based on capabilities
    if caps.kitty_keyboard {
        crossterm::execute!(
            std::io::stdout(),
            crossterm::event::PushKeyboardEnhancementFlags(
                crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | crossterm::event::KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
            )
        )?;
    }

    // Step 5: Emit client.connected event
    // ... via event bus ...

    Ok(())
}

/// Called when a TUI client detaches.
pub async fn detach(client_id: ClientId) -> Result<()> {
    // Disable Kitty keyboard if it was enabled
    // (crossterm handles this via PopKeyboardEnhancementFlags on Drop,
    // but explicit cleanup is safer)
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::PopKeyboardEnhancementFlags
    );

    // Deregister client
    // ... RPC call ...

    Ok(())
}
```

---

## Verification

### Functional

```bash
# Build the workspace
cargo build --workspace

# Verify caps module compiles
cargo check -p shux-ui
cargo check -p shux-core

# Manual verification: run shux in different terminals
# In Kitty:
cargo run -p shux -- new -s test
# Expected: kitty_keyboard=true, synchronized_output=true, color_depth=TrueColor

# In macOS Terminal:
cargo run -p shux -- new -s test
# Expected: kitty_keyboard=false, color_depth=Color256

# Check diagnostics output
# shux doctor | jq '.capabilities'
```

### Tests

```bash
# Run capability detection tests
cargo nextest run -p shux-ui -- caps
cargo nextest run -p shux-core -- client

# Expected passing tests:
# - test_env_detection_truecolor
# - test_env_detection_kitty
# - test_env_detection_ghostty
# - test_env_detection_apple_terminal
# - test_default_caps_conservative
# - test_interpret_da2_xterm
# - test_interpret_xtversion_with_parens
# - test_interpret_xtversion_with_space
# - test_caps_intersection
```

---

## Completion Criteria

- [ ] `ClientCaps` struct defined with all fields: color_depth, kitty_keyboard, osc52, synchronized_output, image_protocol, terminal_name, terminal_version, term, osc8_hyperlinks, unicode, probe_results
- [ ] `ColorDepth` enum: TrueColor, Color256, Color8
- [ ] `ImageProtocol` enum: None, Kitty, Sixel, Iterm2
- [ ] Phase 1 detection: TERM, TERM_PROGRAM, TERM_PROGRAM_VERSION, COLORTERM, VTE_VERSION env vars
- [ ] Terminal-specific heuristics for: Kitty, iTerm2, WezTerm, Ghostty, Alacritty, macOS Terminal
- [ ] Phase 2 probes: DA2, XTVERSION, Kitty keyboard (CSI ? u), DECRQM Mode 2026
- [ ] Each probe has 200ms individual timeout; total probe budget <= 400ms
- [ ] `ClientCaps::detect()` is async and respects probe timeouts
- [ ] `Client` model tracks: id, caps, session_id, connected_at, terminal_size
- [ ] `ClientRegistry` supports: register, deregister, get, caps, clients_for_session
- [ ] `effective_caps_for_session` computes lowest common denominator for multi-client
- [ ] `ClientCaps::intersect` correctly computes LCD of two capability sets
- [ ] `client.connected` event emitted with capability summary
- [ ] Attach flow: detect caps -> register client -> enable features -> emit event
- [ ] Detach flow: disable features -> deregister client -> emit event
- [ ] Kitty keyboard enabled via `PushKeyboardEnhancementFlags` when supported
- [ ] ProbeResults stored for `shux doctor` diagnostics output
- [ ] Attach time stays within p99 <= 150ms budget (probes are fast/timeout quickly)
- [ ] Unit tests pass for env detection, probe interpretation, intersection logic

---

## Commit Message

```
feat(ui,core): add terminal capability negotiation with ClientCaps

- Detect terminal capabilities at attach via env vars and escape probes
- Support detection of: truecolor, Kitty keyboard, OSC 52, synchronized
  output, image protocols (Kitty/Sixel/iTerm2)
- Terminal-specific heuristics for Kitty, iTerm2, WezTerm, Ghostty,
  Alacritty, macOS Terminal, VTE-based terminals
- Escape sequence probes: DA2, XTVERSION, Kitty keyboard, DECRQM 2026
  with 200ms timeout per probe
- ClientRegistry for per-client capability tracking
- LCD intersection when multiple clients attach to same session
- Emit client.connected event with detected capabilities
```

---

## Session Protocol

1. **Before starting:** Read task 010 (minimal TUI client) to understand the attach/detach flow and where capability detection hooks in. Read crossterm 0.29 docs for `PushKeyboardEnhancementFlags`, `BeginSynchronizedUpdate`. Read task 006 (input decoder) for how Kitty keyboard affects input handling.
2. **During:** Implement in order: ClientCaps struct (Step 1), probes (Step 2), Client model (Step 3), intersection (Step 4), events (Step 5), attach integration (Step 6). Run `cargo check` after each step. Test env detection first (no terminal I/O needed), then test probes manually in a real terminal.
3. **Edge cases to watch for:**
   - Running inside tmux/screen (TERM=screen-256color): probes may get intercepted
   - Running inside another shux instance (avoid infinite capability cascades)
   - SSH sessions: TERM_PROGRAM may not propagate; rely on TERM and probes
   - Very slow terminal responses (ensure timeouts work correctly)
   - No terminal at all (piped stdin): all probes should safely fail
   - env vars modified between detection and use (cache at attach time, don't re-read)
4. **After:** Run full test suite. Manually verify detection in at least 2 different terminals. Update `docs/PROGRESS.md`. Update `CLAUDE.md` Learnings with any terminal-specific discoveries.
