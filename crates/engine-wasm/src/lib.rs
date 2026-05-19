//! `engine-wasm` — `#[wasm_bindgen]` surface for the engine.
//!
//! Phase 1 weeks 5–6: load a TTF font, paint a single glyph from `swash`'s
//! alpha-mask raster output to the `OffscreenCanvas` via `putImageData`.
//! The green sentinel from week 4 is replaced by a white-cleared background
//! over which the glyph is drawn in black.

use bridge::{Command, Event, FontMetrics as BridgeMetrics};
use std::collections::HashMap;
use std::sync::Arc;
use text_pipeline::LoadedFont;
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
    /// Clears the canvas to white so the subsequent glyph paint is visible.
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

/// Where on the canvas the PoC paints the glyph. Baseline pen position.
const POC_BASELINE_X: f64 = 50.0;
const POC_BASELINE_Y: f64 = 200.0;

impl Engine {
    async fn apply(&mut self, cmd: Command) -> Event {
        match cmd {
            Command::Ping => Event::Pong,

            Command::LoadFont { id, bytes } => match LoadedFont::parse(id.clone(), bytes) {
                Ok(font) => {
                    /* Report unscaled (in font units) metrics by passing 1.0 for px_size,
                       which gives us per-em values; multiply by px_size in JS to scale. */
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::*;

    wasm_bindgen_test_configure!(run_in_browser);

    /// Ping → Pong round-trip without binding to a canvas. The constructor
    /// is exercised separately by the browser-side PoC; this test isolates
    /// the bridge serde path.
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
