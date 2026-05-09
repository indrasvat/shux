# 019 — Prefix Key System (Tier 2 Keybindings)

**Status:** Pending
**Depends On:** 018
**Parallelizable With:** 022

---

## Problem

While Tier 1 bare-key bindings (task 018) cover the most frequent pane navigation and window switching actions, a large set of less-frequent-but-essential operations -- creating windows, splitting panes, renaming, detaching, entering copy mode, opening the command palette -- needs a prefix-based key sequence system. This is the traditional multiplexer model (tmux's `Ctrl+b`, screen's `Ctrl+a`), but shux uses `Ctrl+Space` as the default prefix for better ergonomics. The prefix system must support a configurable timeout, a visual indicator showing that the user is in "prefix mode," and must cleanly integrate with the input pipeline so that unrecognized second keys are discarded (not forwarded to the PTY).

Without this task, the user has no way to perform structural operations (split, new window, rename, detach, copy mode) via the keyboard. This is a hard prerequisite for copy mode (task 021), command palette (task 032), and help overlay (task 033).

## PRD Reference

- **section 9.2** (Tier 2: Prefix keybindings) -- the full table of Prefix + key sequences
- **section 9.4** (Customization) -- all keybindings are remappable in TOML config
- **section 6.1** (Graded keybindings) -- Tier 2 prefix description
- **section 10.2** (`[ui] prefix = "ctrl+space"`) -- prefix key configuration

---

## Files to Create

- `crates/shux-ui/src/prefix.rs` -- Prefix mode state machine, timeout logic, action dispatch
- `crates/shux-ui/src/prefix_actions.rs` -- Individual action handlers for each prefix binding
- `crates/shux-ui/src/confirmation.rs` -- Confirmation overlay for destructive operations (close pane/window)
- `crates/shux-ui/src/inline_edit.rs` -- Inline text editing for rename operations
- `crates/shux-ui/tests/prefix_test.rs` -- Unit and integration tests for the prefix system

## Files to Modify

- `crates/shux-ui/src/keybindings.rs` -- Add prefix key detection and routing to prefix module
- `crates/shux-ui/src/lib.rs` -- Register new modules
- `crates/shux-ui/src/compositor.rs` -- Render prefix mode indicator in status area
- `crates/shux-ui/Cargo.toml` -- Add any new dependencies (e.g., `tokio::time` for timeout)

---

## Execution Steps

### Step 1: Define the prefix mode state machine

Create the core state machine in `crates/shux-ui/src/prefix.rs`. The prefix system has three states: `Idle` (normal mode), `WaitingForKey` (prefix pressed, awaiting second key), and `SubMode` (a sub-interaction like rename or confirmation is active).

```rust
use std::time::{Duration, Instant};
use crossterm::event::KeyEvent;

/// How long to wait for the second key after prefix is pressed.
const DEFAULT_PREFIX_TIMEOUT: Duration = Duration::from_millis(1000);

/// The three states of the prefix key system.
#[derive(Debug, Clone)]
pub enum PrefixState {
    /// Normal mode -- input goes to PTY.
    Idle,
    /// Prefix key was pressed; waiting for the second key.
    /// The `Instant` records when the prefix was pressed for timeout detection.
    WaitingForKey { entered_at: Instant },
    /// A sub-mode is active (confirmation dialog, inline rename, etc.).
    SubMode(SubModeKind),
}

#[derive(Debug, Clone)]
pub enum SubModeKind {
    /// Waiting for user to confirm a destructive action (y/n).
    Confirmation(ConfirmationContext),
    /// Inline text editing (rename window/session).
    InlineEdit(InlineEditContext),
}

#[derive(Debug, Clone)]
pub struct ConfirmationContext {
    pub prompt: String,
    pub action: PendingAction,
}

#[derive(Debug, Clone)]
pub enum PendingAction {
    ClosePane,
    CloseWindow,
}

#[derive(Debug, Clone)]
pub struct InlineEditContext {
    pub prompt: String,
    pub buffer: String,
    pub cursor_pos: usize,
    pub target: RenameTarget,
}

#[derive(Debug, Clone)]
pub enum RenameTarget {
    Window,
    Session,
}

/// Result of processing a key event through the prefix system.
#[derive(Debug)]
pub enum PrefixResult {
    /// Key was consumed by the prefix system; do NOT forward to PTY.
    Consumed,
    /// Key was not handled by prefix; forward to PTY/tier-1 handler.
    PassThrough(KeyEvent),
    /// An action should be executed.
    Action(PrefixAction),
    /// Prefix mode timed out; return to idle.
    Timeout,
}
```

### Step 2: Define prefix actions

Define the full set of actions that can be triggered from prefix mode. These correspond directly to PRD section 9.2.

```rust
/// Actions triggered by Prefix + <key> sequences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrefixAction {
    /// Prefix + c: Create a new window in the current session.
    NewWindow,
    /// Prefix + x: Close the current pane (with confirmation overlay).
    ClosePane,
    /// Prefix + X: Close the current window (with confirmation overlay).
    CloseWindow,
    /// Prefix + |: Split the current pane vertically.
    SplitVertical,
    /// Prefix + -: Split the current pane horizontally.
    SplitHorizontal,
    /// Prefix + r: Rename the current window (inline editing).
    RenameWindow,
    /// Prefix + R: Rename the current session (inline editing).
    RenameSession,
    /// Prefix + d: Detach from the current session.
    Detach,
    /// Prefix + [: Enter copy mode (delegated to task 021).
    EnterCopyMode,
    /// Prefix + :: Open command palette (delegated to task 032).
    CommandPalette,
    /// Prefix + ?: Show help overlay (delegated to task 033).
    HelpOverlay,
    /// Prefix + t: Set pane theme (delegated to task 025).
    SetPaneTheme,
    /// Prefix + .: Quick command input.
    QuickCommand,
    /// Prefix + Space: Toggle last active pane.
    ToggleLastPane,
    /// Prefix + Tab: Toggle last active window.
    ToggleLastWindow,
}
```

### Step 3: Implement the key-to-action mapping

Build the lookup table that maps the second key (after prefix) to an action. This table is the default set; task 031 (keybinding configuration) will make it customizable.

```rust
use crossterm::event::{KeyCode, KeyModifiers};

/// Default prefix key-to-action mapping.
/// Returns `None` if the key is not a recognized prefix binding.
pub fn default_prefix_binding(key: &KeyEvent) -> Option<PrefixAction> {
    match (key.code, key.modifiers) {
        (KeyCode::Char('c'), KeyModifiers::NONE) => Some(PrefixAction::NewWindow),
        (KeyCode::Char('x'), KeyModifiers::NONE) => Some(PrefixAction::ClosePane),
        (KeyCode::Char('X'), KeyModifiers::SHIFT) => Some(PrefixAction::CloseWindow),
        (KeyCode::Char('|'), KeyModifiers::NONE) => Some(PrefixAction::SplitVertical),
        // Note: '|' requires Shift on most keyboards, but crossterm reports it
        // as KeyCode::Char('|') with SHIFT already resolved.
        (KeyCode::Char('-'), KeyModifiers::NONE) => Some(PrefixAction::SplitHorizontal),
        (KeyCode::Char('r'), KeyModifiers::NONE) => Some(PrefixAction::RenameWindow),
        (KeyCode::Char('R'), KeyModifiers::SHIFT) => Some(PrefixAction::RenameSession),
        (KeyCode::Char('d'), KeyModifiers::NONE) => Some(PrefixAction::Detach),
        (KeyCode::Char('['), KeyModifiers::NONE) => Some(PrefixAction::EnterCopyMode),
        (KeyCode::Char(':'), KeyModifiers::NONE) => Some(PrefixAction::CommandPalette),
        (KeyCode::Char('?'), KeyModifiers::NONE) => Some(PrefixAction::HelpOverlay),
        (KeyCode::Char('t'), KeyModifiers::NONE) => Some(PrefixAction::SetPaneTheme),
        (KeyCode::Char('.'), KeyModifiers::NONE) => Some(PrefixAction::QuickCommand),
        (KeyCode::Char(' '), KeyModifiers::NONE) => Some(PrefixAction::ToggleLastPane),
        (KeyCode::Tab, KeyModifiers::NONE) => Some(PrefixAction::ToggleLastWindow),
        _ => None,
    }
}
```

### Step 4: Implement the prefix mode processor

The core processing loop integrates with the existing input pipeline from task 018. When the input handler in `keybindings.rs` detects the prefix key, it transitions to `WaitingForKey`. Subsequent keys are routed to the prefix processor.

```rust
impl PrefixMode {
    pub fn new(timeout: Duration) -> Self {
        Self {
            state: PrefixState::Idle,
            timeout,
            prefix_key: default_prefix_key(), // Ctrl+Space
        }
    }

    /// Process a key event. Returns how the caller should handle it.
    pub fn process_key(&mut self, key: KeyEvent) -> PrefixResult {
        match &self.state {
            PrefixState::Idle => {
                if self.is_prefix_key(&key) {
                    self.state = PrefixState::WaitingForKey {
                        entered_at: Instant::now(),
                    };
                    PrefixResult::Consumed
                } else {
                    PrefixResult::PassThrough(key)
                }
            }
            PrefixState::WaitingForKey { entered_at } => {
                // Check timeout first.
                if entered_at.elapsed() > self.timeout {
                    self.state = PrefixState::Idle;
                    return PrefixResult::Timeout;
                }

                // Escape cancels prefix mode.
                if key.code == KeyCode::Esc {
                    self.state = PrefixState::Idle;
                    return PrefixResult::Consumed;
                }

                // Look up binding.
                if let Some(action) = default_prefix_binding(&key) {
                    self.state = PrefixState::Idle;
                    PrefixResult::Action(action)
                } else {
                    // Unrecognized key -- discard it, return to idle.
                    self.state = PrefixState::Idle;
                    PrefixResult::Consumed
                }
            }
            PrefixState::SubMode(sub) => {
                self.process_sub_mode(key, sub.clone())
            }
        }
    }

    /// Check if prefix mode has timed out (called from event loop tick).
    pub fn check_timeout(&mut self) -> bool {
        if let PrefixState::WaitingForKey { entered_at } = &self.state {
            if entered_at.elapsed() > self.timeout {
                self.state = PrefixState::Idle;
                return true; // Timed out -- caller should clear visual indicator.
            }
        }
        false
    }

    /// Returns true if the prefix mode is active (for visual indicator).
    pub fn is_active(&self) -> bool {
        !matches!(self.state, PrefixState::Idle)
    }

    /// Returns descriptive text for the status area.
    pub fn status_text(&self) -> Option<&str> {
        match &self.state {
            PrefixState::Idle => None,
            PrefixState::WaitingForKey { .. } => Some("PREFIX"),
            PrefixState::SubMode(SubModeKind::Confirmation(_)) => Some("CONFIRM"),
            PrefixState::SubMode(SubModeKind::InlineEdit(_)) => Some("RENAME"),
        }
    }
}
```

### Step 5: Implement the prefix key detection

The default prefix key is `Ctrl+Space`. In crossterm, this is represented as `KeyCode::Char(' ')` with `KeyModifiers::CONTROL`. Configure this from the `[ui] prefix` config value.

```rust
/// Parse a prefix key string from config (e.g., "ctrl+space") into a
/// crossterm KeyEvent pattern.
pub fn parse_prefix_key(s: &str) -> Result<PrefixKeyPattern, PrefixError> {
    let parts: Vec<&str> = s.split('+').collect();
    let mut modifiers = KeyModifiers::empty();
    let mut key_code = None;

    for part in &parts {
        match part.to_lowercase().as_str() {
            "ctrl" | "control" => modifiers |= KeyModifiers::CONTROL,
            "alt" | "meta" => modifiers |= KeyModifiers::ALT,
            "shift" => modifiers |= KeyModifiers::SHIFT,
            "space" => key_code = Some(KeyCode::Char(' ')),
            "tab" => key_code = Some(KeyCode::Tab),
            "enter" | "return" => key_code = Some(KeyCode::Enter),
            "esc" | "escape" => key_code = Some(KeyCode::Esc),
            s if s.len() == 1 => key_code = Some(KeyCode::Char(s.chars().next().unwrap())),
            other => return Err(PrefixError::UnknownKey(other.to_string())),
        }
    }

    let code = key_code.ok_or(PrefixError::MissingKeyCode)?;
    Ok(PrefixKeyPattern { code, modifiers })
}

#[derive(Debug, Clone)]
pub struct PrefixKeyPattern {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl PrefixKeyPattern {
    pub fn matches(&self, key: &KeyEvent) -> bool {
        key.code == self.code && key.modifiers.contains(self.modifiers)
    }
}

fn default_prefix_key() -> PrefixKeyPattern {
    PrefixKeyPattern {
        code: KeyCode::Char(' '),
        modifiers: KeyModifiers::CONTROL,
    }
}
```

### Step 6: Implement confirmation overlay for destructive actions

When the user presses `Prefix + x` (close pane) or `Prefix + X` (close window), display a confirmation overlay rather than immediately executing the action.

```rust
// crates/shux-ui/src/confirmation.rs

use crossterm::event::{KeyCode, KeyEvent};

pub struct ConfirmationOverlay {
    pub prompt: String,
    pub confirmed: Option<bool>,
}

impl ConfirmationOverlay {
    pub fn close_pane() -> Self {
        Self {
            prompt: "Close this pane? (y/n)".to_string(),
            confirmed: None,
        }
    }

    pub fn close_window() -> Self {
        Self {
            prompt: "Close this window and all its panes? (y/n)".to_string(),
            confirmed: None,
        }
    }

    /// Process a key. Returns `Some(true)` for confirm, `Some(false)` for cancel,
    /// `None` if the key was not a decision key (consumed but no decision yet).
    pub fn process_key(&mut self, key: KeyEvent) -> Option<bool> {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.confirmed = Some(true);
                Some(true)
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.confirmed = Some(false);
                Some(false)
            }
            _ => None, // Ignore other keys.
        }
    }

    /// Render the confirmation overlay as a centered box.
    /// Returns a list of (row, col, styled_text) instructions for the compositor.
    pub fn render(&self, area_width: u16, area_height: u16) -> ConfirmationFrame {
        let box_width = (self.prompt.len() + 4).min(area_width as usize);
        let box_height = 3;
        let x = (area_width as usize - box_width) / 2;
        let y = (area_height as usize - box_height) / 2;

        ConfirmationFrame {
            x: x as u16,
            y: y as u16,
            width: box_width as u16,
            height: box_height as u16,
            prompt: self.prompt.clone(),
        }
    }
}

pub struct ConfirmationFrame {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub prompt: String,
}
```

### Step 7: Implement inline editing for rename operations

When the user presses `Prefix + r` or `Prefix + R`, an inline text editing widget appears in the status area (or overlaying the window/session name) for entering the new name.

```rust
// crates/shux-ui/src/inline_edit.rs

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone)]
pub struct InlineEditor {
    pub prompt: String,
    pub buffer: String,
    pub cursor: usize,
    pub initial_value: String,
}

/// Result of processing a key in the inline editor.
#[derive(Debug)]
pub enum EditResult {
    /// Still editing; key was consumed.
    Continue,
    /// User confirmed the edit (pressed Enter).
    Confirm(String),
    /// User cancelled the edit (pressed Escape).
    Cancel,
}

impl InlineEditor {
    pub fn new(prompt: &str, initial: &str) -> Self {
        Self {
            prompt: prompt.to_string(),
            buffer: initial.to_string(),
            cursor: initial.len(),
            initial_value: initial.to_string(),
        }
    }

    pub fn process_key(&mut self, key: KeyEvent) -> EditResult {
        match key.code {
            KeyCode::Enter => EditResult::Confirm(self.buffer.clone()),
            KeyCode::Esc => EditResult::Cancel,
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.buffer.remove(self.cursor);
                }
                EditResult::Continue
            }
            KeyCode::Delete => {
                if self.cursor < self.buffer.len() {
                    self.buffer.remove(self.cursor);
                }
                EditResult::Continue
            }
            KeyCode::Left => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                }
                EditResult::Continue
            }
            KeyCode::Right => {
                if self.cursor < self.buffer.len() {
                    self.cursor += 1;
                }
                EditResult::Continue
            }
            KeyCode::Home => {
                self.cursor = 0;
                EditResult::Continue
            }
            KeyCode::End => {
                self.cursor = self.buffer.len();
                EditResult::Continue
            }
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cursor = 0;
                EditResult::Continue
            }
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cursor = self.buffer.len();
                EditResult::Continue
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.buffer.drain(..self.cursor);
                self.cursor = 0;
                EditResult::Continue
            }
            KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.buffer.truncate(self.cursor);
                EditResult::Continue
            }
            KeyCode::Char(c) => {
                self.buffer.insert(self.cursor, c);
                self.cursor += 1;
                EditResult::Continue
            }
            _ => EditResult::Continue,
        }
    }

    /// Returns the display string with cursor position marked.
    pub fn display_text(&self) -> (&str, usize) {
        (&self.buffer, self.cursor)
    }
}
```

### Step 8: Implement the action dispatcher

Create `prefix_actions.rs` to handle executing each `PrefixAction`. This module bridges the prefix system to the core daemon operations via the RPC client or direct method calls (depending on whether the UI runs in-process or as a client).

```rust
// crates/shux-ui/src/prefix_actions.rs

use crate::prefix::{PrefixAction, PrefixMode, SubModeKind, ConfirmationContext, PendingAction};
use crate::confirmation::ConfirmationOverlay;
use crate::inline_edit::InlineEditor;

/// Dispatch a prefix action. Some actions execute immediately, others
/// transition to a sub-mode (confirmation, rename).
pub async fn dispatch_action(
    action: PrefixAction,
    prefix: &mut PrefixMode,
    ctx: &mut UiContext,
) -> Result<(), ActionError> {
    match action {
        PrefixAction::NewWindow => {
            ctx.rpc_client.call("window.create", serde_json::json!({
                "session_id": ctx.active_session_id(),
            })).await?;
        }
        PrefixAction::ClosePane => {
            // Enter confirmation sub-mode.
            prefix.enter_confirmation(
                ConfirmationOverlay::close_pane(),
                PendingAction::ClosePane,
            );
        }
        PrefixAction::CloseWindow => {
            prefix.enter_confirmation(
                ConfirmationOverlay::close_window(),
                PendingAction::CloseWindow,
            );
        }
        PrefixAction::SplitVertical => {
            ctx.rpc_client.call("pane.split", serde_json::json!({
                "pane_id": ctx.active_pane_id(),
                "direction": "vertical",
            })).await?;
        }
        PrefixAction::SplitHorizontal => {
            ctx.rpc_client.call("pane.split", serde_json::json!({
                "pane_id": ctx.active_pane_id(),
                "direction": "horizontal",
            })).await?;
        }
        PrefixAction::RenameWindow => {
            let current_name = ctx.active_window_title().to_string();
            prefix.enter_inline_edit(
                InlineEditor::new("Rename window: ", &current_name),
                RenameTarget::Window,
            );
        }
        PrefixAction::RenameSession => {
            let current_name = ctx.active_session_name().to_string();
            prefix.enter_inline_edit(
                InlineEditor::new("Rename session: ", &current_name),
                RenameTarget::Session,
            );
        }
        PrefixAction::Detach => {
            ctx.request_detach();
        }
        PrefixAction::EnterCopyMode => {
            // Delegate to copy mode module (task 021).
            ctx.enter_copy_mode();
        }
        PrefixAction::CommandPalette => {
            // Delegate to command palette module (task 032).
            ctx.open_command_palette();
        }
        PrefixAction::HelpOverlay => {
            // Delegate to help overlay module (task 033).
            ctx.show_help_overlay();
        }
        PrefixAction::SetPaneTheme => {
            // Delegate to theme picker (task 025).
            ctx.open_theme_picker();
        }
        PrefixAction::QuickCommand => {
            // Open a quick command prompt (mini command palette).
            ctx.open_quick_command();
        }
        PrefixAction::ToggleLastPane => {
            ctx.toggle_last_pane();
        }
        PrefixAction::ToggleLastWindow => {
            ctx.toggle_last_window();
        }
    }
    Ok(())
}
```

### Step 9: Integrate with keybindings.rs

Modify `crates/shux-ui/src/keybindings.rs` to integrate the prefix system with the existing Tier 1 keybinding handler. The input pipeline order is:

1. If a sub-mode (confirmation/rename) is active, route to sub-mode handler.
2. If prefix `WaitingForKey` is active, route to prefix key lookup.
3. Check for prefix key press (to enter WaitingForKey).
4. Check Tier 1 bare-key bindings (from task 018).
5. Fall through to PTY input.

```rust
// In crates/shux-ui/src/keybindings.rs

pub async fn handle_key_event(
    key: KeyEvent,
    prefix: &mut PrefixMode,
    ctx: &mut UiContext,
) -> InputResult {
    // Step 1: Route through prefix system first.
    match prefix.process_key(key) {
        PrefixResult::Consumed => return InputResult::Consumed,
        PrefixResult::Action(action) => {
            if let Err(e) = dispatch_action(action, prefix, ctx).await {
                ctx.show_error(&format!("Action failed: {e}"));
            }
            return InputResult::Consumed;
        }
        PrefixResult::Timeout => {
            ctx.clear_prefix_indicator();
            return InputResult::Consumed;
        }
        PrefixResult::PassThrough(key) => {
            // Not consumed by prefix -- fall through to Tier 1 / PTY.
        }
    }

    // Step 2: Tier 1 bare-key bindings (from task 018).
    if let Some(action) = tier1_binding(&key) {
        execute_tier1(action, ctx).await;
        return InputResult::Consumed;
    }

    // Step 3: Forward to PTY.
    InputResult::ForwardToPty(key)
}
```

### Step 10: Add prefix mode visual indicator to compositor

Modify `crates/shux-ui/src/compositor.rs` to display a visual indicator in the status area when prefix mode is active. This is critical for discoverability -- the user must know that shux is waiting for the second key.

```rust
// In the compositor's render_status_bar method:

fn render_prefix_indicator(&self, prefix: &PrefixMode) -> Option<StatusSegment> {
    prefix.status_text().map(|text| {
        StatusSegment {
            text: format!(" {} ", text),
            style: Style {
                fg: Some(Color::Black),
                bg: Some(Color::Yellow),
                bold: true,
                ..Default::default()
            },
            position: StatusPosition::Left,
            priority: StatusPriority::High, // Always visible.
        }
    })
}
```

The indicator appears as a bright yellow `[PREFIX]` badge when the user presses `Ctrl+Space`, changes to `[CONFIRM]` during confirmation dialogs, and `[RENAME]` during inline editing.

### Step 11: Implement prefix timeout with tokio

The prefix timeout is implemented using a `tokio::time::sleep` future that races against the next key event. When the timeout fires, the prefix mode returns to idle and the visual indicator is cleared.

```rust
// In the main event loop (crates/shux-ui/src/event_loop.rs or similar):

loop {
    let timeout_future = if prefix.is_active() {
        let remaining = prefix.remaining_timeout();
        tokio::time::sleep(remaining).boxed()
    } else {
        // No active prefix -- use a very long sleep (effectively infinite).
        tokio::time::sleep(Duration::from_secs(86400)).boxed()
    };

    tokio::select! {
        event = crossterm_event_stream.next() => {
            if let Some(Ok(Event::Key(key))) = event {
                handle_key_event(key, &mut prefix, &mut ctx).await;
            }
        }
        _ = timeout_future => {
            if prefix.check_timeout() {
                ctx.clear_prefix_indicator();
                ctx.request_redraw();
            }
        }
    }
}
```

### Step 12: Track last-active pane and window for toggle operations

`Prefix + Space` (toggle last pane) and `Prefix + Tab` (toggle last window) require tracking the previously-focused pane and window. This is maintained in the UI context.

```rust
pub struct FocusHistory {
    /// Stack of recently focused pane IDs (most recent first).
    pane_history: Vec<PaneId>,
    /// Stack of recently focused window IDs (most recent first).
    window_history: Vec<WindowId>,
    /// Maximum history depth.
    max_depth: usize,
}

impl FocusHistory {
    pub fn new() -> Self {
        Self {
            pane_history: Vec::new(),
            window_history: Vec::new(),
            max_depth: 32,
        }
    }

    pub fn push_pane(&mut self, id: PaneId) {
        // Remove if already in history to avoid duplicates.
        self.pane_history.retain(|p| p != &id);
        self.pane_history.insert(0, id);
        self.pane_history.truncate(self.max_depth);
    }

    pub fn push_window(&mut self, id: WindowId) {
        self.window_history.retain(|w| w != &id);
        self.window_history.insert(0, id);
        self.window_history.truncate(self.max_depth);
    }

    /// Get the previously focused pane (index 1, since 0 is current).
    pub fn last_pane(&self) -> Option<&PaneId> {
        self.pane_history.get(1)
    }

    /// Get the previously focused window.
    pub fn last_window(&self) -> Option<&WindowId> {
        self.window_history.get(1)
    }
}
```

---

## Verification

### Functional

```bash
# Build the project
cargo build --workspace

# Run with a test session
cargo run -p shux -- new -s test

# In the TUI:
# 1. Press Ctrl+Space -- yellow "PREFIX" indicator should appear in status area
# 2. Wait >1 second -- indicator should disappear (timeout)
# 3. Press Ctrl+Space then 'c' -- new window should be created
# 4. Press Ctrl+Space then '|' -- pane should split vertically
# 5. Press Ctrl+Space then '-' -- pane should split horizontally
# 6. Press Ctrl+Space then 'x' -- confirmation overlay "Close this pane? (y/n)"
#    Press 'n' -- overlay dismissed, pane still alive
#    Repeat, press 'y' -- pane closed
# 7. Press Ctrl+Space then 'r' -- inline rename appears with current window name
#    Type new name, press Enter -- window renamed
# 8. Press Ctrl+Space then 'd' -- client detaches
# 9. Press Ctrl+Space then Escape -- prefix cancelled, no action taken
# 10. Press Ctrl+Space then unbound key (e.g., 'z') -- prefix cancelled, nothing happens
```

### Tests

```bash
# Run unit tests for the prefix system
cargo nextest run -p shux-ui --lib prefix

# Run integration tests
cargo nextest run -p shux-ui --test prefix_test

# Test coverage of all bindings
cargo nextest run -p shux-ui -- prefix_bindings

# Specific test scenarios to verify:
# - All 15 prefix bindings are correctly mapped
# - Timeout triggers state transition to Idle
# - Escape cancels prefix mode
# - Unrecognized keys are consumed (not forwarded)
# - Confirmation overlay accepts y/n/Escape
# - Inline editor handles all editing keys (backspace, delete, home, end, ctrl+a/e/u/k)
# - Prefix key is configurable via parse_prefix_key()
# - Focus history correctly tracks last pane/window
```

---

## Completion Criteria

- [ ] Pressing Ctrl+Space enters prefix mode with visual indicator ("PREFIX") in status area
- [ ] All 15 prefix bindings from PRD section 9.2 are implemented and mapped correctly
- [ ] Prefix timeout (default 1s) returns to idle mode and clears indicator
- [ ] Escape cancels prefix mode immediately
- [ ] Unrecognized second keys are consumed (not forwarded to PTY)
- [ ] `Prefix + x` shows confirmation overlay; y confirms, n/Esc cancels
- [ ] `Prefix + X` shows confirmation overlay for closing window
- [ ] `Prefix + r` opens inline editor pre-filled with current window name
- [ ] `Prefix + R` opens inline editor pre-filled with current session name
- [ ] Inline editor supports: character input, backspace, delete, left/right, home/end, Ctrl+a/e/u/k
- [ ] `Prefix + Space` toggles to the last active pane
- [ ] `Prefix + Tab` toggles to the last active window
- [ ] Focus history correctly tracks pane and window changes
- [ ] `Prefix + d` cleanly detaches the client
- [ ] Stub hooks exist for copy mode (021), command palette (032), help overlay (033), theme picker (025)
- [ ] Prefix key is parseable from config string ("ctrl+space", "ctrl+b", etc.)
- [ ] Unit tests cover all state transitions in the prefix state machine
- [ ] Integration tests verify end-to-end prefix key sequences
- [ ] No keys are accidentally forwarded to PTY during prefix mode

---

## Commit Message

```
feat(ui): implement prefix key system (Tier 2 keybindings)

- Add Ctrl+Space prefix with 15 key bindings (PRD §9.2)
- Implement prefix state machine with 1s timeout
- Add confirmation overlays for destructive actions (close pane/window)
- Add inline text editor for rename operations (window/session)
- Track focus history for toggle-last-pane/window
- Display visual "PREFIX" indicator in status area
- Configurable prefix key parsing from TOML string notation
```

---

## Session Protocol

1. **Before starting:** Read tasks 018 (Tier 1 keybindings) to understand the input pipeline. Read PRD section 9.2 for the complete binding table. Verify that `keybindings.rs` exists from task 018.
2. **During:** Implement in order: state machine (Steps 1-4), key detection (Step 5), sub-modes (Steps 6-7), action dispatch (Step 8), integration (Steps 9-10), timeout (Step 11), focus history (Step 12). Run `cargo check` after each step. Run `cargo test` after Steps 4, 7, and 12.
3. **After:** Run full verification suite. Manually test each of the 15 prefix bindings in a running TUI session. Update `docs/PROGRESS.md` (mark 019 done, add session log entry). Update `CLAUDE.md` Learnings with any crossterm keyboard encoding quirks discovered.
