//! Renders Studio UI to a PNG without a window, so a layout can be looked
//! at during development (test builds only).
//!
//! It runs the real theme and widgets for a few frames, then draws the
//! result with `egui-wgpu` into a headless GPU context's offscreen frame
//! and reads it back. The snapshot tests are `#[ignore]`d because they need
//! a GPU adapter:
//!
//! ```sh
//! VGE_SNAPSHOT_DIR=/tmp/shots cargo test -p engine_editor -- --ignored snapshot
//! ```

use std::path::{Path, PathBuf};

use engine_renderer::{GpuContext, OFFSCREEN_FORMAT};

/// Where snapshots are written: `$VGE_SNAPSHOT_DIR`, or the temp folder.
pub(crate) fn snapshot_dir() -> PathBuf {
    std::env::var_os("VGE_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// Draws `draw` into a `size` (points) screen at `scale` pixels per point
/// and writes it to `path` as a PNG.
pub(crate) fn render_png(
    path: &Path,
    size: egui::Vec2,
    scale: f32,
    draw: impl FnMut(&mut egui::Ui),
) -> Result<(), String> {
    render_png_with_weight(path, size, scale, crate::theme::UI_FONT_WEIGHT, draw)
}

/// [`render_png`] with the UI text at another font `weight`, to compare
/// weights before choosing one.
pub(crate) fn render_png_with_weight(
    path: &Path,
    size: egui::Vec2,
    scale: f32,
    weight: f32,
    mut draw: impl FnMut(&mut egui::Ui),
) -> Result<(), String> {
    let (width, height) = (
        (size.x * scale).round() as u32,
        (size.y * scale).round() as u32,
    );
    let gpu = GpuContext::headless(width, height).map_err(|err| err.to_string())?;
    let ctx = egui::Context::default();
    crate::theme::install(&ctx);
    ctx.set_fonts(crate::theme::font_definitions_with_weight(weight));
    ctx.set_pixels_per_point(scale);
    let mut renderer = egui_wgpu::Renderer::new(
        gpu.device(),
        OFFSCREEN_FORMAT,
        egui_wgpu::RendererOptions::default(),
    );
    // A few frames, so windows finish sizing and animations settle; every
    // frame's texture updates (the font atlas first) must be applied.
    let mut output = egui::FullOutput::default();
    for frame in 0..6 {
        output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                time: Some(f64::from(frame) * 0.5),
                ..Default::default()
            },
            &mut draw,
        );
        for (id, deltas) in &output.textures_delta.set {
            for delta in deltas {
                renderer.update_texture(gpu.device(), gpu.queue(), *id, delta);
            }
        }
        output.textures_delta.clear();
    }
    let primitives = ctx.tessellate(std::mem::take(&mut output.shapes), scale);
    let screen = egui_wgpu::ScreenDescriptor {
        size_in_pixels: [width, height],
        pixels_per_point: scale,
    };
    gpu.render_with(|device, queue, encoder, view| {
        renderer.update_buffers(device, queue, encoder, &primitives, &screen);
        let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("ui snapshot"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        let mut pass = pass.forget_lifetime();
        renderer.render(&mut pass, &primitives, &screen);
    })
    .map_err(|err| err.to_string())?;
    let frame = gpu
        .wait_for_frame()
        .ok_or_else(|| "no frame was read back".to_owned())?;
    image::save_buffer(
        path,
        &frame.rgba,
        frame.width,
        frame.height,
        image::ColorType::Rgba8,
    )
    .map_err(|err| err.to_string())
}
