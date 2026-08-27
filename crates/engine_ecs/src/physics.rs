//! Bridges physics simulation results into the ECS: copies each
//! [`RigidBody`]-driven entity's simulated world pose from a
//! [`PhysicsWorld`] back into its [`Transform`] component.

use bevy_ecs::world::World;
use engine_physics::PhysicsWorld;

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
/// # Parenting caveat
///
/// Physics bodies are inherently world-space — rapier has no concept of
/// the ECS's parent/child hierarchy — so this only produces correct
/// results for entities *without* a `ChildOf` parent (or whose parent is
/// always at the identity transform). A physics-driven child entity would
/// have its local `Transform` overwritten with what is really a world
/// pose, then have the parent's transform composed on top of it again by
/// `propagate_transforms`, double-applying the parent's offset.
/// Parent-relative physics bodies are future work once there's a concrete
/// need for them (e.g. vehicles with physics-driven wheels).
///
/// Scale is left untouched — rapier rigid bodies have no scale, only
/// position and orientation.
pub fn sync_rigid_bodies(world: &mut World, physics: &PhysicsWorld) {
    let mut query = world.query::<(&RigidBody, &mut Transform)>();
    for (rigid_body, mut transform) in query.iter_mut(world) {
        let Some(body) = physics.rapier.bodies.get(rigid_body.0) else {
            continue;
        };
        transform.0.translation = body.translation();
        transform.0.rotation = *body.rotation();
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
