//! `Event` — messages the engine emits back to the TypeScript client.

use serde::{Deserialize, Serialize};
use tsify_next::Tsify;

use crate::common::{Direction, DocFormat, LogicalPos, LogicalRange, Rect, Script, TextAttrs};

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
    },

    /* Selection */
    SelectionChanged {
        range: LogicalRange,
        caret: Rect,
        direction: Direction,
        rects: Vec<Rect>,
        attrs_at_caret: TextAttrs,
        /// Undo/redo availability — every interactive edit emits this event,
        /// so the toolbar stays reactive without polling (Phase 4 §11).
        can_undo: bool,
        can_redo: bool,
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
    /// Full accessibility snapshot — emitted after every document mutation
    /// (PHASE_4_HEADLESS_UI.md §10). Fine-grained deltas are deferred; see
    /// BACKLOG.md.
    AccessibilityTreeChanged {
        tree: A11yTree,
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
}

/// A full accessibility snapshot of the document — the structure mirrored
/// into the screen-reader shadow DOM (PHASE_4_HEADLESS_UI.md §10).
#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
pub struct A11yTree {
    pub paragraphs: Vec<A11yParagraph>,
}

/// One paragraph in the accessibility tree — a `<p>` in the shadow DOM.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
pub struct A11yParagraph {
    pub id: u32,
    pub direction: Direction,
    pub runs: Vec<A11yRun>,
}

/// One styled text run within an accessibility paragraph — a `<span>` in the
/// shadow DOM, so screen readers can announce formatting boundaries.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
pub struct A11yRun {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}
