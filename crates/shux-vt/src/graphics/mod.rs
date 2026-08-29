//! Kitty graphics protocol support.

pub(crate) mod apc;
pub(crate) mod kitty;

/// Everything the graphics path writes, in one place.
///
/// Bundled so [`crate::VirtualTerminal::dispatch_graphics`] takes a single
/// `&mut` argument; see that function for why it must not take `&mut self`.
#[derive(Debug, Default, Clone)]
pub(crate) struct GraphicsSink {
    /// Commands declined: an unimplemented transport, animation, or a control
    /// block outside the protocol.
    pub(crate) refusals: u64,
    /// Every command that reached the dispatcher. Test-only: the dispatcher
    /// has no other visible output, so a public-API test alone would pass on a
    /// build that dropped them all.
    #[cfg(test)]
    pub(crate) dispatched: Vec<Vec<u8>>,
}
