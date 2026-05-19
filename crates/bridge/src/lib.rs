//! `bridge` — RPC command/event types shared between the WASM engine and TS.
//!
//! Phase 1 weeks 5–6 subset: Ping/Pong + font load + glyph raster. Full
//! schema lands in Phase 2 per PHASE_2_BRIDGE_MEMORY.md §4–§5.

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

    /// Rasterize and paint `ch` with `font_id` at the given pixel size.
    RasterizeGlyph {
        font_id: String,
        ch: String,
        px_size: f32,
    },
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
