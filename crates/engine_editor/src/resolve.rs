//! The editor's [`SceneResolver`]: turns a scene's asset references into
//! live renderable components, using the CPU-side data
//! [`crate::AssetImporter`] already holds.
//!
//! ## Where the pieces come from
//!
//! [`AssetImporter`] owns decoded assets keyed by
//! [`engine_asset::AssetId`] — an [`ImportedGltf`] per model, an
//! [`ImportedTexture`] per image. A `MeshRenderer`, though, holds *GPU*
//! handles. This module is the bridge: look the id up, upload once, hand
//! back the component.
//!
//! ## Why uploads are cached
//!
//! A scene commonly places the same model many times. Uploading per
//! entity would cost one vertex buffer, one texture, and one bind group
//! per instance. [`EditorResolver`] keeps a per-id cache of the handles
//! it has already registered in [`RenderAssets`], so the fiftieth crate
//! in a scene reuses the first one's GPU memory. Per-object material
//! overrides bypass that cache so editing one instance cannot recolor another.
//!
//! The cache lives on the resolver, which is built fresh per scene load,
//! so it can't outlive the [`RenderAssets`] its handles index into.

use std::collections::HashMap;

use bevy_ecs::prelude::Without;
use engine_asset::AssetId;
use engine_ecs::components::{
    InstancedMeshRenderer, MeshLod, MeshRenderer, MeshSource, SkinnedMeshRenderer, Sprite,
    SpriteSource,
};
use engine_ecs::prelude::{Entity, World};
use engine_renderer::{
    GpuContext, MaterialBinding, Mesh, Pipeline, RenderAssets, SkinnedPipeline, UvRect,
};
use engine_scene::{
    AppliedLod, AppliedMaterialOverride, AssetRef, LodCache, MaterialOverrideData, SceneError,
    SceneFoliage, SceneMaterialOverride, SceneModelAnimation, SceneResolver, SkinnedModel,
};
use engine_utils::AssetHandle;

use crate::import::{AssetImporter, ImportedAsset};

/// Resolves scene asset references against the editor's import cache.
///
/// Borrows everything it needs rather than owning it: the importer holds
/// the decoded data, [`RenderAssets`] owns the uploaded handles, and both
/// outlive any single scene load.
pub struct EditorResolver<'a> {
    importer: &'a AssetImporter,
    gpu: &'a GpuContext,
    pipeline: &'a Pipeline,
    assets: &'a mut RenderAssets,
    /// Handles for meshes already uploaded this load, by asset id — see
    /// the module docs on why. Stores the handle pair rather than a whole
    /// `MeshRenderer`: the GPU resources are shared, but each entity needs
    /// its own model-matrix binding, and each share has to go through
    /// [`MeshRenderer::from_handles`] so the ref counts stay right.
    uploaded: HashMap<AssetId, (AssetHandle<Mesh>, AssetHandle<MaterialBinding>)>,
    /// Draws animated models when set; without it every model is still.
    skinned_pipeline: Option<&'a SkinnedPipeline>,
    /// Detail tiers already uploaded this load.
    lods: LodCache,
}

impl<'a> EditorResolver<'a> {
    /// Builds a resolver over the editor's importer and GPU state.
    pub fn new(
        importer: &'a AssetImporter,
        gpu: &'a GpuContext,
        pipeline: &'a Pipeline,
        assets: &'a mut RenderAssets,
    ) -> Self {
        Self {
            importer,
            gpu,
            pipeline,
            assets,
            uploaded: HashMap::new(),
            skinned_pipeline: None,
            lods: HashMap::new(),
        }
    }

    /// The same resolver, also drawing animated (skinned) models.
    pub fn with_skinned_pipeline(mut self, skinned_pipeline: &'a SkinnedPipeline) -> Self {
        self.skinned_pipeline = Some(skinned_pipeline);
        self
    }

    /// How many distinct meshes this resolver has uploaded.
    pub fn uploaded_count(&self) -> usize {
        self.uploaded.len()
    }
}

/// Parses an [`AssetRef`]'s UUID text into an [`AssetId`].
///
/// Scene files are untrusted, so malformed text is an error rather than a
/// panic. `engine_scene` can only hand back a `uuid::Uuid` (it has no
/// `engine_asset` dependency), so the conversion happens here.
fn asset_id(reference: &AssetRef) -> Result<AssetId, SceneError> {
    let uuid = reference.parse_id().map_err(SceneError::Resolve)?;
    Ok(AssetId::from_uuid(uuid))
}

/// The imported material file `changes` names, if it names one.
fn material_file<'i>(
    importer: &'i AssetImporter,
    changes: Option<&MaterialOverrideData>,
) -> Option<&'i engine_scene::MaterialData> {
    let reference = changes?.material.as_ref()?;
    match importer.get(asset_id(reference).ok()?)? {
        ImportedAsset::Material(data) => Some(data),
        _ => None,
    }
}

/// The pixels of the imported image `reference` names.
fn image_asset<'i>(
    importer: &'i AssetImporter,
    reference: &AssetRef,
) -> Option<engine_ecs::components::RgbaImage<'i>> {
    match importer.get(asset_id(reference).ok()?)? {
        ImportedAsset::Texture(texture) => engine_scene::texture_pixels(texture),
        _ => None,
    }
}

impl SceneResolver for EditorResolver<'_> {
    fn resolve_mesh(
        &mut self,
        mesh: &AssetRef,
        material: &AssetRef,
    ) -> Result<Option<MeshRenderer>, SceneError> {
        self.resolve_mesh_with_material(mesh, material, None)
    }

    fn resolve_mesh_with_material(
        &mut self,
        mesh: &AssetRef,
        _material: &AssetRef,
        material_override: Option<&MaterialOverrideData>,
    ) -> Result<Option<MeshRenderer>, SceneError> {
        let id = asset_id(mesh)?;

        // Already uploaded for an earlier entity in this same load. The
        // ref-count check guards against a stale entry: if every entity
        // using this mesh was despawned, the store freed it and the
        // cached handle now points at nothing, so fall through and
        // re-upload rather than hand out a dangling handle.
        if material_override.is_none()
            && let Some(&(mesh_handle, material_handle)) = self.uploaded.get(&id)
            && self.assets.meshes.ref_count(mesh_handle).is_some()
            && self.assets.materials.ref_count(material_handle).is_some()
        {
            return Ok(Some(MeshRenderer::from_handles(
                self.gpu,
                self.pipeline,
                self.assets,
                mesh_handle,
                material_handle,
            )));
        }

        // Not imported (yet) is not a failure: the asset pass may not
        // have reached it, or the file may arrive later via the watcher.
        let Some(asset) = self.importer.get(id) else {
            return Ok(None);
        };

        let ImportedAsset::Mesh(gltf) = asset else {
            return Err(SceneError::Resolve(format!(
                "asset {id} is not a mesh (a scene entity referenced it as one)"
            )));
        };

        let Some(primitive) = engine_scene::base_mesh(gltf) else {
            return Err(SceneError::Resolve(format!(
                "glTF asset {id} has {} primitives; the editor renderer supports exactly one \
                 (or detail tiers named _LOD0, _LOD1, …)",
                gltf.meshes.len()
            )));
        };

        // First embedded image as the base color; `None` lets
        // `from_geometry` fall back to white, so an untextured model still
        // renders lit. Shared with the standalone player's resolver so the
        // editor and a shipped build agree on what an asset looks like.
        let imported_material = primitive
            .material
            .and_then(|index| gltf.materials.get(index));
        let importer = self.importer;
        let (material, sources) = engine_scene::compose_material(
            imported_material,
            material_file(importer, material_override),
            material_override,
        );
        let images = sources.images(Some(gltf), |reference| image_asset(importer, reference));

        let renderer = MeshRenderer::from_geometry_with_maps(
            self.gpu,
            self.pipeline,
            self.assets,
            &primitive.vertices,
            &primitive.indices,
            images,
            material,
        )
        .map_err(|err| SceneError::Resolve(err.to_string()))?;

        // Remember the handles so the next entity using this asset
        // shares the upload. `renderer` already holds ref count 1 for
        // each; every later share goes through `from_handles`, which
        // retains, so the counts track the number of live entities.
        if material_override.is_none() {
            self.uploaded.insert(id, (renderer.mesh, renderer.material));
        }
        Ok(Some(renderer))
    }

    fn resolve_road(
        &mut self,
        road: &engine_scene::RoadData,
    ) -> Result<Option<MeshRenderer>, SceneError> {
        let image = match &road.texture {
            Some(texture) => match self.importer.get(asset_id(texture)?) {
                Some(ImportedAsset::Texture(image)) => Some(image.as_ref()),
                // Still importing, or not an image: draw it plain.
                _ => None,
            },
            None => None,
        };
        engine_scene::build_road_renderer(self.gpu, self.pipeline, self.assets, road, image)
    }

    fn resolve_mesh_lods(
        &mut self,
        mesh: &AssetRef,
        renderer: &mut MeshRenderer,
        auto_generate: bool,
    ) -> Result<(), SceneError> {
        let id = asset_id(mesh)?;
        let Some(ImportedAsset::Mesh(gltf)) = self.importer.get(id) else {
            return Ok(());
        };
        engine_scene::attach_lod_tiers(
            self.gpu,
            self.assets,
            &mut self.lods,
            renderer,
            id,
            gltf,
            auto_generate,
        )
    }

    fn resolve_skinned_lods(
        &mut self,
        mesh: &AssetRef,
        renderer: &mut SkinnedMeshRenderer,
        auto_generate: bool,
    ) -> Result<(), SceneError> {
        let id = asset_id(mesh)?;
        let Some(ImportedAsset::Mesh(gltf)) = self.importer.get(id) else {
            return Ok(());
        };
        engine_scene::attach_skinned_lod_tiers(self.gpu, self.assets, renderer, gltf, auto_generate)
    }

    fn resolve_skinned_mesh(
        &mut self,
        mesh: &AssetRef,
        _material: &AssetRef,
        material_override: Option<&MaterialOverrideData>,
    ) -> Result<Option<SkinnedModel>, SceneError> {
        let Some(skinned_pipeline) = self.skinned_pipeline else {
            return Ok(None);
        };
        let id = asset_id(mesh)?;
        // Uploaded as a still mesh already: it has no skin.
        if self.uploaded.contains_key(&id) {
            return Ok(None);
        }
        let Some(ImportedAsset::Mesh(gltf)) = self.importer.get(id) else {
            return Ok(None);
        };
        let importer = self.importer;
        let images = |reference: &AssetRef| image_asset(importer, reference);
        engine_scene::upload_skinned_model(
            self.gpu,
            self.pipeline,
            skinned_pipeline,
            self.assets,
            gltf,
            &mesh.id,
            &engine_scene::MaterialSetup {
                changes: material_override,
                file: material_file(importer, material_override),
                images: &images,
            },
        )
    }

    fn resolve_sprite_uv(
        &mut self,
        atlas: &AssetRef,
        _region: &str,
    ) -> Result<Option<UvRect>, SceneError> {
        let id = asset_id(atlas)?;
        let Some(asset) = self.importer.get(id) else {
            return Ok(None);
        };
        if !matches!(asset, ImportedAsset::Texture(_)) {
            return Err(SceneError::Resolve(format!(
                "asset {id} is not a texture (a sprite referenced it as an atlas)"
            )));
        }

        // This provisional full-image UV is replaced by the shared atlas
        // pass, which applies a saved grid cell after packing the images.
        Ok(Some(UvRect {
            min: [0.0, 0.0],
            max: [1.0, 1.0],
        }))
    }
}

/// The import pass an object's material files and images were last read
/// from. A later pass may have changed them, so the object is drawn again.
#[derive(bevy_ecs::component::Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaterialImportPass(pub u64);

/// Whether an object's material reads material files or image assets, which
/// change when they are imported again.
fn uses_material_assets(authored: Option<&MaterialOverrideData>) -> bool {
    authored.is_some_and(|value| value.material.is_some() || !value.images.is_empty())
}

/// Whether an object's drawn material is out of date: its settings changed,
/// or the files they read were imported again since.
fn material_stale(
    authored: Option<&MaterialOverrideData>,
    applied: Option<&AppliedMaterialOverride>,
    pass: Option<&MaterialImportPass>,
    current: u64,
) -> bool {
    material_needs_refresh(authored, applied)
        || (uses_material_assets(authored) && pass.is_none_or(|pass| pass.0 != current))
}

/// Turns every not-yet-resolved renderable reference in `world` into a
/// live component, and reports how many failed.
///
/// Run once per frame. It is the single mechanism behind every way an
/// entity can acquire geometry — a scene load, an asset dragged into the
/// viewport, an import that finished after the entity was created, a
/// source file changed on disk and re-imported. All of them leave a
/// carrier component ([`MeshSource`] or its sprite equivalent) with no live
/// renderable beside it, and all of them are picked up here.
///
/// The missing-renderable query is empty in a fully resolved scene; the
/// material query compares authored and applied values so only changed
/// instances re-upload.
///
/// A reference that fails is logged once per pass and counted, never
/// retried into an error loop — the entity keeps its carrier, so a later
/// successful import still fixes it.
pub fn resolve_pending(
    world: &mut World,
    importer: &AssetImporter,
    gpu: &GpuContext,
    pipeline: &Pipeline,
    skinned_pipeline: &SkinnedPipeline,
    assets: &mut RenderAssets,
) -> ResolveReport {
    let current_pass = importer.pass_id();
    // An animated model whose material changed is rebuilt from scratch:
    // its renderer is released here and it rejoins the pending list.
    let stale_skinned: Vec<Entity> = world
        .query::<(
            Entity,
            &SkinnedMeshRenderer,
            Option<&SceneMaterialOverride>,
            Option<&AppliedMaterialOverride>,
            Option<&MaterialImportPass>,
        )>()
        .iter(world)
        .filter(|(_, _, authored, applied, pass)| {
            material_stale(
                authored.map(|value| &value.0),
                *applied,
                *pass,
                current_pass,
            )
        })
        .map(|(entity, ..)| entity)
        .collect();
    for entity in stale_skinned {
        release_skinned(world, assets, entity);
    }

    let pending: Vec<(Entity, MeshSource, Option<MaterialOverrideData>)> = world
        .query_filtered::<
            (Entity, &MeshSource, Option<&SceneMaterialOverride>),
            (Without<MeshRenderer>, Without<SkinnedMeshRenderer>),
        >()
        .iter(world)
        .map(|(entity, source, authored)| (entity, source.clone(), authored.map(|value| value.0.clone())))
        .collect();

    let refresh: Vec<_> = world
        .query::<(
            Entity,
            &MeshSource,
            &MeshRenderer,
            Option<&SceneMaterialOverride>,
            Option<&AppliedMaterialOverride>,
            Option<&MaterialImportPass>,
        )>()
        .iter(world)
        .filter(|(_, _, _, authored, applied, pass)| {
            material_stale(
                authored.map(|value| &value.0),
                *applied,
                *pass,
                current_pass,
            )
        })
        .map(|(entity, source, _, authored, ..)| {
            (
                entity,
                source.clone(),
                authored.map(|value| value.0.clone()),
            )
        })
        .collect();

    let mut resolver = EditorResolver::new(importer, gpu, pipeline, assets)
        .with_skinned_pipeline(skinned_pipeline);
    let mut report = ResolveReport::default();

    for (entity, source, authored) in pending.into_iter().chain(refresh) {
        let mesh = AssetRef { id: source.mesh };
        let material = AssetRef {
            id: source.material,
        };
        match resolver.resolve_skinned_mesh(&mesh, &material, authored.as_ref()) {
            Ok(Some(mut model)) => {
                if let Some(previous) = world.get::<MeshRenderer>(entity) {
                    previous.release(resolver.assets);
                }
                let auto_generate = wants_generated_lods(world, entity);
                if let Err(err) =
                    resolver.resolve_skinned_lods(&mesh, &mut model.renderer, auto_generate)
                {
                    tracing::warn!(error = %err, "could not load an animated model's detail tiers");
                }
                let settings = world
                    .get::<SceneModelAnimation>(entity)
                    .map(|animation| animation.0.clone())
                    .unwrap_or_default();
                let mut entity_mut = world.entity_mut(entity);
                entity_mut.remove::<MeshRenderer>();
                engine_scene::insert_skinned_model(&mut entity_mut, model, settings, authored);
                entity_mut.insert((AppliedLod(auto_generate), MaterialImportPass(current_pass)));
                if let Some(mut player) = entity_mut.get_mut::<engine_ecs::AnimationPlayer>() {
                    // Studio plays a clip only when asked to (Preview).
                    player.playing = false;
                }
                // The Scene View shows the first frame, not the bind pose.
                if let (Some(player), Some(skinned)) = (
                    entity_mut.get::<engine_ecs::AnimationPlayer>(),
                    entity_mut.get::<SkinnedMeshRenderer>(),
                ) {
                    engine_ecs::upload_pose(gpu, player, skinned);
                }
                report.resolved += 1;
                continue;
            }
            Ok(None) => {}
            Err(err) => {
                report.failed += 1;
                tracing::warn!(error = %err, "could not resolve a scene entity's animated model");
                continue;
            }
        }
        match resolver.resolve_mesh_with_material(&mesh, &material, authored.as_ref()) {
            Ok(Some(mut renderer)) => {
                if let Some(previous) = world.get::<MeshRenderer>(entity) {
                    previous.release(resolver.assets);
                }
                let auto_generate = wants_generated_lods(world, entity);
                if let Err(err) = resolver.resolve_mesh_lods(&mesh, &mut renderer, auto_generate) {
                    tracing::warn!(error = %err, "could not load a mesh's detail tiers");
                }
                world.entity_mut(entity).insert((
                    renderer,
                    AppliedMaterialOverride(authored),
                    AppliedLod(auto_generate),
                    MaterialImportPass(current_pass),
                ));
                report.resolved += 1;
            }
            Ok(None) => report.pending += 1,
            Err(err) => {
                report.failed += 1;
                tracing::warn!(error = %err, "could not resolve a scene entity's mesh");
            }
        }
    }

    refresh_lods(world, &mut resolver);

    let sprite_report = resolve_pending_sprites(world, &mut resolver);
    report.resolved += sprite_report.resolved;
    report.pending += sprite_report.pending;
    report.failed += sprite_report.failed;

    let foliage_report = resolve_pending_foliage(world, &mut resolver);
    report.resolved += foliage_report.resolved;
    report.pending += foliage_report.pending;
    report.failed += foliage_report.failed;

    let road_report = sync_roads(world, &mut resolver);
    report.resolved += road_report.resolved;
    report.failed += road_report.failed;

    if report.resolved > 0 {
        tracing::info!(
            resolved = report.resolved,
            pending = report.pending,
            failed = report.failed,
            "resolved scene asset references into renderable components"
        );
    }

    report
}

/// The road data a road's drawn strip was last built from, so the strip is
/// rebuilt only when the road changes, and whether its image was still
/// missing then (it is rebuilt once the image has imported).
#[derive(bevy_ecs::component::Component, Debug, Clone, PartialEq)]
pub struct BuiltRoad(pub engine_scene::RoadData, pub bool);

/// Builds or rebuilds the strip of every road whose data changed since its
/// strip was built (new, edited, undone), and drops the strip of a road
/// that lost its surface.
fn sync_roads(world: &mut World, resolver: &mut EditorResolver<'_>) -> ResolveReport {
    let importer = resolver.importer;
    let has_image = |road: &engine_scene::RoadData| {
        road.texture
            .as_ref()
            .and_then(|texture| asset_id(texture).ok())
            .is_some_and(|id| matches!(importer.get(id), Some(ImportedAsset::Texture(_))))
    };
    let stale: Vec<(Entity, engine_scene::RoadData)> = world
        .query::<(Entity, &engine_scene::SceneRoad, Option<&BuiltRoad>)>()
        .iter(world)
        .filter(|(_, road, built)| {
            built.is_none_or(|built| built.0 != road.0 || (built.1 && has_image(&road.0)))
        })
        .map(|(entity, road, _)| (entity, road.0.clone()))
        .collect();
    // A road that is no longer a road keeps no strip.
    let orphaned: Vec<Entity> = world
        .query_filtered::<Entity, (
            bevy_ecs::query::With<BuiltRoad>,
            Without<engine_scene::SceneRoad>,
        )>()
        .iter(world)
        .collect();
    let mut report = ResolveReport::default();
    for entity in orphaned {
        if let Some(previous) = world.get::<MeshRenderer>(entity) {
            previous.release(resolver.assets);
        }
        world
            .entity_mut(entity)
            .remove::<(MeshRenderer, BuiltRoad)>();
    }
    for (entity, road) in stale {
        if let Some(previous) = world.get::<MeshRenderer>(entity) {
            previous.release(resolver.assets);
        }
        world.entity_mut(entity).remove::<MeshRenderer>();
        match resolver.resolve_road(&road) {
            Ok(Some(renderer)) => {
                world.entity_mut(entity).insert(renderer);
                report.resolved += 1;
            }
            Ok(None) => {}
            Err(err) => {
                report.failed += 1;
                tracing::warn!(error = %err, "could not build a road's surface");
            }
        }
        let missing_image = road.texture.is_some() && !has_image(&road);
        world
            .entity_mut(entity)
            .insert(BuiltRoad(road, missing_image));
    }
    report
}

/// Whether `entity`'s [`MeshLod`] asks for generated detail tiers.
fn wants_generated_lods(world: &World, entity: Entity) -> bool {
    world
        .get::<MeshLod>(entity)
        .is_some_and(|lod| lod.auto_generate)
}

/// Rebuilds the detail tiers of every live mesh whose "Generate simpler
/// meshes" setting changed since it was built.
fn refresh_lods(world: &mut World, resolver: &mut EditorResolver<'_>) {
    let stale: Vec<(Entity, String, bool)> = world
        .query::<(
            Entity,
            &MeshSource,
            &MeshRenderer,
            Option<&MeshLod>,
            Option<&AppliedLod>,
        )>()
        .iter(world)
        .filter_map(|(entity, source, _, lod, applied)| {
            let wanted = lod.is_some_and(|lod| lod.auto_generate);
            (applied.map(|applied| applied.0) != Some(wanted))
                .then(|| (entity, source.mesh.clone(), wanted))
        })
        .collect();
    for (entity, mesh, wanted) in stale {
        let Ok(mut entity_mut) = world.get_entity_mut(entity) else {
            continue;
        };
        let Some(mut renderer) = entity_mut.take::<MeshRenderer>() else {
            continue;
        };
        if let Err(err) = resolver.resolve_mesh_lods(&AssetRef { id: mesh }, &mut renderer, wanted)
        {
            tracing::warn!(error = %err, "could not rebuild a mesh's detail tiers");
        }
        entity_mut.insert((renderer, AppliedLod(wanted)));
    }

    // Animated models: the same, for their skinned tiers.
    let stale: Vec<(Entity, String, bool)> = world
        .query::<(
            Entity,
            &MeshSource,
            &SkinnedMeshRenderer,
            Option<&MeshLod>,
            Option<&AppliedLod>,
        )>()
        .iter(world)
        .filter_map(|(entity, source, _, lod, applied)| {
            let wanted = lod.is_some_and(|lod| lod.auto_generate);
            (applied.map(|applied| applied.0) != Some(wanted))
                .then(|| (entity, source.mesh.clone(), wanted))
        })
        .collect();
    for (entity, mesh, wanted) in stale {
        let Ok(mut entity_mut) = world.get_entity_mut(entity) else {
            continue;
        };
        let Some(mut renderer) = entity_mut.take::<SkinnedMeshRenderer>() else {
            continue;
        };
        if let Err(err) =
            resolver.resolve_skinned_lods(&AssetRef { id: mesh }, &mut renderer, wanted)
        {
            tracing::warn!(error = %err, "could not rebuild an animated model's detail tiers");
        }
        entity_mut.insert((renderer, AppliedLod(wanted)));
    }
}

/// Drops an animated model's renderer, clips and player, releasing its GPU
/// handles, so the next pass rebuilds it.
pub fn release_skinned(world: &mut World, assets: &mut RenderAssets, entity: Entity) {
    let Ok(mut entity_mut) = world.get_entity_mut(entity) else {
        return;
    };
    if let Some(renderer) = entity_mut.take::<SkinnedMeshRenderer>() {
        renderer.release(assets);
    }
    entity_mut.remove::<(
        engine_ecs::AnimationPlayer,
        engine_ecs::AnimationClips,
        engine_ecs::AnimationEvents,
        AppliedMaterialOverride,
    )>();
}

fn material_needs_refresh(
    authored: Option<&MaterialOverrideData>,
    applied: Option<&AppliedMaterialOverride>,
) -> bool {
    match applied {
        Some(applied) => authored != applied.0.as_ref(),
        None => authored.is_some(),
    }
}

fn resolve_pending_foliage(world: &mut World, resolver: &mut impl SceneResolver) -> ResolveReport {
    let pending: Vec<_> = world
        .query_filtered::<(Entity, &SceneFoliage), Without<InstancedMeshRenderer>>()
        .iter(world)
        .map(|(entity, _)| entity)
        .collect();
    let mut report = ResolveReport::default();
    for entity in pending {
        let Some(source) = world.get::<SceneFoliage>(entity) else {
            continue;
        };
        let source = source.0.clone();
        match engine_scene::resolve_foliage(resolver, &source) {
            Ok(Some(renderer)) => {
                world.entity_mut(entity).insert(renderer);
                report.resolved += 1;
            }
            Ok(None) => report.pending += 1,
            Err(error) => {
                report.failed += 1;
                tracing::warn!(%error, "could not resolve a foliage model");
            }
        }
    }
    report
}

fn resolve_pending_sprites(world: &mut World, resolver: &mut impl SceneResolver) -> ResolveReport {
    let pending: Vec<_> = world
        .query_filtered::<(Entity, &SpriteSource), Without<Sprite>>()
        .iter(world)
        .map(|(entity, source)| (entity, source.clone()))
        .collect();
    let mut report = ResolveReport::default();
    for (entity, source) in pending {
        let atlas = AssetRef { id: source.atlas };
        match resolver.resolve_sprite_uv(&atlas, &source.region) {
            Ok(Some(uv)) => {
                let mut sprite = Sprite::new(glam::Vec2::from(source.size), uv);
                sprite.color = source.color;
                sprite.z_order = source.z_order;
                world.entity_mut(entity).insert(sprite);
                report.resolved += 1;
            }
            Ok(None) => report.pending += 1,
            Err(error) => {
                report.failed += 1;
                tracing::warn!(%error, "could not resolve a scene entity's sprite");
            }
        }
    }
    report
}

/// What one [`resolve_pending`] pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResolveReport {
    /// References turned into live renderable components this pass.
    pub resolved: usize,
    /// References whose asset is not imported yet — will be retried.
    pub pending: usize,
    /// References that failed outright (unknown id, wrong kind, bad
    /// geometry). Surfaced to the Console.
    pub failed: usize,
}

impl ResolveReport {
    /// Whether this pass changed anything worth redrawing for.
    pub fn changed(&self) -> bool {
        self.resolved > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct SpriteResolver;

    impl SceneResolver for SpriteResolver {
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
            Ok(Some(UvRect {
                min: [0.25, 0.5],
                max: [0.75, 1.0],
            }))
        }
    }

    #[test]
    fn pending_scene_sprite_becomes_renderable_after_image_import() {
        let mut world = World::new();
        let entity = world
            .spawn(engine_ecs::components::SpriteSource {
                atlas: "3fa85f64-5717-4562-b3fc-2c963f66afa6".into(),
                region: "full".into(),
                size: [2.0, 3.0],
                color: [1.0, 0.5, 0.25, 1.0],
                z_order: 4.0,
            })
            .id();

        let report = resolve_pending_sprites(&mut world, &mut SpriteResolver);
        assert_eq!(report.resolved, 1);
        let sprite = world.get::<engine_ecs::components::Sprite>(entity).unwrap();
        assert_eq!(sprite.size, glam::Vec2::new(2.0, 3.0));
        assert_eq!(sprite.uv.min, [0.25, 0.5]);
        assert_eq!(sprite.z_order, 4.0);
    }

    #[test]
    fn missing_foliage_model_keeps_its_authored_group_for_later_import() {
        let mut world = World::new();
        let entity = world
            .spawn(engine_scene::SceneFoliage(engine_scene::FoliageData::new(
                reference("3fa85f64-5717-4562-b3fc-2c963f66afa6"),
                reference("3fa85f64-5717-4562-b3fc-2c963f66afa6"),
                vec![engine_scene::TransformData::from(
                    engine_utils::Transform::IDENTITY,
                )],
                None,
            )))
            .id();
        let report = resolve_pending_foliage(&mut world, &mut SpriteResolver);
        assert_eq!(report.pending, 1);
        assert!(world.get::<engine_scene::SceneFoliage>(entity).is_some());
        assert!(
            world
                .get::<engine_ecs::components::InstancedMeshRenderer>(entity)
                .is_none()
        );
    }

    fn reference(id: &str) -> AssetRef {
        AssetRef { id: id.into() }
    }

    #[test]
    fn malformed_asset_id_is_an_error_not_a_panic() {
        let result = asset_id(&reference("not-a-uuid"));
        assert!(matches!(result, Err(SceneError::Resolve(_))));
    }

    #[test]
    fn well_formed_asset_id_parses() {
        let parsed = asset_id(&reference("3fa85f64-5717-4562-b3fc-2c963f66afa6"));
        assert!(parsed.is_ok());
    }

    #[test]
    fn empty_asset_id_is_rejected() {
        assert!(asset_id(&reference("")).is_err());
    }

    #[test]
    fn material_refresh_tracks_added_changed_and_removed_overrides() {
        let red = engine_scene::MaterialOverrideData {
            tint: [1.0, 0.2, 0.2],
            metallic: Some(0.5),
            roughness: None,
            ..Default::default()
        };
        let blue = engine_scene::MaterialOverrideData {
            tint: [0.2, 0.2, 1.0],
            metallic: None,
            roughness: Some(0.8),
            ..Default::default()
        };

        assert!(material_needs_refresh(Some(&red), None));
        assert!(material_needs_refresh(
            Some(&blue),
            Some(&engine_scene::AppliedMaterialOverride(Some(red.clone())))
        ));
        assert!(material_needs_refresh(
            None,
            Some(&engine_scene::AppliedMaterialOverride(Some(red.clone())))
        ));
        assert!(!material_needs_refresh(
            Some(&red),
            Some(&engine_scene::AppliedMaterialOverride(Some(red.clone())))
        ));
        assert!(!material_needs_refresh(
            None,
            Some(&engine_scene::AppliedMaterialOverride(None))
        ));
    }
}
