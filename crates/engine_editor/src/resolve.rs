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
    InstancedMeshRenderer, MeshRenderer, MeshSource, Sprite, SpriteSource,
};
use engine_ecs::prelude::{Entity, World};
use engine_renderer::{
    GpuContext, Material, MaterialBinding, Mesh, Pipeline, RenderAssets, UvRect,
};
use engine_scene::{
    AppliedMaterialOverride, AssetRef, MaterialOverrideData, SceneError, SceneFoliage,
    SceneMaterialOverride, SceneResolver, material_from_imported,
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
        }
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

        let [primitive] = gltf.meshes.as_slice() else {
            return Err(SceneError::Resolve(format!(
                "glTF asset {id} has {} primitives; the editor renderer supports exactly one",
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
        let base_color = imported_material
            .and_then(|material| material.base_color_image)
            .and_then(|index| gltf.images.get(index))
            .map(|image| (image.width, image.height, image.rgba8.as_slice()));
        let material = imported_material.map_or(Material::DEFAULT, material_from_imported);
        let material = material_override.map_or(material, |value| value.apply(material));

        let renderer = MeshRenderer::from_geometry(
            self.gpu,
            self.pipeline,
            self.assets,
            &primitive.vertices,
            &primitive.indices,
            base_color,
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
    assets: &mut RenderAssets,
) -> ResolveReport {
    let pending: Vec<(Entity, MeshSource, Option<MaterialOverrideData>)> = world
        .query_filtered::<(Entity, &MeshSource, Option<&SceneMaterialOverride>), Without<MeshRenderer>>()
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
        )>()
        .iter(world)
        .filter(|(_, _, _, authored, applied)| {
            material_needs_refresh(authored.map(|value| &value.0), *applied)
        })
        .map(|(entity, source, _, authored, _)| {
            (
                entity,
                source.clone(),
                authored.map(|value| value.0.clone()),
            )
        })
        .collect();

    let mut resolver = EditorResolver::new(importer, gpu, pipeline, assets);
    let mut report = ResolveReport::default();

    for (entity, source, authored) in pending.into_iter().chain(refresh) {
        let mesh = AssetRef { id: source.mesh };
        let material = AssetRef {
            id: source.material,
        };
        match resolver.resolve_mesh_with_material(&mesh, &material, authored.as_ref()) {
            Ok(Some(renderer)) => {
                if let Some(previous) = world.get::<MeshRenderer>(entity) {
                    resolver.assets.meshes.release(previous.mesh);
                    resolver.assets.materials.release(previous.material);
                }
                world
                    .entity_mut(entity)
                    .insert((renderer, AppliedMaterialOverride(authored)));
                report.resolved += 1;
            }
            Ok(None) => report.pending += 1,
            Err(err) => {
                report.failed += 1;
                tracing::warn!(error = %err, "could not resolve a scene entity's mesh");
            }
        }
    }

    let sprite_report = resolve_pending_sprites(world, &mut resolver);
    report.resolved += sprite_report.resolved;
    report.pending += sprite_report.pending;
    report.failed += sprite_report.failed;

    let foliage_report = resolve_pending_foliage(world, &mut resolver);
    report.resolved += foliage_report.resolved;
    report.pending += foliage_report.pending;
    report.failed += foliage_report.failed;

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
        let result = resolver
            .resolve_mesh(&source.0.mesh, &source.0.material)
            .map(|resolved| {
                resolved.map(|renderer| InstancedMeshRenderer {
                    mesh: renderer.mesh,
                    material: renderer.material,
                    instances: source.0.instances.iter().copied().map(Into::into).collect(),
                })
            });
        match result {
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
            .spawn(engine_scene::SceneFoliage(engine_scene::FoliageData {
                mesh: reference("3fa85f64-5717-4562-b3fc-2c963f66afa6"),
                material: reference("3fa85f64-5717-4562-b3fc-2c963f66afa6"),
                instances: vec![engine_scene::TransformData::from(
                    engine_utils::Transform::IDENTITY,
                )],
            }))
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
        };
        let blue = engine_scene::MaterialOverrideData {
            tint: [0.2, 0.2, 1.0],
            metallic: None,
            roughness: Some(0.8),
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
