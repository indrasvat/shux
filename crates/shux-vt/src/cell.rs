use std::borrow::Cow;
use std::sync::Arc;

/// Compact cell flags packed into a single byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellFlags(u8);

impl CellFlags {
    pub const BOLD: u8 = 0b0000_0001;
    pub const DIM: u8 = 0b0000_0010;
    pub const ITALIC: u8 = 0b0000_0100;
    pub const UNDERLINE: u8 = 0b0000_1000;
    pub const BLINK: u8 = 0b0001_0000;
    pub const INVERSE: u8 = 0b0010_0000;
    pub const HIDDEN: u8 = 0b0100_0000;
    pub const STRIKETHROUGH: u8 = 0b1000_0000;

    pub fn contains(self, flag: u8) -> bool {
        self.0 & flag != 0
    }

    pub fn set(&mut self, flag: u8) {
        self.0 |= flag;
    }

    pub fn unset(&mut self, flag: u8) {
        self.0 &= !flag;
    }

    pub fn reset(&mut self) {
        self.0 = 0;
    }
}

/// Terminal color representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    /// Default terminal color (foreground or background).
    #[default]
    Default,
    /// Named ANSI color (0-7 normal, 8-15 bright).
    Indexed(u8),
    /// 24-bit RGB color.
    Rgb(u8, u8, u8),
}

/// Pixel color (sRGB).
pub type Rgb = [u8; 3];

/// Dynamic terminal default colors set by OSC 10/11/12.
///
/// `None` means "use the embedding terminal / rasterizer fallback".
/// Real terminal emulators treat OSC 10 and OSC 11 as changes to the
/// default foreground/background that `SGR 39` and `SGR 49` resolve to.
/// OSC 12 sets the cursor color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TerminalDefaultColors {
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    pub cursor: Option<Rgb>,
}

/// Cell style attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellStyle {
    pub fg: Color,
    pub bg: Color,
    pub flags: CellFlags,
}

/// A single terminal cell.
///
/// Optimized for memory usage (PRD 5.5 targets 4 bytes for simple ASCII).
///
/// For the initial implementation, we use a slightly larger struct (approx 16 bytes)
/// to prioritize correctness, but the API hides this detail so internal representation
/// can be optimized later without breaking consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// The character.
    pub ch: char,
    /// Display width.
    pub width: u8,
    /// Style attributes.
    pub style: CellStyle,
    /// Extended attributes (hyperlink, underline color, etc.).
    pub extended: Option<Arc<ExtendedAttrs>>,
}

/// Extended attributes that are rare enough to be heap-allocated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtendedAttrs {
    /// Full display text for rare multi-codepoint cells.
    ///
    /// Normal ASCII and simple Unicode cells keep this as `None` and use
    /// `Cell::ch` directly. The field lives in extended storage so the common
    /// cell layout does not grow for grapheme support.
    pub grapheme: Option<String>,
    /// OSC 8 hyperlink target.
    pub hyperlink: Option<String>,
    /// Underline color (separate from fg).
    pub underline_color: Option<Color>,
    /// Underline style (single, double, curly, dotted, dashed).
    pub underline_style: UnderlineStyle,
}

impl Default for ExtendedAttrs {
    fn default() -> Self {
        ExtendedAttrs {
            grapheme: None,
            hyperlink: None,
            underline_color: None,
            underline_style: UnderlineStyle::None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnderlineStyle {
    #[default]
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

impl Cell {
    /// An empty cell (space with default style).
    pub const EMPTY: Cell = Cell {
        ch: ' ',
        width: 1,
        style: CellStyle {
            fg: Color::Default,
            bg: Color::Default,
            flags: CellFlags(0),
        },
        extended: None,
    };

    /// A wide-character continuation cell (placeholder for the second column).
    pub fn wide_continuation() -> Cell {
        Cell {
            ch: ' ',
            width: 0,
            style: CellStyle::default(),
            extended: None,
        }
    }

    /// Whether this cell is a wide-character continuation placeholder.
    pub fn is_wide_continuation(&self) -> bool {
        self.width == 0
    }

    /// Whether this cell is a wide character (width 2).
    pub fn is_wide(&self) -> bool {
        self.width == 2
    }

    /// Full display text for this cell.
    pub fn display_text(&self) -> Cow<'_, str> {
        self.extended
            .as_ref()
            .and_then(|attrs| attrs.grapheme.as_deref())
            .map(Cow::Borrowed)
            .unwrap_or_else(|| Cow::Owned(self.ch.to_string()))
    }

    /// Append this cell's display text into `out` without allocating on the
    /// simple scalar path.
    pub fn push_display_text(&self, out: &mut String) {
        if let Some(text) = self
            .extended
            .as_ref()
            .and_then(|attrs| attrs.grapheme.as_deref())
        {
            out.push_str(text);
        } else {
            out.push(self.ch);
        }
    }

    /// Rare grapheme payload, if this cell stores multi-codepoint text.
    pub fn grapheme(&self) -> Option<&str> {
        self.extended
            .as_ref()
            .and_then(|attrs| attrs.grapheme.as_deref())
    }

    /// Whether this cell carries a multi-codepoint display payload.
    pub fn has_grapheme_payload(&self) -> bool {
        self.grapheme().is_some()
    }

    /// Set the rare grapheme payload. A payload equal to the scalar fast path is
    /// pruned so common cells keep `extended == None`.
    pub fn set_grapheme_payload(&mut self, text: String) {
        if text.chars().eq(std::iter::once(self.ch)) {
            if let Some(extended) = &mut self.extended {
                Arc::make_mut(extended).grapheme = None;
            }
            self.prune_default_extended();
            return;
        }
        let extended = self
            .extended
            .get_or_insert_with(|| Arc::new(ExtendedAttrs::default()));
        Arc::make_mut(extended).grapheme = Some(text);
    }

    /// Append one scalar to this cell's rare grapheme payload.
    pub fn append_grapheme_scalar(&mut self, ch: char) {
        let mut text = self.display_text().into_owned();
        text.push(ch);
        self.set_grapheme_payload(text);
    }

    /// Remove extended storage when every rare field has returned to default.
    pub fn prune_default_extended(&mut self) {
        if self.extended.as_deref() == Some(&ExtendedAttrs::default()) {
            self.extended = None;
        }
    }

    /// Reset this cell to empty with the given background color.
    pub fn reset(&mut self, bg: Color) {
        self.ch = ' ';
        self.width = 1;
        self.style = CellStyle {
            fg: Color::Default,
            bg,
            flags: CellFlags(0),
        };
        self.extended = None;
    }
}

impl Default for Cell {
    fn default() -> Self {
        Cell::EMPTY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cell_default_is_empty() {
        let cell = Cell::default();
        assert_eq!(cell.ch, ' ');
        assert_eq!(cell.width, 1);
        assert_eq!(cell.style.fg, Color::Default);
        assert_eq!(cell.style.bg, Color::Default);
        assert!(!cell.is_wide());
        assert!(!cell.is_wide_continuation());
        assert!(cell.extended.is_none());
    }

    #[test]
    fn test_cell_reset() {
        let mut cell = Cell {
            ch: 'X',
            width: 1,
            style: CellStyle {
                fg: Color::Indexed(1),
                bg: Color::Indexed(2),
                flags: CellFlags(CellFlags::BOLD),
            },
            extended: None,
        };
        cell.reset(Color::Rgb(10, 20, 30));
        assert_eq!(cell.ch, ' ');
        assert_eq!(cell.width, 1);
        assert_eq!(cell.style.fg, Color::Default);
        assert_eq!(cell.style.bg, Color::Rgb(10, 20, 30));
        assert!(!cell.style.flags.contains(CellFlags::BOLD));
    }

    #[test]
    fn test_wide_continuation() {
        let cell = Cell::wide_continuation();
        assert!(cell.is_wide_continuation());
        assert!(!cell.is_wide());
        assert_eq!(cell.width, 0);
    }

    #[test]
    fn test_cell_flags() {
        let mut flags = CellFlags::default();
        assert!(!flags.contains(CellFlags::BOLD));

        flags.set(CellFlags::BOLD);
        assert!(flags.contains(CellFlags::BOLD));
        assert!(!flags.contains(CellFlags::ITALIC));

        flags.set(CellFlags::ITALIC);
        assert!(flags.contains(CellFlags::BOLD));
        assert!(flags.contains(CellFlags::ITALIC));

        flags.unset(CellFlags::BOLD);
        assert!(!flags.contains(CellFlags::BOLD));
        assert!(flags.contains(CellFlags::ITALIC));

        flags.reset();
        assert!(!flags.contains(CellFlags::ITALIC));
    }

    #[test]
    fn test_color_default() {
        assert_eq!(Color::default(), Color::Default);
    }

    #[test]
    fn test_cell_style_default() {
        let style = CellStyle::default();
        assert_eq!(style.fg, Color::Default);
        assert_eq!(style.bg, Color::Default);
        assert!(!style.flags.contains(CellFlags::BOLD));
    }

    #[test]
    fn test_extended_attrs() {
        let ext = ExtendedAttrs {
            grapheme: None,
            hyperlink: Some("https://example.com".to_string()),
            underline_color: Some(Color::Rgb(255, 0, 0)),
            underline_style: UnderlineStyle::Curly,
        };
        let cell = Cell {
            ch: 'A',
            width: 1,
            style: CellStyle::default(),
            extended: Some(Arc::new(ext)),
        };
        let ext = cell.extended.as_ref().unwrap();
        assert_eq!(ext.hyperlink.as_deref(), Some("https://example.com"));
        assert_eq!(ext.underline_color, Some(Color::Rgb(255, 0, 0)));
        assert_eq!(ext.underline_style, UnderlineStyle::Curly);
    }
}
