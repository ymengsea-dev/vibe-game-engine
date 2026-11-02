//! GPU context: wgpu instance, adapter, device/queue, and where frames go —
//! a window surface, or an offscreen texture read back to the CPU
//! ([`GpuContext::headless`]).

use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use engine_platform::Window;
use wgpu::util::DeviceExt;

use crate::config::{
    build_surface_config, choose_msaa_sample_count, choose_surface_format, should_reconfigure,
};
use crate::error::RendererError;
use crate::offscreen::{CapturedFrame, OFFSCREEN_FORMAT, OffscreenTarget};

/// The MSAA sample count requested for the main color pass. The actual
/// count is this clamped to what the adapter reports for the HDR target
/// format ([`choose_msaa_sample_count`]) — `1` (no MSAA) if 4x isn't
/// available. Making this configurable per project is future work.
pub const REQUESTED_MSAA_SAMPLE_COUNT: u32 = 4;

/// Owns the wgpu handles needed to render: the device/queue pair and where
/// finished frames go (see [`Presenter`]).
///
/// The adapter is not needed after initialization.
pub struct GpuContext {
    presenter: Presenter,
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

/// Where a [`GpuContext`]'s frames are shown.
enum Presenter {
    /// A window's swapchain. The instance and window are retained solely to
    /// recreate the surface if the OS invalidates it.
    Window {
        instance: wgpu::Instance,
        window: Arc<Window>,
        surface: RwLock<wgpu::Surface<'static>>,
    },
    /// No window: frames are read back for another process to display.
    Offscreen(OffscreenTarget),
}

/// A frame acquired for drawing: a swapchain image or the offscreen texture.
pub(crate) enum Frame {
    Surface(wgpu::SurfaceTexture),
    Offscreen(wgpu::Texture),
}

impl Frame {
    /// A view to draw the frame into.
    pub(crate) fn view(&self) -> wgpu::TextureView {
        let texture = match self {
            Frame::Surface(surface) => &surface.texture,
            Frame::Offscreen(texture) => texture,
        };
        texture.create_view(&wgpu::TextureViewDescriptor::default())
    }
}

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
            presenter: Presenter::Window {
                instance,
                window,
                surface: RwLock::new(surface),
            },
            device,
            queue,
            config,
            clear_color: DEFAULT_CLEAR_COLOR,
            msaa_sample_count,
        })
    }

    /// Initializes wgpu with no window: frames are drawn into an offscreen
    /// `width`×`height` texture and read back with
    /// [`GpuContext::take_frame`]. Every render path works as with a
    /// window. Used to stream a game into Studio's Scene View.
    ///
    /// # Errors
    ///
    /// Returns [`RendererError`] if adapter selection or the device request
    /// fails, or if either dimension is zero.
    pub fn headless(width: u32, height: u32) -> Result<Self, RendererError> {
        if width == 0 || height == 0 {
            return Err(RendererError::ZeroSize { width, height });
        }
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .map_err(|err| RendererError::NoSuitableAdapter(err.to_string()))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("VGE headless device"),
            ..Default::default()
        }))
        .map_err(|err| RendererError::DeviceRequest(err.to_string()))?;

        // Pipelines read the colour format and size from here, exactly as
        // with a window surface.
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            format: OFFSCREEN_FORMAT,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width,
            height,
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Opaque,
            view_formats: Vec::new(),
        };
        let msaa_sample_count = choose_msaa_sample_count(
            adapter
                .get_texture_format_features(crate::pipeline::HDR_TEXTURE_FORMAT)
                .flags,
            REQUESTED_MSAA_SAMPLE_COUNT,
        );
        tracing::info!(
            adapter = %adapter.get_info().name,
            width,
            height,
            msaa = msaa_sample_count,
            "headless GPU context initialized"
        );
        let target = OffscreenTarget::new(&device, width, height);
        Ok(Self {
            presenter: Presenter::Offscreen(target),
            device,
            queue,
            config,
            clear_color: DEFAULT_CLEAR_COLOR,
            msaa_sample_count,
        })
    }

    /// Whether frames go to an offscreen texture instead of a window.
    pub fn is_headless(&self) -> bool {
        matches!(self.presenter, Presenter::Offscreen(_))
    }

    /// The newest finished offscreen frame, without blocking; `None` for a
    /// windowed context or while no frame has finished since the last call.
    pub fn take_frame(&self) -> Option<CapturedFrame> {
        match &self.presenter {
            Presenter::Offscreen(target) => target.take(&self.device, false),
            Presenter::Window { .. } => None,
        }
    }

    /// Like [`GpuContext::take_frame`], but first waits for the GPU to
    /// finish every submitted frame.
    pub fn wait_for_frame(&self) -> Option<CapturedFrame> {
        match &self.presenter {
            Presenter::Offscreen(target) => target.take(&self.device, true),
            Presenter::Window { .. } => None,
        }
    }

    /// Offscreen frames dropped because the CPU had not read the previous
    /// ones yet (always `0` for a windowed context).
    pub fn skipped_frames(&self) -> u64 {
        match &self.presenter {
            Presenter::Offscreen(target) => target.skipped(),
            Presenter::Window { .. } => 0,
        }
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

    /// The window's presentable surface; `None` for a
    /// [`GpuContext::headless`] context.
    pub fn surface(&self) -> Option<impl std::ops::Deref<Target = wgpu::Surface<'static>> + '_> {
        match &self.presenter {
            Presenter::Window { surface, .. } => Some(read_surface(surface)),
            Presenter::Offscreen(_) => None,
        }
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
        match &mut self.presenter {
            Presenter::Window { surface, .. } => {
                read_surface(surface).configure(&self.device, &self.config);
            }
            Presenter::Offscreen(target) => {
                *target = OffscreenTarget::new(&self.device, width, height);
            }
        }
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

    /// Shows a previously-acquired frame (from
    /// [`GpuContext::acquire_frame`]) after its command buffer has been
    /// submitted: presents a swapchain image, or starts reading an
    /// offscreen frame back.
    pub(crate) fn present(&self, frame: Frame) {
        match (frame, &self.presenter) {
            (Frame::Surface(surface_texture), _) => self.queue.present(surface_texture),
            (Frame::Offscreen(_), Presenter::Offscreen(target)) => {
                target.capture(&self.device, &self.queue);
            }
            (Frame::Offscreen(_), Presenter::Window { .. }) => {}
        }
    }

    /// Acquires the frame to draw into, or `None` if this frame should be
    /// skipped. An offscreen context always has one.
    ///
    /// `Outdated` surfaces are reconfigured and retried once; `Lost`
    /// surfaces are recreated, configured, and retried once. Transient
    /// failures (`Timeout`, `Occluded`, `Validation`) are logged and skipped
    /// so callers can try again next frame.
    pub(crate) fn acquire_frame(&self) -> Option<Frame> {
        let (instance, window, surface) = match &self.presenter {
            Presenter::Window {
                instance,
                window,
                surface,
            } => (instance, window, surface),
            Presenter::Offscreen(target) => {
                return Some(Frame::Offscreen(target.texture().clone()));
            }
        };
        let first = read_surface(surface).get_current_texture();
        let texture = match surface_recovery(&first) {
            SurfaceRecovery::Ready | SurfaceRecovery::Skip => finish_acquire(first),
            SurfaceRecovery::Reconfigure => {
                tracing::debug!("surface outdated; reconfiguring");
                read_surface(surface).configure(&self.device, &self.config);
                let retry = read_surface(surface).get_current_texture();
                finish_acquire(retry)
            }
            SurfaceRecovery::Recreate => {
                tracing::debug!("surface lost; recreating");
                let fresh = match instance.create_surface(Arc::clone(window)) {
                    Ok(fresh) => fresh,
                    Err(err) => {
                        tracing::error!(error = %err, "could not recreate lost surface");
                        return None;
                    }
                };
                fresh.configure(&self.device, &self.config);
                *write_surface(surface) = fresh;
                let retry = read_surface(surface).get_current_texture();
                finish_acquire(retry)
            }
        };
        texture.map(Frame::Surface)
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
        let Some(frame) = self.acquire_frame() else {
            return Ok(());
        };

        let view = frame.view();

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
        self.present(frame);
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
        let Some(frame) = self.acquire_frame() else {
            return Ok(());
        };

        let view = frame.view();

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("VGE custom frame encoder"),
            });

        draw(&self.device, &self.queue, &mut encoder, &view);

        self.queue.submit(std::iter::once(encoder.finish()));
        self.present(frame);
        Ok(())
    }
}

fn finish_acquire(status: wgpu::CurrentSurfaceTexture) -> Option<wgpu::SurfaceTexture> {
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

fn read_surface<'a>(
    surface: &'a RwLock<wgpu::Surface<'static>>,
) -> RwLockReadGuard<'a, wgpu::Surface<'static>> {
    match surface.read() {
        Ok(surface) => surface,
        Err(poisoned) => {
            tracing::warn!("surface lock was poisoned; recovering it");
            poisoned.into_inner()
        }
    }
}

fn write_surface<'a>(
    surface: &'a RwLock<wgpu::Surface<'static>>,
) -> RwLockWriteGuard<'a, wgpu::Surface<'static>> {
    match surface.write() {
        Ok(surface) => surface,
        Err(poisoned) => {
            tracing::warn!("surface lock was poisoned; recovering it");
            poisoned.into_inner()
        }
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
