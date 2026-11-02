//! Rendering without a window: frames go into an offscreen texture and are
//! read back to the CPU, so another process (Studio's Scene View) can show
//! them.
//!
//! [`crate::GpuContext::headless`] creates a context whose frames land here
//! instead of on a window surface. Every render path (`render_scene`,
//! `render_with`, `render_clear`) works unchanged: they acquire a frame,
//! draw into its view, and present it. Presenting copies the texture into
//! one of two staging buffers, used in turn, so the GPU can render the next
//! frame while the CPU reads the last one. [`OffscreenTarget::take`] never
//! blocks; when both buffers are still busy a frame is skipped rather than
//! stalling the game.
//!
//! The row-padding maths is pure and unit-tested; the GPU parts need a live
//! adapter.

use std::sync::{Arc, Mutex, MutexGuard};

/// The texture format offscreen frames are rendered in. sRGB like a window
/// surface, and RGBA so read-back bytes need no channel swizzle.
pub const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Bytes per pixel of [`OFFSCREEN_FORMAT`].
const BYTES_PER_PIXEL: u32 = 4;

/// Staging buffers per target: one being read while the other is written.
const STAGING_SLOTS: usize = 2;

/// One finished frame read back from the GPU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedFrame {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Tightly packed sRGB RGBA8 pixels, row by row from the top
    /// (`width * height * 4` bytes).
    pub rgba: Vec<u8>,
}

/// Bytes per row of a texture-to-buffer copy `width` pixels wide: wgpu
/// requires each row to start on a [`wgpu::COPY_BYTES_PER_ROW_ALIGNMENT`]
/// boundary.
pub fn padded_bytes_per_row(width: u32) -> u32 {
    let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    (width * BYTES_PER_PIXEL).div_ceil(alignment) * alignment
}

/// Strips the per-row padding from a copy laid out with
/// [`padded_bytes_per_row`], returning `width * height * 4` packed bytes.
/// Rows missing from a too-short `padded` input are left out.
pub fn unpad_rows(padded: &[u8], width: u32, height: u32) -> Vec<u8> {
    let row = (width * BYTES_PER_PIXEL) as usize;
    let stride = padded_bytes_per_row(width) as usize;
    let mut packed = Vec::with_capacity(row * height as usize);
    for chunk in padded.chunks(stride).take(height as usize) {
        if let Some(pixels) = chunk.get(..row) {
            packed.extend_from_slice(pixels);
        }
    }
    packed
}

/// Where a staging buffer is in its copy → map → read cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotState {
    /// Unmapped and unused: may receive the next copy.
    Free,
    /// A copy was submitted and the buffer is waiting to be mapped.
    Mapping,
    /// Mapped and holding a finished frame.
    Ready,
    /// Mapping failed; the buffer is unmapped and reusable.
    Failed,
}

struct Slot {
    buffer: wgpu::Buffer,
    state: Arc<Mutex<SlotState>>,
    /// Order of the frame in this slot, so the newest one wins.
    sequence: u64,
}

/// Locks a slot state, recovering from a poisoned lock: the state is a
/// plain value, so the last write is still meaningful.
fn lock(state: &Mutex<SlotState>) -> MutexGuard<'_, SlotState> {
    match state.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

struct Staging {
    slots: Vec<Slot>,
    next_sequence: u64,
    skipped: u64,
}

/// An offscreen colour texture plus the staging buffers frames are read
/// back through.
pub(crate) struct OffscreenTarget {
    texture: wgpu::Texture,
    width: u32,
    height: u32,
    staging: Mutex<Staging>,
}

impl OffscreenTarget {
    /// A target `width`×`height` pixels (both nonzero; the caller checks).
    pub(crate) fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("VGE offscreen frame"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: OFFSCREEN_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let size = u64::from(padded_bytes_per_row(width)) * u64::from(height);
        let slots = (0..STAGING_SLOTS)
            .map(|_| Slot {
                buffer: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("VGE offscreen readback"),
                    size,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                }),
                state: Arc::new(Mutex::new(SlotState::Free)),
                sequence: 0,
            })
            .collect();
        Self {
            texture,
            width,
            height,
            staging: Mutex::new(Staging {
                slots,
                next_sequence: 0,
                skipped: 0,
            }),
        }
    }

    /// The texture frames are drawn into.
    pub(crate) fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    fn staging(&self) -> MutexGuard<'_, Staging> {
        match self.staging.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Copies the drawn frame into a free staging buffer and starts mapping
    /// it. Skips the frame (counted in [`OffscreenTarget::skipped`]) when
    /// every buffer is still busy.
    pub(crate) fn capture(&self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let mut staging = self.staging();
        let sequence = staging.next_sequence;
        let free = staging
            .slots
            .iter()
            .position(|slot| matches!(*lock(&slot.state), SlotState::Free | SlotState::Failed));
        let Some(index) = free else {
            staging.skipped += 1;
            return;
        };
        staging.next_sequence += 1;
        let slot = &mut staging.slots[index];
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("VGE offscreen readback encoder"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &slot.buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row(self.width)),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(std::iter::once(encoder.finish()));
        *lock(&slot.state) = SlotState::Mapping;
        slot.sequence = sequence;
        let state = Arc::clone(&slot.state);
        slot.buffer
            .map_async(wgpu::MapMode::Read, .., move |result| {
                *lock(&state) = if result.is_ok() {
                    SlotState::Ready
                } else {
                    SlotState::Failed
                };
            });
    }

    /// The newest finished frame, if one is ready, without blocking. Older
    /// finished frames are released unread. `wait` blocks until every
    /// submitted copy has finished first.
    pub(crate) fn take(&self, device: &wgpu::Device, wait: bool) -> Option<CapturedFrame> {
        let poll = if wait {
            wgpu::PollType::wait_indefinitely()
        } else {
            wgpu::PollType::Poll
        };
        if let Err(err) = device.poll(poll) {
            tracing::warn!(error = %err, "offscreen readback poll failed");
        }
        let staging = self.staging();
        let newest = staging
            .slots
            .iter()
            .filter(|slot| *lock(&slot.state) == SlotState::Ready)
            .max_by_key(|slot| slot.sequence)
            .map(|slot| slot.sequence)?;
        let mut frame = None;
        for slot in &staging.slots {
            if *lock(&slot.state) != SlotState::Ready {
                continue;
            }
            if slot.sequence == newest {
                match slot.buffer.get_mapped_range(..) {
                    Ok(view) => {
                        frame = Some(CapturedFrame {
                            width: self.width,
                            height: self.height,
                            rgba: unpad_rows(&view, self.width, self.height),
                        });
                    }
                    Err(err) => tracing::warn!(error = %err, "offscreen readback unreadable"),
                }
            }
            slot.buffer.unmap();
            *lock(&slot.state) = SlotState::Free;
        }
        frame
    }

    /// Frames dropped because both staging buffers were still busy.
    pub(crate) fn skipped(&self) -> u64 {
        self.staging().skipped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_padded_to_the_copy_alignment() {
        assert_eq!(padded_bytes_per_row(64), 256, "already aligned");
        assert_eq!(padded_bytes_per_row(1), 256);
        assert_eq!(padded_bytes_per_row(65), 512);
        assert_eq!(padded_bytes_per_row(1280), 1280 * 4);
        assert_eq!(padded_bytes_per_row(1366) % 256, 0);
    }

    #[test]
    fn unpadding_keeps_only_each_rows_pixels() {
        let (width, height) = (3u32, 2u32);
        let stride = padded_bytes_per_row(width) as usize;
        let mut padded = vec![0xEEu8; stride * height as usize];
        for row in 0..height as usize {
            for byte in 0..(width * 4) as usize {
                padded[row * stride + byte] = (row * 100 + byte) as u8;
            }
        }
        let packed = unpad_rows(&padded, width, height);
        assert_eq!(packed.len(), (width * height * 4) as usize);
        assert_eq!(
            &packed[..12],
            &(0..12).map(|b| b as u8).collect::<Vec<_>>()[..]
        );
        assert_eq!(
            packed[12], 100,
            "second row starts after the first row's pixels"
        );
        assert!(!packed.contains(&0xEE), "no padding bytes survive");
    }

    #[test]
    fn a_short_buffer_yields_only_its_complete_rows() {
        let stride = padded_bytes_per_row(2) as usize;
        let packed = unpad_rows(&vec![1u8; stride + 4], 2, 3);
        assert_eq!(packed.len(), 8, "one full row; the partial row is dropped");
    }

    /// Needs a GPU adapter, so it is opt-in:
    /// `cargo test -p engine_renderer -- --ignored headless`.
    #[test]
    #[ignore = "needs a GPU adapter"]
    fn a_headless_context_reads_back_the_drawn_frame_in_srgb() {
        let mut gpu = crate::GpuContext::headless(70, 3).expect("headless GPU");
        gpu.set_clear_color(wgpu::Color {
            r: 0.2,
            g: 1.0,
            b: 0.0,
            a: 1.0,
        });
        gpu.render_clear().expect("render");
        let frame = gpu.wait_for_frame().expect("a frame");
        assert_eq!((frame.width, frame.height), (70, 3));
        assert_eq!(frame.rgba.len(), 70 * 3 * 4, "padding removed");
        // Linear 0.2 encodes to sRGB 124, as on a window surface.
        assert_eq!(&frame.rgba[..4], &[124, 255, 0, 255]);
        assert_eq!(gpu.take_frame(), None, "each frame is handed out once");

        gpu.resize(8, 8).expect("resize");
        gpu.render_clear().expect("render");
        let resized = gpu.wait_for_frame().expect("a frame");
        assert_eq!((resized.width, resized.height), (8, 8));
    }
}
