//! The object brush: painting copies of a model or prefab onto a terrain,
//! each a normal scene object with its own turn and size.
//!
//! Places come from the same hashed, jittered grid as foliage
//! ([`crate::foliage::plants`]), so painting over the same ground twice adds
//! nothing and painting erased ground again brings back the same objects.
//! Each cell's dice also pick the object's turn and size.

use std::collections::HashSet;

use engine_renderer::TerrainSurface;
use glam::{IVec2, Quat, Vec2, Vec3};

use crate::foliage::{Area, PlaceError, Placement};

/// The most objects one painted group holds: each is a whole scene object,
/// far heavier than a foliage instance.
pub const MAX_PAINTED: usize = 2_000;

/// How painted objects vary.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PropStyle {
    /// Largest random turn about the up axis, in degrees (360 = any way).
    pub turn_degrees: f32,
    /// Smallest size, as a multiple of the model's own.
    pub size_min: f32,
    /// Largest size.
    pub size_max: f32,
    /// Tilt each object to the slope it stands on.
    pub lean: bool,
}

impl Default for PropStyle {
    fn default() -> Self {
        Self {
            turn_degrees: 360.0,
            size_min: 0.8,
            size_max: 1.2,
            lean: false,
        }
    }
}

/// One object to place, terrain-local.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Prop {
    /// The grid cell it belongs to.
    pub cell: IVec2,
    /// Its spot on the ground.
    pub position: Vec3,
    /// Its turn (and lean).
    pub rotation: Quat,
    /// Its size, as a multiple of the model's own.
    pub scale: f32,
}

/// Every object the brush circle at `center` holds on `surface`.
///
/// # Errors
///
/// [`PlaceError::TooDense`] when the circle spans too many grid cells.
pub fn props(
    surface: &TerrainSurface,
    center: Vec2,
    radius: f32,
    placement: Placement,
    style: PropStyle,
) -> Result<Vec<Prop>, PlaceError> {
    let spacing = placement.spacing.max(0.01);
    let turn = style.turn_degrees.clamp(0.0, 360.0).to_radians();
    let (low, high) = if style.size_min <= style.size_max {
        (style.size_min, style.size_max)
    } else {
        (style.size_max, style.size_min)
    };
    let low = low.max(0.01);
    let high = high.max(low);
    let plants = crate::foliage::plants(surface, Area::Circle { center, radius }, placement)?;
    Ok(plants
        .into_iter()
        .map(|plant| {
            let cell =
                crate::foliage::cell_of(Vec2::new(plant.position.x, plant.position.z), spacing);
            let yaw = plant.yaw / std::f32::consts::TAU * turn;
            let spin = Quat::from_rotation_y(yaw);
            let rotation = if style.lean {
                surface
                    .normal_at(plant.position.x, plant.position.z)
                    .map_or(spin, |normal| {
                        Quat::from_rotation_arc(Vec3::Y, normal) * spin
                    })
            } else {
                spin
            };
            let size = low + (high - low) * crate::foliage::dice_in_cell(cell, placement.seed, 1);
            Prop {
                cell,
                position: plant.position,
                rotation,
                scale: size,
            }
        })
        .collect())
}

/// The grid cells `positions` (terrain-local XZ) already fill.
pub fn occupied(positions: impl IntoIterator<Item = Vec2>, spacing: f32) -> HashSet<IVec2> {
    let spacing = spacing.max(0.01);
    positions
        .into_iter()
        .map(|position| crate::foliage::cell_of(position, spacing))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_renderer::TerrainTile;

    fn flat() -> TerrainSurface {
        let mut surface = TerrainSurface::new(33, 64.0).unwrap();
        surface
            .insert_tile((0, 0), TerrainTile::flat(33, 64.0).unwrap())
            .unwrap();
        surface
    }

    fn placement() -> Placement {
        Placement {
            spacing: 2.0,
            density: 1.0,
            seed: 7,
        }
    }

    #[test]
    fn the_same_ground_gets_the_same_objects() {
        let surface = flat();
        let a = props(&surface, Vec2::ZERO, 6.0, placement(), PropStyle::default()).unwrap();
        let b = props(&surface, Vec2::ZERO, 6.0, placement(), PropStyle::default()).unwrap();
        assert!(!a.is_empty());
        assert_eq!(a, b);
    }

    #[test]
    fn sizes_and_turns_stay_in_their_ranges() {
        let surface = flat();
        let style = PropStyle {
            turn_degrees: 90.0,
            size_min: 0.5,
            size_max: 2.0,
            lean: false,
        };
        let placed = props(&surface, Vec2::ZERO, 10.0, placement(), style).unwrap();
        assert!(placed.iter().all(|p| (0.5..=2.0).contains(&p.scale)));
        assert!(placed.iter().any(|p| p.scale > 1.2) && placed.iter().any(|p| p.scale < 1.0));
        for prop in &placed {
            let (axis, angle) = prop.rotation.to_axis_angle();
            assert!(angle <= 90f32.to_radians() + 1e-4, "{angle}");
            assert!(angle < 1e-4 || axis.y > 0.99, "turns only about up");
        }
        let fixed = PropStyle {
            turn_degrees: 0.0,
            size_min: 1.0,
            size_max: 1.0,
            lean: false,
        };
        let placed = props(&surface, Vec2::ZERO, 10.0, placement(), fixed).unwrap();
        assert!(
            placed
                .iter()
                .all(|p| p.scale == 1.0 && p.rotation.angle_between(Quat::IDENTITY) < 1e-4)
        );
    }

    #[test]
    fn occupied_cells_match_placed_cells() {
        let surface = flat();
        let placed = props(&surface, Vec2::ZERO, 6.0, placement(), PropStyle::default()).unwrap();
        let cells = occupied(
            placed.iter().map(|p| Vec2::new(p.position.x, p.position.z)),
            2.0,
        );
        assert!(placed.iter().all(|p| cells.contains(&p.cell)));
    }
}
