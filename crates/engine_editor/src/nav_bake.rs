//! Baking the scene's walkable area: a nav grid built from the ground and
//! the solid objects standing on it.
//!
//! The grid has one layer. For each cell Studio finds the lowest ground
//! under it (terrain, or the top of a fixed box collider) and marks the cell
//! blocked when:
//! - there is ground somewhere in the scene but none under this cell;
//! - the ground is steeper than the bake's maximum slope;
//! - a fixed, solid collider fills the space a character needs above the
//!   ground (from the step height up to the agent's height), grown by the
//!   agent's radius so characters keep clear of walls.
//!
//! Moving bodies (dynamic and kinematic), triggers, disabled objects and
//! objects with AI never block: they are not part of the level. Without
//! any ground, the grid is flat at `0` and spans the solid objects.
//!
//! Colliders count by the box around them, so a round or mesh collider
//! blocks a little more than its shape.

use engine_ecs::prelude::{Entity, World};
use engine_renderer::{Aabb, Heightmap};
use engine_scene::{BodyKind, ColliderShape, NavBakeSettings, NavGridData, SceneCollider};
use glam::{Mat4, Vec2, Vec3};

/// The most cells one bake may make. A grid this size is a few megabytes in
/// the scene file.
pub const MAX_BAKE_CELLS: u64 = 250_000;
/// How far past the solid objects a grid with no ground reaches.
const NO_GROUND_MARGIN: f32 = 5.0;

/// Something characters can stand on.
#[derive(Debug, Clone)]
pub enum Ground {
    /// A sculpted terrain and where it is in the world.
    Terrain {
        /// Its heights, in its own space.
        heightmap: Heightmap,
        /// Its world matrix.
        to_world: Mat4,
    },
    /// The flat top of a box.
    Flat {
        /// Lowest world `(x, z)` corner.
        min: Vec2,
        /// Highest world `(x, z)` corner.
        max: Vec2,
        /// World height of the top.
        top: f32,
    },
}

impl Ground {
    /// The world `(x, z)` rectangle it covers.
    fn extent(&self) -> (Vec2, Vec2) {
        match self {
            Ground::Flat { min, max, .. } => (*min, *max),
            Ground::Terrain {
                heightmap,
                to_world,
            } => {
                let half = heightmap.size() * 0.5;
                let corners = [(-half, -half), (half, -half), (-half, half), (half, half)]
                    .map(|(x, z)| to_world.transform_point3(Vec3::new(x, 0.0, z)));
                let xz = corners.map(|point| Vec2::new(point.x, point.z));
                (
                    xz.iter().fold(Vec2::splat(f32::MAX), |acc, p| acc.min(*p)),
                    xz.iter().fold(Vec2::splat(f32::MIN), |acc, p| acc.max(*p)),
                )
            }
        }
    }

    /// The ground height at world `(x, z)`, if this ground is there.
    fn height_at(&self, at: Vec2) -> Option<f32> {
        match self {
            Ground::Flat { min, max, top } => {
                (at.cmpge(*min).all() && at.cmple(*max).all()).then_some(*top)
            }
            Ground::Terrain {
                heightmap,
                to_world,
            } => {
                let local =
                    to_world
                        .inverse()
                        .transform_point3(Vec3::new(at.x, to_world.w_axis.y, at.y));
                let half = heightmap.size() * 0.5;
                if local.x.abs() > half || local.z.abs() > half {
                    return None;
                }
                let height = heightmap.height_at(local.x, local.z);
                Some(
                    to_world
                        .transform_point3(Vec3::new(local.x, height, local.z))
                        .y,
                )
            }
        }
    }

    /// The slope at world `(x, z)`, in degrees, measured across `step`.
    fn slope_at(&self, at: Vec2, step: f32) -> f32 {
        if matches!(self, Ground::Flat { .. }) {
            return 0.0;
        }
        let height = |offset: Vec2| self.height_at(at + offset);
        let (Some(x0), Some(x1), Some(z0), Some(z1)) = (
            height(Vec2::new(-step, 0.0)),
            height(Vec2::new(step, 0.0)),
            height(Vec2::new(0.0, -step)),
            height(Vec2::new(0.0, step)),
        ) else {
            return 0.0;
        };
        let gradient = Vec2::new(x1 - x0, z1 - z0) / (2.0 * step);
        gradient.length().atan().to_degrees()
    }
}

/// A solid box characters cannot walk through, in world space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Obstacle {
    /// Lowest corner.
    pub min: Vec3,
    /// Highest corner.
    pub max: Vec3,
}

/// What a bake reads from the scene.
#[derive(Debug, Clone, Default)]
pub struct BakeInputs {
    /// Everything characters can stand on.
    pub grounds: Vec<Ground>,
    /// Everything that blocks them.
    pub obstacles: Vec<Obstacle>,
}

/// Builds the grid from `inputs`.
///
/// # Errors
///
/// A message for the user when the settings are out of range or the grid
/// would have more than [`MAX_BAKE_CELLS`] cells.
pub fn bake(inputs: &BakeInputs, settings: NavBakeSettings) -> Result<NavGridData, String> {
    let cell = settings.cell_size;
    if !cell.is_finite() || cell < 0.05 {
        return Err("Cell size must be at least 0.05".to_owned());
    }
    let (min, max) = area(inputs);
    let width = ((max.x - min.x) / cell).ceil().max(1.0);
    let height = ((max.y - min.y) / cell).ceil().max(1.0);
    if width * height > MAX_BAKE_CELLS as f32 {
        return Err(format!(
            "The walkable area would have {} cells (most is {MAX_BAKE_CELLS}). Raise Cell size.",
            (width * height) as u64
        ));
    }
    let (width, height) = (width as u32, height as u32);
    let mut grid = NavGridData::new(width, height, cell, [min.x, min.y]);
    let mut heights = vec![0.0; grid.cell_count()];
    let has_ground = !inputs.grounds.is_empty();
    let half = Vec2::splat(cell * 0.5);
    let grow = Vec2::splat(settings.agent_radius);

    for row in 0..height {
        for column in 0..width {
            let index = (row * width + column) as usize;
            let centre = min + Vec2::new(column as f32 + 0.5, row as f32 + 0.5) * cell;
            let ground = inputs
                .grounds
                .iter()
                .filter_map(|ground| ground.height_at(centre).map(|h| (h, ground)))
                .min_by(|a, b| a.0.total_cmp(&b.0));
            let floor = match ground {
                Some((floor, ground)) => {
                    heights[index] = floor;
                    if ground.slope_at(centre, cell * 0.5) > settings.max_slope_degrees {
                        grid.blocked[index] = true;
                        continue;
                    }
                    floor
                }
                None if has_ground => {
                    grid.blocked[index] = true;
                    continue;
                }
                None => 0.0,
            };
            let band = (floor + settings.step_height, floor + settings.agent_height);
            let (cell_min, cell_max) = (centre - half, centre + half);
            grid.blocked[index] = inputs.obstacles.iter().any(|obstacle| {
                let low = Vec2::new(obstacle.min.x, obstacle.min.z) - grow;
                let high = Vec2::new(obstacle.max.x, obstacle.max.z) + grow;
                cell_min.cmplt(high).all()
                    && cell_max.cmpgt(low).all()
                    && obstacle.min.y < band.1
                    && obstacle.max.y > band.0
            });
        }
    }
    if heights.iter().any(|height| *height != 0.0) {
        grid.heights = heights;
    }
    grid.bake = Some(settings);
    Ok(grid)
}

/// The world `(x, z)` rectangle the grid covers: the ground, or without
/// any, the solid objects plus a margin.
fn area(inputs: &BakeInputs) -> (Vec2, Vec2) {
    let mut min = Vec2::splat(f32::MAX);
    let mut max = Vec2::splat(f32::MIN);
    if inputs.grounds.is_empty() {
        for obstacle in &inputs.obstacles {
            min = min.min(Vec2::new(obstacle.min.x, obstacle.min.z));
            max = max.max(Vec2::new(obstacle.max.x, obstacle.max.z));
        }
        if min.x > max.x {
            return (Vec2::splat(-10.0), Vec2::splat(10.0));
        }
        return (
            min - Vec2::splat(NO_GROUND_MARGIN),
            max + Vec2::splat(NO_GROUND_MARGIN),
        );
    }
    for ground in &inputs.grounds {
        let (low, high) = ground.extent();
        min = min.min(low);
        max = max.max(high);
    }
    (min, max)
}

/// The bake settings as menu fields. Returns whether any changed.
pub fn show_controls(ui: &mut egui::Ui, settings: &mut NavBakeSettings) -> bool {
    let mut changed = false;
    let mut field = |ui: &mut egui::Ui, value: &mut f32, label: &str, range, hint: &str| {
        changed |= ui
            .add(
                egui::DragValue::new(value)
                    .prefix(format!("{label} "))
                    .range(range)
                    .speed(0.05),
            )
            .on_hover_text(hint)
            .changed();
    };
    field(
        ui,
        &mut settings.cell_size,
        "Cell size",
        0.05..=100.0,
        "Smaller cells find narrower gaps but make a bigger grid",
    );
    field(
        ui,
        &mut settings.agent_radius,
        "Character radius",
        0.0..=100.0,
        "How far characters keep from walls",
    );
    field(
        ui,
        &mut settings.agent_height,
        "Character height",
        0.01..=100.0,
        "Anything lower overhead blocks the way",
    );
    field(
        ui,
        &mut settings.step_height,
        "Step height",
        0.0..=100.0,
        "Ledges up to this high can be stepped over",
    );
    field(
        ui,
        &mut settings.max_slope_degrees,
        "Steepest slope °",
        0.0..=89.0,
        "Ground steeper than this is not walkable",
    );
    changed
}

/// A tiled terrain's tiles, each with the matrix from the tile's own space
/// to the terrain object's.
pub type TileSource<'a> = dyn Fn(&World, Entity) -> Vec<(Heightmap, Mat4)> + 'a;

/// Reads the ground and obstacles from the editor's world.
///
/// `mesh_bounds` gives an entity's local mesh bounds, for mesh colliders.
/// `tiles` gives a terrain's tiles, each with the matrix from the tile's own
/// space to the terrain object's.
pub fn collect(
    world: &mut World,
    mesh_bounds: &dyn Fn(&World, Entity) -> Option<Aabb>,
    tiles: &TileSource<'_>,
) -> BakeInputs {
    let mut inputs = BakeInputs::default();
    let colliders: Vec<(Entity, engine_scene::ColliderData)> = world
        .query::<(Entity, &SceneCollider)>()
        .iter(world)
        .map(|(entity, collider)| (entity, collider.0.clone()))
        .collect();
    for (entity, collider) in colliders {
        if collider.trigger
            || collider.body != BodyKind::Fixed
            || world
                .get::<engine_ecs::components::Disabled>(entity)
                .is_some()
            || world.get::<engine_scene::SceneAi>(entity).is_some()
        {
            continue;
        }
        let to_world = crate::hierarchy::world_transform(world, entity).to_matrix();
        if let ColliderShape::Terrain = collider.shape {
            for (heightmap, tile) in tiles(world, entity) {
                inputs.grounds.push(Ground::Terrain {
                    heightmap,
                    to_world: to_world * tile,
                });
            }
            continue;
        }
        let local = match &collider.shape {
            ColliderShape::Ball { radius } => Some(Vec3::splat(*radius)),
            ColliderShape::Cuboid { half_extents } => Some(Vec3::from_array(*half_extents)),
            ColliderShape::Cylinder {
                half_height,
                radius,
            } => Some(Vec3::new(*radius, *half_height, *radius)),
            ColliderShape::Capsule {
                half_height,
                radius,
            } => Some(Vec3::new(*radius, half_height + radius, *radius)),
            ColliderShape::TriMesh { .. } | ColliderShape::Terrain => None,
        };
        let bounds = match local {
            Some(half) => Aabb {
                min: -half,
                max: half,
            },
            None => match mesh_bounds(world, entity) {
                Some(bounds) => bounds,
                None => continue,
            },
        };
        let obstacle = world_box(to_world, bounds);
        if matches!(collider.shape, ColliderShape::Cuboid { .. }) && is_upright(to_world) {
            inputs.grounds.push(Ground::Flat {
                min: Vec2::new(obstacle.min.x, obstacle.min.z),
                max: Vec2::new(obstacle.max.x, obstacle.max.z),
                top: obstacle.max.y,
            });
        }
        inputs.obstacles.push(obstacle);
    }
    inputs
}

/// Whether a box with this world matrix still has a flat top.
fn is_upright(to_world: Mat4) -> bool {
    to_world
        .transform_vector3(Vec3::Y)
        .try_normalize()
        .is_some_and(|up| up.y > 0.999)
}

/// The world box around local `bounds`.
fn world_box(to_world: Mat4, bounds: Aabb) -> Obstacle {
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    for corner in 0..8 {
        let local = Vec3::new(
            if corner & 1 == 0 {
                bounds.min.x
            } else {
                bounds.max.x
            },
            if corner & 2 == 0 {
                bounds.min.y
            } else {
                bounds.max.y
            },
            if corner & 4 == 0 {
                bounds.min.z
            } else {
                bounds.max.z
            },
        );
        let point = to_world.transform_point3(local);
        min = min.min(point);
        max = max.max(point);
    }
    Obstacle { min, max }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn floor(size: f32) -> Ground {
        Ground::Flat {
            min: Vec2::splat(-size * 0.5),
            max: Vec2::splat(size * 0.5),
            top: 0.0,
        }
    }

    fn settings(cell: f32) -> NavBakeSettings {
        NavBakeSettings {
            cell_size: cell,
            agent_radius: 0.0,
            ..NavBakeSettings::default()
        }
    }

    fn blocked_at(grid: &NavGridData, x: f32, z: f32) -> bool {
        let column = ((x - grid.origin[0]) / grid.cell_size).floor() as u32;
        let row = ((z - grid.origin[1]) / grid.cell_size).floor() as u32;
        grid.blocked[(row * grid.width + column) as usize]
    }

    #[test]
    fn a_box_on_the_floor_blocks_its_footprint_and_the_radius_around_it() {
        let crate_box = Obstacle {
            min: Vec3::new(1.0, 0.0, 1.0),
            max: Vec3::new(2.0, 1.0, 2.0),
        };
        let inputs = BakeInputs {
            grounds: vec![floor(10.0)],
            obstacles: vec![crate_box],
        };
        let grid = bake(&inputs, settings(0.5)).expect("bakes");
        assert_eq!((grid.width, grid.height), (20, 20));
        assert!(blocked_at(&grid, 1.5, 1.5));
        assert!(!blocked_at(&grid, 3.0, 3.0));
        assert!(!blocked_at(&grid, 2.25, 1.5));
        assert!(
            grid.heights.is_empty(),
            "a flat floor at 0 stores no heights"
        );

        let wide = NavBakeSettings {
            agent_radius: 0.5,
            ..settings(0.5)
        };
        let grid = bake(&inputs, wide).expect("bakes");
        assert!(blocked_at(&grid, 2.25, 1.5));
        assert_eq!(grid.bake, Some(wide));
    }

    #[test]
    fn low_steps_and_high_overhangs_do_not_block() {
        let step = Obstacle {
            min: Vec3::new(1.0, 0.0, 1.0),
            max: Vec3::new(2.0, 0.2, 2.0),
        };
        let sign = Obstacle {
            min: Vec3::new(3.0, 2.5, 3.0),
            max: Vec3::new(4.0, 3.0, 4.0),
        };
        let inputs = BakeInputs {
            grounds: vec![floor(10.0)],
            obstacles: vec![step, sign],
        };
        let grid = bake(&inputs, settings(0.5)).expect("bakes");
        assert!(!blocked_at(&grid, 1.5, 1.5));
        assert!(!blocked_at(&grid, 3.5, 3.5));
    }

    #[test]
    fn steep_terrain_is_blocked_and_heights_follow_it() {
        // A ramp along X: height = 2 * x, about 63 degrees.
        let heightmap =
            Heightmap::from_fn(9, 8.0, |_, col| (col as f32 - 4.0) * 2.0).expect("heightmap");
        let gentle =
            Heightmap::from_fn(9, 8.0, |_, col| (col as f32 - 4.0) * 0.1).expect("heightmap");
        let steep = BakeInputs {
            grounds: vec![Ground::Terrain {
                heightmap,
                to_world: Mat4::IDENTITY,
            }],
            obstacles: Vec::new(),
        };
        let grid = bake(&steep, settings(1.0)).expect("bakes");
        assert!(grid.blocked.iter().all(|blocked| *blocked));

        let ramp = BakeInputs {
            grounds: vec![Ground::Terrain {
                heightmap: gentle,
                to_world: Mat4::from_translation(Vec3::new(0.0, 1.0, 0.0)),
            }],
            obstacles: Vec::new(),
        };
        let grid = bake(&ramp, settings(1.0)).expect("bakes");
        assert!(grid.blocked.iter().all(|blocked| !*blocked));
        assert_eq!(grid.heights.len(), grid.cell_count());
        let first = grid.heights[0];
        let last = grid.heights[grid.width as usize - 1];
        assert!(last > first, "heights rise along the ramp");
        assert!((grid.heights[0] - (1.0 + -3.5 * 0.1)).abs() < 1e-3);
    }

    #[test]
    fn cells_off_the_ground_are_blocked() {
        let inputs = BakeInputs {
            grounds: vec![
                floor(4.0),
                Ground::Flat {
                    min: Vec2::new(6.0, -2.0),
                    max: Vec2::new(10.0, 2.0),
                    top: 0.0,
                },
            ],
            obstacles: Vec::new(),
        };
        let grid = bake(&inputs, settings(1.0)).expect("bakes");
        assert!(!blocked_at(&grid, 0.5, 0.5));
        assert!(blocked_at(&grid, 4.5, 0.5), "the gap between the floors");
        assert!(!blocked_at(&grid, 8.5, 0.5));
    }

    #[test]
    fn without_ground_the_grid_spans_the_obstacles_and_is_flat() {
        let inputs = BakeInputs {
            grounds: Vec::new(),
            obstacles: vec![Obstacle {
                min: Vec3::new(0.0, 0.0, 0.0),
                max: Vec3::new(1.0, 1.0, 1.0),
            }],
        };
        let grid = bake(&inputs, settings(1.0)).expect("bakes");
        assert_eq!(grid.origin, [-5.0, -5.0]);
        assert_eq!((grid.width, grid.height), (11, 11));
        assert!(blocked_at(&grid, 0.5, 0.5));
        assert!(!blocked_at(&grid, -3.0, -3.0));
    }

    #[test]
    fn a_huge_area_asks_for_bigger_cells() {
        let inputs = BakeInputs {
            grounds: vec![floor(1_000.0)],
            obstacles: Vec::new(),
        };
        let error = bake(&inputs, settings(0.5)).expect_err("too many cells");
        assert!(error.contains("Raise Cell size"), "{error}");
    }

    #[test]
    fn a_tiled_terrain_is_ground_at_each_tile_place() {
        use engine_ecs::prelude::Transform;
        use engine_scene::ColliderData;

        let mut world = World::new();
        let terrain = world
            .spawn((
                Transform::from(engine_utils::Transform::from_translation(Vec3::new(
                    0.0, 2.0, 0.0,
                ))),
                SceneCollider(ColliderData::new(ColliderShape::Terrain, BodyKind::Fixed)),
            ))
            .id();
        let flat = Heightmap::new(9, 8.0).expect("heightmap");
        let tiles = move |_: &World, entity: Entity| {
            if entity != terrain {
                return Vec::new();
            }
            [0.0, 8.0]
                .map(|x| (flat.clone(), Mat4::from_translation(Vec3::new(x, 0.0, 0.0))))
                .to_vec()
        };
        let inputs = collect(&mut world, &|_, _| None, &tiles);
        assert_eq!(inputs.grounds.len(), 2);
        assert!(
            inputs.obstacles.is_empty(),
            "the ground itself never blocks"
        );
        let grid = bake(&inputs, settings(1.0)).expect("bakes");
        assert_eq!((grid.width, grid.height), (16, 8));
        assert_eq!(grid.origin, [-4.0, -4.0]);
        assert!(
            grid.heights
                .iter()
                .all(|height| (*height - 2.0).abs() < 1e-4)
        );
        assert!(grid.blocked.iter().all(|blocked| !*blocked));
    }

    #[test]
    fn collect_reads_fixed_solid_colliders_only() {
        use engine_ecs::prelude::Transform;
        use engine_scene::ColliderData;

        let mut world = World::new();
        let solid = |shape, body| SceneCollider(ColliderData::new(shape, body));
        let cube = ColliderShape::Cuboid {
            half_extents: [0.5, 0.5, 0.5],
        };
        world.spawn((
            Transform::from(engine_utils::Transform::from_translation(Vec3::new(
                0.0, 0.5, 0.0,
            ))),
            solid(cube.clone(), BodyKind::Fixed),
        ));
        world.spawn(solid(cube.clone(), BodyKind::Dynamic));
        world.spawn(solid(cube.clone(), BodyKind::Kinematic));
        let mut trigger = ColliderData::new(cube.clone(), BodyKind::Fixed);
        trigger.trigger = true;
        world.spawn(SceneCollider(trigger));
        world.spawn((
            solid(cube.clone(), BodyKind::Fixed),
            engine_ecs::components::Disabled,
        ));
        world.spawn((
            solid(cube, BodyKind::Fixed),
            engine_scene::SceneAi(engine_scene::AiData::default()),
        ));

        let inputs = collect(&mut world, &|_, _| None, &|_, _| Vec::new());
        assert_eq!(inputs.obstacles.len(), 1);
        assert_eq!(inputs.obstacles[0].min, Vec3::new(-0.5, 0.0, -0.5));
        assert_eq!(inputs.obstacles[0].max, Vec3::new(0.5, 1.0, 0.5));
        assert_eq!(inputs.grounds.len(), 1, "an upright box is also ground");
    }
}
