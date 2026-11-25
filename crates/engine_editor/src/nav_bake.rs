//! Baking the scene's walkable area: a nav grid built from the ground and
//! the solid objects standing on it.
//!
//! Each cell holds up to four floors, found by looking straight down
//! through it: terrain, and the tops of fixed boxes, cylinders and mesh
//! colliders (a bridge over the ground, an upper storey, a ramp). A floor
//! is blocked when:
//! - it is steeper than the bake's maximum slope;
//! - a fixed, solid collider fills the space a character needs above it
//!   (from the step height up to the agent's height), grown by the agent's
//!   radius so characters keep clear of walls.
//!
//! A cell with no floor is blocked when the scene has ground somewhere.
//! Neighbouring floors connect when the height between them is within the
//! climb: the step height plus what the steepest slope rises across a
//! cell.
//!
//! Colliders block by their real shape: balls and capsules are round,
//! turned boxes are turned, meshes block by their triangles and inside.
//! Cylinders count by the smaller of their box and capsule.
//!
//! Moving bodies (dynamic and kinematic), triggers, disabled objects and
//! objects with AI are not part of the level; moving bodies with Blocks
//! paths on block while the game runs instead. Without any ground, the
//! grid is flat at `0` and spans the solid objects.
//!
//! A 2D scene bakes on the XY plane: every fixed solid collider (sprites,
//! solid tilemap cells) blocks the cells it covers, at any depth.

use engine_ai::{Clearance, MAX_LAYERS, Solid};
use engine_ecs::prelude::{Entity, World};
use engine_renderer::{Aabb, Heightmap};
use engine_scene::{
    BodyKind, ColliderMesh, ColliderShape, NavBakeSettings, NavGridData, NavPlane, NavSpanData,
    SCREEN_TO_GRID, SceneCollider, collider_solid,
};
use glam::{Mat4, Vec2, Vec3};

/// The most cells one bake may make. A grid this size is a few megabytes in
/// the scene file.
pub const MAX_BAKE_CELLS: u64 = 250_000;
/// How far past the solid objects a grid with no ground reaches.
const NO_GROUND_MARGIN: f32 = 5.0;
/// Floors closer than this in one cell are the same floor.
const SAME_FLOOR: f32 = 0.05;
/// How far in front of and behind the screen a 2D bake looks.
const SCREEN_DEPTH: f32 = 10_000.0;

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

/// A fixed solid object, in grid space (the world, for a 3D bake).
#[derive(Debug, Clone, PartialEq)]
pub struct Obstacle {
    /// Its shape.
    pub solid: Solid,
    /// Whether characters can stand on its top.
    pub floor: bool,
}

impl Obstacle {
    /// A box from `min` to `max`, with a floor on top.
    pub fn aabb(min: Vec3, max: Vec3) -> Self {
        Self {
            solid: Solid::aabb(min, max),
            floor: true,
        }
    }
}

/// What a bake reads from the scene.
#[derive(Debug, Clone, Default)]
pub struct BakeInputs {
    /// Terrain characters can stand on.
    pub grounds: Vec<Ground>,
    /// Everything solid: it blocks, and boxes, cylinders and meshes are
    /// floors too.
    pub obstacles: Vec<Obstacle>,
    /// The plane the grid lies on.
    pub plane: NavPlane,
}

/// One floor found in a cell.
#[derive(Debug, Clone, Copy)]
struct Floor {
    height: f32,
    steep: bool,
    /// The obstacle whose top it is, if any.
    source: Option<usize>,
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
    grid.plane = inputs.plane;
    let screen = inputs.plane == NavPlane::Screen;
    let clearance = Clearance {
        radius: settings.agent_radius,
        step: if screen { 0.0 } else { settings.step_height },
        height: if screen {
            2.0 * SCREEN_DEPTH
        } else {
            settings.agent_height
        },
    };
    let walkable_cos = settings.max_slope_degrees.to_radians().cos();
    let has_ground =
        !screen && (!inputs.grounds.is_empty() || inputs.obstacles.iter().any(|o| o.floor));
    let floors = if has_ground {
        find_floors(inputs, &grid, settings)
    } else {
        let level = if screen { -SCREEN_DEPTH } else { 0.0 };
        let floor = Floor {
            height: level,
            steep: false,
            source: None,
        };
        vec![vec![floor]; grid.cell_count()]
    };

    let mut blocked: Vec<Vec<bool>> = floors
        .iter()
        .map(|cell| cell.iter().map(|floor| floor.steep).collect())
        .collect();
    let half = Vec2::splat(cell * 0.5);
    let grow = Vec2::splat(settings.agent_radius.max(0.0));
    for (index, obstacle) in inputs.obstacles.iter().enumerate() {
        let (lo, hi) = obstacle.solid.bounds();
        let (lo, hi) = (Vec2::new(lo.x, lo.z) - grow, Vec2::new(hi.x, hi.z) + grow);
        for (cell_index, centre) in cells_in(&grid, lo, hi) {
            let (cell_min, cell_max) = (centre - half, centre + half);
            for (layer, floor) in floors[cell_index].iter().enumerate() {
                if blocked[cell_index][layer] {
                    continue;
                }
                blocked[cell_index][layer] = if floor.source == Some(index) {
                    obstacle.solid.blocks_own_floor(
                        cell_min,
                        cell_max,
                        floor.height,
                        &clearance,
                        walkable_cos,
                    )
                } else {
                    obstacle
                        .solid
                        .blocks_floor(cell_min, cell_max, floor.height, &clearance)
                };
            }
        }
    }

    let mut heights = vec![0.0; grid.cell_count()];
    for (index, cell_floors) in floors.iter().enumerate() {
        match cell_floors.first() {
            Some(floor) => {
                if !screen {
                    heights[index] = floor.height;
                }
                grid.blocked[index] = blocked[index][0];
            }
            None => grid.blocked[index] = true,
        }
        for (layer, floor) in cell_floors.iter().enumerate().skip(1) {
            grid.upper.push(NavSpanData {
                cell: index as u32,
                height: floor.height,
                blocked: blocked[index][layer],
            });
        }
    }
    if heights.iter().any(|height| *height != 0.0) {
        grid.heights = heights;
    }
    if has_ground {
        let rise = cell * std::f32::consts::SQRT_2 * settings.max_slope_degrees.to_radians().tan();
        grid.climb = Some(settings.step_height + rise);
    }
    grid.bake = Some(settings);
    Ok(grid)
}

/// Every floor of every cell, lowest first, at most [`MAX_LAYERS`].
fn find_floors(
    inputs: &BakeInputs,
    grid: &NavGridData,
    settings: NavBakeSettings,
) -> Vec<Vec<Floor>> {
    let cell = grid.cell_size;
    let walkable_cos = settings.max_slope_degrees.to_radians().cos();
    let origin = Vec2::from_array(grid.origin);
    let mut floors = vec![Vec::new(); grid.cell_count()];
    for row in 0..grid.height {
        for column in 0..grid.width {
            let index = (row * grid.width + column) as usize;
            let centre = origin + Vec2::new(column as f32 + 0.5, row as f32 + 0.5) * cell;
            for ground in &inputs.grounds {
                if let Some(height) = ground.height_at(centre) {
                    floors[index].push(Floor {
                        height,
                        steep: ground.slope_at(centre, cell * 0.5) > settings.max_slope_degrees,
                        source: None,
                    });
                }
            }
        }
    }
    for (index, obstacle) in inputs.obstacles.iter().enumerate() {
        if !obstacle.floor {
            continue;
        }
        let (lo, hi) = obstacle.solid.bounds();
        for (cell_index, centre) in cells_in(grid, Vec2::new(lo.x, lo.z), Vec2::new(hi.x, hi.z)) {
            for (height, normal) in obstacle.solid.tops(centre) {
                floors[cell_index].push(Floor {
                    height,
                    steep: normal.y < walkable_cos - 1e-4,
                    source: Some(index),
                });
            }
        }
    }
    for cell_floors in &mut floors {
        cell_floors.sort_by(|a, b| a.height.total_cmp(&b.height));
        let mut merged: Vec<Floor> = Vec::with_capacity(cell_floors.len());
        for floor in cell_floors.drain(..) {
            match merged.last_mut() {
                // Flush surfaces are one floor: the higher top, walkable if
                // either is, owned by the object on top.
                Some(last) if floor.height - last.height < SAME_FLOOR => {
                    last.height = floor.height;
                    last.steep &= floor.steep;
                    last.source = floor.source.or(last.source);
                }
                _ => merged.push(floor),
            }
        }
        merged.truncate(MAX_LAYERS);
        *cell_floors = merged;
    }
    floors
}

/// The index and centre of every cell of `grid` overlapping the `(x, z)`
/// rectangle from `lo` to `hi`.
fn cells_in(grid: &NavGridData, lo: Vec2, hi: Vec2) -> impl Iterator<Item = (usize, Vec2)> {
    let origin = Vec2::from_array(grid.origin);
    let cell = grid.cell_size;
    let far = origin + Vec2::new(grid.width as f32, grid.height as f32) * cell;
    let reaches = hi.x >= origin.x && hi.y >= origin.y && lo.x <= far.x && lo.y <= far.y;
    let to_cell = |value: f32, start: f32, count: u32| {
        ((value - start) / cell)
            .floor()
            .clamp(0.0, count as f32 - 1.0) as u32
    };
    let (x0, x1) = (
        to_cell(lo.x, origin.x, grid.width),
        to_cell(hi.x, origin.x, grid.width),
    );
    let (y0, y1) = (
        to_cell(lo.y, origin.y, grid.height),
        to_cell(hi.y, origin.y, grid.height),
    );
    let width = grid.width;
    (y0..=y1).filter(move |_| reaches).flat_map(move |row| {
        (x0..=x1).map(move |column| {
            (
                (row * width + column) as usize,
                origin + Vec2::new(column as f32 + 0.5, row as f32 + 0.5) * cell,
            )
        })
    })
}

/// The `(x, z)` rectangle the grid covers: the ground and the objects
/// characters stand on, or without any, the solid objects plus a margin.
fn area(inputs: &BakeInputs) -> (Vec2, Vec2) {
    let mut min = Vec2::splat(f32::MAX);
    let mut max = Vec2::splat(f32::MIN);
    let flat = |point: Vec3| Vec2::new(point.x, point.z);
    let screen = inputs.plane == NavPlane::Screen;
    let has_ground =
        !screen && (!inputs.grounds.is_empty() || inputs.obstacles.iter().any(|o| o.floor));
    if !has_ground {
        for obstacle in &inputs.obstacles {
            let (lo, hi) = obstacle.solid.bounds();
            min = min.min(flat(lo));
            max = max.max(flat(hi));
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
    for obstacle in inputs.obstacles.iter().filter(|o| o.floor) {
        let (lo, hi) = obstacle.solid.bounds();
        min = min.min(flat(lo));
        max = max.max(flat(hi));
    }
    (min, max)
}

/// The bake settings as menu fields. Returns whether any changed. A 2D
/// bake (`screen`) has no heights, so it shows only the cell size and
/// character radius.
pub fn show_controls(ui: &mut egui::Ui, settings: &mut NavBakeSettings, screen: bool) -> bool {
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
    if screen {
        return changed;
    }
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

/// A mesh collider's triangles, in the object's own space.
pub type TriangleSource<'a> = dyn Fn(&World, Entity) -> Option<Vec<[Vec3; 3]>> + 'a;

/// Reads the ground and obstacles from the editor's world, for a grid on
/// `plane`.
///
/// `mesh_triangles` gives a mesh collider's triangles; when it has none,
/// `mesh_bounds` gives the box around the object's mesh instead. `tiles`
/// gives a terrain's tiles, each with the matrix from the tile's own space
/// to the terrain object's. A 2D bake leaves terrain out.
pub fn collect(
    world: &mut World,
    plane: NavPlane,
    mesh_triangles: &TriangleSource<'_>,
    mesh_bounds: &dyn Fn(&World, Entity) -> Option<Aabb>,
    tiles: &TileSource<'_>,
) -> BakeInputs {
    let mut inputs = BakeInputs {
        plane,
        ..BakeInputs::default()
    };
    let screen = plane == NavPlane::Screen;
    let to_grid = if screen {
        SCREEN_TO_GRID
    } else {
        Mat4::IDENTITY
    };
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
            if !screen {
                for (heightmap, tile) in tiles(world, entity) {
                    inputs.grounds.push(Ground::Terrain {
                        heightmap,
                        to_world: to_world * tile,
                    });
                }
            }
            continue;
        }
        let Some(solid) =
            collider_solid(&collider.shape, to_world, to_grid, || match mesh_triangles(
                world, entity,
            ) {
                Some(triangles) => Some(ColliderMesh::Triangles(triangles)),
                None => mesh_bounds(world, entity).map(|bounds| ColliderMesh::Bounds {
                    min: bounds.min,
                    max: bounds.max,
                }),
            })
        else {
            continue;
        };
        let floor = !screen
            && matches!(
                collider.shape,
                ColliderShape::Cuboid { .. }
                    | ColliderShape::Cylinder { .. }
                    | ColliderShape::TriMesh { .. }
            );
        inputs.obstacles.push(Obstacle { solid, floor });
    }
    inputs
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
        let crate_box = Obstacle::aabb(Vec3::new(1.0, 0.0, 1.0), Vec3::new(2.0, 1.0, 2.0));
        let inputs = BakeInputs {
            grounds: vec![floor(10.0)],
            obstacles: vec![crate_box],
            ..BakeInputs::default()
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
        let step = Obstacle::aabb(Vec3::new(1.0, 0.0, 1.0), Vec3::new(2.0, 0.2, 2.0));
        let sign = Obstacle::aabb(Vec3::new(3.0, 2.5, 3.0), Vec3::new(4.0, 3.0, 4.0));
        let inputs = BakeInputs {
            grounds: vec![floor(10.0)],
            obstacles: vec![step, sign],
            ..BakeInputs::default()
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
            ..BakeInputs::default()
        };
        let grid = bake(&steep, settings(1.0)).expect("bakes");
        assert!(grid.blocked.iter().all(|blocked| *blocked));

        let ramp = BakeInputs {
            grounds: vec![Ground::Terrain {
                heightmap: gentle,
                to_world: Mat4::from_translation(Vec3::new(0.0, 1.0, 0.0)),
            }],
            obstacles: Vec::new(),
            ..BakeInputs::default()
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
            ..BakeInputs::default()
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
                floor: false,
                ..Obstacle::aabb(Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 1.0, 1.0))
            }],
            ..BakeInputs::default()
        };
        let grid = bake(&inputs, settings(1.0)).expect("bakes");
        assert_eq!(grid.origin, [-5.0, -5.0]);
        assert_eq!((grid.width, grid.height), (11, 11));
        assert!(blocked_at(&grid, 0.5, 0.5));
        assert!(!blocked_at(&grid, -3.0, -3.0));
    }

    #[test]
    fn a_bridge_is_a_second_floor_over_the_ground() {
        let deck = Obstacle::aabb(Vec3::new(-1.0, 2.8, -5.0), Vec3::new(1.0, 3.0, 5.0));
        let inputs = BakeInputs {
            grounds: vec![floor(10.0)],
            obstacles: vec![deck],
            ..BakeInputs::default()
        };
        let grid = bake(&inputs, settings(0.5)).expect("bakes");
        assert!(
            !blocked_at(&grid, 0.25, 0.25),
            "the ground under the deck is open"
        );
        let column = ((0.25 - grid.origin[0]) / grid.cell_size) as u32;
        let row = ((0.25 - grid.origin[1]) / grid.cell_size) as u32;
        let cell = row * grid.width + column;
        let on_deck: Vec<_> = grid.upper.iter().filter(|span| span.cell == cell).collect();
        assert_eq!(on_deck.len(), 1);
        assert!((on_deck[0].height - 3.0).abs() < 1e-4);
        assert!(!on_deck[0].blocked);
        assert!(grid.climb.is_some_and(|climb| climb > 0.4));

        // Lower the deck into the space a character needs: the ground under
        // it is blocked, the deck still walkable.
        let low = Obstacle::aabb(Vec3::new(-1.0, 1.0, -5.0), Vec3::new(1.0, 1.2, 5.0));
        let inputs = BakeInputs {
            obstacles: vec![low],
            ..inputs
        };
        let grid = bake(&inputs, settings(0.5)).expect("bakes");
        assert!(blocked_at(&grid, 0.25, 0.25));
        assert!(
            grid.upper
                .iter()
                .any(|span| span.cell == cell && !span.blocked)
        );
    }

    #[test]
    fn a_ball_blocks_by_its_round_shape() {
        let ball = Obstacle {
            solid: Solid::sphere(Vec3::new(0.0, 0.5, 0.0), 1.0),
            floor: false,
        };
        let inputs = BakeInputs {
            grounds: vec![floor(10.0)],
            obstacles: vec![ball],
            ..BakeInputs::default()
        };
        let grid = bake(&inputs, settings(0.25)).expect("bakes");
        assert!(blocked_at(&grid, 0.1, 0.1));
        assert!(
            !blocked_at(&grid, 0.85, 0.85),
            "the corner of the box round the ball stays open"
        );
    }

    #[test]
    fn a_2d_bake_lies_on_the_screen_and_blocks_at_any_depth() {
        use engine_ecs::prelude::Transform;
        use engine_scene::ColliderData;

        let mut world = World::new();
        // A wall tile at x 2, y 1, well in front of the screen.
        world.spawn((
            Transform::from(engine_utils::Transform::from_translation(Vec3::new(
                2.0, 1.0, 5.0,
            ))),
            SceneCollider(ColliderData::new(
                ColliderShape::Cuboid {
                    half_extents: [0.5, 0.5, 0.05],
                },
                BodyKind::Fixed,
            )),
        ));
        let inputs = collect(
            &mut world,
            NavPlane::Screen,
            &|_, _| None,
            &|_, _| None,
            &|_, _| Vec::new(),
        );
        assert!(!inputs.obstacles[0].floor, "2D objects are never floors");
        let grid = bake(&inputs, settings(0.5)).expect("bakes");
        assert_eq!(grid.plane, NavPlane::Screen);
        assert!(grid.heights.is_empty() && grid.upper.is_empty());
        assert_eq!(grid.climb, None);
        // Grid rows run along world Y.
        assert!(blocked_at(&grid, 2.0, 1.0));
        assert!(!blocked_at(&grid, 2.0, -1.0));
        assert!(!blocked_at(&grid, 0.0, 1.0));
    }

    #[test]
    fn a_mesh_collider_blocks_by_its_triangles() {
        use engine_ecs::prelude::Transform;
        use engine_scene::{AssetRef, ColliderData};

        let mut world = World::new();
        // A thin diagonal wall: its box would cover the whole square.
        let wall = world
            .spawn((
                Transform::default(),
                SceneCollider(ColliderData::new(
                    ColliderShape::TriMesh {
                        mesh: AssetRef {
                            id: uuid::Uuid::nil().to_string(),
                        },
                    },
                    BodyKind::Fixed,
                )),
            ))
            .id();
        let triangles = move |_: &World, entity: Entity| {
            (entity == wall).then(|| {
                let (a, b) = (Vec3::new(-2.0, 0.0, -2.0), Vec3::new(2.0, 0.0, 2.0));
                let up = Vec3::Y * 2.0;
                vec![[a, b, b + up], [a, b + up, a + up]]
            })
        };
        let mut inputs = collect(
            &mut world,
            NavPlane::Ground,
            &triangles,
            &|_, _| None,
            &|_, _| Vec::new(),
        );
        inputs.grounds.push(floor(8.0));
        let grid = bake(&inputs, settings(0.5)).expect("bakes");
        assert!(blocked_at(&grid, 0.1, 0.1), "on the wall");
        assert!(!blocked_at(&grid, 1.5, -1.5), "beside it, inside its box");
    }

    #[test]
    fn a_huge_area_asks_for_bigger_cells() {
        let inputs = BakeInputs {
            grounds: vec![floor(1_000.0)],
            obstacles: Vec::new(),
            ..BakeInputs::default()
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
        let inputs = collect(
            &mut world,
            NavPlane::Ground,
            &|_, _| None,
            &|_, _| None,
            &tiles,
        );
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

        let inputs = collect(
            &mut world,
            NavPlane::Ground,
            &|_, _| None,
            &|_, _| None,
            &|_, _| Vec::new(),
        );
        assert_eq!(inputs.obstacles.len(), 1);
        let (min, max) = inputs.obstacles[0].solid.bounds();
        assert!(min.distance(Vec3::new(-0.5, 0.0, -0.5)) < 1e-5);
        assert!(max.distance(Vec3::new(0.5, 1.0, 0.5)) < 1e-5);
        assert!(inputs.obstacles[0].floor, "a box is also ground");
    }
}
