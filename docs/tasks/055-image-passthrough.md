# 055 — Image Passthrough (DCS, Kitty, Sixel, iTerm2)

**Status:** Pending
**Depends On:** 052
**Parallelizable With:** 053, 054

---

## Problem

Modern terminals support inline images via several protocols: Kitty graphics protocol, Sixel, and iTerm2 inline images. Terminal multiplexers have historically broken image display because they intercept and re-render PTY output, stripping or corrupting image escape sequences. tmux added `allow-passthrough` to address this, and shux must do the same. The PRD requires DCS passthrough for the focused pane, passing image sequences through to the host terminal transparently. Images must be cleared on pane switch to prevent leaked/stale images from background panes appearing on screen.

## PRD Reference

- **SS 6.1** Terminal compatibility: "Image passthrough: DCS passthrough for focused pane (like tmux `allow-passthrough`). Kitty graphics, Sixel, iTerm2 inline images pass through to host terminal. Images cleared on pane switch."
- **SS 12.2** Enhanced features: "Image passthrough — Terminal identification — DCS passthrough for focused pane"
- **SS 12.1** Capability detection: Detect terminal support for image protocols

---

## Files to Create

- `crates/shux-vt/src/passthrough.rs` — Image sequence detection and passthrough logic
- `crates/shux-vt/src/passthrough/kitty.rs` — Kitty graphics protocol detection
- `crates/shux-vt/src/passthrough/sixel.rs` — Sixel sequence detection
- `crates/shux-vt/src/passthrough/iterm2.rs` — iTerm2 inline image detection (OSC 1337)

## Files to Modify

- `crates/shux-vt/src/lib.rs` — Add `pub mod passthrough;`
- `crates/shux-vt/src/parser.rs` — Hook passthrough detection into VT parser output path
- `crates/shux-ui/src/compositor.rs` — Forward passthrough sequences for focused pane, clear on switch
- `crates/shux-core/src/config.rs` — Add `image_passthrough` config option
- `docs/PROGRESS.md` — Mark task 055 complete

---

## Execution Steps

### Step 1: Define Passthrough Types

Create `crates/shux-vt/src/passthrough.rs`:

```rust
//! Image passthrough detection and forwarding.
//!
//! Detects image-related escape sequences in PTY output and marks them
//! for passthrough to the host terminal rather than interpretation by
//! the virtual terminal grid. Only the focused pane's images are
//! forwarded; background pane images are silently consumed.
//!
//! Supported protocols:
//! - Kitty graphics protocol (APC + 'G' sequences)
//! - Sixel (DCS with 'q' final character)
//! - iTerm2 inline images (OSC 1337;File=...)

pub mod iterm2;
pub mod kitty;
pub mod sixel;

/// An image sequence detected in PTY output.
#[derive(Debug, Clone)]
pub enum ImageSequence {
    /// Kitty graphics protocol payload.
    /// Contains the complete APC...ST sequence.
    Kitty(Vec<u8>),

    /// Sixel image data.
    /// Contains the complete DCS...ST sequence.
    Sixel(Vec<u8>),

    /// iTerm2 inline image.
    /// Contains the complete OSC 1337;File=...ST sequence.
    Iterm2(Vec<u8>),
}

impl ImageSequence {
    /// Get the raw bytes of the sequence for passthrough.
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            ImageSequence::Kitty(b) => b,
            ImageSequence::Sixel(b) => b,
            ImageSequence::Iterm2(b) => b,
        }
    }

    /// Protocol name for logging.
    pub fn protocol(&self) -> &'static str {
        match self {
            ImageSequence::Kitty(_) => "kitty",
            ImageSequence::Sixel(_) => "sixel",
            ImageSequence::Iterm2(_) => "iterm2",
        }
    }
}

/// Passthrough mode configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassthroughMode {
    /// No passthrough — all image sequences consumed silently.
    Off,
    /// Passthrough for focused pane only (default, secure).
    FocusedOnly,
    /// Passthrough for all panes (less secure — background panes can
    /// draw to the screen). Not recommended.
    All,
}

impl Default for PassthroughMode {
    fn default() -> Self {
        Self::FocusedOnly
    }
}

/// State machine for detecting image sequences in a byte stream.
///
/// The detector is fed bytes from PTY output and emits `ImageSequence`
/// values when a complete image sequence is detected. Non-image bytes
/// pass through to the normal VT parser.
pub struct PassthroughDetector {
    state: DetectorState,
    buffer: Vec<u8>,
    /// Maximum image sequence size (16 MB — same as frame limit).
    max_size: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum DetectorState {
    /// Normal text processing — watching for escape sequences.
    Normal,
    /// Received ESC, waiting for next byte to determine sequence type.
    Escape,
    /// Inside a DCS sequence (potential Sixel).
    Dcs,
    /// Inside an APC sequence (potential Kitty graphics).
    Apc,
    /// Inside an OSC sequence (potential iTerm2).
    Osc,
    /// Accumulating Sixel data.
    SixelData,
    /// Accumulating Kitty graphics data.
    KittyData,
    /// Accumulating iTerm2 image data.
    Iterm2Data,
}

/// Result of feeding a byte to the detector.
pub enum DetectorOutput {
    /// Byte is not part of an image sequence — pass to VT parser.
    PassThrough(u8),
    /// A complete image sequence was detected.
    Image(ImageSequence),
    /// Byte was consumed as part of an in-progress image sequence.
    Consumed,
}

impl PassthroughDetector {
    pub fn new() -> Self {
        Self {
            state: DetectorState::Normal,
            buffer: Vec::with_capacity(4096),
            max_size: 16 * 1024 * 1024,
        }
    }

    /// Feed a single byte from PTY output.
    pub fn feed(&mut self, byte: u8) -> DetectorOutput {
        match self.state {
            DetectorState::Normal => {
                if byte == 0x1B {
                    self.state = DetectorState::Escape;
                    self.buffer.clear();
                    self.buffer.push(byte);
                    DetectorOutput::Consumed
                } else {
                    DetectorOutput::PassThrough(byte)
                }
            }

            DetectorState::Escape => {
                self.buffer.push(byte);
                match byte {
                    b'P' => {
                        // DCS introducer — could be Sixel
                        self.state = DetectorState::Dcs;
                        DetectorOutput::Consumed
                    }
                    b'_' => {
                        // APC introducer — could be Kitty graphics
                        self.state = DetectorState::Apc;
                        DetectorOutput::Consumed
                    }
                    b']' => {
                        // OSC introducer — could be iTerm2
                        self.state = DetectorState::Osc;
                        DetectorOutput::Consumed
                    }
                    _ => {
                        // Not an image sequence — flush buffered bytes
                        self.state = DetectorState::Normal;
                        let escaped = self.buffer.clone();
                        self.buffer.clear();
                        // Return the ESC byte; caller must handle remaining
                        DetectorOutput::PassThrough(escaped[0])
                    }
                }
            }

            DetectorState::Dcs => {
                self.buffer.push(byte);
                if sixel::is_sixel_start(&self.buffer) {
                    self.state = DetectorState::SixelData;
                    DetectorOutput::Consumed
                } else if byte == 0x1B || self.buffer.len() > 32 {
                    // Not Sixel — flush
                    self.state = DetectorState::Normal;
                    DetectorOutput::PassThrough(self.buffer[0])
                } else {
                    DetectorOutput::Consumed
                }
            }

            DetectorState::Apc => {
                self.buffer.push(byte);
                if kitty::is_kitty_start(&self.buffer) {
                    self.state = DetectorState::KittyData;
                    DetectorOutput::Consumed
                } else if byte == 0x1B || self.buffer.len() > 32 {
                    self.state = DetectorState::Normal;
                    DetectorOutput::PassThrough(self.buffer[0])
                } else {
                    DetectorOutput::Consumed
                }
            }

            DetectorState::Osc => {
                self.buffer.push(byte);
                if iterm2::is_iterm2_image_start(&self.buffer) {
                    self.state = DetectorState::Iterm2Data;
                    DetectorOutput::Consumed
                } else if byte == 0x07 || (byte == 0x5C && self.buffer.ends_with(&[0x1B, 0x5C])) {
                    // OSC terminated but not an image
                    self.state = DetectorState::Normal;
                    DetectorOutput::PassThrough(self.buffer[0])
                } else if self.buffer.len() > 64 && !iterm2::could_be_iterm2(&self.buffer) {
                    self.state = DetectorState::Normal;
                    DetectorOutput::PassThrough(self.buffer[0])
                } else {
                    DetectorOutput::Consumed
                }
            }

            DetectorState::SixelData => {
                self.buffer.push(byte);
                if self.buffer.len() > self.max_size {
                    // Sequence too large — discard
                    self.state = DetectorState::Normal;
                    self.buffer.clear();
                    DetectorOutput::Consumed
                } else if sixel::is_sixel_end(byte, &self.buffer) {
                    let seq = ImageSequence::Sixel(self.buffer.clone());
                    self.state = DetectorState::Normal;
                    self.buffer.clear();
                    DetectorOutput::Image(seq)
                } else {
                    DetectorOutput::Consumed
                }
            }

            DetectorState::KittyData => {
                self.buffer.push(byte);
                if self.buffer.len() > self.max_size {
                    self.state = DetectorState::Normal;
                    self.buffer.clear();
                    DetectorOutput::Consumed
                } else if kitty::is_kitty_end(byte, &self.buffer) {
                    let seq = ImageSequence::Kitty(self.buffer.clone());
                    self.state = DetectorState::Normal;
                    self.buffer.clear();
                    DetectorOutput::Image(seq)
                } else {
                    DetectorOutput::Consumed
                }
            }

            DetectorState::Iterm2Data => {
                self.buffer.push(byte);
                if self.buffer.len() > self.max_size {
                    self.state = DetectorState::Normal;
                    self.buffer.clear();
                    DetectorOutput::Consumed
                } else if iterm2::is_iterm2_end(byte, &self.buffer) {
                    let seq = ImageSequence::Iterm2(self.buffer.clone());
                    self.state = DetectorState::Normal;
                    self.buffer.clear();
                    DetectorOutput::Image(seq)
                } else {
                    DetectorOutput::Consumed
                }
            }
        }
    }

    /// Reset the detector state (e.g., on pane switch).
    pub fn reset(&mut self) {
        self.state = DetectorState::Normal;
        self.buffer.clear();
    }
}
```

### Step 2: Implement Protocol Detectors

Create `crates/shux-vt/src/passthrough/kitty.rs`:

```rust
//! Kitty graphics protocol detection.
//!
//! Kitty uses APC (ESC _ ) with 'G' as the action character:
//!   ESC _ G <payload> ESC \
//!
//! The payload is key=value pairs separated by commas, followed by
//! semicolon and base64-encoded data.

/// Check if the buffer so far looks like a Kitty graphics start.
pub fn is_kitty_start(buf: &[u8]) -> bool {
    // ESC _ G...  (buf[0]=ESC, buf[1]='_', buf[2]='G')
    buf.len() >= 3 && buf[0] == 0x1B && buf[1] == b'_' && buf[2] == b'G'
}

/// Check if the current byte ends the Kitty graphics sequence.
/// Terminated by ST (ESC \) or BEL.
pub fn is_kitty_end(byte: u8, buf: &[u8]) -> bool {
    // ST = ESC \ (0x1B 0x5C)
    if buf.len() >= 2 && buf[buf.len() - 2] == 0x1B && byte == 0x5C {
        return true;
    }
    false
}
```

Create `crates/shux-vt/src/passthrough/sixel.rs`:

```rust
//! Sixel graphics detection.
//!
//! Sixel uses DCS (ESC P) with parameters followed by 'q':
//!   ESC P <params> q <sixel-data> ESC \

/// Check if the buffer looks like a Sixel sequence start.
pub fn is_sixel_start(buf: &[u8]) -> bool {
    // ESC P <optional params> q
    if buf.len() < 3 || buf[0] != 0x1B || buf[1] != b'P' {
        return false;
    }
    // Look for 'q' after optional numeric parameters
    buf[2..].iter().any(|&b| b == b'q')
}

/// Check if the current byte ends the Sixel sequence.
/// Terminated by ST (ESC \).
pub fn is_sixel_end(byte: u8, buf: &[u8]) -> bool {
    buf.len() >= 2 && buf[buf.len() - 2] == 0x1B && byte == 0x5C
}
```

Create `crates/shux-vt/src/passthrough/iterm2.rs`:

```rust
//! iTerm2 inline image detection.
//!
//! iTerm2 uses OSC 1337 with File= parameter:
//!   ESC ] 1337 ; File= <params> : <base64-data> BEL
//!   or
//!   ESC ] 1337 ; File= <params> : <base64-data> ESC \

/// Check if the buffer could be an iTerm2 image.
pub fn could_be_iterm2(buf: &[u8]) -> bool {
    // Minimum: ESC ] 1337;
    if buf.len() < 7 {
        return true; // Too short to tell
    }
    // Check for "1337;" after OSC introducer
    let content = &buf[2..]; // Skip ESC ]
    content.starts_with(b"1337;")
}

/// Check if the buffer is an iTerm2 image start.
pub fn is_iterm2_image_start(buf: &[u8]) -> bool {
    if buf.len() < 12 {
        return false;
    }
    let content = &buf[2..];
    content.starts_with(b"1337;File=")
}

/// Check if the current byte ends the iTerm2 sequence.
pub fn is_iterm2_end(byte: u8, buf: &[u8]) -> bool {
    // BEL terminator
    if byte == 0x07 {
        return true;
    }
    // ST terminator (ESC \)
    buf.len() >= 2 && buf[buf.len() - 2] == 0x1B && byte == 0x5C
}
```

### Step 3: Integrate with VT Parser

Modify `crates/shux-vt/src/parser.rs` to hook the passthrough detector into the output path:

```rust
use crate::passthrough::{PassthroughDetector, DetectorOutput, ImageSequence, PassthroughMode};

pub struct VirtualTerminal {
    // ... existing fields ...

    /// Image passthrough detector.
    passthrough: PassthroughDetector,
    /// Pending image sequences for the focused pane to forward.
    pending_images: Vec<ImageSequence>,
    /// Whether this pane is currently focused.
    is_focused: bool,
    /// Passthrough mode from config.
    passthrough_mode: PassthroughMode,
}

impl VirtualTerminal {
    /// Process raw bytes from PTY output.
    pub fn process_output(&mut self, data: &[u8]) {
        for &byte in data {
            match self.passthrough.feed(byte) {
                DetectorOutput::PassThrough(b) => {
                    // Normal VT processing
                    self.parser.advance(b);
                }
                DetectorOutput::Image(seq) => {
                    // Image detected — queue for passthrough if focused
                    match self.passthrough_mode {
                        PassthroughMode::Off => {
                            // Silently discard
                        }
                        PassthroughMode::FocusedOnly => {
                            if self.is_focused {
                                tracing::debug!(
                                    protocol = seq.protocol(),
                                    size = seq.as_bytes().len(),
                                    "Image passthrough (focused)"
                                );
                                self.pending_images.push(seq);
                            }
                        }
                        PassthroughMode::All => {
                            self.pending_images.push(seq);
                        }
                    }
                }
                DetectorOutput::Consumed => {
                    // Part of an in-progress image sequence
                }
            }
        }
    }

    /// Drain pending image sequences for forwarding to host terminal.
    pub fn drain_images(&mut self) -> Vec<ImageSequence> {
        std::mem::take(&mut self.pending_images)
    }

    /// Called when focus changes. Clears any buffered image state.
    pub fn set_focused(&mut self, focused: bool) {
        let was_focused = self.is_focused;
        self.is_focused = focused;

        if was_focused && !focused {
            // Lost focus — clear pending images
            self.pending_images.clear();
            self.passthrough.reset();
        }
    }
}
```

### Step 4: Integrate with Compositor

Modify `crates/shux-ui/src/compositor.rs`:

```rust
impl Compositor {
    /// Render a frame, including image passthrough for the focused pane.
    pub fn render(&mut self, ctx: &RenderContext) -> Result<()> {
        // ... existing render logic ...

        // Forward image sequences from focused pane
        let focused_pane = ctx.focused_pane();
        let images = focused_pane.vt.drain_images();
        for image in images {
            // Write image bytes directly to the host terminal.
            // These bypass the compositor's diff buffer entirely.
            self.writer.write_all(image.as_bytes())?;
        }

        Ok(())
    }

    /// Clear images when switching focused pane.
    ///
    /// This prevents stale images from the previous focused pane
    /// from remaining on screen.
    pub fn handle_focus_change(
        &mut self,
        old_pane: &mut VirtualTerminal,
        new_pane: &mut VirtualTerminal,
    ) {
        old_pane.set_focused(false);
        new_pane.set_focused(true);

        // Clear any images that may be rendered on screen.
        // For Kitty: send delete-all-placements command
        // For Sixel: overwrite with spaces (Sixel has no delete)
        // For iTerm2: no explicit clear needed (images are cell-positioned)
        self.clear_host_images();
    }

    /// Send escape sequences to clear images from the host terminal.
    fn clear_host_images(&mut self) {
        // Kitty graphics: ESC_G a=d,d=A; ST (delete all placements)
        let _ = self.writer.write_all(b"\x1b_Ga=d,d=A;\x1b\\");

        // Sixel: No standard clear mechanism. The compositor's normal
        // screen redraw will overwrite any sixel pixels.
    }
}
```

### Step 5: Add Configuration

In `crates/shux-core/src/config.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    // ... existing fields ...

    /// Image passthrough mode.
    /// "off" = no passthrough (default for security-conscious environments)
    /// "focused" = passthrough for focused pane only (default)
    /// "all" = passthrough for all panes (less secure)
    #[serde(default = "default_image_passthrough")]
    pub image_passthrough: String,
}

fn default_image_passthrough() -> String {
    "focused".to_string()
}
```

### Step 6: Add Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_kitty_graphics() {
        let mut detector = PassthroughDetector::new();
        // ESC _ G a=T,f=100,s=10,v=10;base64data ESC \
        let seq = b"\x1b_Ga=T,f=100;AAAA\x1b\\";

        let mut result = None;
        for &byte in seq.iter() {
            if let DetectorOutput::Image(img) = detector.feed(byte) {
                result = Some(img);
            }
        }
        assert!(result.is_some());
        assert_eq!(result.unwrap().protocol(), "kitty");
    }

    #[test]
    fn detect_sixel() {
        let mut detector = PassthroughDetector::new();
        // ESC P 0;1;0 q #0;2;0;0;0 !10~ - ESC \
        let seq = b"\x1bP0;1;0q#0;2;0;0;0!10~-\x1b\\";

        let mut result = None;
        for &byte in seq.iter() {
            if let DetectorOutput::Image(img) = detector.feed(byte) {
                result = Some(img);
            }
        }
        assert!(result.is_some());
        assert_eq!(result.unwrap().protocol(), "sixel");
    }

    #[test]
    fn detect_iterm2_inline_image() {
        let mut detector = PassthroughDetector::new();
        let seq = b"\x1b]1337;File=inline=1:AAAA\x07";

        let mut result = None;
        for &byte in seq.iter() {
            if let DetectorOutput::Image(img) = detector.feed(byte) {
                result = Some(img);
            }
        }
        assert!(result.is_some());
        assert_eq!(result.unwrap().protocol(), "iterm2");
    }

    #[test]
    fn non_image_escape_passes_through() {
        let mut detector = PassthroughDetector::new();
        // SGR sequence (not an image)
        let seq = b"\x1b[32m";

        let mut passed = Vec::new();
        for &byte in seq.iter() {
            match detector.feed(byte) {
                DetectorOutput::PassThrough(b) => passed.push(b),
                _ => {}
            }
        }
        // The ESC should eventually pass through
        assert!(!passed.is_empty());
    }

    #[test]
    fn oversized_image_discarded() {
        let mut detector = PassthroughDetector::new();
        detector.max_size = 100; // Very small limit for testing

        // Start a Kitty sequence
        let start = b"\x1b_G";
        for &byte in start.iter() {
            detector.feed(byte);
        }

        // Feed more than max_size bytes
        for _ in 0..200 {
            detector.feed(b'A');
        }

        // Should have been discarded — detector should be back to Normal
        assert_eq!(detector.state, DetectorState::Normal);
    }

    #[test]
    fn focused_pane_receives_images() {
        let mut vt = VirtualTerminal::new(80, 24, 5000);
        vt.set_focused(true);
        vt.passthrough_mode = PassthroughMode::FocusedOnly;

        let seq = b"\x1b_Ga=T;AAAA\x1b\\";
        vt.process_output(seq);

        let images = vt.drain_images();
        assert_eq!(images.len(), 1);
    }

    #[test]
    fn unfocused_pane_discards_images() {
        let mut vt = VirtualTerminal::new(80, 24, 5000);
        vt.set_focused(false);
        vt.passthrough_mode = PassthroughMode::FocusedOnly;

        let seq = b"\x1b_Ga=T;AAAA\x1b\\";
        vt.process_output(seq);

        let images = vt.drain_images();
        assert!(images.is_empty());
    }

    #[test]
    fn passthrough_off_discards_all() {
        let mut vt = VirtualTerminal::new(80, 24, 5000);
        vt.set_focused(true);
        vt.passthrough_mode = PassthroughMode::Off;

        let seq = b"\x1b_Ga=T;AAAA\x1b\\";
        vt.process_output(seq);

        let images = vt.drain_images();
        assert!(images.is_empty());
    }

    #[test]
    fn focus_change_clears_pending() {
        let mut vt = VirtualTerminal::new(80, 24, 5000);
        vt.set_focused(true);
        vt.passthrough_mode = PassthroughMode::FocusedOnly;

        let seq = b"\x1b_Ga=T;AAAA\x1b\\";
        vt.process_output(seq);
        assert_eq!(vt.pending_images.len(), 1);

        vt.set_focused(false);
        assert!(vt.pending_images.is_empty());
    }
}
```

---

## Verification

### Functional

```bash
# Build the workspace
cargo build --workspace

# Test with Kitty terminal:
# 1. Open shux in Kitty
# 2. In a pane, run: kitty +kitten icat /path/to/image.png
# Expected: image displays in the focused pane

# 3. Switch to another pane
# Expected: image clears from the previous pane's area

# Test with iTerm2:
# 1. Open shux in iTerm2
# 2. Run: imgcat /path/to/image.png (iTerm2's image tool)
# Expected: image displays inline

# Test image passthrough off:
# Set config: image_passthrough = "off"
# Run image command — image should not display
```

### Tests

```bash
# Run passthrough tests
cargo nextest run -p shux-vt passthrough

# Expected: all detection and focus tests pass
```

---

## Completion Criteria

- [ ] Kitty graphics protocol sequences detected and passed through
- [ ] Sixel sequences detected and passed through
- [ ] iTerm2 inline image sequences (OSC 1337;File=) detected and passed through
- [ ] Only focused pane images forwarded (PassthroughMode::FocusedOnly default)
- [ ] Images cleared on pane focus switch
- [ ] Passthrough mode configurable: "off", "focused", "all"
- [ ] Non-image escape sequences pass through to VT parser normally
- [ ] Oversized image sequences (>16 MB) silently discarded
- [ ] Detector state machine handles partial/interrupted sequences
- [ ] No memory leak from accumulated image buffers
- [ ] Unit tests for each protocol detection, focus behavior, and edge cases
- [ ] Manual verification in Kitty and iTerm2 terminals

---

## Commit Message

```
feat(vt): add image passthrough for Kitty graphics, Sixel, and iTerm2

- State machine detects image sequences in PTY output
- Passthrough for focused pane only (configurable: off/focused/all)
- Images cleared on pane switch to prevent stale display
- Kitty graphics: APC G sequences
- Sixel: DCS q sequences
- iTerm2: OSC 1337;File= sequences
- Oversized sequences (>16 MB) silently discarded
```

---

## Session Protocol

1. **Before starting:** Read the Kitty graphics protocol specification. Read the Sixel format specification. Read iTerm2's documentation on inline images (OSC 1337). Understand the DCS, APC, and OSC escape sequence terminators (ST = ESC \, BEL = 0x07).
2. **During:** Implement the state machine first (Step 1), then each protocol detector (Step 2), then integration with VT parser (Step 3) and compositor (Step 4). Test with actual terminals — this feature cannot be fully verified with unit tests alone.
3. **Security consideration:** Only the focused pane should have passthrough by default. A background pane running malicious code should not be able to draw images over the visible content.
4. **Edge cases to watch for:**
   - Image sequences split across multiple PTY reads
   - Interleaved image and text sequences
   - Very large images (>10 MB) — must not OOM
   - Partial sequences (connection drops mid-image)
   - Sixel has no explicit "delete image" command — rely on compositor redraw
   - Multiple image protocols in the same pane output
5. **After:** Manually test in Kitty, iTerm2, and WezTerm. Verify images display correctly and clear on pane switch. Run full test suite. Update `docs/PROGRESS.md`. Update `CLAUDE.md` Learnings (create from task 000 template if missing).
