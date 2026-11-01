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
    /// Transmitted images an `a=p` may place: the primary screen's, then the
    /// alternate screen's, which lives and dies with that screen as kitty's
    /// does.
    pub(crate) stores: [image::ImageStore; 2],
    /// Every command that reached the dispatcher. Test-only: the dispatcher
    /// has no other visible output, so a public-API test alone would pass on a
    /// build that dropped them all.
    #[cfg(test)]
    pub(crate) dispatched: Vec<Vec<u8>>,
}
