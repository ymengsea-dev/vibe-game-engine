//! Texture import: decode (any format `image` supports, via
//! [`engine_renderer::decode_rgba8`]) plus mip chain generation.
//!
//! Mip generation is pure CPU-side data transformation — testable without
//! a GPU. Uploading the result is [`engine_renderer::GpuContext::create_texture_from_mip_chain`],
//! kept on the renderer side since it's a GPU concern, not an import one.

use image::RgbaImage;

use crate::error::AssetError;

/// A decoded texture plus its full mip chain, ready for
/// [`engine_renderer::GpuContext::create_texture_from_mip_chain`].
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedTexture {
    /// Base (level 0) width in pixels.
    pub width: u32,
    /// Base (level 0) height in pixels.
    pub height: u32,
    /// Mip levels, base first, each tightly-packed RGBA8. Level `n` is
    /// `max(1, width >> n)` by `max(1, height >> n)`, ending at `1x1`.
    pub mip_levels: Vec<Vec<u8>>,
}

/// Decodes `bytes` (any format the `image` crate supports) and generates
/// its full mip chain.
///
/// # Errors
///
/// Returns [`AssetError::TextureImport`] if `bytes` isn't a valid,
/// supported image.
pub fn import_texture_bytes(bytes: &[u8]) -> Result<ImportedTexture, AssetError> {
    let rgba = engine_renderer::decode_rgba8(bytes)
        .map_err(|err| AssetError::TextureImport(err.to_string()))?;
    let (width, height) = rgba.dimensions();
    Ok(ImportedTexture {
        width,
        height,
        mip_levels: generate_mip_chain(&rgba),
    })
}

/// Generates a full mip chain from `base`: `base` itself, then
/// successive half-sized (box-filtered) downsamples down to `1x1`.
///
/// Pulled out from [`import_texture_bytes`] as a pure function of
/// already-decoded pixels, so mip generation is testable without needing
/// to encode/decode an image file.
pub fn generate_mip_chain(base: &RgbaImage) -> Vec<Vec<u8>> {
    let mut levels = vec![base.clone().into_raw()];
    let mut current = base.clone();
    while current.width() > 1 || current.height() > 1 {
        current = downsample_half(&current);
        levels.push(current.clone().into_raw());
    }
    levels
}

/// Box-filters `image` down to half its size (each dimension, minimum
/// `1`): each output pixel is the average of the corresponding up-to-2x2
/// source block (edge blocks with an odd source dimension sample their
/// last row/column twice rather than reading out of bounds).
fn downsample_half(image: &RgbaImage) -> RgbaImage {
    let (src_width, src_height) = image.dimensions();
    let dst_width = (src_width / 2).max(1);
    let dst_height = (src_height / 2).max(1);
    let mut out = RgbaImage::new(dst_width, dst_height);

    for y in 0..dst_height {
        for x in 0..dst_width {
            let x0 = (x * 2).min(src_width - 1);
            let x1 = (x * 2 + 1).min(src_width - 1);
            let y0 = (y * 2).min(src_height - 1);
            let y1 = (y * 2 + 1).min(src_height - 1);

            let mut sum = [0u32; 4];
            for (sx, sy) in [(x0, y0), (x1, y0), (x0, y1), (x1, y1)] {
                for (channel, value) in sum.iter_mut().zip(image.get_pixel(sx, sy).0) {
                    *channel += u32::from(value);
                }
            }
            let average = sum.map(|channel| (channel / 4) as u8);
            out.put_pixel(x, y, image::Rgba(average));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, pixel: [u8; 4]) -> RgbaImage {
        let mut image = RgbaImage::new(width, height);
        for p in image.pixels_mut() {
            *p = image::Rgba(pixel);
        }
        image
    }

    fn encode_png(image: &RgbaImage) -> Vec<u8> {
        let mut bytes = Vec::new();
        image
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    }

    #[test]
    fn mip_chain_has_one_level_per_halving_down_to_1x1() {
        let base = solid(4, 4, [1, 2, 3, 4]);
        let levels = generate_mip_chain(&base);
        // 4x4 -> 2x2 -> 1x1
        assert_eq!(levels.len(), 3);
        assert_eq!(levels[0].len(), 4 * 4 * 4);
        assert_eq!(levels[1].len(), 2 * 2 * 4);
        assert_eq!(levels[2].len(), 4);
    }

    #[test]
    fn a_1x1_image_has_only_the_base_level() {
        let base = solid(1, 1, [9, 9, 9, 9]);
        let levels = generate_mip_chain(&base);
        assert_eq!(levels.len(), 1);
        assert_eq!(levels[0], vec![9, 9, 9, 9]);
    }

    #[test]
    fn solid_color_downsamples_to_the_same_color() {
        let base = solid(8, 8, [100, 150, 200, 255]);
        let levels = generate_mip_chain(&base);
        for level in &levels {
            for chunk in level.chunks_exact(4) {
                assert_eq!(chunk, [100, 150, 200, 255]);
            }
        }
    }

    #[test]
    fn downsample_averages_a_2x2_block_exactly() {
        let mut base = RgbaImage::new(2, 2);
        base.put_pixel(0, 0, image::Rgba([0, 0, 0, 0]));
        base.put_pixel(1, 0, image::Rgba([100, 100, 100, 100]));
        base.put_pixel(0, 1, image::Rgba([0, 100, 0, 100]));
        base.put_pixel(1, 1, image::Rgba([100, 0, 100, 0]));

        let mip1 = downsample_half(&base);
        assert_eq!(mip1.dimensions(), (1, 1));
        // Average of (0,100,0,100), (100,100,100,100), (0,0,0,0), (100,0,100,0)
        assert_eq!(mip1.get_pixel(0, 0).0, [50, 50, 50, 50]);
    }

    #[test]
    fn odd_dimension_downsample_does_not_panic_and_terminates() {
        let base = solid(3, 5, [7, 7, 7, 7]);
        let levels = generate_mip_chain(&base);
        // 3x5 -> 1x2 -> 1x1
        assert_eq!(levels.len(), 3);
    }

    #[test]
    fn import_texture_bytes_decodes_and_generates_chain() {
        let base = solid(4, 4, [10, 20, 30, 255]);
        let bytes = encode_png(&base);

        let imported = import_texture_bytes(&bytes).unwrap();
        assert_eq!(imported.width, 4);
        assert_eq!(imported.height, 4);
        assert_eq!(imported.mip_levels.len(), 3);
        assert_eq!(
            imported.mip_levels[0],
            base.clone().into_raw(),
            "level 0 must match the decoded base image exactly"
        );
    }

    #[test]
    fn import_texture_bytes_rejects_garbage() {
        let err = import_texture_bytes(&[0u8, 1, 2, 3]).unwrap_err();
        assert!(matches!(err, AssetError::TextureImport(_)));
    }
}
