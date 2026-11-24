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
    AssetSource, Camera, Disabled, InstancedMeshRenderer, Lock, MeshRenderer, MeshSource, Name,
    Script, Sprite, SpriteSource, Transform, WaterVolume,
};
use engine_renderer::UvRect;

use crate::error::SceneError;
use crate::format::{AssetRef, MaterialOverrideData, ModelAnimationData, SceneEntity};

/// An animated model resolved for one scene object: its skinned renderer
/// plus the skeleton and every clip in its file.
pub struct SkinnedModel {
    /// The uploaded skinned mesh and material.
    pub renderer: engine_ecs::components::SkinnedMeshRenderer,
    /// The skin the mesh is bound to.
    pub skeleton: std::sync::Arc<engine_asset::ImportedSkeleton>,
    /// Every clip in the file, in file order.
    pub clips: Vec<std::sync::Arc<engine_asset::ImportedAnimation>>,
}

/// The player an animated model starts with, from its authored settings.
///
/// Picks the clip named in `settings`, or the file's first clip when none
/// is named or the named one is missing (logged). `None` when the file has
/// no clips: the model then stands in its bind pose.
pub fn animation_player_for(
    skeleton: &std::sync::Arc<engine_asset::ImportedSkeleton>,
    clips: &[std::sync::Arc<engine_asset::ImportedAnimation>],
    settings: &ModelAnimationData,
) -> Option<engine_ecs::AnimationPlayer> {
    let named = settings.clip.as_deref().and_then(|name| {
        let found = clips.iter().find(|clip| clip.name.as_deref() == Some(name));
        if found.is_none() {
            tracing::warn!(
                clip = name,
                "animation clip not found; playing the first clip"
            );
        }
        found
    });
    let clip = named.or_else(|| clips.first())?;
    let mut player = engine_ecs::AnimationPlayer::new(
        std::sync::Arc::clone(skeleton),
        std::sync::Arc::clone(clip),
    )
    .with_events(engine_ecs::AnimationEvent::from_clip(clip));
    player.looping = settings.looping;
    player.speed = settings.speed;
    player.playing = settings.play_on_start;
    Some(player)
}

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

    /// Resolves a mesh with per-object material adjustments. Older custom
    /// resolvers keep their previous behavior until they opt in.
    fn resolve_mesh_with_material(
        &mut self,
        mesh: &AssetRef,
        material: &AssetRef,
        material_override: Option<&MaterialOverrideData>,
    ) -> Result<Option<MeshRenderer>, SceneError> {
        let _ = material_override;
        self.resolve_mesh(mesh, material)
    }

    /// Gives `renderer` (just resolved from `mesh`) its lower detail tiers:
    /// those authored in the file, or generated ones when `auto_generate`
    /// is on.
    ///
    /// Defaulted to doing nothing, so a resolver that knows nothing about
    /// detail tiers draws every mesh in full.
    ///
    /// # Errors
    ///
    /// [`SceneError::Resolve`] if a tier cannot be uploaded.
    fn resolve_mesh_lods(
        &mut self,
        mesh: &AssetRef,
        renderer: &mut MeshRenderer,
        auto_generate: bool,
    ) -> Result<(), SceneError> {
        let _ = (mesh, renderer, auto_generate);
        Ok(())
    }

    /// Resolves a mesh reference into an animated model when the file is
    /// skinned. `Ok(None)` means "not animated, or not available": the
    /// caller then falls back to [`SceneResolver::resolve_mesh_with_material`].
    ///
    /// Defaulted to `Ok(None)` so a resolver that knows nothing about
    /// skinning keeps drawing every model still.
    ///
    /// # Errors
    ///
    /// [`SceneError::Resolve`] if the file is skinned but cannot be
    /// uploaded.
    fn resolve_skinned_mesh(
        &mut self,
        mesh: &AssetRef,
        material: &AssetRef,
        material_override: Option<&MaterialOverrideData>,
    ) -> Result<Option<SkinnedModel>, SceneError> {
        let _ = (mesh, material, material_override);
        Ok(None)
    }

    /// Builds the GPU side of a terrain: its chunk meshes and tile
    /// materials. `Ok(None)` means "not drawn by this resolver".
    ///
    /// Defaulted to `Ok(None)`, so a resolver that knows nothing about
    /// terrain still loads the scene; Studio builds terrain from its own
    /// live copy instead.
    ///
    /// # Errors
    ///
    /// [`SceneError::Resolve`] if a tile is missing or can't be uploaded.
    fn resolve_terrain(
        &mut self,
        terrain: &crate::terrain::TerrainData,
    ) -> Result<Option<engine_ecs::TerrainRenderer>, SceneError> {
        let _ = terrain;
        Ok(None)
    }

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

    /// Resolves a sound reference into a decoded clip.
    ///
    /// Defaulted to `Ok(None)` — "not available" — because a resolver
    /// that only knows about geometry (the editor's, before audio is
    /// loaded) should spawn a scene's emitters as authorable data
    /// without having to decode any audio.
    ///
    /// # Errors
    ///
    /// [`SceneError::Resolve`] if the reference is broken or the bytes
    /// are not a sound this build can decode.
    fn resolve_sound(
        &mut self,
        sound: &AssetRef,
        looping: bool,
    ) -> Result<Option<engine_audio::StaticSound>, SceneError> {
        let _ = (sound, looping);
        Ok(None)
    }
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
    entity_mut.insert(crate::SceneObjectId(
        data.object_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
    ));
    if let Some(script) = &data.script {
        entity_mut.insert(Script::new(script.clone()));
    }
    if let Some(transform) = data.transform {
        entity_mut.insert(Transform::from(engine_utils::Transform::from(transform)));
    }
    if let Some(camera) = data.camera {
        entity_mut.insert(Camera::from(engine_renderer::Camera::from(camera)));
        if camera.active {
            entity_mut.insert(engine_ecs::components::ActiveCamera);
        }
    }
    if let Some(follow) = &data.camera_follow {
        entity_mut.insert(engine_ecs::components::CameraFollow::from(follow.clone()));
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
    if data.locked {
        entity_mut.insert(Lock);
    }
    if data.keep_between_scenes {
        entity_mut.insert(crate::KeepBetweenScenes);
    }
    if !data.tags.is_empty() {
        entity_mut.insert(crate::SceneTags(data.tags.clone()));
    }
    if let Some(file) = &data.open_from {
        entity_mut.insert(crate::SceneFileOwner(file.clone()));
    }
    if let Some(link) = &data.prefab {
        entity_mut.insert(crate::ScenePrefabLink(link.clone()));
    }
    if let Some(order) = data.sibling_order {
        entity_mut.insert(crate::format::SiblingOrder(order));
    }
    if let Some(ai) = &data.ai {
        entity_mut.insert(crate::ai::SceneAi(ai.clone()));
    }
    if let Some(emitter) = &data.audio_emitter {
        // The carrier always goes on, resolved or not — same rule as
        // `MeshSource`: it is the only place the authored values survive
        // on a live entity, so dropping it would delete the emitter from
        // the file on the next save.
        entity_mut.insert(crate::format::SceneAudioEmitter(emitter.clone()));
        match resolver.resolve_sound(&emitter.sound, emitter.looping) {
            Ok(Some(sound)) => {
                let mut live = engine_ecs::audio::AudioEmitter::new(sound);
                live.autoplay = emitter.autoplay;
                live.gain = emitter.gain;
                live.radius = emitter.radius;
                entity_mut.insert(live);
            }
            Ok(None) => {}
            Err(err) => {
                *unresolved += 1;
                tracing::warn!(
                    error = %err,
                    sound = %emitter.sound.id,
                    "skipped unresolvable audio emitter"
                );
            }
        }
    }
    if let Some(collider) = &data.collider {
        // A carrier, like `MeshSource` below: the editor has no physics
        // world, so this component is the only place the collider
        // survives on a live entity — and the next save reads it back.
        entity_mut.insert(crate::format::SceneCollider(collider.clone()));
    }
    if let Some(water) = data.water {
        match engine_physics::WaterSurface::try_from(water) {
            Ok(surface) => {
                entity_mut.insert(WaterVolume { surface });
            }
            Err(error) => {
                *unresolved += 1;
                tracing::warn!(%error, "skipped invalid water surface");
            }
        }
    }
    if let Some(light) = data.point_light {
        entity_mut.insert(crate::format::ScenePointLight(light));
    }
    if let Some(ui) = &data.ui {
        entity_mut.insert(crate::format::SceneUi(ui.clone()));
    }
    if let Some(tilemap) = &data.tilemap {
        entity_mut.insert(crate::SceneTilemap(tilemap.clone()));
    }
    if !data.rules.is_empty() {
        entity_mut.insert(crate::SceneRules(data.rules.clone()));
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
        if let Some(material_override) = &mesh_renderer.material_override {
            entity_mut.insert(crate::SceneMaterialOverride(material_override.clone()));
        }
        if let Some(animation) = &mesh_renderer.animation {
            entity_mut.insert(crate::SceneModelAnimation(animation.clone()));
        }
        if let Some(lod) = &mesh_renderer.lod {
            entity_mut.insert(engine_ecs::components::MeshLod::from(lod));
        }
        let auto_generate = mesh_renderer
            .lod
            .as_ref()
            .is_some_and(|lod| lod.auto_generate);
        match resolver.resolve_skinned_mesh(
            &mesh_renderer.mesh,
            &mesh_renderer.material,
            mesh_renderer.material_override.as_ref(),
        ) {
            Ok(Some(model)) => insert_skinned_model(
                entity_mut,
                model,
                mesh_renderer.animation.clone().unwrap_or_default(),
                mesh_renderer.material_override.clone(),
            ),
            Ok(None) => match resolver.resolve_mesh_with_material(
                &mesh_renderer.mesh,
                &mesh_renderer.material,
                mesh_renderer.material_override.as_ref(),
            ) {
                Ok(Some(mut renderer)) => {
                    // A model whose tiers fail still draws, in full detail.
                    if let Err(err) = resolver.resolve_mesh_lods(
                        &mesh_renderer.mesh,
                        &mut renderer,
                        auto_generate,
                    ) {
                        tracing::warn!(
                            mesh = %mesh_renderer.mesh.id,
                            error = %err,
                            "could not load a mesh's detail tiers"
                        );
                    }
                    entity_mut.insert((
                        renderer,
                        crate::AppliedMaterialOverride(mesh_renderer.material_override.clone()),
                        crate::AppliedLod(auto_generate),
                    ));
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
            },
            Err(err) => {
                *unresolved += 1;
                tracing::warn!(
                    entity = data.name.as_deref().unwrap_or("<unnamed>"),
                    mesh = %mesh_renderer.mesh.id,
                    error = %err,
                    "skipped unresolvable animated model"
                );
            }
        }
    }
    if let Some(terrain) = &data.terrain {
        entity_mut.insert(crate::terrain::SceneTerrain(terrain.clone()));
        match resolver.resolve_terrain(terrain) {
            Ok(Some(renderer)) => {
                entity_mut.insert(renderer);
            }
            Ok(None) => {}
            Err(err) => {
                *unresolved += 1;
                tracing::warn!(error = %err, "skipped unresolvable terrain");
            }
        }
    }
    if let Some(foliage) = &data.foliage {
        entity_mut.insert(crate::SceneFoliage(foliage.clone()));
        match resolver.resolve_mesh(&foliage.mesh, &foliage.material) {
            Ok(Some(renderer)) => {
                entity_mut.insert(InstancedMeshRenderer {
                    mesh: renderer.mesh,
                    material: renderer.material,
                    instances: foliage.instances.iter().copied().map(Into::into).collect(),
                });
            }
            Ok(None) => {}
            Err(err) => {
                *unresolved += 1;
                tracing::warn!(error = %err, "skipped unresolvable foliage model");
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
        if let Some(animation) = sprite.animation {
            entity_mut.insert(crate::SceneSpriteAnimation::new(animation));
        } else if !sprite.clips.is_empty() {
            entity_mut.insert(crate::SceneSpriteAnimation::clips_only());
        }
        if !sprite.clips.is_empty() {
            entity_mut.insert(crate::SceneSpriteClips(sprite.clips.clone()));
        }
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

/// Inserts an animated model's renderer, clips and player.
///
/// Shared by scene loading and the editor's pending-resolution pass.
pub fn insert_skinned_model(
    entity_mut: &mut EntityWorldMut<'_>,
    model: SkinnedModel,
    settings: ModelAnimationData,
    material_override: Option<MaterialOverrideData>,
) {
    let player = animation_player_for(&model.skeleton, &model.clips, &settings);
    entity_mut.insert((
        model.renderer,
        engine_ecs::AnimationClips(model.clips),
        engine_ecs::AnimationEvents::default(),
        crate::AppliedMaterialOverride(material_override),
    ));
    if let Some(player) = player {
        entity_mut.insert(player);
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
                animation: None,
                clips: Vec::new(),
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

    fn clip(name: &str) -> std::sync::Arc<engine_asset::ImportedAnimation> {
        std::sync::Arc::new(engine_asset::ImportedAnimation {
            name: Some(name.into()),
            events: vec![engine_asset::ImportedAnimationEvent {
                time: 0.2,
                name: format!("{name}_event"),
            }],
            duration: 1.0,
            channels: std::collections::HashMap::new(),
        })
    }

    fn skeleton() -> std::sync::Arc<engine_asset::ImportedSkeleton> {
        std::sync::Arc::new(engine_asset::ImportedSkeleton {
            name: None,
            joints: Vec::new(),
        })
    }

    #[test]
    fn animation_player_picks_the_named_clip_or_the_first() {
        let clips = [clip("idle"), clip("walk")];
        let settings = ModelAnimationData {
            clip: Some("walk".into()),
            looping: false,
            speed: 2.0,
            play_on_start: false,
            animator: None,
        };
        let player = animation_player_for(&skeleton(), &clips, &settings).expect("player");
        assert_eq!(player.clip.name.as_deref(), Some("walk"));
        assert!(!player.looping && !player.playing);
        assert_eq!(player.speed, 2.0);
        assert_eq!(player.events[0].name, "walk_event", "the clip's own events");

        let missing = ModelAnimationData {
            clip: Some("swim".into()),
            ..Default::default()
        };
        let player = animation_player_for(&skeleton(), &clips, &missing).expect("player");
        assert_eq!(player.clip.name.as_deref(), Some("idle"));

        assert!(animation_player_for(&skeleton(), &[], &missing).is_none());
    }

    /// Loads the island character and checks it arrives animated.
    #[test]
    #[ignore = "needs a GPU adapter"]
    fn skinned_model_uploads_with_its_clips_and_player() {
        let gpu = engine_renderer::GpuContext::headless(4, 4).expect("headless GPU");
        let pipeline = gpu.create_pbr_pipeline("test pbr");
        let skinned_pipeline = gpu.create_skinned_pbr_pipeline(&pipeline, "test skinned");
        let mut assets = engine_renderer::RenderAssets::default();
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../games/island/assets");
        let gltf = engine_asset::import_gltf_file(&root.join("models/character.gltf"), &root)
            .expect("character imports");

        let model = crate::upload_skinned_model(
            &gpu,
            &pipeline,
            &skinned_pipeline,
            &mut assets,
            &gltf,
            "character",
            None,
        )
        .expect("uploads")
        .expect("the character is skinned");
        let mut world = bevy_ecs::world::World::new();
        let mut entity = world.spawn_empty();
        insert_skinned_model(&mut entity, model, ModelAnimationData::default(), None);

        assert!(entity.contains::<engine_ecs::components::SkinnedMeshRenderer>());
        let clips = entity.get::<engine_ecs::AnimationClips>().expect("clips");
        assert!(clips.get("walk").is_some(), "{:?}", clips.names());
        assert!(entity.get::<engine_ecs::AnimationPlayer>().is_some());
    }
}
