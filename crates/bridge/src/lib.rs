//! `bridge` — RPC command/event types shared between the WASM engine and TS.
//!
//! Phase 1 weeks 5–9 subset. Full schema lands in Phase 2 per
//! PHASE_2_BRIDGE_MEMORY.md §4–§5.

use serde::{Deserialize, Serialize};
use tsify_next::Tsify;

#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Command {
    /// Liveness probe; engine replies with `Event::Pong`.
    Ping,

    /// Parse and register a TTF/OTF font buffer under `id`.
    LoadFont {
        id: String,
        #[serde(with = "serde_bytes")]
        #[tsify(type = "Uint8Array")]
        bytes: Vec<u8>,
    },

    /// Rasterize and paint a single glyph by character (no shaping).
    RasterizeGlyph {
        font_id: String,
        ch: String,
        px_size: f32,
    },

    /// Shape `text` with `font_id` via rustybuzz, then rasterize and paint
    /// each resulting glyph sequentially. `direction` is `"LTR"` or `"RTL"`.
    ShapeAndRasterize {
        text: String,
        font_id: String,
        direction: String,
        px_size: f32,
    },

    /// Layout + paint a paragraph onto a synthesized A4 page. Runs the full
    /// BiDi → shape → line-break → justify pipeline (`text-pipeline` + `layout`).
    /// Also resets the in-engine document to a single paragraph of `text` and
    /// caches the layout config so future edits trigger a repaint with the
    /// same parameters.
    RenderPage {
        text: String,
        font_id: String,
        base_direction: String,
        px_size: f32,
        line_height: f32,
        align: String,
    },

    /// Insert `text` into the in-engine document. `at == None` appends at
    /// end-of-document. Pushes a new undo snapshot and (if a layout config
    /// is cached from a prior RenderPage) triggers a repaint.
    InsertText {
        at: Option<LogicalPos>,
        text: String,
    },

    /// Pop one snapshot off the undo stack; repaints.
    Undo,
    /// Re-apply one snapshot from the redo branch; repaints.
    Redo,

    /// Replace the in-engine document with one parsed from a `.docx` blob.
    LoadDocx {
        #[serde(with = "serde_bytes")]
        #[tsify(type = "Uint8Array")]
        bytes: Vec<u8>,
    },

    /// Serialize the in-engine document to a freshly-packed `.docx` blob.
    SaveDocx,
}

#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogicalPos {
    pub para: u32,
    pub offset: u32,
}

#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Event {
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
}

#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
pub struct FontMetrics {
    pub units_per_em: u16,
    pub ascent: f32,
    pub descent: f32,
    pub leading: f32,
    pub cap_height: f32,
    pub x_height: f32,
}
