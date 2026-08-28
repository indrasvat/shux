//! Kitty graphics protocol support.

pub(crate) mod apc;
pub(crate) mod kitty;

use kitty::Action;

/// What a pane has asked of the graphics protocol, and what shux did about it.
///
/// Counters rather than a store, because this change decodes no pixels. They
/// exist so the decision taken per command is observable at all: a refusal that
/// is neither answered nor recorded is indistinguishable from a command that
/// was never parsed, which is exactly the gap that let a scanner doing nothing
/// pass a whole neutrality suite.
#[derive(Debug, Default, Clone)]
pub(crate) struct GraphicsState {
    /// Whether refusals are answered on the wire.
    ///
    /// Off by default, and left off by every production caller in this change.
    /// Any reply -- an error included -- tells the application that shux
    /// supports the protocol, and it will then stop using its text fallback and
    /// transmit into a terminal that cannot draw. See the [`kitty`] module docs.
    pub(crate) replies_enabled: bool,
    /// Well-formed commands, by action.
    pub(crate) transmits: u64,
    pub(crate) placements: u64,
    pub(crate) deletes: u64,
    pub(crate) queries: u64,
    /// Commands shux declined: an unimplemented transport or animation, or a
    /// control block outside the protocol.
    pub(crate) refused: u64,
}

impl GraphicsState {
    pub(crate) fn record(&mut self, action: Action) {
        let counter = match action {
            Action::Transmit => &mut self.transmits,
            Action::TransmitAndPlace => &mut self.placements,
            Action::Put => &mut self.placements,
            Action::Delete => &mut self.deletes,
            Action::Query => &mut self.queries,
            // Refused in `parse`, so it never reaches a counter here.
            Action::Animation => &mut self.refused,
        };
        *counter = counter.saturating_add(1);
    }
}
