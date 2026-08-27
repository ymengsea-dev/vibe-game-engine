//! World-space transform propagation through the entity hierarchy.
//!
//! Uses `bevy_ecs`'s built-in [`ChildOf`]/[`Children`] relationship for
//! parent/child storage (auto-maintained: inserting `ChildOf` on an
//! entity keeps its parent's `Children` in sync) — no reason to hand-roll
//! that part. What `bevy_ecs` doesn't provide is the actual walk that
//! turns a tree of parent-relative [`Transform`]s into world-space
//! [`GlobalTransform`]s; that's what this module does.

use bevy_ecs::entity::Entity;
use bevy_ecs::hierarchy::{ChildOf, Children};
use bevy_ecs::query::{With, Without};
use bevy_ecs::world::World;

use crate::components::{GlobalTransform, Transform};

/// Recomputes [`GlobalTransform`] for every entity that has a [`Transform`]
/// (inserting it if missing), by walking the hierarchy from every root
/// (an entity with [`Transform`] and no [`ChildOf`]) down through its
/// descendants.
///
/// A root's world transform equals its local [`Transform`]. Each child's
/// world transform is its parent's world transform composed with its own
/// local [`Transform`] via [`engine_utils::Transform::mul_transform`].
///
/// # Panics
///
/// Does not panic on well-formed hierarchies. A cycle in the `ChildOf`
/// graph (which nothing in this engine can currently construct — scenes
/// have no hierarchy support yet, and this is the only writer of
/// `ChildOf`-driven traversal) would recurse until the stack overflows;
/// cycle detection is future work if a path to creating one ever exists.
pub fn propagate_transforms(world: &mut World) {
    let mut roots_query = world.query_filtered::<Entity, (With<Transform>, Without<ChildOf>)>();
    let roots: Vec<Entity> = roots_query.iter(world).collect();

    for root in roots {
        propagate_recursive(world, root, engine_utils::Transform::IDENTITY);
    }
}

fn propagate_recursive(world: &mut World, entity: Entity, parent_global: engine_utils::Transform) {
    let local = world
        .get::<Transform>(entity)
        .map(|t| t.0)
        .unwrap_or_default();
    let global = parent_global.mul_transform(&local);

    if let Some(mut existing) = world.get_mut::<GlobalTransform>(entity) {
        existing.0 = global;
    } else {
        world.entity_mut(entity).insert(GlobalTransform(global));
    }

    let Some(children) = world.get::<Children>(entity) else {
        return;
    };
    let children: Vec<Entity> = children.iter().copied().collect();
    for child in children {
        propagate_recursive(world, child, global);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::hierarchy::ChildOf;
    use glam::Vec3;

    #[test]
    fn root_global_transform_equals_local_transform() {
        let mut world = World::new();
        let local = engine_utils::Transform::from_translation(Vec3::new(1.0, 2.0, 3.0));
        let entity = world.spawn(Transform(local)).id();

        propagate_transforms(&mut world);

        assert_eq!(world.get::<GlobalTransform>(entity).unwrap().0, local);
    }

    #[test]
    fn child_global_transform_is_composed_with_parent() {
        let mut world = World::new();
        let parent = world
            .spawn(Transform(engine_utils::Transform::from_translation(
                Vec3::new(10.0, 0.0, 0.0),
            )))
            .id();
        let child = world
            .spawn((
                Transform(engine_utils::Transform::from_translation(Vec3::new(
                    1.0, 0.0, 0.0,
                ))),
                ChildOf(parent),
            ))
            .id();

        propagate_transforms(&mut world);

        assert_eq!(
            world.get::<GlobalTransform>(child).unwrap().0.translation,
            Vec3::new(11.0, 0.0, 0.0)
        );
    }

    #[test]
    fn grandchild_composes_through_two_ancestors() {
        let mut world = World::new();
        let grandparent = world
            .spawn(Transform(engine_utils::Transform::from_translation(
                Vec3::new(100.0, 0.0, 0.0),
            )))
            .id();
        let parent = world
            .spawn((
                Transform(engine_utils::Transform::from_translation(Vec3::new(
                    10.0, 0.0, 0.0,
                ))),
                ChildOf(grandparent),
            ))
            .id();
        let child = world
            .spawn((
                Transform(engine_utils::Transform::from_translation(Vec3::new(
                    1.0, 0.0, 0.0,
                ))),
                ChildOf(parent),
            ))
            .id();

        propagate_transforms(&mut world);

        assert_eq!(
            world.get::<GlobalTransform>(child).unwrap().0.translation,
            Vec3::new(111.0, 0.0, 0.0)
        );
    }

    #[test]
    fn child_without_own_transform_still_inherits_parent_global() {
        let mut world = World::new();
        let parent = world
            .spawn(Transform(engine_utils::Transform::from_translation(
                Vec3::new(5.0, 0.0, 0.0),
            )))
            .id();
        // No `Transform` on the child at all — the root query wouldn't
        // find it, but the recursive walk from `parent` still reaches it
        // via `Children` and defaults its local transform to identity.
        let child = world.spawn(ChildOf(parent)).id();

        propagate_transforms(&mut world);

        assert_eq!(
            world.get::<GlobalTransform>(child).unwrap().0.translation,
            Vec3::new(5.0, 0.0, 0.0)
        );
    }

    #[test]
    fn re_running_propagate_updates_existing_global_transform() {
        let mut world = World::new();
        let entity = world
            .spawn(Transform(engine_utils::Transform::from_translation(
                Vec3::new(1.0, 0.0, 0.0),
            )))
            .id();

        propagate_transforms(&mut world);
        assert_eq!(
            world.get::<GlobalTransform>(entity).unwrap().0.translation,
            Vec3::new(1.0, 0.0, 0.0)
        );

        world.get_mut::<Transform>(entity).unwrap().0.translation = Vec3::new(9.0, 0.0, 0.0);
        propagate_transforms(&mut world);

        assert_eq!(
            world.get::<GlobalTransform>(entity).unwrap().0.translation,
            Vec3::new(9.0, 0.0, 0.0)
        );
    }

    #[test]
    fn entity_with_no_transform_at_all_is_untouched() {
        let mut world = World::new();
        let entity = world.spawn_empty().id();

        propagate_transforms(&mut world);

        assert!(world.get::<GlobalTransform>(entity).is_none());
    }
}
