//! Bridges physics simulation results into the ECS: copies each
//! [`RigidBody`]-driven entity's simulated world pose from a
//! [`PhysicsWorld`] back into its [`Transform`] component.

use std::collections::HashMap;

use bevy_ecs::{hierarchy::ChildOf, prelude::Entity, world::World};
use engine_physics::PhysicsWorld;
use engine_utils::Transform as MathTransform;

use crate::components::{RigidBody, Transform};

/// For every `(RigidBody, &mut Transform)` entity, overwrites `Transform`
/// with that rigid body's current simulated translation/rotation from
/// `physics`.
///
/// Call this after [`PhysicsWorld::step`] and before
/// [`crate::extract_and_render`] each frame — `extract_and_render` runs
/// [`crate::propagate_transforms`] internally, which needs the
/// just-synced `Transform` to compute a correct `GlobalTransform` for any
/// children this frame.
///
/// A stale [`RigidBody`] handle (its body was removed from `physics`,
/// e.g. by an unrelated despawn from `physics.rapier` directly) is
/// skipped rather than treated as an error — the same "missing is a
/// valid transient state, not corruption" choice
/// [`crate::render::extract_and_render`] makes for its own queries.
///
/// Parented bodies convert Rapier's world pose into the parent's local
/// coordinates. The parent must have nonzero scale; otherwise the child is
/// left unchanged for that step. Parent poses are recomputed from local
/// transforms, since the cached world transform may predate this fixed step.
///
/// Scale is left untouched — rapier rigid bodies have no scale, only
/// position and orientation.
pub fn sync_rigid_bodies(world: &mut World, physics: &PhysicsWorld) {
    fn world_transform(
        world: &World,
        entity: Entity,
        cache: &mut HashMap<Entity, MathTransform>,
    ) -> Option<MathTransform> {
        if let Some(&transform) = cache.get(&entity) {
            return Some(transform);
        }
        world.get_entity(entity).ok()?;
        let local = world
            .get::<Transform>(entity)
            .map_or(MathTransform::IDENTITY, |t| t.0);
        let global = match world.get::<ChildOf>(entity) {
            Some(parent) => world_transform(world, parent.parent(), cache)?.mul_transform(&local),
            None => local,
        };
        cache.insert(entity, global);
        Some(global)
    }

    let mut parent_transforms = HashMap::new();
    let mut query = world.query::<(Entity, &RigidBody, Option<&ChildOf>)>();
    let poses: Vec<_> = query
        .iter(world)
        .filter_map(|(entity, rigid_body, child_of)| {
            let body = physics.rapier.bodies.get(rigid_body.0)?;
            let mut translation = body.translation();
            let mut rotation = *body.rotation();
            if let Some(child_of) = child_of {
                // shortcut: a physics-driven parent uses its last ECS pose; resolve its live
                // body pose when nested dynamic bodies become an authored workflow.
                let parent = world_transform(world, child_of.parent(), &mut parent_transforms)?;
                if parent.scale.abs().min_element() <= f32::EPSILON {
                    return None;
                }
                let inverse_rotation = parent.rotation.inverse();
                translation = inverse_rotation * (translation - parent.translation) / parent.scale;
                rotation = inverse_rotation * rotation;
            }
            Some((entity, translation, rotation))
        })
        .collect();
    for (entity, translation, rotation) in poses {
        if let Some(mut transform) = world.get_mut::<Transform>(entity) {
            transform.0.translation = translation;
            transform.0.rotation = rotation;
        }
    }
}

#[cfg(test)]
mod tests {
    use engine_physics::{ColliderBuilder, RigidBodyBuilder};
    use glam::Vec3;

    use super::*;
    use crate::components::RigidBody;

    #[test]
    fn sync_copies_simulated_translation_into_transform() {
        let mut physics = PhysicsWorld::default();
        let (body, _collider) = RigidBody::spawn(
            &mut physics,
            RigidBodyBuilder::dynamic().translation(Vec3::new(0.0, 10.0, 0.0)),
            ColliderBuilder::ball(0.5),
        );

        let mut world = World::new();
        let entity = world.spawn((body, Transform::default())).id();

        // Let gravity move the body before syncing.
        for _ in 0..10 {
            physics.step(1.0 / 60.0).unwrap();
        }

        sync_rigid_bodies(&mut world, &physics);

        let transform = world.get::<Transform>(entity).unwrap();
        // Falling under gravity: y should have decreased from 10.
        assert!(transform.0.translation.y < 10.0);
    }

    #[test]
    fn sync_keeps_parented_body_transform_parent_relative() {
        use crate::components::GlobalTransform;
        use bevy_ecs::hierarchy::ChildOf;

        let mut physics = PhysicsWorld::default();
        let (body, _collider) = RigidBody::spawn(
            &mut physics,
            RigidBodyBuilder::fixed().translation(Vec3::new(6.0, 2.0, 0.0)),
            ColliderBuilder::cuboid(1.0, 1.0, 0.05),
        );
        let mut world = World::new();
        let parent = world
            .spawn(Transform(engine_utils::Transform::from_translation(
                Vec3::new(5.0, 0.0, 0.0),
            )))
            .id();
        let child = world
            .spawn((
                body,
                Transform(engine_utils::Transform::from_translation(Vec3::new(
                    1.0, 2.0, 0.0,
                ))),
                ChildOf(parent),
            ))
            .id();
        crate::propagate_transforms(&mut world);
        assert_eq!(
            world.get::<GlobalTransform>(child).unwrap().0.translation,
            Vec3::new(6.0, 2.0, 0.0)
        );

        sync_rigid_bodies(&mut world, &physics);
        assert_eq!(
            world.get::<Transform>(child).unwrap().0.translation,
            Vec3::new(1.0, 2.0, 0.0)
        );
    }

    #[test]
    fn sync_uses_a_parents_current_local_transform_after_it_moves() {
        let mut physics = PhysicsWorld::default();
        let (body, _collider) = RigidBody::spawn(
            &mut physics,
            RigidBodyBuilder::fixed().translation(Vec3::new(6.0, 2.0, 0.0)),
            ColliderBuilder::cuboid(1.0, 1.0, 0.05),
        );
        let mut world = World::new();
        let parent = world
            .spawn(Transform(engine_utils::Transform::from_translation(
                Vec3::new(5.0, 0.0, 0.0),
            )))
            .id();
        let child = world
            .spawn((body, Transform::default(), ChildOf(parent)))
            .id();
        crate::propagate_transforms(&mut world);
        world.get_mut::<Transform>(parent).unwrap().0.translation.x = 4.0;

        sync_rigid_bodies(&mut world, &physics);

        assert_eq!(
            world.get::<Transform>(child).unwrap().0.translation,
            Vec3::new(2.0, 2.0, 0.0)
        );
    }

    #[test]
    fn sync_leaves_transform_untouched_for_a_stale_handle() {
        let mut physics = PhysicsWorld::default();
        let (body, _collider) = RigidBody::spawn(
            &mut physics,
            RigidBodyBuilder::dynamic(),
            ColliderBuilder::ball(0.5),
        );
        physics.rapier.remove_body(body.0);

        let mut world = World::new();
        let starting = Transform::from(engine_utils::Transform::from_translation(Vec3::new(
            1.0, 2.0, 3.0,
        )));
        let entity = world.spawn((body, starting)).id();

        sync_rigid_bodies(&mut world, &physics);

        let transform = world.get::<Transform>(entity).unwrap();
        assert_eq!(transform.0.translation, Vec3::new(1.0, 2.0, 3.0));
    }

    #[test]
    fn sync_with_no_rigid_body_entities_is_a_harmless_no_op() {
        let physics = PhysicsWorld::default();
        let mut world = World::new();
        world.spawn(Transform::default());

        sync_rigid_bodies(&mut world, &physics);
        // No panic, and the non-physics entity's Transform is untouched.
        let mut query = world.query::<&Transform>();
        assert_eq!(*query.single(&world).unwrap(), Transform::default());
    }
}
