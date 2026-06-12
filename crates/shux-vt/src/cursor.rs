use std::sync::Arc;

use crate::cell::{CellStyle, ExtendedAttrs};

/// Cursor shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}

/// Saved cursor state (for DECSC/DECRC -- ESC 7 / ESC 8).
#[derive(Debug, Clone)]
pub struct SavedCursor {
    pub row: usize,
    pub col: usize,
    pub style: CellStyle,
    pub extended: Option<Arc<ExtendedAttrs>>,
    pub auto_wrap_pending: bool,
    pub origin_mode: bool,
}

/// Terminal cursor state.
#[derive(Debug, Clone)]
pub struct Cursor {
    /// Current row (0-indexed, relative to visible area top).
    pub row: usize,
    /// Current column (0-indexed).
    pub col: usize,
    /// Current style that will be applied to newly written cells.
    pub style: CellStyle,
    /// Current extended attributes that will be applied to newly written cells.
    pub extended: Option<Arc<ExtendedAttrs>>,
    /// Cursor shape.
    pub shape: CursorShape,
    /// Whether the cursor is visible.
    pub visible: bool,
    /// Whether a wrap is pending (cursor is past the right margin but hasn't wrapped yet).
    /// This is the "auto-wrap pending" state described in VT102 behavior.
    pub auto_wrap_pending: bool,
    /// Saved cursor state (DECSC/DECRC).
    pub saved: Option<SavedCursor>,
}

impl Cursor {
    pub fn new() -> Self {
        Cursor {
            row: 0,
            col: 0,
            style: CellStyle::default(),
            extended: None,
            shape: CursorShape::Block,
            visible: true,
            auto_wrap_pending: false,
            saved: None,
        }
    }

    /// Save cursor state (DECSC -- ESC 7).
    pub fn save(&mut self, origin_mode: bool) {
        self.saved = Some(SavedCursor {
            row: self.row,
            col: self.col,
            style: self.style,
            extended: self.extended.clone(),
            auto_wrap_pending: self.auto_wrap_pending,
            origin_mode,
        });
    }

    /// Restore cursor state (DECRC -- ESC 8). Returns the saved origin_mode.
    pub fn restore(&mut self) -> Option<bool> {
        if let Some(ref saved) = self.saved.clone() {
            self.row = saved.row;
            self.col = saved.col;
            self.style = saved.style;
            self.extended = saved.extended.clone();
            self.auto_wrap_pending = saved.auto_wrap_pending;
            Some(saved.origin_mode)
        } else {
            None
        }
    }

    /// Clamp cursor position to the grid bounds.
    pub fn clamp(&mut self, rows: usize, cols: usize) {
        self.row = self.row.min(rows.saturating_sub(1));
        self.col = self.col.min(cols.saturating_sub(1));
        self.auto_wrap_pending = false;
        if let Some(saved) = &mut self.saved {
            saved.row = saved.row.min(rows.saturating_sub(1));
            saved.col = saved.col.min(cols.saturating_sub(1));
            saved.auto_wrap_pending = false;
        }
    }
}

impl Default for Cursor {
    fn default() -> Self {
        Cursor::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::{CellFlags, Color};

    #[test]
    fn test_cursor_default() {
        let cursor = Cursor::new();
        assert_eq!(cursor.row, 0);
        assert_eq!(cursor.col, 0);
        assert_eq!(cursor.shape, CursorShape::Block);
        assert!(cursor.visible);
        assert!(!cursor.auto_wrap_pending);
        assert!(cursor.saved.is_none());
    }

    #[test]
    fn test_cursor_save_restore() {
        let mut cursor = Cursor::new();
        cursor.row = 5;
        cursor.col = 10;
        cursor.style.fg = Color::Indexed(1);
        cursor.style.flags.set(CellFlags::BOLD);
        cursor.auto_wrap_pending = true;

        cursor.save(true);

        // Move cursor elsewhere.
        cursor.row = 0;
        cursor.col = 0;
        cursor.style = CellStyle::default();
        cursor.auto_wrap_pending = false;

        let origin = cursor.restore();
        assert_eq!(origin, Some(true));
        assert_eq!(cursor.row, 5);
        assert_eq!(cursor.col, 10);
        assert_eq!(cursor.style.fg, Color::Indexed(1));
        assert!(cursor.style.flags.contains(CellFlags::BOLD));
        assert!(cursor.auto_wrap_pending);
    }

    #[test]
    fn test_cursor_restore_without_save() {
        let mut cursor = Cursor::new();
        assert_eq!(cursor.restore(), None);
    }

    #[test]
    fn test_cursor_clamp() {
        let mut cursor = Cursor::new();
        cursor.row = 100;
        cursor.col = 200;
        cursor.auto_wrap_pending = true;
        cursor.clamp(24, 80);
        assert_eq!(cursor.row, 23);
        assert_eq!(cursor.col, 79);
        assert!(!cursor.auto_wrap_pending);
    }

    #[test]
    fn test_cursor_clamp_zero_size() {
        let mut cursor = Cursor::new();
        cursor.row = 5;
        cursor.col = 10;
        cursor.clamp(0, 0);
        assert_eq!(cursor.row, 0);
        assert_eq!(cursor.col, 0);
    }

    #[test]
    fn test_cursor_clamp_also_clamps_saved_cursor() {
        let mut cursor = Cursor::new();
        cursor.row = 5;
        cursor.col = 10;
        cursor.auto_wrap_pending = true;
        cursor.save(false);

        cursor.clamp(2, 3);
        cursor.row = 0;
        cursor.col = 0;
        cursor.restore();

        assert_eq!(cursor.row, 1);
        assert_eq!(cursor.col, 2);
        assert!(!cursor.auto_wrap_pending);
    }

    #[test]
    fn test_cursor_save_persists_after_restore() {
        let mut cursor = Cursor::new();
        cursor.row = 3;
        cursor.col = 7;
        cursor.save(false);

        // First restore.
        cursor.row = 0;
        cursor.col = 0;
        let origin1 = cursor.restore();
        assert_eq!(origin1, Some(false));
        assert_eq!(cursor.row, 3);
        assert_eq!(cursor.col, 7);

        // Second restore should still work (saved state is retained).
        cursor.row = 10;
        cursor.col = 20;
        let origin2 = cursor.restore();
        assert_eq!(origin2, Some(false));
        assert_eq!(cursor.row, 3);
        assert_eq!(cursor.col, 7);
    }
}
