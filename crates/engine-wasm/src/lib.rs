//! `engine-wasm` — `#[wasm_bindgen]` surface for the engine.
//!
//! Phase 1 weeks 15–24: document model (engine crate, `im::Vector`-backed) +
//! undo/redo + `.docx` load/save + InsertText that triggers an automatic
//! repaint when a layout config was cached by a prior `RenderPage`.

use bridge::{
    A11yCell, A11yNode, A11yParagraph, A11yPatch, A11yRow, A11yRun, A11yTable, A11yTree,
    Alignment as BridgeAlignment, AnnouncementPriority, BlockPath as BridgeBlockPath,
    BridgeBorderStroke, BridgeBorderStyle, BridgeCellBorders, BridgeCellProperties, BridgeIndent,
    BridgeSectionGeometry, BridgeStyleProperties, Color, Command, Direction, DocFormat,
    EngineStats, Event, FontMetrics as BridgeMetrics, ImageBlob as BridgeImageBlob, ImageFit,
    LogicalPos as BridgeLogicalPos, LogicalRange as BridgeLogicalRange, MoveDirection,
    PageOrientation as BridgePageOrientation, PathStep as BridgePathStep, PdfConformance,
    Point as BridgePoint, Rect as BridgeRect, SectionBreakKind, SelectionKind, TextAttrs,
    TextAttrsPatch, UnderlineStyle, VerticalScript,
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
    /// Effective device scale — always `base_scale × zoom`. Layout + paint
    /// are scaled by this; `px_size` and `line_height` stay logical (the
    /// toolbar + document model use them raw).
    scale: f32,
    /// Boot device scale — `RenderPage.device_pixel_ratio` (the shell
    /// passes `devicePixelRatio × 4/3`). Never touched by `SetZoom`;
    /// keeping it separate is what lets zoom `1.0` restore the exact
    /// boot rendering instead of clobbering DPI scaling.
    base_scale: f32,
    /// User zoom fraction set by `SetZoom`, clamped to `[0.25, 4.0]`.
    zoom: f32,
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
/// committed on a committing end. The on-canvas inline preview shipped in
/// Phase 5 sprint 11 (`composition_layout_spans`).
#[derive(Clone)]
struct CompositionState {
    at: BridgeLogicalPos,
    text: String,
}

/// Audit gap C.H1 — viewport-culled lazy pagination state. Owned by
/// `Engine`; mutated by `SetViewport` (the TS shell records the scrolled
/// viewport band) and `ExpandLayout` (the shell asks the engine to flow
/// blocks down to a deeper Y). The Y coordinates are layout pixels at
/// the active device scale, measured from the document top.
///
/// `min_target_y` is the high-water mark of how deep the paginator has
/// been *asked* to lay out — `build_pages` keeps emitting pages while
/// the running document height is below it (plus a buffer). It only
/// ever rises within one document's session; a document swap
/// (`load_docx_bytes`) and crash recovery restart it at the cold-open
/// band so the new doc's first paint is bounded.
#[derive(Clone, Copy)]
struct LazyLayoutState {
    viewport_y: f32,
    viewport_h: f32,
    /// Lower bound the paginator must cover on its next run. Floor is
    /// `INITIAL_COLD_OPEN_BUDGET_PT` so a fresh / post-edit engine
    /// always lays out the visible band even if the TS shell hasn't
    /// posted a viewport yet.
    min_target_y: f32,
}

impl Default for LazyLayoutState {
    fn default() -> Self {
        Self {
            viewport_y: 0.0,
            viewport_h: INITIAL_COLD_OPEN_BUDGET_PT,
            min_target_y: INITIAL_COLD_OPEN_BUDGET_PT,
        }
    }
}

/// Audit gap C.H1 — how far below the viewport bottom the paginator
/// keeps pre-laying out, in layout pt at scale=1. One-and-a-half
/// page-heights of slack so a slow scroll doesn't visibly pause while
/// the next page renders. The TS shell can still drive deeper by
/// calling `ExpandLayout` directly.
const LAYOUT_BUFFER_PT: f32 = 1200.0;

/// Audit gap C.H1 — cold-open default budget, in layout pt at scale=1.
/// Roughly two A4 pages tall (842 × 2 ≈ 1684); the paginator processes
/// blocks until the running document height clears this much without
/// waiting for the TS shell to register a viewport. Picked so the
/// first paint after `LoadDocx` is bounded regardless of total document
/// length.
const INITIAL_COLD_OPEN_BUDGET_PT: f32 = 1684.0;

/// Audit gap C.H1 — virtual-height fallback per *unlaid-out* body
/// block, in layout pt at scale=1. Tuned against the perf-fixtures
/// corpus: a typical .docx body block at 12 pt averages ~18 pt per
/// line with ~3 lines per paragraph, so ~54 pt; round up so the
/// scrollbar slightly *over-* rather than *under-* estimates and
/// background completion only ever shrinks the scroll range. (An
/// estimate that grew would yank the scroll thumb downward on every
/// page filled in.)
const AVG_BLOCK_HEIGHT_PT: f32 = 64.0;

/// L3.1 (#5) — predictive runway past the requested `target_y` for the
/// lazy-pagination cull budget. Returns the larger of:
///
/// * `LAYOUT_BUFFER_PT` — the legacy static buffer; protects clients
///   that haven't posted a viewport yet (initial cold open, headless
///   harness) and small viewports (mobile) where doubling the band
///   would still fall under the legacy floor.
/// * `viewport_h * scale * 2.0` — two viewport-heights ahead of the
///   target. A fast-flick scroll up to one viewport-height past the
///   current band still hits already-laid-out pages, eliminating the
///   blank-frame window on hard scrolls.
///
/// Units: `viewport_h` is layout pt at scale=1 (what `do_set_viewport`
/// records); the multiplication by `scale` converts to device px so
/// the result is directly addable to the device-px `target_y` cull
/// arithmetic.
fn lazy_runway(viewport_h_pt: f32, scale: f32) -> f32 {
    LAYOUT_BUFFER_PT.max(viewport_h_pt * scale * 2.0)
}

/// Audit gap C.H1 — pagination-completion info that rides alongside
/// the laid-out pages. The caller folds this into `Painted` so the TS
/// shell knows (a) whether more pages may materialize via
/// `ExpandLayout`, and (b) the running virtual-height estimate that
/// drives the scrollbar's backing store.
#[derive(Clone, Copy)]
struct LazyLayoutInfo {
    /// `true` when every body block was consumed; `false` when the
    /// viewport-cull budget halted the paginator early.
    is_full_layout: bool,
    /// Number of top-level blocks across the doc that have not yet
    /// been processed (paragraph or table). Drives the height estimate.
    remaining_blocks: u32,
}

/// Issue #34/#51 — memo of the most recent `build_pages` output. Every
/// interactive command used to run TWO identical band layouts (the
/// auto-repaint, then `selection_changed → document_geometry`), and a
/// pointer click ran up to three. The memo hands the second and third
/// consumers the first run's boxes.
///
/// Validity is (document revision, scale, cull target, live-composition
/// flag). The revision comes from [`UndoStack::revision`] — the single
/// chokepoint every document mutation passes through. The two states
/// the revision cannot see are handled explicitly: swapping the whole
/// stack (`load_docx_bytes` / `do_recover` / `RenderPage` seed) and
/// non-document layout inputs (font set, IME composition text) call
/// [`Engine::invalidate_layout_snapshot`] directly.
///
/// Memory: holds one laid-out band (a few pages in the lazy steady
/// state) — the same boxes `build_pages` was already allocating per
/// call, now retained between commands instead of rebuilt.
struct LayoutSnapshot {
    doc_revision: u64,
    scale_bits: u32,
    target_bits: Option<u32>,
    /// `lazy_layout.viewport_h` feeds the cull budget via `lazy_runway`,
    /// so a viewport resize must miss the memo like any other layout
    /// input change.
    viewport_h_bits: u32,
    composition_active: bool,
    pages: Vec<PageBox>,
    page_paths: Vec<Vec<EngineBlockPath>>,
    info: LazyLayoutInfo,
}

/// Cached dimensions from the most recent `render_document`, replayed by
/// the worker's synthetic `Painted` side-channel after every mutating
/// command. Carries `estimated_document_height` + `is_full_layout` so
/// the TS shell's scrollbar resizes against the virtual estimate, not
/// just the laid-out tail.
#[derive(Clone, Default)]
struct LastPaintDims {
    document_height: f32,
    page_count: u32,
    estimated_document_height: f32,
    is_full_layout: bool,
    /// Issue #26 — per-page absolute tops + heights (device px),
    /// re-broadcast on synthetic `Painted`s so overlays stay exact
    /// between real paints.
    page_tops: Vec<f32>,
    page_heights: Vec<f32>,
    /// Phase 3 (#39) — per-page top/bottom margins (device px) for the
    /// shell's header/footer double-click zone gate.
    page_margin_tops: Vec<f32>,
    page_margin_bottoms: Vec<f32>,
}

/// A candidate caret position on a line — an absolute x (canvas device px)
/// paired with the source byte offset a caret there maps to.
#[derive(Clone, Copy)]
struct CaretSlot {
    x: f32,
    byte: u32,
}

/// Caret affinity at a BiDi seam — see `SelectionState::affinity`. The
/// default is `LeadingX` so a fresh selection / pointer-set caret
/// renders at the smaller-x slot (consistent with the existing
/// `slot_x_for_byte` choice).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum CaretAffinity {
    #[default]
    LeadingX,
    TrailingX,
}

/// Phase 3 (#39) — which content story caret-relative commands address.
/// While a header/footer story is active, `SelectionState` paths are
/// STORY-relative (`Block(i)` = the story's i-th paragraph), the
/// geometry pipeline projects onto the anchor page's band, and every
/// document mutation routes through the story adapter
/// (`commit_story_edit`) instead of the body tree.
#[derive(Clone, PartialEq, Eq, Default)]
enum StoryTarget {
    #[default]
    Body,
    Header {
        /// Relationship id of the part being edited.
        rid: String,
        /// Anchor page (0-based) — where caret/selection geometry lives.
        page: u32,
        /// A top-level block index inside the OWNING section, kept so
        /// section-scoped reads (`section_geometry_for_caret`) can
        /// resolve the section while selection paths are story-rooted.
        section_block: u32,
    },
    Footer {
        rid: String,
        page: u32,
        section_block: u32,
    },
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
    /// Phase 6c multi-canvas refactor — one entry per page (`None` for
    /// pages whose TS-side canvas has not been transferred yet). Index
    /// `0` is the boot canvas; later indexes are filled by
    /// `set_page_canvas` as the TS shell mounts more `<canvas>`
    /// elements for additional pages.
    page_ctxs: Vec<Option<OffscreenCanvasRenderingContext2d>>,
    /// Legacy accessor — `page_ctxs[0]` for backwards compatibility
    /// with code paths that still expect the single-canvas world.
    /// Internal mirror only; never set independently.
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
    /// Issue #34/#51 — memo of the last `build_pages` run (see
    /// [`LayoutSnapshot`]). `RefCell` because `document_geometry` and
    /// friends populate it behind `&self`.
    layout_snapshot: RefCell<Option<LayoutSnapshot>>,
    /// Phase 3 (#40) — revision-keyed memo of the derived
    /// `effective_sections()` table. Sections now derive from paragraph
    /// markers by walking the block list, and the lookup sits on the
    /// per-keystroke `SelectionChanged` path
    /// (`section_for_block_cached`), so cache by undo revision — same
    /// pattern as `layout_snapshot`, cleared at the same out-of-band
    /// chokepoint (`invalidate_layout_snapshot`).
    sections_memo: RefCell<Option<(u64, Vec<engine::Section>)>>,
    /// Phase 3 (#39) — the active content story. `Body` outside
    /// header/footer editing.
    active_story: StoryTarget,
    /// Phase 3 (#39) — the body selection captured on
    /// `EnterHeaderFooter`, restored (clamped) on `ExitHeaderFooter`.
    stashed_body_selection: Option<SelectionState>,
    /// Last accessibility tree broadcast to the UI (Backlog #10).
    /// `build_a11y_delta` diffs the freshly built tree against this so a
    /// keystroke emits only the changed paragraph. `None` until the first
    /// delta — that one is a full `Replace`, so a fresh engine after crash
    /// recovery hands the mirror a clean rebuild.
    a11y_cache: Option<Vec<A11yNode>>,
    /// Phase 7 — decoded inline-image cache keyed by archive relationship
    /// id. The TS shell decodes every `word/media/*` blob into an
    /// `ImageBitmap` after the document loads and installs the result
    /// here via the direct wasm method [`Engine::register_image`]. The
    /// Canvas2D backend looks each painted inline image up here; a miss
    /// falls back to a placeholder.
    image_cache: HashMap<String, web_sys::ImageBitmap>,
    /// Last `render_document` output dimensions, cached so the worker
    /// can post a synthetic `Painted` side-channel after every mutating
    /// command without re-rendering. The TS shell drives the CSS
    /// `.editor-page` height off this so multi-page documents scroll
    /// correctly after typing (Phase 6b documented this wire but only
    /// the `REQUEST_PAINT` path emitted `Painted` — mutating commands
    /// rendered but stayed silent on the dims).
    last_paint_dims: LastPaintDims,
    /// Audit gap C.H1 — viewport-culled lazy pagination state. The TS
    /// shell drives this via `SetViewport` (records the visible band
    /// the user is scrolled to) and `ExpandLayout` (asks the engine to
    /// lay out down to a target Y). The paginator stops generating
    /// pages once the laid-out tail covers `viewport_y + viewport_h +
    /// LAYOUT_BUFFER_PT`. Cold open with a 50-page document touches
    /// only the first few pages instead of every block — the document
    /// height the TS shell sees on the first paint is an estimate; it
    /// converges to the real height as `ExpandLayout` calls fill in
    /// the tail (and the scrollbar never jumps because the estimate
    /// is always ≥ the real running total).
    lazy_layout: LazyLayoutState,
    /// Caret affinity at a BiDi seam (UX_BEHAVIOR_SPEC §III.5). When
    /// the caret offset lands on a byte with TWO valid visual slots —
    /// the end of one directional run and the start of the next —
    /// `caret_rect_geom` consults this flag to pick which side
    /// renders. The visual-step arrow sets it on each motion
    /// (`Right` → `TrailingX`, `Left` → `LeadingX`); a non-arrow
    /// selection change (click, SET_SELECTION, SelectAll, paste,
    /// type) resets to `LeadingX`. Stored on `Engine` rather than
    /// `SelectionState` so the 13+ places that mint a `SelectionState`
    /// struct literal stay untouched.
    caret_affinity: CaretAffinity,
    /// Sprint 10 — `aria-live` announcements queued by user-visible
    /// mutation handlers ("Aligned center", "Page break inserted", …).
    /// The worker drains this after each command via
    /// `Engine::drain_announcements` and posts each entry as a
    /// broadcast `Event::Announcement` so the TS shell's `aria-live`
    /// region narrates engine actions in order.
    pending_announcements: Vec<(AnnouncementPriority, String)>,
    /// Sprint 14 (#14) — track-changes RECORDING flag. When `true`,
    /// every text-mutation handler gates its work into a tracked
    /// `Revision` instead of mutating in place. Toggled via
    /// `Command::ToggleTrackChanges`; reflected on every
    /// `Event::SelectionChanged.is_tracking_changes` so the UI's
    /// toggle stays in sync with engine state.
    tracking_changes: bool,
    /// Sprint 14 (#14) — review identity stamped onto every tracked
    /// revision the engine synthesises (`w:author` / `w:date`).
    /// Default `author = "You"`; set via `Command::SetReviewIdentity`.
    review_author: String,
    /// Sprint 14 (#14) — review date for synthesised revisions.
    /// Empty until the worker stamps `Date.now()` at command time.
    review_date: String,
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
    let page_ctxs = vec![ctx.clone()];
    Engine {
        page_ctxs,
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
        layout_snapshot: RefCell::new(None),
        sections_memo: RefCell::new(None),
        active_story: StoryTarget::Body,
        stashed_body_selection: None,
        a11y_cache: None,
        image_cache: HashMap::new(),
        last_paint_dims: LastPaintDims::default(),
        lazy_layout: LazyLayoutState::default(),
        caret_affinity: CaretAffinity::default(),
        pending_announcements: Vec::new(),
        tracking_changes: false,
        review_author: "You".to_string(),
        review_date: String::new(),
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

    /// Phase 7 — list every inline-image media blob the document carries,
    /// keyed by archive relationship id (`r:id`). The TS shell consumes
    /// this list once after `OpenDocx`, decodes each blob into an
    /// `ImageBitmap` via the browser, and installs the result via
    /// [`Engine::register_image`]. Returns an array of
    /// `{ rel_id, mime, bytes }` objects.
    pub fn media_entries(&self) -> Result<JsValue, JsValue> {
        let doc = self.undo.current();
        let entries: Vec<MediaEntryOut> = doc
            .media
            .iter()
            .map(|(rid, blob)| MediaEntryOut {
                rel_id: rid.clone(),
                mime: blob.content_type.clone(),
                bytes: blob.data.clone(),
            })
            .collect();
        serde_wasm_bindgen::to_value(&entries)
            .map_err(|e| JsValue::from_str(&format!("encode media entries: {e}")))
    }

    /// Phase 7 — install a decoded inline-image bitmap. Idempotent; later
    /// registrations overwrite earlier ones (lets the worker re-decode on
    /// DPR change without leaking).
    pub fn register_image(&mut self, rel_id: String, bitmap: web_sys::ImageBitmap) {
        self.image_cache.insert(rel_id, bitmap);
    }

    /// Phase 6c — multi-canvas DOM refactor. Register an `OffscreenCanvas`
    /// for page `idx`. The TS shell calls this whenever it mounts a new
    /// `<canvas>` for a paginated page, transferring the surface to the
    /// worker so each page draws into its own DOM element. The
    /// previous single-canvas architecture grew one giant canvas as the
    /// document grew, hitting Safari's 4096 px and Chrome's 32 k height
    /// limits on long documents.
    pub fn set_page_canvas(
        &mut self,
        idx: u32,
        canvas: web_sys::OffscreenCanvas,
    ) -> Result<(), JsValue> {
        let ctx_obj = canvas
            .get_context("2d")?
            .ok_or_else(|| JsValue::from_str("OffscreenCanvas 2d context unavailable"))?;
        let ctx: OffscreenCanvasRenderingContext2d = ctx_obj.dyn_into()?;
        let target = idx as usize;
        while self.page_ctxs.len() <= target {
            self.page_ctxs.push(None);
        }
        self.page_ctxs[target] = Some(ctx);
        if target == 0 {
            self.ctx = self.page_ctxs[0].clone();
        }
        Ok(())
    }

    /// Last `render_document` output dimensions, exposed so the worker
    /// can side-channel a synthetic `Painted` event after every mutating
    /// command. Returns `{document_height, page_count}` (device px). The
    /// TS shell uses `document_height` to size `.editor-page`'s CSS so
    /// multi-page documents grow + scroll instead of getting squashed
    /// vertically into a single A4 box.
    pub fn paint_dims(&self) -> Result<JsValue, JsValue> {
        let dims = self.last_paint_dims.clone();
        serde_wasm_bindgen::to_value(&PaintDimsOut {
            document_height: dims.document_height,
            page_count: dims.page_count,
            estimated_document_height: dims.estimated_document_height,
            is_full_layout: dims.is_full_layout,
            page_tops: dims.page_tops,
            page_heights: dims.page_heights,
            image_count: self.undo.current().count_inline_images(),
            page_margin_tops: dims.page_margin_tops,
            page_margin_bottoms: dims.page_margin_bottoms,
        })
        .map_err(|e| JsValue::from_str(&format!("encode paint dims: {e}")))
    }

    /// Sprint 10 — drain queued `aria-live` announcements as
    /// `Event::Announcement` payloads. The worker calls this after
    /// every command and posts each entry as a broadcast event so the
    /// `Announcements.tsx` `aria-live` region narrates engine
    /// actions in dispatch order. Returns an array of events; empty
    /// when no announcement was queued.
    pub fn drain_announcements(&mut self) -> Result<JsValue, JsValue> {
        let drained: Vec<Event> = self
            .pending_announcements
            .drain(..)
            .map(|(priority, message)| Event::Announcement { priority, message })
            .collect();
        serde_wasm_bindgen::to_value(&drained)
            .map_err(|e| JsValue::from_str(&format!("encode announcements: {e}")))
    }

    /// Phase 8b — flat snapshot of every tracked-change revision the
    /// document carries. Each row pairs a `(block, start_offset,
    /// end_offset)` byte range with the revision's kind (`Insert` /
    /// `Delete`), author, and date. The TS shell uses this to render
    /// hover tooltips over revision-marked text in the canvas.
    pub fn revisions_snapshot(&self) -> Result<JsValue, JsValue> {
        let doc = self.undo.current();
        let mut rows: Vec<RevisionOut> = Vec::new();
        for (block_idx, block) in doc.blocks.iter().enumerate() {
            if let engine::Block::Paragraph(p) = block {
                for r in &p.revisions {
                    rows.push(RevisionOut {
                        block: block_idx as u32,
                        start: r.start,
                        end: r.end,
                        kind: match r.kind {
                            engine::RevisionKind::Insert => "insert",
                            engine::RevisionKind::Delete => "delete",
                            engine::RevisionKind::FormatChange => "format",
                        },
                        author: r.author.clone(),
                        date: r.date.clone(),
                    });
                }
            }
        }
        serde_wasm_bindgen::to_value(&rows)
            .map_err(|e| JsValue::from_str(&format!("encode revisions: {e}")))
    }

    /// Phase 8a — flat snapshot of every parsed `<w:comment>` plus the
    /// document-side `<w:commentRangeStart>` / `<w:commentRangeEnd>`
    /// span. The TS shell renders these in a sidebar; no canvas
    /// drawing of comment overlays in this MVP.
    pub fn comments_snapshot(&self) -> Result<JsValue, JsValue> {
        let doc = self.undo.current();
        let comments: Vec<CommentOut> = doc
            .comment_ranges
            .iter()
            .map(|r| {
                let def = doc.comment_defs.get(&r.id).cloned().unwrap_or_default();
                CommentOut {
                    id: r.id,
                    author: def.author,
                    date: def.date,
                    text: def.paragraphs.join("\n"),
                    resolved: def.resolved,
                    parent_id: def.parent_id,
                    start_block: r.start.path.last_block_index().unwrap_or(0),
                    start_offset: r.start.offset,
                    end_block: r.end.path.last_block_index().unwrap_or(0),
                    end_offset: r.end.offset,
                }
            })
            .collect();
        serde_wasm_bindgen::to_value(&comments)
            .map_err(|e| JsValue::from_str(&format!("encode comments: {e}")))
    }
}

#[derive(::serde::Serialize)]
struct PaintDimsOut {
    document_height: f32,
    page_count: u32,
    /// Audit gap C.H1 — virtual scrollbar height the TS shell should
    /// size its backing CSS to. Equal to `document_height` once
    /// `is_full_layout` is `true`.
    estimated_document_height: f32,
    /// Audit gap C.H1 — `true` once the paginator has consumed every
    /// body block, `false` while a viewport-cull budget is still
    /// holding back the tail.
    is_full_layout: bool,
    /// Issue #26 — per-page absolute tops + heights in device px.
    page_tops: Vec<f32>,
    page_heights: Vec<f32>,
    /// Issue #44 — inline-image count, so the broadcast PAINTED can gate
    /// the shell's `GetImageRects` refresh (mirrors `Event::Painted`).
    image_count: u32,
    /// Phase 3 (#39) — per-page margins for the shell's header/footer
    /// zone gate (mirrors `Event::Painted`).
    page_margin_tops: Vec<f32>,
    page_margin_bottoms: Vec<f32>,
}

#[derive(::serde::Serialize)]
struct RevisionOut {
    block: u32,
    start: u32,
    end: u32,
    kind: &'static str,
    author: String,
    date: String,
}

#[derive(::serde::Serialize)]
struct CommentOut {
    id: u32,
    author: String,
    date: String,
    text: String,
    /// Sprint 9 — the `<w15:commentEx w15:done>` flag; the UI's
    /// resolve/reopen toggle reads this to invert the state.
    resolved: bool,
    /// Issue #27 — `Some(parent w:id)` marks this row as a threaded
    /// reply; `None` (serialized `undefined`) marks a top-level
    /// comment. The rail groups rows into threads on this field.
    parent_id: Option<u32>,
    start_block: u32,
    start_offset: u32,
    end_block: u32,
    end_offset: u32,
}

/// Serialization surface for [`Engine::media_entries`]. Mirrors
/// `bridge::ImageBlob`'s on-wire shape but keyed by `rel_id` so the TS
/// shell can route decoded bitmaps back via [`Engine::register_image`].
#[derive(::serde::Serialize)]
struct MediaEntryOut {
    rel_id: String,
    mime: String,
    #[serde(with = "serde_bytes")]
    bytes: Vec<u8>,
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
    if n_rows == 0 || table.rows.iter().all(|r| r.cells.is_empty()) {
        return caret.clone();
    }
    /* Row-aware traversal over CELL indices (merged rows carry fewer
    cells than the table has grid columns), skipping vMerge-Continue
    landing spots — Word tabs through visible cells only. */
    let row_cols = |undo: &UndoStack, rr: u32| -> u32 {
        undo.current()
            .table_at_path(&engine_table)
            .and_then(|t| t.rows.get(rr as usize))
            .map(|row| row.cells.len() as u32)
            .unwrap_or(0)
    };
    let is_continue = |undo: &UndoStack, rr: u32, cc: u32| -> bool {
        undo.current()
            .table_at_path(&engine_table)
            .and_then(|t| t.rows.get(rr as usize))
            .and_then(|row| row.cells.get(cc as usize))
            .is_some_and(|cell| matches!(cell.props.v_merge, engine::VMergeRole::Continue))
    };
    let (mut rr, mut cc) = (r, c);
    let (next_r, next_c) = loop {
        if forward {
            if cc + 1 < row_cols(undo, rr) {
                cc += 1;
            } else if rr + 1 < n_rows {
                rr += 1;
                cc = 0;
            } else {
                /* Last cell + forward → append a fresh row and land on
                its first cell (Word default). `at = n_rows` lands after
                the current last row under the post-hotfix signature. */
                let new_doc = undo
                    .current()
                    .insert_row(bridge_to_engine_path(table_path.clone()), n_rows as usize);
                undo.push(new_doc);
                break (n_rows, 0);
            }
        } else if cc > 0 {
            cc -= 1;
        } else if rr > 0 {
            rr -= 1;
            cc = row_cols(undo, rr).saturating_sub(1);
        } else {
            return caret.clone();
        }
        if !is_continue(undo, rr, cc) {
            break (rr, cc);
        }
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

/// Bridge `UnderlineStyle` ↔ engine `UnderlineStyle` — same variant
/// names; mapper just keeps the two enum types from leaking into each
/// other's crates.
/// Phase 2 audit (gap A.12) — for each `\u{000C}` (FORM FEED) byte in
/// `text`, find the index of the [`ParagraphBox`] line whose source
/// range covers that byte. Returns the list deduped + sorted so the
/// paginator's split walks them in document order.
///
/// A line "covers" a byte when any of its `VisualRun`s has a
/// `source_range` whose `[start, end)` straddles the FORM FEED's byte
/// position. The U+000C glyph itself shapes to .notdef in most fonts
/// (zero advance, no visible mark), so the line carrying it ends
/// naturally at the mandatory break ICU inserted at that position.
fn compute_page_break_lines(text: &str, para_box: &ParagraphBox) -> Vec<usize> {
    let ff_positions: Vec<usize> = text
        .char_indices()
        .filter_map(|(i, c)| (c == '\u{000C}').then_some(i))
        .collect();
    if ff_positions.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<usize> = Vec::new();
    for (line_idx, line) in para_box.lines.iter().enumerate() {
        for run in &line.runs {
            let lo = run.source_range.start as usize;
            let hi = run.source_range.end as usize;
            for &pos in &ff_positions {
                if pos >= lo && pos < hi && !out.contains(&line_idx) {
                    out.push(line_idx);
                }
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

fn bridge_to_engine_underline(b: UnderlineStyle) -> engine::UnderlineStyle {
    match b {
        UnderlineStyle::None => engine::UnderlineStyle::None,
        UnderlineStyle::Single => engine::UnderlineStyle::Single,
        UnderlineStyle::Double => engine::UnderlineStyle::Double,
        UnderlineStyle::Dotted => engine::UnderlineStyle::Dotted,
        UnderlineStyle::Dashed => engine::UnderlineStyle::Dashed,
        UnderlineStyle::Wavy => engine::UnderlineStyle::Wavy,
    }
}

fn engine_to_bridge_underline(e: engine::UnderlineStyle) -> UnderlineStyle {
    match e {
        engine::UnderlineStyle::None => UnderlineStyle::None,
        engine::UnderlineStyle::Single => UnderlineStyle::Single,
        engine::UnderlineStyle::Double => UnderlineStyle::Double,
        engine::UnderlineStyle::Dotted => UnderlineStyle::Dotted,
        engine::UnderlineStyle::Dashed => UnderlineStyle::Dashed,
        engine::UnderlineStyle::Wavy => UnderlineStyle::Wavy,
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
        caps: patch.caps.unwrap_or(false),
        small_caps: patch.small_caps.unwrap_or(false),
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
fn font_family_id(family: &EngineFontFamily) -> &str {
    family.id()
}

/// Parse a toolbar font-family id back to the document-model enum. Unknown ids
/// (any font registered in `fonts.json` beyond the three seed faces) resolve to
/// a `Custom` face rather than being dropped — issue #23.
fn parse_font_family(id: &str) -> Option<EngineFontFamily> {
    EngineFontFamily::from_id(id)
}

/// Phase 7 — Word's default hyperlink character style colour, mirroring the
/// stock "Hyperlink" character style. Used as the overlay tint when a
/// hyperlink range's underlying run carries the default (black) colour.
const HYPERLINK_BLUE: [u8; 4] = [0x05, 0x63, 0xC1, 0xFF];

/// Phase 8b — markup palette for "Show Markup" mode (default ON).
/// Reviewer-1 colour scheme; per-author multi-colour rotation ships
/// with a later cut.
const REVISION_INSERT_COLOR: [u8; 4] = [0x00, 0x80, 0x00, 0xFF];
const REVISION_DELETE_COLOR: [u8; 4] = [0xCC, 0x00, 0x00, 0xFF];

/// Phase 8b — overlay each revision range so insertions render with
/// `underline = true` + the insert colour and deletions render with
/// `strike = true` + the delete colour. The base span list is split at
/// every revision boundary; sub-spans covered by a revision get the
/// markup styling. An explicit `<w:rPr>` colour wins over the markup
/// tint (matches Word's "Show Markup" semantics where a hand-tinted
/// run keeps its custom colour and just gains the strike / underline
/// overlay). Nested revisions (insertion inside a deletion) stack
/// additively for underline / strike — the inner revision's colour
/// wins last.
fn apply_revision_overlay(
    spans: Vec<StyleSpan>,
    revisions: &[engine::Revision],
    default_color: [u8; 4],
) -> Vec<StyleSpan> {
    if revisions.is_empty() {
        return spans;
    }
    let mut out: Vec<StyleSpan> = Vec::with_capacity(spans.len());
    for span in spans {
        let mut cuts: Vec<u32> = vec![span.start, span.end];
        for r in revisions {
            if r.end > span.start && r.start < span.end {
                cuts.push(r.start.max(span.start));
                cuts.push(r.end.min(span.end));
            }
        }
        cuts.sort_unstable();
        cuts.dedup();
        for w in cuts.windows(2) {
            let (s, e) = (w[0], w[1]);
            if s >= e {
                continue;
            }
            let mut sub = StyleSpan {
                start: s,
                end: e,
                ..span.clone()
            };
            for r in revisions {
                if r.start > s || r.end < e {
                    continue;
                }
                match r.kind {
                    engine::RevisionKind::Insert => {
                        sub.underline = engine::UnderlineStyle::Single;
                        if sub.color == default_color {
                            sub.color = REVISION_INSERT_COLOR;
                        }
                    }
                    engine::RevisionKind::Delete => {
                        sub.strike = true;
                        if sub.color == default_color {
                            sub.color = REVISION_DELETE_COLOR;
                        }
                    }
                    /* Sprint 14 (#14) — FormatChange has no text-wrap
                    visual; the style change itself is the visible
                    diff. Skip styling. */
                    engine::RevisionKind::FormatChange => {}
                }
            }
            out.push(sub);
        }
    }
    out
}

/// Phase 7 — overlay each hyperlink range with `underline = true` and the
/// hyperlink blue when no explicit colour was set. The base span list is
/// split at every hyperlink boundary so the overlay applies to sub-spans
/// only — non-hyperlinked spans keep their original style.
fn apply_hyperlink_overlay(
    spans: Vec<StyleSpan>,
    hyperlinks: &[engine::Hyperlink],
    default_color: [u8; 4],
) -> Vec<StyleSpan> {
    if hyperlinks.is_empty() {
        return spans;
    }
    let mut out: Vec<StyleSpan> = Vec::with_capacity(spans.len());
    for span in spans {
        let mut cuts: Vec<u32> = vec![span.start, span.end];
        for h in hyperlinks {
            if h.end > span.start && h.start < span.end {
                cuts.push(h.start.max(span.start));
                cuts.push(h.end.min(span.end));
            }
        }
        cuts.sort_unstable();
        cuts.dedup();
        for w in cuts.windows(2) {
            let (s, e) = (w[0], w[1]);
            if s >= e {
                continue;
            }
            let mut sub = StyleSpan {
                start: s,
                end: e,
                ..span.clone()
            };
            let linked = hyperlinks.iter().any(|h| h.start <= s && h.end >= e);
            if linked {
                sub.underline = engine::UnderlineStyle::Single;
                /* Only overlay the hyperlink blue when the underlying run
                carries the plain default colour — explicit `<w:rPr>`
                colour wins (matches Word's behaviour where a hand-tinted
                hyperlink keeps its custom colour). */
                if sub.color == default_color {
                    sub.color = HYPERLINK_BLUE;
                }
            }
            out.push(sub);
        }
    }
    out
}

/// Phase 7 — build the layout-side inline object table for one paragraph.
/// EMU dimensions become layout pixels at the current scale: 914400 EMU is
/// one inch, one inch is 72 pt, so `px = emu * scale / 12700`. The
/// resulting `width_px` / `height_px` flow through to the line's ascent
/// and the renderer's image-paint command.
fn build_inline_object_infos(
    para: &engine::Paragraph,
    cfg: &RenderConfig,
    scale: f32,
) -> Vec<layout::paragraph::InlineObjectInfo> {
    para.inline_objects
        .iter()
        .map(|obj| match &obj.kind {
            engine::InlineKind::Image {
                rel_id,
                width_emu,
                height_emu,
            } => layout::paragraph::InlineObjectInfo {
                at: obj.at,
                width_px: engine::emu_to_pt(*width_emu) * scale,
                height_px: engine::emu_to_pt(*height_emu) * scale,
                kind: layout::paragraph::InlineObjectInfoKind::Image {
                    rel_id: rel_id.clone(),
                },
            },
            engine::InlineKind::FootnoteRef { display_number, .. } => {
                /* Phase 8a — footnote markers reserve a small fixed
                width sized to the body font: ~0.45 em per digit at
                the body size. The renderer paints the number as a
                superscript at the glyph's pen position. */
                let label = display_number.to_string();
                let em = cfg.px_size * scale;
                let width = em * 0.45 * (label.len() as f32).max(1.0);
                let height = em * 0.7;
                layout::paragraph::InlineObjectInfo {
                    at: obj.at,
                    width_px: width,
                    height_px: height,
                    kind: layout::paragraph::InlineObjectInfoKind::FootnoteMarker { text: label },
                }
            }
        })
        .collect()
}

/// Issue #29 — the slice of the style table span materialization needs
/// to fold the run cascade (`docDefaults <w:rPr> → pStyle chain`) under
/// each span's direct formatting. `Copy` so it threads through the
/// layout call chain without borrow gymnastics.
#[derive(Clone, Copy)]
struct StyleContext<'a> {
    styles: &'a std::collections::HashMap<String, engine::ParagraphStyle>,
    run_defaults: &'a engine::SpanStyle,
}

impl StyleContext<'_> {
    fn of(doc: &engine::DocumentTree) -> StyleContext<'_> {
        StyleContext {
            styles: &doc.styles,
            run_defaults: &doc.style_run_defaults,
        }
    }

    fn run_base(&self, style_id: Option<&str>) -> engine::SpanStyle {
        engine::resolve_run_cascade(self.styles, self.run_defaults, style_id)
    }
}

fn build_style_spans(
    para: &engine::Paragraph,
    sctx: StyleContext,
    default_size: f32,
    default_color: [u8; 4],
    scale: f32,
) -> Vec<StyleSpan> {
    let len = para.text.len() as u32;
    let mut spans: Vec<StyleSpan> = Vec::new();
    let mut cursor = 0_u32;
    /* Issue #29 — the run-cascade base for this paragraph. Direct span
    formatting folds over it; gap (unstyled) text renders it as-is, so
    a Heading 1 with zero direct formatting picks up the style's bold /
    size. Unstyled paragraphs resolve to the doc-default rPr, which is
    empty in fresh documents — byte-identical to the old flat gap. */
    let run_base = sctx.run_base(para.style_id.as_deref());
    let emit = |style: &engine::SpanStyle, start: u32, end: u32, out: &mut Vec<StyleSpan>| {
        let raw_base_px = style.font_size.unwrap_or(default_size) * scale;
        let vert = style.vert_align.unwrap_or(engine::VertAlign::Baseline);
        let (px_factor, shift_factor) = match vert {
            engine::VertAlign::Baseline => (1.0_f32, 0.0_f32),
            engine::VertAlign::Superscript => (0.65, 0.33),
            engine::VertAlign::Subscript => (0.65, -0.15),
        };
        let base_px = (raw_base_px * px_factor).max(1.0);
        let baseline_shift_px = raw_base_px * shift_factor;
        let template = StyleSpan {
            start,
            end,
            px_size: base_px,
            color: style.color.unwrap_or(default_color),
            bold: style.bold.unwrap_or(false),
            italic: style.italic.unwrap_or(false),
            underline: style.underline.unwrap_or(engine::UnderlineStyle::None),
            strike: style.strike.unwrap_or(false),
            bg_color: style.bg_color,
            font_family: style
                .font_family
                .as_ref()
                .map(font_family_id)
                .map(str::to_string),
            caps_transform: false,
            baseline_shift_px,
        };
        push_caps_spans(&para.text, style, &template, base_px, out);
    };
    for run in &para.spans {
        if run.start > cursor {
            emit(&run_base, cursor, run.start, &mut spans);
        }
        let effective = run_base.clone().merged_with(run.style.clone());
        emit(&effective, run.start, run.end, &mut spans);
        cursor = run.end;
    }
    if cursor < len {
        emit(&run_base, cursor, len, &mut spans);
    }
    spans
}

/// Audit gap A.H3 — expand a single engine `StyleRun` into one or more
/// layout `StyleSpan`s with the caps / smallCaps display contract baked in.
///
/// * `<w:caps>` (full caps) → one span covering the run, `caps_transform`
///   on, full px_size.
/// * `<w:smallCaps>` → walk the **original** source bytes and split at
///   case boundaries:
///     * originally-lowercase ASCII / Unicode letters → sub-span shrunk to
///       ~80 % of the run's nominal px_size, `caps_transform` on (they
///       upper-case at shape time);
///     * everything else (originally-uppercase letters, digits, punctuation,
///       whitespace) → sub-span at full px_size, `caps_transform` on (a
///       no-op for non-letters and already-upper letters).
///
///   Inspecting the source bytes here — *before* any uppercase transform
///   has been applied — is how the renderer differentiates the two
///   classes; once the text reaches the shaper everything is upper.
///
/// `caps` wins over `small_caps` when both flags are set, matching OOXML
/// §17.3.2.7 (`<w:caps>` is the more aggressive of the pair).
fn push_caps_spans(
    para_text: &str,
    rstyle: &engine::SpanStyle,
    template: &StyleSpan,
    base_px: f32,
    out: &mut Vec<StyleSpan>,
) {
    let caps = rstyle.caps == Some(true);
    let small = rstyle.small_caps == Some(true);
    if !caps && !small {
        out.push(template.clone());
        return;
    }
    if caps {
        out.push(StyleSpan {
            caps_transform: true,
            ..template.clone()
        });
        return;
    }
    /* smallCaps split. The slice is paragraph-relative; guard against
    out-of-bounds (defensive — `run.end` is always ≤ paragraph length). */
    let lo = template.start as usize;
    let hi = (template.end as usize).min(para_text.len());
    if lo >= hi {
        out.push(StyleSpan {
            caps_transform: true,
            ..template.clone()
        });
        return;
    }
    let slice = &para_text[lo..hi];
    let small_px = (base_px * 0.8).max(1.0);
    let mut sub_start = lo as u32;
    let mut sub_is_lower: Option<bool> = None;
    for (off, ch) in slice.char_indices() {
        let abs = (lo + off) as u32;
        let ch_is_lower = ch.is_lowercase();
        if sub_is_lower.is_none() {
            sub_is_lower = Some(ch_is_lower);
            sub_start = abs;
            continue;
        }
        if Some(ch_is_lower) != sub_is_lower {
            out.push(StyleSpan {
                start: sub_start,
                end: abs,
                px_size: if sub_is_lower == Some(true) {
                    small_px
                } else {
                    base_px
                },
                caps_transform: true,
                ..template.clone()
            });
            sub_start = abs;
            sub_is_lower = Some(ch_is_lower);
        }
    }
    if let Some(was_lower) = sub_is_lower {
        out.push(StyleSpan {
            start: sub_start,
            end: hi as u32,
            px_size: if was_lower { small_px } else { base_px },
            caps_transform: true,
            ..template.clone()
        });
    }
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
    sctx: StyleContext,
    off: u32,
    comp_len: u32,
    default_size: f32,
    scale: f32,
) -> Vec<StyleSpan> {
    let base = build_style_spans(para, sctx, default_size, [0, 0, 0, 255], scale);
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
        underline: engine::UnderlineStyle::Single,
        strike: st.strike.unwrap_or(false),
        bg_color: st.bg_color,
        font_family: st
            .font_family
            .as_ref()
            .map(font_family_id)
            .map(str::to_string),
        caps_transform: false,
        baseline_shift_px: 0.0,
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
fn paragraph_layout_key(
    para: &engine::Paragraph,
    cfg: &RenderConfig,
    scale: f32,
    max_width_px: f32,
    sctx: StyleContext,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    para.text.hash(&mut h);
    /* Issue #29/#21 — the resolved run-cascade base feeds span
    materialization, so two style-table states must never collide on a
    key (ModifyStyle also clears the cache, but the key is the belt to
    that suspender). */
    let run_base = sctx.run_base(para.style_id.as_deref());
    run_base.font_size.map(f32::to_bits).hash(&mut h);
    run_base.color.hash(&mut h);
    run_base.bold.hash(&mut h);
    run_base.italic.hash(&mut h);
    format!("{:?}", run_base.underline).hash(&mut h);
    run_base.strike.hash(&mut h);
    run_base.bg_color.hash(&mut h);
    run_base.raw_font_family.hash(&mut h);
    run_base.caps.hash(&mut h);
    run_base.small_caps.hash(&mut h);
    /* Audit gap A.H2 — the cache key now folds the laid-out max width
    in. Same paragraph laid out at page-wide vs column-narrow widths
    produces different line breaks; without the mix-in a doc that
    swaps a section's `<w:cols>` mid-edit would serve stale layout. */
    max_width_px.to_bits().hash(&mut h);
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
        /* Without these three, flipping `<w:caps>`, `<w:smallCaps>`, or
        `<w:vertAlign>` produces the same hash as the prior state and
        the cache returns a stale `ParagraphBox` — the visible bug the
        Caps toggle reproduced (text stayed all-uppercase after caps
        was unset; small caps rendered identically to all caps because
        the 0.8× shrink in `push_caps_spans` was bypassed by the cache
        hit). Encode each as a small discriminant. */
        run.style.caps.hash(&mut h);
        run.style.small_caps.hash(&mut h);
        match run.style.vert_align {
            None => 0u8.hash(&mut h),
            Some(engine::VertAlign::Baseline) => 1u8.hash(&mut h),
            Some(engine::VertAlign::Superscript) => 2u8.hash(&mut h),
            Some(engine::VertAlign::Subscript) => 3u8.hash(&mut h),
        }
    }
    /* `engine::Alignment` / `text_pipeline::Alignment` carry no `Hash` derive —
    hash a small discriminant instead. */
    engine_align_disc(para.props.alignment).hash(&mut h);
    /* Phase 9c — paragraph base direction is part of the layout
    contract now that `resolve_base_direction` prefers the paragraph's
    explicit override. Without this hash, a paragraph whose
    `props.direction` flips would re-use stale RTL/LTR shaping from
    cache. Encoded as `Option<bool>`: `0` for absent, `1`/`2` for
    Ltr/Rtl. */
    match para.props.direction {
        None => 0u8.hash(&mut h),
        Some(engine::TextDirection::Ltr) => 1u8.hash(&mut h),
        Some(engine::TextDirection::Rtl) => 2u8.hash(&mut h),
    }
    /* Phase 2 — indent + line-height override change layout geometry, so the
    cache key must mix them in or stale boxes leak across edits. */
    para.props.indent.start_twips.hash(&mut h);
    para.props.indent.end_twips.hash(&mut h);
    para.props.indent.first_line_twips.hash(&mut h);
    para.props.indent.hanging_twips.hash(&mut h);
    /* Issue #50 — the marker text and the numbering level's indent are
    layout inputs now (`marker_text` + `effective_layout_indents`). Without
    them a cross-paragraph renumber (Enter inside a numbered list) or an
    indent-bearing ToggleList would serve a stale cached `ParagraphBox` —
    `do_toggle_list`'s cache clear masks the toggle itself, but not edits
    that renumber OTHER paragraphs. */
    para.resolved_marker.hash(&mut h);
    match para.resolved_list_indent {
        None => 0u8.hash(&mut h),
        Some(li) => {
            1u8.hash(&mut h);
            li.start_twips.hash(&mut h);
            li.end_twips.hash(&mut h);
            li.first_line_twips.hash(&mut h);
            li.hanging_twips.hash(&mut h);
        }
    }
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
    /* Phase 7 — hyperlinks change span overlay; Phase 8b — revisions
    do too. Both mix into the layout-cache key so an `<w:ins>` edit
    that changes only the overlay (not the text) still re-shapes. */
    (para.hyperlinks.len() as u64).hash(&mut h);
    for hl in &para.hyperlinks {
        hl.start.hash(&mut h);
        hl.end.hash(&mut h);
    }
    (para.revisions.len() as u64).hash(&mut h);
    for r in &para.revisions {
        r.start.hash(&mut h);
        r.end.hash(&mut h);
        matches!(r.kind, engine::RevisionKind::Insert).hash(&mut h);
    }
    /* Audit gap A.M3 — tab stops affect glyph advances at the line
    builder's post-pass; without them in the key, two paragraphs with
    identical text but different `<w:tabs>` would collide on cache hits. */
    (para.props.tab_stops.len() as u64).hash(&mut h);
    for s in &para.props.tab_stops {
        s.position_pt.to_bits().hash(&mut h);
    }
    cfg.font_id.hash(&mut h);
    matches!(cfg.base_direction, ShapingDirection::Rtl).hash(&mut h);
    cfg.px_size.to_bits().hash(&mut h);
    cfg.line_height.to_bits().hash(&mut h);
    tp_align_disc(cfg.alignment).hash(&mut h);
    scale.to_bits().hash(&mut h);
    h.finish()
}

/// OOXML twips → layout px, the single conversion every twips-valued
/// geometry field (paragraph indents, paragraph spacing, table grid
/// widths, cell padding, row heights) shares.
///
/// **20 twips = 1 pt.** The result is `pt * scale`, IDENTICAL to how every
/// other geometry field reaches layout px — page margins and tab stops
/// (`position_pt * scale`), font sizes, line height. `scale` carries the
/// whole logical-pt → device-px factor: the 96/72 = 4/3 print-DPI lift
/// **and** the HiDPI `devicePixelRatio` (the TS shell folds both into the
/// `device_pixel_ratio` it seeds — `dpr * 4/3`), or `1.0` for PDF export
/// (PDF user space is logical points).
///
/// Previously this divided by **15** (twips-per-CSS-px at 96 DPI), which
/// silently baked a SECOND 4/3 on top of the one already in `scale` — so a
/// 48 pt indent rendered 4/3 too wide (85.3 px instead of 64 px at
/// `scale = 4/3`) and never lined up with a 48 pt margin. Dividing by 20
/// (twips → pt) and letting `scale` do the pt → px lift removes the
/// double-dip and unifies the math.
fn twips_to_layout_px(twips: i32, scale: f32) -> f32 {
    (twips as f32) / 20.0 * scale
}

/// Issue #25 — resolve a paragraph's effective layout line height from
/// its `<w:spacing w:line>` override. `None` inherits the config
/// default; `Auto` is a 240-ths multiple of that default (Word's
/// "Multiple" spacing — 1.0× ≡ the default, so unspaced documents
/// render byte-identically); `Exact`/`AtLeast` are absolute twips.
/// Returns `(px, exact)` — `exact` makes the line box clip instead of
/// growing for tall glyphs.
fn resolve_line_height(
    line_height: Option<engine::LineHeight>,
    cfg_line_height: f32,
    scale: f32,
) -> (f32, bool) {
    match line_height {
        None => (cfg_line_height * scale, false),
        Some(engine::LineHeight::Auto { twips }) => {
            (cfg_line_height * scale * (twips as f32 / 240.0), false)
        }
        Some(engine::LineHeight::Exact { twips }) => (twips_to_layout_px(twips, scale), true),
        Some(engine::LineHeight::AtLeast { twips }) => (twips_to_layout_px(twips, scale), false),
    }
}

/// Phase 8a — walk every body paragraph in document order, mirror the
/// `InlineKind::FootnoteRef` id → display_number mapping the parser
/// assigned, then lay out each referenced footnote's body paragraph(s)
/// into a single combined `ParagraphBox`. The paginator keys its
/// `with_footnote_bodies` lookup by display_number — the same number
/// that lives on every footnote-marker glyph — so layout never sees
/// the OOXML `w:id`.
fn build_footnote_bodies(
    doc: &DocumentTree,
    font_stack: &FontStack,
    cfg: &RenderConfig,
    scale: f32,
) -> std::collections::HashMap<u32, ParagraphBox> {
    let mut by_display: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    for block in doc.blocks.iter() {
        walk_block_for_footnote_refs(block, &mut by_display);
    }
    let mut out: std::collections::HashMap<u32, ParagraphBox> = std::collections::HashMap::new();
    /* Footnote body width — content width of the default A4 section;
    section-specific page widths are a follow-up (the table is built
    once per document, not once per section, since footnotes flow
    against the section they reference into). */
    let body_width = engine::PageGeometry::a4().content_width() * scale;
    for (display, w_id) in &by_display {
        let Some(paragraphs) = doc.footnotes.get(w_id) else {
            continue;
        };
        /* Flatten the footnote's per-`<w:p>` plain text into one body
        paragraph so the band lays out a single block per footnote.
        Rich body formatting + multi-paragraph footnote bodies ship
        with the Phase 8c sprint. */
        let joined: String = paragraphs.join(" ");
        let combined = format!("{display}. {joined}");
        let spans = [StyleSpan {
            start: 0,
            end: combined.len() as u32,
            px_size: cfg.px_size * scale * 0.85,
            color: [0, 0, 0, 255],
            bold: false,
            italic: false,
            underline: engine::UnderlineStyle::None,
            strike: false,
            bg_color: None,
            font_family: None,
            caps_transform: false,
            baseline_shift_px: 0.0,
        }];
        let p = layout_paragraph(ParagraphConfig {
            text: &combined,
            fonts: font_stack,
            spans: &spans,
            base_direction: first_strong_direction(&combined).unwrap_or(cfg.base_direction),
            max_width: body_width,
            line_height: cfg.line_height * scale * 0.85,
            line_height_exact: false,
            alignment: cfg.alignment,
            indent_start_px: 0.0,
            indent_end_px: 0.0,
            first_line_indent_px: 0.0,
            hanging_indent_px: 0.0,
            marker_text: None,
            px_size_for_marker: cfg.px_size * scale * 0.85,
            inline_objects: &[],
            tab_stops_px: &[],
        });
        out.insert(*display, p);
    }
    out
}

/// Phase 8a — recursive walker that fills `by_display[display_number] = w_id`.
fn walk_block_for_footnote_refs(
    block: &engine::Block,
    by_display: &mut std::collections::HashMap<u32, u32>,
) {
    match block {
        engine::Block::Paragraph(p) => {
            for obj in &p.inline_objects {
                if let engine::InlineKind::FootnoteRef { id, display_number } = &obj.kind {
                    by_display.insert(*display_number, *id);
                }
            }
        }
        engine::Block::Table(t) => {
            for row in &t.rows {
                for cell in &row.cells {
                    for b in &cell.blocks {
                        walk_block_for_footnote_refs(b, by_display);
                    }
                }
            }
        }
    }
}

/// Phase 2 audit (gap D.1 follow-up) — lay out one section's header
/// (or footer) paragraphs into a [`layout::HeaderFooterBox`]. The
/// input is the full `engine::Paragraph` model now, not plain text:
/// style spans, hyperlinks, revisions and `Field` overlays all
/// propagate into the laid-out `ParagraphBox` so the paginator's
/// per-page field evaluator can stamp PAGE / NUMPAGES.
///
/// Each paragraph runs through the same `build_style_spans` →
/// `apply_hyperlink_overlay` → `apply_revision_overlay` →
/// `layout_paragraph` pipeline as a body paragraph. Indents,
/// alignment, base direction and line-height inherit from the
/// paragraph's `props` exactly as in the body path. Returns `None`
/// when every paragraph is empty.
fn build_header_footer_box(
    paragraphs: &[engine::Paragraph],
    content_width: f32,
    fonts: &FontStack,
    cfg: &RenderConfig,
    scale: f32,
    sctx: StyleContext,
) -> Option<layout::HeaderFooterBox> {
    if paragraphs.iter().all(|p| p.text.is_empty()) {
        return None;
    }
    let mut paras: Vec<ParagraphBox> = Vec::with_capacity(paragraphs.len());
    let mut y = 0.0_f32;
    for para in paragraphs {
        let spans = apply_revision_overlay(
            apply_hyperlink_overlay(
                build_style_spans(para, sctx, cfg.px_size, [0, 0, 0, 255], scale),
                &para.hyperlinks,
                [0, 0, 0, 255],
            ),
            &para.revisions,
            [0, 0, 0, 255],
        );
        let base_direction = resolve_base_direction(para, cfg);
        let (ind_s, ind_e, ind_fl, ind_h) = effective_layout_indents(para, base_direction, scale);
        let (lh_px, lh_exact) = resolve_line_height(para.props.line_height, cfg.line_height, scale);
        let mut p = layout_paragraph(ParagraphConfig {
            text: &para.text,
            fonts,
            spans: &spans,
            base_direction,
            max_width: content_width,
            line_height: lh_px,
            line_height_exact: lh_exact,
            alignment: para.props.alignment.map_or(cfg.alignment, layout_align),
            indent_start_px: ind_s,
            indent_end_px: ind_e,
            first_line_indent_px: ind_fl,
            hanging_indent_px: ind_h,
            marker_text: para.resolved_marker.clone(),
            px_size_for_marker: cfg.px_size * scale,
            inline_objects: &[],
            tab_stops_px: &[],
        });
        /* Phase 2 audit (gap D.1) — propagate field overlays so the
        paginator can re-evaluate PAGE / NUMPAGES per page. Identical
        plumbing to the body-paragraph path. */
        p.fields = para
            .fields
            .iter()
            .map(|f| layout::LayoutField {
                byte_range: f.start..f.end,
                instruction: f.instruction.clone(),
                evaluated_text: None,
            })
            .collect();
        p.origin = Point { x: 0.0, y };
        y += p.size.height;
        paras.push(p);
    }
    Some(layout::HeaderFooterBox { paragraphs: paras })
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

/// Resolved base direction for a paragraph layout pass — Phase 9c fix
/// (UX_BEHAVIOR_SPEC §III). UAX #9's first-strong inference is correct
/// for paragraphs WITHOUT an explicit `<w:bidi>` setting, but when the
/// user (or the source `.docx`) has explicitly set `props.direction`,
/// that override MUST win over first-strong — otherwise the BiDi
/// algorithm shapes neutrals (trailing `!!`, parentheses, digits)
/// using the implicit character-level RTL of the Arabic glyphs even
/// when the paragraph was explicitly LTR.
///
/// Precedence:
/// 1. Paragraph's explicit `props.direction` (Word's `<w:bidi>`).
/// 2. `first_strong_direction(text)` — first-strong inference for
///    paragraphs with no explicit setting.
/// 3. `cfg.base_direction` — document-wide default seeded at boot.
fn resolve_base_direction(p: &engine::Paragraph, cfg: &RenderConfig) -> ShapingDirection {
    if let Some(d) = p.props.direction {
        return match d {
            engine::TextDirection::Ltr => ShapingDirection::Ltr,
            engine::TextDirection::Rtl => ShapingDirection::Rtl,
        };
    }
    first_strong_direction(&p.text).unwrap_or(cfg.base_direction)
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

/// Issue #50 / #58 — per-slot merge of a paragraph's direct `<w:ind>` over
/// its resolved numbering-level indent (`resolved_list_indent`, stamped by
/// the marker resolver next to `resolved_marker`), in twips. A direct
/// `<w:ind>` attribute wins over the level's, but a PARTIAL direct indent
/// (a ruler end-indent drag writes `{0, E, 0, 0}`; a docx pPr may carry only
/// `w:ind w:end`) must not strip the level's start/hanging wholesale — that
/// re-creates the issue-#50 invisible-marker collapse this fallback exists
/// to fix. "Set" is approximated as non-zero, the same flat-struct
/// trade-off documented on `ParaProperties::merged_with`. `firstLine`/
/// `hanging` merge as a pair (mutually exclusive in OOXML — one side being
/// set shadows both of the level's).
///
/// The level indent is authored leading-edge-relative (OOXML logical
/// start), so for an RTL paragraph it lands on the end slot before the
/// merge — that is the slot `layout_paragraph` reads the leading offset
/// from when the base direction is RTL; a direct indent keeps the legacy
/// slot convention untouched.
///
/// Shared by [`effective_layout_indents`] (layout, converts to px) and
/// `indent_for_caret` (the Ruler read-back, converts to pt) so the two can
/// never diverge again — issue #58 was exactly that divergence, where the
/// Ruler read raw `props.indent` and reported 0 for a toggled list whose
/// rendered text was visibly indented.
fn effective_indent_twips(
    para: &engine::Paragraph,
    base_direction: ShapingDirection,
) -> (i32, i32, i32, i32) {
    let d = para.props.indent;
    let Some(li) = para.resolved_list_indent else {
        return (
            d.start_twips,
            d.end_twips,
            d.first_line_twips,
            d.hanging_twips,
        );
    };
    let (li_start_slot, li_end_slot) = match base_direction {
        ShapingDirection::Rtl => (li.end_twips, li.start_twips),
        ShapingDirection::Ltr => (li.start_twips, li.end_twips),
    };
    let s = if d.start_twips != 0 {
        d.start_twips
    } else {
        li_start_slot
    };
    let e = if d.end_twips != 0 {
        d.end_twips
    } else {
        li_end_slot
    };
    let (fl, hang) = if d.first_line_twips != 0 || d.hanging_twips != 0 {
        (d.first_line_twips, d.hanging_twips)
    } else {
        (li.first_line_twips, li.hanging_twips)
    };
    (s, e, fl, hang)
}

/// Issue #50 — the indents that actually reach `layout_paragraph`. Without
/// the numbering-level fallback an interactively-toggled list lays out at
/// indent 0 and `build_marker`'s clamp paints the bullet underneath the
/// first text glyph — the invisible marker of issue #50.
fn effective_layout_indents(
    para: &engine::Paragraph,
    base_direction: ShapingDirection,
    scale: f32,
) -> (f32, f32, f32, f32) {
    if para.resolved_list_indent.is_none() {
        return props_to_layout_indents(&para.props, scale);
    }
    let (s, e, fl, hang) = effective_indent_twips(para, base_direction);
    (
        twips_to_layout_px(s, scale),
        twips_to_layout_px(e, scale),
        twips_to_layout_px(fl, scale),
        twips_to_layout_px(hang, scale),
    )
}

/// Audit gap A.M3 / L2.1 (#6) — convert `engine::TabStop` positions
/// (pt at scale=1) into `(device-px, layout::paragraph::TabKind)`
/// pairs the layout pass consumes. `Clear` kinds drop here (the
/// layout treats absence as "no stop"; clear-of-inherited only
/// matters for the cascade resolver, not the line builder). Sorted
/// ascending by position so `next_tab_stop_after`'s `min_by` scan is
/// monotonic.
fn tab_stops_to_layout_px(
    stops: &[engine::TabStop],
    scale: f32,
) -> Vec<(f32, layout::paragraph::TabKind)> {
    let mut out: Vec<(f32, layout::paragraph::TabKind)> = stops
        .iter()
        .filter_map(|s| {
            let kind = match s.kind {
                engine::TabKind::Left => layout::paragraph::TabKind::Left,
                engine::TabKind::Center => layout::paragraph::TabKind::Center,
                engine::TabKind::Right => layout::paragraph::TabKind::Right,
                engine::TabKind::Decimal => layout::paragraph::TabKind::Decimal,
                engine::TabKind::Clear => return None,
            };
            Some((s.position_pt * scale, kind))
        })
        .collect();
    out.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    out
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
    sctx: StyleContext,
    cache: &mut LruCache<u64, ParagraphBox>,
) -> TableBox {
    /* Column widths in device px. Decision tree:
    - `<w:tblLayout w:type="fixed"/>` AND grid present → use grid as-is.
    - Default (Autofit) AND grid present → start with grid, then run
      the autofit measure-then-distribute pass to right-size each
      column to its content (audit gap A.M8).
    - Grid absent → autofit always, the grid simply has no anchor. */
    let mut columns: Vec<f32> = if table.grid.is_empty() {
        Vec::new()
    } else {
        table
            .grid
            .iter()
            .map(|&t| twips_to_layout_px(t, scale))
            .collect()
    };
    /* Audit gap A.M8 — autofit. Measure each column's intrinsic
    natural width (max line width across all cells in that column at
    a generous probe width), then redistribute `available_width_px`
    so wide columns get more room and narrow columns aren't forced
    to wrap their longest unbreakable word. Single-pass: re-uses the
    cell layout result from the measure pass as the layout (no second
    layout call for unchanged column widths). */
    if matches!(table.props.layout, engine::TableLayout::Autofit) {
        columns = autofit_distribute(
            table,
            available_width_px,
            fonts,
            cfg,
            scale,
            &columns,
            sctx,
            cache,
        );
    }

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
            /* Phase 2 audit (gap B.1/B.2) — per-edge padding resolves
            cell override (`<w:tcMar>`) → table default
            (`<w:tblCellMar>`) → Word stock (0/108/0/108 twips). */
            let eff = engine::CellMargins::resolve_edges(
                cell.props.cell_margins.as_ref(),
                &table.props.cell_margins,
            );
            let pad_top = twips_to_layout_px(eff.top_twips, scale);
            let pad_bottom = twips_to_layout_px(eff.bottom_twips, scale);
            let pad_left = twips_to_layout_px(eff.left_twips, scale);
            let pad_right = twips_to_layout_px(eff.right_twips, scale);
            let content_width = (cell_width - pad_left - pad_right).max(0.0);
            let inner_blocks =
                layout_cell_blocks(&cell.blocks, content_width, fonts, cfg, scale, sctx, cache);
            let content_height: f32 = inner_blocks.iter().map(|b| b.size().height).sum();
            /* `VMergeRole::Continue` cells contribute zero — the matching
            `Restart` cell visually owns the merged region. */
            let measured = if matches!(cell.props.v_merge, engine::VMergeRole::Continue) {
                0.0
            } else {
                content_height + pad_top + pad_bottom
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
                padding_left: pad_left,
                padding_top: pad_top,
                padding_right: pad_right,
                padding_bottom: pad_bottom,
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
        /* Audit gap A.H1 — `<w:vAlign>`. Now that the row height is final,
        shift each cell's inner content down by `(cell_inner_height -
        content_height) * factor` so Center / Bottom alignments park
        content in the right band of the cell. Continue cells own no
        content; Restart cells with content shorter than the row gain a
        visible offset; cells whose content already fills the row stay
        flush against the top padding (clamp negative slack to 0). */
        for (c, src_cell) in cells_out.iter_mut().zip(row.cells.iter()) {
            if matches!(src_cell.props.v_merge, engine::VMergeRole::Continue) {
                continue;
            }
            let factor = match src_cell.props.v_align {
                engine::VerticalAlign::Top => continue,
                engine::VerticalAlign::Center => 0.5_f32,
                engine::VerticalAlign::Bottom => 1.0_f32,
            };
            let inner_height = (row_height - c.padding_top - c.padding_bottom).max(0.0);
            let content_h: f32 = c.content.iter().map(|b| b.size().height).sum();
            let slack = (inner_height - content_h).max(0.0);
            if slack <= 0.0 {
                continue;
            }
            let shift = slack * factor;
            for inner in c.content.iter_mut() {
                let mut o = inner.origin();
                o.y += shift;
                inner.set_origin(o);
            }
        }
        let row_width = cells_out.iter().map(|c| c.size.width).sum::<f32>();
        rows_out.push(TableRowBox {
            origin: Point { x: 0.0, y },
            size: Size {
                width: row_width.max(table_width),
                height: row_height,
            },
            cells: cells_out,
            header: row.props.header,
            cant_split: row.props.cant_split,
        });
        y += row_height;
    }

    /* Audit gap C.M1 — vMerge height accumulation. Walk each Restart
    cell and grow its `size.height` to span the consecutive Continue
    cells beneath it in the same column. Done after every row is
    finalized (so row heights are stable) and before the box leaves
    `layout_table_box`. Without this, a vertically-merged cell whose
    Restart row is short renders the Restart cell's content shrunk
    to that single row's height, and the Continue rows below show
    blank — the visual bug A.M8 / C.M1 covers. */
    accumulate_vmerge_heights(&mut rows_out);

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

/// Audit gap A.M8 — autofit two-pass column distribution.
///
/// Pass 1 (measure): for every cell, lay out its paragraphs against
/// the table's available width and read back the per-paragraph maximum
/// line width (the natural max-content metric). Track per-column the
/// max over rows. Empty cells contribute a floor of ~30 pt so a
/// single-cell column doesn't collapse to zero. Grid_span > 1 cells
/// contribute their natural width split evenly across the spanned
/// columns.
///
/// Pass 2 (min-content measure, L2.2 #7): for every cell, also probe
/// the line breaker at `width = 1.0` to learn the *longest unbreakable
/// atom* (a URL, a code token, a single glyph). `col_min_content[i]`
/// records the worst such atom across cells in column `i`.
/// `col_floor[i] = max(grid_hint[i], col_min_content[i])` — a HARD
/// floor every column must respect so the line breaker never wraps
/// mid-character.
///
/// Pass 3 (distribute): three cases against `available_width_px`:
///   * **Overflow** — `Σ col_floor > available`. Return `col_floor`
///     as-is; the table grows past the page margin rather than
///     clipping a URL or token mid-character. Word does the same.
///   * **Fit** — `Σ col_natural <= available`. Scale columns
///     proportionally up to fill the available band (Word's "Autofit
///     Window" behaviour). No floor violations possible since
///     `col_natural >= col_floor` after pass 1.
///   * **Shrink** — `Σ col_natural > available` but
///     `Σ col_floor <= available`. Iteratively pin columns whose
///     proportional share would fall below the floor, then redistribute
///     the remaining width among the unpinned columns. Bounded by
///     `n_cols` iterations.
///
/// Cost: 2 × O(rows × cols) calls to `layout_cell_blocks` in the
/// measure passes (probe width + min-content width). Cells re-layout
/// once more at the final widths in the caller. Total ≈ 3× a fixed-
/// grid table for autofit. Bounded for typical tables; infinite-loop
/// risk in the iterative shrink is zero because each iteration pins
/// strictly more columns or terminates.
#[allow(clippy::too_many_arguments)]
fn autofit_distribute(
    table: &engine::Table,
    available_width_px: f32,
    fonts: &FontStack,
    cfg: &RenderConfig,
    scale: f32,
    grid_hint: &[f32],
    sctx: StyleContext,
    cache: &mut LruCache<u64, ParagraphBox>,
) -> Vec<f32> {
    /* Number of columns: max(grid, max row's cell-count). The grid
    might be empty; cells might over-/under-shoot it; take the union. */
    let max_cols_in_rows = table
        .rows
        .iter()
        .map(|r| {
            r.cells
                .iter()
                .map(|c| c.props.grid_span.max(1) as usize)
                .sum::<usize>()
        })
        .max()
        .unwrap_or(0);
    let n_cols = grid_hint.len().max(max_cols_in_rows);
    if n_cols == 0 {
        return Vec::new();
    }
    const MIN_COL_WIDTH_PT: f32 = 30.0;
    let probe_width = (available_width_px / n_cols as f32).max(MIN_COL_WIDTH_PT * scale);

    let mut col_natural: Vec<f32> = vec![MIN_COL_WIDTH_PT * scale; n_cols];
    let mut col_min_content: Vec<f32> = vec![0.0_f32; n_cols];
    for row in &table.rows {
        let mut col_cursor: usize = 0;
        for cell in &row.cells {
            let span = cell.props.grid_span.max(1) as usize;
            if matches!(cell.props.v_merge, engine::VMergeRole::Continue) {
                col_cursor += span;
                continue;
            }
            /* Effective padding eats into the natural-width measurement
            because the eventual layout subtracts it from the column
            width before passing to the paragraph builder. Apply
            symmetrically here so the measure matches the actual fit. */
            let eff = engine::CellMargins::resolve_edges(
                cell.props.cell_margins.as_ref(),
                &table.props.cell_margins,
            );
            let pad = twips_to_layout_px(eff.left_twips + eff.right_twips, scale);
            /* Lay out at the probe width — max line width across all
            paragraphs is the natural fit. */
            let inner =
                layout_cell_blocks(&cell.blocks, probe_width, fonts, cfg, scale, sctx, cache);
            let natural_content_w = block_max_width(&inner);
            let cell_natural = natural_content_w + pad;
            let share = cell_natural / span as f32;
            for i in 0..span {
                let ci = col_cursor + i;
                if ci < col_natural.len() && col_natural[ci] < share {
                    col_natural[ci] = share;
                }
            }
            /* L2.2 (#7) — min-content probe. Width=1.0 forces the line
            breaker to wrap at every available break opportunity from
            `icu_segmenter`; the resulting max line width is the
            longest atom that cannot be split. Padding is folded into
            the floor on the same basis as the natural measure. */
            let (cell_min_content, _cell_max_content) =
                measure_unbreakable_width(&cell.blocks, sctx, fonts, cfg, scale, cache);
            let min_share = (cell_min_content + pad) / span as f32;
            for i in 0..span {
                let ci = col_cursor + i;
                if ci < col_min_content.len() && col_min_content[ci] < min_share {
                    col_min_content[ci] = min_share;
                }
            }
            col_cursor += span;
        }
    }
    /* L2.2 (#7) — column floor: max(grid_hint, min_content). The
    natural measurement must respect this floor too, since a column
    whose probe-width yielded a small width but whose min-content
    is large would otherwise propose a shrunk-then-clipped layout. */
    let col_floor: Vec<f32> = (0..n_cols)
        .map(|i| {
            let hint = grid_hint.get(i).copied().unwrap_or(0.0);
            hint.max(col_min_content[i])
        })
        .collect();
    for i in 0..n_cols {
        if col_natural[i] < col_floor[i] {
            col_natural[i] = col_floor[i];
        }
    }

    let total: f32 = col_natural.iter().sum();
    let floor_total: f32 = col_floor.iter().sum();

    if total <= 0.0 {
        return vec![available_width_px / n_cols as f32; n_cols];
    }

    /* CASE 1 — overflow. Even the floors won't fit; emit the floors
    and let the table extend past the available band. No column ever
    shrinks below its longest unbreakable atom (the issue #7 invariant). */
    if floor_total > available_width_px {
        return col_floor;
    }

    /* CASE 2 — fit. The natural total fits; scale proportionally to
    fill the available width. Word's "Autofit Window" expand path.
    No floor violations possible since col_natural >= col_floor. */
    if total <= available_width_px {
        let factor = available_width_px / total;
        return col_natural.iter().map(|w| w * factor).collect();
    }

    /* CASE 3 — shrink. Natural total exceeds available, but floors
    fit. Iteratively pin columns whose proportional share would fall
    below their floor; redistribute remaining width among the
    unpinned ones. Convergence: each iteration pins ≥ 1 more column
    or terminates, bounded by n_cols passes. */
    let mut pinned = vec![false; n_cols];
    let mut final_widths = vec![0.0_f32; n_cols];
    loop {
        let mut pinned_total = 0.0_f32;
        let mut unpinned_natural = 0.0_f32;
        for i in 0..n_cols {
            if pinned[i] {
                pinned_total += col_floor[i];
            } else {
                unpinned_natural += col_natural[i];
            }
        }
        if unpinned_natural <= 0.0 {
            /* All columns pinned. Total may exceed available; same
            "horizontal overflow rather than clip" rule applies. */
            final_widths[..n_cols].copy_from_slice(&col_floor[..n_cols]);
            break;
        }
        let unpinned_room = (available_width_px - pinned_total).max(0.0);
        let factor = unpinned_room / unpinned_natural;
        let mut newly_pinned = false;
        for i in 0..n_cols {
            if !pinned[i] {
                let proposed = col_natural[i] * factor;
                if proposed < col_floor[i] {
                    pinned[i] = true;
                    newly_pinned = true;
                }
            }
        }
        if !newly_pinned {
            for i in 0..n_cols {
                if pinned[i] {
                    final_widths[i] = col_floor[i];
                } else {
                    final_widths[i] = col_natural[i] * factor;
                }
            }
            break;
        }
    }
    final_widths
}

/// L2.2 (#7) — measure a cell's `(min_content, max_content)` widths.
///
/// `min_content` is the pixel width of the longest contiguous segment
/// between adjacent break opportunities reported by `icu_segmenter`
/// (UAX #14). For pure-letter text "aaa..." there is one segment
/// covering the whole string, so min_content equals the string's
/// shaped advance; for URLs / hyphenated tokens, ICU breaks at `/`,
/// `-`, `:` and similar characters, so the segment widths are
/// individually smaller. This bypasses the layout's `compose_lines`
/// force-break path (which would otherwise wrap mid-character when
/// probed at width = 1.0, reducing min_content to a single glyph).
///
/// `max_content` is the sum of every segment's advance — equivalent
/// to shaping the entire text on one line.
///
/// Nested tables recurse through `autofit_distribute` at width = 1.0
/// so the inner table's min-content sum bubbles up to its parent
/// cell.
///
/// Cost: one extra `shape_text` call per segment per paragraph. The
/// existing probe-width `layout_cell_blocks` pass continues to drive
/// the natural-width measurement.
fn measure_unbreakable_width(
    blocks: &[engine::Block],
    sctx: StyleContext,
    fonts: &FontStack,
    cfg: &RenderConfig,
    scale: f32,
    cache: &mut LruCache<u64, ParagraphBox>,
) -> (f32, f32) {
    let mut min_content = 0.0_f32;
    let mut max_content = 0.0_f32;
    for block in blocks {
        match block {
            engine::Block::Paragraph(para) => {
                let (mc, xc) = paragraph_min_max_advance(para, fonts, cfg, scale);
                if mc > min_content {
                    min_content = mc;
                }
                if xc > max_content {
                    max_content = xc;
                }
            }
            engine::Block::Table(t) => {
                /* Nested table — recurse. Sum of inner column min-content
                widths bubbles up as this cell's contribution. Probe the
                inner table against width = 1.0 so its own
                `autofit_distribute` lands in the CASE 1 overflow path
                (returns col_floor verbatim). */
                let sub_widths = autofit_distribute(t, 1.0, fonts, cfg, scale, &[], sctx, cache);
                let sub_sum: f32 = sub_widths.iter().sum();
                if sub_sum > min_content {
                    min_content = sub_sum;
                }
                if sub_sum > max_content {
                    max_content = sub_sum;
                }
            } /* `engine::Block` is exhaustive at Paragraph + Table; the
              non-exhaustive marker is `#[allow(clippy::large_enum_variant)]`
              only. New variants land here as separate arms. */
        }
    }
    (min_content, max_content)
}

/// L2.2 (#7) — per-paragraph helper for `measure_unbreakable_width`.
/// Walks `break_opportunities` returned by `icu_segmenter`; for each
/// adjacent pair, measures the segment's shaped advance via
/// `shape_text`. Returns `(longest_segment, sum_of_segments)`.
fn paragraph_min_max_advance(
    para: &engine::Paragraph,
    fonts: &FontStack,
    cfg: &RenderConfig,
    scale: f32,
) -> (f32, f32) {
    use text_pipeline::break_opportunities;
    if para.text.is_empty() {
        return (0.0, 0.0);
    }
    let breaks = break_opportunities(&para.text);
    let mut longest = 0.0_f32;
    let mut total = 0.0_f32;
    for window in breaks.windows(2) {
        let a = window[0];
        let b = window[1];
        if a >= b {
            continue;
        }
        let seg = &para.text[a..b];
        let seg_w = measure_segment_advance(seg, fonts, cfg, scale);
        if seg_w > longest {
            longest = seg_w;
        }
        total += seg_w;
    }
    (longest, total)
}

/// L2.2 (#7) — shape `text` against the resolved font for each
/// script segment at `cfg.px_size * scale`; sum the per-segment
/// `total_advance` to get the natural pixel width.
fn measure_segment_advance(text: &str, fonts: &FontStack, cfg: &RenderConfig, scale: f32) -> f32 {
    use text_pipeline::{segment_by_script, shape_text};
    let mut total = 0.0_f32;
    for (range, script) in segment_by_script(text) {
        let Some((_, face, _)) = fonts.resolve(script, None, false, false) else {
            continue;
        };
        let shaped = shape_text(face, &text[range], cfg.base_direction, cfg.px_size * scale);
        total += shaped.total_advance;
    }
    total
}

/// Max width across a slice of laid-out cell blocks. Paragraphs report
/// the widest line; nested tables report the table's outer width.
fn block_max_width(blocks: &[LayoutBlock]) -> f32 {
    blocks
        .iter()
        .map(|b| match b {
            LayoutBlock::Paragraph(p) => p.lines.iter().map(|l| l.width).fold(0.0_f32, f32::max),
            LayoutBlock::Table(t) => t.size.width,
        })
        .fold(0.0_f32, f32::max)
}

/// Audit gap C.M1 — vertical-merge height pass. For every Restart cell
/// at column index `C`, walk subsequent rows scanning column `C`; while
/// the cell at `C` is `Continue`, add its row's height to the Restart's
/// `size.height`. The Restart cell visually owns the merged span; its
/// inner-content `v_align` shift (computed earlier off the now-final
/// `size.height`) is re-applied so Bottom-aligned content inside a
/// 4-row vMerge actually sits at the bottom of all 4 rows.
fn accumulate_vmerge_heights(rows: &mut [TableRowBox]) {
    /* Identify (row_idx, col_idx) of every Restart cell and how many
    Continue rows follow at the same `col_idx`. Build a list first
    then mutate, so the loop borrows immutably for scanning and
    mutably for the patch. */
    let mut extensions: Vec<(usize, usize, f32)> = Vec::new();
    for (r, row) in rows.iter().enumerate() {
        let mut col_cursor: u32 = 0;
        for cell in &row.cells {
            let span = cell.grid_span.max(1) as u32;
            if matches!(cell.v_merge, engine::VMergeRole::Restart) {
                let mut extra = 0.0_f32;
                for next_row in rows.iter().skip(r + 1) {
                    /* Does `next_row` carry a Continue cell at the same
                    column? Re-walk grid_spans because cells before col_cursor
                    may consume different widths. */
                    let mut nc: u32 = 0;
                    let mut hit_continue = false;
                    for next_cell in &next_row.cells {
                        if nc == col_cursor {
                            if matches!(next_cell.v_merge, engine::VMergeRole::Continue) {
                                hit_continue = true;
                            }
                            break;
                        }
                        nc += next_cell.grid_span.max(1) as u32;
                        if nc > col_cursor {
                            break;
                        }
                    }
                    if hit_continue {
                        extra += next_row.size.height;
                    } else {
                        break;
                    }
                }
                if extra > 0.0 {
                    /* Find this cell's index inside its row to record
                    the patch. `col_cursor` tracks the column; we need
                    the actual `cells[i]` index. */
                    let cell_idx = row
                        .cells
                        .iter()
                        .scan(0_u32, |acc, c| {
                            let start = *acc;
                            *acc += c.grid_span.max(1) as u32;
                            Some(start)
                        })
                        .position(|s| s == col_cursor)
                        .unwrap_or(0);
                    extensions.push((r, cell_idx, extra));
                }
            }
            col_cursor += span;
        }
    }
    for (r, c, extra) in extensions {
        if let Some(row) = rows.get_mut(r)
            && let Some(cell) = row.cells.get_mut(c)
        {
            cell.size.height += extra;
        }
    }
}

/// Issue #51/#34 — the one shared, LRU-backed paragraph layout used by
/// both the body-block arm of `build_pages` and every table cell. Cells
/// previously bypassed the cache entirely (re-shaped on every paint —
/// with autofit's multi-pass measurement, ~3× per paint) and also
/// skipped the hyperlink / revision overlays and inline objects the
/// body path applies; routing both arms through here fixes the cost and
/// the parity gap in one move, and makes the cache key semantics
/// identical for every consumer.
fn layout_paragraph_cached(
    para: &engine::Paragraph,
    fonts: &FontStack,
    cfg: &RenderConfig,
    scale: f32,
    max_width: f32,
    sctx: StyleContext,
    cache: &mut LruCache<u64, ParagraphBox>,
) -> ParagraphBox {
    let key = paragraph_layout_key(para, cfg, scale, max_width, sctx);
    if let Some(cached) = cache.get(&key) {
        return cached.clone();
    }
    let spans = apply_revision_overlay(
        apply_hyperlink_overlay(
            build_style_spans(para, sctx, cfg.px_size, [0, 0, 0, 255], scale),
            &para.hyperlinks,
            [0, 0, 0, 255],
        ),
        &para.revisions,
        [0, 0, 0, 255],
    );
    let base_direction = resolve_base_direction(para, cfg);
    let (ind_s, ind_e, ind_fl, ind_h) = effective_layout_indents(para, base_direction, scale);
    let inline_infos = build_inline_object_infos(para, cfg, scale);
    let (lh_px, lh_exact) = resolve_line_height(para.props.line_height, cfg.line_height, scale);
    let para_cfg = ParagraphConfig {
        text: &para.text,
        fonts,
        spans: &spans,
        base_direction,
        max_width,
        line_height: lh_px,
        line_height_exact: lh_exact,
        alignment: para.props.alignment.map_or(cfg.alignment, layout_align),
        indent_start_px: ind_s,
        indent_end_px: ind_e,
        first_line_indent_px: ind_fl,
        hanging_indent_px: ind_h,
        marker_text: para.resolved_marker.clone(),
        px_size_for_marker: cfg.px_size * scale,
        inline_objects: &inline_infos,
        tab_stops_px: &tab_stops_to_layout_px(&para.props.tab_stops, scale),
    };
    let laid = layout_paragraph(para_cfg);
    cache.put(key, laid.clone());
    laid
}

fn layout_cell_blocks(
    blocks: &[engine::Block],
    content_width_px: f32,
    fonts: &FontStack,
    cfg: &RenderConfig,
    scale: f32,
    sctx: StyleContext,
    cache: &mut LruCache<u64, ParagraphBox>,
) -> Vec<LayoutBlock> {
    let mut out: Vec<LayoutBlock> = Vec::with_capacity(blocks.len());
    let mut y = 0.0_f32;
    for b in blocks {
        let mut lb = match b {
            engine::Block::Paragraph(p) => LayoutBlock::Paragraph(layout_paragraph_cached(
                p,
                fonts,
                cfg,
                scale,
                content_width_px.max(1.0),
                sctx,
                cache,
            )),
            engine::Block::Table(t) => LayoutBlock::Table(layout_table_box(
                t,
                content_width_px,
                fonts,
                cfg,
                scale,
                sctx,
                cache,
            )),
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
        /* Empty lines (a trailing / doubled soft-break placeholder) carry no
        runs, so fall back to the layout-supplied `source_start` instead of
        byte 0. Without this the line reports [0, 0]; a caret at
        `offset == text.len()` then brackets NO line and `caret_rect_geom`
        snaps it back to the paragraph's first line — the trailing-soft-break
        "caret doesn't drop" bug. */
        let start_byte = line
            .runs
            .iter()
            .map(|r| r.source_range.start)
            .min()
            .unwrap_or(line.source_start);
        let end_byte = line
            .runs
            .iter()
            .map(|r| r.source_range.end)
            .max()
            .unwrap_or(line.source_start);
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

/// Issue #44 — collect on-canvas rects for every inline image in a
/// paragraph box. Mirrors the display-list image math in `render::scene`
/// exactly: the image's bottom edge sits flush with the line baseline,
/// its height is the glyph's `inline_object_height` (the grown ascent),
/// and its width is the reserved glyph advance. Coordinates are absolute
/// device px, matching `LineGeom` / `SelectionChanged.rects`.
fn collect_paragraph_image_rects(
    para_box: &ParagraphBox,
    parent_origin_x: f32,
    parent_origin_y: f32,
    path: &BridgeBlockPath,
    out: &mut Vec<bridge::ImageRect>,
) {
    let para_x = parent_origin_x + para_box.origin.x;
    let para_y = parent_origin_y + para_box.origin.y;
    for line in &para_box.lines {
        let line_x = para_x + line.origin.x;
        let baseline = para_y + line.origin.y + line.baseline;
        /* One pen across the whole line, exactly as `render::scene`
        places glyphs — runs lie left-to-right in visual order. */
        let mut pen = 0.0_f32;
        for run in &line.runs {
            for g in &run.glyphs {
                if let Some(rel) = g.inline_image_rel_id.as_deref() {
                    let x0 = line_x + pen;
                    let y1 = baseline;
                    let y0 = y1 - g.inline_object_height;
                    out.push(bridge::ImageRect {
                        path: path.clone(),
                        at: run.source_range.start + g.cluster,
                        rel_id: rel.to_string(),
                        rect: BridgeRect {
                            x: x0,
                            y: y0,
                            w: g.x_advance,
                            h: g.inline_object_height,
                        },
                        /* Filled from the document model in
                        `image_geometry` — the layout box doesn't carry
                        the EMU extent. */
                        width_emu: 0,
                        height_emu: 0,
                    });
                }
                pen += g.x_advance;
            }
        }
    }
}

/// Walk a table's cells and emit image rects for every inline image
/// inside a cell paragraph — the image analog of `collect_table_line_geom`.
fn collect_table_image_rects(
    table_box: &TableBox,
    table_block_idx: u32,
    table_origin_x: f32,
    table_origin_y: f32,
    out: &mut Vec<bridge::ImageRect>,
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
                collect_paragraph_image_rects(para_box, cell_x, cell_y, &path, out);
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

/// Audit gap B.H3 — pure spatial neighbour scan. Returns the byte of
/// the caret slot whose `x` is strictly greater than `current_x`
/// (`going_right`) or strictly less (`!going_right`); `None` when no
/// such slot exists (the caret sits at the visual edge of the line).
///
/// This is *the* mechanism that makes ArrowLeft / ArrowRight feel
/// natural across BiDi seams: the slot list interleaves bytes from
/// both directional runs at their actual visual x positions, so the
/// scan crosses the seam without needing to know that one logical
/// byte sits before another. A run boundary is just another slot.
///
/// The 0.5 epsilon dodges floating-point ties — a caret sitting
/// exactly on a slot would otherwise be returned as its own neighbour
/// on the same-x edge.
fn neighbor_slot_byte_by_x(slots: &[CaretSlot], current_x: f32, going_right: bool) -> Option<u32> {
    use core::cmp::Ordering;
    let candidate = if going_right {
        slots
            .iter()
            .filter(|s| s.x > current_x + 0.5)
            .min_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(Ordering::Equal))
    } else {
        slots
            .iter()
            .filter(|s| s.x < current_x - 0.5)
            .max_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(Ordering::Equal))
    };
    candidate.map(|s| s.byte)
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

/// Phase 6c — perpendicular x-distance from `target_x` to a line's
/// horizontal span. Zero when `target_x` falls inside `[start_x,
/// start_x + width]`; otherwise the gap to the nearest edge. Used to
/// pick the "right" cell when an Up / Down walk lands on a row with
/// multiple cells (the cell that contains `ideal_x` wins; if none
/// does, the nearest one).
fn x_distance_to_line(line: &LineGeom, target_x: f32) -> f32 {
    let left = line.hit_left;
    let right = line.hit_left + line.hit_width;
    if target_x < left {
        left - target_x
    } else if target_x > right {
        target_x - right
    } else {
        0.0
    }
}

/// Step one grapheme cluster backward in logical byte order (UAX #29).
/// `unicode-segmentation::UnicodeSegmentation::grapheme_indices` returns
/// the byte index of every grapheme break; the caret lands on the LAST
/// break strictly less than the current offset, so combining marks
/// (Arabic harakat, Devanagari conjuncts, emoji ZWJ sequences) traverse
/// as a single user-perceived character (UX_BEHAVIOR_SPEC.md §I.1).
/// At the start of a paragraph the caret jumps to the end of the
/// previous paragraph in the doc-flat walk; at byte 0 of the document
/// it pins.
fn step_left(doc: &DocumentTree, pos: BridgeLogicalPos) -> BridgeLogicalPos {
    use unicode_segmentation::UnicodeSegmentation;
    let off = pos.offset as usize;
    let engine_path = bridge_to_engine_path(pos.path.clone());
    if let Some(para) = doc.paragraph_at_path(&engine_path) {
        if off > 0 {
            let text = &para.text;
            let target = text
                .grapheme_indices(true)
                .map(|(i, _)| i)
                .take_while(|&i| i < off)
                .last()
                .unwrap_or(0);
            return BridgeLogicalPos {
                path: pos.path,
                offset: target as u32,
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

/// Caret position at the start of the visual line containing `caret`
/// (Home key, UX_BEHAVIOR_SPEC §I.2). Finds the LineGeom whose
/// `path == caret.path` and `start_byte..end_byte` brackets the caret,
/// then returns a position at `line.start_byte`. `None` when the
/// document is empty or `caret` doesn't resolve to any line — caller
/// keeps the existing caret in that case.
fn line_home(engine: &Engine, caret: &BridgeLogicalPos) -> Option<BridgeLogicalPos> {
    /* Audit gap B.H2 — direction-aware Home. `start_byte` is the
    paragraph-relative byte of the line's first **logical** character;
    visual leading edge maps to that byte for both LTR (visually
    leftmost) and RTL (visually rightmost) lines. This is the
    direction-aware contract per UX_BEHAVIOR_SPEC §III.4: Home in RTL
    lands on the rightmost slot because the rightmost slot IS the
    logical start. No separate paragraph-direction check needed —
    line geometry already encodes the asymmetry. */
    let geom = engine.document_geometry().ok()?;
    let line = geom.into_iter().find(|l| {
        l.path == caret.path && caret.offset >= l.start_byte && caret.offset <= l.end_byte
    })?;
    Some(BridgeLogicalPos {
        path: caret.path.clone(),
        offset: line.start_byte,
    })
}

/// Caret position at the end of the visual line containing `caret`
/// (End key). Audit gap B.H2 — direction-aware End. Symmetric to
/// [`line_home`]: `end_byte` is the logical end of the line, which
/// is the visually trailing edge of the line — visually rightmost
/// for LTR, visually leftmost for RTL.
fn line_end(engine: &Engine, caret: &BridgeLogicalPos) -> Option<BridgeLogicalPos> {
    let geom = engine.document_geometry().ok()?;
    let line = geom.into_iter().find(|l| {
        l.path == caret.path && caret.offset >= l.start_byte && caret.offset <= l.end_byte
    })?;
    Some(BridgeLogicalPos {
        path: caret.path.clone(),
        offset: line.end_byte,
    })
}

/// UAX #29 word-like segment starts (byte offsets) in `text`. A segment
/// is "word-like" iff `unicode-segmentation` classifies it via
/// `unicode_word_indices` — Letters, Numbers, and a small set of
/// connector chars (`'` inside contractions, `.` / `,` between digits in
/// `3.14` and `1,000`). Whitespace and stand-alone punctuation are NOT
/// word-like — they're the gaps the word-jump motions skip.
fn word_starts(text: &str) -> Vec<usize> {
    use unicode_segmentation::UnicodeSegmentation;
    text.unicode_word_indices().map(|(i, _)| i).collect()
}

/// Jump one UAX #29 word backward in logical byte order — the engine
/// half of Ctrl/Cmd + ArrowLeft in an LTR paragraph (an RTL paragraph
/// flips the visual mapping; see `do_move_caret`). Lands on the start
/// of the previous word-like segment relative to `pos.offset`. At
/// paragraph start the caret hops to the end of the previous paragraph
/// in the doc-flat walk (descending into cells); at document start it
/// pins.
///
/// Word-like classification handles the cases the previous whitespace-
/// only scanner missed: `"isn't"` is ONE word (apostrophe is a Word
/// connector under WB6/WB7), `"3.14"` is ONE word (period between
/// digits is MidNumLet/MidNum under WB11/WB12), and CJK ideographs
/// segment per-ideograph.
fn step_word_left(doc: &DocumentTree, pos: BridgeLogicalPos) -> BridgeLogicalPos {
    let engine_path = bridge_to_engine_path(pos.path.clone());
    let Some(para) = doc.paragraph_at_path(&engine_path) else {
        return pos;
    };
    let text = &para.text;
    let off = (pos.offset as usize).min(text.len());
    if off == 0 {
        if let Some((prev_path, prev_para)) = doc_paragraph_neighbor(doc, &engine_path, false) {
            return BridgeLogicalPos {
                path: engine_to_bridge_path(prev_path),
                offset: prev_para.text.len() as u32,
            };
        }
        return pos;
    }
    /* Last word-start strictly before the caret. `unicode_word_indices`
    is monotonic ascending; the last one < off wins. If the caret sits
    at offset 0 of the first word, `word_starts` may still emit 0 →
    falling through to "no candidate" lands the caret at 0 (pinned at
    paragraph start). */
    let target = word_starts(text)
        .into_iter()
        .take_while(|&i| i < off)
        .last()
        .unwrap_or(0);
    BridgeLogicalPos {
        path: pos.path,
        offset: target as u32,
    }
}

/// Jump one UAX #29 word forward in logical byte order — the engine
/// half of Ctrl/Cmd + ArrowRight in an LTR paragraph. Lands on the
/// **start** of the next word-like segment after `pos.offset` (Word /
/// Google Docs convention — not the end of the current word). At
/// paragraph end the caret hops to offset 0 of the next paragraph; at
/// document end it pins.
fn step_word_right(doc: &DocumentTree, pos: BridgeLogicalPos) -> BridgeLogicalPos {
    let engine_path = bridge_to_engine_path(pos.path.clone());
    let Some(para) = doc.paragraph_at_path(&engine_path) else {
        return pos;
    };
    let text = &para.text;
    let off = (pos.offset as usize).min(text.len());
    if off >= text.len() {
        if let Some((next_path, _)) = doc_paragraph_neighbor(doc, &engine_path, true) {
            return BridgeLogicalPos {
                path: engine_to_bridge_path(next_path),
                offset: 0,
            };
        }
        return pos;
    }
    /* First word-start strictly greater than the caret. If no further
    word exists (caret in trailing whitespace/punctuation), pin at
    paragraph end so a follow-up WordRight hops to the next paragraph. */
    let target = word_starts(text)
        .into_iter()
        .find(|&i| i > off)
        .unwrap_or(text.len());
    BridgeLogicalPos {
        path: pos.path,
        offset: target as u32,
    }
}

/// Step one grapheme cluster forward in logical byte order (UAX #29).
/// At a paragraph's end the caret jumps to offset 0 of the next
/// paragraph in the flat walk; at the document end it pins.
fn step_right(doc: &DocumentTree, pos: BridgeLogicalPos) -> BridgeLogicalPos {
    use unicode_segmentation::UnicodeSegmentation;
    let off = pos.offset as usize;
    let engine_path = bridge_to_engine_path(pos.path.clone());
    if let Some(para) = doc.paragraph_at_path(&engine_path) {
        let text = &para.text;
        if off < text.len() {
            let target = text
                .grapheme_indices(true)
                .map(|(i, _)| i)
                .find(|&i| i > off)
                .unwrap_or(text.len());
            return BridgeLogicalPos {
                path: pos.path,
                offset: target as u32,
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
/// Sample every distinct `SpanStyle` covered by `[lo, hi)` in `para` —
/// the maximal segments coming out of the same per-byte walk
/// `serialize_paragraph` uses. A default-styled gap between explicit
/// spans counts as `SpanStyle::default()`; the empty range yields
/// nothing. Used by `attrs_mixed_over` to detect non-uniform flags
/// across a selection.
fn sample_paragraph_styles(
    para: &engine::Paragraph,
    lo: u32,
    hi: u32,
    record: &mut dyn FnMut(SpanStyle),
) {
    if lo >= hi {
        return;
    }
    let text_len = para.text.len() as u32;
    let lo = lo.min(text_len);
    let hi = hi.min(text_len);
    if lo == hi {
        /* Empty paragraph clipped to nothing — record the default so
        the toolbar still reports a uniform-default state. */
        record(SpanStyle::default());
        return;
    }
    let mut cursor = lo;
    for run in &para.spans {
        if run.end <= cursor {
            continue;
        }
        if run.start >= hi {
            break;
        }
        let rs = run.start.max(cursor);
        let re = run.end.min(hi);
        if rs > cursor {
            record(SpanStyle::default());
        }
        if re > rs {
            record(run.style.clone());
        }
        cursor = re;
    }
    if cursor < hi {
        record(SpanStyle::default());
    }
}

/// Paths to every paragraph STRICTLY BETWEEN `start_path` and `end_path`
/// in the doc-flat walk — i.e., the middle paragraphs of a
/// cross-paragraph selection, excluding the endpoints themselves.
/// Returns an empty vec when no such paragraphs exist (adjacent or
/// same-paragraph selection).
fn doc_paragraph_paths_between(
    doc: &DocumentTree,
    start_path: &EngineBlockPath,
    end_path: &EngineBlockPath,
) -> Vec<EngineBlockPath> {
    let all = doc_paragraph_paths(doc);
    let Some(si) = all.iter().position(|p| p == start_path) else {
        return Vec::new();
    };
    let Some(ei) = all.iter().position(|p| p == end_path) else {
        return Vec::new();
    };
    if si + 1 >= ei {
        return Vec::new();
    }
    all[si + 1..ei].to_vec()
}

/// Count of `Block` entries in the cell addressed by `cell_prefix`
/// (a path with exactly `[Block(t), Cell{r,c}]`). `None` when the
/// prefix doesn't resolve to a real cell. Used by `do_select_cell_at`
/// to know which `Block(N)` step terminates the cell's last paragraph.
fn cell_block_count(doc: &DocumentTree, cell_prefix: &EngineBlockPath) -> Option<usize> {
    if cell_prefix.steps.len() < 2 {
        return None;
    }
    let EnginePathStep::Block(t_idx) = cell_prefix.steps[0] else {
        return None;
    };
    let EnginePathStep::Cell { row, col } = cell_prefix.steps[1] else {
        return None;
    };
    let block = doc.blocks.get(t_idx as usize)?;
    let engine::Block::Table(t) = block else {
        return None;
    };
    let cell = t.rows.get(row as usize)?.cells.get(col as usize)?;
    Some(cell.blocks.len())
}

/// `(start, end)` positions spanning the ENTIRE content of the cell the
/// path descends into (first paragraph offset 0 → last paragraph byte
/// length). `None` when the path is not inside a table cell. Shared by
/// quadruple-click cell selection and Word's Tab-selects-cell semantics.
fn cell_content_span(
    doc: &DocumentTree,
    path: &BridgeBlockPath,
) -> Option<(BridgeLogicalPos, BridgeLogicalPos)> {
    if !matches!(path.steps.get(1), Some(BridgePathStep::Cell { .. })) {
        return None;
    }
    let cell_prefix: Vec<BridgePathStep> = path.steps.iter().take(2).cloned().collect();
    let engine_prefix = bridge_to_engine_path(BridgeBlockPath {
        steps: cell_prefix.clone(),
    });
    let cell_blocks = cell_block_count(doc, &engine_prefix)?;
    let last_block_idx = cell_blocks.saturating_sub(1) as u32;
    let mut start_steps = cell_prefix.clone();
    start_steps.push(BridgePathStep::Block { idx: 0 });
    let mut end_steps = cell_prefix;
    end_steps.push(BridgePathStep::Block {
        idx: last_block_idx,
    });
    let end_path = BridgeBlockPath { steps: end_steps };
    let end_engine = bridge_to_engine_path(end_path.clone());
    let end_len = doc
        .paragraph_at_path(&end_engine)
        .map_or(0, |p| p.text.len() as u32);
    Some((
        BridgeLogicalPos {
            path: BridgeBlockPath { steps: start_steps },
            offset: 0,
        },
        BridgeLogicalPos {
            path: end_path,
            offset: end_len,
        },
    ))
}

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
    affinity: CaretAffinity,
) -> BridgeRect {
    let line = geom
        .iter()
        .find(|l| l.path == pos.path && pos.offset >= l.start_byte && pos.offset <= l.end_byte)
        .or_else(|| geom.first());
    match line {
        Some(line) => BridgeRect {
            x: slot_x_for_byte_with_affinity(line, pos.offset, affinity),
            y: line.y_top,
            w: caret_w,
            h: line.height,
        },
        None => fallback,
    }
}

/// Affinity-aware x lookup. At a BiDi seam two slots share the same
/// byte offset (logical end of one run + logical end of the next
/// directional run); they sit at DIFFERENT x's. Pick whichever the
/// caret's affinity asks for: `LeadingX` = smaller x (left side of
/// the seam), `TrailingX` = greater x (right side).
///
/// When only one slot matches the byte, that slot wins regardless of
/// affinity. When no slot matches, fall back to `line.start_x` like
/// the existing `slot_x_for_byte`.
fn slot_x_for_byte_with_affinity(line: &LineGeom, byte: u32, affinity: CaretAffinity) -> f32 {
    use core::cmp::Ordering;
    let candidates: Vec<&CaretSlot> = line.slots.iter().filter(|s| s.byte == byte).collect();
    if candidates.is_empty() {
        return slot_x_for_byte(line, byte);
    }
    if candidates.len() == 1 {
        return candidates[0].x;
    }
    let pick = match affinity {
        CaretAffinity::LeadingX => candidates
            .iter()
            .min_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(Ordering::Equal)),
        CaretAffinity::TrailingX => candidates
            .iter()
            .max_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(Ordering::Equal)),
    };
    pick.map_or(line.start_x, |s| s.x)
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
    /* Cross-container selection (Backlog table-trap bug). A linear
    drag from a body paragraph BEFORE a table to a body paragraph
    AFTER it should shade the entire table — not just the
    text-spans inside each cell, which would otherwise look like
    fragmented strips with white gutters between cells (broken).
    Detect tables whose top-level block index sits strictly
    between `start.path` and `end.path`, and whose `end.path` is
    NOT a descendant of the table (otherwise the selection ends
    INSIDE the table — partial-cell highlight, not whole-table).
    For lines inside a bracketed table, paint a cell-full-width
    rect (`hit_left .. hit_left + hit_width`) instead of clipping
    against the line's text-spans. Each line still emits its own
    rect, so the highlight tracks line wraps + spans pages
    correctly. */
    let mut bracketed_tables: std::collections::BTreeSet<u32> = Default::default();
    {
        let mut considered: std::collections::BTreeSet<u32> = Default::default();
        for line in geom {
            let Some(BridgePathStep::Block { idx: t_idx }) = line.path.steps.first() else {
                continue;
            };
            if !matches!(line.path.steps.get(1), Some(BridgePathStep::Cell { .. })) {
                continue;
            }
            if !considered.insert(*t_idx) {
                continue;
            }
            let table_path = BridgeBlockPath::top(*t_idx);
            let strictly_after_start = table_path.cmp_doc_order(&start.path) == Ordering::Greater;
            let strictly_before_end = table_path.cmp_doc_order(&end.path) == Ordering::Less
                && !table_path.is_ancestor_of(&end.path);
            if strictly_after_start && strictly_before_end {
                bracketed_tables.insert(*t_idx);
            }
        }
    }
    let mut rects: Vec<BridgeRect> = Vec::new();
    for line in geom {
        /* Skip lines whose paragraph sits before start or after end in
        document order; equal paths clip to the per-paragraph offsets. */
        let cmp_start = line.path.cmp_doc_order(&start.path);
        let cmp_end = line.path.cmp_doc_order(&end.path);
        if cmp_start == Ordering::Less || cmp_end == Ordering::Greater {
            continue;
        }
        /* Bracketed-table cell line → full-cell-width rect (one per line
        of cell content). Skip the per-run text-span clip path entirely
        so cell gutters/padding don't leak white between rects. */
        let in_bracketed = line
            .path
            .steps
            .first()
            .and_then(|s| match s {
                BridgePathStep::Block { idx } => Some(*idx),
                BridgePathStep::Cell { .. } => None,
            })
            .is_some_and(|t_idx| bracketed_tables.contains(&t_idx))
            && matches!(line.path.steps.get(1), Some(BridgePathStep::Cell { .. }));
        if in_bracketed {
            rects.push(BridgeRect {
                x: line.hit_left,
                y: line.y_top,
                w: line.hit_width,
                h: line.height,
            });
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
            underline: style
                .underline
                .unwrap_or(engine::UnderlineStyle::None)
                .is_visible(),
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
        push_run(&mut runs, &para.text, sr.start, sr.end, sr.style.clone());
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

/// Bridge `TextAttrsPatch` → engine `SpanStyle`. Shared by the body
/// `ApplyFormatting` handler and the Phase 3 (#39) story twin.
fn patch_to_span_style(attrs: &TextAttrsPatch) -> SpanStyle {
    SpanStyle {
        font_size: attrs.font_size,
        color: attrs.color.map(|c| [c.r, c.g, c.b, c.a]),
        bold: attrs.bold,
        italic: attrs.italic,
        /* Bridge → engine underline variant mapping (1:1 by name). */
        underline: attrs.underline.map(bridge_to_engine_underline),
        strike: attrs.strike,
        bg_color: attrs.bg_color.map(|c| [c.r, c.g, c.b, c.a]),
        font_family: attrs.font_family.as_deref().and_then(parse_font_family),
        /* Audit gap A.H3 — closed: `TextAttrsPatch` now carries
        `caps` / `small_caps` so the toolbar's Caps controls
        round-trip through the same `ApplyFormatting` path as
        bold / italic. */
        caps: attrs.caps,
        small_caps: attrs.small_caps,
        /* Audit gap A.M1 — `<w:vertAlign>` toggles map from the
        existing `script: Option<VerticalScript>` patch field. The
        bridge enum's `Normal` variant ⇒ explicit `Baseline`
        (defeats an inherited super/subscript from the style chain). */
        vert_align: attrs.script.map(|s| match s {
            bridge::VerticalScript::Superscript => engine::VertAlign::Superscript,
            bridge::VerticalScript::Subscript => engine::VertAlign::Subscript,
            bridge::VerticalScript::Normal => engine::VertAlign::Baseline,
        }),
        /* Audit gap A.M2 — interactive toolbar doesn't surface
        raw-font / theme overrides yet; the open-font path is
        file-round-trip only. */
        raw_font_family: None,
        font_theme: None,
    }
}

impl Engine {
    /// Phase 3 (#39) — story-mode command firewall. While a
    /// header/footer story is active, commands split three ways:
    /// - STORY-SCOPED (typing, deletes, splits, formatting, plain
    ///   paste, selection/caret/hit-test) — handled by their normal
    ///   arms; the handlers themselves branch into the story twins,
    ///   and the geometry + selection-doc adapters re-root the rest.
    /// - GLOBAL (paint, viewport, zoom, stats, undo/redo, save/export,
    ///   fonts, a11y, enter/exit) — fall through unchanged.
    /// - EVERYTHING ELSE (tables, images, comments, hyperlinks,
    ///   sections, page setup, styles, lists, track-changes, IME
    ///   composition, rich clipboard) — rejected loudly. The UI
    ///   disables these controls in story mode (Honest UX); this is
    ///   the engine-side backstop, never the primary affordance.
    ///
    /// Returns `Some(Event::Error)` for a rejected command; `None`
    /// lets `apply`'s normal dispatch proceed. Document loads exit the
    /// story first (their ground is being torn away) and proceed.
    fn story_gate(&mut self, cmd: &Command) -> Option<Event> {
        if !self.story_active() {
            return None;
        }
        match cmd {
            Command::Ping
            | Command::LoadFont { .. }
            | Command::InsertText { .. }
            | Command::DeleteAtCaret { .. }
            | Command::SplitParagraph { .. }
            | Command::ApplyFormatting { .. }
            | Command::PastePlain { .. }
            | Command::SetSelection { .. }
            | Command::ExtendSelection { .. }
            | Command::SelectAll
            | Command::SelectWordAt { .. }
            | Command::SelectParagraphAt { .. }
            | Command::MoveCaret { .. }
            | Command::HitTest { .. }
            | Command::HitTestInPage { .. }
            | Command::PlaceCaretAtPoint { .. }
            | Command::Undo
            | Command::Redo
            | Command::SetViewport { .. }
            | Command::SetZoom { .. }
            | Command::SetDeviceScale { .. }
            | Command::RequestPaint { .. }
            | Command::ExpandLayout { .. }
            | Command::RequestStats
            | Command::RequestAccessibilityDelta
            | Command::SaveDocx
            | Command::SaveDocument { .. }
            | Command::ExportPdf { .. }
            | Command::EnterHeaderFooter { .. }
            | Command::ExitHeaderFooter => None,
            /* Loading a document tears the story's ground away —
            exit first, then handle normally. */
            Command::LoadDocx { .. } | Command::OpenDocument { .. } => {
                self.exit_story_to_body();
                None
            }
            _ => Some(Event::Error {
                message: "This action isn't available while editing a header or footer \
                          — exit the header/footer first."
                    .into(),
            }),
        }
    }

    async fn apply(&mut self, cmd: Command) -> Event {
        if let Some(rejected) = self.story_gate(&cmd) {
            return rejected;
        }
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
                    self.invalidate_layout_snapshot();
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
                let base_scale = device_pixel_ratio.unwrap_or(1.0).max(1.0);
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
                    scale: base_scale,
                    base_scale,
                    zoom: 1.0,
                };
                self.render_page(text, cfg)
            }

            Command::InsertText { at, text } => {
                /* Issue #53 — `None` resolves to the engine's LIVE
                caret, which the serialized queue guarantees reflects
                any `PlaceCaretAtPoint`/`SetSelection` posted before
                this insert. An explicit `at` stays honoured for the
                harness / API callers, and `None` with no selection at
                all keeps the Phase-1 contract (append at end of
                document, emits TextInserted). */
                match self.resolve_interactive_insert_at(at) {
                    Some(p) => self.do_insert_text_interactive(p, text),
                    None => self.insert_text(None, text),
                }
            }
            Command::Undo => self.do_undo(),
            Command::Redo => self.do_redo(),

            Command::LoadDocx { bytes } => self.load_docx_bytes(&bytes, "LoadDocx"),

            Command::SaveDocx => self.save_docx_bytes("SaveDocx"),

            // ===============================================================
            // Phase 2 schema stubs — PHASE_2_BRIDGE_MEMORY.md §4.
            // The typed RPC surface accepts these commands; real engine
            // behavior lands in Phase 3 behind the RequestPaint pipeline.
            // ===============================================================
            Command::Init { .. } => phase3_stub("Init"),
            Command::Recover { .. } => self.do_recover(),
            Command::Dispose => phase3_stub("Dispose"),
            Command::Tick { .. } => phase3_stub("Tick"),
            // Sprint 3 (UI Edition) — Document I/O. OpenDocument /
            // SaveDocument route to the existing legacy load/save_docx
            // pipelines for the Docx format. PlainText + HTML export via
            // crates/format-html and DocumentTree::to_plain_text (closed
            // issue #9).
            Command::OpenDocument {
                bytes,
                format,
                name: _,
            } => match format {
                DocFormat::Docx => self.load_docx_bytes(&bytes, "OpenDocument"),
                other => Event::Error {
                    message: format!(
                        "OpenDocument: format {other:?} not supported — only Docx ships today"
                    ),
                },
            },
            Command::SaveDocument { format } => match format {
                DocFormat::Docx => self.save_docx_bytes("SaveDocument"),
                DocFormat::Pdf => Event::Error {
                    message: "SaveDocument: use ExportPdf for PDF output".into(),
                },
                DocFormat::Html => self.save_html_bytes(),
                DocFormat::PlainText => self.save_plain_text_bytes(),
            },
            Command::ExportPdf { conformance } => self.do_export_pdf(conformance),
            Command::CloseDocument => phase3_stub("CloseDocument"),
            Command::DeleteRange { range } => self.do_delete_range(range),
            Command::ReplaceRange { range, text } => self.do_replace_range(range, text),
            Command::ApplyFormatting { range, attrs } => self.apply_formatting(range, attrs),
            Command::SplitParagraph { at } => self.do_split_paragraph(at),
            Command::MergeParagraph { .. } => phase3_stub("MergeParagraph"),
            // Sprint 3 (UI Edition) — wired now (was a Phase 3 stub
            // through Sprint 1, which silently broke the InsertImageButton
            // I shipped over it).
            Command::InsertImage { at, image, fit } => self.do_insert_image(at, image, fit),
            Command::ResizeImage {
                path,
                at,
                width_emu,
                height_emu,
            } => self.do_resize_image(path, at, width_emu, height_emu),
            Command::SetSelection { range, caret } => self.do_set_selection(range, caret),
            Command::ExtendSelection { to, .. } => self.do_extend_selection(to),
            Command::SelectAll => self.do_select_all(),
            Command::MoveCaret { direction, extend } => self.do_move_caret(direction, extend),
            Command::BeginComposition { at } => self.do_begin_composition(at),
            Command::UpdateComposition { text, target_range } => {
                self.do_update_composition(text, target_range)
            }
            Command::EndComposition { commit } => self.do_end_composition(commit),
            Command::SetViewport { rect } => self.do_set_viewport(rect),
            // Sprint 3 (UI Edition) — wired now (Sprint 1 shipped the
            // ZoomControls component over a stubbed dispatch).
            Command::SetZoom { scale } => self.do_set_zoom(scale),
            Command::SetDeviceScale { scale } => self.do_set_device_scale(scale),
            Command::RequestPaint { viewport, dirty } => self.do_request_paint(viewport, dirty),
            Command::ExpandLayout { target_y } => self.do_expand_layout(target_y),
            Command::UnloadFont { .. } => phase3_stub("UnloadFont"),
            Command::RequestStats => self.request_stats(),

            // Phase 4 — PHASE_4_HEADLESS_UI.md §7. Additive pointer commands.
            Command::HitTest { at } => self.do_hit_test(at),
            Command::HitTestInPage { page, at } => self.do_hit_test_in_page(page, at),
            Command::PlaceCaretAtPoint { page, at } => self.do_place_caret_at_point(page, at),
            Command::GetImageRects => self.do_get_image_rects(),
            Command::SelectWordAt { at } => self.do_select_word_at(at),
            Command::SelectParagraphAt { at } => self.do_select_paragraph_at(at),
            Command::SelectCellAt { at } => self.do_select_cell_at(at),
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
            // Phase 9c — paragraph base direction (`<w:bidi>`), distinct
            // from alignment (UX_BEHAVIOR_SPEC §III, ISO/IEC 29500).
            Command::SetParagraphDirection { range, direction } => {
                self.do_set_paragraph_direction(range, direction)
            }

            // Phase 5 PR 3 — table mutation commands. `BlockPath` flows
            // straight through to the engine; every command flips
            // `Table.dirty = true` so the writer regenerates on save.
            Command::InsertTable { at, rows, cols } => self.do_insert_table(at, rows, cols),
            Command::DeleteTable { path } => self.do_delete_table(path),
            Command::InsertRow {
                table_path,
                row,
                side,
            } => self.do_insert_row(table_path, row, side),
            Command::DeleteRow { table_path, row } => self.do_delete_row(table_path, row),
            Command::InsertColumn {
                table_path,
                col,
                side,
            } => self.do_insert_column(table_path, col, side),
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

            // Sprint 2 (UI Edition) — layout authoring commands.
            Command::SetColumns {
                at,
                count,
                gutter_pt,
            } => self.do_set_columns(at, count, gutter_pt),
            Command::InsertPageBreak { at } => self.do_insert_page_break(at),
            Command::InsertSectionBreak { at, kind } => self.do_insert_section_break(at, kind),
            Command::EnterHeaderFooter { page, area } => self.do_enter_header_footer(page, area),
            Command::ExitHeaderFooter => self.do_exit_header_footer(),
            Command::SetParagraphBorders { range, borders } => {
                self.do_set_paragraph_borders(range, borders)
            }

            // Sprint 4 (UI Edition) — page setup commands.
            Command::SetPageMargins {
                at,
                top_pt,
                right_pt,
                bottom_pt,
                left_pt,
            } => self.do_set_page_margins(at, top_pt, right_pt, bottom_pt, left_pt),
            Command::SetPageOrientation { at, orientation } => {
                self.do_set_page_orientation(at, orientation)
            }

            // Sprint 5 (UI Edition) — list toggle. Off path mutates
            // the model; Bullet / Number return Error until the
            // numbering synthesizer ships.
            Command::ToggleList { range, kind } => self.do_toggle_list(range, kind),
            Command::ChangeListLevel { range, delta } => self.do_change_list_level(range, delta),

            // Sprint 6 (UI Edition) — paragraph indent, line spacing,
            // shading.
            Command::SetParagraphIndent {
                range,
                start_pt,
                end_pt,
                first_line_pt,
            } => self.do_set_paragraph_indent(range, start_pt, end_pt, first_line_pt),
            Command::SetTabStops { range, stops } => self.do_set_tab_stops(range, stops),
            Command::ApplyStyle { range, style_id } => self.do_apply_style(range, style_id),
            Command::ModifyStyle {
                style_id,
                properties,
            } => self.do_modify_style(style_id, properties),
            Command::SetLineSpacing { range, multiplier } => {
                self.do_set_line_spacing(range, multiplier)
            }
            Command::SetParagraphShading { range, color } => {
                self.do_set_paragraph_shading(range, color)
            }

            // Sprint 14 (#14) — review recording commands.
            Command::ToggleTrackChanges { enabled } => self.do_toggle_track_changes(enabled),
            Command::SetReviewIdentity { author, date } => {
                self.do_set_review_identity(author, date)
            }
            Command::AcceptRevision { block, start, end } => {
                self.do_accept_revision(block, start, end)
            }
            Command::RejectRevision { block, start, end } => {
                self.do_reject_revision(block, start, end)
            }
            Command::InsertComment {
                range,
                text,
                author,
            } => self.do_insert_comment(range, text, author),
            Command::DeleteComment { id } => self.do_delete_comment(id),
            Command::ResolveComment { id, resolved } => self.do_resolve_comment(id, resolved),
            Command::ReplyToComment {
                parent_id,
                text,
                author,
            } => self.do_reply_to_comment(parent_id, text, author),
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
        /* A RenderPage reset is a fresh document; a surviving selection
        or IME preview from the previous session would be load-bearing
        for the issue-#53 live-caret insert path (and could splice
        preview text into the new doc). The interactive boot dispatches
        its own SET_SELECTION right after; the Phase-1 harness relies on
        selection being None so `InsertText { at: None }` keeps the
        legacy append contract. */
        self.selection = None;
        self.composition = None;
        /* New document + config — drop every cached paragraph layout.
        The fresh stack restarts revision at 0, so the memo must drop
        explicitly too. */
        self.layout_cache.get_mut().clear();
        self.invalidate_layout_snapshot();

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

    fn apply_formatting(
        &mut self,
        range: Option<BridgeLogicalRange>,
        attrs: TextAttrsPatch,
    ) -> Event {
        if self.story_active() {
            return self.story_apply_formatting(range, attrs);
        }
        /* `None` binds to the engine-owned live selection — mirroring
        the interactive `InsertText` path — because the UI's mirrored
        selection is async and can be stale when a mutation is still
        in flight. An explicit range is a deliberate override. */
        let range = match range {
            Some(r) => r,
            None => match self.selection.as_ref() {
                Some(sel) => {
                    let (start, end) = ordered(sel.anchor.clone(), sel.caret.clone());
                    BridgeLogicalRange { start, end }
                }
                None => {
                    return Event::Error {
                        message: "ApplyFormatting: no range given and no active selection".into(),
                    };
                }
            },
        };
        let patch = patch_to_span_style(&attrs);
        /* Sticky formatting (Backlog #11): a collapsed caret has no text to
        style. Rather than push a no-op edit, arm the patch as the pending
        style — the next interactive InsertText overlays it onto the typed
        text. Toggling the same button merges over the prior pending value.
        Gated on an active selection so the visual-diff harness (which has no
        selection) keeps its original no-op path. */
        if range.start == range.end && self.selection.is_some() {
            let armed = self
                .pending_format
                .clone()
                .unwrap_or_default()
                .merged_with(patch);
            self.pending_format = Some(armed);
            return self.selection_changed();
        }
        /* Sprint 14 (#14) — capture the pre-mutation `SpanStyle` at
        the range's start before applying the patch; the
        `FormatChange` revision carries it as `prev_attrs` so reject
        can restore the original look. */
        let prev_attrs_for_revision = if self.tracking_changes {
            self.undo
                .current()
                .paragraph_at_path(&bridge_to_engine_path(range.start.path.clone()))
                .map(|p| p.style_at(range.start.offset))
                .unwrap_or_default()
        } else {
            SpanStyle::default()
        };
        let new_doc = self.undo.current().apply_style(
            to_engine_pos(range.start.clone()),
            to_engine_pos(range.end.clone()),
            patch,
        );
        let new_doc = if self.tracking_changes {
            new_doc.tracked_format_change(
                to_engine_pos(range.start.clone()),
                to_engine_pos(range.end.clone()),
                prev_attrs_for_revision,
                self.review_author.clone(),
                self.current_review_date(),
            )
        } else {
            new_doc
        };
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
        self.story_validity_guard();
        self.dirty.invalidate(full_page_rect(self.scale()));
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        self.after_history_change()
    }

    /// Phase 3 (#39) — an undo/redo can remove the active story's part
    /// (e.g. undoing past materialize-on-enter) or shrink its paragraph
    /// list under the selection. Exit to body in the first case; clamp
    /// the story selection in the second.
    fn story_validity_guard(&mut self) {
        if !self.story_active() {
            return;
        }
        match self.story_paras() {
            None => self.exit_story_to_body(),
            Some(_) => {
                if let Some(sel) = self.selection.clone() {
                    let clamped = self.with_selection_doc(|d| SelectionState {
                        anchor: clamp_pos(d, sel.anchor),
                        caret: clamp_pos(d, sel.caret),
                        ideal_x: None,
                        kind: sel.kind,
                    });
                    self.selection = Some(clamped);
                }
            }
        }
    }

    /// Issue #22 — warm crash recovery. The worker has already been
    /// respawned by the host (see `EngineClient.onTrap` in
    /// `ts/src/engine/engine-client.ts`) so a fresh `Engine` already
    /// owns the new `OffscreenCanvas`. This handler ensures the
    /// engine starts every recovery path in the same state a cold
    /// boot would — *no* lingering font / layout / undo / a11y
    /// cache from a prior session can poison the next one (the
    /// reported HarfBuzz / Arabic-shaping degradation post-recovery
    /// was a stale-cache symptom).
    ///
    /// `setupEngine` on the TS side re-issues `loadFont` + the boot
    /// `RenderPage` after this returns, so wiping the engine to a
    /// near-cold state is safe and idempotent. Rendering surfaces
    /// (`page_ctxs`, `ctx`, `vello`, `atlas`) are preserved — those
    /// own the live `OffscreenCanvas` context, which cannot be
    /// re-transferred.
    fn do_recover(&mut self) -> Event {
        /* Full-stack rehydration: clear every cache + transient state
        and reset the document to a fresh `DocumentTree`. The TS
        shell's `setupEngine` immediately repopulates fonts + seed
        text via the established cold-boot path. */
        self.fonts.clear();
        self.undo = UndoStack::new(DocumentTree::new(), 100);
        self.layout_cfg = None;
        self.selection = None;
        self.composition = None;
        self.pending_format = None;
        self.layout_cache = new_layout_cache();
        self.invalidate_layout_snapshot();
        self.a11y_cache = None;
        self.image_cache.clear();
        self.last_paint_dims = LastPaintDims::default();
        self.lazy_layout = LazyLayoutState::default();
        self.caret_affinity = CaretAffinity::default();
        self.pending_announcements.clear();
        self.tracking_changes = false;
        self.review_author = "You".to_string();
        self.review_date = String::new();
        self.dirty = DirtyTracker::new();
        /* Phase 3 (#39) — recovery lands in body mode. */
        self.active_story = StoryTarget::Body;
        self.stashed_body_selection = None;
        Event::Recovered {
            applied_commands: 0,
        }
    }

    fn do_redo(&mut self) -> Event {
        self.undo.redo();
        self.story_validity_guard();
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
            paragraph_count: doc.paragraph_count(),
            word_count: doc.word_count(),
            character_count: doc.character_count(),
        })
    }

    fn maybe_repaint_result(&mut self) -> Result<(), Box<Event>> {
        if self.layout_cfg.is_none() {
            return Ok(());
        }
        self.render_document(None).map(|_| ())
    }

    /// Issue #34/#51 — drop the layout memo. Needed only where the
    /// [`UndoStack::revision`] chokepoint cannot see the change: a
    /// whole-stack swap (revision restarts at 0), a font-set change, or
    /// an IME composition update (preview text lives outside the doc).
    fn invalidate_layout_snapshot(&self) {
        self.layout_snapshot.replace(None);
        /* Phase 3 (#40) — the derived-sections memo shares the layout
        snapshot's staleness model exactly: revision-keyed, plus the
        same out-of-band invalidation chokepoints (stack swap resets
        the revision counter, which could collide with a cached
        entry). */
        self.sections_memo.replace(None);
    }

    /// Ensure [`Engine::layout_snapshot`] holds a `build_pages` run for
    /// exactly (current doc revision, `scale`, `target_y`, live
    /// composition). On return the snapshot is populated; callers
    /// `borrow()` it. Consecutive consumers inside one command (paint →
    /// geometry → probes) hit the memo instead of re-laying the band.
    fn ensure_layout_snapshot(
        &self,
        scale: f32,
        with_composition: bool,
        target_y: Option<f32>,
    ) -> Result<(), Box<Event>> {
        let composition_active = with_composition && self.composition.is_some();
        let doc_revision = self.undo.revision();
        let scale_bits = scale.to_bits();
        let target_bits = target_y.map(f32::to_bits);
        let viewport_h_bits = self.lazy_layout.viewport_h.to_bits();
        {
            let snap = self.layout_snapshot.borrow();
            if let Some(s) = snap.as_ref()
                && s.doc_revision == doc_revision
                && s.scale_bits == scale_bits
                && s.target_bits == target_bits
                && s.viewport_h_bits == viewport_h_bits
                && s.composition_active == composition_active
            {
                return Ok(());
            }
        }
        let (pages, _fonts, page_paths, info) =
            self.build_pages(scale, with_composition, target_y)?;
        *self.layout_snapshot.borrow_mut() = Some(LayoutSnapshot {
            doc_revision,
            scale_bits,
            target_bits,
            viewport_h_bits,
            composition_active,
            pages,
            page_paths,
            info,
        });
        Ok(())
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
    /// Audit gap C.H1 — `target_y == Some(y)` switches the build into
    /// viewport-cull mode: the paginator stops processing further blocks
    /// once `emitted_pages_height >= y + LAYOUT_BUFFER_PT`. `None`
    /// preserves the historical "lay everything out" contract — used by
    /// PDF export and hit-tests that need every page resolved.
    ///
    /// The returned [`LazyLayoutInfo`] carries the cull state so the
    /// caller can synthesize the scrollbar's virtual height + know
    /// whether more `ExpandLayout` round-trips are needed.
    #[allow(clippy::type_complexity)]
    fn build_pages(
        &self,
        scale: f32,
        with_composition: bool,
        target_y: Option<f32>,
    ) -> Result<
        (
            Vec<PageBox>,
            FontStack,
            Vec<Vec<EngineBlockPath>>,
            LazyLayoutInfo,
        ),
        Box<Event>,
    > {
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
        let sctx = StyleContext::of(&doc);
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
        /* Phase 8a — pre-resolve the footnote-body table the paginator
        uses to grow the bottom band. The paginator keys by *display
        number* (the marker text it sees on glyphs); we discover the
        OOXML w:id → display_number mapping by scanning every body
        paragraph's `InlineKind::FootnoteRef`. */
        let footnote_bodies = build_footnote_bodies(&doc, &font_stack, &cfg, scale);
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
        /* Audit gap C.H1 — viewport-cull bookkeeping. `processed_blocks`
        is the running count of top-level blocks fully consumed by the
        paginator (paragraph or table). `total_blocks` is the doc-wide
        count we'll subtract from to derive `remaining_blocks` for the
        scrollbar estimator. `cull_budget` is the absolute Y past which
        we may stop processing further blocks; `None` means "lay
        everything out" (PDF export, hit-test, etc.). The 50 % buffer
        on top of `LAYOUT_BUFFER_PT` is the slack that keeps a smooth
        scroll from outrunning the layout. */
        let mut processed_blocks: u32 = 0;
        let total_blocks: u32 = doc.blocks.len() as u32;
        /* L3.1 (#5) — predictive runway. See `lazy_runway` for the
        `max(LAYOUT_BUFFER_PT, viewport_h × scale × 2)` rationale: a
        fast-flick scroll up to one viewport-height past the current
        band still hits laid-out pages, eliminating the blank-frame
        window. Falls back to the legacy `LAYOUT_BUFFER_PT` floor for
        small viewports and pre-viewport clients. */
        let runway = lazy_runway(self.lazy_layout.viewport_h, scale);
        let cull_budget = target_y.map(|y| y + runway);
        let mut culled = false;
        let gap = render::scene::PAGE_GAP_PT * scale;
        let height_so_far = |pages: &[PageBox], in_progress: f32| -> f32 {
            if pages.is_empty() {
                return in_progress;
            }
            pages.iter().map(|p| p.size.height + gap).sum::<f32>() - gap
                + if in_progress > 0.0 {
                    gap + in_progress
                } else {
                    0.0
                }
        };

        'outer: for (sect_idx, section) in sections.iter().enumerate() {
            let geom = scaled_paginator_geometry(section.geometry, scale);
            /* Audit gap A.M12 — `Continuous` section break swaps
            geometry / columns / page-numbering in place on the SAME
            paginator without flushing the page. The new section's
            first block flows directly below the previous section.
            First-section is always non-continuous (there's no prior
            page to continue from). */
            let is_continuous = sect_idx > 0
                && matches!(section.section_type, engine::SectionType::Continuous)
                && paginator.is_some();
            if !is_continuous && let Some(p) = paginator.take() {
                /* NextPage / EvenPage / OddPage section break — finish
                the prior paginator (which flushes the in-progress
                page if any), then build a fresh one below. */
                let mut pages = p.finish();
                let consume = pages.len();
                emitted_pages.append(&mut pages);
                let mut paths_taken: Vec<Vec<EngineBlockPath>> = std::mem::take(&mut page_paths);
                paths_taken.resize_with(consume, Vec::new);
                emitted_paths.append(&mut paths_taken);
            }
            /* Phase 6b — resolve header / footer text the parser stashed
            on `doc.headers` / `doc.footers` (keyed by `r:id`) into laid-
            out paragraphs the renderer paints into the margin bands. */
            let content_w = geom.width - geom.margins.left - geom.margins.right;
            let lay_band = |slot: Option<&String>,
                            table: &std::collections::HashMap<String, Vec<engine::Paragraph>>|
             -> Option<layout::HeaderFooterBox> {
                let rid = slot?;
                build_header_footer_box(table.get(rid)?, content_w, &font_stack, &cfg, scale, sctx)
            };
            let headers = layout::HeaderBands {
                default: lay_band(section.header_refs.default.as_ref(), &doc.headers),
                first: lay_band(section.header_refs.first.as_ref(), &doc.headers),
                even: lay_band(section.header_refs.even.as_ref(), &doc.headers),
            };
            let footers = layout::HeaderBands {
                default: lay_band(section.footer_refs.default.as_ref(), &doc.footers),
                first: lay_band(section.footer_refs.first.as_ref(), &doc.footers),
                even: lay_band(section.footer_refs.even.as_ref(), &doc.footers),
            };
            if is_continuous {
                /* Audit gap A.M12 — in-place section swap. Keep the
                existing paginator's `cur_y` / `cur_blocks` so the new
                section's content begins immediately below the prior
                content on the same page. Headers / footers / title-pg
                flag belong to the NEW section but only matter on the
                page that flushes — the current page's header / footer
                stays whatever the prior section had. */
                let pag = paginator
                    .as_mut()
                    .expect("continuous: paginator must exist");
                /* L2.3 (#8) — column balance pass. Runs BEFORE
                `set_section_cursor` / `set_columns` so the prior
                multi-column section's column descriptor is still
                installed on the paginator. The pass redistributes
                the prior section's `cur_blocks` so all columns end
                near the average height and updates `cur_y` to the
                deepest column's bottom edge — the new section's
                first block then lands cleanly below that edge. */
                pag.balance_current_section_columns();
                pag.set_section_cursor(geom);
                /* Re-install the new section's column descriptor.
                `set_columns` already resets `cur_column_index = 0`
                which is the right answer for a continuous swap —
                the new column layout starts fresh below the prior
                content. `cur_y` is preserved by `set_columns`. */
                pag.set_columns(
                    section.columns.count.max(1),
                    section.columns.gutter_pt * scale,
                );
                let doc_offset = emitted_pages.len() as u32;
                pag.set_page_numbering(section.page_num, doc_offset);
            } else {
                let mut pag = Paginator::new(
                    geom,
                    headers,
                    footers,
                    section.title_pg,
                    doc.settings.even_and_odd_headers,
                )
                .with_footnote_bodies(footnote_bodies.clone());
                if section.columns.is_multi() {
                    pag.set_columns(section.columns.count, section.columns.gutter_pt * scale);
                }
                /* Audit gap A.M11 — install pgNumType + doc-offset so
                PAGE fields in this section render as section-relative
                / formatted numbers. */
                let doc_offset = emitted_pages.len() as u32;
                pag.set_page_numbering(section.page_num, doc_offset);
                paginator = Some(pag);
                page_paths.clear();
                page_paths.push(Vec::new());
            }

            for block_idx in section.start_block..section.end_block {
                /* Audit gap C.H1 — viewport-cull stop. Check at the
                top of every block (so we don't half-process a block).
                Compare the total height already committed (finished
                pages + the paginator's in-progress cursor) against
                the cull budget. We DON'T stop mid-section because the
                paginator's flush_page on section break is a hard
                guarantee — stopping mid-section would emit a
                half-flushed page. The check fires at every block
                boundary, which is enough granularity for typical
                docs (50 pages × ~20 paragraphs each = 1000 boundaries). */
                if let Some(budget) = cull_budget {
                    /* The paginator finalises overflow pages INTERNALLY
                    (they only reach `emitted_pages` at a section break),
                    and `cursor_y` resets to the top of each new page — so
                    the committed height must count the paginator's own
                    finalised pages too. Without them a single-section
                    document never trips the budget and "lazy" layout
                    quietly lays out every page (the #51 hang). */
                    let pag_h = paginator.as_ref().map_or(0.0, |p| {
                        p.emitted_pages_height()
                            + p.page_count_emitted() as f32 * gap
                            + p.cursor_y()
                    });
                    if height_so_far(&emitted_pages, pag_h) >= budget {
                        culled = true;
                        break 'outer;
                    }
                }
                let Some(block) = doc.blocks.get(block_idx as usize) else {
                    continue;
                };
                let pag = paginator.as_mut().expect("paginator created");
                let para_path = EngineBlockPath::top(block_idx);

                match block {
                    engine::Block::Table(t) => {
                        let mut tb = layout_table_box(
                            t,
                            pag.column_width(),
                            &font_stack,
                            &cfg,
                            scale,
                            sctx,
                            &mut cache,
                        );
                        assign_source_ids_table(&mut tb, &mut next_para_id);
                        let prev_pages_in_pag = pag.page_count_emitted();
                        pag.push_block(LayoutBlock::Table(tb), 0.0, 0.0);
                        attach_block_paths(pag, prev_pages_in_pag, &mut page_paths, &para_path);
                        processed_blocks += 1;
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
                            let spans = apply_revision_overlay(
                                apply_hyperlink_overlay(
                                    composition_layout_spans(
                                        para,
                                        sctx,
                                        off as u32,
                                        c.text.len() as u32,
                                        cfg.px_size,
                                        scale,
                                    ),
                                    &para.hyperlinks,
                                    [0, 0, 0, 255],
                                ),
                                &para.revisions,
                                [0, 0, 0, 255],
                            );
                            let base_direction = resolve_base_direction(para, &cfg);
                            let (ind_s, ind_e, ind_fl, ind_h) =
                                effective_layout_indents(para, base_direction, scale);
                            let tab_stops_px = tab_stops_to_layout_px(&para.props.tab_stops, scale);
                            let (lh_px, lh_exact) =
                                resolve_line_height(para.props.line_height, cfg.line_height, scale);
                            layout_paragraph(ParagraphConfig {
                                text: &text,
                                fonts: &font_stack,
                                spans: &spans,
                                base_direction,
                                max_width: pag.column_width(),
                                line_height: lh_px,
                                line_height_exact: lh_exact,
                                alignment: para.props.alignment.map_or(cfg.alignment, layout_align),
                                indent_start_px: ind_s,
                                indent_end_px: ind_e,
                                first_line_indent_px: ind_fl,
                                hanging_indent_px: ind_h,
                                marker_text: para.resolved_marker.clone(),
                                px_size_for_marker: cfg.px_size * scale,
                                inline_objects: &[],
                                tab_stops_px: &tab_stops_px,
                            })
                        } else {
                            /* Issue #51/#34 — same shared LRU-backed
                            layout the table cells use. */
                            layout_paragraph_cached(
                                para,
                                &font_stack,
                                &cfg,
                                scale,
                                pag.column_width(),
                                sctx,
                                &mut cache,
                            )
                        };
                        let before_px = twips_to_layout_px(para.props.spacing.before_twips, scale);
                        let after_px = twips_to_layout_px(para.props.spacing.after_twips, scale);
                        let mut para_box = para_box;
                        para_box.source_paragraph_id = next_para_id;
                        next_para_id += 1;
                        /* Phase 2 audit (gap D.1) — propagate complex-field
                        overlays so the paginator can re-evaluate PAGE /
                        NUMPAGES per page when the paragraph lands. */
                        para_box.fields = para
                            .fields
                            .iter()
                            .map(|f| layout::LayoutField {
                                byte_range: f.start..f.end,
                                instruction: f.instruction.clone(),
                                evaluated_text: None,
                            })
                            .collect();
                        /* Phase 2 audit (gap A.12) — scan paragraph text
                        for U+000C FORM FEED (the reader's mapping of
                        `<w:br w:type="page"/>`) and stamp the
                        containing line index onto
                        `page_break_after_line`. The paginator's
                        push_paragraph_split consults this list before
                        the budget-based split so each page-break char
                        forces a flush regardless of remaining content
                        height. */
                        para_box.page_break_after_line =
                            compute_page_break_lines(&para.text, &para_box);
                        /* Audit gap A.M4 — propagate `<w:pBdr>` strokes
                        into the laid-out box so the renderer can paint
                        them against the paragraph rect. Cloning the
                        engine model's `CellBorders` is cheap (a handful
                        of `Option<BorderStroke>` slots). */
                        para_box.borders = para.props.borders.clone();
                        /* Sprint 6 (UI Edition) — propagate `<w:shd>`
                        paragraph shading into the laid-out box. */
                        para_box.shading = para.props.shading;
                        let prev_pages_in_pag = pag.page_count_emitted();
                        pag.push_block(LayoutBlock::Paragraph(para_box), before_px, after_px);
                        attach_block_paths(pag, prev_pages_in_pag, &mut page_paths, &para_path);
                        processed_blocks += 1;
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
                header_offset: default_geom.header_offset,
                footer_offset: default_geom.footer_offset,
                footnotes: Vec::new(),
            });
            emitted_paths.push(Vec::new());
        }
        /* Audit gap C.H1 — fold the cull bookkeeping into the result. */
        let info = LazyLayoutInfo {
            is_full_layout: !culled,
            remaining_blocks: total_blocks.saturating_sub(processed_blocks),
        };
        Ok((emitted_pages, font_stack, emitted_paths, info))
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
        let (mut pages, fonts, mut paths, _info) =
            self.build_pages(scale, with_composition, None)?;
        let page = pages.drain(..).next().unwrap_or_else(|| PageBox {
            size: Size {
                width: 0.0,
                height: 0.0,
            },
            margins: A4Page::a4().margin,
            blocks: Vec::new(),
            header: None,
            footer: None,
            header_offset: 0.0,
            footer_offset: 0.0,
            footnotes: Vec::new(),
        });
        let p0 = paths.drain(..).next().unwrap_or_default();
        Ok((page, fonts, p0))
    }

    fn render_document(&mut self, clip: Option<Rect>) -> Result<RenderStats, Box<Event>> {
        /* `true` — splice the live IME composition preview into the
        paint. Audit gap C.H1 — `target_y` comes from `lazy_layout`
        (high-water mark the TS shell has asked us to cover). The
        running estimate the scrollbar reads adds an
        `AVG_BLOCK_HEIGHT_PT` fudge per skipped block on top.
        Issue #34 — the band layout comes from the shared snapshot; the
        `selection_changed → document_geometry` that follows every
        mutation then reuses these exact boxes instead of re-laying. */
        let target_y = Some(self.lazy_layout.min_target_y * self.scale());
        self.ensure_layout_snapshot(self.scale(), true, target_y)?;
        let snap_cell = self.layout_snapshot.borrow();
        let snap = snap_cell
            .as_ref()
            .expect("ensure_layout_snapshot populated the memo");
        let pages = &snap.pages;
        let info = snap.info;

        let mut line_count: u32 = 0;
        let mut glyph_count: u32 = 0;
        for page in pages {
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
        let scene = render::scene::build_document_scene(pages, gap);
        /* Stats report the first page's dimensions for back-compat; the
        `page_count` / total document height live alongside `paint_ms` in
        the `Painted` event so the TS shell can resize the canvas to fit
        every page (the multi-page scrolled viewport is wired in the
        post-Phase-6 sprint). */
        let (page_width, page_height) = pages
            .first()
            .map(|p| (p.size.width, p.size.height))
            .unwrap_or((0.0, 0.0));
        let document_height: f32 = if pages.is_empty() {
            0.0
        } else {
            pages.iter().map(|p| p.size.height + gap).sum::<f32>() - gap
        };
        /* Audit gap C.H1 — virtual scrollbar height. When every block
        was consumed, the estimate IS the real height. When the cull
        budget halted us early, pad with a per-block average so the
        scrollbar is large enough to allow scrolling into the
        not-yet-laid-out tail; the estimate only ever *shrinks* as
        background `ExpandLayout` calls fill in (real heights are
        bounded above by the over-estimating average — the comment on
        `AVG_BLOCK_HEIGHT_PT` calls this out). */
        let estimated_document_height: f32 = if info.is_full_layout {
            document_height
        } else {
            document_height + (info.remaining_blocks as f32) * AVG_BLOCK_HEIGHT_PT * scale
        };
        /* Issue #26 — accumulate the exact per-page top offsets the
        geometry walk uses, so overlays and pointer math stop deriving
        them from uniform-A4 constants. */
        let mut page_tops = Vec::with_capacity(pages.len());
        let mut page_heights = Vec::with_capacity(pages.len());
        let mut page_margin_tops = Vec::with_capacity(pages.len());
        let mut page_margin_bottoms = Vec::with_capacity(pages.len());
        let mut top_acc = 0.0f32;
        for page in pages {
            page_tops.push(top_acc);
            page_heights.push(page.size.height);
            /* Phase 3 (#39) — per-page margins for the shell's
            header/footer zone gate; exact under mixed-geometry
            sections. */
            page_margin_tops.push(page.margins.top);
            page_margin_bottoms.push(page.margins.bottom);
            top_acc += page.size.height + gap;
        }
        let stats = RenderStats {
            page_width,
            page_height,
            line_count,
            glyph_count,
            document_height,
            is_full_layout: info.is_full_layout,
            estimated_document_height,
            page_count: pages.len() as u32,
            page_tops,
            page_heights,
            page_margin_tops,
            page_margin_bottoms,
        };

        /* Vello path: encode the whole display list and present it over
        WebGPU. Vello runs its own GPU-side glyph cache, so the Canvas2D
        `GlyphAtlas` stays untouched. Clipped partial repaint is not wired on
        this path yet — Vello redraws the full scene each frame. */
        if let Some(vr) = self.vello.as_mut() {
            /* Issue #54 — keep the WebGPU surface sized to the page-0
            target instead of freezing it at INIT-time dimensions; a
            DPR / zoom change otherwise renders cropped forever. */
            vr.resize(page_width.round() as u32, page_height.round() as u32);
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
            self.last_paint_dims = LastPaintDims {
                document_height: stats.document_height,
                page_count: stats.page_count,
                estimated_document_height: stats.estimated_document_height,
                is_full_layout: stats.is_full_layout,
                page_tops: stats.page_tops.clone(),
                page_heights: stats.page_heights.clone(),
                page_margin_tops: stats.page_margin_tops.clone(),
                page_margin_bottoms: stats.page_margin_bottoms.clone(),
            };
            return Ok(stats);
        }

        /* Canvas2D path — Phase 6c multi-canvas architecture. Each page
        gets its own `<canvas>` element (registered by the TS shell via
        `set_page_canvas`); we paint each page into ITS canvas at local
        origin `(0, 0)`. No more single-canvas-grows-unbounded — a
        50-page document is 50 canvases of `~1190 × 1684` device px,
        each well inside Safari's 4096 / Chrome's 32 k size limits.
        Pages whose canvas hasn't been registered yet (TS shell racing
        the engine's page count) silently skip; the next paint after
        registration picks them up. */
        let _ = clip;
        let image_cache = &self.image_cache;
        for (idx, page) in pages.iter().enumerate() {
            let Some(ctx_opt) = self.page_ctxs.get(idx) else {
                continue;
            };
            let Some(ctx) = ctx_opt.clone() else {
                continue;
            };
            let page_scene = render::scene::build_single_page_scene(page);
            let canvas = ctx.canvas();
            let want_w = page.size.width.max(1.0).ceil() as u32;
            let want_h = page.size.height.max(1.0).ceil() as u32;
            if canvas.width() != want_w {
                canvas.set_width(want_w);
            }
            if canvas.height() != want_h {
                canvas.set_height(want_h);
            }
            let clip_rect = Rect::new(0.0, 0.0, f64::from(want_w), f64::from(want_h));
            if let Err(e) = render_canvas2d(
                &ctx,
                &page_scene,
                &mut self.atlas,
                |id| self.fonts.get(id).cloned(),
                |rel| image_cache.get(rel).cloned(),
                clip_rect,
            ) {
                return Err(Box::new(Event::Error {
                    message: format!("paint page {idx}: {e:?}"),
                }));
            }
        }

        self.last_paint_dims = LastPaintDims {
            document_height: stats.document_height,
            page_count: stats.page_count,
            estimated_document_height: stats.estimated_document_height,
            is_full_layout: stats.is_full_layout,
            page_tops: stats.page_tops.clone(),
            page_heights: stats.page_heights.clone(),
            page_margin_tops: stats.page_margin_tops.clone(),
            page_margin_bottoms: stats.page_margin_bottoms.clone(),
        };
        Ok(stats)
    }

    /// Export the current document to a single-page PDF (D3.7). Always laid
    /// out at scale `1.0` — PDF user space is logical points, never device px.
    ///
    /// `conformance` selects the output profile (D5.4 / issue #28): `A1b`
    /// emits PDF/A-1b, `A2u` emits PDF/A-2u, `X3` emits PDF/X-3:2003 —
    /// each maps 1:1 onto its `format_pdf::PdfProfile`.
    fn do_export_pdf(&self, conformance: PdfConformance) -> Event {
        /* `false` — a PDF export is the committed document, never the
        in-progress IME composition. */
        /* `target_y: None` — PDF export is a full-document materialization,
        not a viewport paint. The cull budget would corrupt page count. */
        let (pages, font_stack, _box_paths, _info) = match self.build_pages(1.0, false, None) {
            Ok(v) => v,
            Err(e) => return *e,
        };
        let profile = match conformance {
            PdfConformance::A1b => format_pdf::PdfProfile::A1b,
            PdfConformance::A2u => format_pdf::PdfProfile::A2u,
            PdfConformance::X3 => format_pdf::PdfProfile::X3,
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
    /// Audit gap C.H1 — bump the lazy-layout high-water mark to cover
    /// `target_y_px` (device px from doc top). Any interactive
    /// (hit-test, pointer click) that targets a Y past the current
    /// laid-out tail must call this before `document_geometry()` so
    /// the next layout pass extends to the new target. Cheaper than
    /// `target_y: None` because the layout still respects the
    /// established buffer — clicking on page 5 of a 50-page doc
    /// extends to page 5, not all 50.
    fn lazy_layout_bump_for_y(&mut self, target_y_px: f32) {
        let scale = self.scale().max(0.0001);
        let target_pt = target_y_px / scale;
        if target_pt > self.lazy_layout.min_target_y {
            self.lazy_layout.min_target_y = target_pt;
        }
    }

    /// Audit gap C.H1 — the TS shell broadcasts the scrolled viewport
    /// (in document-relative device pixels) on every scroll tick. The
    /// engine records the visible band + lifts `min_target_y` so the
    /// next paint lays out down to the bottom of the buffered band.
    /// Returns the cached dimensions as a synthetic `Painted`; the
    /// TS shell re-sizes its scrollbar against `estimated_document_height`
    /// without waiting for the next real paint.
    fn do_set_viewport(&mut self, rect: BridgeRect) -> Event {
        let scale = self.scale().max(0.0001);
        let bottom_pt = (rect.y + rect.h) / scale;
        self.lazy_layout.viewport_y = rect.y / scale;
        self.lazy_layout.viewport_h = rect.h / scale;
        if bottom_pt > self.lazy_layout.min_target_y {
            self.lazy_layout.min_target_y = bottom_pt;
        }
        let dims = self.last_paint_dims.clone();
        Event::Painted {
            dirty: BridgeRect {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            },
            version: u64::from(self.undo.depth()),
            paint_ms: 0.0,
            document_height: dims.document_height,
            page_count: dims.page_count,
            is_full_layout: dims.is_full_layout,
            estimated_document_height: dims.estimated_document_height,
            page_tops: dims.page_tops,
            page_heights: dims.page_heights,
            image_count: self.undo.current().count_inline_images(),
            page_margin_tops: dims.page_margin_tops,
            page_margin_bottoms: dims.page_margin_bottoms,
        }
    }

    /// Audit gap C.H1 — explicit "lay out down to `target_y` pt" request.
    /// Used by the TS shell on scroll-near-bottom and by interactive
    /// motions that would otherwise land in the not-yet-laid-out tail
    /// (Ctrl+End, PageDown past the buffered band). Issues a fresh
    /// paint so the new pages reach the canvas. `target_y` is in
    /// layout pt at scale=1 (the engine multiplies by `self.scale()`
    /// when it cull-checks).
    fn do_expand_layout(&mut self, target_y: f32) -> Event {
        if target_y > self.lazy_layout.min_target_y {
            self.lazy_layout.min_target_y = target_y;
        }
        /* Re-render with the bumped target_y. The viewport rect we
        pass is irrelevant — paint is computed against the new
        `min_target_y`, not the rect arg. */
        let scale = self.scale();
        let rect = BridgeRect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: target_y * scale,
        };
        self.do_request_paint(rect, None)
    }

    fn do_request_paint(&mut self, viewport: BridgeRect, dirty: Option<BridgeRect>) -> Event {
        let drained = self.dirty.drain();
        let region = dirty
            .map(bridge_to_kurbo)
            .or(drained)
            .unwrap_or_else(|| bridge_to_kurbo(viewport));
        /* Audit gap C.H1 — fold viewport hint into lazy_layout so the
        next `render_document` call lays out down to the visible
        bottom. Caller may have just sent a `SetViewport`; this is
        also a safety net for clients that skip the explicit setter. */
        let bottom_pt = (viewport.y + viewport.h) / self.scale();
        if bottom_pt > self.lazy_layout.min_target_y {
            self.lazy_layout.min_target_y = bottom_pt;
        }
        let stats = match self.render_document(Some(region)) {
            Ok(s) => s,
            Err(e) => return *e,
        };
        Event::Painted {
            dirty: kurbo_to_bridge(region),
            version: u64::from(self.undo.depth()),
            paint_ms: 0.0,
            document_height: stats.document_height,
            page_count: stats.page_count,
            is_full_layout: stats.is_full_layout,
            estimated_document_height: stats.estimated_document_height,
            page_tops: stats.page_tops,
            page_heights: stats.page_heights,
            image_count: self.undo.current().count_inline_images(),
            page_margin_tops: stats.page_margin_tops,
            page_margin_bottoms: stats.page_margin_bottoms,
        }
    }

    /* ===========================================================
    Phase 3 (#39) — the story adapter. While a header/footer story
    is active, selection paths are story-rooted; these helpers give
    every existing consumer a story-shaped view without touching the
    body machinery.
    =========================================================== */

    fn story_active(&self) -> bool {
        !matches!(self.active_story, StoryTarget::Body)
    }

    /// The active story's paragraphs (cloned), or `None` in body mode /
    /// when the part vanished (undo past materialize-on-enter).
    fn story_paras(&self) -> Option<Vec<engine::Paragraph>> {
        let doc = self.undo.current();
        match &self.active_story {
            StoryTarget::Body => None,
            StoryTarget::Header { rid, .. } => doc.headers.get(rid).cloned(),
            StoryTarget::Footer { rid, .. } => doc.footers.get(rid).cloned(),
        }
    }

    /// Build the synthetic story-rooted tree the existing pure
    /// `DocumentTree` mutators + text lookups run against. Clone set per
    /// the Phase 3 design review: styles / style_defaults /
    /// style_run_defaults / numbering MUST ride along (cascade + marker
    /// resolution read them); media / footnotes / comments stay empty
    /// (cheap + irrelevant — story commands touching them are gated).
    fn story_doc(&self) -> Option<DocumentTree> {
        let paras = self.story_paras()?;
        let real = self.undo.current();
        let mut tree = DocumentTree::from_rich_paragraphs(paras);
        tree.styles = real.styles.clone();
        tree.style_defaults = real.style_defaults.clone();
        tree.style_run_defaults = real.style_run_defaults.clone();
        tree.numbering = real.numbering.clone();
        Some(tree)
    }

    /// Run `f` against the document the CURRENT SELECTION's paths
    /// address: the real tree in body mode, the synthetic story tree in
    /// story mode. Every read helper that resolves `sel.caret.path` to
    /// a paragraph goes through here so story paths can never
    /// mis-resolve against body blocks.
    fn with_selection_doc<R>(&self, f: impl FnOnce(&DocumentTree) -> R) -> R {
        match self.story_doc() {
            Some(t) => f(&t),
            None => f(self.undo.current()),
        }
    }

    /// Owned variant for callers that need to hold the tree across
    /// mutation steps (`do_move_caret`'s word-steps).
    fn selection_doc(&self) -> DocumentTree {
        self.with_selection_doc(|d| d.clone())
    }

    /// Wire shape of the active story for `SelectionChanged`.
    fn bridge_story_ref(&self) -> Option<bridge::BridgeStoryRef> {
        match &self.active_story {
            StoryTarget::Body => None,
            StoryTarget::Header { rid, page, .. } => Some(bridge::BridgeStoryRef {
                area: bridge::HeaderFooterArea::Header,
                rid: rid.clone(),
                page: *page,
            }),
            StoryTarget::Footer { rid, page, .. } => Some(bridge::BridgeStoryRef {
                area: bridge::HeaderFooterArea::Footer,
                rid: rid.clone(),
                page: *page,
            }),
        }
    }

    /// Story-mode line geometry: the anchor page's band paragraphs
    /// flattened into the same `LineGeom` shape body geometry produces,
    /// at absolute page coordinates. Band placement comes from
    /// `PageBox::header_band_top` / `footer_band_top` — the SAME
    /// methods `render::scene` paints with, so caret geometry and
    /// pixels cannot diverge. Paths are story-rooted (`Block(i)`).
    fn story_geometry(&self, page_idx: u32, header: bool) -> Result<Vec<LineGeom>, Box<Event>> {
        let target_y = Some(self.lazy_layout.min_target_y * self.scale());
        self.ensure_layout_snapshot(self.scale(), false, target_y)?;
        let snap_cell = self.layout_snapshot.borrow();
        let snap = snap_cell
            .as_ref()
            .expect("ensure_layout_snapshot populated the memo");
        let pages = &snap.pages;
        let gap = render::scene::PAGE_GAP_PT * self.scale();
        let mut page_top = 0.0_f32;
        let mut geom: Vec<LineGeom> = Vec::new();
        let idx = (page_idx as usize).min(pages.len().saturating_sub(1));
        for page in pages.iter().take(idx) {
            page_top += page.size.height + gap;
        }
        let Some(page) = pages.get(idx) else {
            return Ok(geom);
        };
        let band = if header { &page.header } else { &page.footer };
        let Some(band) = band else {
            return Ok(geom);
        };
        let content_x = page.margins.left;
        let content_w = page.size.width - page.margins.left - page.margins.right;
        let band_top = page_top
            + if header {
                page.header_band_top()
            } else {
                page.footer_band_top()
            };
        for (i, para_box) in band.paragraphs.iter().enumerate() {
            collect_paragraph_line_geom(
                para_box,
                content_x,
                band_top,
                content_x,
                content_w,
                &BridgeBlockPath {
                    steps: vec![BridgePathStep::Block { idx: i as u32 }],
                },
                &mut geom,
            );
        }
        Ok(geom)
    }

    /// Flatten the current document into per-line hit-test geometry. PR 4:
    /// recurses into table cells so a click inside a cell maps to a
    /// `BlockPath` ending at the cell's paragraph (`[Block(t), Cell{r,c},
    /// Block(p)]`).
    fn document_geometry(&self) -> Result<Vec<LineGeom>, Box<Event>> {
        /* Phase 3 (#39) — in story mode every geometry consumer
        (hit-test, caret rect, selection rects, arrow walks) sees the
        band's lines instead of the body's; combined with
        `with_selection_doc` this is the whole read-side adapter. */
        match &self.active_story {
            StoryTarget::Header { page, .. } => return self.story_geometry(*page, true),
            StoryTarget::Footer { page, .. } => return self.story_geometry(*page, false),
            StoryTarget::Body => {}
        }
        /* `false` — hit-test + caret geometry run on committed document
        offsets, which `self.selection` is also expressed in. Audit gap
        C.H1 — geometry queries that need to resolve a deep caret /
        hit-test target must lay out far enough to cover it. `target_y`
        is the high-water mark of any caret motion or scroll the TS
        shell has requested; `do_hit_test_in_page` and the keyboard
        navigation helpers bump it ahead of calling this. */
        let target_y = Some(self.lazy_layout.min_target_y * self.scale());
        /* Issue #34 — reuse the auto-repaint's boxes when the doc /
        scale / band haven't changed since (`false` and `true`
        composition modes produce identical spans while no composition
        is live, and the snapshot key folds that in). */
        self.ensure_layout_snapshot(self.scale(), false, target_y)?;
        let snap_cell = self.layout_snapshot.borrow();
        let snap = snap_cell
            .as_ref()
            .expect("ensure_layout_snapshot populated the memo");
        let pages = &snap.pages;
        let page_paths = &snap.page_paths;
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

    /// Issue #44 — flatten the current document into absolute-device-px
    /// image rectangles (with resize addresses) for the shell's
    /// resize-handle overlay + image hit-testing. Reuses the cached
    /// layout snapshot like `document_geometry`, so it never re-lays-out.
    fn image_geometry(&self) -> Result<Vec<bridge::ImageRect>, Box<Event>> {
        let target_y = Some(self.lazy_layout.min_target_y * self.scale());
        self.ensure_layout_snapshot(self.scale(), false, target_y)?;
        let snap_cell = self.layout_snapshot.borrow();
        let snap = snap_cell
            .as_ref()
            .expect("ensure_layout_snapshot populated the memo");
        let pages = &snap.pages;
        let page_paths = &snap.page_paths;
        let gap = render::scene::PAGE_GAP_PT * self.scale();
        let mut out: Vec<bridge::ImageRect> = Vec::new();
        let mut page_top: f32 = 0.0;
        for (pi, page) in pages.iter().enumerate() {
            let content_x = page.margins.left;
            let content_y = page_top + page.margins.top;
            let paths = page_paths.get(pi).map(|v| v.as_slice()).unwrap_or(&[]);
            for (i, layout_block) in page.blocks.iter().enumerate() {
                let Some(path) = paths.get(i) else {
                    continue;
                };
                match layout_block {
                    LayoutBlock::Paragraph(para_box) => {
                        collect_paragraph_image_rects(
                            para_box,
                            content_x,
                            content_y,
                            &engine_to_bridge_path(path.clone()),
                            &mut out,
                        );
                    }
                    LayoutBlock::Table(table_box) => {
                        let table_block_idx = path.last_block_index().unwrap_or(0);
                        collect_table_image_rects(
                            table_box,
                            table_block_idx,
                            content_x + table_box.origin.x,
                            content_y + table_box.origin.y,
                            &mut out,
                        );
                    }
                }
            }
            page_top += page.size.height + gap;
        }
        /* Enrich each rect with its EMU extent from the document model
        (the layout box only knows the reserved px size). The shell scales
        resizes by `new_emu = emu × new_px / old_px`, so it needs the
        current EMU alongside the current px rect. */
        let doc = self.undo.current();
        for im in &mut out {
            let epath = bridge_path_to_engine(&im.path);
            if let Some(p) = doc.paragraph_at_path(&epath)
                && let Some(engine::InlineKind::Image {
                    width_emu,
                    height_emu,
                    ..
                }) = p
                    .inline_objects
                    .iter()
                    .find(|io| io.at == im.at)
                    .map(|io| &io.kind)
            {
                im.width_emu = *width_emu;
                im.height_emu = *height_emu;
            }
        }
        Ok(out)
    }

    /// `Command::GetImageRects` — reply with every inline image's rect.
    fn do_get_image_rects(&self) -> Event {
        match self.image_geometry() {
            Ok(images) => Event::ImageRects { images },
            Err(e) => *e,
        }
    }

    /// `Command::HitTest` — pixel → logical position. A pure query; the
    /// selection is not mutated. Coords are in absolute document-device
    /// pixels (the legacy single-canvas convention). For the Phase 6c
    /// multi-canvas DOM architecture, the TS shell uses the page-aware
    /// path below.
    fn do_hit_test(&mut self, at: BridgePoint) -> Event {
        /* Audit gap C.H1 — hit-test guardrail. The click's Y might
        sit past the lazy-laid tail (user scrolled into estimated
        space then clicked). Bump `min_target_y` to cover it so the
        next `document_geometry()` extends the layout down to the
        click. `at.y` is doc-relative device px; lazy_layout stores
        layout pt at scale=1. */
        let scale = self.scale().max(0.0001);
        let target_pt = at.y / scale;
        if target_pt > self.lazy_layout.min_target_y {
            self.lazy_layout.min_target_y = target_pt;
        }
        match self.document_geometry() {
            Ok(geom) => Event::HitResult {
                pos: hit_test_geom(&geom, at.x, at.y),
            },
            Err(e) => *e,
        }
    }

    /// Phase 6c — `Command::HitTestInPage` — pixel → logical position with
    /// the click expressed in the clicked page's LOCAL device-pixel
    /// coordinates (origin at the page's top-left). The engine adds the
    /// page's accumulated top offset in document space and calls the
    /// same geometry walker. Lets the multi-canvas TS shell route
    /// pointer events from N independent `<canvas>` elements without
    /// any TS-side offset math.
    fn do_hit_test_in_page(&mut self, page_idx: u32, at: BridgePoint) -> Event {
        let scale = self.scale();
        let gap = render::scene::PAGE_GAP_PT * scale;
        /* Issue #34/#51 — this used to request a FULL document layout
        (`target_y = None`) on every click and every drag pointermove,
        which on a long document re-shaped everything the cull had
        skipped. The clicked page's `<canvas>` only exists because a
        previous paint laid that page out, so the culled band already
        covers it in the normal case; when it doesn't (band shrank
        after a zoom change), extend the band to just past the clicked
        page instead of laying out the world. */
        let page_top_in_band = |snap: &LayoutSnapshot| -> Option<f32> {
            if (page_idx as usize) >= snap.pages.len() {
                return None;
            }
            let mut top = 0.0f32;
            for page in snap.pages.iter().take(page_idx as usize) {
                top += page.size.height + gap;
            }
            Some(top)
        };
        let target_y = Some(self.lazy_layout.min_target_y * scale);
        if let Err(e) = self.ensure_layout_snapshot(scale, false, target_y) {
            return *e;
        }
        let mut page_top = {
            let snap_cell = self.layout_snapshot.borrow();
            page_top_in_band(snap_cell.as_ref().expect("snapshot populated"))
        };
        if page_top.is_none() {
            let assumed_pt = (page_idx as f32 + 1.0)
                * (engine::PageGeometry::a4().height + render::scene::PAGE_GAP_PT);
            if assumed_pt > self.lazy_layout.min_target_y {
                self.lazy_layout.min_target_y = assumed_pt;
            }
            let target_y = Some(self.lazy_layout.min_target_y * scale);
            if let Err(e) = self.ensure_layout_snapshot(scale, false, target_y) {
                return *e;
            }
            let snap_cell = self.layout_snapshot.borrow();
            let snap = snap_cell.as_ref().expect("snapshot populated");
            /* Still short (non-A4 geometry taller than assumed): clamp
            to the last laid page rather than hit-testing thin air. */
            page_top = Some(page_top_in_band(snap).unwrap_or_else(|| {
                let mut top = 0.0f32;
                let all_but_last = snap.pages.len().saturating_sub(1);
                for page in snap.pages.iter().take(all_but_last) {
                    top += page.size.height + gap;
                }
                top
            }));
        }
        let global_y = at.y + page_top.unwrap_or(0.0);
        match self.document_geometry() {
            Ok(geom) => Event::HitResult {
                pos: hit_test_geom(&geom, at.x, global_y),
            },
            Err(e) => *e,
        }
    }

    /// Issue #53 — where an interactive `InsertText { at: None }`
    /// lands: the explicit position wins, else the live caret, else
    /// `None` (the Phase-1 append-at-end contract).
    fn resolve_interactive_insert_at(
        &self,
        at: Option<BridgeLogicalPos>,
    ) -> Option<BridgeLogicalPos> {
        at.or_else(|| self.selection.as_ref().map(|s| s.caret.clone()))
    }

    /// Issue #53 — `Command::PlaceCaretAtPoint`: hit-test + collapsed
    /// selection in ONE dispatch. The shell posts this synchronously at
    /// pointerdown (no awaited round-trip in between), so every
    /// keystroke that enters the serialized queue after the click is
    /// guaranteed to execute against the just-clicked caret.
    fn do_place_caret_at_point(&mut self, page_idx: u32, at: BridgePoint) -> Event {
        let pos = match self.do_hit_test_in_page(page_idx, at) {
            Event::HitResult { pos } => pos,
            other => return other,
        };
        self.do_set_selection(
            BridgeLogicalRange {
                start: pos.clone(),
                end: pos.clone(),
            },
            pos,
        )
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
        /* Audit gap B.M4 — non-arrow selection change resets the BiDi
        seam affinity to the leading-x default. The arrow-key motions
        carry their own directional intent; a click, SetSelection,
        SelectAll, paste, or type starts a fresh visual journey from
        the leading slot at a seam. */
        self.caret_affinity = CaretAffinity::default();
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
        /* Audit gap B.M4 — shift+click is a non-arrow caret motion;
        reset affinity so the new caret position renders at the
        leading-x seam slot. */
        self.caret_affinity = CaretAffinity::default();
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
        self.lazy_layout_bump_for_y(at.y);
        let geom = match self.document_geometry() {
            Ok(g) => g,
            Err(e) => return *e,
        };
        let hit = hit_test_geom(&geom, at.x, at.y);
        let engine_path = bridge_to_engine_path(hit.path.clone());
        let (lo, hi) = self.with_selection_doc(|d| {
            d.paragraph_at_path(&engine_path)
                .map_or((hit.offset, hit.offset), |p| p.word_bounds(hit.offset))
        });
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
        self.lazy_layout_bump_for_y(at.y);
        let geom = match self.document_geometry() {
            Ok(g) => g,
            Err(e) => return *e,
        };
        let hit = hit_test_geom(&geom, at.x, at.y);
        let engine_path = bridge_to_engine_path(hit.path.clone());
        let len = self.with_selection_doc(|d| {
            d.paragraph_at_path(&engine_path)
                .map_or(0, |p| p.text.len() as u32)
        });
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

    /// `Command::SelectCellAt` — hit-test, then select every byte of
    /// the containing cell (quadruple-click inside a table).
    /// `UX_BEHAVIOR_SPEC §I.3`. The hit's path tells us which cell —
    /// path steps `[Block(t_idx), Cell{r,c}, Block(0)]` etc.
    /// Anchor lands at offset 0 of the cell's first paragraph; caret
    /// lands at `text.len()` of its last paragraph. Outside any cell
    /// the command falls back to `SelectAll`.
    fn do_select_cell_at(&mut self, at: BridgePoint) -> Event {
        self.lazy_layout_bump_for_y(at.y);
        let geom = match self.document_geometry() {
            Ok(g) => g,
            Err(e) => return *e,
        };
        let hit = hit_test_geom(&geom, at.x, at.y);
        let in_cell = matches!(hit.path.steps.get(1), Some(BridgePathStep::Cell { .. }));
        if !in_cell {
            return self.do_select_all();
        }
        /* Cell address = first two steps of the hit's path
        (`Block(t_idx)` + `Cell{r,c}`); `cell_content_span` builds the
        first-paragraph → last-paragraph span. */
        let Some((start, end)) = cell_content_span(self.undo.current(), &hit.path) else {
            return self.do_select_all();
        };
        self.pending_format = None;
        self.selection = Some(SelectionState {
            anchor: start,
            caret: end,
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
        /* Audit gap B.M4 — SelectAll is a non-arrow motion; reset
        affinity. */
        self.caret_affinity = CaretAffinity::default();
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
        /* Phase 3 (#39) — story-aware document view; word-steps + doc-end
        must walk the story's paragraphs, not the body's. */
        let doc = self.selection_doc();
        /* UAX #9 visual-arrow mapping (UX_BEHAVIOR_SPEC §III.1).
        Plain Left/Right are SPATIAL — they consult LineGeom slot x's
        to find the visual neighbour, so a caret inside an Arabic word
        in an LTR paragraph still moves visually right when the user
        presses Right (the paragraph-direction flip would step
        logical-forward, which is visually LEFT inside the RTL run —
        the very bug §III.1 calls out).
        Word-jump (WordLeft/WordRight) keeps the paragraph-direction
        flip — Word's observed Ctrl+Arrow behaviour in mixed text is
        paragraph-level, not run-level (jumps logical word boundaries
        regardless of the caret's current run direction). */
        let para_rtl = self.paragraph_direction_at(&sel.caret.path) == ShapingDirection::Rtl;
        let mut new_affinity = self.caret_affinity;
        let (new_caret, new_ideal) = match direction {
            MoveDirection::Left => {
                let (p, aff) = self.visual_step_arrow(&sel.caret, false);
                new_affinity = aff;
                (p, None)
            }
            MoveDirection::Right => {
                let (p, aff) = self.visual_step_arrow(&sel.caret, true);
                new_affinity = aff;
                (p, None)
            }
            MoveDirection::WordLeft => {
                let f = if para_rtl {
                    step_word_right
                } else {
                    step_word_left
                };
                new_affinity = CaretAffinity::LeadingX;
                (f(&doc, sel.caret.clone()), None)
            }
            MoveDirection::WordRight => {
                let f = if para_rtl {
                    step_word_left
                } else {
                    step_word_right
                };
                new_affinity = CaretAffinity::TrailingX;
                (f(&doc, sel.caret.clone()), None)
            }
            MoveDirection::LineHome => (
                line_home(self, &sel.caret).unwrap_or(sel.caret.clone()),
                None,
            ),
            MoveDirection::LineEnd => (
                line_end(self, &sel.caret).unwrap_or(sel.caret.clone()),
                None,
            ),
            MoveDirection::DocHome => (bpos_top(0, 0), None),
            MoveDirection::DocEnd => {
                let last_path = doc
                    .path_to_last_top_paragraph()
                    .unwrap_or(EngineBlockPath::top(0));
                let last_len = doc
                    .paragraph_at_path(&last_path)
                    .map_or(0, |p| p.text.len() as u32);
                (
                    BridgeLogicalPos {
                        path: engine_to_bridge_path(last_path),
                        offset: last_len,
                    },
                    None,
                )
            }
            MoveDirection::NextCell | MoveDirection::PrevCell => {
                if self.story_active() {
                    /* No tables inside stories — Tab is a no-op. */
                    return self.selection_changed();
                }
                (
                    cell_tab_step(
                        &mut self.undo,
                        &sel.caret,
                        direction == MoveDirection::NextCell,
                    ),
                    None,
                )
            }
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
                /* Phase 6c — spatial vertical walk. Locate the caret's
                line in `geom`, then look for the geometrically next
                line above (Up) / below (Down) by Y position — NOT by
                `geom` index order. Tables interleave cells in
                document order that don't match the visual order:
                cell(0,1) is to the *right* of cell(0,0) on the same
                row, so a doc-order walk Up from cell(1,0) jumps to
                cell(0,1) (the previous block) instead of cell(0,0).
                Among lines at the target Y, prefer the one whose
                horizontal span contains `ideal_x` (handles same-row
                cells with different widths). */
                let cur = geom.iter().find(|l| {
                    l.path == caret_path
                        && caret_offset >= l.start_byte
                        && caret_offset <= l.end_byte
                });
                let target: Option<&LineGeom> = if let Some(cur_line) = cur {
                    let going_up = direction == MoveDirection::Up;
                    /* Find best candidate at the next y-level. */
                    let mut best: Option<&LineGeom> = None;
                    for line in &geom {
                        if std::ptr::eq(line, cur_line) {
                            continue;
                        }
                        let on_other_side = if going_up {
                            line.y_top + 0.5 < cur_line.y_top
                        } else {
                            line.y_top > cur_line.y_top + 0.5
                        };
                        if !on_other_side {
                            continue;
                        }
                        best = match best {
                            None => Some(line),
                            Some(b) => {
                                /* Prefer the candidate CLOSER in Y to the
                                current line; tie-break by X-overlap with
                                `ideal` (cells at same row Y but different
                                X spans). */
                                let cur_dy = (line.y_top - cur_line.y_top).abs();
                                let b_dy = (b.y_top - cur_line.y_top).abs();
                                if cur_dy < b_dy - 0.5 {
                                    Some(line)
                                } else if (cur_dy - b_dy).abs() < 0.5 {
                                    let line_x_dist = x_distance_to_line(line, ideal);
                                    let b_x_dist = x_distance_to_line(b, ideal);
                                    if line_x_dist < b_x_dist {
                                        Some(line)
                                    } else {
                                        Some(b)
                                    }
                                } else {
                                    Some(b)
                                }
                            }
                        };
                    }
                    /* Boundary escape — when the spatial scan returns
                    nothing AND the caret sits inside a table cell,
                    explicitly hop out to the body paragraph adjacent
                    to the table. Without this, pressing Up at the
                    top row / Down at the bottom row with no body
                    line on the correct side of the table TRAPS the
                    caret inside the table (the spatial scan needs a
                    geom line to land on, and the scan above filters
                    lines by strict y-side relative to `cur_line` —
                    if the only candidate is on the wrong y-side, it
                    is rejected). `insert_table` now appends a
                    trailing paragraph so Down usually finds it
                    through the spatial scan; this fallback covers
                    the residual cases (in particular when geometry
                    rounding misattributes the trailing line's
                    y_top, or when the caret is in a top-of-doc
                    table and the only escape route is below). */
                    if best.is_none() {
                        let cur_table_idx = cur_line.path.steps.first().and_then(|s| match s {
                            BridgePathStep::Block { idx } => Some(*idx),
                            BridgePathStep::Cell { .. } => None,
                        });
                        let in_cell = matches!(
                            cur_line.path.steps.get(1),
                            Some(BridgePathStep::Cell { .. })
                        );
                        if let (Some(t_idx), true) = (cur_table_idx, in_cell) {
                            best = geom
                                .iter()
                                .filter(|l| {
                                    /* Body lines (no Cell step) whose top-level
                                    block index sits on the correct side of the
                                    current table. */
                                    let outside_table = !matches!(
                                        l.path.steps.get(1),
                                        Some(BridgePathStep::Cell { .. })
                                    );
                                    let l_idx = l.path.steps.first().and_then(|s| match s {
                                        BridgePathStep::Block { idx } => Some(*idx),
                                        BridgePathStep::Cell { .. } => None,
                                    });
                                    outside_table
                                        && match l_idx {
                                            Some(i) if going_up => i < t_idx,
                                            Some(i) => i > t_idx,
                                            None => false,
                                        }
                                })
                                .min_by(|a, b| {
                                    /* Going up → the closest body line is
                                    the one with the GREATEST y_top below
                                    `cur_line` in document order (i.e.
                                    last paragraph above the table).
                                    Going down → the SMALLEST y_top
                                    above `cur_line` (first paragraph
                                    below the table). */
                                    let key = |l: &&LineGeom| {
                                        if going_up { -l.y_top } else { l.y_top }
                                    };
                                    key(a)
                                        .partial_cmp(&key(b))
                                        .unwrap_or(core::cmp::Ordering::Equal)
                                });
                        }
                    }
                    best
                } else {
                    /* Fallback to first line when current isn't found
                    (defensive — caret should always sit on a geom line). */
                    if direction == MoveDirection::Down {
                        geom.first()
                    } else {
                        geom.last()
                    }
                };
                let new_caret = match target {
                    Some(line) => {
                        /* An empty paragraph's placeholder line carries
                        no slots — fall back to the line's start_byte
                        (= end_byte = 0) so the caret still LANDS on
                        that line instead of bouncing back to the
                        current position. Without this, Down from
                        the bottom row of a table whose trailing
                        paragraph is empty resolves the right target
                        line, then loses the caret because
                        `nearest_slot_by_x` returns None on a slotless
                        line. */
                        let offset = nearest_slot_by_x(&line.slots, ideal)
                            .map(|s| s.byte)
                            .unwrap_or(line.start_byte);
                        BridgeLogicalPos {
                            path: line.path.clone(),
                            offset,
                        }
                    }
                    None => sel.caret.clone(),
                };
                (new_caret, Some(ideal))
            }
        };
        let new_caret = clamp_pos(self.undo.current(), new_caret);
        /* Word parity — Tab/Shift+Tab don't just move the caret: they
        select the destination cell's ENTIRE content (which collapses
        naturally in a freshly-appended empty row). No-op steps
        (outside a table, Shift+Tab in the first cell) keep plain
        caret semantics. Read the doc fresh — `cell_tab_step` may have
        just appended a row. */
        if matches!(direction, MoveDirection::NextCell | MoveDirection::PrevCell)
            && !extend
            && new_caret != sel.caret
            && let Some((start, end)) = cell_content_span(self.undo.current(), &new_caret.path)
        {
            self.pending_format = None;
            self.caret_affinity = new_affinity;
            let kind = derive_selection_kind(&start, &end);
            self.selection = Some(SelectionState {
                anchor: start,
                caret: end,
                ideal_x: None,
                kind,
            });
            return self.selection_changed();
        }
        self.pending_format = None;
        let anchor = if extend {
            sel.anchor.clone()
        } else {
            new_caret.clone()
        };
        let kind = derive_selection_kind(&anchor, &new_caret);
        self.caret_affinity = new_affinity;
        self.selection = Some(SelectionState {
            anchor,
            caret: new_caret,
            ideal_x: new_ideal,
            kind,
        });
        self.selection_changed()
    }

    /// Sprint 10 — queue an `aria-live` announcement for the worker to
    /// drain after the current command's primary reply lands. The
    /// engine owns the wording + priority so the TS shell never has
    /// to compute ARIA semantics from event payloads.
    fn announce(&mut self, priority: AnnouncementPriority, message: impl Into<String>) {
        self.pending_announcements.push((priority, message.into()));
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
            caret: caret_rect_geom(
                &geom,
                &sel.caret,
                fallback,
                CARET_WIDTH * scale,
                self.caret_affinity,
            ),
            direction,
            rects,
            /* A collapsed caret reflects any armed pending style; a real
            selection reports the document's own attributes (Backlog #11). */
            attrs_at_caret: self.attrs_at(probe, start == end),
            paragraph_alignment: self.paragraph_alignment_at(&sel.caret.path),
            paragraph_style_id: self.with_selection_doc(|d| {
                d.paragraph_at_path(&bridge_to_engine_path(sel.caret.path.clone()))
                    .and_then(|p| p.style_id.clone())
            }),
            can_undo: self.undo.can_undo(),
            can_redo: self.undo.can_redo(),
            selection_kind: sel.kind.clone(),
            attrs_mixed: self.attrs_mixed_over(&start, &end),
            paragraph_direction: self.paragraph_direction_over(&start, &end),
            section_geometry: self.section_geometry_for_caret(&sel.caret.path),
            cell_properties: self.cell_properties_for_caret(&sel.caret.path),
            tab_stops: self.tab_stops_for_caret(&sel.caret.path),
            paragraph_indent: self.indent_for_caret(&sel.caret.path),
            is_tracking_changes: self.tracking_changes,
            undo_depth: self.undo.depth(),
            list_ilvl: self.list_ilvl_for_caret(&sel.caret.path),
            paragraph_borders: self.paragraph_borders_for_caret(&sel.caret.path),
            editing_story: self.bridge_story_ref(),
        }
    }

    /// Issue #42 — the paragraph-under-caret's `<w:numPr><w:ilvl>`, or
    /// `None` when it is not a list item / not addressable. Drives the
    /// shell's Tab-key handler so it can dispatch `ChangeListLevel`
    /// instead of falling back to tab-char insertion.
    fn list_ilvl_for_caret(&self, path: &BridgeBlockPath) -> Option<u8> {
        let engine_path = bridge_path_to_engine(path);
        /* Phase 3 (#39) — `with_selection_doc` re-roots the lookup into
        the active story so a story path can never mis-resolve against
        a body block (same discipline for every caret-path read below). */
        self.with_selection_doc(|d| {
            d.paragraph_at_path(&engine_path)
                .and_then(|p| p.list_item)
                .map(|item| item.ilvl)
        })
    }

    /// Sprint 15 (#13) — extract the paragraph-under-caret's
    /// `<w:pPr><w:ind>` indentation as a wire-shaped [`BridgeIndent`]
    /// in layout pt. `Default` (all-zero) when the caret is not inside
    /// a paragraph — same contract as [`Self::tab_stops_for_caret`]
    /// returning empty. `first_line_pt` folds the engine's mutually-
    /// exclusive `first_line_twips` / `hanging_twips` into one SIGNED
    /// pt value (`> 0` first-line, `< 0` hanging), matching
    /// `Command::SetParagraphIndent`'s convention so a Ruler drag
    /// round-trips without re-deriving the sign.
    ///
    /// Issue #58 — this used to read raw `para.props.indent` directly, so
    /// a toggled list (whose indent lives in `resolved_list_indent`, not
    /// `props.indent`) read back as all-zero even though the paragraph
    /// visibly rendered indented. Routes through the same per-slot merge
    /// [`effective_layout_indents`] uses so the Ruler can never lie about
    /// what was actually painted. `paragraph_direction_at` resolves the
    /// same base-direction precedence `effective_layout_indents`' RTL
    /// slot-swap depends on.
    fn indent_for_caret(&self, path: &BridgeBlockPath) -> BridgeIndent {
        let engine_path = bridge_path_to_engine(path);
        let base_direction = self.paragraph_direction_at(path);
        self.with_selection_doc(|d| {
            let Some(para) = d.paragraph_at_path(&engine_path) else {
                return BridgeIndent::default();
            };
            let (s, e, fl, hang) = effective_indent_twips(para, base_direction);
            /* twips → pt (20 twips = 1 pt). first_line/hanging are mutually
            exclusive in OOXML; at most one is non-zero, so the difference
            recovers the signed offset. */
            BridgeIndent {
                start_pt: s as f32 / 20.0,
                end_pt: e as f32 / 20.0,
                first_line_pt: (fl - hang) as f32 / 20.0,
            }
        })
    }

    /// Sprint 11 (#13) — extract the paragraph-under-caret's
    /// `<w:tabs>` stops as a wire-shaped `Vec<BridgeTabStop>`. Empty
    /// when the caret is not inside a paragraph or when the paragraph
    /// has no custom tabs (Ruler renders the default grid).
    fn tab_stops_for_caret(&self, path: &BridgeBlockPath) -> Vec<bridge::BridgeTabStop> {
        let engine_path = bridge_path_to_engine(path);
        self.with_selection_doc(|d| {
            let Some(para) = d.paragraph_at_path(&engine_path) else {
                return Vec::new();
            };
            para.props
                .tab_stops
                .iter()
                .map(|s| bridge::BridgeTabStop {
                    position_pt: s.position_pt,
                    kind: match s.kind {
                        engine::TabKind::Left => bridge::BridgeTabKind::Left,
                        engine::TabKind::Center => bridge::BridgeTabKind::Center,
                        engine::TabKind::Right => bridge::BridgeTabKind::Right,
                        engine::TabKind::Decimal => bridge::BridgeTabKind::Decimal,
                        engine::TabKind::Clear => bridge::BridgeTabKind::Clear,
                    },
                })
                .collect()
        })
    }

    /// Phase 3 (#40) — run `f` against the revision-keyed derived
    /// sections table ([`Engine::sections_memo`]). The underlying
    /// `effective_sections()` derive walks every top-level block, and
    /// its consumers sit on the per-keystroke `SelectionChanged` path —
    /// so amortize to a slice scan against the memo, rebuilt only when
    /// the undo revision moves.
    fn with_cached_sections<R>(&self, f: impl FnOnce(&[engine::Section]) -> R) -> R {
        let revision = self.undo.revision();
        {
            let memo = self.sections_memo.borrow();
            if let Some((rev, sections)) = memo.as_ref()
                && *rev == revision
            {
                return f(sections);
            }
        }
        let sections = self.undo.current().effective_sections();
        let out = f(&sections);
        *self.sections_memo.borrow_mut() = Some((revision, sections));
        out
    }

    /// Phase 3 (#40) — `DocumentTree::section_for_block` through the
    /// memo. Falls back to the trailing section for a beyond-the-end
    /// index, mirroring the engine helper's body_section fallthrough.
    fn section_for_block_cached(&self, block_idx: u32) -> engine::Section {
        self.with_cached_sections(|sections| {
            sections
                .iter()
                .find(|s| s.start_block <= block_idx && block_idx < s.end_block)
                .or_else(|| sections.last())
                .cloned()
                .unwrap_or_default()
        })
    }

    /// Sprint 10 — resolve the section that covers the caret's
    /// top-level block and project its geometry onto the wire shape.
    /// Returns `None` for an empty path (the caret can't sit nowhere
    /// in a valid selection, but the helper is defensive).
    fn section_geometry_for_caret(&self, path: &BridgeBlockPath) -> Option<BridgeSectionGeometry> {
        /* Phase 3 (#39) — a story caret's path is story-rooted and says
        nothing about body sections; the owning section was resolved at
        EnterHeaderFooter time. */
        let top_idx = match &self.active_story {
            StoryTarget::Header { section_block, .. }
            | StoryTarget::Footer { section_block, .. } => *section_block,
            StoryTarget::Body => match path.steps.first()? {
                BridgePathStep::Block { idx } => *idx,
                BridgePathStep::Cell { .. } => return None,
            },
        };
        let (section, section_index, section_count) = self.with_cached_sections(|sections| {
            let idx = sections
                .iter()
                .position(|s| s.start_block <= top_idx && top_idx < s.end_block)
                .unwrap_or(sections.len().saturating_sub(1));
            (
                sections.get(idx).cloned().unwrap_or_default(),
                idx as u32,
                sections.len() as u32,
            )
        });
        let geo = section.geometry;
        let orientation = if geo.width > geo.height {
            BridgePageOrientation::Landscape
        } else {
            BridgePageOrientation::Portrait
        };
        Some(BridgeSectionGeometry {
            width_pt: geo.width,
            height_pt: geo.height,
            margin_top_pt: geo.margin_top,
            margin_right_pt: geo.margin_right,
            margin_bottom_pt: geo.margin_bottom,
            margin_left_pt: geo.margin_left,
            orientation,
            columns: u32::from(section.columns.count.max(1)),
            column_gutter_pt: section.columns.gutter_pt,
            section_index,
            section_count,
        })
    }

    /// Sprint 10 — project the innermost cell's shading + borders into
    /// the wire shape; `None` outside any table.
    fn cell_properties_for_caret(&self, path: &BridgeBlockPath) -> Option<BridgeCellProperties> {
        let engine_path = bridge_path_to_engine(path);
        let cell = self.undo.current().innermost_cell_props_at(&engine_path)?;
        Some(BridgeCellProperties {
            shading: cell.shading.map(rgba_to_bridge_color),
            borders: engine_borders_to_bridge(cell.borders.as_ref()),
        })
    }

    /// Issue #41 — the caret paragraph's `<w:pPr><w:pBdr>` per-edge
    /// borders as a wire `BridgeCellBorders`, or `None` when the
    /// paragraph carries no borders. Feeds the border picker's prefill so
    /// it reflects the live document instead of a local optimistic guess.
    fn paragraph_borders_for_caret(&self, path: &BridgeBlockPath) -> Option<BridgeCellBorders> {
        let engine_path = bridge_path_to_engine(path);
        self.with_selection_doc(|d| {
            let para = d.paragraph_at_path(&engine_path)?;
            para.props
                .borders
                .as_ref()
                .map(|b| engine_borders_to_bridge(Some(b)))
        })
    }

    /// Compute the per-flag "mixed across the selection" bitmap. A
    /// collapsed caret has nothing to disagree over — all `false`. For
    /// a real range we walk the paragraph(s) the selection spans and
    /// compare each contributing `SpanStyle`'s flags against the first
    /// observed value; any disagreement flips the flag to mixed.
    ///
    /// Cross-container ranges (paragraph in body + paragraph in cell)
    /// fall back to scanning each endpoint's paragraph independently —
    /// good enough until cross-container linear selections land
    /// (UX_BEHAVIOR_SPEC §IV.6, GH #3).
    fn attrs_mixed_over(
        &self,
        start: &BridgeLogicalPos,
        end: &BridgeLogicalPos,
    ) -> bridge::AttrsMixed {
        if start == end {
            return bridge::AttrsMixed::default();
        }
        /* Phase 3 (#39) — story-aware: paths resolve against the story
        tree while a header/footer is being edited. */
        let doc = &self.selection_doc();
        /* Track each flag's first observation; flip to `Some(true)` for
        mixed when a later sample disagrees. `None` until the first
        sample is recorded. */
        let mut bold_seen: Option<bool> = None;
        let mut italic_seen: Option<bool> = None;
        let mut underline_seen: Option<engine::UnderlineStyle> = None;
        let mut strike_seen: Option<bool> = None;
        let mut mixed = bridge::AttrsMixed::default();
        let mut record = |s: SpanStyle| {
            let b = s.bold.unwrap_or(false);
            let i = s.italic.unwrap_or(false);
            let u = s.underline.unwrap_or(engine::UnderlineStyle::None);
            let st = s.strike.unwrap_or(false);
            match bold_seen {
                None => bold_seen = Some(b),
                Some(prev) if prev != b => mixed.bold = true,
                _ => {}
            }
            match italic_seen {
                None => italic_seen = Some(i),
                Some(prev) if prev != i => mixed.italic = true,
                _ => {}
            }
            match underline_seen {
                None => underline_seen = Some(u),
                Some(prev) if prev != u => mixed.underline = true,
                _ => {}
            }
            match strike_seen {
                None => strike_seen = Some(st),
                Some(prev) if prev != st => mixed.strike = true,
                _ => {}
            }
        };
        /* Same-paragraph range: sample every span-boundary-bracketed
        sub-range that intersects `[start.offset, end.offset)`. */
        if start.path == end.path {
            let engine_path = bridge_to_engine_path(start.path.clone());
            if let Some(p) = doc.paragraph_at_path(&engine_path) {
                sample_paragraph_styles(p, start.offset, end.offset, &mut record);
            }
            return mixed;
        }
        /* Cross-paragraph range: sample the head paragraph from
        `start.offset` to its end, every full paragraph between, and
        the tail paragraph from 0 to `end.offset`. Defensively cap at
        each paragraph's `text.len()`. */
        let engine_start = bridge_to_engine_path(start.path.clone());
        let engine_end = bridge_to_engine_path(end.path.clone());
        if let Some(p) = doc.paragraph_at_path(&engine_start) {
            sample_paragraph_styles(p, start.offset, p.text.len() as u32, &mut record);
        }
        for path in doc_paragraph_paths_between(doc, &engine_start, &engine_end) {
            if let Some(p) = doc.paragraph_at_path(&path) {
                sample_paragraph_styles(p, 0, p.text.len() as u32, &mut record);
            }
        }
        if let Some(p) = doc.paragraph_at_path(&engine_end) {
            sample_paragraph_styles(p, 0, end.offset, &mut record);
        }
        mixed
    }

    /// Effective alignment of the paragraph at `path` for the toolbar
    /// — the paragraph's own override, else the document's render-
    /// config default.
    fn paragraph_alignment_at(&self, path: &BridgeBlockPath) -> BridgeAlignment {
        let engine_path = bridge_to_engine_path(path.clone());
        let stored = self.with_selection_doc(|d| {
            d.paragraph_at_path(&engine_path)
                .and_then(|p| p.props.alignment)
                .map(layout_align)
        });
        let default = self
            .layout_cfg
            .as_ref()
            .map_or(Alignment::Start, |c| c.alignment);
        bridge_align(stored.unwrap_or(default))
    }

    /// SPATIAL visual-arrow step (UX_BEHAVIOR_SPEC §III.1 run-level
    /// fix). Find the caret's host LineGeom and return the slot
    /// immediately to the visual right (`going_right: true`) or left
    /// of the caret's current x. This is the **only** correct way to
    /// handle ArrowLeft / ArrowRight in a BiDi-mixed line: a
    /// paragraph-direction flip moves logically and breaks INSIDE
    /// directional runs (an LTR paragraph with an inline Arabic word
    /// — ArrowRight inside the Arabic chunk would step logical-forward,
    /// which is VISUALLY LEFT in the RTL reorder).
    ///
    /// At a line's visual edge (no slot on the requested side), fall
    /// back to the paragraph-direction–aware logical step which
    /// handles line and paragraph wrap. The paragraph-direction flip
    /// stays for word-jump (`WordLeft` / `WordRight`) — Word's
    /// observed behaviour in mixed text Ctrl+Arrow.
    fn visual_step_arrow(
        &self,
        caret: &BridgeLogicalPos,
        going_right: bool,
    ) -> (BridgeLogicalPos, CaretAffinity) {
        let affinity = if going_right {
            CaretAffinity::TrailingX
        } else {
            CaretAffinity::LeadingX
        };
        if let Ok(geom) = self.document_geometry() {
            let line = geom.iter().find(|l| {
                l.path == caret.path && caret.offset >= l.start_byte && caret.offset <= l.end_byte
            });
            if let Some(line) = line {
                let current_x = slot_x_for_byte(line, caret.offset);
                if let Some(byte) = neighbor_slot_byte_by_x(&line.slots, current_x, going_right) {
                    return (
                        BridgeLogicalPos {
                            path: caret.path.clone(),
                            offset: byte,
                        },
                        affinity,
                    );
                }
            }
        }
        /* Visual edge of the line OR geometry unavailable (no
        layout_cfg yet — happens in unit tests + before the first
        render). Fall back to the paragraph-direction–aware logical
        step which handles line + paragraph wrap. Visual-right in an
        LTR line is logical-forward; in an RTL line it is
        logical-backward. */
        let para_rtl = self.paragraph_direction_at(&caret.path) == ShapingDirection::Rtl;
        let doc = self.undo.current();
        let stepped = if going_right ^ para_rtl {
            step_right(doc, caret.clone())
        } else {
            step_left(doc, caret.clone())
        };
        (stepped, affinity)
    }

    /// Resolved base direction of the paragraph at `path`, used by the
    /// UAX #9 visual-arrow flip (UX_BEHAVIOR_SPEC §III.1). Falls back in
    /// the same precedence the layout pass uses:
    /// 1. Paragraph's explicit `props.direction` (Word's `<w:bidi>`).
    /// 2. `first_strong_direction` over the paragraph text — the same
    ///    inference layout applies when `props.direction` is `None`.
    /// 3. The engine's `layout_cfg.base_direction` (the document-wide
    ///    default the TS shell seeds at boot).
    /// 4. `Ltr` if no `layout_cfg` is set (defensive — engine should
    ///    always carry a config once `render_document` has run).
    fn paragraph_direction_at(&self, path: &BridgeBlockPath) -> ShapingDirection {
        let engine_path = bridge_to_engine_path(path.clone());
        let resolved = self.with_selection_doc(|d| {
            let p = d.paragraph_at_path(&engine_path)?;
            if let Some(dir) = p.props.direction {
                return Some(match dir {
                    engine::TextDirection::Ltr => ShapingDirection::Ltr,
                    engine::TextDirection::Rtl => ShapingDirection::Rtl,
                });
            }
            first_strong_direction(&p.text)
        });
        resolved.unwrap_or_else(|| {
            self.layout_cfg
                .as_ref()
                .map(|c| c.base_direction)
                .unwrap_or(ShapingDirection::Ltr)
        })
    }

    /// Tri-state paragraph direction across the selection: `Some(dir)`
    /// when every paragraph the range spans agrees on the EFFECTIVE
    /// direction (resolved via the same precedence
    /// `paragraph_direction_at` uses), `None` when paragraphs disagree.
    /// Drives the toolbar's LTR/RTL toggle and the indeterminate
    /// state when a multi-paragraph selection straddles a direction
    /// boundary (UX_BEHAVIOR_SPEC §III).
    fn paragraph_direction_over(
        &self,
        start: &BridgeLogicalPos,
        end: &BridgeLogicalPos,
    ) -> Option<Direction> {
        let to_bridge = |d: ShapingDirection| match d {
            ShapingDirection::Ltr => Direction::Ltr,
            ShapingDirection::Rtl => Direction::Rtl,
        };
        if start.path == end.path {
            return Some(to_bridge(self.paragraph_direction_at(&start.path)));
        }
        /* Multi-paragraph: walk every paragraph in the range and require
        uniform agreement. Cross-container ranges fall back to the
        endpoints (matching `attrs_mixed_over`'s behavior). */
        let head = to_bridge(self.paragraph_direction_at(&start.path));
        let tail = to_bridge(self.paragraph_direction_at(&end.path));
        if head != tail {
            return None;
        }
        let doc = &self.selection_doc();
        let engine_start = bridge_to_engine_path(start.path.clone());
        let engine_end = bridge_to_engine_path(end.path.clone());
        for path in doc_paragraph_paths_between(doc, &engine_start, &engine_end) {
            let bridge_path = engine_to_bridge_path(path);
            let d = to_bridge(self.paragraph_direction_at(&bridge_path));
            if d != head {
                return None;
            }
        }
        Some(head)
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
        let prev = self.with_selection_doc(|d| {
            d.paragraph_at_path(&engine_path)
                .map_or(start.offset, |p| p.prev_offset(start.offset))
        });
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
        /* Issue #29 — fold the paragraph's style-chain run base UNDER
        the direct span style so the toolbar reads the same cascaded
        values the renderer paints (Heading 1 reports bold even with
        zero direct formatting). Phase 3 (#39) — story-aware doc. */
        let mut style = self.with_selection_doc(|doc| {
            doc.paragraph_at_path(&engine_path)
                .map_or_else(SpanStyle::default, |p| {
                    doc.resolve_style_run_cascade(p.style_id.as_deref())
                        .merged_with(p.style_at(pos.offset))
                })
        });
        if apply_pending && let Some(pending) = self.pending_format.as_ref() {
            style = style.merged_with(pending.clone());
        }
        let default_size = self.layout_cfg.as_ref().map_or(16.0, |c| c.px_size);
        let [r, g, b, a] = style.color.unwrap_or([0, 0, 0, 255]);
        let underline =
            engine_to_bridge_underline(style.underline.unwrap_or(engine::UnderlineStyle::None));
        TextAttrs {
            bold: style.bold.unwrap_or(false),
            italic: style.italic.unwrap_or(false),
            underline,
            strike: style.strike.unwrap_or(false),
            /* The span's own family, else the document's default font id. */
            font_family: style
                .font_family
                .as_ref()
                .map(font_family_id)
                .map(str::to_string)
                .or_else(|| self.layout_cfg.as_ref().map(|c| c.font_id.clone()))
                .unwrap_or_default(),
            font_size: style.font_size.unwrap_or(default_size),
            color: Color { r, g, b, a },
            bg_color: style.bg_color.map(|[r, g, b, a]| Color { r, g, b, a }),
            /* Map the engine's `VertAlign` (Baseline / Superscript /
            Subscript) to the bridge's `VerticalScript`
            (Normal / Superscript / Subscript). Hardcoding `Normal`
            here used to defeat the SuperSub button's active-state
            read-back. */
            script: match style.vert_align {
                Some(engine::VertAlign::Superscript) => VerticalScript::Superscript,
                Some(engine::VertAlign::Subscript) => VerticalScript::Subscript,
                _ => VerticalScript::Normal,
            },
            language: String::new(),
            caps: style.caps.unwrap_or(false),
            small_caps: style.small_caps.unwrap_or(false),
        }
    }

    /// Commit a document edit: push undo, collapse the caret at `caret`,
    /// invalidate + repaint, then emit `SelectionChanged`.
    fn commit_edit(&mut self, new_doc: DocumentTree, caret: BridgeLogicalPos) -> Event {
        self.undo.push(new_doc);
        let caret = clamp_pos(self.undo.current(), caret);
        let anchor = caret.clone();
        /* Audit gap B.M4 — a content edit (type, paste, delete) is a
        non-arrow caret motion; reset affinity so the post-edit caret
        rect at a fresh seam picks the leading slot. */
        self.caret_affinity = CaretAffinity::default();
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

    /* ===========================================================
    Phase 3 (#39) — story mutation path. Each handler mirrors its
    body twin MINUS track-changes + sticky pending formatting (both
    gated out of stories in v1): mutate the synthetic story tree
    with the existing pure `DocumentTree` fns, then merge the
    paragraphs back into the real tree's part map as ONE undo push.
    =========================================================== */

    /// Merge a mutated story tree back into the real document: extract
    /// its paragraphs into the active part (marking it dirty for the
    /// writer), push ONE undo snapshot, collapse the caret, repaint.
    fn commit_story_edit(&mut self, mutated: &DocumentTree, caret: BridgeLogicalPos) -> Event {
        let (rid, is_header) = match &self.active_story {
            StoryTarget::Header { rid, .. } => (rid.clone(), true),
            StoryTarget::Footer { rid, .. } => (rid.clone(), false),
            StoryTarget::Body => {
                return Event::Error {
                    message: "commit_story_edit outside a story".into(),
                };
            }
        };
        let mut paras: Vec<engine::Paragraph> = mutated
            .blocks
            .iter()
            .filter_map(|b| b.as_paragraph().cloned())
            .collect();
        if paras.is_empty() {
            /* The band always keeps one paragraph so the caret has a
            home and the part stays a valid `<w:hdr>/<w:ftr>`. */
            paras.push(engine::Paragraph::default());
        }
        let real = self.undo.current();
        let new_real = if is_header {
            real.with_updated_header_part(&rid, paras)
        } else {
            real.with_updated_footer_part(&rid, paras)
        };
        self.undo.push(new_real);
        let caret = self.with_selection_doc(|d| clamp_pos(d, caret));
        self.caret_affinity = CaretAffinity::default();
        self.selection = Some(SelectionState {
            anchor: caret.clone(),
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

    /// Story `InsertText` — plain (untracked, no sticky format) insert.
    fn story_insert_text(&mut self, at: BridgeLogicalPos, text: String) -> Event {
        let sel = self.selection.clone().unwrap_or(SelectionState {
            anchor: at.clone(),
            caret: at,
            ideal_x: None,
            kind: SelectionKind::Linear,
        });
        let (start, end) = ordered(sel.anchor, sel.caret);
        let Some(temp) = self.story_doc() else {
            return self.story_vanished();
        };
        let base = if start == end {
            temp
        } else {
            temp.delete_range(to_engine_pos(start.clone()), to_engine_pos(end))
        };
        let new_doc = base.insert_text(to_engine_pos(start.clone()), &text);
        let caret = BridgeLogicalPos {
            path: start.path,
            offset: start.offset + text.len() as u32,
        };
        self.commit_story_edit(&new_doc, caret)
    }

    /// Story `SplitParagraph` — Enter inside a band.
    fn story_split_paragraph(&mut self, at: BridgeLogicalPos) -> Event {
        let Some(temp) = self.story_doc() else {
            return self.story_vanished();
        };
        let (base, split_at) = match self.selection.clone() {
            Some(s) => {
                let (start, end) = ordered(s.anchor, s.caret);
                let doc = if start == end {
                    temp
                } else {
                    temp.delete_range(to_engine_pos(start.clone()), to_engine_pos(end))
                };
                (doc, start)
            }
            None => (temp, at),
        };
        let new_doc = base.split_paragraph(to_engine_pos(split_at.clone()));
        let next_path = engine::bump_last_block_index(&bridge_to_engine_path(split_at.path));
        let caret = BridgeLogicalPos {
            path: engine_to_bridge_path(next_path),
            offset: 0,
        };
        self.commit_story_edit(&new_doc, caret)
    }

    /// Story `DeleteAtCaret` — selection delete, or one grapheme/word
    /// step; paragraph-edge deletes merge via `delete_range`'s
    /// cross-paragraph semantics.
    fn story_delete_at_caret(&mut self, forward: bool, by_word: bool) -> Event {
        let Some(sel) = self.selection.clone() else {
            return Event::Error {
                message: "DeleteAtCaret: no active selection".into(),
            };
        };
        let Some(temp) = self.story_doc() else {
            return self.story_vanished();
        };
        let (start, end) = ordered(sel.anchor, sel.caret.clone());
        if start != end {
            let new_doc = temp.delete_range(to_engine_pos(start.clone()), to_engine_pos(end));
            return self.commit_story_edit(&new_doc, start);
        }
        let caret = sel.caret;
        let epath = bridge_to_engine_path(caret.path.clone());
        let Some(para) = temp.paragraph_at_path(&epath) else {
            return self.selection_changed();
        };
        let top_idx = caret.path.steps.first().map_or(0, |s| match s {
            BridgePathStep::Block { idx } => *idx,
            BridgePathStep::Cell { .. } => 0,
        });
        let (del_start, del_end): (BridgeLogicalPos, BridgeLogicalPos) = if forward {
            if caret.offset < para.text.len() as u32 {
                let next = if by_word {
                    para.word_bounds(caret.offset).1
                } else {
                    para.next_offset(caret.offset)
                };
                (caret.clone(), bpos_top(top_idx, next))
            } else if (top_idx as usize + 1) < temp.blocks.len() {
                /* Forward-delete at paragraph end: merge with the next. */
                (caret.clone(), bpos_top(top_idx + 1, 0))
            } else {
                return self.selection_changed();
            }
        } else if caret.offset > 0 {
            let prev = if by_word {
                para.word_bounds(caret.offset.saturating_sub(1)).0
            } else {
                para.prev_offset(caret.offset)
            };
            (bpos_top(top_idx, prev), caret.clone())
        } else if top_idx > 0 {
            /* Backspace at paragraph start: merge with the previous. */
            let prev_len = temp
                .paragraph_at_path(&EngineBlockPath::top(top_idx - 1))
                .map_or(0, |p| p.text.len() as u32);
            (bpos_top(top_idx - 1, prev_len), caret.clone())
        } else {
            return self.selection_changed();
        };
        let new_doc = temp.delete_range(to_engine_pos(del_start.clone()), to_engine_pos(del_end));
        self.commit_story_edit(&new_doc, del_start)
    }

    /// Story `ApplyFormatting` — span patch over the selection (or the
    /// explicit range). Collapsed carets are a no-op in v1: sticky
    /// pending formatting stays a body-only feature for now.
    fn story_apply_formatting(
        &mut self,
        range: Option<BridgeLogicalRange>,
        patch: TextAttrsPatch,
    ) -> Event {
        let (start, end) = match range {
            Some(r) => ordered(r.start, r.end),
            None => match self.selection.clone() {
                Some(s) => ordered(s.anchor, s.caret),
                None => return self.selection_changed(),
            },
        };
        if start == end {
            return self.selection_changed();
        }
        let Some(temp) = self.story_doc() else {
            return self.story_vanished();
        };
        let style = patch_to_span_style(&patch);
        let new_doc = temp.apply_style(
            to_engine_pos(start.clone()),
            to_engine_pos(end.clone()),
            style,
        );
        /* Keep the selection (a formatting change should not collapse
        it) — commit manually rather than through commit_story_edit. */
        let (rid, is_header) = match &self.active_story {
            StoryTarget::Header { rid, .. } => (rid.clone(), true),
            StoryTarget::Footer { rid, .. } => (rid.clone(), false),
            StoryTarget::Body => unreachable!("guarded by caller"),
        };
        let mut paras: Vec<engine::Paragraph> = new_doc
            .blocks
            .iter()
            .filter_map(|b| b.as_paragraph().cloned())
            .collect();
        if paras.is_empty() {
            paras.push(engine::Paragraph::default());
        }
        let real = self.undo.current();
        let new_real = if is_header {
            real.with_updated_header_part(&rid, paras)
        } else {
            real.with_updated_footer_part(&rid, paras)
        };
        self.undo.push(new_real);
        self.selection = Some(SelectionState {
            anchor: start,
            caret: end,
            ideal_x: None,
            kind: SelectionKind::Linear,
        });
        self.dirty.invalidate(full_page_rect(self.scale()));
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        self.selection_changed()
    }

    /// Story `PastePlain` — multiline plain-text paste into the band.
    fn story_paste_plain(&mut self, text: String) -> Event {
        let Some(sel) = self.selection.clone() else {
            return Event::Error {
                message: "PastePlain: no active selection".into(),
            };
        };
        let Some(temp) = self.story_doc() else {
            return self.story_vanished();
        };
        let (start, end) = ordered(sel.anchor, sel.caret);
        let base = if start == end {
            temp
        } else {
            temp.delete_range(to_engine_pos(start.clone()), to_engine_pos(end))
        };
        let (new_doc, caret_pos) = base.insert_multiline(to_engine_pos(start), &text);
        let caret = BridgeLogicalPos {
            path: engine_to_bridge_path(caret_pos.path),
            offset: caret_pos.offset,
        };
        self.commit_story_edit(&new_doc, caret)
    }

    /// The active story's part disappeared out from under us (an undo
    /// past materialize-on-enter). Drop back to body editing.
    fn story_vanished(&mut self) -> Event {
        self.exit_story_to_body();
        self.selection_changed()
    }

    /// Shared exit: restore + clamp the stashed body selection.
    fn exit_story_to_body(&mut self) {
        self.active_story = StoryTarget::Body;
        let doc = self.undo.current();
        let restored = self.stashed_body_selection.take().map(|s| SelectionState {
            anchor: clamp_pos(doc, s.anchor),
            caret: clamp_pos(doc, s.caret),
            ideal_x: None,
            kind: s.kind,
        });
        self.selection = restored.or_else(|| {
            Some(SelectionState {
                anchor: bpos_top(0, 0),
                caret: bpos_top(0, 0),
                ideal_x: None,
                kind: SelectionKind::Linear,
            })
        });
        self.caret_affinity = CaretAffinity::default();
    }

    /// `Command::EnterHeaderFooter` (Phase 3, #39) — activate story
    /// editing for the section owning page `page`. Guarantees the
    /// section OWNS a private part before the caret lands:
    ///
    /// - no part referenced → MATERIALIZE an empty one (Word also
    ///   creates the part + dirties the document on header entry);
    /// - referenced part shared with another section → FORK a copy for
    ///   this section only (the v1 stand-in for Word's Link-to-Previous
    ///   toggle; documented deviation, tracked as a follow-up).
    ///
    /// Either mutation is ONE undo entry; entering an already-private
    /// part mutates nothing.
    fn do_enter_header_footer(&mut self, page: u32, area: bridge::HeaderFooterArea) -> Event {
        let is_header = matches!(area, bridge::HeaderFooterArea::Header);
        let target_y = Some(self.lazy_layout.min_target_y * self.scale());
        if let Err(e) = self.ensure_layout_snapshot(self.scale(), false, target_y) {
            return *e;
        }
        /* Resolve the section owning `page`: the first laid-out block
        on (or after) that page. Blank section-lead pages fall through
        to the next page's first block; a fully-empty tail falls back
        to block 0. */
        let section_block: u32 = {
            let snap_cell = self.layout_snapshot.borrow();
            let snap = snap_cell
                .as_ref()
                .expect("ensure_layout_snapshot populated the memo");
            snap.page_paths
                .iter()
                .skip(page as usize)
                .find_map(|paths| paths.first())
                .and_then(|p| p.last_block_index())
                .unwrap_or(0)
        };
        let section = self.section_for_block_cached(section_block);
        let refs = if is_header {
            &section.header_refs
        } else {
            &section.footer_refs
        };
        let existing = refs.default.clone();
        let shared = existing.as_ref().is_some_and(|rid| {
            self.with_cached_sections(|sections| {
                sections
                    .iter()
                    .filter(|s| {
                        let r = if is_header {
                            &s.header_refs
                        } else {
                            &s.footer_refs
                        };
                        [&r.default, &r.first, &r.even]
                            .into_iter()
                            .any(|slot| slot.as_deref() == Some(rid.as_str()))
                    })
                    .count()
                    > 1
            })
        });
        let doc = self.undo.current();
        let sect_pos = EnginePos {
            path: EngineBlockPath::top(section.start_block),
            offset: 0,
        };
        let rid = match (existing, shared) {
            (Some(rid), false) => rid,
            (maybe_shared, _) => {
                /* Materialize (None) or fork (Some + shared). */
                let new_rid = doc.fresh_hf_rid();
                let seed = maybe_shared
                    .as_ref()
                    .and_then(|old| {
                        if is_header {
                            doc.headers.get(old).cloned()
                        } else {
                            doc.footers.get(old).cloned()
                        }
                    })
                    .unwrap_or_else(|| vec![engine::Paragraph::default()]);
                let with_part = if is_header {
                    doc.with_updated_header_part(&new_rid, seed)
                } else {
                    doc.with_updated_footer_part(&new_rid, seed)
                };
                let with_ref =
                    with_part.set_section_hf_default_ref_at(sect_pos, is_header, &new_rid);
                self.undo.push(with_ref);
                self.dirty.invalidate(full_page_rect(self.scale()));
                new_rid
            }
        };
        self.stashed_body_selection = self.selection.clone();
        self.active_story = if is_header {
            StoryTarget::Header {
                rid,
                page,
                section_block,
            }
        } else {
            StoryTarget::Footer {
                rid,
                page,
                section_block,
            }
        };
        self.selection = Some(SelectionState {
            anchor: bpos_top(0, 0),
            caret: bpos_top(0, 0),
            ideal_x: None,
            kind: SelectionKind::Linear,
        });
        self.caret_affinity = CaretAffinity::default();
        self.pending_format = None;
        self.announce(
            AnnouncementPriority::Polite,
            if is_header {
                "Editing header"
            } else {
                "Editing footer"
            },
        );
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        self.selection_changed()
    }

    /// `Command::ExitHeaderFooter` (Phase 3, #39).
    fn do_exit_header_footer(&mut self) -> Event {
        if !self.story_active() {
            return self.selection_changed();
        }
        self.exit_story_to_body();
        self.announce(AnnouncementPriority::Polite, "Returned to document body");
        self.selection_changed()
    }

    /// Interactive `InsertText` — replace any non-empty selection with `text`,
    /// then place the caret after it.
    fn do_insert_text_interactive(&mut self, at: BridgeLogicalPos, text: String) -> Event {
        if self.story_active() {
            return self.story_insert_text(at, text);
        }
        let sel = self.selection.clone().unwrap_or(SelectionState {
            anchor: at.clone(),
            caret: at,
            ideal_x: None,
            kind: SelectionKind::Linear,
        });
        let (start, end) = ordered(sel.anchor, sel.caret);
        let tracking = self.tracking_changes;
        let author = self.review_author.clone();
        let date = self.current_review_date();
        let base = if start == end {
            self.undo.current().clone()
        } else if tracking {
            /* Sprint 14 (#14) — replacing a selection while tracking
            = mark-old-as-delete + insert-new-as-insert. */
            self.undo.current().tracked_delete_range(
                to_engine_pos(start.clone()),
                to_engine_pos(end),
                author.clone(),
                date.clone(),
            )
        } else {
            self.undo
                .current()
                .delete_range(to_engine_pos(start.clone()), to_engine_pos(end))
        };
        let mut new_doc = if tracking {
            base.tracked_insert_text(
                to_engine_pos(start.clone()),
                &text,
                author.clone(),
                date.clone(),
            )
        } else {
            base.insert_text(to_engine_pos(start.clone()), &text)
        };
        let inserted_end = start.offset + text.len() as u32;
        /* Sticky formatting (Backlog #11): overlay any armed pending style
        onto the just-inserted run. It is intentionally NOT cleared here — it
        stays armed across consecutive keystrokes so a whole typed run shares
        the style, and is dropped only when the caret moves. */
        if let Some(pending) = self.pending_format.clone() {
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
    /// `Command::ReplaceRange` — atomic delete + insert as ONE undo step
    /// (issue #30). The range is explicit and authoritative (an
    /// API-driven find-and-replace primitive), never resolved from the
    /// live selection. Under track-changes the old text is marked
    /// deleted and the new text marked inserted, mirroring the
    /// interactive replace path. The caret lands after the inserted
    /// text; sticky pending formatting is deliberately not applied
    /// (this is not a typing path).
    fn do_replace_range(&mut self, range: BridgeLogicalRange, text: String) -> Event {
        let (start, end) = ordered(range.start, range.end);
        let tracking = self.tracking_changes;
        let author = self.review_author.clone();
        let date = self.current_review_date();
        let base = if start == end {
            self.undo.current().clone()
        } else if tracking {
            self.undo.current().tracked_delete_range(
                to_engine_pos(start.clone()),
                to_engine_pos(end),
                author.clone(),
                date.clone(),
            )
        } else {
            self.undo
                .current()
                .delete_range(to_engine_pos(start.clone()), to_engine_pos(end))
        };
        let new_doc = if tracking {
            base.tracked_insert_text(to_engine_pos(start.clone()), &text, author, date)
        } else {
            base.insert_text(to_engine_pos(start.clone()), &text)
        };
        let caret = BridgeLogicalPos {
            path: start.path,
            offset: start.offset + text.len() as u32,
        };
        self.commit_edit(new_doc, caret)
    }

    /// Sprint 14 (#14) — when track-changes is on, mark the range as
    /// a `Delete` revision instead of removing text.
    fn do_delete_range(&mut self, range: BridgeLogicalRange) -> Event {
        let (start, end) = ordered(range.start, range.end);
        let (new_doc, caret) = if self.tracking_changes {
            let d = self.undo.current().tracked_delete_range(
                to_engine_pos(start.clone()),
                to_engine_pos(end.clone()),
                self.review_author.clone(),
                self.current_review_date(),
            );
            /* Marker-only tracked delete keeps text; caret lands at
            the end of the marked range so further typing extends past. */
            (d, end)
        } else {
            let d = self
                .undo
                .current()
                .delete_range(to_engine_pos(start.clone()), to_engine_pos(end));
            (d, start)
        };
        self.commit_edit(new_doc, caret)
    }

    /// `Command::SplitParagraph` — break the paragraph at the caret (replacing
    /// any non-empty selection first); the caret moves to the new paragraph.
    fn do_split_paragraph(&mut self, at: BridgeLogicalPos) -> Event {
        if self.story_active() {
            return self.story_split_paragraph(at);
        }
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
        if self.story_active() {
            return self.story_delete_at_caret(forward, by_word);
        }
        let Some(sel) = self.selection.clone() else {
            return Event::Error {
                message: "DeleteAtCaret: no active selection".into(),
            };
        };
        let (start, end) = ordered(sel.anchor, sel.caret.clone());
        if start != end {
            let (new_doc, caret) = self.delete_or_mark(start.clone(), end);
            return self.commit_edit(new_doc, caret);
        }
        let Some((del_start, del_end)) = self.delete_target(sel.caret, forward, by_word) else {
            /* Caret at a document edge — nothing to delete. */
            return self.selection_changed();
        };
        let (new_doc, caret) = self.delete_or_mark(del_start, del_end);
        self.commit_edit(new_doc, caret)
    }

    /// Sprint 14 (#14) — shared dispatch for "delete a logical range":
    /// route through `tracked_delete_range` when tracking is on
    /// (marker-only, caret lands at end), else the plain `delete_range`
    /// (text removed, caret lands at start).
    fn delete_or_mark(
        &self,
        start: BridgeLogicalPos,
        end: BridgeLogicalPos,
    ) -> (engine::DocumentTree, BridgeLogicalPos) {
        if self.tracking_changes {
            let d = self.undo.current().tracked_delete_range(
                to_engine_pos(start.clone()),
                to_engine_pos(end.clone()),
                self.review_author.clone(),
                self.current_review_date(),
            );
            (d, end)
        } else {
            let d = self
                .undo
                .current()
                .delete_range(to_engine_pos(start.clone()), to_engine_pos(end));
            (d, start)
        }
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
                    /* UAX #29 word boundary forward — delete from caret
                    to the start of the next word-like segment. Matches
                    Ctrl+Delete in Word / Google Docs. Falls back to
                    paragraph end when no further word exists. */
                    word_starts(&para.text)
                        .into_iter()
                        .find(|&i| i as u32 > caret.offset)
                        .map(|i| i as u32)
                        .unwrap_or(para_len)
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
                /* UAX #29 word boundary backward — delete from the
                start of the previous word-like segment up to the
                caret. Matches Ctrl+Backspace in Word / Google Docs. */
                word_starts(&para.text)
                    .into_iter()
                    .take_while(|&i| (i as u32) < caret.offset)
                    .last()
                    .map(|i| i as u32)
                    .unwrap_or(0)
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
        /* The preview text lives outside the document, so the layout
        memo's revision key cannot see it change. */
        self.invalidate_layout_snapshot();
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
        /* The preview text changed but the doc revision did not — drop
        the layout memo or the repaint would reuse the previous
        composition's boxes. */
        self.invalidate_layout_snapshot();
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
        self.invalidate_layout_snapshot();
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
        /* The selection's blocks (paragraphs + any tables it spans),
        clipped at the endpoints — fed to the rich HTML serializer so
        the clipboard carries cell shading + borders + image extents
        for high-fidelity round-trip (UX_BEHAVIOR_SPEC §V). The
        paragraph-only `slice` still feeds the legacy `.docx`
        fragment path until `from_rich_paragraphs` learns to accept
        blocks. */
        let block_slice = doc.slice_blocks(estart.clone(), eend.clone());
        let html = engine::html::to_html_blocks(&block_slice);
        let paragraph_slice = doc.slice(estart, eend);
        let docx_fragment =
            build_minimal_docx(&DocumentTree::from_rich_paragraphs(paragraph_slice))
                .unwrap_or_default();
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
        if self.story_active() {
            return self.story_paste_plain(normalized);
        }
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
        let blocks_in = engine::html::from_html_blocks(&html);
        if blocks_in.is_empty() {
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
        let (new_doc, caret) = base.insert_rich_blocks(to_engine_pos(start), &blocks_in);
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
        let label = match align {
            BridgeAlignment::Start => "Aligned start",
            BridgeAlignment::End => "Aligned end",
            BridgeAlignment::Center => "Aligned center",
            BridgeAlignment::Justify => "Justified",
        };
        self.announce(AnnouncementPriority::Polite, label);
        self.selection_changed()
    }

    /// `Command::SetParagraphDirection` — set `props.direction` on every
    /// paragraph the range spans. Word's `<w:bidi>` semantics: direction
    /// is logical (text flow + punctuation placement), separate from
    /// alignment (visual anchoring). Existing `Start` / `End`
    /// alignments are direction-relative, so toggling direction
    /// automatically swaps which visual edge they resolve to — no
    /// alignment rewrite needed for the auto-flip behavior Word
    /// users expect.
    fn do_set_paragraph_direction(
        &mut self,
        range: BridgeLogicalRange,
        direction: Direction,
    ) -> Event {
        let engine_dir = match direction {
            Direction::Ltr => engine::TextDirection::Ltr,
            Direction::Rtl => engine::TextDirection::Rtl,
        };
        let (start, end) = ordered(range.start, range.end);
        let new_doc = self.undo.current().set_direction(
            to_engine_pos(start.clone()),
            to_engine_pos(end),
            engine_dir,
        );
        self.undo.push(new_doc);
        self.dirty.invalidate(full_page_rect(self.scale()));
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        let label = match direction {
            Direction::Ltr => "Direction set to left to right",
            Direction::Rtl => "Direction set to right to left",
        };
        self.announce(AnnouncementPriority::Polite, label);
        self.selection_changed()
    }

    /// `Command::AcceptRevision` (Sprint 7 UI Edition).
    /// Sprint 14 (#14) — best-effort current review date stamp.
    /// Prefers the explicit value `Command::SetReviewIdentity` set;
    /// otherwise falls back to the worker thread's `Date.now()` via
    /// `js_sys::Date::new_0().to_iso_string()`.
    fn current_review_date(&self) -> String {
        if !self.review_date.is_empty() {
            return self.review_date.clone();
        }
        js_sys::Date::new_0()
            .to_iso_string()
            .as_string()
            .unwrap_or_default()
    }

    /// Sprint 14 (#14) — `Command::ToggleTrackChanges`. Flips the
    /// engine's recording flag; subsequent mutations route through
    /// the tracked-revision-wrap path. UI binds its toggle's active
    /// state to `Event::SelectionChanged.is_tracking_changes` so a
    /// `ToggleTrackChanges` issued from any path (UI, macro, undo)
    /// stays in sync.
    fn do_toggle_track_changes(&mut self, enabled: bool) -> Event {
        self.tracking_changes = enabled;
        self.announce(
            AnnouncementPriority::Polite,
            if enabled {
                "Track changes on"
            } else {
                "Track changes off"
            },
        );
        self.selection_changed()
    }

    /// Sprint 14 (#14) — `Command::SetReviewIdentity`. Stamped onto
    /// every tracked revision the engine synthesises. Empty `date`
    /// leaves the engine's stored date untouched so the worker can
    /// stamp `Date.now()` per-mutation; empty `author` keeps the
    /// existing author.
    fn do_set_review_identity(&mut self, author: String, date: String) -> Event {
        if !author.is_empty() {
            self.review_author = author;
        }
        if !date.is_empty() {
            self.review_date = date;
        }
        self.selection_changed()
    }

    fn do_accept_revision(&mut self, block: u32, start: u32, end: u32) -> Event {
        let new_doc = self.undo.current().accept_revision_at(block, start, end);
        self.undo.push(new_doc);
        self.layout_cache.get_mut().clear();
        self.dirty.invalidate(full_page_rect(self.scale()));
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        self.announce(AnnouncementPriority::Polite, "Revision accepted");
        self.selection_changed()
    }

    /// `Command::RejectRevision` (Sprint 7 UI Edition).
    fn do_reject_revision(&mut self, block: u32, start: u32, end: u32) -> Event {
        let new_doc = self.undo.current().reject_revision_at(block, start, end);
        self.undo.push(new_doc);
        self.layout_cache.get_mut().clear();
        self.dirty.invalidate(full_page_rect(self.scale()));
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        self.announce(AnnouncementPriority::Polite, "Revision rejected");
        self.selection_changed()
    }

    /// `Command::InsertComment` (Sprint 7 UI Edition). Uses an
    /// ISO-8601 timestamp derived from `Date.now()` on the worker
    /// thread (passed in via `js_sys::Date::new_0().to_iso_string()`)
    /// when wired through; for the engine handler the date is the
    /// current `wasm_bindgen` UTC epoch ms formatted as RFC 3339.
    fn do_insert_comment(
        &mut self,
        range: BridgeLogicalRange,
        text: String,
        author: String,
    ) -> Event {
        let now = js_sys::Date::new_0().to_iso_string();
        let date = now.as_string().unwrap_or_default();
        let (start, end) = ordered(range.start, range.end);
        let (new_doc, _new_id) = self.undo.current().insert_comment(
            to_engine_pos(start),
            to_engine_pos(end),
            text,
            author,
            date,
        );
        self.undo.push(new_doc);
        self.layout_cache.get_mut().clear();
        self.dirty.invalidate(full_page_rect(self.scale()));
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        self.announce(AnnouncementPriority::Polite, "Comment added");
        self.selection_changed()
    }

    /// `Command::DeleteComment` (Sprint 7 UI Edition).
    fn do_delete_comment(&mut self, id: u32) -> Event {
        let new_doc = self.undo.current().delete_comment(id);
        self.undo.push(new_doc);
        self.layout_cache.get_mut().clear();
        self.dirty.invalidate(full_page_rect(self.scale()));
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        self.announce(AnnouncementPriority::Polite, "Comment deleted");
        self.selection_changed()
    }

    /// `Command::ResolveComment` (Sprint 9 — resolved round-trips
    /// through `word/commentsExtended.xml`).
    fn do_resolve_comment(&mut self, id: u32, resolved: bool) -> Event {
        let new_doc = self.undo.current().set_comment_resolved(id, resolved);
        self.undo.push(new_doc);
        self.announce(
            AnnouncementPriority::Polite,
            if resolved {
                "Comment resolved"
            } else {
                "Comment reopened"
            },
        );
        self.selection_changed()
    }

    /// `Command::ReplyToComment` (issue #27) — threaded reply anchored
    /// to the parent comment's range. Mirrors `do_insert_comment`
    /// (worker-thread ISO-8601 date stamp, undo push, full-cache
    /// invalidation, polite announcement). An unknown parent maps the
    /// engine's `None` to `Event::Error` instead of mutating the doc.
    fn do_reply_to_comment(&mut self, parent_id: u32, text: String, author: String) -> Event {
        let now = js_sys::Date::new_0().to_iso_string();
        let date = now.as_string().unwrap_or_default();
        let Some((new_doc, _new_id)) = self
            .undo
            .current()
            .reply_to_comment(parent_id, text, author, date)
        else {
            return Event::Error {
                message: format!("ReplyToComment: unknown parent comment id {parent_id}"),
            };
        };
        self.undo.push(new_doc);
        self.layout_cache.get_mut().clear();
        self.dirty.invalidate(full_page_rect(self.scale()));
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        self.announce(AnnouncementPriority::Polite, "Reply added");
        self.selection_changed()
    }

    /// `Command::SetParagraphIndent` (Sprint 6 UI Edition).
    fn do_set_paragraph_indent(
        &mut self,
        range: BridgeLogicalRange,
        start_pt: f32,
        end_pt: f32,
        first_line_pt: f32,
    ) -> Event {
        let (start, end) = ordered(range.start, range.end);
        let new_doc = self.undo.current().set_paragraph_indent(
            to_engine_pos(start),
            to_engine_pos(end),
            start_pt,
            end_pt,
            first_line_pt,
        );
        self.undo.push(new_doc);
        self.layout_cache.get_mut().clear();
        self.dirty.invalidate(full_page_rect(self.scale()));
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        self.selection_changed()
    }

    /// Sprint 11 (#13) — `Command::SetTabStops`. The Ruler dispatches
    /// once on drag-release; the entire stops vector replaces the
    /// paragraph's `<w:pPr><w:tabs>` so one drag = one undo entry.
    fn do_set_tab_stops(
        &mut self,
        range: BridgeLogicalRange,
        stops: Vec<bridge::BridgeTabStop>,
    ) -> Event {
        let (start, end) = ordered(range.start, range.end);
        let engine_stops: Vec<engine::TabStop> = stops
            .into_iter()
            .map(|s| engine::TabStop {
                position_pt: s.position_pt,
                kind: match s.kind {
                    bridge::BridgeTabKind::Left => engine::TabKind::Left,
                    bridge::BridgeTabKind::Center => engine::TabKind::Center,
                    bridge::BridgeTabKind::Right => engine::TabKind::Right,
                    bridge::BridgeTabKind::Decimal => engine::TabKind::Decimal,
                    bridge::BridgeTabKind::Clear => engine::TabKind::Clear,
                },
            })
            .collect();
        let new_doc = self.undo.current().set_tab_stops(
            to_engine_pos(start),
            to_engine_pos(end),
            engine_stops,
        );
        self.undo.push(new_doc);
        self.layout_cache.get_mut().clear();
        self.dirty.invalidate(full_page_rect(self.scale()));
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        self.announce(AnnouncementPriority::Polite, "Tab stops updated");
        self.selection_changed()
    }

    /// Sprint 12 (#11) — `Command::ApplyStyle`. Routes through the
    /// engine's shadow-direct-overrides resolver: sets `style_id` on
    /// every paragraph the range spans, then recomputes the resolved
    /// `props` view from `style_cascade(style_id) ∪ direct_overrides`.
    /// Pre-existing direct edits survive.
    fn do_apply_style(&mut self, range: BridgeLogicalRange, style_id: Option<String>) -> Event {
        let (start, end) = ordered(range.start, range.end);
        let new_doc = self.undo.current().set_paragraph_style(
            to_engine_pos(start),
            to_engine_pos(end),
            style_id.clone(),
        );
        self.undo.push(new_doc);
        self.layout_cache.get_mut().clear();
        self.dirty.invalidate(full_page_rect(self.scale()));
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        let label = match style_id.as_deref() {
            Some(id) => format!("Applied style {id}"),
            None => "Cleared paragraph style".to_string(),
        };
        self.announce(AnnouncementPriority::Polite, label);
        self.selection_changed()
    }

    /// Issue #21 — mutate a style definition and re-cascade. Mirrors
    /// `do_apply_style`'s invalidation discipline.
    fn do_modify_style(&mut self, style_id: String, props: BridgeStyleProperties) -> Event {
        if !self.undo.current().styles.contains_key(&style_id) {
            return Event::Error {
                message: format!("ModifyStyle: unknown style id {style_id}"),
            };
        }
        let para_patch = props.para_props.map(|p| {
            let mut out = engine::ParaProperties {
                alignment: p.alignment.map(engine_align),
                direction: p.direction.map(|d| match d {
                    Direction::Ltr => engine::TextDirection::Ltr,
                    Direction::Rtl => engine::TextDirection::Rtl,
                }),
                shading: p.shading.map(|c| [c.r, c.g, c.b, c.a]),
                ..Default::default()
            };
            if let Some(m) = p.line_spacing_multiplier
                && m > 0.0
            {
                out.line_height = Some(engine::LineHeight::Auto {
                    twips: (m * 240.0).round() as i32,
                });
            }
            if let Some(v) = p.indent_start_pt {
                out.indent.start_twips = (v * 20.0).round() as i32;
            }
            if let Some(v) = p.indent_end_pt {
                out.indent.end_twips = (v * 20.0).round() as i32;
            }
            if let Some(v) = p.first_line_pt {
                if v >= 0.0 {
                    out.indent.first_line_twips = (v * 20.0).round() as i32;
                } else {
                    out.indent.hanging_twips = (-v * 20.0).round() as i32;
                }
            }
            out
        });
        let run_patch = props.run_props.map(|r| engine::SpanStyle {
            bold: r.bold,
            italic: r.italic,
            underline: r.underline.map(bridge_to_engine_underline),
            strike: r.strike,
            font_size: r.font_size,
            color: r.color.map(|c| [c.r, c.g, c.b, c.a]),
            bg_color: r.bg_color.map(|c| [c.r, c.g, c.b, c.a]),
            font_family: r
                .font_family
                .as_deref()
                .and_then(engine::FontFamily::from_id),
            caps: r.caps,
            small_caps: r.small_caps,
            vert_align: None,
            raw_font_family: r.font_family,
            font_theme: None,
        });
        let based_on = if props.clear_based_on == Some(true) {
            Some(None)
        } else {
            props.based_on.map(Some)
        };
        let new_doc = self.undo.current().modify_style(
            &style_id,
            para_patch,
            run_patch,
            based_on,
            props.display_name,
        );
        self.undo.push(new_doc);
        self.layout_cache.get_mut().clear();
        self.dirty.invalidate(full_page_rect(self.scale()));
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        self.announce(
            AnnouncementPriority::Polite,
            format!("Style {style_id} updated"),
        );
        self.selection_changed()
    }

    /// `Command::SetLineSpacing` (Sprint 6 UI Edition).
    fn do_set_line_spacing(&mut self, range: BridgeLogicalRange, multiplier: f32) -> Event {
        let (start, end) = ordered(range.start, range.end);
        let new_doc = self.undo.current().set_line_spacing(
            to_engine_pos(start),
            to_engine_pos(end),
            multiplier,
        );
        self.undo.push(new_doc);
        self.layout_cache.get_mut().clear();
        self.dirty.invalidate(full_page_rect(self.scale()));
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        self.selection_changed()
    }

    /// `Command::SetParagraphShading` (Sprint 6 UI Edition).
    fn do_set_paragraph_shading(
        &mut self,
        range: BridgeLogicalRange,
        color: Option<Color>,
    ) -> Event {
        let target = color.map(|c| [c.r, c.g, c.b, c.a]);
        let (start, end) = ordered(range.start, range.end);
        let new_doc = self.undo.current().set_paragraph_shading(
            to_engine_pos(start),
            to_engine_pos(end),
            target,
        );
        self.undo.push(new_doc);
        self.layout_cache.get_mut().clear();
        self.dirty.invalidate(full_page_rect(self.scale()));
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        self.selection_changed()
    }

    /// `Command::ToggleList` — Sprint 13 (#12).
    /// - `Off` clears the paragraph's `list_item`.
    /// - `Bullet` / `Number` run the idempotent numbering synthesis
    ///   (`DocumentTree::toggle_list_on_range`) so repeated toggles
    ///   reuse existing AbstractNum / Num templates and never bloat
    ///   `numbering.xml`.
    fn do_toggle_list(&mut self, range: BridgeLogicalRange, kind: bridge::ListKind) -> Event {
        let (start, end) = ordered(range.start, range.end);
        let (new_doc, label) = match kind {
            bridge::ListKind::Off => {
                let d = self
                    .undo
                    .current()
                    .clear_list_item_on_range(to_engine_pos(start), to_engine_pos(end));
                (d, "List cleared")
            }
            bridge::ListKind::Bullet => {
                let d = self.undo.current().toggle_list_on_range(
                    to_engine_pos(start),
                    to_engine_pos(end),
                    engine::numbering::ListSynthesisKind::Bullet,
                );
                (d, "Bullet list")
            }
            bridge::ListKind::Number => {
                let d = self.undo.current().toggle_list_on_range(
                    to_engine_pos(start),
                    to_engine_pos(end),
                    engine::numbering::ListSynthesisKind::Number,
                );
                (d, "Numbered list")
            }
        };
        self.undo.push(new_doc);
        self.layout_cache.get_mut().clear();
        self.dirty.invalidate(full_page_rect(self.scale()));
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        self.announce(AnnouncementPriority::Polite, label);
        self.selection_changed()
    }

    /// `Command::ChangeListLevel` — Issue #42. Demotes (`delta > 0`) or
    /// promotes (`delta < 0`) the outline level of every list paragraph
    /// the range spans; a no-op on non-list paragraphs.
    fn do_change_list_level(&mut self, range: BridgeLogicalRange, delta: i8) -> Event {
        let (start, end) = ordered(range.start, range.end);
        let new_doc = self.undo.current().change_list_level_on_range(
            to_engine_pos(start),
            to_engine_pos(end),
            delta,
        );
        self.undo.push(new_doc);
        self.layout_cache.get_mut().clear();
        self.dirty.invalidate(full_page_rect(self.scale()));
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        self.announce(
            AnnouncementPriority::Polite,
            if delta > 0 {
                "List level demoted"
            } else {
                "List level promoted"
            },
        );
        self.selection_changed()
    }

    /// `Command::SetPageMargins` (Sprint 4 UI Edition) — set the
    /// page margins (pt) on the section containing `at`.
    fn do_set_page_margins(
        &mut self,
        at: BridgeLogicalPos,
        top_pt: f32,
        right_pt: f32,
        bottom_pt: f32,
        left_pt: f32,
    ) -> Event {
        let new_doc = self.undo.current().set_section_margins_at(
            to_engine_pos(at),
            top_pt,
            right_pt,
            bottom_pt,
            left_pt,
        );
        self.undo.push(new_doc);
        self.layout_cache.get_mut().clear();
        self.dirty.invalidate(full_page_rect(self.scale()));
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        self.selection_changed()
    }

    /// `Command::SetPageOrientation` (Sprint 4 UI Edition) — flip
    /// the orientation of the section containing `at`. No-op when
    /// the page already matches the requested orientation.
    fn do_set_page_orientation(
        &mut self,
        at: BridgeLogicalPos,
        orientation: bridge::PageOrientation,
    ) -> Event {
        let landscape = matches!(orientation, bridge::PageOrientation::Landscape);
        let new_doc = self
            .undo
            .current()
            .set_section_orientation_at(to_engine_pos(at), landscape);
        self.undo.push(new_doc);
        self.layout_cache.get_mut().clear();
        self.dirty.invalidate(full_page_rect(self.scale()));
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        self.selection_changed()
    }

    /// Sprint 3 (UI Edition) — shared body for the legacy
    /// `LoadDocx` and the new `OpenDocument { format: Docx }`
    /// commands. Replaces the active document, resets the caret,
    /// invalidates layout and repaints.
    fn load_docx_bytes(&mut self, bytes: &[u8], origin: &'static str) -> Event {
        match format_docx::read_docx(bytes) {
            Ok(archive) => {
                let paragraph_count = archive.document.paragraph_count();
                self.undo = UndoStack::new(archive.document, 100);
                self.selection = Some(SelectionState {
                    anchor: bpos_top(0, 0),
                    caret: bpos_top(0, 0),
                    ideal_x: None,
                    kind: SelectionKind::Linear,
                });
                /* Issue #51 — a document swap invalidates every piece of
                per-document state, not just the layout cache. Leaving
                `lazy_layout` alone carried the previous doc's scroll
                high-water mark into the new doc, so the first paint laid
                out from page 1 down to wherever the OLD document was
                scrolled. Keep `viewport_h` (a property of the canvas,
                not the document) so `lazy_runway` stays calibrated, but
                restart the band at the top. Stale `composition` /
                `pending_format` could splice preview text or styling
                into the new doc; stale `image_cache` bitmaps collide on
                reused rel ids (`rId4` exists in most .docx files);
                a stale `a11y_cache` would diff against the old tree.
                (do_recover resets the same set for the same reason.) */
                self.lazy_layout = LazyLayoutState {
                    viewport_y: 0.0,
                    viewport_h: self.lazy_layout.viewport_h,
                    min_target_y: INITIAL_COLD_OPEN_BUDGET_PT.max(self.lazy_layout.viewport_h),
                };
                self.composition = None;
                self.pending_format = None;
                self.caret_affinity = CaretAffinity::default();
                self.a11y_cache = None;
                self.image_cache.clear();
                self.last_paint_dims = LastPaintDims::default();
                self.layout_cache.get_mut().clear();
                /* A fresh UndoStack restarts revision at 0, which the
                memo key cannot distinguish from the old stack's 0. */
                self.invalidate_layout_snapshot();
                self.dirty.invalidate(full_page_rect(self.scale()));
                /* Issue #51/#54 — this was the only repaint in the file
                that DISCARDED its Result. A failed post-load paint
                (missing font, backend error) previously returned
                `DocumentLoaded` anyway, leaving every canvas showing the
                previous document while the status bar reported the new
                one. Surface the error; the document itself is loaded,
                and the shell decides how to present the failure. */
                if let Err(e) = self.maybe_repaint_result() {
                    return *e;
                }
                Event::DocumentLoaded { paragraph_count }
            }
            Err(e) => Event::Error {
                message: format!("{origin}: {e}"),
            },
        }
    }

    /// Sprint 3 (UI Edition) — shared body for the legacy `SaveDocx`
    /// and the new `SaveDocument { format: Docx }` commands.
    fn save_docx_bytes(&self, origin: &'static str) -> Event {
        match build_minimal_docx(self.undo.current()) {
            Ok(bytes) => {
                let size = bytes.len() as u32;
                Event::DocumentSaved { bytes, size }
            }
            Err(e) => Event::Error {
                message: format!("{origin}: {e}"),
            },
        }
    }

    /// Sprint 9 — serialize the current document to a standalone HTML5
    /// blob via `format_html::to_html`. `String::into_bytes()` consumes
    /// the buffer in place — no extra copy crossing the wasm bridge;
    /// the resulting `Vec<u8>` flows back to TS as a single `Uint8Array`.
    fn save_html_bytes(&self) -> Event {
        let html = format_html::to_html(self.undo.current());
        let bytes = html.into_bytes();
        let size = bytes.len() as u32;
        Event::DocumentSaved { bytes, size }
    }

    /// Sprint 9 — flatten the current document to plain text via
    /// `DocumentTree::to_plain_text`.
    fn save_plain_text_bytes(&self) -> Event {
        let text = self.undo.current().to_plain_text();
        let bytes = text.into_bytes();
        let size = bytes.len() as u32;
        Event::DocumentSaved { bytes, size }
    }

    /// Sprint 3 (UI Edition) — set the user zoom fraction and force
    /// a full repaint. Clamped to `[0.25, 4.0]` and COMPOSED with the
    /// boot `base_scale` (`scale = base_scale × zoom`) so zooming
    /// never clobbers DPI scaling and zoom `1.0` restores the exact
    /// boot rendering. `RenderPage` must have cached a `layout_cfg`
    /// first (a fresh engine before any render has no scale to
    /// mutate — return a no-op `selection_changed` so the caller
    /// still sees a reply).
    fn do_set_zoom(&mut self, zoom: f32) -> Event {
        let zoom = zoom.clamp(0.25, 4.0);
        if let Some(cfg) = self.layout_cfg.as_mut() {
            cfg.zoom = zoom;
            cfg.scale = cfg.base_scale * zoom;
        } else {
            return self.selection_changed();
        }
        self.layout_cache.get_mut().clear();
        self.dirty.invalidate(full_page_rect(self.scale()));
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        self.selection_changed()
    }

    /// `SetDeviceScale` — post-boot devicePixelRatio change (monitor move
    /// / browser zoom). Replaces the boot `base_scale` and recomposes
    /// `scale = base_scale × zoom`, leaving the user zoom untouched —
    /// the mirror image of `do_set_zoom`. The wider clamp admits real
    /// device ratios (dpr up to ~6 × the 4/3 CSS-pt factor).
    fn do_set_device_scale(&mut self, scale: f32) -> Event {
        let base = scale.clamp(0.5, 8.0);
        if let Some(cfg) = self.layout_cfg.as_mut() {
            cfg.base_scale = base;
            cfg.scale = base * cfg.zoom;
        } else {
            return self.selection_changed();
        }
        self.layout_cache.get_mut().clear();
        self.dirty.invalidate(full_page_rect(self.scale()));
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        self.selection_changed()
    }

    /// Sprint 3 (UI Edition) — insert an inline image at `at` (or at
    /// the active caret when `at` lands on a no-longer-resolvable
    /// path). Width/height are computed from the bridge `ImageBlob`
    /// pixel dimensions + the requested `ImageFit` mode, then passed
    /// to the engine in EMU (914_400 / inch).
    ///
    /// **Renderer note.** The blob lands in `DocumentTree.media`; the
    /// renderer paints a placeholder rect until the worker decodes
    /// the bytes via `createImageBitmap` and calls
    /// `Engine::register_image(rel_id, bitmap)`. That worker-side
    /// post-insert registration is filed alongside this handler as
    /// a follow-up sprint deliverable.
    fn do_insert_image(
        &mut self,
        at: BridgeLogicalPos,
        image: BridgeImageBlob,
        fit: ImageFit,
    ) -> Event {
        const EMU_PER_PX: i64 = 9525; // 914_400 EMU/inch ÷ 96 px/inch
        const FIT_WIDTH_EMU: i64 = 5_486_400; // 6 inches — Word stock
        const FIT_PAGE_EMU: i64 = 4_572_000; // 5 inches — slightly inset

        let natural_w_emu = i64::from(image.width).saturating_mul(EMU_PER_PX);
        let natural_h_emu = i64::from(image.height).saturating_mul(EMU_PER_PX);
        let (w_emu, h_emu) = match fit {
            ImageFit::Original => (natural_w_emu, natural_h_emu),
            ImageFit::FitWidth | ImageFit::FitPage => {
                let target = if matches!(fit, ImageFit::FitWidth) {
                    FIT_WIDTH_EMU
                } else {
                    FIT_PAGE_EMU
                };
                /* Preserve aspect ratio. Guard against zero-sized
                 * blobs the decoder may report. */
                if natural_w_emu <= 0 {
                    (target, target)
                } else {
                    let h = (natural_h_emu as i128 * target as i128 / natural_w_emu as i128) as i64;
                    (target, h.max(1))
                }
            }
        };

        let engine_blob = engine::ImageBlob {
            content_type: image.mime,
            data: image.bytes,
        };
        let new_doc = self.undo.current().insert_inline_image_at(
            to_engine_pos(at),
            engine_blob,
            w_emu,
            h_emu,
        );
        self.undo.push(new_doc);
        self.layout_cache.get_mut().clear();
        self.dirty.invalidate(full_page_rect(self.scale()));
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        self.selection_changed()
    }

    /// `Command::ResizeImage` (Issue #44) — overwrite the inline image's
    /// `<wp:extent>` at `(path, at)` with new EMU dimensions. Clears the
    /// layout cache (the reserved image box changed size) and repaints.
    fn do_resize_image(
        &mut self,
        path: BridgeBlockPath,
        at: u32,
        width_emu: i64,
        height_emu: i64,
    ) -> Event {
        let new_doc = self.undo.current().resize_inline_image_at(
            &bridge_to_engine_path(path),
            at,
            width_emu,
            height_emu,
        );
        self.undo.push(new_doc);
        self.layout_cache.get_mut().clear();
        self.invalidate_layout_snapshot();
        self.dirty.invalidate(full_page_rect(self.scale()));
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        self.selection_changed()
    }

    /// `Command::SetColumns` (Sprint 2 UI Edition) — set the multi-
    /// column layout on the section containing `at`. Mutates
    /// `Section.columns` via [`engine::DocumentTree::set_section_columns_at`]
    /// and triggers a full repaint so the paginator re-flows.
    fn do_set_columns(&mut self, at: BridgeLogicalPos, count: u8, gutter_pt: f32) -> Event {
        let new_doc =
            self.undo
                .current()
                .set_section_columns_at(to_engine_pos(at), count, gutter_pt);
        self.undo.push(new_doc);
        self.dirty.invalidate(full_page_rect(self.scale()));
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        self.selection_changed()
    }

    /// `Command::InsertPageBreak` (Sprint 2 UI Edition) — flip
    /// `ParaProperties.page_break_before` on the paragraph at `at`.
    fn do_insert_page_break(&mut self, at: BridgeLogicalPos) -> Event {
        let new_doc = self
            .undo
            .current()
            .set_page_break_before(to_engine_pos(at), true);
        self.undo.push(new_doc);
        self.announce(AnnouncementPriority::Polite, "Page break inserted");
        self.dirty.invalidate(full_page_rect(self.scale()));
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        self.selection_changed()
    }

    /// `Command::InsertSectionBreak` (Phase 3, #40) — split the caret
    /// paragraph into two sections via
    /// [`DocumentTree::insert_section_break_at`]. The caret lands at
    /// the start of the second section's first paragraph (Word
    /// behavior). Rejected inside table cells — an interior sectPr is
    /// a body-level paragraph property, and the UI disables the Breaks
    /// menu there (Honest UX: the error is the backstop, not the
    /// affordance).
    fn do_insert_section_break(&mut self, at: BridgeLogicalPos, kind: SectionBreakKind) -> Event {
        if at.path.steps.len() != 1 {
            return Event::Error {
                message: "InsertSectionBreak: section breaks inside table cells are not supported"
                    .into(),
            };
        }
        let engine_kind = match kind {
            SectionBreakKind::NextPage => engine::SectionType::NextPage,
            SectionBreakKind::Continuous => engine::SectionType::Continuous,
        };
        let new_doc = self
            .undo
            .current()
            .insert_section_break_at(to_engine_pos(at.clone()), engine_kind);
        let next_path = engine::bump_last_block_index(&bridge_to_engine_path(at.path));
        let caret = BridgeLogicalPos {
            path: engine_to_bridge_path(next_path),
            offset: 0,
        };
        self.announce(
            AnnouncementPriority::Polite,
            match kind {
                SectionBreakKind::NextPage => "Section break (next page) inserted",
                SectionBreakKind::Continuous => "Section break (continuous) inserted",
            },
        );
        self.commit_edit(new_doc, caret)
    }

    /// `Command::SetParagraphBorders` (Sprint 2 UI Edition) — set
    /// `<w:pPr><w:pBdr>` on every paragraph the range spans. Reuses
    /// [`bridge_to_engine_borders`] from the cell-border path; an
    /// all-edges-`None` value clears the borders.
    fn do_set_paragraph_borders(
        &mut self,
        range: BridgeLogicalRange,
        borders: bridge::BridgeCellBorders,
    ) -> Event {
        let engine_borders = bridge_to_engine_borders(borders);
        let cleared = engine_borders.top.is_none()
            && engine_borders.left.is_none()
            && engine_borders.bottom.is_none()
            && engine_borders.right.is_none();
        let target = if cleared { None } else { Some(engine_borders) };
        let (start, end) = ordered(range.start, range.end);
        let new_doc = self.undo.current().set_paragraph_borders(
            to_engine_pos(start),
            to_engine_pos(end),
            target,
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
        self.announce(
            AnnouncementPriority::Polite,
            format!("Table inserted, {rows} rows by {cols} columns"),
        );
        self.push_table_edit(new_doc)
    }
    fn do_delete_table(&mut self, path: bridge::BlockPath) -> Event {
        let new_doc = self
            .undo
            .current()
            .delete_table(bridge_to_engine_path(path));
        self.announce(AnnouncementPriority::Polite, "Table deleted");
        self.push_table_edit(new_doc)
    }
    fn do_insert_row(
        &mut self,
        path: bridge::BlockPath,
        row: u32,
        side: bridge::InsertSide,
    ) -> Event {
        /* Resolve Before/After at the bridge boundary so the engine
         * never performs signed arithmetic on `usize`. `Before` lands
         * the new row at `row`; `After` lands it at `row + 1`. The
         * `usize` widening of `u32` is lossless on 32-bit and
         * 64-bit targets — wasm32 is 32-bit but `usize` is 32-bit
         * there too, so the `+ 1` for `After` cannot overflow given
         * we already arrived from a `u32` (`u32::MAX + 1` could
         * overflow `usize` only on a 32-bit target; treat it as
         * "append to the end" by saturating). */
        let at = match side {
            bridge::InsertSide::Before => row as usize,
            bridge::InsertSide::After => (row as usize).saturating_add(1),
        };
        let new_doc = self
            .undo
            .current()
            .insert_row(bridge_to_engine_path(path), at);
        self.announce(AnnouncementPriority::Polite, "Row inserted");
        self.push_table_edit(new_doc)
    }
    fn do_delete_row(&mut self, path: bridge::BlockPath, row: u32) -> Event {
        let new_doc = self
            .undo
            .current()
            .delete_row(bridge_to_engine_path(path), row);
        self.announce(AnnouncementPriority::Polite, "Row deleted");
        self.push_table_edit(new_doc)
    }
    fn do_insert_column(
        &mut self,
        path: bridge::BlockPath,
        col: u32,
        side: bridge::InsertSide,
    ) -> Event {
        let at = match side {
            bridge::InsertSide::Before => col as usize,
            bridge::InsertSide::After => (col as usize).saturating_add(1),
        };
        let new_doc = self
            .undo
            .current()
            .insert_column(bridge_to_engine_path(path), at);
        self.announce(AnnouncementPriority::Polite, "Column inserted");
        self.push_table_edit(new_doc)
    }
    fn do_delete_column(&mut self, path: bridge::BlockPath, col: u32) -> Event {
        let new_doc = self
            .undo
            .current()
            .delete_column(bridge_to_engine_path(path), col);
        self.announce(AnnouncementPriority::Polite, "Column deleted");
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
        self.announce(AnnouncementPriority::Polite, "Cells merged");
        self.push_table_edit(new_doc)
    }
    fn do_split_cell(&mut self, path: bridge::BlockPath, row: u32, col: u32) -> Event {
        let new_doc = self
            .undo
            .current()
            .split_cell(bridge_to_engine_path(path), row, col);
        self.announce(AnnouncementPriority::Polite, "Cell split");
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
        self.announce(
            AnnouncementPriority::Polite,
            if rgba.is_some() {
                "Cell shading updated"
            } else {
                "Cell shading cleared"
            },
        );
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
        self.announce(AnnouncementPriority::Polite, "Cell borders updated");
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

/// Sprint 10 — borrow-shaped variant of `bridge_to_engine_path` for the
/// `SelectionChanged` hot path; cloning is the same `Vec` walk the
/// owned variant does, but the call sites already hold `&BridgeBlockPath`.
fn bridge_path_to_engine(p: &bridge::BlockPath) -> engine::BlockPath {
    engine::BlockPath {
        steps: p
            .steps
            .iter()
            .map(|s| match s {
                bridge::PathStep::Block { idx } => engine::PathStep::Block(*idx),
                bridge::PathStep::Cell { row, col } => engine::PathStep::Cell {
                    row: *row,
                    col: *col,
                },
            })
            .collect(),
    }
}

/// Sprint 10 — engine RGBA bytes → bridge `Color`.
fn rgba_to_bridge_color(c: [u8; 4]) -> bridge::Color {
    bridge::Color {
        r: c[0],
        g: c[1],
        b: c[2],
        a: c[3],
    }
}

/// Sprint 10 — engine `CellBorders` (option-of-strokes) → bridge wire
/// shape. `None` collapses to an all-`None` `BridgeCellBorders` (a
/// `Default` value) so the dialog sees "no overrides".
fn engine_borders_to_bridge(b: Option<&engine::CellBorders>) -> bridge::BridgeCellBorders {
    let Some(b) = b else {
        return bridge::BridgeCellBorders::default();
    };
    bridge::BridgeCellBorders {
        top: b.top.as_ref().map(engine_stroke_to_bridge),
        left: b.left.as_ref().map(engine_stroke_to_bridge),
        bottom: b.bottom.as_ref().map(engine_stroke_to_bridge),
        right: b.right.as_ref().map(engine_stroke_to_bridge),
    }
}

fn engine_stroke_to_bridge(s: &engine::BorderStroke) -> BridgeBorderStroke {
    let style = match s.style {
        engine::BorderStyle::Single => BridgeBorderStyle::Single,
        engine::BorderStyle::Double => BridgeBorderStyle::Double,
        engine::BorderStyle::Dotted => BridgeBorderStyle::Dotted,
        engine::BorderStyle::Dashed => BridgeBorderStyle::Dashed,
        /* `BorderStyle::None` + the round-trip-preserving `Other`
        token collapse to wire `None` — Word treats unknown stroke
        styles as a solid edge. */
        engine::BorderStyle::None | engine::BorderStyle::Other(_) => BridgeBorderStyle::None,
    };
    BridgeBorderStroke {
        style,
        size_eighth_pt: s.size_eighth_pt,
        color: s.color.map(rgba_to_bridge_color),
    }
}

struct RenderStats {
    page_width: f32,
    page_height: f32,
    line_count: u32,
    glyph_count: u32,
    /// Phase 6b — paginated total of the pages **already laid out**:
    /// every emitted page's height summed plus inter-page gaps.
    document_height: f32,
    page_count: u32,
    /// Audit gap C.H1 — `true` when every body block was consumed;
    /// `false` when the viewport-cull budget stopped the paginator early.
    is_full_layout: bool,
    /// Audit gap C.H1 — best-guess total document height including
    /// blocks that have not been laid out yet (the running total plus
    /// `AVG_BLOCK_HEIGHT_PT` × remaining body blocks). The scrollbar
    /// reads this, so it always shrinks (never grows) as background
    /// completion fills in.
    estimated_document_height: f32,
    /// Issue #26 — absolute per-page tops + heights in device px.
    page_tops: Vec<f32>,
    page_heights: Vec<f32>,
    /// Phase 3 (#39) — per-page top/bottom margins in device px,
    /// index-aligned with `page_tops`; the shell's header/footer
    /// double-click zone gate.
    page_margin_tops: Vec<f32>,
    page_margin_bottoms: Vec<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::*;

    wasm_bindgen_test_configure!(run_in_browser);

    #[wasm_bindgen_test]
    async fn ping_pong_round_trip() {
        let mut engine = Engine {
            page_ctxs: Vec::new(),
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
            layout_snapshot: RefCell::new(None),
            sections_memo: RefCell::new(None),
            active_story: StoryTarget::Body,
            stashed_body_selection: None,
            a11y_cache: None,
            image_cache: HashMap::new(),
            last_paint_dims: LastPaintDims::default(),
            lazy_layout: LazyLayoutState::default(),
            caret_affinity: CaretAffinity::default(),
            pending_announcements: Vec::new(),
            tracking_changes: false,
            review_author: "You".to_string(),
            review_date: String::new(),
        };
        let cmd_js = serde_wasm_bindgen::to_value(&Command::Ping).expect("encode ping");
        let evt_js = engine
            .dispatch(cmd_js)
            .await
            .expect("dispatch should succeed");
        let evt: Event = serde_wasm_bindgen::from_value(evt_js).expect("decode event");
        assert!(matches!(evt, Event::Pong), "expected Pong, got {evt:?}");
    }

    /// Issue #53 — the acceptance shape: SET_SELECTION followed by
    /// INSERT_TEXT{at: None} through the real dispatch path must insert
    /// at the JUST-SET caret (the serialized queue means "posted after"
    /// = "executes after", and None reads the live caret).
    #[wasm_bindgen_test]
    async fn set_selection_then_insert_none_lands_at_new_caret() {
        let mut engine = test_engine_with_doc(DocumentTree::from_text("hello world"));
        let set = serde_wasm_bindgen::to_value(&Command::SetSelection {
            range: BridgeLogicalRange {
                start: bpos_top(0, 5),
                end: bpos_top(0, 5),
            },
            caret: bpos_top(0, 5),
        })
        .expect("encode set-selection");
        engine.dispatch(set).await.expect("set selection");

        let ins = serde_wasm_bindgen::to_value(&Command::InsertText {
            at: None,
            text: "X".to_string(),
        })
        .expect("encode insert");
        let evt_js = engine.dispatch(ins).await.expect("insert");
        let evt: Event = serde_wasm_bindgen::from_value(evt_js).expect("decode event");
        assert!(
            matches!(evt, Event::SelectionChanged { .. }),
            "live-caret insert replies SelectionChanged, got {evt:?}"
        );
        assert_eq!(
            engine.undo.current().paragraph_text(0),
            Some("helloX world"),
            "insert must land at the just-set caret, not append at the end"
        );
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

    /// Tables epic — Tab traversal is row-aware over cell indices,
    /// skips vMerge continuations, and appends a fresh full-width row
    /// from the last cell (Word parity).
    #[test]
    fn tab_walks_cells_skips_continuations_and_appends_row() {
        let cell_pos = |r: u32, c: u32| BridgeLogicalPos {
            path: BridgeBlockPath {
                steps: vec![
                    BridgePathStep::Block { idx: 1 },
                    BridgePathStep::Cell { row: r, col: c },
                    BridgePathStep::Block { idx: 0 },
                ],
            },
            offset: 0,
        };
        let cell_of_pos = |p: &BridgeLogicalPos| match p.path.steps[1] {
            BridgePathStep::Cell { row, col } => (row, col),
            _ => panic!("expected cell step"),
        };
        /* 2×2 with column 0 vertically merged: row-1 cell-0 is Continue. */
        let doc = engine::DocumentTree::from_text("hi")
            .insert_table(EngineBlockPath::top(1), 2, 2)
            .merge_cells(EngineBlockPath::top(1), 0, 0, 1, 0);
        let mut undo = UndoStack::new(doc, 16);
        let s1 = cell_tab_step(&mut undo, &cell_pos(0, 0), true);
        assert_eq!(cell_of_pos(&s1), (0, 1));
        /* Forward wrap to row 1 — cell 0 is a Continue, skip to (1,1). */
        let s2 = cell_tab_step(&mut undo, &s1, true);
        assert_eq!(cell_of_pos(&s2), (1, 1));
        /* Last cell + forward appends a full-width row, lands on (2,0). */
        let s3 = cell_tab_step(&mut undo, &s2, true);
        assert_eq!(cell_of_pos(&s3), (2, 0));
        let t = undo.current().blocks[1].as_table().unwrap();
        assert_eq!(t.rows.len(), 3, "Tab in the last cell appends a row");
        assert_eq!(t.rows[2].cells.len(), 2, "appended row is full grid width");
        /* Backward from (1,1) skips the Continue at (1,0) → (0,1). */
        let b = cell_tab_step(&mut undo, &s2, false);
        assert_eq!(cell_of_pos(&b), (0, 1));
        /* Backward from the very first cell is a no-op. */
        let stay = cell_tab_step(&mut undo, &cell_pos(0, 0), false);
        assert_eq!(cell_of_pos(&stay), (0, 0));
    }

    /// Tables epic — `cell_content_span` spans first → last paragraph
    /// of the addressed cell; `None` outside tables.
    #[test]
    fn cell_content_span_covers_cell_and_rejects_body() {
        let doc = engine::DocumentTree::from_text("hi").insert_table(EngineBlockPath::top(1), 1, 1);
        let path = BridgeBlockPath {
            steps: vec![
                BridgePathStep::Block { idx: 1 },
                BridgePathStep::Cell { row: 0, col: 0 },
                BridgePathStep::Block { idx: 0 },
            ],
        };
        let (start, end) = cell_content_span(&doc, &path).expect("span");
        assert_eq!(start.offset, 0);
        assert_eq!(end.offset, 0, "default cell holds one empty paragraph");
        assert_eq!(start.path.steps.len(), 3);
        assert!(cell_content_span(&doc, &BridgeBlockPath::top(0)).is_none());
    }

    /// Tables epic — anchor and caret in different cells of the same
    /// table promote the selection to a TABLE_CELLS rectangle.
    #[test]
    fn cross_cell_endpoints_promote_to_table_cells() {
        let pos = |r: u32, c: u32| BridgeLogicalPos {
            path: BridgeBlockPath {
                steps: vec![
                    BridgePathStep::Block { idx: 1 },
                    BridgePathStep::Cell { row: r, col: c },
                    BridgePathStep::Block { idx: 0 },
                ],
            },
            offset: 0,
        };
        match derive_selection_kind(&pos(0, 0), &pos(1, 1)) {
            SelectionKind::TableCells {
                from_row,
                from_col,
                to_row,
                to_col,
                ..
            } => {
                assert_eq!((from_row, from_col, to_row, to_col), (0, 0, 1, 1));
            }
            SelectionKind::Linear => panic!("cross-cell endpoints must promote"),
        }
        assert!(matches!(
            derive_selection_kind(&pos(0, 0), &pos(0, 0)),
            SelectionKind::Linear
        ));
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
            underline: engine::UnderlineStyle::None,
            strike: false,
            bg_color: None,
            baseline_shift_px: 0.0,
        };
        let glyph = |cluster: u32, adv: f32, synthetic: bool| layout::PositionedGlyph {
            id: 1,
            cluster,
            x_advance: adv,
            y_advance: 0.0,
            x_offset: 0.0,
            y_offset: 0.0,
            synthetic,
            inline_image_rel_id: None,
            inline_footnote_marker: None,
            inline_object_height: 0.0,
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
            source_start: 0,
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
            base_scale: 1.0,
            zoom: 1.0,
        };
        let para = |text: &str| engine::Paragraph {
            text: text.to_string(),
            spans: Vec::new(),
            props: engine::ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            resolved_list_indent: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: engine::ParaProperties::default(),
            section_end: None,
        };
        let a = para("hello world");
        /* Identical content + config -> identical key. */
        assert_eq!(
            paragraph_layout_key(&a, &cfg, 1.0, 451.0, empty_sctx()),
            paragraph_layout_key(&a, &cfg, 1.0, 451.0, empty_sctx()),
        );
        /* Different text -> different key. */
        assert_ne!(
            paragraph_layout_key(&a, &cfg, 1.0, 451.0, empty_sctx()),
            paragraph_layout_key(&para("hello there"), &cfg, 1.0, 451.0, empty_sctx()),
        );
        /* A paragraph alignment override -> different key. */
        let mut centered = para("hello world");
        centered.props.alignment = Some(EngineAlignment::Center);
        assert_ne!(
            paragraph_layout_key(&a, &cfg, 1.0, 451.0, empty_sctx()),
            paragraph_layout_key(&centered, &cfg, 1.0, 451.0, empty_sctx()),
        );
        /* A different device scale -> different key. */
        assert_ne!(
            paragraph_layout_key(&a, &cfg, 1.0, 451.0, empty_sctx()),
            paragraph_layout_key(&a, &cfg, 2.0, 451.0, empty_sctx()),
        );
        /* Audit gap A.H2 — a different `max_width` (e.g. swapping a
        single-column body for a 2-column section that halves the
        layout width) must miss the cache. */
        assert_ne!(
            paragraph_layout_key(&a, &cfg, 1.0, 451.0, empty_sctx()),
            paragraph_layout_key(&a, &cfg, 1.0, 220.0, empty_sctx()),
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
            resolved_list_indent: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: engine::ParaProperties::default(),
            section_end: None,
        };
        /* Compose 3 bytes at offset 3 — splits the one committed span. */
        let spans = composition_layout_spans(&p, empty_sctx(), 3, 3, 16.0, 1.0);
        assert_eq!(spans.len(), 3, "split + composition span");
        assert_eq!((spans[0].start, spans[0].end), (0, 3));
        assert!(!spans[0].underline.is_visible());
        assert_eq!((spans[1].start, spans[1].end), (3, 6));
        assert!(
            spans[1].underline.is_visible(),
            "composition span must be underlined"
        );
        assert_eq!((spans[2].start, spans[2].end), (6, 9));
        assert!(!spans[2].underline.is_visible());
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
            resolved_list_indent: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: engine::ParaProperties::default(),
            section_end: None,
        };
        let spans = composition_layout_spans(&p, empty_sctx(), 3, 2, 16.0, 1.0);
        assert_eq!(spans.len(), 2);
        assert_eq!((spans[0].start, spans[0].end), (0, 3));
        assert_eq!((spans[1].start, spans[1].end), (3, 5));
        assert!(spans[1].underline.is_visible());
    }

    /// Helper: build a single-paragraph document for the word-jump tests.
    fn one_para_doc(text: &str) -> DocumentTree {
        DocumentTree::from_text(text)
    }

    /// Ctrl+ArrowLeft mid-word jumps to the start of the current word.
    #[test]
    fn word_left_jumps_to_word_start() {
        let doc = one_para_doc("hello world rust");
        /* Caret in the middle of "world" (offset 9 — between r and l). */
        let out = step_word_left(&doc, bpos_top(0, 9));
        assert_eq!(out.offset, 6, "land on 'w' of 'world'");
    }

    /// Ctrl+ArrowLeft at the start of a word jumps to the start of the
    /// PREVIOUS word, skipping the whitespace between them.
    #[test]
    fn word_left_at_word_start_jumps_to_previous_word() {
        let doc = one_para_doc("hello world rust");
        let out = step_word_left(&doc, bpos_top(0, 6));
        assert_eq!(out.offset, 0, "land on 'h' of 'hello'");
    }

    /// Ctrl+ArrowLeft inside whitespace skips back through the whitespace
    /// and through the preceding word to its start.
    #[test]
    fn word_left_from_whitespace_jumps_past_previous_word() {
        let doc = one_para_doc("hello world");
        let out = step_word_left(&doc, bpos_top(0, 5));
        assert_eq!(out.offset, 0);
    }

    /// Ctrl+ArrowRight from inside a word lands on the first char of the
    /// next word — Word's convention, not the end of the current word.
    #[test]
    fn word_right_jumps_to_next_word_start() {
        let doc = one_para_doc("hello world rust");
        let out = step_word_right(&doc, bpos_top(0, 2));
        assert_eq!(out.offset, 6, "land on 'w' of 'world'");
    }

    /// Ctrl+ArrowRight at the end of the last word pins at paragraph
    /// end when no next paragraph exists.
    #[test]
    fn word_right_pins_at_document_end() {
        let doc = one_para_doc("hello world");
        let len = "hello world".len() as u32;
        let out = step_word_right(&doc, bpos_top(0, len));
        assert_eq!(out.offset, len);
    }

    /// Word-jump scanner is char-aware — multi-byte UTF-8 (Arabic) is
    /// segmented by ASCII whitespace and never lands inside a char.
    #[test]
    fn word_left_arabic_finds_word_start() {
        let doc = one_para_doc("hello مرحبا بالعالم");
        let len = doc
            .paragraph_at_path(&engine::BlockPath::top(0))
            .unwrap()
            .text
            .len() as u32;
        /* From the very end, one ArrowLeft jumps to the start of the
        last Arabic word ("بالعالم") — the byte offset of its first
        char. */
        let out = step_word_left(&doc, bpos_top(0, len));
        let expected_word = "بالعالم";
        let expected_offset = "hello مرحبا ".len() as u32;
        assert_eq!(out.offset, expected_offset);
        assert_eq!(
            &doc.paragraph_at_path(&engine::BlockPath::top(0))
                .unwrap()
                .text[out.offset as usize..],
            expected_word
        );
    }

    /// Word-jump across the paragraph boundary — Ctrl+ArrowRight at the
    /// end of a paragraph hops to offset 0 of the next paragraph.
    #[test]
    fn word_right_crosses_paragraph_boundary() {
        let doc = DocumentTree::from_text("firstsecond").split_paragraph(engine::LogicalPos::new(
            engine::BlockPath::top(0),
            "first".len() as u32,
        ));
        let first_len = "first".len() as u32;
        let out = step_word_right(&doc, bpos_top(0, first_len));
        assert_eq!(out.path.steps[0], bridge::PathStep::Block { idx: 1 });
        assert_eq!(out.offset, 0);
    }

    /// Ctrl+ArrowLeft at offset 0 of a non-first paragraph hops to the
    /// END of the previous paragraph.
    #[test]
    fn word_left_crosses_paragraph_boundary() {
        let doc = DocumentTree::from_text("firstsecond").split_paragraph(engine::LogicalPos::new(
            engine::BlockPath::top(0),
            "first".len() as u32,
        ));
        let out = step_word_left(&doc, bpos_top(1, 0));
        assert_eq!(out.path.steps[0], bridge::PathStep::Block { idx: 0 });
        assert_eq!(out.offset, "first".len() as u32);
    }

    /* ---- Roadmap Phase 1: UAX #29 grapheme stepping (§I.1) ------ */

    /// `step_right` walks one grapheme cluster — a base char + its
    /// combining marks traverse as ONE step, never bisecting a fused
    /// glyph. Arabic ARABIC LETTER YEH + FATHATAN is `يً` (2 chars, 4
    /// UTF-8 bytes, 1 grapheme).
    #[test]
    fn step_right_skips_combining_marks_as_one_grapheme() {
        let doc = one_para_doc("aيًb");
        /* Offsets:
           0 = 'a'         (1 byte)
           1 = 'ي'         (2 bytes)
           3 = ARABIC FATHATAN U+064B (2 bytes, combining mark on ي)
           5 = 'b'         (1 byte)
           6 = end
        */
        let out = step_right(&doc, bpos_top(0, 1));
        assert_eq!(out.offset, 5, "ي+ٌ are one grapheme — caret skips past both");
    }

    /// Symmetric — `step_left` from after 'b' skips both ي and its
    /// combining mark in one motion.
    #[test]
    fn step_left_skips_combining_marks_as_one_grapheme() {
        let doc = one_para_doc("aيًb");
        let out = step_left(&doc, bpos_top(0, 5));
        assert_eq!(out.offset, 1);
    }

    /* ---- Roadmap Phase 1: UAX #29 word boundaries (§II) ---------- */

    /// `"isn't"` is ONE word — the apostrophe joins under UAX #29 WB6/WB7,
    /// so WordRight from offset 0 lands past the contraction, on 'd'.
    #[test]
    fn word_right_treats_contraction_as_one_word() {
        let doc = one_para_doc("isn't done");
        let out = step_word_right(&doc, bpos_top(0, 0));
        let expected = "isn't ".len() as u32;
        assert_eq!(out.offset, expected, "land on 'd' of 'done'");
    }

    /// `"3.14"` is ONE word — period between digits is MidNumLet/MidNum
    /// under WB11/WB12, so WordRight from offset 0 jumps past `3.14`.
    #[test]
    fn word_right_keeps_decimal_number_intact() {
        let doc = one_para_doc("3.14 pi");
        let out = step_word_right(&doc, bpos_top(0, 0));
        assert_eq!(out.offset, "3.14 ".len() as u32, "land on 'p' of 'pi'");
    }

    /* ---- Roadmap Phase 1: Home / End (§I.2, §III.4) -------------- */

    /// `MoveDirection::DocHome` lands the caret at offset 0 of the
    /// document's first paragraph. (Native — drives the engine directly,
    /// no layout config; `DocHome` doesn't depend on geometry.)
    #[test]
    fn doc_home_and_end_address_document_boundaries() {
        let mut e = Engine {
            page_ctxs: Vec::new(),
            ctx: None,
            fonts: HashMap::new(),
            undo: UndoStack::new(DocumentTree::from_text("hello world"), 8),
            layout_cfg: None,
            atlas: GlyphAtlas::new(),
            vello: None,
            dirty: DirtyTracker::new(),
            selection: Some(SelectionState {
                anchor: bpos_top(0, 5),
                caret: bpos_top(0, 5),
                ideal_x: None,
                kind: SelectionKind::Linear,
            }),
            composition: None,
            pending_format: None,
            layout_cache: new_layout_cache(),
            layout_snapshot: RefCell::new(None),
            sections_memo: RefCell::new(None),
            active_story: StoryTarget::Body,
            stashed_body_selection: None,
            a11y_cache: None,
            image_cache: HashMap::new(),
            last_paint_dims: LastPaintDims::default(),
            lazy_layout: LazyLayoutState::default(),
            caret_affinity: CaretAffinity::default(),
            pending_announcements: Vec::new(),
            tracking_changes: false,
            review_author: "You".to_string(),
            review_date: String::new(),
        };
        e.do_move_caret(MoveDirection::DocHome, false);
        assert_eq!(e.selection.as_ref().unwrap().caret.offset, 0);
        e.do_move_caret(MoveDirection::DocEnd, false);
        assert_eq!(
            e.selection.as_ref().unwrap().caret.offset,
            "hello world".len() as u32
        );
    }

    /* ---- Roadmap Phase 1: BiDi visual-arrow flip (§III.1) -------- */

    /// In an RTL paragraph, `MoveDirection::Right` maps to logical
    /// **backward** (smaller byte offset) — the visual-right caret motion
    /// the user expects in Arabic text.
    #[test]
    fn arrow_right_in_rtl_paragraph_steps_logical_backward() {
        let mut e = Engine {
            page_ctxs: Vec::new(),
            ctx: None,
            fonts: HashMap::new(),
            undo: UndoStack::new(DocumentTree::from_text("مرحبا"), 8),
            layout_cfg: None,
            atlas: GlyphAtlas::new(),
            vello: None,
            dirty: DirtyTracker::new(),
            selection: Some(SelectionState {
                /* Mid-word in "مرحبا" — offset 4 sits between two
                Arabic letters (each 2 bytes). */
                anchor: bpos_top(0, 4),
                caret: bpos_top(0, 4),
                ideal_x: None,
                kind: SelectionKind::Linear,
            }),
            composition: None,
            pending_format: None,
            layout_cache: new_layout_cache(),
            layout_snapshot: RefCell::new(None),
            sections_memo: RefCell::new(None),
            active_story: StoryTarget::Body,
            stashed_body_selection: None,
            a11y_cache: None,
            image_cache: HashMap::new(),
            last_paint_dims: LastPaintDims::default(),
            lazy_layout: LazyLayoutState::default(),
            caret_affinity: CaretAffinity::default(),
            pending_announcements: Vec::new(),
            tracking_changes: false,
            review_author: "You".to_string(),
            review_date: String::new(),
        };
        e.do_move_caret(MoveDirection::Right, false);
        /* RTL flip: visual-Right is logical-backward, so 4 → 2. */
        assert_eq!(e.selection.as_ref().unwrap().caret.offset, 2);
    }

    /// In an RTL paragraph, `MoveDirection::Left` steps logical
    /// **forward** (visual-left = logical-forward in RTL text).
    #[test]
    fn arrow_left_in_rtl_paragraph_steps_logical_forward() {
        let mut e = Engine {
            page_ctxs: Vec::new(),
            ctx: None,
            fonts: HashMap::new(),
            undo: UndoStack::new(DocumentTree::from_text("مرحبا"), 8),
            layout_cfg: None,
            atlas: GlyphAtlas::new(),
            vello: None,
            dirty: DirtyTracker::new(),
            selection: Some(SelectionState {
                anchor: bpos_top(0, 4),
                caret: bpos_top(0, 4),
                ideal_x: None,
                kind: SelectionKind::Linear,
            }),
            composition: None,
            pending_format: None,
            layout_cache: new_layout_cache(),
            layout_snapshot: RefCell::new(None),
            sections_memo: RefCell::new(None),
            active_story: StoryTarget::Body,
            stashed_body_selection: None,
            a11y_cache: None,
            image_cache: HashMap::new(),
            last_paint_dims: LastPaintDims::default(),
            lazy_layout: LazyLayoutState::default(),
            caret_affinity: CaretAffinity::default(),
            pending_announcements: Vec::new(),
            tracking_changes: false,
            review_author: "You".to_string(),
            review_date: String::new(),
        };
        e.do_move_caret(MoveDirection::Left, false);
        /* RTL flip: visual-Left is logical-forward, so 4 → 6. */
        assert_eq!(e.selection.as_ref().unwrap().caret.offset, 6);
    }

    /* ---- Roadmap Phase 2: run-level visual step (§III.1) -------- */

    /// Audit gap B.H3 — spatial visual arrow stepping is byte-order-agnostic.
    /// Build a synthetic mixed-direction slot list where the LTR run
    /// covers bytes 0..4 left-to-right (x: 0,10,20,30,40) and the RTL
    /// run covers bytes 4..8 right-to-left (x: 90,80,70,60,50). The
    /// directional seam sits at byte 4 / x≈45. ArrowRight from byte 3
    /// (x=30) must land at x=40 (byte 4) — the visually next slot —
    /// **not** stride into the Arabic run logically. Successive
    /// ArrowRight presses then climb x=50,60,70,80,90, which step
    /// **logically backward** through the RTL bytes (8,7,6,5,4) — the
    /// hallmark of correct BiDi visual stepping.
    #[test]
    fn spatial_visual_step_crosses_bidi_seam_by_x_alone() {
        let slots = vec![
            CaretSlot { x: 0.0, byte: 0 },
            CaretSlot { x: 10.0, byte: 1 },
            CaretSlot { x: 20.0, byte: 2 },
            CaretSlot { x: 30.0, byte: 3 },
            CaretSlot { x: 40.0, byte: 4 }, // LTR trailing edge
            CaretSlot { x: 50.0, byte: 8 }, // RTL trailing (logical end)
            CaretSlot { x: 60.0, byte: 7 },
            CaretSlot { x: 70.0, byte: 6 },
            CaretSlot { x: 80.0, byte: 5 },
            CaretSlot { x: 90.0, byte: 4 }, // RTL leading edge (logical start)
        ];
        /* Right from x=30 → x=40, byte 4 (LTR side of the seam). */
        assert_eq!(neighbor_slot_byte_by_x(&slots, 30.0, true), Some(4));
        /* Right from x=40 → x=50, byte 8 (RTL trailing, logical END
        of the Arabic word — the scan crossed the seam by x). */
        assert_eq!(neighbor_slot_byte_by_x(&slots, 40.0, true), Some(8));
        /* Right from x=50 → x=60, byte 7 (logical step BACKWARD inside
        the RTL run; the spatial scan doesn't care). */
        assert_eq!(neighbor_slot_byte_by_x(&slots, 50.0, true), Some(7));
        /* Right at the visual edge (x=90) returns None — caller falls
        back to logical step to wrap onto the next line. */
        assert_eq!(neighbor_slot_byte_by_x(&slots, 90.0, true), None);
        /* Symmetric: Left from x=60 → x=50 (byte 8). */
        assert_eq!(neighbor_slot_byte_by_x(&slots, 60.0, false), Some(8));
        /* Left from x=50 → x=40 (byte 4 LTR side); the scan re-crosses
        the seam in the opposite direction with no special-case. */
        assert_eq!(neighbor_slot_byte_by_x(&slots, 50.0, false), Some(4));
        /* Left at the visual edge (x=0) returns None. */
        assert_eq!(neighbor_slot_byte_by_x(&slots, 0.0, false), None);
    }

    /// `slot_x_for_byte_with_affinity` resolves a BiDi-seam byte that
    /// hosts two slots to the side the caret's affinity asks for.
    #[test]
    fn affinity_picks_correct_slot_at_bidi_seam() {
        /* Synthesise a line whose `slots` carry two entries with byte=4
        at very different x's — the LTR-run trailing edge (x=80) and
        the RTL-run trailing edge (x=20). */
        let line = LineGeom {
            path: BridgeBlockPath::top(0),
            start_x: 0.0,
            hit_left: 0.0,
            hit_width: 100.0,
            y_top: 0.0,
            height: 20.0,
            start_byte: 0,
            end_byte: 8,
            slots: vec![
                CaretSlot { x: 20.0, byte: 4 },
                CaretSlot { x: 80.0, byte: 4 },
            ],
            runs: Vec::new(),
        };
        let leading = slot_x_for_byte_with_affinity(&line, 4, CaretAffinity::LeadingX);
        let trailing = slot_x_for_byte_with_affinity(&line, 4, CaretAffinity::TrailingX);
        assert_eq!(leading, 20.0);
        assert_eq!(trailing, 80.0);
    }

    /* ---- Roadmap Phase 2: UAX #29 deletion (§II.2) -------------- */

    /// `Ctrl+Backspace` deletes back to the start of the previous
    /// word-like segment per UAX #29, treating `isn't` as one word so
    /// the apostrophe is not the boundary the delete stops at.
    #[test]
    fn delete_word_backward_treats_contraction_as_one_word() {
        let mut e = Engine {
            page_ctxs: Vec::new(),
            ctx: None,
            fonts: HashMap::new(),
            undo: UndoStack::new(DocumentTree::from_text("isn't done"), 8),
            layout_cfg: None,
            atlas: GlyphAtlas::new(),
            vello: None,
            dirty: DirtyTracker::new(),
            selection: Some(SelectionState {
                anchor: bpos_top(0, "isn't done".len() as u32),
                caret: bpos_top(0, "isn't done".len() as u32),
                ideal_x: None,
                kind: SelectionKind::Linear,
            }),
            composition: None,
            pending_format: None,
            layout_cache: new_layout_cache(),
            layout_snapshot: RefCell::new(None),
            sections_memo: RefCell::new(None),
            active_story: StoryTarget::Body,
            stashed_body_selection: None,
            a11y_cache: None,
            image_cache: HashMap::new(),
            last_paint_dims: LastPaintDims::default(),
            lazy_layout: LazyLayoutState::default(),
            caret_affinity: CaretAffinity::default(),
            pending_announcements: Vec::new(),
            tracking_changes: false,
            review_author: "You".to_string(),
            review_date: String::new(),
        };
        e.do_delete_at_caret(false, true);
        /* "done" deleted → "isn't " remains. The whitespace-classifier
        scanner from before would have stopped at the apostrophe. */
        assert_eq!(
            e.undo
                .current()
                .paragraph_at_path(&engine::BlockPath::top(0))
                .unwrap()
                .text,
            "isn't "
        );
    }

    /* ---------- L2.2 (#7) table autofit min-content ---------- */

    /// Issue #29 — an empty style table for tests that don't exercise
    /// the run cascade (leaked so the borrows are 'static).
    fn empty_sctx() -> StyleContext<'static> {
        StyleContext {
            styles: Box::leak(Box::default()),
            run_defaults: Box::leak(Box::default()),
        }
    }

    /// Issue #29 — `build_style_spans` folds the pStyle-chain `<w:rPr>`
    /// under direct formatting; unstyled gap text inherits it wholesale.
    #[test]
    fn style_run_cascade_folds_into_materialized_spans() {
        let mut styles: std::collections::HashMap<String, engine::ParagraphStyle> =
            std::collections::HashMap::new();
        styles.insert(
            "Heading1".into(),
            engine::ParagraphStyle {
                id: "Heading1".into(),
                name: "Heading 1".into(),
                based_on: None,
                para: engine::ParaProperties::default(),
                run: engine::SpanStyle {
                    bold: Some(true),
                    font_size: Some(16.0),
                    ..Default::default()
                },
            },
        );
        let run_defaults = engine::SpanStyle::default();
        let sctx = StyleContext {
            styles: &styles,
            run_defaults: &run_defaults,
        };
        let mut para = engine::Paragraph {
            text: "hello world".into(),
            ..Default::default()
        };
        para.style_id = Some("Heading1".into());
        /* Direct italic on "hello" only. */
        para.spans.push(engine::StyleRun {
            start: 0,
            end: 5,
            style: engine::SpanStyle {
                italic: Some(true),
                ..Default::default()
            },
        });
        let spans = build_style_spans(&para, sctx, 12.0, [0, 0, 0, 255], 1.0);
        assert_eq!(spans.len(), 2);
        assert!(spans[0].bold && spans[0].italic, "direct folds OVER style");
        assert!((spans[0].px_size - 16.0).abs() < 0.01, "style size applies");
        assert!(
            spans[1].bold && !spans[1].italic,
            "gap text takes the style run"
        );
        assert!((spans[1].px_size - 16.0).abs() < 0.01);
        /* attrs parity: the layout key must see the style table too. */
        let cfg = autofit_test_cfg();
        let plain = empty_sctx();
        assert_ne!(
            paragraph_layout_key(&para, &cfg, 1.0, 451.0, sctx),
            paragraph_layout_key(&para, &cfg, 1.0, 451.0, plain),
            "two style-table states must not collide on a cache key"
        );
    }

    fn test_font_stack() -> FontStack {
        use std::sync::Arc;
        let bytes = include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf").to_vec();
        let font = LoadedFont::parse("test-latin".to_string(), bytes).expect("parse test font");
        let mut faces: HashMap<String, Arc<LoadedFont>> = HashMap::new();
        faces.insert("test-latin".to_string(), Arc::new(font));
        FontStack::from_faces(faces, "test-latin")
    }

    /// Issue #25 — the per-paragraph line-height resolver maps each
    /// `<w:spacing w:lineRule>` variant onto (px, exact-clip).
    #[test]
    fn resolve_line_height_maps_rules_to_px() {
        assert_eq!(resolve_line_height(None, 26.0, 2.0), (52.0, false));
        assert_eq!(
            resolve_line_height(Some(engine::LineHeight::Auto { twips: 480 }), 26.0, 1.0),
            (52.0, false),
            "Auto 480 twips = 2.0x the config single height"
        );
        assert_eq!(
            resolve_line_height(Some(engine::LineHeight::Exact { twips: 400 }), 26.0, 1.0),
            (20.0, true),
            "Exact 400 twips = 20 pt, clipping"
        );
        assert_eq!(
            resolve_line_height(Some(engine::LineHeight::AtLeast { twips: 600 }), 26.0, 1.0),
            (30.0, false),
            "AtLeast 600 twips = 30 pt minimum"
        );
    }

    /// Issue #25 — end-to-end through `layout_paragraph`: a 2.0x Auto
    /// multiplier doubles the line pitch; Exact clips below the glyph
    /// envelope instead of growing.
    #[test]
    fn line_spacing_changes_paragraph_height() {
        let stack = test_font_stack();
        let lay = |lh: f32, exact: bool| {
            let spans = [StyleSpan {
                start: 0,
                end: 2,
                px_size: 12.0,
                color: [0, 0, 0, 255],
                bold: false,
                italic: false,
                underline: engine::UnderlineStyle::None,
                strike: false,
                bg_color: None,
                font_family: None,
                caps_transform: false,
                baseline_shift_px: 0.0,
            }];
            layout_paragraph(ParagraphConfig {
                text: "hi",
                fonts: &stack,
                spans: &spans,
                base_direction: ShapingDirection::Ltr,
                max_width: 300.0,
                line_height: lh,
                line_height_exact: exact,
                alignment: Alignment::Start,
                indent_start_px: 0.0,
                indent_end_px: 0.0,
                first_line_indent_px: 0.0,
                hanging_indent_px: 0.0,
                marker_text: None,
                px_size_for_marker: 12.0,
                inline_objects: &[],
                tab_stops_px: &[],
            })
            .size
            .height
        };
        let single = lay(16.0, false);
        let (dbl_px, dbl_exact) =
            resolve_line_height(Some(engine::LineHeight::Auto { twips: 480 }), 16.0, 1.0);
        let double = lay(dbl_px, dbl_exact);
        assert!(
            (double - single * 2.0).abs() < 0.5,
            "2.0x multiplier must double the pitch: single {single}, double {double}"
        );
        let exact = lay(6.0, true);
        assert!(
            (exact - 6.0).abs() < 0.01,
            "Exact spacing clips to the requested band: got {exact}"
        );
    }

    fn full_span(end: u32) -> StyleSpan {
        StyleSpan {
            start: 0,
            end,
            px_size: 12.0,
            color: [0, 0, 0, 255],
            bold: false,
            italic: false,
            underline: engine::UnderlineStyle::None,
            strike: false,
            bg_color: None,
            font_family: None,
            caps_transform: false,
            baseline_shift_px: 0.0,
        }
    }

    /// Issue #50 — end-to-end marker geometry through `layout_paragraph`:
    /// with the stock list indent (start 36 pt, hanging 18 pt at scale 1)
    /// the bullet parks in the hanging gutter and every text line sits at
    /// the start indent. The pre-#50 regression laid both at x = 0 — the
    /// marker painted underneath the first glyph, i.e. invisible.
    #[test]
    fn list_marker_lands_in_the_hanging_gutter() {
        let stack = test_font_stack();
        let text = "hello bullet list";
        let spans = [full_span(text.len() as u32)];
        let para = layout_paragraph(ParagraphConfig {
            text,
            fonts: &stack,
            spans: &spans,
            base_direction: ShapingDirection::Ltr,
            max_width: 300.0,
            line_height: 16.0,
            line_height_exact: false,
            alignment: Alignment::Start,
            indent_start_px: 36.0,
            indent_end_px: 0.0,
            first_line_indent_px: 0.0,
            hanging_indent_px: 18.0,
            marker_text: Some("\u{2022}".to_string()),
            px_size_for_marker: 12.0,
            inline_objects: &[],
            tab_stops_px: &[],
        });
        let marker = para.marker.as_ref().expect("marker box");
        assert!(
            (marker.origin.x - 18.0).abs() < 0.01,
            "marker leading edge sits at start - hanging: got {}",
            marker.origin.x
        );
        assert!(
            marker.run.glyphs.iter().any(|g| g.id != 0),
            "bullet must map to a real glyph"
        );
        assert!(
            (para.lines[0].origin.x - 36.0).abs() < 0.01,
            "first text line must NOT outdent under the marker: got {}",
            para.lines[0].origin.x
        );
    }

    /// Issue #50 review follow-up — a marker WIDER than the hanging gutter
    /// ("viii." deep in a roman list) must shift away from the text instead
    /// of bleeding under the first glyph; the text lines stay pinned at the
    /// start indent.
    #[test]
    fn oversized_marker_shifts_out_of_the_text_zone() {
        let stack = test_font_stack();
        let text = "item";
        let spans = [full_span(text.len() as u32)];
        let para = layout_paragraph(ParagraphConfig {
            text,
            fonts: &stack,
            spans: &spans,
            base_direction: ShapingDirection::Ltr,
            max_width: 300.0,
            line_height: 16.0,
            line_height_exact: false,
            alignment: Alignment::Start,
            indent_start_px: 36.0,
            indent_end_px: 0.0,
            first_line_indent_px: 0.0,
            hanging_indent_px: 18.0,
            marker_text: Some("viii.".to_string()),
            px_size_for_marker: 16.0,
            inline_objects: &[],
            tab_stops_px: &[],
        });
        let marker = para.marker.as_ref().expect("marker box");
        assert!(
            marker.width > 18.0,
            "precondition: this marker must overflow the 18 px gutter (got {})",
            marker.width
        );
        assert!(
            marker.origin.x + marker.width <= 36.0 - 16.0 * 0.125 + 0.01,
            "marker trailing edge must clear the text start: origin {} width {}",
            marker.origin.x,
            marker.width
        );
        assert!(
            (para.lines[0].origin.x - 36.0).abs() < 0.01,
            "text stays pinned at the start indent"
        );
    }

    /// Issue #50 — RTL mirror: the marker parks in the RIGHT hanging
    /// gutter, inside the paragraph box. Pre-#50, a zero leading indent
    /// ejected it past `max_width` into the margin.
    #[test]
    fn rtl_list_marker_mirrors_into_right_gutter() {
        let stack = test_font_stack();
        let text = "hello";
        let spans = [full_span(text.len() as u32)];
        let para = layout_paragraph(ParagraphConfig {
            text,
            fonts: &stack,
            spans: &spans,
            base_direction: ShapingDirection::Rtl,
            max_width: 300.0,
            line_height: 16.0,
            line_height_exact: false,
            alignment: Alignment::Start,
            indent_start_px: 0.0,
            /* leading (right) inset for RTL */
            indent_end_px: 36.0,
            first_line_indent_px: 0.0,
            hanging_indent_px: 18.0,
            marker_text: Some("\u{2022}".to_string()),
            px_size_for_marker: 12.0,
            inline_objects: &[],
            tab_stops_px: &[],
        });
        let marker = para.marker.as_ref().expect("marker box");
        let trailing_edge = marker.origin.x + marker.width;
        assert!(
            (trailing_edge - (300.0 - 18.0)).abs() < 0.01,
            "marker leading (right) edge sits at max_width - (start - hanging): got {trailing_edge}"
        );
        assert!(
            marker.origin.x + marker.width <= 300.0,
            "marker must stay inside the paragraph box"
        );
    }

    /// Issue #68 — an RTL paragraph resolves its marker through the
    /// Arabic-script face, which (Amiri / NotoNaskhArabic) lacks the stock
    /// hollow / filled bullet glyphs `◦` (U+25E6) / `▪` (U+25AA). Without a
    /// coverage fallback the marker shapes to `.notdef` and vanishes. This
    /// stack loads an Arabic face as primary PLUS the Latin face that does
    /// cover the bullets, so the fix must swap in the covering face.
    #[test]
    fn rtl_bullet_falls_back_to_a_covering_face() {
        use std::sync::Arc;
        let arabic = LoadedFont::parse(
            "test-arabic".to_string(),
            include_bytes!("../../../ts/fonts/Amiri-Regular.ttf").to_vec(),
        )
        .expect("parse Amiri");
        let latin = LoadedFont::parse(
            "test-latin".to_string(),
            include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf").to_vec(),
        )
        .expect("parse LiberationSans");
        /* Precondition: the Arabic face genuinely lacks the deeper bullets;
        the Latin face has them. If a future font swap changes this the
        test is no longer exercising the bug. */
        assert!(
            !arabic.covers('\u{25E6}') && !arabic.covers('\u{25AA}'),
            "precondition: the Arabic face must NOT cover the hollow/filled bullets"
        );
        assert!(
            latin.covers('\u{25E6}') && latin.covers('\u{25AA}'),
            "precondition: the Latin face must cover the hollow/filled bullets"
        );
        let mut faces: HashMap<String, Arc<LoadedFont>> = HashMap::new();
        faces.insert("test-arabic".to_string(), Arc::new(arabic));
        faces.insert("test-latin".to_string(), Arc::new(latin));
        /* Arabic is primary so `resolve(Script::Arabic, ...)` picks it for
        an RTL paragraph — exactly the production path for the RTL seed. */
        let stack = FontStack::from_faces(faces, "test-arabic");

        for bullet in ['\u{25E6}', '\u{25AA}'] {
            let para = layout_paragraph(ParagraphConfig {
                text: "\u{0628}",
                fonts: &stack,
                spans: &[full_span(2)],
                base_direction: ShapingDirection::Rtl,
                max_width: 300.0,
                line_height: 16.0,
                line_height_exact: false,
                alignment: Alignment::Start,
                indent_start_px: 0.0,
                indent_end_px: 36.0,
                first_line_indent_px: 0.0,
                hanging_indent_px: 18.0,
                marker_text: Some(bullet.to_string()),
                px_size_for_marker: 12.0,
                inline_objects: &[],
                tab_stops_px: &[],
            });
            let marker = para.marker.as_ref().expect("marker box");
            assert!(
                marker.run.glyphs.iter().any(|g| g.id != 0),
                "RTL bullet U+{:04X} must map to a real glyph, not .notdef",
                bullet as u32
            );
            assert_eq!(
                marker.run.font, "test-latin",
                "the marker must swap to the covering Latin face for U+{:04X}",
                bullet as u32
            );
        }
    }

    /// Issue #68 — the coverage swap must NOT fire when the direction-
    /// derived face already covers the marker: an Arabic-Indic numbered
    /// marker legitimately belongs to the Arabic face, and `•` (U+2022) is
    /// in Amiri too. Guards against a blanket-Latin regression.
    #[test]
    fn rtl_marker_keeps_arabic_face_when_it_covers_the_glyph() {
        use std::sync::Arc;
        let arabic = LoadedFont::parse(
            "test-arabic".to_string(),
            include_bytes!("../../../ts/fonts/Amiri-Regular.ttf").to_vec(),
        )
        .expect("parse Amiri");
        assert!(
            arabic.covers('\u{2022}'),
            "precondition: Amiri covers the level-0 bullet"
        );
        let mut faces: HashMap<String, Arc<LoadedFont>> = HashMap::new();
        faces.insert("test-arabic".to_string(), Arc::new(arabic));
        faces.insert(
            "test-latin".to_string(),
            Arc::new(
                LoadedFont::parse(
                    "test-latin".to_string(),
                    include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf").to_vec(),
                )
                .expect("parse LiberationSans"),
            ),
        );
        let stack = FontStack::from_faces(faces, "test-arabic");
        let para = layout_paragraph(ParagraphConfig {
            text: "\u{0628}",
            fonts: &stack,
            spans: &[full_span(2)],
            base_direction: ShapingDirection::Rtl,
            max_width: 300.0,
            line_height: 16.0,
            line_height_exact: false,
            alignment: Alignment::Start,
            indent_start_px: 0.0,
            indent_end_px: 36.0,
            first_line_indent_px: 0.0,
            hanging_indent_px: 18.0,
            marker_text: Some("\u{2022}".to_string()),
            px_size_for_marker: 12.0,
            inline_objects: &[],
            tab_stops_px: &[],
        });
        let marker = para.marker.as_ref().expect("marker box");
        assert_eq!(
            marker.run.font, "test-arabic",
            "a covered marker must stay on the direction-derived face"
        );
        assert!(marker.run.glyphs.iter().any(|g| g.id != 0));
    }

    /// Issue #50 — `effective_layout_indents`: the numbering level's
    /// indent applies only when the paragraph carries no direct `<w:ind>`,
    /// and it is leading-edge-relative (RTL reads the leading offset from
    /// the end slot).
    #[test]
    fn effective_indents_fall_back_to_list_level() {
        let mut para = engine::Paragraph {
            text: "x".into(),
            ..Default::default()
        };
        para.resolved_list_indent = Some(engine::Indent {
            start_twips: 720,
            end_twips: 0,
            first_line_twips: 0,
            hanging_twips: 360,
        });
        assert_eq!(
            effective_layout_indents(&para, ShapingDirection::Ltr, 1.0),
            (36.0, 0.0, 0.0, 18.0)
        );
        assert_eq!(
            effective_layout_indents(&para, ShapingDirection::Rtl, 1.0),
            (0.0, 36.0, 0.0, 18.0),
            "RTL reads the leading offset from the end slot"
        );
        /* direct <w:ind> start wins per-slot; the level's hanging gutter
        survives (Word's per-attribute w:ind cascade) */
        para.props.indent = engine::Indent {
            start_twips: 1440,
            ..Default::default()
        };
        assert_eq!(
            effective_layout_indents(&para, ShapingDirection::Ltr, 1.0),
            (72.0, 0.0, 0.0, 18.0)
        );
        /* a PARTIAL direct indent (ruler end-drag writes {0, E, 0, 0})
        must not strip the level's start indent + gutter — the issue-#50
        invisible-marker collapse */
        para.props.indent = engine::Indent {
            end_twips: 720,
            ..Default::default()
        };
        assert_eq!(
            effective_layout_indents(&para, ShapingDirection::Ltr, 1.0),
            (36.0, 36.0, 0.0, 18.0)
        );
        /* a direct hanging shadows the level's firstLine/hanging PAIR */
        para.props.indent = engine::Indent {
            hanging_twips: 240,
            ..Default::default()
        };
        assert_eq!(
            effective_layout_indents(&para, ShapingDirection::Ltr, 1.0),
            (36.0, 0.0, 0.0, 12.0)
        );
    }

    /// Issue #58 — the Ruler read-back (`indent_for_caret`) must never
    /// diverge from what `effective_layout_indents` actually paints. A
    /// toggled bullet-list paragraph has all-zero `props.indent` but a real
    /// `resolved_list_indent`; before this fix the Ruler reported 0/0 for a
    /// paragraph visibly indented 36pt with an 18pt hanging gutter.
    #[test]
    fn indent_for_caret_falls_back_to_list_level() {
        fn test_engine(para: engine::Paragraph) -> Engine {
            let mut doc = DocumentTree::from_text("item");
            doc.blocks[0] = engine::Block::Paragraph(para);
            Engine {
                page_ctxs: Vec::new(),
                ctx: None,
                fonts: HashMap::new(),
                undo: UndoStack::new(doc, 8),
                layout_cfg: None,
                atlas: GlyphAtlas::new(),
                vello: None,
                dirty: DirtyTracker::new(),
                selection: None,
                composition: None,
                pending_format: None,
                layout_cache: new_layout_cache(),
                layout_snapshot: RefCell::new(None),
                sections_memo: RefCell::new(None),
                active_story: StoryTarget::Body,
                stashed_body_selection: None,
                a11y_cache: None,
                image_cache: HashMap::new(),
                last_paint_dims: LastPaintDims::default(),
                lazy_layout: LazyLayoutState::default(),
                caret_affinity: CaretAffinity::default(),
                pending_announcements: Vec::new(),
                tracking_changes: false,
                review_author: "You".to_string(),
                review_date: String::new(),
            }
        }

        let mut para = engine::Paragraph {
            text: "item".into(),
            ..Default::default()
        };
        para.resolved_list_indent = Some(engine::Indent {
            start_twips: 720,
            end_twips: 0,
            first_line_twips: 0,
            hanging_twips: 360,
        });
        let path = BridgeBlockPath::top(0);
        let e = test_engine(para.clone());
        let ind = e.indent_for_caret(&path);
        assert_eq!(ind.start_pt, 36.0);
        assert_eq!(ind.end_pt, 0.0);
        assert_eq!(
            ind.first_line_pt, -18.0,
            "a level hanging indent reads back as a negative first_line_pt"
        );

        /* a partial direct end-indent (a ruler end-drag writes {0, E, 0, 0})
        must not strip the level's start indent + hanging gutter from the
        read-back — the same per-slot merge invariant `effective_layout_indents`
        protects (issue #50). */
        para.props.indent = engine::Indent {
            end_twips: 720,
            ..Default::default()
        };
        let e = test_engine(para);
        let ind = e.indent_for_caret(&path);
        assert_eq!(
            ind.start_pt, 36.0,
            "level start must survive a partial direct end-indent"
        );
        assert_eq!(ind.end_pt, 36.0);
        assert_eq!(
            ind.first_line_pt, -18.0,
            "level hanging must survive a partial direct end-indent"
        );
    }

    /// Issue #50 — the layout-cache key must see the marker text and the
    /// stamped level indent: a cross-paragraph renumber ("1." -> "2.")
    /// changes painted output without touching text or props.
    #[test]
    fn paragraph_layout_key_sees_marker_and_list_indent() {
        let cfg = autofit_test_cfg();
        let mut para = engine::Paragraph {
            text: "item".into(),
            ..Default::default()
        };
        let k_plain = paragraph_layout_key(&para, &cfg, 1.0, 300.0, empty_sctx());
        para.resolved_marker = Some("1.".into());
        let k1 = paragraph_layout_key(&para, &cfg, 1.0, 300.0, empty_sctx());
        para.resolved_marker = Some("2.".into());
        let k2 = paragraph_layout_key(&para, &cfg, 1.0, 300.0, empty_sctx());
        para.resolved_list_indent = Some(engine::Indent {
            start_twips: 720,
            end_twips: 0,
            first_line_twips: 0,
            hanging_twips: 360,
        });
        let k3 = paragraph_layout_key(&para, &cfg, 1.0, 300.0, empty_sctx());
        assert_ne!(k_plain, k1);
        assert_ne!(k1, k2, "renumbered marker must miss the cache");
        assert_ne!(k2, k3, "level indent must be part of the key");
    }

    /// Issue #37 — the ApplyFormatting caps patch reaches layout: applying
    /// `caps` over an existing range materializes `caps_transform` spans
    /// AND moves the paragraph off its old layout-cache key. The reported
    /// interactive symptom traced to conditions outside the code path
    /// (collapsed-caret pending arm / stale wasm artifact); this test pins
    /// the seam so the chain stays verified.
    #[test]
    fn caps_patch_reaches_layout_spans_and_cache_key() {
        let cfg = autofit_test_cfg();
        let para = engine::Paragraph {
            text: "hello".into(),
            ..Default::default()
        };
        let k_before = paragraph_layout_key(&para, &cfg, 1.0, 300.0, empty_sctx());
        let before = build_style_spans(&para, empty_sctx(), 12.0, [0, 0, 0, 255], 1.0);
        assert!(before.iter().all(|s| !s.caps_transform));
        let patch = engine::SpanStyle {
            caps: Some(true),
            ..Default::default()
        };
        let capped = para.apply_style(0, 5, patch);
        let spans = build_style_spans(&capped, empty_sctx(), 12.0, [0, 0, 0, 255], 1.0);
        assert!(
            !spans.is_empty() && spans.iter().any(|s| s.caps_transform),
            "caps must materialize as caps_transform spans"
        );
        let k_after = paragraph_layout_key(&capped, &cfg, 1.0, 300.0, empty_sctx());
        assert_ne!(k_before, k_after, "caps flip must miss the layout cache");
    }

    /// Issue #59/#60 live-QA seam — a paragraph with no other formatting
    /// (the common "plain hyperlink" case: `build_style_spans` still
    /// contributes exactly one gap-fill span covering the whole
    /// paragraph) must still get its hyperlink range split out and
    /// overlaid with the blue/underline style. Pins the exact rendering
    /// path `read_docx`-loaded hyperlinks go through, verified live in a
    /// real Chrome session against the #59 fixture.
    #[test]
    fn plain_hyperlink_still_renders_blue_underline_span() {
        let mut para = engine::Paragraph {
            text: "Visit our website for details".into(),
            ..Default::default()
        };
        para.hyperlinks.push(engine::Hyperlink {
            start: 10,
            end: 17,
            target: "https://example.com".to_string(),
        });

        let base_spans = build_style_spans(&para, empty_sctx(), 24.0, [0, 0, 0, 255], 1.0);
        assert_eq!(
            base_spans.len(),
            1,
            "an unstyled paragraph still gets one gap-fill span for apply_hyperlink_overlay to split"
        );
        let overlaid = apply_hyperlink_overlay(base_spans, &para.hyperlinks, [0, 0, 0, 255]);
        let linked_span = overlaid
            .iter()
            .find(|s| s.start == 10 && s.end == 17)
            .expect("must have a span exactly covering the hyperlink range");
        assert_eq!(linked_span.underline, engine::UnderlineStyle::Single);
        assert_eq!(linked_span.color, HYPERLINK_BLUE);
    }

    fn autofit_test_cfg() -> RenderConfig {
        RenderConfig {
            font_id: "test-latin".to_string(),
            base_direction: ShapingDirection::Ltr,
            px_size: 12.0,
            line_height: 16.0,
            alignment: Alignment::Start,
            scale: 1.0,
            base_scale: 1.0,
            zoom: 1.0,
        }
    }

    fn cell_with_text(text: &str) -> engine::TableCell {
        engine::TableCell {
            props: engine::CellProperties::default(),
            blocks: vec![engine::Block::Paragraph(engine::Paragraph {
                text: text.to_string(),
                spans: Vec::new(),
                props: engine::ParaProperties::default(),
                list_item: None,
                resolved_marker: None,
                resolved_list_indent: None,
                dirty: false,
                source_xml: None,
                inline_objects: Vec::new(),
                hyperlinks: Vec::new(),
                revisions: Vec::new(),
                fields: Vec::new(),
                style_id: None,
                direct_overrides: engine::ParaProperties::default(),
                section_end: None,
            })],
        }
    }

    fn one_row_table(cells: Vec<engine::TableCell>) -> engine::Table {
        engine::Table {
            grid: Vec::new(),
            props: engine::TableProperties::default(),
            rows: vec![engine::TableRow {
                props: engine::RowProperties::default(),
                cells,
            }],
            dirty: true,
            source_xml: None,
        }
    }

    /// L2.2 (#7) — a single cell containing an unbreakable long word
    /// (no `/`, `-`, or other ICU break opportunities) must keep the
    /// column at least as wide as the word's rendered pixel width even
    /// when that exceeds the available band; the table overflows
    /// horizontally rather than wrapping the token mid-char.
    #[test]
    fn autofit_one_cell_long_url_floors_at_min_content() {
        let fonts = test_font_stack();
        let cfg = autofit_test_cfg();
        /* Pure-letter run — ICU `LineSegmenter::new_auto` reports no
        break opportunity inside contiguous letters, so this is the
        true min_content case the issue describes. */
        let token: String = "a".repeat(80);
        let table = one_row_table(vec![cell_with_text(&token)]);

        /* Narrow viewport — well under the token's pixel width. */
        let available = 80.0_f32;
        let mut cache = new_layout_cache();
        let widths = autofit_distribute(
            &table,
            available,
            &fonts,
            &cfg,
            1.0,
            &[],
            empty_sctx(),
            cache.get_mut(),
        );

        assert_eq!(widths.len(), 1);
        let (cell_min, _cell_max) = measure_unbreakable_width(
            &table.rows[0].cells[0].blocks,
            empty_sctx(),
            &fonts,
            &cfg,
            1.0,
            cache.get_mut(),
        );
        assert!(
            cell_min > available,
            "test fixture sanity: the long token's min_content ({cell_min}) \
             must exceed the available viewport ({available}); raise the token \
             length or shrink the viewport"
        );
        assert!(
            widths[0] >= cell_min - 0.5,
            "column floor must be at least cell min_content ({cell_min}); got {}",
            widths[0]
        );
        assert!(
            widths[0] > available,
            "narrow viewport must allow horizontal overflow rather than clip; \
             available={available}, got width={}",
            widths[0]
        );
    }

    /// L2.2 (#7) — a 2-col table with an unbreakable long token in
    /// column 0 and normal prose in column 1 must pin column 0 to the
    /// token's min_content; column 1 absorbs the remaining width.
    #[test]
    fn autofit_two_col_url_plus_prose_floors_url_column() {
        let fonts = test_font_stack();
        let cfg = autofit_test_cfg();
        let token: String = "a".repeat(40);
        let table = one_row_table(vec![
            cell_with_text(&token),
            cell_with_text("normal short prose"),
        ]);

        /* Wide-enough viewport that the token's min_content alone fits
        plus some room for column 1. */
        let available = 600.0_f32;
        let mut cache = new_layout_cache();
        let widths = autofit_distribute(
            &table,
            available,
            &fonts,
            &cfg,
            1.0,
            &[],
            empty_sctx(),
            cache.get_mut(),
        );

        assert_eq!(widths.len(), 2);
        let (col0_min, _) = measure_unbreakable_width(
            &table.rows[0].cells[0].blocks,
            empty_sctx(),
            &fonts,
            &cfg,
            1.0,
            cache.get_mut(),
        );
        assert!(
            widths[0] >= col0_min - 0.5,
            "col0 must respect token min_content {col0_min}; got {}",
            widths[0]
        );
        /* Column 1 must take what's left. Floor on col1 is its own
        min_content (small) — it should expand to absorb the slack. */
        let sum = widths[0] + widths[1];
        assert!(
            (sum - available).abs() < 1.0,
            "two-col layout fills available width; got sum={sum}, available={available}"
        );
    }

    /* ---------- L3.1 (#5) predictive lazy_runway ---------- */

    /// L3.1 (#5) — `lazy_runway` returns the larger of `LAYOUT_BUFFER_PT`
    /// and `viewport_h × scale × 2`. Pins the four observable scenarios
    /// from the design table.
    #[test]
    fn lazy_runway_picks_doubled_viewport_or_legacy_floor() {
        /* Headless cold open: default viewport_h = 1684 pt (two A4
        pages). Doubled = 3368, exceeds the 1200 floor. */
        let cold_open = lazy_runway(1684.0, 1.0);
        assert!(
            (cold_open - 3368.0).abs() < 0.01,
            "cold open: expected 3368, got {cold_open}"
        );

        /* A4 desktop viewport: 842 pt × 1.0 × 2 = 1684; exceeds floor. */
        let a4_desktop = lazy_runway(842.0, 1.0);
        assert!(
            (a4_desktop - 1684.0).abs() < 0.01,
            "A4 desktop: expected 1684, got {a4_desktop}"
        );

        /* A4 hi-DPI (Retina): 842 × 2.0 × 2 = 3368; exceeds floor. */
        let a4_retina = lazy_runway(842.0, 2.0);
        assert!(
            (a4_retina - 3368.0).abs() < 0.01,
            "A4 Retina: expected 3368, got {a4_retina}"
        );

        /* Mobile narrow viewport: 400 × 1.0 × 2 = 800; under the 1200
        floor → floor wins. */
        let mobile = lazy_runway(400.0, 1.0);
        assert!(
            (mobile - LAYOUT_BUFFER_PT).abs() < 0.01,
            "mobile small viewport: expected LAYOUT_BUFFER_PT={LAYOUT_BUFFER_PT}, got {mobile}"
        );

        /* Pre-viewport (TS shell hasn't posted a SetViewport yet but
        the engine seeded `viewport_h = INITIAL_COLD_OPEN_BUDGET_PT`).
        Doubled = 3368 — same as cold open. */
        let pre_viewport = lazy_runway(INITIAL_COLD_OPEN_BUDGET_PT, 1.0);
        assert!(
            pre_viewport >= LAYOUT_BUFFER_PT,
            "pre-viewport must never regress below LAYOUT_BUFFER_PT floor"
        );
        assert!(
            (pre_viewport - 3368.0).abs() < 0.01,
            "pre-viewport: expected 3368, got {pre_viewport}"
        );
    }

    /// L2.2 (#7) — short-text cells whose min_content is far below the
    /// probe width must see the existing scale-to-fit behaviour. Pins
    /// the "no regression on the common case" invariant; this guard
    /// protects the visual-diff farm from any drift introduced by the
    /// new floor.
    #[test]
    fn autofit_short_text_unchanged_by_min_content_floor() {
        let fonts = test_font_stack();
        let cfg = autofit_test_cfg();
        let table = one_row_table(vec![
            cell_with_text("Name"),
            cell_with_text("Age"),
            cell_with_text("City"),
        ]);
        let available = 600.0_f32;
        let mut cache = new_layout_cache();
        let widths = autofit_distribute(
            &table,
            available,
            &fonts,
            &cfg,
            1.0,
            &[],
            empty_sctx(),
            cache.get_mut(),
        );

        assert_eq!(widths.len(), 3);
        let sum: f32 = widths.iter().sum();
        assert!(
            (sum - available).abs() < 1.0,
            "short-text autofit fills available width; sum={sum}, available={available}"
        );
        /* No column compressed below its short-word min_content — that
        is the trivial floor case (column already >= floor). */
        for &w in &widths {
            assert!(w > 30.0, "every column expands above MIN_COL_WIDTH_PT");
        }
    }

    /// Issue #51 — the viewport cull must actually stop the paginator on a
    /// long single-section document. Before the fix, the budget check only
    /// counted pages flushed at SECTION boundaries plus the within-page
    /// cursor, so a single-section 116-page doc laid out in full on every
    /// "lazy" paint (and the shell then mounted 100+ page canvases).
    #[test]
    fn viewport_cull_stops_single_section_layout() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/perf/50p.docx");
        let bytes = std::fs::read(path).expect("read 50p.docx fixture");
        let archive = format_docx::read_docx(&bytes).expect("parse 50p.docx");
        let engine = test_engine_with_doc(archive.document);

        /* A ~2-page band must NOT lay out the whole document. The runway
        slack admits a few pages past the target, but two dozen means the
        cull is broken again. */
        let (pages, _, _, info) = engine
            .build_pages(2.0, false, Some(2000.0))
            .expect("band layout");
        assert!(
            !info.is_full_layout,
            "a 2000px band over a 116-page doc must report a culled layout"
        );
        assert!(
            pages.len() < 24,
            "band layout leaked to {} pages — cull not firing",
            pages.len()
        );
        assert!(
            info.remaining_blocks > 0,
            "culled layout must report unprocessed blocks for the scrollbar estimate"
        );

        /* target_y = None stays the historical lay-everything contract
        (PDF export, explicit full expands). */
        let (all_pages, _, _, full_info) =
            engine.build_pages(2.0, false, None).expect("full layout");
        assert!(full_info.is_full_layout);
        assert!(
            all_pages.len() > 100,
            "full layout of the 50p fixture spans 100+ pages, got {}",
            all_pages.len()
        );
    }

    /// Issue #51 — opening a document must not inherit the previous
    /// document's lazy-layout high-water mark (or its composition /
    /// caret state): a deep scroll in doc A would otherwise force doc
    /// B's first paint to lay out from page 1 to A's scroll depth.
    #[test]
    fn load_docx_resets_per_document_state() {
        let doc = DocumentTree::from_text("fresh document body");
        let bytes = build_minimal_docx(&doc).expect("serialize fixture");

        let mut engine = test_engine_with_doc(DocumentTree::from_text("old doc"));
        engine.lazy_layout.min_target_y = 250_000.0;
        engine.lazy_layout.viewport_y = 240_000.0;
        engine.lazy_layout.viewport_h = 900.0;
        engine.composition = Some(CompositionState {
            at: bpos_top(0, 0),
            text: "stale ime preview".to_string(),
        });

        let evt = engine.load_docx_bytes(&bytes, "test");
        assert!(
            matches!(evt, Event::DocumentLoaded { .. }),
            "load must succeed, got {evt:?}"
        );
        assert!(engine.composition.is_none(), "stale composition must drop");
        assert_eq!(
            engine.lazy_layout.viewport_y, 0.0,
            "band restarts at the document top"
        );
        assert_eq!(
            engine.lazy_layout.viewport_h, 900.0,
            "viewport height is a canvas property and survives the swap"
        );
        let expected = INITIAL_COLD_OPEN_BUDGET_PT.max(900.0);
        assert!(
            (engine.lazy_layout.min_target_y - expected).abs() < 0.5,
            "min_target_y restarts at the cold-open band, got {}",
            engine.lazy_layout.min_target_y
        );
    }

    /// Issue #53 — the interactive insert resolution ladder: explicit
    /// `at` > live caret > legacy append (None).
    #[test]
    fn insert_at_resolution_prefers_live_caret_over_none() {
        let mut engine = test_engine_with_doc(DocumentTree::from_text("hello world"));
        /* Live caret at (0,5) — a click just landed there. */
        engine.selection = Some(SelectionState {
            anchor: bpos_top(0, 5),
            caret: bpos_top(0, 5),
            ideal_x: None,
            kind: SelectionKind::Linear,
        });
        assert_eq!(
            engine.resolve_interactive_insert_at(None),
            Some(bpos_top(0, 5)),
            "None must resolve to the engine's live caret"
        );
        assert_eq!(
            engine.resolve_interactive_insert_at(Some(bpos_top(0, 2))),
            Some(bpos_top(0, 2)),
            "an explicit at is honoured verbatim"
        );
        /* Interactive insert through the resolved caret really lands
        there and advances the engine caret. */
        let evt = engine.do_insert_text_interactive(bpos_top(0, 5), "X".to_string());
        assert!(matches!(evt, Event::SelectionChanged { .. }));
        assert_eq!(
            engine.undo.current().paragraph_text(0),
            Some("helloX world")
        );

        /* No selection at all → the Phase-1 append contract survives. */
        engine.selection = None;
        assert_eq!(engine.resolve_interactive_insert_at(None), None);
    }

    /// Issue #53 — `PlaceCaretAtPoint` = hit-test + collapsed selection
    /// in one command, so the shell can post it synchronously at
    /// pointerdown and rely on queue order alone.
    #[test]
    fn place_caret_at_point_sets_collapsed_selection() {
        let mut engine = test_engine_with_doc(DocumentTree::from_text("hello wide world"));
        let evt = engine.do_place_caret_at_point(
            0,
            BridgePoint {
                /* Well inside the first line's text band. */
                x: engine
                    .undo
                    .current()
                    .paragraph_text(0)
                    .map_or(0.0, |_| 300.0),
                y: 130.0,
            },
        );
        let Event::SelectionChanged { range, .. } = evt else {
            panic!("expected SelectionChanged, got {evt:?}");
        };
        assert_eq!(range.start, range.end, "placement collapses the selection");
        let sel = engine.selection.as_ref().expect("selection installed");
        assert_eq!(sel.anchor, sel.caret);
        assert_eq!(sel.caret, range.start, "engine caret matches the reply");
        assert!(
            sel.caret.offset > 0,
            "a mid-line x must not land at offset 0 (hit-test really ran)"
        );
    }

    /// Issue #51/#34 — table cell paragraphs must ride the layout LRU.
    /// Before the fix, `layout_cell_blocks` called `layout_paragraph`
    /// raw, so every paint re-shaped every cell (×3 with autofit's
    /// probe passes). Two identical builds must produce identical
    /// geometry with zero new cache entries on the second run.
    #[test]
    fn table_cells_ride_the_layout_cache() {
        let mut doc = DocumentTree::from_text("intro paragraph");
        doc.blocks
            .push_back(engine::Block::Table(one_row_table(vec![
                cell_with_text("alpha beta gamma"),
                cell_with_text("delta"),
                cell_with_text("epsilon zeta eta theta"),
            ])));
        let engine = test_engine_with_doc(doc);

        let (p1, _, _, _) = engine
            .build_pages(2.0, false, None)
            .expect("first table build");
        let after_first = engine.layout_cache.borrow().len();
        assert!(
            after_first > 3,
            "cell paragraphs must populate the cache (got {after_first} entries)"
        );
        engine.invalidate_layout_snapshot();
        let (p2, _, _, _) = engine
            .build_pages(2.0, false, None)
            .expect("second table build");
        let after_second = engine.layout_cache.borrow().len();
        assert_eq!(
            after_first, after_second,
            "an unchanged doc must not mint new cache entries on repaint"
        );
        assert_eq!(p1.len(), p2.len());
        assert!(
            (p1[0].size.height - p2[0].size.height).abs() < 0.01,
            "cache-hit geometry must be identical"
        );
    }

    /// Issue #42 — `Command::ChangeListLevel`'s handler demotes the
    /// caret's list item and the `SelectionChanged` reply reads back the
    /// new `list_ilvl` — the exact field the shell's Tab handler branches
    /// on to decide demote/promote vs. tab-char insertion.
    #[test]
    fn change_list_level_dispatch_reads_back_new_ilvl() {
        let doc = DocumentTree::from_text("alpha").toggle_list_on_range(
            engine::LogicalPos::new(engine::BlockPath::top(0), 0),
            engine::LogicalPos::new(engine::BlockPath::top(0), 0),
            engine::numbering::ListSynthesisKind::Bullet,
        );
        let mut engine = test_engine_with_doc(doc);
        let range = BridgeLogicalRange {
            start: bpos_top(0, 0),
            end: bpos_top(0, 0),
        };
        let evt = engine.do_change_list_level(range, 1);
        let Event::SelectionChanged { list_ilvl, .. } = evt else {
            panic!("expected SelectionChanged, got {evt:?}");
        };
        assert_eq!(list_ilvl, Some(1), "reply must read back the demoted level");
    }

    /// Issue #42 — dispatching `ChangeListLevel` on a non-list paragraph
    /// is a no-op end-to-end: `list_ilvl` reads back `None`, matching the
    /// shell's decision to fall through to tab-char insertion instead.
    #[test]
    fn change_list_level_dispatch_noop_reads_back_none() {
        let mut engine = test_engine_with_doc(DocumentTree::from_text("alpha"));
        let range = BridgeLogicalRange {
            start: bpos_top(0, 0),
            end: bpos_top(0, 0),
        };
        let evt = engine.do_change_list_level(range, 1);
        let Event::SelectionChanged { list_ilvl, .. } = evt else {
            panic!("expected SelectionChanged, got {evt:?}");
        };
        assert_eq!(list_ilvl, None);
    }

    /// Issue #38 — `SelectionChanged.undo_depth` increases on a real edit
    /// (something the sidebar must refetch for) but stays put across a
    /// pure caret placement (navigation the sidebar must NOT refetch on).
    /// This is the exact invariant `TrackChangesSidebar`'s reactive
    /// refetch depends on.
    #[test]
    fn undo_depth_changes_on_edit_not_on_caret_placement() {
        let mut engine = test_engine_with_doc(DocumentTree::from_text("hello world"));
        let before = engine.undo.depth();
        let evt = engine.do_place_caret_at_point(0, BridgePoint { x: 300.0, y: 130.0 });
        let Event::SelectionChanged { undo_depth, .. } = evt else {
            panic!("expected SelectionChanged, got {evt:?}");
        };
        assert_eq!(
            undo_depth, before,
            "a plain caret placement must not push a new undo snapshot"
        );
        let evt = engine.do_insert_text_interactive(bpos_top(0, 0), "X".to_string());
        let Event::SelectionChanged { undo_depth, .. } = evt else {
            panic!("expected SelectionChanged, got {evt:?}");
        };
        assert_eq!(
            undo_depth,
            before + 1,
            "an interactive edit must push exactly one new snapshot"
        );
    }

    /// Issue #44 — `image_geometry()` surfaces one rect per inline image,
    /// addressed by `(path, at)` at the sentinel byte offset, with a
    /// non-degenerate size and the correct `rel_id`. This is the geometry
    /// the resize-handle overlay hit-tests and positions against.
    #[test]
    fn image_geometry_exposes_inline_image_rects() {
        let mut doc = DocumentTree::from_text("a\u{FFFC}b");
        if let Some(p) = doc.blocks[0].as_paragraph_mut() {
            p.inline_objects.push(engine::InlineObject {
                at: 1,
                kind: engine::InlineKind::Image {
                    rel_id: "nge_img_1".to_string(),
                    width_emu: 914_400,  // 1 inch
                    height_emu: 457_200, // 0.5 inch
                },
            });
        }
        let engine = test_engine_with_doc(doc);
        let rects = engine.image_geometry().expect("image geometry");
        assert_eq!(rects.len(), 1, "exactly one image rect");
        let img = &rects[0];
        assert_eq!(img.at, 1, "addressed at the sentinel byte offset");
        assert_eq!(img.rel_id, "nge_img_1");
        assert_eq!(
            img.path.steps,
            vec![BridgePathStep::Block { idx: 0 }],
            "top-level paragraph path"
        );
        assert!(
            img.rect.w > 0.0 && img.rect.h > 0.0,
            "image rect must have a real size: {:?}",
            img.rect
        );
        /* The reserved box is wider than it is tall (2:1 EMU), so the
        laid-out rect should preserve that aspect ordering. */
        assert!(
            img.rect.w > img.rect.h,
            "1in × 0.5in image must lay out wider than tall: {:?}",
            img.rect
        );
        assert_eq!(
            (img.width_emu, img.height_emu),
            (914_400, 457_200),
            "the EMU extent must ride along for the shell's resize ratio"
        );
    }

    /// Issue #44 — `Command::ResizeImage` dispatch overwrites the extent
    /// and the follow-up `GetImageRects` reflects the larger box.
    #[test]
    fn resize_image_dispatch_grows_the_geometry() {
        let mut doc = DocumentTree::from_text("\u{FFFC}");
        if let Some(p) = doc.blocks[0].as_paragraph_mut() {
            p.inline_objects.push(engine::InlineObject {
                at: 0,
                kind: engine::InlineKind::Image {
                    rel_id: "nge_img_1".to_string(),
                    width_emu: 457_200,
                    height_emu: 457_200,
                },
            });
        }
        let mut engine = test_engine_with_doc(doc);
        let before = engine.image_geometry().expect("geom")[0].rect.w;
        let evt = engine.do_resize_image(BridgeBlockPath::top(0), 0, 1_828_800, 1_828_800);
        assert!(matches!(evt, Event::SelectionChanged { .. }));
        let after = engine.image_geometry().expect("geom")[0].rect.w;
        assert!(
            after > before * 3.0,
            "quadrupling the EMU width must widen the laid-out rect (before {before}, after {after})"
        );
    }

    #[test]
    fn insert_section_break_dispatch_reports_two_sections() {
        /* Phase 3 (#40) — end-to-end through the command handler: the
        break lands, the caret moves into the second section, and
        SelectionChanged's section_geometry carries index 1 of 2. */
        let doc = DocumentTree::from_text("hello world");
        let mut engine = test_engine_with_doc(doc);
        let evt = engine.do_insert_section_break(bpos_top(0, 5), SectionBreakKind::NextPage);
        let Event::SelectionChanged {
            caret: _,
            section_geometry: Some(geom),
            ..
        } = evt
        else {
            panic!("expected SelectionChanged with section_geometry, got {evt:?}");
        };
        assert_eq!(geom.section_count, 2);
        assert_eq!(geom.section_index, 1, "caret starts the second section");
        assert_eq!(engine.undo.current().effective_sections().len(), 2);
        /* One undo entry reverts the whole break. */
        assert!(engine.undo.can_undo());
        let _ = engine.undo.undo();
        assert_eq!(engine.undo.current().effective_sections().len(), 1);
    }

    #[test]
    fn insert_section_break_in_cell_is_an_error() {
        let mut doc = DocumentTree::from_text("body");
        doc.blocks.push_back(engine::Block::Table(engine::Table {
            grid: vec![2000],
            props: engine::TableProperties::default(),
            rows: vec![engine::TableRow {
                props: engine::RowProperties::default(),
                cells: vec![engine::TableCell {
                    props: engine::CellProperties::default(),
                    blocks: vec![engine::Block::Paragraph(engine::Paragraph {
                        text: "cell".into(),
                        ..Default::default()
                    })],
                }],
            }],
            dirty: true,
            source_xml: None,
        }));
        let mut engine = test_engine_with_doc(doc);
        let cell_path = BridgeBlockPath {
            steps: vec![
                BridgePathStep::Block { idx: 1 },
                BridgePathStep::Cell { row: 0, col: 0 },
                BridgePathStep::Block { idx: 0 },
            ],
        };
        let evt = engine.do_insert_section_break(
            BridgeLogicalPos {
                path: cell_path,
                offset: 0,
            },
            SectionBreakKind::Continuous,
        );
        assert!(matches!(evt, Event::Error { .. }));
        assert_eq!(engine.undo.current().effective_sections().len(), 1);
    }

    /* ================================================================
    Phase 3 (#39) — header/footer story editing.
    ================================================================ */

    #[test]
    fn enter_header_materializes_a_part_and_reroots_typing() {
        let mut engine = test_engine_with_doc(DocumentTree::from_text("body text"));
        let evt = engine.do_enter_header_footer(0, bridge::HeaderFooterArea::Header);
        let Event::SelectionChanged {
            editing_story: Some(story),
            ..
        } = &evt
        else {
            panic!("expected story-mode SelectionChanged, got {evt:?}");
        };
        assert!(matches!(story.area, bridge::HeaderFooterArea::Header));
        assert_eq!(story.rid, "ngeHf1");
        let doc = engine.undo.current();
        assert!(doc.headers.contains_key("ngeHf1"), "part materialized");
        assert!(doc.hf_dirty.headers.contains("ngeHf1"), "part marked dirty");
        assert_eq!(
            doc.effective_sections()[0].header_refs.default.as_deref(),
            Some("ngeHf1"),
            "section now references the fresh part"
        );
        /* Typing lands in the STORY, not the body. */
        let evt = engine.do_insert_text_interactive(bpos_top(0, 0), "Header A".into());
        assert!(matches!(evt, Event::SelectionChanged { .. }));
        let doc = engine.undo.current();
        assert_eq!(doc.headers["ngeHf1"][0].text, "Header A");
        assert_eq!(
            doc.paragraph_text(0),
            Some("body text"),
            "body untouched by story typing"
        );
        /* Exit restores body editing. */
        let evt = engine.do_exit_header_footer();
        let Event::SelectionChanged { editing_story, .. } = &evt else {
            panic!("expected SelectionChanged");
        };
        assert!(editing_story.is_none());
    }

    #[test]
    fn header_a_header_b_epic_scenario() {
        /* The Phase 3 validation core: section break → Header A on
        page 1, Header B on page 2, independent parts. */
        let mut engine = test_engine_with_doc(DocumentTree::from_text("hello world"));
        let evt = engine.do_insert_section_break(bpos_top(0, 5), SectionBreakKind::NextPage);
        assert!(matches!(evt, Event::SelectionChanged { .. }));

        let evt = engine.do_enter_header_footer(0, bridge::HeaderFooterArea::Header);
        assert!(matches!(evt, Event::SelectionChanged { .. }));
        engine.do_insert_text_interactive(bpos_top(0, 0), "Header A".into());
        engine.do_exit_header_footer();

        let evt = engine.do_enter_header_footer(1, bridge::HeaderFooterArea::Header);
        let Event::SelectionChanged {
            editing_story: Some(story),
            ..
        } = &evt
        else {
            panic!("expected story mode on page 1, got {evt:?}");
        };
        assert_ne!(story.rid, "ngeHf1", "section 2 owns a DIFFERENT part");
        let rid_b = story.rid.clone();
        engine.do_insert_text_interactive(bpos_top(0, 0), "Header B".into());
        engine.do_exit_header_footer();

        let doc = engine.undo.current();
        assert_eq!(doc.headers["ngeHf1"][0].text, "Header A");
        assert_eq!(doc.headers[&rid_b][0].text, "Header B");
        let sections = doc.effective_sections();
        assert_eq!(sections.len(), 2);
        assert_eq!(
            sections[0].header_refs.default.as_deref(),
            Some("ngeHf1"),
            "section 1 → Header A's part"
        );
        assert_eq!(
            sections[1].header_refs.default.as_deref(),
            Some(rid_b.as_str()),
            "section 2 → Header B's part"
        );
        /* And the laid-out pages carry DIFFERENT header bands. */
        engine
            .ensure_layout_snapshot(engine.scale(), false, None)
            .expect("layout");
        let snap = engine.layout_snapshot.borrow();
        let pages = &snap.as_ref().unwrap().pages;
        assert!(pages.len() >= 2, "next-page break yields two pages");
        assert!(pages[0].header.is_some(), "page 1 paints a header band");
        assert!(pages[1].header.is_some(), "page 2 paints a header band");
    }

    #[test]
    fn entering_a_shared_header_forks_it() {
        /* Header FIRST, then break (which copies refs) → both sections
        share one part. Entering page 2's header must fork a private
        copy seeded with the shared content, leaving section 1 alone. */
        let mut engine = test_engine_with_doc(DocumentTree::from_text("hello world"));
        engine.do_enter_header_footer(0, bridge::HeaderFooterArea::Header);
        engine.do_insert_text_interactive(bpos_top(0, 0), "Shared".into());
        engine.do_exit_header_footer();
        engine.do_insert_section_break(bpos_top(0, 5), SectionBreakKind::NextPage);

        let evt = engine.do_enter_header_footer(1, bridge::HeaderFooterArea::Header);
        let Event::SelectionChanged {
            editing_story: Some(story),
            ..
        } = &evt
        else {
            panic!("expected story mode, got {evt:?}");
        };
        assert_ne!(story.rid, "ngeHf1", "shared part must fork on enter");
        let doc = engine.undo.current();
        assert_eq!(
            doc.headers[&story.rid][0].text, "Shared",
            "fork seeds from the shared content"
        );
        /* Edit the fork (story caret parked at the band start by
        enter — move it to the end first); the original stays. */
        engine.selection = Some(SelectionState {
            anchor: bpos_top(0, 6),
            caret: bpos_top(0, 6),
            ideal_x: None,
            kind: SelectionKind::Linear,
        });
        engine.do_insert_text_interactive(bpos_top(0, 6), " B".into());
        let doc = engine.undo.current();
        assert_eq!(doc.headers[&story.rid][0].text, "Shared B");
        assert_eq!(doc.headers["ngeHf1"][0].text, "Shared");
    }

    #[test]
    fn story_undo_past_materialize_falls_back_to_body() {
        let mut engine = test_engine_with_doc(DocumentTree::from_text("body"));
        engine.do_enter_header_footer(0, bridge::HeaderFooterArea::Header);
        assert!(engine.story_active());
        /* One undo reverts the materialize push; the story's part is
        gone and the guard must exit story mode. */
        let evt = engine.do_undo();
        assert!(!engine.story_active(), "guard exited the vanished story");
        assert!(matches!(evt, Event::SelectionChanged { .. }));
        assert!(engine.undo.current().headers.is_empty());
    }

    #[test]
    fn story_firewall_rejects_non_story_commands() {
        let mut engine = test_engine_with_doc(DocumentTree::from_text("body"));
        engine.do_enter_header_footer(0, bridge::HeaderFooterArea::Footer);
        let rejected = engine.story_gate(&Command::InsertPageBreak { at: bpos_top(0, 0) });
        assert!(matches!(rejected, Some(Event::Error { .. })));
        let allowed = engine.story_gate(&Command::InsertText {
            at: None,
            text: "x".into(),
        });
        assert!(allowed.is_none());
        engine.do_exit_header_footer();
        let body_mode = engine.story_gate(&Command::InsertPageBreak { at: bpos_top(0, 0) });
        assert!(body_mode.is_none(), "firewall inert in body mode");
    }

    #[test]
    fn story_enter_exit_restores_body_selection() {
        let mut engine = test_engine_with_doc(DocumentTree::from_text("body text"));
        engine.selection = Some(SelectionState {
            anchor: bpos_top(0, 2),
            caret: bpos_top(0, 7),
            ideal_x: None,
            kind: SelectionKind::Linear,
        });
        engine.do_enter_header_footer(0, bridge::HeaderFooterArea::Header);
        let sel = engine.selection.clone().unwrap();
        assert_eq!(sel.caret.offset, 0, "story caret starts at band origin");
        engine.do_exit_header_footer();
        let sel = engine.selection.clone().unwrap();
        assert_eq!((sel.anchor.offset, sel.caret.offset), (2, 7));
    }

    /// Shared scaffold: a native Engine over `doc` with a real Latin font
    /// and a cached layout config, mirroring the interactive boot state.
    fn test_engine_with_doc(doc: DocumentTree) -> Engine {
        let bytes_font = include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf").to_vec();
        let font =
            LoadedFont::parse("test-latin".to_string(), bytes_font).expect("parse test font");
        let mut fonts: HashMap<String, Arc<LoadedFont>> = HashMap::new();
        fonts.insert("test-latin".to_string(), Arc::new(font));
        Engine {
            page_ctxs: Vec::new(),
            ctx: None,
            fonts,
            undo: UndoStack::new(doc, 100),
            layout_cfg: Some(RenderConfig {
                font_id: "test-latin".to_string(),
                base_direction: ShapingDirection::Ltr,
                px_size: 16.0,
                line_height: 26.0,
                alignment: Alignment::Start,
                scale: 2.0,
                base_scale: 2.0,
                zoom: 1.0,
            }),
            atlas: GlyphAtlas::new(),
            vello: None,
            dirty: DirtyTracker::new(),
            selection: Some(SelectionState {
                anchor: bpos_top(0, 0),
                caret: bpos_top(0, 0),
                ideal_x: None,
                kind: SelectionKind::Linear,
            }),
            composition: None,
            pending_format: None,
            layout_cache: new_layout_cache(),
            layout_snapshot: RefCell::new(None),
            sections_memo: RefCell::new(None),
            active_story: StoryTarget::Body,
            stashed_body_selection: None,
            a11y_cache: None,
            image_cache: HashMap::new(),
            last_paint_dims: LastPaintDims::default(),
            lazy_layout: LazyLayoutState::default(),
            caret_affinity: CaretAffinity::default(),
            pending_announcements: Vec::new(),
            tracking_changes: false,
            review_author: "You".to_string(),
            /* Non-empty so `current_review_date` never reaches
            `js_sys::Date`, which panics on native targets. */
            review_date: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    /// Manual profiling harness for the #51/#34 perf overhaul. Run with:
    /// `cargo test -p engine-wasm --release profile_50p -- --ignored --nocapture`
    #[test]
    #[ignore = "manual profiling harness, not a correctness gate"]
    fn profile_50p_layout() {
        use std::time::Instant;
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/perf/50p.docx");
        let bytes = std::fs::read(path).expect("read 50p.docx fixture");

        let t0 = Instant::now();
        let archive = format_docx::read_docx(&bytes).expect("parse 50p.docx");
        eprintln!("read_docx:                {:?}", t0.elapsed());
        eprintln!(
            "blocks: {}  paragraphs: {}",
            archive.document.blocks.len(),
            archive.document.paragraph_count()
        );

        let mut engine = test_engine_with_doc(archive.document);

        let doc = engine.undo.current().clone();
        let t = Instant::now();
        let sctx = StyleContext::of(&doc);
        eprintln!("StyleContext::of:         {:?}", t.elapsed());
        let _ = sctx;

        let t = Instant::now();
        let (pages, _, _, info) = engine
            .build_pages(2.0, false, None)
            .expect("full layout (cold)");
        eprintln!(
            "build_pages FULL (cold):  {:?}  ({} pages, full={})",
            t.elapsed(),
            pages.len(),
            info.is_full_layout
        );

        let t = Instant::now();
        let (pages, _, _, _) = engine
            .build_pages(2.0, false, None)
            .expect("full layout (warm)");
        eprintln!(
            "build_pages FULL (warm):  {:?}  ({} pages)",
            t.elapsed(),
            pages.len()
        );

        engine.layout_cache = new_layout_cache();
        let t = Instant::now();
        let (pages, _, _, info) = engine
            .build_pages(2.0, false, Some(2000.0))
            .expect("band layout (cold)");
        eprintln!(
            "build_pages 2000px (cold):{:?}  ({} pages, full={})",
            t.elapsed(),
            pages.len(),
            info.is_full_layout
        );

        let t = Instant::now();
        let geom = engine.document_geometry().expect("document_geometry");
        eprintln!(
            "document_geometry (warm): {:?}  ({} lines)",
            t.elapsed(),
            geom.len()
        );

        let t = Instant::now();
        let evt = engine.selection_changed();
        eprintln!("selection_changed (warm): {:?}", t.elapsed());
        let _ = evt;

        let t = Instant::now();
        let wc = engine.undo.current().word_count();
        eprintln!(
            "word_count:               {:?}  ({} words)",
            t.elapsed(),
            wc
        );

        let t = Instant::now();
        let cc = engine.undo.current().character_count();
        eprintln!(
            "character_count:          {:?}  ({} chars)",
            t.elapsed(),
            cc
        );

        /* #34 shape — one interactive insert on the warm 50p doc. No canvases
        are registered, so this measures layout + scene build + geometry +
        probes, excluding the actual blit. */
        for i in 0..3 {
            let t = Instant::now();
            let evt = engine.do_insert_text_interactive(bpos_top(0, 0), "x".to_string());
            eprintln!("insert cycle 50p #{i}:      {:?}", t.elapsed());
            assert!(
                matches!(evt, Event::SelectionChanged { .. }),
                "insert must succeed"
            );
        }

        /* #34 shape — the tools/perf benchmark uses a ONE-page seeded doc. */
        engine.undo = UndoStack::new(
            DocumentTree::from_text("The quick brown fox jumps over the lazy dog."),
            100,
        );
        engine.selection = Some(SelectionState {
            anchor: bpos_top(0, 0),
            caret: bpos_top(0, 0),
            ideal_x: None,
            kind: SelectionKind::Linear,
        });
        engine.layout_cache = new_layout_cache();
        engine.lazy_layout = LazyLayoutState::default();
        for i in 0..5 {
            let t = Instant::now();
            let evt = engine.do_insert_text_interactive(bpos_top(0, i), "x".to_string());
            eprintln!("insert cycle 1p  #{i}:      {:?}", t.elapsed());
            assert!(
                matches!(evt, Event::SelectionChanged { .. }),
                "insert must succeed"
            );
        }
    }
}
