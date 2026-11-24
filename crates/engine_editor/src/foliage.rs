//! Foliage groups on terrains: filling a terrain, painting and erasing with
//! a brush, and keeping instances on the ground after sculpting.
//!
//! Placement is a jittered grid in terrain-local XZ. Every grid cell holds
//! at most one plant, decided by a hash of the seed and the cell alone, so
//! painting over the same ground twice places nothing new, and painting
//! erased ground again brings back the same plants. Instances are stored
//! in world space (see [`engine_scene::FoliageData`]); a group remembers
//! its terrain by object ID, which is how sculpting finds what to move.

use std::collections::HashSet;

use engine_ecs::components::InstancedMeshRenderer;
use engine_ecs::prelude::{Entity, World};
use engine_renderer::TerrainSurface;
use engine_scene::{SceneFoliage, SceneObjectId, TransformData};
use engine_utils::Transform;
use glam::{IVec2, Quat, Vec2, Vec3};

/// The most instances one group holds; matches the scene file's limit.
pub const MAX_GROUP_INSTANCES: usize = 20_000;

/// The most grid cells one fill or brush dab may visit, so a tiny Spacing
/// over a large area is refused instead of freezing the editor.
pub const MAX_CELLS: u64 = 200_000;

/// How far a plant may wander from its cell centre, as a fraction of the
/// cell. Below `1` so each plant stays inside its own cell.
const JITTER: f32 = 0.85;

/// How far (metres) an older group's first instance may sit from the
/// ground and still count as standing on that terrain.
const LINK_TOLERANCE: f32 = 0.5;

/// The grid and dice that decide where plants go.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    /// Cell size: roughly the distance between plants, in metres.
    pub spacing: f32,
    /// Share of cells that get a plant, `[0, 1]`.
    pub density: f32,
    /// Changes every cell's dice without moving the grid.
    pub seed: u64,
}

/// Where to place, in terrain-local XZ.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Area {
    /// Every tile of the terrain.
    Everywhere,
    /// A brush circle.
    Circle {
        /// Centre, terrain-local XZ.
        center: Vec2,
        /// Radius in metres.
        radius: f32,
    },
}

/// Why nothing could be placed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PlaceError {
    /// The area holds more than [`MAX_CELLS`] grid cells.
    #[error("Foliage area is too dense; increase Spacing")]
    TooDense,
    /// The group already holds [`MAX_GROUP_INSTANCES`].
    #[error("This foliage group is full (20,000 plants); start another model or erase some")]
    Full,
}

/// One candidate plant, terrain-local.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plant {
    /// Position on the ground, terrain-local.
    pub position: Vec3,
    /// Rotation about +Y, radians.
    pub yaw: f32,
}

fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A float in `[0, 1)` from the top 24 bits.
fn unit(state: &mut u64) -> f32 {
    (splitmix(state) >> 40) as f32 / (1u64 << 24) as f32
}

/// The grid cell holding terrain-local `point`.
fn cell_of(point: Vec2, spacing: f32) -> IVec2 {
    (point / spacing).floor().as_ivec2()
}

/// The plant cell `cell` holds, if its dice say it has one.
fn plant_in_cell(cell: IVec2, placement: Placement) -> Option<Vec2> {
    let mut state = placement.seed
        ^ (cell.x as i64 as u64).wrapping_mul(0xD6E8_FEB8_6659_FD93)
        ^ (cell.y as i64 as u64).wrapping_mul(0xA076_1D64_78BD_642F);
    let jitter = Vec2::new(unit(&mut state) - 0.5, unit(&mut state) - 0.5) * JITTER;
    let keep = unit(&mut state);
    (keep < placement.density)
        .then(|| (cell.as_vec2() + Vec2::splat(0.5) + jitter) * placement.spacing)
}

fn yaw_in_cell(cell: IVec2, seed: u64) -> f32 {
    let mut state = !seed ^ ((cell.x as i64 as u64) << 32) ^ (cell.y as i64 as u64 & 0xFFFF_FFFF);
    unit(&mut state) * std::f32::consts::TAU
}

/// Every plant `area` holds on `surface`, nearest cell row first. Plants
/// off the terrain's tiles are skipped.
///
/// # Errors
///
/// [`PlaceError::TooDense`] when the area spans more than [`MAX_CELLS`].
pub fn plants(
    surface: &TerrainSurface,
    area: Area,
    placement: Placement,
) -> Result<Vec<Plant>, PlaceError> {
    let spacing = placement.spacing.max(0.01);
    let placement = Placement {
        spacing,
        density: placement.density.clamp(0.0, 1.0),
        ..placement
    };
    let Some((terrain_min, terrain_max)) = surface.bounds() else {
        return Ok(Vec::new());
    };
    let (min, max) = match area {
        Area::Everywhere => (terrain_min, terrain_max),
        Area::Circle { center, radius } => (
            (center - Vec2::splat(radius)).max(terrain_min),
            (center + Vec2::splat(radius)).min(terrain_max),
        ),
    };
    if min.x >= max.x || min.y >= max.y {
        return Ok(Vec::new());
    }
    let (first, last) = (cell_of(min, spacing), cell_of(max, spacing));
    let cells = (i64::from(last.x - first.x) + 1) as u64 * (i64::from(last.y - first.y) + 1) as u64;
    if cells > MAX_CELLS {
        return Err(PlaceError::TooDense);
    }
    let mut out = Vec::new();
    for z in first.y..=last.y {
        for x in first.x..=last.x {
            let cell = IVec2::new(x, z);
            let Some(point) = plant_in_cell(cell, placement) else {
                continue;
            };
            if let Area::Circle { center, radius } = area
                && point.distance_squared(center) > radius * radius
            {
                continue;
            }
            let Some(height) = surface.height_at(point.x, point.y) else {
                continue;
            };
            out.push(Plant {
                position: Vec3::new(point.x, height, point.y),
                yaw: yaw_in_cell(cell, placement.seed),
            });
        }
    }
    Ok(out)
}

/// `plant` placed in the world by its terrain's world transform.
pub fn plant_to_world(terrain_world: &Transform, plant: Plant) -> Transform {
    terrain_world.mul_transform(&Transform {
        translation: plant.position,
        rotation: Quat::from_rotation_y(plant.yaw),
        scale: Vec3::ONE,
    })
}

/// Terrain-local XZ of a world-space instance.
fn local_xz(world_to_terrain: &glam::Mat4, instance: &TransformData) -> Vec2 {
    let local = world_to_terrain.transform_point3(Vec3::from(instance.translation));
    Vec2::new(local.x, local.z)
}

/// Adds `area`'s plants to `instances`, skipping grid cells that already
/// hold one. Returns how many were added; stops at
/// [`MAX_GROUP_INSTANCES`].
///
/// # Errors
///
/// [`PlaceError::TooDense`] as for [`plants`]; [`PlaceError::Full`] when
/// the group was already full and the area had room for more.
pub fn add(
    instances: &mut Vec<TransformData>,
    terrain_world: &Transform,
    surface: &TerrainSurface,
    area: Area,
    placement: Placement,
) -> Result<usize, PlaceError> {
    let world_to_terrain = terrain_world.to_matrix().inverse();
    let spacing = placement.spacing.max(0.01);
    let occupied: HashSet<IVec2> = instances
        .iter()
        .map(|instance| cell_of(local_xz(&world_to_terrain, instance), spacing))
        .collect();
    let mut added = 0;
    for plant in plants(surface, area, placement)? {
        if occupied.contains(&cell_of(
            Vec2::new(plant.position.x, plant.position.z),
            spacing,
        )) {
            continue;
        }
        if instances.len() >= MAX_GROUP_INSTANCES {
            return if added == 0 {
                Err(PlaceError::Full)
            } else {
                Ok(added)
            };
        }
        instances.push(TransformData::from(plant_to_world(terrain_world, plant)));
        added += 1;
    }
    Ok(added)
}

/// Removes the instances inside the brush circle (terrain-local XZ).
/// Returns how many were removed.
pub fn erase(
    instances: &mut Vec<TransformData>,
    terrain_world: &Transform,
    center: Vec2,
    radius: f32,
) -> usize {
    let world_to_terrain = terrain_world.to_matrix().inverse();
    let before = instances.len();
    instances.retain(|instance| {
        local_xz(&world_to_terrain, instance).distance_squared(center) > radius * radius
    });
    before - instances.len()
}

/// Moves each instance inside `area` (terrain-local XZ min/max, or the
/// whole terrain for `None`) onto the ground of `surface`, keeping its
/// rotation and scale. Returns whether any instance moved.
pub fn settle_on_ground(
    instances: &mut [TransformData],
    terrain_world: &Transform,
    surface: &TerrainSurface,
    area: Option<(Vec2, Vec2)>,
) -> bool {
    let terrain_to_world = terrain_world.to_matrix();
    let world_to_terrain = terrain_to_world.inverse();
    let mut moved = false;
    for instance in instances {
        let mut local = world_to_terrain.transform_point3(Vec3::from(instance.translation));
        if let Some((min, max)) = area
            && (local.x < min.x || local.x > max.x || local.z < min.y || local.z > max.y)
        {
            continue;
        }
        let Some(height) = surface.height_at(local.x, local.z) else {
            continue;
        };
        if (local.y - height).abs() <= 1e-4 {
            continue;
        }
        local.y = height;
        instance.translation = terrain_to_world.transform_point3(local).to_array();
        moved = true;
    }
    moved
}

/// `entity`'s object ID, giving it a new one if it has none yet.
pub fn ensure_object_id(world: &mut World, entity: Entity) -> String {
    if let Some(id) = world.get::<SceneObjectId>(entity) {
        return id.0.clone();
    }
    let id = uuid::Uuid::new_v4().to_string();
    world.entity_mut(entity).insert(SceneObjectId(id.clone()));
    id
}

/// Whether an unlinked group's first instance stands on `surface`.
fn stands_on(
    data: &engine_scene::FoliageData,
    world_to_terrain: &glam::Mat4,
    surface: &TerrainSurface,
) -> bool {
    let Some(first) = data.instances.first() else {
        return false;
    };
    let local = world_to_terrain.transform_point3(Vec3::from(first.translation));
    surface
        .height_at(local.x, local.z)
        .is_some_and(|height| (local.y - height).abs() <= LINK_TOLERANCE)
}

/// Keeps every foliage group on `terrain` standing on its ground after the
/// ground changed inside `area` (terrain-local XZ min/max; `None` for the
/// whole terrain).
///
/// Groups made before foliage followed the ground carry no terrain link;
/// one whose first instance stands on this terrain is linked to it here.
/// Returns whether any group changed.
pub fn follow_ground(
    world: &mut World,
    terrain: Entity,
    surface: &TerrainSurface,
    area: Option<(Vec2, Vec2)>,
) -> bool {
    let terrain_world = crate::hierarchy::world_transform(world, terrain);
    let world_to_terrain = terrain_world.to_matrix().inverse();
    let terrain_id = world.get::<SceneObjectId>(terrain).map(|id| id.0.clone());
    let mut query = world.query::<(Entity, &SceneFoliage)>();
    let mut linked = Vec::new();
    let mut unlinked = Vec::new();
    for (entity, group) in query.iter(world) {
        match &group.0.terrain {
            Some(id) if Some(id) == terrain_id.as_ref() => linked.push(entity),
            None if stands_on(&group.0, &world_to_terrain, surface) => unlinked.push(entity),
            _ => {}
        }
    }
    if !unlinked.is_empty() {
        let id = ensure_object_id(world, terrain);
        for &entity in &unlinked {
            if let Some(mut group) = world.get_mut::<SceneFoliage>(entity) {
                group.0.terrain = Some(id.clone());
            }
        }
    }
    let mut changed = !unlinked.is_empty();
    for entity in linked.into_iter().chain(unlinked) {
        let Some(mut group) = world.get_mut::<SceneFoliage>(entity) else {
            continue;
        };
        if settle_on_ground(&mut group.0.instances, &terrain_world, surface, area) {
            let instances: Vec<Transform> =
                group.0.instances.iter().copied().map(Into::into).collect();
            if let Some(mut renderer) = world.get_mut::<InstancedMeshRenderer>(entity) {
                renderer.instances = instances;
            }
            changed = true;
        }
    }
    changed
}

/// Copies a group's authored instances into its live renderer, after a
/// brush edit.
pub fn sync_renderer(world: &mut World, entity: Entity) {
    let Some(group) = world.get::<SceneFoliage>(entity) else {
        return;
    };
    let instances: Vec<Transform> = group.0.instances.iter().copied().map(Into::into).collect();
    if let Some(mut renderer) = world.get_mut::<InstancedMeshRenderer>(entity) {
        renderer.instances = instances;
    }
}

/// The group of model `mesh_id` standing on terrain `terrain_id`, if any.
pub fn group_for(world: &mut World, terrain_id: &str, mesh_id: &str) -> Option<Entity> {
    world
        .query::<(Entity, &SceneFoliage)>()
        .iter(world)
        .find(|(_, group)| {
            group.0.terrain.as_deref() == Some(terrain_id) && group.0.mesh.id == mesh_id
        })
        .map(|(entity, _)| entity)
}

/// Every group standing on terrain `terrain_id`.
pub fn groups_on(world: &mut World, terrain_id: &str) -> Vec<Entity> {
    world
        .query::<(Entity, &SceneFoliage)>()
        .iter(world)
        .filter(|(_, group)| group.0.terrain.as_deref() == Some(terrain_id))
        .map(|(entity, _)| entity)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_renderer::Heightmap;

    /// One flat 16 m tile at height `height`, centred on the origin.
    fn flat(height: f32) -> TerrainSurface {
        let mut tile = engine_renderer::TerrainTile::flat(17, 16.0).unwrap();
        tile.heights = Heightmap::from_heights(17, 16.0, vec![height; 17 * 17]).unwrap();
        let mut surface = TerrainSurface::new(17, 16.0).unwrap();
        surface.insert_tile((0, 0), tile).unwrap();
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
    fn painting_the_same_ground_twice_adds_nothing_new() {
        let surface = flat(1.0);
        let mut instances = Vec::new();
        let area = Area::Circle {
            center: Vec2::ZERO,
            radius: 4.0,
        };
        let first = add(
            &mut instances,
            &Transform::IDENTITY,
            &surface,
            area,
            placement(),
        )
        .unwrap();
        assert!(first > 0);
        let again = add(
            &mut instances,
            &Transform::IDENTITY,
            &surface,
            area,
            placement(),
        )
        .unwrap();
        assert_eq!(again, 0);
        for instance in &instances {
            let p = Vec3::from(instance.translation);
            assert!(Vec2::new(p.x, p.z).length() <= 4.0 + 1e-4);
            assert!((p.y - 1.0).abs() < 1e-4, "on the ground");
        }
    }

    #[test]
    fn neighbouring_plants_keep_to_their_own_cells() {
        let surface = flat(0.0);
        let plants = plants(&surface, Area::Everywhere, placement()).unwrap();
        let cells: HashSet<IVec2> = plants
            .iter()
            .map(|plant| cell_of(Vec2::new(plant.position.x, plant.position.z), 2.0))
            .collect();
        assert_eq!(cells.len(), plants.len());
        // A 16 m tile at 2 m spacing is an 8 x 8 grid at full density.
        assert_eq!(plants.len(), 64);
    }

    #[test]
    fn erasing_then_repainting_brings_back_the_same_plants() {
        let surface = flat(0.0);
        let mut instances = Vec::new();
        add(
            &mut instances,
            &Transform::IDENTITY,
            &surface,
            Area::Everywhere,
            placement(),
        )
        .unwrap();
        let original = instances.clone();
        let removed = erase(&mut instances, &Transform::IDENTITY, Vec2::ZERO, 3.0);
        assert!(removed > 0);
        assert!(instances.iter().all(|instance| {
            Vec2::new(instance.translation[0], instance.translation[2]).length() > 3.0
        }));
        add(
            &mut instances,
            &Transform::IDENTITY,
            &surface,
            Area::Circle {
                center: Vec2::ZERO,
                radius: 3.0,
            },
            placement(),
        )
        .unwrap();
        let key = |t: &TransformData| t.translation.map(f32::to_bits);
        let mut a: Vec<_> = original.iter().map(key).collect();
        let mut b: Vec<_> = instances.iter().map(key).collect();
        a.sort_unstable();
        b.sort_unstable();
        assert_eq!(a, b);
    }

    #[test]
    fn a_full_group_refuses_more() {
        let surface = flat(0.0);
        let mut instances = vec![TransformData::from(Transform::IDENTITY); MAX_GROUP_INSTANCES];
        // Put the existing ones far off so no cell is already occupied.
        for instance in &mut instances {
            instance.translation = [1000.0, 0.0, 1000.0];
        }
        let result = add(
            &mut instances,
            &Transform::IDENTITY,
            &surface,
            Area::Everywhere,
            placement(),
        );
        assert_eq!(result, Err(PlaceError::Full));
        assert_eq!(instances.len(), MAX_GROUP_INSTANCES);
    }

    #[test]
    fn a_tiny_spacing_over_a_big_area_is_refused() {
        let surface = flat(0.0);
        let dense = Placement {
            spacing: 0.01,
            ..placement()
        };
        assert_eq!(
            plants(&surface, Area::Everywhere, dense),
            Err(PlaceError::TooDense)
        );
    }

    #[test]
    fn settling_moves_only_instances_inside_the_area_onto_the_ground() {
        let surface = flat(3.0);
        let at = |x: f32, z: f32| {
            TransformData::from(Transform {
                rotation: Quat::from_rotation_y(1.0),
                ..Transform::from_translation(Vec3::new(x, 0.0, z))
            })
        };
        let mut instances = vec![at(1.0, 1.0), at(6.0, 6.0)];
        let moved = settle_on_ground(
            &mut instances,
            &Transform::IDENTITY,
            &surface,
            Some((Vec2::splat(-2.0), Vec2::splat(2.0))),
        );
        assert!(moved);
        assert_eq!(instances[0].translation, [1.0, 3.0, 1.0]);
        assert_eq!(instances[1].translation, [6.0, 0.0, 6.0]);
        assert_eq!(
            instances[0].rotation,
            at(1.0, 1.0).rotation,
            "rotation kept"
        );
    }

    #[test]
    fn settling_follows_a_moved_terrain() {
        let surface = flat(2.0);
        let terrain_world = Transform::from_translation(Vec3::new(100.0, 10.0, 0.0));
        let mut instances = vec![TransformData::from(Transform::from_translation(Vec3::new(
            101.0, 0.0, 1.0,
        )))];
        assert!(settle_on_ground(
            &mut instances,
            &terrain_world,
            &surface,
            None
        ));
        assert_eq!(instances[0].translation, [101.0, 12.0, 1.0]);
    }

    #[test]
    fn follow_ground_links_an_older_group_standing_on_the_terrain() {
        let surface = flat(0.0);
        let mut world = World::new();
        let terrain = world
            .spawn(engine_ecs::components::Transform::default())
            .id();
        let mesh = engine_scene::AssetRef {
            id: uuid::Uuid::new_v4().to_string(),
        };
        let on_ground = world
            .spawn(SceneFoliage(engine_scene::FoliageData::new(
                mesh.clone(),
                mesh.clone(),
                vec![TransformData::from(Transform::from_translation(Vec3::new(
                    1.0, 0.2, 1.0,
                )))],
                None,
            )))
            .id();
        let floating = world
            .spawn(SceneFoliage(engine_scene::FoliageData::new(
                mesh.clone(),
                mesh,
                vec![TransformData::from(Transform::from_translation(Vec3::new(
                    1.0, 40.0, 1.0,
                )))],
                None,
            )))
            .id();

        assert!(follow_ground(&mut world, terrain, &surface, None));
        let id = world.get::<SceneObjectId>(terrain).unwrap().0.clone();
        let linked = &world.get::<SceneFoliage>(on_ground).unwrap().0;
        assert_eq!(linked.terrain.as_ref(), Some(&id));
        assert_eq!(linked.instances[0].translation, [1.0, 0.0, 1.0]);
        assert_eq!(world.get::<SceneFoliage>(floating).unwrap().0.terrain, None);
    }
}
