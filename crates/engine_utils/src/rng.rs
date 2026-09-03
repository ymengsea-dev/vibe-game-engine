//! [`Rng`] — a deterministic SplitMix64 pseudo-random generator.
//!
//! Small, fast, good-enough distribution, and fully reproducible from a
//! seed, so anything built on it (particle jitter, procedural scattering,
//! test fixtures) replays identically. Lives here in `engine_utils`
//! because several crates above it — `engine_ecs`, `engine_renderer` —
//! need the same generator and there is no `rand` dependency in the
//! workspace.

use glam::Vec3;

/// A deterministic SplitMix64 pseudo-random generator.
#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
}

impl Rng {
    /// A generator seeded with `seed`. Any `seed` is valid, including `0`.
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// The next raw 64-bit value (SplitMix64).
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A uniform `f32` in `[0.0, 1.0)` (24 bits of mantissa precision).
    pub fn next_f32(&mut self) -> f32 {
        // Top 24 bits -> [0, 2^24) -> [0, 1).
        (self.next_u64() >> 40) as f32 / (1u32 << 24) as f32
    }

    /// A uniform `f32` in `[min, max)` (returns `min` if `min >= max`).
    pub fn range(&mut self, min: f32, max: f32) -> f32 {
        if min >= max {
            return min;
        }
        min + (max - min) * self.next_f32()
    }

    /// A uniform `f32` in `[-1.0, 1.0)`.
    pub fn signed_unit(&mut self) -> f32 {
        self.range(-1.0, 1.0)
    }

    /// A unit vector within `spread` radians of `axis` (a cone around
    /// `axis`). `spread` is clamped to `[0, PI]`; `0.0` returns `axis`
    /// exactly. A degenerate `axis` falls back to `+Y`.
    pub fn cone_direction(&mut self, axis: Vec3, spread: f32) -> Vec3 {
        let axis = {
            let normalized = axis.normalize_or_zero();
            if normalized == Vec3::ZERO {
                Vec3::Y
            } else {
                normalized
            }
        };
        let spread = spread.clamp(0.0, core::f32::consts::PI);
        // Uniform-ish sampling on a spherical cap: cos(theta) in
        // [cos(spread), 1], phi in [0, 2*PI).
        let cos_theta = self.range(spread.cos(), 1.0);
        let phi = self.range(0.0, core::f32::consts::TAU);
        let sin_theta = (1.0 - cos_theta * cos_theta).max(0.0).sqrt();
        let local = Vec3::new(sin_theta * phi.cos(), sin_theta * phi.sin(), cos_theta);
        let (tangent, bitangent) = orthonormal_basis(axis);
        (tangent * local.x + bitangent * local.y + axis * local.z).normalize_or_zero()
    }
}

/// Two unit vectors completing an orthonormal basis with `normal` as the
/// third axis. `normal` is assumed already normalized.
fn orthonormal_basis(normal: Vec3) -> (Vec3, Vec3) {
    let seed = if normal.z.abs() < 0.999 {
        Vec3::Z
    } else {
        Vec3::X
    };
    let tangent = seed.cross(normal).normalize_or_zero();
    let bitangent = normal.cross(tangent);
    (tangent, bitangent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_deterministic_for_a_seed() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..64 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn distinct_seeds_diverge() {
        let mut a = Rng::new(1);
        let mut b = Rng::new(2);
        assert_ne!(a.next_u64(), b.next_u64());
    }

    #[test]
    fn next_f32_stays_in_unit_range() {
        let mut rng = Rng::new(7);
        for _ in 0..10_000 {
            let x = rng.next_f32();
            assert!((0.0..1.0).contains(&x), "{x} out of [0, 1)");
        }
    }

    #[test]
    fn range_respects_bounds_and_handles_inverted() {
        let mut rng = Rng::new(9);
        for _ in 0..1000 {
            let x = rng.range(-3.0, 5.0);
            assert!((-3.0..5.0).contains(&x));
        }
        assert_eq!(rng.range(2.0, 2.0), 2.0);
        assert_eq!(rng.range(5.0, 1.0), 5.0);
    }

    #[test]
    fn cone_direction_zero_spread_returns_the_axis() {
        let mut rng = Rng::new(3);
        let dir = rng.cone_direction(Vec3::new(0.0, 0.0, 1.0), 0.0);
        assert!((dir - Vec3::Z).length() < 1e-5);
    }

    #[test]
    fn cone_direction_stays_within_spread_of_the_axis() {
        let mut rng = Rng::new(11);
        let axis = Vec3::new(1.0, 2.0, -0.5).normalize();
        let spread = 0.4_f32;
        for _ in 0..2000 {
            let dir = rng.cone_direction(axis, spread);
            assert!((dir.length() - 1.0).abs() < 1e-4);
            let angle = dir.dot(axis).clamp(-1.0, 1.0).acos();
            assert!(angle <= spread + 1e-3, "angle {angle} > spread {spread}");
        }
    }
}
