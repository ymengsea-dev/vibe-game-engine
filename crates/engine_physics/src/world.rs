//! Physics world: owns rapier3d's simulation state and provides a
//! validated stepping entry point.

use glam::Vec3;
use rapier3d::pipeline::PhysicsWorld as RapierWorld;

use crate::error::PhysicsError;

/// Owns a rapier3d physics simulation: rigid bodies, colliders, joints,
/// and the broad-phase/narrow-phase/island/CCD machinery rapier bundles
/// into [`rapier3d::pipeline::PhysicsWorld`].
///
/// `rapier`'s fields are all `pub` (rapier's own design) — reach in
/// directly for anything this thin wrapper doesn't add convenience for
/// (spawning bodies/colliders, running queries, ...; that surface grows
/// with the next iterations — colliders/rigid bodies, character
/// controller, ray casting). [`PhysicsWorld::step`] is the one thing worth
/// wrapping here: rapier's own `step()` takes no arguments and reads a
/// fixed `dt` out of `integration_parameters`, which doesn't fit an engine
/// loop that hands in a real per-frame `dt` each call — and rapier doesn't
/// validate it (a `NaN`/negative `dt` would silently corrupt the
/// simulation rather than erroring).
///
/// No conversion layer to/from this crate's other types: as of rapier3d
/// 0.35, `rapier3d::math::Vector`/`Rotation` are literal type aliases for
/// `glam::Vec3`/`Quat` (the same `glam` version this workspace already
/// pins), so a [`glam::Vec3`] computed anywhere else in the engine is
/// already the type rapier's APIs expect.
pub struct PhysicsWorld {
    /// The underlying rapier3d simulation state.
    pub rapier: RapierWorld,
}

impl PhysicsWorld {
    /// A new physics world with the given gravity and otherwise-default
    /// rapier parameters (solver iterations, timestep, ...).
    pub fn new(gravity: Vec3) -> Self {
        let mut rapier = RapierWorld::new();
        rapier.gravity = gravity;
        Self { rapier }
    }

    /// Advances the simulation by `dt` seconds.
    ///
    /// `dt` of exactly `0.0` is a valid no-op (a paused simulation still
    /// gets stepped by callers each frame) — only a negative, `NaN`, or
    /// infinite `dt` is rejected.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsError::InvalidTimestep`] if `dt` is negative,
    /// `NaN`, or infinite, without stepping the simulation.
    pub fn step(&mut self, dt: f32) -> Result<(), PhysicsError> {
        if !dt.is_finite() || dt < 0.0 {
            return Err(PhysicsError::InvalidTimestep(dt));
        }
        self.rapier.integration_parameters.dt = dt;
        self.rapier.step();
        Ok(())
    }
}

impl Default for PhysicsWorld {
    /// Gravity `(0, -9.81, 0)` — [`rapier3d::pipeline::PhysicsWorld`]'s own
    /// default.
    fn default() -> Self {
        Self {
            rapier: RapierWorld::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_sets_gravity() {
        let world = PhysicsWorld::new(Vec3::new(0.0, -20.0, 0.0));
        assert_eq!(world.rapier.gravity, Vec3::new(0.0, -20.0, 0.0));
    }

    #[test]
    fn default_uses_rapiers_own_default_gravity() {
        let world = PhysicsWorld::default();
        assert_eq!(world.rapier.gravity, RapierWorld::default().gravity);
    }

    #[test]
    fn step_succeeds_on_an_empty_world() {
        let mut world = PhysicsWorld::default();
        for _ in 0..10 {
            assert!(world.step(1.0 / 60.0).is_ok());
        }
    }

    #[test]
    fn step_updates_integration_parameters_dt() {
        let mut world = PhysicsWorld::default();
        world.step(0.5).unwrap();
        assert_eq!(world.rapier.integration_parameters.dt, 0.5);
    }

    #[test]
    fn step_accepts_zero_dt_as_a_noop() {
        let mut world = PhysicsWorld::default();
        assert!(world.step(0.0).is_ok());
        assert_eq!(world.rapier.integration_parameters.dt, 0.0);
    }

    #[test]
    fn step_rejects_negative_dt() {
        let mut world = PhysicsWorld::default();
        let err = world.step(-0.1).unwrap_err();
        assert!(matches!(err, PhysicsError::InvalidTimestep(dt) if dt == -0.1));
    }

    #[test]
    fn step_rejects_nan_dt() {
        let mut world = PhysicsWorld::default();
        assert!(world.step(f32::NAN).is_err());
    }

    #[test]
    fn step_rejects_infinite_dt() {
        let mut world = PhysicsWorld::default();
        assert!(world.step(f32::INFINITY).is_err());
        assert!(world.step(f32::NEG_INFINITY).is_err());
    }

    #[test]
    fn failed_step_does_not_mutate_integration_parameters_dt() {
        let mut world = PhysicsWorld::default();
        world.step(1.0 / 30.0).unwrap();
        let _ = world.step(f32::NAN);
        assert_eq!(world.rapier.integration_parameters.dt, 1.0 / 30.0);
    }
}
