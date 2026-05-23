//! Vello (WebGPU) backend — encodes a `DisplayList` into a `vello::Scene` and
//! presents it through a `wgpu` surface (PHASE_3_RENDER_RTL.md §9.2).
//!
//! Phase 3 P3-4: the full plumbing is implemented and made reachable so the
//! linker retains the `wgpu` + `vello` stack (revealing the true WASM size).
//! Canvas2D remains the active render path — `VelloRenderer` is constructed
//! only via `Engine::init_vello`, which the worker does not call yet.

use crate::scene::{DisplayCmd, DisplayList};
use kurbo::{Affine, Stroke};
use peniko::{Color, Fill, FontData};
use vello::util::{RenderContext, RenderSurface};
use vello::{AaConfig, Glyph, RenderParams, Renderer, Scene};

/// Shear factor for faux italic — mirrors `render::synth::SHEAR` (Sprint 6),
/// applied via Vello's `glyph_transform` so each glyph shape leans right.
const FAUX_ITALIC_SHEAR: f64 = 0.22;

/// Faux-bold x offset as a fraction of `px_size`. A small second draw on top
/// of the glyphs approximates the alpha-mask dilation that the Canvas2D path
/// does via `render::synth::embolden` (Sprint 6) — Vello has no real bold face
/// to switch to.
const FAUX_BOLD_DX_FRAC: f32 = 0.04;

/// Encode a slice of `DisplayCmd`s into a `vello::Scene`.
///
/// `resolve_font` maps a `FontId` to a `peniko::FontData` (the post-rename
/// vello font handle). Vello does its own GPU-side glyph caching, so there is
/// no `GlyphAtlas` on this path.
pub fn render_vello(
    scene: &mut Scene,
    cmds: &[DisplayCmd],
    resolve_font: impl Fn(&str) -> Option<FontData>,
) {
    let mut transform = Affine::IDENTITY;
    let mut transform_stack: Vec<Affine> = Vec::new();
    let mut clip_depth: u32 = 0;

    for cmd in cmds {
        match cmd {
            DisplayCmd::FillRect { rect, paint } => {
                scene.fill(Fill::NonZero, transform, &paint.brush, None, rect);
            }
            DisplayCmd::StrokeRect { rect, paint, width } => {
                scene.stroke(&Stroke::new(*width), transform, &paint.brush, None, rect);
            }
            DisplayCmd::DrawGlyphRun(run) => {
                let Some(font) = resolve_font(&run.font) else {
                    continue;
                };
                /* Vello has no real Bold / Italic face wired here, so faux
                synthesis matches what the Canvas2D backend does via
                `render::synth` (Sprint 6): a per-glyph skew for italic + a
                second offset pass for bold. */
                let glyph_xform = if run.faux_italic {
                    Some(Affine::skew(FAUX_ITALIC_SHEAR, 0.0))
                } else {
                    None
                };
                scene
                    .draw_glyphs(&font)
                    .font_size(run.px_size)
                    .transform(transform)
                    .glyph_transform(glyph_xform)
                    .brush(&run.paint.brush)
                    .draw(
                        Fill::NonZero,
                        run.glyphs.iter().map(|g| Glyph {
                            id: u32::from(g.glyph_id),
                            x: g.x as f32,
                            y: g.y as f32,
                        }),
                    );
                if run.faux_bold {
                    let dx = run.px_size * FAUX_BOLD_DX_FRAC;
                    scene
                        .draw_glyphs(&font)
                        .font_size(run.px_size)
                        .transform(transform)
                        .glyph_transform(glyph_xform)
                        .brush(&run.paint.brush)
                        .draw(
                            Fill::NonZero,
                            run.glyphs.iter().map(|g| Glyph {
                                id: u32::from(g.glyph_id),
                                x: g.x as f32 + dx,
                                y: g.y as f32,
                            }),
                        );
                }
            }
            DisplayCmd::DrawPageCard { rect } => {
                /* UI polish — Vello has no built-in CSS-style shadow
                primitive; paint the white page only. The full shadow
                ships with the Vello activation (BACKLOG #4); Canvas2D
                is the active renderer until then. */
                let white = peniko::Brush::Solid(peniko::Color::from_rgba8(0xff, 0xff, 0xff, 0xff));
                scene.fill(Fill::NonZero, transform, &white, None, rect);
            }
            DisplayCmd::DrawImage { rect, rel_id: _ } => {
                /* Phase 7 — Vello path paints a placeholder rectangle.
                Full image decoding through Vello's `Image` resource
                ships with the dedicated Vello-renderer activation
                (BACKLOG #4); the placeholder keeps the layout footprint
                correct on the WebGPU path. */
                let placeholder =
                    peniko::Brush::Solid(peniko::Color::from_rgba8(0xdd, 0xdd, 0xdd, 0xff));
                scene.fill(Fill::NonZero, transform, &placeholder, None, rect);
            }
            DisplayCmd::PushTransform(affine) => {
                transform_stack.push(transform);
                transform *= *affine;
            }
            DisplayCmd::PopTransform => {
                transform = transform_stack.pop().unwrap_or(Affine::IDENTITY);
            }
            DisplayCmd::PushClip { rect } => {
                scene.push_layer(Fill::NonZero, peniko::Mix::Normal, 1.0, transform, rect);
                clip_depth += 1;
            }
            DisplayCmd::PopClip => {
                if clip_depth > 0 {
                    clip_depth -= 1;
                    scene.pop_layer();
                }
            }
        }
    }
}

/// Wrap leaked-`'static` font bytes as a [`peniko::FontData`] for the Vello
/// glyph path. Zero-copy: the bytes already live for the engine's lifetime, so
/// the `Blob` borrows them through an `Arc` rather than copying the face every
/// frame. Index `0` — the first (only) face in the file.
pub fn font_data(bytes: &'static [u8]) -> FontData {
    FontData::new(peniko::Blob::new(std::sync::Arc::new(bytes)), 0)
}

/// A persistent WebGPU device + Vello renderer bound to one `OffscreenCanvas`.
///
/// P3-4: this exists to make the `wgpu` + `vello` GPU stack reachable so the
/// linker retains it and the true WASM size is observable. Canvas2D stays the
/// active render path; `Engine::init_vello` is the reachability root and is
/// not called from the worker yet.
///
/// Built on `vello::util::{RenderContext, RenderSurface}`: Vello renders via a
/// compute shader into an intermediate `Rgba8Unorm` texture, then blits that to
/// the surface with `wgpu::util::TextureBlitter` — the surface texture cannot
/// be bound as a compute-shader storage target.
pub struct VelloRenderer {
    context: RenderContext,
    surface: RenderSurface<'static>,
    renderer: Renderer,
    scene: Scene,
}

impl VelloRenderer {
    /// Set up a WebGPU surface + device + Vello renderer for `canvas`.
    /// WebGPU-surface creation is wasm-only.
    #[cfg(target_arch = "wasm32")]
    pub async fn new(canvas: web_sys::OffscreenCanvas) -> Result<Self, String> {
        let width = canvas.width();
        let height = canvas.height();
        let mut context = RenderContext::new();
        let surface = context
            .create_surface(
                wgpu::SurfaceTarget::OffscreenCanvas(canvas),
                width,
                height,
                wgpu::PresentMode::AutoVsync,
            )
            .await
            .map_err(|e| format!("create_surface failed: {e:?}"))?;
        let renderer = Renderer::new(
            &context.devices[surface.dev_id].device,
            vello::RendererOptions::default(),
        )
        .map_err(|e| format!("vello::Renderer::new failed: {e:?}"))?;
        Ok(Self {
            context,
            surface,
            renderer,
            scene: Scene::new(),
        })
    }

    /// Encode `list` into the scene, render it to the intermediate texture, and
    /// blit the result onto the surface.
    pub fn render(
        &mut self,
        list: &DisplayList,
        resolve_font: impl Fn(&str) -> Option<FontData>,
    ) -> Result<(), String> {
        self.scene.reset();
        render_vello(&mut self.scene, &list.cmds, resolve_font);

        let device_handle = &self.context.devices[self.surface.dev_id];
        let params = RenderParams {
            base_color: Color::from_rgba8(0xff, 0xff, 0xff, 0xff),
            width: self.surface.config.width,
            height: self.surface.config.height,
            antialiasing_method: AaConfig::Area,
        };
        self.renderer
            .render_to_texture(
                &device_handle.device,
                &device_handle.queue,
                &self.scene,
                &self.surface.target_view,
                &params,
            )
            .map_err(|e| format!("render_to_texture failed: {e:?}"))?;

        let surface_texture = match self.surface.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(tex)
            | wgpu::CurrentSurfaceTexture::Suboptimal(tex) => tex,
            _ => return Err("get_current_texture failed".to_string()),
        };
        let target_view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device_handle
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        self.surface.blitter.copy(
            &device_handle.device,
            &mut encoder,
            &self.surface.target_view,
            &target_view,
        );
        device_handle.queue.submit([encoder.finish()]);
        surface_texture.present();
        Ok(())
    }
}
