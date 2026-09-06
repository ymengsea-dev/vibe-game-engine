//! Text rendering: a built-in pixel font, layout, and the pass that
//! draws it.
//!
//! Before this, nothing in the engine could put a character on screen —
//! no HUD, no menu, no dialogue, no debug readout. [`engine_ui`] emitted
//! text draw commands carrying a `String` that nothing consumed.
//!
//! ## Why a bitmap font
//!
//! The glyphs are generated in code rather than rasterized from a `.ttf`.
//! That keeps the engine self-contained — no font file to vendor, no
//! licence to track — and suits a stylized game. The cost is real and
//! worth stating: ASCII only, one fixed advance per glyph, no kerning,
//! and no hinting. Anything outside the covered range draws a `.notdef`
//! box rather than vanishing silently.
//!
//! [`GlyphAtlas`] and [`layout_text`] are deliberately independent of
//! where glyph bitmaps come from, so swapping in a TTF rasterizer later
//! means replacing [`glyph_bitmap`] and nothing else.
//!
//! ## Drawn last
//!
//! Text renders after post-processing. Tonemapping and bloom exist to
//! make a *scene* look right; running them over a HUD makes it muddy and
//! makes white text stop being white.

use std::collections::HashMap;

/// Width of one glyph cell in the source bitmaps, in pixels.
pub const GLYPH_WIDTH: usize = 5;

/// Height of one glyph cell in the source bitmaps, in pixels.
pub const GLYPH_HEIGHT: usize = 7;

/// Horizontal advance between glyph origins, in source pixels. One more
/// than [`GLYPH_WIDTH`] so adjacent characters do not touch.
pub const GLYPH_ADVANCE: usize = GLYPH_WIDTH + 1;

/// Vertical advance between baselines, in source pixels.
pub const LINE_ADVANCE: usize = GLYPH_HEIGHT + 2;

/// The first character the built-in font covers.
const FIRST_CHAR: char = ' ';

/// The last character the built-in font covers.
const LAST_CHAR: char = '~';

/// Row bitmaps for one glyph: [`GLYPH_HEIGHT`] rows, each with the low
/// [`GLYPH_WIDTH`] bits set where the glyph is inked.
type GlyphRows = [u8; GLYPH_HEIGHT];

/// The box drawn for a character the font does not cover.
///
/// A visible marker rather than a blank: silently dropping characters
/// makes a missing-glyph bug look like a data bug.
const NOTDEF: GlyphRows = [
    0b11111, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11111,
];

/// The built-in 5x7 glyph set, indexed from [`FIRST_CHAR`].
///
/// Hand-plotted as binary literals so each glyph's shape is visible in
/// the source — the `1` bits are the inked pixels.
#[rustfmt::skip]
const GLYPHS: [GlyphRows; 95] = [
    [0b00000,0b00000,0b00000,0b00000,0b00000,0b00000,0b00000], // space
    [0b00100,0b00100,0b00100,0b00100,0b00100,0b00000,0b00100], // !
    [0b01010,0b01010,0b00000,0b00000,0b00000,0b00000,0b00000], // "
    [0b01010,0b11111,0b01010,0b01010,0b01010,0b11111,0b01010], // #
    [0b00100,0b01111,0b10100,0b01110,0b00101,0b11110,0b00100], // $
    [0b11000,0b11001,0b00010,0b00100,0b01000,0b10011,0b00011], // %
    [0b01000,0b10100,0b10100,0b01000,0b10101,0b10010,0b01101], // &
    [0b00100,0b00100,0b00000,0b00000,0b00000,0b00000,0b00000], // '
    [0b00010,0b00100,0b01000,0b01000,0b01000,0b00100,0b00010], // (
    [0b01000,0b00100,0b00010,0b00010,0b00010,0b00100,0b01000], // )
    [0b00000,0b00100,0b10101,0b01110,0b10101,0b00100,0b00000], // *
    [0b00000,0b00100,0b00100,0b11111,0b00100,0b00100,0b00000], // +
    [0b00000,0b00000,0b00000,0b00000,0b00100,0b00100,0b01000], // ,
    [0b00000,0b00000,0b00000,0b11111,0b00000,0b00000,0b00000], // -
    [0b00000,0b00000,0b00000,0b00000,0b00000,0b00100,0b00100], // .
    [0b00001,0b00010,0b00010,0b00100,0b01000,0b01000,0b10000], // /
    [0b01110,0b10001,0b10011,0b10101,0b11001,0b10001,0b01110], // 0
    [0b00100,0b01100,0b00100,0b00100,0b00100,0b00100,0b01110], // 1
    [0b01110,0b10001,0b00001,0b00010,0b00100,0b01000,0b11111], // 2
    [0b11111,0b00010,0b00100,0b00010,0b00001,0b10001,0b01110], // 3
    [0b00010,0b00110,0b01010,0b10010,0b11111,0b00010,0b00010], // 4
    [0b11111,0b10000,0b11110,0b00001,0b00001,0b10001,0b01110], // 5
    [0b00110,0b01000,0b10000,0b11110,0b10001,0b10001,0b01110], // 6
    [0b11111,0b00001,0b00010,0b00100,0b01000,0b01000,0b01000], // 7
    [0b01110,0b10001,0b10001,0b01110,0b10001,0b10001,0b01110], // 8
    [0b01110,0b10001,0b10001,0b01111,0b00001,0b00010,0b01100], // 9
    [0b00000,0b00100,0b00100,0b00000,0b00100,0b00100,0b00000], // :
    [0b00000,0b00100,0b00100,0b00000,0b00100,0b00100,0b01000], // ;
    [0b00010,0b00100,0b01000,0b10000,0b01000,0b00100,0b00010], // <
    [0b00000,0b00000,0b11111,0b00000,0b11111,0b00000,0b00000], // =
    [0b01000,0b00100,0b00010,0b00001,0b00010,0b00100,0b01000], // >
    [0b01110,0b10001,0b00001,0b00010,0b00100,0b00000,0b00100], // ?
    [0b01110,0b10001,0b10111,0b10101,0b10111,0b10000,0b01110], // @
    [0b01110,0b10001,0b10001,0b11111,0b10001,0b10001,0b10001], // A
    [0b11110,0b10001,0b10001,0b11110,0b10001,0b10001,0b11110], // B
    [0b01110,0b10001,0b10000,0b10000,0b10000,0b10001,0b01110], // C
    [0b11100,0b10010,0b10001,0b10001,0b10001,0b10010,0b11100], // D
    [0b11111,0b10000,0b10000,0b11110,0b10000,0b10000,0b11111], // E
    [0b11111,0b10000,0b10000,0b11110,0b10000,0b10000,0b10000], // F
    [0b01110,0b10001,0b10000,0b10111,0b10001,0b10001,0b01111], // G
    [0b10001,0b10001,0b10001,0b11111,0b10001,0b10001,0b10001], // H
    [0b01110,0b00100,0b00100,0b00100,0b00100,0b00100,0b01110], // I
    [0b00111,0b00010,0b00010,0b00010,0b00010,0b10010,0b01100], // J
    [0b10001,0b10010,0b10100,0b11000,0b10100,0b10010,0b10001], // K
    [0b10000,0b10000,0b10000,0b10000,0b10000,0b10000,0b11111], // L
    [0b10001,0b11011,0b10101,0b10101,0b10001,0b10001,0b10001], // M
    [0b10001,0b11001,0b10101,0b10011,0b10001,0b10001,0b10001], // N
    [0b01110,0b10001,0b10001,0b10001,0b10001,0b10001,0b01110], // O
    [0b11110,0b10001,0b10001,0b11110,0b10000,0b10000,0b10000], // P
    [0b01110,0b10001,0b10001,0b10001,0b10101,0b10010,0b01101], // Q
    [0b11110,0b10001,0b10001,0b11110,0b10100,0b10010,0b10001], // R
    [0b01111,0b10000,0b10000,0b01110,0b00001,0b00001,0b11110], // S
    [0b11111,0b00100,0b00100,0b00100,0b00100,0b00100,0b00100], // T
    [0b10001,0b10001,0b10001,0b10001,0b10001,0b10001,0b01110], // U
    [0b10001,0b10001,0b10001,0b10001,0b10001,0b01010,0b00100], // V
    [0b10001,0b10001,0b10001,0b10101,0b10101,0b11011,0b10001], // W
    [0b10001,0b10001,0b01010,0b00100,0b01010,0b10001,0b10001], // X
    [0b10001,0b10001,0b01010,0b00100,0b00100,0b00100,0b00100], // Y
    [0b11111,0b00001,0b00010,0b00100,0b01000,0b10000,0b11111], // Z
    [0b01110,0b01000,0b01000,0b01000,0b01000,0b01000,0b01110], // [
    [0b10000,0b01000,0b01000,0b00100,0b00010,0b00010,0b00001], // \
    [0b01110,0b00010,0b00010,0b00010,0b00010,0b00010,0b01110], // ]
    [0b00100,0b01010,0b10001,0b00000,0b00000,0b00000,0b00000], // ^
    [0b00000,0b00000,0b00000,0b00000,0b00000,0b00000,0b11111], // _
    [0b01000,0b00100,0b00000,0b00000,0b00000,0b00000,0b00000], // `
    [0b00000,0b00000,0b01110,0b00001,0b01111,0b10001,0b01111], // a
    [0b10000,0b10000,0b11110,0b10001,0b10001,0b10001,0b11110], // b
    [0b00000,0b00000,0b01110,0b10000,0b10000,0b10001,0b01110], // c
    [0b00001,0b00001,0b01111,0b10001,0b10001,0b10001,0b01111], // d
    [0b00000,0b00000,0b01110,0b10001,0b11111,0b10000,0b01110], // e
    [0b00110,0b01001,0b01000,0b11100,0b01000,0b01000,0b01000], // f
    [0b00000,0b01111,0b10001,0b10001,0b01111,0b00001,0b01110], // g
    [0b10000,0b10000,0b11110,0b10001,0b10001,0b10001,0b10001], // h
    [0b00100,0b00000,0b01100,0b00100,0b00100,0b00100,0b01110], // i
    [0b00010,0b00000,0b00110,0b00010,0b00010,0b10010,0b01100], // j
    [0b10000,0b10000,0b10010,0b10100,0b11000,0b10100,0b10010], // k
    [0b01100,0b00100,0b00100,0b00100,0b00100,0b00100,0b01110], // l
    [0b00000,0b00000,0b11010,0b10101,0b10101,0b10001,0b10001], // m
    [0b00000,0b00000,0b11110,0b10001,0b10001,0b10001,0b10001], // n
    [0b00000,0b00000,0b01110,0b10001,0b10001,0b10001,0b01110], // o
    [0b00000,0b11110,0b10001,0b10001,0b11110,0b10000,0b10000], // p
    [0b00000,0b01111,0b10001,0b10001,0b01111,0b00001,0b00001], // q
    [0b00000,0b00000,0b10110,0b11001,0b10000,0b10000,0b10000], // r
    [0b00000,0b00000,0b01111,0b10000,0b01110,0b00001,0b11110], // s
    [0b01000,0b01000,0b11100,0b01000,0b01000,0b01001,0b00110], // t
    [0b00000,0b00000,0b10001,0b10001,0b10001,0b10011,0b01101], // u
    [0b00000,0b00000,0b10001,0b10001,0b10001,0b01010,0b00100], // v
    [0b00000,0b00000,0b10001,0b10001,0b10101,0b10101,0b01010], // w
    [0b00000,0b00000,0b10001,0b01010,0b00100,0b01010,0b10001], // x
    [0b00000,0b10001,0b10001,0b01111,0b00001,0b00010,0b01100], // y
    [0b00000,0b00000,0b11111,0b00010,0b00100,0b01000,0b11111], // z
    [0b00010,0b00100,0b00100,0b01000,0b00100,0b00100,0b00010], // {
    [0b00100,0b00100,0b00100,0b00100,0b00100,0b00100,0b00100], // |
    [0b01000,0b00100,0b00100,0b00010,0b00100,0b00100,0b01000], // }
    [0b00000,0b00000,0b01000,0b10101,0b00010,0b00000,0b00000], // ~
];

/// The row bitmaps for `character`, or the `.notdef` box if the font does not
/// cover it.
///
/// The single point where glyph shapes come from — replace this to swap
/// in a TTF rasterizer without touching layout or the render pass.
pub fn glyph_bitmap(character: char) -> GlyphRows {
    if character < FIRST_CHAR || character > LAST_CHAR {
        return NOTDEF;
    }
    let index = character as usize - FIRST_CHAR as usize;
    GLYPHS.get(index).copied().unwrap_or(NOTDEF)
}

/// Whether the built-in font has a real glyph for `character`.
pub fn covers(character: char) -> bool {
    (FIRST_CHAR..=LAST_CHAR).contains(&character)
}

/// One positioned glyph produced by [`layout_text`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PositionedGlyph {
    /// The character to draw.
    pub character: char,
    /// Left edge, in pixels from the layout origin.
    pub x: f32,
    /// Top edge, in pixels from the layout origin.
    pub y: f32,
    /// Rendered width in pixels.
    pub width: f32,
    /// Rendered height in pixels.
    pub height: f32,
}

/// The result of laying out a string.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TextLayout {
    /// Every glyph, in reading order. Whitespace produces no entry.
    pub glyphs: Vec<PositionedGlyph>,
    /// Width of the widest line, in pixels.
    pub width: f32,
    /// Total height of all lines, in pixels.
    pub height: f32,
    /// How many lines the text occupies, including wrapped ones.
    pub lines: usize,
}

/// Lays out `text` at `pixel_height` per glyph, wrapping at `max_width`.
///
/// `max_width` of `None` wraps only at explicit `\n`.
///
/// Iterates by `char`, never by byte, so multi-byte input cannot be split
/// mid-character. Characters outside the font's range still occupy a cell
/// and draw as `.notdef`.
pub fn layout_text(text: &str, pixel_height: f32, max_width: Option<f32>) -> TextLayout {
    let scale = (pixel_height / GLYPH_HEIGHT as f32).max(0.0);
    let advance = GLYPH_ADVANCE as f32 * scale;
    let line_height = LINE_ADVANCE as f32 * scale;
    let glyph_width = GLYPH_WIDTH as f32 * scale;

    let mut layout = TextLayout::default();
    if scale <= 0.0 || text.is_empty() {
        return layout;
    }

    let mut cursor_x = 0.0_f32;
    let mut cursor_y = 0.0_f32;
    let mut widest = 0.0_f32;
    let mut lines = 1_usize;

    let newline = |cursor_x: &mut f32, cursor_y: &mut f32, widest: &mut f32, lines: &mut usize| {
        *widest = widest.max(*cursor_x);
        *cursor_x = 0.0;
        *cursor_y += line_height;
        *lines += 1;
    };

    // Split on whitespace so wrapping happens between words, keeping the
    // separators to reproduce explicit newlines.
    for word in split_keeping_whitespace(text) {
        if word == "\n" {
            newline(&mut cursor_x, &mut cursor_y, &mut widest, &mut lines);
            continue;
        }

        let word_width = word.chars().count() as f32 * advance;
        if let Some(max_width) = max_width
            && cursor_x > 0.0
            && cursor_x + word_width > max_width
            && !word.trim().is_empty()
        {
            newline(&mut cursor_x, &mut cursor_y, &mut widest, &mut lines);
        }

        for character in word.chars() {
            if character == '\n' {
                newline(&mut cursor_x, &mut cursor_y, &mut widest, &mut lines);
                continue;
            }

            // A single word longer than the whole line: break inside it
            // rather than looping forever trying to fit it.
            if let Some(max_width) = max_width
                && cursor_x > 0.0
                && cursor_x + glyph_width > max_width
            {
                newline(&mut cursor_x, &mut cursor_y, &mut widest, &mut lines);
            }

            if !character.is_whitespace() {
                layout.glyphs.push(PositionedGlyph {
                    character,
                    x: cursor_x,
                    y: cursor_y,
                    width: glyph_width,
                    height: GLYPH_HEIGHT as f32 * scale,
                });
            }
            cursor_x += advance;
        }
    }

    layout.width = widest.max(cursor_x);
    layout.height = cursor_y + line_height;
    layout.lines = lines;
    layout
}

/// Splits `text` into words, with each `\n` as its own item.
///
/// Keeping newlines as items means the layout loop handles explicit
/// breaks and wrapping through one path.
fn split_keeping_whitespace(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    for (index, character) in text.char_indices() {
        if character == '\n' {
            if index > start {
                parts.push(&text[start..index]);
            }
            parts.push("\n");
            start = index + character.len_utf8();
        } else if character == ' ' {
            // Include the space with the preceding word so its advance
            // is not lost.
            let end = index + character.len_utf8();
            parts.push(&text[start..end]);
            start = end;
        }
    }
    if start < text.len() {
        parts.push(&text[start..]);
    }
    parts
}

/// Where a glyph lives in the atlas texture, in normalized UVs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlyphUv {
    /// Top-left UV.
    pub min: [f32; 2],
    /// Bottom-right UV.
    pub max: [f32; 2],
}

/// A texture holding every glyph the font covers, laid out in a grid.
///
/// Built once. Because the font is a fixed bitmap set, the atlas has a
/// known size up front and never needs to grow or evict — the dynamic
/// packing a TTF rasterizer would require is not needed here, and adding
/// it speculatively would be complexity with nothing behind it.
#[derive(Debug, Clone)]
pub struct GlyphAtlas {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    uvs: HashMap<char, GlyphUv>,
    columns: u32,
}

impl GlyphAtlas {
    /// Rasterizes the built-in font into an RGBA8 atlas.
    ///
    /// White pixels with alpha `0` or `255`: the text pass tints them, so
    /// one atlas serves every colour.
    pub fn build() -> Self {
        let glyph_count = (LAST_CHAR as u32 - FIRST_CHAR as u32) + 2; // +1 for .notdef
        let columns = 16;
        let rows = glyph_count.div_ceil(columns);
        let width = columns * GLYPH_WIDTH as u32;
        let height = rows * GLYPH_HEIGHT as u32;
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let mut uvs = HashMap::new();

        let mut blit = |index: u32, rows_bits: GlyphRows| {
            let cell_x = (index % columns) * GLYPH_WIDTH as u32;
            let cell_y = (index / columns) * GLYPH_HEIGHT as u32;
            for (row, bits) in rows_bits.iter().enumerate() {
                for column in 0..GLYPH_WIDTH {
                    // Bit 0 is the rightmost pixel.
                    let inked = bits & (1 << (GLYPH_WIDTH - 1 - column)) != 0;
                    let x = cell_x + column as u32;
                    let y = cell_y + row as u32;
                    let offset = ((y * width + x) * 4) as usize;
                    let value = if inked { 255 } else { 0 };
                    pixels[offset] = 255;
                    pixels[offset + 1] = 255;
                    pixels[offset + 2] = 255;
                    pixels[offset + 3] = value;
                }
            }
            GlyphUv {
                min: [cell_x as f32 / width as f32, cell_y as f32 / height as f32],
                max: [
                    (cell_x + GLYPH_WIDTH as u32) as f32 / width as f32,
                    (cell_y + GLYPH_HEIGHT as u32) as f32 / height as f32,
                ],
            }
        };

        for index in 0..=(LAST_CHAR as u32 - FIRST_CHAR as u32) {
            let character = char::from_u32(FIRST_CHAR as u32 + index).unwrap_or('?');
            let uv = blit(index, glyph_bitmap(character));
            uvs.insert(character, uv);
        }
        // `.notdef` sits in the slot after the covered range.
        let notdef_uv = blit(LAST_CHAR as u32 - FIRST_CHAR as u32 + 1, NOTDEF);

        Self {
            width,
            height,
            pixels,
            uvs,
            columns,
        }
        .with_notdef(notdef_uv)
    }

    fn with_notdef(mut self, uv: GlyphUv) -> Self {
        // Stored under a character the font never covers, so lookups for
        // any uncovered character land here.
        self.uvs.insert('\u{FFFD}', uv);
        self
    }

    /// Atlas dimensions in pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The RGBA8 pixel data, ready for
    /// [`crate::GpuContext::create_texture_from_rgba`].
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// How many glyph cells sit in one atlas row.
    pub fn columns(&self) -> u32 {
        self.columns
    }

    /// Where `character` lives in the atlas, falling back to `.notdef`.
    pub fn uv(&self, character: char) -> GlyphUv {
        self.uvs
            .get(&character)
            .or_else(|| self.uvs.get(&'\u{FFFD}'))
            .copied()
            .unwrap_or(GlyphUv {
                min: [0.0, 0.0],
                max: [0.0, 0.0],
            })
    }
}

impl Default for GlyphAtlas {
    fn default() -> Self {
        Self::build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covered_characters_have_distinct_glyphs() {
        assert_ne!(glyph_bitmap('A'), glyph_bitmap('B'));
        assert_ne!(glyph_bitmap('0'), glyph_bitmap('O'));
        assert_eq!(glyph_bitmap(' '), [0; GLYPH_HEIGHT], "space must be blank");
    }

    #[test]
    fn missing_glyph_falls_back_to_notdef() {
        assert_eq!(glyph_bitmap('é'), NOTDEF);
        assert_eq!(glyph_bitmap('日'), NOTDEF);
        assert_eq!(glyph_bitmap('\u{1F600}'), NOTDEF);
        assert!(!covers('é'));
        assert!(covers('A'));
    }

    #[test]
    fn non_ascii_input_does_not_panic_or_split_bytes() {
        // Multi-byte characters must each occupy one cell, not several.
        let layout = layout_text("héllo", 14.0, None);
        assert_eq!(layout.glyphs.len(), 5, "one glyph per char, not per byte");
        assert_eq!(layout.glyphs[1].character, 'é');
    }

    #[test]
    fn layout_advances_each_glyph_by_a_fixed_step() {
        let layout = layout_text("AB", 14.0, None);
        assert_eq!(layout.glyphs.len(), 2);
        let step = layout.glyphs[1].x - layout.glyphs[0].x;
        let expected = GLYPH_ADVANCE as f32 * (14.0 / GLYPH_HEIGHT as f32);
        assert!(
            (step - expected).abs() < 1e-4,
            "got {step}, want {expected}"
        );
    }

    #[test]
    fn whitespace_advances_but_emits_no_glyph() {
        let layout = layout_text("A B", 14.0, None);
        assert_eq!(layout.glyphs.len(), 2, "the space should not be drawn");
        assert!(layout.glyphs[1].x > layout.glyphs[0].x * 2.0);
    }

    #[test]
    fn newline_starts_a_new_line_at_the_left() {
        let layout = layout_text("A\nB", 14.0, None);
        assert_eq!(layout.lines, 2);
        assert_eq!(layout.glyphs.len(), 2);
        assert_eq!(layout.glyphs[1].x, 0.0, "second line restarts at x=0");
        assert!(layout.glyphs[1].y > layout.glyphs[0].y);
    }

    #[test]
    fn wrap_breaks_at_word_boundaries() {
        // Narrow enough to force a break between the two words.
        let layout = layout_text("aaa bbb", 7.0, Some(30.0));
        assert_eq!(layout.lines, 2);
        let first_b = layout
            .glyphs
            .iter()
            .find(|g| g.character == 'b')
            .expect("the second word should be laid out");
        assert_eq!(first_b.x, 0.0, "a wrapped word starts a fresh line");
    }

    #[test]
    fn wrap_terminates_on_an_overlong_single_word() {
        // One word far wider than the line. Must break inside it and
        // finish, rather than spinning.
        let layout = layout_text("aaaaaaaaaaaaaaaaaaaaaaaa", 7.0, Some(20.0));
        assert!(layout.lines > 1, "an overlong word must be broken");
        assert_eq!(layout.glyphs.len(), 24, "no character may be lost");
    }

    #[test]
    fn empty_and_degenerate_input_is_handled() {
        assert!(layout_text("", 14.0, None).glyphs.is_empty());
        assert!(layout_text("abc", 0.0, None).glyphs.is_empty());
        assert!(layout_text("abc", -5.0, None).glyphs.is_empty());
    }

    #[test]
    fn reported_width_covers_every_glyph() {
        let layout = layout_text("hello", 14.0, None);
        let rightmost = layout
            .glyphs
            .iter()
            .map(|g| g.x + g.width)
            .fold(0.0_f32, f32::max);
        assert!(
            layout.width >= rightmost,
            "reported width {} must cover the rightmost glyph at {rightmost}",
            layout.width,
        );
    }

    #[test]
    fn atlas_is_rgba8_and_correctly_sized() {
        let atlas = GlyphAtlas::build();
        let (width, height) = atlas.size();
        assert_eq!(atlas.pixels().len(), (width * height * 4) as usize);
        assert!(width > 0 && height > 0);
    }

    #[test]
    fn atlas_gives_every_covered_character_a_distinct_cell() {
        let atlas = GlyphAtlas::build();
        let a = atlas.uv('A');
        let b = atlas.uv('B');
        assert_ne!(a.min, b.min, "two characters must not share a cell");
        assert!(a.max[0] > a.min[0] && a.max[1] > a.min[1]);
    }

    #[test]
    fn atlas_maps_uncovered_characters_to_the_notdef_cell() {
        let atlas = GlyphAtlas::build();
        assert_eq!(atlas.uv('é'), atlas.uv('日'), "both should be .notdef");
        assert_ne!(atlas.uv('é'), atlas.uv('A'));
    }

    #[test]
    fn atlas_ink_matches_the_glyph_bitmaps() {
        let atlas = GlyphAtlas::build();
        let (width, _) = atlas.size();
        // 'space' is blank; check its cell has zero alpha throughout.
        let uv = atlas.uv(' ');
        let x0 = (uv.min[0] * width as f32).round() as u32;
        let y0 = (uv.min[1] * atlas.size().1 as f32).round() as u32;
        for row in 0..GLYPH_HEIGHT as u32 {
            for column in 0..GLYPH_WIDTH as u32 {
                let offset = (((y0 + row) * width + x0 + column) * 4 + 3) as usize;
                assert_eq!(atlas.pixels()[offset], 0, "space must be fully transparent");
            }
        }
    }
}
