//! Entity types for the shux data model (PRD 5.1).
//!
//! Defines Session, Window, Pane and their ID types.
//! All entities carry version stamps for optimistic concurrency.

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A newtype wrapper around UUID for type-safe entity identification.
macro_rules! define_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, Serialize, Deserialize)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            pub fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            pub fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl From<Uuid> for $name {
            fn from(uuid: Uuid) -> Self {
                Self(uuid)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(Self(Uuid::parse_str(s)?))
            }
        }
    };
}

define_id!(SessionId);
define_id!(WindowId);
define_id!(PaneId);
define_id!(PluginId);

/// Restart policy for a pane's child process (PRD 5.1, 6.2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    #[default]
    Never,
    OnFail,
    Always,
}

/// A reference to a named theme (PRD 5.3 cascade).
pub type ThemeRef = String;

/// Tags are arbitrary key-value metadata visible to plugins (PRD 5.1).
pub type Tags = HashMap<String, String>;

/// Monotonically increasing version stamp for optimistic concurrency (PRD 5.4).
pub type Version = u64;

/// A session groups windows and represents a named workspace (PRD 5.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub name: String,
    pub created_at: SystemTime,
    /// Ordered list of window IDs. Order determines window index (1-based in UI).
    pub windows: Vec<WindowId>,
    pub active_window: WindowId,
    pub env: HashMap<String, String>,
    pub theme: Option<ThemeRef>,
    pub tags: Tags,
    pub version: Version,
    /// Plugin that created this entity, if any. `None` for entities
    /// created by user CLI / RPC calls. Used by the permission model
    /// to grant a plugin authority over its own entities without an
    /// explicit grant (see `docs/designs/permissions/README.md` §5.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by_plugin: Option<PluginId>,
}

impl Session {
    pub fn new(name: impl Into<String>, initial_window_id: WindowId) -> Self {
        Self {
            id: SessionId::new(),
            name: name.into(),
            created_at: SystemTime::now(),
            windows: vec![initial_window_id],
            active_window: initial_window_id,
            env: HashMap::new(),
            theme: None,
            tags: HashMap::new(),
            version: 1,
            created_by_plugin: None,
        }
    }
}

/// A window contains a layout tree of panes (PRD 5.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Window {
    pub id: WindowId,
    pub session_id: SessionId,
    pub title: String,
    pub active_pane: PaneId,
    pub layout: crate::layout::WindowLayout,
    pub cwd: Option<PathBuf>,
    pub theme: Option<ThemeRef>,
    pub tags: Tags,
    pub version: Version,
    /// Plugin that created this entity. See [`Session::created_by_plugin`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by_plugin: Option<PluginId>,
}

impl Window {
    pub fn new(session_id: SessionId, title: impl Into<String>, initial_pane_id: PaneId) -> Self {
        Self {
            id: WindowId::new(),
            session_id,
            title: title.into(),
            active_pane: initial_pane_id,
            layout: crate::layout::WindowLayout::new(initial_pane_id),
            cwd: None,
            theme: None,
            tags: HashMap::new(),
            version: 1,
            created_by_plugin: None,
        }
    }
}

/// A pane is a terminal viewport running a child process (PRD 5.1).
///
/// Title resolution priority (highest first), exposed via
/// [`Pane::effective_title`]:
///
/// 1. `manual_title` — explicitly set via `pane.set_title` RPC / `shux
///    pane title` CLI. Never overwritten by automatic sources.
/// 2. `osc_title` — set by the running app via OSC 0/2 escape
///    sequences (bash's `PROMPT_COMMAND` writes one of these per cwd
///    change; vim sets one per buffer).
/// 3. Auto-derived from `command` (first token, basename) or `cwd`.
/// 4. Empty string — never panic, fall back gracefully.
///
/// `auto_title = false` pins whatever was last computed and stops
/// future automatic updates (OSC + command). A subsequent
/// `set_manual_title(None)` re-enables auto.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pane {
    pub id: PaneId,
    pub window_id: WindowId,
    /// The currently-displayed title. Computed by `recalculate_title()`
    /// from the four sources above. Read directly by renderers (the
    /// compositor border draw doesn't need to know about priority).
    pub title: String,
    /// Set explicitly via API / CLI. Highest priority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_title: Option<String>,
    /// Set by the running app via OSC 0/2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub osc_title: Option<String>,
    /// When true, automatic sources (OSC + command/cwd derivation)
    /// flow into `title`. When false, the current `title` is pinned.
    pub auto_title: bool,
    pub cwd: PathBuf,
    pub command: Vec<String>,
    pub exit_status: Option<i32>,
    pub restart: RestartPolicy,
    pub theme: Option<ThemeRef>,
    pub tags: Tags,
    pub version: Version,
    /// Plugin that created this entity. See [`Session::created_by_plugin`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by_plugin: Option<PluginId>,
}

impl Pane {
    pub fn new(window_id: WindowId, cwd: impl Into<PathBuf>) -> Self {
        let mut pane = Self {
            id: PaneId::new(),
            window_id,
            title: String::new(),
            manual_title: None,
            osc_title: None,
            auto_title: true,
            cwd: cwd.into(),
            command: Vec::new(),
            exit_status: None,
            restart: RestartPolicy::default(),
            theme: None,
            tags: HashMap::new(),
            version: 1,
            created_by_plugin: None,
        };
        pane.recalculate_title();
        pane
    }

    pub fn with_command(
        window_id: WindowId,
        cwd: impl Into<PathBuf>,
        command: Vec<String>,
    ) -> Self {
        let mut pane = Self::new(window_id, cwd);
        pane.command = command;
        pane.recalculate_title();
        pane
    }

    pub fn is_alive(&self) -> bool {
        self.exit_status.is_none()
    }

    pub fn should_restart(&self) -> bool {
        match (self.restart, self.exit_status) {
            (RestartPolicy::Always, Some(_)) => true,
            (RestartPolicy::OnFail, Some(code)) => code != 0,
            _ => false,
        }
    }

    /// Resolved title following the priority listed in the struct docs.
    /// Cheaper read than `&self.title` if you want to bypass any stale
    /// `title` cache, but `title` is kept in sync by `recalculate_title()`
    /// so direct reads are also fine.
    pub fn effective_title(&self) -> &str {
        if let Some(m) = self.manual_title.as_deref() {
            return m;
        }
        if self.auto_title
            && let Some(o) = self.osc_title.as_deref()
        {
            return o;
        }
        &self.title
    }

    /// Set or clear the manual title. Setting `None` lets automatic
    /// sources (OSC + command/cwd) flow back into `title`. Setting
    /// `Some` overrides them.
    pub fn set_manual_title(&mut self, title: Option<String>) {
        self.manual_title = title.map(|t| sanitize_title(&t));
        self.recalculate_title();
    }

    /// Record an OSC 0/2 title update from the running app. Returns
    /// `true` iff the new title actually changed the displayed
    /// `title` field (i.e. no manual override, auto enabled, value
    /// differs) — callers use this to decide whether to fire a
    /// `PaneTitleChanged` event without re-computing the priority.
    pub fn set_osc_title(&mut self, title: String) -> bool {
        let sanitized = sanitize_title(&title);
        let new_osc = if sanitized.is_empty() {
            None
        } else {
            Some(sanitized)
        };
        if self.osc_title == new_osc {
            return false;
        }
        self.osc_title = new_osc;
        let old = self.title.clone();
        self.recalculate_title();
        old != self.title
    }

    /// Toggle the auto-title flag. When turning OFF, the current
    /// title is pinned (re-derivation stops). When turning ON, the
    /// priority resolution kicks back in. Callers should fire a
    /// `PaneTitleChanged` event if the displayed title changes.
    pub fn set_auto_title(&mut self, enabled: bool) {
        if self.auto_title == enabled {
            return;
        }
        self.auto_title = enabled;
        self.recalculate_title();
    }

    /// Recompute `self.title` from the priority sources. Called
    /// internally on every mutation that could affect display.
    pub(crate) fn recalculate_title(&mut self) {
        if let Some(m) = &self.manual_title {
            self.title = m.clone();
            return;
        }
        if self.auto_title {
            if let Some(o) = &self.osc_title {
                self.title = o.clone();
                return;
            }
            // Auto from command (first arg basename) or cwd basename.
            if let Some(cmd) = self.command.first() {
                self.title = std::path::Path::new(cmd)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(cmd)
                    .to_string();
                return;
            }
            if let Some(name) = self.cwd.file_name().and_then(|s| s.to_str()) {
                self.title = name.to_string();
            }
        }
        // Auto disabled and no manual override → keep whatever we had.
        // If title is empty (fresh pane with no command and no cwd
        // basename), leave it empty rather than dropping garbage in.
    }
}

/// Clamp a title to a sane single-line ASCII-ish display. Newlines,
/// nulls and other control bytes inside an OSC payload are an attack
/// surface (some terminals render them and re-trigger parsing); the
/// border-draw code also assumes a single line. Hard cap at 64 chars
/// — the border has limited room and very long titles squeeze out
/// the rest of the chrome.
pub(crate) fn sanitize_title(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_string();
    if cleaned.chars().count() <= 64 {
        cleaned
    } else {
        cleaned.chars().take(64).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_id_uniqueness() {
        let a = SessionId::new();
        let b = SessionId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn test_id_copy_and_eq() {
        let a = SessionId::new();
        let b = a;
        assert_eq!(a, b);
    }

    #[test]
    fn test_id_display() {
        let id = SessionId::new();
        let s = id.to_string();
        assert!(!s.is_empty());
        // UUID v4 format
        assert_eq!(s.len(), 36);
    }

    #[test]
    fn test_id_from_uuid() {
        let uuid = Uuid::new_v4();
        let id = PaneId::from_uuid(uuid);
        assert_eq!(*id.as_uuid(), uuid);
    }

    #[test]
    fn test_id_serialize_roundtrip() {
        let id = WindowId::new();
        let json = serde_json::to_string(&id).unwrap();
        let deserialized: WindowId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, deserialized);
    }

    #[test]
    fn test_session_new() {
        let wid = WindowId::new();
        let session = Session::new("work", wid);
        assert_eq!(session.name, "work");
        assert_eq!(session.windows, vec![wid]);
        assert_eq!(session.active_window, wid);
        assert_eq!(session.version, 1);
    }

    #[test]
    fn test_window_new() {
        let sid = SessionId::new();
        let pid = PaneId::new();
        let window = Window::new(sid, "editor", pid);
        assert_eq!(window.session_id, sid);
        assert_eq!(window.title, "editor");
        assert_eq!(window.active_pane, pid);
    }

    #[test]
    fn test_pane_new() {
        let wid = WindowId::new();
        let pane = Pane::new(wid, "/home/test");
        assert_eq!(pane.window_id, wid);
        assert!(pane.is_alive());
        assert!(!pane.should_restart());
    }

    #[test]
    fn test_pane_with_command() {
        let wid = WindowId::new();
        let pane = Pane::with_command(wid, "/home/test", vec!["vim".into()]);
        assert_eq!(pane.command, vec!["vim"]);
    }

    #[test]
    fn test_pane_restart_policy() {
        let wid = WindowId::new();
        let mut pane = Pane::new(wid, "/home/test");

        // Never restart (default)
        pane.exit_status = Some(1);
        assert!(!pane.should_restart());

        // OnFail with failure
        pane.restart = RestartPolicy::OnFail;
        pane.exit_status = Some(1);
        assert!(pane.should_restart());

        // OnFail with success
        pane.exit_status = Some(0);
        assert!(!pane.should_restart());

        // Always
        pane.restart = RestartPolicy::Always;
        pane.exit_status = Some(0);
        assert!(pane.should_restart());

        // Still running
        pane.exit_status = None;
        assert!(!pane.should_restart());
    }

    #[test]
    fn test_restart_policy_serde() {
        let json = serde_json::to_string(&RestartPolicy::OnFail).unwrap();
        assert_eq!(json, "\"on-fail\"");
        let deserialized: RestartPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, RestartPolicy::OnFail);
    }

    // ── PR 4 / task 027: pane title resolution ────────────────────────

    #[test]
    fn test_pane_auto_title_derives_from_command() {
        let wid = WindowId::new();
        let pane = Pane::with_command(wid, "/home/test", vec!["vim".into(), "foo.rs".into()]);
        // First-arg basename → "vim".
        assert_eq!(pane.effective_title(), "vim");
        assert_eq!(pane.title, "vim");
    }

    #[test]
    fn test_pane_auto_title_takes_basename_of_command() {
        let wid = WindowId::new();
        let pane = Pane::with_command(wid, "/home/test", vec!["/usr/bin/htop".into()]);
        assert_eq!(pane.effective_title(), "htop");
    }

    #[test]
    fn test_pane_auto_title_falls_back_to_cwd_basename() {
        let wid = WindowId::new();
        let pane = Pane::new(wid, "/home/test/projects/myproj");
        assert_eq!(pane.effective_title(), "myproj");
    }

    #[test]
    fn test_pane_manual_title_overrides_command_derived() {
        let wid = WindowId::new();
        let mut pane = Pane::with_command(wid, "/home/test", vec!["vim".into()]);
        pane.set_manual_title(Some("notes".into()));
        // Manual wins over the command-derived auto.
        assert_eq!(pane.effective_title(), "notes");
        assert_eq!(pane.title, "notes");
    }

    #[test]
    fn test_pane_osc_title_overrides_command_derived_when_auto() {
        let wid = WindowId::new();
        let mut pane = Pane::with_command(wid, "/home/test", vec!["bash".into()]);
        let changed = pane.set_osc_title("~/work/x".into());
        assert!(changed);
        assert_eq!(pane.effective_title(), "~/work/x");
    }

    #[test]
    fn test_pane_manual_title_beats_osc_title() {
        let wid = WindowId::new();
        let mut pane = Pane::new(wid, "/home/test");
        pane.set_osc_title("from-osc".into());
        pane.set_manual_title(Some("manual".into()));
        // Both set → manual priority.
        assert_eq!(pane.effective_title(), "manual");
        // Clearing manual lets OSC flow back.
        pane.set_manual_title(None);
        assert_eq!(pane.effective_title(), "from-osc");
    }

    #[test]
    fn test_pane_auto_title_off_pins_current_title() {
        let wid = WindowId::new();
        let mut pane = Pane::with_command(wid, "/home/test", vec!["bash".into()]);
        assert_eq!(pane.title, "bash");
        pane.set_auto_title(false);
        // Subsequent OSC updates must NOT change the displayed title.
        let changed = pane.set_osc_title("changed".into());
        // osc_title field still records the value (so re-enabling auto
        // picks it up), but title stays pinned.
        assert!(!changed);
        assert_eq!(pane.title, "bash");
        // Re-enabling auto pulls the recorded osc_title into title.
        pane.set_auto_title(true);
        assert_eq!(pane.title, "changed");
    }

    #[test]
    fn test_pane_osc_title_idempotent() {
        let wid = WindowId::new();
        let mut pane = Pane::with_command(wid, "/home/test", vec!["bash".into()]);
        let first = pane.set_osc_title("same".into());
        let second = pane.set_osc_title("same".into());
        assert!(first, "first call must report a change");
        assert!(!second, "second call with same value must report no change");
    }

    #[test]
    fn test_pane_sanitize_title_strips_control_chars() {
        // BEL (0x07), ESC (0x1b), LF (0x0a) are control bytes and
        // must be dropped. The closing `]` is a printable ASCII char
        // and survives — sanitize_title only drops `c.is_control()`,
        // it doesn't try to strip OSC syntax. (Border-draw code
        // displays the result one char per cell, so as long as no
        // control byte slips through, we're safe.)
        let wid = WindowId::new();
        let mut pane = Pane::new(wid, "/home/test");
        pane.set_manual_title(Some("hello\x07\x1b]world\n".into()));
        assert_eq!(pane.title, "hello]world");
    }

    #[test]
    fn test_pane_sanitize_title_clamps_to_64_chars() {
        let wid = WindowId::new();
        let mut pane = Pane::new(wid, "/home/test");
        let long: String = "x".repeat(120);
        pane.set_manual_title(Some(long));
        assert_eq!(pane.title.chars().count(), 64);
    }

    #[test]
    fn test_pane_title_serde_round_trips() {
        let wid = WindowId::new();
        let mut pane = Pane::with_command(wid, "/home/test", vec!["bash".into()]);
        pane.set_manual_title(Some("agent-1".into()));
        pane.set_osc_title("from-shell".into());
        let json = serde_json::to_string(&pane).unwrap();
        let back: Pane = serde_json::from_str(&json).unwrap();
        // After round-trip, recalculate_title runs implicitly in Deserialize?
        // No — Pane uses derive(Deserialize), so the fields come back as
        // stored. effective_title() still resolves correctly from the
        // stored fields.
        assert_eq!(back.manual_title.as_deref(), Some("agent-1"));
        assert_eq!(back.osc_title.as_deref(), Some("from-shell"));
        assert_eq!(back.effective_title(), "agent-1");
    }
}
