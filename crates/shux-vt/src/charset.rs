/// GL charset slot selected by SI/SO.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CharsetSlot {
    #[default]
    G0,
    G1,
}

/// Supported printable charset designation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TerminalCharset {
    #[default]
    Ascii,
    DecSpecialGraphics,
}

/// Minimal VT100 charset state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TerminalCharsets {
    pub g0: TerminalCharset,
    pub g1: TerminalCharset,
    pub active: CharsetSlot,
}
