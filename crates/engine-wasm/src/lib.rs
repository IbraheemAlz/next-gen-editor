//! `engine-wasm` — `#[wasm_bindgen]` surface for the engine.
//!
//! Phase 1 PoC: Engine constructor paints a green sentinel square on the
//! transferred `OffscreenCanvas`; `dispatch(Command::Ping)` returns
//! `Event::Pong`. Real engine state (held canvas, document tree, etc.) lands
//! in later weeks.

use bridge::{Command, Event};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(start)]
pub fn boot() {
    console_error_panic_hook::set_once();
}

/// Public engine surface exposed to JS via `wasm-bindgen`.
#[wasm_bindgen]
pub struct Engine {}

#[wasm_bindgen]
impl Engine {
    /// Construct from an `OffscreenCanvas` transferred from the main thread.
    ///
    /// PoC sentinel: paints a 10×10 green square at (0, 0) to prove the
    /// JS → WASM → canvas binding chain is intact.
    #[wasm_bindgen(constructor)]
    pub fn new(canvas: web_sys::OffscreenCanvas) -> Result<Engine, JsValue> {
        let ctx_obj = canvas
            .get_context("2d")?
            .ok_or_else(|| JsValue::from_str("OffscreenCanvas 2d context unavailable"))?;
        let ctx: web_sys::OffscreenCanvasRenderingContext2d = ctx_obj.dyn_into()?;
        ctx.set_fill_style_str("#00ff00");
        ctx.fill_rect(0.0, 0.0, 10.0, 10.0);
        Ok(Engine {})
    }

    /// Decode a `Command`, apply it, encode the resulting `Event`.
    pub async fn dispatch(&mut self, cmd: JsValue) -> Result<JsValue, JsValue> {
        let cmd: Command = serde_wasm_bindgen::from_value(cmd)
            .map_err(|e| JsValue::from_str(&format!("decode command: {e}")))?;
        let evt: Event = self.apply(cmd).await;
        serde_wasm_bindgen::to_value(&evt)
            .map_err(|e| JsValue::from_str(&format!("encode event: {e}")))
    }
}

impl Engine {
    async fn apply(&mut self, cmd: Command) -> Event {
        match cmd {
            Command::Ping => Event::Pong,
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
        let mut engine = Engine {};
        let cmd_js = serde_wasm_bindgen::to_value(&Command::Ping).expect("encode ping");
        let evt_js = engine
            .dispatch(cmd_js)
            .await
            .expect("dispatch should succeed");
        let evt: Event = serde_wasm_bindgen::from_value(evt_js).expect("decode event");
        assert!(matches!(evt, Event::Pong), "expected Pong, got {evt:?}");
    }
}
