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

/// A low-poly crown made from separate, faceted leaf clusters.
pub fn canopy_mesh(rng: &mut Rng) -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for (cluster, (offset, radius)) in [
        (Vec3::new(0.0, 2.65, 0.0), 0.50),
        (Vec3::new(0.42, 2.35, 0.08), 0.48),
        (Vec3::new(-0.40, 2.32, -0.10), 0.50),
        (Vec3::new(0.04, 2.25, 0.46), 0.48),
        (Vec3::new(-0.08, 2.25, -0.45), 0.48),
        (Vec3::new(0.68, 1.98, 0.04), 0.45),
        (Vec3::new(-0.65, 1.98, -0.08), 0.46),
        (Vec3::new(0.03, 1.92, 0.68), 0.44),
        (Vec3::new(-0.04, 1.92, -0.66), 0.45),
    ]
    .into_iter()
    .enumerate()
    {
        let rings = 4;
        let sectors = 8;
        let cluster_scale = rng.range(0.92, 1.08);
        for ring in 0..rings {
            for sector in 0..sectors {
                let point = |ring: usize, sector: usize| {
                    let phi = ring as f32 / rings as f32 * std::f32::consts::PI;
                    let theta = sector as f32 / sectors as f32 * std::f32::consts::TAU;
                    let wobble = 0.88
                        + value_noise(Vec2::new(
                            cluster as f32 * 11.0 + sector as f32,
                            ring as f32,
                        )) * 0.24;
                    offset
                        + Vec3::new(
                            phi.sin() * theta.cos() * radius * cluster_scale * wobble,
                            phi.cos() * radius * cluster_scale * 0.85,
                            phi.sin() * theta.sin() * radius * cluster_scale * wobble,
                        )
                };
                let a = point(ring, sector);
                let b = point(ring + 1, sector);
                let c = point(ring, sector + 1);
                let d = point(ring + 1, sector + 1);
                push_triangle(&mut vertices, &mut indices, a, b, c);
                push_triangle(&mut vertices, &mut indices, c, b, d);
            }
        }
    }
    (vertices, indices)
}

/// Thick, short limbs that stay inside the crown silhouette.
pub fn tree_branch_mesh(rng: &mut Rng) -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let root = Vec3::new(0.0, rng.range(1.25, 1.65), 0.0);
    for angle in [0.2_f32, 1.3, 2.3, 3.4, 4.4, 5.5] {
        let (sin, cos) = angle.sin_cos();
        let end = root + Vec3::new(cos * 0.62, 0.42, sin * 0.62);
        let side = Vec3::new(-sin, 0.0, cos) * 0.11;
        let up = Vec3::Y * 0.11;
        for (a, b) in [
            (root - side - up, root + side - up),
            (root + side - up, root + side + up),
            (root + side + up, root - side + up),
            (root - side + up, root - side - up),
        ] {
            push_triangle(&mut vertices, &mut indices, a, b, end);
        }
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
            push_triangle(&mut vertices, &mut indices, a, c, b);
            push_triangle(&mut vertices, &mut indices, c, d, b);
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

/// Authored region used to keep deterministic prop scatter composed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Biome {
    /// Open starting grassland.
    Meadow,
    /// Dense shaded woodland.
    Grove,
    /// Sparse, elevated final approach.
    Heights,
}

/// Radius that keeps props away from the authored walk route.
pub const ROUTE_CLEARANCE: f32 = 3.6;

/// Centre of the sheltered grove pond.
pub const POND_CENTER: Vec3 = Vec3::new(-23.0, 0.0, -3.0);
/// Horizontal pond bounds, shared by lilies and the runtime water surface.
pub const POND_HALF_EXTENT: Vec2 = Vec2::new(3.2, 2.4);

/// Still-water level for the shallow grove pond.
pub fn pond_surface_height() -> f32 {
    height_at(POND_CENTER.x, POND_CENTER.z) + 0.08
}

fn biome_bounds(biome: Biome) -> (Vec2, Vec2) {
    match biome {
        Biome::Meadow => (Vec2::new(-14.0, -4.0), Vec2::new(3.0, 15.0)),
        Biome::Grove => (Vec2::new(-27.0, -19.0), Vec2::new(-5.0, 5.0)),
        Biome::Heights => (Vec2::new(3.0, -28.0), Vec2::new(27.0, -11.0)),
    }
}

/// Ground-level route samples, including every objective clearing.
pub fn route_points() -> Vec<Vec3> {
    let mut points = vec![
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(-5.0, 0.0, 4.0),
        Vec3::new(-14.0, 0.0, 0.0),
        Vec3::new(-12.0, 0.0, -8.0),
        Vec3::new(-7.0, 0.0, -15.0),
        Vec3::new(2.0, 0.0, -18.0),
        Vec3::new(15.0, 0.0, -19.0),
    ];
    points.extend(crate::journey::landmarks().map(|landmark| landmark.position));
    points.push(crate::journey::summit_position());
    for point in &mut points {
        point.y = height_at(point.x, point.z);
    }
    points
}

/// Deterministically scatters props within one biome without blocking route clearings.
pub fn scatter_biome(biome: Biome, seed: u64, count: usize) -> Vec<Placement> {
    let mut rng = Rng::new(seed);
    let mut placements = Vec::with_capacity(count);
    let (min, max) = biome_bounds(biome);
    let route = route_points();
    for _ in 0..count.saturating_mul(60) {
        if placements.len() == count {
            break;
        }
        let x = rng.range(min.x, max.x);
        let z = rng.range(min.y, max.y);
        let height = height_at(x, z);
        if height < 0.35 || normal_at(x, z).y < 0.72 {
            continue;
        }
        let position = Vec3::new(x, height, z);
        if route.iter().any(|point| {
            let delta = position - *point;
            delta.x * delta.x + delta.z * delta.z < ROUTE_CLEARANCE * ROUTE_CLEARANCE
        }) {
            continue;
        }
        placements.push(Placement {
            position,
            yaw: rng.range(0.0, std::f32::consts::TAU),
            scale: rng.range(0.75, 1.35),
        });
    }
    placements
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

fn box_mesh(centre: Vec3, half: Vec3) -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let corners = [
        Vec3::new(-1.0, -1.0, -1.0),
        Vec3::new(1.0, -1.0, -1.0),
        Vec3::new(1.0, 1.0, -1.0),
        Vec3::new(-1.0, 1.0, -1.0),
        Vec3::new(-1.0, -1.0, 1.0),
        Vec3::new(1.0, -1.0, 1.0),
        Vec3::new(1.0, 1.0, 1.0),
        Vec3::new(-1.0, 1.0, 1.0),
    ];
    for [a, b, c, d] in [
        [0, 1, 2, 3],
        [5, 4, 7, 6],
        [4, 0, 3, 7],
        [1, 5, 6, 2],
        [3, 2, 6, 7],
        [4, 5, 1, 0],
    ] {
        let a = centre + corners[a] * half;
        let b = centre + corners[b] * half;
        let c = centre + corners[c] * half;
        let d = centre + corners[d] * half;
        push_triangle(&mut vertices, &mut indices, a, b, c);
        push_triangle(&mut vertices, &mut indices, a, c, d);
    }
    (vertices, indices)
}

fn joined_mesh(
    parts: impl IntoIterator<Item = (Vec<Vertex>, Vec<u32>)>,
) -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for (part_vertices, part_indices) in parts {
        let offset = vertices.len() as u32;
        vertices.extend(part_vertices);
        indices.extend(part_indices.into_iter().map(|index| index + offset));
    }
    (vertices, indices)
}

fn pitched_roof_mesh(
    half_width: f32,
    half_depth: f32,
    eave_y: f32,
    ridge_y: f32,
) -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let left_front = Vec3::new(-half_width, eave_y, -half_depth);
    let right_front = Vec3::new(half_width, eave_y, -half_depth);
    let left_back = Vec3::new(-half_width, eave_y, half_depth);
    let right_back = Vec3::new(half_width, eave_y, half_depth);
    let ridge_front = Vec3::new(0.0, ridge_y, -half_depth);
    let ridge_back = Vec3::new(0.0, ridge_y, half_depth);
    for [a, b, c] in [
        [left_front, ridge_front, ridge_back],
        [left_front, ridge_back, left_back],
        [ridge_front, right_front, right_back],
        [ridge_front, right_back, ridge_back],
        [left_front, right_front, ridge_front],
        [left_back, ridge_back, right_back],
    ] {
        push_triangle(&mut vertices, &mut indices, a, b, c);
    }
    (vertices, indices)
}

/// Low, rounded-looking foliage cluster for meadow and grove layering.
pub fn shrub_mesh(rng: &mut Rng) -> (Vec<Vertex>, Vec<u32>) {
    let mut mesh = rock_mesh(rng);
    for vertex in &mut mesh.0 {
        vertex.position[1] = vertex.position[1].max(-0.25) * 0.7 + 0.55;
    }
    mesh
}

/// A fallen log that gains its horizontal orientation from its scene transform.
pub fn log_mesh(rng: &mut Rng) -> (Vec<Vertex>, Vec<u32>) {
    trunk_mesh(rng)
}

/// A crossed-card flower cluster with a small readable silhouette.
pub fn flower_mesh(_rng: &mut Rng) -> (Vec<Vertex>, Vec<u32>) {
    joined_mesh([flower_stem_mesh(), flower_leaf_mesh(), flower_petal_mesh()])
}

/// A small cluster of upright grass blades with several viewing angles.
pub fn grass_clump_mesh() -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for (angle, height) in [(0.0_f32, 0.42), (1.1, 0.56), (2.3, 0.48), (3.5, 0.62)] {
        let (sin, cos) = angle.sin_cos();
        let width = Vec3::new(cos * 0.07, 0.0, sin * 0.07);
        let base = Vec3::new(sin * 0.11, 0.0, cos * 0.11);
        let tip = base + Vec3::new(sin * 0.09, height, cos * 0.09);
        push_triangle(&mut vertices, &mut indices, base - width, base + width, tip);
        push_triangle(
            &mut vertices,
            &mut indices,
            base + width * 0.4,
            base - width * 0.4 + Vec3::new(sin * 0.06, 0.0, cos * 0.06),
            tip,
        );
    }
    (vertices, indices)
}

/// One solid, faceted leaf cluster. Scene composition places several around
/// every tree so foliage retains gaps, branches, and a readable silhouette.
pub fn tree_leaf_cluster_mesh(rng: &mut Rng) -> (Vec<Vertex>, Vec<u32>) {
    let (mut vertices, indices) = rock_mesh(rng);
    for vertex in &mut vertices {
        let position = Vec3::from_array(vertex.position);
        vertex.position = (position * Vec3::new(0.72, 0.56, 0.72)).to_array();
    }
    (vertices, indices)
}

/// Narrow solid stem for a flower, exported independently from its petals.
pub fn flower_stem_mesh() -> (Vec<Vertex>, Vec<u32>) {
    box_mesh(Vec3::new(0.0, 0.28, 0.0), Vec3::new(0.035, 0.28, 0.035))
}

/// Four thick petals around the flower head, not a flat card.
pub fn flower_petal_mesh() -> (Vec<Vertex>, Vec<u32>) {
    let mut parts = Vec::new();
    for angle in [0.0_f32, std::f32::consts::FRAC_PI_2] {
        let (sin, cos) = angle.sin_cos();
        let offset = Vec3::new(cos * 0.13, 0.58, sin * 0.13);
        parts.push(box_mesh(
            offset,
            Vec3::new(0.18 * cos.abs() + 0.04, 0.05, 0.18 * sin.abs() + 0.04),
        ));
    }
    joined_mesh(parts)
}

/// Small angled leaves that give flower clusters visible depth near ground.
pub fn flower_leaf_mesh() -> (Vec<Vertex>, Vec<u32>) {
    joined_mesh([
        box_mesh(Vec3::new(0.10, 0.20, 0.0), Vec3::new(0.13, 0.025, 0.055)),
        box_mesh(Vec3::new(-0.09, 0.28, 0.04), Vec3::new(0.11, 0.025, 0.05)),
    ])
}

/// A simple lantern post with a cube light housing.
pub fn lantern_mesh() -> (Vec<Vertex>, Vec<u32>) {
    box_mesh(Vec3::new(0.0, 0.65, 0.0), Vec3::new(0.16, 0.65, 0.16))
}

/// A reusable stone marker for arches, shrines, and lookouts.
pub fn landmark_mesh() -> (Vec<Vertex>, Vec<u32>) {
    box_mesh(Vec3::new(0.0, 0.7, 0.0), Vec3::new(0.65, 0.7, 0.28))
}

/// Compact timber cottage silhouette: stone base, walls, steep roof, and chimney.
pub fn cottage_mesh() -> (Vec<Vertex>, Vec<u32>) {
    joined_mesh([
        cottage_foundation_mesh(),
        cottage_walls_mesh(),
        cottage_roof_mesh(),
        cottage_chimney_mesh(),
        cottage_window_mesh(),
    ])
}

/// Stone foundation of the cottage.
pub fn cottage_foundation_mesh() -> (Vec<Vertex>, Vec<u32>) {
    box_mesh(Vec3::new(0.0, 0.22, 0.0), Vec3::new(1.8, 0.22, 1.45))
}

/// Timber cottage wall volume.
pub fn cottage_walls_mesh() -> (Vec<Vertex>, Vec<u32>) {
    box_mesh(Vec3::new(0.0, 1.0, 0.0), Vec3::new(1.55, 0.58, 1.2))
}

/// Steep clay cottage roof.
pub fn cottage_roof_mesh() -> (Vec<Vertex>, Vec<u32>) {
    pitched_roof_mesh(1.9, 1.48, 1.55, 2.75)
}

/// Stone cottage chimney.
pub fn cottage_chimney_mesh() -> (Vec<Vertex>, Vec<u32>) {
    box_mesh(Vec3::new(0.92, 2.15, 0.35), Vec3::new(0.18, 0.60, 0.18))
}

/// Recessed front cottage windows.
pub fn cottage_window_mesh() -> (Vec<Vertex>, Vec<u32>) {
    joined_mesh([
        box_mesh(Vec3::new(-0.72, 1.03, -1.23), Vec3::new(0.30, 0.28, 0.04)),
        box_mesh(Vec3::new(0.72, 1.03, -1.23), Vec3::new(0.30, 0.28, 0.04)),
    ])
}

/// Short timber footbridge spanning the pond edge.
pub fn bridge_mesh() -> (Vec<Vertex>, Vec<u32>) {
    joined_mesh([bridge_deck_mesh(), bridge_rail_mesh()])
}

/// Timber deck of the pond bridge.
pub fn bridge_deck_mesh() -> (Vec<Vertex>, Vec<u32>) {
    box_mesh(Vec3::new(0.0, 0.16, 0.0), Vec3::new(1.65, 0.16, 0.72))
}

/// Raised timber rails of the pond bridge.
pub fn bridge_rail_mesh() -> (Vec<Vertex>, Vec<u32>) {
    joined_mesh([
        box_mesh(Vec3::new(0.0, 0.57, -0.62), Vec3::new(1.72, 0.08, 0.08)),
        box_mesh(Vec3::new(0.0, 0.57, 0.62), Vec3::new(1.72, 0.08, 0.08)),
    ])
}

/// Low garden fence section, placed in repeated authored rows.
pub fn fence_mesh() -> (Vec<Vertex>, Vec<u32>) {
    joined_mesh([fence_post_mesh(), fence_rail_mesh()])
}

/// Upright posts of one garden fence section.
pub fn fence_post_mesh() -> (Vec<Vertex>, Vec<u32>) {
    joined_mesh([
        box_mesh(Vec3::new(-0.8, 0.42, 0.0), Vec3::new(0.08, 0.42, 0.08)),
        box_mesh(Vec3::new(0.8, 0.42, 0.0), Vec3::new(0.08, 0.42, 0.08)),
    ])
}

/// Horizontal rail of one garden fence section.
pub fn fence_rail_mesh() -> (Vec<Vertex>, Vec<u32>) {
    box_mesh(Vec3::new(0.0, 0.48, 0.0), Vec3::new(0.9, 0.07, 0.06))
}

/// Flat lily pad cluster for the calm grove pond.
pub fn lily_mesh() -> (Vec<Vertex>, Vec<u32>) {
    joined_mesh([lily_pad_mesh(), lily_blossom_mesh()])
}

/// Floating pad of a pond lily.
pub fn lily_pad_mesh() -> (Vec<Vertex>, Vec<u32>) {
    box_mesh(Vec3::new(0.0, 0.025, 0.0), Vec3::new(0.48, 0.025, 0.40))
}

/// Raised blossom of a pond lily.
pub fn lily_blossom_mesh() -> (Vec<Vertex>, Vec<u32>) {
    box_mesh(Vec3::new(0.0, 0.10, 0.0), Vec3::new(0.13, 0.10, 0.13))
}

/// Weathered stone ruin used at the windward lookout.
pub fn ruin_mesh() -> (Vec<Vertex>, Vec<u32>) {
    joined_mesh([ruin_pillar_mesh(), ruin_lintel_mesh()])
}

/// Weathered upright stones and base of the lookout ruin.
pub fn ruin_pillar_mesh() -> (Vec<Vertex>, Vec<u32>) {
    joined_mesh([
        box_mesh(Vec3::new(-0.88, 0.72, 0.0), Vec3::new(0.34, 0.72, 0.34)),
        box_mesh(Vec3::new(0.88, 0.95, 0.0), Vec3::new(0.34, 0.95, 0.34)),
        box_mesh(Vec3::new(0.0, 0.28, 0.0), Vec3::new(1.35, 0.28, 0.48)),
    ])
}

/// Crosspiece of the lookout ruin.
pub fn ruin_lintel_mesh() -> (Vec<Vertex>, Vec<u32>) {
    box_mesh(Vec3::new(0.0, 1.55, 0.0), Vec3::new(1.3, 0.22, 0.34))
}

/// Static lily positions inside the pond. They have no colliders.
pub fn lily_placements() -> Vec<Placement> {
    [
        (-1.4, -0.7),
        (-0.4, 0.65),
        (0.8, -0.25),
        (1.6, 0.85),
        (0.25, -1.15),
    ]
    .into_iter()
    .map(|(x, z)| Placement {
        position: Vec3::new(
            POND_CENTER.x + x,
            pond_surface_height() + 0.03,
            POND_CENTER.z + z,
        ),
        yaw: 0.0,
        scale: 1.0,
    })
    .collect()
}

/// Landmark-adjacent scenery placed outside all route clearings.
pub fn storybook_prop_placements() -> Vec<Placement> {
    [(-1.0, 10.5), (-3.0, 12.7), (-23.0, -0.2), (11.0, -26.5)]
        .into_iter()
        .map(|(x, z)| Placement {
            position: Vec3::new(x, height_at(x, z), z),
            yaw: 0.0,
            scale: 1.0,
        })
        .collect()
}

fn authored_placements(points: &[(f32, f32)]) -> Vec<Placement> {
    points
        .iter()
        .map(|&(x, z)| Placement {
            position: Vec3::new(x, height_at(x, z), z),
            yaw: 0.0,
            scale: 1.0,
        })
        .collect()
}

/// Route-safe foreground locations around the meadow cottage and garden.
pub fn meadow_placements() -> Vec<Placement> {
    authored_placements(&[
        (-4.6, 14.0),
        (-3.8, 11.4),
        (-2.4, 13.5),
        (-0.2, 12.8),
        (1.4, 10.8),
        (0.8, 14.2),
    ])
}

/// Route-safe perimeter locations framing the pond without filling its view.
pub fn grove_placements() -> Vec<Placement> {
    authored_placements(&[
        (-26.1, -0.8),
        (-25.6, -5.0),
        (-20.1, -1.0),
        (-20.6, -5.2),
        (-24.7, 0.4),
    ])
}

/// Route-safe sparse locations for the windward lookout.
pub fn heights_placements() -> Vec<Placement> {
    authored_placements(&[(11.0, -26.5), (17.5, -25.5), (24.0, -13.0), (8.0, -24.0)])
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

/// Warm earth texture for path props and landmark bases.
pub fn path_texture() -> (u32, u32, Vec<u8>) {
    texture(96, |u, v| {
        let coarse = value_noise(Vec2::new(u * 12.0, v * 12.0));
        let fine = value_noise(Vec2::new(u * 48.0, v * 48.0));
        let shade = 0.68 + coarse * 0.22 + fine * 0.10;
        [
            (150.0 * shade) as u8,
            (104.0 * shade) as u8,
            (67.0 * shade) as u8,
            255,
        ]
    })
}

/// Petal-pink texture for the small flower card mesh.
pub fn flower_texture() -> (u32, u32, Vec<u8>) {
    texture(64, |u, v| {
        let dx = (u - 0.5) * 2.0;
        let dy = (v - 0.62) * 2.0;
        let petal = 1.0 - (dx * dx + dy * dy).sqrt();
        let centre = 1.0 - ((u - 0.5).abs() + (v - 0.58).abs()) * 3.0;
        if petal > 0.0 {
            if centre > 0.0 {
                [226, 188, 112, 255]
            } else {
                [211, 142, 160, 255]
            }
        } else {
            [0, 0, 0, 0]
        }
    })
}

/// Amber lantern texture with a soft central glow.
pub fn lantern_texture() -> (u32, u32, Vec<u8>) {
    texture(32, |u, v| {
        let dx = u - 0.5;
        let dy = v - 0.5;
        let glow = (1.0 - (dx * dx + dy * dy).sqrt() * 1.7).clamp(0.0, 1.0);
        [
            (155.0 + 100.0 * glow) as u8,
            (72.0 + 150.0 * glow) as u8,
            (22.0 + 70.0 * glow) as u8,
            255,
        ]
    })
}

/// A flat colour, for the player capsule and anything else that needs no
/// detail.
pub fn flat_texture(rgb: [u8; 3]) -> (u32, u32, Vec<u8>) {
    (1, 1, vec![rgb[0], rgb[1], rgb[2], 255])
}

/// The footfall sound, as 16-bit mono samples plus their sample rate.
///
/// A deterministic pseudo-noise burst under a low thump, with a fast
/// decay: cheap, and it reads as a footstep rather than a tone. Returned
/// as samples rather than an encoded file so the baker can write a
/// `.wav` and a test can inspect the waveform.
pub fn footstep_samples() -> (u32, Vec<i16>) {
    const RATE: u32 = 22_050;
    const SECONDS: f32 = 0.16;
    let sample_count = (RATE as f32 * SECONDS) as usize;

    let mut noise = Rng::new(0x5EED);
    let mut samples = Vec::with_capacity(sample_count);
    for i in 0..sample_count {
        let t = i as f32 / RATE as f32;
        let envelope = (1.0 - t / SECONDS).max(0.0).powi(3);
        let thump = (t * 70.0 * std::f32::consts::TAU).sin() * 0.5;
        let hiss = (noise.unit() * 2.0 - 1.0) * 0.35;
        let value = (thump + hiss) * envelope * 0.5;
        samples.push((value * i16::MAX as f32) as i16);
    }
    (RATE, samples)
}

/// A looping wind bed for the island's ambience: filtered noise with a
/// slow swell, seamless end-to-end so it can loop without a click.
///
/// Procedural like everything else here — the file the baker writes is
/// the asset; this generator is not consulted at runtime.
pub fn wind_samples() -> (u32, Vec<i16>) {
    const RATE: u32 = 22_050;
    const SECONDS: f32 = 4.0;
    let sample_count = (RATE as f32 * SECONDS) as usize;

    let mut noise = Rng::new(0x_1234_5678);
    let mut smoothed = 0.0f32;
    let mut samples = Vec::with_capacity(sample_count);
    for i in 0..sample_count {
        let t = i as f32 / RATE as f32;
        // One-pole low pass over white noise: wind is mostly low
        // frequency, and unfiltered noise reads as static.
        let white = noise.unit() * 2.0 - 1.0;
        smoothed += (white - smoothed) * 0.02;
        // Two swells per loop, and a cosine window at the seam so the
        // loop point is inaudible.
        let swell = 0.55 + 0.45 * (t / SECONDS * std::f32::consts::TAU * 2.0).sin();
        let seam = (t / SECONDS * std::f32::consts::TAU)
            .cos()
            .mul_add(-0.5, 0.5);
        let value = smoothed * swell * (0.35 + 0.65 * seam) * 0.6;
        samples.push((value.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
    }
    (RATE, samples)
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
    fn canopy_is_a_real_three_dimensional_crown() {
        let (vertices, indices) = canopy_mesh(&mut Rng::new(9));
        assert!(
            vertices.len() >= 60,
            "a canopy needs volume, not crossed cards"
        );
        assert!(indices.len() >= 120);
    }

    #[test]
    fn canopy_uses_many_small_leaf_clusters_not_giant_facets() {
        let (vertices, indices) = canopy_mesh(&mut Rng::new(9));
        assert!(
            vertices.len() >= 1_500,
            "leaf clusters need fine silhouettes"
        );
        assert!(indices.len() >= 1_500);
    }

    #[test]
    fn storybook_trees_have_branches_and_leaf_volume() {
        let (branches, branch_indices) = tree_branch_mesh(&mut Rng::new(4));
        let (leaves, leaf_indices) = canopy_mesh(&mut Rng::new(4));
        assert!(branches.len() >= 36, "trees need visible branch geometry");
        assert!(leaves.iter().any(|vertex| vertex.position[1] > 2.0));
        assert!(
            branch_indices
                .iter()
                .all(|&i| (i as usize) < branches.len())
        );
        assert!(leaf_indices.iter().all(|&i| (i as usize) < leaves.len()));
    }

    #[test]
    fn grass_and_flowers_have_real_three_dimensional_extent() {
        for (vertices, indices) in [grass_clump_mesh(), flower_mesh(&mut Rng::new(8))] {
            let min = vertices
                .iter()
                .fold(Vec3::splat(f32::INFINITY), |min, vertex| {
                    min.min(Vec3::from_array(vertex.position))
                });
            let max = vertices
                .iter()
                .fold(Vec3::splat(f32::NEG_INFINITY), |max, vertex| {
                    max.max(Vec3::from_array(vertex.position))
                });
            assert!((max - min).min_element() > 0.01);
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

    #[test]
    fn route_landmarks_and_path_are_walkable() {
        let points = route_points();
        assert!(points.len() >= 6, "the journey needs a visible route");
        for point in points {
            assert!(
                height_at(point.x, point.z) > 0.0,
                "route is underwater at {point:?}"
            );
            assert!(
                normal_at(point.x, point.z).y > 0.72,
                "route is too steep at {point:?}",
            );
        }
    }

    #[test]
    fn biome_scatter_is_reproducible_and_keeps_objectives_clear() {
        let placements = scatter_biome(Biome::Grove, 42, 80);
        assert_eq!(placements, scatter_biome(Biome::Grove, 42, 80));
        assert!(!placements.is_empty());
        for placement in placements {
            for point in route_points() {
                assert!(
                    placement.position.distance_squared(point) > ROUTE_CLEARANCE * ROUTE_CLEARANCE,
                    "prop blocks route at {:?}",
                    placement.position
                );
            }
        }
    }

    #[test]
    fn showcase_prop_meshes_are_well_formed() {
        let mut rng = Rng::new(77);
        for (vertices, indices) in [
            shrub_mesh(&mut rng),
            log_mesh(&mut rng),
            flower_mesh(&mut rng),
            lantern_mesh(),
            landmark_mesh(),
        ] {
            assert!(!vertices.is_empty());
            assert_eq!(indices.len() % 3, 0);
            assert!(
                indices
                    .iter()
                    .all(|&index| (index as usize) < vertices.len())
            );
        }
    }

    #[test]
    fn storybook_prop_meshes_are_well_formed() {
        for (vertices, indices) in [
            cottage_mesh(),
            bridge_mesh(),
            fence_mesh(),
            lily_mesh(),
            ruin_mesh(),
        ] {
            assert!(!vertices.is_empty());
            assert_eq!(indices.len() % 3, 0);
            assert!(
                indices
                    .iter()
                    .all(|&index| (index as usize) < vertices.len())
            );
        }
    }

    #[test]
    fn cottage_has_a_pitched_roof_silhouette() {
        let (vertices, _) = cottage_mesh();
        let highest = vertices
            .iter()
            .map(|vertex| vertex.position[1])
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(highest > 2.5, "cottage needs a steep roof above its walls");
    }

    #[test]
    fn lilies_stay_inside_the_authored_pond() {
        for placement in lily_placements() {
            let local = Vec2::new(
                placement.position.x - POND_CENTER.x,
                placement.position.z - POND_CENTER.z,
            );
            assert!(local.x.abs() <= POND_HALF_EXTENT.x);
            assert!(local.y.abs() <= POND_HALF_EXTENT.y);
            assert!(
                route_points().iter().all(|point| {
                    placement.position.distance_squared(*point) >= ROUTE_CLEARANCE.powi(2)
                }),
                "lily blocks route at {:?}",
                placement.position
            );
        }
    }

    #[test]
    fn authored_storybook_props_keep_route_clearings_walkable() {
        for placement in storybook_prop_placements() {
            assert!(
                route_points().iter().all(|point| {
                    placement.position.distance_squared(*point) >= ROUTE_CLEARANCE.powi(2)
                }),
                "authored prop blocks route at {:?}",
                placement.position
            );
        }
    }

    #[test]
    fn showcase_textures_are_rgba8() {
        for (width, height, pixels) in [path_texture(), flower_texture(), lantern_texture()] {
            assert_eq!(pixels.len(), (width * height * 4) as usize);
            assert!(pixels.iter().any(|&value| value > 0));
        }
    }

    #[test]
    fn storybook_tree_has_branch_and_leaf_geometry() {
        let mut rng = Rng::new(73);
        for (vertices, indices) in [
            trunk_mesh(&mut rng),
            tree_branch_mesh(&mut rng),
            tree_leaf_cluster_mesh(&mut rng),
        ] {
            assert!(!vertices.is_empty());
            assert!(
                indices
                    .iter()
                    .all(|&index| (index as usize) < vertices.len())
            );
        }
    }

    #[test]
    fn grass_and_flower_clusters_have_three_dimensional_extent() {
        for (vertices, indices) in [
            grass_clump_mesh(),
            flower_stem_mesh(),
            flower_petal_mesh(),
            flower_leaf_mesh(),
        ] {
            let (min, max) = vertices.iter().fold(
                (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
                |(min, max), vertex| {
                    let position = Vec3::from_array(vertex.position);
                    (min.min(position), max.max(position))
                },
            );
            assert!((max - min).min_element() > 0.01);
            assert!(
                indices
                    .iter()
                    .all(|&index| (index as usize) < vertices.len())
            );
        }
    }

    #[test]
    fn authored_zones_are_reproducible_and_keep_the_route_clear() {
        for placements in [
            meadow_placements(),
            grove_placements(),
            heights_placements(),
        ] {
            let repeated = match placements.first().map(|placement| placement.position.x) {
                Some(x) if x < -15.0 => grove_placements(),
                Some(x) if x > 2.0 => heights_placements(),
                _ => meadow_placements(),
            };
            assert_eq!(placements, repeated);
            for placement in placements {
                assert!(route_points().iter().all(|point| {
                    placement.position.distance_squared(*point) >= ROUTE_CLEARANCE.powi(2)
                }));
            }
        }
    }
}
