//! The game UI's text: the built-in Inter font, drawn into a glyph atlas as
//! each character is first needed at each size.
//!
//! Glyphs are cached per font, size and quarter-pixel position, so text stays
//! sharp at any size and evenly spaced. When the atlas fills up it starts
//! over; [`UiFont::generation`] tells a caller its earlier glyphs moved.

use std::collections::HashMap;

use ab_glyph::{Font, FontRef, GlyphId, InvalidFont, PxScale, ScaleFont, point};

use crate::gpu::GpuContext;
use crate::texture::Texture;

const REGULAR: &[u8] = include_bytes!("../fonts/Inter-Medium.ttf");
const BOLD: &[u8] = include_bytes!("../fonts/Inter-SemiBold.ttf");

/// Atlas width and height in pixels.
const ATLAS_SIZE: u32 = 2048;
/// Side of the white block reserved for solid fills, at the atlas's corner.
const WHITE_BLOCK: u32 = 4;
/// Empty pixels around each glyph, so filtering never reaches a neighbour.
const PADDING: u32 = 1;
/// Horizontal glyph positions per pixel.
const SUBPIXEL_STEPS: f32 = 4.0;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    bold: bool,
    glyph: GlyphId,
    /// Font size in quarter pixels.
    size: u32,
    subpixel: u8,
}

/// A glyph in the atlas, relative to its pen position on the baseline.
#[derive(Clone, Copy)]
struct CachedGlyph {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    uv: [f32; 4],
}

/// One glyph of laid-out text.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UiGlyph {
    /// Left edge in pixels from the text's left.
    pub x: f32,
    /// Top edge in pixels from the text's top.
    pub y: f32,
    /// Width in pixels.
    pub width: f32,
    /// Height in pixels.
    pub height: f32,
    /// Atlas sub-rect: `min_u`, `min_v`, `max_u`, `max_v`.
    pub uv: [f32; 4],
}

/// Text placed and ready to draw.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UiTextLayout {
    /// Every visible glyph.
    pub glyphs: Vec<UiGlyph>,
    /// Width of the widest line in pixels.
    pub width: f32,
    /// Height of all lines in pixels.
    pub height: f32,
}

/// The UI's fonts and the glyph atlas they draw into.
pub struct UiFont {
    regular: FontRef<'static>,
    bold: FontRef<'static>,
    pixels: Vec<u8>,
    glyphs: HashMap<GlyphKey, Option<CachedGlyph>>,
    cursor: (u32, u32),
    row_height: u32,
    /// Rows changed since the last upload.
    dirty: Option<(u32, u32)>,
    generation: u64,
}

impl UiFont {
    /// Loads the built-in fonts with an empty atlas.
    ///
    /// # Errors
    ///
    /// [`InvalidFont`] if a bundled font file cannot be read.
    pub fn new() -> Result<Self, InvalidFont> {
        let mut font = Self {
            regular: FontRef::try_from_slice(REGULAR)?,
            bold: FontRef::try_from_slice(BOLD)?,
            pixels: Vec::new(),
            glyphs: HashMap::new(),
            cursor: (0, 0),
            row_height: 0,
            dirty: None,
            generation: 0,
        };
        font.reset();
        Ok(font)
    }

    /// Empties the atlas, keeping the white block.
    fn reset(&mut self) {
        let side = ATLAS_SIZE as usize;
        // White everywhere; a glyph only writes alpha, so tinting works and
        // filtering at a glyph's edge never darkens it.
        self.pixels = [255, 255, 255, 0].repeat(side * side);
        for y in 0..WHITE_BLOCK as usize {
            for x in 0..WHITE_BLOCK as usize {
                self.pixels[(y * side + x) * 4 + 3] = 255;
            }
        }
        self.glyphs.clear();
        self.cursor = (WHITE_BLOCK + PADDING, 0);
        self.row_height = WHITE_BLOCK;
        self.dirty = Some((0, ATLAS_SIZE));
        self.generation += 1;
    }

    /// Changes each time the atlas starts over. If it changed during a frame's
    /// layouts, the frame's earlier glyphs must be laid out again.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The atlas UV of an opaque white texel, for solid fills.
    pub fn white_uv(&self) -> [f32; 4] {
        let centre = WHITE_BLOCK as f32 * 0.5 / ATLAS_SIZE as f32;
        [centre, centre, centre, centre]
    }

    /// Places `text` at font size `size` in pixels, wrapping between words at
    /// `max_width`, and at each `\n`. A character the font lacks shows as a box.
    pub fn layout(
        &mut self,
        text: &str,
        size: f32,
        bold: bool,
        max_width: Option<f32>,
    ) -> UiTextLayout {
        let font = if bold {
            self.bold.clone()
        } else {
            self.regular.clone()
        };
        let units = font.units_per_em().unwrap_or(1000.0);
        // `PxScale` is the full line height; font size means the em square.
        let scale = PxScale::from(size * font.height_unscaled() / units);
        let scaled = font.as_scaled(scale);
        let ascent = scaled.ascent().ceil();
        let line_height = (scaled.height() + scaled.line_gap()).ceil();

        let mut layout = UiTextLayout::default();
        let mut lines = 0_u32;
        for line in text.split('\n') {
            let mut pen = 0.0_f32;
            let mut previous = None;
            for word in line.split_inclusive(' ') {
                let word_width = word
                    .trim_end_matches(' ')
                    .chars()
                    .fold((0.0, None), |(width, last), character| {
                        let id = scaled.glyph_id(character);
                        let kern = last.map_or(0.0, |last| scaled.kern(last, id));
                        (width + kern + scaled.h_advance(id), Some(id))
                    })
                    .0;
                if max_width.is_some_and(|limit| pen > 0.0 && pen + word_width > limit) {
                    layout.width = layout.width.max(pen);
                    pen = 0.0;
                    previous = None;
                    lines += 1;
                }
                let baseline = lines as f32 * line_height + ascent;
                for character in word.chars() {
                    let id = scaled.glyph_id(character);
                    if let Some(last) = previous {
                        pen += scaled.kern(last, id);
                    }
                    let whole = pen.floor();
                    let subpixel = ((pen - whole) * SUBPIXEL_STEPS) as u8;
                    if let Some(glyph) = self.glyph(bold, id, size, subpixel, scale) {
                        layout.glyphs.push(UiGlyph {
                            x: whole + glyph.x,
                            y: baseline + glyph.y,
                            width: glyph.width,
                            height: glyph.height,
                            uv: glyph.uv,
                        });
                    }
                    pen += scaled.h_advance(id);
                    previous = Some(id);
                }
            }
            // A trailing space does not count towards the line's width.
            layout.width = layout.width.max(pen);
            lines += 1;
        }
        layout.height = lines as f32 * line_height;
        layout
    }

    /// The cached glyph, drawing it into the atlas first if needed. `None` for
    /// one with no shape, such as a space.
    fn glyph(
        &mut self,
        bold: bool,
        glyph: GlyphId,
        size: f32,
        subpixel: u8,
        scale: PxScale,
    ) -> Option<CachedGlyph> {
        let key = GlyphKey {
            bold,
            glyph,
            size: (size * 4.0).round() as u32,
            subpixel,
        };
        if let Some(cached) = self.glyphs.get(&key) {
            return *cached;
        }
        let font = if bold {
            self.bold.clone()
        } else {
            self.regular.clone()
        };
        let positioned =
            glyph.with_scale_and_position(scale, point(f32::from(subpixel) / SUBPIXEL_STEPS, 0.0));
        let drawn = font
            .outline_glyph(positioned)
            .and_then(|outline| self.place(&outline));
        self.glyphs.insert(key, drawn);
        drawn
    }

    /// Writes an outline into free atlas space. `None` if it is too big to fit
    /// in an empty atlas.
    fn place(&mut self, outline: &ab_glyph::OutlinedGlyph) -> Option<CachedGlyph> {
        let bounds = outline.px_bounds();
        let width = bounds.width().ceil() as u32;
        let height = bounds.height().ceil() as u32;
        if width + PADDING * 2 > ATLAS_SIZE || height + PADDING * 2 > ATLAS_SIZE {
            return None;
        }
        if self.cursor.0 + width + PADDING > ATLAS_SIZE {
            self.cursor = (0, self.cursor.1 + self.row_height + PADDING);
            self.row_height = 0;
        }
        if self.cursor.1 + height + PADDING > ATLAS_SIZE {
            self.reset();
        }
        let (left, top) = self.cursor;
        let side = ATLAS_SIZE as usize;
        let pixels = &mut self.pixels;
        outline.draw(|x, y, coverage| {
            let index = ((top + y) as usize * side + (left + x) as usize) * 4 + 3;
            if let Some(alpha) = pixels.get_mut(index) {
                *alpha = (coverage.clamp(0.0, 1.0) * 255.0) as u8;
            }
        });
        self.cursor.0 = left + width + PADDING;
        self.row_height = self.row_height.max(height);
        let (first, last) = self.dirty.unwrap_or((top, top + height));
        self.dirty = Some((first.min(top), last.max(top + height)));
        let atlas = ATLAS_SIZE as f32;
        Some(CachedGlyph {
            x: bounds.min.x,
            y: bounds.min.y,
            width: width as f32,
            height: height as f32,
            uv: [
                left as f32 / atlas,
                top as f32 / atlas,
                (left + width) as f32 / atlas,
                (top + height) as f32 / atlas,
            ],
        })
    }
}

impl GpuContext {
    /// Creates the texture [`UiFont`]'s atlas lives in.
    pub fn create_ui_font_texture(&self, font: &UiFont) -> Texture {
        let size = wgpu::Extent3d {
            width: ATLAS_SIZE,
            height: ATLAS_SIZE,
            depth_or_array_layers: 1,
        };
        let texture = self.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("ui font atlas"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.write_font_rows(&texture, font, 0, ATLAS_SIZE);
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = self.device().create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ui font sampler"),
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

    /// Sends glyphs drawn since the last call to the GPU.
    pub fn upload_ui_font(&self, texture: &Texture, font: &mut UiFont) {
        if let Some((first, last)) = font.dirty.take() {
            self.write_font_rows(&texture.texture, font, first, last);
        }
    }

    fn write_font_rows(&self, texture: &wgpu::Texture, font: &UiFont, first: u32, last: u32) {
        let start = first as usize * ATLAS_SIZE as usize * 4;
        self.queue().write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: first,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &font.pixels[start..],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(ATLAS_SIZE * 4),
                rows_per_image: Some(last - first),
            },
            wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: last - first,
                depth_or_array_layers: 1,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font() -> UiFont {
        UiFont::new().expect("bundled font")
    }

    #[test]
    fn the_same_glyph_is_drawn_once_and_a_new_size_is_drawn_again() {
        let mut font = font();
        let first = font.layout("A", 16.0, false, None);
        let count = font.glyphs.len();
        assert_eq!(font.layout("A", 16.0, false, None), first);
        assert_eq!(font.glyphs.len(), count);
        let big = font.layout("A", 32.0, false, None);
        assert!(big.glyphs[0].height > first.glyphs[0].height * 1.8);
        assert_ne!(big.glyphs[0].uv, first.glyphs[0].uv);
    }

    #[test]
    fn bold_uses_its_own_glyphs_and_is_wider() {
        let mut font = font();
        let regular = font.layout("Paused", 28.0, false, None);
        let bold = font.layout("Paused", 28.0, true, None);
        assert!(bold.width > regular.width);
    }

    #[test]
    fn long_text_wraps_between_words_and_at_line_breaks() {
        let mut font = font();
        let one = font.layout("Hello there", 16.0, false, None);
        let wrapped = font.layout("Hello there", 16.0, false, Some(one.width - 10.0));
        assert!(wrapped.width < one.width);
        assert!((wrapped.height - one.height * 2.0).abs() < 0.01);
        let broken = font.layout("a\nb\nc", 16.0, false, None);
        assert!((broken.height - one.height * 3.0).abs() < 0.01);
    }

    #[test]
    fn spaces_draw_nothing_but_take_room() {
        let mut font = font();
        let spaced = font.layout("a a", 16.0, false, None);
        assert_eq!(spaced.glyphs.len(), 2);
        assert!(spaced.glyphs[1].x > font.layout("aa", 16.0, false, None).glyphs[1].x);
    }

    #[test]
    fn a_full_atlas_starts_over_and_says_so() {
        let mut font = font();
        let start = font.generation();
        for size in 100..400 {
            font.layout("WMQ", size as f32 / 2.0, true, None);
        }
        assert!(font.generation() > start);
        let glyph = font.layout("W", 16.0, false, None).glyphs[0];
        assert!(glyph.uv[2] <= 1.0 && glyph.uv[3] <= 1.0);
    }
}
