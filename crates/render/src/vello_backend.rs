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
                scene
                    .draw_glyphs(&font)
                    .font_size(run.px_size)
                    .transform(transform)
                    .brush(&run.paint.brush)
                    .draw(
                        Fill::NonZero,
                        run.glyphs.iter().map(|g| Glyph {
                            id: u32::from(g.glyph_id),
                            x: g.x as f32,
                            y: g.y as f32,
                        }),
                    );
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
