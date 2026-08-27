//! Kinematic 2D character movement: axis-separated AABB collision
//! resolution against static platform geometry — the 2D counterpart to
//! [`crate::move_character`], built on [`crate::Aabb2d`]/
//! [`crate::aabb_vs_aabb`] rather than rapier (no `rapier2d` dependency;
//! see [`crate::collision2d`]'s module docs for why this crate stays off
//! `rapier2d` until a validation game proves it's needed).
//!
//! **Algorithm and its trade-offs:** resolves movement one axis at a time
//! — move on X, clamp against every overlapping platform, then move on Y
//! (using the already-resolved X) and clamp again. This is the standard
//! simple-platformer technique (fast, easy to reason about) but not a
//! full swept solver, in two specific ways:
//!
//! - **Discrete, not continuous.** Only the *final* candidate position is
//!   tested against each platform — a `desired_translation` large enough
//!   to land past a thin platform entirely (rather than inside it) will
//!   tunnel straight through undetected. Safe for the normal case (small
//!   per-frame steps — `velocity * dt` at real frame rates — against
//!   platforms at least as thick as one frame's worth of movement); not
//!   safe for arbitrarily large single-call movements.
//! - **Corner cases.** Resolving X fully before Y (using the pre-Y
//!   position) can leave a small residual overlap in tight configurations
//!   (e.g. squeezing into a gap between two platforms placed diagonally)
//!   that a true continuous solver wouldn't.
//!
//! Both are acceptable at "not full `rapier2d`" scope — flagged here
//! rather than hidden.

use glam::Vec2;

use crate::PhysicsError;
use crate::collision2d::{Aabb2d, aabb_vs_aabb};

/// The result of applying [`move_character_2d`]'s movement for one frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CharacterMovement2D {
    /// The movement actually applied, after collision response — may
    /// differ from the requested `desired_translation` (e.g. blocked by a
    /// wall or stopped by landing on a platform).
    pub translation: Vec2,
    /// Whether the character is touching a platform's top surface after
    /// this movement. The caller's cue for whether e.g. a jump input
    /// should be honored, or downward velocity should keep accumulating —
    /// the same role [`crate::CharacterMovement::grounded`] plays in 3D.
    pub grounded: bool,
}

/// Moves a character (an axis-aligned box, `half_extents` wide/tall
/// around `position`) by `desired_translation` this frame, resolving
/// collisions against `platforms` axis-by-axis (see the module docs for
/// the algorithm and its corner-case trade-off).
///
/// `desired_translation` is the raw movement this call should attempt —
/// e.g. `input_direction * speed * dt`, plus any accumulated gravity/jump
/// velocity the caller is tracking; this function has no gravity or input
/// policy of its own, only collision-aware movement resolution — the same
/// contract [`crate::move_character`] has in 3D.
///
/// `half_extents`/`platforms` aren't validated (same "garbage in, garbage
/// out, never panics" precedent [`crate::Aabb2d`] itself already sets) —
/// only `desired_translation`, the actual per-call movement input, is
/// checked.
///
/// # Errors
///
/// Returns [`PhysicsError::InvalidTranslation2D`] if `desired_translation`
/// has a `NaN`/infinite component.
pub fn move_character_2d(
    position: Vec2,
    half_extents: Vec2,
    desired_translation: Vec2,
    platforms: &[Aabb2d],
) -> Result<CharacterMovement2D, PhysicsError> {
    if !desired_translation.is_finite() {
        return Err(PhysicsError::InvalidTranslation2D(desired_translation));
    }

    let resolved_x = resolve_x(position, half_extents, desired_translation.x, platforms);
    let position_after_x = Vec2::new(resolved_x, position.y);
    let (resolved_y, grounded) = resolve_y(
        position_after_x,
        half_extents,
        desired_translation.y,
        platforms,
    );
    let resolved_position = Vec2::new(resolved_x, resolved_y);

    Ok(CharacterMovement2D {
        translation: resolved_position - position,
        grounded,
    })
}

/// Resolves horizontal movement: starts at the fully-desired `x`, then for
/// every platform the character's box (at the candidate `x`, original `y`)
/// overlaps, clamps back to that platform's near edge. Re-testing with the
/// progressively-clamped `x` each iteration means multiple overlapping
/// platforms converge to the nearest valid stop, not just the last one
/// checked.
fn resolve_x(position: Vec2, half_extents: Vec2, desired_x: f32, platforms: &[Aabb2d]) -> f32 {
    let mut x = position.x + desired_x;
    if desired_x == 0.0 {
        return x;
    }
    for platform in platforms {
        let candidate = Aabb2d {
            center: Vec2::new(x, position.y),
            half_extents,
        };
        if aabb_vs_aabb(&candidate, platform) {
            x = if desired_x > 0.0 {
                x.min(platform.min().x - half_extents.x)
            } else {
                x.max(platform.max().x + half_extents.x)
            };
        }
    }
    x
}

/// Resolves vertical movement the same way [`resolve_x`] resolves
/// horizontal — plus reports `grounded`: true whenever a non-positive
/// (falling or resting) vertical move is stopped by landing on a
/// platform's top surface.
fn resolve_y(
    position: Vec2,
    half_extents: Vec2,
    desired_y: f32,
    platforms: &[Aabb2d],
) -> (f32, bool) {
    let mut y = position.y + desired_y;
    let mut grounded = false;
    for platform in platforms {
        let candidate = Aabb2d {
            center: Vec2::new(position.x, y),
            half_extents,
        };
        if aabb_vs_aabb(&candidate, platform) {
            if desired_y > 0.0 {
                y = y.min(platform.min().y - half_extents.y);
            } else {
                y = y.max(platform.max().y + half_extents.y);
                grounded = true;
            }
        }
    }
    (y, grounded)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HALF_EXTENTS: Vec2 = Vec2::new(0.3, 0.5);

    fn ground_platform() -> Aabb2d {
        Aabb2d {
            center: Vec2::new(0.0, -1.0),
            half_extents: Vec2::new(10.0, 0.5),
        }
    }

    #[test]
    fn open_ground_applies_the_full_desired_translation() {
        let movement =
            move_character_2d(Vec2::new(0.0, 5.0), HALF_EXTENTS, Vec2::new(1.0, 0.0), &[]).unwrap();
        assert_eq!(movement.translation, Vec2::new(1.0, 0.0));
        assert!(!movement.grounded);
    }

    #[test]
    fn falling_onto_a_platform_stops_at_its_surface_and_reports_grounded() {
        // Character's feet at y - 0.5; ground top surface at y = -0.5, so
        // starting at y = 0.0 and falling 2.0 units should stop exactly at
        // y = 0.0 (feet resting on the surface), not pass through.
        let platform = ground_platform();
        let movement = move_character_2d(
            Vec2::new(0.0, 0.0),
            HALF_EXTENTS,
            Vec2::new(0.0, -2.0),
            &[platform],
        )
        .unwrap();
        assert!(movement.grounded);
        assert!((movement.translation.y - 0.0).abs() < 1e-4);
    }

    #[test]
    fn resting_on_a_platform_with_no_vertical_input_still_reports_grounded() {
        let platform = ground_platform();
        // Already resting exactly on the surface (center y = 0.0, feet at
        // -0.5, matching the platform's top).
        let movement = move_character_2d(
            Vec2::new(0.0, 0.0),
            HALF_EXTENTS,
            Vec2::new(1.0, 0.0),
            &[platform],
        )
        .unwrap();
        assert!(movement.grounded);
    }

    #[test]
    fn jumping_up_through_open_air_is_not_grounded() {
        let platform = ground_platform();
        let movement = move_character_2d(
            Vec2::new(0.0, 0.0),
            HALF_EXTENTS,
            Vec2::new(0.0, 3.0),
            &[platform],
        )
        .unwrap();
        assert!(!movement.grounded);
        assert!((movement.translation.y - 3.0).abs() < 1e-4);
    }

    #[test]
    fn jumping_into_a_ceiling_is_blocked_and_not_grounded() {
        // Discrete (not swept) resolution: the requested move must land
        // the character's box *inside* the platform to be caught (see the
        // module docs' tunneling caveat) — 2.0 does that (ceiling spans
        // y = 1.5..2.5, requested y = 2.0 lands inside it); a much larger
        // request would tunnel straight through instead, same as the 3D
        // controller's own broad-phase-only guarantees.
        let ceiling = Aabb2d {
            center: Vec2::new(0.0, 2.0),
            half_extents: Vec2::new(10.0, 0.5),
        };
        let movement = move_character_2d(
            Vec2::new(0.0, 0.0),
            HALF_EXTENTS,
            Vec2::new(0.0, 2.0),
            &[ceiling],
        )
        .unwrap();
        // Ceiling's bottom surface at y = 1.5; character's head (half
        // extent 0.5) should stop with its center at y = 1.0.
        assert!((movement.translation.y - 1.0).abs() < 1e-4);
        assert!(!movement.grounded);
    }

    #[test]
    fn movement_into_a_wall_is_blocked_not_passed_through() {
        // Same discrete-resolution caveat as the ceiling test above: 1.0
        // lands the character's box inside the wall (spans x = 0.5..1.5).
        let wall = Aabb2d {
            center: Vec2::new(1.0, 0.0),
            half_extents: Vec2::new(0.5, 5.0),
        };
        let movement =
            move_character_2d(Vec2::ZERO, HALF_EXTENTS, Vec2::new(1.0, 0.0), &[wall]).unwrap();
        // Wall's left face at x = 0.5; character's right edge (half extent
        // 0.3) should stop with its center at x = 0.2.
        assert!((movement.translation.x - 0.2).abs() < 1e-4);
    }

    #[test]
    fn movement_away_from_a_wall_is_unobstructed() {
        let wall = Aabb2d {
            center: Vec2::new(1.0, 0.0),
            half_extents: Vec2::new(0.5, 5.0),
        };
        let movement =
            move_character_2d(Vec2::ZERO, HALF_EXTENTS, Vec2::new(-5.0, 0.0), &[wall]).unwrap();
        assert_eq!(movement.translation, Vec2::new(-5.0, 0.0));
    }

    #[test]
    fn rejects_nan_translation() {
        let err =
            move_character_2d(Vec2::ZERO, HALF_EXTENTS, Vec2::new(f32::NAN, 0.0), &[]).unwrap_err();
        assert!(matches!(err, PhysicsError::InvalidTranslation2D(_)));
    }

    #[test]
    fn rejects_infinite_translation() {
        let err = move_character_2d(Vec2::ZERO, HALF_EXTENTS, Vec2::new(0.0, f32::INFINITY), &[])
            .unwrap_err();
        assert!(matches!(err, PhysicsError::InvalidTranslation2D(_)));
    }

    #[test]
    fn zero_translation_is_a_harmless_no_op() {
        let movement = move_character_2d(
            Vec2::new(3.0, 4.0),
            HALF_EXTENTS,
            Vec2::ZERO,
            &[ground_platform()],
        )
        .unwrap();
        assert_eq!(movement.translation, Vec2::ZERO);
    }
}
