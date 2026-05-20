//! `engine-wasm` — `#[wasm_bindgen]` surface for the engine.
//!
//! Phase 1 weeks 15–24: document model (engine crate, `im::Vector`-backed) +
//! undo/redo + `.docx` load/save + InsertText that triggers an automatic
//! repaint when a layout config was cached by a prior `RenderPage`.

use bridge::{
    Command, EngineStats, Event, FontMetrics as BridgeMetrics, LogicalPos as BridgeLogicalPos,
};
use engine::{DocumentTree, LogicalPos as EnginePos, UndoStack};
use format_docx::writer::build_minimal_docx;
use layout::{A4Page, LineBox, ParagraphConfig, layout_paragraph};
use render::atlas::GlyphAtlas;
use render::canvas2d_backend::render_canvas2d;
use render::scene::{PaintConfig, build_page_scene};
use render::vello_backend::VelloRenderer;
use std::collections::HashMap;
use std::sync::Arc;
use text_pipeline::{Alignment, LoadedFont, ShapingDirection, shape_text};
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
            Command::ExportPdf { .. } => phase3_stub("ExportPdf"),
            Command::CloseDocument => phase3_stub("CloseDocument"),
            Command::DeleteRange { .. } => phase3_stub("DeleteRange"),
            Command::ReplaceRange { .. } => phase3_stub("ReplaceRange"),
            Command::ApplyFormatting { .. } => phase3_stub("ApplyFormatting"),
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
            Command::RequestPaint { .. } => phase3_stub("RequestPaint"),
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

        let stats = match self.render_document() {
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

    fn do_undo(&mut self) -> Event {
        self.undo.undo();
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
        let _ = self.render_document();
    }

    fn maybe_repaint_result(&mut self) -> Result<(), Box<Event>> {
        if self.layout_cfg.is_none() {
            return Ok(());
        }
        self.render_document().map(|_| ())
    }

    fn render_document(&mut self) -> Result<RenderStats, Box<Event>> {
        let cfg = match self.layout_cfg.clone() {
            Some(c) => c,
            None => {
                return Err(Box::new(Event::Error {
                    message: "render_document: no layout config cached".into(),
                }));
            }
        };
        let font = match self.fonts.get(&cfg.font_id) {
            Some(f) => f.clone(),
            None => {
                return Err(Box::new(Event::Error {
                    message: format!("font `{}` not loaded", cfg.font_id),
                }));
            }
        };
        let page = A4Page::a4();

        /* Lay out every paragraph and stack them into a flat line list whose
        `baseline_y` is page-absolute, ready for the scene builder. */
        let margin_top = page.margin.top;
        let mut flat_lines: Vec<LineBox> = Vec::new();
        let mut para_y_offset = 0.0_f32;
        let doc = self.undo.current().clone();
        for para in &doc.paragraphs {
            if para.text.is_empty() {
                para_y_offset += cfg.line_height;
                continue;
            }
            let para_cfg = ParagraphConfig {
                text: &para.text,
                font: &font,
                base_direction: cfg.base_direction,
                px_size: cfg.px_size,
                max_width: page.content_width(),
                line_height: cfg.line_height,
                alignment: cfg.alignment,
            };
            let lines = layout_paragraph(para_cfg);
            let n_lines = lines.len();
            for mut line in lines {
                line.baseline_y += margin_top + para_y_offset;
                flat_lines.push(line);
            }
            para_y_offset += n_lines as f32 * cfg.line_height;
        }

        let line_count = flat_lines.len() as u32;
        let glyph_count: u32 = flat_lines.iter().map(|l| l.glyphs.len() as u32).sum();

        /* Layout → scene builder → Canvas2D backend. */
        let scene = build_page_scene(
            &page,
            &flat_lines,
            &PaintConfig {
                font: cfg.font_id.clone(),
                px_size: cfg.px_size,
                alignment: cfg.alignment,
            },
        );
        let ctx = match &self.ctx {
            Some(c) => c.clone(),
            None => {
                return Err(Box::new(Event::Error {
                    message: "no canvas".into(),
                }));
            }
        };
        if let Err(e) = render_canvas2d(&ctx, &scene, &mut self.atlas, |id| {
            self.fonts.get(id).cloned()
        }) {
            return Err(Box::new(Event::Error {
                message: format!("paint: {e:?}"),
            }));
        }

        Ok(RenderStats {
            page_width: page.width,
            page_height: page.height,
            line_count,
            glyph_count,
        })
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
