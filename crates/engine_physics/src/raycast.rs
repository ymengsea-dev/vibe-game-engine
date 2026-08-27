//! Ray casting against a [`PhysicsWorld`]'s colliders: hit-scan weapons,
//! mouse/screen-space picking, line-of-sight checks, ground detection,
//! and the like.

use glam::Vec3;
use rapier3d::geometry::Ray;
use rapier3d::prelude::{ColliderHandle, QueryFilter};

use crate::error::PhysicsError;
use crate::world::PhysicsWorld;

/// The result of a ray hitting a collider: which one, how far along the
/// ray, the surface normal there, and the world-space hit point (a
/// convenience — `origin + direction.normalize() * distance`, computed
/// once so callers don't each redo it).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RayHit {
    /// The collider the ray struck.
    pub collider: ColliderHandle,
    /// Distance from `origin` to the hit point, along the normalized ray
    /// direction.
    pub distance: f32,
    /// The struck surface's normal at the hit point.
    pub normal: Vec3,
    /// The world-space hit point.
    pub point: Vec3,
}

/// Casts a ray from `origin` in `direction` (normalized internally — pass
/// any non-zero-length vector), returning the closest collider it hits
/// within `max_distance`, or `None` if nothing is hit.
///
/// `exclude`, if given, is skipped even if the ray would otherwise hit it
/// first — the common case being a hit-scan ray excluding the shooter's
/// own collider, the same way [`crate::move_character`] excludes the
/// character's own collider from its collision queries.
///
/// Colliders are treated as solid: a ray starting *inside* one hits it
/// immediately at distance `0.0`, rather than passing through to hit its
/// far side or something behind it.
///
/// Reads collision candidates from `physics`'s broad-phase, which only
/// refreshes during [`PhysicsWorld::step`] — colliders inserted since the
/// last step (including on the very first frame after spawning them)
/// won't be seen yet. Call [`PhysicsWorld::step`] at least once (`dt:
/// 0.0` is a valid no-op that still refreshes the broad-phase) before
/// relying on `cast_ray` to see newly spawned obstacles.
///
/// # Errors
///
/// Returns [`PhysicsError::InvalidRay`] if `origin` is non-finite,
/// `direction` is non-finite or zero-length, or `max_distance` is
/// negative, `NaN`, or infinite.
pub fn cast_ray(
    physics: &PhysicsWorld,
    origin: Vec3,
    direction: Vec3,
    max_distance: f32,
    exclude: Option<ColliderHandle>,
) -> Result<Option<RayHit>, PhysicsError> {
    let invalid = || PhysicsError::InvalidRay {
        origin,
        direction,
        max_distance,
    };
    if !origin.is_finite() {
        return Err(invalid());
    }
    let Some(unit_direction) = direction
        .is_finite()
        .then(|| direction.try_normalize())
        .flatten()
    else {
        return Err(invalid());
    };
    if !max_distance.is_finite() || max_distance < 0.0 {
        return Err(invalid());
    }

    let ray = Ray::new(origin, unit_direction);
    let filter = match exclude {
        Some(handle) => QueryFilter::default().exclude_collider(handle),
        None => QueryFilter::default(),
    };

    let hit = physics
        .rapier
        .cast_ray_and_get_normal(&ray, max_distance, true, filter)
        .map(|(collider, intersection)| RayHit {
            collider,
            distance: intersection.time_of_impact,
            normal: intersection.normal,
            point: origin + unit_direction * intersection.time_of_impact,
        });

    Ok(hit)
}

#[cfg(test)]
mod tests {
    use rapier3d::prelude::{ColliderBuilder, RigidBodyBuilder};

    use super::*;

    #[test]
    fn ray_straight_down_hits_the_ground() {
        let mut physics = PhysicsWorld::default();
        physics.rapier.insert(
            RigidBodyBuilder::fixed().translation(Vec3::new(0.0, -1.0, 0.0)),
            ColliderBuilder::cuboid(10.0, 0.1, 10.0),
        );
        physics.step(0.0).unwrap();

        let hit = cast_ray(&physics, Vec3::new(0.0, 5.0, 0.0), Vec3::NEG_Y, 100.0, None)
            .unwrap()
            .unwrap();

        // Ground top surface: translation.y (-1.0) + half-extent.y (0.1).
        assert!((hit.distance - 5.9).abs() < 1e-3);
        assert!((hit.normal - Vec3::Y).length() < 1e-3);
        assert!((hit.point - Vec3::new(0.0, -0.9, 0.0)).length() < 1e-3);
    }

    #[test]
    fn ray_with_nothing_in_range_misses() {
        let mut physics = PhysicsWorld::default();
        physics.rapier.insert(
            RigidBodyBuilder::fixed().translation(Vec3::new(0.0, -100.0, 0.0)),
            ColliderBuilder::cuboid(10.0, 0.1, 10.0),
        );
        physics.step(0.0).unwrap();

        let hit = cast_ray(&physics, Vec3::new(0.0, 5.0, 0.0), Vec3::NEG_Y, 10.0, None).unwrap();
        assert!(hit.is_none());
    }

    #[test]
    fn excluded_collider_is_skipped_in_favor_of_the_next_hit() {
        let mut physics = PhysicsWorld::default();
        let (_body, near) = physics.rapier.insert(
            RigidBodyBuilder::fixed().translation(Vec3::new(0.0, 2.0, 0.0)),
            ColliderBuilder::cuboid(1.0, 0.1, 1.0),
        );
        physics.rapier.insert(
            RigidBodyBuilder::fixed().translation(Vec3::new(0.0, -1.0, 0.0)),
            ColliderBuilder::cuboid(10.0, 0.1, 10.0),
        );
        physics.step(0.0).unwrap();

        // Without exclusion, the near platform is hit first.
        let hit = cast_ray(
            &physics,
            Vec3::new(0.0, 10.0, 0.0),
            Vec3::NEG_Y,
            100.0,
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(hit.collider, near);

        // Excluding it, the ray passes through to the ground below.
        let hit = cast_ray(
            &physics,
            Vec3::new(0.0, 10.0, 0.0),
            Vec3::NEG_Y,
            100.0,
            Some(near),
        )
        .unwrap()
        .unwrap();
        assert_ne!(hit.collider, near);
    }

    #[test]
    fn direction_does_not_need_to_be_pre_normalized() {
        let mut physics = PhysicsWorld::default();
        physics.rapier.insert(
            RigidBodyBuilder::fixed().translation(Vec3::new(0.0, -1.0, 0.0)),
            ColliderBuilder::cuboid(10.0, 0.1, 10.0),
        );
        physics.step(0.0).unwrap();

        // A deliberately non-unit direction vector.
        let hit = cast_ray(
            &physics,
            Vec3::new(0.0, 5.0, 0.0),
            Vec3::new(0.0, -3.0, 0.0),
            100.0,
            None,
        )
        .unwrap()
        .unwrap();
        assert!((hit.distance - 5.9).abs() < 1e-3);
    }

    #[test]
    fn rejects_non_finite_origin() {
        let physics = PhysicsWorld::default();
        let err = cast_ray(
            &physics,
            Vec3::new(f32::NAN, 0.0, 0.0),
            Vec3::NEG_Y,
            10.0,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, PhysicsError::InvalidRay { .. }));
    }

    #[test]
    fn rejects_zero_length_direction() {
        let physics = PhysicsWorld::default();
        let err = cast_ray(&physics, Vec3::ZERO, Vec3::ZERO, 10.0, None).unwrap_err();
        assert!(matches!(err, PhysicsError::InvalidRay { .. }));
    }

    #[test]
    fn rejects_non_finite_direction() {
        let physics = PhysicsWorld::default();
        let err = cast_ray(
            &physics,
            Vec3::ZERO,
            Vec3::new(f32::INFINITY, 0.0, 0.0),
            10.0,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, PhysicsError::InvalidRay { .. }));
    }

    #[test]
    fn rejects_negative_max_distance() {
        let physics = PhysicsWorld::default();
        let err = cast_ray(&physics, Vec3::ZERO, Vec3::NEG_Y, -1.0, None).unwrap_err();
        assert!(matches!(err, PhysicsError::InvalidRay { .. }));
    }

    #[test]
    fn rejects_nan_max_distance() {
        let physics = PhysicsWorld::default();
        let err = cast_ray(&physics, Vec3::ZERO, Vec3::NEG_Y, f32::NAN, None).unwrap_err();
        assert!(matches!(err, PhysicsError::InvalidRay { .. }));
    }
}
