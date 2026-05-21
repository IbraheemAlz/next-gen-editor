//! `Command` — messages the TypeScript client sends to the engine.

use serde::{Deserialize, Serialize};
use tsify_next::Tsify;

use crate::common::{
    Color, DocFormat, LogicalPos, LogicalRange, Point, Rect, UnderlineStyle, VerticalScript,
};

/// A command issued to the engine. Serialized internally-tagged
/// (`{ "type": "INSERT_TEXT", ... }`).
///
/// `tsify-next` renders `Option<T>` as `T | undefined`; TS callers must pass
/// `undefined`, never `null`.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Command {
    // ===================================================================
    // Phase 1 PoC commands.
    // TODO: Deprecate in Phase 3 — superseded by the §4 schema below once the
    // RequestPaint render pipeline lands. Kept now so the visual-diff goldens
    // and test harnesses stay 100% green.
    // ===================================================================
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

    /// Shape `text` with `font_id` via rustybuzz, then rasterize each glyph.
    ShapeAndRasterize {
        text: String,
        font_id: String,
        direction: String,
        px_size: f32,
    },

    /// Layout + paint a paragraph onto a synthesized A4 page, caching the
    /// layout config so later edits auto-repaint.
    RenderPage {
        text: String,
        font_id: String,
        base_direction: String,
        px_size: f32,
        line_height: f32,
        align: String,
        /// Device-pixel ratio. The engine lays out + paints the page scaled by
        /// this so HiDPI canvases stay crisp; `None` ⇒ 1.0 (the golden suite).
        device_pixel_ratio: Option<f32>,
    },

    /// Insert `text` into the in-engine document. `at == None` appends.
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

    // ===================================================================
    // Phase 2 schema — PHASE_2_BRIDGE_MEMORY.md §4.
    // ===================================================================
    /* Lifecycle */
    /// Initialize the engine surface for a canvas.
    Init {
        canvas_id: u32,
        dpi: f32,
        locale: String,
        capabilities: ClientCapabilities,
    },
    /// Restore engine state from a snapshot plus a tail of replay commands.
    Recover {
        #[serde(with = "serde_bytes")]
        #[tsify(type = "Uint8Array")]
        snapshot: Vec<u8>,
        log_tail: Vec<Command>,
    },
    /// Tear down the engine and release resources.
    Dispose,
    /// Animation/heartbeat tick.
    Tick {
        now_ms: f64,
    },

    /* Document I/O */
    OpenDocument {
        #[serde(with = "serde_bytes")]
        #[tsify(type = "Uint8Array")]
        bytes: Vec<u8>,
        format: DocFormat,
        name: Option<String>,
    },
    SaveDocument {
        format: DocFormat,
    },
    ExportPdf {
        conformance: PdfConformance,
    },
    CloseDocument,

    /* Editing */
    DeleteRange {
        range: LogicalRange,
    },
    ReplaceRange {
        range: LogicalRange,
        text: String,
    },
    ApplyFormatting {
        range: LogicalRange,
        attrs: TextAttrsPatch,
    },
    SplitParagraph {
        at: LogicalPos,
    },
    MergeParagraph {
        left: ParagraphId,
        right: ParagraphId,
    },
    InsertImage {
        at: LogicalPos,
        image: ImageBlob,
        fit: ImageFit,
    },

    /* Selection */
    SetSelection {
        range: LogicalRange,
        caret: LogicalPos,
    },
    ExtendSelection {
        to: LogicalPos,
        modifier: SelectionModifier,
    },
    SelectAll,

    /* IME */
    BeginComposition {
        at: LogicalPos,
    },
    UpdateComposition {
        text: String,
        target_range: Option<LogicalRange>,
    },
    EndComposition {
        commit: bool,
    },

    /* View */
    SetViewport {
        rect: Rect,
    },
    SetZoom {
        scale: f32,
    },
    RequestPaint {
        viewport: Rect,
        dirty: Option<Rect>,
    },

    /* Fonts / resources */
    UnloadFont {
        id: String,
    },

    /* Telemetry */
    RequestStats,

    // ===================================================================
    // Phase 4 schema — PHASE_4_HEADLESS_UI.md §7.
    // Additive: the frozen §4 schema carries selection commands keyed by
    // `LogicalPos` but no pixel→logical path. These supply it.
    // ===================================================================
    /// Hit-test a canvas pixel to a logical document position. The engine
    /// replies with [`crate::Event::HitResult`]; selection is not mutated.
    HitTest {
        at: Point,
    },

    /// Select the word under a canvas pixel (double-click). The engine
    /// updates the selection and replies with `Event::SelectionChanged`.
    SelectWordAt {
        at: Point,
    },

    /// Delete relative to the caret (Backspace / Delete). If the selection
    /// is non-empty it is deleted; otherwise one grapheme — or one word when
    /// `by_word` — is removed in the `forward` direction. The frozen
    /// `DeleteRange` needs explicit positions a document-blind UI cannot
    /// compute for a collapsed caret; this supplies the caret-relative path.
    DeleteAtCaret {
        forward: bool,
        by_word: bool,
    },

    /// Request a full accessibility snapshot; the engine replies with
    /// `Event::AccessibilityTreeChanged`. The worker issues this after every
    /// document mutation to keep the screen-reader shadow tree synced (§10).
    RequestAccessibilityTree,

    /// Snapshot the current selection for the clipboard — the engine replies
    /// with `Event::ClipboardPayload` (PHASE_4_HEADLESS_UI.md §12).
    GetSelectionAsClipboard,

    /// Paste plain text at the caret, replacing any non-empty selection.
    PastePlain {
        text: String,
    },
}

/// Browser/runtime capabilities advertised to the engine at `Init`.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
pub struct ClientCapabilities {
    pub shared_array_buffer: bool,
    pub offscreen_canvas: bool,
    pub simd: bool,
    pub device_pixel_ratio: f32,
}

/// Target PDF/A (or PDF/X) conformance level for `ExportPdf`.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
pub enum PdfConformance {
    A1b,
    A2u,
    X3,
}

/// A sparse patch of inline text attributes — `None` fields are left
/// unchanged. The resolved counterpart is [`crate::TextAttrs`].
#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
pub struct TextAttrsPatch {
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<UnderlineStyle>,
    pub strike: Option<bool>,
    pub font_family: Option<String>,
    pub font_size: Option<f32>,
    pub color: Option<Color>,
    pub bg_color: Option<Color>,
    pub script: Option<VerticalScript>,
    pub language: Option<String>,
}

/// Stable identifier for a paragraph in the document model.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParagraphId {
    pub id: u32,
}

/// How an inserted image is sized relative to the content area.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
pub enum ImageFit {
    Original,
    FitWidth,
    FitPage,
}

/// An encoded image payload plus its intrinsic dimensions.
#[derive(Serialize, Deserialize, Tsify, Clone, Debug)]
pub struct ImageBlob {
    #[serde(with = "serde_bytes")]
    #[tsify(type = "Uint8Array")]
    pub bytes: Vec<u8>,
    pub mime: String,
    pub width: u32,
    pub height: u32,
}

/// Keyboard modifier accompanying a selection-extend gesture.
#[derive(Serialize, Deserialize, Tsify, Clone, Copy, Debug)]
pub enum SelectionModifier {
    None,
    Shift,
    Alt,
    ShiftAlt,
}
