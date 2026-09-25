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
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct LogicalPos {
    pub path: BlockPath,
    pub offset: u32,
}

/// Address of a `Block` inside the document. Walks from the root
/// `blocks` container; the final step terminates at a block (or, when
/// followed by a `Cell` step, descends into a table cell). Phase 5
/// PR 3 mirrors `engine::BlockPath` over the wire.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, PartialEq, Eq, Default)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct BlockPath {
    pub steps: Vec<PathStep>,
}

#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
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
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
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
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
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
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// A point in canvas device pixels — a pointer hit-test coordinate
/// (PHASE_4_HEADLESS_UI.md §7).
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
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
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct ImageRect {
    pub path: BlockPath,
    pub at: u32,
    /// Issue #224 — deprecated: this is already the `DocumentTree::media`
    /// key (issue #188), not an OOXML relationship id (part-scoped ids can
    /// collide across parts, e.g. a header's `rId5` and the body's
    /// `rId5`). Kept populated for one release; use [`Self::media_key`].
    pub rel_id: String,
    /// Issue #224 — the `DocumentTree::media` key this rect's picture
    /// paints from. Same value [`Self::rel_id`] already carried; this is
    /// the correctly-named field going forward. Always populated (issue
    /// #214's audit: no reason to make tsify-next mark this optional when
    /// the engine always emits it).
    pub media_key: String,
    pub rect: Rect,
    /// Current display extent in EMU (`<wp:extent>`). Lets the resize
    /// handles scale by a pure CSS-px ratio (`new_emu = emu × new_px /
    /// old_px`) — zoom / DPR independent, no reconstruction of the layout
    /// scale chain in the shell.
    pub width_emu: i64,
    pub height_emu: i64,
    /// Issue #69 — `true` for a floating (`<wp:anchor>`) image. Floats
    /// are positioned against a reference frame instead of flowing with
    /// the text, so the shell's body-drag repositions them through
    /// `Command::MoveImage`; inline images (`false`) only resize.
    pub floating: bool,
    /// Issue #69 — top-left corner of the reference frame the float's
    /// offsets are measured from, in the same absolute device-px space as
    /// `rect` (`0.0` for inline images and `simplePos` floats, whose frame
    /// is the page corner). The shell turns a dragged rect back into
    /// frame-relative EMU offsets with the pure ratio `width_emu / rect.w`
    /// — zoom / DPR independent, like the resize handles.
    pub frame_x: f32,
    pub frame_y: f32,
    /// Issue #82 — the floating image's text-wrap mode (`None` for an
    /// inline image, which flows with the text). Drives the wrap picker's
    /// checked state; changed through `Command::SetImageWrap`.
    #[serde(default)]
    pub wrap: Option<ImageWrapMode>,
    /// Issue #206 — the text-box story the picture lives in: empty for a
    /// body (or table-cell) picture; otherwise one [`TextBoxHop`] per box
    /// descended into, outermost first (a picture in a box nested in a
    /// box carries two). `path` is then rooted in the LAST hop's story
    /// (`Block(i)` = that story's i-th block). Hand it back verbatim as
    /// the `story` of `Command::ResizeImage` / `MoveImage` /
    /// `SetImageWrap` to address the picture.
    #[serde(default)]
    pub story: Vec<TextBoxHop>,
    /// Issue #206 — the owning story's id in the `outer/inner` form
    /// `SelectionChanged.editing_story.rid` and the a11y text-box regions
    /// use (`"1@0"`, `"1@0/1@0"`); empty for a body picture.
    #[serde(default)]
    pub story_rid: String,
}

/// Issue #206 — one step into a text-box story: the text box anchored at
/// byte `at` (its `U+FFFC` sentinel) of the paragraph at `path`. The
/// first hop's `path` is body-rooted; every later hop's is rooted in the
/// previous hop's story. Depth is bounded by the text-box nesting cap
/// (two) — a longer chain addresses nothing.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, PartialEq, Eq, Default)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct TextBoxHop {
    pub path: BlockPath,
    pub at: u32,
}

/// Issue #82 — the user-facing text-wrap modes of a floating image, as
/// Word's "Wrap Text" menu names them. `BehindText` / `InFrontOfText` are
/// both `<wp:wrapNone/>`, told apart by `behindDoc`.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[serde(rename_all = "snake_case")]
pub enum ImageWrapMode {
    Square,
    Tight,
    Through,
    TopAndBottom,
    BehindText,
    InFrontOfText,
}

/// Document container format.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[serde(rename_all = "snake_case")]
pub enum DocFormat {
    Docx,
    Pdf,
    PlainText,
    Html,
}

/// 8-bit-per-channel RGBA color.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

/// Underline decoration style.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
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
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub enum VerticalScript {
    Normal,
    Superscript,
    Subscript,
}

/// Directionality of a selection or a resolved text run.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub enum Direction {
    Ltr,
    Rtl,
}

/// Paragraph text alignment (Backlog #9). `Start` / `End` are
/// writing-direction-relative — they resolve against the paragraph's base
/// direction at layout time; `Center` and `Justify` are absolute. Serializes
/// as the bare variant string (`"Start"`, `"Center"`, …).
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub enum Alignment {
    Start,
    End,
    Center,
    Justify,
}

/// Unicode script, reported when a glyph needs a font the engine lacks.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
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
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
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

/// Issue #99 — a crash-loop renderer downgrade. When the engine worker
/// traps repeatedly while painting with Vello, the shell stops re-probing
/// the GPU on recovery and boots the next worker generation on Canvas2D.
/// The worker hands this record to `Command::Recover`; the engine echoes
/// it on `Event::Recovered` so the shell (Dev HUD) and telemetry (the
/// `Crash` sample) learn why the session no longer runs on Vello.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct RendererDowngrade {
    /// Backend the trapping generations painted with (`"vello"`).
    pub from: String,
    /// Backend the recovered generation was forced onto (`"canvas2d"`).
    pub to: String,
    pub reason: RendererDowngradeReason,
    /// Consecutive traps on `from` that triggered the downgrade.
    pub consecutive_traps: u32,
}

/// Issue #99 — why a [`RendererDowngrade`] happened. One arm today; the
/// enum keeps the wire shape open for e.g. a lost-device downgrade.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RendererDowngradeReason {
    /// N consecutive worker traps on the same GPU backend — re-probing it
    /// on every recovery would crash-loop.
    CrashLoop,
}
