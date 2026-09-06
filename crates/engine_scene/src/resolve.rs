//! Turning a scene's on-disk asset references into live renderable
//! components.
//!
//! ## Why this is a trait
//!
//! A [`SceneEntity`]'s renderable data is stored by reference — an
//! [`AssetRef`] holding a UUID ([`crate::format::MeshRendererData`],
//! [`crate::format::SpriteData`]). Turning one into a live
//! [`engine_ecs::components::MeshRenderer`] needs a decoded mesh, a
//! decoded texture, a [`engine_renderer::GpuContext`] to upload them
//! with, and a [`engine_renderer::RenderAssets`] to register them in.
//! This crate has none of those, and deliberately doesn't depend on
//! `engine_asset` (see [`AssetRef::parse_id`]) — so the lookup has to
//! happen one layer up, in whoever owns the import cache:
//!
//! - the editor, whose `AssetImporter` holds decoded assets by id, and
//! - the standalone player, which reads them out of an asset bundle.
//!
//! [`SceneResolver`] is the seam between them. [`Scene::instantiate`]
//! passes [`NullResolver`], which resolves nothing — the behaviour every
//! caller had before this trait existed.
//!
//! ## Failure is not fatal
//!
//! Scene files are untrusted input. A reference to a deleted, renamed,
//! or corrupt asset resolves to an error, and instantiation *continues* —
//! the entity spawns with its name, transform, and hierarchy intact, just
//! without geometry, and [`InstantiateReport::unresolved`] counts it. One
//! bad asset must never cost you the rest of the scene.
//!
//! [`Scene::instantiate`]: crate::Scene::instantiate
//! [`SceneEntity`]: crate::SceneEntity

use bevy_ecs::prelude::Entity;
use bevy_ecs::world::EntityWorldMut;
use engine_ecs::components::{
    AssetSource, Camera, Disabled, Lock, MeshRenderer, MeshSource, Name, Sprite, SpriteSource,
    Static, Transform,
};
use engine_renderer::UvRect;

use crate::error::SceneError;
use crate::format::{AssetRef, SceneEntity};

/// Resolves a scene's asset references into live renderable components.
///
/// Implemented by whoever owns decoded asset data and a GPU context:
/// the editor's asset importer, or the standalone player's bundle
/// reader. It is a trait rather than a function because `engine_scene`
/// has no GPU context and deliberately does not depend on
/// `engine_asset` — the lookup can only happen one layer up.
///
/// Both methods distinguish three outcomes:
///
/// - `Ok(Some(_))` — resolved; the component is inserted on the entity.
/// - `Ok(None)` — the reference is valid but the asset isn't available
///   yet (still importing, streamed in later). Not an error, not counted
///   as unresolved, no warning logged.
/// - `Err(_)` — the reference is broken (unknown id, wrong asset kind,
///   failed upload). Logged once and counted in
///   [`InstantiateReport::unresolved`]; instantiation continues.
pub trait SceneResolver {
    /// Resolves a mesh + material reference pair into a live renderer
    /// component.
    ///
    /// `&mut self` because implementors typically upload GPU resources on
    /// first use and cache the handles.
    ///
    /// # Errors
    ///
    /// [`SceneError::Resolve`] if either reference can't be resolved into
    /// a usable mesh/material.
    fn resolve_mesh(
        &mut self,
        mesh: &AssetRef,
        material: &AssetRef,
    ) -> Result<Option<MeshRenderer>, SceneError>;

    /// Resolves an atlas reference plus a named region into the UV
    /// rectangle that region occupies.
    ///
    /// Only the UVs are resolved here — a [`Sprite`]'s size and tint come
    /// from the scene data itself ([`crate::format::SpriteData`]), so an
    /// implementor never has to know about them.
    ///
    /// # Errors
    ///
    /// [`SceneError::Resolve`] if the atlas is unknown or has no such
    /// region.
    fn resolve_sprite_uv(
        &mut self,
        atlas: &AssetRef,
        region: &str,
    ) -> Result<Option<UvRect>, SceneError>;
}

/// A [`SceneResolver`] that resolves nothing, successfully.
///
/// Every reference reports `Ok(None)` ("not available"), so entities
/// spawn without renderable components and nothing is counted as
/// unresolved. This is exactly the behaviour [`crate::Scene::instantiate`]
/// had before [`SceneResolver`] existed, and is what that method still
/// uses — a scene loaded without a resolver is not *failing* to find its
/// assets, it simply isn't looking for them.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullResolver;

impl SceneResolver for NullResolver {
    fn resolve_mesh(
        &mut self,
        _mesh: &AssetRef,
        _material: &AssetRef,
    ) -> Result<Option<MeshRenderer>, SceneError> {
        Ok(None)
    }

    fn resolve_sprite_uv(
        &mut self,
        _atlas: &AssetRef,
        _region: &str,
    ) -> Result<Option<UvRect>, SceneError> {
        Ok(None)
    }
}

/// What one instantiation produced.
///
/// Returned by [`crate::Scene::instantiate_with_resolver`]. A non-zero
/// [`unresolved`](Self::unresolved) means the scene spawned, but some
/// entities are missing their geometry — surface it to the user (the
/// editor's Console, the player's log) rather than letting a silently
/// invisible entity look like a rendering bug.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstantiateReport {
    /// The spawned entities, in the scene's own entity order.
    pub spawned: Vec<Entity>,
    /// How many renderable references failed to resolve.
    ///
    /// Counts individual failed references, not entities: an entity whose
    /// mesh *and* sprite both fail adds two.
    pub unresolved: usize,
}

impl InstantiateReport {
    /// Whether every renderable reference in the scene resolved (or was
    /// reported as not-yet-available).
    pub fn is_fully_resolved(&self) -> bool {
        self.unresolved == 0
    }
}

/// Inserts one [`SceneEntity`]'s components onto an already-spawned
/// entity, resolving its renderable references through `resolver`.
///
/// The single shared implementation behind both
/// [`crate::Scene::instantiate_with_resolver`] and
/// [`crate::Prefab::instantiate_with_resolver`] — the two used to carry
/// divergent copies of this logic.
///
/// `parent` is deliberately not handled here: it's an index into an
/// owning [`crate::Scene`]'s entity list, which a lone
/// [`crate::Prefab`] has no equivalent of, so the caller wires it up.
///
/// Failed resolutions increment `unresolved` and log one warning; they
/// never abort.
pub(crate) fn insert_components(
    entity_mut: &mut EntityWorldMut<'_>,
    data: &SceneEntity,
    resolver: &mut dyn SceneResolver,
    unresolved: &mut usize,
) {
    if let Some(name) = &data.name {
        entity_mut.insert(Name::new(name.clone()));
    }
    if let Some(transform) = data.transform {
        entity_mut.insert(Transform::from(engine_utils::Transform::from(transform)));
    }
    if let Some(camera) = data.camera {
        entity_mut.insert(Camera::from(engine_renderer::Camera::from(camera)));
    }
    if data.asset_source.is_some() || data.asset_id.is_some() {
        entity_mut.insert(AssetSource {
            path: data.asset_source.clone().unwrap_or_default(),
            id: data.asset_id.clone(),
        });
    }
    if data.disabled {
        entity_mut.insert(Disabled);
    }
    if data.is_static {
        entity_mut.insert(Static);
    }
    if data.locked {
        entity_mut.insert(Lock);
    }

    if let Some(mesh_renderer) = &data.mesh_renderer {
        // Stamped before resolution is even attempted, and kept whether
        // or not it succeeds: this carrier is the only place the asset
        // ids survive on a live entity, so dropping it for an
        // unresolvable asset would mean the next save silently deletes
        // the reference from the scene file.
        entity_mut.insert(MeshSource::new(
            mesh_renderer.mesh.id.clone(),
            mesh_renderer.material.id.clone(),
        ));
        match resolver.resolve_mesh(&mesh_renderer.mesh, &mesh_renderer.material) {
            Ok(Some(renderer)) => {
                entity_mut.insert(renderer);
            }
            Ok(None) => {}
            Err(err) => {
                *unresolved += 1;
                tracing::warn!(
                    entity = data.name.as_deref().unwrap_or("<unnamed>"),
                    mesh = %mesh_renderer.mesh.id,
                    material = %mesh_renderer.material.id,
                    error = %err,
                    "skipped unresolvable mesh renderer"
                );
            }
        }
    }

    if let Some(sprite) = &data.sprite {
        // Unconditional for the same reason as `MeshSource` above, and
        // additionally carries size/tint: an unresolved sprite has no
        // live `Sprite` component to read those back off.
        entity_mut.insert(SpriteSource {
            atlas: sprite.atlas.id.clone(),
            region: sprite.region.clone(),
            size: sprite.size,
            color: sprite.color,
            z_order: sprite.z_order,
        });
        match resolver.resolve_sprite_uv(&sprite.atlas, &sprite.region) {
            Ok(Some(uv)) => {
                entity_mut.insert(Sprite {
                    size: glam::Vec2::from(sprite.size),
                    uv,
                    color: sprite.color,
                    z_order: sprite.z_order,
                });
            }
            Ok(None) => {}
            Err(err) => {
                *unresolved += 1;
                tracing::warn!(
                    entity = data.name.as_deref().unwrap_or("<unnamed>"),
                    atlas = %sprite.atlas.id,
                    region = %sprite.region,
                    error = %err,
                    "skipped unresolvable sprite"
                );
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// A resolver whose every answer is configured up front, so a test
    /// can pin down one outcome per method without a GPU.
    pub(crate) struct StubResolver {
        pub mesh: fn() -> Result<Option<MeshRenderer>, SceneError>,
        pub uv: fn() -> Result<Option<UvRect>, SceneError>,
        pub mesh_calls: usize,
        pub uv_calls: usize,
    }

    impl StubResolver {
        /// Both methods report "not available yet".
        pub(crate) fn absent() -> Self {
            Self {
                mesh: || Ok(None),
                uv: || Ok(None),
                mesh_calls: 0,
                uv_calls: 0,
            }
        }

        /// Both methods fail.
        pub(crate) fn failing() -> Self {
            Self {
                mesh: || Err(SceneError::Resolve("no such mesh".into())),
                uv: || Err(SceneError::Resolve("no such region".into())),
                mesh_calls: 0,
                uv_calls: 0,
            }
        }

        /// Sprites resolve to a known rect; meshes stay unavailable
        /// (a real `MeshRenderer` can't be built without a GPU).
        pub(crate) fn resolving_sprites() -> Self {
            Self {
                mesh: || Ok(None),
                uv: || {
                    Ok(Some(UvRect {
                        min: [0.25, 0.5],
                        max: [0.75, 1.0],
                    }))
                },
                mesh_calls: 0,
                uv_calls: 0,
            }
        }
    }

    impl SceneResolver for StubResolver {
        fn resolve_mesh(
            &mut self,
            _mesh: &AssetRef,
            _material: &AssetRef,
        ) -> Result<Option<MeshRenderer>, SceneError> {
            self.mesh_calls += 1;
            (self.mesh)()
        }

        fn resolve_sprite_uv(
            &mut self,
            _atlas: &AssetRef,
            _region: &str,
        ) -> Result<Option<UvRect>, SceneError> {
            self.uv_calls += 1;
            (self.uv)()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::StubResolver;
    use super::*;
    use crate::format::SpriteData;
    use bevy_ecs::world::World;

    fn sprite_entity() -> SceneEntity {
        SceneEntity {
            name: Some("Coin".into()),
            sprite: Some(SpriteData {
                atlas: AssetRef {
                    id: "3fa85f64-5717-4562-b3fc-2c963f66afa6".into(),
                },
                region: "coin_0".into(),
                size: [2.0, 3.0],
                color: [1.0, 0.5, 0.25, 1.0],
                z_order: 0.0,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn null_resolver_resolves_nothing_without_erroring() {
        let mut resolver = NullResolver;
        let reference = AssetRef {
            id: "3fa85f64-5717-4562-b3fc-2c963f66afa6".into(),
        };
        assert!(matches!(
            resolver.resolve_mesh(&reference, &reference),
            Ok(None)
        ));
        assert!(matches!(
            resolver.resolve_sprite_uv(&reference, "region"),
            Ok(None)
        ));
    }

    #[test]
    fn resolved_sprite_takes_uv_from_resolver_and_size_from_scene_data() {
        let mut world = World::new();
        let mut resolver = StubResolver::resolving_sprites();
        let mut unresolved = 0;

        let mut entity_mut = world.spawn_empty();
        insert_components(
            &mut entity_mut,
            &sprite_entity(),
            &mut resolver,
            &mut unresolved,
        );
        let entity = entity_mut.id();

        let sprite = world
            .get::<Sprite>(entity)
            .expect("sprite should have been inserted");
        assert_eq!(sprite.uv.min, [0.25, 0.5]);
        assert_eq!(sprite.size, glam::Vec2::new(2.0, 3.0));
        assert_eq!(sprite.color, [1.0, 0.5, 0.25, 1.0]);
        assert_eq!(unresolved, 0);
    }

    #[test]
    fn sprite_z_order_survives_instantiation() {
        // A layer set in scene data has to reach both the live `Sprite`
        // (which the renderer sorts by) and the `SpriteSource` carrier
        // (which a later save reads back), or a 2D level loses its
        // layering the first time it round-trips.
        let mut world = World::new();
        let mut resolver = StubResolver::resolving_sprites();
        let mut unresolved = 0;

        let mut data = sprite_entity();
        if let Some(sprite) = &mut data.sprite {
            sprite.z_order = 3.5;
        }

        let mut entity_mut = world.spawn_empty();
        insert_components(&mut entity_mut, &data, &mut resolver, &mut unresolved);
        let entity = entity_mut.id();

        assert_eq!(
            world
                .get::<Sprite>(entity)
                .expect("sprite should have been inserted")
                .z_order,
            3.5,
        );
        assert_eq!(
            world
                .get::<engine_ecs::components::SpriteSource>(entity)
                .expect("carrier should have been inserted")
                .z_order,
            3.5,
        );
    }

    #[test]
    fn absent_asset_is_not_counted_as_unresolved() {
        let mut world = World::new();
        let mut resolver = StubResolver::absent();
        let mut unresolved = 0;

        let mut entity_mut = world.spawn_empty();
        insert_components(
            &mut entity_mut,
            &sprite_entity(),
            &mut resolver,
            &mut unresolved,
        );
        let entity = entity_mut.id();

        assert!(world.get::<Sprite>(entity).is_none());
        assert_eq!(unresolved, 0, "Ok(None) means 'not yet', not 'broken'");
        assert!(
            world.get::<Name>(entity).is_some(),
            "rest of the entity survives"
        );
    }

    #[test]
    fn failed_resolve_is_counted_and_leaves_the_entity_otherwise_intact() {
        let mut world = World::new();
        let mut resolver = StubResolver::failing();
        let mut unresolved = 0;

        let mut entity_mut = world.spawn_empty();
        insert_components(
            &mut entity_mut,
            &sprite_entity(),
            &mut resolver,
            &mut unresolved,
        );
        let entity = entity_mut.id();

        assert!(world.get::<Sprite>(entity).is_none());
        assert_eq!(unresolved, 1);
        assert_eq!(
            world.get::<Name>(entity).map(|n| n.0.as_str()),
            Some("Coin"),
            "a broken asset reference must not cost the entity its other components"
        );
    }

    #[test]
    fn report_tracks_full_resolution() {
        let clean = InstantiateReport {
            spawned: Vec::new(),
            unresolved: 0,
        };
        assert!(clean.is_fully_resolved());

        let dirty = InstantiateReport {
            spawned: Vec::new(),
            unresolved: 2,
        };
        assert!(!dirty.is_fully_resolved());
    }
}
