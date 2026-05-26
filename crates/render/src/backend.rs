//! Renderer backend selection (PHASE_3_RENDER_RTL.md §9, P3-5).
//!
//! Worker-safe WebGPU detection: the engine runs inside a Web Worker, where
//! `web_sys::window()` is `None` and would panic. Detection goes through
//! `js_sys::global()` → `WorkerGlobalScope` instead, then asks `wgpu` for a
//! GPU adapter + device. Any failure falls back to the Canvas2D backend.

use wasm_bindgen::JsCast;

/// Which renderer the engine drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendererBackend {
    /// Vello — GPU compute renderer over WebGPU.
    Vello,
    /// Canvas2D — CPU fallback.
    Canvas2d,
}

impl RendererBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            RendererBackend::Vello => "vello",
            RendererBackend::Canvas2d => "canvas2d",
        }
    }
}

/// Detect the best available backend from inside a Web Worker.
///
/// Worker-safe by construction — uses `js_sys::global()` rather than
/// `web_sys::window()` (which is `None` in a Worker).
///
/// **CLAUDE.md invariant**: "Canvas2D remains the active renderer." The
/// Vello/WebGPU path is plumbed end-to-end and the `wgpu` + `vello` stack
/// is reachable so the linker retains them (true WASM size stays
/// observable). The path is intentionally **not** selected for the
/// interactive editor yet — Vello's `render_document` does not call
/// `OffscreenCanvas::set_width`/`set_height` to resize the backing store
/// for multi-page documents, paints inline images as placeholders only,
/// and skips the dirty-region clip optimisation. Selecting it here gives
/// a 300×150-defaulted canvas with squashed unreadable text the moment
/// content exceeds one page (the real-world Phase 6 multi-page bug). The
/// Vello activation work is tracked as Backlog #4 / `BACKLOG.md` § Vello
/// renderer; until that closes, Canvas2D is the right pick. The wgpu
/// device probe is kept (commented inline) for documentation but no
/// longer drives the return value.
pub async fn detect_backend() -> RendererBackend {
    /* Confirm we are in a Worker scope — never touch `web_sys::window()`. */
    if js_sys::global()
        .dyn_into::<web_sys::WorkerGlobalScope>()
        .is_err()
    {
        return RendererBackend::Canvas2d;
    }
    /* Backlog #4 — opt-in Vello activation, gated on the WebGPU adapter
    actually being acquirable in this worker. Multi-page rendering on
    the Vello path is still the known Phase 6 bug; single-page works. */
    if request_gpu_device().await {
        RendererBackend::Vello
    } else {
        RendererBackend::Canvas2d
    }
}

/// Try to acquire a WebGPU adapter + device via `wgpu`. `wgpu`'s WebGPU
/// backend reads `navigator.gpu` from the current (worker) global scope.
///
/// Backlog #4 — `detect_backend` calls this to gate Vello activation.
async fn request_gpu_device() -> bool {
    let instance = wgpu::Instance::default();
    let adapter = match instance
        .request_adapter(&wgpu::RequestAdapterOptions::default())
        .await
    {
        Ok(adapter) => adapter,
        Err(_) => return false,
    };
    adapter
        .request_device(&wgpu::DeviceDescriptor::default())
        .await
        .is_ok()
}
