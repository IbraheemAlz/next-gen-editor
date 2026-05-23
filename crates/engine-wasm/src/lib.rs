//! `engine-wasm` — `#[wasm_bindgen]` surface for the engine.
//!
//! Phase 1 weeks 15–24: document model (engine crate, `im::Vector`-backed) +
//! undo/redo + `.docx` load/save + InsertText that triggers an automatic
//! repaint when a layout config was cached by a prior `RenderPage`.

use bridge::{
    A11yCell, A11yNode, A11yParagraph, A11yPatch, A11yRow, A11yRun, A11yTable, A11yTree,
    Alignment as BridgeAlignment, BlockPath as BridgeBlockPath, Color, Command, Direction,
    EngineStats, Event, FontMetrics as BridgeMetrics, LogicalPos as BridgeLogicalPos,
    LogicalRange as BridgeLogicalRange, MoveDirection, PathStep as BridgePathStep, PdfConformance,
    Point as BridgePoint, Rect as BridgeRect, SelectionKind, TextAttrs, TextAttrsPatch,
    UnderlineStyle, VerticalScript,
};
use engine::{
    Alignment as EngineAlignment, BlockPath as EngineBlockPath, DocumentTree,
    FontFamily as EngineFontFamily, LogicalPos as EnginePos, PathStep as EnginePathStep, SpanStyle,
    UndoStack,
};
use format_docx::writer::build_minimal_docx;
use kurbo::Rect;
use layout::{
    A4Page, LayoutBlock, LineBox, PageBox, ParagraphBox, ParagraphConfig, Point, Size, StyleSpan,
    TableBox, TableCellBox, TableRowBox, layout_paragraph,
    paginate::{PageGeometry as PaginatorGeometry, Paginator},
};
use lru::LruCache;
use render::atlas::GlyphAtlas;
use render::canvas2d_backend::render_canvas2d;
use render::dirty::DirtyTracker;
use render::vello_backend::VelloRenderer;
use std::cell::RefCell;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;
use text_pipeline::{
    Alignment, FontStack, LoadedFont, ShapingDirection, first_strong_direction, shape_text,
};
use wasm_bindgen::prelude::*;
use web_sys::OffscreenCanvasRenderingContext2d;

#[wasm_bindgen(start)]
pub fn boot() {
    console_error_panic_hook::set_once();
}

#[derive(Clone)]
struct RenderConfig {
    font_id: String,
    base_direction: ShapingDirection,
    px_size: f32,
    line_height: f32,
    alignment: Alignment,
    /// Device-pixel ratio. Layout + paint are scaled by this; `px_size` and
    /// `line_height` stay logical (the toolbar + document model use them raw).
    scale: f32,
}

/// Width of the rendered caret, in canvas device pixels.
const CARET_WIDTH: f32 = 2.0;

/// The current selection: a fixed `anchor` and a moving `caret` end. The
/// selected range is the two ends ordered into document order.
///
/// `ideal_x` is the device-pixel X column the caret returns to on vertical
/// `MoveCaret` motion (Backlog #14). `Some` only while a Up/Down walk is in
/// progress — every horizontal move, click, selection-set, or edit drops it
/// by constructing a new `SelectionState` with `ideal_x: None`.
#[derive(Clone)]
struct SelectionState {
    anchor: BridgeLogicalPos,
    caret: BridgeLogicalPos,
    ideal_x: Option<f32>,
    /// Phase 5 PR 4 — selection flavour. `Linear` is the default
    /// text-span selection; `TableCells` covers cell-rectangular drags
    /// inside a table. Set by the engine when both endpoints share a
    /// table ancestor with different cell positions.
    kind: SelectionKind,
}

/// An in-progress IME composition (PHASE_4_HEADLESS_UI.md §6). Tracked
/// between `BeginComposition` and `EndComposition`; the latest `text` is
/// committed on a committing end. No on-canvas preview — see BACKLOG.md.
#[derive(Clone)]
struct CompositionState {
    at: BridgeLogicalPos,
    text: String,
}

/// A candidate caret position on a line — an absolute x (canvas device px)
/// paired with the source byte offset a caret there maps to.
#[derive(Clone, Copy)]
struct CaretSlot {
    x: f32,
    byte: u32,
}

/// One laid-out line flattened for pointer hit-testing and caret/selection
/// geometry. All coordinates are absolute page points (= canvas device px).
struct LineGeom {
    /// `BlockPath` of the paragraph this line belongs to. PR 4: cells
    /// flatten into their own paths, so a cell-paragraph's line carries
    /// the full descent path (e.g. `[Block(2), Cell{r,c}, Block(0)]`).
    path: BridgeBlockPath,
    /// Line's logical leading edge in absolute coords — where the
    /// caret lands when the line has no slots (an empty paragraph).
    /// RTL-aware: for an empty RTL cell paragraph this sits at the
    /// cell's right edge.
    start_x: f32,
    /// Hit-target rectangle's left edge in absolute coords. For body
    /// paragraphs this is the page content area's left; for cell
    /// paragraphs it is the containing cell's left edge. Two cells
    /// in the same row share `y_top` and must disambiguate by `x` —
    /// this rectangle is how.
    hit_left: f32,
    /// Hit-target rectangle's width — page content width for body
    /// paragraphs, cell content width for cell paragraphs.
    hit_width: f32,
    y_top: f32,
    height: f32,
    start_byte: u32,
    end_byte: u32,
    /// Caret slots in visual emission order — searched by nearest x or byte.
    /// The flat concatenation of every run's slots.
    slots: Vec<CaretSlot>,
    /// Per-`VisualRun` geometry, in visual order. Selection rectangles clip
    /// the selected byte range against each run (Backlog #7).
    runs: Vec<RunGeom>,
}

/// One `VisualRun` flattened — its source byte span plus its caret slots. A
/// run is unidirectional, so a selection clipped to its byte span is visually
/// contiguous and yields exactly one rectangle.
struct RunGeom {
    src_start: u32,
    src_end: u32,
    slots: Vec<CaretSlot>,
}

#[wasm_bindgen]
pub struct Engine {
    ctx: Option<OffscreenCanvasRenderingContext2d>,
    fonts: HashMap<String, Arc<LoadedFont>>,
    undo: UndoStack,
    layout_cfg: Option<RenderConfig>,
    atlas: GlyphAtlas,
    vello: Option<VelloRenderer>,
    dirty: DirtyTracker,
    selection: Option<SelectionState>,
    composition: Option<CompositionState>,
    /// Sticky / pending formatting (Backlog #11). Armed when the toolbar
    /// dispatches `ApplyFormatting` over a collapsed caret; the next
    /// interactive `InsertText` overlays it onto the typed text. It persists
    /// across keystrokes and is cleared only when the caret moves.
    pending_format: Option<SpanStyle>,
    /// Incremental-relayout cache (Backlog #13): memoizes `layout_paragraph`
    /// keyed by a content + render-config hash. An edit only re-shapes the
    /// changed paragraph; the rest are clones shifted by a Y delta. `RefCell`
    /// because `build_page` populates it behind a `&self` borrow.
    layout_cache: RefCell<LruCache<u64, ParagraphBox>>,
    /// Last accessibility tree broadcast to the UI (Backlog #10).
    /// `build_a11y_delta` diffs the freshly built tree against this so a
    /// keystroke emits only the changed paragraph. `None` until the first
    /// delta — that one is a full `Replace`, so a fresh engine after crash
    /// recovery hands the mirror a clean rebuild.
    a11y_cache: Option<Vec<A11yNode>>,
}

/// Capacity of the paragraph layout cache — comfortably covers a 50-page
/// document (1000 paragraphs) plus edit churn.
const LAYOUT_CACHE_CAP: usize = 4096;

/// A fresh, empty paragraph layout cache.
fn new_layout_cache() -> RefCell<LruCache<u64, ParagraphBox>> {
    RefCell::new(LruCache::new(
        NonZeroUsize::new(LAYOUT_CACHE_CAP).expect("LAYOUT_CACHE_CAP is non-zero"),
    ))
}

/// Assemble an `Engine` around a chosen renderer surface. Exactly one of `ctx`
/// (Canvas2D) and `vello` (WebGPU) is `Some` — the backend is picked once at
/// INIT, since an `OffscreenCanvas` is one-context-for-life.
fn assemble_engine(
    ctx: Option<OffscreenCanvasRenderingContext2d>,
    vello: Option<VelloRenderer>,
) -> Engine {
    Engine {
        ctx,
        fonts: HashMap::new(),
        undo: UndoStack::new(DocumentTree::new(), 100),
        layout_cfg: None,
        atlas: GlyphAtlas::new(),
        vello,
        dirty: DirtyTracker::new(),
        selection: None,
        composition: None,
        pending_format: None,
        layout_cache: new_layout_cache(),
        a11y_cache: None,
    }
}

#[wasm_bindgen]
impl Engine {
    /// Construct a Canvas2D engine — the always-available CPU renderer. The
    /// worker uses this directly, or falls back to it when WebGPU is absent.
    #[wasm_bindgen(constructor)]
    pub fn new(canvas: web_sys::OffscreenCanvas) -> Result<Engine, JsValue> {
        let ctx_obj = canvas
            .get_context("2d")?
            .ok_or_else(|| JsValue::from_str("OffscreenCanvas 2d context unavailable"))?;
        let ctx: OffscreenCanvasRenderingContext2d = ctx_obj.dyn_into()?;
        ctx.set_fill_style_str("#ffffff");
        ctx.fill_rect(0.0, 0.0, canvas.width() as f64, canvas.height() as f64);
        Ok(assemble_engine(Some(ctx), None))
    }

    pub async fn dispatch(&mut self, cmd: JsValue) -> Result<JsValue, JsValue> {
        let cmd: Command = serde_wasm_bindgen::from_value(cmd)
            .map_err(|e| JsValue::from_str(&format!("decode command: {e}")))?;
        let evt: Event = self.apply(cmd).await;
        serde_wasm_bindgen::to_value(&evt)
            .map_err(|e| JsValue::from_str(&format!("encode event: {e}")))
    }
}

/// Vello (WebGPU) activation — wasm-only, since WebGPU surface creation is.
///
/// An `OffscreenCanvas` is one-context-for-life: `Engine::new` claims a `2d`
/// context, so the Vello path needs its own constructor that hands the still-
/// uncontexted canvas straight to `wgpu`. The worker chooses between the two
/// at INIT, after [`detect_backend`].
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
impl Engine {
    /// Construct an engine that renders through Vello on `canvas`. `canvas`
    /// must not have had a context taken — `wgpu` claims a `webgpu` one.
    pub async fn with_vello(canvas: web_sys::OffscreenCanvas) -> Result<Engine, JsValue> {
        let vr = VelloRenderer::new(canvas)
            .await
            .map_err(|e| JsValue::from_str(&e))?;
        Ok(assemble_engine(None, Some(vr)))
    }
}

/// Detect the best renderer from inside the Web Worker — `"vello"` when a
/// WebGPU device is acquired, `"canvas2d"` otherwise. Canvas-free, so the
/// worker calls it *before* constructing the engine and claiming a context.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub async fn detect_backend() -> String {
    render::backend::detect_backend().await.as_str().to_string()
}

const POC_BASELINE_X: f64 = 50.0;
const POC_BASELINE_Y: f64 = 200.0;

fn to_engine_pos(p: BridgeLogicalPos) -> EnginePos {
    EnginePos {
        path: bridge_to_engine_path(p.path),
        offset: p.offset,
    }
}

fn to_bridge_pos(p: EnginePos) -> BridgeLogicalPos {
    BridgeLogicalPos {
        path: engine_to_bridge_path(p.path),
        offset: p.offset,
    }
}

/// Engine `BlockPath` → bridge `BlockPath` (mirror enums, parallel
/// shape — see `bridge_to_engine_path` in the table-command path).
fn engine_to_bridge_path(p: EngineBlockPath) -> BridgeBlockPath {
    BridgeBlockPath {
        steps: p
            .steps
            .into_iter()
            .map(|s| match s {
                EnginePathStep::Block(idx) => BridgePathStep::Block { idx },
                EnginePathStep::Cell { row, col } => BridgePathStep::Cell { row, col },
            })
            .collect(),
    }
}

/// Wire-shape constructor for the document-empty fallback used by
/// crash recovery and the empty-document caret seed.
fn bpos_top(para: u32, offset: u32) -> BridgeLogicalPos {
    BridgeLogicalPos {
        path: BridgeBlockPath::top(para),
        offset,
    }
}

/// Find the cell `(row, col)` of `path` inside `table_path`. `None`
/// when `path` does not descend into a cell of that exact table.
fn cell_of(path: &BridgeBlockPath, table_path: &BridgeBlockPath) -> Option<(u32, u32)> {
    if !table_path.is_ancestor_of(path) || path.steps.len() <= table_path.steps.len() {
        return None;
    }
    match path.steps.get(table_path.steps.len())? {
        BridgePathStep::Cell { row, col } => Some((*row, *col)),
        _ => None,
    }
}

/// Walk back from a paragraph path through `Block` / `Cell` step
/// pairs and return the path of the innermost containing table. `None`
/// when the position is not inside any table.
fn enclosing_table_path(path: &BridgeBlockPath) -> Option<BridgeBlockPath> {
    /* The path shape inside a table is `... Block(N) Cell{r,c} ...`.
    Strip the trailing `Cell{r,c} Block(N)` pair (and anything inside
    a nested cell) to find the table path. */
    let mut steps = path.steps.clone();
    while !steps.is_empty() {
        let n = steps.len();
        if n >= 2
            && matches!(steps[n - 1], BridgePathStep::Block { .. })
            && matches!(steps[n - 2], BridgePathStep::Cell { .. })
        {
            steps.truncate(n - 2);
            return Some(BridgeBlockPath { steps });
        }
        steps.pop();
    }
    None
}

/// Tab / Shift+Tab cell navigation. Inside a table the caret jumps
/// to the next (or previous) cell in row-major order, landing on the
/// first paragraph at offset 0; at the table's last cell, Tab inserts
/// a fresh row and lands at its first cell (Word default). Outside a
/// table the caret stays put.
fn cell_tab_step(
    undo: &mut UndoStack,
    caret: &BridgeLogicalPos,
    forward: bool,
) -> BridgeLogicalPos {
    let Some(table_path) = enclosing_table_path(&caret.path) else {
        return caret.clone();
    };
    let Some((r, c)) = cell_of(&caret.path, &table_path) else {
        return caret.clone();
    };
    let engine_table = bridge_to_engine_path(table_path.clone());
    let Some(table) = undo.current().table_at_path(&engine_table) else {
        return caret.clone();
    };
    let n_rows = table.rows.len() as u32;
    let n_cols = table.rows.iter().map(|r| r.cells.len()).max().unwrap_or(0) as u32;
    if n_rows == 0 || n_cols == 0 {
        return caret.clone();
    }
    let (next_r, next_c) = if forward {
        if c + 1 < n_cols {
            (r, c + 1)
        } else if r + 1 < n_rows {
            (r + 1, 0)
        } else {
            /* Last cell + forward → append a fresh row and land on its
            first cell (Word default). */
            let new_doc = undo
                .current()
                .insert_row(bridge_to_engine_path(table_path.clone()), n_rows - 1);
            undo.push(new_doc);
            (n_rows, 0)
        }
    } else if c > 0 {
        (r, c - 1)
    } else if r > 0 {
        let cols_at = undo
            .current()
            .table_at_path(&engine_table)
            .and_then(|t| t.rows.get((r - 1) as usize))
            .map(|row| row.cells.len() as u32)
            .unwrap_or(n_cols);
        (r - 1, cols_at.saturating_sub(1))
    } else {
        return caret.clone();
    };
    /* Address the first paragraph of the destination cell. */
    let mut steps = table_path.steps.clone();
    steps.push(BridgePathStep::Cell {
        row: next_r,
        col: next_c,
    });
    steps.push(BridgePathStep::Block { idx: 0 });
    BridgeLogicalPos {
        path: BridgeBlockPath { steps },
        offset: 0,
    }
}

/// Build cell-rectangular highlight rects for `TableCells` selection
/// — one rect per spanned cell, the union of every line band that
/// belongs to a paragraph inside that cell.
fn table_cell_rects(
    geom: &[LineGeom],
    table_path: &BridgeBlockPath,
    from_row: u32,
    from_col: u32,
    to_row: u32,
    to_col: u32,
) -> Vec<BridgeRect> {
    use std::collections::HashMap;
    let mut by_cell: HashMap<(u32, u32), BridgeRect> = HashMap::new();
    for line in geom {
        let Some((r, c)) = cell_of(&line.path, table_path) else {
            continue;
        };
        if r < from_row || r > to_row || c < from_col || c > to_col {
            continue;
        }
        let line_rect = BridgeRect {
            x: line.start_x,
            y: line.y_top,
            w: line
                .runs
                .iter()
                .map(|run_g| {
                    run_g.slots.iter().map(|s| s.x).fold(line.start_x, f32::max) - line.start_x
                })
                .fold(0.0_f32, f32::max),
            h: line.height,
        };
        by_cell
            .entry((r, c))
            .and_modify(|r| {
                let x0 = r.x.min(line_rect.x);
                let y0 = r.y.min(line_rect.y);
                let x1 = (r.x + r.w).max(line_rect.x + line_rect.w);
                let y1 = (r.y + r.h).max(line_rect.y + line_rect.h);
                *r = BridgeRect {
                    x: x0,
                    y: y0,
                    w: x1 - x0,
                    h: y1 - y0,
                };
            })
            .or_insert(line_rect);
    }
    by_cell.into_values().collect()
}

/// Derive the selection flavour from the two endpoints. PR 4: both
/// endpoints share an enclosing table AND descend into different
/// cells → `TableCells`; otherwise `Linear`.
fn derive_selection_kind(anchor: &BridgeLogicalPos, caret: &BridgeLogicalPos) -> SelectionKind {
    let Some(table_a) = enclosing_table_path(&anchor.path) else {
        return SelectionKind::Linear;
    };
    let Some(table_b) = enclosing_table_path(&caret.path) else {
        return SelectionKind::Linear;
    };
    if table_a != table_b {
        return SelectionKind::Linear;
    }
    let Some((ar, ac)) = cell_of(&anchor.path, &table_a) else {
        return SelectionKind::Linear;
    };
    let Some((br, bc)) = cell_of(&caret.path, &table_b) else {
        return SelectionKind::Linear;
    };
    if ar == br && ac == bc {
        return SelectionKind::Linear;
    }
    SelectionKind::TableCells {
        table_path: table_a,
        from_row: ar.min(br),
        from_col: ac.min(bc),
        to_row: ar.max(br),
        to_col: ac.max(bc),
    }
}

/// Phase 2 schema stub. The §4 command is accepted by the typed RPC surface,
/// but its engine behavior is implemented in Phase 3 behind the RequestPaint
/// pipeline. Returns a descriptive error so callers fail loudly rather than
/// silently no-op'ing.
fn phase3_stub(name: &str) -> Event {
    Event::Error {
        message: format!("{name}: accepted by the Phase 2 schema, implemented in Phase 3"),
    }
}

/// Current WASM linear-memory size in bytes (PHASE_2_BRIDGE_MEMORY.md §8.2).
/// Real on the `wasm32` artifact; `0` on native `cargo check`/`test` builds,
/// where the `memory_size` intrinsic does not exist.
fn wasm_heap_bytes() -> u32 {
    #[cfg(target_arch = "wasm32")]
    {
        let pages = core::arch::wasm32::memory_size(0) as u64;
        (pages * 65536).min(u32::MAX as u64) as u32
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        0
    }
}

/// The full A4 page rectangle (scaled by `scale`) — the dirty region an edit
/// invalidates, since an edit can reflow the whole page.
fn full_page_rect(scale: f32) -> Rect {
    let page = A4Page::a4().scaled(scale);
    Rect::new(0.0, 0.0, f64::from(page.width), f64::from(page.height))
}

/// Convert a bridge `Rect` (x, y, w, h) to a `kurbo::Rect` (x0, y0, x1, y1).
fn bridge_to_kurbo(r: BridgeRect) -> Rect {
    Rect::new(
        f64::from(r.x),
        f64::from(r.y),
        f64::from(r.x + r.w),
        f64::from(r.y + r.h),
    )
}

/// Convert a `kurbo::Rect` back to a bridge `Rect` for an event payload.
fn kurbo_to_bridge(r: Rect) -> BridgeRect {
    BridgeRect {
        x: r.x0 as f32,
        y: r.y0 as f32,
        w: r.width() as f32,
        h: r.height() as f32,
    }
}

/// Build a fully-resolved `TextAttrs` for the `FormattingChanged` reply,
/// filling unset patch fields with defaults.
fn resolved_attrs(patch: &TextAttrsPatch, default_size: f32) -> TextAttrs {
    TextAttrs {
        bold: patch.bold.unwrap_or(false),
        italic: patch.italic.unwrap_or(false),
        underline: patch.underline.unwrap_or(UnderlineStyle::None),
        strike: patch.strike.unwrap_or(false),
        font_family: patch.font_family.clone().unwrap_or_default(),
        font_size: patch.font_size.unwrap_or(default_size),
        color: patch.color.unwrap_or(Color {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        }),
        bg_color: patch.bg_color,
        script: patch.script.unwrap_or(VerticalScript::Normal),
        language: patch.language.clone().unwrap_or_default(),
    }
}

/* Alignment crosses three crates with identical shapes: the pure document
model (`engine`), the layout / shaping crate (`text_pipeline`), and the RPC
wire (`bridge`). These map between them. */

/// `engine::Alignment` → the layout crate's `Alignment`.
fn layout_align(a: EngineAlignment) -> Alignment {
    match a {
        EngineAlignment::Start => Alignment::Start,
        EngineAlignment::End => Alignment::End,
        EngineAlignment::Center => Alignment::Center,
        EngineAlignment::Justify => Alignment::Justify,
    }
}

/// The layout crate's `Alignment` → the RPC `bridge::Alignment`.
fn bridge_align(a: Alignment) -> BridgeAlignment {
    match a {
        Alignment::Start => BridgeAlignment::Start,
        Alignment::End => BridgeAlignment::End,
        Alignment::Center => BridgeAlignment::Center,
        Alignment::Justify => BridgeAlignment::Justify,
    }
}

/// The RPC `bridge::Alignment` → `engine::Alignment` for the document model.
fn engine_align(a: BridgeAlignment) -> EngineAlignment {
    match a {
        BridgeAlignment::Start => EngineAlignment::Start,
        BridgeAlignment::End => EngineAlignment::End,
        BridgeAlignment::Center => EngineAlignment::Center,
        BridgeAlignment::Justify => EngineAlignment::Justify,
    }
}

/// Expand a paragraph's sparse style runs into a gap-free list of resolved
/// [`StyleSpan`]s covering `[0, text.len())`, filling unstyled gaps with the
/// document defaults. Every `px_size` is multiplied by `scale` so glyphs
/// rasterize at device resolution (`default_size` arrives logical).
/// Map a font family to its loaded font id. The TS shell loads each family
/// under exactly this id (Backlog #9).
fn font_family_id(family: EngineFontFamily) -> &'static str {
    match family {
        EngineFontFamily::Amiri => "amiri",
        EngineFontFamily::LiberationSans => "liberation",
        EngineFontFamily::NotoNaskhArabic => "noto-naskh",
    }
}

/// Parse a toolbar font-family id back to the document-model enum.
fn parse_font_family(id: &str) -> Option<EngineFontFamily> {
    match id {
        "amiri" => Some(EngineFontFamily::Amiri),
        "liberation" => Some(EngineFontFamily::LiberationSans),
        "noto-naskh" => Some(EngineFontFamily::NotoNaskhArabic),
        _ => None,
    }
}

fn build_style_spans(
    para: &engine::Paragraph,
    default_size: f32,
    default_color: [u8; 4],
    scale: f32,
) -> Vec<StyleSpan> {
    let len = para.text.len() as u32;
    let mut spans: Vec<StyleSpan> = Vec::new();
    let mut cursor = 0_u32;
    /* A default-styled gap span — covers text between or outside style runs. */
    let gap = |start: u32, end: u32| StyleSpan {
        start,
        end,
        px_size: default_size * scale,
        color: default_color,
        bold: false,
        italic: false,
        underline: false,
        strike: false,
        bg_color: None,
        font_family: None,
    };
    for run in &para.spans {
        if run.start > cursor {
            spans.push(gap(cursor, run.start));
        }
        spans.push(StyleSpan {
            start: run.start,
            end: run.end,
            px_size: run.style.font_size.unwrap_or(default_size) * scale,
            color: run.style.color.unwrap_or(default_color),
            bold: run.style.bold.unwrap_or(false),
            italic: run.style.italic.unwrap_or(false),
            underline: run.style.underline.unwrap_or(false),
            strike: run.style.strike.unwrap_or(false),
            bg_color: run.style.bg_color,
            font_family: run
                .style
                .font_family
                .map(font_family_id)
                .map(str::to_string),
        });
        cursor = run.end;
    }
    if cursor < len {
        spans.push(gap(cursor, len));
    }
    spans
}

/// Style spans for a paragraph with an IME composition spliced in at `off`.
///
/// The committed spans are shifted — and split where one straddles `off` — to
/// make room for the `comp_len` inserted bytes, and the composition itself
/// gets an `underline` span, the on-canvas preview marker (Backlog #8). The
/// composition inherits the resolved style at `off`, so its size / font match
/// the text it is being typed into.
fn composition_layout_spans(
    para: &engine::Paragraph,
    off: u32,
    comp_len: u32,
    default_size: f32,
    scale: f32,
) -> Vec<StyleSpan> {
    let base = build_style_spans(para, default_size, [0, 0, 0, 255], scale);
    let mut out: Vec<StyleSpan> = Vec::with_capacity(base.len() + 2);
    for s in base {
        if s.end <= off {
            out.push(s);
        } else if s.start >= off {
            out.push(StyleSpan {
                start: s.start + comp_len,
                end: s.end + comp_len,
                ..s
            });
        } else {
            /* One committed span straddles `off` — split it so the
            composition span slots cleanly into the gap. */
            out.push(StyleSpan {
                end: off,
                ..s.clone()
            });
            out.push(StyleSpan {
                start: off + comp_len,
                end: s.end + comp_len,
                ..s
            });
        }
    }
    let st = para.style_at(off);
    out.push(StyleSpan {
        start: off,
        end: off + comp_len,
        px_size: st.font_size.unwrap_or(default_size) * scale,
        color: st.color.unwrap_or([0, 0, 0, 255]),
        bold: st.bold.unwrap_or(false),
        italic: st.italic.unwrap_or(false),
        underline: true,
        strike: st.strike.unwrap_or(false),
        bg_color: st.bg_color,
        font_family: st.font_family.map(font_family_id).map(str::to_string),
    });
    out.sort_by_key(|s| s.start);
    out
}

/// Content + render-config hash that keys the paragraph layout cache
/// (Backlog #13). Two paragraphs hash equal only when `layout_paragraph`
/// would produce identical boxes: same text, same style runs, same paragraph
/// alignment, and the same layout-affecting `RenderConfig` fields. `scale` is
/// the value passed to `build_page` — PDF export lays out at `1.0` regardless
/// of the cached device scale — so it is hashed explicitly.
fn paragraph_layout_key(para: &engine::Paragraph, cfg: &RenderConfig, scale: f32) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    para.text.hash(&mut h);
    (para.spans.len() as u64).hash(&mut h);
    for run in &para.spans {
        run.start.hash(&mut h);
        run.end.hash(&mut h);
        run.style.font_size.map(f32::to_bits).hash(&mut h);
        run.style.color.hash(&mut h);
        run.style.bold.hash(&mut h);
        run.style.italic.hash(&mut h);
        run.style.underline.hash(&mut h);
        run.style.strike.hash(&mut h);
        run.style.bg_color.hash(&mut h);
        run.style.font_family.hash(&mut h);
    }
    /* `engine::Alignment` / `text_pipeline::Alignment` carry no `Hash` derive —
    hash a small discriminant instead. */
    engine_align_disc(para.props.alignment).hash(&mut h);
    /* Phase 2 — indent + line-height override change layout geometry, so the
    cache key must mix them in or stale boxes leak across edits. */
    para.props.indent.start_twips.hash(&mut h);
    para.props.indent.end_twips.hash(&mut h);
    para.props.indent.first_line_twips.hash(&mut h);
    para.props.indent.hanging_twips.hash(&mut h);
    match para.props.line_height {
        None => 0u8.hash(&mut h),
        Some(engine::LineHeight::Auto { twips }) => {
            1u8.hash(&mut h);
            twips.hash(&mut h);
        }
        Some(engine::LineHeight::Exact { twips }) => {
            2u8.hash(&mut h);
            twips.hash(&mut h);
        }
        Some(engine::LineHeight::AtLeast { twips }) => {
            3u8.hash(&mut h);
            twips.hash(&mut h);
        }
    }
    cfg.font_id.hash(&mut h);
    matches!(cfg.base_direction, ShapingDirection::Rtl).hash(&mut h);
    cfg.px_size.to_bits().hash(&mut h);
    cfg.line_height.to_bits().hash(&mut h);
    tp_align_disc(cfg.alignment).hash(&mut h);
    scale.to_bits().hash(&mut h);
    h.finish()
}

/// OOXML twips → layout px at the current device scale. 1 CSS px = 15 twips
/// (= 0.75 pt = 1/96 inch when DPI = 96); multiplying by `scale` lifts to
/// device px, which is the unit layout / render operate in.
fn twips_to_layout_px(twips: i32, scale: f32) -> f32 {
    (twips as f32) / 15.0 * scale
}

/// Phase 6 — walk a freshly laid-out `TableBox` and stamp every nested
/// `ParagraphBox` with the next-flat source paragraph id. Skips
/// `VMergeRole::Continue` cells (they reuse the Restart cell's content).
/// The traversal order must match `walk_block_texts` so the same id maps
/// to the same source text in `para_texts`.
fn assign_source_ids_table(table: &mut TableBox, next_id: &mut u32) {
    for row in table.rows.iter_mut() {
        for cell in row.cells.iter_mut() {
            if matches!(cell.v_merge, engine::VMergeRole::Continue) {
                continue;
            }
            assign_source_ids_blocks(&mut cell.content, next_id);
        }
    }
}

fn assign_source_ids_blocks(blocks: &mut [LayoutBlock], next_id: &mut u32) {
    for b in blocks.iter_mut() {
        match b {
            LayoutBlock::Paragraph(p) => {
                p.source_paragraph_id = *next_id;
                *next_id += 1;
            }
            LayoutBlock::Table(t) => assign_source_ids_table(t, next_id),
        }
    }
}

/// Phase 6 — scale an `engine::PageGeometry` into the paginator's geometry
/// type. `engine::PageGeometry` carries pt values directly (the OOXML twips
/// → pt conversion happened at parse time); the paginator operates in
/// layout pixels, so every coordinate multiplies by `scale`.
fn scaled_paginator_geometry(geom: engine::PageGeometry, scale: f32) -> PaginatorGeometry {
    PaginatorGeometry {
        width: geom.width * scale,
        height: geom.height * scale,
        margins: layout::Margins {
            top: geom.margin_top * scale,
            right: geom.margin_right * scale,
            bottom: geom.margin_bottom * scale,
            left: geom.margin_left * scale,
        },
        header_offset: geom.header_offset * scale,
        footer_offset: geom.footer_offset * scale,
    }
}

/// Phase 6 — sync `page_paths` with the paginator's emitted-page count
/// before recording the just-pushed block's engine path. A `push_block`
/// call can:
/// - leave the page count unchanged (block fit on the current page) → push
///   one path entry on the current page;
/// - bump it by one or more (overflow; the head landed on the page that
///   was finalised, the tail on the next) → push the path on each new
///   page the block lands on.
///
/// The bookkeeping intentionally keeps `page_paths` parallel to the
/// pages the paginator has accumulated *so far*: page_paths[i] holds the
/// paths for `paginator.pages[i]`, page_paths.last() is the in-progress page.
fn attach_block_paths(
    paginator: &Paginator,
    prev_emitted: usize,
    page_paths: &mut Vec<Vec<EngineBlockPath>>,
    path: &EngineBlockPath,
) {
    let new_pages = paginator.page_count_emitted() - prev_emitted;
    /* The block contributed to the previously in-progress page and to
    every newly emitted page. */
    if let Some(cur) = page_paths.last_mut() {
        cur.push(path.clone());
    }
    for _ in 0..new_pages {
        /* The page that just finalised already has its path entry from
        the line above. The next page is the new in-progress page; it
        gets a path entry only if the block spilled onto it (handled by
        the next iteration). For multi-page overflow the same path is
        pushed onto each page. */
        page_paths.push(Vec::new());
        if let Some(cur) = page_paths.last_mut() {
            cur.push(path.clone());
        }
    }
    /* If `new_pages > 0` the final entry was for the new in-progress
    page. But the block may have actually ended on the previously
    finalised page (no tail). The conservative pass above always assigns
    one path per page from finalised+1 onwards; that overcounts when a
    block exactly fills a page with no tail. The hit-test consumers only
    use paths to map *flow position → engine block*, so a duplicate path
    entry is harmless. */
}

/// Pull the four Phase-2 indent fields off `para.props`, convert to layout
/// px. Returned in `(indent_start, indent_end, first_line, hanging)`.
fn props_to_layout_indents(props: &engine::ParaProperties, scale: f32) -> (f32, f32, f32, f32) {
    (
        twips_to_layout_px(props.indent.start_twips, scale),
        twips_to_layout_px(props.indent.end_twips, scale),
        twips_to_layout_px(props.indent.first_line_twips, scale),
        twips_to_layout_px(props.indent.hanging_twips, scale),
    )
}

/* ===================================================================
Phase 5 PR 2 — table layout
==================================================================== */

/// Lay out an `engine::Table` into a `layout::TableBox`. Column widths
/// come straight from `<w:tblGrid>` (literal twips → px); cells with
/// `grid_span > 1` consume the sum of their N spanned columns. Each
/// cell's content recursively lays out via [`layout_block_for_layout`]
/// so nested tables work. Row height = max cell measured height,
/// skipping `VMergeRole::Continue` cells (their content is owned by
/// the matching `Restart` cell — Phase 5c will accumulate Restart
/// cell heights across the merged span; PR 2 simply renders the
/// Restart cell at the row's natural height).
fn layout_table_box(
    table: &engine::Table,
    available_width_px: f32,
    fonts: &FontStack,
    cfg: &RenderConfig,
    scale: f32,
) -> TableBox {
    /* Column widths in device px. If `<w:tblGrid>` is missing, fall back
    to equal-divide across the available width — defensive for malformed
    documents. */
    let columns: Vec<f32> = if table.grid.is_empty() {
        Vec::new()
    } else {
        table
            .grid
            .iter()
            .map(|&t| twips_to_layout_px(t, scale))
            .collect()
    };

    let mut rows_out: Vec<TableRowBox> = Vec::with_capacity(table.rows.len());
    let mut y = 0.0_f32;
    let mut table_width = columns.iter().sum::<f32>();
    if table_width <= 0.0 {
        table_width = available_width_px;
    }
    for row in &table.rows {
        let mut cells_out: Vec<TableCellBox> = Vec::with_capacity(row.cells.len());
        let mut x = 0.0_f32;
        let mut row_height = 0.0_f32;
        let mut col_cursor: usize = 0;
        for cell in &row.cells {
            let span = cell.props.grid_span.max(1) as usize;
            let cell_width: f32 = if columns.is_empty() {
                /* No grid info — give every cell an equal share. */
                table_width / row.cells.len().max(1) as f32
            } else {
                /* Sum N spanned columns starting at `col_cursor`. */
                let lo = col_cursor.min(columns.len());
                let hi = (col_cursor + span).min(columns.len());
                columns[lo..hi].iter().sum::<f32>().max(1.0)
            };
            /* Recursively lay out cell content. Phase 5 PR 2 ships a fixed
            cell-internal padding of 4 px on every side — the OOXML
            `<w:tcMar>` / table-level `<w:tblCellMar>` model is a
            Phase 5b refinement. */
            let inner_pad = 4.0_f32;
            let content_width = (cell_width - inner_pad * 2.0).max(0.0);
            let inner_blocks = layout_cell_blocks(&cell.blocks, content_width, fonts, cfg, scale);
            let content_height: f32 = inner_blocks.iter().map(|b| b.size().height).sum();
            /* `VMergeRole::Continue` cells contribute zero — the matching
            `Restart` cell visually owns the merged region. */
            let measured = if matches!(cell.props.v_merge, engine::VMergeRole::Continue) {
                0.0
            } else {
                content_height + inner_pad * 2.0
            };
            row_height = row_height.max(measured);
            cells_out.push(TableCellBox {
                origin: Point { x, y: 0.0 },
                size: Size {
                    width: cell_width,
                    /* Filled below once the row height is final. */
                    height: 0.0,
                },
                grid_span: cell.props.grid_span.max(1),
                v_merge: cell.props.v_merge,
                borders: cell.props.borders.clone().unwrap_or_default(),
                shading: cell.props.shading,
                content: inner_blocks,
            });
            x += cell_width;
            col_cursor += span;
        }
        /* Apply row min-height from `<w:trHeight>` if present. */
        if let Some(rh) = row.props.height {
            match rh {
                engine::RowHeight::AtLeast { twips } | engine::RowHeight::Exact { twips } => {
                    row_height = row_height.max(twips_to_layout_px(twips, scale));
                }
                engine::RowHeight::Auto => {}
            }
        }
        /* Stamp final row height onto every cell. */
        for c in &mut cells_out {
            c.size.height = row_height;
        }
        let row_width = cells_out.iter().map(|c| c.size.width).sum::<f32>();
        rows_out.push(TableRowBox {
            origin: Point { x: 0.0, y },
            size: Size {
                width: row_width.max(table_width),
                height: row_height,
            },
            cells: cells_out,
        });
        y += row_height;
    }

    TableBox {
        origin: Point::default(),
        size: Size {
            width: table_width,
            height: y,
        },
        columns,
        rows: rows_out,
        outer_borders: table.props.borders.clone().unwrap_or_default(),
    }
}

fn layout_cell_blocks(
    blocks: &[engine::Block],
    content_width_px: f32,
    fonts: &FontStack,
    cfg: &RenderConfig,
    scale: f32,
) -> Vec<LayoutBlock> {
    let mut out: Vec<LayoutBlock> = Vec::with_capacity(blocks.len());
    let mut y = 0.0_f32;
    for b in blocks {
        let mut lb = match b {
            engine::Block::Paragraph(p) => {
                let spans = build_style_spans(p, cfg.px_size, [0, 0, 0, 255], scale);
                let (ind_s, ind_e, ind_fl, ind_h) = props_to_layout_indents(&p.props, scale);
                let pcfg = ParagraphConfig {
                    text: &p.text,
                    fonts,
                    spans: &spans,
                    base_direction: first_strong_direction(&p.text).unwrap_or(cfg.base_direction),
                    max_width: content_width_px.max(1.0),
                    line_height: cfg.line_height * scale,
                    alignment: p.props.alignment.map_or(cfg.alignment, layout_align),
                    indent_start_px: ind_s,
                    indent_end_px: ind_e,
                    first_line_indent_px: ind_fl,
                    hanging_indent_px: ind_h,
                    marker_text: p.resolved_marker.clone(),
                    px_size_for_marker: cfg.px_size * scale,
                };
                LayoutBlock::Paragraph(layout_paragraph(pcfg))
            }
            engine::Block::Table(t) => {
                LayoutBlock::Table(layout_table_box(t, content_width_px, fonts, cfg, scale))
            }
        };
        let mut o = lb.origin();
        o.y = y;
        lb.set_origin(o);
        y += lb.size().height;
        out.push(lb);
    }
    out
}

/// Discriminant for an optional paragraph alignment (the enum has no `Hash`).
fn engine_align_disc(a: Option<EngineAlignment>) -> u8 {
    match a {
        None => 0,
        Some(EngineAlignment::Start) => 1,
        Some(EngineAlignment::End) => 2,
        Some(EngineAlignment::Center) => 3,
        Some(EngineAlignment::Justify) => 4,
    }
}

/// Discriminant for a layout-crate alignment (the enum has no `Hash`).
fn tp_align_disc(a: Alignment) -> u8 {
    match a {
        Alignment::Start => 0,
        Alignment::End => 1,
        Alignment::Center => 2,
        Alignment::Justify => 3,
    }
}

/// Flatten one line into per-[`VisualRun`] geometry — each run's source byte
/// span plus a [`CaretSlot`] per cluster boundary. `line_abs_x` is the line's
/// absolute left edge. Mirrors the pen walk in
/// `render::scene::build_page_scene` so hit-testing inverts exactly the
/// geometry the renderer drew.
fn build_line_run_geom(line: &LineBox, line_abs_x: f32) -> Vec<RunGeom> {
    let mut runs: Vec<RunGeom> = Vec::new();
    let mut pen = 0.0_f32;
    for run in &line.runs {
        let run_start_x = line_abs_x + pen;
        let run_advance: f32 = run.glyphs.iter().map(|g| g.x_advance).sum();
        let mut slots: Vec<CaretSlot> = Vec::new();
        match run.direction {
            ShapingDirection::Ltr => {
                let mut cum = 0.0_f32;
                for g in &run.glyphs {
                    /* Synthetic glyphs (Kashida Tatweels) advance the pen but
                    are not caret stops — they emit no slot (Backlog #2). */
                    if !g.synthetic {
                        slots.push(CaretSlot {
                            x: run_start_x + cum,
                            byte: run.source_range.start + g.cluster,
                        });
                    }
                    cum += g.x_advance;
                }
                slots.push(CaretSlot {
                    x: run_start_x + run_advance,
                    byte: run.source_range.end,
                });
            }
            ShapingDirection::Rtl => {
                /* Visual L→R; an RTL glyph's logical-start edge is its right
                side, and the run's logical end sits at its visual left edge. */
                let mut cum = 0.0_f32;
                for g in &run.glyphs {
                    if !g.synthetic {
                        slots.push(CaretSlot {
                            x: run_start_x + cum + g.x_advance,
                            byte: run.source_range.start + g.cluster,
                        });
                    }
                    cum += g.x_advance;
                }
                slots.push(CaretSlot {
                    x: run_start_x,
                    byte: run.source_range.end,
                });
            }
        }
        runs.push(RunGeom {
            src_start: run.source_range.start,
            src_end: run.source_range.end,
            slots,
        });
        pen += run_advance;
    }
    runs
}

/// Vertical distance from `y` to a line's band; `0.0` when inside it.
fn line_y_dist(line: &LineGeom, y: f32) -> f32 {
    if y < line.y_top {
        line.y_top - y
    } else if y > line.y_top + line.height {
        y - line.y_top - line.height
    } else {
        0.0
    }
}

/// Horizontal distance from `x` to a line's hit-target rectangle;
/// `0.0` when inside it. Used to disambiguate sibling cells that
/// share `y_top` — without this the leftmost cell always wins.
fn line_x_dist(line: &LineGeom, x: f32) -> f32 {
    let x0 = line.hit_left;
    let x1 = line.hit_left + line.hit_width;
    if x < x0 {
        x0 - x
    } else if x > x1 {
        x - x1
    } else {
        0.0
    }
}

/// The line nearest `(x, y)` — the rectangle containing it, else
/// the closest by y then x. y dominates because lines stack
/// vertically; x only matters when multiple lines share the same
/// y-band (cells in a table row).
fn nearest_line(geom: &[LineGeom], x: f32, y: f32) -> Option<&LineGeom> {
    geom.iter().min_by(|a, b| {
        let ya = line_y_dist(a, y);
        let yb = line_y_dist(b, y);
        ya.total_cmp(&yb)
            .then_with(|| line_x_dist(a, x).total_cmp(&line_x_dist(b, x)))
    })
}

/// Flatten one `ParagraphBox` into `LineGeom`s stamped with `path`.
/// `container_left` / `container_width` define the horizontal
/// hit-target rectangle each emitted line carries — usually the
/// containing cell's left edge + content width, or the page content
/// area for top-level paragraphs. Without this rectangle, multiple
/// cells in the same row share `y_top` and the first-emitted line
/// always wins (clicks in column C land in column 0).
fn collect_paragraph_line_geom(
    para_box: &ParagraphBox,
    parent_origin_x: f32,
    parent_origin_y: f32,
    container_left: f32,
    container_width: f32,
    path: &BridgeBlockPath,
    out: &mut Vec<LineGeom>,
) {
    let para_x = parent_origin_x + para_box.origin.x;
    let para_y = parent_origin_y + para_box.origin.y;
    for line in &para_box.lines {
        let line_x = para_x + line.origin.x;
        let line_y = para_y + line.origin.y;
        let start_byte = line
            .runs
            .iter()
            .map(|r| r.source_range.start)
            .min()
            .unwrap_or(0);
        let end_byte = line
            .runs
            .iter()
            .map(|r| r.source_range.end)
            .max()
            .unwrap_or(0);
        let runs = build_line_run_geom(line, line_x);
        let slots: Vec<CaretSlot> = runs.iter().flat_map(|r| r.slots.iter().copied()).collect();
        out.push(LineGeom {
            path: path.clone(),
            start_x: line_x,
            hit_left: container_left,
            hit_width: container_width,
            y_top: line_y,
            height: line.height,
            start_byte,
            end_byte,
            slots,
            runs,
        });
    }
}

/// Walk a table's rows/cells and emit `LineGeom`s for every paragraph
/// inside a cell. Continue cells are skipped — their visual content
/// is owned by the Restart cell above them.
fn collect_table_line_geom(
    table_box: &TableBox,
    table_block_idx: u32,
    table_origin_x: f32,
    table_origin_y: f32,
    out: &mut Vec<LineGeom>,
) {
    for (r, row) in table_box.rows.iter().enumerate() {
        let row_x = table_origin_x + row.origin.x;
        let row_y = table_origin_y + row.origin.y;
        for (c, cell) in row.cells.iter().enumerate() {
            if cell.v_merge == engine::VMergeRole::Continue {
                continue;
            }
            let cell_x = row_x + cell.origin.x;
            let cell_y = row_y + cell.origin.y;
            for (block_idx, content) in cell.content.iter().enumerate() {
                let LayoutBlock::Paragraph(para_box) = content else {
                    continue;
                };
                let path = BridgeBlockPath {
                    steps: vec![
                        BridgePathStep::Block {
                            idx: table_block_idx,
                        },
                        BridgePathStep::Cell {
                            row: r as u32,
                            col: c as u32,
                        },
                        BridgePathStep::Block {
                            idx: block_idx as u32,
                        },
                    ],
                };
                collect_paragraph_line_geom(
                    para_box,
                    cell_x,
                    cell_y,
                    /* Hit-target = the entire cell rectangle, so a
                    click anywhere in the cell lands on this
                    paragraph's lines — not the leftmost cell that
                    happens to share `y_top`. */
                    cell_x,
                    cell.size.width,
                    &path,
                    out,
                );
            }
        }
    }
}

/// Map an absolute pixel to a logical position — nearest line by `y`, then
/// nearest caret slot by `x`. The returned `path` is the line's owning
/// paragraph path, descending into a cell when the hit lands inside one.
fn hit_test_geom(geom: &[LineGeom], x: f32, y: f32) -> BridgeLogicalPos {
    let Some(line) = nearest_line(geom, x, y) else {
        return bpos_top(0, 0);
    };
    let offset = line
        .slots
        .iter()
        .min_by(|a, b| (a.x - x).abs().total_cmp(&(b.x - x).abs()))
        .map_or(line.start_byte, |s| s.byte);
    BridgeLogicalPos {
        path: line.path.clone(),
        offset,
    }
}

/// Absolute x of the slot whose byte is nearest `byte`, within one slot list.
fn nearest_slot_x(slots: &[CaretSlot], byte: u32, fallback: f32) -> f32 {
    slots
        .iter()
        .min_by_key(|s| s.byte.abs_diff(byte))
        .map_or(fallback, |s| s.x)
}

/// Absolute x of the caret slot whose byte is nearest `byte`, across the
/// whole line.
fn slot_x_for_byte(line: &LineGeom, byte: u32) -> f32 {
    nearest_slot_x(&line.slots, byte, line.start_x)
}

/// Caret slot whose `x` is closest to `target_x` (Backlog #14, ideal-x snap).
/// `None` only when the line carries no slots — an empty paragraph.
fn nearest_slot_by_x(slots: &[CaretSlot], target_x: f32) -> Option<&CaretSlot> {
    slots.iter().min_by(|a, b| {
        (a.x - target_x)
            .abs()
            .partial_cmp(&(b.x - target_x).abs())
            .unwrap_or(core::cmp::Ordering::Equal)
    })
}

/// Step one Unicode char left in logical order (Backlog #14). At the start of
/// a paragraph the caret jumps to the end of the previous paragraph in the
/// containing flat-paragraph walk; at byte 0 of the document it pins.
fn step_left(doc: &DocumentTree, pos: BridgeLogicalPos) -> BridgeLogicalPos {
    let off = pos.offset as usize;
    let engine_path = bridge_to_engine_path(pos.path.clone());
    if let Some(para) = doc.paragraph_at_path(&engine_path) {
        if off > 0 {
            let text = &para.text;
            let mut o = off - 1;
            while o > 0 && !text.is_char_boundary(o) {
                o -= 1;
            }
            return BridgeLogicalPos {
                path: pos.path,
                offset: o as u32,
            };
        }
        if let Some((prev_path, prev_para)) = doc_paragraph_neighbor(doc, &engine_path, false) {
            return BridgeLogicalPos {
                path: engine_to_bridge_path(prev_path),
                offset: prev_para.text.len() as u32,
            };
        }
    }
    pos
}

/// Step one Unicode char right in logical order. At a paragraph's end the
/// caret jumps to the start of the next paragraph in the flat walk; at the
/// document end it pins.
fn step_right(doc: &DocumentTree, pos: BridgeLogicalPos) -> BridgeLogicalPos {
    let off = pos.offset as usize;
    let engine_path = bridge_to_engine_path(pos.path.clone());
    if let Some(para) = doc.paragraph_at_path(&engine_path) {
        let text = &para.text;
        if off < text.len() {
            let mut o = off + 1;
            while o < text.len() && !text.is_char_boundary(o) {
                o += 1;
            }
            return BridgeLogicalPos {
                path: pos.path,
                offset: o as u32,
            };
        }
        if let Some((next_path, _)) = doc_paragraph_neighbor(doc, &engine_path, true) {
            return BridgeLogicalPos {
                path: engine_to_bridge_path(next_path),
                offset: 0,
            };
        }
    }
    pos
}

/// Walk the document's paragraphs in linear order (depth-first into
/// table cells) and return the neighbour of `path`. `forward` selects
/// the next paragraph; otherwise the previous. `None` when at the
/// boundary.
fn doc_paragraph_neighbor(
    doc: &DocumentTree,
    path: &EngineBlockPath,
    forward: bool,
) -> Option<(EngineBlockPath, engine::Paragraph)> {
    let paths = doc_paragraph_paths(doc);
    let pos = paths.iter().position(|p| p == path)?;
    let target = if forward {
        pos.checked_add(1)?
    } else {
        pos.checked_sub(1)?
    };
    let candidate = paths.get(target)?.clone();
    let para = doc.paragraph_at_path(&candidate)?.clone();
    Some((candidate, para))
}

/// Flat list of every paragraph path in document order (top-level
/// paragraphs + recursive descent into table cells). Cheap for the
/// PoC and the small-table common case; cache when editing lands.
fn doc_paragraph_paths(doc: &DocumentTree) -> Vec<EngineBlockPath> {
    let mut out: Vec<EngineBlockPath> = Vec::new();
    let mut prefix: Vec<EnginePathStep> = Vec::new();
    for (i, b) in doc.blocks.iter().enumerate() {
        prefix.push(EnginePathStep::Block(i as u32));
        walk_block(b, &mut prefix, &mut out);
        prefix.pop();
    }
    out
}

fn walk_block(
    block: &engine::Block,
    prefix: &mut Vec<EnginePathStep>,
    out: &mut Vec<EngineBlockPath>,
) {
    match block {
        engine::Block::Paragraph(_) => out.push(EngineBlockPath {
            steps: prefix.clone(),
        }),
        engine::Block::Table(t) => {
            for (r, row) in t.rows.iter().enumerate() {
                for (c, cell) in row.cells.iter().enumerate() {
                    if cell.props.v_merge == engine::VMergeRole::Continue {
                        continue;
                    }
                    prefix.push(EnginePathStep::Cell {
                        row: r as u32,
                        col: c as u32,
                    });
                    collect_paragraph_paths_vec(&cell.blocks, prefix, out);
                    prefix.pop();
                }
            }
        }
    }
}

/// Collect every paragraph's text in document order (top-level
/// paragraphs first, then each table's rows × cells × cell.blocks,
/// skipping `VMergeRole::Continue` cells). Order must match
/// `format_pdf::for_each_paragraph` so the PDF `/ToUnicode` CMap
/// aligns with the laid-out paragraph stream.
fn walk_block_texts<'a>(block: &'a engine::Block, out: &mut Vec<&'a str>) {
    match block {
        engine::Block::Paragraph(p) => out.push(p.text.as_str()),
        engine::Block::Table(t) => {
            for row in &t.rows {
                for cell in &row.cells {
                    if cell.props.v_merge == engine::VMergeRole::Continue {
                        continue;
                    }
                    for b in &cell.blocks {
                        walk_block_texts(b, out);
                    }
                }
            }
        }
    }
}

fn collect_paragraph_paths_vec(
    blocks: &[engine::Block],
    prefix: &mut Vec<EnginePathStep>,
    out: &mut Vec<EngineBlockPath>,
) {
    for (i, b) in blocks.iter().enumerate() {
        prefix.push(EnginePathStep::Block(i as u32));
        walk_block(b, prefix, out);
        prefix.pop();
    }
}

/// Caret rectangle for `pos`, `caret_w` device px wide. Falls back to
/// `fallback` when the document has no geometry yet (empty document).
fn caret_rect_geom(
    geom: &[LineGeom],
    pos: &BridgeLogicalPos,
    fallback: BridgeRect,
    caret_w: f32,
) -> BridgeRect {
    let line = geom
        .iter()
        .find(|l| l.path == pos.path && pos.offset >= l.start_byte && pos.offset <= l.end_byte)
        .or_else(|| geom.first());
    match line {
        Some(line) => BridgeRect {
            x: slot_x_for_byte(line, pos.offset),
            y: line.y_top,
            w: caret_w,
            h: line.height,
        },
        None => fallback,
    }
}

/// Per-`VisualRun` selection rectangles for `[start, end]` — the discontinuous
/// BiDi subset (Backlog #7). The selected byte range is clipped against each
/// run's byte span; every intersected run yields one tight rect. A run is
/// unidirectional, so its clipped sub-span is visually contiguous — a
/// selection crossing an LTR↔RTL seam renders as separate, accurate segments
/// instead of one rect that over-covers the gap between them. A line with a
/// single run (no BiDi) still yields exactly one rect, as before.
fn selection_rects_geom(
    geom: &[LineGeom],
    start: &BridgeLogicalPos,
    end: &BridgeLogicalPos,
) -> Vec<BridgeRect> {
    use core::cmp::Ordering;
    let mut rects: Vec<BridgeRect> = Vec::new();
    for line in geom {
        /* Skip lines whose paragraph sits before start or after end in
        document order; equal paths clip to the per-paragraph offsets. */
        let cmp_start = line.path.cmp_doc_order(&start.path);
        let cmp_end = line.path.cmp_doc_order(&end.path);
        if cmp_start == Ordering::Less || cmp_end == Ordering::Greater {
            continue;
        }
        let lo = if cmp_start == Ordering::Equal {
            start.offset.max(line.start_byte)
        } else {
            line.start_byte
        };
        let hi = if cmp_end == Ordering::Equal {
            end.offset.min(line.end_byte)
        } else {
            line.end_byte
        };
        if lo >= hi {
            continue;
        }
        for run in &line.runs {
            /* Clip the selected byte range to this run's source span. Runs
            partition the line's bytes, so the clips are disjoint. */
            let clip_lo = lo.max(run.src_start);
            let clip_hi = hi.min(run.src_end);
            if clip_lo >= clip_hi {
                continue;
            }
            let xa = nearest_slot_x(&run.slots, clip_lo, line.start_x);
            let xb = nearest_slot_x(&run.slots, clip_hi, line.start_x);
            let (x0, x1) = if xa <= xb { (xa, xb) } else { (xb, xa) };
            rects.push(BridgeRect {
                x: x0,
                y: line.y_top,
                w: x1 - x0,
                h: line.height,
            });
        }
    }
    rects
}

/// Order two positions into document order (path, then offset).
fn ordered(a: BridgeLogicalPos, b: BridgeLogicalPos) -> (BridgeLogicalPos, BridgeLogicalPos) {
    use core::cmp::Ordering;
    let ord = a.path.cmp_doc_order(&b.path);
    let swap = match ord {
        Ordering::Less => false,
        Ordering::Greater => true,
        Ordering::Equal => a.offset > b.offset,
    };
    if swap { (b, a) } else { (a, b) }
}

/// Clamp a position into `doc` — `path` resolved to a real paragraph
/// (falling back to the document end), `offset` capped at the
/// paragraph's UTF-8 length.
fn clamp_pos(doc: &DocumentTree, pos: BridgeLogicalPos) -> BridgeLogicalPos {
    if doc.paragraph_count() == 0 {
        return bpos_top(0, 0);
    }
    let engine_path = bridge_to_engine_path(pos.path.clone());
    let (resolved_path, para_len) = match doc.paragraph_at_path(&engine_path) {
        Some(p) => (engine_path, p.text.len() as u32),
        None => {
            let fallback = doc
                .path_to_last_top_paragraph()
                .unwrap_or(EngineBlockPath::top(0));
            let len = doc
                .paragraph_at_path(&fallback)
                .map(|p| p.text.len() as u32)
                .unwrap_or(0);
            (fallback, len)
        }
    };
    let offset = pos.offset.min(para_len);
    BridgeLogicalPos {
        path: engine_to_bridge_path(resolved_path),
        offset,
    }
}

/// Append a non-empty `[s, e)` slice of `text` as an accessibility run.
fn push_run(runs: &mut Vec<A11yRun>, text: &str, s: u32, e: u32, style: SpanStyle) {
    if s < e {
        runs.push(A11yRun {
            text: text[s as usize..e as usize].to_string(),
            bold: style.bold.unwrap_or(false),
            italic: style.italic.unwrap_or(false),
            underline: style.underline.unwrap_or(false),
        });
    }
}

/// Split a paragraph into gap-free accessibility runs by its style spans.
fn a11y_runs(para: &engine::Paragraph) -> Vec<A11yRun> {
    let len = para.text.len() as u32;
    let mut runs: Vec<A11yRun> = Vec::new();
    let mut cursor = 0_u32;
    for sr in &para.spans {
        push_run(
            &mut runs,
            &para.text,
            cursor,
            sr.start,
            SpanStyle::default(),
        );
        push_run(&mut runs, &para.text, sr.start, sr.end, sr.style);
        cursor = sr.end;
    }
    push_run(&mut runs, &para.text, cursor, len, SpanStyle::default());
    runs
}

/// Build the accessibility node for one top-level block.
/// Paragraphs map to `A11yNode::Paragraph`; tables walk rows and cells,
/// resolving `<w:gridSpan>` → `col_span` and counting consecutive
/// `VMergeRole::Continue` rows below a `Restart` cell → `row_span`.
fn build_a11y_block(block: &engine::Block, block_index: u32, direction: Direction) -> A11yNode {
    match block {
        engine::Block::Paragraph(p) => A11yNode::Paragraph(A11yParagraph {
            direction,
            runs: a11y_runs(p),
        }),
        engine::Block::Table(t) => A11yNode::Table(build_a11y_table(t, block_index, direction)),
    }
}

fn build_a11y_table(t: &engine::Table, block_index: u32, direction: Direction) -> A11yTable {
    /* Pre-compute vMerge row spans: for every (r, c) that is a Restart,
    count the run of Continue rows directly below at the same column.
    Continue cells are skipped from the DOM — the Restart cell carries
    the span. */
    let n_rows = t.rows.len();
    let row_span_for = |r: usize, c: usize| -> u32 {
        let mut span = 1u32;
        let mut k = r + 1;
        while k < n_rows
            && t.rows[k]
                .cells
                .get(c)
                .map(|cell| cell.props.v_merge == engine::VMergeRole::Continue)
                .unwrap_or(false)
        {
            span += 1;
            k += 1;
        }
        span
    };

    let mut rows: Vec<A11yRow> = Vec::with_capacity(t.rows.len());
    for (r, row) in t.rows.iter().enumerate() {
        let mut out_cells: Vec<A11yCell> = Vec::with_capacity(row.cells.len());
        for (c, cell) in row.cells.iter().enumerate() {
            /* Continue rows are visually owned by the Restart above —
            skip from the a11y tree so screen readers don't read them
            twice. */
            if cell.props.v_merge == engine::VMergeRole::Continue {
                continue;
            }
            let col_span = cell.props.grid_span.max(1) as u32;
            let row_span = if cell.props.v_merge == engine::VMergeRole::Restart {
                row_span_for(r, c)
            } else {
                1
            };
            /* PR 3b: cell paragraphs become paragraphs; nested tables
            stay flat (recursive descent is PR 4). */
            let nodes: Vec<A11yNode> = cell
                .blocks
                .iter()
                .filter_map(|b| match b {
                    engine::Block::Paragraph(p) => Some(A11yNode::Paragraph(A11yParagraph {
                        direction,
                        runs: a11y_runs(p),
                    })),
                    engine::Block::Table(_) => None,
                })
                .collect();
            out_cells.push(A11yCell {
                row: r as u32,
                col: c as u32,
                row_span,
                col_span,
                nodes,
            });
        }
        rows.push(A11yRow { cells: out_cells });
    }
    A11yTable { block_index, rows }
}

/// Diff two accessibility node lists into the minimal patch set (Backlog
/// #10).
///
/// A prefix/suffix trim: nodes equal at the front and back are skipped,
/// leaving one changed region. Overlapping positions in that region become
/// `Update`s; then the longer side contributes `Insert`s or `Remove`s — never
/// both, since trimming leaves a single region that only grows or shrinks.
/// Typing in paragraph K trims to `{K}` → one `Update`; pressing Enter in K
/// yields `Update(K)` + `Insert(K+1)`.
fn diff_a11y(prev: &[A11yNode], next: &[A11yNode]) -> Vec<A11yPatch> {
    let max_common = prev.len().min(next.len());

    /* Common prefix: nodes identical from the front. */
    let mut pre = 0;
    while pre < max_common && prev[pre] == next[pre] {
        pre += 1;
    }

    /* Common suffix: nodes identical from the back, not overrunning the
    prefix already matched on either side. */
    let mut suf = 0;
    while suf < max_common - pre && prev[prev.len() - 1 - suf] == next[next.len() - 1 - suf] {
        suf += 1;
    }

    let old_mid = &prev[pre..prev.len() - suf];
    let new_mid = &next[pre..next.len() - suf];
    let overlap = old_mid.len().min(new_mid.len());

    let mut patches: Vec<A11yPatch> = Vec::new();
    /* Overlapping positions: replace where the node actually differs. */
    for (j, (old, new)) in old_mid.iter().zip(new_mid.iter()).enumerate() {
        if old != new {
            patches.push(A11yPatch::Update {
                index: (pre + j) as u32,
                node: new.clone(),
            });
        }
    }
    /* The longer side: grow with `Insert`s or shrink with `Remove`s. */
    if new_mid.len() > overlap {
        for (j, node) in new_mid.iter().enumerate().skip(overlap) {
            patches.push(A11yPatch::Insert {
                index: (pre + j) as u32,
                node: node.clone(),
            });
        }
    } else {
        /* Each removal shrinks the list, so the next stale node slides into
        the same index — remove there repeatedly. */
        for _ in overlap..old_mid.len() {
            patches.push(A11yPatch::Remove {
                index: (pre + overlap) as u32,
            });
        }
    }
    patches
}

impl Engine {
    async fn apply(&mut self, cmd: Command) -> Event {
        match cmd {
            Command::Ping => Event::Pong,

            Command::LoadFont { id, bytes } => match LoadedFont::parse(id.clone(), bytes) {
                Ok(font) => {
                    let m = font.metrics(1.0);
                    let bridge_metrics = BridgeMetrics {
                        units_per_em: m.units_per_em,
                        ascent: m.ascent,
                        descent: m.descent,
                        leading: m.leading,
                        cap_height: m.cap_height,
                        x_height: m.x_height,
                    };
                    self.fonts.insert(id.clone(), Arc::new(font));
                    /* The font set feeds layout; stale boxes must not survive. */
                    self.layout_cache.get_mut().clear();
                    Event::FontLoaded {
                        id,
                        metrics: bridge_metrics,
                    }
                }
                Err(e) => Event::Error {
                    message: format!("LoadFont: {e}"),
                },
            },

            Command::RasterizeGlyph {
                font_id,
                ch,
                px_size,
            } => self.raster_glyph(font_id, ch, px_size),

            Command::ShapeAndRasterize {
                text,
                font_id,
                direction,
                px_size,
            } => self.shape_and_paint(text, font_id, direction, px_size),

            Command::RenderPage {
                text,
                font_id,
                base_direction,
                px_size,
                line_height,
                align,
                device_pixel_ratio,
            } => {
                let cfg = RenderConfig {
                    font_id,
                    base_direction: match base_direction.to_ascii_uppercase().as_str() {
                        "RTL" => ShapingDirection::Rtl,
                        _ => ShapingDirection::Ltr,
                    },
                    px_size,
                    line_height,
                    alignment: match align.to_ascii_uppercase().as_str() {
                        "JUSTIFY" => Alignment::Justify,
                        "END" => Alignment::End,
                        "CENTER" => Alignment::Center,
                        _ => Alignment::Start,
                    },
                    scale: device_pixel_ratio.unwrap_or(1.0).max(1.0),
                };
                self.render_page(text, cfg)
            }

            Command::InsertText { at, text } => match at {
                /* A set `at` is the interactive caret path — selection-aware,
                emits SelectionChanged. `None` is the Phase-1 harness path
                (append at end-of-document, emits TextInserted). */
                Some(p) => self.do_insert_text_interactive(p, text),
                None => self.insert_text(None, text),
            },
            Command::Undo => self.do_undo(),
            Command::Redo => self.do_redo(),

            Command::LoadDocx { bytes } => match format_docx::read_docx(&bytes) {
                Ok(archive) => {
                    let paragraph_count = archive.document.paragraph_count();
                    self.undo = UndoStack::new(archive.document, 100);
                    /* The prior selection points into the replaced document —
                    reset the caret to the start of the loaded one. */
                    self.selection = Some(SelectionState {
                        anchor: bpos_top(0, 0),
                        caret: bpos_top(0, 0),
                        ideal_x: None,
                        kind: SelectionKind::Linear,
                    });
                    self.layout_cache.get_mut().clear();
                    self.dirty.invalidate(full_page_rect(self.scale()));
                    self.maybe_repaint();
                    Event::DocumentLoaded { paragraph_count }
                }
                Err(e) => Event::Error {
                    message: format!("LoadDocx: {e}"),
                },
            },

            Command::SaveDocx => match build_minimal_docx(self.undo.current()) {
                Ok(bytes) => {
                    let size = bytes.len() as u32;
                    Event::DocumentSaved { bytes, size }
                }
                Err(e) => Event::Error {
                    message: format!("SaveDocx: {e}"),
                },
            },

            // ===============================================================
            // Phase 2 schema stubs — PHASE_2_BRIDGE_MEMORY.md §4.
            // The typed RPC surface accepts these commands; real engine
            // behavior lands in Phase 3 behind the RequestPaint pipeline.
            // ===============================================================
            Command::Init { .. } => phase3_stub("Init"),
            Command::Recover { .. } => phase3_stub("Recover"),
            Command::Dispose => phase3_stub("Dispose"),
            Command::Tick { .. } => phase3_stub("Tick"),
            Command::OpenDocument { .. } => phase3_stub("OpenDocument"),
            Command::SaveDocument { .. } => phase3_stub("SaveDocument"),
            Command::ExportPdf { conformance } => self.do_export_pdf(conformance),
            Command::CloseDocument => phase3_stub("CloseDocument"),
            Command::DeleteRange { range } => self.do_delete_range(range),
            Command::ReplaceRange { .. } => phase3_stub("ReplaceRange"),
            Command::ApplyFormatting { range, attrs } => self.apply_formatting(range, attrs),
            Command::SplitParagraph { at } => self.do_split_paragraph(at),
            Command::MergeParagraph { .. } => phase3_stub("MergeParagraph"),
            Command::InsertImage { .. } => phase3_stub("InsertImage"),
            Command::SetSelection { range, caret } => self.do_set_selection(range, caret),
            Command::ExtendSelection { to, .. } => self.do_extend_selection(to),
            Command::SelectAll => self.do_select_all(),
            Command::MoveCaret { direction, extend } => self.do_move_caret(direction, extend),
            Command::BeginComposition { at } => self.do_begin_composition(at),
            Command::UpdateComposition { text, target_range } => {
                self.do_update_composition(text, target_range)
            }
            Command::EndComposition { commit } => self.do_end_composition(commit),
            Command::SetViewport { .. } => phase3_stub("SetViewport"),
            Command::SetZoom { .. } => phase3_stub("SetZoom"),
            Command::RequestPaint { viewport, dirty } => self.do_request_paint(viewport, dirty),
            Command::UnloadFont { .. } => phase3_stub("UnloadFont"),
            Command::RequestStats => self.request_stats(),

            // Phase 4 — PHASE_4_HEADLESS_UI.md §7. Additive pointer commands.
            Command::HitTest { at } => self.do_hit_test(at),
            Command::SelectWordAt { at } => self.do_select_word_at(at),
            Command::SelectParagraphAt { at } => self.do_select_paragraph_at(at),
            Command::DeleteAtCaret { forward, by_word } => {
                self.do_delete_at_caret(forward, by_word)
            }

            // Phase 4 §10 — accessibility mirror (Backlog #10: fine-grained deltas).
            Command::RequestAccessibilityDelta => Event::AccessibilityTreeDelta {
                patches: self.build_a11y_delta(),
            },

            // Phase 4 §12 — clipboard. Backlog sprint 7 adds rich HTML paste.
            Command::GetSelectionAsClipboard => self.do_get_selection_as_clipboard(),
            Command::PastePlain { text } => self.do_paste_plain(text),
            Command::PasteHtml { html } => self.do_paste_html(html),

            // Backlog sprint 1 — paragraph alignment (Backlog #9).
            Command::SetParagraphAlign { range, align } => {
                self.do_set_paragraph_align(range, align)
            }

            // Phase 5 PR 3 — table mutation commands. `BlockPath` flows
            // straight through to the engine; every command flips
            // `Table.dirty = true` so the writer regenerates on save.
            Command::InsertTable { at, rows, cols } => self.do_insert_table(at, rows, cols),
            Command::DeleteTable { path } => self.do_delete_table(path),
            Command::InsertRow {
                table_path,
                after_row,
            } => self.do_insert_row(table_path, after_row),
            Command::DeleteRow { table_path, row } => self.do_delete_row(table_path, row),
            Command::InsertColumn {
                table_path,
                after_col,
            } => self.do_insert_column(table_path, after_col),
            Command::DeleteColumn { table_path, col } => self.do_delete_column(table_path, col),
            Command::MergeCells {
                table_path,
                from_row,
                from_col,
                to_row,
                to_col,
            } => self.do_merge_cells(table_path, from_row, from_col, to_row, to_col),
            Command::SplitCell {
                table_path,
                row,
                col,
            } => self.do_split_cell(table_path, row, col),
            Command::SetCellShading {
                table_path,
                row,
                col,
                color,
            } => self.do_set_cell_shading(table_path, row, col, color),
            Command::SetCellBorders {
                table_path,
                row,
                col,
                borders,
            } => self.do_set_cell_borders(table_path, row, col, borders),
        }
    }

    fn raster_glyph(&mut self, font_id: String, ch: String, px_size: f32) -> Event {
        let ch = match ch.chars().next() {
            Some(c) => c,
            None => {
                return Event::Error {
                    message: "RasterizeGlyph: empty char string".into(),
                };
            }
        };
        let font = match self.fonts.get(&font_id) {
            Some(f) => f.clone(),
            None => {
                return Event::Error {
                    message: format!("font `{font_id}` not loaded"),
                };
            }
        };
        let gm = match font.glyph_metrics(ch, px_size) {
            Ok(g) => g,
            Err(e) => {
                return Event::Error {
                    message: format!("glyph_metrics: {e}"),
                };
            }
        };
        let raster = match font.rasterize(ch, px_size) {
            Ok(r) => r,
            Err(e) => {
                return Event::Error {
                    message: format!("rasterize: {e}"),
                };
            }
        };
        let scaled = font.metrics(px_size);
        if let Some(ctx) = &self.ctx {
            if let Err(e) = render::canvas2d_backend::paint_alpha_glyph(
                ctx,
                &raster,
                POC_BASELINE_X,
                POC_BASELINE_Y,
                [0, 0, 0],
                None,
            ) {
                return Event::Error {
                    message: format!("paint: {e:?}"),
                };
            }
        }
        Event::GlyphPainted {
            font_id,
            ch: ch.to_string(),
            advance_width: gm.advance_width,
            ascent: scaled.ascent,
            glyph_width: raster.width,
            glyph_height: raster.height,
        }
    }

    fn shape_and_paint(
        &mut self,
        text: String,
        font_id: String,
        direction: String,
        px_size: f32,
    ) -> Event {
        let dir = match direction.as_str() {
            "RTL" | "rtl" => ShapingDirection::Rtl,
            _ => ShapingDirection::Ltr,
        };
        let font = match self.fonts.get(&font_id) {
            Some(f) => f.clone(),
            None => {
                return Event::Error {
                    message: format!("font `{font_id}` not loaded"),
                };
            }
        };
        let shaped = shape_text(&font, &text, dir, px_size);
        let scaled = font.metrics(px_size);

        if let Some(ctx) = &self.ctx {
            let mut pen_x = POC_BASELINE_X;
            let pen_y = POC_BASELINE_Y;
            for g in &shaped.glyphs {
                if g.glyph_id == 0 {
                    pen_x += g.x_advance as f64;
                    continue;
                }
                let raster = match font.rasterize_glyph(g.glyph_id as u16, px_size) {
                    Ok(r) => r,
                    Err(_) => {
                        pen_x += g.x_advance as f64;
                        continue;
                    }
                };
                let dx = pen_x + g.x_offset as f64;
                let dy = pen_y - g.y_offset as f64;
                if let Err(e) = render::canvas2d_backend::paint_alpha_glyph(
                    ctx,
                    &raster,
                    dx,
                    dy,
                    [0, 0, 0],
                    None,
                ) {
                    return Event::Error {
                        message: format!("paint: {e:?}"),
                    };
                }
                pen_x += g.x_advance as f64;
            }
        }

        let glyph_ids: Vec<u32> = shaped.glyphs.iter().map(|g| g.glyph_id).collect();
        Event::ShapedAndPainted {
            font_id,
            text,
            direction,
            glyph_count: shaped.glyphs.len() as u32,
            total_advance: shaped.total_advance,
            ascent: scaled.ascent,
            glyph_ids,
        }
    }

    /// Reset the document + undo stack to a single paragraph of `text`, cache
    /// `cfg` so subsequent InsertText/Undo/Redo commands repaint without
    /// re-specifying params, then paint the first frame.
    fn render_page(&mut self, text: String, cfg: RenderConfig) -> Event {
        self.undo = UndoStack::new(DocumentTree::from_text(&text), 100);
        self.layout_cfg = Some(cfg);
        /* New document + config — drop every cached paragraph layout. */
        self.layout_cache.get_mut().clear();

        let stats = match self.render_document(None) {
            Ok(s) => s,
            Err(e) => return *e,
        };
        Event::PageRendered {
            page_width: stats.page_width,
            page_height: stats.page_height,
            line_count: stats.line_count,
            glyph_count: stats.glyph_count,
        }
    }

    fn insert_text(&mut self, at: Option<BridgeLogicalPos>, text: String) -> Event {
        let pos = at
            .map(to_engine_pos)
            .unwrap_or_else(|| self.undo.current().end_of_document());
        let new_doc = self.undo.current().insert_text(pos, &text);
        self.undo.push(new_doc);
        self.dirty.invalidate(full_page_rect(self.scale()));
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        Event::TextInserted {
            paragraph_count: self.undo.current().paragraph_count(),
            can_undo: self.undo.can_undo(),
            can_redo: self.undo.can_redo(),
            undo_depth: self.undo.depth(),
        }
    }

    fn apply_formatting(&mut self, range: BridgeLogicalRange, attrs: TextAttrsPatch) -> Event {
        let patch = SpanStyle {
            font_size: attrs.font_size,
            color: attrs.color.map(|c| [c.r, c.g, c.b, c.a]),
            bold: attrs.bold,
            italic: attrs.italic,
            /* Underline is a stored on/off flag in the model. */
            underline: attrs.underline.map(|u| !matches!(u, UnderlineStyle::None)),
            strike: attrs.strike,
            bg_color: attrs.bg_color.map(|c| [c.r, c.g, c.b, c.a]),
            font_family: attrs.font_family.as_deref().and_then(parse_font_family),
        };
        /* Sticky formatting (Backlog #11): a collapsed caret has no text to
        style. Rather than push a no-op edit, arm the patch as the pending
        style — the next interactive InsertText overlays it onto the typed
        text. Toggling the same button merges over the prior pending value.
        Gated on an active selection so the visual-diff harness (which has no
        selection) keeps its original no-op path. */
        if range.start == range.end && self.selection.is_some() {
            let armed = self.pending_format.unwrap_or_default().merged_with(patch);
            self.pending_format = Some(armed);
            return self.selection_changed();
        }
        let new_doc = self.undo.current().apply_style(
            to_engine_pos(range.start.clone()),
            to_engine_pos(range.end.clone()),
            patch,
        );
        self.undo.push(new_doc);
        self.dirty.invalidate(full_page_rect(self.scale()));
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        /* Interactive (a selection is set) → SelectionChanged so the toolbar
        sees the new attrs; the visual-diff harness has no selection and
        keeps the Phase-1 FormattingChanged reply. */
        if self.selection.is_some() {
            self.selection_changed()
        } else {
            let default_size = self.layout_cfg.as_ref().map_or(16.0, |c| c.px_size);
            Event::FormattingChanged {
                range,
                attrs: resolved_attrs(&attrs, default_size),
            }
        }
    }

    fn do_undo(&mut self) -> Event {
        self.undo.undo();
        self.dirty.invalidate(full_page_rect(self.scale()));
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        self.after_history_change()
    }

    fn do_redo(&mut self) -> Event {
        self.undo.redo();
        self.dirty.invalidate(full_page_rect(self.scale()));
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        self.after_history_change()
    }

    /// D2.5 telemetry. `wasm_heap_bytes` and undo/font counters are real;
    /// `document_tree_bytes` is an estimate (sum of paragraph text bytes);
    /// the glyph cache and frame timings land with the Phase 3 renderer.
    fn request_stats(&self) -> Event {
        let doc = self.undo.current();
        let document_tree_bytes: usize = doc
            .blocks
            .iter()
            .filter_map(engine::Block::as_paragraph)
            .map(|p| p.text.len())
            .sum();
        Event::Stats(EngineStats {
            wasm_heap_bytes: wasm_heap_bytes(),
            document_tree_bytes: document_tree_bytes as u32,
            glyph_cache_entries: 0,
            undo_stack_depth: self.undo.depth(),
            fonts_resident: self.fonts.len() as u32,
            last_paint_ms: 0.0,
            last_command_ms: 0.0,
        })
    }

    fn maybe_repaint(&mut self) {
        let _ = self.render_document(None);
    }

    fn maybe_repaint_result(&mut self) -> Result<(), Box<Event>> {
        if self.layout_cfg.is_none() {
            return Ok(());
        }
        self.render_document(None).map(|_| ())
    }

    /// The device-pixel ratio the page is currently laid out + painted at.
    /// `1.0` until a `RenderPage` caches a config.
    fn scale(&self) -> f32 {
        self.layout_cfg.as_ref().map_or(1.0, |c| c.scale)
    }

    /// Lay out the current document into one or more `PageBox`es plus the
    /// `FontStack` used to shape it. Shared by the Canvas2D repaint and PDF
    /// export. Every dimension is multiplied by `scale` — `dpr` for the
    /// HiDPI canvas, `1.0` for PDF (PDF user space is logical points).
    ///
    /// Phase 6 — the engine flows blocks through a [`Paginator`], emitting
    /// a fresh `PageBox` whenever content overflows or the document moves
    /// into a section with different page geometry. The returned
    /// `Vec<EngineBlockPath>` is per-page, parallel to the `PageBox`
    /// vector: `paths[i][j]` is the engine block path of `pages[i].blocks[j]`.
    /// Paragraph splits emit the same engine path on both pages.
    #[allow(clippy::type_complexity)]
    fn build_pages(
        &self,
        scale: f32,
        with_composition: bool,
    ) -> Result<(Vec<PageBox>, FontStack, Vec<Vec<EngineBlockPath>>), Box<Event>> {
        let cfg = match self.layout_cfg.clone() {
            Some(c) => c,
            None => {
                return Err(Box::new(Event::Error {
                    message: "build_pages: no layout config cached".into(),
                }));
            }
        };
        if !self.fonts.contains_key(&cfg.font_id) {
            return Err(Box::new(Event::Error {
                message: format!("font `{}` not loaded", cfg.font_id),
            }));
        }

        /* Per-script font stack; the cached `font_id` is the fallback root. */
        let font_stack = FontStack::from_faces(self.fonts.clone(), &cfg.font_id);
        let doc = self.undo.current().clone();
        let mut cache = self.layout_cache.borrow_mut();
        let composition = if with_composition {
            self.composition.as_ref()
        } else {
            None
        };

        let sections = doc.effective_sections();
        /* Phase 6 — every laid-out paragraph carries a flat
        `source_paragraph_id` indexing into the per-document paragraph
        text table the PDF `/ToUnicode` builder consumes. The id is the
        walk-order index of the source paragraph in `walk_block_texts`,
        so PDF receives `&[&str]` of those texts and looks up by id —
        split paragraphs (head + tail) share the same id and resolve
        to the same source string. */
        let mut next_para_id: u32 = 0;
        /* Each top-level block is covered by at most one effective section. The
        paginator runs once across the whole document; section boundaries
        trigger a hard page break + geometry swap. */
        let mut paginator: Option<Paginator> = None;
        let mut page_paths: Vec<Vec<EngineBlockPath>> = Vec::new();
        /* Track the page index at the start of the current paginator's
        accumulated `cur_blocks` so we know where to attach paths emitted
        by `push_block` (paginator may emit prior pages first). */
        let mut emitted_pages: Vec<PageBox> = Vec::new();
        let mut emitted_paths: Vec<Vec<EngineBlockPath>> = Vec::new();

        for section in &sections {
            let geom = scaled_paginator_geometry(section.geometry, scale);
            /* Flush the prior paginator (if any) before swapping geometry. */
            if let Some(p) = paginator.take() {
                let mut pages = p.finish();
                /* `paginator` was started fresh — `page_paths` carries the
                same number of entries. Move them en bloc. */
                let consume = pages.len();
                emitted_pages.append(&mut pages);
                let mut paths_taken: Vec<Vec<EngineBlockPath>> = std::mem::take(&mut page_paths);
                paths_taken.resize_with(consume, Vec::new);
                emitted_paths.append(&mut paths_taken);
            }
            paginator = Some(Paginator::new(geom, None, None));
            page_paths.clear();
            page_paths.push(Vec::new());

            for block_idx in section.start_block..section.end_block {
                let Some(block) = doc.blocks.get(block_idx as usize) else {
                    continue;
                };
                let pag = paginator.as_mut().expect("paginator created");
                let para_path = EngineBlockPath::top(block_idx);

                match block {
                    engine::Block::Table(t) => {
                        let mut tb =
                            layout_table_box(t, pag.content_width(), &font_stack, &cfg, scale);
                        assign_source_ids_table(&mut tb, &mut next_para_id);
                        let prev_pages_in_pag = pag.page_count_emitted();
                        pag.push_block(LayoutBlock::Table(tb), 0.0, 0.0);
                        attach_block_paths(pag, prev_pages_in_pag, &mut page_paths, &para_path);
                    }
                    engine::Block::Paragraph(para) => {
                        let comp = composition.filter(|c| {
                            bridge_to_engine_path(c.at.path.clone()) == para_path
                                && !c.text.is_empty()
                                && (c.at.offset as usize) <= para.text.len()
                                && para.text.is_char_boundary(c.at.offset as usize)
                        });
                        let para_box = if let Some(c) = comp {
                            let off = c.at.offset as usize;
                            let mut text = String::with_capacity(para.text.len() + c.text.len());
                            text.push_str(&para.text[..off]);
                            text.push_str(&c.text);
                            text.push_str(&para.text[off..]);
                            let spans = composition_layout_spans(
                                para,
                                off as u32,
                                c.text.len() as u32,
                                cfg.px_size,
                                scale,
                            );
                            let (ind_s, ind_e, ind_fl, ind_h) =
                                props_to_layout_indents(&para.props, scale);
                            layout_paragraph(ParagraphConfig {
                                text: &text,
                                fonts: &font_stack,
                                spans: &spans,
                                base_direction: first_strong_direction(&text)
                                    .unwrap_or(cfg.base_direction),
                                max_width: pag.content_width(),
                                line_height: cfg.line_height * scale,
                                alignment: para.props.alignment.map_or(cfg.alignment, layout_align),
                                indent_start_px: ind_s,
                                indent_end_px: ind_e,
                                first_line_indent_px: ind_fl,
                                hanging_indent_px: ind_h,
                                marker_text: para.resolved_marker.clone(),
                                px_size_for_marker: cfg.px_size * scale,
                            })
                        } else {
                            let key = paragraph_layout_key(para, &cfg, scale);
                            if let Some(cached) = cache.get(&key) {
                                cached.clone()
                            } else {
                                let spans =
                                    build_style_spans(para, cfg.px_size, [0, 0, 0, 255], scale);
                                let (ind_s, ind_e, ind_fl, ind_h) =
                                    props_to_layout_indents(&para.props, scale);
                                let para_cfg = ParagraphConfig {
                                    text: &para.text,
                                    fonts: &font_stack,
                                    spans: &spans,
                                    base_direction: first_strong_direction(&para.text)
                                        .unwrap_or(cfg.base_direction),
                                    max_width: pag.content_width(),
                                    line_height: cfg.line_height * scale,
                                    alignment: para
                                        .props
                                        .alignment
                                        .map_or(cfg.alignment, layout_align),
                                    indent_start_px: ind_s,
                                    indent_end_px: ind_e,
                                    first_line_indent_px: ind_fl,
                                    hanging_indent_px: ind_h,
                                    marker_text: para.resolved_marker.clone(),
                                    px_size_for_marker: cfg.px_size * scale,
                                };
                                let laid = layout_paragraph(para_cfg);
                                cache.put(key, laid.clone());
                                laid
                            }
                        };
                        let before_px = twips_to_layout_px(para.props.spacing.before_twips, scale);
                        let after_px = twips_to_layout_px(para.props.spacing.after_twips, scale);
                        let mut para_box = para_box;
                        para_box.source_paragraph_id = next_para_id;
                        next_para_id += 1;
                        let prev_pages_in_pag = pag.page_count_emitted();
                        pag.push_block(LayoutBlock::Paragraph(para_box), before_px, after_px);
                        attach_block_paths(pag, prev_pages_in_pag, &mut page_paths, &para_path);
                    }
                }
            }
        }
        drop(cache);

        if let Some(p) = paginator.take() {
            let mut pages = p.finish();
            let consume = pages.len();
            emitted_pages.append(&mut pages);
            let mut paths_taken: Vec<Vec<EngineBlockPath>> = std::mem::take(&mut page_paths);
            paths_taken.resize_with(consume, Vec::new);
            emitted_paths.append(&mut paths_taken);
        }
        /* Always at least one page so downstream consumers can index `[0]`. */
        if emitted_pages.is_empty() {
            let default_geom = scaled_paginator_geometry(engine::PageGeometry::a4(), scale);
            emitted_pages.push(PageBox {
                size: Size {
                    width: default_geom.width,
                    height: default_geom.height,
                },
                margins: default_geom.margins,
                blocks: Vec::new(),
                header: None,
                footer: None,
            });
            emitted_paths.push(Vec::new());
        }
        Ok((emitted_pages, font_stack, emitted_paths))
    }

    /// Backwards-compatible single-page wrapper for callers still on the
    /// pre-Phase-6 single-page contract. Returns the *first* paginated
    /// page; the multi-page paint / hit-test / PDF paths consume
    /// [`build_pages`] directly.
    #[allow(dead_code)]
    fn build_page(
        &self,
        scale: f32,
        with_composition: bool,
    ) -> Result<(PageBox, FontStack, Vec<EngineBlockPath>), Box<Event>> {
        let (mut pages, fonts, mut paths) = self.build_pages(scale, with_composition)?;
        let page = pages.drain(..).next().unwrap_or_else(|| PageBox {
            size: Size {
                width: 0.0,
                height: 0.0,
            },
            margins: A4Page::a4().margin,
            blocks: Vec::new(),
            header: None,
            footer: None,
        });
        let p0 = paths.drain(..).next().unwrap_or_default();
        Ok((page, fonts, p0))
    }

    fn render_document(&mut self, clip: Option<Rect>) -> Result<RenderStats, Box<Event>> {
        /* `true` — splice the live IME composition preview into the paint. */
        let (pages, _font_stack, _box_paths) = self.build_pages(self.scale(), true)?;

        let mut line_count: u32 = 0;
        let mut glyph_count: u32 = 0;
        for page in &pages {
            for p in page.blocks.iter().filter_map(LayoutBlock::as_paragraph) {
                line_count += p.lines.len() as u32;
                for line in &p.lines {
                    for run in &line.runs {
                        glyph_count += run.glyphs.len() as u32;
                    }
                }
            }
        }

        /* Phase 6 — paginated scene: stack every `PageBox` with a small
        inter-page gap so a section / overflow break is visible. */
        let scale = self.scale();
        let gap = render::scene::PAGE_GAP_PT * scale;
        let scene = render::scene::build_document_scene(&pages, gap);
        /* Stats report the first page's dimensions for back-compat; the
        `page_count` / total document height live alongside `paint_ms` in
        the `Painted` event so the TS shell can resize the canvas to fit
        every page (the multi-page scrolled viewport is wired in the
        post-Phase-6 sprint). */
        let (page_width, page_height) = pages
            .first()
            .map(|p| (p.size.width, p.size.height))
            .unwrap_or((0.0, 0.0));
        let doc_height: f32 = pages.iter().map(|p| p.size.height + gap).sum::<f32>() - gap;
        let stats = RenderStats {
            page_width,
            page_height,
            line_count,
            glyph_count,
        };
        let _ = doc_height; // reserved for the post-Phase-6 viewport-height event field

        /* Vello path: encode the whole display list and present it over
        WebGPU. Vello runs its own GPU-side glyph cache, so the Canvas2D
        `GlyphAtlas` stays untouched. Clipped partial repaint is not wired on
        this path yet — Vello redraws the full scene each frame. */
        if let Some(vr) = self.vello.as_mut() {
            let fonts = &self.fonts;
            vr.render(&scene, |id| {
                fonts
                    .get(id)
                    .map(|f| render::vello_backend::font_data(f.data_static()))
            })
            .map_err(|e| {
                Box::new(Event::Error {
                    message: format!("vello paint: {e}"),
                })
            })?;
            return Ok(stats);
        }

        /* Canvas2D path — clipped to the dirty region (D3.8). */
        let total_h: f32 = pages.iter().map(|p| p.size.height + gap).sum::<f32>() - gap;
        let widest: f32 = pages.iter().map(|p| p.size.width).fold(0.0_f32, f32::max);
        let clip_rect =
            clip.unwrap_or_else(|| Rect::new(0.0, 0.0, f64::from(widest), f64::from(total_h)));
        let ctx = match &self.ctx {
            Some(c) => c.clone(),
            None => {
                return Err(Box::new(Event::Error {
                    message: "no canvas".into(),
                }));
            }
        };
        if let Err(e) = render_canvas2d(
            &ctx,
            &scene,
            &mut self.atlas,
            |id| self.fonts.get(id).cloned(),
            clip_rect,
        ) {
            return Err(Box::new(Event::Error {
                message: format!("paint: {e:?}"),
            }));
        }

        Ok(stats)
    }

    /// Export the current document to a single-page PDF (D3.7). Always laid
    /// out at scale `1.0` — PDF user space is logical points, never device px.
    ///
    /// `conformance` selects the output profile (D5.4): `A1b` emits a
    /// PDF/A-1b-conformant file; the unimplemented `A2u` / `X3` targets fall
    /// back to a plain PDF.
    fn do_export_pdf(&self, conformance: PdfConformance) -> Event {
        /* `false` — a PDF export is the committed document, never the
        in-progress IME composition. */
        let (pages, font_stack, _box_paths) = match self.build_pages(1.0, false) {
            Ok(v) => v,
            Err(e) => return *e,
        };
        let profile = match conformance {
            PdfConformance::A1b => format_pdf::PdfProfile::A1b,
            PdfConformance::A2u | PdfConformance::X3 => format_pdf::PdfProfile::Plain,
        };
        /* Phase 6 — `para_texts` is a flat per-document table indexed by
        `ParagraphBox::source_paragraph_id`. The walk order matches the
        engine layer that stamped those ids (`walk_block_texts` over
        `doc.blocks`, skipping `VMergeRole::Continue`). Paragraphs split
        across pages share an id, so both halves resolve to the same
        source text. */
        let doc = self.undo.current();
        let mut para_texts: Vec<&str> = Vec::new();
        for b in doc.blocks.iter() {
            walk_block_texts(b, &mut para_texts);
        }
        let mut bytes: Vec<u8> = Vec::new();
        if let Err(e) =
            format_pdf::export_pdf(&pages, &font_stack, &para_texts, profile, &mut bytes)
        {
            return Event::Error {
                message: format!("ExportPdf: {e}"),
            };
        }
        let pages_count = pages.len() as u32;
        Event::PdfExported {
            bytes,
            pages: pages_count,
        }
    }

    /// D3.8: repaint the document clipped to the dirty region. The command's
    /// `dirty` rect wins; else the accumulated [`DirtyTracker`] region; else
    /// the full `viewport`.
    fn do_request_paint(&mut self, viewport: BridgeRect, dirty: Option<BridgeRect>) -> Event {
        let drained = self.dirty.drain();
        let region = dirty
            .map(bridge_to_kurbo)
            .or(drained)
            .unwrap_or_else(|| bridge_to_kurbo(viewport));
        if let Err(e) = self.render_document(Some(region)) {
            return *e;
        }
        Event::Painted {
            dirty: kurbo_to_bridge(region),
            version: u64::from(self.undo.depth()),
            paint_ms: 0.0,
        }
    }

    /// Flatten the current document into per-line hit-test geometry. PR 4:
    /// recurses into table cells so a click inside a cell maps to a
    /// `BlockPath` ending at the cell's paragraph (`[Block(t), Cell{r,c},
    /// Block(p)]`). Re-lays out the document on every call — cheap for the
    /// single-page PoC; cache when editing lands.
    fn document_geometry(&self) -> Result<Vec<LineGeom>, Box<Event>> {
        /* `false` — hit-test + caret geometry run on committed document
        offsets, which `self.selection` is also expressed in. */
        let (pages, _fonts, page_paths) = self.build_pages(self.scale(), false)?;
        let gap = render::scene::PAGE_GAP_PT * self.scale();
        let mut geom: Vec<LineGeom> = Vec::new();
        let mut page_top: f32 = 0.0;
        for (pi, page) in pages.iter().enumerate() {
            let content_x = page.margins.left;
            let content_y = page_top + page.margins.top;
            let content_w = page.size.width - page.margins.left - page.margins.right;
            let paths = page_paths.get(pi).map(|v| v.as_slice()).unwrap_or(&[]);
            for (i, layout_block) in page.blocks.iter().enumerate() {
                let Some(path) = paths.get(i) else {
                    continue;
                };
                match layout_block {
                    LayoutBlock::Paragraph(para_box) => {
                        collect_paragraph_line_geom(
                            para_box,
                            content_x,
                            content_y,
                            content_x,
                            content_w,
                            &engine_to_bridge_path(path.clone()),
                            &mut geom,
                        );
                    }
                    LayoutBlock::Table(table_box) => {
                        let table_block_idx = path.last_block_index().unwrap_or(0);
                        collect_table_line_geom(
                            table_box,
                            table_block_idx,
                            content_x + table_box.origin.x,
                            content_y + table_box.origin.y,
                            &mut geom,
                        );
                    }
                }
            }
            page_top += page.size.height + gap;
        }
        Ok(geom)
    }

    /// `Command::HitTest` — pixel → logical position. A pure query; the
    /// selection is not mutated.
    fn do_hit_test(&self, at: BridgePoint) -> Event {
        match self.document_geometry() {
            Ok(geom) => Event::HitResult {
                pos: hit_test_geom(&geom, at.x, at.y),
            },
            Err(e) => *e,
        }
    }

    /// `Command::SetSelection` — set the selection to `range`, caret at `caret`.
    fn do_set_selection(&mut self, range: BridgeLogicalRange, caret: BridgeLogicalPos) -> Event {
        let anchor = if caret == range.start {
            range.end
        } else {
            range.start
        };
        /* A caret move discards any armed sticky style (Backlog #11). */
        self.pending_format = None;
        let kind = derive_selection_kind(&anchor, &caret);
        self.selection = Some(SelectionState {
            anchor,
            caret,
            ideal_x: None,
            kind,
        });
        self.selection_changed()
    }

    /// `Command::ExtendSelection` — keep the anchor, move the caret to `to`.
    fn do_extend_selection(&mut self, to: BridgeLogicalPos) -> Event {
        let anchor = self
            .selection
            .as_ref()
            .map_or_else(|| to.clone(), |s| s.anchor.clone());
        self.pending_format = None;
        let kind = derive_selection_kind(&anchor, &to);
        self.selection = Some(SelectionState {
            anchor,
            caret: to,
            ideal_x: None,
            kind,
        });
        self.selection_changed()
    }

    /// `Command::SelectWordAt` — hit-test, then select the whole word
    /// (double-click).
    fn do_select_word_at(&mut self, at: BridgePoint) -> Event {
        let geom = match self.document_geometry() {
            Ok(g) => g,
            Err(e) => return *e,
        };
        let hit = hit_test_geom(&geom, at.x, at.y);
        let engine_path = bridge_to_engine_path(hit.path.clone());
        let (lo, hi) = self
            .undo
            .current()
            .paragraph_at_path(&engine_path)
            .map_or((hit.offset, hit.offset), |p| p.word_bounds(hit.offset));
        self.pending_format = None;
        self.selection = Some(SelectionState {
            anchor: BridgeLogicalPos {
                path: hit.path.clone(),
                offset: lo,
            },
            caret: BridgeLogicalPos {
                path: hit.path,
                offset: hi,
            },
            ideal_x: None,
            kind: SelectionKind::Linear,
        });
        self.selection_changed()
    }

    /// `Command::SelectParagraphAt` — hit-test then select the whole
    /// paragraph (triple-click).
    fn do_select_paragraph_at(&mut self, at: BridgePoint) -> Event {
        let geom = match self.document_geometry() {
            Ok(g) => g,
            Err(e) => return *e,
        };
        let hit = hit_test_geom(&geom, at.x, at.y);
        let engine_path = bridge_to_engine_path(hit.path.clone());
        let len = self
            .undo
            .current()
            .paragraph_at_path(&engine_path)
            .map_or(0, |p| p.text.len() as u32);
        self.pending_format = None;
        self.selection = Some(SelectionState {
            anchor: BridgeLogicalPos {
                path: hit.path.clone(),
                offset: 0,
            },
            caret: BridgeLogicalPos {
                path: hit.path,
                offset: len,
            },
            ideal_x: None,
            kind: SelectionKind::Linear,
        });
        self.selection_changed()
    }

    /// `Command::SelectAll` — anchor at the document start, caret at the very
    /// last paragraph's byte length. Empty document collapses to (0, 0).
    fn do_select_all(&mut self) -> Event {
        let doc = self.undo.current();
        let last_path = doc
            .path_to_last_top_paragraph()
            .unwrap_or(EngineBlockPath::top(0));
        let last_len = doc
            .paragraph_at_path(&last_path)
            .map_or(0, |p| p.text.len() as u32);
        self.pending_format = None;
        self.selection = Some(SelectionState {
            anchor: bpos_top(0, 0),
            caret: BridgeLogicalPos {
                path: engine_to_bridge_path(last_path),
                offset: last_len,
            },
            ideal_x: None,
            kind: SelectionKind::Linear,
        });
        self.selection_changed()
    }

    /// `Command::MoveCaret` (Backlog #14). Left/Right step one Unicode char in
    /// logical order and reset the ideal-x; Up/Down walk to the adjacent line
    /// and snap to the slot nearest the stored ideal-x. `extend: true` keeps
    /// the anchor put so the gesture extends the selection (Shift + Arrow).
    fn do_move_caret(&mut self, direction: MoveDirection, extend: bool) -> Event {
        let sel = self.selection.clone().unwrap_or(SelectionState {
            anchor: bpos_top(0, 0),
            caret: bpos_top(0, 0),
            ideal_x: None,
            kind: SelectionKind::Linear,
        });
        let doc = self.undo.current().clone();
        let (new_caret, new_ideal) = match direction {
            MoveDirection::Left => (step_left(&doc, sel.caret.clone()), None),
            MoveDirection::Right => (step_right(&doc, sel.caret.clone()), None),
            MoveDirection::NextCell | MoveDirection::PrevCell => (
                cell_tab_step(
                    &mut self.undo,
                    &sel.caret,
                    direction == MoveDirection::NextCell,
                ),
                None,
            ),
            MoveDirection::Up | MoveDirection::Down => {
                let geom = match self.document_geometry() {
                    Ok(g) => g,
                    Err(e) => return *e,
                };
                let caret_path = sel.caret.path.clone();
                let caret_offset = sel.caret.offset;
                /* Re-use the carried column if a vertical walk is in progress;
                otherwise lock in the caret's current x. */
                let ideal = sel.ideal_x.unwrap_or_else(|| {
                    geom.iter()
                        .find(|l| {
                            l.path == caret_path
                                && caret_offset >= l.start_byte
                                && caret_offset <= l.end_byte
                        })
                        .or_else(|| geom.first())
                        .map_or(0.0, |line| slot_x_for_byte(line, caret_offset))
                });
                let cur_idx = geom.iter().position(|l| {
                    l.path == caret_path
                        && caret_offset >= l.start_byte
                        && caret_offset <= l.end_byte
                });
                let target = match (cur_idx, direction) {
                    (Some(i), MoveDirection::Up) if i > 0 => geom.get(i - 1),
                    (Some(i), MoveDirection::Down) => geom.get(i + 1),
                    _ => None,
                };
                let new_caret = match target {
                    Some(line) => nearest_slot_by_x(&line.slots, ideal)
                        .map(|s| BridgeLogicalPos {
                            path: line.path.clone(),
                            offset: s.byte,
                        })
                        .unwrap_or_else(|| sel.caret.clone()),
                    None => sel.caret.clone(),
                };
                (new_caret, Some(ideal))
            }
        };
        let new_caret = clamp_pos(self.undo.current(), new_caret);
        self.pending_format = None;
        let anchor = if extend {
            sel.anchor.clone()
        } else {
            new_caret.clone()
        };
        let kind = derive_selection_kind(&anchor, &new_caret);
        self.selection = Some(SelectionState {
            anchor,
            caret: new_caret,
            ideal_x: new_ideal,
            kind,
        });
        self.selection_changed()
    }

    /// Assemble a `SelectionChanged` event from the current selection.
    fn selection_changed(&self) -> Event {
        let Some(sel) = self.selection.clone() else {
            return Event::Error {
                message: "selection_changed: no active selection".into(),
            };
        };
        let geom = match self.document_geometry() {
            Ok(g) => g,
            Err(e) => return *e,
        };
        let (start, end) = ordered(sel.anchor.clone(), sel.caret.clone());
        let scale = self.scale();
        let page = A4Page::a4().scaled(scale);
        let fallback = BridgeRect {
            x: page.margin.left,
            y: page.margin.top,
            w: CARET_WIDTH * scale,
            h: self.layout_cfg.as_ref().map_or(16.0, |c| c.line_height) * scale,
        };
        let rects = match &sel.kind {
            SelectionKind::TableCells {
                table_path,
                from_row,
                from_col,
                to_row,
                to_col,
            } => table_cell_rects(&geom, table_path, *from_row, *from_col, *to_row, *to_col),
            SelectionKind::Linear => {
                if start == end {
                    Vec::new()
                } else {
                    selection_rects_geom(&geom, &start, &end)
                }
            }
        };
        let direction = match self.layout_cfg.as_ref().map(|c| c.base_direction) {
            Some(ShapingDirection::Rtl) => Direction::Rtl,
            _ => Direction::Ltr,
        };
        let probe = self.attrs_probe(&start, &end);
        Event::SelectionChanged {
            range: BridgeLogicalRange {
                start: start.clone(),
                end: end.clone(),
            },
            caret: caret_rect_geom(&geom, &sel.caret, fallback, CARET_WIDTH * scale),
            direction,
            rects,
            /* A collapsed caret reflects any armed pending style; a real
            selection reports the document's own attributes (Backlog #11). */
            attrs_at_caret: self.attrs_at(probe, start == end),
            paragraph_alignment: self.paragraph_alignment_at(&sel.caret.path),
            can_undo: self.undo.can_undo(),
            can_redo: self.undo.can_redo(),
            selection_kind: sel.kind.clone(),
        }
    }

    /// Effective alignment of the paragraph at `path` for the toolbar
    /// — the paragraph's own override, else the document's render-
    /// config default.
    fn paragraph_alignment_at(&self, path: &BridgeBlockPath) -> BridgeAlignment {
        let engine_path = bridge_to_engine_path(path.clone());
        let stored = self
            .undo
            .current()
            .paragraph_at_path(&engine_path)
            .and_then(|p| p.props.alignment)
            .map(layout_align);
        let default = self
            .layout_cfg
            .as_ref()
            .map_or(Alignment::Start, |c| c.alignment);
        bridge_align(stored.unwrap_or(default))
    }

    /// The offset whose style the toolbar should reflect: the selection start
    /// for a range, or the char before a collapsed caret (the style typing
    /// there would extend). `style_at(caret)` alone reads the char *after* the
    /// caret, which is unstyled right after formatting a selection.
    fn attrs_probe(&self, start: &BridgeLogicalPos, end: &BridgeLogicalPos) -> BridgeLogicalPos {
        if start != end || start.offset == 0 {
            return start.clone();
        }
        let engine_path = bridge_to_engine_path(start.path.clone());
        let prev = self
            .undo
            .current()
            .paragraph_at_path(&engine_path)
            .map_or(start.offset, |p| p.prev_offset(start.offset));
        BridgeLogicalPos {
            path: start.path.clone(),
            offset: prev,
        }
    }

    /// Resolved text attributes at `pos`. Spans carry size + colour + the
    /// bold/italic/underline flags; `strike`, `bg_color`, `script` and
    /// `language` default until those land. When `apply_pending` is set (a
    /// collapsed caret), any armed sticky style is overlaid so the toolbar
    /// previews what the next keystroke will adopt (Backlog #11).
    fn attrs_at(&self, pos: BridgeLogicalPos, apply_pending: bool) -> TextAttrs {
        let engine_path = bridge_to_engine_path(pos.path.clone());
        let mut style = self
            .undo
            .current()
            .paragraph_at_path(&engine_path)
            .map_or(SpanStyle::default(), |p| p.style_at(pos.offset));
        if apply_pending && let Some(pending) = self.pending_format {
            style = style.merged_with(pending);
        }
        let default_size = self.layout_cfg.as_ref().map_or(16.0, |c| c.px_size);
        let [r, g, b, a] = style.color.unwrap_or([0, 0, 0, 255]);
        let underline = if style.underline.unwrap_or(false) {
            UnderlineStyle::Single
        } else {
            UnderlineStyle::None
        };
        TextAttrs {
            bold: style.bold.unwrap_or(false),
            italic: style.italic.unwrap_or(false),
            underline,
            strike: style.strike.unwrap_or(false),
            /* The span's own family, else the document's default font id. */
            font_family: style
                .font_family
                .map(font_family_id)
                .map(str::to_string)
                .or_else(|| self.layout_cfg.as_ref().map(|c| c.font_id.clone()))
                .unwrap_or_default(),
            font_size: style.font_size.unwrap_or(default_size),
            color: Color { r, g, b, a },
            bg_color: style.bg_color.map(|[r, g, b, a]| Color { r, g, b, a }),
            script: VerticalScript::Normal,
            language: String::new(),
        }
    }

    /// Commit a document edit: push undo, collapse the caret at `caret`,
    /// invalidate + repaint, then emit `SelectionChanged`.
    fn commit_edit(&mut self, new_doc: DocumentTree, caret: BridgeLogicalPos) -> Event {
        self.undo.push(new_doc);
        let caret = clamp_pos(self.undo.current(), caret);
        let anchor = caret.clone();
        self.selection = Some(SelectionState {
            anchor,
            caret,
            ideal_x: None,
            kind: SelectionKind::Linear,
        });
        self.dirty.invalidate(full_page_rect(self.scale()));
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        self.selection_changed()
    }

    /// Interactive `InsertText` — replace any non-empty selection with `text`,
    /// then place the caret after it.
    fn do_insert_text_interactive(&mut self, at: BridgeLogicalPos, text: String) -> Event {
        let sel = self.selection.clone().unwrap_or(SelectionState {
            anchor: at.clone(),
            caret: at,
            ideal_x: None,
            kind: SelectionKind::Linear,
        });
        let (start, end) = ordered(sel.anchor, sel.caret);
        let base = if start == end {
            self.undo.current().clone()
        } else {
            self.undo
                .current()
                .delete_range(to_engine_pos(start.clone()), to_engine_pos(end))
        };
        let mut new_doc = base.insert_text(to_engine_pos(start.clone()), &text);
        let inserted_end = start.offset + text.len() as u32;
        /* Sticky formatting (Backlog #11): overlay any armed pending style
        onto the just-inserted run. It is intentionally NOT cleared here — it
        stays armed across consecutive keystrokes so a whole typed run shares
        the style, and is dropped only when the caret moves. */
        if let Some(pending) = self.pending_format {
            new_doc = new_doc.apply_style(
                to_engine_pos(start.clone()),
                EnginePos {
                    path: bridge_to_engine_path(start.path.clone()),
                    offset: inserted_end,
                },
                pending,
            );
        }
        let caret = BridgeLogicalPos {
            path: start.path,
            offset: inserted_end,
        };
        self.commit_edit(new_doc, caret)
    }

    /// `Command::DeleteRange` — delete an explicit logical range.
    fn do_delete_range(&mut self, range: BridgeLogicalRange) -> Event {
        let (start, end) = ordered(range.start, range.end);
        let new_doc = self
            .undo
            .current()
            .delete_range(to_engine_pos(start.clone()), to_engine_pos(end));
        self.commit_edit(new_doc, start)
    }

    /// `Command::SplitParagraph` — break the paragraph at the caret (replacing
    /// any non-empty selection first); the caret moves to the new paragraph.
    fn do_split_paragraph(&mut self, at: BridgeLogicalPos) -> Event {
        let (base, split_at) = match self.selection.clone() {
            Some(s) => {
                let (start, end) = ordered(s.anchor, s.caret);
                let doc = if start == end {
                    self.undo.current().clone()
                } else {
                    self.undo
                        .current()
                        .delete_range(to_engine_pos(start.clone()), to_engine_pos(end))
                };
                (doc, start)
            }
            None => (self.undo.current().clone(), at),
        };
        let new_doc = base.split_paragraph(to_engine_pos(split_at.clone()));
        let next_path = engine::bump_last_block_index(&bridge_to_engine_path(split_at.path));
        let caret = BridgeLogicalPos {
            path: engine_to_bridge_path(next_path),
            offset: 0,
        };
        self.commit_edit(new_doc, caret)
    }

    /// `Command::DeleteAtCaret` — delete the selection if non-empty, else one
    /// grapheme (or word) in the `forward` direction from the caret.
    fn do_delete_at_caret(&mut self, forward: bool, by_word: bool) -> Event {
        let Some(sel) = self.selection.clone() else {
            return Event::Error {
                message: "DeleteAtCaret: no active selection".into(),
            };
        };
        let (start, end) = ordered(sel.anchor, sel.caret.clone());
        if start != end {
            let new_doc = self
                .undo
                .current()
                .delete_range(to_engine_pos(start.clone()), to_engine_pos(end));
            return self.commit_edit(new_doc, start);
        }
        let Some((del_start, del_end)) = self.delete_target(sel.caret, forward, by_word) else {
            /* Caret at a document edge — nothing to delete. */
            return self.selection_changed();
        };
        let new_doc = self
            .undo
            .current()
            .delete_range(to_engine_pos(del_start.clone()), to_engine_pos(del_end));
        self.commit_edit(new_doc, del_start)
    }

    /// The range a collapsed-caret delete should remove. `None` at the matching
    /// document edge. A paragraph-boundary delete returns a cross-paragraph
    /// range, which `delete_range` resolves as a merge.
    fn delete_target(
        &self,
        caret: BridgeLogicalPos,
        forward: bool,
        by_word: bool,
    ) -> Option<(BridgeLogicalPos, BridgeLogicalPos)> {
        let doc = self.undo.current();
        let engine_path = bridge_to_engine_path(caret.path.clone());
        let para = doc.paragraph_at_path(&engine_path)?;
        let para_len = para.text.len() as u32;
        if forward {
            if caret.offset < para_len {
                let to = if by_word {
                    para.word_bounds(caret.offset).1
                } else {
                    para.next_offset(caret.offset)
                };
                let end = BridgeLogicalPos {
                    path: caret.path.clone(),
                    offset: to,
                };
                Some((caret, end))
            } else if let Some((next_path, _)) = doc_paragraph_neighbor(doc, &engine_path, true) {
                Some((
                    caret,
                    BridgeLogicalPos {
                        path: engine_to_bridge_path(next_path),
                        offset: 0,
                    },
                ))
            } else {
                None
            }
        } else if caret.offset > 0 {
            let from = if by_word {
                para.word_bounds(para.prev_offset(caret.offset)).0
            } else {
                para.prev_offset(caret.offset)
            };
            let start = BridgeLogicalPos {
                path: caret.path.clone(),
                offset: from,
            };
            Some((start, caret))
        } else if let Some((prev_path, prev_para)) =
            doc_paragraph_neighbor(doc, &engine_path, false)
        {
            Some((
                BridgeLogicalPos {
                    path: engine_to_bridge_path(prev_path),
                    offset: prev_para.text.len() as u32,
                },
                caret,
            ))
        } else {
            None
        }
    }

    /// `Command::BeginComposition` — start tracking an IME composition.
    fn do_begin_composition(&mut self, at: BridgeLogicalPos) -> Event {
        self.composition = Some(CompositionState {
            at: at.clone(),
            text: String::new(),
        });
        Event::CompositionUpdated {
            at,
            text: String::new(),
            target_range: None,
        }
    }

    /// `Command::UpdateComposition` — record the in-progress composed text.
    fn do_update_composition(
        &mut self,
        text: String,
        target_range: Option<BridgeLogicalRange>,
    ) -> Event {
        let at = match &self.composition {
            Some(c) => c.at.clone(),
            None => self
                .selection
                .as_ref()
                .map_or_else(|| bpos_top(0, 0), |s| s.caret.clone()),
        };
        self.composition = Some(CompositionState {
            at: at.clone(),
            text: text.clone(),
        });
        /* Backlog #8: repaint so the inline composition preview tracks the
        latest composed text. */
        let _ = self.render_document(None);
        Event::CompositionUpdated {
            at,
            text,
            target_range,
        }
    }

    /// `Command::EndComposition` — commit the tracked composed text (when
    /// `commit`), inserting it at the composition start.
    fn do_end_composition(&mut self, commit: bool) -> Event {
        match self.composition.take() {
            Some(c) if commit && !c.text.is_empty() => {
                self.do_insert_text_interactive(c.at, c.text)
            }
            _ => {
                /* Cancelled or empty — drop the preview and repaint the
                committed document so the canvas no longer shows it. */
                let _ = self.render_document(None);
                self.selection_changed()
            }
        }
    }

    /// Re-emit selection after an undo/redo, clamping the caret into the
    /// restored document. Falls back to `UndoStateChanged` when no selection
    /// exists (the Phase-1 harness path).
    fn after_history_change(&mut self) -> Event {
        match self.selection.clone() {
            Some(sel) => {
                let doc = self.undo.current();
                let anchor = clamp_pos(doc, sel.anchor);
                let caret = clamp_pos(doc, sel.caret);
                let kind = derive_selection_kind(&anchor, &caret);
                self.selection = Some(SelectionState {
                    anchor,
                    caret,
                    ideal_x: None,
                    kind,
                });
                self.selection_changed()
            }
            None => Event::UndoStateChanged {
                can_undo: self.undo.can_undo(),
                can_redo: self.undo.can_redo(),
                undo_depth: self.undo.depth(),
            },
        }
    }

    /// Build the accessibility node list for the current document — one node
    /// per top-level block (PHASE_4_HEADLESS_UI.md §10). Paragraphs emit
    /// `A11yNode::Paragraph`; tables emit `A11yNode::Table` with resolved
    /// `rowSpan` / `colSpan` per cell so the DOM can stamp ARIA spans
    /// directly. Phase 5 PR 3b: nested tables inside cells stay flat — the
    /// recursive `nodes` slot is empty for nested tables until PR 4.
    fn build_a11y_nodes(&self) -> Vec<A11yNode> {
        let direction = match self.layout_cfg.as_ref().map(|c| c.base_direction) {
            Some(ShapingDirection::Rtl) => Direction::Rtl,
            _ => Direction::Ltr,
        };
        self.undo
            .current()
            .blocks
            .iter()
            .enumerate()
            .map(|(block_index, b)| build_a11y_block(b, block_index as u32, direction))
            .collect()
    }

    /// Build an incremental accessibility delta (Backlog #10). The first call
    /// on an engine instance emits a single `Replace` — there is no prior tree
    /// to diff, and a post-recovery engine must hand the UI a clean rebuild.
    /// Every later call diffs against the cached tree, so a keystroke that
    /// touches one paragraph emits exactly one `Update`.
    fn build_a11y_delta(&mut self) -> Vec<A11yPatch> {
        let next = self.build_a11y_nodes();
        let patches = match &self.a11y_cache {
            None => vec![A11yPatch::Replace {
                tree: A11yTree {
                    nodes: next.clone(),
                },
            }],
            Some(prev) => diff_a11y(prev, &next),
        };
        self.a11y_cache = Some(next);
        patches
    }

    /// `Command::GetSelectionAsClipboard` — snapshot the selection as the
    /// three clipboard MIME payloads (Backlog #12): plain text, semantic
    /// HTML, and a minimal standalone `.docx`. An empty selection yields all
    /// three empty.
    fn do_get_selection_as_clipboard(&self) -> Event {
        let empty = Event::ClipboardPayload {
            plain: String::new(),
            html: String::new(),
            docx_fragment: Vec::new(),
        };
        let Some(sel) = self.selection.clone() else {
            return empty;
        };
        let (start, end) = ordered(sel.anchor, sel.caret);
        if start == end {
            return empty;
        }
        let doc = self.undo.current();
        let estart = to_engine_pos(start);
        let eend = to_engine_pos(end);
        let plain = doc.text_range(estart.clone(), eend.clone());
        /* The selection's styled paragraphs, clipped to local offsets — fed
        to the HTML serializer and packed into a one-document `.docx`. */
        let slice = doc.slice(estart, eend);
        let html = engine::html::to_html(&slice);
        let docx_fragment =
            build_minimal_docx(&DocumentTree::from_rich_paragraphs(slice)).unwrap_or_default();
        Event::ClipboardPayload {
            plain,
            html,
            docx_fragment,
        }
    }

    /// `Command::PastePlain` — insert clipboard text at the caret, replacing
    /// any non-empty selection. Newlines split the text into separate
    /// paragraphs (Backlog #12); a newline-free paste keeps the single-line
    /// caret-relative path (so it still picks up any pending sticky style).
    fn do_paste_plain(&mut self, text: String) -> Event {
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let at = self
            .selection
            .as_ref()
            .map_or_else(|| bpos_top(0, 0), |s| s.caret.clone());
        if !normalized.contains('\n') {
            return self.do_insert_text_interactive(at, normalized);
        }
        /* Multi-line: replace any non-empty selection, then insert the text
        with a paragraph break at every newline. The caret lands at the end
        of the final pasted line. */
        let sel = self.selection.clone().unwrap_or(SelectionState {
            anchor: at.clone(),
            caret: at,
            ideal_x: None,
            kind: SelectionKind::Linear,
        });
        let (start, end) = ordered(sel.anchor, sel.caret);
        let base = if start == end {
            self.undo.current().clone()
        } else {
            self.undo
                .current()
                .delete_range(to_engine_pos(start.clone()), to_engine_pos(end))
        };
        let (new_doc, caret) = base.insert_multiline(to_engine_pos(start), &normalized);
        self.commit_edit(new_doc, to_bridge_pos(caret))
    }

    /// `Command::PasteHtml` (Backlog #12) — parse HTML into styled paragraphs
    /// and splice them in at the caret, replacing any non-empty selection.
    /// The rich counterpart of `do_paste_plain`.
    fn do_paste_html(&mut self, html: String) -> Event {
        let paras = engine::html::from_html(&html);
        if paras.is_empty() {
            /* No parseable content — leave the document untouched. */
            return self.selection_changed();
        }
        let at = self
            .selection
            .as_ref()
            .map_or_else(|| bpos_top(0, 0), |s| s.caret.clone());
        let sel = self.selection.clone().unwrap_or(SelectionState {
            anchor: at.clone(),
            caret: at,
            ideal_x: None,
            kind: SelectionKind::Linear,
        });
        let (start, end) = ordered(sel.anchor, sel.caret);
        let base = if start == end {
            self.undo.current().clone()
        } else {
            self.undo
                .current()
                .delete_range(to_engine_pos(start.clone()), to_engine_pos(end))
        };
        let (new_doc, caret) = base.insert_rich(to_engine_pos(start), &paras);
        self.commit_edit(new_doc, to_bridge_pos(caret))
    }

    /// `Command::SetParagraphAlign` (Backlog #9) — set the alignment of every
    /// paragraph the range spans. A real edit (undo snapshot + reflow), but
    /// unlike a text edit it leaves the selection in place so the user can
    /// re-align without losing their place.
    fn do_set_paragraph_align(
        &mut self,
        range: BridgeLogicalRange,
        align: BridgeAlignment,
    ) -> Event {
        let (start, end) = ordered(range.start, range.end);
        let new_doc = self.undo.current().set_alignment(
            to_engine_pos(start.clone()),
            to_engine_pos(end),
            engine_align(align),
        );
        self.undo.push(new_doc);
        self.dirty.invalidate(full_page_rect(self.scale()));
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        self.selection_changed()
    }

    /* ===========================================================
    Phase 5 PR 3 — table command dispatch.
    Each method maps the bridge `BlockPath` → engine, runs the
    mutation, pushes undo, repaints. Selection update is naive
    at PR 3 (caret stays put; PR 3b implements cell-rectangular
    selection refresh).
    =========================================================== */

    fn do_insert_table(&mut self, at: bridge::BlockPath, rows: u32, cols: u32) -> Event {
        let new_doc = self
            .undo
            .current()
            .insert_table(bridge_to_engine_path(at), rows, cols);
        self.push_table_edit(new_doc)
    }
    fn do_delete_table(&mut self, path: bridge::BlockPath) -> Event {
        let new_doc = self
            .undo
            .current()
            .delete_table(bridge_to_engine_path(path));
        self.push_table_edit(new_doc)
    }
    fn do_insert_row(&mut self, path: bridge::BlockPath, after_row: u32) -> Event {
        let new_doc = self
            .undo
            .current()
            .insert_row(bridge_to_engine_path(path), after_row);
        self.push_table_edit(new_doc)
    }
    fn do_delete_row(&mut self, path: bridge::BlockPath, row: u32) -> Event {
        let new_doc = self
            .undo
            .current()
            .delete_row(bridge_to_engine_path(path), row);
        self.push_table_edit(new_doc)
    }
    fn do_insert_column(&mut self, path: bridge::BlockPath, after_col: u32) -> Event {
        let new_doc = self
            .undo
            .current()
            .insert_column(bridge_to_engine_path(path), after_col);
        self.push_table_edit(new_doc)
    }
    fn do_delete_column(&mut self, path: bridge::BlockPath, col: u32) -> Event {
        let new_doc = self
            .undo
            .current()
            .delete_column(bridge_to_engine_path(path), col);
        self.push_table_edit(new_doc)
    }
    fn do_merge_cells(
        &mut self,
        path: bridge::BlockPath,
        from_row: u32,
        from_col: u32,
        to_row: u32,
        to_col: u32,
    ) -> Event {
        let new_doc = self.undo.current().merge_cells(
            bridge_to_engine_path(path),
            from_row,
            from_col,
            to_row,
            to_col,
        );
        self.push_table_edit(new_doc)
    }
    fn do_split_cell(&mut self, path: bridge::BlockPath, row: u32, col: u32) -> Event {
        let new_doc = self
            .undo
            .current()
            .split_cell(bridge_to_engine_path(path), row, col);
        self.push_table_edit(new_doc)
    }
    fn do_set_cell_shading(
        &mut self,
        path: bridge::BlockPath,
        row: u32,
        col: u32,
        color: Option<bridge::Color>,
    ) -> Event {
        let rgba = color.map(|c| [c.r, c.g, c.b, c.a]);
        let new_doc =
            self.undo
                .current()
                .set_cell_shading(bridge_to_engine_path(path), row, col, rgba);
        self.push_table_edit(new_doc)
    }
    fn do_set_cell_borders(
        &mut self,
        path: bridge::BlockPath,
        row: u32,
        col: u32,
        borders: bridge::BridgeCellBorders,
    ) -> Event {
        let new_doc = self.undo.current().set_cell_borders(
            bridge_to_engine_path(path),
            row,
            col,
            bridge_to_engine_borders(borders),
        );
        self.push_table_edit(new_doc)
    }

    /// Common tail for every table command — push undo, invalidate +
    /// repaint, fire a SelectionChanged event so the UI re-fetches
    /// state.
    fn push_table_edit(&mut self, new_doc: engine::DocumentTree) -> Event {
        self.undo.push(new_doc);
        self.dirty.invalidate(full_page_rect(self.scale()));
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        self.selection_changed()
    }
}

/// Bridge `BridgeCellBorders` → engine `CellBorders` (cell-level only;
/// `inside_h` / `inside_v` are not exposed on the wire — they apply
/// only at table level).
fn bridge_to_engine_borders(b: bridge::BridgeCellBorders) -> engine::CellBorders {
    engine::CellBorders {
        top: b.top.map(bridge_to_engine_stroke),
        left: b.left.map(bridge_to_engine_stroke),
        bottom: b.bottom.map(bridge_to_engine_stroke),
        right: b.right.map(bridge_to_engine_stroke),
        inside_h: None,
        inside_v: None,
    }
}

fn bridge_to_engine_stroke(s: bridge::BridgeBorderStroke) -> engine::BorderStroke {
    let style = match s.style {
        bridge::BridgeBorderStyle::Single => engine::BorderStyle::Single,
        bridge::BridgeBorderStyle::Double => engine::BorderStyle::Double,
        bridge::BridgeBorderStyle::Dotted => engine::BorderStyle::Dotted,
        bridge::BridgeBorderStyle::Dashed => engine::BorderStyle::Dashed,
        bridge::BridgeBorderStyle::None => engine::BorderStyle::None,
    };
    engine::BorderStroke {
        style,
        size_eighth_pt: s.size_eighth_pt,
        color: s.color.map(|c| [c.r, c.g, c.b, c.a]),
    }
}

/// Bridge `BlockPath` → engine `BlockPath`. Trivial 1:1 mapping; the
/// two crates carry parallel enums so neither has a build-time
/// dependency on the other's serde derives.
fn bridge_to_engine_path(p: bridge::BlockPath) -> engine::BlockPath {
    engine::BlockPath {
        steps: p
            .steps
            .into_iter()
            .map(|s| match s {
                bridge::PathStep::Block { idx } => engine::PathStep::Block(idx),
                bridge::PathStep::Cell { row, col } => engine::PathStep::Cell { row, col },
            })
            .collect(),
    }
}

struct RenderStats {
    page_width: f32,
    page_height: f32,
    line_count: u32,
    glyph_count: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::*;

    wasm_bindgen_test_configure!(run_in_browser);

    #[wasm_bindgen_test]
    async fn ping_pong_round_trip() {
        let mut engine = Engine {
            ctx: None,
            fonts: HashMap::new(),
            undo: UndoStack::new(DocumentTree::new(), 8),
            layout_cfg: None,
            atlas: GlyphAtlas::new(),
            vello: None,
            dirty: DirtyTracker::new(),
            selection: None,
            composition: None,
            pending_format: None,
            layout_cache: new_layout_cache(),
            a11y_cache: None,
        };
        let cmd_js = serde_wasm_bindgen::to_value(&Command::Ping).expect("encode ping");
        let evt_js = engine
            .dispatch(cmd_js)
            .await
            .expect("dispatch should succeed");
        let evt: Event = serde_wasm_bindgen::from_value(evt_js).expect("decode event");
        assert!(matches!(evt, Event::Pong), "expected Pong, got {evt:?}");
    }

    /// Backlog #7 — a line with one LTR run and one RTL run; a selection
    /// crossing the seam must yield two disjoint rects, not one box that
    /// over-covers the gap.
    #[test]
    fn selection_rects_split_at_bidi_seam() {
        let ltr = RunGeom {
            src_start: 0,
            src_end: 4,
            slots: vec![
                CaretSlot { x: 0.0, byte: 0 },
                CaretSlot { x: 10.0, byte: 1 },
                CaretSlot { x: 20.0, byte: 2 },
                CaretSlot { x: 30.0, byte: 3 },
                CaretSlot { x: 40.0, byte: 4 },
            ],
        };
        /* RTL run: byte order is the reverse of x order. */
        let rtl = RunGeom {
            src_start: 4,
            src_end: 8,
            slots: vec![
                CaretSlot { x: 90.0, byte: 4 },
                CaretSlot { x: 80.0, byte: 5 },
                CaretSlot { x: 70.0, byte: 6 },
                CaretSlot { x: 60.0, byte: 7 },
                CaretSlot { x: 50.0, byte: 8 },
            ],
        };
        let line = LineGeom {
            path: BridgeBlockPath::top(0),
            start_x: 0.0,
            hit_left: 0.0,
            hit_width: 100.0,
            y_top: 5.0,
            height: 20.0,
            start_byte: 0,
            end_byte: 8,
            slots: Vec::new(),
            runs: vec![ltr, rtl],
        };
        let rects = selection_rects_geom(&[line], &bpos_top(0, 2), &bpos_top(0, 6));
        assert_eq!(rects.len(), 2, "one rect per intersected run");
        let approx = |a: f32, b: f32| (a - b).abs() < 0.01;
        /* LTR clip [2,4): x 20..40. */
        assert!(approx(rects[0].x, 20.0), "rect0.x = {}", rects[0].x);
        assert!(approx(rects[0].w, 20.0), "rect0.w = {}", rects[0].w);
        /* RTL clip [4,6): bytes 4@90 and 6@70 → x 70..90. */
        assert!(approx(rects[1].x, 70.0), "rect1.x = {}", rects[1].x);
        assert!(approx(rects[1].w, 20.0), "rect1.w = {}", rects[1].w);
        /* The whole point: the two segments do not overlap. */
        assert!(rects[0].x + rects[0].w <= rects[1].x);
    }

    /// A non-BiDi line (one run) still yields exactly one rect — the
    /// single-run path matches the old per-line behaviour.
    #[test]
    fn selection_rects_single_run_one_rect() {
        let run = RunGeom {
            src_start: 0,
            src_end: 4,
            slots: vec![
                CaretSlot { x: 0.0, byte: 0 },
                CaretSlot { x: 10.0, byte: 1 },
                CaretSlot { x: 20.0, byte: 2 },
                CaretSlot { x: 30.0, byte: 3 },
                CaretSlot { x: 40.0, byte: 4 },
            ],
        };
        let line = LineGeom {
            path: BridgeBlockPath::top(0),
            start_x: 0.0,
            hit_left: 0.0,
            hit_width: 100.0,
            y_top: 5.0,
            height: 20.0,
            start_byte: 0,
            end_byte: 4,
            slots: Vec::new(),
            runs: vec![run],
        };
        let rects = selection_rects_geom(&[line], &bpos_top(0, 1), &bpos_top(0, 3));
        assert_eq!(rects.len(), 1);
        let approx = |a: f32, b: f32| (a - b).abs() < 0.01;
        assert!(approx(rects[0].x, 10.0));
        assert!(approx(rects[0].w, 20.0));
        assert!(approx(rects[0].y, 5.0));
        assert!(approx(rects[0].h, 20.0));
    }

    /// PR 4 / Bug 5 — three cells in the same row share `y_top`.
    /// `hit_test_geom` must pick the cell whose hit-rectangle
    /// contains the click's x, not the first-emitted one.
    #[test]
    fn hit_test_disambiguates_sibling_cells_by_x() {
        let cell_line = |cell_idx: u32, hit_left: f32, hit_width: f32| LineGeom {
            path: BridgeBlockPath {
                steps: vec![
                    BridgePathStep::Block { idx: 1 },
                    BridgePathStep::Cell {
                        row: 0,
                        col: cell_idx,
                    },
                    BridgePathStep::Block { idx: 0 },
                ],
            },
            start_x: hit_left,
            hit_left,
            hit_width,
            y_top: 100.0,
            height: 24.0,
            start_byte: 0,
            end_byte: 0,
            slots: Vec::new(),
            runs: Vec::new(),
        };
        let geom = vec![
            cell_line(0, 0.0, 150.0),
            cell_line(1, 150.0, 150.0),
            cell_line(2, 300.0, 150.0),
        ];
        /* Click inside cell C3 (x ∈ [300..450]). Must land on cell 2. */
        let hit = hit_test_geom(&geom, 400.0, 110.0);
        assert_eq!(hit.path.steps.len(), 3);
        let BridgePathStep::Cell { col, .. } = hit.path.steps[1] else {
            panic!("expected Cell step");
        };
        assert_eq!(col, 2, "click in C3 must land in column 2, not 0");
    }

    /// Backlog #2 — a synthetic glyph (an injected Kashida Tatweel) advances
    /// the pen but emits no caret slot, so the byte<->glyph map stays intact.
    #[test]
    fn synthetic_glyphs_emit_no_caret_slot() {
        let attrs = layout::TextAttrs {
            px_size: 16.0,
            color: [0, 0, 0, 255],
            faux_bold: false,
            faux_italic: false,
            underline: false,
            strike: false,
            bg_color: None,
        };
        let glyph = |cluster: u32, adv: f32, synthetic: bool| layout::PositionedGlyph {
            id: 1,
            cluster,
            x_advance: adv,
            y_advance: 0.0,
            x_offset: 0.0,
            y_offset: 0.0,
            synthetic,
        };
        let run = layout::VisualRun {
            glyphs: vec![
                glyph(0, 10.0, false), // real letter at byte 0
                glyph(0, 7.0, true),   // injected Tatweel — same cluster, no slot
                glyph(1, 10.0, false), // real letter at byte 1
            ],
            font: "f".to_string(),
            direction: ShapingDirection::Ltr,
            source_range: 0..2,
            attrs,
        };
        let line = LineBox {
            origin: Point { x: 0.0, y: 0.0 },
            baseline: 0.0,
            height: 20.0,
            width: 27.0,
            runs: vec![run],
            alignment: Alignment::Start,
        };
        let geom = build_line_run_geom(&line, 0.0);
        assert_eq!(geom.len(), 1);
        let slots = &geom[0].slots;
        let approx = |a: f32, b: f32| (a - b).abs() < 0.01;
        /* Two real glyphs + the run-end slot — the Tatweel is skipped. */
        assert_eq!(slots.len(), 3);
        assert_eq!((slots[0].byte, slots[1].byte, slots[2].byte), (0, 1, 2));
        /* Byte 1's slot is still pushed right by the Tatweel's 7px advance. */
        assert!(approx(slots[0].x, 0.0));
        assert!(approx(slots[1].x, 17.0));
        assert!(approx(slots[2].x, 27.0));
    }

    /// Backlog #13 — the layout cache key is stable for identical input and
    /// distinct when the text, alignment, or scale changes.
    #[test]
    fn paragraph_layout_key_is_content_sensitive() {
        let cfg = RenderConfig {
            font_id: "f".to_string(),
            base_direction: ShapingDirection::Ltr,
            px_size: 16.0,
            line_height: 24.0,
            alignment: Alignment::Start,
            scale: 1.0,
        };
        let para = |text: &str| engine::Paragraph {
            text: text.to_string(),
            spans: Vec::new(),
            props: engine::ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            dirty: false,
            source_xml: None,
        };
        let a = para("hello world");
        /* Identical content + config -> identical key. */
        assert_eq!(
            paragraph_layout_key(&a, &cfg, 1.0),
            paragraph_layout_key(&a, &cfg, 1.0),
        );
        /* Different text -> different key. */
        assert_ne!(
            paragraph_layout_key(&a, &cfg, 1.0),
            paragraph_layout_key(&para("hello there"), &cfg, 1.0),
        );
        /* A paragraph alignment override -> different key. */
        let mut centered = para("hello world");
        centered.props.alignment = Some(EngineAlignment::Center);
        assert_ne!(
            paragraph_layout_key(&a, &cfg, 1.0),
            paragraph_layout_key(&centered, &cfg, 1.0),
        );
        /* A different device scale -> different key. */
        assert_ne!(
            paragraph_layout_key(&a, &cfg, 1.0),
            paragraph_layout_key(&a, &cfg, 2.0),
        );
    }

    /// Build a single-run accessibility paragraph node for the `diff_a11y`
    /// tests.
    fn a11y_para(text: &str) -> A11yNode {
        A11yNode::Paragraph(A11yParagraph {
            direction: Direction::Ltr,
            runs: vec![A11yRun {
                text: text.to_string(),
                bold: false,
                italic: false,
                underline: false,
            }],
        })
    }

    #[test]
    fn diff_a11y_identical_is_empty() {
        let tree = vec![a11y_para("a"), a11y_para("b")];
        assert_eq!(diff_a11y(&tree, &tree), vec![]);
    }

    #[test]
    fn diff_a11y_typing_emits_one_update() {
        /* A keystroke in paragraph 1 — the rest of the document is untouched. */
        let prev = vec![a11y_para("a"), a11y_para("b"), a11y_para("c")];
        let next = vec![a11y_para("a"), a11y_para("bX"), a11y_para("c")];
        assert_eq!(
            diff_a11y(&prev, &next),
            vec![A11yPatch::Update {
                index: 1,
                node: a11y_para("bX"),
            }],
        );
    }

    #[test]
    fn diff_a11y_split_emits_update_then_insert() {
        /* Enter inside paragraph 1: "bc" -> "b" + "c". Paragraph "d" shifts
        down a slot but its content is unchanged, so it is not re-emitted. */
        let prev = vec![a11y_para("a"), a11y_para("bc"), a11y_para("d")];
        let next = vec![
            a11y_para("a"),
            a11y_para("b"),
            a11y_para("c"),
            a11y_para("d"),
        ];
        assert_eq!(
            diff_a11y(&prev, &next),
            vec![
                A11yPatch::Update {
                    index: 1,
                    node: a11y_para("b"),
                },
                A11yPatch::Insert {
                    index: 2,
                    node: a11y_para("c"),
                },
            ],
        );
    }

    #[test]
    fn diff_a11y_merge_emits_update_then_remove() {
        /* Backspace at the start of paragraph 2: "b" + "c" -> "bc". */
        let prev = vec![
            a11y_para("a"),
            a11y_para("b"),
            a11y_para("c"),
            a11y_para("d"),
        ];
        let next = vec![a11y_para("a"), a11y_para("bc"), a11y_para("d")];
        assert_eq!(
            diff_a11y(&prev, &next),
            vec![
                A11yPatch::Update {
                    index: 1,
                    node: a11y_para("bc"),
                },
                A11yPatch::Remove { index: 2 },
            ],
        );
    }

    #[test]
    fn diff_a11y_append_emits_insert_only() {
        let prev = vec![a11y_para("a")];
        let next = vec![a11y_para("a"), a11y_para("b")];
        assert_eq!(
            diff_a11y(&prev, &next),
            vec![A11yPatch::Insert {
                index: 1,
                node: a11y_para("b"),
            }],
        );
    }

    /// Backlog #8 — the IME composition is spliced into the layout spans:
    /// committed spans shift past it, the composition itself is underlined.
    #[test]
    fn composition_spans_splice_and_underline() {
        let p = engine::Paragraph {
            text: "abcdef".to_string(),
            spans: Vec::new(),
            props: engine::ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            dirty: false,
            source_xml: None,
        };
        /* Compose 3 bytes at offset 3 — splits the one committed span. */
        let spans = composition_layout_spans(&p, 3, 3, 16.0, 1.0);
        assert_eq!(spans.len(), 3, "split + composition span");
        assert_eq!((spans[0].start, spans[0].end), (0, 3));
        assert!(!spans[0].underline);
        assert_eq!((spans[1].start, spans[1].end), (3, 6));
        assert!(spans[1].underline, "composition span must be underlined");
        assert_eq!((spans[2].start, spans[2].end), (6, 9));
        assert!(!spans[2].underline);
    }

    /// Composing at the end of a paragraph appends an underlined span past
    /// the committed text — no split needed.
    #[test]
    fn composition_spans_at_paragraph_end() {
        let p = engine::Paragraph {
            text: "abc".to_string(),
            spans: Vec::new(),
            props: engine::ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            dirty: false,
            source_xml: None,
        };
        let spans = composition_layout_spans(&p, 3, 2, 16.0, 1.0);
        assert_eq!(spans.len(), 2);
        assert_eq!((spans[0].start, spans[0].end), (0, 3));
        assert_eq!((spans[1].start, spans[1].end), (3, 5));
        assert!(spans[1].underline);
    }
}
