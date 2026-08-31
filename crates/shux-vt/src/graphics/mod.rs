//! Kitty graphics protocol support.

pub(crate) mod apc;
pub(crate) mod image;
pub(crate) mod kitty;

/// Everything the graphics path writes, in one place.
///
/// Bundled so the graphics path's state stays in one field of
/// [`crate::VirtualTerminal`] rather than spread across it.
#[derive(Debug, Default, Clone)]
pub(crate) struct GraphicsSink {
    /// Commands declined: an unimplemented transport, animation, or a control
    /// block outside the protocol.
    pub(crate) refusals: u64,
    /// One chunked transmission in flight.
    pub(crate) assembler: image::Assembler,
    /// Every command that reached the dispatcher. Test-only: the dispatcher
    /// has no other visible output, so a public-API test alone would pass on a
    /// build that dropped them all.
    #[cfg(test)]
    pub(crate) dispatched: Vec<Vec<u8>>,
}
