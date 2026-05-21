//! `engine-wasm` — `#[wasm_bindgen]` surface for the engine.
//!
//! Phase 1 weeks 15–24: document model (engine crate, `im::Vector`-backed) +
//! undo/redo + `.docx` load/save + InsertText that triggers an automatic
//! repaint when a layout config was cached by a prior `RenderPage`.

use bridge::{
    A11yParagraph, A11yRun, A11yTree, Color, Command, Direction, EngineStats, Event,
    FontMetrics as BridgeMetrics, LogicalPos as BridgeLogicalPos,
    LogicalRange as BridgeLogicalRange, Point as BridgePoint, Rect as BridgeRect, TextAttrs,
    TextAttrsPatch, UnderlineStyle, VerticalScript,
};
use engine::{DocumentTree, LogicalPos as EnginePos, SpanStyle, UndoStack};
use format_docx::writer::build_minimal_docx;
use kurbo::Rect;
use layout::{
    A4Page, LineBox, PageBox, ParagraphBox, ParagraphConfig, Point, Size, StyleSpan,
    layout_paragraph,
};
use render::atlas::GlyphAtlas;
use render::canvas2d_backend::render_canvas2d;
use render::dirty::DirtyTracker;
use render::scene::build_page_scene;
use render::vello_backend::VelloRenderer;
use std::collections::HashMap;
use std::sync::Arc;
use text_pipeline::{Alignment, FontStack, LoadedFont, ShapingDirection, shape_text};
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
#[derive(Clone, Copy)]
struct SelectionState {
    anchor: BridgeLogicalPos,
    caret: BridgeLogicalPos,
}

/// An in-progress IME composition (PHASE_4_HEADLESS_UI.md §6). Tracked
/// between `BeginComposition` and `EndComposition`; the latest `text` is
/// committed on a committing end. No on-canvas preview — see BACKLOG.md.
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
    /// Document paragraph index (not the box-tree index — empties are skipped).
    para: u32,
    start_x: f32,
    y_top: f32,
    height: f32,
    start_byte: u32,
    end_byte: u32,
    /// Caret slots in visual emission order — searched by nearest x or byte.
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
}

#[wasm_bindgen]
impl Engine {
    #[wasm_bindgen(constructor)]
    pub fn new(canvas: web_sys::OffscreenCanvas) -> Result<Engine, JsValue> {
        let ctx_obj = canvas
            .get_context("2d")?
            .ok_or_else(|| JsValue::from_str("OffscreenCanvas 2d context unavailable"))?;
        let ctx: OffscreenCanvasRenderingContext2d = ctx_obj.dyn_into()?;
        ctx.set_fill_style_str("#ffffff");
        ctx.fill_rect(0.0, 0.0, canvas.width() as f64, canvas.height() as f64);
        Ok(Engine {
            ctx: Some(ctx),
            fonts: HashMap::new(),
            undo: UndoStack::new(DocumentTree::new(), 100),
            layout_cfg: None,
            atlas: GlyphAtlas::new(),
            vello: None,
            dirty: DirtyTracker::new(),
            selection: None,
            composition: None,
        })
    }

    pub async fn dispatch(&mut self, cmd: JsValue) -> Result<JsValue, JsValue> {
        let cmd: Command = serde_wasm_bindgen::from_value(cmd)
            .map_err(|e| JsValue::from_str(&format!("decode command: {e}")))?;
        let evt: Event = self.apply(cmd).await;
        serde_wasm_bindgen::to_value(&evt)
            .map_err(|e| JsValue::from_str(&format!("encode event: {e}")))
    }

    /// P3-5: detect the best renderer backend — `vello` when a WebGPU device
    /// is acquired, else `canvas2d`. Worker-safe (no `web_sys::window()`).
    /// Vello rendering itself is routed in a later batch; Canvas2D stays the
    /// active path for now.
    pub async fn detect_renderer(&self) -> String {
        render::backend::detect_backend().await.as_str().to_string()
    }

    /// Whether the Vello (WebGPU) pipeline has been initialized via
    /// [`Engine::init_vello`]. Always `false` on the active Canvas2D path;
    /// `init_vello` is a P3-4 reachability root the worker does not call.
    pub fn vello_ready(&self) -> bool {
        self.vello.is_some()
    }
}

/// P3-4 dead-code-elimination retention root.
///
/// `init_vello` is a `#[wasm_bindgen]` export, so `wasm-ld --gc-sections`
/// retains it and everything it transitively calls: `VelloRenderer::new`
/// (WebGPU device + surface + `vello::Renderer`) and `VelloRenderer::render`
/// (scene encoding + `render_to_texture` + surface blit). With no reachable
/// caller the linker strips the whole `wgpu` + `vello` stack and the WASM
/// artifact understates its true size. The worker never calls this, so
/// Canvas2D stays the active renderer and the visual-diff goldens are
/// unaffected.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
impl Engine {
    /// Build the Vello (WebGPU) pipeline for `canvas` and run one frame.
    /// Reachability root only — see the impl-block docs.
    pub async fn init_vello(&mut self, canvas: web_sys::OffscreenCanvas) -> Result<(), JsValue> {
        let mut vr = VelloRenderer::new(canvas)
            .await
            .map_err(|e| JsValue::from_str(&e))?;
        vr.render(&render::scene::DisplayList::default(), |_| None)
            .map_err(|e| JsValue::from_str(&e))?;
        self.vello = Some(vr);
        Ok(())
    }
}

const POC_BASELINE_X: f64 = 50.0;
const POC_BASELINE_Y: f64 = 200.0;

fn to_engine_pos(p: BridgeLogicalPos) -> EnginePos {
    EnginePos {
        para: p.para,
        offset: p.offset,
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

/// Expand a paragraph's sparse style runs into a gap-free list of resolved
/// [`StyleSpan`]s covering `[0, text.len())`, filling unstyled gaps with the
/// document defaults. Every `px_size` is multiplied by `scale` so glyphs
/// rasterize at device resolution (`default_size` arrives logical).
fn build_style_spans(
    para: &engine::Paragraph,
    default_size: f32,
    default_color: [u8; 4],
    scale: f32,
) -> Vec<StyleSpan> {
    let len = para.text.len() as u32;
    let mut spans: Vec<StyleSpan> = Vec::new();
    let mut cursor = 0_u32;
    for run in &para.spans {
        if run.start > cursor {
            spans.push(StyleSpan {
                start: cursor,
                end: run.start,
                px_size: default_size * scale,
                color: default_color,
            });
        }
        spans.push(StyleSpan {
            start: run.start,
            end: run.end,
            px_size: run.style.font_size.unwrap_or(default_size) * scale,
            color: run.style.color.unwrap_or(default_color),
        });
        cursor = run.end;
    }
    if cursor < len {
        spans.push(StyleSpan {
            start: cursor,
            end: len,
            px_size: default_size * scale,
            color: default_color,
        });
    }
    spans
}

/// Flatten one line into [`CaretSlot`]s — one caret position per cluster
/// boundary. `line_abs_x` is the line's absolute left edge. Mirrors the pen
/// walk in `render::scene::build_page_scene` so hit-testing inverts exactly
/// the geometry the renderer drew.
fn build_line_slots(line: &LineBox, line_abs_x: f32) -> Vec<CaretSlot> {
    let mut slots: Vec<CaretSlot> = Vec::new();
    let mut pen = 0.0_f32;
    for run in &line.runs {
        let run_start_x = line_abs_x + pen;
        let run_advance: f32 = run.glyphs.iter().map(|g| g.x_advance).sum();
        match run.direction {
            ShapingDirection::Ltr => {
                let mut cum = 0.0_f32;
                for g in &run.glyphs {
                    slots.push(CaretSlot {
                        x: run_start_x + cum,
                        byte: run.source_range.start + g.cluster,
                    });
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
                    slots.push(CaretSlot {
                        x: run_start_x + cum + g.x_advance,
                        byte: run.source_range.start + g.cluster,
                    });
                    cum += g.x_advance;
                }
                slots.push(CaretSlot {
                    x: run_start_x,
                    byte: run.source_range.end,
                });
            }
        }
        pen += run_advance;
    }
    slots
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

/// The line nearest `y` — the band containing it, else the closest above/below.
fn nearest_line(geom: &[LineGeom], y: f32) -> Option<&LineGeom> {
    geom.iter()
        .min_by(|a, b| line_y_dist(a, y).total_cmp(&line_y_dist(b, y)))
}

/// Map an absolute pixel to a logical position — nearest line by `y`, then
/// nearest caret slot by `x`.
fn hit_test_geom(geom: &[LineGeom], x: f32, y: f32) -> BridgeLogicalPos {
    let Some(line) = nearest_line(geom, y) else {
        return BridgeLogicalPos { para: 0, offset: 0 };
    };
    let offset = line
        .slots
        .iter()
        .min_by(|a, b| (a.x - x).abs().total_cmp(&(b.x - x).abs()))
        .map_or(line.start_byte, |s| s.byte);
    BridgeLogicalPos {
        para: line.para,
        offset,
    }
}

/// Absolute x of the caret slot whose byte is nearest `byte`.
fn slot_x_for_byte(line: &LineGeom, byte: u32) -> f32 {
    line.slots
        .iter()
        .min_by_key(|s| s.byte.abs_diff(byte))
        .map_or(line.start_x, |s| s.x)
}

/// Caret rectangle for `pos`, `caret_w` device px wide. Falls back to
/// `fallback` when the document has no geometry yet (empty document).
fn caret_rect_geom(
    geom: &[LineGeom],
    pos: BridgeLogicalPos,
    fallback: BridgeRect,
    caret_w: f32,
) -> BridgeRect {
    let line = geom
        .iter()
        .find(|l| l.para == pos.para && pos.offset >= l.start_byte && pos.offset <= l.end_byte)
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

/// Per-line bounding selection rectangles for `[start, end]` — the D4.6
/// pragmatic subset (one rect per line). Discontinuous per-BiDi-run rects
/// are deferred (see BACKLOG.md).
fn selection_rects_geom(
    geom: &[LineGeom],
    start: BridgeLogicalPos,
    end: BridgeLogicalPos,
) -> Vec<BridgeRect> {
    let mut rects: Vec<BridgeRect> = Vec::new();
    for line in geom {
        if line.para < start.para || line.para > end.para {
            continue;
        }
        let lo = if line.para == start.para {
            start.offset.max(line.start_byte)
        } else {
            line.start_byte
        };
        let hi = if line.para == end.para {
            end.offset.min(line.end_byte)
        } else {
            line.end_byte
        };
        if lo >= hi {
            continue;
        }
        let xa = slot_x_for_byte(line, lo);
        let xb = slot_x_for_byte(line, hi);
        let (x0, x1) = if xa <= xb { (xa, xb) } else { (xb, xa) };
        rects.push(BridgeRect {
            x: x0,
            y: line.y_top,
            w: x1 - x0,
            h: line.height,
        });
    }
    rects
}

/// Order two positions into document order (paragraph, then offset).
fn ordered(a: BridgeLogicalPos, b: BridgeLogicalPos) -> (BridgeLogicalPos, BridgeLogicalPos) {
    if (a.para, a.offset) <= (b.para, b.offset) {
        (a, b)
    } else {
        (b, a)
    }
}

/// Clamp a position into `doc` — paragraph index and byte offset both in range.
fn clamp_pos(doc: &DocumentTree, pos: BridgeLogicalPos) -> BridgeLogicalPos {
    if doc.paragraphs.is_empty() {
        return BridgeLogicalPos { para: 0, offset: 0 };
    }
    let para = (pos.para as usize).min(doc.paragraphs.len() - 1);
    let offset = pos.offset.min(doc.paragraphs[para].text.len() as u32);
    BridgeLogicalPos {
        para: para as u32,
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
                        anchor: BridgeLogicalPos { para: 0, offset: 0 },
                        caret: BridgeLogicalPos { para: 0, offset: 0 },
                    });
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
            Command::ExportPdf { .. } => self.do_export_pdf(),
            Command::CloseDocument => phase3_stub("CloseDocument"),
            Command::DeleteRange { range } => self.do_delete_range(range),
            Command::ReplaceRange { .. } => phase3_stub("ReplaceRange"),
            Command::ApplyFormatting { range, attrs } => self.apply_formatting(range, attrs),
            Command::SplitParagraph { at } => self.do_split_paragraph(at),
            Command::MergeParagraph { .. } => phase3_stub("MergeParagraph"),
            Command::InsertImage { .. } => phase3_stub("InsertImage"),
            Command::SetSelection { range, caret } => self.do_set_selection(range, caret),
            Command::ExtendSelection { to, .. } => self.do_extend_selection(to),
            Command::SelectAll => phase3_stub("SelectAll"),
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
            Command::DeleteAtCaret { forward, by_word } => {
                self.do_delete_at_caret(forward, by_word)
            }

            // Phase 4 §10 — accessibility shadow tree.
            Command::RequestAccessibilityTree => Event::AccessibilityTreeChanged {
                tree: self.build_a11y_tree(),
            },

            // Phase 4 §12 — clipboard.
            Command::GetSelectionAsClipboard => self.do_get_selection_as_clipboard(),
            Command::PastePlain { text } => self.do_paste_plain(text),
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
                if let Err(e) =
                    render::canvas2d_backend::paint_alpha_glyph(ctx, &raster, dx, dy, [0, 0, 0])
                {
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
        };
        let new_doc = self.undo.current().apply_style(
            to_engine_pos(range.start),
            to_engine_pos(range.end),
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
        let document_tree_bytes: usize = doc.paragraphs.iter().map(|p| p.text.len()).sum();
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

    /// Lay out the current document into a `PageBox` plus the `FontStack` used
    /// to shape it. Shared by the Canvas2D repaint and PDF export. Every
    /// dimension is multiplied by `scale` — `dpr` for the HiDPI canvas, `1.0`
    /// for PDF (PDF user space is logical points).
    fn build_page(&self, scale: f32) -> Result<(PageBox, FontStack, Vec<u32>), Box<Event>> {
        let cfg = match self.layout_cfg.clone() {
            Some(c) => c,
            None => {
                return Err(Box::new(Event::Error {
                    message: "build_page: no layout config cached".into(),
                }));
            }
        };
        if !self.fonts.contains_key(&cfg.font_id) {
            return Err(Box::new(Event::Error {
                message: format!("font `{}` not loaded", cfg.font_id),
            }));
        }
        let page = A4Page::a4().scaled(scale);

        /* Per-script font stack; the cached `font_id` is the fallback root. */
        let font_stack = FontStack::from_faces(self.fonts.clone(), &cfg.font_id);

        /* Lay out each paragraph, stacking them down the content area. */
        let mut paragraphs: Vec<ParagraphBox> = Vec::new();
        /* Document paragraph index of each emitted `ParagraphBox` — empty
        paragraphs produce no box, so box index != document index. */
        let mut box_doc_index: Vec<u32> = Vec::new();
        let mut para_y_offset = 0.0_f32;
        let doc = self.undo.current().clone();
        for (doc_idx, para) in doc.paragraphs.iter().enumerate() {
            if para.text.is_empty() {
                para_y_offset += cfg.line_height * scale;
                continue;
            }
            let spans = build_style_spans(para, cfg.px_size, [0, 0, 0, 255], scale);
            let para_cfg = ParagraphConfig {
                text: &para.text,
                fonts: &font_stack,
                spans: &spans,
                base_direction: cfg.base_direction,
                max_width: page.content_width(),
                line_height: cfg.line_height * scale,
                alignment: cfg.alignment,
            };
            let mut para_box = layout_paragraph(para_cfg);
            para_box.origin = Point {
                x: 0.0,
                y: para_y_offset,
            };
            para_y_offset += para_box.size.height;
            paragraphs.push(para_box);
            box_doc_index.push(doc_idx as u32);
        }

        let page_box = PageBox {
            size: Size {
                width: page.width,
                height: page.height,
            },
            margins: page.margin,
            paragraphs,
        };
        Ok((page_box, font_stack, box_doc_index))
    }

    fn render_document(&mut self, clip: Option<Rect>) -> Result<RenderStats, Box<Event>> {
        let (page_box, _font_stack, _box_doc_index) = self.build_page(self.scale())?;

        let line_count: u32 = page_box
            .paragraphs
            .iter()
            .map(|p| p.lines.len() as u32)
            .sum();
        let glyph_count: u32 = page_box
            .paragraphs
            .iter()
            .flat_map(|p| &p.lines)
            .flat_map(|l| &l.runs)
            .map(|r| r.glyphs.len() as u32)
            .sum();

        let scene = build_page_scene(&page_box);
        let clip_rect = clip.unwrap_or_else(|| {
            Rect::new(
                0.0,
                0.0,
                f64::from(page_box.size.width),
                f64::from(page_box.size.height),
            )
        });
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

        Ok(RenderStats {
            page_width: page_box.size.width,
            page_height: page_box.size.height,
            line_count,
            glyph_count,
        })
    }

    /// Export the current document to a single-page PDF (D3.7). Always laid
    /// out at scale `1.0` — PDF user space is logical points, never device px.
    fn do_export_pdf(&self) -> Event {
        let (page_box, font_stack, _box_doc_index) = match self.build_page(1.0) {
            Ok(v) => v,
            Err(e) => return *e,
        };
        let mut bytes: Vec<u8> = Vec::new();
        if let Err(e) = format_pdf::export_pdf(&page_box, &font_stack, &mut bytes) {
            return Event::Error {
                message: format!("ExportPdf: {e}"),
            };
        }
        Event::PdfExported { bytes, pages: 1 }
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

    /// Flatten the current document into per-line hit-test geometry. Re-lays
    /// out the document — cheap for the single-page PoC; cache when editing
    /// lands.
    fn document_geometry(&self) -> Result<Vec<LineGeom>, Box<Event>> {
        let (page, _fonts, box_doc_index) = self.build_page(self.scale())?;
        let content_x = page.margins.left;
        let content_y = page.margins.top;
        let mut geom: Vec<LineGeom> = Vec::new();
        for (k, para_box) in page.paragraphs.iter().enumerate() {
            let doc_idx = box_doc_index.get(k).copied().unwrap_or(0);
            let para_x = content_x + para_box.origin.x;
            let para_y = content_y + para_box.origin.y;
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
                geom.push(LineGeom {
                    para: doc_idx,
                    start_x: line_x,
                    y_top: line_y,
                    height: line.height,
                    start_byte,
                    end_byte,
                    slots: build_line_slots(line, line_x),
                });
            }
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
        self.selection = Some(SelectionState { anchor, caret });
        self.selection_changed()
    }

    /// `Command::ExtendSelection` — keep the anchor, move the caret to `to`.
    fn do_extend_selection(&mut self, to: BridgeLogicalPos) -> Event {
        let anchor = self.selection.map_or(to, |s| s.anchor);
        self.selection = Some(SelectionState { anchor, caret: to });
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
        let (lo, hi) = self
            .undo
            .current()
            .paragraphs
            .get(hit.para as usize)
            .map_or((hit.offset, hit.offset), |p| p.word_bounds(hit.offset));
        self.selection = Some(SelectionState {
            anchor: BridgeLogicalPos {
                para: hit.para,
                offset: lo,
            },
            caret: BridgeLogicalPos {
                para: hit.para,
                offset: hi,
            },
        });
        self.selection_changed()
    }

    /// Assemble a `SelectionChanged` event from the current selection.
    fn selection_changed(&self) -> Event {
        let Some(sel) = self.selection else {
            return Event::Error {
                message: "selection_changed: no active selection".into(),
            };
        };
        let geom = match self.document_geometry() {
            Ok(g) => g,
            Err(e) => return *e,
        };
        let (start, end) = ordered(sel.anchor, sel.caret);
        let scale = self.scale();
        let page = A4Page::a4().scaled(scale);
        let fallback = BridgeRect {
            x: page.margin.left,
            y: page.margin.top,
            w: CARET_WIDTH * scale,
            h: self.layout_cfg.as_ref().map_or(16.0, |c| c.line_height) * scale,
        };
        let rects = if start == end {
            Vec::new()
        } else {
            selection_rects_geom(&geom, start, end)
        };
        let direction = match self.layout_cfg.as_ref().map(|c| c.base_direction) {
            Some(ShapingDirection::Rtl) => Direction::Rtl,
            _ => Direction::Ltr,
        };
        Event::SelectionChanged {
            range: BridgeLogicalRange { start, end },
            caret: caret_rect_geom(&geom, sel.caret, fallback, CARET_WIDTH * scale),
            direction,
            rects,
            attrs_at_caret: self.attrs_at(self.attrs_probe(start, end)),
            can_undo: self.undo.can_undo(),
            can_redo: self.undo.can_redo(),
        }
    }

    /// The offset whose style the toolbar should reflect: the selection start
    /// for a range, or the char before a collapsed caret (the style typing
    /// there would extend). `style_at(caret)` alone reads the char *after* the
    /// caret, which is unstyled right after formatting a selection.
    fn attrs_probe(&self, start: BridgeLogicalPos, end: BridgeLogicalPos) -> BridgeLogicalPos {
        if start != end || start.offset == 0 {
            return start;
        }
        let prev = self
            .undo
            .current()
            .paragraphs
            .get(start.para as usize)
            .map_or(start.offset, |p| p.prev_offset(start.offset));
        BridgeLogicalPos {
            para: start.para,
            offset: prev,
        }
    }

    /// Resolved text attributes at `pos`. Spans carry size + colour + the
    /// bold/italic/underline flags; `strike`, `bg_color`, `script` and
    /// `language` default until those land.
    fn attrs_at(&self, pos: BridgeLogicalPos) -> TextAttrs {
        let style = self
            .undo
            .current()
            .paragraphs
            .get(pos.para as usize)
            .map_or(SpanStyle::default(), |p| p.style_at(pos.offset));
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
            strike: false,
            font_family: self
                .layout_cfg
                .as_ref()
                .map_or(String::new(), |c| c.font_id.clone()),
            font_size: style.font_size.unwrap_or(default_size),
            color: Color { r, g, b, a },
            bg_color: None,
            script: VerticalScript::Normal,
            language: String::new(),
        }
    }

    /// Commit a document edit: push undo, collapse the caret at `caret`,
    /// invalidate + repaint, then emit `SelectionChanged`.
    fn commit_edit(&mut self, new_doc: DocumentTree, caret: BridgeLogicalPos) -> Event {
        self.undo.push(new_doc);
        let caret = clamp_pos(self.undo.current(), caret);
        self.selection = Some(SelectionState {
            anchor: caret,
            caret,
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
        let sel = self.selection.unwrap_or(SelectionState {
            anchor: at,
            caret: at,
        });
        let (start, end) = ordered(sel.anchor, sel.caret);
        let base = if start == end {
            self.undo.current().clone()
        } else {
            self.undo
                .current()
                .delete_range(to_engine_pos(start), to_engine_pos(end))
        };
        let new_doc = base.insert_text(to_engine_pos(start), &text);
        let caret = BridgeLogicalPos {
            para: start.para,
            offset: start.offset + text.len() as u32,
        };
        self.commit_edit(new_doc, caret)
    }

    /// `Command::DeleteRange` — delete an explicit logical range.
    fn do_delete_range(&mut self, range: BridgeLogicalRange) -> Event {
        let (start, end) = ordered(range.start, range.end);
        let new_doc = self
            .undo
            .current()
            .delete_range(to_engine_pos(start), to_engine_pos(end));
        self.commit_edit(new_doc, start)
    }

    /// `Command::SplitParagraph` — break the paragraph at the caret (replacing
    /// any non-empty selection first); the caret moves to the new paragraph.
    fn do_split_paragraph(&mut self, at: BridgeLogicalPos) -> Event {
        let (base, split_at) = match self.selection {
            Some(s) => {
                let (start, end) = ordered(s.anchor, s.caret);
                let doc = if start == end {
                    self.undo.current().clone()
                } else {
                    self.undo
                        .current()
                        .delete_range(to_engine_pos(start), to_engine_pos(end))
                };
                (doc, start)
            }
            None => (self.undo.current().clone(), at),
        };
        let new_doc = base.split_paragraph(to_engine_pos(split_at));
        let caret = BridgeLogicalPos {
            para: split_at.para + 1,
            offset: 0,
        };
        self.commit_edit(new_doc, caret)
    }

    /// `Command::DeleteAtCaret` — delete the selection if non-empty, else one
    /// grapheme (or word) in the `forward` direction from the caret.
    fn do_delete_at_caret(&mut self, forward: bool, by_word: bool) -> Event {
        let Some(sel) = self.selection else {
            return Event::Error {
                message: "DeleteAtCaret: no active selection".into(),
            };
        };
        let (start, end) = ordered(sel.anchor, sel.caret);
        if start != end {
            let new_doc = self
                .undo
                .current()
                .delete_range(to_engine_pos(start), to_engine_pos(end));
            return self.commit_edit(new_doc, start);
        }
        let Some((del_start, del_end)) = self.delete_target(sel.caret, forward, by_word) else {
            /* Caret at a document edge — nothing to delete. */
            return self.selection_changed();
        };
        let new_doc = self
            .undo
            .current()
            .delete_range(to_engine_pos(del_start), to_engine_pos(del_end));
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
        let para = doc.paragraphs.get(caret.para as usize)?;
        let para_len = para.text.len() as u32;
        if forward {
            if caret.offset < para_len {
                let to = if by_word {
                    para.word_bounds(caret.offset).1
                } else {
                    para.next_offset(caret.offset)
                };
                Some((
                    caret,
                    BridgeLogicalPos {
                        para: caret.para,
                        offset: to,
                    },
                ))
            } else if (caret.para as usize) + 1 < doc.paragraphs.len() {
                Some((
                    caret,
                    BridgeLogicalPos {
                        para: caret.para + 1,
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
            Some((
                BridgeLogicalPos {
                    para: caret.para,
                    offset: from,
                },
                caret,
            ))
        } else if caret.para > 0 {
            let prev_len = doc
                .paragraphs
                .get(caret.para as usize - 1)
                .map_or(0, |p| p.text.len() as u32);
            Some((
                BridgeLogicalPos {
                    para: caret.para - 1,
                    offset: prev_len,
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
            at,
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
            Some(c) => c.at,
            None => self
                .selection
                .map_or(BridgeLogicalPos { para: 0, offset: 0 }, |s| s.caret),
        };
        self.composition = Some(CompositionState {
            at,
            text: text.clone(),
        });
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
            _ => self.selection_changed(),
        }
    }

    /// Re-emit selection after an undo/redo, clamping the caret into the
    /// restored document. Falls back to `UndoStateChanged` when no selection
    /// exists (the Phase-1 harness path).
    fn after_history_change(&mut self) -> Event {
        match self.selection {
            Some(sel) => {
                let doc = self.undo.current();
                self.selection = Some(SelectionState {
                    anchor: clamp_pos(doc, sel.anchor),
                    caret: clamp_pos(doc, sel.caret),
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

    /// Build a full accessibility snapshot of the current document — every
    /// paragraph, split into style runs (PHASE_4_HEADLESS_UI.md §10).
    fn build_a11y_tree(&self) -> A11yTree {
        let direction = match self.layout_cfg.as_ref().map(|c| c.base_direction) {
            Some(ShapingDirection::Rtl) => Direction::Rtl,
            _ => Direction::Ltr,
        };
        let paragraphs = self
            .undo
            .current()
            .paragraphs
            .iter()
            .enumerate()
            .map(|(i, para)| A11yParagraph {
                id: i as u32,
                direction,
                runs: a11y_runs(para),
            })
            .collect();
        A11yTree { paragraphs }
    }

    /// `Command::GetSelectionAsClipboard` — the selection text as clipboard
    /// payloads. `html` / `docx_fragment` await rich clipboard generation.
    fn do_get_selection_as_clipboard(&self) -> Event {
        let plain = match self.selection {
            Some(sel) => {
                let (start, end) = ordered(sel.anchor, sel.caret);
                self.undo
                    .current()
                    .text_range(to_engine_pos(start), to_engine_pos(end))
            }
            None => String::new(),
        };
        Event::ClipboardPayload {
            plain,
            html: String::new(),
            docx_fragment: Vec::new(),
        }
    }

    /// `Command::PastePlain` — insert clipboard text at the caret, replacing
    /// any non-empty selection.
    fn do_paste_plain(&mut self, text: String) -> Event {
        let at = self
            .selection
            .map_or(BridgeLogicalPos { para: 0, offset: 0 }, |s| s.caret);
        self.do_insert_text_interactive(at, text)
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
        };
        let cmd_js = serde_wasm_bindgen::to_value(&Command::Ping).expect("encode ping");
        let evt_js = engine
            .dispatch(cmd_js)
            .await
            .expect("dispatch should succeed");
        let evt: Event = serde_wasm_bindgen::from_value(evt_js).expect("decode event");
        assert!(matches!(evt, Event::Pong), "expected Pong, got {evt:?}");
    }
}
