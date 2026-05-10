# 018 — Tier 1 Keybindings (Bare Keys)

**Status:** Partial — `crates/shux-ui/src/attach.rs::key_to_bare_action` ships Alt+Enter (smart split), Alt+| / Alt+\ (vertical split), Alt+- (horizontal split), Alt+arrows (directional focus), Alt+z (toggle zoom), Alt+x (kill pane), Alt+Tab (focus next). **Still missing per PRD §9.1**: bare **Alt+h/j/k/l** (only the prefix-mode `Prefix h/j/k/l` works today), bare **Alt+n/p** for next/prev window, and **Alt+1..9** for window-by-index. Tier-2 prefix bindings (Ctrl+Space + h/j/k/l/n/p) are wired in `key_to_prefix_action` and verified via `test_017_attach_multipane.py`. Closing the bare-key gap is small (one match in `key_to_bare_action`) and rolls into the M1 quality gate (034) or a focused follow-up PR.
**Depends On:** 017
**Parallelizable With:** 020, 022

---

## Problem

With multi-pane rendering in place (task 017), users can see multiple panes on screen but have no keyboard-driven way to navigate between them, create new panes, switch windows, or toggle zoom. All interaction requires the JSON-RPC API.

Tier 1 keybindings are the "bare keys" -- modifier+key combinations that work without any prefix sequence. They are the most frequently used keybindings and must feel instant. These keys are intercepted by the input decoder before being forwarded to the PTY, so they must not conflict with common terminal application keybindings.

The Alt key modifier is chosen because it has the fewest conflicts with terminal applications. Alt+h/j/k/l follows vim conventions for directional navigation. Alt+Shift for resize builds naturally on the navigate bindings. Alt+n/p for next/previous window follows a widespread convention. Alt+1..9 for window-by-index mirrors most terminal multiplexers.

Key routing is the critical design challenge: the input decoder receives raw terminal input, checks whether it matches a keybinding, and either executes the bound action or forwards the input to the active pane's PTY. This must happen with zero perceptible latency.

## PRD Reference

- **PRD section 9.1 (Tier 1: Bare keys)**: Complete keybinding table with all Tier 1 bindings
- **PRD section 6.1 (UX & keybindings)**: Graded keybindings system, discoverability
- **PRD section 9.4 (Customization)**: All keybindings are remappable in TOML config
- **PRD section 14.1 (Performance)**: Keypress to visible update p50 <= 8ms

---

## Files to Create

- `crates/shux-ui/src/keybindings.rs` — Keybinding registry, matching, and action execution
- `crates/shux-core/src/navigation.rs` — Directional navigation logic (may extend from task 015)
- `crates/shux-ui/src/key_router.rs` — Key routing: intercept keybindings vs forward to PTY
- `crates/shux-ui/tests/keybinding_tests.rs` — Unit tests for key matching and routing

## Files to Modify

- `crates/shux-ui/src/lib.rs` — Export keybinding and key_router modules
- `crates/shux-ui/src/input.rs` — Integrate key router into the input decode pipeline
- `crates/shux-ui/src/compositor.rs` — Wire keybinding actions to compositor state changes

---

## Execution Steps

### Step 1: Define the action model

Every keybinding maps to an `Action` enum. Actions are the bridge between input and state changes. They decouple keybinding configuration from implementation.

In `crates/shux-ui/src/keybindings.rs`:

```rust
use crossterm::event::{KeyCode, KeyModifiers, KeyEvent};
use uuid::Uuid;

/// An action that can be triggered by a keybinding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    // ── Pane navigation ──────────────────────────
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,

    // ── Pane resize ──────────────────────────────
    ResizeLeft,
    ResizeRight,
    ResizeUp,
    ResizeDown,

    // ── Window navigation ────────────────────────
    NextWindow,
    PreviousWindow,
    WindowByIndex(usize), // 1-indexed

    // ── Pane operations ──────────────────────────
    ToggleZoom,
    SmartSplit, // Alt+Enter: smart split direction

    // ── Forwarded to PTY ─────────────────────────
    /// No keybinding matched; forward the raw input to the active pane.
    ForwardToPty,
}

impl Action {
    /// Human-readable description for the help overlay.
    pub fn description(&self) -> String {
        match self {
            Action::FocusLeft => "Focus pane left".to_string(),
            Action::FocusRight => "Focus pane right".to_string(),
            Action::FocusUp => "Focus pane up".to_string(),
            Action::FocusDown => "Focus pane down".to_string(),
            Action::ResizeLeft => "Resize pane left".to_string(),
            Action::ResizeRight => "Resize pane right".to_string(),
            Action::ResizeUp => "Resize pane up".to_string(),
            Action::ResizeDown => "Resize pane down".to_string(),
            Action::NextWindow => "Next window".to_string(),
            Action::PreviousWindow => "Previous window".to_string(),
            Action::WindowByIndex(n) => format!("Switch to window {n}"),
            Action::ToggleZoom => "Toggle zoom on current pane".to_string(),
            Action::SmartSplit => "New pane (smart split)".to_string(),
            Action::ForwardToPty => "(forward to terminal)".to_string(),
        }
    }
}
```

### Step 2: Define key patterns and the keybinding registry

A `KeyPattern` describes a key combination (modifier + key code). The `KeybindingRegistry` maps patterns to actions.

```rust
/// A key pattern to match against input events.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyPattern {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl KeyPattern {
    pub fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }

    /// Parse a key pattern from a string like "alt-h", "alt-shift-h", "alt-1".
    pub fn from_str(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split('-').collect();
        if parts.is_empty() {
            return None;
        }

        let mut modifiers = KeyModifiers::empty();
        let key_str = parts.last()?;

        for part in &parts[..parts.len() - 1] {
            match part.to_lowercase().as_str() {
                "alt" => modifiers |= KeyModifiers::ALT,
                "ctrl" | "control" => modifiers |= KeyModifiers::CONTROL,
                "shift" => modifiers |= KeyModifiers::SHIFT,
                "super" | "meta" => modifiers |= KeyModifiers::SUPER,
                _ => return None,
            }
        }

        let code = match *key_str {
            "h" => KeyCode::Char('h'),
            "j" => KeyCode::Char('j'),
            "k" => KeyCode::Char('k'),
            "l" => KeyCode::Char('l'),
            "H" => KeyCode::Char('H'),
            "J" => KeyCode::Char('J'),
            "K" => KeyCode::Char('K'),
            "L" => KeyCode::Char('L'),
            "n" => KeyCode::Char('n'),
            "p" => KeyCode::Char('p'),
            "z" => KeyCode::Char('z'),
            "enter" | "Enter" => KeyCode::Enter,
            "1" => KeyCode::Char('1'),
            "2" => KeyCode::Char('2'),
            "3" => KeyCode::Char('3'),
            "4" => KeyCode::Char('4'),
            "5" => KeyCode::Char('5'),
            "6" => KeyCode::Char('6'),
            "7" => KeyCode::Char('7'),
            "8" => KeyCode::Char('8'),
            "9" => KeyCode::Char('9'),
            c if c.len() == 1 => KeyCode::Char(c.chars().next().unwrap()),
            _ => return None,
        };

        Some(Self { code, modifiers })
    }

    /// Convert to a human-readable string for display.
    pub fn display(&self) -> String {
        let mut parts = Vec::new();
        if self.modifiers.contains(KeyModifiers::CONTROL) {
            parts.push("Ctrl");
        }
        if self.modifiers.contains(KeyModifiers::ALT) {
            parts.push("Alt");
        }
        if self.modifiers.contains(KeyModifiers::SHIFT) {
            parts.push("Shift");
        }

        let key = match self.code {
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Enter => "Enter".to_string(),
            KeyCode::Tab => "Tab".to_string(),
            KeyCode::Esc => "Esc".to_string(),
            _ => format!("{:?}", self.code),
        };
        parts.push(&key);

        // Join format: workaround for borrow issues
        let parts_owned: Vec<String> = parts.iter().map(|s| s.to_string()).collect();
        parts_owned.join("+")
    }
}

/// Registry of keybindings mapping key patterns to actions.
pub struct KeybindingRegistry {
    bindings: std::collections::HashMap<KeyPattern, Action>,
}

impl KeybindingRegistry {
    /// Create a registry with the default Tier 1 bindings (PRD section 9.1).
    pub fn with_defaults() -> Self {
        let mut bindings = std::collections::HashMap::new();

        // ── Pane navigation: Alt+h/j/k/l ──────────────
        bindings.insert(
            KeyPattern::new(KeyCode::Char('h'), KeyModifiers::ALT),
            Action::FocusLeft,
        );
        bindings.insert(
            KeyPattern::new(KeyCode::Char('j'), KeyModifiers::ALT),
            Action::FocusDown,
        );
        bindings.insert(
            KeyPattern::new(KeyCode::Char('k'), KeyModifiers::ALT),
            Action::FocusUp,
        );
        bindings.insert(
            KeyPattern::new(KeyCode::Char('l'), KeyModifiers::ALT),
            Action::FocusRight,
        );

        // ── Pane resize: Alt+Shift+h/j/k/l (Alt+H/J/K/L) ──
        bindings.insert(
            KeyPattern::new(
                KeyCode::Char('H'),
                KeyModifiers::ALT | KeyModifiers::SHIFT,
            ),
            Action::ResizeLeft,
        );
        bindings.insert(
            KeyPattern::new(
                KeyCode::Char('J'),
                KeyModifiers::ALT | KeyModifiers::SHIFT,
            ),
            Action::ResizeDown,
        );
        bindings.insert(
            KeyPattern::new(
                KeyCode::Char('K'),
                KeyModifiers::ALT | KeyModifiers::SHIFT,
            ),
            Action::ResizeUp,
        );
        bindings.insert(
            KeyPattern::new(
                KeyCode::Char('L'),
                KeyModifiers::ALT | KeyModifiers::SHIFT,
            ),
            Action::ResizeRight,
        );

        // ── Window navigation: Alt+n, Alt+p ────────────
        bindings.insert(
            KeyPattern::new(KeyCode::Char('n'), KeyModifiers::ALT),
            Action::NextWindow,
        );
        bindings.insert(
            KeyPattern::new(KeyCode::Char('p'), KeyModifiers::ALT),
            Action::PreviousWindow,
        );

        // ── Window by index: Alt+1..9 ──────────────────
        for i in 1..=9u8 {
            bindings.insert(
                KeyPattern::new(
                    KeyCode::Char((b'0' + i) as char),
                    KeyModifiers::ALT,
                ),
                Action::WindowByIndex(i as usize),
            );
        }

        // ── Zoom: Alt+z ────────────────────────────────
        bindings.insert(
            KeyPattern::new(KeyCode::Char('z'), KeyModifiers::ALT),
            Action::ToggleZoom,
        );

        // ── Smart split: Alt+Enter ─────────────────────
        bindings.insert(
            KeyPattern::new(KeyCode::Enter, KeyModifiers::ALT),
            Action::SmartSplit,
        );

        Self { bindings }
    }

    /// Look up an action for a key event.
    pub fn lookup(&self, event: &KeyEvent) -> Option<&Action> {
        let pattern = KeyPattern::new(event.code, event.modifiers);
        self.bindings.get(&pattern)
    }

    /// Override a binding (for user customization).
    pub fn set(&mut self, pattern: KeyPattern, action: Action) {
        self.bindings.insert(pattern, action);
    }

    /// Remove a binding.
    pub fn remove(&mut self, pattern: &KeyPattern) -> Option<Action> {
        self.bindings.remove(pattern)
    }

    /// List all bindings (for help overlay).
    pub fn list(&self) -> Vec<(&KeyPattern, &Action)> {
        let mut entries: Vec<_> = self.bindings.iter().collect();
        entries.sort_by(|a, b| a.0.display().cmp(&b.0.display()));
        entries
    }

    /// Apply user overrides from config (HashMap<String, String>).
    /// Key: "alt-h", Value: "focus-left" or similar action name.
    pub fn apply_overrides(&mut self, overrides: &std::collections::HashMap<String, String>) {
        for (key_str, action_str) in overrides {
            if let Some(pattern) = KeyPattern::from_str(key_str) {
                if let Some(action) = parse_action_name(action_str) {
                    self.bindings.insert(pattern, action);
                } else {
                    tracing::warn!("Unknown action in keybinding config: {}", action_str);
                }
            } else {
                tracing::warn!("Invalid key pattern in config: {}", key_str);
            }
        }
    }
}

fn parse_action_name(name: &str) -> Option<Action> {
    match name {
        "focus-left" => Some(Action::FocusLeft),
        "focus-right" => Some(Action::FocusRight),
        "focus-up" => Some(Action::FocusUp),
        "focus-down" => Some(Action::FocusDown),
        "resize-left" => Some(Action::ResizeLeft),
        "resize-right" => Some(Action::ResizeRight),
        "resize-up" => Some(Action::ResizeUp),
        "resize-down" => Some(Action::ResizeDown),
        "next-window" => Some(Action::NextWindow),
        "previous-window" | "prev-window" => Some(Action::PreviousWindow),
        "toggle-zoom" | "zoom" => Some(Action::ToggleZoom),
        "smart-split" | "split" => Some(Action::SmartSplit),
        _ => {
            // Check for window-by-index: "window-1" through "window-9"
            if let Some(num_str) = name.strip_prefix("window-") {
                if let Ok(n) = num_str.parse::<usize>() {
                    if (1..=9).contains(&n) {
                        return Some(Action::WindowByIndex(n));
                    }
                }
            }
            None
        }
    }
}
```

### Step 3: Implement the key router

The key router is the decision point in the input pipeline. For each key event, it checks the keybinding registry and either executes the bound action or forwards the input bytes to the active pane's PTY.

In `crates/shux-ui/src/key_router.rs`:

```rust
use crossterm::event::KeyEvent;
use crate::keybindings::{KeybindingRegistry, Action};

/// Result of routing a key event.
#[derive(Debug)]
pub enum KeyRouteResult {
    /// Key matched a binding and the action was executed.
    Handled(Action),
    /// Key did not match any binding; forward to PTY.
    Forward,
}

/// The key router decides whether to handle a key event or forward it to the PTY.
pub struct KeyRouter {
    registry: KeybindingRegistry,
}

impl KeyRouter {
    pub fn new(registry: KeybindingRegistry) -> Self {
        Self { registry }
    }

    /// Route a key event: check bindings, return action or forward.
    pub fn route(&self, event: &KeyEvent) -> KeyRouteResult {
        if let Some(action) = self.registry.lookup(event) {
            KeyRouteResult::Handled(action.clone())
        } else {
            KeyRouteResult::Forward
        }
    }

    /// Get a reference to the keybinding registry.
    pub fn registry(&self) -> &KeybindingRegistry {
        &self.registry
    }

    /// Get a mutable reference for runtime reconfiguration.
    pub fn registry_mut(&mut self) -> &mut KeybindingRegistry {
        &mut self.registry
    }
}
```

### Step 4: Implement the action executor

The action executor translates actions into state mutations. It connects the keybinding system to the core mutation channel.

```rust
use uuid::Uuid;
use shux_core::navigation::{Direction, find_neighbor, smart_split_direction};
use shux_core::layout::{SplitDirection, Rect, LayoutNode};

/// Executes actions resulting from keybinding matches.
pub struct ActionExecutor {
    mutation_tx: tokio::sync::mpsc::Sender<Mutation>,
}

impl ActionExecutor {
    pub fn new(mutation_tx: tokio::sync::mpsc::Sender<Mutation>) -> Self {
        Self { mutation_tx }
    }

    /// Execute an action in the context of the current session/window state.
    pub async fn execute(
        &self,
        action: &Action,
        ctx: &ActionContext,
    ) -> anyhow::Result<()> {
        match action {
            // ── Pane navigation ──────────────────────────
            Action::FocusLeft => self.focus_direction(ctx, Direction::Left).await,
            Action::FocusRight => self.focus_direction(ctx, Direction::Right).await,
            Action::FocusUp => self.focus_direction(ctx, Direction::Up).await,
            Action::FocusDown => self.focus_direction(ctx, Direction::Down).await,

            // ── Pane resize ──────────────────────────────
            Action::ResizeLeft => self.resize_pane(ctx, -2, 0).await,
            Action::ResizeRight => self.resize_pane(ctx, 2, 0).await,
            Action::ResizeUp => self.resize_pane(ctx, 0, -1).await,
            Action::ResizeDown => self.resize_pane(ctx, 0, 1).await,

            // ── Window navigation ────────────────────────
            Action::NextWindow => self.next_window(ctx).await,
            Action::PreviousWindow => self.previous_window(ctx).await,
            Action::WindowByIndex(n) => self.window_by_index(ctx, *n).await,

            // ── Pane operations ──────────────────────────
            Action::ToggleZoom => self.toggle_zoom(ctx).await,
            Action::SmartSplit => self.smart_split(ctx).await,

            Action::ForwardToPty => Ok(()), // handled by caller
        }
    }

    async fn focus_direction(
        &self,
        ctx: &ActionContext,
        direction: Direction,
    ) -> anyhow::Result<()> {
        if let Some(neighbor) = find_neighbor(
            &ctx.layout,
            ctx.window_rect,
            ctx.active_pane,
            direction,
        ) {
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.mutation_tx.send(Mutation::Pane(
                PaneCommand::Focus { pane_id: neighbor },
                tx,
            )).await?;
            let _ = rx.await;
        }
        // No neighbor in this direction: do nothing (no error, no beep)
        Ok(())
    }

    async fn resize_pane(
        &self,
        ctx: &ActionContext,
        width_delta: i16,
        height_delta: i16,
    ) -> anyhow::Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.mutation_tx.send(Mutation::Pane(
            PaneCommand::Resize {
                pane_id: ctx.active_pane,
                width_delta,
                height_delta,
            },
            tx,
        )).await?;
        let _ = rx.await;
        Ok(())
    }

    async fn next_window(&self, ctx: &ActionContext) -> anyhow::Result<()> {
        // Find the next window in the session's window list
        let current_index = ctx.window_index;
        let next_index = (current_index + 1) % ctx.window_count;
        let target_window = ctx.window_ids[next_index];

        let (tx, rx) = tokio::sync::oneshot::channel();
        self.mutation_tx.send(Mutation::Window(
            WindowCommand::Focus { window_id: target_window },
            tx,
        )).await?;
        let _ = rx.await;
        Ok(())
    }

    async fn previous_window(&self, ctx: &ActionContext) -> anyhow::Result<()> {
        let current_index = ctx.window_index;
        let prev_index = if current_index == 0 {
            ctx.window_count.saturating_sub(1)
        } else {
            current_index - 1
        };
        let target_window = ctx.window_ids[prev_index];

        let (tx, rx) = tokio::sync::oneshot::channel();
        self.mutation_tx.send(Mutation::Window(
            WindowCommand::Focus { window_id: target_window },
            tx,
        )).await?;
        let _ = rx.await;
        Ok(())
    }

    async fn window_by_index(
        &self,
        ctx: &ActionContext,
        index: usize,
    ) -> anyhow::Result<()> {
        // index is 1-based (Alt+1 = window 0)
        let zero_index = index.saturating_sub(1);
        if zero_index < ctx.window_count {
            let target_window = ctx.window_ids[zero_index];
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.mutation_tx.send(Mutation::Window(
                WindowCommand::Focus { window_id: target_window },
                tx,
            )).await?;
            let _ = rx.await;
        }
        // Out of range: silently ignore (no error, no beep)
        Ok(())
    }

    async fn toggle_zoom(&self, ctx: &ActionContext) -> anyhow::Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.mutation_tx.send(Mutation::Pane(
            PaneCommand::Zoom { pane_id: ctx.active_pane },
            tx,
        )).await?;
        let _ = rx.await;
        Ok(())
    }

    async fn smart_split(&self, ctx: &ActionContext) -> anyhow::Result<()> {
        let direction = smart_split_direction(
            ctx.active_pane_rect.width,
            ctx.active_pane_rect.height,
        );

        let (tx, rx) = tokio::sync::oneshot::channel();
        self.mutation_tx.send(Mutation::Pane(
            PaneCommand::Split {
                target_pane_id: ctx.active_pane,
                direction,
                ratio: 0.5,
                command: vec![], // default shell
                cwd: None,      // inherit from current pane
            },
            tx,
        )).await?;
        let _ = rx.await;
        Ok(())
    }
}

/// Context provided to the action executor for each key event.
/// Built from the current ArcSwap snapshot.
pub struct ActionContext {
    pub session_id: Uuid,
    pub active_window: Uuid,
    pub active_pane: Uuid,
    pub active_pane_rect: Rect,
    pub layout: LayoutNode,
    pub window_rect: Rect,
    pub window_index: usize,
    pub window_count: usize,
    pub window_ids: Vec<Uuid>,
}

impl ActionContext {
    /// Build an action context from the current state snapshot.
    pub fn from_snapshot(
        snapshot: &shux_core::graph::SessionGraph,
        session_id: Uuid,
        terminal_size: (u16, u16),
    ) -> Option<Self> {
        let session = snapshot.sessions.get(&session_id)?;
        let active_window_id = session.active_window;
        let window = snapshot.windows.get(&active_window_id)?;
        let window_index = session.windows.iter()
            .position(|id| *id == active_window_id)
            .unwrap_or(0);

        let window_rect = Rect {
            x: 0, y: 0,
            width: terminal_size.0,
            height: terminal_size.1.saturating_sub(1), // status bar
        };

        let pane_rects = window.layout.compute_rects_with_borders(window_rect);
        let active_pane_rect = pane_rects.iter()
            .find(|(id, _)| *id == window.active_pane)
            .map(|(_, r)| *r)
            .unwrap_or(window_rect);

        Some(Self {
            session_id,
            active_window: active_window_id,
            active_pane: window.active_pane,
            active_pane_rect,
            layout: window.layout.clone(),
            window_rect,
            window_index,
            window_count: session.windows.len(),
            window_ids: session.windows.clone(),
        })
    }
}
```

### Step 5: Integrate key router into the TUI input loop

Wire the key router into the main input processing loop of the TUI client. Every crossterm key event passes through the router before reaching the PTY.

```rust
// In crates/shux-ui/src/input.rs (or the main TUI loop)

use crossterm::event::{self, Event as CrosstermEvent, KeyEvent};
use crate::key_router::{KeyRouter, KeyRouteResult};

/// The main input processing loop for the TUI client.
pub async fn input_loop(
    key_router: &KeyRouter,
    action_executor: &ActionExecutor,
    pty_writer: &PtyWriter,
    state: &Arc<ArcSwap<SessionGraph>>,
    session_id: Uuid,
    terminal_size: (u16, u16),
) -> anyhow::Result<()> {
    loop {
        // Read the next crossterm event
        if event::poll(std::time::Duration::from_millis(16))? {
            match event::read()? {
                CrosstermEvent::Key(key_event) => {
                    match key_router.route(&key_event) {
                        KeyRouteResult::Handled(action) => {
                            // Build context from current state
                            let snapshot = state.load();
                            if let Some(ctx) = ActionContext::from_snapshot(
                                &snapshot, session_id, terminal_size,
                            ) {
                                if let Err(e) = action_executor.execute(&action, &ctx).await {
                                    tracing::warn!("Action execution failed: {}", e);
                                }
                            }
                        }
                        KeyRouteResult::Forward => {
                            // Forward to active pane's PTY
                            let bytes = key_event_to_bytes(&key_event);
                            pty_writer.write(&bytes).await?;
                        }
                    }
                }

                CrosstermEvent::Resize(width, height) => {
                    // Handle terminal resize (task 017)
                    // Recompute layouts, resize PTYs
                }

                _ => {
                    // Mouse events handled in task 020
                }
            }
        }
    }
}

/// Convert a crossterm KeyEvent to raw bytes for PTY forwarding.
/// This handles the mapping from crossterm's key representation
/// back to the byte sequences that terminals expect.
fn key_event_to_bytes(event: &KeyEvent) -> Vec<u8> {
    match event.code {
        KeyCode::Char(c) => {
            if event.modifiers.contains(KeyModifiers::CONTROL) {
                // Ctrl+a = 0x01, Ctrl+z = 0x1a
                if c.is_ascii_lowercase() {
                    vec![c as u8 - b'a' + 1]
                } else if c.is_ascii_uppercase() {
                    vec![c as u8 - b'A' + 1]
                } else {
                    c.to_string().into_bytes()
                }
            } else if event.modifiers.contains(KeyModifiers::ALT) {
                // Alt sends ESC prefix
                let mut bytes = vec![0x1b];
                bytes.extend(c.to_string().as_bytes());
                bytes
            } else {
                c.to_string().into_bytes()
            }
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::F(n) => {
            match n {
                1 => b"\x1bOP".to_vec(),
                2 => b"\x1bOQ".to_vec(),
                3 => b"\x1bOR".to_vec(),
                4 => b"\x1bOS".to_vec(),
                5 => b"\x1b[15~".to_vec(),
                6 => b"\x1b[17~".to_vec(),
                7 => b"\x1b[18~".to_vec(),
                8 => b"\x1b[19~".to_vec(),
                9 => b"\x1b[20~".to_vec(),
                10 => b"\x1b[21~".to_vec(),
                11 => b"\x1b[23~".to_vec(),
                12 => b"\x1b[24~".to_vec(),
                _ => vec![],
            }
        }
        _ => vec![],
    }
}
```

### Step 6: Handle Alt key detection edge cases

In legacy terminal mode (no Kitty keyboard protocol), Alt+key arrives as ESC followed by the key character. This creates an ambiguity: is `ESC h` a standalone Escape followed by `h`, or is it `Alt+h`? crossterm handles this with a timeout: if no bytes follow ESC within a short window (~50ms), it is treated as standalone Escape.

```rust
/// Notes on Alt key handling:
///
/// With Kitty keyboard protocol (when ClientCaps.kitty_keyboard is true):
///   - Alt+h arrives as a single event with KeyModifiers::ALT
///   - No ambiguity, no timeout needed
///
/// With legacy terminal mode:
///   - Alt+h arrives as ESC (0x1b) followed by 'h' (0x68)
///   - crossterm's event reader handles the disambiguation:
///     - If ESC is followed by another byte within ~50ms, it's Alt+key
///     - If ESC stands alone (no following byte), it's standalone Escape
///   - This is crossterm's default behavior; no special handling needed here
///
/// Shift detection:
///   - Alt+Shift+h (resize) arrives as Alt+H (uppercase)
///   - crossterm sets both ALT and SHIFT modifiers
///   - Our KeyPattern uses both modifiers for the resize bindings
///   - In legacy mode, terminals may send ESC H for Alt+Shift+h
///     or may not distinguish; this is terminal-dependent
```

### Step 7: Write unit tests for keybinding matching

In `crates/shux-ui/tests/keybinding_tests.rs`:

```rust
use crossterm::event::{KeyCode, KeyModifiers, KeyEvent, KeyEventKind, KeyEventState};
use shux_ui::keybindings::{KeybindingRegistry, Action, KeyPattern};

fn make_key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    }
}

#[test]
fn test_default_focus_bindings() {
    let registry = KeybindingRegistry::with_defaults();

    assert_eq!(
        registry.lookup(&make_key(KeyCode::Char('h'), KeyModifiers::ALT)),
        Some(&Action::FocusLeft)
    );
    assert_eq!(
        registry.lookup(&make_key(KeyCode::Char('j'), KeyModifiers::ALT)),
        Some(&Action::FocusDown)
    );
    assert_eq!(
        registry.lookup(&make_key(KeyCode::Char('k'), KeyModifiers::ALT)),
        Some(&Action::FocusUp)
    );
    assert_eq!(
        registry.lookup(&make_key(KeyCode::Char('l'), KeyModifiers::ALT)),
        Some(&Action::FocusRight)
    );
}

#[test]
fn test_default_resize_bindings() {
    let registry = KeybindingRegistry::with_defaults();

    assert_eq!(
        registry.lookup(&make_key(
            KeyCode::Char('H'),
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        )),
        Some(&Action::ResizeLeft)
    );
    assert_eq!(
        registry.lookup(&make_key(
            KeyCode::Char('L'),
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        )),
        Some(&Action::ResizeRight)
    );
}

#[test]
fn test_window_navigation_bindings() {
    let registry = KeybindingRegistry::with_defaults();

    assert_eq!(
        registry.lookup(&make_key(KeyCode::Char('n'), KeyModifiers::ALT)),
        Some(&Action::NextWindow)
    );
    assert_eq!(
        registry.lookup(&make_key(KeyCode::Char('p'), KeyModifiers::ALT)),
        Some(&Action::PreviousWindow)
    );
}

#[test]
fn test_window_index_bindings() {
    let registry = KeybindingRegistry::with_defaults();

    for i in 1..=9 {
        assert_eq!(
            registry.lookup(&make_key(
                KeyCode::Char((b'0' + i as u8) as char),
                KeyModifiers::ALT,
            )),
            Some(&Action::WindowByIndex(i))
        );
    }
}

#[test]
fn test_zoom_binding() {
    let registry = KeybindingRegistry::with_defaults();

    assert_eq!(
        registry.lookup(&make_key(KeyCode::Char('z'), KeyModifiers::ALT)),
        Some(&Action::ToggleZoom)
    );
}

#[test]
fn test_smart_split_binding() {
    let registry = KeybindingRegistry::with_defaults();

    assert_eq!(
        registry.lookup(&make_key(KeyCode::Enter, KeyModifiers::ALT)),
        Some(&Action::SmartSplit)
    );
}

#[test]
fn test_unbound_key_returns_none() {
    let registry = KeybindingRegistry::with_defaults();

    // Regular 'a' without any modifier
    assert_eq!(
        registry.lookup(&make_key(KeyCode::Char('a'), KeyModifiers::empty())),
        None
    );

    // Ctrl+C (should forward to PTY, not handled by shux)
    assert_eq!(
        registry.lookup(&make_key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        None
    );
}

#[test]
fn test_custom_override() {
    let mut registry = KeybindingRegistry::with_defaults();

    // Override Alt+h to be resize instead of focus
    registry.set(
        KeyPattern::new(KeyCode::Char('h'), KeyModifiers::ALT),
        Action::ResizeLeft,
    );

    assert_eq!(
        registry.lookup(&make_key(KeyCode::Char('h'), KeyModifiers::ALT)),
        Some(&Action::ResizeLeft)
    );
}

#[test]
fn test_remove_binding() {
    let mut registry = KeybindingRegistry::with_defaults();

    registry.remove(&KeyPattern::new(KeyCode::Char('z'), KeyModifiers::ALT));

    assert_eq!(
        registry.lookup(&make_key(KeyCode::Char('z'), KeyModifiers::ALT)),
        None
    );
}

#[test]
fn test_list_bindings_sorted() {
    let registry = KeybindingRegistry::with_defaults();
    let bindings = registry.list();

    // Should have all Tier 1 bindings
    // 4 (focus) + 4 (resize) + 2 (next/prev window) + 9 (window by index) + 1 (zoom) + 1 (smart split) = 21
    assert_eq!(bindings.len(), 21);
}

#[test]
fn test_key_pattern_from_str() {
    let p = KeyPattern::from_str("alt-h").unwrap();
    assert_eq!(p.code, KeyCode::Char('h'));
    assert_eq!(p.modifiers, KeyModifiers::ALT);

    let p = KeyPattern::from_str("alt-shift-H").unwrap();
    assert_eq!(p.code, KeyCode::Char('H'));
    assert!(p.modifiers.contains(KeyModifiers::ALT));
    assert!(p.modifiers.contains(KeyModifiers::SHIFT));

    let p = KeyPattern::from_str("alt-enter").unwrap();
    assert_eq!(p.code, KeyCode::Enter);
    assert_eq!(p.modifiers, KeyModifiers::ALT);
}

#[test]
fn test_key_pattern_display() {
    let p = KeyPattern::new(KeyCode::Char('h'), KeyModifiers::ALT);
    assert_eq!(p.display(), "Alt+h");

    let p = KeyPattern::new(
        KeyCode::Char('H'),
        KeyModifiers::ALT | KeyModifiers::SHIFT,
    );
    assert_eq!(p.display(), "Alt+Shift+H");
}

#[test]
fn test_key_event_to_bytes_basic() {
    use shux_ui::input::key_event_to_bytes;

    let event = make_key(KeyCode::Char('a'), KeyModifiers::empty());
    assert_eq!(key_event_to_bytes(&event), b"a");

    let event = make_key(KeyCode::Enter, KeyModifiers::empty());
    assert_eq!(key_event_to_bytes(&event), b"\r");

    let event = make_key(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(key_event_to_bytes(&event), vec![0x03]);
}

#[test]
fn test_apply_overrides() {
    let mut registry = KeybindingRegistry::with_defaults();
    let mut overrides = std::collections::HashMap::new();
    overrides.insert("alt-h".to_string(), "resize-left".to_string());

    registry.apply_overrides(&overrides);

    assert_eq!(
        registry.lookup(&make_key(KeyCode::Char('h'), KeyModifiers::ALT)),
        Some(&Action::ResizeLeft),
    );
}
```

---

## Verification

### Functional

```bash
# Start shux with multiple panes
shux new -s test
# In the TUI: Alt+Enter to create splits

# Test pane navigation
# Alt+h → focus moves left
# Alt+l → focus moves right
# Alt+j → focus moves down
# Alt+k → focus moves up
# Verify: border color changes to show focused pane

# Test pane resize
# Alt+Shift+L → active pane grows right
# Alt+Shift+H → active pane shrinks left
# Alt+Shift+J → active pane grows down
# Alt+Shift+K → active pane shrinks up

# Test window navigation
# Create a second window (Prefix+c, or via API)
# Alt+n → switches to next window
# Alt+p → switches to previous window
# Alt+1 → switches to window at index 1
# Alt+2 → switches to window at index 2

# Test zoom
# Alt+z → current pane fills screen
# Alt+z → previous layout restored

# Test smart split
# In a wide pane: Alt+Enter → creates vertical split
# In a tall pane: Alt+Enter → creates horizontal split

# Test that unbound keys forward to PTY
# Type regular characters → they appear in the terminal
# Ctrl+C → sends interrupt to the running process
```

### Tests

```bash
# Keybinding matching tests
cargo nextest run -p shux-ui --test keybinding_tests

# Unit tests for key_event_to_bytes
cargo nextest run -p shux-ui --lib -- input

# All tests
cargo nextest run --workspace

# Clippy
cargo clippy --workspace --all-targets -- -D warnings
```

---

## Completion Criteria

- [ ] Alt+h/j/k/l navigates focus between panes (left/down/up/right directional)
- [ ] Alt+H/J/K/L (Shift+Alt) resizes the active pane in the corresponding direction
- [ ] Alt+n switches to the next window (wraps around)
- [ ] Alt+p switches to the previous window (wraps around)
- [ ] Alt+1..9 switches to the window at that index (1-indexed, 0-based internally)
- [ ] Alt+z toggles zoom on the current pane
- [ ] Alt+Enter creates a new pane with smart split direction
- [ ] Unbound keys (regular typing, Ctrl+C, arrows, etc.) forward to the active pane's PTY
- [ ] Key routing has zero perceptible latency (< 1ms lookup time)
- [ ] Directional focus finds the nearest pane using layout geometry
- [ ] Smart split: wider pane splits vertically, taller pane splits horizontally
- [ ] KeybindingRegistry supports custom overrides from TOML config
- [ ] KeyPattern supports parsing from string format ("alt-h", "alt-shift-H")
- [ ] key_event_to_bytes correctly converts all common key events to PTY byte sequences
- [ ] Out-of-range window indices and missing neighbors are silently ignored (no error/beep)
- [ ] Unit tests pass for all keybinding matches, overrides, and key-to-bytes conversion
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes
- [ ] `cargo nextest run --workspace` passes

---

## Commit Message

```
feat: implement Tier 1 bare keybindings with key routing

- Add keybinding registry with all PRD Tier 1 bare key defaults:
  Alt+hjkl (focus), Alt+HJKL (resize), Alt+np (window nav),
  Alt+1..9 (window index), Alt+z (zoom), Alt+Enter (smart split)
- Key router intercepts bound keys and forwards unbound to PTY
- Action executor translates keybinding actions to state mutations
- Directional pane focus via layout geometry (nearest neighbor)
- Smart split direction based on pane aspect ratio
- KeyPattern parsing from string format for config-driven overrides
- key_event_to_bytes for correct PTY forwarding of all key types
- Comprehensive unit tests for keybinding matching and key routing
```

---

## Session Protocol

1. **Before starting:** Verify task 017 is complete. Multi-pane rendering must work. The compositor must correctly display multiple panes with borders. Read `CLAUDE.md`.
2. **During:** Start with the Action enum and KeybindingRegistry (pure data, easy to test). Write keybinding matching tests immediately. Then implement the key router and action executor. Wire into the TUI input loop last.
3. **Key patterns:**
   - The keybinding lookup must be a HashMap lookup (O(1)). Do not use linear search over a list of bindings.
   - Alt key handling in legacy terminals uses ESC prefix. crossterm handles this transparently, but be aware of the ~50ms timeout for Escape disambiguation.
   - The action executor reads state via ArcSwap snapshots (lock-free) and sends mutations via the mpsc channel. It never holds locks.
   - `key_event_to_bytes` is critical for correct PTY forwarding. Test it thoroughly. Wrong bytes = garbled terminal output.
4. **After:** Run full verification. Manually test all keybindings in a real terminal. Verify that regular typing and Ctrl sequences still work. Update `docs/PROGRESS.md`. This task unlocks task 019 (Tier 2 prefix keys).
