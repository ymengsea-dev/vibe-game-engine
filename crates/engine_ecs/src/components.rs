//! Core ECS components: [`Transform`], [`Camera`], [`MeshRenderer`],
//! [`Sprite`], [`RigidBody`], [`Collider`].
//!
//! Each wraps a plain data type (or, for physics, a lightweight `Copy`
//! handle) from another crate (`engine_utils`, `engine_renderer`,
//! `engine_physics`) in a newtype rather than deriving `Component`
//! directly on that type. This isn't a style choice — Rust's orphan rule
//! forbids implementing a foreign trait (`bevy_ecs::component::Component`)
//! for a foreign type (defined in another crate), so a locally-defined
//! wrapper is the only way to make these types usable as components here.
//! [`Sprite`] doesn't need this — it isn't a wrapper around a single
//! foreign type, just plain per-instance draw data (size/UV/tint), so it's
//! defined directly here.
//!
//! Not re-exported through `engine::prelude`: `engine_renderer::Camera`
//! (the plain math type) is already there, and re-exporting this crate's
//! `Camera` too would collide on the name. Reach these via
//! `engine::ecs::{Transform, Camera, MeshRenderer, Sprite, RigidBody,
//! Collider}` instead.

use bevy_ecs::prelude::Component;
use engine_physics::{
    ColliderBuilder, ColliderHandle, PhysicsWorld, RigidBodyBuilder, RigidBodyHandle,
};

/// An entity's position, rotation, and scale.
///
/// Wraps [`engine_utils::Transform`] — see that type for the actual
/// translation/rotation/scale fields and matrix/direction helpers.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct Transform(pub engine_utils::Transform);

impl Default for Transform {
    /// [`engine_utils::Transform::IDENTITY`].
    fn default() -> Self {
        Self(engine_utils::Transform::IDENTITY)
    }
}

impl From<engine_utils::Transform> for Transform {
    fn from(transform: engine_utils::Transform) -> Self {
        Self(transform)
    }
}

/// An entity's resolved *world-space* transform, computed by
/// [`crate::propagate::propagate_transforms`] from its own [`Transform`]
/// (parent-relative if the entity has a `ChildOf`, world-space otherwise)
/// composed with its ancestors' transforms.
///
/// Read-only from user code's perspective — always overwritten on the
/// next propagation pass, so treat any value set by hand as transient.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct GlobalTransform(pub engine_utils::Transform);

impl Default for GlobalTransform {
    /// [`engine_utils::Transform::IDENTITY`].
    fn default() -> Self {
        Self(engine_utils::Transform::IDENTITY)
    }
}

impl From<engine_utils::Transform> for GlobalTransform {
    fn from(transform: engine_utils::Transform) -> Self {
        Self(transform)
    }
}

/// An entity that views the scene through a projection.
///
/// Wraps [`engine_renderer::Camera`] — see that type for the actual
/// eye/target/fov/near/far fields and view/projection matrix methods.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct Camera(pub engine_renderer::Camera);

impl From<engine_renderer::Camera> for Camera {
    fn from(camera: engine_renderer::Camera) -> Self {
        Self(camera)
    }
}

/// An entity that renders as a mesh: its geometry, texture, and the
/// per-object GPU binding that lets its sibling [`Transform`] component
/// actually move it (updated each frame by [`crate::render::extract_and_render`]).
///
/// Holds GPU resources directly rather than through an asset handle —
/// there's no asset system yet (Milestone 5). Expect `mesh`/`material` to
/// become handles once one exists; storing them directly works for now
/// since nothing needs to share mesh/texture data across entities yet.
#[derive(Component)]
pub struct MeshRenderer {
    /// This entity's geometry.
    pub mesh: engine_renderer::Mesh,
    /// This entity's material binding (base color texture + PBR
    /// metallic-roughness factors).
    pub material: engine_renderer::MaterialBinding,
    /// This entity's model-matrix binding, rewritten from the sibling
    /// [`GlobalTransform`] component each frame.
    pub model: engine_renderer::ModelBinding,
}

impl MeshRenderer {
    /// Builds a [`MeshRenderer`], creating the material and (identity)
    /// model bindings `mesh`/`texture`/`material` need to actually draw.
    pub fn new(
        gpu: &engine_renderer::GpuContext,
        pipeline: &engine_renderer::Pipeline,
        mesh: engine_renderer::Mesh,
        texture: &engine_renderer::Texture,
        material: engine_renderer::Material,
    ) -> Self {
        let material_binding = gpu.create_material_binding(pipeline, texture, &material.into());
        let model_binding =
            gpu.create_model_binding(pipeline, &engine_renderer::ModelUniform::IDENTITY);
        Self {
            mesh,
            material: material_binding,
            model: model_binding,
        }
    }
}

/// An entity that renders as a 2D sprite: its world-space size, which
/// region of a shared atlas it samples, and its tint.
///
/// Unlike [`MeshRenderer`], this holds no GPU resources of its own — a
/// sprite's whole point is sharing one atlas texture and one draw call
/// across every sprite entity ([`crate::render::extract_and_render_sprites`]
/// builds one `SpriteBatch` from every `Sprite` each frame), so there's
/// nothing per-entity to upload. No future "becomes an asset handle"
/// rework debt the way `MeshRenderer` has.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct Sprite {
    /// World-space width/height.
    pub size: glam::Vec2,
    /// The atlas region this sprite samples.
    pub uv: engine_renderer::UvRect,
    /// Linear RGBA tint, multiplied into the sampled texture color.
    pub color: [f32; 4],
}

impl Sprite {
    /// A sprite `size` world units across, sampling `uv`, with no tint
    /// (white).
    pub fn new(size: glam::Vec2, uv: engine_renderer::UvRect) -> Self {
        Self {
            size,
            uv,
            color: [1.0, 1.0, 1.0, 1.0],
        }
    }
}

/// A human-readable label for an entity — display only (e.g. the
/// editor's hierarchy panel); has no effect on simulation or rendering.
///
/// Unlike [`Transform`]/[`Camera`]/[`MeshRenderer`], this doesn't wrap a
/// foreign type — a plain `String` newtype, since nothing outside this
/// crate needs to define what a "name" is.
#[derive(Component, Debug, Clone, PartialEq, Eq, Default)]
pub struct Name(pub String);

impl Name {
    /// Builds a [`Name`] from anything that converts to a `String`.
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }
}

impl std::fmt::Display for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for Name {
    fn from(name: &str) -> Self {
        Self(name.to_string())
    }
}

impl From<String> for Name {
    fn from(name: String) -> Self {
        Self(name)
    }
}

/// An entity with a simulated rigid body in a
/// [`engine_physics::PhysicsWorld`] — a handle into that world's
/// `RigidBodySet`, not the simulation state itself (which stays owned by
/// the `PhysicsWorld` the caller's app loop already holds, the same
/// arrangement [`MeshRenderer`] has with `GpuContext`).
///
/// Each frame, [`crate::physics::sync_rigid_bodies`] copies this body's
/// simulated world-space translation/rotation into the sibling
/// [`Transform`] component — see that function's docs for the parenting
/// caveat (physics bodies are always world-space; a physics-driven entity
/// that's also a `ChildOf` gets its parent's transform double-applied).
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct RigidBody(pub RigidBodyHandle);

/// An entity with a collision shape in a [`engine_physics::PhysicsWorld`]
/// — a handle into that world's `ColliderSet`. See [`RigidBody`] for why
/// this holds a handle rather than the collider itself.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Collider(pub ColliderHandle);

impl RigidBody {
    /// Inserts `rigid_body` and `collider` into `physics` as one attached
    /// pair (rapier's own [`PhysicsWorld::rapier`]'s `insert` — the
    /// collider moves with the body) and returns the `(RigidBody,
    /// Collider)` component pair referencing them.
    ///
    /// # Example
    ///
    /// ```
    /// use engine_ecs::components::RigidBody;
    /// use engine_physics::{ColliderBuilder, PhysicsWorld, RigidBodyBuilder};
    /// use glam::Vec3;
    ///
    /// let mut physics = PhysicsWorld::default();
    /// let (body, collider) = RigidBody::spawn(
    ///     &mut physics,
    ///     RigidBodyBuilder::dynamic().translation(Vec3::new(0.0, 5.0, 0.0)),
    ///     ColliderBuilder::cuboid(0.5, 0.5, 0.5),
    /// );
    /// assert!(physics.rapier.bodies.get(body.0).is_some());
    /// assert!(physics.rapier.colliders.get(collider.0).is_some());
    /// ```
    pub fn spawn(
        physics: &mut PhysicsWorld,
        rigid_body: RigidBodyBuilder,
        collider: ColliderBuilder,
    ) -> (RigidBody, Collider) {
        let (body_handle, collider_handle) = physics.rapier.insert(rigid_body, collider);
        (RigidBody(body_handle), Collider(collider_handle))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::world::World;
    use engine_utils::Transform as MathTransform;
    use glam::Vec3;

    #[test]
    fn transform_default_is_identity() {
        assert_eq!(Transform::default().0, MathTransform::IDENTITY);
    }

    #[test]
    fn transform_from_math_transform_round_trips() {
        let math = MathTransform::from_translation(Vec3::new(1.0, 2.0, 3.0));
        let wrapped: Transform = math.into();
        assert_eq!(wrapped.0, math);
    }

    #[test]
    fn global_transform_default_is_identity() {
        assert_eq!(GlobalTransform::default().0, MathTransform::IDENTITY);
    }

    #[test]
    fn global_transform_from_math_transform_round_trips() {
        let math = MathTransform::from_translation(Vec3::new(4.0, 5.0, 6.0));
        let wrapped: GlobalTransform = math.into();
        assert_eq!(wrapped.0, math);
    }

    #[test]
    fn camera_from_renderer_camera_round_trips() {
        let camera = engine_renderer::Camera::new(Vec3::new(0.0, 1.0, 5.0), Vec3::ZERO, 1.5);
        let wrapped: Camera = camera.into();
        assert_eq!(wrapped.0, camera);
    }

    #[test]
    fn transform_and_camera_spawn_and_query_together() {
        let mut world = World::new();
        let math = MathTransform::from_translation(Vec3::new(5.0, 0.0, 0.0));
        let camera = engine_renderer::Camera::new(Vec3::ZERO, Vec3::ZERO, 1.0);
        world.spawn((Transform::from(math), Camera::from(camera)));

        let mut query = world.query::<(&Transform, &Camera)>();
        let (transform, queried_camera) = query.single(&world).unwrap();
        assert_eq!(transform.0.translation, Vec3::new(5.0, 0.0, 0.0));
        assert_eq!(queried_camera.0.aspect_ratio, 1.0);
    }

    #[test]
    fn query_finds_no_entities_when_none_spawned() {
        let mut world = World::new();
        let mut query = world.query::<&Transform>();
        assert!(query.single(&world).is_err());
    }

    #[test]
    fn name_new_wraps_the_given_string() {
        assert_eq!(Name::new("Player").0, "Player");
    }

    #[test]
    fn name_default_is_empty() {
        assert_eq!(Name::default().0, "");
    }

    #[test]
    fn name_display_matches_inner_string() {
        assert_eq!(Name::new("Camera").to_string(), "Camera");
    }

    #[test]
    fn name_from_str_and_string_round_trip() {
        assert_eq!(Name::from("Light"), Name::new("Light"));
        assert_eq!(Name::from(String::from("Light")), Name::new("Light"));
    }

    #[test]
    fn name_spawns_and_queries_as_a_component() {
        let mut world = World::new();
        world.spawn(Name::new("Root"));

        let mut query = world.query::<&Name>();
        let name = query.single(&world).unwrap();
        assert_eq!(name.0, "Root");
    }

    #[test]
    fn rigid_body_spawn_inserts_a_body_and_an_attached_collider() {
        let mut physics = PhysicsWorld::default();
        let (body, collider) = RigidBody::spawn(
            &mut physics,
            RigidBodyBuilder::dynamic().translation(Vec3::new(0.0, 5.0, 0.0)),
            ColliderBuilder::cuboid(0.5, 0.5, 0.5),
        );
        assert!(physics.rapier.bodies.get(body.0).is_some());
        assert!(physics.rapier.colliders.get(collider.0).is_some());
    }

    #[test]
    fn rigid_body_and_collider_spawn_and_query_together() {
        let mut physics = PhysicsWorld::default();
        let (body, collider) = RigidBody::spawn(
            &mut physics,
            RigidBodyBuilder::fixed(),
            ColliderBuilder::ball(1.0),
        );

        let mut world = World::new();
        world.spawn((body, collider));

        let mut query = world.query::<(&RigidBody, &Collider)>();
        let (queried_body, queried_collider) = query.single(&world).unwrap();
        assert_eq!(*queried_body, body);
        assert_eq!(*queried_collider, collider);
    }

    #[test]
    fn sprite_new_has_white_tint() {
        let uv = engine_renderer::UvRect {
            min: [0.0, 0.0],
            max: [1.0, 1.0],
        };
        let sprite = Sprite::new(glam::Vec2::new(2.0, 3.0), uv);
        assert_eq!(sprite.size, glam::Vec2::new(2.0, 3.0));
        assert_eq!(sprite.uv, uv);
        assert_eq!(sprite.color, [1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn sprite_spawns_and_queries_as_a_component() {
        let mut world = World::new();
        let uv = engine_renderer::UvRect {
            min: [0.25, 0.0],
            max: [0.5, 1.0],
        };
        world.spawn(Sprite::new(glam::Vec2::splat(1.0), uv));

        let mut query = world.query::<&Sprite>();
        let sprite = query.single(&world).unwrap();
        assert_eq!(sprite.uv, uv);
    }
}
