//! Tilemap viewport picking and edits of already-painted sprite children.

use engine_ecs::components::{Sprite, SpriteSource, Transform};
use engine_ecs::prelude::{Children, Entity, World};
use engine_scene::{BodyKind, ColliderData, ColliderShape, GridRegion, SceneTilemap, TilemapData};
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
        world.entity_mut(parent).insert(SceneTilemap(next));
        return Ok(true);
    }
    let old_collider = brush_collider(old.cell_size);
    let next_collider = brush_collider(next.cell_size);
    let children: Vec<_> = world
        .get::<Children>(parent)
        .map(|children| children.iter().copied().collect())
        .unwrap_or_default();
    for child in children {
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

#[cfg(test)]
mod tests {
    use bevy_ecs::change_detection::DetectChanges;
    use engine_ecs::components::{Name, Sprite, SpriteSource, Transform};
    use engine_ecs::prelude::{ChildOf, World};
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
}
