//! `Event` — messages the engine emits back to the TypeScript client.

use serde::{Deserialize, Serialize};
use tsify_next::Tsify;

use crate::command::{BridgeCellBorders, BridgeTabStop, PageOrientation};
use crate::common::{
    Alignment, Color, Direction, DocFormat, LogicalPos, LogicalRange, Rect, Script, SelectionKind,
    TextAttrs,
};

/// An event emitted by the engine. Serialized internally-tagged
/// (`{ "type": "PAINTED", ... }`).
#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Event {
    // ===================================================================
    // Phase 1 PoC events.
    // TODO: Deprecate in Phase 3 — superseded by the §5 schema below. Kept
    // now so the visual-diff goldens and test harnesses stay 100% green.
    // ===================================================================
    Pong,
    Log {
        message: String,
    },
    FontLoaded {
        id: String,
        metrics: FontMetrics,
    },
    GlyphPainted {
        font_id: String,
        ch: String,
        advance_width: f32,
        ascent: f32,
        glyph_width: u32,
        glyph_height: u32,
    },
    ShapedAndPainted {
        font_id: String,
        text: String,
        direction: String,
        glyph_count: u32,
        total_advance: f32,
        ascent: f32,
        glyph_ids: Vec<u32>,
    },
    PageRendered {
        page_width: f32,
        page_height: f32,
        line_count: u32,
        glyph_count: u32,
    },
    TextInserted {
        paragraph_count: u32,
        can_undo: bool,
        can_redo: bool,
        undo_depth: u32,
    },
    UndoStateChanged {
        can_undo: bool,
        can_redo: bool,
        undo_depth: u32,
    },
    DocumentLoaded {
        paragraph_count: u32,
    },
    DocumentSaved {
        #[serde(with = "serde_bytes")]
        #[tsify(type = "Uint8Array")]
        bytes: Vec<u8>,
        size: u32,
    },
    Error {
        message: String,
    },

    // ===================================================================
    // Phase 2 schema — PHASE_2_BRIDGE_MEMORY.md §5.
    // ===================================================================
    /* Lifecycle */
    Ready {
        version: String,
        capabilities: EngineCapabilities,
    },
    Recovered {
        applied_commands: u32,
    },

    /* Document */
    DocumentClosed,
    PdfExported {
        #[serde(with = "serde_bytes")]
        #[tsify(type = "Uint8Array")]
        bytes: Vec<u8>,
        pages: u32,
    },

    /* Rendering */
    Painted {
        dirty: Rect,
        version: u64,
        paint_ms: f32,
        /// Phase 6b — total height in layout pixels of the pages **already
        /// laid out**, including inter-page gaps. The TS shell sizes its
        /// canvas backing store to `estimated_document_height` (≥ this),
        /// but uses this value for `last_emitted_y` so a hit-test below
        /// it triggers a layout-extend request. `0.0` for harness paths
        /// that bypass pagination (Phase-1 `?test=` cases).
        document_height: f32,
        /// Phase 6b — number of paginated pages the renderer drew so far.
        page_count: u32,
        /// Audit gap C.H1 — pagination state on this paint. `true` when
        /// every body block was consumed; `false` when the paginator
        /// stopped early under a `viewport_budget` so the scrollbar
        /// reflects the *estimated* document height, not the final one.
        /// The TS shell uses this to know whether more pages may
        /// materialize on scroll / `ExpandLayout`.
        is_full_layout: bool,
        /// Audit gap C.H1 — best-guess total document height in layout
        /// pixels at the current scale, including the inter-page gaps
        /// and an `AVG_PARAGRAPH_HEIGHT_PT` × `remaining_blocks` reserve
        /// for blocks that have not yet been laid out. `== document_height`
        /// once `is_full_layout` is `true`. Drives the scrollbar so it
        /// never jumps as background layout fills in the tail.
        estimated_document_height: f32,
    },

    /* Selection */
    SelectionChanged {
        range: LogicalRange,
        caret: Rect,
        direction: Direction,
        rects: Vec<Rect>,
        attrs_at_caret: TextAttrs,
        /// Effective alignment of the caret's paragraph — its own override, or
        /// the document default. With a pending (sticky) style armed this is
        /// unaffected; `attrs_at_caret` carries the pending overlay instead.
        /// Drives the toolbar's alignment picker (Backlog #9, #11).
        paragraph_alignment: Alignment,
        /// Undo/redo availability — every interactive edit emits this event,
        /// so the toolbar stays reactive without polling (Phase 4 §11).
        can_undo: bool,
        can_redo: bool,
        /// Phase 5 PR 4 — selection flavour. `Linear` for classic text
        /// spans, `TableCells` when the caret is inside a table and the
        /// drag covered multiple cells. UI overlays inspect this to
        /// switch between text-span and cell-rect highlights.
        selection_kind: SelectionKind,
        /// Phase 9b — per-flag "mixed across the selection" bitmap. A flag
        /// is `true` when the underlying spans in `[range.start, range.end)`
        /// don't all agree (some bold + some not). The toolbar renders an
        /// **indeterminate** state for those buttons so a user can't
        /// mis-read the start position's value as the whole selection's.
        /// Always `false` on a collapsed caret (nothing to disagree over).
        attrs_mixed: AttrsMixed,
        /// Phase 9c — paragraph base direction (`<w:bidi>`) reflected as
        /// a tri-state for the toolbar LTR/RTL buttons. `Some(Ltr)` /
        /// `Some(Rtl)` when every paragraph in the range agrees on the
        /// EFFECTIVE direction (explicit `props.direction` if set, else
        /// first-strong inference, else the layout-config default).
        /// `None` when paragraphs disagree → toolbar renders both
        /// direction buttons as INDETERMINATE.
        paragraph_direction: Option<Direction>,
        /// Sprint 10 — page geometry of the section the caret sits in.
        /// Drives `PageSetupDialog` prefill (margin / orientation /
        /// columns). `None` when no section data is available (e.g. a
        /// fresh empty document still on the implicit-default A4).
        section_geometry: Option<BridgeSectionGeometry>,
        /// Sprint 10 — cell shading + per-edge borders for the active
        /// cell when the caret is inside a table. `None` outside any
        /// table — `CellPropertiesDialog` falls back to engine defaults.
        cell_properties: Option<BridgeCellProperties>,
        /// Sprint 11 (#13) — `<w:pPr><w:tabs>` custom tab stops of the
        /// paragraph under the caret. Empty when the paragraph
        /// inherits the default 0.5-inch grid. Drives the Ruler's
        /// tab-stop markers + their drag-edit handles.
        tab_stops: Vec<BridgeTabStop>,
        /// Sprint 14 (#14) — current engine track-changes recording
        /// state. The UI binds `ReviewControls`'s Track-Changes
        /// toggle to this field instead of carrying local Solid
        /// state, so a `ToggleTrackChanges` issued by another tab,
        /// macro, or undo path stays in sync.
        is_tracking_changes: bool,
    },

    /* IME */
    CompositionUpdated {
        at: LogicalPos,
        text: String,
        target_range: Option<LogicalRange>,
    },

    /* Editing feedback */
    FormattingChanged {
        range: LogicalRange,
        attrs: TextAttrs,
    },

    /* Accessibility */
    /// Fine-grained accessibility patches — broadcast after every document
    /// mutation (PHASE_4_HEADLESS_UI.md §10, Backlog #10). The first delta from
    /// any engine instance (boot or post-recovery) is a single `Replace`; every
    /// later delta carries only the paragraphs that actually changed.
    AccessibilityTreeDelta {
        patches: Vec<A11yPatch>,
    },

    /* Telemetry */
    Stats(EngineStats),

    /* Resource events */
    FontMissing {
        script: Script,
        requested: String,
    },

    /* Errors */
    /// Fatal — the worker is about to die; the TS client should recover.
    Trap {
        stack: String,
    },

    // ===================================================================
    // Phase 4 schema — PHASE_4_HEADLESS_UI.md §7.
    // ===================================================================
    /// Reply to [`crate::Command::HitTest`] — the logical position under
    /// the hit-tested pixel.
    HitResult {
        pos: LogicalPos,
    },

    /// Reply to `Command::GetSelectionAsClipboard` — the selection as
    /// clipboard MIME payloads. `html` / `docx_fragment` stay empty until
    /// rich clipboard generation lands (see BACKLOG.md).
    ClipboardPayload {
        plain: String,
        html: String,
        #[serde(with = "serde_bytes")]
        #[tsify(type = "Uint8Array")]
        docx_fragment: Vec<u8>,
    },

    /// Sprint 10 — broadcast a short, human-readable message describing
    /// a user-visible mutation ("Aligned center", "Page break inserted",
    /// "Comment added"). The TS shell pipes these into an `aria-live`
    /// region so screen readers narrate engine actions; the engine
    /// owns the wording + priority so the UI never has to compute
    /// ARIA semantics from event payloads.
    Announcement {
        priority: AnnouncementPriority,
        message: String,
    },
}

/// Sprint 10 — `aria-live` priority for an `Event::Announcement`. The
/// TS shell routes `Polite` into an `aria-live="polite"` region (the
/// reader waits for the user to stop typing) and `Assertive` into an
/// `aria-live="assertive"` region (the reader interrupts immediately).
///
/// Engine-side mapping: every user-visible mutation emits `Polite`;
/// only error / blocked-action notifications emit `Assertive`.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnnouncementPriority {
    Polite,
    Assertive,
}

/// Sprint 10 — lightweight wire shape for the page geometry of the
/// section under the caret. Units are layout pt (1/72 in) at scale=1,
/// matching `engine::PageGeometry` exactly; the UI converts to its
/// preferred unit (in / mm / pt) for display.
///
/// "Lightweight" matters: `SelectionChanged` fires on every keystroke
/// and pointer move. Eight `f32`s + one `u32` + one enum is a single
/// 40-ish byte struct, allocation-free across the wasm bridge.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
pub struct BridgeSectionGeometry {
    pub width_pt: f32,
    pub height_pt: f32,
    pub margin_top_pt: f32,
    pub margin_right_pt: f32,
    pub margin_bottom_pt: f32,
    pub margin_left_pt: f32,
    pub orientation: PageOrientation,
    /// `<w:cols w:num>` — 1 for the default single-column body.
    pub columns: u32,
    /// `<w:cols w:space>` — inter-column gutter in pt.
    pub column_gutter_pt: f32,
}

/// Sprint 10 — wire shape for the active cell's shading + per-edge
/// borders. Mirror of `engine::CellProperties`'s
/// CellPropertiesDialog-relevant subset; `grid_span`, `v_merge`,
/// `cell_margins` are deliberately omitted (the dialog does not edit
/// them).
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, Default)]
pub struct BridgeCellProperties {
    pub shading: Option<Color>,
    pub borders: BridgeCellBorders,
}

/// Per-flag "this attribute is mixed across the selection" bitmap that
/// rides on `Event::SelectionChanged`. The toolbar reads it to switch
/// each button between OFF / ON / INDETERMINATE — without it, a
/// selection that crosses a bold/non-bold boundary would mis-render
/// the start position's bold state as the whole selection's.
///
/// Each field is `true` iff the underlying spans in the selection
/// don't unanimously agree on that flag. A collapsed caret reports all
/// `false` (there's no range to disagree over).
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, Default)]
pub struct AttrsMixed {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
}

/// Font vertical metrics, scaled to a requested pixel size.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
pub struct FontMetrics {
    pub units_per_em: u16,
    pub ascent: f32,
    pub descent: f32,
    pub leading: f32,
    pub cap_height: f32,
    pub x_height: f32,
}

/// Engine-side capabilities reported in `Event::Ready`.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
pub struct EngineCapabilities {
    pub simd: bool,
    pub shared_array_buffer: bool,
    pub max_document_pages: u32,
    pub formats: Vec<DocFormat>,
}

/// Memory + performance counters, re-emitted on `Command::RequestStats`.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
pub struct EngineStats {
    pub wasm_heap_bytes: u32,
    pub document_tree_bytes: u32,
    pub glyph_cache_entries: u32,
    pub undo_stack_depth: u32,
    pub fonts_resident: u32,
    pub last_paint_ms: f32,
    pub last_command_ms: f32,
    /// Sprint 8 (UI Edition) — total paragraph count across the
    /// active document, including paragraphs nested in tables.
    pub paragraph_count: u32,
    /// Sprint 8 (UI Edition) — whitespace-split word count. Cheap
    /// and Latin-script-leaning; UAX-#29 segmentation is queued
    /// as a follow-up.
    pub word_count: u32,
    /// Sprint 8 (UI Edition) — Unicode scalar character count
    /// (matches Word's "Characters (with spaces)").
    pub character_count: u32,
}

/// A full accessibility snapshot of the document — the structure mirrored into
/// the screen-reader DOM (PHASE_4_HEADLESS_UI.md §10). Carried by the
/// `A11yPatch::Replace` reset patch.
///
/// Phase 5 PR 3b: widened from a flat `paragraphs` list to a node list so a
/// table can appear inline with paragraphs. Cells nest further `nodes` —
/// recursive in shape; PR 3b only emits paragraphs inside cells.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, PartialEq, Eq)]
pub struct A11yTree {
    pub nodes: Vec<A11yNode>,
}

/// A top-level (or nested) accessibility node — a paragraph or a table.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum A11yNode {
    Paragraph(A11yParagraph),
    Table(A11yTable),
}

/// One paragraph in the accessibility tree — a `<p>` in the mirror DOM.
///
/// Carries no id: the engine has no stable paragraph identity, so `diff_a11y`
/// matches paragraphs by content and the patches address them by position.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, PartialEq, Eq)]
pub struct A11yParagraph {
    pub direction: Direction,
    pub runs: Vec<A11yRun>,
}

/// One styled text run within an accessibility paragraph — a `<span>` in the
/// mirror DOM, so screen readers can announce formatting boundaries.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, PartialEq, Eq)]
pub struct A11yRun {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

/// Table node — mirrored as `<table role="table">` in the DOM. PR 3b
/// keeps the block-level position (top-level index) as the addressing
/// scheme; nested-table support lands with the `BlockPath` migration.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, PartialEq, Eq)]
pub struct A11yTable {
    /// Top-level block index of this table — pinned to its position in
    /// the document `Vec<Block>` so the TS shell can target it by
    /// `BlockPath::top(block_index)` for table mutations.
    pub block_index: u32,
    pub rows: Vec<A11yRow>,
}

/// Row in a table — `<tr role="row">`.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, PartialEq, Eq)]
pub struct A11yRow {
    pub cells: Vec<A11yCell>,
}

/// Cell in a row — `<td role="gridcell">`. `row_span` / `col_span` are
/// resolved at build time from `vMerge` / `gridSpan` so the DOM can stamp
/// `aria-rowspan` / `aria-colspan` directly. PR 3b emits paragraphs
/// only; the recursive `nodes` slot allows nested tables (Phase 5b).
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, PartialEq, Eq)]
pub struct A11yCell {
    pub row: u32,
    pub col: u32,
    pub row_span: u32,
    pub col_span: u32,
    pub nodes: Vec<A11yNode>,
}

/// One incremental patch to the mirrored accessibility tree (Backlog #10).
///
/// Patches in a delta apply in order against the current node list:
/// every `Update` first, then either the `Insert`s (ascending index) or the
/// `Remove`s — a prefix/suffix diff only ever grows or shrinks one contiguous
/// region, so a single delta never mixes inserts and removes.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum A11yPatch {
    /// Discard the whole mirror and rebuild it — the first delta from any
    /// engine instance (boot or post-recovery), where there is no prior tree
    /// to diff against.
    Replace { tree: A11yTree },
    /// Replace the node at `index` in place — its content or structure
    /// changed (the common case: a keystroke lands in one paragraph).
    Update { index: u32, node: A11yNode },
    /// Insert a new node at `index`, shifting later nodes down.
    Insert { index: u32, node: A11yNode },
    /// Remove the node at `index`, shifting later nodes up.
    Remove { index: u32 },
}
