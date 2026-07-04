//! Shared bridge types referenced by both `Command` and `Event`.

use serde::{Deserialize, Serialize};
use tsify_next::Tsify;

/// Caret/anchor position in the document model: a `BlockPath` that
/// terminates at a `Block::Paragraph` plus a byte offset inside that
/// paragraph's UTF-8 text.
///
/// **Phase 5 PR 4.** Migrated from the paragraph-flat `{ para, offset }`
/// shape so the caret can sit inside a table cell. Cross-paragraph
/// ranges whose endpoints share a parent container behave the same as
/// the paragraph-flat path; cross-container ranges (cell ↔ body) fall
/// back to in-container clamping at PR 4 — full cross-container
/// linear semantics land with Phase 5c.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, PartialEq, Eq)]
pub struct LogicalPos {
    pub path: BlockPath,
    pub offset: u32,
}

/// Address of a `Block` inside the document. Walks from the root
/// `blocks` container; the final step terminates at a block (or, when
/// followed by a `Cell` step, descends into a table cell). Phase 5
/// PR 3 mirrors `engine::BlockPath` over the wire.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, PartialEq, Eq, Default)]
pub struct BlockPath {
    pub steps: Vec<PathStep>,
}

#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PathStep {
    /// Index into the current `Vec<Block>` / `Vector<Block>`.
    Block { idx: u32 },
    /// Step from a `Block::Table` into one of its cells.
    Cell { row: u32, col: u32 },
}

impl BlockPath {
    pub fn top(idx: u32) -> Self {
        Self {
            steps: vec![PathStep::Block { idx }],
        }
    }

    /// Empty path — the document root container. `[]` matches no
    /// block; useful as a sentinel.
    pub fn root() -> Self {
        Self { steps: Vec::new() }
    }

    /// Parent container path — every step but the last.
    pub fn parent(&self) -> Self {
        let mut steps = self.steps.clone();
        steps.pop();
        Self { steps }
    }

    /// The final step's block index when this path terminates at a
    /// `Block`-step. `None` for empty paths or paths whose last step
    /// is a `Cell`.
    pub fn last_block_index(&self) -> Option<u32> {
        match self.steps.last()? {
            PathStep::Block { idx } => Some(*idx),
            PathStep::Cell { .. } => None,
        }
    }

    /// Compare two paths in document order (depth-first walk). Earlier
    /// blocks sort before later; a parent sorts before its first
    /// child. Used to put selection endpoints in canonical order.
    pub fn cmp_doc_order(&self, other: &Self) -> core::cmp::Ordering {
        use core::cmp::Ordering;
        let n = self.steps.len().min(other.steps.len());
        for i in 0..n {
            let ord = match (&self.steps[i], &other.steps[i]) {
                (PathStep::Block { idx: a }, PathStep::Block { idx: b }) => a.cmp(b),
                (PathStep::Cell { row: r1, col: c1 }, PathStep::Cell { row: r2, col: c2 }) => {
                    r1.cmp(r2).then_with(|| c1.cmp(c2))
                }
                /* Shape mismatch between two well-formed paths is
                unreachable (a Cell step always follows a Block step
                that descends into a table). Fall through to comparing
                by index when it happens. */
                (PathStep::Block { idx: a }, PathStep::Cell { row: b, .. }) => a.cmp(b),
                (PathStep::Cell { row: a, .. }, PathStep::Block { idx: b }) => a.cmp(b),
            };
            if ord != Ordering::Equal {
                return ord;
            }
        }
        self.steps.len().cmp(&other.steps.len())
    }

    /// `true` when this path is a prefix of `descendant` (or equal).
    pub fn is_ancestor_of(&self, descendant: &Self) -> bool {
        if self.steps.len() > descendant.steps.len() {
            return false;
        }
        self.steps
            .iter()
            .zip(descendant.steps.iter())
            .all(|(a, b)| a == b)
    }
}

/// Half-open span between two logical positions.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, PartialEq, Eq)]
pub struct LogicalRange {
    pub start: LogicalPos,
    pub end: LogicalPos,
}

/// What flavour of selection is currently active (RFC §4.4).
///
/// `Linear` is the classic text-span selection — the caret highlights
/// a contiguous byte range. `TableCells` is the cell-rectangular
/// selection a user drags inside a table: every cell in the rectangle
/// `(from_row, from_col) ..= (to_row, to_col)` is highlighted as a
/// whole.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, Default, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SelectionKind {
    #[default]
    Linear,
    TableCells {
        table_path: BlockPath,
        from_row: u32,
        from_col: u32,
        to_row: u32,
        to_col: u32,
    },
}

/// Axis-aligned rectangle in CSS pixels.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// A point in canvas device pixels — a pointer hit-test coordinate
/// (PHASE_4_HEADLESS_UI.md §7).
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

/// Issue #44 — one inline image's on-canvas rectangle plus the address
/// needed to resize it. `rect` is absolute document device px (same
/// space as `Event::SelectionChanged.rects`; the shell divides by
/// `devicePixelRatio` for its DOM overlay). `path` + `at` (the `U+FFFC`
/// sentinel byte offset) address the image for `Command::ResizeImage`.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
pub struct ImageRect {
    pub path: BlockPath,
    pub at: u32,
    pub rel_id: String,
    pub rect: Rect,
    /// Current display extent in EMU (`<wp:extent>`). Lets the resize
    /// handles scale by a pure CSS-px ratio (`new_emu = emu × new_px /
    /// old_px`) — zoom / DPR independent, no reconstruction of the layout
    /// scale chain in the shell.
    pub width_emu: i64,
    pub height_emu: i64,
}

/// Document container format.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
#[serde(rename_all = "snake_case")]
pub enum DocFormat {
    Docx,
    Pdf,
    PlainText,
    Html,
}

/// 8-bit-per-channel RGBA color.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

/// Underline decoration style.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
pub enum UnderlineStyle {
    None,
    Single,
    Double,
    Dotted,
    Dashed,
    Wavy,
}

/// Sub-/super-script positioning.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
pub enum VerticalScript {
    Normal,
    Superscript,
    Subscript,
}

/// Directionality of a selection or a resolved text run.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Ltr,
    Rtl,
}

/// Paragraph text alignment (Backlog #9). `Start` / `End` are
/// writing-direction-relative — they resolve against the paragraph's base
/// direction at layout time; `Center` and `Justify` are absolute. Serializes
/// as the bare variant string (`"Start"`, `"Center"`, …).
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alignment {
    Start,
    End,
    Center,
    Justify,
}

/// Unicode script, reported when a glyph needs a font the engine lacks.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
pub enum Script {
    Common,
    Latin,
    Greek,
    Cyrillic,
    Arabic,
    Hebrew,
    Han,
    Hiragana,
    Katakana,
    Hangul,
    Devanagari,
    Thai,
    Unknown,
}

/// Resolved (fully-specified) inline text attributes at a position or range.
/// The sparse-patch counterpart is [`crate::TextAttrsPatch`].
#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
pub struct TextAttrs {
    pub bold: bool,
    pub italic: bool,
    pub underline: UnderlineStyle,
    pub strike: bool,
    pub font_family: String,
    pub font_size: f32,
    pub color: Color,
    pub bg_color: Option<Color>,
    pub script: VerticalScript,
    pub language: String,
    /// `<w:caps/>` — render every glyph in its uppercase form. Wins
    /// over `small_caps` per OOXML §17.3.2.7 when both are set.
    pub caps: bool,
    /// `<w:smallCaps/>` — render lowercase as smaller uppercase glyphs.
    pub small_caps: bool,
}
