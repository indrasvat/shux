use serde::{Deserialize, Serialize};

use crate::model::{Kind, RecordId, State};

/// Records per state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateCounts {
    pub idle: u16,
    pub working: u16,
    pub done: u16,
    pub blocked: u16,
    pub error: u16,
}

impl StateCounts {
    pub(crate) fn add(&mut self, state: State) {
        let slot = match state {
            State::Idle => &mut self.idle,
            State::Working => &mut self.working,
            State::Done => &mut self.done,
            State::Blocked => &mut self.blocked,
            State::Error => &mut self.error,
        };
        *slot = slot.saturating_add(1);
    }
}

/// One terminal's status at a glance: its most urgent record plus counts.
///
/// Carries no program-authored text (`title`, `msg`), so it is safe to put on
/// the event bus and in public listings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusSummary {
    pub state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// The id of the record the summary leads with; `None` is the root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<RecordId>,
    pub counts: StateCounts,
}
