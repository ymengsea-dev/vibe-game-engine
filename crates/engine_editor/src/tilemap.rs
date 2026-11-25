//! Tilemap viewport picking, layers, rule tiles and edits of already-painted
//! sprite children.
//!
//! A map's painted cells are sprite children. With layers, each layer is a
//! child object (found by its object ID) holding that layer's cells; a map
//! from before layers keeps its cells straight under it until it gets
//! layers.

use std::collections::HashMap;

use engine_ecs::components::{Name, Sprite, SpriteSource, Transform};
use engine_ecs::prelude::{ChildOf, Children, Entity, World};
use engine_scene::{
    AutoTileLayout, BodyKind, ColliderData, ColliderShape, GridRegion, MAX_TILE_LAYERS,
    SceneObjectId, SceneTilemap, TileLayerData, TilemapData,
};
use glam::{Mat4, Vec2, Vec4};

pub(crate) fn brush_collider(size: [f32; 2]) -> ColliderData {
    ColliderData::new(
        ColliderShape::Cuboid {
            half_extents: [size[0] * 0.5, size[1] * 0.5, 0.05],
        },
        BodyKind::Fixed,
    )
}

/// Intersects the viewport pointer ray with a tilemap parent's local XY plane.
pub(crate) fn cell_from_screen(
    view_projection: Mat4,
    model: Mat4,
    pointer: Vec2,
    size: Vec2,
    cell_size: [f32; 2],
) -> Option<(i32, i32)> {
    if !view_projection.is_finite()
        || !model.is_finite()
        || !pointer.is_finite()
        || !size.is_finite()
        || size.min_element() <= 0.0
        || pointer.min_element() < 0.0
        || pointer.x > size.x
        || pointer.y > size.y
        || !cell_size
            .iter()
            .all(|value| value.is_finite() && *value > 0.0)
    {
        return None;
    }
    let inverse_view = view_projection.inverse();
    let inverse_model = model.inverse();
    if !inverse_view.is_finite() || !inverse_model.is_finite() {
        return None;
    }
    let ndc = Vec2::new(
        pointer.x / size.x * 2.0 - 1.0,
        1.0 - pointer.y / size.y * 2.0,
    );
    let unproject = |depth| {
        let point = inverse_view * Vec4::new(ndc.x, ndc.y, depth, 1.0);
        (point.w.abs() > f32::EPSILON)
            .then(|| inverse_model.transform_point3(point.truncate() / point.w))
    };
    let near = unproject(0.0)?;
    let far = unproject(1.0)?;
    let direction = far - near;
    if direction.z.abs() < f32::EPSILON {
        return None;
    }
    let distance = -near.z / direction.z;
    if !(0.0..=1.0).contains(&distance) {
        return None;
    }
    let hit = near + direction * distance;
    let x = (hit.x / cell_size[0]).floor();
    let y = (hit.y / cell_size[1]).floor();
    if !x.is_finite() || !y.is_finite() || x.abs() > 10_000.0 || y.abs() > 10_000.0 {
        return None;
    }
    Some((x as i32, y as i32))
}

/// Applies a tilemap Inspector edit, keeping painted children's cells aligned.
pub(crate) fn update_settings(
    world: &mut World,
    parent: Entity,
    next: TilemapData,
) -> Result<bool, String> {
    next.validate()?;
    let old = world
        .get::<SceneTilemap>(parent)
        .ok_or("Selected object is not a tilemap")?
        .0
        .clone();
    old.validate()?;
    if old == next {
        return Ok(false);
    }
    if old.atlas == next.atlas
        && old.columns == next.columns
        && old.rows == next.rows
        && old.cell_size == next.cell_size
    {
        let layers_changed = old.layers != next.layers;
        world.entity_mut(parent).insert(SceneTilemap(next.clone()));
        if layers_changed {
            apply_layer_looks(world, parent, &old, &next);
        }
        return Ok(true);
    }
    let old_collider = brush_collider(old.cell_size);
    let next_collider = brush_collider(next.cell_size);
    for child in all_cells(world, parent) {
        let Some(mut source) = world.get::<SpriteSource>(child).cloned() else {
            continue;
        };
        let Some(region) = GridRegion::parse(&source.region) else {
            continue;
        };
        if source.atlas != old.atlas.id || region.columns != old.columns || region.rows != old.rows
        {
            continue;
        }
        let Some(mut transform) = world.get::<Transform>(child).copied() else {
            continue;
        };
        let old_x = transform.0.translation.x / old.cell_size[0] - 0.5;
        let old_y = transform.0.translation.y / old.cell_size[1] - 0.5;
        if !old_x.is_finite() || !old_y.is_finite() {
            continue;
        }
        let x = old_x.round();
        let y = old_y.round();
        if (old_x - x).abs() > 0.001
            || (old_y - y).abs() > 0.001
            || x.abs() > 10_000.0
            || y.abs() > 10_000.0
        {
            continue;
        }
        transform.0.translation.x = (x + 0.5) * next.cell_size[0];
        transform.0.translation.y = (y + 0.5) * next.cell_size[1];
        source.atlas = next.atlas.id.clone();
        source.size = next.cell_size;
        source.region = GridRegion {
            columns: next.columns,
            rows: next.rows,
            column: region.column.min(next.columns - 1),
            row: region.row.min(next.rows - 1),
        }
        .name();
        if let Ok(mut entity_mut) = world.get_entity_mut(child) {
            if entity_mut
                .get::<engine_scene::SceneCollider>()
                .is_some_and(|collider| collider.0 == old_collider)
            {
                entity_mut.insert(engine_scene::SceneCollider(next_collider.clone()));
            }
            entity_mut.insert((transform, source));
            if let Some(mut sprite) = entity_mut.get_mut::<Sprite>() {
                sprite.size = Vec2::from_array(next.cell_size);
            }
        }
    }
    world.entity_mut(parent).insert(SceneTilemap(next));
    Ok(true)
}

/// Children of `holder` that are painted cells.
pub(crate) fn cells(world: &World, holder: Entity) -> Vec<Entity> {
    world
        .get::<Children>(holder)
        .map(|children| {
            children
                .iter()
                .copied()
                .filter(|child| world.get::<SpriteSource>(*child).is_some())
                .collect()
        })
        .unwrap_or_default()
}

/// The object holding `layer`'s cells, if it still exists under `parent`.
pub(crate) fn layer_group(world: &World, parent: Entity, layer: &TileLayerData) -> Option<Entity> {
    world
        .get::<Children>(parent)?
        .iter()
        .copied()
        .find(|child| {
            world
                .get::<SceneObjectId>(*child)
                .is_some_and(|id| id.0 == layer.object)
        })
}

/// Every painted cell of the map: those under it and those in its layers.
pub(crate) fn all_cells(world: &World, parent: Entity) -> Vec<Entity> {
    let mut found = cells(world, parent);
    if let Some(map) = world.get::<SceneTilemap>(parent) {
        for layer in &map.0.layers {
            if let Some(group) = layer_group(world, parent, layer) {
                found.extend(cells(world, group));
            }
        }
    }
    found
}

/// A new, empty layer object under `parent`, and the layer for it.
fn spawn_layer(world: &mut World, parent: Entity, name: &str, draw_order: f32) -> TileLayerData {
    let object = uuid::Uuid::new_v4().to_string();
    world.spawn((
        Name::new(name),
        Transform::default(),
        ChildOf(parent),
        SceneObjectId(object.clone()),
    ));
    TileLayerData {
        name: name.to_owned(),
        object,
        draw_order,
        opacity: 1.0,
        solid: false,
    }
}

/// Gives the map at `parent` its layers when it has none: Background,
/// Ground and Front for an empty map (painting goes to Ground), or one
/// Ground layer holding the cells an older map already has. Also brings
/// back the object of a layer whose object was deleted, empty. Returns
/// whether anything changed.
pub(crate) fn ensure_layers(world: &mut World, parent: Entity) -> Result<bool, String> {
    let mut map = world
        .get::<SceneTilemap>(parent)
        .ok_or("Select a tilemap first")?
        .0
        .clone();
    let mut changed = false;
    if map.layers.is_empty() {
        let loose = cells(world, parent);
        if loose.is_empty() {
            map.layers = vec![
                spawn_layer(world, parent, "Background", -1.0),
                spawn_layer(world, parent, "Ground", 0.0),
                spawn_layer(world, parent, "Front", 1.0),
            ];
            map.active_layer = 1;
        } else {
            let ground = spawn_layer(world, parent, "Ground", 0.0);
            if let Some(group) = layer_group(world, parent, &ground) {
                for cell in loose {
                    world.entity_mut(cell).insert(ChildOf(group));
                }
            }
            map.layers = vec![ground];
            map.active_layer = 0;
        }
        changed = true;
    } else {
        for index in 0..map.layers.len() {
            if layer_group(world, parent, &map.layers[index]).is_none() {
                let layer = &map.layers[index];
                world.spawn((
                    Name::new(layer.name.clone()),
                    Transform::default(),
                    ChildOf(parent),
                    SceneObjectId(layer.object.clone()),
                ));
                changed = true;
            }
        }
    }
    if changed {
        world.entity_mut(parent).insert(SceneTilemap(map));
    }
    Ok(changed)
}

/// Adds a layer on top of the others and makes it the one painted.
pub(crate) fn add_layer(world: &mut World, parent: Entity) -> Result<(), String> {
    ensure_layers(world, parent)?;
    let mut map = world
        .get::<SceneTilemap>(parent)
        .ok_or("Select a tilemap first")?
        .0
        .clone();
    if map.layers.len() >= MAX_TILE_LAYERS {
        return Err(format!("A tilemap has at most {MAX_TILE_LAYERS} layers"));
    }
    let top = map
        .layers
        .iter()
        .map(|layer| layer.draw_order)
        .fold(f32::MIN, f32::max);
    let draw_order = if top == f32::MIN { 0.0 } else { top + 1.0 };
    let name = format!("Layer {}", map.layers.len() + 1);
    map.layers
        .push(spawn_layer(world, parent, &name, draw_order));
    map.active_layer = (map.layers.len() - 1) as u32;
    world.entity_mut(parent).insert(SceneTilemap(map));
    Ok(())
}

/// Swaps layer `index` with the one above it (`up`) or below it in draw
/// order, keeping the painted layer the same.
pub(crate) fn move_layer(map: &mut TilemapData, index: usize, up: bool) -> bool {
    let other = if up {
        index + 1
    } else {
        match index.checked_sub(1) {
            Some(other) => other,
            None => return false,
        }
    };
    if other >= map.layers.len() || index >= map.layers.len() {
        return false;
    }
    let (a, b) = (map.layers[index].draw_order, map.layers[other].draw_order);
    map.layers[index].draw_order = b;
    map.layers[other].draw_order = a;
    map.layers.swap(index, other);
    let active = map.active_layer as usize;
    if active == index {
        map.active_layer = other as u32;
    } else if active == other {
        map.active_layer = index as u32;
    }
    true
}

/// Brings each layer's cells and object in line with its settings where
/// they changed from `old` to `next`: draw order and opacity on every cell,
/// the layer's name on its object, and collision added to or taken from
/// every cell when Solid is switched.
fn apply_layer_looks(world: &mut World, parent: Entity, old: &TilemapData, next: &TilemapData) {
    let collider = brush_collider(next.cell_size);
    for layer in &next.layers {
        let before = old.layers.iter().find(|other| other.object == layer.object);
        if before == Some(layer) {
            continue;
        }
        let Some(group) = layer_group(world, parent, layer) else {
            continue;
        };
        if before.is_none_or(|before| before.name != layer.name) {
            world
                .entity_mut(group)
                .insert(Name::new(layer.name.clone()));
        }
        let solid_switched = before.map(|before| before.solid) != Some(layer.solid);
        for cell in cells(world, group) {
            let mut entity = world.entity_mut(cell);
            if let Some(mut source) = entity.get_mut::<SpriteSource>() {
                source.z_order = layer.draw_order;
                source.color[3] = layer.opacity;
            }
            if let Some(mut sprite) = entity.get_mut::<Sprite>() {
                sprite.z_order = layer.draw_order;
                sprite.color[3] = layer.opacity;
            }
            if solid_switched {
                if layer.solid {
                    if entity.get::<engine_scene::SceneCollider>().is_none() {
                        entity.insert(engine_scene::SceneCollider(collider.clone()));
                    }
                } else if entity
                    .get::<engine_scene::SceneCollider>()
                    .is_some_and(|current| current.0 == collider)
                {
                    entity.remove::<engine_scene::SceneCollider>();
                }
            }
        }
    }
}

/// The grid cell a painted cell sits in, from its position.
pub(crate) fn cell_of(world: &World, cell: Entity, cell_size: [f32; 2]) -> Option<(i32, i32)> {
    let at = world.get::<Transform>(cell)?.0.translation;
    let x = (at.x / cell_size[0] - 0.5).round();
    let y = (at.y / cell_size[1] - 0.5).round();
    (x.is_finite() && y.is_finite() && x.abs() <= 10_000.0 && y.abs() <= 10_000.0)
        .then_some((x as i32, y as i32))
}

/// Up, right, down and left, as grid steps (rows grow upward).
const SIDES: [(i32, i32); 4] = [(0, 1), (1, 0), (0, -1), (-1, 0)];
/// Up, up-right, right, down-right, down, down-left, left, up-left.
const AROUND: [(i32, i32); 8] = [
    (0, 1),
    (1, 1),
    (1, 0),
    (1, -1),
    (0, -1),
    (-1, -1),
    (-1, 0),
    (-1, 1),
];

/// The tile of a rule-tile block for a cell whose filled neighbours are
/// `filled(dx, dy)`, as `(column, row)` inside the block.
pub(crate) fn auto_tile(layout: AutoTileLayout, filled: impl Fn(i32, i32) -> bool) -> (u32, u32) {
    match layout {
        AutoTileLayout::Edges16 => {
            let mask = SIDES
                .iter()
                .enumerate()
                .filter(|(_, (dx, dy))| filled(*dx, *dy))
                .fold(0u32, |mask, (bit, _)| mask | 1 << bit);
            (mask % 4, mask / 4)
        }
        AutoTileLayout::Blob47 => {
            let raw = AROUND
                .iter()
                .enumerate()
                .filter(|(_, (dx, dy))| filled(*dx, *dy))
                .fold(0u32, |mask, (bit, _)| mask | 1 << bit);
            let reduced = blob_reduce(raw);
            let index = BLOB_MASKS
                .iter()
                .position(|mask| *mask == reduced)
                .unwrap_or(0) as u32;
            (index % 8, index / 8)
        }
    }
}

/// `mask` (bits as [`AROUND`]) with every corner dropped whose two sides
/// are not both filled.
const fn blob_reduce(mask: u32) -> u32 {
    let mut reduced = mask & 0b0101_0101;
    let mut corner = 1;
    while corner < 8 {
        let before = 1 << (corner - 1);
        let after = 1 << ((corner + 1) % 8);
        if mask & (1 << corner) != 0 && mask & before != 0 && mask & after != 0 {
            reduced |= 1 << corner;
        }
        corner += 2;
    }
    reduced
}

/// The 47 neighbour shapes of a blob block, in the block's tile order.
const BLOB_MASKS: [u32; 47] = blob_masks();

const fn blob_masks() -> [u32; 47] {
    let mut masks = [0u32; 47];
    let mut count = 0;
    let mut mask = 0;
    while mask < 256 {
        if blob_reduce(mask) == mask && count < 47 {
            masks[count] = mask;
            count += 1;
        }
        mask += 1;
    }
    masks
}

/// Re-picks the tile of every rule-tile cell of `holder` in the 3x3 cells
/// round `(x, y)`, after `(x, y)` was painted or erased. A cell is a
/// rule-tile cell when its tile lies in the map's rule-tile block. Returns
/// whether any cell changed.
pub(crate) fn retile_around(
    world: &mut World,
    holder: Entity,
    map: &TilemapData,
    x: i32,
    y: i32,
) -> bool {
    let Some(layout) = map.brush_auto else {
        return false;
    };
    let (block_columns, block_rows) = layout.block();
    let in_block = |world: &World, cell: Entity| {
        world.get::<SpriteSource>(cell).is_some_and(|source| {
            source.atlas == map.atlas.id
                && GridRegion::parse(&source.region).is_some_and(|region| {
                    region.columns == map.columns
                        && region.rows == map.rows
                        && (map.brush_column..map.brush_column + block_columns)
                            .contains(&region.column)
                        && (map.brush_row..map.brush_row + block_rows).contains(&region.row)
                })
        })
    };
    let rule_cells: HashMap<(i32, i32), Entity> = cells(world, holder)
        .into_iter()
        .filter(|cell| in_block(world, *cell))
        .filter_map(|cell| cell_of(world, cell, map.cell_size).map(|at| (at, cell)))
        .collect();
    let mut changed = false;
    for dy in -1..=1 {
        for dx in -1..=1 {
            let at = (x + dx, y + dy);
            let Some(&cell) = rule_cells.get(&at) else {
                continue;
            };
            let (column, row) = auto_tile(layout, |nx, ny| {
                rule_cells.contains_key(&(at.0 + nx, at.1 + ny))
            });
            let region = GridRegion {
                columns: map.columns,
                rows: map.rows,
                column: map.brush_column + column,
                row: map.brush_row + row,
            }
            .name();
            if let Some(mut source) = world.get_mut::<SpriteSource>(cell)
                && source.region != region
            {
                source.region = region;
                changed = true;
            }
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use bevy_ecs::change_detection::DetectChanges;
    use engine_ecs::components::{Name, Sprite, SpriteSource, Transform};
    use engine_ecs::prelude::{ChildOf, Entity, World};
    use engine_scene::{
        AssetRef, BodyKind, ColliderData, ColliderShape, SceneCollider, SceneTilemap, TilemapData,
    };
    use glam::{Mat4, Vec2, Vec3};

    #[test]
    fn viewport_pointer_maps_into_parent_local_tile_cell() {
        assert_eq!(
            super::cell_from_screen(
                Mat4::IDENTITY,
                Mat4::IDENTITY,
                Vec2::new(75.0, 25.0),
                Vec2::splat(100.0),
                [0.5, 0.5],
            ),
            Some((1, 1)),
        );
        assert_eq!(
            super::cell_from_screen(
                Mat4::ZERO,
                Mat4::IDENTITY,
                Vec2::splat(50.0),
                Vec2::splat(100.0),
                [1.0, 1.0],
            ),
            None,
        );
    }

    #[test]
    fn changing_tilemap_grid_reflows_existing_cells() {
        let id = uuid::Uuid::new_v4().to_string();
        let mut world = World::new();
        let parent = world
            .spawn((
                Name::new("Tilemap"),
                SceneTilemap(TilemapData::new(AssetRef { id: id.clone() })),
            ))
            .id();
        let child = world
            .spawn((
                Name::new("Tile (-1, 2)"),
                ChildOf(parent),
                Transform::from(engine_utils::Transform::from_translation(Vec3::new(
                    -0.5, 2.5, 0.0,
                ))),
                SpriteSource {
                    atlas: id.clone(),
                    region: "grid:4,4:3,0".into(),
                    size: [1.0, 1.0],
                    color: [1.0; 4],
                    z_order: 0.0,
                },
                Sprite::new(
                    Vec2::ONE,
                    engine_renderer::UvRect {
                        min: [0.0; 2],
                        max: [1.0; 2],
                    },
                ),
                SceneCollider(ColliderData::new(
                    ColliderShape::Cuboid {
                        half_extents: [0.5, 0.5, 0.05],
                    },
                    BodyKind::Fixed,
                )),
            ))
            .id();
        let mut next = world.get::<SceneTilemap>(parent).unwrap().0.clone();
        next.columns = 2;
        next.rows = 2;
        next.cell_size = [2.0, 3.0];

        assert!(super::update_settings(&mut world, parent, next).unwrap());
        assert_eq!(
            world.get::<Transform>(child).unwrap().0.translation,
            Vec3::new(-1.0, 7.5, 0.0)
        );
        assert_eq!(
            world.get::<SpriteSource>(child).unwrap().region,
            "grid:2,2:1,0"
        );
        assert_eq!(
            world.get::<Sprite>(child).unwrap().size,
            Vec2::new(2.0, 3.0)
        );
        assert_eq!(
            world.get::<SceneCollider>(child).unwrap().0.shape,
            ColliderShape::Cuboid {
                half_extents: [1.0, 1.5, 0.05]
            }
        );
    }

    #[test]
    fn changing_only_tilemap_brush_does_not_rewrite_painted_cells() {
        let id = uuid::Uuid::new_v4().to_string();
        let mut world = World::new();
        let parent = world
            .spawn((
                Name::new("Tilemap"),
                SceneTilemap(TilemapData::new(AssetRef { id: id.clone() })),
            ))
            .id();
        let child = world
            .spawn((
                Name::new("Tile (0, 0)"),
                ChildOf(parent),
                Transform::from(engine_utils::Transform::from_translation(Vec3::new(
                    0.5, 0.5, 0.0,
                ))),
                SpriteSource {
                    atlas: id,
                    region: "grid:4,4:0,0".into(),
                    size: [1.0, 1.0],
                    color: [1.0; 4],
                    z_order: 0.0,
                },
            ))
            .id();
        world.clear_trackers();
        let mut next = world.get::<SceneTilemap>(parent).unwrap().0.clone();
        next.brush_collision = true;

        assert!(super::update_settings(&mut world, parent, next).unwrap());
        assert!(
            !world
                .entity(child)
                .get_ref::<Transform>()
                .unwrap()
                .is_changed()
        );
        assert!(
            !world
                .entity(child)
                .get_ref::<SpriteSource>()
                .unwrap()
                .is_changed()
        );
    }

    #[test]
    fn edge_rule_tiles_follow_the_side_neighbours() {
        use engine_scene::AutoTileLayout::Edges16;
        let with = |filled: &'static [(i32, i32)]| {
            super::auto_tile(Edges16, move |dx, dy| filled.contains(&(dx, dy)))
        };
        assert_eq!(with(&[]), (0, 0), "alone");
        assert_eq!(with(&[(0, 1)]), (1, 0), "up is 1");
        assert_eq!(with(&[(1, 0), (-1, 0)]), (2, 2), "right 2 + left 8");
        assert_eq!(
            with(&[(0, 1), (1, 0), (0, -1), (-1, 0)]),
            (3, 3),
            "all four"
        );
        assert_eq!(with(&[(1, 1)]), (0, 0), "corners don't count");
    }

    #[test]
    fn blob_rule_tiles_have_47_shapes_and_need_both_sides_for_a_corner() {
        use engine_scene::AutoTileLayout::Blob47;
        let masks = super::BLOB_MASKS;
        assert_eq!(masks.len(), 47);
        assert!(
            masks.windows(2).all(|pair| pair[0] < pair[1]),
            "in number order"
        );
        let with = |filled: &'static [(i32, i32)]| {
            super::auto_tile(Blob47, move |dx, dy| filled.contains(&(dx, dy)))
        };
        assert_eq!(with(&[]), (0, 0));
        let everything: &[(i32, i32)] = &[
            (0, 1),
            (1, 1),
            (1, 0),
            (1, -1),
            (0, -1),
            (-1, -1),
            (-1, 0),
            (-1, 1),
        ];
        assert_eq!(with(everything), (46 % 8, 46 / 8), "the last tile");
        assert_eq!(
            with(&[(0, 1), (1, 0), (1, 1)]),
            with(&[(0, 1), (1, 0), (1, 1)]),
        );
        assert_ne!(
            with(&[(0, 1), (1, 0), (1, 1)]),
            with(&[(0, 1), (1, 0)]),
            "a corner with both sides is its own shape"
        );
        assert_eq!(
            with(&[(0, 1), (1, 1)]),
            with(&[(0, 1)]),
            "a corner without both sides is dropped"
        );
    }

    fn map_with_cell(world: &mut World) -> (Entity, Entity, String) {
        let id = uuid::Uuid::new_v4().to_string();
        let parent = world
            .spawn((
                Name::new("Tilemap"),
                SceneTilemap(TilemapData::new(AssetRef { id: id.clone() })),
            ))
            .id();
        let cell = world
            .spawn((
                Name::new("Tile (0, 0)"),
                ChildOf(parent),
                Transform::from(engine_utils::Transform::from_translation(Vec3::new(
                    0.5, 0.5, 0.0,
                ))),
                SpriteSource {
                    atlas: id.clone(),
                    region: "grid:4,4:0,0".into(),
                    size: [1.0, 1.0],
                    color: [1.0; 4],
                    z_order: 0.0,
                },
                Sprite::new(
                    Vec2::ONE,
                    engine_renderer::UvRect {
                        min: [0.0; 2],
                        max: [1.0; 2],
                    },
                ),
            ))
            .id();
        (parent, cell, id)
    }

    #[test]
    fn an_older_map_gets_one_layer_holding_its_cells() {
        let mut world = World::new();
        let (parent, cell, _) = map_with_cell(&mut world);
        assert!(super::ensure_layers(&mut world, parent).unwrap());
        let map = world.get::<SceneTilemap>(parent).unwrap().0.clone();
        assert_eq!(map.layers.len(), 1);
        assert_eq!(map.layers[0].name, "Ground");
        let group = super::layer_group(&world, parent, &map.layers[0]).unwrap();
        assert_eq!(world.get::<ChildOf>(cell).unwrap().parent(), group);
        assert_eq!(super::all_cells(&world, parent), vec![cell]);
        assert!(
            !super::ensure_layers(&mut world, parent).unwrap(),
            "only once"
        );
    }

    #[test]
    fn a_new_map_gets_background_ground_and_front() {
        let mut world = World::new();
        let parent = world
            .spawn(SceneTilemap(TilemapData::new(AssetRef {
                id: uuid::Uuid::new_v4().to_string(),
            })))
            .id();
        super::ensure_layers(&mut world, parent).unwrap();
        let map = world.get::<SceneTilemap>(parent).unwrap().0.clone();
        let names: Vec<_> = map.layers.iter().map(|layer| layer.name.as_str()).collect();
        assert_eq!(names, ["Background", "Ground", "Front"]);
        assert_eq!(map.active_layer, 1);
        assert!(
            map.layers
                .windows(2)
                .all(|pair| pair[0].draw_order < pair[1].draw_order)
        );

        // A deleted layer object comes back, empty.
        let front = super::layer_group(&world, parent, &map.layers[2]).unwrap();
        world.despawn(front);
        assert!(super::ensure_layers(&mut world, parent).unwrap());
        assert!(super::layer_group(&world, parent, &map.layers[2]).is_some());
    }

    #[test]
    fn moving_a_layer_swaps_its_draw_order_and_keeps_it_painted() {
        let mut world = World::new();
        let parent = world
            .spawn(SceneTilemap(TilemapData::new(AssetRef {
                id: uuid::Uuid::new_v4().to_string(),
            })))
            .id();
        super::ensure_layers(&mut world, parent).unwrap();
        let mut map = world.get::<SceneTilemap>(parent).unwrap().0.clone();
        assert!(super::move_layer(&mut map, 1, true));
        assert_eq!(map.layers[2].name, "Ground");
        assert_eq!(map.layers[2].draw_order, 1.0);
        assert_eq!(map.layers[1].draw_order, 0.0);
        assert_eq!(map.active_layer, 2);
        assert!(!super::move_layer(&mut map, 2, true), "already on top");
        assert!(
            !super::move_layer(&mut map, 0, false),
            "already at the bottom"
        );
    }

    #[test]
    fn layer_settings_reach_its_cells() {
        let mut world = World::new();
        let (parent, cell, _) = map_with_cell(&mut world);
        super::ensure_layers(&mut world, parent).unwrap();
        let mut next = world.get::<SceneTilemap>(parent).unwrap().0.clone();
        next.layers[0].opacity = 0.5;
        next.layers[0].draw_order = 3.0;
        next.layers[0].solid = true;
        next.layers[0].name = "Walls".into();
        assert!(super::update_settings(&mut world, parent, next.clone()).unwrap());
        let sprite = world.get::<Sprite>(cell).unwrap();
        assert_eq!((sprite.color[3], sprite.z_order), (0.5, 3.0));
        assert_eq!(world.get::<SpriteSource>(cell).unwrap().z_order, 3.0);
        assert!(world.get::<SceneCollider>(cell).is_some());
        let group = super::layer_group(&world, parent, &next.layers[0]).unwrap();
        assert_eq!(world.get::<Name>(group).unwrap().0.as_str(), "Walls");

        next.layers[0].solid = false;
        super::update_settings(&mut world, parent, next).unwrap();
        assert!(world.get::<SceneCollider>(cell).is_none());
    }

    #[test]
    fn a_rule_tile_block_must_fit_the_sheet() {
        let mut map = TilemapData::new(AssetRef {
            id: uuid::Uuid::new_v4().to_string(),
        });
        map.brush_auto = Some(engine_scene::AutoTileLayout::Edges16);
        assert!(map.validate().is_ok(), "4 x 4 fits a 4 x 4 sheet");
        map.brush_column = 1;
        assert!(map.validate().is_err());
        map.brush_column = 0;
        map.brush_auto = Some(engine_scene::AutoTileLayout::Blob47);
        assert!(map.validate().is_err(), "8 x 6 does not");
        map.columns = 8;
        map.rows = 6;
        assert!(map.validate().is_ok());
    }
}
