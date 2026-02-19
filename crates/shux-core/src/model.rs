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
    pub cwd: Option<PathBuf>,
    pub theme: Option<ThemeRef>,
    pub tags: Tags,
    pub version: Version,
}

impl Window {
    pub fn new(session_id: SessionId, title: impl Into<String>, initial_pane_id: PaneId) -> Self {
        Self {
            id: WindowId::new(),
            session_id,
            title: title.into(),
            active_pane: initial_pane_id,
            cwd: None,
            theme: None,
            tags: HashMap::new(),
            version: 1,
        }
    }
}

/// A pane is a terminal viewport running a child process (PRD 5.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pane {
    pub id: PaneId,
    pub window_id: WindowId,
    pub title: String,
    pub auto_title: bool,
    pub cwd: PathBuf,
    pub command: Vec<String>,
    pub exit_status: Option<i32>,
    pub restart: RestartPolicy,
    pub theme: Option<ThemeRef>,
    pub tags: Tags,
    pub version: Version,
}

impl Pane {
    pub fn new(window_id: WindowId, cwd: impl Into<PathBuf>) -> Self {
        Self {
            id: PaneId::new(),
            window_id,
            title: String::new(),
            auto_title: true,
            cwd: cwd.into(),
            command: Vec::new(),
            exit_status: None,
            restart: RestartPolicy::default(),
            theme: None,
            tags: HashMap::new(),
            version: 1,
        }
    }

    pub fn with_command(
        window_id: WindowId,
        cwd: impl Into<PathBuf>,
        command: Vec<String>,
    ) -> Self {
        let mut pane = Self::new(window_id, cwd);
        pane.command = command;
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
}
