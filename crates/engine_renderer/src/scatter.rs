//! Procedural scattering: place many instances over a world-space area
//! with placement rules, producing the [`Transform`]s an
//! `InstancedMeshRenderer` / `VegetationRenderer` draws.
//!
//! Sampling is a **jittered grid** — the area is divided into `spacing`-
//! sized cells and one candidate is placed per cell at a seeded-random
//! offset within it. O(cells), no rejection loop, fully reproducible from
//! `(config, seed)`. True Poisson-disk sampling, weighted multi-object
//! scatter, and density-from-a-mask are future work.
//!
//! A candidate survives to become a [`Transform`] only if it passes every
//! configured mask: keep-out [`ScatterConfig::exclusions`] circles, a
//! terrain [`ScatterConfig::slope_limit`], a world-Y
//! [`ScatterConfig::height_band`], and a [`ScatterConfig::density`]
//! thinning roll. The slope and height masks need a [`Heightmap`]; without
//! one they are ignored (not an error). All heightmap sampling assumes the
//! terrain has no XZ rotation or scale — the caller supplies its world-XZ
//! origin and a Y offset.

use glam::{Quat, Vec2, Vec3};

use engine_utils::{Rng, Transform};

use crate::terrain::Heightmap;

/// Upper bound on the jittered grid's cell count, so an untrusted
/// area/spacing can't request a huge allocation.
const MAX_SCATTER_CELLS: u64 = 4_000_000;

/// A world-space rectangle on the XZ (ground) plane to scatter over.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScatterArea {
    /// World-space `(x, z)` of the rectangle's minimum corner.
    pub origin: Vec2,
    /// World-space `(width, depth)` extent. Both components must be
    /// positive and finite.
    pub size: Vec2,
}

impl ScatterArea {
    /// A rectangle at `origin` (its min corner) of `size` `(width, depth)`.
    pub fn new(origin: Vec2, size: Vec2) -> Self {
        Self { origin, size }
    }
}

/// Everything [`scatter`] needs: the area, the grid spacing, and the
/// placement rules. Build with [`ScatterConfig::new`] and adjust fields.
#[derive(Debug, Clone)]
pub struct ScatterConfig {
    /// The region to fill.
    pub area: ScatterArea,
    /// Jittered-grid cell size — roughly the mean distance between
    /// instances. Must be positive and finite.
    pub spacing: f32,
    /// How far a candidate may wander from its cell centre, as a fraction
    /// of the cell (`0.0` = dead centre, `1.0` = anywhere in the cell).
    /// Clamped to `[0, 1]`.
    pub jitter: f32,
    /// Probability a candidate that passed every mask is kept. Clamped to
    /// `[0, 1]`; `0.0` yields nothing, `1.0` keeps all.
    pub density: f32,
    /// Uniform per-instance scale is drawn from this `(min, max)` range
    /// (order-independent, negatives clamped to `0`).
    pub scale_range: (f32, f32),
    /// Give each instance a random rotation about `+Y`.
    pub yaw_random: bool,
    /// How far each instance tilts from upright toward the terrain surface
    /// normal: `0.0` upright, `1.0` fully aligned. Clamped to `[0, 1]`;
    /// needs a [`Heightmap`].
    pub align_to_normal: f32,
    /// Reject candidates on terrain steeper than this angle from vertical,
    /// in radians. Needs a [`Heightmap`]; ignored without one.
    pub slope_limit: Option<f32>,
    /// Reject candidates whose surface Y falls outside this `(min, max)`
    /// band (order-independent). Needs a [`Heightmap`]; ignored without
    /// one.
    pub height_band: Option<(f32, f32)>,
    /// World-XZ keep-out circles `(centre, radius)` — a candidate inside
    /// any of them is rejected.
    pub exclusions: Vec<(Vec2, f32)>,
    /// World-XZ position of the [`Heightmap`]'s local origin, so world
    /// candidates can be sampled in the heightmap's local space.
    pub terrain_origin_xz: Vec2,
    /// Added to the sampled terrain height to get an instance's Y. With no
    /// heightmap, this *is* every instance's Y.
    pub terrain_y_offset: f32,
}

impl ScatterConfig {
    /// A config for `area` at `spacing`, with sensible defaults: full
    /// jitter and density, unit scale, random yaw, upright, no masks.
    pub fn new(area: ScatterArea, spacing: f32) -> Self {
        Self {
            area,
            spacing,
            jitter: 0.85,
            density: 1.0,
            scale_range: (1.0, 1.0),
            yaw_random: true,
            align_to_normal: 0.0,
            slope_limit: None,
            height_band: None,
            exclusions: Vec::new(),
            terrain_origin_xz: Vec2::ZERO,
            terrain_y_offset: 0.0,
        }
    }
}

/// Errors from [`scatter`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ScatterError {
    /// [`ScatterArea::size`] had a non-positive or non-finite component.
    #[error("scatter area size must be positive and finite (got {width}x{height})")]
    InvalidArea {
        /// The rejected width.
        width: f32,
        /// The rejected depth.
        height: f32,
    },
    /// [`ScatterConfig::spacing`] was non-positive or non-finite.
    #[error("scatter spacing must be positive and finite (got {spacing})")]
    InvalidSpacing {
        /// The rejected spacing.
        spacing: f32,
    },
    /// `area / spacing` produced more grid cells than the internal limit.
    #[error("scatter grid too large: {cells} cells exceeds the {max} limit")]
    TooManyCells {
        /// Requested cell count.
        cells: u64,
        /// The maximum allowed.
        max: u64,
    },
}

/// Scatters instances over `config.area` using `seed` for all randomness.
/// When `heightmap` is `Some`, instances sit on the terrain surface and
/// the slope / height-band masks apply; when `None`, every instance's Y is
/// [`ScatterConfig::terrain_y_offset`] and those masks are skipped.
///
/// The result is deterministic: the same `(config, seed)` always produces
/// the same `Vec<Transform>`, in row-major cell order.
///
/// # Errors
///
/// - [`ScatterError::InvalidArea`] if either `area.size` component is
///   non-positive or non-finite.
/// - [`ScatterError::InvalidSpacing`] if `spacing` is non-positive or
///   non-finite.
/// - [`ScatterError::TooManyCells`] if the jittered grid would exceed the
///   internal cell-count limit.
pub fn scatter(
    config: &ScatterConfig,
    seed: u64,
    heightmap: Option<&Heightmap>,
) -> Result<Vec<Transform>, ScatterError> {
    let origin = config.area.origin;
    let size = config.area.size;
    if size.x <= 0.0 || size.y <= 0.0 || !size.is_finite() {
        return Err(ScatterError::InvalidArea {
            width: size.x,
            height: size.y,
        });
    }
    if config.spacing <= 0.0 || !config.spacing.is_finite() {
        return Err(ScatterError::InvalidSpacing {
            spacing: config.spacing,
        });
    }

    let columns = (size.x / config.spacing).ceil().max(1.0) as u64;
    let rows = (size.y / config.spacing).ceil().max(1.0) as u64;
    let cells = columns.saturating_mul(rows);
    if cells > MAX_SCATTER_CELLS {
        return Err(ScatterError::TooManyCells {
            cells,
            max: MAX_SCATTER_CELLS,
        });
    }

    let jitter = config.jitter.clamp(0.0, 1.0);
    let density = config.density.clamp(0.0, 1.0);
    let align = config.align_to_normal.clamp(0.0, 1.0);
    let scale_low = config.scale_range.0.min(config.scale_range.1).max(0.0);
    let scale_high = config.scale_range.0.max(config.scale_range.1).max(0.0);

    let mut rng = Rng::new(seed);
    let mut out = Vec::new();

    for row in 0..rows {
        for column in 0..columns {
            // Every random for this cell is drawn up front in a fixed
            // order, so a rejected candidate still advances the stream the
            // same amount — that's what keeps the output reproducible.
            let offset_x = rng.range(-0.5, 0.5) * jitter;
            let offset_z = rng.range(-0.5, 0.5) * jitter;
            let yaw = rng.range(0.0, std::f32::consts::TAU);
            let scale = rng.range(scale_low, scale_high.max(scale_low));
            let keep_roll = rng.next_f32();

            let world_x = (origin.x + (column as f32 + 0.5 + offset_x) * config.spacing)
                .clamp(origin.x, origin.x + size.x);
            let world_z = (origin.y + (row as f32 + 0.5 + offset_z) * config.spacing)
                .clamp(origin.y, origin.y + size.y);
            let world_xz = Vec2::new(world_x, world_z);

            if config
                .exclusions
                .iter()
                .any(|(centre, radius)| world_xz.distance_squared(*centre) <= radius * radius)
            {
                continue;
            }

            let local_x = world_x - config.terrain_origin_xz.x;
            let local_z = world_z - config.terrain_origin_xz.y;

            let (surface_y, up) = match heightmap {
                Some(heightmap) => {
                    if let Some(limit) = config.slope_limit
                        && heightmap.slope_at(local_x, local_z) > limit
                    {
                        continue;
                    }
                    let surface_y = config.terrain_y_offset + heightmap.height_at(local_x, local_z);
                    if let Some((band_a, band_b)) = config.height_band
                        && (surface_y < band_a.min(band_b) || surface_y > band_a.max(band_b))
                    {
                        continue;
                    }
                    (surface_y, heightmap.normal_at(local_x, local_z))
                }
                None => (config.terrain_y_offset, Vec3::Y),
            };

            if keep_roll >= density {
                continue;
            }

            let mut rotation = if config.yaw_random {
                Quat::from_rotation_y(yaw)
            } else {
                Quat::IDENTITY
            };
            if align > 0.0 && up != Vec3::Y {
                let tilt = Quat::IDENTITY.slerp(Quat::from_rotation_arc(Vec3::Y, up), align);
                rotation = tilt * rotation;
            }

            out.push(
                Transform::from_translation(Vec3::new(world_x, surface_y, world_z))
                    .with_rotation(rotation)
                    .with_scale(Vec3::splat(scale)),
            );
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(w: f32, d: f32) -> ScatterArea {
        ScatterArea::new(Vec2::ZERO, Vec2::new(w, d))
    }

    #[test]
    fn rejects_bad_area() {
        assert!(matches!(
            scatter(&ScatterConfig::new(area(0.0, 4.0), 1.0), 0, None),
            Err(ScatterError::InvalidArea { .. })
        ));
        assert!(scatter(&ScatterConfig::new(area(4.0, -1.0), 1.0), 0, None).is_err());
        assert!(scatter(&ScatterConfig::new(area(f32::INFINITY, 4.0), 1.0), 0, None).is_err());
    }

    #[test]
    fn rejects_bad_spacing() {
        assert!(matches!(
            scatter(&ScatterConfig::new(area(4.0, 4.0), 0.0), 0, None),
            Err(ScatterError::InvalidSpacing { .. })
        ));
        assert!(scatter(&ScatterConfig::new(area(4.0, 4.0), -1.0), 0, None).is_err());
        assert!(scatter(&ScatterConfig::new(area(4.0, 4.0), f32::NAN), 0, None).is_err());
    }

    #[test]
    fn rejects_oversized_grids() {
        assert!(matches!(
            scatter(
                &ScatterConfig::new(area(100_000.0, 100_000.0), 0.01),
                0,
                None
            ),
            Err(ScatterError::TooManyCells { .. })
        ));
    }

    #[test]
    fn is_deterministic_for_a_config_and_seed() {
        let config = ScatterConfig::new(area(20.0, 20.0), 1.5);
        let a = scatter(&config, 42, None).unwrap();
        let b = scatter(&config, 42, None).unwrap();
        let c = scatter(&config, 43, None).unwrap();
        assert_eq!(a.len(), b.len());
        assert!(
            a.iter()
                .zip(&b)
                .all(|(x, y)| x.translation == y.translation)
        );
        assert!(
            a.iter()
                .zip(&c)
                .any(|(x, y)| x.translation != y.translation)
        );
    }

    #[test]
    fn density_zero_scatters_nothing_and_one_fills_the_lattice() {
        let mut config = ScatterConfig::new(area(4.0, 4.0), 1.0);
        config.jitter = 0.0;
        config.yaw_random = false;

        config.density = 0.0;
        assert!(scatter(&config, 1, None).unwrap().is_empty());

        config.density = 1.0;
        let full = scatter(&config, 1, None).unwrap();
        assert_eq!(full.len(), 16);
    }

    #[test]
    fn zero_jitter_places_candidates_on_the_cell_centre_lattice() {
        let mut config = ScatterConfig::new(area(3.0, 3.0), 1.0);
        config.jitter = 0.0;
        let placed = scatter(&config, 7, None).unwrap();
        for transform in &placed {
            let x = transform.translation.x;
            let z = transform.translation.z;
            assert!((x - x.floor() - 0.5).abs() < 1e-5, "x {x} off lattice");
            assert!((z - z.floor() - 0.5).abs() < 1e-5, "z {z} off lattice");
        }
    }

    #[test]
    fn no_heightmap_puts_every_instance_at_the_y_offset() {
        let mut config = ScatterConfig::new(area(6.0, 6.0), 1.0);
        config.terrain_y_offset = -2.5;
        // A slope limit is set but ignored without a heightmap.
        config.slope_limit = Some(0.01);
        let placed = scatter(&config, 3, None).unwrap();
        assert!(!placed.is_empty());
        assert!(placed.iter().all(|t| (t.translation.y - -2.5).abs() < 1e-6));
    }

    #[test]
    fn height_band_filters_against_the_terrain_surface() {
        let flat = Heightmap::from_heights(4, 4.0, vec![5.0; 16]).unwrap();
        let mut config = ScatterConfig::new(area(4.0, 4.0), 1.0);
        config.terrain_origin_xz = Vec2::new(-2.0, -2.0);

        config.height_band = Some((0.0, 1.0));
        assert!(scatter(&config, 1, Some(&flat)).unwrap().is_empty());

        config.height_band = Some((4.0, 6.0));
        assert!(!scatter(&config, 1, Some(&flat)).unwrap().is_empty());
    }

    #[test]
    fn slope_limit_keeps_flat_ground_and_rejects_a_ramp() {
        let flat = Heightmap::new(5, 4.0).unwrap();
        let mut config = ScatterConfig::new(area(4.0, 4.0), 1.0);
        config.terrain_origin_xz = Vec2::new(-2.0, -2.0);
        config.slope_limit = Some(0.2);
        assert!(!scatter(&config, 1, Some(&flat)).unwrap().is_empty());

        // height = x -> a 45-degree ramp everywhere; a tight slope limit
        // rejects the lot.
        let ramp = Heightmap::from_fn(9, 8.0, |_, col| col as f32).unwrap();
        let mut ramp_config = ScatterConfig::new(area(8.0, 8.0), 1.0);
        ramp_config.terrain_origin_xz = Vec2::new(-4.0, -4.0);
        ramp_config.slope_limit = Some(0.1);
        assert!(scatter(&ramp_config, 1, Some(&ramp)).unwrap().is_empty());
    }

    #[test]
    fn exclusion_circle_covering_the_area_scatters_nothing() {
        let mut config = ScatterConfig::new(area(6.0, 6.0), 1.0);
        config.exclusions = vec![(Vec2::new(3.0, 3.0), 20.0)];
        assert!(scatter(&config, 5, None).unwrap().is_empty());
    }

    #[test]
    fn scale_stays_within_the_configured_range() {
        let mut config = ScatterConfig::new(area(10.0, 10.0), 1.0);
        config.scale_range = (0.5, 2.0);
        for transform in scatter(&config, 9, None).unwrap() {
            let s = transform.scale.x;
            assert!((0.5..=2.0).contains(&s), "scale {s} out of range");
            assert_eq!(transform.scale, Vec3::splat(s));
        }

        config.scale_range = (1.3, 1.3);
        for transform in scatter(&config, 9, None).unwrap() {
            assert!((transform.scale.x - 1.3).abs() < 1e-6);
        }
    }

    #[test]
    fn upright_when_align_to_normal_is_zero() {
        let mut config = ScatterConfig::new(area(6.0, 6.0), 1.0);
        config.align_to_normal = 0.0;
        for transform in scatter(&config, 4, None).unwrap() {
            let up = transform.rotation * Vec3::Y;
            assert!((up - Vec3::Y).length() < 1e-5, "not upright: {up:?}");
        }
    }

    #[test]
    fn align_to_normal_tilts_instances_on_a_ramp() {
        let ramp = Heightmap::from_fn(9, 8.0, |_, col| col as f32 * 0.3).unwrap();
        let mut config = ScatterConfig::new(area(6.0, 6.0), 1.0);
        config.terrain_origin_xz = Vec2::new(-3.0, -3.0);
        config.yaw_random = false;
        config.align_to_normal = 1.0;
        let placed = scatter(&config, 2, Some(&ramp)).unwrap();
        assert!(placed.iter().any(|t| {
            let up = t.rotation * Vec3::Y;
            (up - Vec3::Y).length() > 1e-3
        }));
    }

    #[test]
    fn scatter_error_displays() {
        let err = ScatterError::InvalidSpacing { spacing: -1.0 };
        assert!(err.to_string().contains("spacing"));
    }
}
