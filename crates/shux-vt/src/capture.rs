//! Lens-gate capture schema (task 078).
//!
//! Serializes a [`VirtualTerminal`](crate::VirtualTerminal) frame into a
//! canonical, lossless, self-describing JSON envelope for visual-regression
//! goldens. The frozen design rulings (R1–R10) are recorded in
//! `docs/tasks/078-lens-gate-capture-schema-contract.md`; the load-bearing ones:
//!
//! - **R2** — run content is a JSON *string* iff every cell in the run is simple
//!   (one Unicode scalar, `width == 1`), else a per-column *array*. Exactly one
//!   legal encoding per run (the validator rejects the other).
//! - **R3** — a wide glyph's continuation cell is an explicit `""` array entry
//!   that *escapes the run's style* and decodes to [`Cell::wide_continuation`].
//!   Geometry is therefore self-describing: decode never consults `unicode-width`
//!   (drift-immune). The validator *does* use width, but only to reject a
//!   non-canonical encoding — a real width-table change correctly invalidates a
//!   golden, which task 080's fingerprint catches as `stale_golden`.
//! - **R6/R7** — style flags are named booleans (skip-if-false); extended attrs
//!   (`hyperlink`, `underline_color`, `underline_style`) live in the run style.
//! - **R8** — a mask is structural: `[col, null, {"mask":true,"cells":n}]`, never
//!   glyph text (a `▮` sentinel could collide with real content).
//! - **R9** — `schema: 1` + `deny_unknown_fields` (fail closed); an unsupported
//!   schema is a typed error ([`CaptureError::UnsupportedSchema`]), never a panic.

use serde::{Deserialize, Serialize};
use unicode_width::UnicodeWidthStr;

use crate::cell::{Cell, CellFlags, CellStyle, Color, ExtendedAttrs, UnderlineStyle};
use crate::cursor::{Cursor, CursorShape};
use crate::grid::Grid;
use crate::{TerminalDefaultColors, VirtualTerminal};

/// Frozen schema version. Bump only with a `GATE-TEST-CHANGE:` trailer.
pub const SCHEMA_VERSION: u32 = 1;

/// Errors from parsing or validating a captured frame. Typed so a CI gate can
/// classify (`stale_golden` / `scenario_error`) instead of panicking (R9).
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CaptureError {
    #[error("unsupported capture schema {found} (this build speaks {expected})")]
    UnsupportedSchema { found: u32, expected: u32 },
    #[error("malformed capture JSON: {0}")]
    Json(String),
    #[error("non-canonical capture at row {row}: {detail}")]
    NonCanonical { row: u16, detail: String },
}

// ── Colour ────────────────────────────────────────────────────────────────

/// Semantic colour on the wire: `{"idx":n}` | `{"rgb":[r,g,b]}`. `Default` is
/// represented by ABSENCE at the style level (an omitted field), never a value —
/// this is what lets a `cell` diff stay theme-correct (grok collapses everything
/// to hex + default-as-absence, losing the distinction; we keep it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum CapColor {
    Idx(u8),
    Rgb([u8; 3]),
}

impl CapColor {
    /// `None` for [`Color::Default`] (encoded as an absent field).
    fn from_color(c: Color) -> Option<Self> {
        match c {
            Color::Default => None,
            Color::Indexed(i) => Some(CapColor::Idx(i)),
            Color::Rgb(r, g, b) => Some(CapColor::Rgb([r, g, b])),
        }
    }

    fn to_color(self) -> Color {
        match self {
            CapColor::Idx(i) => Color::Indexed(i),
            CapColor::Rgb([r, g, b]) => Color::Rgb(r, g, b),
        }
    }
}

fn color_from_opt(o: Option<CapColor>) -> Color {
    o.map(CapColor::to_color).unwrap_or(Color::Default)
}

// ── Underline style ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnderlineStyleRepr {
    #[default]
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

impl UnderlineStyleRepr {
    fn is_none(&self) -> bool {
        matches!(self, UnderlineStyleRepr::None)
    }

    fn from_vt(u: UnderlineStyle) -> Self {
        match u {
            UnderlineStyle::None => UnderlineStyleRepr::None,
            UnderlineStyle::Single => UnderlineStyleRepr::Single,
            UnderlineStyle::Double => UnderlineStyleRepr::Double,
            UnderlineStyle::Curly => UnderlineStyleRepr::Curly,
            UnderlineStyle::Dotted => UnderlineStyleRepr::Dotted,
            UnderlineStyle::Dashed => UnderlineStyleRepr::Dashed,
        }
    }

    fn to_vt(self) -> UnderlineStyle {
        match self {
            UnderlineStyleRepr::None => UnderlineStyle::None,
            UnderlineStyleRepr::Single => UnderlineStyle::Single,
            UnderlineStyleRepr::Double => UnderlineStyle::Double,
            UnderlineStyleRepr::Curly => UnderlineStyle::Curly,
            UnderlineStyleRepr::Dotted => UnderlineStyle::Dotted,
            UnderlineStyleRepr::Dashed => UnderlineStyle::Dashed,
        }
    }
}

// ── Style ───────────────────────────────────────────────────────────────────

fn is_false(b: &bool) -> bool {
    !*b
}

/// The serialized style object for a run. Every field is skip-if-default so a
/// default-styled run emits no style object at all (R6). Extended attrs are
/// part of style, not text (R7).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapStyle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg: Option<CapColor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<CapColor>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub bold: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub dim: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub italic: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub underline: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub blink: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub inverse: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub hidden: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub strikethrough: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hyperlink: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underline_color: Option<CapColor>,
    #[serde(default, skip_serializing_if = "UnderlineStyleRepr::is_none")]
    pub underline_style: UnderlineStyleRepr,
}

impl CapStyle {
    fn is_default(&self) -> bool {
        *self == CapStyle::default()
    }

    /// Build from a VT cell's style + extended attrs. `inverse` is stored raw
    /// (the rasterizer resolves it), so the capture matches VT state, not render
    /// state (grok gotcha #2).
    fn from_cell(cell: &Cell) -> Self {
        let s: &CellStyle = &cell.style;
        let f = s.flags;
        let ext = cell.extended.as_deref();
        CapStyle {
            fg: CapColor::from_color(s.fg),
            bg: CapColor::from_color(s.bg),
            bold: f.contains(CellFlags::BOLD),
            dim: f.contains(CellFlags::DIM),
            italic: f.contains(CellFlags::ITALIC),
            underline: f.contains(CellFlags::UNDERLINE),
            blink: f.contains(CellFlags::BLINK),
            inverse: f.contains(CellFlags::INVERSE),
            hidden: f.contains(CellFlags::HIDDEN),
            strikethrough: f.contains(CellFlags::STRIKETHROUGH),
            hyperlink: ext.and_then(|e| e.hyperlink.clone()),
            underline_color: ext
                .and_then(|e| e.underline_color)
                .and_then(CapColor::from_color),
            underline_style: ext
                .map(|e| UnderlineStyleRepr::from_vt(e.underline_style))
                .unwrap_or_default(),
        }
    }

    fn to_flags(&self) -> CellFlags {
        let mut flags = CellFlags::default();
        if self.bold {
            flags.set(CellFlags::BOLD);
        }
        if self.dim {
            flags.set(CellFlags::DIM);
        }
        if self.italic {
            flags.set(CellFlags::ITALIC);
        }
        if self.underline {
            flags.set(CellFlags::UNDERLINE);
        }
        if self.blink {
            flags.set(CellFlags::BLINK);
        }
        if self.inverse {
            flags.set(CellFlags::INVERSE);
        }
        if self.hidden {
            flags.set(CellFlags::HIDDEN);
        }
        if self.strikethrough {
            flags.set(CellFlags::STRIKETHROUGH);
        }
        flags
    }

    /// The [`ExtendedAttrs`] implied by this style, or `None` if all extended
    /// fields are default (so the common path keeps `Cell::extended == None`).
    fn to_extended(&self, grapheme: Option<String>) -> Option<ExtendedAttrs> {
        let underline_color = self.underline_color.map(CapColor::to_color);
        let underline_style = self.underline_style.to_vt();
        if grapheme.is_none()
            && self.hyperlink.is_none()
            && underline_color.is_none()
            && matches!(underline_style, UnderlineStyle::None)
        {
            return None;
        }
        Some(ExtendedAttrs {
            grapheme,
            hyperlink: self.hyperlink.clone(),
            underline_color,
            underline_style,
        })
    }
}

// ── Run ─────────────────────────────────────────────────────────────────────

/// The content payload of a non-mask run: a bare string when every cell is
/// simple (R2), else one entry per grid column (a `""` entry is a wide
/// continuation, R3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunContent {
    Simple(String),
    Complex(Vec<String>),
}

/// One run: a maximal span of same-styled cells, or a structural mask region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Run {
    Cells {
        col: u16,
        content: RunContent,
        style: CapStyle,
    },
    Mask {
        col: u16,
        cells: u16,
    },
}

/// Third array element when it is a mask: `{"mask":true,"cells":n}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MaskMeta {
    mask: MaskTrue,
    cells: u16,
}

/// A serde shim that only accepts the literal `true` (so a `false` mask marker
/// cannot exist).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MaskTrue;

impl Serialize for MaskTrue {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bool(true)
    }
}

impl<'de> Deserialize<'de> for MaskTrue {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let b = bool::deserialize(d)?;
        if b {
            Ok(MaskTrue)
        } else {
            Err(serde::de::Error::custom("mask marker must be true"))
        }
    }
}

/// Untagged view of a run's content slot: `null` (mask), a string (simple), or
/// an array (complex).
#[derive(Deserialize)]
#[serde(untagged)]
enum RawContent {
    Str(String),
    Arr(Vec<String>),
}

/// Untagged view of the optional third slot: a mask meta or a style. `MaskMeta`
/// requires a `mask` field and `CapStyle` denies unknown fields, so the two are
/// unambiguous.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawThird {
    Mask(MaskMeta),
    Style(Box<CapStyle>),
}

impl Serialize for Run {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        match self {
            Run::Cells {
                col,
                content,
                style,
            } => {
                let emit_style = !style.is_default();
                let len = if emit_style { 3 } else { 2 };
                let mut seq = serializer.serialize_seq(Some(len))?;
                seq.serialize_element(col)?;
                match content {
                    RunContent::Simple(s) => seq.serialize_element(s)?,
                    RunContent::Complex(v) => seq.serialize_element(v)?,
                }
                if emit_style {
                    seq.serialize_element(style)?;
                }
                seq.end()
            }
            Run::Mask { col, cells } => {
                let mut seq = serializer.serialize_seq(Some(3))?;
                seq.serialize_element(col)?;
                seq.serialize_element(&Option::<String>::None)?; // null
                seq.serialize_element(&MaskMeta {
                    mask: MaskTrue,
                    cells: *cells,
                })?;
                seq.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for Run {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct RunVisitor;

        impl<'de> serde::de::Visitor<'de> for RunVisitor {
            type Value = Run;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a run array [col, content, style?]")
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Run, A::Error> {
                use serde::de::Error;
                let col: u16 = seq
                    .next_element()?
                    .ok_or_else(|| A::Error::invalid_length(0, &self))?;
                let content: Option<RawContent> = seq
                    .next_element()?
                    .ok_or_else(|| A::Error::invalid_length(1, &self))?;
                let third: Option<RawThird> = seq.next_element()?;
                // Reject trailing elements.
                if seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
                    return Err(A::Error::custom("run array has too many elements"));
                }

                match content {
                    None => {
                        // mask run: third must be a MaskMeta
                        match third {
                            Some(RawThird::Mask(m)) => {
                                if m.cells == 0 {
                                    return Err(A::Error::custom("mask run must cover >0 cells"));
                                }
                                Ok(Run::Mask {
                                    col,
                                    cells: m.cells,
                                })
                            }
                            _ => Err(A::Error::custom(
                                "null content requires a mask meta as the third element",
                            )),
                        }
                    }
                    Some(RawContent::Str(s)) => match third {
                        Some(RawThird::Mask(_)) => {
                            Err(A::Error::custom("string content cannot carry a mask meta"))
                        }
                        Some(RawThird::Style(st)) => Ok(Run::Cells {
                            col,
                            content: RunContent::Simple(s),
                            style: *st,
                        }),
                        None => Ok(Run::Cells {
                            col,
                            content: RunContent::Simple(s),
                            style: CapStyle::default(),
                        }),
                    },
                    Some(RawContent::Arr(v)) => match third {
                        Some(RawThird::Mask(_)) => {
                            Err(A::Error::custom("array content cannot carry a mask meta"))
                        }
                        Some(RawThird::Style(st)) => Ok(Run::Cells {
                            col,
                            content: RunContent::Complex(v),
                            style: *st,
                        }),
                        None => Ok(Run::Cells {
                            col,
                            content: RunContent::Complex(v),
                            style: CapStyle::default(),
                        }),
                    },
                }
            }
        }

        deserializer.deserialize_seq(RunVisitor)
    }
}

// ── Row / cursor / defaults / size ──────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RowRepr {
    pub row: u16,
    pub runs: Vec<Run>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorShapeRepr {
    #[default]
    Block,
    Underline,
    Bar,
}

impl CursorShapeRepr {
    fn from_vt(s: CursorShape) -> Self {
        match s {
            CursorShape::Block => CursorShapeRepr::Block,
            CursorShape::Underline => CursorShapeRepr::Underline,
            CursorShape::Bar => CursorShapeRepr::Bar,
        }
    }
}

/// Cursor semantics carried in the envelope. No `blinking` (the VT does not
/// track it, R4) and no `color` (OSC 12 lives in [`Defaults::cursor`], R4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CursorRepr {
    pub row: u16,
    pub col: u16,
    pub visible: bool,
    pub shape: CursorShapeRepr,
}

/// OSC 10/11/12 dynamic default colours. Omitted when the embedding terminal's
/// fallback is in effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg: Option<[u8; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<[u8; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<[u8; 3]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Size {
    pub rows: u16,
    pub cols: u16,
}

// ── Mask set ────────────────────────────────────────────────────────────────

/// A rectangular region to redact before serialize/hash/diff (R8). Columns are
/// `[col, col+width)` on `row`. A mask replaces real content with a structural
/// sentinel, so a timestamp or random ID never enters a golden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaskRect {
    pub row: u16,
    pub col: u16,
    pub width: u16,
}

/// The masks to apply to a capture. Empty = no redaction.
#[derive(Debug, Clone, Default)]
pub struct MaskSet {
    rects: Vec<MaskRect>,
}

impl MaskSet {
    pub fn new() -> Self {
        MaskSet::default()
    }

    pub fn with(mut self, row: u16, col: u16, width: u16) -> Self {
        if width > 0 {
            self.rects.push(MaskRect { row, col, width });
        }
        self
    }

    /// `true` iff column `col` of `row` is masked.
    fn masked(&self, row: u16, col: u16) -> bool {
        self.rects
            .iter()
            .any(|r| r.row == row && col >= r.col && col < r.col.saturating_add(r.width))
    }

    fn any_in_row(&self, row: u16) -> bool {
        self.rects.iter().any(|r| r.row == row)
    }
}

// ── Envelope ────────────────────────────────────────────────────────────────

/// A self-describing, lossless snapshot of a terminal frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrameEnvelope {
    pub schema: u32,
    pub size: Size,
    pub alt_screen: bool,
    pub defaults: Defaults,
    pub cursor: CursorRepr,
    pub palette_overridden: bool,
    pub rows: Vec<RowRepr>,
}

impl FrameEnvelope {
    /// Capture the visible frame of a live terminal.
    pub fn from_terminal(vt: &VirtualTerminal, masks: &MaskSet) -> Self {
        Self::from_parts(
            vt.grid(),
            vt.cursor(),
            vt.default_colors(),
            vt.is_alternate_screen(),
            vt.palette_overridden(),
            masks,
        )
    }

    /// Capture from raw parts (used by tests and by callers that already hold a
    /// grid). Prefer [`FrameEnvelope::from_terminal`].
    pub fn from_parts(
        grid: &Grid,
        cursor: &Cursor,
        defaults: TerminalDefaultColors,
        alt_screen: bool,
        palette_overridden: bool,
        masks: &MaskSet,
    ) -> Self {
        let rows = grid.rows();
        let cols = grid.cols();
        let mut out_rows = Vec::with_capacity(rows);

        for r in 0..rows {
            let runs = build_row_runs(grid, r, cols, masks);
            out_rows.push(RowRepr {
                row: r as u16,
                runs,
            });
        }

        FrameEnvelope {
            schema: SCHEMA_VERSION,
            size: Size {
                rows: rows as u16,
                cols: cols as u16,
            },
            alt_screen,
            defaults: Defaults {
                fg: defaults.fg,
                bg: defaults.bg,
                cursor: defaults.cursor,
            },
            cursor: CursorRepr {
                row: cursor.row as u16,
                col: cursor.col as u16,
                visible: cursor.visible,
                shape: CursorShapeRepr::from_vt(cursor.shape),
            },
            palette_overridden,
            rows: out_rows,
        }
    }

    /// Canonical, deterministic, pretty JSON (CI-greppable, byte-stable).
    pub fn to_canonical_json(&self) -> String {
        // Struct field order is fixed, no maps, no floats → deterministic.
        serde_json::to_string_pretty(self).expect("FrameEnvelope serialization is infallible")
    }

    /// Parse canonical JSON, failing closed on unknown fields (R9) and returning
    /// a typed [`CaptureError::UnsupportedSchema`] for a schema this build does
    /// not speak — never a panic.
    pub fn from_canonical_json(s: &str) -> Result<Self, CaptureError> {
        // Peek the schema first so an unsupported version is a clean, typed error
        // rather than a deny_unknown_fields mismatch against a future shape.
        #[derive(Deserialize)]
        struct SchemaPeek {
            schema: u32,
        }
        let peek: SchemaPeek =
            serde_json::from_str(s).map_err(|e| CaptureError::Json(e.to_string()))?;
        if peek.schema != SCHEMA_VERSION {
            return Err(CaptureError::UnsupportedSchema {
                found: peek.schema,
                expected: SCHEMA_VERSION,
            });
        }
        serde_json::from_str(s).map_err(|e| CaptureError::Json(e.to_string()))
    }

    /// Reconstruct the grid cells this envelope describes: `rows` × `cols`,
    /// row-major. Geometry comes entirely from the encoding (explicit `""`
    /// continuations, R3) — `unicode-width` is never consulted here.
    pub fn to_cells(&self) -> Vec<Vec<Cell>> {
        let cols = self.size.cols as usize;
        let mut grid = vec![vec![Cell::EMPTY; cols]; self.size.rows as usize];
        for row in &self.rows {
            let Some(line) = grid.get_mut(row.row as usize) else {
                continue;
            };
            for run in &row.runs {
                decode_run_into(run, line, cols);
            }
        }
        grid
    }

    /// Validate canonicality (the frozen validator, R2/R3/R8). See
    /// [`validate_row`] for the individual assertions.
    pub fn validate(&self) -> Result<(), CaptureError> {
        if self.schema != SCHEMA_VERSION {
            return Err(CaptureError::UnsupportedSchema {
                found: self.schema,
                expected: SCHEMA_VERSION,
            });
        }
        if self.rows.len() != self.size.rows as usize {
            return Err(CaptureError::NonCanonical {
                row: 0,
                detail: format!(
                    "row count {} != size.rows {}",
                    self.rows.len(),
                    self.size.rows
                ),
            });
        }
        for (i, row) in self.rows.iter().enumerate() {
            if row.row as usize != i {
                return Err(CaptureError::NonCanonical {
                    row: row.row,
                    detail: format!("rows out of order: index {i} carries row {}", row.row),
                });
            }
            validate_row(row, self.size.cols)?;
        }
        Ok(())
    }
}

// ── Encode: grid row → runs ─────────────────────────────────────────────────

/// Is this cell "simple" for R2 (one Unicode scalar, width 1) — eligible for the
/// compact string form?
fn cell_is_simple(cell: &Cell) -> bool {
    cell.width == 1
        && cell
            .extended
            .as_ref()
            .and_then(|e| e.grapheme.as_ref())
            .is_none()
}

/// A blank cell with default style contributes nothing — it is omitted, breaking
/// the surrounding run (runs are contiguous non-default spans).
fn cell_is_blank_default(cell: &Cell) -> bool {
    cell.ch == ' '
        && cell.width == 1
        && cell.style == CellStyle::default()
        && cell.extended.is_none()
}

struct RunBuilder {
    start: u16,
    entries: Vec<String>,
    style: CapStyle,
    all_simple: bool,
    next_col: u16,
}

impl RunBuilder {
    fn finish(self) -> Run {
        let content = if self.all_simple {
            RunContent::Simple(self.entries.concat())
        } else {
            RunContent::Complex(self.entries)
        };
        Run::Cells {
            col: self.start,
            content,
            style: self.style,
        }
    }
}

fn build_row_runs(grid: &Grid, r: usize, cols: usize, masks: &MaskSet) -> Vec<Run> {
    let row = grid.row(r);
    let mut runs: Vec<Run> = Vec::new();
    let mut cur: Option<RunBuilder> = None;
    let mut mask_start: Option<u16> = None;
    let row_u = r as u16;
    let row_has_masks = masks.any_in_row(row_u);

    let flush_cells = |cur: &mut Option<RunBuilder>, runs: &mut Vec<Run>| {
        if let Some(b) = cur.take() {
            runs.push(b.finish());
        }
    };

    let mut col = 0usize;
    while col < cols {
        let colu = col as u16;

        if row_has_masks && masks.masked(row_u, colu) {
            flush_cells(&mut cur, &mut runs);
            if mask_start.is_none() {
                mask_start = Some(colu);
            }
            col += 1;
            continue;
        } else if let Some(ms) = mask_start.take() {
            runs.push(Run::Mask {
                col: ms,
                cells: colu - ms,
            });
        }

        let cell = row
            .and_then(|rw| rw.get(col))
            .cloned()
            .unwrap_or(Cell::EMPTY);

        if cell.is_wide_continuation() {
            // A bare continuation (no preceding wide head, e.g. post-resize
            // debris) is treated as blank: break the run, skip it.
            flush_cells(&mut cur, &mut runs);
            col += 1;
            continue;
        }

        if cell_is_blank_default(&cell) {
            flush_cells(&mut cur, &mut runs);
            col += 1;
            continue;
        }

        let style = CapStyle::from_cell(&cell);
        let wide = cell.is_wide();
        let simple = cell_is_simple(&cell);

        // Extend the current run only if the style matches and the column is
        // contiguous; otherwise start a fresh run.
        let matches = cur
            .as_ref()
            .is_some_and(|b| b.style == style && b.next_col == colu);
        if !matches {
            flush_cells(&mut cur, &mut runs);
            cur = Some(RunBuilder {
                start: colu,
                entries: Vec::new(),
                style: style.clone(),
                all_simple: true,
                next_col: colu,
            });
        }

        let b = cur.as_mut().expect("run builder present");
        b.entries.push(cell.display_text().into_owned());
        if wide {
            b.entries.push(String::new()); // explicit continuation (R3)
            b.all_simple = false;
            b.next_col = colu + 2;
            col += 2;
        } else {
            if !simple {
                b.all_simple = false;
            }
            b.next_col = colu + 1;
            col += 1;
        }
    }

    flush_cells(&mut cur, &mut runs);
    if let Some(ms) = mask_start.take() {
        runs.push(Run::Mask {
            col: ms,
            cells: cols as u16 - ms,
        });
    }
    runs
}

// ── Decode: run → cells ─────────────────────────────────────────────────────

fn decode_run_into(run: &Run, line: &mut [Cell], cols: usize) {
    match run {
        Run::Mask { col, cells } => {
            // A masked cell decodes to a stable structural placeholder so
            // geometry round-trips; its real content was never serialized.
            let start = (*col as usize).min(cols);
            let end = (start + *cells as usize).min(cols);
            for cell in &mut line[start..end] {
                *cell = mask_placeholder();
            }
        }
        Run::Cells {
            col,
            content,
            style,
        } => {
            let flags = style.to_flags();
            let cell_style = CellStyle {
                fg: color_from_opt(style.fg),
                bg: color_from_opt(style.bg),
                flags,
            };
            let mut c = *col as usize;
            match content {
                RunContent::Simple(s) => {
                    for ch in s.chars() {
                        if c >= cols {
                            break;
                        }
                        line[c] = make_cell(ch, None, 1, &cell_style, style);
                        c += 1;
                    }
                }
                RunContent::Complex(entries) => {
                    let mut i = 0;
                    while i < entries.len() {
                        if c >= cols {
                            break;
                        }
                        let s = &entries[i];
                        if s.is_empty() {
                            // Bare "" with no preceding head: treat as blank.
                            line[c] = Cell::EMPTY;
                            c += 1;
                            i += 1;
                            continue;
                        }
                        let wide = entries.get(i + 1).is_some_and(|n| n.is_empty());
                        let width = if wide { 2 } else { 1 };
                        let base = s.chars().next().unwrap_or(' ');
                        let grapheme = if s.chars().count() > 1 {
                            Some(s.clone())
                        } else {
                            None
                        };
                        line[c] = make_cell(base, grapheme, width, &cell_style, style);
                        if wide {
                            if c + 1 < cols {
                                line[c + 1] = Cell::wide_continuation();
                            }
                            c += 2;
                            i += 2;
                        } else {
                            c += 1;
                            i += 1;
                        }
                    }
                }
            }
        }
    }
}

fn make_cell(
    ch: char,
    grapheme: Option<String>,
    width: u8,
    cell_style: &CellStyle,
    style: &CapStyle,
) -> Cell {
    Cell {
        ch,
        width,
        style: *cell_style,
        extended: style.to_extended(grapheme).map(std::sync::Arc::new),
    }
}

/// The cell a mask decodes to: a stable, styleless placeholder. Never emitted
/// into JSON (masks serialize as [`Run::Mask`]); used only so [`to_cells`]
/// yields a full grid.
fn mask_placeholder() -> Cell {
    Cell {
        ch: '\u{25AE}', // ▮ — display only; the wire form carries no glyph
        width: 1,
        style: CellStyle::default(),
        extended: None,
    }
}

// ── Validate ────────────────────────────────────────────────────────────────

fn validate_row(row: &RowRepr, cols: u16) -> Result<(), CaptureError> {
    let err = |detail: String| CaptureError::NonCanonical {
        row: row.row,
        detail,
    };

    // A "coalescing key" identifies runs that MUST merge if adjacent (no gap):
    // two cells runs with the same style, or two masks. Adjacent runs sharing a
    // key are non-canonical (the encoder always coalesces them), so exactly one
    // encoding exists per grid.
    #[derive(PartialEq)]
    enum Key<'a> {
        Mask,
        Style(&'a CapStyle),
    }

    let mut expected_min = 0u16; // next legal start column (sorted, non-overlapping)
    let mut prev: Option<(u16, Key)> = None; // (end_col, key) of the previous run
    for run in &row.runs {
        let (col, span, key) = match run {
            Run::Mask { col, cells } => {
                if *cells == 0 {
                    return Err(err("mask covers 0 cells".into()));
                }
                (*col, *cells, Key::Mask)
            }
            Run::Cells {
                col,
                content,
                style,
            } => {
                let span = validate_cells_run(row.row, *col, content, style)?;
                (*col, span, Key::Style(style))
            }
        };

        if col < expected_min {
            return Err(err(format!(
                "run at col {col} overlaps or is out of order (expected >= {expected_min})"
            )));
        }
        // Adjacent (no gap) + same key ⇒ should have been one run.
        if let Some((prev_end, prev_key)) = &prev {
            if *prev_end == col && *prev_key == key {
                return Err(err(format!(
                    "run at col {col} is adjacent to the previous run with the same style/kind; it must coalesce (non-canonical)"
                )));
            }
        }
        let end = col
            .checked_add(span)
            .ok_or_else(|| err(format!("run at col {col} overflows")))?;
        if end > cols {
            return Err(err(format!(
                "run at col {col} spans {span} cells, past width {cols}"
            )));
        }
        expected_min = end;
        prev = Some((end, key));
    }
    Ok(())
}

/// Validate a cells run and return its column span. Enforces R2 canonicality
/// (string form iff all-simple), R3 (`""` only after a wide head), and that no
/// entry has an illegal display width.
fn validate_cells_run(
    row: u16,
    col: u16,
    content: &RunContent,
    _style: &CapStyle,
) -> Result<u16, CaptureError> {
    let err = |detail: String| CaptureError::NonCanonical { row, detail };
    match content {
        RunContent::Simple(s) => {
            if s.is_empty() {
                return Err(err(format!("empty string run at col {col}")));
            }
            // Every char must be a simple, width-1 single scalar; otherwise the
            // canonical form is the array (reject non-canonical).
            for ch in s.chars() {
                let w = UnicodeWidthStr::width(ch.to_string().as_str());
                if w != 1 {
                    return Err(err(format!(
                        "string-form run at col {col} contains non-simple char {ch:?} (width {w}); canonical form is the array"
                    )));
                }
            }
            Ok(s.chars().count() as u16)
        }
        RunContent::Complex(entries) => {
            if entries.is_empty() {
                return Err(err(format!("empty array run at col {col}")));
            }
            let mut span = 0u16;
            let mut all_simple = true;
            let mut i = 0;
            while i < entries.len() {
                let s = &entries[i];
                if s.is_empty() {
                    return Err(err(format!(
                        "array run at col {col} has a leading/orphan \"\" at index {i}"
                    )));
                }
                let scalars = s.chars().count();
                let w = UnicodeWidthStr::width(s.as_str());
                let followed_by_empty = entries.get(i + 1).is_some_and(|n| n.is_empty());
                if followed_by_empty {
                    // wide head: must be width 2
                    if w != 2 {
                        return Err(err(format!(
                            "entry {s:?} at col {col} is followed by \"\" but has width {w}, not 2"
                        )));
                    }
                    // a second "" immediately after is an error
                    if entries.get(i + 2).is_some_and(|n| n.is_empty()) {
                        return Err(err(format!("double \"\" after entry {s:?} at col {col}")));
                    }
                    all_simple = false;
                    span = span.saturating_add(2);
                    i += 2;
                } else {
                    if w == 0 || w > 2 {
                        return Err(err(format!(
                            "entry {s:?} at col {col} has illegal display width {w}"
                        )));
                    }
                    if w == 2 {
                        return Err(err(format!(
                            "wide entry {s:?} at col {col} is missing its \"\" continuation"
                        )));
                    }
                    if scalars > 1 || w != 1 {
                        all_simple = false;
                    }
                    span = span.saturating_add(w as u16);
                    i += 1;
                }
            }
            if all_simple {
                return Err(err(format!(
                    "array-form run at col {col} is all-simple; canonical form is the string"
                )));
            }
            Ok(span)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VirtualTerminal;
    use crate::grid::Grid;

    /// Capture a VT's visible frame with no masks.
    fn capture(vt: &VirtualTerminal) -> FrameEnvelope {
        FrameEnvelope::from_terminal(vt, &MaskSet::new())
    }

    /// Rebuild a Grid from a decoded cell matrix (for the fixed-point property).
    fn grid_from_cells(cells: &[Vec<Cell>]) -> Grid {
        let rows = cells.len();
        let cols = cells.first().map(|r| r.len()).unwrap_or(0);
        let mut g = Grid::new(rows, cols, crate::GridConfig::default());
        for (r, row) in cells.iter().enumerate() {
            let dst = g.visible_row_mut_marked(r);
            for (c, cell) in row.iter().enumerate() {
                dst.cells[c] = cell.clone();
            }
        }
        g
    }

    /// The core lossless invariants for a mask-free capture: serde round-trips,
    /// validates canonical, and re-encoding the decoded cells is a fixed point.
    fn assert_lossless(vt: &VirtualTerminal) -> FrameEnvelope {
        let env = capture(vt);
        env.validate().expect("capture must be canonical");

        // Serde round-trip.
        let json = env.to_canonical_json();
        let back = FrameEnvelope::from_canonical_json(&json).expect("parse");
        assert_eq!(env, back, "serde round-trip changed the envelope");

        // Byte-stability.
        assert_eq!(
            json,
            back.to_canonical_json(),
            "serialization is not byte-stable"
        );

        // Fixed point: encode(decode(env)) == env.
        let rebuilt = grid_from_cells(&env.to_cells());
        let env2 = FrameEnvelope::from_parts(
            &rebuilt,
            vt.cursor(),
            vt.default_colors(),
            vt.is_alternate_screen(),
            vt.palette_overridden(),
            &MaskSet::new(),
        );
        assert_eq!(env, env2, "encode∘decode is not a fixed point");
        env
    }

    fn vt_with(bytes: &[u8]) -> VirtualTerminal {
        let mut vt = VirtualTerminal::new(4, 20);
        vt.process(bytes);
        vt
    }

    #[test]
    fn simple_ascii_round_trips() {
        let vt = vt_with(b"hello");
        let env = assert_lossless(&vt);
        // "hello" is a single default-styled simple run → string form.
        let row0 = &env.rows[0];
        assert_eq!(row0.runs.len(), 1);
        match &row0.runs[0] {
            Run::Cells {
                col,
                content,
                style,
            } => {
                assert_eq!(*col, 0);
                assert_eq!(content, &RunContent::Simple("hello".into()));
                assert!(style.is_default());
            }
            _ => panic!("expected a cells run"),
        }
    }

    #[test]
    fn color_variants_are_distinct() {
        // indexed 4, then rgb, then default — three different runs.
        let vt = vt_with(b"\x1b[34mA\x1b[38;2;10;20;30mB\x1b[0mC");
        let env = assert_lossless(&vt);
        let runs = &env.rows[0].runs;
        assert_eq!(runs.len(), 3, "three colour runs");
        let fg = |r: &Run| match r {
            Run::Cells { style, .. } => style.fg,
            _ => None,
        };
        assert_eq!(fg(&runs[0]), Some(CapColor::Idx(4)));
        assert_eq!(fg(&runs[1]), Some(CapColor::Rgb([10, 20, 30])));
        assert_eq!(fg(&runs[2]), None, "default fg is absence, not a value");
    }

    #[test]
    fn wide_glyph_round_trips() {
        let vt = vt_with("漢字".as_bytes());
        let env = assert_lossless(&vt);
        // Wide glyphs force the array form with explicit "" continuations.
        match &env.rows[0].runs[0] {
            Run::Cells {
                content: RunContent::Complex(v),
                ..
            } => {
                assert_eq!(
                    v,
                    &vec!["漢".to_string(), "".into(), "字".into(), "".into()]
                );
            }
            other => panic!("expected complex run, got {other:?}"),
        }
    }

    #[test]
    fn combining_grapheme_round_trips() {
        // e + combining acute accent = one width-1 grapheme cell.
        let vt = vt_with("e\u{0301}x".as_bytes());
        let env = assert_lossless(&vt);
        // The grapheme cell is multi-scalar → array form; 'x' is simple.
        let cells = env.to_cells();
        assert_eq!(cells[0][0].display_text(), "e\u{0301}");
        assert_eq!(cells[0][1].ch, 'x');
    }

    #[test]
    fn zwj_emoji_round_trips() {
        let vt = vt_with("👨\u{200d}💻".as_bytes());
        let env = assert_lossless(&vt);
        let cells = env.to_cells();
        assert_eq!(cells[0][0].display_text(), "👨\u{200d}💻");
    }

    #[test]
    fn style_flags_round_trip() {
        // bold + italic + underline + strikethrough.
        let vt = vt_with(b"\x1b[1;3;4;9mX\x1b[0m");
        let env = assert_lossless(&vt);
        match &env.rows[0].runs[0] {
            Run::Cells { style, .. } => {
                assert!(style.bold && style.italic && style.underline && style.strikethrough);
                assert!(!style.dim && !style.blink && !style.inverse && !style.hidden);
            }
            _ => panic!("cells"),
        }
    }

    #[test]
    fn blank_default_cells_are_omitted() {
        // content, gap, content → two runs, gap not serialized.
        let vt = vt_with(b"A    B");
        let env = assert_lossless(&vt);
        assert_eq!(env.rows[0].runs.len(), 2, "the default gap breaks the run");
        let content = |r: &Run| match r {
            Run::Cells {
                col,
                content: RunContent::Simple(s),
                ..
            } => (*col, s.clone()),
            _ => panic!("expected simple runs"),
        };
        assert_eq!(content(&env.rows[0].runs[0]), (0, "A".into()));
        assert_eq!(
            content(&env.rows[0].runs[1]),
            (5, "B".into()),
            "B resumes after the omitted gap"
        );
    }

    #[test]
    fn colored_space_is_serialized() {
        // A space with a non-default background IS content (a coloured bar).
        let vt = vt_with(b"\x1b[41m \x1b[0m");
        let env = assert_lossless(&vt);
        assert_eq!(env.rows[0].runs.len(), 1, "the coloured space is a run");
    }

    #[test]
    fn mask_replaces_content_and_is_stable() {
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process(b"secret-token-42");
        let masks = MaskSet::new().with(0, 0, 15);
        let env = FrameEnvelope::from_terminal(&vt, &masks);
        env.validate().expect("masked capture is canonical");
        let json = env.to_canonical_json();
        assert!(
            !json.contains("secret"),
            "masked content must not enter the golden"
        );
        assert!(json.contains("mask"), "a mask run is emitted");
        // Geometry is stable: size unchanged, mask covers the 15 columns.
        assert_eq!(env.size.cols, 20);
        match &env.rows[0].runs[0] {
            Run::Mask { col, cells } => {
                assert_eq!((*col, *cells), (0, 15));
            }
            _ => panic!("expected a mask run"),
        }
    }

    #[test]
    fn mask_content_change_does_not_alter_serialization() {
        // R8 / task-080 mask invariance (080-owned artifacts): changing the
        // masked text does not change the serialized bytes.
        let masks = MaskSet::new().with(0, 0, 10);
        let mut a = VirtualTerminal::new(2, 20);
        a.process(b"AAAAAAAAAA tail");
        let mut b = VirtualTerminal::new(2, 20);
        b.process(b"BBBBBBBBBB tail");
        let ja = FrameEnvelope::from_terminal(&a, &masks).to_canonical_json();
        let jb = FrameEnvelope::from_terminal(&b, &masks).to_canonical_json();
        assert_eq!(ja, jb, "masked-region content must not affect the golden");
    }

    #[test]
    fn palette_override_flag_is_captured_without_bump() {
        let mut vt = VirtualTerminal::new(2, 10);
        let rev = vt.content_revision();
        vt.process(b"\x1b]4;1;#ff0000\x07");
        assert!(vt.palette_overridden(), "OSC 4 SET sets the sticky flag");
        assert_eq!(
            vt.content_revision(),
            rev,
            "flag must not bump content_revision"
        );
        // sticky: a later write keeps it set.
        vt.process(b"x");
        assert!(vt.palette_overridden());
        let env = capture(&vt);
        assert!(env.palette_overridden);
    }

    #[test]
    fn alt_screen_grid_and_flag_are_consistent() {
        // Entering alt then writing must capture the ALT content with the alt
        // flag set — capturing the primary grid while flagging alt would be a
        // consistency bug (from_terminal must read the presented grid).
        let mut vt = VirtualTerminal::new(4, 20);
        vt.process(b"primary");
        vt.process(b"\x1b[?1049h\x1b[3;5Hhi");
        let env = assert_lossless(&vt);
        assert!(env.alt_screen, "alt flag set");
        let cells = env.to_cells();
        // "hi" is on the alt screen at row 2, cols 4-5.
        assert_eq!(cells[2][4].ch, 'h');
        assert_eq!(cells[2][5].ch, 'i');
        // "primary" must NOT appear on the captured (alt) grid.
        let row0: String = cells[0].iter().map(|c| c.ch).collect();
        assert!(
            !row0.contains("primary"),
            "captured the primary grid, not alt"
        );
        assert_eq!(env.cursor.row, 2);
        assert_eq!(env.cursor.col, 6);

        // Leaving alt restores + captures the primary content.
        vt.process(b"\x1b[?1049l");
        let env2 = capture(&vt);
        assert!(!env2.alt_screen);
        let back: String = env2.to_cells()[0].iter().map(|c| c.ch).collect();
        assert!(back.starts_with("primary"), "primary restored on alt leave");
    }

    #[test]
    fn differing_hyperlinks_split_runs() {
        // Two adjacent cells whose ONLY style difference is the hyperlink target
        // must be separate runs — merging them would lose one link (data loss).
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process(
            b"\x1b]8;;https://a.invalid/\x1b\\A\x1b]8;;https://b.invalid/\x1b\\B\x1b]8;;\x1b\\",
        );
        let env = assert_lossless(&vt);
        let links: Vec<_> = env.rows[0]
            .runs
            .iter()
            .filter_map(|r| match r {
                Run::Cells { style, .. } => Some(style.hyperlink.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            links,
            vec![
                Some("https://a.invalid/".to_string()),
                Some("https://b.invalid/".to_string())
            ],
            "adjacent differing hyperlinks must not merge into one run"
        );
    }

    #[test]
    fn schema_mismatch_is_a_typed_error() {
        let json = r#"{"schema":2,"size":{"rows":1,"cols":1},"alt_screen":false,"defaults":{},"cursor":{"row":0,"col":0,"visible":true,"shape":"block"},"palette_overridden":false,"rows":[{"row":0,"runs":[]}]}"#;
        match FrameEnvelope::from_canonical_json(json) {
            Err(CaptureError::UnsupportedSchema {
                found: 2,
                expected: 1,
            }) => {}
            other => panic!("expected UnsupportedSchema, got {other:?}"),
        }
    }

    #[test]
    fn unknown_field_fails_closed() {
        let json = r#"{"schema":1,"size":{"rows":1,"cols":1},"alt_screen":false,"defaults":{},"cursor":{"row":0,"col":0,"visible":true,"shape":"block"},"palette_overridden":false,"rows":[],"surprise":true}"#;
        assert!(matches!(
            FrameEnvelope::from_canonical_json(json),
            Err(CaptureError::Json(_))
        ));
    }

    // ── Validator negatives (hand-built envelopes) ──────────────────────────

    fn env_with_row(cols: u16, runs: Vec<Run>) -> FrameEnvelope {
        FrameEnvelope {
            schema: SCHEMA_VERSION,
            size: Size { rows: 1, cols },
            alt_screen: false,
            defaults: Defaults::default(),
            cursor: CursorRepr {
                row: 0,
                col: 0,
                visible: true,
                shape: CursorShapeRepr::Block,
            },
            palette_overridden: false,
            rows: vec![RowRepr { row: 0, runs }],
        }
    }

    fn cells(col: u16, s: &str) -> Run {
        Run::Cells {
            col,
            content: RunContent::Simple(s.into()),
            style: CapStyle::default(),
        }
    }

    #[test]
    fn rejects_overlapping_runs() {
        let env = env_with_row(20, vec![cells(0, "abc"), cells(2, "de")]);
        assert!(env.validate().is_err(), "col 2 overlaps run [0,3)");
    }

    #[test]
    fn rejects_unsorted_runs() {
        let env = env_with_row(20, vec![cells(5, "de"), cells(0, "ab")]);
        assert!(env.validate().is_err());
    }

    #[test]
    fn rejects_wrong_row_count() {
        let mut env = env_with_row(20, vec![]);
        env.size.rows = 3; // claims 3 rows, carries 1
        assert!(env.validate().is_err());
    }

    #[test]
    fn rejects_string_form_with_wide_char() {
        let env = env_with_row(
            20,
            vec![Run::Cells {
                col: 0,
                content: RunContent::Simple("漢".into()),
                style: CapStyle::default(),
            }],
        );
        assert!(env.validate().is_err(), "wide char must use the array form");
    }

    #[test]
    fn rejects_array_form_when_all_simple() {
        let env = env_with_row(
            20,
            vec![Run::Cells {
                col: 0,
                content: RunContent::Complex(vec!["a".into(), "b".into()]),
                style: CapStyle::default(),
            }],
        );
        assert!(
            env.validate().is_err(),
            "all-simple must use the string form"
        );
    }

    #[test]
    fn rejects_wide_head_without_continuation() {
        let env = env_with_row(
            20,
            vec![Run::Cells {
                col: 0,
                content: RunContent::Complex(vec!["漢".into(), "x".into()]),
                style: CapStyle::default(),
            }],
        );
        assert!(
            env.validate().is_err(),
            "wide head missing its \"\" continuation"
        );
    }

    #[test]
    fn rejects_double_continuation() {
        let env = env_with_row(
            20,
            vec![Run::Cells {
                col: 0,
                content: RunContent::Complex(vec!["漢".into(), "".into(), "".into()]),
                style: CapStyle::default(),
            }],
        );
        assert!(env.validate().is_err(), "double \"\" is malformed");
    }

    #[test]
    fn rejects_run_past_width() {
        let env = env_with_row(3, vec![cells(0, "abcde")]);
        assert!(env.validate().is_err(), "5 cells past width 3");
    }

    #[test]
    fn rejects_zero_cell_mask() {
        let env = env_with_row(20, vec![Run::Mask { col: 0, cells: 0 }]);
        assert!(env.validate().is_err());
    }

    #[test]
    fn rejects_non_coalesced_adjacent_same_style_runs() {
        // "ab" as two adjacent default-styled runs is non-canonical — it must be
        // one run [0,"ab"]. Two encodings for one grid → golden instability.
        let env = env_with_row(20, vec![cells(0, "a"), cells(1, "b")]);
        assert!(
            env.validate().is_err(),
            "adjacent same-style runs must coalesce (exactly one encoding)"
        );
    }

    #[test]
    fn allows_adjacent_runs_with_different_styles() {
        // "AB" where A and B differ in style ARE two adjacent runs (they cannot
        // coalesce) — this must remain valid.
        let red = CapStyle {
            fg: Some(CapColor::Idx(1)),
            ..CapStyle::default()
        };
        let env = env_with_row(
            20,
            vec![
                Run::Cells {
                    col: 0,
                    content: RunContent::Simple("A".into()),
                    style: red,
                },
                cells(1, "B"),
            ],
        );
        assert!(
            env.validate().is_ok(),
            "adjacent DIFFERENT-style runs are canonical"
        );
    }

    #[test]
    fn rejects_non_coalesced_adjacent_masks() {
        let env = env_with_row(
            20,
            vec![
                Run::Mask { col: 0, cells: 2 },
                Run::Mask { col: 2, cells: 3 },
            ],
        );
        assert!(env.validate().is_err(), "adjacent masks must coalesce");
    }

    #[test]
    fn empty_grid_round_trips() {
        let vt = VirtualTerminal::new(3, 10);
        let env = assert_lossless(&vt);
        assert!(env.rows.iter().all(|r| r.runs.is_empty()));
    }

    #[test]
    fn hyperlink_extended_attr_round_trips() {
        // OSC 8 hyperlink — extended attr must survive round-trip (R7); its
        // absence in the first-draft schema was a real data-loss bug.
        let vt = vt_with(b"\x1b]8;;https://example.invalid/x\x1b\\LINK\x1b]8;;\x1b\\");
        let env = assert_lossless(&vt);
        let cells = env.to_cells();
        let link = cells[0][0]
            .extended
            .as_ref()
            .and_then(|e| e.hyperlink.clone());
        assert_eq!(link.as_deref(), Some("https://example.invalid/x"));
    }

    #[test]
    fn multi_row_layout_round_trips() {
        let mut vt = VirtualTerminal::new(4, 20);
        vt.process(b"\x1b[31mred\x1b[0m\r\n\x1b[1mbold\x1b[0m\r\n\x1b[4munder\x1b[0m");
        let env = assert_lossless(&vt);
        assert_eq!(env.rows.len(), 4);
        assert!(!env.rows[0].runs.is_empty() && !env.rows[1].runs.is_empty());
    }

    // ── Property: any driven VT round-trips losslessly ──────────────────────

    use proptest::prelude::*;

    prop_compose! {
        /// A short sequence of printable text + SGR toggles + colours, driven
        /// through a real VT so the grid is always a valid terminal state.
        fn arb_ansi()(ops in prop::collection::vec(
            prop_oneof![
                "[a-zA-Z0-9 ]{1,4}".prop_map(|s| s.into_bytes()),
                "[漢字ありがと]{1,2}".prop_map(|s| s.into_bytes()),
                Just(b"\x1b[1m".to_vec()),
                Just(b"\x1b[3m".to_vec()),
                Just(b"\x1b[4m".to_vec()),
                Just(b"\x1b[7m".to_vec()),
                Just(b"\x1b[0m".to_vec()),
                (0u8..=7).prop_map(|n| format!("\x1b[3{n}m").into_bytes()),
                (0u8..=255, 0u8..=255, 0u8..=255)
                    .prop_map(|(r,g,b)| format!("\x1b[38;2;{r};{g};{b}m").into_bytes()),
                Just(b"\r\n".to_vec()),
            ],
            0..40)) -> Vec<u8> {
            ops.concat()
        }
    }

    // ── Adversarial edge cases (preempting the adv-schema review) ───────────

    #[test]
    fn wide_glyph_in_last_column_round_trips() {
        // A wide glyph with only one column left. Whatever the VT decides
        // (wrap / drop / place), the capture must stay canonical + round-trip.
        for cols in [1usize, 2, 3, 4] {
            let mut vt = VirtualTerminal::new(2, cols);
            vt.process("aa漢字bb".as_bytes());
            let env = assert_lossless(&vt); // validate + serde + fixed point
            // No run may claim a column past the grid width (the boundary bug).
            for row in &env.rows {
                for run in &row.runs {
                    if let Run::Cells { col, content, .. } = run {
                        let span = match content {
                            RunContent::Simple(s) => s.chars().count(),
                            RunContent::Complex(v) => v.len(),
                        };
                        assert!(
                            *col as usize + span <= cols,
                            "cols={cols}: run at {col} span {span} exceeds width"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn mask_splitting_a_wide_glyph_stays_canonical() {
        // A mask whose edge falls in the middle of a wide glyph must not desync
        // columns or emit a non-canonical capture.
        let mut vt = VirtualTerminal::new(2, 10);
        vt.process("漢字ab".as_bytes()); // 漢 at 0-1, 字 at 2-3, a=4, b=5
        for (mcol, mwidth) in [(0u16, 1u16), (1, 1), (1, 2), (0, 3), (3, 2)] {
            let masks = MaskSet::new().with(0, mcol, mwidth);
            let env = FrameEnvelope::from_terminal(&vt, &masks);
            env.validate()
                .unwrap_or_else(|e| panic!("mask ({mcol},{mwidth}) not canonical: {e:?}"));
            // serde round-trip still holds.
            let json = env.to_canonical_json();
            let back = FrameEnvelope::from_canonical_json(&json).unwrap();
            assert_eq!(env, back);
        }
    }

    #[test]
    fn masked_capture_is_a_fixed_point_under_the_same_masks() {
        // Masks are lossy w.r.t. the ORIGINAL content by design, but a masked
        // capture must be stable: re-encoding its decoded cells UNDER THE SAME
        // masks reproduces it (the ▮ placeholders re-mask to the same run).
        let mut vt = VirtualTerminal::new(2, 20);
        vt.process(b"2026-07-17T09:00 tail");
        let masks = MaskSet::new().with(0, 0, 16);
        let env = FrameEnvelope::from_terminal(&vt, &masks);
        env.validate().unwrap();

        let rebuilt = grid_from_cells(&env.to_cells());
        let env2 = FrameEnvelope::from_parts(
            &rebuilt,
            vt.cursor(),
            vt.default_colors(),
            vt.is_alternate_screen(),
            vt.palette_overridden(),
            &masks,
        );
        assert_eq!(
            env, env2,
            "masked capture must be a fixed point under the same masks"
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(400))]
        #[test]
        fn arbitrary_frames_round_trip(bytes in arb_ansi()) {
            let mut vt = VirtualTerminal::new(6, 24);
            vt.process(&bytes);
            let env = capture(&vt);

            // Always canonical.
            prop_assert!(env.validate().is_ok(), "not canonical: {:?}", env.validate());

            // Serde round-trip + byte-stability.
            let json = env.to_canonical_json();
            let back = FrameEnvelope::from_canonical_json(&json).expect("parse");
            prop_assert_eq!(&env, &back);
            prop_assert_eq!(&json, &back.to_canonical_json());

            // Encode∘decode fixed point.
            let rebuilt = grid_from_cells(&env.to_cells());
            let env2 = FrameEnvelope::from_parts(
                &rebuilt, vt.cursor(), vt.default_colors(),
                vt.is_alternate_screen(), vt.palette_overridden(), &MaskSet::new());
            prop_assert_eq!(&env, &env2, "encode∘decode not a fixed point");
        }
    }
}
