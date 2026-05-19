//! `engine-wasm` — `#[wasm_bindgen]` surface for the engine.
//!
//! Phase 1 weeks 5–9: font load + glyph metrics + alpha-mask raster +
//! `rustybuzz` shaping for both LTR and RTL.

use bridge::{Command, Event, FontMetrics as BridgeMetrics};
use layout::{A4Page, ParagraphConfig, layout_paragraph};
use std::collections::HashMap;
use std::sync::Arc;
use text_pipeline::{Alignment, LoadedFont, ShapingDirection, shape_text};
use wasm_bindgen::prelude::*;
use web_sys::OffscreenCanvasRenderingContext2d;

#[wasm_bindgen(start)]
pub fn boot() {
    console_error_panic_hook::set_once();
}

/// Public engine surface exposed to JS via `wasm-bindgen`.
#[wasm_bindgen]
pub struct Engine {
    ctx: Option<OffscreenCanvasRenderingContext2d>,
    fonts: HashMap<String, Arc<LoadedFont>>,
}

#[wasm_bindgen]
impl Engine {
    /// Construct from an `OffscreenCanvas` transferred from the main thread.
    /// Clears the canvas to white so subsequent glyph paints are visible.
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
        })
    }

    pub async fn dispatch(&mut self, cmd: JsValue) -> Result<JsValue, JsValue> {
        let cmd: Command = serde_wasm_bindgen::from_value(cmd)
            .map_err(|e| JsValue::from_str(&format!("decode command: {e}")))?;
        let evt: Event = self.apply(cmd).await;
        serde_wasm_bindgen::to_value(&evt)
            .map_err(|e| JsValue::from_str(&format!("encode event: {e}")))
    }
}

const POC_BASELINE_X: f64 = 50.0;
const POC_BASELINE_Y: f64 = 200.0;

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
            } => {
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

            Command::ShapeAndRasterize {
                text,
                font_id,
                direction,
                px_size,
            } => {
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
                        /* Skip `.notdef` (glyph 0) — typically only present for
                           characters not covered by the font; they have no outline. */
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
                            ctx, &raster, dx, dy, [0, 0, 0],
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

            Command::RenderPage {
                text,
                font_id,
                base_direction,
                px_size,
                line_height,
                align,
            } => {
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
                let font = match self.fonts.get(&font_id) {
                    Some(f) => f.clone(),
                    None => {
                        return Event::Error {
                            message: format!("font `{font_id}` not loaded"),
                        };
                    }
                };

                let page = A4Page::a4();
                let cfg = ParagraphConfig {
                    text: &text,
                    font: &font,
                    base_direction: dir,
                    px_size,
                    max_width: page.content_width(),
                    line_height,
                    alignment,
                };
                let lines = layout_paragraph(cfg);

                let glyph_count: u32 = lines.iter().map(|l| l.glyphs.len() as u32).sum();

                if let Some(ctx) = &self.ctx {
                    /* Page background (white) + light gray border. */
                    ctx.set_fill_style_str("#ffffff");
                    ctx.fill_rect(0.0, 0.0, page.width as f64, page.height as f64);
                    ctx.set_stroke_style_str("#cccccc");
                    ctx.set_line_width(1.0);
                    ctx.stroke_rect(0.5, 0.5, page.width as f64 - 1.0, page.height as f64 - 1.0);

                    /* Paint each line within the page margins. */
                    let margin_left = page.margin.left as f64;
                    let margin_top = page.margin.top as f64;
                    let content_width = page.content_width() as f64;
                    for line in &lines {
                        let direction_is_rtl =
                            matches!(line.direction, Some(ShapingDirection::Rtl));
                        let natural_w = line.natural_width as f64;
                        /* Decide x_origin per line:
                           - For RTL paragraphs whose line doesn't fill the
                             content width (last line, or non-justified
                             alignment), hang from the right margin.
                           - Center for explicit Center alignment.
                           - Otherwise (LTR, or any line that fills the box),
                             start at the left margin. */
                        let x_origin = if matches!(alignment, Alignment::Center) {
                            margin_left + (content_width - natural_w) / 2.0
                        } else if direction_is_rtl && natural_w < content_width - 0.5 {
                            margin_left + (content_width - natural_w)
                        } else {
                            margin_left
                        };
                        let baseline_y = margin_top + line.baseline_y as f64;
                        for g in &line.glyphs {
                            if g.glyph_id == 0 {
                                continue;
                            }
                            let raster = match font.rasterize_glyph(g.glyph_id as u16, px_size) {
                                Ok(r) => r,
                                Err(_) => continue,
                            };
                            let dx = x_origin + g.x as f64 + g.x_offset as f64;
                            let dy = baseline_y - g.y_offset as f64;
                            if let Err(e) = render::canvas2d_backend::paint_alpha_glyph(
                                ctx,
                                &raster,
                                dx,
                                dy,
                                [0, 0, 0],
                            ) {
                                return Event::Error {
                                    message: format!("paint: {e:?}"),
                                };
                            }
                        }
                    }
                }

                Event::PageRendered {
                    page_width: page.width,
                    page_height: page.height,
                    line_count: lines.len() as u32,
                    glyph_count,
                }
            }
        }
    }
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
