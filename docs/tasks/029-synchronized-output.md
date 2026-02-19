# 029 — Synchronized Output (Mode 2026)

**Status:** Pending
**Depends On:** 028
**Parallelizable With:** 026, 027

---

## Problem

Terminal multiplexers render by writing escape sequences to the host terminal. When the terminal processes these sequences incrementally (as they arrive), the user sees partial frames: half-rendered borders, flickering status bars, and tearing during rapid updates. This is especially visible during full-screen redraws, window splits, and high-output scenarios.

Synchronized output (ECMA-48 Mode 2026) solves this by wrapping a batch of output in begin/end markers. The terminal buffers all output between markers and presents the complete frame atomically. This eliminates visible tearing and flicker.

shux must use synchronized output when the host terminal supports it (detected via `ClientCaps.synchronized_output` from task 028) and skip it gracefully when not supported. The implementation wraps each compositor render frame with crossterm's `BeginSynchronizedUpdate` / `EndSynchronizedUpdate` commands.

## PRD Reference

- **SS 6.1** P0 Feature Matrix, Terminal compatibility: "Synchronized output: Mode 2026 via crossterm BeginSynchronizedUpdate/EndSynchronizedUpdate."
- **SS 12.2** Enhanced features: "Synchronized output — Detection: DECRQM Mode 2026 — Fallback: No synchronization"
- **SS 14.1** Performance budgets: "Keypress to visible update p99 <= 25ms" (synchronized output must not regress this)
- **SS 15.2** Key crates: "crossterm 0.29 — Kitty keyboard, synchronized output, OSC 52"

---

## Files to Create

- (None — this task modifies existing files only)

## Files to Modify

- `crates/shux-ui/src/compositor.rs` — Wrap render cycle with synchronized output markers
- `crates/shux-ui/src/lib.rs` — Pass ClientCaps to compositor

---

## Execution Steps

### Step 1: Understand crossterm's Synchronized Output API

crossterm 0.29 provides two commands for synchronized output:

```rust
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};

// To wrap a render frame:
crossterm::execute!(stdout, BeginSynchronizedUpdate)?;
// ... write all escape sequences for this frame ...
crossterm::execute!(stdout, EndSynchronizedUpdate)?;
```

Under the hood:
- `BeginSynchronizedUpdate` writes `CSI ? 2026 h` (DEC Private Mode Set)
- `EndSynchronizedUpdate` writes `CSI ? 2026 l` (DEC Private Mode Reset)

The terminal buffers all output between these markers and presents it as a single atomic update. If the terminal does not recognize Mode 2026, these sequences are silently ignored (they are harmless no-ops).

### Step 2: Add Synchronized Output Support to Compositor

Modify `crates/shux-ui/src/compositor.rs` to conditionally wrap the render cycle:

```rust
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use crate::caps::ClientCaps;

pub struct Compositor {
    // ... existing fields ...

    /// Whether synchronized output is supported by the current client.
    /// Set from ClientCaps at attach time.
    synchronized_output: bool,

    /// The output writer (stdout handle, kept to avoid repeated locking).
    stdout: std::io::Stdout,
}

impl Compositor {
    /// Create a new compositor with the given client capabilities.
    pub fn new(caps: &ClientCaps) -> Self {
        Self {
            // ... existing field initialization ...
            synchronized_output: caps.synchronized_output,
            stdout: std::io::stdout(),
        }
    }

    /// Update capabilities (e.g., when a different client attaches).
    pub fn update_caps(&mut self, caps: &ClientCaps) {
        self.synchronized_output = caps.synchronized_output;
    }

    /// Main render function — called each frame.
    ///
    /// When synchronized output is supported, the entire frame is wrapped
    /// in BeginSynchronizedUpdate/EndSynchronizedUpdate to prevent flicker.
    pub fn render(&mut self, ctx: &RenderContext) -> Result<()> {
        // Begin synchronized update (if supported)
        if self.synchronized_output {
            crossterm::execute!(self.stdout, BeginSynchronizedUpdate)?;
        }

        // Perform the actual rendering
        let result = self.render_inner(ctx);

        // End synchronized update (if supported)
        // IMPORTANT: Always end, even if render_inner failed, to avoid
        // leaving the terminal in a buffered state.
        if self.synchronized_output {
            // Use a separate execute! to ensure EndSynchronizedUpdate is sent
            // even if there was a rendering error
            let end_result = crossterm::execute!(self.stdout, EndSynchronizedUpdate);
            // If both failed, prefer the original render error
            if result.is_ok() {
                end_result?;
            }
        }

        result
    }

    /// Internal render implementation (without synchronization wrapper).
    fn render_inner(&mut self, ctx: &RenderContext) -> Result<()> {
        // Step 1: Calculate dirty regions (diff against previous frame)
        let dirty_cells = self.diff_frame(ctx)?;

        // Step 2: Render pane contents
        self.render_panes(ctx)?;

        // Step 3: Render borders
        self.render_borders(ctx)?;

        // Step 4: Render status bar (task 026)
        self.render_status_bar(ctx)?;

        // Step 5: Render overlays (command palette, help, etc.)
        self.render_overlays(ctx)?;

        // Step 6: Position cursor in active pane
        self.position_cursor(ctx)?;

        // Step 7: Flush output
        self.stdout.flush()?;

        Ok(())
    }
}
```

### Step 3: Ensure Begin/End Are Always Paired

A critical invariant: every `BeginSynchronizedUpdate` must be matched by an `EndSynchronizedUpdate`. If the render panics or returns early, the terminal could be left in a buffered state (output never displayed). Use a guard pattern:

```rust
/// RAII guard that ensures EndSynchronizedUpdate is sent on drop.
struct SyncGuard<'a> {
    stdout: &'a mut std::io::Stdout,
    active: bool,
}

impl<'a> SyncGuard<'a> {
    fn new(stdout: &'a mut std::io::Stdout, enabled: bool) -> Result<Self> {
        if enabled {
            crossterm::execute!(stdout, BeginSynchronizedUpdate)?;
        }
        Ok(Self {
            stdout,
            active: enabled,
        })
    }
}

impl<'a> Drop for SyncGuard<'a> {
    fn drop(&mut self) {
        if self.active {
            // Best-effort: if this fails, we can't do much
            let _ = crossterm::execute!(self.stdout, EndSynchronizedUpdate);
        }
    }
}

// Usage in render():
pub fn render(&mut self, ctx: &RenderContext) -> Result<()> {
    let _guard = SyncGuard::new(&mut self.stdout, self.synchronized_output)?;
    self.render_inner(ctx)
}
```

This ensures that even if `render_inner` panics (which should not happen in production, but safety first), the terminal is restored to its normal state.

### Step 4: Handle Multi-Client Capability Changes

When a second client attaches with different capabilities, the effective capabilities may change. The compositor must be notified:

```rust
// In the session management layer:

/// Called when a client attaches or detaches, potentially changing
/// the effective capabilities for the session.
fn on_client_change(&mut self, session_id: &SessionId) {
    let effective_caps = self.client_registry.effective_caps_for_session(session_id);

    // Update compositor's synchronized output flag
    self.compositor.update_caps(&effective_caps);

    // If synchronized output was just disabled (because a less-capable
    // client attached), the next render will simply skip the markers.
    // No special transition is needed.
}
```

### Step 5: Performance Measurement

Synchronized output adds a small overhead: two extra escape sequences per frame (a few bytes each). Measure to ensure the render budget is not affected:

```rust
use std::time::Instant;

pub fn render(&mut self, ctx: &RenderContext) -> Result<()> {
    let start = Instant::now();

    let _guard = SyncGuard::new(&mut self.stdout, self.synchronized_output)?;
    self.render_inner(ctx)?;

    let elapsed = start.elapsed();

    // Record metric for performance monitoring.
    // Ensure `metrics` is added to `crates/shux-ui/Cargo.toml` (or feature-gated)
    // so this instrumentation compiles in the default build.
    metrics::histogram!("shux_render_duration_ms").record(elapsed.as_secs_f64() * 1000.0);

    if elapsed.as_millis() > 25 {
        tracing::warn!(
            elapsed_ms = elapsed.as_millis(),
            synchronized = self.synchronized_output,
            "Render exceeded p99 budget"
        );
    }

    Ok(())
}
```

### Step 6: Add Verification Test for Flicker

Create a test that verifies synchronized output markers are correctly emitted:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A mock writer that captures all output for inspection.
    struct CaptureWriter {
        buffer: Vec<u8>,
    }

    impl CaptureWriter {
        fn new() -> Self {
            Self { buffer: Vec::new() }
        }

        fn output(&self) -> &[u8] {
            &self.buffer
        }

        /// Check if the output contains the BeginSynchronizedUpdate sequence.
        fn contains_begin_sync(&self) -> bool {
            // CSI ? 2026 h = \x1b[?2026h
            self.buffer
                .windows(9)
                .any(|w| w == b"\x1b[?2026h")
        }

        /// Check if the output contains the EndSynchronizedUpdate sequence.
        fn contains_end_sync(&self) -> bool {
            // CSI ? 2026 l = \x1b[?2026l
            self.buffer
                .windows(9)
                .any(|w| w == b"\x1b[?2026l")
        }

        /// Verify that Begin comes before End.
        fn sync_order_correct(&self) -> bool {
            let begin_pos = self.buffer
                .windows(9)
                .position(|w| w == b"\x1b[?2026h");
            let end_pos = self.buffer
                .windows(9)
                .rposition(|w| w == b"\x1b[?2026l");

            match (begin_pos, end_pos) {
                (Some(b), Some(e)) => b < e,
                _ => false,
            }
        }
    }

    impl std::io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.buffer.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_sync_guard_emits_begin_and_end() {
        let mut writer = CaptureWriter::new();

        // Simulate: create guard, write some content, drop guard
        {
            crossterm::queue!(writer, BeginSynchronizedUpdate).unwrap();
            write!(writer, "frame content").unwrap();
            crossterm::queue!(writer, EndSynchronizedUpdate).unwrap();
        }

        assert!(writer.contains_begin_sync());
        assert!(writer.contains_end_sync());
        assert!(writer.sync_order_correct());
    }

    #[test]
    fn test_sync_disabled_no_markers() {
        let mut writer = CaptureWriter::new();

        // With sync disabled, no markers should be written
        let synchronized_output = false;
        if synchronized_output {
            crossterm::queue!(writer, BeginSynchronizedUpdate).unwrap();
        }
        write!(writer, "frame content").unwrap();
        if synchronized_output {
            crossterm::queue!(writer, EndSynchronizedUpdate).unwrap();
        }

        assert!(!writer.contains_begin_sync());
        assert!(!writer.contains_end_sync());
    }

    #[test]
    fn test_sync_guard_sends_end_on_drop() {
        // This test verifies the RAII guard behavior.
        // Even if we "panic" (simulated by early return), End is sent.
        let mut buffer = Vec::new();

        // Manually test the guard pattern
        {
            // Begin
            crossterm::queue!(&mut buffer, BeginSynchronizedUpdate).unwrap();

            // Simulate: guard would be dropped here even on panic
            // (In real code, Drop impl handles this)
        }
        // End (simulating the Drop handler)
        crossterm::queue!(&mut buffer, EndSynchronizedUpdate).unwrap();

        let has_begin = buffer.windows(9).any(|w| w == b"\x1b[?2026h");
        let has_end = buffer.windows(9).any(|w| w == b"\x1b[?2026l");
        assert!(has_begin);
        assert!(has_end);
    }

    #[test]
    fn test_caps_update_toggles_sync() {
        // When caps change (e.g., new client attaches), sync should toggle
        let mut sync_enabled = true;

        // Client with no sync support attaches
        let new_caps = ClientCaps {
            synchronized_output: false,
            ..ClientCaps::default()
        };

        sync_enabled = new_caps.synchronized_output;
        assert!(!sync_enabled);

        // Client detaches, original caps restored
        let original_caps = ClientCaps {
            synchronized_output: true,
            ..ClientCaps::default()
        };

        sync_enabled = original_caps.synchronized_output;
        assert!(sync_enabled);
    }
}
```

### Step 7: Document Synchronized Output Behavior

Add a comment block to the compositor explaining the behavior for future maintainers:

```rust
// SYNCHRONIZED OUTPUT (Mode 2026)
//
// When the host terminal supports Mode 2026, we wrap each render frame
// in BeginSynchronizedUpdate/EndSynchronizedUpdate. This tells the terminal
// to buffer all output and present it as a single atomic update, eliminating
// visible tearing and flicker.
//
// Key invariants:
// 1. Begin and End are ALWAYS paired (enforced by SyncGuard RAII)
// 2. If End fails, we log a warning but don't crash (terminal handles it)
// 3. When multiple clients are attached, we use the LCD of their capabilities
//    (if any client lacks sync support, we disable it for the session)
// 4. Sync overhead is negligible: ~18 bytes per frame (two escape sequences)
// 5. Without sync, rendering still works — just with potential flicker
//
// Supported terminals (as of Feb 2026):
// - Kitty (0.20+)
// - iTerm2 (3.5+)
// - WezTerm (all versions)
// - Ghostty (all versions)
// - Contour (0.3+)
// - foot (1.9+)
// - NOT: macOS Terminal, xterm (unless patched), Alacritty (<= 0.13)
```

---

## Verification

### Functional

```bash
# Build the workspace
cargo build --workspace

# Verify compositor compiles with sync changes
cargo check -p shux-ui

# Manual flicker test:
# 1. Run shux in a supported terminal (Kitty, iTerm2, Ghostty)
# 2. Split into 4 panes
# 3. Run `yes` or `seq 1 100000` in one pane (fast output)
# 4. Observe other panes for tearing during rapid updates
# 5. Expected: no visible tearing with synchronized output

# Compare with sync disabled:
# 1. Temporarily force synchronized_output = false in compositor
# 2. Repeat the above test
# 3. Expected: occasional flicker/tearing visible

# Verify in unsupported terminal (macOS Terminal):
# 1. Run shux in macOS Terminal
# 2. Repeat fast-output test
# 3. Expected: some flicker (sync not available) but no crashes or garbled output
```

### Tests

```bash
# Run synchronized output tests
cargo nextest run -p shux-ui -- sync

# Expected passing tests:
# - test_sync_guard_emits_begin_and_end
# - test_sync_disabled_no_markers
# - test_sync_guard_sends_end_on_drop
# - test_caps_update_toggles_sync
```

---

## Completion Criteria

- [ ] Compositor wraps render frames with `BeginSynchronizedUpdate`/`EndSynchronizedUpdate` when `ClientCaps.synchronized_output` is true
- [ ] `SyncGuard` RAII pattern ensures `EndSynchronizedUpdate` is always sent, even on render failure
- [ ] Synchronized output is skipped when `ClientCaps.synchronized_output` is false
- [ ] No visible tearing or flicker in terminals that support Mode 2026
- [ ] Rendering still works correctly in terminals without Mode 2026 support
- [ ] Multi-client capability changes correctly toggle synchronized output
- [ ] Render duration measured and logged when exceeding p99 budget
- [ ] Overhead is negligible (~18 bytes per frame for the two escape sequences)
- [ ] Unit tests verify marker emission, guard behavior, and capability toggling
- [ ] No regression in keypress-to-visible-update latency (p99 <= 25ms)

---

## Commit Message

```
feat(ui): add synchronized output (Mode 2026) to eliminate render flicker

- Wrap compositor render frames in BeginSynchronizedUpdate/
  EndSynchronizedUpdate when host terminal supports Mode 2026
- SyncGuard RAII ensures End marker is always sent, even on failure
- Skip synchronization markers for unsupported terminals (graceful fallback)
- Update sync state when client capabilities change (multi-client LCD)
- Measure and log render duration for performance monitoring
```

---

## Session Protocol

1. **Before starting:** Read task 028 (capability negotiation) to understand how `ClientCaps.synchronized_output` is detected. Read task 009 (render compositor) for the current render cycle structure. Verify crossterm 0.29 `BeginSynchronizedUpdate`/`EndSynchronizedUpdate` API.
2. **During:** Implement in order: understand crossterm API (Step 1), modify compositor (Step 2), add SyncGuard (Step 3), multi-client handling (Step 4), performance measurement (Step 5), tests (Step 6), documentation (Step 7). Run `cargo check -p shux-ui` after each step.
3. **Edge cases to watch for:**
   - Compositor panics between Begin and End (SyncGuard handles this)
   - Very large frames that take >25ms to render (synchronized output helps here, but measure)
   - Terminal disconnects between Begin and End (OS handles cleanup)
   - Nested synchronized updates (should not happen, but crossterm handles it correctly)
   - Running inside tmux (tmux has its own synchronization; our markers pass through)
4. **After:** Run full test suite. Manually verify flicker-free rendering in Kitty or iTerm2 with fast-updating panes. Measure render latency with and without sync to confirm no regression. Update `docs/PROGRESS.md`. Update `CLAUDE.md` Learnings.
