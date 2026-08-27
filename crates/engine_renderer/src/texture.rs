//! Texture loading: image decode, GPU upload, and sampling.

use wgpu::util::DeviceExt;

use crate::error::RendererError;
use crate::gpu::GpuContext;

/// A GPU-resident texture: the texture itself, a default view, and a
/// sampler configured for it.
///
/// One [`wgpu::Sampler`] per [`Texture`] rather than a shared sampler pool
/// — simple and correct for now; sampler reuse is a future perf pass once
/// there are enough textures for it to matter.
pub struct Texture {
    /// The GPU texture resource.
    pub texture: wgpu::Texture,
    /// A full view over `texture` (all mips, all layers).
    pub view: wgpu::TextureView,
    /// Sampler: linear filtering, clamp-to-edge addressing, no mipmaps.
    pub sampler: wgpu::Sampler,
}

/// Decodes `bytes` (any format the `image` crate supports: PNG, JPEG,
/// GIF, BMP, ...) into raw RGBA8 pixel data.
///
/// Split out from GPU upload so decoding — the part that can fail on
/// untrusted input — is unit-testable without a real device.
///
/// # Errors
///
/// Returns [`RendererError::ImageDecode`] if `bytes` isn't a valid,
/// supported image. Treat all externally sourced image bytes (assets,
/// downloads, ...) as untrusted and handle this error rather than assuming
/// success.
pub fn decode_rgba8(bytes: &[u8]) -> Result<image::RgbaImage, RendererError> {
    image::load_from_memory(bytes)
        .map(|img| img.to_rgba8())
        .map_err(|err| RendererError::ImageDecode(err.to_string()))
}

impl GpuContext {
    /// Decodes `bytes` and uploads it as a new [`Texture`].
    ///
    /// # Errors
    ///
    /// Returns [`RendererError::ImageDecode`] if `bytes` can't be decoded
    /// (see [`decode_rgba8`]).
    pub fn create_texture_from_bytes(
        &self,
        label: &str,
        bytes: &[u8],
    ) -> Result<Texture, RendererError> {
        let rgba = decode_rgba8(bytes)?;
        let (width, height) = rgba.dimensions();
        Ok(self.create_texture_from_rgba(label, width, height, &rgba))
    }

    /// Uploads already-decoded RGBA8 pixel data as a new [`Texture`].
    ///
    /// `rgba` must contain exactly `width * height * 4` bytes (tightly
    /// packed, no row padding) — wgpu validates this and will panic on
    /// mismatch. Useful for procedurally generated textures that never go
    /// through [`decode_rgba8`].
    pub fn create_texture_from_rgba(
        &self,
        label: &str,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Texture {
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };

        let texture = self.device().create_texture_with_data(
            self.queue(),
            &wgpu::TextureDescriptor {
                label: Some(label),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            rgba,
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = self.device().create_sampler(&wgpu::SamplerDescriptor {
            label: Some(&format!("{label} sampler")),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        Texture {
            texture,
            view,
            sampler,
        }
    }

    /// Uploads a full mip chain as a new [`Texture`], with trilinear
    /// filtering (`mipmap_filter: Linear`) enabled on its sampler.
    ///
    /// `mip_levels[0]` is the base (full-resolution) level; each
    /// subsequent level must be exactly `width >> n` by `height >> n`
    /// (minimum `1`), tightly packed RGBA8 — the standard mip halving
    /// sequence, as produced by e.g. `engine_asset`'s texture importer.
    /// wgpu validates this and will panic on a mismatched level size or
    /// byte count.
    pub fn create_texture_from_mip_chain(
        &self,
        label: &str,
        width: u32,
        height: u32,
        mip_levels: &[Vec<u8>],
    ) -> Texture {
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let data: Vec<u8> = mip_levels.iter().flatten().copied().collect();

        let texture = self.device().create_texture_with_data(
            self.queue(),
            &wgpu::TextureDescriptor {
                label: Some(label),
                size,
                mip_level_count: mip_levels.len() as u32,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            &data,
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = self.device().create_sampler(&wgpu::SamplerDescriptor {
            label: Some(&format!("{label} sampler")),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });

        Texture {
            texture,
            view,
            sampler,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encodes a tiny image in memory (real PNG bytes, not hand-rolled
    /// fixtures) so the test exercises the actual decode path.
    fn encode_test_png(width: u32, height: u32, pixel: [u8; 4]) -> Vec<u8> {
        let mut img = image::RgbaImage::new(width, height);
        for p in img.pixels_mut() {
            *p = image::Rgba(pixel);
        }
        let mut bytes = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .expect("encoding a valid in-memory test PNG must not fail");
        bytes
    }

    #[test]
    fn decode_rgba8_round_trips_a_real_png() {
        let bytes = encode_test_png(2, 3, [10, 20, 30, 255]);
        let decoded = decode_rgba8(&bytes).unwrap();
        assert_eq!(decoded.dimensions(), (2, 3));
        assert_eq!(decoded.get_pixel(0, 0).0, [10, 20, 30, 255]);
        assert_eq!(decoded.get_pixel(1, 2).0, [10, 20, 30, 255]);
    }

    #[test]
    fn decode_rgba8_rejects_garbage_bytes() {
        let err = decode_rgba8(&[0u8, 1, 2, 3, 4]).unwrap_err();
        assert!(matches!(err, RendererError::ImageDecode(_)));
    }

    #[test]
    fn decode_rgba8_rejects_empty_input() {
        let err = decode_rgba8(&[]).unwrap_err();
        assert!(matches!(err, RendererError::ImageDecode(_)));
    }
}
