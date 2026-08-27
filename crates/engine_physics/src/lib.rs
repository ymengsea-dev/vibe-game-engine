//! # engine_physics
//!
//! Physics simulation via rapier: collision detection, rigid bodies, character controller, and ray casting.
//!
//! ## Status
//!
//! Milestone 7 in progress: [`PhysicsWorld`] wraps rapier3d's simulation
//! state (bodies, colliders, joints, broad/narrow phase, islands, CCD —
//! see [`rapier3d::pipeline::PhysicsWorld`]) with a validated
//! [`PhysicsWorld::step`] entry point. Rapier speaks `glam` natively as of
//! 0.35 (`rapier3d::math::Vector`/`Rotation` are literal aliases for
//! `glam::Vec3`/`Quat`, the same `glam` version this workspace already
//! pins), so no conversion layer is needed between this crate and the
//! rest of the engine. [`RigidBodyBuilder`]/[`ColliderBuilder`] (and the
//! [`RigidBodyHandle`]/[`ColliderHandle`] they produce via
//! [`PhysicsWorld::rapier`]'s `insert`) are re-exported for
//! `engine_ecs`'s rigid-body/collider components. [`move_character`]
//! drives a `KinematicPositionBased` body through rapier's own
//! [`CharacterController`] for collision-aware player/NPC movement.
//! [`cast_ray`] queries the world for the closest collider along a ray
//! (hit-scan weapons, picking, line-of-sight, ground checks).
//! [`debug_render_lines`] extracts collider/body/joint wireframes (via
//! rapier's own `debug-render` feature) as plain line segments — no
//! rendering dependency of this crate's own; turning them into
//! GPU-drawable geometry is `engine_renderer`'s job. Milestone 7 is
//! complete.
//!
//! Stage 2 "2D collision": [`Aabb2d`]/[`Circle2d`] boolean overlap checks
//! ([`aabb_vs_aabb`], [`circle_vs_circle`], [`aabb_vs_circle`]) plus
//! [`Collider2d`] for dispatching over either shape — plain `glam::Vec2`
//! math, deliberately separate from the rapier3d simulation above (no
//! rigid bodies, no simulation step). See the `collision2d` module docs
//! for why this exists instead of `rapier2d`.
//!
//! Stage 2 "Validation Game 2": [`move_character_2d`] is the 2D
//! counterpart to [`move_character`] — axis-separated AABB collision
//! resolution against static platform geometry, built on `collision2d`
//! rather than rapier. See the `character2d` module docs for its
//! algorithm and documented trade-offs.

mod character;
mod character2d;
mod collision2d;
mod debug;
mod error;
mod raycast;
mod world;

pub use character::{CharacterController, CharacterMovement, move_character};
pub use character2d::{CharacterMovement2D, move_character_2d};
pub use collision2d::{
    Aabb2d, Circle2d, Collider2d, aabb_vs_aabb, aabb_vs_circle, circle_vs_circle,
};
pub use debug::{DebugLine, DebugRenderMode, DebugRenderStyle, debug_render_lines};
pub use error::PhysicsError;
pub use raycast::{RayHit, cast_ray};
pub use world::PhysicsWorld;

// Re-exported so downstream crates (e.g. engine_ecs's rigid-body/collider
// components) can build and reference rapier bodies/colliders without
// adding `rapier3d` as a direct dependency of their own — the same reason
// `engine_renderer` re-exports the `wgpu` handle types its bindings hold.
pub use rapier3d::prelude::{ColliderBuilder, ColliderHandle, RigidBodyBuilder, RigidBodyHandle};
