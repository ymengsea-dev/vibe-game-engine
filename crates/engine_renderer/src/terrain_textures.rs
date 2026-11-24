//! Studio's built-in ground textures for terrain paint layers, generated in
//! code so no art files ship with it.
//!
//! Each is deterministic value noise in one material's colours. They tile
//! seamlessly: the noise lattice wraps at the texture edge.

use serde::{Deserialize, Serialize};

/// A built-in ground texture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BuiltinLayer {
    /// Green and yellow speckles.
    #[default]
    Grass,
    /// Grey with darker streaks.
    Rock,
    /// Mottled brown.
    Dirt,
    /// Pale, fine grain.
    Sand,
}

impl BuiltinLayer {
    /// All of them, in the order a new terrain lists its layers.
    pub const ALL: [BuiltinLayer; 4] = [
        BuiltinLayer::Grass,
        BuiltinLayer::Rock,
        BuiltinLayer::Dirt,
        BuiltinLayer::Sand,
    ];

    /// Display name.
    pub fn label(self) -> &'static str {
        match self {
            BuiltinLayer::Grass => "Grass",
            BuiltinLayer::Rock => "Rock",
            BuiltinLayer::Dirt => "Dirt",
            BuiltinLayer::Sand => "Sand",
        }
    }

    /// The texture's average colour in linear RGB, for swatches.
    pub fn swatch(self) -> [f32; 3] {
        let (dark, light) = self.colours();
        [0, 1, 2].map(|i| srgb_to_linear((dark[i] + light[i]) * 0.5))
    }

    /// Dark and light sRGB colours the noise blends between.
    fn colours(self) -> ([f32; 3], [f32; 3]) {
        match self {
            BuiltinLayer::Grass => ([0.20, 0.36, 0.12], [0.45, 0.58, 0.22]),
            BuiltinLayer::Rock => ([0.33, 0.33, 0.34], [0.60, 0.59, 0.57]),
            BuiltinLayer::Dirt => ([0.30, 0.20, 0.12], [0.50, 0.37, 0.24]),
            BuiltinLayer::Sand => ([0.74, 0.66, 0.48], [0.88, 0.81, 0.64]),
        }
    }

    /// Noise lattice cells across the texture: more means a finer grain.
    fn frequency(self) -> u32 {
        match self {
            BuiltinLayer::Grass => 32,
            BuiltinLayer::Rock => 8,
            BuiltinLayer::Dirt => 16,
            BuiltinLayer::Sand => 64,
        }
    }
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// A repeatable hash of a lattice point, `0..1`.
fn lattice(x: u32, y: u32, seed: u32) -> f32 {
    let mut h = x
        .wrapping_mul(0x8da6_b343)
        .wrapping_add(y.wrapping_mul(0xd816_3841))
        .wrapping_add(seed.wrapping_mul(0xcb1a_b31f));
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1_e995);
    h ^= h >> 15;
    (h & 0x00ff_ffff) as f32 / 0x00ff_ffff as f32
}

/// Smooth value noise at texel `(x, y)` of a `size` texture with
/// `cells` lattice cells across, wrapping at the edge.
fn value_noise(x: u32, y: u32, size: u32, cells: u32, seed: u32) -> f32 {
    let cells = cells.clamp(1, size);
    let fx = x as f32 * cells as f32 / size as f32;
    let fy = y as f32 * cells as f32 / size as f32;
    let (x0, y0) = (fx.floor() as u32, fy.floor() as u32);
    let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
    let (sx, sy) = (tx * tx * (3.0 - 2.0 * tx), ty * ty * (3.0 - 2.0 * ty));
    let at = |cx: u32, cy: u32| lattice(cx % cells, cy % cells, seed);
    let top = at(x0, y0) + (at(x0 + 1, y0) - at(x0, y0)) * sx;
    let bottom = at(x0, y0 + 1) + (at(x0 + 1, y0 + 1) - at(x0, y0 + 1)) * sx;
    top + (bottom - top) * sy
}

/// A `size` x `size` RGBA8 (sRGB) texture for `layer`.
pub fn builtin_layer_rgba(layer: BuiltinLayer, size: u32) -> Vec<u8> {
    let size = size.max(1);
    let (dark, light) = layer.colours();
    let base = layer.frequency();
    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            // Three octaves; rock gets stretched streaks along X.
            let mut n = value_noise(x, y, size, base, 1) * 0.5
                + value_noise(x, y, size, base * 2, 2) * 0.3
                + value_noise(x, y, size, base * 4, 3) * 0.2;
            if layer == BuiltinLayer::Rock {
                let streak = value_noise(x, y, size, base * 8, 4);
                let band = value_noise(x / 4, y, size / 4, base * 2, 5);
                n = n * 0.7 + (streak * band) * 0.3;
            }
            for i in 0..3 {
                let value = dark[i] + (light[i] - dark[i]) * n;
                pixels.push((value.clamp(0.0, 1.0) * 255.0).round() as u8);
            }
            pixels.push(255);
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    fn average(pixels: &[u8]) -> [f32; 3] {
        let count = (pixels.len() / 4) as f32;
        let mut sum = [0.0f32; 3];
        for texel in pixels.as_chunks::<4>().0 {
            for i in 0..3 {
                sum[i] += f32::from(texel[i]);
            }
        }
        sum.map(|s| s / count / 255.0)
    }

    #[test]
    fn builtin_textures_are_deterministic_and_look_like_their_material() {
        for layer in BuiltinLayer::ALL {
            let a = builtin_layer_rgba(layer, 64);
            assert_eq!(a, builtin_layer_rgba(layer, 64));
            assert_eq!(a.len(), 64 * 64 * 4);
            // Not one flat colour.
            assert!(a.as_chunks::<4>().0.iter().any(|t| t[..] != a[..4]));
        }
        let grass = average(&builtin_layer_rgba(BuiltinLayer::Grass, 64));
        let rock = average(&builtin_layer_rgba(BuiltinLayer::Rock, 64));
        let dirt = average(&builtin_layer_rgba(BuiltinLayer::Dirt, 64));
        let sand = average(&builtin_layer_rgba(BuiltinLayer::Sand, 64));
        assert!(
            grass[1] > grass[0] && grass[1] > grass[2],
            "grass is green: {grass:?}"
        );
        assert!((rock[0] - rock[2]).abs() < 0.05, "rock is grey: {rock:?}");
        assert!(
            dirt[0] > dirt[2] && dirt[0] < 0.6,
            "dirt is brown: {dirt:?}"
        );
        assert!(
            sand.iter().sum::<f32>() > rock.iter().sum::<f32>(),
            "sand is bright"
        );
    }

    #[test]
    fn builtin_textures_wrap_at_the_edges() {
        // The lattice wraps, so the noise just past the last texel is the
        // noise at the first.
        let size = 64;
        for cells in [8, 32] {
            let first = value_noise(0, 5, size, cells, 1);
            let wrapped = value_noise(size, 5, size, cells, 1);
            assert!((first - wrapped).abs() < 1e-6);
        }
    }
}
