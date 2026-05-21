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
    AccessibilityTreeChanged {
        delta: A11yDelta,
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

/// One node in an accessibility-tree delta.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
pub struct A11yNode {
    pub id: u32,
    pub role: String,
    pub label: String,
    pub bounds: Rect,
}

/// Incremental update to the accessibility tree.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
pub struct A11yDelta {
    pub updated: Vec<A11yNode>,
    pub removed: Vec<u32>,
}
