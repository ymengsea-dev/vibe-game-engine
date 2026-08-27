//! Kinematic character movement: a validated, per-frame entry point that
//! drives a `KinematicPositionBased` rigid body through rapier's own
//! [`CharacterController`] (slope climbing, step-up, ground-snapping,
//! sliding), so player/NPC movement gets correct collision response
//! without full rigid-body dynamics (forces/impulses would fight
//! scripted movement input rather than cooperate with it).

use glam::Vec3;
use rapier3d::prelude::{ColliderHandle, QueryFilter, RigidBodyHandle};

use crate::error::PhysicsError;
use crate::world::PhysicsWorld;

/// Configuration for [`move_character`] — how a character climbs slopes,
/// steps, snaps to the ground, and slides. A direct re-export of rapier's
/// own controller type; see its fields for what each setting does.
/// [`Default::default`] gives sane defaults (a small collision offset,
/// slope climbing up to 45°, snap-to-ground enabled, autostep disabled).
pub use rapier3d::control::KinematicCharacterController as CharacterController;

/// The result of applying [`move_character`]'s movement for one frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CharacterMovement {
    /// The movement actually applied, after collision response — may
    /// differ from the requested `desired_translation` (e.g. blocked or
    /// deflected by a wall or slope).
    pub translation: Vec3,
    /// Whether the character is touching the ground after this movement.
    /// The caller's cue for whether e.g. a jump input should be honored,
    /// or downward velocity should keep accumulating.
    pub grounded: bool,
}

/// Moves a `KinematicPositionBased` character body by `desired_translation`
/// this frame, resolving collisions/slopes/steps/stepping via
/// `controller`, and schedules the result on `body` (applied when
/// `physics` is next [`PhysicsWorld::step`]ped — rapier moves kinematic
/// bodies to their `next_kinematic_*` target during the step, not
/// immediately).
///
/// `desired_translation` is the raw movement this call should attempt —
/// e.g. `input_direction * speed * dt`, plus any accumulated gravity/jump
/// velocity the caller is tracking; this function has no gravity or input
/// policy of its own, only collision-aware movement resolution.
///
/// `collider` is excluded from `controller`'s own collision queries (a
/// character's shape must never treat itself as an obstacle); `body` must
/// be the `collider`'s attached rigid body.
///
/// Reads collision candidates from `physics`'s broad-phase, which only
/// refreshes during [`PhysicsWorld::step`] — colliders inserted since the
/// last step (including on the very first frame after spawning them)
/// won't be seen yet. Call [`PhysicsWorld::step`] at least once (`dt:
/// 0.0` is a valid no-op that still refreshes the broad-phase) before
/// relying on `move_character` to see newly spawned obstacles.
///
/// # Errors
///
/// - [`PhysicsError::InvalidTranslation`] if `desired_translation` has a
///   `NaN`/infinite component.
/// - [`PhysicsError::InvalidTimestep`] if `dt` is negative, `NaN`, or
///   infinite.
/// - [`PhysicsError::UnknownCharacterHandle`] if `body` or `collider`
///   doesn't exist in `physics`.
pub fn move_character(
    physics: &mut PhysicsWorld,
    controller: &CharacterController,
    body: RigidBodyHandle,
    collider: ColliderHandle,
    desired_translation: Vec3,
    dt: f32,
) -> Result<CharacterMovement, PhysicsError> {
    if !desired_translation.is_finite() {
        return Err(PhysicsError::InvalidTranslation(desired_translation));
    }
    if !dt.is_finite() || dt < 0.0 {
        return Err(PhysicsError::InvalidTimestep(dt));
    }

    let effective = {
        let collider_ref = physics
            .rapier
            .colliders
            .get(collider)
            .ok_or(PhysicsError::UnknownCharacterHandle)?;
        let body_ref = physics
            .rapier
            .bodies
            .get(body)
            .ok_or(PhysicsError::UnknownCharacterHandle)?;
        let current_pose = *body_ref.position();

        let queries = physics
            .rapier
            .query_pipeline_with_filter(QueryFilter::default().exclude_collider(collider));

        controller.move_shape(
            dt,
            &queries,
            collider_ref.shape(),
            &current_pose,
            desired_translation,
            |_collision| {},
        )
    };

    let Some(body_mut) = physics.rapier.bodies.get_mut(body) else {
        return Err(PhysicsError::UnknownCharacterHandle);
    };
    body_mut.set_next_kinematic_translation(body_mut.translation() + effective.translation);

    Ok(CharacterMovement {
        translation: effective.translation,
        grounded: effective.grounded,
    })
}

#[cfg(test)]
mod tests {
    use rapier3d::prelude::{ColliderBuilder, RigidBodyBuilder};

    use super::*;

    fn spawn_character(physics: &mut PhysicsWorld, at: Vec3) -> (RigidBodyHandle, ColliderHandle) {
        physics.rapier.insert(
            RigidBodyBuilder::kinematic_position_based().translation(at),
            ColliderBuilder::capsule_y(0.5, 0.3),
        )
    }

    #[test]
    fn moving_over_open_ground_applies_the_full_desired_translation() {
        let mut physics = PhysicsWorld::default();
        // Ground flush with the capsule's feet (capsule half-height 0.5 +
        // radius 0.3 = 0.8 below center) so the character starts grounded
        // but has nothing directly in front of it to collide with.
        physics.rapier.insert(
            RigidBodyBuilder::fixed().translation(Vec3::new(0.0, -0.9, 0.0)),
            ColliderBuilder::cuboid(10.0, 0.1, 10.0),
        );
        let (body, collider) = spawn_character(&mut physics, Vec3::new(0.0, 0.0, 0.0));
        // The broad-phase (which the character controller's queries read)
        // only updates during a step — a freshly inserted collider isn't
        // visible to `move_character` until at least one step has run.
        // `dt: 0.0` is a valid no-op step (see `PhysicsWorld::step`) that
        // still refreshes the broad-phase without moving anything.
        physics.step(0.0).unwrap();

        let controller = CharacterController::default();
        let movement = move_character(
            &mut physics,
            &controller,
            body,
            collider,
            Vec3::new(1.0, 0.0, 0.0),
            1.0 / 60.0,
        )
        .unwrap();

        assert!((movement.translation.x - 1.0).abs() < 1e-3);
        assert!(movement.grounded);
    }

    #[test]
    fn movement_into_a_wall_is_blocked_not_passed_through() {
        let mut physics = PhysicsWorld::default();
        physics.rapier.insert(
            RigidBodyBuilder::fixed().translation(Vec3::new(0.0, -1.0, 0.0)),
            ColliderBuilder::cuboid(10.0, 0.1, 10.0),
        );
        // A wall immediately in the character's path.
        physics.rapier.insert(
            RigidBodyBuilder::fixed().translation(Vec3::new(1.0, 0.0, 0.0)),
            ColliderBuilder::cuboid(0.1, 2.0, 10.0),
        );
        let (body, collider) = spawn_character(&mut physics, Vec3::new(0.0, 0.0, 0.0));
        // See the note in the test above: refresh the broad-phase before
        // querying against it.
        physics.step(0.0).unwrap();

        let controller = CharacterController::default();
        let movement = move_character(
            &mut physics,
            &controller,
            body,
            collider,
            Vec3::new(5.0, 0.0, 0.0),
            1.0 / 60.0,
        )
        .unwrap();

        // Requested 5 units into a wall ~0.6 units away (wall face at
        // x=0.9, character radius 0.3): the controller must stop it well
        // short of 5.
        assert!(movement.translation.x < 1.0);
    }

    #[test]
    fn rejects_nan_translation() {
        let mut physics = PhysicsWorld::default();
        let (body, collider) = spawn_character(&mut physics, Vec3::ZERO);
        let controller = CharacterController::default();
        let err = move_character(
            &mut physics,
            &controller,
            body,
            collider,
            Vec3::new(f32::NAN, 0.0, 0.0),
            1.0 / 60.0,
        )
        .unwrap_err();
        assert!(matches!(err, PhysicsError::InvalidTranslation(_)));
    }

    #[test]
    fn rejects_invalid_dt() {
        let mut physics = PhysicsWorld::default();
        let (body, collider) = spawn_character(&mut physics, Vec3::ZERO);
        let controller = CharacterController::default();
        let err = move_character(&mut physics, &controller, body, collider, Vec3::ZERO, -1.0)
            .unwrap_err();
        assert!(matches!(err, PhysicsError::InvalidTimestep(dt) if dt == -1.0));
    }

    #[test]
    fn unknown_body_handle_errors_instead_of_panicking() {
        let mut physics = PhysicsWorld::default();
        let (_body, collider) = spawn_character(&mut physics, Vec3::ZERO);
        let (other_body, _other_collider) = spawn_character(&mut physics, Vec3::new(5.0, 0.0, 0.0));
        physics.rapier.remove_body(other_body);

        let controller = CharacterController::default();
        let err = move_character(
            &mut physics,
            &controller,
            other_body,
            collider,
            Vec3::ZERO,
            1.0 / 60.0,
        )
        .unwrap_err();
        assert!(matches!(err, PhysicsError::UnknownCharacterHandle));
    }
}
