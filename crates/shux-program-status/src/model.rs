use serde::{Deserialize, Serialize};

use crate::limits::{MAX_ID_BYTES, MAX_ID_DEPTH, MAX_ID_SEGMENT_BYTES};

/// A record's state. `clear` is not a state; it is [`Report::Clear`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Idle,
    Working,
    Done,
    Blocked,
    Error,
}

impl State {
    pub const ALL: [State; 5] = [
        State::Idle,
        State::Working,
        State::Done,
        State::Blocked,
        State::Error,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::Working => "working",
            State::Done => "done",
            State::Blocked => "blocked",
            State::Error => "error",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|st| st.as_str() == s)
    }

    /// Whether the record survives process exit and a new shell prompt.
    /// The spec requires dropping `working` and `blocked`; shux also drops
    /// `idle`, which it permits, so a dead program never reads as waiting.
    pub fn survives_exit(self) -> bool {
        matches!(self, State::Done | State::Error)
    }
}

/// What a `blocked` program needs from the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Permission,
    Question,
    Auth,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Permission, Kind::Question, Kind::Auth];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Permission => "permission",
            Kind::Question => "question",
            Kind::Auth => "auth",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// A validated `/`-separated record path: `segment := [A-Za-z0-9_.+-]{1,32}`,
/// at most 8 levels and 128 bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RecordId(Box<str>);

impl RecordId {
    pub fn parse(s: &str) -> Option<Self> {
        if s.is_empty() || s.len() > MAX_ID_BYTES {
            return None;
        }
        let mut depth = 0;
        for segment in s.split('/') {
            depth += 1;
            let valid = !segment.is_empty()
                && segment.len() <= MAX_ID_SEGMENT_BYTES
                && segment.bytes().all(is_id_byte);
            if !valid || depth > MAX_ID_DEPTH {
                return None;
            }
        }
        Some(Self(s.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `self` is `other` or lies beneath it.
    pub fn is_within(&self, other: &RecordId) -> bool {
        self.0
            .strip_prefix(&*other.0)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    }

    /// The parent path; `None` for a top-level id, whose parent is the root.
    pub fn parent(&self) -> Option<RecordId> {
        self.0.rsplit_once('/').map(|(p, _)| Self(p.into()))
    }
}

impl TryFrom<String> for RecordId {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        Self::parse(&s).ok_or_else(|| format!("invalid program-status record id: {s:?}"))
    }
}

impl From<RecordId> for String {
    fn from(id: RecordId) -> String {
        id.0.into()
    }
}

pub(crate) fn is_id_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'+' | b'-')
}

/// Everything a report sets on one record. `kind` is only ever present on a
/// `blocked` record and `progress` only on `working` or `blocked`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub state: State,
    pub kind: Option<Kind>,
    pub progress: Option<u8>,
    pub app: Option<String>,
    pub title: Option<String>,
    pub msg: Option<String>,
}

/// A decoded report. `id: None` addresses the root record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    Set {
        id: Option<RecordId>,
        record: Record,
    },
    /// Removes the addressed record and its subtree; with no id, everything.
    Clear { id: Option<RecordId> },
}

/// One stored record as readers see it, with `app` already inherited.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordView {
    pub id: Option<RecordId>,
    pub state: State,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msg: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_grammar_accepts_paths_within_limits() {
        assert!(RecordId::parse("deploy/us-east.1/step_2+x").is_some());
        assert!(RecordId::parse(&["a"; 8].join("/")).is_some());
        assert!(RecordId::parse(&"x".repeat(32)).is_some());
    }

    #[test]
    fn id_grammar_rejects_empty_segments_bad_bytes_and_limits() {
        for bad in ["", "/a", "a/", "a//b", "a@b", "a b", "a:b", "a;b", "é"] {
            assert!(RecordId::parse(bad).is_none(), "{bad:?}");
        }
        assert!(RecordId::parse(&["a"; 9].join("/")).is_none(), "depth 9");
        assert!(RecordId::parse(&"x".repeat(33)).is_none(), "segment 33");
        let long = vec!["x".repeat(32); 4].join("/") + "/x";
        assert_eq!(long.len(), 133);
        assert!(RecordId::parse(&long).is_none(), "over 128 bytes");
    }

    #[test]
    fn is_within_matches_whole_segments_only() {
        let id = |s| RecordId::parse(s).unwrap();
        assert!(id("a/b").is_within(&id("a")));
        assert!(id("a").is_within(&id("a")));
        assert!(!id("ab").is_within(&id("a")));
        assert!(!id("a").is_within(&id("a/b")));
    }

    #[test]
    fn state_and_kind_parse_lowercase_only() {
        assert_eq!(State::parse("blocked"), Some(State::Blocked));
        assert_eq!(State::parse("IDLE"), None);
        assert_eq!(State::parse("clear"), None);
        assert_eq!(Kind::parse("auth"), Some(Kind::Auth));
        assert_eq!(Kind::parse("Auth"), None);
    }

    #[test]
    fn record_id_serde_rejects_invalid_paths() {
        assert!(serde_json::from_str::<RecordId>("\"a/b\"").is_ok());
        assert!(serde_json::from_str::<RecordId>("\"a//b\"").is_err());
    }
}
