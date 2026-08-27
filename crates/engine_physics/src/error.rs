//! Errors produced by `engine_physics`.

use glam::{Vec2, Vec3};
use thiserror::Error;

/// Errors that can occur while driving the physics simulation.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PhysicsError {
    /// [`crate::PhysicsWorld::step`] was called with a negative, `NaN`, or
    /// infinite timestep.
    #[error("invalid physics timestep: {0} (must be finite and non-negative)")]
    InvalidTimestep(f32),

    /// [`crate::move_character`] was called with a non-finite
    /// `desired_translation` (a `NaN`/infinite component).
    #[error("invalid character movement translation: {0:?} (all components must be finite)")]
    InvalidTranslation(Vec3),

    /// [`crate::move_character`] was called with a `body` or `collider`
    /// handle that doesn't exist in the [`crate::PhysicsWorld`] (e.g. it
    /// was already despawned).
    #[error("character's rigid body or collider handle no longer exists in this physics world")]
    UnknownCharacterHandle,

    /// [`crate::cast_ray`] was called with a non-finite `origin`, a
    /// non-finite or zero-length `direction`, or a negative/non-finite
    /// `max_distance`.
    #[error(
        "invalid ray cast: origin={origin:?} direction={direction:?} max_distance={max_distance} \
         (origin/direction must be finite, direction non-zero, max_distance finite and non-negative)"
    )]
    InvalidRay {
        /// The ray's requested origin.
        origin: Vec3,
        /// The ray's requested direction (before normalization).
        direction: Vec3,
        /// The requested maximum cast distance.
        max_distance: f32,
    },

    /// [`crate::move_character_2d`] was called with a non-finite
    /// `desired_translation` (a `NaN`/infinite component).
    #[error("invalid 2D character movement translation: {0:?} (all components must be finite)")]
    InvalidTranslation2D(Vec2),
}
