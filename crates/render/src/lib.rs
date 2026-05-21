//! `render` — backend-agnostic display list + Canvas2D backend.
//!
//! Phase 3 batch 1: `scene` (display list + page scene builder), `atlas`
//! (LRU glyph cache), `canvas2d_backend` (display-list interpreter). The
//! Vello (WebGPU) backend lands in batch 2 (PHASE_3_RENDER_RTL.md §9.2).

pub mod atlas;
pub mod backend;
pub mod canvas2d_backend;
pub mod dirty;
pub mod scene;
mod synth;
pub mod vello_backend;
