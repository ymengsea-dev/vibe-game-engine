//! GPU context: wgpu instance, adapter, device/queue, and window surface.

use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use engine_platform::Window;
use wgpu::util::DeviceExt;

use crate::config::{
    build_surface_config, choose_msaa_sample_count, choose_surface_format, should_reconfigure,
};
use crate::error::RendererError;

/// The MSAA sample count requested for the main color pass. The actual
/// count is this clamped to what the adapter reports for the HDR target
/// format ([`choose_msaa_sample_count`]) — `1` (no MSAA) if 4x isn't
/// available. Making this configurable per project is future work.
pub const REQUESTED_MSAA_SAMPLE_COUNT: u32 = 4;

/// Owns the wgpu handles needed to render into a window: the device/queue
/// pair and the window's presentable surface.
///
/// The instance and window are retained solely to recreate the surface if
/// the OS invalidates it. The adapter is not needed after initialization.
pub struct GpuContext {
    instance: wgpu::Instance,
    window: Arc<Window>,
    surface: RwLock<wgpu::Surface<'static>>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    clear_color: wgpu::Color,
    msaa_sample_count: u32,
}

/// Default clear color: a muted blue-grey, distinguishable from both a
/// crashed-black window and a plain white one so a clean first frame is
/// obviously "the engine drew something" rather than "nothing happened".
const DEFAULT_CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 0.05,
    g: 0.07,
    b: 0.12,
    a: 1.0,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SurfaceRecovery {
    Ready,
    Skip,
    Reconfigure,
    Recreate,
}

fn surface_recovery(status: &wgpu::CurrentSurfaceTexture) -> SurfaceRecovery {
    match status {
        wgpu::CurrentSurfaceTexture::Success(_) | wgpu::CurrentSurfaceTexture::Suboptimal(_) => {
            SurfaceRecovery::Ready
        }
        wgpu::CurrentSurfaceTexture::Outdated => SurfaceRecovery::Reconfigure,
        wgpu::CurrentSurfaceTexture::Lost => SurfaceRecovery::Recreate,
        wgpu::CurrentSurfaceTexture::Timeout
        | wgpu::CurrentSurfaceTexture::Occluded
        | wgpu::CurrentSurfaceTexture::Validation => SurfaceRecovery::Skip,
    }
}

impl GpuContext {
    /// Initializes wgpu against `window`: creates an instance, picks an
    /// adapter compatible with the window's surface, requests a device and
    /// queue, and configures the surface at the window's current size.
    ///
    /// # Errors
    ///
    /// Returns [`RendererError`] if surface creation, adapter selection,
    /// or device request fails, or if the window's current size is zero in
    /// either dimension.
    pub fn new(window: Arc<Window>) -> Result<Self, RendererError> {
        let size = window.inner_size();

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());

        let surface = instance
            .create_surface(Arc::clone(&window))
            .map_err(|err| RendererError::SurfaceCreation(err.to_string()))?;

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .map_err(|err| RendererError::NoSuitableAdapter(err.to_string()))?;

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("VGE device"),
            ..Default::default()
        }))
        .map_err(|err| RendererError::DeviceRequest(err.to_string()))?;

        let capabilities = surface.get_capabilities(&adapter);
        let format = choose_surface_format(&capabilities.formats)?;
        let config = build_surface_config(&capabilities, format, size.width, size.height)?;
        surface.configure(&device, &config);

        let msaa_sample_count = choose_msaa_sample_count(
            adapter
                .get_texture_format_features(crate::pipeline::HDR_TEXTURE_FORMAT)
                .flags,
            REQUESTED_MSAA_SAMPLE_COUNT,
        );

        tracing::info!(
            adapter = %adapter.get_info().name,
            format = ?format,
            width = size.width,
            height = size.height,
            msaa = msaa_sample_count,
            "GPU context initialized"
        );

        Ok(Self {
            instance,
            window,
            surface: RwLock::new(surface),
            device,
            queue,
            config,
            clear_color: DEFAULT_CLEAR_COLOR,
            msaa_sample_count,
        })
    }

    /// The wgpu device, for creating pipelines, buffers, and textures.
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// The wgpu queue, for submitting command buffers and writing to
    /// buffers/textures.
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// The window's presentable surface.
    pub fn surface(&self) -> impl std::ops::Deref<Target = wgpu::Surface<'static>> + '_ {
        self.read_surface()
    }

    /// The surface's current configuration.
    pub fn config(&self) -> &wgpu::SurfaceConfiguration {
        &self.config
    }

    /// The MSAA sample count the main color pass and its pipelines use —
    /// `4` where the adapter supports it for the HDR target format, `1`
    /// (no MSAA) otherwise. Stable for this context's lifetime, so
    /// pipelines built against it once at startup stay valid.
    pub fn msaa_sample_count(&self) -> u32 {
        self.msaa_sample_count
    }

    /// Reconfigures the surface at a new size (e.g. after a window resize).
    ///
    /// Two edge cases are handled explicitly rather than left to wgpu:
    /// - **Zero size** (e.g. the window was minimized): rejected with
    ///   [`RendererError::ZeroSize`] instead of configuring an invalid
    ///   surface. The surface keeps its last valid configuration, so
    ///   restoring the window to a real size later just works.
    /// - **Unchanged size** (window managers routinely fire redundant
    ///   resize events, e.g. duplicate `Resized` events at startup):
    ///   skipped without touching the surface, since reconfigure is not
    ///   free.
    ///
    /// # Errors
    ///
    /// Returns [`RendererError::ZeroSize`] if either dimension is zero.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), RendererError> {
        if width == 0 || height == 0 {
            return Err(RendererError::ZeroSize { width, height });
        }
        if !should_reconfigure((self.config.width, self.config.height), (width, height)) {
            return Ok(());
        }
        self.config.width = width;
        self.config.height = height;
        self.read_surface().configure(&self.device, &self.config);
        Ok(())
    }

    /// Sets the color used to clear the frame in [`GpuContext::render_clear`]
    /// and [`crate::Pipeline`]-based rendering.
    pub fn set_clear_color(&mut self, color: wgpu::Color) {
        self.clear_color = color;
    }

    /// The color frames are cleared to.
    pub(crate) fn clear_color(&self) -> wgpu::Color {
        self.clear_color
    }

    /// Presents a previously-acquired surface texture (from
    /// [`GpuContext::acquire_frame`]) after its command buffer has been
    /// submitted.
    pub(crate) fn present(&self, surface_texture: wgpu::SurfaceTexture) {
        self.queue.present(surface_texture);
    }

    /// Acquires the current surface texture, or `None` if this frame
    /// should be skipped.
    ///
    /// `Outdated` surfaces are reconfigured and retried once; `Lost`
    /// surfaces are recreated, configured, and retried once. Transient
    /// failures (`Timeout`, `Occluded`, `Validation`) are logged and skipped
    /// so callers can try again next frame.
    pub(crate) fn acquire_frame(&self) -> Option<wgpu::SurfaceTexture> {
        let first = self.read_surface().get_current_texture();
        match surface_recovery(&first) {
            SurfaceRecovery::Ready | SurfaceRecovery::Skip => self.finish_acquire(first),
            SurfaceRecovery::Reconfigure => {
                tracing::debug!("surface outdated; reconfiguring");
                self.read_surface().configure(&self.device, &self.config);
                let retry = self.read_surface().get_current_texture();
                self.finish_acquire(retry)
            }
            SurfaceRecovery::Recreate => {
                tracing::debug!("surface lost; recreating");
                let surface = match self.instance.create_surface(Arc::clone(&self.window)) {
                    Ok(surface) => surface,
                    Err(err) => {
                        tracing::error!(error = %err, "could not recreate lost surface");
                        return None;
                    }
                };
                surface.configure(&self.device, &self.config);
                *self.write_surface() = surface;
                let retry = self.read_surface().get_current_texture();
                self.finish_acquire(retry)
            }
        }
    }

    fn finish_acquire(&self, status: wgpu::CurrentSurfaceTexture) -> Option<wgpu::SurfaceTexture> {
        match status {
            wgpu::CurrentSurfaceTexture::Success(texture) => Some(texture),
            wgpu::CurrentSurfaceTexture::Suboptimal(texture) => {
                tracing::debug!("suboptimal surface texture acquired; presenting anyway");
                Some(texture)
            }
            skip @ (wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Outdated
            | wgpu::CurrentSurfaceTexture::Lost
            | wgpu::CurrentSurfaceTexture::Validation) => {
                tracing::debug!(?skip, "skipping frame: surface texture unavailable");
                None
            }
        }
    }

    fn read_surface(&self) -> RwLockReadGuard<'_, wgpu::Surface<'static>> {
        match self.surface.read() {
            Ok(surface) => surface,
            Err(poisoned) => {
                tracing::warn!("surface lock was poisoned; recovering it");
                poisoned.into_inner()
            }
        }
    }

    fn write_surface(&self) -> RwLockWriteGuard<'_, wgpu::Surface<'static>> {
        match self.surface.write() {
            Ok(surface) => surface,
            Err(poisoned) => {
                tracing::warn!("surface lock was poisoned; recovering it");
                poisoned.into_inner()
            }
        }
    }

    /// Creates a GPU buffer initialized with `data`, usable as a uniform
    /// (and as a copy destination, for later updates via
    /// [`GpuContext::write_uniform_buffer`]).
    ///
    /// `T` must be [`bytemuck::Pod`] (safely castable to bytes) — see
    /// [`crate::CameraUniform`] for the pattern GPU-layout structs follow.
    pub fn create_uniform_buffer<T: bytemuck::Pod>(&self, label: &str, data: &T) -> wgpu::Buffer {
        self.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytemuck::bytes_of(data),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            })
    }

    /// Overwrites `buffer`'s contents with `data`, starting at offset 0.
    ///
    /// `buffer` must have been created large enough to hold `data` (e.g.
    /// via [`GpuContext::create_uniform_buffer`] with a same-sized or
    /// larger value) — wgpu validates this and will panic on mismatch.
    pub fn write_uniform_buffer<T: bytemuck::Pod>(&self, buffer: &wgpu::Buffer, data: &T) {
        self.queue.write_buffer(buffer, 0, bytemuck::bytes_of(data));
    }

    /// Renders one frame: acquires the current surface texture, clears it
    /// to [`GpuContext::set_clear_color`]'s color, and presents.
    ///
    /// No geometry is drawn — for that, use a [`crate::Pipeline`] and
    /// [`GpuContext::render_scene`] instead. This stays around as the
    /// smallest possible "something visibly happened" frame (e.g. for an
    /// app with no scene loaded yet).
    ///
    /// `Outdated` and `Lost` surface errors are recovered once before the
    /// frame is skipped. Other transient acquisition failures are logged
    /// and skipped — see the `acquire_frame` internals for details.
    ///
    /// # Errors
    ///
    /// This currently never returns `Err` (all failure modes are
    /// recoverable skips), but returns `Result` since surface
    /// loss/recreation handling is expected to grow real error paths.
    pub fn render_clear(&self) -> Result<(), RendererError> {
        let Some(surface_texture) = self.acquire_frame() else {
            return Ok(());
        };

        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("VGE frame encoder"),
            });

        {
            let _render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("VGE clear pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // Render pass dropped here, ending it: nothing to draw yet.
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        self.present(surface_texture);
        Ok(())
    }

    /// Acquires the current frame, hands `draw` the device/queue/encoder/
    /// view to record into, then submits and presents.
    ///
    /// The generic counterpart to [`GpuContext::render_clear`] and
    /// [`crate::Pipeline`]-based [`GpuContext::render_scene`] — for
    /// callers that need to draw something this crate has no built-in
    /// pipeline for (e.g. `engine_editor`'s egui integration) without
    /// duplicating the acquire/submit/present boilerplate those already
    /// handle. Keeps `engine_renderer` from needing to know anything
    /// about what `draw` actually draws.
    ///
    /// An empty `draw` still clears nothing and presents whatever was
    /// already in the acquired texture — callers are expected to fully
    /// paint `view` themselves (e.g. via a `LoadOp::Clear` in their own
    /// render pass), the same responsibility [`GpuContext::render_scene`]'s
    /// caller-supplied pipeline has.
    ///
    /// Skip/error handling for surface texture acquisition matches
    /// [`GpuContext::render_clear`] — see its docs for the full list of
    /// transient conditions treated as "skip this frame".
    ///
    /// # Errors
    ///
    /// This currently never returns `Err` (all failure modes are
    /// recoverable skips), but returns `Result` for the same reason
    /// [`GpuContext::render_clear`] does.
    pub fn render_with(
        &self,
        draw: impl FnOnce(&wgpu::Device, &wgpu::Queue, &mut wgpu::CommandEncoder, &wgpu::TextureView),
    ) -> Result<(), RendererError> {
        let Some(surface_texture) = self.acquire_frame() else {
            return Ok(());
        };

        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("VGE custom frame encoder"),
            });

        draw(&self.device, &self.queue, &mut encoder, &view);

        self.queue.submit(std::iter::once(encoder.finish()));
        self.present(surface_texture);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_failures_choose_the_required_recovery() {
        assert_eq!(
            surface_recovery(&wgpu::CurrentSurfaceTexture::Outdated),
            SurfaceRecovery::Reconfigure,
        );
        assert_eq!(
            surface_recovery(&wgpu::CurrentSurfaceTexture::Lost),
            SurfaceRecovery::Recreate,
        );
        assert_eq!(
            surface_recovery(&wgpu::CurrentSurfaceTexture::Timeout),
            SurfaceRecovery::Skip,
        );
    }
}
