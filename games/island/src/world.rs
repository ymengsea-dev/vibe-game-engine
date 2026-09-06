//! Procedural content for the island: terrain, trees, rocks, grass, and
//! the textures they use.
//!
//! Everything here is generated in code. No art files, by decision —
//! the slice stays self-contained and reproducible, at the cost of
//! reading as stylized-simple rather than hand-authored.
//!
//! All functions are deterministic given the same seed, so a run is
//! reproducible and the pure ones are unit-testable without a GPU.

use engine::prelude::Vertex;
use glam::{Vec2, Vec3};

/// Half-width of the island's ground plane, in world units.
pub const ISLAND_RADIUS: f32 = 34.0;

/// Vertices per side of the terrain grid. 96x96 quads is enough for the
/// hills to read as smooth at this scale without a heavy mesh.
const TERRAIN_RESOLUTION: usize = 96;

/// A tiny deterministic PRNG, so the island is identical every run.
///
/// xorshift64* — not cryptographic, not trying to be. Local rather than
/// pulled from `engine_utils::Rng` so scattering logic can be tested in
/// isolation from engine state.
pub struct Rng(u64);

impl Rng {
    /// Seeds the generator. Zero is remapped, since xorshift is stuck at
    /// zero.
    pub fn new(seed: u64) -> Self {
        Self(if seed == 0 { 0x9E3779B97F4A7C15 } else { seed })
    }

    /// The next raw 64 bits.
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }

    /// A float in `[0, 1)`.
    pub fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u32 << 24) as f32
    }

    /// A float in `[min, max)`.
    pub fn range(&mut self, min: f32, max: f32) -> f32 {
        min + self.unit() * (max - min)
    }
}

/// Smooth value noise built from a hash, used for the terrain height.
///
/// Value noise rather than Perlin: fewer moving parts, and at this scale
/// the difference is invisible.
fn hash2(x: i32, y: i32) -> f32 {
    let h = (x as i64).wrapping_mul(374_761_393) ^ (y as i64).wrapping_mul(668_265_263);
    let h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    ((h ^ (h >> 16)) & 0xFFFF) as f32 / 65_535.0
}

/// Cosine-interpolated value noise at `p`.
fn value_noise(p: Vec2) -> f32 {
    let xi = p.x.floor() as i32;
    let yi = p.y.floor() as i32;
    let fx = p.x - xi as f32;
    let fy = p.y - yi as f32;
    // Smoothstep the fractional part so cell boundaries don't show.
    let sx = fx * fx * (3.0 - 2.0 * fx);
    let sy = fy * fy * (3.0 - 2.0 * fy);

    let n00 = hash2(xi, yi);
    let n10 = hash2(xi + 1, yi);
    let n01 = hash2(xi, yi + 1);
    let n11 = hash2(xi + 1, yi + 1);

    let top = n00 + (n10 - n00) * sx;
    let bottom = n01 + (n11 - n01) * sx;
    top + (bottom - top) * sy
}

/// How high the island's centre sits above the waterline before any
/// hills are added.
///
/// Without this the middle is wherever the noise happens to land, which
/// can be at or under water — and the player spawns there. A dome makes
/// "the centre is walkable land" a property of the terrain rather than
/// luck with the seed.
const CENTRE_ELEVATION: f32 = 2.2;

/// The island's surface height at world `(x, z)`.
///
/// A radial dome for the island's body, two octaves of value noise for
/// the hills on top, both faded out by the same falloff so the land dips
/// below zero at the edges and reads as an island rather than an
/// infinite plain.
pub fn height_at(x: f32, z: f32) -> f32 {
    let p = Vec2::new(x, z);
    let hills = value_noise(p * 0.045) * 3.2 + value_noise(p * 0.11) * 0.9;

    // 1 at the centre, 0 past the shoreline.
    let distance = p.length() / ISLAND_RADIUS;
    let falloff = (1.0 - distance * distance).clamp(0.0, 1.0);

    (CENTRE_ELEVATION + hills) * falloff - (1.0 - falloff) * 2.5
}

/// The surface normal at world `(x, z)`, from finite differences.
pub fn normal_at(x: f32, z: f32) -> Vec3 {
    const EPS: f32 = 0.35;
    let dx = height_at(x + EPS, z) - height_at(x - EPS, z);
    let dz = height_at(x, z + EPS) - height_at(x, z - EPS);
    Vec3::new(-dx, 2.0 * EPS, -dz).normalize_or(Vec3::Y)
}

/// Builds the terrain mesh: a grid sampled from [`height_at`].
pub fn terrain_mesh() -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices = Vec::with_capacity(TERRAIN_RESOLUTION * TERRAIN_RESOLUTION);
    let step = (ISLAND_RADIUS * 2.0) / (TERRAIN_RESOLUTION - 1) as f32;

    for row in 0..TERRAIN_RESOLUTION {
        for column in 0..TERRAIN_RESOLUTION {
            let x = -ISLAND_RADIUS + column as f32 * step;
            let z = -ISLAND_RADIUS + row as f32 * step;
            vertices.push(Vertex {
                position: [x, height_at(x, z), z],
                normal: normal_at(x, z).to_array(),
                // Tiled so the ground texture repeats rather than
                // stretching one texel across the whole island.
                uv: [column as f32 / 6.0, row as f32 / 6.0],
            });
        }
    }

    let mut indices = Vec::with_capacity((TERRAIN_RESOLUTION - 1) * (TERRAIN_RESOLUTION - 1) * 6);
    for row in 0..TERRAIN_RESOLUTION - 1 {
        for column in 0..TERRAIN_RESOLUTION - 1 {
            let i = (row * TERRAIN_RESOLUTION + column) as u32;
            let right = i + 1;
            let below = i + TERRAIN_RESOLUTION as u32;
            let below_right = below + 1;
            indices.extend_from_slice(&[i, below, right, right, below, below_right]);
        }
    }

    (vertices, indices)
}

/// Appends a flat-shaded triangle to a mesh under construction.
fn push_triangle(vertices: &mut Vec<Vertex>, indices: &mut Vec<u32>, a: Vec3, b: Vec3, c: Vec3) {
    let normal = (b - a).cross(c - a).normalize_or(Vec3::Y).to_array();
    let base = vertices.len() as u32;
    for (position, uv) in [(a, [0.5, 0.0]), (b, [0.0, 1.0]), (c, [1.0, 1.0])] {
        vertices.push(Vertex {
            position: position.to_array(),
            normal,
            uv,
        });
    }
    indices.extend_from_slice(&[base, base + 1, base + 2]);
}

/// The trunk of a tree, as a tapered prism. Opaque, textured with bark.
///
/// Split from the canopy since T-15: the two now need different
/// materials (opaque bark vs alpha-cut leaves), and one mesh can only
/// carry one.
pub fn trunk_mesh(rng: &mut Rng) -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();

    let trunk_height = rng.range(1.4, 2.2);
    let trunk_radius = rng.range(0.11, 0.18);
    let sides = 6;

    for side in 0..sides {
        let a0 = side as f32 / sides as f32 * std::f32::consts::TAU;
        let a1 = (side + 1) as f32 / sides as f32 * std::f32::consts::TAU;
        let (s0, c0) = a0.sin_cos();
        let (s1, c1) = a1.sin_cos();
        let bottom0 = Vec3::new(c0 * trunk_radius, 0.0, s0 * trunk_radius);
        let bottom1 = Vec3::new(c1 * trunk_radius, 0.0, s1 * trunk_radius);
        let top0 = bottom0 + Vec3::Y * trunk_height;
        let top1 = bottom1 + Vec3::Y * trunk_height;
        push_triangle(&mut vertices, &mut indices, bottom0, top0, bottom1);
        push_triangle(&mut vertices, &mut indices, bottom1, top0, top1);
    }

    (vertices, indices)
}

/// A tree's canopy as intersecting alpha-cut cards.
///
/// Three quads crossed through the trunk axis, each textured with a leaf
/// cluster whose alpha does the shaping. This is how real-time foliage is
/// actually built, and it is what [`crate::world::foliage_texture`]'s
/// alpha channel exists for — before T-15 the renderer had no alpha-mask
/// material, so the canopy had to be solid cones instead.
///
/// The cards are unlit-looking from edge-on, which is normal: the fix is
/// two-sided lighting, not more geometry.
pub fn canopy_mesh(rng: &mut Rng) -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();

    let cards = 3;
    let height = rng.range(2.0, 3.0);
    let width = rng.range(1.6, 2.4);
    let base_y = rng.range(1.0, 1.5);

    for card in 0..cards {
        let angle = card as f32 / cards as f32 * std::f32::consts::PI;
        let (s, c) = angle.sin_cos();
        let half = Vec3::new(c * width * 0.5, 0.0, s * width * 0.5);
        let bottom = Vec3::Y * base_y;
        let top = Vec3::Y * (base_y + height);

        let base = vertices.len() as u32;
        // Normal faces outward along the card; both faces are drawn
        // because the transparent/cutout path disables back-face culling.
        let normal = Vec3::new(-s, 0.35, c).normalize().to_array();
        for (position, uv) in [
            (bottom - half, [0.0, 1.0]),
            (bottom + half, [1.0, 1.0]),
            (top + half, [1.0, 0.0]),
            (top - half, [0.0, 0.0]),
        ] {
            vertices.push(Vertex {
                position: position.to_array(),
                normal,
                uv,
            });
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    (vertices, indices)
}

/// A lumpy low-poly boulder: an icosphere-ish blob with jittered radii.
pub fn rock_mesh(rng: &mut Rng) -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();

    let rings = 4;
    let sectors = 7;
    let base_radius = rng.range(0.35, 0.85);

    // Per-vertex radius jitter, sampled once so adjacent faces agree and
    // the rock looks faceted rather than noisy.
    let mut radii = vec![0.0; (rings + 1) * (sectors + 1)];
    for radius in radii.iter_mut() {
        *radius = base_radius * rng.range(0.75, 1.25);
    }
    let at = |ring: usize, sector: usize, radii: &[f32]| -> Vec3 {
        let phi = ring as f32 / rings as f32 * std::f32::consts::PI;
        let theta = sector as f32 / sectors as f32 * std::f32::consts::TAU;
        let radius = radii[ring * (sectors + 1) + sector];
        Vec3::new(
            phi.sin() * theta.cos() * radius,
            phi.cos() * radius * 0.7,
            phi.sin() * theta.sin() * radius,
        )
    };

    for ring in 0..rings {
        for sector in 0..sectors {
            let a = at(ring, sector, &radii);
            let b = at(ring + 1, sector, &radii);
            let c = at(ring, sector + 1, &radii);
            let d = at(ring + 1, sector + 1, &radii);
            push_triangle(&mut vertices, &mut indices, a, b, c);
            push_triangle(&mut vertices, &mut indices, c, b, d);
        }
    }

    (vertices, indices)
}

/// One scattered prop: where it goes, how big, how turned.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    /// World position, already sitting on the terrain surface.
    pub position: Vec3,
    /// Rotation about Y, radians.
    pub yaw: f32,
    /// Uniform scale.
    pub scale: f32,
}

/// Scatters `count` props across the island, rejecting anywhere too steep
/// or below the waterline.
///
/// Pure over `seed`, so the same seed always produces the same island and
/// the rules are testable without a GPU.
pub fn scatter(seed: u64, count: usize, min_height: f32, max_slope: f32) -> Vec<Placement> {
    let mut rng = Rng::new(seed);
    let mut placements = Vec::with_capacity(count);

    // Bounded attempts: rejection sampling must never spin forever on a
    // seed where the constraints are hard to satisfy.
    let max_attempts = count * 40;
    for _ in 0..max_attempts {
        if placements.len() >= count {
            break;
        }
        let x = rng.range(-ISLAND_RADIUS, ISLAND_RADIUS);
        let z = rng.range(-ISLAND_RADIUS, ISLAND_RADIUS);
        let height = height_at(x, z);
        if height < min_height {
            continue;
        }
        // `normal.y` is 1 on flat ground and falls off as it steepens.
        if normal_at(x, z).y < max_slope {
            continue;
        }
        placements.push(Placement {
            position: Vec3::new(x, height, z),
            yaw: rng.range(0.0, std::f32::consts::TAU),
            scale: rng.range(0.75, 1.35),
        });
    }

    placements
}

/// Generates an RGBA8 texture by evaluating `shade` per texel.
fn texture(size: u32, mut shade: impl FnMut(f32, f32) -> [u8; 4]) -> (u32, u32, Vec<u8>) {
    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            pixels.extend_from_slice(&shade(x as f32 / size as f32, y as f32 / size as f32));
        }
    }
    (size, size, pixels)
}

/// Mottled grass, with a little noise so large flat areas aren't a
/// single flat colour.
pub fn grass_texture() -> (u32, u32, Vec<u8>) {
    texture(128, |u, v| {
        let n = value_noise(Vec2::new(u * 18.0, v * 18.0));
        let fine = value_noise(Vec2::new(u * 60.0, v * 60.0));
        let shade = 0.72 + n * 0.22 + fine * 0.10;
        [
            (74.0 * shade) as u8,
            (122.0 * shade) as u8,
            (58.0 * shade) as u8,
            255,
        ]
    })
}

/// Bark: vertical streaks.
pub fn bark_texture() -> (u32, u32, Vec<u8>) {
    texture(64, |u, v| {
        let streak = value_noise(Vec2::new(u * 30.0, v * 4.0));
        let shade = 0.7 + streak * 0.45;
        [
            (94.0 * shade) as u8,
            (66.0 * shade) as u8,
            (44.0 * shade) as u8,
            255,
        ]
    })
}

/// A leaf cluster for an alpha-cut canopy card.
///
/// The **alpha channel is the shape**: a soft blob mask, eroded by noise
/// so the silhouette breaks up into leaf-like clumps instead of reading
/// as a rectangle. Everything below the material's cutoff is discarded
/// outright, which is what makes a flat quad look like foliage.
pub fn foliage_texture() -> (u32, u32, Vec<u8>) {
    texture(128, |u, v| {
        let n = value_noise(Vec2::new(u * 22.0, v * 22.0));
        let shade = 0.62 + n * 0.45;

        // Distance from the card's centre-bottom, so the mask forms a
        // rough teardrop rather than a circle.
        let dx = (u - 0.5) * 2.0;
        let dy = (v - 0.35) * 1.7;
        let radial = 1.0 - (dx * dx + dy * dy).sqrt();

        // Two noise octaves erode the edge into clumps.
        let erosion = value_noise(Vec2::new(u * 9.0, v * 9.0)) * 0.55
            + value_noise(Vec2::new(u * 26.0, v * 26.0)) * 0.3;
        let mask = radial * 0.9 + erosion - 0.42;

        [
            (44.0 * shade) as u8,
            (108.0 * shade) as u8,
            (48.0 * shade) as u8,
            if mask > 0.0 { 255 } else { 0 },
        ]
    })
}

/// Grey stone with speckle.
pub fn stone_texture() -> (u32, u32, Vec<u8>) {
    texture(64, |u, v| {
        let n = value_noise(Vec2::new(u * 26.0, v * 26.0));
        let speckle = value_noise(Vec2::new(u * 90.0, v * 90.0));
        let shade = 0.68 + n * 0.3 + speckle * 0.14;
        [
            (132.0 * shade) as u8,
            (130.0 * shade) as u8,
            (126.0 * shade) as u8,
            255,
        ]
    })
}

/// A flat colour, for the player capsule and anything else that needs no
/// detail.
pub fn flat_texture(rgb: [u8; 3]) -> (u32, u32, Vec<u8>) {
    (1, 1, vec![rgb[0], rgb[1], rgb[2], 255])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_is_deterministic_for_a_seed() {
        let a: Vec<f32> = (0..8).map(|_| Rng::new(42).unit()).collect();
        let b: Vec<f32> = (0..8).map(|_| Rng::new(42).unit()).collect();
        assert_eq!(a, b);
    }

    #[test]
    fn rng_zero_seed_still_produces_variety() {
        let mut rng = Rng::new(0);
        let values: Vec<f32> = (0..16).map(|_| rng.unit()).collect();
        assert!(
            values.windows(2).any(|w| w[0] != w[1]),
            "a zero seed must not lock the generator",
        );
    }

    #[test]
    fn rng_unit_stays_in_range() {
        let mut rng = Rng::new(7);
        for _ in 0..2000 {
            let value = rng.unit();
            assert!((0.0..1.0).contains(&value), "out of range: {value}");
        }
    }

    #[test]
    fn island_is_above_water_at_the_centre_and_below_at_the_edge() {
        assert!(
            height_at(0.0, 0.0) >= CENTRE_ELEVATION,
            "the spawn point must be solid land, not luck with the seed",
        );
        let edge = ISLAND_RADIUS * 1.05;
        assert!(
            height_at(edge, 0.0) < 0.0,
            "past the shoreline should be below the waterline",
        );
    }

    #[test]
    fn the_whole_central_area_is_walkable_land() {
        // Not just the exact origin — the player can move before the
        // camera settles, so a ring around spawn must be land too.
        for i in 0..16 {
            let angle = i as f32 / 16.0 * std::f32::consts::TAU;
            let (s, c) = angle.sin_cos();
            let (x, z) = (c * 5.0, s * 5.0);
            assert!(
                height_at(x, z) > 0.0,
                "spawn ring is underwater at ({x:.1}, {z:.1})",
            );
        }
    }

    #[test]
    fn height_is_finite_everywhere_sampled() {
        for i in -40..40 {
            for j in -40..40 {
                let h = height_at(i as f32, j as f32);
                assert!(h.is_finite(), "height at ({i}, {j}) was {h}");
            }
        }
    }

    #[test]
    fn normals_are_unit_length_and_point_upward() {
        for i in -20..20 {
            let n = normal_at(i as f32, i as f32 * 0.7);
            assert!((n.length() - 1.0).abs() < 1e-3, "not normalised: {n:?}");
            assert!(n.y > 0.0, "terrain normals should never point down");
        }
    }

    #[test]
    fn terrain_mesh_is_well_formed() {
        let (vertices, indices) = terrain_mesh();
        assert_eq!(vertices.len(), TERRAIN_RESOLUTION * TERRAIN_RESOLUTION);
        assert_eq!(indices.len() % 3, 0);
        assert!(
            indices.iter().all(|&i| (i as usize) < vertices.len()),
            "every index must be in bounds",
        );
        assert!(
            vertices
                .iter()
                .all(|v| v.position.iter().all(|c| c.is_finite())),
            "no NaN positions",
        );
    }

    #[test]
    fn generated_props_have_in_bounds_indices() {
        let mut rng = Rng::new(3);
        for (vertices, indices) in [
            trunk_mesh(&mut rng),
            canopy_mesh(&mut rng),
            rock_mesh(&mut rng),
        ] {
            assert!(!vertices.is_empty());
            assert_eq!(indices.len() % 3, 0);
            assert!(indices.iter().all(|&i| (i as usize) < vertices.len()));
        }
    }

    #[test]
    fn scatter_respects_height_and_slope_limits() {
        let placements = scatter(11, 60, 0.4, 0.75);
        assert!(!placements.is_empty(), "some spots should qualify");
        for placement in &placements {
            assert!(placement.position.y >= 0.4, "placed below the limit");
            assert!(
                normal_at(placement.position.x, placement.position.z).y >= 0.75,
                "placed on a slope steeper than allowed",
            );
            assert!(placement.scale > 0.0);
        }
    }

    #[test]
    fn scatter_terminates_even_when_nothing_qualifies() {
        // An impossible constraint: nowhere is this high.
        let placements = scatter(5, 50, 10_000.0, 0.0);
        assert!(placements.is_empty(), "no spot should satisfy this");
    }

    #[test]
    fn scatter_is_reproducible() {
        assert_eq!(scatter(99, 30, 0.3, 0.7), scatter(99, 30, 0.3, 0.7));
    }

    #[test]
    fn textures_are_rgba8_and_correctly_sized() {
        for (width, height, pixels) in [
            grass_texture(),
            bark_texture(),
            foliage_texture(),
            stone_texture(),
            flat_texture([200, 80, 80]),
        ] {
            assert_eq!(
                pixels.len(),
                (width * height * 4) as usize,
                "pixel buffer must be exactly RGBA8",
            );
            assert!(pixels.iter().any(|&b| b > 0), "texture should not be black");
        }
    }
}
