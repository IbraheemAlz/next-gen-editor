//! `engine-wasm` — `#[wasm_bindgen]` surface for the engine.
//!
//! Phase 1 weeks 15–24: document model (engine crate, `im::Vector`-backed) +
//! undo/redo + `.docx` load/save + InsertText that triggers an automatic
//! repaint when a layout config was cached by a prior `RenderPage`.

use bridge::{
    Color, Command, EngineStats, Event, FontMetrics as BridgeMetrics,
    LogicalPos as BridgeLogicalPos, LogicalRange as BridgeLogicalRange, Rect as BridgeRect,
    TextAttrs, TextAttrsPatch, UnderlineStyle, VerticalScript,
};
use engine::{DocumentTree, LogicalPos as EnginePos, SpanStyle, UndoStack};
use format_docx::writer::build_minimal_docx;
use kurbo::Rect;
use layout::{
    A4Page, PageBox, ParagraphBox, ParagraphConfig, Point, Size, StyleSpan, layout_paragraph,
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

/// The full A4 page rectangle — the dirty region an edit invalidates, since an
/// edit can reflow the whole page.
fn full_page_rect() -> Rect {
    let page = A4Page::a4();
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
/// document defaults.
fn build_style_spans(
    para: &engine::Paragraph,
    default_size: f32,
    default_color: [u8; 4],
) -> Vec<StyleSpan> {
    let len = para.text.len() as u32;
    let mut spans: Vec<StyleSpan> = Vec::new();
    let mut cursor = 0_u32;
    for run in &para.spans {
        if run.start > cursor {
            spans.push(StyleSpan {
                start: cursor,
                end: run.start,
                px_size: default_size,
                color: default_color,
            });
        }
        spans.push(StyleSpan {
            start: run.start,
            end: run.end,
            px_size: run.style.font_size.unwrap_or(default_size),
            color: run.style.color.unwrap_or(default_color),
        });
        cursor = run.end;
    }
    if cursor < len {
        spans.push(StyleSpan {
            start: cursor,
            end: len,
            px_size: default_size,
            color: default_color,
        });
    }
    spans
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
            } => self.render_page(text, font_id, base_direction, px_size, line_height, align),

            Command::InsertText { at, text } => self.insert_text(at, text),
            Command::Undo => self.do_undo(),
            Command::Redo => self.do_redo(),

            Command::LoadDocx { bytes } => match format_docx::read_docx(&bytes) {
                Ok(archive) => {
                    let paragraph_count = archive.document.paragraph_count();
                    self.undo = UndoStack::new(archive.document, 100);
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
            Command::DeleteRange { .. } => phase3_stub("DeleteRange"),
            Command::ReplaceRange { .. } => phase3_stub("ReplaceRange"),
            Command::ApplyFormatting { range, attrs } => self.apply_formatting(range, attrs),
            Command::SplitParagraph { .. } => phase3_stub("SplitParagraph"),
            Command::MergeParagraph { .. } => phase3_stub("MergeParagraph"),
            Command::InsertImage { .. } => phase3_stub("InsertImage"),
            Command::SetSelection { .. } => phase3_stub("SetSelection"),
            Command::ExtendSelection { .. } => phase3_stub("ExtendSelection"),
            Command::SelectAll => phase3_stub("SelectAll"),
            Command::BeginComposition { .. } => phase3_stub("BeginComposition"),
            Command::UpdateComposition { .. } => phase3_stub("UpdateComposition"),
            Command::EndComposition { .. } => phase3_stub("EndComposition"),
            Command::SetViewport { .. } => phase3_stub("SetViewport"),
            Command::SetZoom { .. } => phase3_stub("SetZoom"),
            Command::RequestPaint { viewport, dirty } => self.do_request_paint(viewport, dirty),
            Command::UnloadFont { .. } => phase3_stub("UnloadFont"),
            Command::RequestStats => self.request_stats(),
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

    fn render_page(
        &mut self,
        text: String,
        font_id: String,
        base_direction: String,
        px_size: f32,
        line_height: f32,
        align: String,
    ) -> Event {
        let dir = match base_direction.to_ascii_uppercase().as_str() {
            "RTL" => ShapingDirection::Rtl,
            _ => ShapingDirection::Ltr,
        };
        let alignment = match align.to_ascii_uppercase().as_str() {
            "JUSTIFY" => Alignment::Justify,
            "END" => Alignment::End,
            "CENTER" => Alignment::Center,
            _ => Alignment::Start,
        };

        /* Reset the document + undo stack to a single paragraph of `text`,
        then cache the layout config so subsequent InsertText/Undo/Redo
        commands can repaint without re-specifying params. */
        self.undo = UndoStack::new(DocumentTree::from_text(&text), 100);
        self.layout_cfg = Some(RenderConfig {
            font_id,
            base_direction: dir,
            px_size,
            line_height,
            alignment,
        });

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
        self.dirty.invalidate(full_page_rect());
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
        };
        let new_doc = self.undo.current().apply_style(
            to_engine_pos(range.start),
            to_engine_pos(range.end),
            patch,
        );
        self.undo.push(new_doc);
        self.dirty.invalidate(full_page_rect());
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        let default_size = self.layout_cfg.as_ref().map_or(16.0, |c| c.px_size);
        Event::FormattingChanged {
            range,
            attrs: resolved_attrs(&attrs, default_size),
        }
    }

    fn do_undo(&mut self) -> Event {
        self.undo.undo();
        self.dirty.invalidate(full_page_rect());
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        Event::UndoStateChanged {
            can_undo: self.undo.can_undo(),
            can_redo: self.undo.can_redo(),
            undo_depth: self.undo.depth(),
        }
    }

    fn do_redo(&mut self) -> Event {
        self.undo.redo();
        self.dirty.invalidate(full_page_rect());
        if let Err(e) = self.maybe_repaint_result() {
            return *e;
        }
        Event::UndoStateChanged {
            can_undo: self.undo.can_undo(),
            can_redo: self.undo.can_redo(),
            undo_depth: self.undo.depth(),
        }
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

    /// Lay out the current document into a `PageBox` plus the `FontStack` used
    /// to shape it. Shared by the Canvas2D repaint and PDF export.
    fn build_page(&self) -> Result<(PageBox, FontStack), Box<Event>> {
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
        let page = A4Page::a4();

        /* Per-script font stack; the cached `font_id` is the fallback root. */
        let font_stack = FontStack::from_faces(self.fonts.clone(), &cfg.font_id);

        /* Lay out each paragraph, stacking them down the content area. */
        let mut paragraphs: Vec<ParagraphBox> = Vec::new();
        let mut para_y_offset = 0.0_f32;
        let doc = self.undo.current().clone();
        for para in &doc.paragraphs {
            if para.text.is_empty() {
                para_y_offset += cfg.line_height;
                continue;
            }
            let spans = build_style_spans(para, cfg.px_size, [0, 0, 0, 255]);
            let para_cfg = ParagraphConfig {
                text: &para.text,
                fonts: &font_stack,
                spans: &spans,
                base_direction: cfg.base_direction,
                max_width: page.content_width(),
                line_height: cfg.line_height,
                alignment: cfg.alignment,
            };
            let mut para_box = layout_paragraph(para_cfg);
            para_box.origin = Point {
                x: 0.0,
                y: para_y_offset,
            };
            para_y_offset += para_box.size.height;
            paragraphs.push(para_box);
        }

        let page_box = PageBox {
            size: Size {
                width: page.width,
                height: page.height,
            },
            margins: page.margin,
            paragraphs,
        };
        Ok((page_box, font_stack))
    }

    fn render_document(&mut self, clip: Option<Rect>) -> Result<RenderStats, Box<Event>> {
        let (page_box, _font_stack) = self.build_page()?;

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

    /// Export the current document to a single-page PDF (D3.7).
    fn do_export_pdf(&self) -> Event {
        let (page_box, font_stack) = match self.build_page() {
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
