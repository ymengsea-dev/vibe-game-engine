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

/// An entity that renders as a mesh: a handle to its geometry, a handle to
/// its material binding, and the per-object GPU binding that lets its
/// sibling [`Transform`] component actually move it (updated each frame by
/// [`crate::render::extract_and_render`]).
///
/// Holds asset handles rather than the GPU resources themselves —
/// [`engine_renderer::RenderAssets`] is where the actual [`engine_renderer::Mesh`]/
/// [`engine_renderer::MaterialBinding`] live, resolved from `mesh`/`material`
/// each frame. This is what lets multiple entities share one mesh/material
/// for the cost of one GPU upload instead of one per entity — see
/// [`MeshRenderer::with_mesh`]/[`MeshRenderer::from_handles`]. `model` stays
/// a direct, unshared [`engine_renderer::ModelBinding`] — a model matrix is
/// inherently per-entity data, never shareable the way geometry or a
/// material binding can be.
///
/// Despawn a `MeshRenderer` entity through
/// [`crate::render::despawn_mesh_renderer`], not a bare `World::despawn` —
/// that's what releases `mesh`/`material`'s ref counts in whatever
/// [`engine_renderer::RenderAssets`] they were registered in.
#[derive(Component)]
pub struct MeshRenderer {
    /// A handle to this entity's geometry, resolved through a
    /// [`engine_renderer::RenderAssets`].
    pub mesh: engine_utils::AssetHandle<engine_renderer::Mesh>,
    /// A handle to this entity's material binding (base color texture +
    /// PBR metallic-roughness factors), resolved through a
    /// [`engine_renderer::RenderAssets`].
    pub material: engine_utils::AssetHandle<engine_renderer::MaterialBinding>,
    /// This entity's model-matrix binding, rewritten from the sibling
    /// [`GlobalTransform`] component each frame.
    pub model: engine_renderer::ModelBinding,
}

impl MeshRenderer {
    /// Builds a [`MeshRenderer`] straight from decoded geometry and an
    /// optional base-color image.
    ///
    /// The shared "an imported asset just became a renderable entity"
    /// path: the editor resolves references against its import cache, the
    /// standalone player against an asset bundle, and both end up here so
    /// the upload rules (and the no-texture fallback) are defined once.
    ///
    /// `base_color` is `(width, height, rgba8)`. `None` uploads a 1x1
    /// opaque white pixel instead, so a model whose source carried no
    /// image still renders lit rather than vanishing — sampling white
    /// leaves `material`'s own `base_color_factor` untouched.
    ///
    /// # Errors
    ///
    /// [`engine_renderer::RendererError`] if the geometry is empty or the
    /// mesh upload fails.
    pub fn from_geometry(
        gpu: &engine_renderer::GpuContext,
        pipeline: &engine_renderer::Pipeline,
        assets: &mut engine_renderer::RenderAssets,
        vertices: &[engine_renderer::Vertex],
        indices: &[u32],
        base_color: Option<(u32, u32, &[u8])>,
        material: engine_renderer::Material,
    ) -> Result<Self, engine_renderer::RendererError> {
        const LABEL: &str = "scene mesh";
        let mesh = gpu.create_mesh(LABEL, vertices, indices)?;
        let texture = match base_color {
            Some((width, height, rgba8)) => {
                gpu.create_texture_from_rgba(LABEL, width, height, rgba8)
            }
            None => gpu.create_texture_from_rgba(LABEL, 1, 1, &[255, 255, 255, 255]),
        };
        Ok(Self::new(gpu, pipeline, assets, mesh, &texture, material))
    }

    /// Builds a [`MeshRenderer`], uploading `mesh` and building a material
    /// binding from `texture`/`material`, then registering both as freshly
    /// owned entries (ref count `1` each) in `assets`.
    ///
    /// Use this for an entity whose geometry and appearance are both new —
    /// [`MeshRenderer::with_mesh`]/[`MeshRenderer::from_handles`] instead
    /// reuse an existing entry when another entity already registered one.
    pub fn new(
        gpu: &engine_renderer::GpuContext,
        pipeline: &engine_renderer::Pipeline,
        assets: &mut engine_renderer::RenderAssets,
        mesh: engine_renderer::Mesh,
        texture: &engine_renderer::Texture,
        material: engine_renderer::Material,
    ) -> Self {
        let material_binding = gpu.create_material_binding(pipeline, texture, &material.into());
        let model_binding =
            gpu.create_model_binding(pipeline, &engine_renderer::ModelUniform::IDENTITY);
        Self {
            mesh: assets.meshes.insert(mesh),
            material: assets.materials.insert(material_binding),
            model: model_binding,
        }
    }

    /// Builds a [`MeshRenderer`] that reuses an already-registered `mesh`
    /// (bumping its ref count) while still uploading a fresh material
    /// binding from `texture`/`material` — for an entity that shares
    /// geometry with an existing one but not its appearance.
    ///
    /// Logs a warning (via `tracing`) and proceeds anyway if `mesh` isn't
    /// registered in `assets` — the resulting `MeshRenderer` simply won't
    /// draw anything until a valid handle replaces it (see
    /// [`crate::render::extract_and_render`]'s missing-asset handling),
    /// rather than panicking on what's likely a caller bug.
    pub fn with_mesh(
        gpu: &engine_renderer::GpuContext,
        pipeline: &engine_renderer::Pipeline,
        assets: &mut engine_renderer::RenderAssets,
        mesh: engine_utils::AssetHandle<engine_renderer::Mesh>,
        texture: &engine_renderer::Texture,
        material: engine_renderer::Material,
    ) -> Self {
        if !assets.meshes.retain(mesh) {
            tracing::warn!(
                ?mesh,
                "MeshRenderer::with_mesh given an unregistered mesh handle"
            );
        }
        let material_binding = gpu.create_material_binding(pipeline, texture, &material.into());
        let model_binding =
            gpu.create_model_binding(pipeline, &engine_renderer::ModelUniform::IDENTITY);
        Self {
            mesh,
            material: assets.materials.insert(material_binding),
            model: model_binding,
        }
    }

    /// Builds a [`MeshRenderer`] that reuses both an already-registered
    /// `mesh` and `material` (bumping both ref counts) — for an entity
    /// that's visually identical to an existing one, differing only by its
    /// sibling [`Transform`].
    ///
    /// Logs a warning and proceeds for each handle not found in `assets`,
    /// same as [`MeshRenderer::with_mesh`].
    pub fn from_handles(
        gpu: &engine_renderer::GpuContext,
        pipeline: &engine_renderer::Pipeline,
        assets: &mut engine_renderer::RenderAssets,
        mesh: engine_utils::AssetHandle<engine_renderer::Mesh>,
        material: engine_utils::AssetHandle<engine_renderer::MaterialBinding>,
    ) -> Self {
        if !assets.meshes.retain(mesh) {
            tracing::warn!(
                ?mesh,
                "MeshRenderer::from_handles given an unregistered mesh handle"
            );
        }
        if !assets.materials.retain(material) {
            tracing::warn!(
                ?material,
                "MeshRenderer::from_handles given an unregistered material handle"
            );
        }
        let model_binding =
            gpu.create_model_binding(pipeline, &engine_renderer::ModelUniform::IDENTITY);
        Self {
            mesh,
            material,
            model: model_binding,
        }
    }
}

/// An entity that renders as a GPU-skinned mesh: a handle to its skinned
/// geometry, a handle to its material binding, and the per-object
/// `@group(2)` binding (model matrix + joint matrices) that lets its
/// sibling [`Transform`] move it and a caller-supplied pose deform it.
///
/// The skinned counterpart to [`MeshRenderer`]. `mesh`/`material` resolve
/// against a caller-owned [`engine_renderer::RenderAssets`] each frame the
/// same way; `skin` stays a direct, unshared
/// [`engine_renderer::SkinnedBinding`] because both a model matrix and a
/// joint-matrix set are inherently per-entity.
///
/// [`crate::render::extract_and_render`] rewrites `skin`'s model buffer
/// from [`GlobalTransform`] each frame. The joint matrices are the
/// *caller's* job to upload (via
/// [`engine_renderer::GpuContext::write_uniform_buffer`] into
/// `skin.joints_buffer`) — this crate has no dependency on
/// `engine_animation`, so it neither samples poses nor computes skinning
/// matrices.
///
/// Despawn through [`crate::render::despawn_skinned_mesh_renderer`], not a
/// bare `World::despawn`, to release `mesh`/`material`'s ref counts.
#[derive(Component)]
pub struct SkinnedMeshRenderer {
    /// A handle to this entity's skinned geometry, resolved through a
    /// [`engine_renderer::RenderAssets`].
    pub mesh: engine_utils::AssetHandle<engine_renderer::SkinnedMesh>,
    /// A handle to this entity's material binding, resolved through a
    /// [`engine_renderer::RenderAssets`].
    pub material: engine_utils::AssetHandle<engine_renderer::MaterialBinding>,
    /// This entity's `@group(2)` binding: model-matrix uniform (rewritten
    /// from [`GlobalTransform`] each frame by
    /// [`crate::render::extract_and_render`]) plus joint-matrices uniform
    /// (uploaded by the caller from a sampled pose).
    pub skin: engine_renderer::SkinnedBinding,
}

impl SkinnedMeshRenderer {
    /// Builds a [`SkinnedMeshRenderer`], uploading `mesh` and building a
    /// material binding from `texture`/`material`, then registering both
    /// as freshly owned entries (ref count `1` each) in `assets`.
    ///
    /// The joint matrices start at identity ([`engine_renderer::JointMatricesUniform::IDENTITY`])
    /// — the mesh renders in its bind pose until the caller uploads a real
    /// pose.
    pub fn new(
        gpu: &engine_renderer::GpuContext,
        pipeline: &engine_renderer::Pipeline,
        skinned_pipeline: &engine_renderer::SkinnedPipeline,
        assets: &mut engine_renderer::RenderAssets,
        mesh: engine_renderer::SkinnedMesh,
        texture: &engine_renderer::Texture,
        material: engine_renderer::Material,
    ) -> Self {
        let material_binding = gpu.create_material_binding(pipeline, texture, &material.into());
        let skin = gpu.create_skinned_binding(
            skinned_pipeline,
            &engine_renderer::ModelUniform::IDENTITY,
            &engine_renderer::JointMatricesUniform::IDENTITY,
        );
        Self {
            mesh: assets.skinned_meshes.insert(mesh),
            material: assets.materials.insert(material_binding),
            skin,
        }
    }
}

/// An entity that renders many copies of one mesh in a single instanced
/// draw call: a handle to the shared geometry, a handle to the shared
/// material, and one world-space [`engine_utils::Transform`] per copy.
///
/// Unlike [`MeshRenderer`], there is no per-object GPU binding — the model
/// matrices come from an instance buffer
/// ([`engine_renderer::GpuContext::create_instance_buffer`]) that
/// [`crate::render::extract_and_render`] rebuilds each frame from the
/// frustum-visible subset of `instances`. The entity's own `Transform` is
/// not applied; `instances` are already world-space.
///
/// Despawn through [`crate::render::despawn_instanced_mesh_renderer`] to
/// release the `mesh`/`material` ref counts.
#[derive(Component)]
pub struct InstancedMeshRenderer {
    /// A handle to the geometry every instance shares.
    pub mesh: engine_utils::AssetHandle<engine_renderer::Mesh>,
    /// A handle to the material binding every instance shares.
    pub material: engine_utils::AssetHandle<engine_renderer::MaterialBinding>,
    /// One world-space transform per instance.
    pub instances: Vec<engine_utils::Transform>,
}

impl InstancedMeshRenderer {
    /// Builds an [`InstancedMeshRenderer`], uploading `mesh` and building a
    /// material binding from `texture`/`material`, then registering both as
    /// freshly owned entries (ref count `1` each) in `assets`.
    pub fn new(
        gpu: &engine_renderer::GpuContext,
        pipeline: &engine_renderer::Pipeline,
        assets: &mut engine_renderer::RenderAssets,
        mesh: engine_renderer::Mesh,
        texture: &engine_renderer::Texture,
        material: engine_renderer::Material,
        instances: Vec<engine_utils::Transform>,
    ) -> Self {
        let material_binding = gpu.create_material_binding(pipeline, texture, &material.into());
        Self {
            mesh: assets.meshes.insert(mesh),
            material: assets.materials.insert(material_binding),
            instances,
        }
    }

    /// Builds an [`InstancedMeshRenderer`] that reuses an
    /// already-registered `mesh` (bumping its ref count) while uploading a
    /// fresh material binding — for instancing geometry another entity
    /// already owns (e.g. the same `cube` mesh a [`MeshRenderer`] uses).
    ///
    /// Logs a warning and proceeds if `mesh` isn't registered in `assets`,
    /// same as [`MeshRenderer::with_mesh`].
    pub fn with_mesh(
        gpu: &engine_renderer::GpuContext,
        pipeline: &engine_renderer::Pipeline,
        assets: &mut engine_renderer::RenderAssets,
        mesh: engine_utils::AssetHandle<engine_renderer::Mesh>,
        texture: &engine_renderer::Texture,
        material: engine_renderer::Material,
        instances: Vec<engine_utils::Transform>,
    ) -> Self {
        if !assets.meshes.retain(mesh) {
            tracing::warn!(
                ?mesh,
                "InstancedMeshRenderer::with_mesh given an unregistered mesh handle"
            );
        }
        let material_binding = gpu.create_material_binding(pipeline, texture, &material.into());
        Self {
            mesh,
            material: assets.materials.insert(material_binding),
            instances,
        }
    }
}

/// An entity that renders many wind-animated plants in one instanced draw:
/// a handle to the shared blade/frond geometry, a handle to the shared
/// material, and one world-space [`engine_utils::Transform`] per plant.
///
/// The vegetation counterpart to [`InstancedMeshRenderer`] — same
/// per-instance frustum culling and per-frame instance buffer, but
/// [`crate::render::extract_and_render`] draws it through
/// `engine_renderer::VegetationPipeline` (whose vertex stage bends each
/// vertex along a global wind, pivoting at object-space `y = 0`). The mesh
/// should therefore be authored with its base at the origin. The entity's
/// own `Transform` is not applied; `instances` are already world-space.
///
/// Despawn through [`crate::render::despawn_vegetation_renderer`] to
/// release the `mesh`/`material` ref counts.
#[derive(Component)]
pub struct VegetationRenderer {
    /// A handle to the geometry every plant shares.
    pub mesh: engine_utils::AssetHandle<engine_renderer::Mesh>,
    /// A handle to the material binding every plant shares.
    pub material: engine_utils::AssetHandle<engine_renderer::MaterialBinding>,
    /// One world-space transform per plant.
    pub instances: Vec<engine_utils::Transform>,
}

impl VegetationRenderer {
    /// Builds a [`VegetationRenderer`], uploading `mesh` and building a
    /// material binding from `texture`/`material`, then registering both as
    /// freshly owned entries (ref count `1` each) in `assets`.
    pub fn new(
        gpu: &engine_renderer::GpuContext,
        pipeline: &engine_renderer::Pipeline,
        assets: &mut engine_renderer::RenderAssets,
        mesh: engine_renderer::Mesh,
        texture: &engine_renderer::Texture,
        material: engine_renderer::Material,
        instances: Vec<engine_utils::Transform>,
    ) -> Self {
        let material_binding = gpu.create_material_binding(pipeline, texture, &material.into());
        Self {
            mesh: assets.meshes.insert(mesh),
            material: assets.materials.insert(material_binding),
            instances,
        }
    }

    /// Builds a [`VegetationRenderer`] that reuses an already-registered
    /// `mesh` (bumping its ref count) while uploading a fresh material
    /// binding.
    ///
    /// Logs a warning and proceeds if `mesh` isn't registered in `assets`,
    /// same as [`MeshRenderer::with_mesh`].
    pub fn with_mesh(
        gpu: &engine_renderer::GpuContext,
        pipeline: &engine_renderer::Pipeline,
        assets: &mut engine_renderer::RenderAssets,
        mesh: engine_utils::AssetHandle<engine_renderer::Mesh>,
        texture: &engine_renderer::Texture,
        material: engine_renderer::Material,
        instances: Vec<engine_utils::Transform>,
    ) -> Self {
        if !assets.meshes.retain(mesh) {
            tracing::warn!(
                ?mesh,
                "VegetationRenderer::with_mesh given an unregistered mesh handle"
            );
        }
        let material_binding = gpu.create_material_binding(pipeline, texture, &material.into());
        Self {
            mesh,
            material: assets.materials.insert(material_binding),
            instances,
        }
    }
}

/// An entity that renders as a 2D sprite: its world-space size, which
/// region of a shared atlas it samples, and its tint.
///
/// Unlike [`MeshRenderer`], this holds no GPU resources of its own — a
/// sprite's whole point is sharing one atlas texture and one draw call
/// across every sprite entity ([`crate::render::extract_sprites`] builds
/// one `SpriteBatch` from every `Sprite` each frame), so there's nothing
/// per-entity to upload. No future "becomes an asset handle" rework debt
/// the way `MeshRenderer` has.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct Sprite {
    /// World-space width/height.
    pub size: glam::Vec2,
    /// The atlas region this sprite samples.
    pub uv: engine_renderer::UvRect,
    /// Linear RGBA tint, multiplied into the sampled texture color.
    pub color: [f32; 4],
    /// Explicit 2D layer. Higher draws on top; the primary sort key, so
    /// layering is a property of the sprite rather than of spawn order.
    ///
    /// Sprites sharing a `z_order` fall back to back-to-front view depth,
    /// which is what a 3D scene's billboards want — leave this at `0.0`
    /// and depth alone decides. A 2D game sets it and it decides.
    pub z_order: f32,
}

impl Sprite {
    /// A sprite `size` world units across, sampling `uv`, with no tint
    /// (white) on layer `0.0`.
    pub fn new(size: glam::Vec2, uv: engine_renderer::UvRect) -> Self {
        Self {
            size,
            uv,
            color: [1.0, 1.0, 1.0, 1.0],
            z_order: 0.0,
        }
    }

    /// This sprite moved to layer `z_order`. Chainable after
    /// [`Sprite::new`].
    pub fn with_z_order(mut self, z_order: f32) -> Self {
        self.z_order = z_order;
        self
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

/// A reference to the asset file an entity was created from (e.g.
/// dragged into the scene from the editor's asset browser).
///
/// Carries both a project-relative `path` (human-readable, and the
/// fallback when nothing else resolves) and an optional stable `id` —
/// the UUID from the asset's `.meta` sidecar
/// (`engine_asset::AssetMeta`), stored as its canonical text. The `id`
/// is what lets the reference survive a rename or move of the source
/// file: the editor re-resolves `id` to the current path on scene load.
///
/// Display / bookkeeping only for now — nothing loads or renders from
/// it yet. Round-trips through `engine_scene::SceneEntity`'s
/// `asset_source` (path) and `asset_id` fields.
#[derive(Component, Debug, Clone, PartialEq, Eq, Default)]
pub struct AssetSource {
    /// Project-relative path to the source file.
    pub path: String,
    /// Canonical UUID text of the asset's `.meta` id, once known.
    pub id: Option<String>,
}

/// A project-relative Rust script attached to an entity.
///
/// The editor owns script discovery and compilation; the component is kept
/// deliberately small so scenes can carry the authoring reference without
/// pulling a scripting runtime into the core ECS.
#[derive(Component, Debug, Clone, PartialEq, Eq, Default)]
pub struct Script {
    /// Path relative to the project root (normally under `src/`).
    pub path: String,
}

impl Script {
    /// Builds a script reference from a project-relative path.
    pub fn new(path: impl Into<String>) -> Self {
        Self { path: path.into() }
    }
}

impl AssetSource {
    /// Builds an [`AssetSource`] from a path, with no id yet.
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            id: None,
        }
    }

    /// Builds an [`AssetSource`] from a path and a known id (canonical
    /// UUID text).
    pub fn with_id(path: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            id: Some(id.into()),
        }
    }
}

/// The asset references a sibling [`MeshRenderer`] was built from.
///
/// A live `MeshRenderer` holds GPU handles, not asset ids, so it cannot
/// be serialized back out on its own — the ids have to be carried
/// alongside it. This component is that carrier: it round-trips through
/// `engine_scene::SceneEntity::mesh_renderer`, and is what lets a saved
/// scene reproduce its geometry on the next load.
///
/// Held as canonical UUID *text* rather than a typed reference because
/// `engine_scene` already depends on this crate — the reverse dependency
/// needed to name its `AssetRef` here would be a cycle.
///
/// Present whether or not the reference actually resolved: an entity
/// whose mesh file is missing still carries its ids, so opening and
/// re-saving a scene never silently deletes the reference.
#[derive(Component, Debug, Clone, PartialEq, Eq, Default)]
pub struct MeshSource {
    /// Canonical UUID text of the mesh asset.
    pub mesh: String,
    /// Canonical UUID text of the material asset.
    pub material: String,
}

impl MeshSource {
    /// Builds a [`MeshSource`] from a mesh and material id.
    pub fn new(mesh: impl Into<String>, material: impl Into<String>) -> Self {
        Self {
            mesh: mesh.into(),
            material: material.into(),
        }
    }
}

/// The atlas reference a sibling [`Sprite`] was built from, plus the
/// on-disk size and tint.
///
/// The same carrier role [`MeshSource`] plays for meshes. `size`/`color`
/// are duplicated here rather than read back off the live [`Sprite`]
/// because a sprite whose atlas failed to resolve has no live `Sprite`
/// component at all — without these, an unresolved sprite would lose its
/// dimensions on the next save.
///
/// When a live [`Sprite`] *is* present it takes precedence on capture, so
/// Inspector edits are never overwritten by the values stored here.
#[derive(Component, Debug, Clone, PartialEq, Default)]
pub struct SpriteSource {
    /// Canonical UUID text of the atlas texture asset.
    pub atlas: String,
    /// The named (or gridded, e.g. `"0_0"`) region within the atlas.
    pub region: String,
    /// World-space width/height as stored on disk.
    pub size: [f32; 2],
    /// Linear RGBA tint as stored on disk.
    pub color: [f32; 4],
    /// Layer as stored on disk. See [`Sprite::z_order`].
    pub z_order: f32,
}

/// Marks an entity as disabled in the editor: the Scene view skips it and
/// the hierarchy panel greys it out. A view-only marker for now — it
/// does not stop simulation or scripts. Round-trips through
/// `engine_scene::SceneEntity::disabled`.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Disabled;

/// Marks an entity as static — the editor treats it as non-moving. A
/// view-only marker for now (a hook for later culling / batching); it
/// has no runtime effect yet. Round-trips through
/// `engine_scene::SceneEntity::is_static`.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Static;

/// Marks an entity as locked in the editor: the Scene-view gizmo, batch
/// transform, and hierarchy reparent-drag all skip it, so it can't be
/// moved or re-parented by accident. View-only — no runtime effect.
/// Round-trips through `engine_scene::SceneEntity::locked`.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Lock;

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
    fn with_z_order_sets_the_layer() {
        let uv = engine_renderer::UvRect {
            min: [0.0, 0.0],
            max: [1.0, 1.0],
        };
        let sprite = Sprite::new(glam::Vec2::ONE, uv).with_z_order(-2.0);
        assert_eq!(sprite.z_order, -2.0);
        assert_eq!(Sprite::new(glam::Vec2::ONE, uv).z_order, 0.0);
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

    #[test]
    fn asset_source_carries_a_path_and_optional_id() {
        let bare = AssetSource::new("props/barrel.gltf");
        assert_eq!(bare.path, "props/barrel.gltf");
        assert_eq!(bare.id, None);

        let identified =
            AssetSource::with_id("props/barrel.gltf", "3fa00000-0000-0000-0000-000000000000");
        assert_eq!(identified.path, "props/barrel.gltf");
        assert_eq!(
            identified.id.as_deref(),
            Some("3fa00000-0000-0000-0000-000000000000")
        );
        assert_eq!(
            AssetSource::default(),
            AssetSource {
                path: String::new(),
                id: None
            }
        );
    }
}
